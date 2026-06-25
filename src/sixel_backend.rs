//! A Slint backend that draws the UI in a terminal using the **sixel** graphics protocol.
//!
//! Instead of opening a native window, this [`Platform`] implementation drives Slint's
//! [`SoftwareRenderer`] into an in-memory RGB framebuffer, encodes that frame as sixels
//! (via the pure-Rust [`icy_sixel`] crate) and writes it to the terminal. Keyboard and
//! mouse input are read from the terminal in raw mode with SGR mouse reporting (through
//! [`crossterm`]) and translated into Slint [`WindowEvent`]s.
//!
//! Enable it from `main` by calling [`init`] before creating any Slint component. The app
//! then renders entirely inside a sixel-capable terminal (xterm with sixel support,
//! WezTerm, foot, mlterm, Konsole, …).

use std::cell::{Cell, RefCell};
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{
    EventLoopProxy, Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
};
use slint::{EventLoopError, LogicalPosition, PhysicalSize, PlatformError, Rgb8Pixel};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::{cursor, execute, terminal};

use icy_sixel::{
    sixel_string, DiffusionMethod, MethodForLargest, MethodForRep, PixelFormat, Quality,
};

/// Geometry of the terminal text area, refreshed whenever the terminal is resized.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Geometry {
    /// Number of character cells horizontally / vertically.
    cols: u16,
    rows: u16,
    /// Size of the rendered framebuffer in pixels (`height` is trimmed to a sixel band).
    width: u32,
    height: u32,
    /// Full pixel height of the text area before trimming (used for pointer mapping).
    full_height: u32,
}

impl Geometry {
    fn cell_w(&self) -> f32 {
        self.width as f32 / self.cols.max(1) as f32
    }
    fn cell_h(&self) -> f32 {
        self.full_height as f32 / self.rows.max(1) as f32
    }
}

/// State shared with the [`EventLoopProxy`] so other threads (or `quit_event_loop`) can
/// ask the loop to stop or to run a closure on the UI thread.
#[derive(Default)]
struct Shared {
    quit: AtomicBool,
    queue: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

struct Proxy(Arc<Shared>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        self.0.quit.store(true, Ordering::Release);
        Ok(())
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), EventLoopError> {
        self.0
            .queue
            .lock()
            .map_err(|_| EventLoopError::EventLoopTerminated)?
            .push(event);
        Ok(())
    }
}

/// The sixel terminal backend.
pub struct SixelBackend {
    window: Rc<MinimalSoftwareWindow>,
    shared: Arc<Shared>,
    /// Persistent framebuffer; kept across frames so the software renderer can render
    /// only the dirty region ([`RepaintBufferType::ReusedBuffer`]).
    framebuffer: RefCell<Vec<Rgb8Pixel>>,
    geometry: Cell<Geometry>,
}

impl SixelBackend {
    fn new() -> Self {
        // ReusedBuffer: the renderer keeps previously drawn pixels and only repaints the
        // dirty region into our persistent buffer, which is exactly what we re-encode.
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        Self {
            window,
            shared: Arc::new(Shared::default()),
            framebuffer: RefCell::new(Vec::new()),
            geometry: Cell::new(Geometry::default()),
        }
    }

    /// Query the terminal size and, if it changed, resize the framebuffer and tell Slint.
    /// Returns `true` when the geometry changed (the caller should clear & repaint).
    fn sync_geometry(&self) -> io::Result<bool> {
        let ws = terminal::window_size()?;
        let cols = ws.columns.max(1);
        let rows = ws.rows.max(1);
        let (mut pw, mut ph) = (ws.width as u32, ws.height as u32);
        if pw == 0 || ph == 0 {
            // The terminal didn't report a pixel size; assume a common 8x16 cell.
            pw = cols as u32 * 8;
            ph = rows as u32 * 16;
        }
        // Reserve roughly one text row at the bottom so the image never makes the terminal
        // scroll, then round the height down to a whole sixel band (6 pixels).
        let cell_h = ph / rows as u32;
        let usable_h = ph.saturating_sub(cell_h).max(6);
        let height = (usable_h / 6) * 6;
        let width = pw;

        let geom = Geometry { cols, rows, width, height, full_height: ph };
        if geom == self.geometry.get() && !self.framebuffer.borrow().is_empty() {
            return Ok(false);
        }
        self.geometry.set(geom);
        self.framebuffer
            .borrow_mut()
            .resize((width as usize) * (height as usize), Rgb8Pixel::new(0, 0, 0));
        self.window.set_size(PhysicalSize::new(width, height));
        self.window.window().request_redraw();
        Ok(true)
    }

