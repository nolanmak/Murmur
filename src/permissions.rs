//! Pure permission model: which macOS grants are missing, what that breaks,
//! and what to ask for next. The native shell supplies snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    Undetermined,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    Microphone,
    Accessibility,
    InputMonitoring,
}
impl Grant {
    pub fn name(self) -> &'static str {
        match self {
            Self::Microphone => "Microphone",
            Self::Accessibility => "Accessibility",
            Self::InputMonitoring => "Input Monitoring",
        }
    }
    fn effect(self) -> &'static str {
        match self {
            Self::Microphone => "Murmur can't hear you",
            Self::Accessibility => "Murmur can't paste",
            Self::InputMonitoring => "hold Control won't start",
        }
    }
    pub fn settings_url(self) -> String {
        let pane = match self {
            Self::Microphone => "Privacy_Microphone",
            Self::Accessibility => "Privacy_Accessibility",
            Self::InputMonitoring => "Privacy_ListenEvent",
        };
        format!("x-apple.systempreferences:com.apple.preference.security?{pane}")
    }
}
/// Where a dictation would be delivered, which decides the grants it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Inserted into the focused field or a detected RustDesk window (needs Accessibility).
    Local,
    /// Held for explicit review and clipboard copy.
    RemoteReview,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub microphone: Access,
    pub accessibility: bool,
    pub input_monitoring: bool,
    /// Ad-hoc grants are pinned to one build's cdhash and vanish on rebuild.
    pub ad_hoc: bool,
}
const AD_HOC_HINT: &str = "Ad-hoc build: grants from earlier builds don't carry over, so remove Murmur in System Settings and add it again";
impl Snapshot {
    pub fn missing(&self) -> Vec<Grant> {
        [
            (Grant::Microphone, self.microphone == Access::Granted),
            (Grant::Accessibility, self.accessibility),
            (Grant::InputMonitoring, self.input_monitoring),
        ]
        .into_iter()
        .filter_map(|(grant, ok)| (!ok).then_some(grant))
        .collect()
    }
    /// Whether a dictation may start recording; the first blocking grant otherwise.
    pub fn gate(&self, route: Route) -> Result<(), Grant> {
        if self.microphone != Access::Granted {
            return Err(Grant::Microphone);
        }
        if route == Route::Local && !self.accessibility {
            return Err(Grant::Accessibility);
        }
        Ok(())
    }
    /// Persistent idle status naming every missing grant.
    pub fn warning(&self) -> Option<String> {
        let missing = self.missing();
        let headline = match missing.as_slice() {
            [] => return None,
            [grant] => format!("{} off · {}", grant.name(), grant.effect()),
            [rest @ .., last] => {
                let rest: Vec<_> = rest.iter().map(|g| g.name()).collect();
                format!("{} and {} off", rest.join(", "), last.name())
            }
        };
        // macOS applies an Input Monitoring grant only to newly launched processes.
        let reopen = if self.input_monitoring {
            ""
        } else {
            ", then Reopen Murmur"
        };
        Some(self.with_hint(format!(
            "{headline}. Use Set up permissions in the Control menu{reopen}"
        )))
    }
    pub fn report(&self, keyboard_listener: bool) -> String {
        let microphone = match self.microphone {
            Access::Granted => "granted",
            Access::Denied => "denied",
            Access::Undetermined => "undetermined",
        };
        format!(
            "permissions microphone={microphone} accessibility={} input_monitoring={} keyboard_listener={keyboard_listener} signature={}",
            self.accessibility,
            self.input_monitoring,
            if self.ad_hoc { "adhoc" } else { "certificate" }
        )
    }
    fn with_hint(&self, message: String) -> String {
        if self.ad_hoc {
            format!("{message}. {AD_HOC_HINT}")
        } else {
            message
        }
    }
}
/// Shown when a dictation is refused; the pill keeps the first clause.
pub fn blocked_message(grant: Grant, snapshot: &Snapshot) -> String {
    let next = match request(grant, snapshot) {
        Request::MicrophonePrompt => "Allow it in the system prompt",
        _ => "Opening System Settings",
    };
    snapshot.with_hint(format!("{} off · {}. {next}", grant.name(), grant.effect()))
}
/// `…/Murmur.app` for an executable inside a bundle; `None` for bare binaries.
pub fn app_bundle(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    let is_app = bundle.extension().is_some_and(|e| e == "app");
    is_app.then(|| bundle.to_path_buf())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    MicrophonePrompt,
    AccessibilityPrompt,
    InputMonitoringPrompt,
    OpenSettings(Grant),
}
/// The system prompt appears only while undetermined; afterwards only Settings can grant.
pub fn request(grant: Grant, snapshot: &Snapshot) -> Request {
    match grant {
        Grant::Microphone if snapshot.microphone == Access::Undetermined => {
            Request::MicrophonePrompt
        }
        Grant::Microphone => Request::OpenSettings(Grant::Microphone),
        Grant::Accessibility => Request::AccessibilityPrompt,
        Grant::InputMonitoring => Request::InputMonitoringPrompt,
    }
}
/// Launch-time guidance: one prompt at a time, each grant asked at most once per launch.
#[derive(Debug, Default)]
pub struct Onboarding {
    asked: Vec<Grant>,
    pending: Option<Grant>,
}
impl Onboarding {
    pub fn next(&mut self, snapshot: &Snapshot) -> Option<Grant> {
        let missing = snapshot.missing();
        if let Some(pending) = self.pending {
            if missing.contains(&pending) {
                return None;
            }
            self.pending = None;
        }
        let grant = missing.into_iter().find(|g| !self.asked.contains(g))?;
        self.asked.push(grant);
        self.pending = Some(grant);
        Some(grant)
    }
    /// The pending prompt was dismissed without granting; continue with the next grant.
    pub fn answered(&mut self) {
        self.pending = None;
    }
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }
}
pub fn newly_granted(before: &Snapshot, after: &Snapshot) -> Vec<Grant> {
    let was = before.missing();
    let now = after.missing();
    was.into_iter().filter(|g| !now.contains(g)).collect()
}
/// `kSecCodeSignatureAdhoc` in the flags from `SecCodeCopySigningInformation`.
pub fn is_ad_hoc(code_signature_flags: u32) -> bool {
    code_signature_flags & 0x0002 != 0
}
