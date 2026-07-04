#!/bin/sh
# Container entrypoint: configure sshd for the requested auth mode and run it in
# the foreground on $SSH_PORT. The forced login command runs whatever $APP names.
set -eu

: "${APP:?APP is required — e.g. -e APP='/opt/app --flag' and mount the binary: -v ./prog:/opt/app:ro}"
APP_USER="${APP_USER:-app}"
PORT="${SSH_PORT:-2222}"

# Generate host keys if missing (persist /etc/ssh via a volume to keep them
# stable across container recreation).
ssh-keygen -A >/dev/null 2>&1 || true
mkdir -p /run/sshd

# sshd clears the environment, so hand the program to the forced command via a
# file rather than an env var.
printf '%s\n' "$APP" > /etc/ssh-runner.app
chmod 644 /etc/ssh-runner.app

{
    echo "PermitRootLogin no"
    echo "UsePAM no"
    echo "AllowUsers $APP_USER"
    echo "X11Forwarding no"
    echo "AllowTcpForwarding no"
    echo "AllowAgentForwarding no"
    echo "PermitTunnel no"

    if [ -n "${AUTHORIZED_KEYS:-}" ]; then
        # Key mode: passwordless for holders of the private key, nobody else.
        install -d -o "$APP_USER" -g "$APP_USER" -m 700 "/home/$APP_USER/.ssh"
        printf '%s\n' "$AUTHORIZED_KEYS" > "/home/$APP_USER/.ssh/authorized_keys"
        chown "$APP_USER:$APP_USER" "/home/$APP_USER/.ssh/authorized_keys"
        chmod 600 "/home/$APP_USER/.ssh/authorized_keys"
        echo "PasswordAuthentication no"
        echo "PubkeyAuthentication yes"
        MODE="public-key"
    else
        # Anonymous mode: empty password, accepted during the initial "none"
        # auth probe, so the client is logged in with no prompt at all.
        passwd -d "$APP_USER" >/dev/null 2>&1 || true
        echo "PasswordAuthentication yes"
        echo "PermitEmptyPasswords yes"
        echo "KbdInteractiveAuthentication no"
        MODE="anonymous (NO AUTH)"
    fi

    echo "Match User $APP_USER"
    echo "    ForceCommand /usr/local/bin/run-login"
    echo "    PermitTTY yes"
} > /etc/ssh/sshd_config.d/runner.conf

echo "ssh-runner: mode=$MODE  port=$PORT  user=$APP_USER  app='$APP'" >&2
exec /usr/sbin/sshd -D -e -p "$PORT"
