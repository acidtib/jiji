# TODO: Installing a recent Podman version

`jiji server setup`'s engine installer (`crates/jiji-cli/src/engine.rs`)
currently installs Podman via plain `apt-get install podman` on Debian/Ubuntu
and `dnf install podman` on Fedora/CentOS/RHEL, with `PODMAN_MIN_VERSION`
set to `4.9.3` — the version Ubuntu 24.04's own apt repos actually ship.
This is well behind current upstream Podman releases (6.x).

Researched during the `server setup` smoke test against a real Ubuntu 24.04
VPS (2026-07-22) and confirmed apt is capped at 4.9.3 there
(`apt-cache policy podman`). Options considered for reaching a newer version:

1. **Kubic OBS repo** (`devel:kubic:libcontainers:stable`) — the historical
   go-to for newer Podman on Debian/Ubuntu. Confirmed discontinued; no longer
   ships Podman packages at all.
2. **Build from source** — needs a Go toolchain plus `libseccomp-dev`,
   `libgpgme-dev`, etc., and manual wiring of `crun`/`conmon`/`netavark`/
   `aardvark-dns`. Heavy and fragile to automate reliably across distros.
3. **Official static Linux binaries from upstream** — checked the
   `podman-container-tools/podman` (formerly `containers/podman`) GitHub
   releases directly. The only Linux assets are `podman-remote-static-*.tar.gz`
   — the **remote client only**, no local container runtime. Doesn't give a
   working local `podman run`.
4. **`mgoltzsche/podman-static`** — a real, current, static bundle (podman +
   crun/conmon/netavark/aardvark-dns) that does provide a working local
   podman 6.x. But it's an **unofficial, single-maintainer GitHub project**,
   not the Podman project or a distro/vendor repo — a materially different
   supply-chain trust decision than pulling from docker.com's own apt repo
   (which is what the Docker install path does). Explicitly not wired in
   without a deliberate decision to accept that trust boundary.

Decision so far: keep `PODMAN_MIN_VERSION` at what apt can actually deliver
rather than silently failing on stock installs. Revisit if/when there's an
official (Podman-project- or distro-blessed) way to get a newer version, or
if the third-party static-binary route above is deliberately accepted.
