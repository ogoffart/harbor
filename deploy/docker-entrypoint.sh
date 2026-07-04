#!/bin/sh
# Container entrypoint: prepare host keys + authorized_keys, then run sshd in the
# foreground on $SSH_PORT (default 2222).
set -eu

# Generate SSH host keys if none are present (persist /etc/ssh via a volume to
# keep them stable across container recreation and avoid client key warnings).
ssh-keygen -A >/dev/null 2>&1 || true

# Install the authorized key(s) either from $AUTHORIZED_KEYS or from a file
# mounted at /srv/harbor/.ssh/authorized_keys.
if [ -n "${AUTHORIZED_KEYS:-}" ]; then
    install -d -o harbor -g harbor -m 700 /srv/harbor/.ssh
    printf '%s\n' "$AUTHORIZED_KEYS" > /srv/harbor/.ssh/authorized_keys
    chown harbor:harbor /srv/harbor/.ssh/authorized_keys
    chmod 600 /srv/harbor/.ssh/authorized_keys
fi

if [ ! -s /srv/harbor/.ssh/authorized_keys ]; then
    echo "error: no authorized keys — pass -e AUTHORIZED_KEYS=... or mount" \
         "/srv/harbor/.ssh/authorized_keys" >&2
    exit 1
fi

mkdir -p /run/sshd
exec /usr/sbin/sshd -D -e -p "${SSH_PORT:-2222}"
