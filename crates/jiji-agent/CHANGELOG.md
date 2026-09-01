# Changelog

## [0.7.1](https://github.com/acidtib/jiji/compare/jiji-agent-v0.7.0...jiji-agent-v0.7.1) (2026-09-01)


### Bug Fixes

* verify candidate health before trusting a recovered deploy ([#99](https://github.com/acidtib/jiji/issues/99)) ([805b507](https://github.com/acidtib/jiji/commit/805b507fe3ca1574d403e74eab662f37359f9dcb))

## [0.7.0](https://github.com/acidtib/jiji/compare/jiji-agent-v0.6.5...jiji-agent-v0.7.0) (2026-08-15)


### ⚠ BREAKING CHANGES

* make servers a literal deploy target list and replace replicas/placement with per-server scale ([#98](https://github.com/acidtib/jiji/issues/98))

### Features

* complete audit coverage and harden validation and distribution ([f350693](https://github.com/acidtib/jiji/commit/f350693ea8723e90b5056903b1620bed2dd42c26))
* implement network_mode: host, reject network_mode: none ([#97](https://github.com/acidtib/jiji/issues/97)) ([75f34c7](https://github.com/acidtib/jiji/commit/75f34c725411d5f4509e2461f479148870ed97e4))
* make servers a literal deploy target list and replace replicas/placement with per-server scale ([#98](https://github.com/acidtib/jiji/issues/98)) ([beba101](https://github.com/acidtib/jiji/commit/beba101b243c28edf0e4ac054999e29165a07aa2))

## [0.6.5](https://github.com/acidtib/jiji/compare/jiji-agent-v0.6.4...jiji-agent-v0.6.5) (2026-08-13)


### Bug Fixes

* **cli:** prune orphaned images left by a moving image tag ([62793c4](https://github.com/acidtib/jiji/commit/62793c428882c84b50909ac5228f6e6937661f54))
* harden update, server upgrade, and image retention ([af5f219](https://github.com/acidtib/jiji/commit/af5f21901f04f6273b19820ad78e93ad927cb446))

## [0.6.4](https://github.com/acidtib/jiji/compare/jiji-agent-v0.6.3...jiji-agent-v0.6.4) (2026-08-10)


### Bug Fixes

* **jiji-agent:** repair legacy upgrade state ([a271e7c](https://github.com/acidtib/jiji/commit/a271e7c0c86bf6f81494aaa75287fd903a676f03))
* prevent proxy restart lock inheritance ([a98b4a4](https://github.com/acidtib/jiji/commit/a98b4a4c245a3bc5d894a4b12c398bfd2606dfe6))

## [0.6.3](https://github.com/acidtib/jiji/compare/jiji-agent-v0.6.2...jiji-agent-v0.6.3) (2026-08-10)


### Bug Fixes

* **jiji-agent:** release inherited proxy lease ([23ce8ec](https://github.com/acidtib/jiji/commit/23ce8ec73735dee8e5f73b78c44684c00302d8ec))

## [0.6.2](https://github.com/acidtib/jiji/compare/jiji-agent-v0.6.1...jiji-agent-v0.6.2) (2026-08-10)


### Bug Fixes

* stabilize builds and proxy route updates ([4547010](https://github.com/acidtib/jiji/commit/4547010b822ac7d2be1795e5b432750f458468dc))

## [0.6.1](https://github.com/acidtib/jiji/compare/jiji-agent-v0.6.0...jiji-agent-v0.6.1) (2026-08-09)

## [0.6.0](https://github.com/acidtib/jiji/compare/jiji-agent-v0.5.2...jiji-agent-v0.6.0) (2026-08-09)


### Features

* add scheduled service cron jobs ([#82](https://github.com/acidtib/jiji/issues/82)) ([bb9e771](https://github.com/acidtib/jiji/commit/bb9e771e6710b7c2537ba96276ab3feb719e44f8))

## [0.5.2](https://github.com/acidtib/jiji/compare/jiji-agent-v0.5.1...jiji-agent-v0.5.2) (2026-08-08)

## [0.5.1](https://github.com/acidtib/jiji/compare/jiji-agent-v0.5.0...jiji-agent-v0.5.1) (2026-08-08)

## [0.5.0](https://github.com/acidtib/jiji/compare/jiji-agent-v0.4.9...jiji-agent-v0.5.0) (2026-08-08)


### Features

* rewrite jiji in Rust with agent-based control plane ([#66](https://github.com/acidtib/jiji/issues/66)) ([f7a1984](https://github.com/acidtib/jiji/commit/f7a19848954c3a3e6e33f154d9a3abc870ceb2ce))
