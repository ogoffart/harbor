#![windows_subsystem = "windows"]
slint::include_modules!();

mod data;
mod terminal_backend;

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

use chrono::Local;
use slint::{ModelRc, VecModel};

use data::*;

#[derive(Clone, PartialEq)]
enum Location {
    Folder(Vec<String>),
    Recents,
    Tag(i32),
}

fn favorites() -> Vec<(&'static str, &'static str, Location)> {
    vec![
        ("Recents", "clock", Location::Recents),
        ("Desktop", "system", Location::Folder(vec!["Desktop".into()])),
        ("Documents", "doc", Location::Folder(vec!["Documents".into()])),
        ("Downloads", "download", Location::Folder(vec!["Downloads".into()])),
        ("Pictures", "image", Location::Folder(vec!["Pictures".into()])),
        ("Projects", "code", Location::Folder(vec!["Projects".into()])),
        ("Music", "audio", Location::Folder(vec!["Music".into()])),
        ("Movies", "video", Location::Folder(vec!["Movies".into()])),
    ]
}

struct App {
    root: NodeRef,
    now: f64,
    next_id: Cell<i32>,
    rng: Cell<u64>,
    drives: RefCell<Vec<Drive>>,

    history: RefCell<Vec<Location>>,
    hindex: Cell<usize>,

    view: RefCell<String>,
    sort_key: RefCell<String>,
    sort_dir: RefCell<String>,
    query: RefCell<String>,
    sidebar_collapsed: Cell<bool>,
    details_collapsed: Cell<bool>,
    theme_pref: Cell<i32>,

    selected: RefCell<HashSet<i32>>,
    anchor: Cell<i32>, // -1 = none

    items: RefCell<Vec<NodeRef>>,

    dl: RefCell<Option<NodeRef>>,
    dl_progress: Cell<f32>,
    transfer_done: Cell<bool>,

    ui: RefCell<slint::Weak<AppWindow>>,
}

impl App {
    fn rand01(&self) -> f64 {
        let mut s = self.rng.get();
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.rng.set(s);
        ((s >> 11) as f64) / ((1u64 << 53) as f64)
    }

    fn location(&self) -> Location {
        self.history.borrow()[self.hindex.get()].clone()
    }

    fn get_node(&self, path: &[String]) -> Option<NodeRef> {
        let mut node = self.root.clone();
        for name in path {
            let next = {
                let n = node.borrow();
                n.children.as_ref()?.iter().find(|c| {
                    let c = c.borrow();
                    c.is_folder && &c.name == name
                }).cloned()
            };
            node = next?;
        }
        Some(node)
    }

    fn find_path(&self, target: &NodeRef) -> Option<Vec<String>> {
        fn walk(node: &NodeRef, trail: &mut Vec<String>, tid: i32) -> Option<Vec<String>> {
            let children = node.borrow().children.clone();
            if let Some(ch) = children {
                for c in ch {
                    let (cid, cname, isf) = {
                        let b = c.borrow();
                        (b.id, b.name.clone(), b.is_folder)
                    };
                    trail.push(cname);
                    if cid == tid {
                        return Some(trail.clone());
                    }
                    if isf {
                        if let Some(r) = walk(&c, trail, tid) {
                            return Some(r);
                        }
                    }
                    trail.pop();
                }
            }
            None
        }
        let tid = target.borrow().id;
        let mut t = vec![];
        walk(&self.root, &mut t, tid)
    }

    fn sort_items(&self, items: &mut [NodeRef], key: &str, dir: &str) {
        let asc = dir == "asc";
        items.sort_by(|a, b| {
            let a = a.borrow();
            let b = b.borrow();
            let fa = if a.is_folder { 0 } else { 1 };
            let fb = if b.is_folder { 0 } else { 1 };
            if fa != fb {
                return fa.cmp(&fb);
            }
            let mut ord = match key {
                "modified" => a.modified.partial_cmp(&b.modified).unwrap_or(Ordering::Equal),
                "size" => {
                    let sa = if a.is_folder { folder_size(&a) } else { a.size };
                    let sb = if b.is_folder { folder_size(&b) } else { b.size };
                    sa.partial_cmp(&sb).unwrap_or(Ordering::Equal)
                }
                "kind" => {
                    let ka = if a.is_folder { "0folder".to_string() } else { a.kind.label().to_lowercase() };
                    let kb = if b.is_folder { "0folder".to_string() } else { b.kind.label().to_lowercase() };
                    ka.cmp(&kb)
                }
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            };
            if ord == Ordering::Equal {
                ord = a.name.to_lowercase().cmp(&b.name.to_lowercase());
            }
            if asc { ord } else { ord.reverse() }
        });
    }

