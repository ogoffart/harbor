# Serving Harbor over SSH

Run Harbor as the "shell" for an SSH user: connecting drops straight into the
terminal UI (sixel / Kitty graphics), sandboxed to a single directory with no
network access. Rendering happens on the server; the frames travel over the SSH
channel and are drawn by **your** terminal, so your local client must support
sixel or the Kitty graphics protocol (WezTerm, kitty, Ghostty, foot, Konsole,
iTerm2, …). SSH forwards the terminal's pixel size, so output is crisp and the
mouse maps correctly.

> Requires a Linux host with OpenSSH and [`bubblewrap`](https://github.com/containers/bubblewrap)
> (`bwrap`). `sudo apt install bubblewrap` / `dnf install bubblewrap`.

## 1. Build and install the binary

```sh
cargo build --release
sudo install -Dm755 target/release/harbor /usr/local/bin/harbor
sudo install -Dm755 deploy/harbor-login  /usr/local/bin/harbor-login
```

## 2. Create the dedicated user and its data directory

```sh
sudo useradd -m -d /srv/harbor -s /usr/sbin/nologin harbor
sudo -u harbor mkdir -p /srv/harbor/data     # the ONLY directory the app can see
```

Add the public keys allowed to connect:

```sh
sudo install -d -o harbor -g harbor -m700 /srv/harbor/.ssh
sudo -u harbor tee /srv/harbor/.ssh/authorized_keys < your_key.pub
sudo -u harbor chmod 600 /srv/harbor/.ssh/authorized_keys
```

## 3. Wire it into sshd

```sh
sudo install -m644 deploy/sshd_harbor.conf /etc/ssh/sshd_config.d/harbor.conf
sudo sshd -t && sudo systemctl reload ssh
```

`ForceCommand` runs `harbor-login` no matter what the client asks for, so
`ssh harbor@host /bin/sh` cannot get a shell.

## 4. Connect

```sh
ssh harbor@your-host
```

No command → sshd allocates a PTY and launches Harbor. **Ctrl-C** / **Ctrl-Q**
quit and close the session. Force a protocol with
`HARBOR_IMAGE_PROTOCOL=kitty|sixel` if auto-detection guesses wrong (set it in
`harbor-login`, since SSH won't forward arbitrary env vars).

## What the sandbox allows

`harbor-login` uses `bwrap` to give each session:

- **only** `$HARBOR_DATA` (default `/srv/harbor/data`) as a writable path, mapped
  to `/data` — nothing else on the host filesystem is visible except read-only
  system libraries and fonts (needed to render text);
- **no network** (`--unshare-all` includes a fresh, empty network namespace);
- fresh PID/IPC/UTS namespaces, a private `/tmp`, and cleanup on disconnect
  (`--die-with-parent`).

Point it at a different directory with `HARBOR_DATA=/path HARBOR_BIN=/path/harbor`
in the environment, or edit the defaults at the top of `harbor-login`.

> Note: Harbor currently renders a **mock, in-memory** filesystem (`src/data.rs`),
> so today the sandbox is defense-in-depth around the process rather than the
> scope of a browsing feature. It's the right thing to have in place before
> wiring Harbor up to real files.

## Hardening / limits (optional)

Each connection runs a render loop, so consider capping resources — e.g. launch
under a transient systemd scope from `harbor-login`:

```sh
exec systemd-run --quiet --scope -p CPUQuota=50% -p MemoryMax=256M \
    bwrap … /usr/local/bin/harbor
```

and add `MaxStartups`, `LoginGraceTime`, and per-key `restrict` options in
`authorized_keys`. Stronger isolation is available by swapping `bwrap` for a
container (`podman run --rm -i --network=none -v /srv/harbor/data:/data …`).
