# Changelog

## [0.7.0](https://github.com/acidtib/jiji/compare/jiji-proxy-v0.6.2...jiji-proxy-v0.7.0) (2026-10-29)


### ⚠ BREAKING CHANGES

* **proxy:** set X-Forwarded-* from the connecting peer instead of the client ([#133](https://github.com/acidtib/jiji/issues/133))

### Bug Fixes

* **proxy:** set X-Forwarded-* from the connecting peer instead of the client ([#133](https://github.com/acidtib/jiji/issues/133)) ([76a8581](https://github.com/acidtib/jiji/commit/76a85811ca8581aa94114856f9f7ccefc8d37aae))

## [0.6.2](https://github.com/acidtib/jiji/compare/jiji-proxy-v0.6.1...jiji-proxy-v0.6.2) (2026-10-27)


### Bug Fixes

* remediate dependency advisories and clear ssh warnings ([#121](https://github.com/acidtib/jiji/issues/121)) ([e4e8630](https://github.com/acidtib/jiji/commit/e4e8630ca7c89d711823b38d73f975183505dabc))

## [0.6.1](https://github.com/acidtib/jiji/compare/jiji-proxy-v0.6.0...jiji-proxy-v0.6.1) (2026-08-10)


### Bug Fixes

* harden server setup and static TLS deployment ([2ad8220](https://github.com/acidtib/jiji/commit/2ad8220135106579f17b488759da256a1af6c66b))

## [0.6.0](https://github.com/acidtib/jiji/compare/jiji-proxy-v0.5.0...jiji-proxy-v0.6.0) (2026-08-09)


### Features

* add scheduled service cron jobs ([#82](https://github.com/acidtib/jiji/issues/82)) ([bb9e771](https://github.com/acidtib/jiji/commit/bb9e771e6710b7c2537ba96276ab3feb719e44f8))

## [0.5.0](https://github.com/acidtib/jiji/compare/jiji-proxy-v0.4.9...jiji-proxy-v0.5.0) (2026-08-08)


### Features

* rewrite jiji in Rust with agent-based control plane ([#66](https://github.com/acidtib/jiji/issues/66)) ([f7a1984](https://github.com/acidtib/jiji/commit/f7a19848954c3a3e6e33f154d9a3abc870ceb2ce))