    fn recompute(&self) {
        let loc = self.location();
        let mut base: Vec<NodeRef> = match &loc {
            Location::Folder(path) => self
                .get_node(path)
                .and_then(|n| n.borrow().children.clone())
                .unwrap_or_default(),
            Location::Recents => {
                let mut v = vec![];
                collect_all(&self.root, &|n| !n.is_folder, &mut v);
                v.sort_by(|a, b| {
                    b.borrow().modified.partial_cmp(&a.borrow().modified).unwrap_or(Ordering::Equal)
                });
                v.truncate(24);
                v
            }
            Location::Tag(i) => {
                let mut v = vec![];
                let idx = *i;
                collect_all(&self.root, &|n| n.tags.contains(&idx), &mut v);
                v
            }
        };

        let q = self.query.borrow().trim().to_lowercase();
        if !q.is_empty() {
            base.retain(|n| n.borrow().name.to_lowercase().contains(&q));
        }

        let key = self.sort_key.borrow().clone();
        let dir = self.sort_dir.borrow().clone();
        let keep_recents = matches!(loc, Location::Recents) && key == "name" && dir == "asc";
        if !keep_recents {
            self.sort_items(&mut base, &key, &dir);
        }
        *self.items.borrow_mut() = base;
    }

    fn sel_items(&self) -> Vec<NodeRef> {
        let sel = self.selected.borrow();
        self.items.borrow().iter().filter(|n| sel.contains(&n.borrow().id)).cloned().collect()
    }

