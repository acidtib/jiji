#!/bin/sh
set -e

mkdir -p /shared/ssh

# Idempotent: a repeated `up` on an already-populated volume must not rotate the key out from
# under a vm container that already trusts the old one.
if [ ! -f /shared/ssh/id_ed25519 ]; then
    ssh-keygen -t ed25519 -N "" -C jiji-docker-tests -f /shared/ssh/id_ed25519
fi

# The key is generated as root inside this container but read both by vm1's sshd and by the Rust
# test harness / a developer's `ssh` client running as an unprivileged host user (this container
# has no user namespace remapping, so root here is uid 0 on the host too). World-readable, not
# world-writable, so OpenSSH's own "unprotected private key" check still passes -- disposable,
# throwaway test-only credential, not a real secret.
chmod 644 /shared/ssh/id_ed25519
chmod 644 /shared/ssh/id_ed25519.pub

exec sleep infinity
