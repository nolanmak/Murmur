//! Consent as a *capability*, not a universal macOS-shaped flow.
//!
//! Windows endpoint loopback needs no system-audio permission and shows no
//! prompt; macOS needs one that cannot even be queried. Onboarding renders off
//! [`PlatformCaps`] and [`PermissionState::NotApplicable`] rather than
//! assuming a request-and-wait dance exists. See docs/REQUIREMENTS.md 6.5.

/// A permission a backend might require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Capture the microphone.
    Microphone,
    /// Capture system output audio.
    SystemAudio,
    /// Capture the screen. Only relevant to a ScreenCaptureKit fallback, which
    /// we do not ship; present so a backend can report it rather than lie.
    ScreenRecording,
}

/// The state of a [`Permission`] on a given platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionState {
    /// Granted.
    Granted,
    /// Explicitly denied by the user.
    Denied,
    /// Never asked.
    NotDetermined,
    /// This platform has no such gate. Not the same as `Granted`: onboarding
    /// must show no consent step at all rather than a pre-satisfied one.
    NotApplicable,
    /// Blocked by policy (MDM, parental controls) — the user cannot grant it.
    Restricted,
    /// The platform provides no way to observe this permission.
    ///
    /// macOS system-audio capture is exactly this: the prompt fires only on
    /// the first `AudioDeviceStart` of an aggregate containing a tap, and a
    /// denial delivers silence indistinguishable from a quiet room. Callers
    /// must fall back to a round-trip capture probe (`fotw doctor`) rather
    /// than believing any queried value.
    Unobservable,
}

impl PermissionState {
    /// True when onboarding should show the user a step for this.
    #[must_use]
    pub const fn requires_user_action(&self) -> bool {
        matches!(self, Self::NotDetermined | Self::Denied)
    }

    /// True when capture cannot proceed until something changes.
    #[must_use]
    pub const fn is_blocking(&self) -> bool {
        matches!(self, Self::Denied | Self::Restricted)
    }

    /// True when the value can be trusted at all.
    #[must_use]
    pub const fn is_observable(&self) -> bool {
        !matches!(self, Self::Unobservable)
    }
}

/// What a platform backend can actually do.
///
/// Defaults to "nothing", so a UI that gates affordances on capabilities hides
/// them rather than failing at runtime, and a half-written backend cannot
/// accidentally advertise a feature it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlatformCaps {
    /// Can capture the whole system output mix.
    pub system_mix: bool,
    /// Can scope capture to specific applications.
    pub app_scoped: bool,
    /// Can capture everything *except* named applications (how we exclude our
    /// own output).
    pub exclude_scope: bool,
    /// Delivers callbacks even when nothing is playing.
    ///
    /// `false` on Windows endpoint loopback, which stops delivering entirely
    /// while the endpoint is idle. When this is false the layer above **must**
    /// synthesise the gap from `host_ns`, or 30 seconds of silence at the
    /// start of a meeting shifts the whole recording.
    pub emits_silence_when_idle: bool,
    /// Requires a user consent grant for system audio.
    pub needs_consent_for_system: bool,
}
