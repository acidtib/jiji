# Changelog

## [0.9.0](https://github.com/acidtib/jiji/compare/jiji-config-v0.8.2...jiji-config-v0.9.0) (2026-08-15)


### ⚠ BREAKING CHANGES

* make servers a literal deploy target list and replace replicas/placement with per-server scale ([#98](https://github.com/acidtib/jiji/issues/98))

### Features

* add mounted build secrets support ([d6ccab4](https://github.com/acidtib/jiji/commit/d6ccab4e738cf0547784df7239e0400e3efee32f))
* complete audit coverage and harden validation and distribution ([f350693](https://github.com/acidtib/jiji/commit/f350693ea8723e90b5056903b1620bed2dd42c26))
* implement network_mode: host, reject network_mode: none ([#97](https://github.com/acidtib/jiji/issues/97)) ([75f34c7](https://github.com/acidtib/jiji/commit/75f34c725411d5f4509e2461f479148870ed97e4))
* make servers a literal deploy target list and replace replicas/placement with per-server scale ([#98](https://github.com/acidtib/jiji/issues/98)) ([beba101](https://github.com/acidtib/jiji/commit/beba101b243c28edf0e4ac054999e29165a07aa2))

## [0.8.2](https://github.com/acidtib/jiji/compare/jiji-config-v0.8.1...jiji-config-v0.8.2) (2026-08-13)


### Bug Fixes

* **cli:** prune orphaned images left by a moving image tag ([62793c4](https://github.com/acidtib/jiji/commit/62793c428882c84b50909ac5228f6e6937661f54))
* **config:** default build context to project root ([409ab61](https://github.com/acidtib/jiji/commit/409ab61ead08e261ede0f9e77e16caf1606391b0))

## [0.8.1](https://github.com/acidtib/jiji/compare/jiji-config-v0.8.0...jiji-config-v0.8.1) (2026-08-10)


### Bug Fixes

* **jiji-cli:** resolve server hosts from environment ([a9c2f9f](https://github.com/acidtib/jiji/commit/a9c2f9fb901444c80f967557a9c3e64e6f40569c))
* stabilize builds and proxy route updates ([4547010](https://github.com/acidtib/jiji/commit/4547010b822ac7d2be1795e5b432750f458468dc))

## [0.8.0](https://github.com/acidtib/jiji/compare/jiji-config-v0.7.0...jiji-config-v0.8.0) (2026-08-09)


### Features

* infer builder, registry, and network defaults ([#85](https://github.com/acidtib/jiji/issues/85)) ([e089f46](https://github.com/acidtib/jiji/commit/e089f46b31d7fb1ec9088f086b6633eafafe3eab))

## [0.7.0](https://github.com/acidtib/jiji/compare/jiji-config-v0.6.0...jiji-config-v0.7.0) (2026-08-09)


### Features

* add scheduled service cron jobs ([#82](https://github.com/acidtib/jiji/issues/82)) ([bb9e771](https://github.com/acidtib/jiji/commit/bb9e771e6710b7c2537ba96276ab3feb719e44f8))

## [0.6.0](https://github.com/acidtib/jiji/compare/jiji-config-v0.5.0...jiji-config-v0.6.0) (2026-08-08)


### Features

* rewrite jiji in Rust with agent-based control plane ([#66](https://github.com/acidtib/jiji/issues/66)) ([f7a1984](https://github.com/acidtib/jiji/commit/f7a19848954c3a3e6e33f154d9a3abc870ceb2ce))


### Bug Fixes

* **release:** stop release-please version-bump loop ([#72](https://github.com/acidtib/jiji/issues/72)) ([7974111](https://github.com/acidtib/jiji/commit/797411143673e9668e93d1d3c0b837b241ded35b))

## [0.5.0](https://github.com/acidtib/jiji/compare/jiji-config-v0.4.9...jiji-config-v0.5.0) (2026-08-08)


### Features

* rewrite jiji in Rust with agent-based control plane ([#66](https://github.com/acidtib/jiji/issues/66)) ([f7a1984](https://github.com/acidtib/jiji/commit/f7a19848954c3a3e6e33f154d9a3abc870ceb2ce))
