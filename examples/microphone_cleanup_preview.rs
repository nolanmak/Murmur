//! Explicit owned AppKit view rendering; no microphone or global input hooks.
#[cfg(all(target_os = "macos", debug_assertions))]
fn main() -> Result<(), String> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or("Usage: microphone_cleanup_preview OUTPUT_DIRECTORY")?;
    murmur::platform::macos::preview_microphone_cleanup(std::path::Path::new(&output))
}
#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() -> Result<(), String> {
    Err("Use a macOS debug build for the owned AppKit preview".into())
}
