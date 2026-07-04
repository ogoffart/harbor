# Serve Harbor over SSH from a container.
#
# Build:  docker build -t harbor-ssh -f deploy/Dockerfile .
# Run:    docker run -d --name harbor -p 2222:2222 \
#             -e AUTHORIZED_KEYS="$(cat ~/.ssh/id_ed25519.pub)" \
#             -v harbor-data:/srv/harbor/data harbor-ssh
# Connect: ssh -p 2222 harbor@your-host
#
# The container itself is the sandbox boundary: only the mounted data volume is
# writable, and the app has no network features. Pick the listening port with
# `-e SSH_PORT=` (default 2222) and publish it with `-p`.

# syntax=docker/dockerfile:1

# ---- build stage: compile the terminal-only binary (no GUI/display deps) ----
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# --no-default-features drops the winit/femtovg desktop stack; the terminal
# backend only needs the software renderer.
RUN cargo build --release --no-default-features

# ---- runtime stage: sshd + fonts + the binary ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      openssh-server fontconfig fonts-dejavu-core \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -d /srv/harbor -s /usr/local/bin/harbor-login harbor \
 && mkdir -p /run/sshd /srv/harbor/data \
 && chown -R harbor:harbor /srv/harbor

COPY --from=build /src/target/release/harbor /usr/local/bin/harbor
COPY deploy/harbor-login          /usr/local/bin/harbor-login
COPY deploy/sshd_harbor.conf      /etc/ssh/sshd_config.d/harbor.conf
COPY deploy/docker-entrypoint.sh  /usr/local/bin/docker-entrypoint.sh
# The container already isolates the process, so skip the nested bwrap layer.
RUN chmod +x /usr/local/bin/harbor-login /usr/local/bin/docker-entrypoint.sh \
 && printf '    SetEnv HARBOR_NO_SANDBOX=1\n' >> /etc/ssh/sshd_config.d/harbor.conf

ENV SSH_PORT=2222
EXPOSE 2222
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
