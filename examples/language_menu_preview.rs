//! Isolated native menu QA: language_menu_preview /absolute/fixture/profile.json
//! Uses only the explicit fixture profile; no microphone, credentials or input tap.
#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    use block2::RcBlock;
    use muda::{Menu, MenuEvent, MenuItem, Submenu};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSTimer;
    use std::{cell::RefCell, ptr::NonNull, rc::Rc};
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("Pass an absolute fixture profile path")?;
    let mtm = MainThreadMarker::new().ok_or("Run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    let menu = Menu::new();
    let application = Submenu::new("Murmur Language Preview", true);
    let status = MenuItem::new("Synthetic profile · no recording", false, None);
    application.append(&status).map_err(|e| e.to_string())?;
    application
        .append(&MenuItem::with_id("quit", "Quit preview", true, None))
        .map_err(|e| e.to_string())?;
    menu.append(&application).map_err(|e| e.to_string())?;
    let language = Rc::new(RefCell::new(
        murmur::platform::macos_language::LanguageMenu::new(Ok(path))?,
    ));
    menu.append(&language.borrow().menu)
        .map_err(|e| e.to_string())?;
    menu.init_for_nsapp();
    app.activate();
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        let mut language = language.borrow_mut();
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id.as_ref() == "quit" {
                std::process::exit(0);
            }
            if let Some(result) = language.handle(event.id.as_ref()) {
                status.set_text(match result {
                    Ok(message) => message,
                    Err(error) => error,
                });
            }
        }
        language.refresh_if_due();
    });
    let _timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &block) };
    app.run();
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("The native language menu preview requires macOS");
}
