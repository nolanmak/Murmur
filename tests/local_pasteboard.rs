#[cfg(target_os = "macos")]
fn main() {
    use murmur::{
        local_clipboard::{Board, Failure, Lease},
        platform::macos_local_clipboard::MacLocalBoard,
    };
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString};
    let board = NSPasteboard::pasteboardWithUniqueName();
    board.clearContents();
    let a = NSPasteboardItem::new();
    let b = NSPasteboardItem::new();
    let plain = NSString::from_str("public.utf8-plain-text");
    let rich = NSString::from_str("public.rtf");
    assert!(a.setString_forType(&NSString::from_str("one"), &plain));
    assert!(a.setString_forType(&NSString::from_str("{\\rtf1 one}"), &rich));
    assert!(b.setString_forType(&NSString::from_str("two"), &plain));
    let refs: [&ProtocolObject<dyn NSPasteboardWriting>; 2] =
        [ProtocolObject::from_ref(&*a), ProtocolObject::from_ref(&*b)];
    assert!(board.writeObjects(&NSArray::from_slice(&refs)));
    let main = objc2::MainThreadMarker::new().expect("native test on main thread");
    let mut adapter = MacLocalBoard::new(board.clone(), main, || true);
    let before = adapter.snapshot().unwrap();
    assert_eq!(before.items.len(), 2);
    assert!(before.items[0].formats.len() >= 2);
    let mut lease = Lease::default();
    lease.send(&mut adapter, "Café 👋").unwrap();
    assert_eq!(board.stringForType(&plain).unwrap().to_string(), "Café 👋");
    lease.restore(&mut adapter).unwrap();
    let after = adapter.snapshot().unwrap();
    assert!(before.items == after.items);
    assert_eq!(board.pasteboardItems().unwrap().len(), 2);
    lease.send(&mut adapter, "pending").unwrap();
    board.clearContents();
    assert!(board.setString_forType(&NSString::from_str("newer"), &plain));
    assert_eq!(lease.restore(&mut adapter), Err(Failure::Changed));
    assert_eq!(board.stringForType(&plain).unwrap().to_string(), "newer");
    board.clearContents();
    println!("local native clipboard item round-trip and newer owner checks passed");
}
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("SKIP: native pasteboard test requires macOS");
}
