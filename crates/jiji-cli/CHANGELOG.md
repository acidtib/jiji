# Changelog

## [0.10.0](https://github.com/acidtib/jiji/compare/v0.9.0...v0.10.0) (2026-08-15)


### ⚠ BREAKING CHANGES

* make servers a literal deploy target list and replace replicas/placement with per-server scale ([#98](https://github.com/acidtib/jiji/issues/98))

### Features

* add mounted build secrets support ([d6ccab4](https://github.com/acidtib/jiji/commit/d6ccab4e738cf0547784df7239e0400e3efee32f))
* complete audit coverage and harden validation and distribution ([f350693](https://github.com/acidtib/jiji/commit/f350693ea8723e90b5056903b1620bed2dd42c26))
* implement network_mode: host, reject network_mode: none ([#97](https://github.com/acidtib/jiji/issues/97)) ([75f34c7](https://github.com/acidtib/jiji/commit/75f34c725411d5f4509e2461f479148870ed97e4))
* make servers a literal deploy target list and replace replicas/placement with per-server scale ([#98](https://github.com/acidtib/jiji/issues/98)) ([beba101](https://github.com/acidtib/jiji/commit/beba101b243c28edf0e4ac054999e29165a07aa2))

## [0.9.0](https://github.com/acidtib/jiji/compare/v0.8.0...v0.9.0) (2026-08-13)


### Features

* **jiji-cli:** add jiji server upgrade command ([b342d8c](https://github.com/acidtib/jiji/commit/b342d8c3f8954a2d8690b7fcf5685853ec7ac2db))
* **jiji-cli:** add jiji update command ([df2e456](https://github.com/acidtib/jiji/commit/df2e4568cfeb9b8e88028ebb7bd06d0f60f3d13a))
* **jiji-cli:** stream health-check output during a deploy wait ([c382dd5](https://github.com/acidtib/jiji/commit/c382dd5bdfaeb378801c6117900361b54f0d2d75))


### Bug Fixes

* **cli:** prune orphaned images left by a moving image tag ([62793c4](https://github.com/acidtib/jiji/commit/62793c428882c84b50909ac5228f6e6937661f54))
* **config:** default build context to project root ([409ab61](https://github.com/acidtib/jiji/commit/409ab61ead08e261ede0f9e77e16caf1606391b0))
* harden update, server upgrade, and image retention ([af5f219](https://github.com/acidtib/jiji/commit/af5f21901f04f6273b19820ad78e93ad927cb446))

## [0.8.0](https://github.com/acidtib/jiji/compare/v0.7.5...v0.8.0) (2026-08-12)


### Features

* **jiji-tui:** add live multi-endpoint progress dashboards ([c9a4aae](https://github.com/acidtib/jiji/commit/c9a4aae6fccc1574a5bfe816c37c40a56dcb47e0))

## [0.7.5](https://github.com/acidtib/jiji/compare/v0.7.4...v0.7.5) (2026-08-11)


### Bug Fixes

* **jiji-cli:** surface actionable setup hint when jiji-agent is missing during deploy ([78382b7](https://github.com/acidtib/jiji/commit/78382b713ee6c856b1fdb516271ddd39d563bd79))

## [0.7.4](https://github.com/acidtib/jiji/compare/v0.7.3...v0.7.4) (2026-08-10)


### Bug Fixes

* prevent proxy restart lock inheritance ([a98b4a4](https://github.com/acidtib/jiji/commit/a98b4a4c245a3bc5d894a4b12c398bfd2606dfe6))

## [0.7.3](https://github.com/acidtib/jiji/compare/v0.7.2...v0.7.3) (2026-08-10)

## [0.7.2](https://github.com/acidtib/jiji/compare/v0.7.1...v0.7.2) (2026-08-10)


### Bug Fixes

* **jiji-cli:** resolve server hosts from environment ([a9c2f9f](https://github.com/acidtib/jiji/commit/a9c2f9fb901444c80f967557a9c3e64e6f40569c))
* stabilize builds and proxy route updates ([4547010](https://github.com/acidtib/jiji/commit/4547010b822ac7d2be1795e5b432750f458468dc))

## [0.7.1](https://github.com/acidtib/jiji/compare/v0.7.0...v0.7.1) (2026-08-10)


### Bug Fixes

* harden server setup and static TLS deployment ([2ad8220](https://github.com/acidtib/jiji/commit/2ad8220135106579f17b488759da256a1af6c66b))
* **jiji-cli:** support Ubuntu WireGuard confinement ([2c02196](https://github.com/acidtib/jiji/commit/2c02196bf24802f32406fefb57c11bc9977171a2))

## [0.7.0](https://github.com/acidtib/jiji/compare/v0.6.0...v0.7.0) (2026-08-09)


### Features

* infer builder, registry, and network defaults ([#85](https://github.com/acidtib/jiji/issues/85)) ([e089f46](https://github.com/acidtib/jiji/commit/e089f46b31d7fb1ec9088f086b6633eafafe3eab))

## [0.6.0](https://github.com/acidtib/jiji/compare/v0.5.2...v0.6.0) (2026-08-09)


### Features

* add scheduled service cron jobs ([#82](https://github.com/acidtib/jiji/issues/82)) ([bb9e771](https://github.com/acidtib/jiji/commit/bb9e771e6710b7c2537ba96276ab3feb719e44f8))

## [0.5.2](https://github.com/acidtib/jiji/compare/v0.5.1...v0.5.2) (2026-08-08)

## [0.5.1](https://github.com/acidtib/jiji/compare/v0.5.0...v0.5.1) (2026-08-08)

## [0.5.0](https://github.com/acidtib/jiji/compare/v0.4.9...v0.5.0) (2026-08-08)


### Features

* rewrite jiji in Rust with agent-based control plane ([#66](https://github.com/acidtib/jiji/issues/66)) ([f7a1984](https://github.com/acidtib/jiji/commit/f7a19848954c3a3e6e33f154d9a3abc870ceb2ce))