    fn refresh(&self) {
        self.recompute();
        let Some(ui) = self.ui.borrow().upgrade() else { return };
        let app = ui.global::<AppData>();

        let now = self.now;
        let items = self.items.borrow();
        let sel = self.selected.borrow();

        // ---- file rows ----
        let rows: Vec<FileRow> = items.iter().map(|n| {
            let n = n.borrow();
            let tags: Vec<i32> = n.tags.clone();
            FileRow {
                id: n.id,
                name: n.name.clone().into(),
                icon: (if n.is_folder { "folder" } else { n.kind.icon() }).into(),
                is_folder: n.is_folder,
                is_media: !n.is_folder && n.kind.is_media(),
                date_text: format_date(n.modified, now).into(),
                size_text: if n.is_folder { "—".into() } else { format_bytes(n.size).into() },
                kind_text: (if n.is_folder { "Folder" } else { n.kind.label() }).into(),
                tags: ModelRc::new(VecModel::from(tags)),
                selected: sel.contains(&n.id),
                downloading: n.downloading,
                progress: n.progress,
            }
        }).collect();
        app.set_files(ModelRc::new(VecModel::from(rows)));

        // ---- breadcrumb ----
        let loc = self.location();
        let crumbs: Vec<CrumbRow> = match &loc {
            Location::Folder(path) => {
                let mut v = vec![CrumbRow {
                    label: "Harbor".into(),
                    icon: "".into(),
                    tag: -1,
                    nav_index: 0,
                    last: path.is_empty(),
                }];
                for (i, name) in path.iter().enumerate() {
                    v.push(CrumbRow {
                        label: name.clone().into(),
                        icon: "".into(),
                        tag: -1,
                        nav_index: (i + 1) as i32,
                        last: i == path.len() - 1,
                    });
                }
                v
            }
            Location::Recents => vec![CrumbRow {
                label: "Recents".into(), icon: "clock".into(), tag: -1, nav_index: -1, last: true,
            }],
            Location::Tag(i) => vec![CrumbRow {
                label: tag_name(*i).into(), icon: "".into(), tag: *i, nav_index: -1, last: true,
            }],
        };
        app.set_crumbs(ModelRc::new(VecModel::from(crumbs)));

        // ---- favorites ----
        let favs: Vec<FavRow> = favorites().into_iter().map(|(label, icon, floc)| FavRow {
            label: label.into(),
            icon: icon.into(),
            active: floc == loc,
        }).collect();
        app.set_favorites(ModelRc::new(VecModel::from(favs)));

        // ---- tags ----
        let tagrows: Vec<TagRow> = (0..7).map(|i| TagRow {
            key: tag_key(i).into(),
            name: tag_name(i).into(),
            color: i,
            active: loc == Location::Tag(i),
        }).collect();
        app.set_tags(ModelRc::new(VecModel::from(tagrows)));

        // ---- drives ----
        let drives = self.drives.borrow();
        let driverows: Vec<DriveRow> = drives.iter().map(|d| DriveRow {
            id: d.id.clone().into(),
            name: d.name.clone().into(),
            icon: d.icon.clone().into(),
            free_text: format_bytes(d.total - d.used).into(),
            total_text: format_bytes(d.total).into(),
            pct: (d.used / d.total) as f32,
            ejectable: d.ejectable,
            active: false,
        }).collect();
        app.set_drives(ModelRc::new(VecModel::from(driverows)));

        // ---- status bar ----
        let item_count = items.len();
        let selected_items: Vec<NodeRef> = items.iter().filter(|n| sel.contains(&n.borrow().id)).cloned().collect();
        let sel_count = selected_items.len();
        let sel_size: f64 = selected_items.iter().map(|n| {
            let n = n.borrow();
            if n.is_folder { folder_size(&n) } else { n.size }
        }).sum();
        app.set_status_left(if sel_count > 0 {
            format!("{} of {} selected · {}", sel_count, item_count, format_bytes(sel_size)).into()
        } else {
            format!("{} items", item_count).into()
        });

        let sys = drives.iter().find(|d| d.system).or_else(|| drives.first());
        if let Some(s) = sys {
            app.set_drive_icon(s.icon.clone().into());
            app.set_drive_available(format!("{} available", format_bytes(s.total - s.used)).into());
        } else {
            app.set_drive_available("".into());
        }

        // ---- details panel ----
        self.set_details(&app, &selected_items, &items, &loc);

        // ---- transfer ----
        let transfer = if !self.transfer_done.get() {
            self.dl.borrow().clone()
        } else {
            None
        };
        if let Some(dl) = transfer {
            let dl = dl.borrow();
            let pct = self.dl_progress.get();
            let rate = 28 + (self.rand01() * 12.0).round() as i32;
            app.set_transfer_active(true);
            app.set_transfer_pct(pct);
            app.set_transfer_name(dl.name.clone().into());
            app.set_transfer_stat(format!(
                "{} of {} · {} MB/s",
                format_bytes(dl.size * pct as f64),
                format_bytes(dl.size),
                rate
            ).into());
        } else {
            app.set_transfer_active(false);
        }

        // ---- toolbar / nav state ----
        let hi = self.hindex.get();
        let hlen = self.history.borrow().len();
        app.set_can_back(hi > 0);
        app.set_can_forward(hi + 1 < hlen);
        app.set_can_up(matches!(&loc, Location::Folder(p) if !p.is_empty()));
        app.set_view(self.view.borrow().clone().into());
        app.set_query(self.query.borrow().clone().into());
        app.set_sort_key(self.sort_key.borrow().clone().into());
        app.set_sort_dir(self.sort_dir.borrow().clone().into());
        app.set_sort_active(!(*self.sort_key.borrow() == "name" && *self.sort_dir.borrow() == "asc"));
        app.set_sidebar_collapsed(self.sidebar_collapsed.get());
        app.set_details_collapsed(self.details_collapsed.get());

        let pref = self.theme_pref.get();
        app.set_theme_pref(pref);
        app.set_theme_icon((match pref { 1 => "sun", 2 => "moon", _ => "system" }).into());
        ui.global::<Theme>().set_pref(pref);
    }

