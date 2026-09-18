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
    let empty = adapter.snapshot().unwrap();
    assert!(empty.items.is_empty());
    lease.send(&mut adapter, "empty case").unwrap();
    lease.restore(&mut adapter).unwrap();
    assert!(adapter.snapshot().unwrap().items.is_empty());

    board.clearContents();
    assert!(board.setString_forType(&NSString::from_str("single"), &plain));
    let single = adapter.snapshot().unwrap();
    assert_eq!(single.items.len(), 1);
    lease.send(&mut adapter, "single case").unwrap();
    lease.restore(&mut adapter).unwrap();
    assert!(adapter.snapshot().unwrap().items == single.items);

    board.clearContents();
    let many: Vec<_> = (0..33)
        .map(|_| {
            let item = NSPasteboardItem::new();
            assert!(item.setString_forType(&NSString::from_str("synthetic"), &plain));
            item
        })
        .collect();
    let refs: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = many
        .iter()
        .map(|item| ProtocolObject::from_ref(&**item))
        .collect();
    assert!(board.writeObjects(&NSArray::from_slice(&refs)));
    let revision = board.changeCount();
    assert!(matches!(
        adapter.snapshot(),
        Err(Failure::UnsupportedClipboard)
    ));
    assert_eq!(board.changeCount(), revision);
    board.clearContents();
    println!(
        "local native clipboard empty/single/multi-item, bounds and newer owner checks passed"
    );
}
#[cfg(not(target_os = "macos"))]
fn main() {
    println!("SKIP: native pasteboard test requires macOS");
}
