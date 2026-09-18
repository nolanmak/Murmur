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

#[test]
fn manual_copy_failure_is_not_reported_as_success_and_pending_restore_blocks_it() {
    use murmur::local_delivery::{CopyBoard, CopyFailure, copy_transcript};
    struct FakeCopy {
        attempts: usize,
        fail: bool,
    }
    impl CopyBoard for FakeCopy {
        fn overwrite_with_text(&mut self, _: &str) -> Result<(), CopyFailure> {
            self.attempts += 1;
            if self.fail {
                Err(CopyFailure::WriteFailedAfterClear)
            } else {
                Ok(())
            }
        }
    }
    let transcript = "Café 👋";
    let mut board = FakeCopy {
        attempts: 0,
        fail: true,
    };
    assert_eq!(
        copy_transcript(&mut board, true, transcript),
        Err(CopyFailure::PendingRestore)
    );
    assert_eq!(board.attempts, 0);
    assert_eq!(
        copy_transcript(&mut board, false, transcript),
        Err(CopyFailure::WriteFailedAfterClear)
    );
    assert_eq!(board.attempts, 1);
    assert!(
        CopyFailure::WriteFailedAfterClear
            .message()
            .contains("may have changed")
    );
    assert!(
        !CopyFailure::WriteFailedAfterClear
            .message()
            .contains("copied")
    );
    board.fail = false;
    assert_eq!(copy_transcript(&mut board, false, transcript), Ok(()));
}
