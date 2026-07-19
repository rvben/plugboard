# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

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
