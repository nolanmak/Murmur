use murmur::{remote::Attempt, remote_clipboard::*};
#[test]
fn copy_failures_have_distinct_bounded_actionable_messages() {
    let errors = [
        Error::Busy,
        Error::Unsupported,
        Error::Unavailable,
        Error::Changed,
        Error::WriteFailed,
        Error::InvalidText,
    ];
    let messages: std::collections::HashSet<_> = errors.iter().map(|e| e.message()).collect();
    assert_eq!(messages.len(), errors.len());
    assert!(
        messages
            .iter()
            .all(|m| m.len() < 110 && !m.contains("copied"))
    );
}
#[derive(Default)]
struct Fake {
    revision: u64,
    formats: Vec<Format>,
    unsupported: bool,
    conflict: bool,
    partial: bool,
    unavailable: bool,
    writes: usize,
}
fn formats() -> Vec<Format> {
    vec![
        Format {
            name: "public.utf8-plain-text".into(),
            bytes: b"original".to_vec(),
        },
        Format {
            name: "public.rtf".into(),
            bytes: br"{\rtf1 original}".to_vec(),
        },
    ]
}
fn original() -> Fake {
    Fake {
        formats: formats(),
        ..Default::default()
    }
}
fn payload(c: &Fake) -> &[u8] {
    &c.formats[0].bytes
}
impl Clipboard for Fake {
    fn snapshot(&mut self) -> Result<Snapshot, Error> {
        if self.unsupported {
            return Err(Error::Unsupported);
        }
        if self.unavailable {
            return Err(Error::Unavailable);
        }
        Ok(Snapshot {
            revision: Revision(self.revision),
            formats: self
                .formats
                .iter()
                .map(|f| Format {
                    name: f.name.clone(),
                    bytes: f.bytes.clone(),
                })
                .collect(),
        })
    }
    fn replace(&mut self, expected: Revision, text: &str) -> Result<Revision, WriteFailure> {
        self.write(
            expected,
            vec![Format {
                name: "public.utf8-plain-text".into(),
                bytes: text.as_bytes().to_vec(),
            }],
        )
    }
    fn restore(&mut self, expected: Revision, values: &[Format]) -> Result<Revision, WriteFailure> {
        self.write(
            expected,
            values
                .iter()
                .map(|f| Format {
                    name: f.name.clone(),
                    bytes: f.bytes.clone(),
                })
                .collect(),
        )
    }
}
impl Fake {
    fn write(
        &mut self,
        expected: Revision,
        formats: Vec<Format>,
    ) -> Result<Revision, WriteFailure> {
        if self.conflict {
            self.revision += 1;
            self.formats = vec![Format {
                name: "new owner".into(),
                bytes: b"external".to_vec(),
            }];
            self.conflict = false;
        }
        if expected != Revision(self.revision) {
            return Err(WriteFailure::OwnershipLost);
        }
        if self.unavailable {
            return Err(WriteFailure::Unchanged(Error::Unavailable));
        }
        self.writes += 1;
        self.revision += 1;
        if self.partial {
            self.formats.clear();
            self.partial = false;
            return Err(WriteFailure::Owned(Revision(self.revision)));
        }
        self.formats = formats;
        Ok(Revision(self.revision))
    }
}
#[test]
fn preserves_unicode_and_restores_every_supported_format_after_consumption() {
    let mut c = original();
    let mut lease = Lease::default();
    lease
        .share(&mut c, Attempt(1), "Café 👋  こんにちは")
        .unwrap();
    assert_eq!(payload(&c), "Café 👋  こんにちは".as_bytes());
    assert!(lease.pending());
    assert_eq!(
        lease.recover(
            &mut c,
            Attempt(1),
            Some(RestoreReason::ConsumptionConfirmed)
        ),
        Ok(Recovery::Restored)
    );
    assert_eq!(c.formats.len(), 2);
    for (actual, expected) in c.formats.iter().zip(formats()) {
        assert_eq!(actual.name, expected.name);
        assert_eq!(actual.bytes, expected.bytes);
    }
    assert!(!lease.pending());
}
#[test]
fn cancellation_timeout_or_shutdown_without_receipt_keeps_transcript_copied() {
    let mut c = original();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "reviewed").unwrap();
    for _ in 0..4 {
        assert_eq!(
            lease.recover(&mut c, Attempt(1), None),
            Ok(Recovery::Retained)
        );
    }
    assert_eq!(payload(&c), b"reviewed");
    assert_eq!(c.writes, 1);
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Ok(Recovery::Restored)
    );
    assert_eq!(payload(&c), b"original");
}
#[test]
fn never_overwrites_newer_clipboard_owner_even_with_explicit_restore() {
    for reason in [
        RestoreReason::ConsumptionConfirmed,
        RestoreReason::UserRequested,
    ] {
        let mut c = original();
        let mut lease = Lease::default();
        lease.share(&mut c, Attempt(1), "reviewed").unwrap();
        c.conflict = true;
        assert_eq!(
            lease.recover(&mut c, Attempt(1), Some(reason)),
            Ok(Recovery::OwnershipLost)
        );
        assert_eq!(payload(&c), b"external");
        assert!(!lease.pending());
    }
}
#[test]
fn rejects_unsupported_snapshot_and_changes_between_snapshot_and_write() {
    let mut c = Fake {
        unsupported: true,
        ..original()
    };
    let mut lease = Lease::default();
    assert_eq!(
        lease.share(&mut c, Attempt(1), "reviewed"),
        Err(Error::Unsupported)
    );
    assert_eq!(c.writes, 0);
    c.unsupported = false;
    c.conflict = true;
    assert_eq!(
        lease.share(&mut c, Attempt(2), "reviewed"),
        Err(Error::Changed)
    );
    assert_eq!(payload(&c), b"external");
    assert_eq!(c.writes, 0);
}
#[test]
fn stale_attempt_and_duplicate_cleanup_cannot_restore_another_attempt() {
    let mut c = original();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "first").unwrap();
    assert_eq!(lease.share(&mut c, Attempt(1), "second"), Err(Error::Busy));
    assert_eq!(
        lease.recover(
            &mut c,
            Attempt(2),
            Some(RestoreReason::ConsumptionConfirmed)
        ),
        Ok(Recovery::Nothing)
    );
    assert_eq!(payload(&c), b"first");
    lease
        .recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested))
        .unwrap();
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Ok(Recovery::Nothing)
    );
    lease.share(&mut c, Attempt(2), "second").unwrap();
    assert_eq!(
        lease.recover(
            &mut c,
            Attempt(1),
            Some(RestoreReason::ConsumptionConfirmed)
        ),
        Ok(Recovery::Nothing)
    );
    assert_eq!(payload(&c), b"second");
}

