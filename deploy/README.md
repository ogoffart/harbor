# Serving Harbor over SSH

Run Harbor as the "shell" for an SSH user: connecting drops straight into the
terminal UI (sixel / Kitty graphics), sandboxed to a single directory with no
network access. Rendering happens on the server; the frames travel over the SSH
channel and are drawn by **your** terminal, so your local client must support
sixel or the Kitty graphics protocol (WezTerm, kitty, Ghostty, foot, Konsole,
iTerm2, …). SSH forwards the terminal's pixel size, so output is crisp and the
mouse maps correctly.

There are three ways to set this up:

- **[Option A — bare metal](#option-a--bare-metal-sshd--bubblewrap)**: your host's
  `sshd` plus a `bubblewrap` sandbox.
- **[Option B — Docker](#option-b--docker)**: a self-contained Harbor image running
  its own `sshd` on a port you choose. The container *is* the sandbox.
- **[Option C — generic `ssh-runner`](#option-c--generic-ssh-runner-any-program-no-password)**:
  an app-agnostic container that runs *any* program you mount, logging in with no
  password by default.

---

## Option A — bare metal (sshd + bubblewrap)

> Requires a Linux host with OpenSSH and [`bubblewrap`](https://github.com/containers/bubblewrap)
> (`bwrap`). `sudo apt install bubblewrap` / `dnf install bubblewrap`.

### 1. Build and install the binary

`--no-default-features` builds the terminal-only binary (no winit/femtovg, so no
X11/OpenGL build or runtime dependencies):

```sh
cargo build --release --no-default-features
sudo install -Dm755 target/release/harbor /usr/local/bin/harbor
sudo install -Dm755 deploy/harbor-login  /usr/local/bin/harbor-login
```

### 2. Create the dedicated user and its data directory

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

### 3. Wire it into sshd

```sh
sudo install -m644 deploy/sshd_harbor.conf /etc/ssh/sshd_config.d/harbor.conf
sudo sshd -t && sudo systemctl reload ssh
```

`ForceCommand` runs `harbor-login` no matter what the client asks for, so
`ssh harbor@host /bin/sh` cannot get a shell.

### 4. Connect

```sh
ssh harbor@your-host
```

No command → sshd allocates a PTY and launches Harbor. **Ctrl-C** / **Ctrl-Q**
quit and close the session. Force a protocol with
`HARBOR_IMAGE_PROTOCOL=kitty|sixel` if auto-detection guesses wrong (set it in
`harbor-login`, since SSH won't forward arbitrary env vars).

### What the sandbox allows

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

### Hardening / limits (optional)

Each connection runs a render loop, so consider capping resources — e.g. launch
under a transient systemd scope from `harbor-login`:

```sh
exec systemd-run --quiet --scope -p CPUQuota=50% -p MemoryMax=256M \
    bwrap … /usr/local/bin/harbor
```

and add `MaxStartups`, `LoginGraceTime`, and per-key `restrict` options in
`authorized_keys`. Stronger isolation is available by swapping `bwrap` for a
container (`podman run --rm -i --network=none -v /srv/harbor/data:/data …`).

---

## Option B — Docker

A self-contained image that runs its own `sshd` on a port you choose. See
[`Dockerfile`](Dockerfile) — it builds the terminal-only binary
(`--no-default-features`) and ships it with `sshd`, fonts, and the launcher. Here
the **container is the sandbox**: only the mounted data volume is writable and
the app has no network features, so `bubblewrap` is skipped inside it.

### Build

```sh
docker build -t harbor-ssh -f deploy/Dockerfile .
```

### Run (pick your port)

```sh
docker run -d --name harbor \
    -p 2222:2222 \
    -e AUTHORIZED_KEYS="$(cat ~/.ssh/id_ed25519.pub)" \
    -v harbor-keys:/etc/ssh \
    -v harbor-data:/srv/harbor/data \
    harbor-ssh
```

- **Port**: `-p HOST:2222` maps a host port to the container's sshd. To change
  the in-container port too, add `-e SSH_PORT=2200 -p 2200:2200`.
- **IPv6**: on a dual-stack Docker host, `-p 2222:2222` binds both families;
  to bind IPv6 explicitly use `-p '[::]:2222:2222'` (requires the daemon's
  `ipv6`/`ip6tables` support enabled).
- **Keys**: pass `AUTHORIZED_KEYS` inline (above) or mount a file at
  `/srv/harbor/.ssh/authorized_keys`.
- **Host keys**: the `harbor-keys` volume persists `/etc/ssh` so the server
  identity is stable across `docker run`s (no client "host key changed" warnings).
- **Data**: `harbor-data` is the only writable directory the app sees
  (`/srv/harbor/data`); mount a host path instead if you prefer.

### Connect

```sh
ssh -p 2222 harbor@your-host
```

No command → a PTY is allocated and Harbor launches. Your local terminal must
support sixel or the Kitty graphics protocol; SSH forwards its pixel size for
crisp output. **Ctrl-C** / **Ctrl-Q** quit. Force a protocol with
`-e HARBOR_IMAGE_PROTOCOL=kitty|sixel` on `docker run` if detection guesses wrong.

> Note: this Dockerfile could not be built in the authoring sandbox (its network
> policy blocks Docker Hub and the Slint git dependency), so build it in your own
> environment. The `sshd`/`ForceCommand` wiring matches the bare-metal setup
> above, which was verified end to end.

---

## Option C — generic `ssh-runner` (any program, no password)

If you don't want a Harbor-specific image, [`ssh-runner/`](ssh-runner/) is an
app-agnostic container: it runs `sshd` on a port you choose and drops each
connection straight into a program you point it at with `-e APP=`, logging the
user in automatically with **no password** by default. Mount your (statically
built) binary and go:

```sh
docker build -t ssh-runner deploy/ssh-runner
docker run -d -p 2222:2222 -v /path/to/prog:/opt/app:ro -e APP='/opt/app --tui' ssh-runner
ssh -p 2222 app@your-host
```

See [`ssh-runner/README.md`](ssh-runner/README.md) for auth modes (anonymous vs.
key), IPv6, and the security note.
