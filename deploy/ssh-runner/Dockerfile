# syntax=docker/dockerfile:1
#
# Generic "run a program over SSH" container.
#
# It runs an sshd on a port you choose; connecting drops straight into whatever
# program you point it at — no shell, and (by default) no password. The program
# is NOT baked in: mount your (statically built) binary and name it at run time.
#
#   docker build -t ssh-runner deploy/ssh-runner
#   docker run -d -p 2222:2222 \
#       -v /path/to/prog:/opt/app:ro \
#       -e APP='/opt/app --some-flag' \
#       ssh-runner
#   ssh -p 2222 app@your-host        # no password → runs the program
#
# See README.md in this directory for auth modes (anonymous vs. key), IPv6, and
# the security note.

FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      openssh-server ca-certificates fontconfig fonts-dejavu-core \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -d /home/app -s /bin/sh app \
 && mkdir -p /run/sshd

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
COPY run-login     /usr/local/bin/run-login
RUN chmod +x /usr/local/bin/entrypoint.sh /usr/local/bin/run-login

# APP is required at run time (the command to execute). SSH_PORT / APP_USER are
# optional overrides.
ENV SSH_PORT=2222 APP_USER=app APP=""
EXPOSE 2222
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
