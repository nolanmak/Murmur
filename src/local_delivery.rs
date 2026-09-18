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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFailure {
    PendingRestore,
    WriteFailedAfterClear,
}
impl CopyFailure {
    pub fn message(self) -> &'static str {
        match self {
            Self::PendingRestore => "Restore previous clipboard before copying transcript",
            Self::WriteFailedAfterClear => {
                "Clipboard write failed; clipboard may have changed · transcript retained"
            }
        }
    }
}
pub trait CopyBoard {
    /// User explicitly requested replacement; failure may leave modified contents.
    fn overwrite_with_text(&mut self, text: &str) -> Result<(), CopyFailure>;
}
pub fn copy_transcript(
    board: &mut impl CopyBoard,
    pending_restore: bool,
    text: &str,
) -> Result<(), CopyFailure> {
    if pending_restore {
        return Err(CopyFailure::PendingRestore);
    }
    board.overwrite_with_text(text)
}
