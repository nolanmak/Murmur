//! Owned AppKit fixtures: vocabulary_native_qa /absolute/new-fixture-directory
#[cfg(all(target_os = "macos", debug_assertions))]
fn main() -> Result<(), String> {
    let output = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or("Pass a new absolute fixture directory")?;
    murmur::platform::macos_vocabulary::qa::run(&output)
}
#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn main() {
    eprintln!("Owned native fixtures require a debug macOS build");
}
