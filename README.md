# tasmota-web

An unofficial web dashboard and admin for [Tasmota](https://tasmota.github.io/docs/)
smart devices: live status, on/off control, energy readouts, and MQTT/console
access, in the browser. Built with axum, maud, and htmx.

> This is a third-party tool. It is not affiliated with the Tasmota project.

It is a sibling to the `tasmota` CLI and shares its core library
(`tasmota-core`): talks directly to each device over HTTP (no MQTT broker
required for control), and polls devices to keep the dashboard live.

## Install

```sh
cargo install tasmota-web
```

Or run the container image built from this repo's `Dockerfile`.

## Usage

```sh
tasmota-web --config /path/to/tasmota-web.toml
```

The config file lists devices (name, host, optional password), the bind
address (default `127.0.0.1:8088`), the poll interval, and authentication
settings. See `src/config.rs` for the full schema. `tasmota-web hash-password`
generates a password hash for `auth.password_hash` when `auth.mode = "builtin"`.

Absent data (an offline device, a plug with no energy sensor) renders as
`n/a`, never a coerced `0`.

## Development

```sh
make check   # fmt --check, clippy -D warnings, tests
make test
make lint
```

CI runs the same `make` targets.

## License

MIT
