#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_clipboard;

#[cfg(target_os = "macos")]
pub mod macos_local_clipboard;

#[cfg(target_os = "macos")]
pub mod macos_language;

#[cfg(target_os = "macos")]
pub mod macos_vocabulary;

#[cfg(target_os = "macos")]
mod macos_sleep;

#[cfg(target_os = "macos")]
pub mod macos_cleanup;
