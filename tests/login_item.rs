use murmur::login_item::{self, LoginItem};
use std::path::Path;

fn item(home: &Path) -> LoginItem {
    LoginItem::new(home.to_path_buf())
}

#[test]
fn bundle_is_found_only_for_app_executables() {
    let app = Path::new("/Apps/My Murmur.app");
    assert_eq!(
        login_item::app_bundle(&app.join("Contents/MacOS/murmur")),
        Some(app.to_path_buf())
    );
    assert_eq!(
        login_item::app_bundle(Path::new("/repo/target/debug/murmur")),
        None
    );
}

#[test]
fn agent_opens_the_bundle_at_login_with_escaped_path() {
    let plist = login_item::agent_plist(Path::new("/Apps/R&D <Murmur>.app"));
    assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    assert!(plist.contains("<string>/usr/bin/open</string>"));
    assert!(plist.contains("<string>/Apps/R&amp;D &lt;Murmur&gt;.app</string>"));
    assert!(plist.contains(&format!("<string>{}</string>", login_item::LABEL)));
}

#[test]
fn first_launch_enables_and_relaunch_from_new_location_refreshes_agent() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/old/Murmur.app");
    assert!(!login.is_enabled(app));
    assert!(login.sync(Path::new("/old/Murmur.app")).unwrap());
    assert!(login.is_enabled(app));
    assert!(!login.sync(Path::new("/old/Murmur.app")).unwrap());
    assert!(login.sync(Path::new("/new/Murmur.app")).unwrap());
    let written = std::fs::read_to_string(login.agent_path()).unwrap();
    assert!(written.contains("/new/Murmur.app"));
    assert!(!written.contains("/old/Murmur.app"));
}

#[test]
fn turning_off_survives_relaunch_until_turned_back_on() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/Murmur.app");
    login.sync(app).unwrap();
    login.set_enabled(false, app).unwrap();
    assert!(!login.is_enabled(app));
    assert!(!login.agent_path().exists());
    assert!(!login.sync(app).unwrap());
    assert!(!login.agent_path().exists());
    login.set_enabled(true, app).unwrap();
    assert!(login.is_enabled(app));
    assert!(login.agent_path().exists());
}

#[test]
fn invalid_registration_is_not_reported_enabled_and_is_repaired() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/Murmur.app");
    login.sync(app).unwrap();
    std::fs::write(login.agent_path(), "not a plist").unwrap();
    assert!(!login.is_enabled(app));
    assert!(login.sync(app).unwrap());
    assert!(login.is_enabled(app));
}

#[test]
fn opt_out_removes_leftover_registration_on_next_launch() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/Murmur.app");
    login.sync(app).unwrap();
    login.set_enabled(false, app).unwrap();
    std::fs::write(login.agent_path(), login_item::agent_plist(app)).unwrap();
    assert!(!login.is_enabled(app));
    login.sync(app).unwrap();
    assert!(!login.agent_path().exists());
}

#[test]
fn failed_enable_preserves_explicit_opt_out() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/Murmur.app");
    login.set_enabled(false, app).unwrap();
    // Deterministic failure on every OS, even when tests run as root.
    std::fs::write(home.path().join("Library/LaunchAgents"), "block directory").unwrap();
    assert!(login.set_enabled(true, app).is_err());
    std::fs::remove_file(home.path().join("Library/LaunchAgents")).unwrap();
    login.sync(app).unwrap();
    assert!(
        !login.is_enabled(app),
        "failed enable must not discard user's opt-out"
    );
}

#[test]
fn stale_registration_is_not_reported_enabled_for_new_bundle() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    login.sync(Path::new("/old/Murmur.app")).unwrap();
    let new = Path::new("/new/Murmur.app");
    assert!(!login.is_enabled(new));
    login.sync(new).unwrap();
    assert!(login.is_enabled(new));
}

#[test]
fn failed_disable_is_retried_and_can_recover() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/Murmur.app");
    std::fs::create_dir_all(login.agent_path()).unwrap();
    assert!(login.set_enabled(false, app).is_err());
    assert!(!login.is_enabled(app));
    assert!(login.sync(app).is_err());
    std::fs::remove_dir(login.agent_path()).unwrap();
    login.sync(app).unwrap();
    assert!(!login.agent_path().exists());
    login.set_enabled(true, app).unwrap();
    assert!(login.is_enabled(app));
}

#[cfg(unix)]
#[test]
fn failed_relocation_preserves_previous_registration() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let old = Path::new("/old/Murmur.app");
    login.sync(old).unwrap();
    let path = login.agent_path();
    let parent = path.parent().unwrap();
    let permissions = std::fs::metadata(parent).unwrap().permissions();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = login.sync(Path::new("/new/Murmur.app"));
    std::fs::set_permissions(parent, permissions).unwrap();
    assert!(result.is_err());
    assert!(login.is_enabled(old));
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
}

#[cfg(target_os = "macos")]
#[test]
fn generated_plist_parses_with_exact_argument_boundaries() {
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/R&D <My Murmur>.app");
    login.sync(app).unwrap();
    let output = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "ProgramArguments.1", "raw", "-o", "-"])
        .arg(login.agent_path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim_end(),
        app.to_str().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn unreadable_opt_out_never_reports_enabled_or_discards_preference() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let login = item(home.path());
    let app = Path::new("/Apps/Murmur.app");
    login.set_enabled(false, app).unwrap();
    std::fs::create_dir_all(login.agent_path().parent().unwrap()).unwrap();
    std::fs::write(login.agent_path(), login_item::agent_plist(app)).unwrap();
    let preferences = home.path().join("Library/Application Support/Murmur");
    let permissions = std::fs::metadata(&preferences).unwrap().permissions();
    std::fs::set_permissions(&preferences, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = login.sync(app);
    let enabled = login.is_enabled(app);
    std::fs::set_permissions(&preferences, permissions).unwrap();
    assert!(result.is_err());
    assert!(!enabled, "unknown opt-out must not be treated as absent");
    login.sync(app).unwrap();
    assert!(!login.agent_path().exists());
    assert!(preferences.join("open-at-login-disabled").exists());
}