    /// Render the current frame and write it to the terminal as a sixel image.
    fn emit_frame(&self, out: &mut impl Write) -> io::Result<()> {
        let geom = self.geometry.get();
        let (w, h) = (geom.width as usize, geom.height as usize);
        let stride = w;

        let rendered = self.window.draw_if_needed(|renderer| {
            let mut fb = self.framebuffer.borrow_mut();
            renderer.render(fb.as_mut_slice(), stride);
        });
        if !rendered {
            return Ok(());
        }

        // Flatten the RGB pixels into the byte layout icy_sixel expects (RGB888).
        let fb = self.framebuffer.borrow();
        let mut bytes = Vec::with_capacity(w * h * 3);
        for px in fb.iter() {
            bytes.push(px.r);
            bytes.push(px.g);
            bytes.push(px.b);
        }
        drop(fb);

        if let Ok(sixel) = sixel_string(
            &bytes,
            w as i32,
            h as i32,
            PixelFormat::RGB888,
            DiffusionMethod::Atkinson,
            MethodForLargest::Auto,
            MethodForRep::Auto,
            Quality::LOW,
        ) {
            // Move to the home position and draw the frame in place (no trailing newline,
            // so the terminal does not scroll).
            out.write_all(b"\x1b[H")?;
            out.write_all(sixel.as_bytes())?;
            out.flush()?;
        }
        Ok(())
    }

    /// Translate one terminal event into Slint window events.
    fn handle_event(&self, ev: Event, out: &mut impl Write) -> io::Result<()> {
        let win = self.window.window();
        match ev {
            Event::Key(k) => {
                if k.kind == KeyEventKind::Release {
                    return Ok(());
                }
                // Ctrl+C / Ctrl+Q quit the application.
                if k.modifiers.contains(KeyModifiers::CONTROL) {
                    if let KeyCode::Char('c' | 'q') = k.code {
                        self.shared.quit.store(true, Ordering::Release);
                        return Ok(());
                    }
                }
                let repeat = k.kind == KeyEventKind::Repeat;
                if let Some((text, is_char)) = keycode_to_text(k.code) {
                    // For plain characters the shift state is already baked into the case,
                    // so only synthesize shift for non-text keys (arrows, tab, …).
                    with_modifiers(win, k.modifiers, !is_char, |win| {
                        win.dispatch_event(if repeat {
                            WindowEvent::KeyPressRepeated { text: text.clone() }
                        } else {
                            WindowEvent::KeyPressed { text: text.clone() }
                        });
                        win.dispatch_event(WindowEvent::KeyReleased { text });
                    });
                }
            }
            Event::Mouse(m) => {
                let geom = self.geometry.get();
                let position = LogicalPosition::new(
                    (m.column as f32 + 0.5) * geom.cell_w(),
                    (m.row as f32 + 0.5) * geom.cell_h(),
                );
                match m.kind {
                    MouseEventKind::Down(button) => {
                        with_modifiers(win, m.modifiers, true, |win| {
                            win.dispatch_event(WindowEvent::PointerPressed {
                                position,
                                button: map_button(button),
                            });
                        });
                    }
                    MouseEventKind::Up(button) => {
                        with_modifiers(win, m.modifiers, true, |win| {
                            win.dispatch_event(WindowEvent::PointerReleased {
                                position,
                                button: map_button(button),
                            });
                        });
                    }
                    MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                        win.dispatch_event(WindowEvent::PointerMoved { position });
                    }
                    MouseEventKind::ScrollUp => {
                        win.dispatch_event(WindowEvent::PointerScrolled {
                            position,
                            delta_x: 0.0,
                            delta_y: geom.cell_h() * 3.0,
                        });
                    }
                    MouseEventKind::ScrollDown => {
                        win.dispatch_event(WindowEvent::PointerScrolled {
                            position,
                            delta_x: 0.0,
                            delta_y: -geom.cell_h() * 3.0,
                        });
                    }
                    MouseEventKind::ScrollLeft => {
                        win.dispatch_event(WindowEvent::PointerScrolled {
                            position,
                            delta_x: geom.cell_w() * 3.0,
                            delta_y: 0.0,
                        });
                    }
                    MouseEventKind::ScrollRight => {
                        win.dispatch_event(WindowEvent::PointerScrolled {
                            position,
                            delta_x: -geom.cell_w() * 3.0,
                            delta_y: 0.0,
                        });
                    }
                }
            }
            Event::Resize(..) => {
                if self.sync_geometry()? {
                    execute!(out, terminal::Clear(terminal::ClearType::All))?;
                }
            }
            Event::FocusGained => win.dispatch_event(WindowEvent::WindowActiveChanged(true)),
            Event::FocusLost => win.dispatch_event(WindowEvent::WindowActiveChanged(false)),
            Event::Paste(text) => {
                for ch in text.chars() {
                    let t = slint::SharedString::from(ch.to_string());
                    win.dispatch_event(WindowEvent::KeyPressed { text: t.clone() });
                    win.dispatch_event(WindowEvent::KeyReleased { text: t });
                }
            }
        }
        Ok(())
    }

    /// Drain closures queued through [`slint::invoke_from_event_loop`].
    fn drain_queue(&self) {
        let drained: Vec<_> = match self.shared.queue.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => return,
        };
        for f in drained {
            f();
        }
    }
}

