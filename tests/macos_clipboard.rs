#![cfg(target_os = "macos")]
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};
use text_to_speech::{
    platform::macos_clipboard::MacClipboard, remote::Attempt, remote_clipboard::*,
};

struct Board(Retained<NSPasteboard>);
impl Board {
    fn new() -> Self {
        Self(NSPasteboard::pasteboardWithUniqueName())
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.0.clearContents();
    }
}
fn text_type() -> Retained<NSString> {
    NSString::from_str("public.utf8-plain-text")
}
fn set(board: &NSPasteboard, text: &str) {
    board.clearContents();
    assert!(board.setString_forType(&NSString::from_str(text), &text_type()));
}
#[test]
fn native_adapter_round_trips_unicode_and_multiple_materialized_formats() {
    let board = Board::new();
    set(&board.0, "original");
    let rtf = NSString::from_str("public.rtf");
    unsafe {
        board.0.addTypes_owner(&NSArray::from_slice(&[&*rtf]), None);
    }
    assert!(
        board
            .0
            .setData_forType(Some(&NSData::with_bytes(br"{\rtf1 original}")), &rtf)
    );
    let mut clipboard = MacClipboard::new(board.0.clone());
    let mut lease = Lease::default();
    lease
        .share(&mut clipboard, Attempt(1), "Café 👋  日本語")
        .unwrap();
    assert_eq!(
        board.0.stringForType(&text_type()).unwrap().to_string(),
        "Café 👋  日本語"
    );
    assert_eq!(
        lease.recover(&mut clipboard, Attempt(1), None),
        Ok(Recovery::Retained)
    );
    assert_eq!(
        lease.recover(
            &mut clipboard,
            Attempt(1),
            Some(RestoreReason::ConsumptionConfirmed)
        ),
        Ok(Recovery::Restored)
    );
    assert_eq!(
        board.0.stringForType(&text_type()).unwrap().to_string(),
        "original"
    );
    assert_eq!(
        board.0.dataForType(&rtf).unwrap().to_vec(),
        br"{\rtf1 original}"
    );
}
#[test]
fn native_adapter_preserves_newer_owner_and_refuses_stale_writes() {
    let board = Board::new();
    set(&board.0, "original");
    let mut clipboard = MacClipboard::new(board.0.clone());
    let mut lease = Lease::default();
    let stale = clipboard.snapshot().unwrap().revision;
    lease.share(&mut clipboard, Attempt(1), "reviewed").unwrap();
    set(&board.0, "external");
    assert_eq!(
        lease.recover(
            &mut clipboard,
            Attempt(1),
            Some(RestoreReason::UserRequested)
        ),
        Ok(Recovery::OwnershipLost)
    );
    assert_eq!(
        clipboard.replace(stale, "bad"),
        Err(WriteFailure::OwnershipLost)
    );
    assert_eq!(
        board.0.stringForType(&text_type()).unwrap().to_string(),
        "external"
    );
}
#[test]
fn native_adapter_rejects_multiple_items_without_modifying_them() {
    let board = Board::new();
    board.0.clearContents();
    let a = NSPasteboardItem::new();
    let b = NSPasteboardItem::new();
    assert!(a.setString_forType(&NSString::from_str("one"), &text_type()));
    assert!(b.setString_forType(&NSString::from_str("two"), &text_type()));
    let objects: [&ProtocolObject<dyn NSPasteboardWriting>; 2] =
        [ProtocolObject::from_ref(&*a), ProtocolObject::from_ref(&*b)];
    assert!(board.0.writeObjects(&NSArray::from_slice(&objects)));
    let before = board.0.changeCount();
    let mut clipboard = MacClipboard::new(board.0.clone());
    assert_eq!(
        Lease::default().share(&mut clipboard, Attempt(1), "reviewed"),
        Err(Error::Unsupported)
    );
    assert_eq!(board.0.changeCount(), before);
    assert_eq!(board.0.pasteboardItems().unwrap().len(), 2);
}
#[test]
fn native_adapter_restores_an_empty_clipboard() {
    let board = Board::new();
    board.0.clearContents();
    let mut clipboard = MacClipboard::new(board.0.clone());
    let mut lease = Lease::default();
    lease.share(&mut clipboard, Attempt(1), "reviewed").unwrap();
    lease
        .recover(
            &mut clipboard,
            Attempt(1),
            Some(RestoreReason::UserRequested),
        )
        .unwrap();
    assert!(board.0.types().is_none_or(|types| types.is_empty()));
}
