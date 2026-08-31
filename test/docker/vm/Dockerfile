FROM ubuntu:24.04

# Deliberately minimal: openssh-server (so jiji can reach it), systemd (so a real
# `jiji-agent-*.service` unit can actually start), iproute2 (a bare `ubuntu:24.04` image ships
# without it, unlike a real cloud droplet image -- `ip link`/`wg` need it). Everything jiji itself
# needs on a target host (podman, wireguard-tools, nftables, dnsutils, ...) is installed by the
# real `jiji server setup` run under test, not baked in here -- that install path is what this
# suite exists to exercise.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        openssh-server systemd systemd-sysv iproute2 ca-certificates sudo && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /root/.ssh && \
    ln -s /shared/ssh/id_ed25519.pub /root/.ssh/authorized_keys && \
    mkdir -p /etc/ssh/sshd_config.d && \
    printf 'PermitRootLogin prohibit-password\n' > /etc/ssh/sshd_config.d/50-jiji-test.conf && \
    # Boot straight to a normal multi-user target; sshd is started by boot.sh once the shared
    # key volume is ready, not by systemd itself, since compose starts vm/sshkeys concurrently.
    systemctl mask systemd-udev-trigger.service systemd-udevd.service getty@tty1.service && \
    systemctl enable ssh && \
    ssh-keygen -A

WORKDIR /work
COPY boot.sh .
RUN chmod +x boot.sh

HEALTHCHECK --interval=2s --start-period=30s CMD systemctl is-system-running --wait || systemctl is-system-running | grep -qE '^(running|degraded)$'

STOPSIGNAL SIGRTMIN+3
CMD ["/work/boot.sh"]
