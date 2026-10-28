//! Owned AppKit menu fixture. No Shell, microphone, credential, clipboard or input event.
#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use murmur::{
        cleanup_config::{NetworkPolicy, Settings},
        platform::macos_cleanup::CleanupMenu,
    };
    let root = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or("Pass a new absolute fixture directory")?;
    if !root.is_absolute() {
        return Err("Fixture directory must be absolute".into());
    }
    std::fs::create_dir(&root).map_err(|_| "Fixture directory must be new")?;
    let mtm = objc2::MainThreadMarker::new().ok_or("Fixture needs main thread")?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Prohibited);
    let settings = root.join("cleanup.json");
    let policy = root.join("network-policy.json");
    let config = Settings {
        endpoint: "https://provider.example/v1/chat/completions".into(),
        model: "fixture-model".into(),
        ..Default::default()
    };
    config.save(&settings)?;
    let mut menu = CleanupMenu::new(Ok(settings.clone()), Ok(policy.clone()))?;
    if menu.menu.text() != "Text cleanup: Off" {
        return Err("Initial menu state wrong".into());
    }
    menu.handle("cleanup:toggle")
        .ok_or("Missing toggle route")??;
    if !Settings::load(&settings)?.enabled || menu.menu.text() != "Text cleanup: On" {
        return Err("Cleanup toggle failed".into());
    }
    menu.handle("cleanup:offline")
        .ok_or("Missing privacy route")??;
    if menu.speech_label(
        "Transcription: Deepgram",
        "Transcription: Local Parakeet (Offline-all)",
    ) != "Transcription: Local Parakeet (Offline-all)"
    {
        return Err("Privacy override hidden in speech label".into());
    }
    if !NetworkPolicy::load(&policy)?.offline_all || !menu.offline_all() {
        return Err("Offline-all toggle failed".into());
    }
    if menu.handle("cleanup:toggle").unwrap().is_ok() {
        return Err("Offline-all allowed cleanup enable".into());
    }
    menu.handle("cleanup:offline").unwrap()?;
    let mut changed = Settings::load(&settings)?;
    changed.endpoint = "https://changed.example/v1/chat/completions".into();
    changed.enabled = false;
    changed.save(&settings)?;
    if menu.handle("cleanup:toggle").unwrap().is_ok() {
        return Err("Unreviewed destination was enabled".into());
    }
    if Settings::load(&settings)?.enabled {
        return Err("Stale toggle modified saved choice".into());
    }
    std::fs::write(&policy, b"broken").map_err(|_| "Cannot create invalid policy fixture")?;
    menu.refresh();
    if !menu.network_error() {
        return Err("Invalid privacy settings hidden".into());
    }
    menu.handle("cleanup:offline").unwrap()?;
    if !menu.offline_all() {
        return Err("Private repair failed".into());
    }
    std::fs::write(root.join("result.json"),b"{\"menu_actions\":\"passed\",\"credentials_read\":false,\"capture\":false,\"clipboard\":false}").map_err(|_|"Cannot save fixture result")?;
    println!("Owned cleanup/privacy menu actions passed");
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Owned AppKit menu fixture requires macOS");
}
