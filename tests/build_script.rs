//! scripts/sign-app.sh against fake codesign/security tools; no keychain is touched.
#![cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CODESIGN: &str = r#"#!/bin/sh
echo "codesign $*" >> "$FAKE_LOG"
case "$1" in
--force)
    if [ -n "${FAKE_SIGN_FAIL:-}" ]; then echo "$FAKE_SIGN_FAIL" >&2; exit 1; fi
    for last; do :; done
    echo "$3" > "$last/signature" ;;
--verify) exit "${FAKE_VERIFY_STATUS:-0}" ;;
-d)
    if [ -n "${FAKE_CDHASH_REQUIREMENT:-}" ]; then
        echo '# designated => cdhash H"0123"'
    else
        echo 'designated => identifier "com.shipsystems.texttospeech" and certificate leaf = H"f0"'
    fi ;;
esac
"#;
const SECURITY: &str = r#"#!/bin/sh
echo "security $*" >> "$FAKE_LOG"
case "$1" in
find-identity) printf '%s\n' "${FAKE_IDENTITIES:-     0 valid identities found}" ;;
unlock-keychain) exit "${FAKE_UNLOCK_STATUS:-0}" ;;
esac
"#;
const DEV_IDENTITY: &str = "  1) F053 \"FlyOnTheWall Dev\"\n     1 valid identities found";

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for (name, body) in [("codesign", CODESIGN), ("security", SECURITY)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        let fixture = Self { dir };
        bundle(&fixture.stage(), "new build");
        bundle(&fixture.installed(), "previous build");
        fixture
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn stage(&self) -> PathBuf {
        self.path("stage/Murmur.app")
    }
    fn installed(&self) -> PathBuf {
        self.path("Murmur.app")
    }
    fn keychain(&self) -> PathBuf {
        let keychain = self.path("home/.fotw-dev-cert/fotw-dev.keychain-db");
        std::fs::create_dir_all(keychain.parent().unwrap()).unwrap();
        std::fs::write(&keychain, "fixture").unwrap();
        keychain
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.path("log")).unwrap_or_default()
    }
    fn run(&self, env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/sign-app.sh");
        let mut command = Command::new("sh");
        command
            .arg(script)
            .arg(self.stage())
            .arg(self.installed())
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("FAKE_LOG", self.path("log"));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }
}
fn bundle(path: &Path, marker: &str) {
    std::fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
    std::fs::write(path.join("Contents/MacOS/murmur"), marker).unwrap();
}
fn binary(app: &Path) -> String {
    std::fs::read_to_string(app.join("Contents/MacOS/murmur")).unwrap()
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into()
}
fn assert_previous_bundle_kept(fixture: &Fixture, output: &Output) {
    assert!(!output.status.success(), "{}", stderr(output));
    assert_eq!(binary(&fixture.installed()), "previous build");
    assert!(!fixture.installed().join("signature").exists());
    assert!(!fixture.stage().exists(), "failed stage must be cleaned up");
}

#[test]
fn explicit_identity_signs_stage_then_replaces_installed_bundle() {
    let fixture = Fixture::new();
    let output = fixture.run(&[("MURMUR_SIGN_IDENTITY", "Team Dev")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture
            .log()
            .contains("--sign Team Dev --identifier com.shipsystems.texttospeech")
    );
    assert!(!fixture.log().contains("unlock-keychain"));
    assert_eq!(binary(&fixture.installed()), "new build");
    assert_eq!(
        std::fs::read_to_string(fixture.installed().join("signature")).unwrap(),
        "Team Dev\n"
    );
    assert!(!fixture.stage().exists());
}

#[test]
fn legacy_identity_variable_is_still_honoured() {
    let fixture = Fixture::new();
    let output = fixture.run(&[("TTS_SIGN_IDENTITY", "Legacy Dev")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(fixture.log().contains("--sign Legacy Dev"));
}

#[test]
fn local_dev_identity_is_found_and_its_keychain_unlocked_automatically() {
    let fixture = Fixture::new();
    let keychain = fixture.keychain();
    let output = fixture.run(&[("FAKE_IDENTITIES", DEV_IDENTITY)]);
    assert!(output.status.success(), "{}", stderr(&output));
    let log = fixture.log();
    let unlock = log
        .find(&format!("unlock-keychain -p fotw {}", keychain.display()))
        .expect("keychain unlocked");
    let sign = log
        .find("--sign FlyOnTheWall Dev")
        .expect("signed with dev identity");
    assert!(unlock < sign, "unlock must precede signing:\n{log}");
    assert!(!stderr(&output).contains("ad-hoc"));
}

#[test]
fn keychain_location_and_password_can_be_overridden() {
    let fixture = Fixture::new();
    let custom = fixture.path("custom.keychain-db");
    std::fs::write(&custom, "fixture").unwrap();
    let output = fixture.run(&[
        ("FAKE_IDENTITIES", DEV_IDENTITY),
        ("MURMUR_SIGN_KEYCHAIN", custom.to_str().unwrap()),
        ("MURMUR_SIGN_KEYCHAIN_PASSWORD", "other"),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture
            .log()
            .contains(&format!("unlock-keychain -p other {}", custom.display()))
    );
}

#[test]
fn missing_identity_falls_back_to_ad_hoc_with_a_permission_warning() {
    let fixture = Fixture::new();
    let output = fixture.run(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(fixture.log().contains("--sign - --identifier"));
    let warning = stderr(&output);
    assert!(warning.contains("ad-hoc"), "{warning}");
    assert!(warning.contains("permissions"), "{warning}");
    assert!(warning.contains("MURMUR_SIGN_IDENTITY"), "{warning}");
    assert_eq!(binary(&fixture.installed()), "new build");
}

#[test]
fn required_identity_refuses_ad_hoc_and_keeps_previous_bundle() {
    let fixture = Fixture::new();
    let output = fixture.run(&[("MURMUR_REQUIRE_IDENTITY", "1")]);
    assert_previous_bundle_kept(&fixture, &output);
    assert!(!fixture.log().contains("--force"));
}

#[test]
fn signing_failure_keeps_previous_bundle_and_explains_locked_keychain() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        ("MURMUR_SIGN_IDENTITY", "Team Dev"),
        ("FAKE_SIGN_FAIL", "Murmur.app: errSecInternalComponent"),
    ]);
    assert_previous_bundle_kept(&fixture, &output);
    let message = stderr(&output);
    assert!(message.contains("errSecInternalComponent"), "{message}");
    assert!(message.contains("locked"), "{message}");
}

#[test]
fn verification_failure_keeps_previous_bundle() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        ("MURMUR_SIGN_IDENTITY", "Team Dev"),
        ("FAKE_VERIFY_STATUS", "1"),
    ]);
    assert_previous_bundle_kept(&fixture, &output);
}

#[test]
fn certificate_build_with_cdhash_requirement_is_rejected() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        ("MURMUR_SIGN_IDENTITY", "Team Dev"),
        ("FAKE_CDHASH_REQUIREMENT", "1"),
    ]);
    assert_previous_bundle_kept(&fixture, &output);
    assert!(stderr(&output).contains("cdhash"));
}

#[test]
fn first_build_installs_without_a_previous_bundle() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(fixture.installed()).unwrap();
    let output = fixture.run(&[("MURMUR_SIGN_IDENTITY", "Team Dev")]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(binary(&fixture.installed()), "new build");
}
