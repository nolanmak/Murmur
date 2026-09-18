//! Local insertion outcomes. Messages contain no clipboard or transcript data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    AxWrite,
    PasteSent,
}
impl Outcome {
    pub fn message(self) -> &'static str {
        match self {
            Self::AxWrite => "Text inserted",
            Self::PasteSent => "Paste sent · verify text in target",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    MissingTarget,
    TargetChanged,
    SecureTarget,
    UnsupportedTarget,
    UnsupportedClipboard,
    ClipboardChanged,
    ClipboardWriteFailed,
    PendingRestore,
    PasteDispatchFailed,
}
impl Failure {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingTarget => "Transcript ready · choose Copy Last Transcript",
            Self::TargetChanged => "Transcript ready · focus changed. Use Copy Last Transcript",
            Self::SecureTarget => "Choose a supported text field. Password fields are blocked",
            Self::UnsupportedTarget => {
                "Transcript ready · field unsupported. Use Copy Last Transcript"
            }
            Self::UnsupportedClipboard => {
                "Transcript ready · clipboard format unsupported. Use Copy Last Transcript"
            }
            Self::ClipboardChanged => {
                "Transcript ready · clipboard changed. Use Copy Last Transcript"
            }
            Self::ClipboardWriteFailed => {
                "Transcript ready · clipboard write failed. Restore previous clipboard"
            }
            Self::PendingRestore => {
                "Transcript ready · Restore previous clipboard before another paste"
            }
            Self::PasteDispatchFailed => {
                "Transcript ready · paste not sent. Check focus; restore previous clipboard"
            }
        }
    }
}
