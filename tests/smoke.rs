use std::io::Write;

#[test]
fn config_default_has_loopback_bind() {
    let c = tasmota_web::config::Config::default();
    assert_eq!(c.poll_interval_secs, 5);
    assert!(c.bind.ip().is_loopback());
}

#[test]
fn config_load_round_trip() {
    let path = std::env::temp_dir().join(format!(
        "tasmota-web-smoke-{}-{}.toml",
        std::process::id(),
        line!()
    ));
    let mut f = std::fs::File::create(&path).unwrap();
    write!(
        f,
        r#"
bind = "127.0.0.1:9090"
poll_interval_secs = 10

[auth]
mode = "builtin"

[[devices]]
name = "plug1"
host = "10.0.0.5"
"#
    )
    .unwrap();
    drop(f);

    let loaded = tasmota_web::config::Config::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    assert_eq!(loaded.bind.port(), 9090);
    assert_eq!(loaded.poll_interval_secs, 10);
    assert_eq!(loaded.auth.mode, tasmota_web::config::AuthMode::Builtin);
    assert_eq!(loaded.devices.len(), 1);
    assert_eq!(loaded.devices[0].name, "plug1");
    assert_eq!(loaded.devices[0].host, "10.0.0.5");
}

#[test]
fn config_load_missing_file_returns_default() {
    let path = std::env::temp_dir().join(format!(
        "tasmota-web-smoke-missing-{}-{}.toml",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&path);

    let loaded = tasmota_web::config::Config::load(&path).unwrap();

    assert_eq!(loaded.poll_interval_secs, 5);
    assert!(loaded.devices.is_empty());
}