    fn set_details(&self, app: &AppData, sel: &[NodeRef], items: &[NodeRef], loc: &Location) {
        if sel.len() == 1 {
            let it = sel[0].borrow();
            app.set_detail_mode(1);
            app.set_det_name(it.name.clone().into());
            let size_str = if it.is_folder { format_bytes(folder_size(&it)) } else { format_bytes(it.size) };
            let kind_str = if it.is_folder { "Folder" } else { it.kind.label() };
            app.set_det_kind(format!("{} · {}", kind_str, size_str).into());
            app.set_det_icon((if it.is_folder { "folder" } else { it.kind.icon() }).into());
            app.set_det_is_media(!it.is_folder && it.kind.is_media());
            app.set_det_is_folder(it.is_folder);
            app.set_det_tags(ModelRc::new(VecModel::from(it.tags.clone())));

            let mut rows: Vec<DetailRow> = vec![];
            let mut push = |k: &str, v: String| rows.push(DetailRow { key: k.into(), value: v.into() });
            push("Kind", kind_str.to_string());
            if it.is_folder {
                push("Size", format!("{} items, {}", count_items(&it), format_bytes(folder_size(&it))));
            } else {
                push("Size", format_bytes(it.size));
            }
            if let Some(d) = &it.dims {
                push("Dimensions", d.clone());
            }
            push("Modified", format_date(it.modified, self.now));
            push("Created", full_date(it.modified));
            if it.downloading {
                push("Status", format!("Downloading · {}%", (it.progress * 100.0).round() as i32));
            }
            app.set_det_rows(ModelRc::new(VecModel::from(rows)));
        } else if sel.len() > 1 {
            let total: f64 = sel.iter().map(|n| {
                let n = n.borrow();
                if n.is_folder { folder_size(&n) } else { n.size }
            }).sum();
            let folders = sel.iter().filter(|n| n.borrow().is_folder).count();
            app.set_detail_mode(2);
            app.set_det_name(format!("{} items selected", sel.len()).into());
            app.set_det_kind(format!("{} folders · {} files", folders, sel.len() - folders).into());
            app.set_det_icon("copy".into());
            app.set_det_is_media(false);
            app.set_det_is_folder(false);
            app.set_det_tags(ModelRc::new(VecModel::from(Vec::<i32>::new())));
            let rows = vec![
                DetailRow { key: "Total size".into(), value: format_bytes(total).into() },
                DetailRow { key: "Items".into(), value: sel.len().to_string().into() },
            ];
            app.set_det_rows(ModelRc::new(VecModel::from(rows)));
        } else {
            let label = match loc {
                Location::Folder(p) => p.last().cloned().unwrap_or_else(|| "Harbor".into()),
                Location::Recents => "Recents".into(),
                Location::Tag(i) => tag_name(*i).into(),
            };
            let total: f64 = items.iter().map(|n| {
                let n = n.borrow();
                if n.is_folder { 0.0 } else { n.size }
            }).sum();
            app.set_detail_mode(0);
            app.set_det_name(label.into());
            app.set_det_kind(format!("{} items · {}", items.len(), format_bytes(total)).into());
            app.set_det_hint("Select a file to see a preview, metadata, and quick actions.".into());
        }
    }

    // ---------- interactions ----------
    fn navigate(&self, loc: Location) {
        let mut h = self.history.borrow_mut();
        let hi = self.hindex.get();
        h.truncate(hi + 1);
        h.push(loc);
        drop(h);
        self.hindex.set(hi + 1);
        self.selected.borrow_mut().clear();
        *self.query.borrow_mut() = String::new();
        self.anchor.set(-1);
        self.refresh();
    }

    fn row_down(&self, index: i32, ctrl: bool, shift: bool, right: bool) {
        let items = self.items.borrow();
        if index < 0 || index as usize >= items.len() {
            return;
        }
        let idx = index as usize;
        let id = items[idx].borrow().id;
        if right {
            if !self.selected.borrow().contains(&id) {
                *self.selected.borrow_mut() = HashSet::from([id]);
                self.anchor.set(index);
            }
        } else if ctrl {
            let mut s = self.selected.borrow_mut();
            if s.contains(&id) { s.remove(&id); } else { s.insert(id); }
            drop(s);
            self.anchor.set(index);
        } else if shift && self.anchor.get() >= 0 {
            let a = self.anchor.get() as usize;
            let (lo, hi) = if a <= idx { (a, idx) } else { (idx, a) };
            let set: HashSet<i32> = items[lo..=hi.min(items.len() - 1)].iter().map(|n| n.borrow().id).collect();
            *self.selected.borrow_mut() = set;
        } else {
            *self.selected.borrow_mut() = HashSet::from([id]);
            self.anchor.set(index);
        }
        drop(items);
        self.refresh();
    }

    fn row_open(&self, index: i32) {
        let node = {
            let items = self.items.borrow();
            if index < 0 || index as usize >= items.len() {
                return;
            }
            items[index as usize].clone()
        };
        let is_folder = node.borrow().is_folder;
        if is_folder {
            let name = node.borrow().name.clone();
            let path = match self.location() {
                Location::Folder(p) => {
                    let mut p = p.clone();
                    p.push(name);
                    Some(p)
                }
                _ => self.find_path(&node),
            };
            if let Some(p) = path {
                self.navigate(Location::Folder(p));
            }
        } else {
            self.details_collapsed.set(false);
            let id = node.borrow().id;
            *self.selected.borrow_mut() = HashSet::from([id]);
            self.refresh();
        }
    }

