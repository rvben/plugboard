# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

## [0.2.1](https://github.com/rvben/plugboard/compare/v0.2.0...v0.2.1) - 2026-07-21

### Added

- **ui**: fleet-wide power moves behind a menu, and confirms name their blast radius ([d67262d](https://github.com/rvben/plugboard/commit/d67262d76cfe0724b8af6d8be4e786e5d11c538c))
- organize the fleet into groups with live subtotals and per-group power ([0c1096f](https://github.com/rvben/plugboard/commit/0c1096f09c1b44a1a4dcf69a6adef0e77cca9fbb))
- Update all, opt-in auto-apply, and a native-submit safety net ([e47d343](https://github.com/rvben/plugboard/commit/e47d343847f790ebb2fc6ad52f053f66ec0fc913))
- the firmware update follows its real lifecycle in the callout ([2493465](https://github.com/rvben/plugboard/commit/2493465aa4bb99f58204e61c84d5e7e1f49818a3))
- **ui**: styled error pages, scan progress, and remaining rough-edge polish ([d295663](https://github.com/rvben/plugboard/commit/d2956631742a08532499c63d740c3c8b9d68349e))
- **ui**: instant tooltip on the update dot, and a composed firmware callout ([894c43a](https://github.com/rvben/plugboard/commit/894c43afc0e3bd3e11b7a1ea00d9eba2747498f8))
- **ui**: update availability as an ambient dot on cards, a jump-link on detail ([b6898a8](https://github.com/rvben/plugboard/commit/b6898a8095ff93edfecd6855ba7fac22cfd58996))
- chart axis labels with hover readout, and automatic firmware update discovery ([6e5abad](https://github.com/rvben/plugboard/commit/6e5abad11c35e3524295f055e7ec75c0140d1279))
- **ui**: live power history, instrument heroes, per-relay switches, and a console terminal ([4fd9696](https://github.com/rvben/plugboard/commit/4fd9696149816754f2eb179e1682cc803d8b9186))
- **ui**: redesign the web UI as a coherent instrument panel ([4922c30](https://github.com/rvben/plugboard/commit/4922c30ad7a8b2a6f796fc16a9f8c6570a390237))

### Fixed

- **ui**: toasts name what they switched ([eb13729](https://github.com/rvben/plugboard/commit/eb13729173f025ee9c1f3302c1c288d7a2f6e7a8))
- **auth**: answer logout with hx-redirect so the htmx sign-out button navigates ([ab7a564](https://github.com/rvben/plugboard/commit/ab7a56474c858157d6c94da28a718f0be6dd54a6))



## [0.2.0](https://github.com/rvben/plugboard/compare/v0.1.2...v0.2.0) - 2026-07-20

### Changed

- **BREAKING**: renamed the crate, binary, and config file from `tasmota-web`
  (`tasmota-web.toml`) to `plugboard` (`plugboard.toml`), the multi-vendor
  successor supporting both Tasmota and Shelly devices. Metric names moved
  from `tasmota_web_*` to `plugboard_*` in a prior change; this rename covers
  everything else (crate/lib/bin names, CLI name, tracing target, UI brand,
  Docker image user/paths).


## [0.1.2](https://github.com/rvben/tasmota-web/compare/v0.1.1...v0.1.2) - 2026-07-19

### Added

- **metrics**: add honest Prometheus /metrics exporter ([0bce98f](https://github.com/rvben/tasmota-web/commit/0bce98f99f82d5b801d42d2aed850e7856ff7862))

### Fixed

- **metrics**: avoid fabricated timestamps, prune orphaned counters ([86438e6](https://github.com/rvben/tasmota-web/commit/86438e677ff1aba879e1f664519902d9ab25cabf))

## [0.1.1](https://github.com/rvben/tasmota-web/compare/v0.1.0...v0.1.1) - 2026-07-19

### Added

- render Wi-Fi RSSI as a signal-strength indicator ([d2c6445](https://github.com/rvben/tasmota-web/commit/d2c6445c2f316f8936ca6cd9463304410e75d7be))

## [0.1.0] - 2026-07-19

### Added

- cohesive responsive design system ([6551873](https://github.com/rvben/tasmota-web/commit/65518732e6baccf56ab642bd240ab70282faa509))
- optional built-in login (argon2, rate-limited, secure cookie) ([7e4cc1e](https://github.com/rvben/tasmota-web/commit/7e4cc1ea6da9294983df689727497ece17753193))
- settings page ([e885b23](https://github.com/rvben/tasmota-web/commit/e885b230df1532210115dc07cf11a0db60867f2d))
- device discovery ([746ebe0](https://github.com/rvben/tasmota-web/commit/746ebe08df67575603433efd8fa0486fab9692bd))
- per-device admin panel with reused guardrails ([1c4c2c2](https://github.com/rvben/tasmota-web/commit/1c4c2c26259b6a0f103255e3fd5b0817f8da9759))
- device detail page ([ce1cf7f](https://github.com/rvben/tasmota-web/commit/ce1cf7fb42d0569746b6e527eb6b8e9ee62d38a8))
- bulk all-on/off with confirm and partial-failure reporting ([b9cb683](https://github.com/rvben/tasmota-web/commit/b9cb683a1cc44c68760abacc79af31df2c71d383))
- relay toggle with confirmed-state card, undo toast, protected confirm ([21a14d2](https://github.com/rvben/tasmota-web/commit/21a14d2e46e8440cbc1b26c9fbe9048e85b7d541))
- session-bound CSRF token + same-origin enforcement on writes ([c1ea1b6](https://github.com/rvben/tasmota-web/commit/c1ea1b6d65826343b40301ae987ad8f410dcbb08))
- embedded assets and SSE live updates ([49b089f](https://github.com/rvben/tasmota-web/commit/49b089fc88459d6ab8042095ca8f01fb826c70c8))
- dashboard page with live device cards ([363689b](https://github.com/rvben/tasmota-web/commit/363689b0ec5e4b9c148043f7a593d385695bba98))
- async core wrappers and background poller ([d8894d5](https://github.com/rvben/tasmota-web/commit/d8894d5801c4689ec8302c14b68d1527b3ffcfe2))
- fleet model and shared app state ([9c791cb](https://github.com/rvben/tasmota-web/commit/9c791cb59d51b026711eee424dcaae354489c05f))
- scaffold tasmota-web (config + axum server) ([2b148eb](https://github.com/rvben/tasmota-web/commit/2b148eb2415a326199e8d91446023aa7beaff244))

### Fixed

- **security**: scrub device credentials from error responses, logs, and stored state ([f3ac5f1](https://github.com/rvben/tasmota-web/commit/f3ac5f1340b7ac766ac3428f3c26cf4fbc75a3cd))
- **discover**: roll back ghost device on config save failure ([beaa9c2](https://github.com/rvben/tasmota-web/commit/beaa9c281c031adb61c522ac3becbd9a98f6f54d))
- **auth**: close CSRF Origin==Host test gap and harden origin parsing ([966189d](https://github.com/rvben/tasmota-web/commit/966189d0dead6d39be01e4b2fb029aa9e1ddce4a))
- **poller**: mark devices offline when their poll task panics ([73c3211](https://github.com/rvben/tasmota-web/commit/73c321191d0de314b30c04b6559f206d8397c1b5))
