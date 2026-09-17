//! Per-OS backends behind the seam.
//!
//! Real capture is cfg-gated so nothing macOS-specific compiles on Linux, and
//! CI cross-checks this crate against `x86_64-pc-windows-msvc` against a stub.
//! Building the seam before any capture code costs a few days; retrofitting it
//! later costs weeks plus macOS regression risk (docs/REQUIREMENTS.md 6.5).

pub mod file;
pub mod stub;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;

pub use crate::platform::stub::StubPlatform;

#[cfg(target_os = "macos")]
pub use crate::platform::macos::MacOsPlatform;

/// The backend for the OS this binary was built for.
///
/// Callers never gain an `#[cfg]`: the type differs per platform but the
/// trait does not.
#[cfg(target_os = "macos")]
#[must_use]
pub fn host() -> MacOsPlatform {
    MacOsPlatform::new()
}

/// The backend for the OS this binary was built for.
///
/// Windows and Linux resolve to [`StubPlatform`], which refuses to open
/// anything with a typed error rather than recording silence.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn host() -> StubPlatform {
    StubPlatform::new()
}