    fn go_back(&self) {
        if self.hindex.get() > 0 {
            self.hindex.set(self.hindex.get() - 1);
            self.selected.borrow_mut().clear();
            *self.query.borrow_mut() = String::new();
            self.anchor.set(-1);
            self.refresh();
        }
    }
    fn go_forward(&self) {
        if self.hindex.get() + 1 < self.history.borrow().len() {
            self.hindex.set(self.hindex.get() + 1);
            self.selected.borrow_mut().clear();
            *self.query.borrow_mut() = String::new();
            self.anchor.set(-1);
            self.refresh();
        }
    }
    fn go_up(&self) {
        if let Location::Folder(p) = self.location() {
            if !p.is_empty() {
                let mut p = p.clone();
                p.pop();
                self.navigate(Location::Folder(p));
            }
        }
    }

    fn sort_col(&self, key: &str) {
        if *self.sort_key.borrow() == key {
            let flip = if *self.sort_dir.borrow() == "asc" { "desc" } else { "asc" };
            *self.sort_dir.borrow_mut() = flip.into();
        } else {
            *self.sort_key.borrow_mut() = key.into();
            *self.sort_dir.borrow_mut() = if key == "modified" || key == "size" { "desc" } else { "asc" }.into();
        }
        self.refresh();
    }

    fn current_folder(&self) -> Option<NodeRef> {
        if let Location::Folder(p) = self.location() {
            self.get_node(&p)
        } else {
            None
        }
    }

    fn new_id(&self) -> i32 {
        let n = self.next_id.get() + 1;
        self.next_id.set(n);
        n
    }

    fn new_folder(&self) {
        if let Some(folder) = self.current_folder() {
            let node = Rc::new(RefCell::new(Node {
                id: self.new_id(),
                name: "untitled folder".into(),
                kind: Kind::Folder,
                size: 0.0,
                modified: Local::now().timestamp_millis() as f64,
                is_folder: true,
                tags: vec![],
                children: Some(vec![]),
                downloading: false,
                progress: 0.0,
                dims: None,
            }));
            let id = node.borrow().id;
            if let Some(ch) = folder.borrow_mut().children.as_mut() {
                ch.insert(0, node);
            }
            *self.selected.borrow_mut() = HashSet::from([id]);
            self.refresh();
        }
    }

    fn duplicate_selected(&self) {
        let Some(folder) = self.current_folder() else { return };
        let sel = self.selected.borrow().clone();
        let mut fb = folder.borrow_mut();
        let Some(children) = fb.children.as_mut() else { return };
        let mut i = 0;
        while i < children.len() {
            let (id, is_sel) = {
                let c = children[i].borrow();
                (c.id, sel.contains(&c.id))
            };
            let _ = id;
            if is_sel {
                let src = children[i].borrow();
                let copy_name = {
                    let dot = src.name.rfind('.');
                    match (src.is_folder, dot) {
                        (false, Some(d)) if d > 0 => format!("{} copy{}", &src.name[..d], &src.name[d..]),
                        _ => format!("{} copy", src.name),
                    }
                };
                let clone = Node {
                    id: self.new_id(),
                    name: copy_name,
                    kind: src.kind,
                    size: src.size,
                    modified: Local::now().timestamp_millis() as f64,
                    is_folder: src.is_folder,
                    tags: src.tags.clone(),
                    children: src.children.clone(),
                    downloading: false,
                    progress: 0.0,
                    dims: src.dims.clone(),
                };
                drop(src);
                children.insert(i + 1, Rc::new(RefCell::new(clone)));
                i += 2;
            } else {
                i += 1;
            }
        }
        drop(fb);
        self.refresh();
    }

    fn trash_selected(&self) {
        let Some(folder) = self.current_folder() else { return };
        let sel = self.selected.borrow().clone();
        if let Some(ch) = folder.borrow_mut().children.as_mut() {
            ch.retain(|c| !sel.contains(&c.borrow().id));
        }
        self.selected.borrow_mut().clear();
        self.refresh();
    }

