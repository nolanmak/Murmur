use murmur::local_clipboard::{
    Board, Failure, Format, Item, Lease, Revision, Snapshot, WriteFailure,
};

#[derive(Default)]
struct FakeBoard {
    revision: u64,
    items: Vec<Item>,
    fail_write: bool,
    fail_before_clear: bool,
    fail_dispatch: bool,
    mutate_before_replace: bool,
    mutate_after_clear: bool,
    unavailable_snapshot: bool,
    changed_snapshot: bool,
    dispatches: usize,
}
impl Board for FakeBoard {
    fn snapshot(&mut self) -> Result<Snapshot, Failure> {
        if self.unavailable_snapshot {
            return Err(Failure::UnsupportedClipboard);
        }
        if self.changed_snapshot {
            self.revision += 1;
            return Err(Failure::Changed);
        }
        Ok(Snapshot {
            revision: Revision(self.revision),
            items: self.items.clone(),
        })
    }
    fn replace(&mut self, expected: Revision, items: &[Item]) -> Result<Revision, WriteFailure> {
        if self.mutate_before_replace {
            self.revision += 1;
            self.mutate_before_replace = false;
        }
        if self.revision != expected.0 {
            return Err(WriteFailure::Changed);
        }
        if self.fail_before_clear {
            return Err(WriteFailure::Unchanged);
        }
        self.revision += 1;
        if self.mutate_after_clear {
            self.revision += 1;
            self.items = vec![text("newer owner")];
            self.mutate_after_clear = false;
            return Err(WriteFailure::Changed);
        }
        if self.fail_write {
            return Err(WriteFailure::Owned(Revision(self.revision)));
        }
        self.items = items.to_vec();
        Ok(Revision(self.revision))
    }
    fn dispatch_paste(&mut self) -> bool {
        self.dispatches += 1;
        !self.fail_dispatch
    }
}
fn text(value: &str) -> Item {
    Item {
        formats: vec![Format {
            kind: "public.utf8-plain-text".into(),
            data: value.as_bytes().to_vec(),
        }],
    }
}
#[test]
fn two_items_are_preserved_until_explicit_restore_even_after_delayed_paste() {
    let mut board = FakeBoard {
        items: vec![text("one"), text("two")],
        ..Default::default()
    };
    let mut lease = Lease::default();
    lease.send(&mut board, "Café 👋").unwrap();
    assert_eq!(board.dispatches, 1);
    assert!(board.items == vec![text("Café 👋")]);
    assert!(lease.pending());
    assert!(board.items == vec![text("Café 👋")]); // no one-second timer
    lease.restore(&mut board).unwrap();
    assert!(board.items == vec![text("one"), text("two")]);
}
#[test]
fn newer_copy_is_never_restored_over_and_failed_write_keeps_recovery() {
    let mut board = FakeBoard {
        items: vec![text("original")],
        ..Default::default()
    };
    let mut lease = Lease::default();
    board.fail_write = true;
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::WriteFailed));
    assert!(lease.pending());
    board.fail_write = false;
    board.revision += 1;
    board.items = vec![text("newer")];
    assert_eq!(lease.restore(&mut board), Err(Failure::Changed));
    assert!(board.items == vec![text("newer")]);
    assert_eq!(board.dispatches, 0);
}
#[test]
fn pending_recovery_blocks_next_write_and_oversize_snapshot_leaves_board_untouched() {
    let mut board = FakeBoard {
        items: vec![text("old")],
        ..Default::default()
    };
    let mut lease = Lease::default();
    lease.send(&mut board, "first").unwrap();
    assert_eq!(
        lease.send(&mut board, "second"),
        Err(Failure::PendingRestore)
    );
    lease.restore(&mut board).unwrap();
    board.items = (0..33).map(|_| text("x")).collect();
    let before = board.revision;
    assert_eq!(
        lease.send(&mut board, "third"),
        Err(Failure::UnsupportedClipboard)
    );
    assert_eq!(board.revision, before);
}

#[test]
fn an_external_clipboard_change_releases_the_old_lease_for_the_next_paste() {
    let mut board = FakeBoard {
        items: vec![text("old")],
        ..Default::default()
    };
    let mut lease = Lease::default();
    lease.send(&mut board, "first").unwrap();
    board.revision += 1;
    board.items = vec![text("copied elsewhere")];

    lease.send(&mut board, "second").unwrap();

    assert!(board.items == vec![text("second")]);
    assert_eq!(board.dispatches, 2);
    assert!(lease.pending());
}

