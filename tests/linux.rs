#![cfg(target_os = "linux")]
use murmur::platform::linux::{config_path, microphones_from_json};
use std::collections::HashMap;
#[test]
fn sources_exclude_monitor_and_missing_identity() {
    let raw = r#"[{"info":{"props":{"media.class":"Audio/Sink","object.serial":1}}},{"info":{"props":{"media.class":"Audio/Source","object.serial":2,"node.name":"mic","node.description":"Microphone"}}},{"info":{"props":{"media.class":"Audio/Source","object.serial":3,"node.name":"output.monitor"}}},{"info":{"props":{"media.class":"Audio/Source"}}}]"#;
    let sources = microphones_from_json(raw).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].serial, "2");
    assert!(microphones_from_json("invalid").is_err());
}
#[test]
fn config_precedence_is_explicit_then_xdg_then_home() {
    let mut vars = HashMap::from([("HOME".into(), "/fixture".into())]);
    assert_eq!(
        config_path(&vars).unwrap().to_str().unwrap(),
        "/fixture/.config/murmur/env"
    );
    vars.insert("XDG_CONFIG_HOME".into(), "/config".into());
    assert_eq!(
        config_path(&vars).unwrap().to_str().unwrap(),
        "/config/murmur/env"
    );
    vars.insert("FOTW_ENV_FILE".into(), "/explicit".into());
    assert_eq!(config_path(&vars).unwrap().to_str().unwrap(), "/explicit");
}
#[test]
fn linux_missing_key_has_linux_guidance() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_murmur"))
        .arg("doctor")
        .env_remove("DEEPGRAM_API_KEY")
        .env("FOTW_ENV_FILE", "/nonexistent/murmur.env")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("Keychain"));
}

#[test]
fn credential_file_permissions_and_environment_override() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("env");
    std::fs::write(&file, "DEEPGRAM_API_KEY=fixture-secret\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let doctor = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_murmur"));
        command
            .arg("doctor")
            .env_remove("DEEPGRAM_API_KEY")
            .env("FOTW_ENV_FILE", &file);
        command
    };
    let blocked = doctor().output().unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("chmod 600"));
    let override_key = doctor()
        .env("DEEPGRAM_API_KEY", "process-fixture")
        .output()
        .unwrap();
    assert!(override_key.status.success());
    assert!(!String::from_utf8_lossy(&override_key.stdout).contains("process-fixture"));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let private = doctor().output().unwrap();
    assert!(private.status.success());
    assert!(!String::from_utf8_lossy(&private.stdout).contains("fixture-secret"));
    std::fs::write(&file, "bad-entry-without-equals\n").unwrap();
    let malformed = doctor().output().unwrap();
    assert!(!malformed.status.success());
    assert!(!String::from_utf8_lossy(&malformed.stderr).contains("bad-entry"));
}

#[test]
fn headless_desktop_exits_without_capture_or_key_access() {
    let output = std::process::Command::new("timeout")
        .args(["5", env!("CARGO_BIN_EXE_murmur"), "run"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DEEPGRAM_API_KEY")
        .env("FOTW_ENV_FILE", "/nonexistent/murmur.env")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_ne!(output.status.code(), Some(124));
    assert!(String::from_utf8_lossy(&output.stderr).contains("graphical"));
}

#[test]
fn pipewire_streamed_snapshots_apply_device_removal() {
    let raw = r#"[{"id":1,"info":{"props":{"media.class":"Audio/Source","object.serial":11,"node.name":"mic-one"}}},{"id":2,"info":{"props":{"media.class":"Audio/Source","object.serial":22,"node.name":"mic-two"}}}]
[{"id":1,"info":null}]
[{"id":2,"info":{"state":"running"}}]"#;
    let sources = microphones_from_json(raw).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].serial, "22");
}