    fn tag_toggle(&self, key: &str) {
        let idx = tag_index(key);
        let sel = self.selected.borrow().clone();
        for n in self.items.borrow().iter() {
            let id = n.borrow().id;
            if sel.contains(&id) {
                let mut nb = n.borrow_mut();
                if let Some(pos) = nb.tags.iter().position(|t| *t == idx) {
                    nb.tags.remove(pos);
                } else {
                    nb.tags.push(idx);
                }
            }
        }
        self.refresh();
    }

    fn key_move(&self, down: bool) {
        let len = self.items.borrow().len();
        if len == 0 {
            return;
        }
        let cur = self.anchor.get();
        let new = if down {
            (cur + 1).min(len as i32 - 1).max(0)
        } else {
            (cur - 1).max(0)
        };
        let id = self.items.borrow()[new as usize].borrow().id;
        *self.selected.borrow_mut() = HashSet::from([id]);
        self.anchor.set(new);
        self.refresh();
    }

    fn key_enter(&self) {
        let sel = self.sel_items();
        if sel.len() == 1 {
            if let Some(pos) = self.items.borrow().iter().position(|n| Rc::ptr_eq(n, &sel[0])) {
                self.row_open(pos as i32);
            }
        }
    }

    fn select_all(&self) {
        let all: HashSet<i32> = self.items.borrow().iter().map(|n| n.borrow().id).collect();
        *self.selected.borrow_mut() = all;
        self.refresh();
    }

    fn clear_selection(&self) {
        self.selected.borrow_mut().clear();
        self.refresh();
    }
}

fn collect_all(node: &NodeRef, pred: &dyn Fn(&Node) -> bool, out: &mut Vec<NodeRef>) {
    let children = node.borrow().children.clone();
    if let Some(ch) = children {
        for c in ch {
            if pred(&c.borrow()) {
                out.push(c.clone());
            }
            if c.borrow().is_folder {
                collect_all(&c, pred, out);
            }
        }
    }
}

/// Whether to draw the UI in the terminal (sixel / Kitty graphics) instead of a native
/// window. Enabled with `--sixel`/`--tui` or `HARBOR_BACKEND=sixel|terminal|tui`.
fn wants_terminal_backend() -> bool {
    std::env::args().skip(1).any(|a| a == "--sixel" || a == "--tui")
        || std::env::var("HARBOR_BACKEND")
            .map(|v| matches!(v.as_str(), "sixel" | "terminal" | "tui"))
            .unwrap_or(false)
}

