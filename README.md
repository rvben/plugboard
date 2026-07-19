# tasmota-web

An unofficial, phone-first web dashboard and admin for [Tasmota](https://tasmota.github.io/docs/)
smart devices: a live status grid, one-click on/off, energy readouts, discovery,
and a per-device admin panel (console, config, firmware), all in the browser.
Built with axum, maud, and htmx.

> This is a third-party tool. It is not affiliated with or endorsed by the
> Tasmota project. "Tasmota" is used only to describe compatibility.

## Screenshots

| Dashboard (light) | Device admin |
| --- | --- |
| ![Dashboard grid in light mode, showing a set of Tasmota device cards with live status](docs/screenshots/dashboard-light.png) | ![Per-device admin panel with console, config, and firmware controls](docs/screenshots/device-admin.png) |

![Dashboard in dark mode on a phone-sized viewport](docs/screenshots/dashboard-dark-mobile.png)

## What it is

`tasmota-web` is a single self-contained binary: a live dashboard plus
per-device admin, talking directly to each device over HTTP. No MQTT broker,
no database, and no cloud dependency, the same no-broker-required model as
the sibling `tasmota` CLI. CSS and JS are embedded in the binary (no separate
asset directory to deploy), and all device I/O reuses the published
`tasmota-core` library, so status parsing and safety guardrails match the CLI
exactly.

It is a sibling to the `tasmota` CLI: `tasmota-web` is the browser-based
counterpart for when you want a dashboard on your phone or a wall-mounted
tablet instead of a terminal.

## Features

- **Live dashboard grid** - every configured device as a card, updated over
  Server-Sent Events with no page reload.
- **One-click toggle** with a confirmed-state card and an undo toast.
- **Protected devices** (opt-in per device) require an extra confirmation
  before switching, for anything you don't want a stray tap to flip.
- **Bulk all-on / all-off**, with a confirmation step and a per-device
  success/failure summary.
- **Per-device detail**: relays, energy (power, voltage, current, today's/total
  kWh), firmware version, network info, and MQTT status.
- **Per-device admin panel**: console commands, config get/set, firmware
  check/update, and a config backup download, reusing the same destructive-command
  guardrails as the `tasmota` CLI (`tasmota_core::guardrail`), so a
  destructive or unclassifiable command always requires an explicit
  confirmation before it reaches the device.
- **Network discovery**: scan a CIDR range and add found devices to the
  config from the browser.
- **Settings**: manage the device list, per-device credentials, and the poll
  interval, all from the UI.
- **Optional built-in login** (argon2, rate-limited) for deployments that
  don't sit behind an auth-aware reverse proxy.

## Install

### Cargo

```sh
cargo install tasmota-web
```

Requires Rust 1.90 or newer (the crate's `rust-version`).

### Homebrew

```sh
brew install rvben/tap/tasmota-web
```

### Docker

No image is published to a registry; build it from this repo's `Dockerfile`:

```sh
docker build -t tasmota-web .
docker run -d \
  --name tasmota-web \
  -p 8088:8088 \
  -v /path/to/tasmota-web.toml:/etc/tasmota-web/tasmota-web.toml:ro \
  tasmota-web
```

The image reads its config from `/etc/tasmota-web/tasmota-web.toml` (the
container's default `CMD`) and listens on `8088`, the same default bind port
as a local run. Never bake device credentials into the image, only mount them
in at runtime.

## Configuration

`tasmota-web` reads a TOML config file (default path `./tasmota-web.toml`,
override with `--config`). A minimal file with no `[[devices]]` entries and
no `[auth]` section is valid, everything has a default, but you'll usually
want at least one device:

```toml
# Address tasmota-web listens on. Defaults to 127.0.0.1:8088 if omitted.
bind = "127.0.0.1:8088"

# How often (in seconds) the poller refreshes device status in the
# background. Defaults to 5.
poll_interval_secs = 5

[auth]
# "proxy" (default): trust a reverse proxy in front of this app to have
# already authenticated the request. "builtin": require a login against
# username/password_hash below. See "Authentication" for details.
mode = "proxy"
# Required only when mode = "builtin".
# username = "admin"
# password_hash = "$argon2id$..."
# Set the Secure flag on the session cookie. Leave true (the default) behind
# TLS or on http://localhost; set false ONLY for a trusted plain-http LAN
# deployment (documented as insecure, see "Authentication" below).
cookie_secure = true

[[devices]]
name = "Living Room Lamp"
host = "192.0.2.10"
# Set true to require an extra confirmation before toggling this device.
protected = false

[[devices]]
name = "Freezer"
host = "192.0.2.11"
protected = true
# Only needed if the device has a web/console password set.
password = "device-web-password"
```

Run it against that file:

```sh
tasmota-web --config /path/to/tasmota-web.toml
```

Devices can also be added, renamed, removed, and have their credentials or
`protected` flag changed from the Settings page in the running app, which
writes back to the same config file.

## Authentication

`tasmota-web` has two auth modes, set via `[auth] mode`:

- **`proxy`** (default): the app trusts that a reverse proxy in front of it
  (Authelia, or similar) has already authenticated the request. There is no
  built-in login; anyone who can reach the app can use it. This is the
  intended mode for a homelab deployment behind an authenticating proxy.
- **`builtin`**: a single admin login, handled by the app itself. Generate an
  argon2 password hash with the built-in subcommand:

  ```sh
  tasmota-web hash-password
  Password: <typed here, echoed>
  $argon2id$v=19$...
  ```

  or pipe it in non-interactively: `printf '%s' "$PW" | tasmota-web hash-password`.
  Paste the output into `[auth] password_hash`, set `[auth] username`, and set
  `[auth] mode = "builtin"`.

The session cookie is `Secure` by default (`cookie_secure = true`), which
works behind a TLS-terminating proxy and on `http://localhost` (browsers treat
localhost as a secure context either way). If you deploy `builtin` mode over
plain HTTP to a LAN IP (not `localhost`, no TLS), the browser will silently
refuse to store a `Secure` cookie and login will appear to fail; set
`cookie_secure = false` in that case, but treat it as insecure (the session
cookie then travels in the clear).

Regardless of mode: prefer binding to loopback or a private interface and
terminating TLS and, if you want it, authentication at a reverse proxy in
front of `tasmota-web`. `proxy` mode plus a proxy like Authelia is the
recommended setup; `builtin` mode exists for deployments that can't put an
authenticating proxy in front of the app.

## Data-honesty and safety

- Absent data (an offline device, a sensor a device doesn't have) always
  renders as `n/a`, never a coerced `0`. A missing reading, a device that
  doesn't report that field, and a genuine zero are different facts and are
  never collapsed into each other.
- `mqtt.connected` is hard-coded to `n/a`: the app talks to devices over
  HTTP only, so it has no way to know a device's live MQTT connection state,
  and the status page never guesses.
- An offline device is shown as offline, its last-known values are not
  reused to fake a live reading.
- Destructive operations (firmware update, config set, console commands
  classified as destructive or unclassifiable) require an explicit
  confirmation before anything reaches the device, using the same guardrail
  classification as the `tasmota` CLI.
- `restore` (uploading a `.dmp` config backup to a device) is intentionally
  not wired to a route: its endpoint hasn't been verified against real
  hardware yet.
- Device credentials (per-device passwords) stay server-side; they are read
  from the config file and never sent to the browser.
- Every write request is checked for a session-bound CSRF token and
  same-origin (`Sec-Fetch-Site` / `Origin`) before it is allowed through,
  regardless of auth mode.

## Build / develop

```sh
make check   # fmt --check, clippy -D warnings, tests
make test
make lint
```

CI runs the same `make` targets. Two related crates: `tasmota-core` (the
I/O-agnostic library shared with the CLI: transport, status parsing,
discovery, guardrails) and this crate, `tasmota-web`.

See [`tasmota`](https://github.com/rvben/tasmota-cli) for the command-line
counterpart.

## License

MIT
