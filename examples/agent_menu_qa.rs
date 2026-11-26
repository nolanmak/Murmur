//! Owned AppKit menu fixture for the Agent voice toggle (#114). No Shell,
//! microphone, credential, agent CLI or audio.
#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use murmur::{
        agent::{speech::Settings, voice},
        platform::macos_agent::{AgentMenu, TOGGLE},
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
    let path = root.join("agent-voice.json");
    let mut menu = AgentMenu::new(Ok(path.clone()));
    if menu.on() || menu.item.text() != "Agent voice: Off" || !menu.item.is_enabled() {
        return Err("Initial agent voice state wrong".into());
    }
    if menu.handle("cleanup:toggle").is_some() {
        return Err("Agent menu claimed another menu's event".into());
    }
    menu.handle(TOGGLE).ok_or("Missing toggle route")??;
    if !menu.on() || !voice::routed(&path) || !menu.item.text().starts_with("Agent voice: On") {
        return Err("Agent voice toggle failed".into());
    }
    // The saved route survives a restart and keeps the voice choice.
    let reopened = AgentMenu::new(Ok(path.clone()));
    if !reopened.on()
        || Settings::load(&path)?
            != (Settings {
                route_dictation: true,
                ..Default::default()
            })
    {
        return Err("Agent voice route was not persisted".into());
    }
    menu.handle(TOGGLE).ok_or("Missing toggle route")??;
    if menu.on() || voice::routed(&path) || menu.item.text() != "Agent voice: Off" {
        return Err("Agent voice off failed".into());
    }
    // Unusable settings: the item is disabled and a toggle reports the error.
    let mut broken = AgentMenu::new(Err("Cannot locate home directory".into()));
    if broken.item.is_enabled() || !matches!(broken.handle(TOGGLE), Some(Err(_))) {
        return Err("Unusable settings were not reported".into());
    }
    std::fs::write(
        root.join("result.json"),
        b"{\"agent_menu\":\"passed\",\"credentials_read\":false,\"capture\":false,\"agents_started\":false}",
    )
    .map_err(|_| "Cannot save fixture result")?;
    println!("Owned agent voice menu actions passed");
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Owned AppKit menu fixture requires macOS");
}