impl Platform for SixelBackend {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.shared.clone())))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut out = io::stdout();
        let _guard = TerminalGuard::enter().map_err(io_err)?;

        self.sync_geometry().map_err(io_err)?;
        // The software renderer defaults to a scale factor of 1; make it explicit.
        self.window
            .window()
            .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 1.0 });
        execute!(out, terminal::Clear(terminal::ClearType::All)).map_err(io_err)?;

        while !self.shared.quit.load(Ordering::Acquire) {
            slint::platform::update_timers_and_animations();
            self.drain_queue();
            if self.shared.quit.load(Ordering::Acquire) {
                break;
            }

            // Wait for input, but never longer than the next timer/animation tick.
            let timeout = if self.window.window().has_active_animations() {
                Duration::from_millis(16)
            } else {
                slint::platform::duration_until_next_timer_update()
                    .unwrap_or(Duration::from_millis(50))
            }
            .min(Duration::from_millis(100));

            if event::poll(timeout).map_err(io_err)? {
                loop {
                    let ev = event::read().map_err(io_err)?;
                    self.handle_event(ev, &mut out).map_err(io_err)?;
                    if !event::poll(Duration::ZERO).map_err(io_err)? {
                        break;
                    }
                }
            }
            if self.shared.quit.load(Ordering::Acquire) {
                break;
            }

            self.emit_frame(&mut out).map_err(io_err)?;
        }
        Ok(())
    }
}

/// Install the sixel backend as the active Slint platform. Call this once, before any
/// Slint component is created.
pub fn init() -> Result<(), PlatformError> {
    slint::platform::set_platform(Box::new(SixelBackend::new()))
        .map_err(PlatformError::SetPlatformError)
}

fn io_err(e: io::Error) -> PlatformError {
    PlatformError::Other(format!("sixel terminal backend: {e}"))
}

fn map_button(b: MouseButton) -> PointerEventButton {
    match b {
        MouseButton::Left => PointerEventButton::Left,
        MouseButton::Right => PointerEventButton::Right,
        MouseButton::Middle => PointerEventButton::Middle,
    }
}

/// Press the modifier keys present in `mods`, run `f`, then release them in reverse, so
/// that Slint sees a clean modifier+event combination (it tracks modifiers via key
/// events, since pointer events carry no modifier field).
fn with_modifiers(
    win: &slint::Window,
    mods: KeyModifiers,
    include_shift: bool,
    f: impl FnOnce(&slint::Window),
) {
    let mut held: Vec<Key> = Vec::new();
    if include_shift && mods.contains(KeyModifiers::SHIFT) {
        held.push(Key::Shift);
    }
    if mods.contains(KeyModifiers::CONTROL) {
        held.push(Key::Control);
    }
    if mods.contains(KeyModifiers::ALT) {
        held.push(Key::Alt);
    }
    if mods.contains(KeyModifiers::SUPER) || mods.contains(KeyModifiers::META) {
        held.push(Key::Meta);
    }
    for k in &held {
        win.dispatch_event(WindowEvent::KeyPressed { text: (*k).into() });
    }
    f(win);
    for k in held.iter().rev() {
        win.dispatch_event(WindowEvent::KeyReleased { text: (*k).into() });
    }
}

/// Map a crossterm key code to the text Slint expects. The returned bool is `true` when
/// the key is a printable character (whose case already encodes Shift).
fn keycode_to_text(code: KeyCode) -> Option<(slint::SharedString, bool)> {
    let key = |k: Key| Some((k.into(), false));
    match code {
        KeyCode::Char(c) => Some((slint::SharedString::from(c.to_string()), true)),
        KeyCode::Enter => key(Key::Return),
        KeyCode::Backspace => key(Key::Backspace),
        KeyCode::Esc => key(Key::Escape),
        KeyCode::Tab => key(Key::Tab),
        KeyCode::BackTab => key(Key::Backtab),
        KeyCode::Delete => key(Key::Delete),
        KeyCode::Insert => key(Key::Insert),
        KeyCode::Up => key(Key::UpArrow),
        KeyCode::Down => key(Key::DownArrow),
        KeyCode::Left => key(Key::LeftArrow),
        KeyCode::Right => key(Key::RightArrow),
        KeyCode::Home => key(Key::Home),
        KeyCode::End => key(Key::End),
        KeyCode::PageUp => key(Key::PageUp),
        KeyCode::PageDown => key(Key::PageDown),
        KeyCode::F(_) | KeyCode::Null | KeyCode::Menu => None,
        _ => None,
    }
}

/// RAII guard: switches the terminal into the mode the backend needs and restores it
/// (alternate screen, raw mode, mouse capture, cursor) on drop, even on panic.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, terminal::EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = execute!(out, cursor::Show, DisableMouseCapture, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