fn main() -> Result<(), slint::PlatformError> {
    // Install the terminal platform before any Slint component is created.
    if wants_terminal_backend() {
        terminal_backend::init()?;
    }

    let (root, drives, next_id, now) = build();

    // locate the live download node in Downloads
    let dl = root
        .borrow()
        .children
        .as_ref()
        .and_then(|ch| ch.iter().find(|c| c.borrow().name == "Downloads").cloned())
        .and_then(|d| d.borrow().children.as_ref().and_then(|ch| ch.iter().find(|c| c.borrow().downloading).cloned()));

    let app = Rc::new(App {
        root,
        now,
        next_id: Cell::new(next_id),
        rng: Cell::new((now as u64) | 1),
        drives: RefCell::new(drives),
        history: RefCell::new(vec![Location::Folder(vec!["Downloads".into()])]),
        hindex: Cell::new(0),
        view: RefCell::new("list".into()),
        sort_key: RefCell::new("name".into()),
        sort_dir: RefCell::new("asc".into()),
        query: RefCell::new(String::new()),
        sidebar_collapsed: Cell::new(false),
        details_collapsed: Cell::new(false),
        theme_pref: Cell::new(0),
        selected: RefCell::new(HashSet::new()),
        anchor: Cell::new(-1),
        items: RefCell::new(vec![]),
        dl: RefCell::new(dl.clone()),
        dl_progress: Cell::new(0.34),
        transfer_done: Cell::new(false),
        ui: RefCell::new(slint::Weak::default()),
    });

    let ui = AppWindow::new()?;
    *app.ui.borrow_mut() = ui.as_weak();

    let logic = ui.global::<Logic>();

    {
        let a = app.clone();
        logic.on_fav_clicked(move |i| {
            let favs = favorites();
            if let Some((_, _, loc)) = favs.into_iter().nth(i as usize) {
                a.navigate(loc);
            }
        });
    }
    {
        let a = app.clone();
        logic.on_tag_clicked(move |key| a.navigate(Location::Tag(tag_index(&key))));
    }
    {
        let a = app.clone();
        logic.on_drive_eject(move |id| {
            a.drives.borrow_mut().retain(|d| d.id != id.as_str());
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_crumb_clicked(move |ni| {
            if ni >= 0 {
                if let Location::Folder(p) = a.location() {
                    a.navigate(Location::Folder(p[..(ni as usize).min(p.len())].to_vec()));
                }
            }
        });
    }
    {
        let a = app.clone();
        logic.on_back(move || a.go_back());
    }
    {
        let a = app.clone();
        logic.on_forward(move || a.go_forward());
    }
    {
        let a = app.clone();
        logic.on_up(move || a.go_up());
    }
    {
        let a = app.clone();
        logic.on_toggle_sidebar(move || {
            a.sidebar_collapsed.set(!a.sidebar_collapsed.get());
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_toggle_details(move || {
            a.details_collapsed.set(!a.details_collapsed.get());
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_row_down(move |i, ctrl, shift, right| a.row_down(i, ctrl, shift, right));
    }
    {
        let a = app.clone();
        logic.on_row_open(move |i| a.row_open(i));
    }
    {
        let a = app.clone();
        logic.on_empty_click(move || a.clear_selection());
    }
    {
        let a = app.clone();
        logic.on_sort_col(move |k| a.sort_col(&k));
    }
    {
        let a = app.clone();
        logic.on_set_sort_key(move |k| a.sort_col(&k));
    }
    {
        let a = app.clone();
        logic.on_set_sort_dir(move |d| {
            *a.sort_dir.borrow_mut() = d.to_string();
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_set_view(move |v| {
            *a.view.borrow_mut() = v.to_string();
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_set_query(move |q| {
            *a.query.borrow_mut() = q.to_string();
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_set_theme(move |t| {
            a.theme_pref.set(t);
            a.refresh();
        });
    }
    {
        let a = app.clone();
        logic.on_tag_toggle(move |key| a.tag_toggle(&key));
    }
    {
        let a = app.clone();
        logic.on_menu_action(move |action| match action.as_str() {
            "new-folder" => a.new_folder(),
            "get-info" => {
                a.details_collapsed.set(false);
                a.refresh();
            }
            "duplicate" => a.duplicate_selected(),
            "trash" => a.trash_selected(),
            "refresh" => a.refresh(),
            "open-selected" => {
                let sel = a.sel_items();
                if let Some(first) = sel.first() {
                    if let Some(pos) = a.items.borrow().iter().position(|n| Rc::ptr_eq(n, first)) {
                        a.row_open(pos as i32);
                    }
                }
            }
            _ => {}
        });
    }
    {
        let a = app.clone();
        logic.on_key_move(move |down| a.key_move(down));
    }
    {
        let a = app.clone();
        logic.on_key_enter(move || a.key_enter());
    }
    {
        let a = app.clone();
        logic.on_key_escape(move || a.clear_selection());
    }
    {
        let a = app.clone();
        logic.on_key_select_all(move || a.select_all());
    }
    {
        let a = app.clone();
        logic.on_key_trash(move || a.trash_selected());
    }

    // ---- live timers ----
    let dl_timer = slint::Timer::default();
    {
        let a = app.clone();
        dl_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(900), move || {
            if a.transfer_done.get() {
                return;
            }
            let next = (a.dl_progress.get() + 0.012 + (a.rand01() as f32) * 0.02).min(1.0);
            a.dl_progress.set(next);
            if let Some(dl) = a.dl.borrow().as_ref() {
                dl.borrow_mut().progress = next;
                if next >= 1.0 {
                    let mut d = dl.borrow_mut();
                    d.downloading = false;
                    d.modified = Local::now().timestamp_millis() as f64;
                }
            }
            if next >= 1.0 {
                a.transfer_done.set(true);
                if let Some(ui) = a.ui.borrow().upgrade() {
                    ui.global::<AppData>().set_xfer_open(false);
                }
            }
            a.refresh();
        });
    }

    let drift_timer = slint::Timer::default();
    {
        let a = app.clone();
        drift_timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(2600), move || {
            {
                let mut drives = a.drives.borrow_mut();
                for d in drives.iter_mut() {
                    if d.system {
                        let delta = (a.rand01() - 0.45) * GB * 1.5;
                        d.used = (d.used + delta).clamp(d.total * 0.4, d.total * 0.85);
                    }
                }
            }
            a.refresh();
        });
    }

    app.refresh();

    ui.run()
}