#[test]
fn an_external_clipboard_change_releases_the_old_remote_lease_for_retry() {
    let mut c = original();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "first").unwrap();
    c.revision += 1;
    c.formats = vec![Format {
        name: "public.utf8-plain-text".into(),
        bytes: b"copied elsewhere".to_vec(),
    }];

    lease.share(&mut c, Attempt(2), "second").unwrap();

    assert_eq!(payload(&c), b"second");
    assert_eq!(c.writes, 2);
    assert!(lease.pending());
}
#[test]
fn partial_copy_failure_keeps_recovery_data_and_never_reports_success() {
    let mut c = Fake {
        partial: true,
        ..original()
    };
    let mut lease = Lease::default();
    assert_eq!(
        lease.share(&mut c, Attempt(1), "reviewed"),
        Err(Error::WriteFailed)
    );
    assert!(lease.pending());
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Ok(Recovery::Restored)
    );
    assert_eq!(payload(&c), b"original");
}
#[test]
fn partial_restore_failure_can_retry_using_our_new_revision() {
    let mut c = original();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "reviewed").unwrap();
    c.partial = true;
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Err(Error::WriteFailed)
    );
    assert!(lease.pending());
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Ok(Recovery::Restored)
    );
    assert_eq!(payload(&c), b"original");
}
#[test]
fn unchanged_restore_failure_does_not_discard_recovery_data() {
    let mut c = original();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "reviewed").unwrap();
    c.unavailable = true;
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Err(Error::Unavailable)
    );
    assert!(lease.pending());
    c.unavailable = false;
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Ok(Recovery::Restored)
    );
}
#[test]
fn empty_clipboard_is_restored_as_empty() {
    let mut c = Fake::default();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "reviewed").unwrap();
    lease
        .recover(
            &mut c,
            Attempt(1),
            Some(RestoreReason::ConsumptionConfirmed),
        )
        .unwrap();
    assert!(c.formats.is_empty());
}
#[test]
fn invalid_transcripts_never_reach_the_system_clipboard() {
    for text in [
        "",
        " ",
        "hi\n",
        "hi\r",
        "hi\t",
        "hi\0",
        "hi\u{2028}",
        "hi\u{2029}",
    ] {
        let mut c = original();
        assert_eq!(
            Lease::default().share(&mut c, Attempt(1), text),
            Err(Error::InvalidText)
        );
        assert_eq!(c.writes, 0);
    }
}

#[test]
fn consecutive_reviewed_copies_preserve_the_original_recovery_snapshot() {
    let mut c = original();
    let mut lease = Lease::default();
    lease.share(&mut c, Attempt(1), "first").unwrap();
    lease.share(&mut c, Attempt(2), "second").unwrap();
    assert_eq!(payload(&c), b"second");
    assert_eq!(
        lease.recover(&mut c, Attempt(1), Some(RestoreReason::UserRequested)),
        Ok(Recovery::Nothing)
    );
    lease
        .recover(&mut c, Attempt(2), Some(RestoreReason::UserRequested))
        .unwrap();
    assert_eq!(payload(&c), b"original");
}
