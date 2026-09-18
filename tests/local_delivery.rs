use murmur::local_delivery::{Failure, Outcome};
#[test]
fn every_delivery_failure_has_an_actionable_bounded_message() {
    for failure in [
        Failure::MissingTarget,
        Failure::TargetChanged,
        Failure::SecureTarget,
        Failure::UnsupportedTarget,
        Failure::UnsupportedClipboard,
        Failure::ClipboardChanged,
        Failure::ClipboardWriteFailed,
        Failure::PendingRestore,
        Failure::PasteDispatchFailed,
    ] {
        let message = failure.message();
        assert!(message.len() < 110);
        assert!(message.contains("Transcript") || matches!(failure, Failure::SecureTarget));
        assert!(!message.contains("Text inserted"));
    }
    assert_eq!(
        Outcome::PasteSent.message(),
        "Paste sent · verify text in target"
    );
    assert_eq!(Outcome::AxWrite.message(), "Text inserted");
}
#[test]
fn clipboard_failures_distinguish_unsupported_changed_and_pending() {
    assert!(Failure::UnsupportedClipboard.message().contains("format"));
    assert!(Failure::ClipboardChanged.message().contains("changed"));
    assert!(Failure::PendingRestore.message().contains("Restore"));
}
