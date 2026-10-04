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
    assert!(!login.is_enabled());
    assert!(login.sync(Path::new("/old/Murmur.app")).unwrap());
    assert!(login.is_enabled());
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
    assert!(!login.is_enabled());
    assert!(!login.agent_path().exists());
    assert!(!login.sync(app).unwrap());
    assert!(!login.agent_path().exists());
    login.set_enabled(true, app).unwrap();
    assert!(login.is_enabled());
    assert!(login.agent_path().exists());
}
