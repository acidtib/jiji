# jiji-proxy

Traps specific to this crate. Root `AGENTS.md` rules still apply.

- **One proxy serves every project on a host.** A behavior change here reaches
  every project on that host the moment `jiji server upgrade` recreates the
  container. Treat default changes as breaking and say so in the PR.
- **Old proxies reject new flags.** `route apply` is parsed by clap, so a newer
  `jiji-cli` that sends an unknown flag fails against an un-upgraded host. New
  route flags must default off, and `jiji-cli` must emit them only when set.
  New `AdminRequest` fields need `#[serde(default)]`.
- **Backends see whatever headers the client sent** unless this crate rewrites
  them. Never trust or forward a client-supplied `X-Forwarded-*` header
  without deciding it on purpose.
- **Pingora only accepts services before `run_forever()`.** Runtime routes go
  through `RouteManager`'s shared tick loop, never as new background services.
- **Health checks here are HTTP/TCP only.** `healthcheck.cmd` belongs to
  `jiji-cli`'s pre-activation gate; this proxy cannot exec into backends on
  other hosts.
- **TLS is per host, not per path.** If sibling routes on one host disagree,
  any `tls: true` wins.
- **Keep the Tokio pin.** `socket_compatibility_tests` in `proxy.rs` guards a
  Tokio version that panics on every real connection under Pingora 0.9.
