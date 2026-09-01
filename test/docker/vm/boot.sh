#!/bin/bash
set -e

# Defense in depth alongside compose's `depends_on: sshkeys: condition: service_healthy`: don't
# hand off to systemd (which starts sshd immediately) until the shared authorized_keys target
# actually exists, so a real SSH client never races a broken symlink.
while [ ! -f /shared/ssh/id_ed25519.pub ]; do
    echo "Waiting for shared SSH key..."
    sleep 1
done

exec /lib/systemd/systemd
