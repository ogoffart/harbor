# ssh-runner — run any program over SSH

A small, **app-agnostic** container: it runs `sshd` on a port you choose, and a
connection is dropped straight into a program you specify at run time — no shell,
and by default **no password** (you're logged in automatically and the program
starts). Ideal for serving a terminal/TUI program (e.g. Harbor's sixel/Kitty
backend) to anyone who can reach the port.

The program is **not** baked into the image — mount your (statically built)
binary and name it with `-e APP=`.

## Build

```sh
docker build -t ssh-runner deploy/ssh-runner
```

## Run

```sh
docker run -d --name app \
    -p 2222:2222 \
    -v /path/to/your/static-binary:/opt/app:ro \
    -e APP='/opt/app --tui' \
    -v runner-keys:/etc/ssh \
    ssh-runner
```

Then, from anywhere:

```sh
ssh -p 2222 app@your-host
```

No password, no key setup — you're logged straight in and the program runs.
Quit the program → the session closes.

- **`APP`** (required) — the command to run, arguments included (whitespace-split
  by the shell), e.g. `-e APP='/opt/app --tui'`.
- **Mount the binary** at the path `APP` points to (`-v host-bin:/opt/app:ro`).
  A statically linked binary needs nothing else; if your program reads other
  files (fonts, data), mount those too.
- **Port** — `-p HOST:2222`, or change the in-container port with
  `-e SSH_PORT=2200 -p 2200:2200`.
- **Host keys** — the `runner-keys` volume persists `/etc/ssh` so the server
  identity is stable across restarts (no client "host key changed" warning).

### IPv6

On a dual-stack Docker host, `-p 2222:2222` usually binds both families; to bind
IPv6 explicitly use `-p '[::]:2222:2222'` (requires the daemon's `ipv6` /
`ip6tables` support). Then `ssh -p 2222 app@[your:v6::addr]`.

## Auth modes

| Mode | How | When |
|---|---|---|
| **Anonymous** (default) | empty-password account; the SSH `none` method accepts it, so there's no prompt | demos, kiosks, trusted networks |
| **Public key** | set `-e AUTHORIZED_KEYS="$(cat ~/.ssh/id_ed25519.pub)"` | anything exposed beyond a trusted network |

> ⚠️ **Anonymous mode is unauthenticated**: *anyone* who can reach the port is
> logged in and runs the program. It's confined — a forced command (no shell),
> no TCP/agent/X11 forwarding, and the container's own isolation — but still
> treat it like a public kiosk: bind it to a trusted network or firewall the
> port, and mount only what the program needs. For anything internet-facing, set
> `AUTHORIZED_KEYS` (that's still passwordless for you, just not for everyone).

## Example: serve Harbor

Harbor's terminal backend renders images, so build it and run it here. Easiest is
to reuse the binary from Harbor's own image, or build a portable one and mount it:

```sh
# build the terminal-only Harbor binary (see ../README.md), then:
docker run -d -p 2222:2222 \
    -v "$PWD/target/release/harbor:/opt/app:ro" \
    -e APP='/opt/app --tui' \
    ssh-runner
ssh -p 2222 app@your-host          # Harbor opens in your terminal
```

Your local terminal must support sixel or the Kitty graphics protocol; SSH
forwards its pixel size automatically. Force a protocol if needed with
`-e APP='/opt/app --tui'` and adding `HARBOR_IMAGE_PROTOCOL` via a wrapper, or
just bake it into `APP`: `-e APP='env HARBOR_IMAGE_PROTOCOL=kitty /opt/app --tui'`.

> Note: this image could not be built in the authoring sandbox (its network
> policy blocks Docker Hub), so build it in your own environment. The sshd
> configuration is standard OpenSSH.