#[test]
fn revision_change_before_replacement_preserves_existing_items() {
    let mut board = FakeBoard {
        items: vec![text("old")],
        mutate_before_replace: true,
        ..Default::default()
    };
    let mut lease = Lease::default();
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::Changed));
    assert!(board.items == vec![text("old")]);
    assert!(!lease.pending());
    assert_eq!(board.dispatches, 0);
}
#[test]
fn failed_dispatch_retains_recovery_and_retry_never_dispatches_twice() {
    let mut board = FakeBoard {
        items: vec![text("old")],
        fail_dispatch: true,
        ..Default::default()
    };
    let mut lease = Lease::default();
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::DispatchFailed));
    assert!(lease.pending());
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::PendingRestore));
    assert_eq!(board.dispatches, 1);
    lease.restore(&mut board).unwrap();
    assert!(board.items == vec![text("old")]);
}
#[test]
fn oversized_single_item_is_rejected_before_replacement() {
    let mut big = text("x");
    big.formats[0].data = vec![b'x'; 8 * 1024 * 1024 + 1];
    let mut board = FakeBoard {
        items: vec![big],
        ..Default::default()
    };
    let mut lease = Lease::default();
    assert_eq!(
        lease.send(&mut board, "test"),
        Err(Failure::UnsupportedClipboard)
    );
    assert_eq!(board.revision, 0);
    assert_eq!(board.dispatches, 0);
}

#[test]
fn unavailable_or_too_many_formats_never_mutate_clipboard() {
    let mut board = FakeBoard {
        items: vec![text("old")],
        unavailable_snapshot: true,
        ..Default::default()
    };
    let mut lease = Lease::default();
    assert_eq!(
        lease.send(&mut board, "test"),
        Err(Failure::UnsupportedClipboard)
    );
    assert_eq!(board.revision, 0);
    board.unavailable_snapshot = false;
    board.items[0].formats = (0..33).map(|_| text("x").formats.remove(0)).collect();
    assert_eq!(
        lease.send(&mut board, "test"),
        Err(Failure::UnsupportedClipboard)
    );
    assert_eq!(board.revision, 0);
}
#[test]
fn aggregate_size_limit_rejects_three_individually_supported_items() {
    let mut board = FakeBoard {
        items: vec![text("a"), text("b"), text("c")],
        ..Default::default()
    };
    for item in &mut board.items {
        item.formats[0].data = vec![b'x'; 6 * 1024 * 1024];
    }
    let mut lease = Lease::default();
    assert_eq!(
        lease.send(&mut board, "test"),
        Err(Failure::UnsupportedClipboard)
    );
    assert_eq!(board.revision, 0);
}

#[test]
fn changed_snapshot_and_failed_preclear_write_never_dispatch_or_take_a_lease() {
    let mut board = FakeBoard {
        items: vec![text("original")],
        changed_snapshot: true,
        ..Default::default()
    };
    let mut lease = Lease::default();
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::Changed));
    assert!(!lease.pending());
    assert_eq!(board.dispatches, 0);
    assert!(board.items == vec![text("original")]);

    board.changed_snapshot = false;
    board.fail_before_clear = true;
    let revision = board.revision;
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::WriteFailed));
    assert!(!lease.pending());
    assert_eq!(board.revision, revision);
    assert_eq!(board.dispatches, 0);
    assert!(board.items == vec![text("original")]);
}

#[test]
fn a_new_owner_after_clear_wins_without_paste_or_restore() {
    let mut board = FakeBoard {
        items: vec![text("original")],
        mutate_after_clear: true,
        ..Default::default()
    };
    let mut lease = Lease::default();
    assert_eq!(lease.send(&mut board, "test"), Err(Failure::Changed));
    assert!(!lease.pending());
    assert_eq!(board.dispatches, 0);
    assert!(board.items == vec![text("newer owner")]);
}

#[test]
fn a_partial_restore_failure_retains_recovery_at_its_new_revision() {
    let mut board = FakeBoard {
        items: vec![text("original")],
        ..Default::default()
    };
    let mut lease = Lease::default();
    lease.send(&mut board, "test").unwrap();
    board.fail_write = true;
    assert_eq!(lease.restore(&mut board), Err(Failure::WriteFailed));
    assert!(lease.pending());
    let failed_revision = board.revision;
    board.fail_write = false;
    lease.restore(&mut board).unwrap();
    assert!(board.revision > failed_revision);
    assert!(board.items == vec![text("original")]);
    assert!(!lease.pending());
}

#[test]
fn a_restore_rejected_before_clear_can_be_retried_without_losing_the_snapshot() {
    let mut board = FakeBoard {
        items: vec![text("original")],
        ..Default::default()
    };
    let mut lease = Lease::default();
    lease.send(&mut board, "test").unwrap();
    let owned_revision = board.revision;
    board.fail_before_clear = true;
    assert_eq!(lease.restore(&mut board), Err(Failure::WriteFailed));
    assert!(lease.pending());
    assert_eq!(board.revision, owned_revision);
    assert!(board.items == vec![text("test")]);
    board.fail_before_clear = false;
    lease.restore(&mut board).unwrap();
    assert!(board.items == vec![text("original")]);
}
