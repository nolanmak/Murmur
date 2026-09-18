//! macOS-only boundary: AppKit shell, Control event tap and accessibility insertion.
//! All retained CF/AX objects stay on the main thread. Workers receive no UI pointers.
use crate::{
    core::{CompletionEffect, Dictation, Phase},
    indicator::{self, Indicator, Look},
    insertion::{Target, allowed, same_window},
    local_clipboard::Lease as LocalLease,
    local_delivery::{Failure as DeliveryFailure, Outcome as DeliveryOutcome},
    platform::macos_local_clipboard::MacLocalBoard,
};
use block2::RcBlock;
use fotw_audio::{AudioPlatform, DeviceId, FormatRequest, Permission, PermissionState};
use muda::{Menu, MenuEvent, MenuItem};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBox, NSBoxType,
    NSButton, NSColor, NSFont, NSFontWeightMedium, NSLineBreakMode, NSPanel, NSPasteboard,
    NSRunningApplication, NSScreen, NSScrollView, NSTextField, NSTextView, NSTitlePosition, NSView,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};
use std::{
    cell::RefCell,
    ffi::{CString, c_char, c_void},
    ptr::{self, NonNull},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tray_icon::{TrayIcon, TrayIconBuilder};
type CF = *const c_void;
const LOCAL_FIXTURE_TEXT: &str = "Murmur fixture Café 👋";
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateSystemWide() -> CF;
    fn AXUIElementCreateApplication(pid: i32) -> CF;
    fn AXUIElementCopyAttributeValue(element: CF, attribute: CF, value: *mut CF) -> i32;
    fn AXUIElementSetAttributeValue(element: CF, attribute: CF, value: CF) -> i32;
    fn AXUIElementGetPid(element: CF, pid: *mut i32) -> i32;
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events: u64,
        callback: unsafe extern "C" fn(CF, u32, CF, *mut c_void) -> CF,
        info: *mut c_void,
    ) -> CF;
    fn CGEventGetFlags(event: CF) -> u64;
    fn CGEventGetIntegerValueField(event: CF, field: u32) -> i64;
    fn CGEventTapEnable(tap: CF, enable: bool);
    fn CGPreflightListenEventAccess() -> bool;
    fn CGEventCreateKeyboardEvent(source: CF, key: u16, down: bool) -> CF;
    fn CGEventSetFlags(event: CF, flags: u64);
    fn CGEventPost(tap: u32, event: CF);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CF);
    fn CFEqual(a: CF, b: CF) -> bool;
    fn CFHash(value: CF) -> usize;
    fn CFStringCreateWithCString(allocator: CF, text: *const c_char, encoding: u32) -> CF;
    fn CFStringGetCString(value: CF, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFMachPortCreateRunLoopSource(allocator: CF, port: CF, order: isize) -> CF;
    fn CFRunLoopGetMain() -> CF;
    fn CFRunLoopAddSource(runloop: CF, source: CF, mode: CF);
    fn CFRunLoopRemoveSource(runloop: CF, source: CF, mode: CF);
    static kCFRunLoopCommonModes: CF;
}
#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}
struct Owned(CF);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
fn cfstr(s: &str) -> Owned {
    let s = CString::new(s).expect("static AX name or validated text");
    Owned(unsafe { CFStringCreateWithCString(ptr::null(), s.as_ptr(), 0x08000100) })
}
fn attribute(element: CF, name: &str) -> Option<Owned> {
    let mut out = ptr::null();
    let key = cfstr(name);
    let code = unsafe { AXUIElementCopyAttributeValue(element, key.0, &mut out) };
    (code == 0 && !out.is_null()).then(|| Owned(out))
}
fn string(value: CF) -> String {
    let mut bytes = [0u8; 256];
    if unsafe {
        CFStringGetCString(
            value,
            bytes.as_mut_ptr().cast(),
            bytes.len() as isize,
            0x08000100,
        )
    } {
        String::from_utf8_lossy(&bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())])
            .into()
    } else {
        String::new()
    }
}
struct Focus {
    element: Owned,
    window: Option<Owned>,
    target: Target,
    paste_only: bool,
}
impl Focus {
    fn current() -> Result<Self, String> {
        if !unsafe { AXIsProcessTrusted() } {
            return Err("Enable Accessibility in System Settings, then relaunch".into());
        }
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        let element = attribute(system.0, "AXFocusedUIElement")
            .ok_or("Click an editable text field first")?;
        let mut pid = 0;
        unsafe { AXUIElementGetPid(element.0, &mut pid) };
        let role = attribute(element.0, "AXRole")
            .map(|s| string(s.0))
            .unwrap_or_default();
        let subrole = attribute(element.0, "AXSubrole")
            .map(|s| string(s.0))
            .unwrap_or_default();
        let secure = role.contains("Secure")
            || subrole.contains("Secure")
            || role.contains("Password")
            || subrole.contains("Password");
        let bundle = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .and_then(|app| app.bundleIdentifier())
            .map(|id| id.to_string())
            .unwrap_or_default();
        let paste_only = crate::insertion::terminal_surface(&bundle, &role, &subrole)
            || crate::insertion::browser_surface(&bundle, &role, &subrole);
        let editable = crate::insertion::text_role(&role, &subrole) || paste_only;
        eprintln!("focus role={role} editable={editable} secure={secure} paste={paste_only}");
        let window = attribute(element.0, "AXWindow");
        let target = Target {
            pid,
            element: unsafe { CFHash(element.0) } as u64,
            secure,
            editable,
        };
        Ok(Self {
            element,
            window,
            target,
            paste_only,
        })
    }
    fn insert(
        &self,
        text: &str,
        lease: &mut LocalLease,
    ) -> Result<DeliveryOutcome, DeliveryFailure> {
        let now = Self::current().map_err(|_| DeliveryFailure::TargetChanged)?;
        if !allowed(&self.target, &now.target, text)
            || !unsafe { CFEqual(self.element.0, now.element.0) }
            || !same_window(
                self.window.as_ref(),
                now.window.as_ref(),
                |before, after| unsafe { CFEqual(before.0, after.0) },
            )
        {
            return Err(if now.target.secure {
                DeliveryFailure::SecureTarget
            } else if !now.target.editable {
                DeliveryFailure::UnsupportedTarget
            } else {
                DeliveryFailure::TargetChanged
            });
        }
        let key = cfstr("AXSelectedText");
        let value = cfstr(text);
        if self.paste_only {
            return paste_text(text, lease, self);
        }
        if unsafe { AXUIElementSetAttributeValue(now.element.0, key.0, value.0) } != 0 {
            return paste_text(text, lease, self);
        }
        Ok(DeliveryOutcome::AxWrite)
    }
}
fn paste_text(
    text: &str,
    lease: &mut LocalLease,
    focus: &Focus,
) -> Result<DeliveryOutcome, DeliveryFailure> {
    let main = MainThreadMarker::new().ok_or(DeliveryFailure::PasteDispatchFailed)?;
    // Creating events before changing the board prevents a missing shortcut from
    // leaving a new clipboard owner behind.
    let down = Owned(unsafe { CGEventCreateKeyboardEvent(ptr::null(), 9, true) });
    let up = Owned(unsafe { CGEventCreateKeyboardEvent(ptr::null(), 9, false) });
    if down.0.is_null() || up.0.is_null() {
        return Err(DeliveryFailure::PasteDispatchFailed);
    }
    let mut board = MacLocalBoard::new(NSPasteboard::generalPasteboard(), main, move || {
        let Ok(now) = Focus::current() else {
            return false;
        };
        if !allowed(&focus.target, &now.target, text)
            || !unsafe { CFEqual(focus.element.0, now.element.0) }
            || !same_window(
                focus.window.as_ref(),
                now.window.as_ref(),
                |before, after| unsafe { CFEqual(before.0, after.0) },
            )
        {
            return false;
        }
        unsafe {
            CGEventSetFlags(down.0, 1 << 20);
            CGEventSetFlags(up.0, 1 << 20);
            CGEventPost(1, down.0);
            CGEventPost(1, up.0);
        }
        true // Event dispatch is not confirmation that the target consumed text.
    });
    lease
        .send(&mut board, text)
        .map_err(|reason| match reason {
            crate::local_clipboard::Failure::PendingRestore => DeliveryFailure::PendingRestore,
            crate::local_clipboard::Failure::UnsupportedClipboard => {
                DeliveryFailure::UnsupportedClipboard
            }
            crate::local_clipboard::Failure::Changed => DeliveryFailure::ClipboardChanged,
            crate::local_clipboard::Failure::WriteFailed => DeliveryFailure::ClipboardWriteFailed,
            crate::local_clipboard::Failure::DispatchFailed => DeliveryFailure::PasteDispatchFailed,
        })?;
    Ok(DeliveryOutcome::PasteSent)
}

#[derive(Clone, Copy)]
enum Input {
    Flags(bool, bool),
    HotkeyDown,
    HotkeyAbort,
    Cancel,
    TapDisabled,
}
struct TapContext {
    sender: mpsc::SyncSender<Input>,
    dropped: AtomicBool,
    space: RefCell<crate::space_hotkey::HoldSpace>,
    origin: Instant,
    busy: std::cell::Cell<bool>,
    control_down: std::cell::Cell<bool>,
}
struct EventTap {
    port: Owned,
    source: Owned,
    _context: Box<TapContext>,
}
fn send_command(context: &TapContext, command: crate::space_hotkey::Command) {
    use crate::space_hotkey::Command;
    let input = match command {
        Command::Start => Input::Flags(true, false),
        Command::Finish => Input::Flags(false, false),
        Command::Cancel => Input::Cancel,
    };
    if context.sender.try_send(input).is_err() {
        context.dropped.store(true, Ordering::Release);
    }
}
unsafe extern "C" fn callback(_proxy: CF, kind: u32, event: CF, info: *mut c_void) -> CF {
    use crate::space_hotkey::Key;
    let context = unsafe { &*(info as *const TapContext) };
    if matches!(kind, 0xffff_fffe | 0xffff_ffff) {
        let _ = context.sender.try_send(Input::TapDisabled);
        return event;
    }
    let flags = unsafe { CGEventGetFlags(event) };
    let code = unsafe { CGEventGetIntegerValueField(event, 9) };
    let other_modifier = flags & ((1 << 17) | (1 << 19) | (1 << 20) | (1 << 23)) != 0;
    let key = match (kind, code) {
        // Control is represented as flagsChanged; use the flag so both left and right
        // Control work on keyboards whose modifier keycodes differ.
        (12, _) if flags & (1 << 18) != 0 && !context.control_down.get() => {
            context.control_down.set(true);
            if other_modifier || context.busy.get() {
                Key::ModifiedDown
            } else {
                Key::Down
            }
        }
        (12, _) if flags & (1 << 18) == 0 && context.control_down.get() => {
            context.control_down.set(false);
            Key::Up
        }
        (12, _) if flags & (1 << 18) != 0 && context.control_down.get() && other_modifier => {
            Key::Other
        }
        (10, 53) => Key::Escape,
        (10, _) if context.control_down.get() => Key::Other,
        _ => return event,
    };
    let Ok(mut space) = context.space.try_borrow_mut() else {
        return event;
    };
    let action = space.key(key, context.origin.elapsed().as_millis() as u64);
    drop(space);
    if let Some(command) = action.command {
        send_command(context, command);
    }
    if matches!(key, Key::Down) {
        let _ = context.sender.try_send(Input::HotkeyDown);
    }
    if action.replay {
        let _ = context.sender.try_send(Input::HotkeyAbort);
    }
    event
}
impl EventTap {
    fn new(sender: mpsc::SyncSender<Input>) -> Option<Self> {
        let mut context = Box::new(TapContext {
            sender,
            dropped: AtomicBool::new(false),
            space: RefCell::new(Default::default()),
            origin: Instant::now(),
            busy: std::cell::Cell::new(false),
            control_down: std::cell::Cell::new(false),
        });
        let port = unsafe {
            CGEventTapCreate(
                1,
                0,
                1, // Listen only: Control does not need to suppress or replay typing.
                (1 << 12) | (1 << 10),
                callback,
                (&mut *context as *mut TapContext).cast(),
            )
        };
        if port.is_null() {
            return None;
        }
        let port = Owned(port);
        let source = unsafe { CFMachPortCreateRunLoopSource(ptr::null(), port.0, 0) };
        if source.is_null() {
            return None;
        }
        let source = Owned(source);
        unsafe {
            CFRunLoopAddSource(CFRunLoopGetMain(), source.0, kCFRunLoopCommonModes);
            CGEventTapEnable(port.0, true)
        };
        Some(Self {
            port,
            source,
            _context: context,
        })
    }
}
impl Drop for EventTap {
    fn drop(&mut self) {
        unsafe {
            CGEventTapEnable(self.port.0, false);
            CFRunLoopRemoveSource(CFRunLoopGetMain(), self.source.0, kCFRunLoopCommonModes)
        }
    }
}
struct Completion {
    generation: u64,
    result: Result<String, String>,
}
fn shape(mtm: MainThreadMarker, parent: &NSView, radius: f64) -> Retained<NSBox> {
    let shape = NSBox::new(mtm);
    shape.setBoxType(NSBoxType::Custom);
    shape.setTitlePosition(NSTitlePosition::NoTitle);
    shape.setBorderWidth(0.0);
    shape.setCornerRadius(radius);
    shape.setFillColor(&NSColor::whiteColor());
    shape.setHidden(true);
    parent.addSubview(&shape);
    shape
}
/// Click-through pill anchored bottom-centre; eases between looks on the pump timer.
struct Pill {
    panel: Retained<NSPanel>,
    body: Retained<NSBox>,
    bars: Vec<Retained<NSBox>>,
    dots: Vec<Retained<NSBox>>,
    label: Retained<NSTextField>,
    look: Look,
    size: (f64, f64),
    frame: NSRect,
    text_width: f64,
    content: bool,
}
impl Pill {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        let size = Look::Idle.size(0.0);
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(size.0, size.1)),
            NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setLevel(25);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setHidesOnDeactivate(false);
        panel.setIgnoresMouseEvents(true);
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        unsafe { panel.setReleasedWhenClosed(false) };
        let content = panel.contentView().ok_or("No panel content")?;
        let body = shape(mtm, &content, size.1 / 2.0);
        body.setBorderWidth(0.5);
        body.setHidden(false);
        let bars = (0..indicator::BARS)
            .map(|_| shape(mtm, &content, indicator::BAR_WIDTH / 2.0))
            .collect();
        let dots = (0..indicator::DOTS)
            .map(|_| shape(mtm, &content, indicator::DOT_SIZE / 2.0))
            .collect();
        let label = NSTextField::labelWithString(&NSString::new(), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, unsafe {
            NSFontWeightMedium
        })));
        label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.96, 1.0)));
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        label.setHidden(true);
        content.addSubview(&label);
        let mut pill = Self {
            panel,
            body,
            bars,
            dots,
            label,
            look: Look::Idle,
            size,
            frame: NSRect::ZERO,
            text_width: 0.0,
            content: false,
        };
        pill.paint();
        pill.place();
        pill.panel.orderFrontRegardless();
        Ok(pill)
    }
    fn set_text(&mut self, text: &str) {
        if self.label.stringValue().to_string() == text {
            return;
        }
        self.label.setStringValue(&NSString::from_str(text));
        self.text_width = self.label.intrinsicContentSize().width;
        self.label.setHidden(true);
        self.content = false;
    }
    fn paint(&self) {
        let white = |alpha| NSColor::colorWithWhite_alpha(1.0, alpha);
        let (fill, border) = match self.look {
            Look::Idle => (NSColor::colorWithWhite_alpha(0.0, 0.5), white(0.2)),
            Look::Warning => (
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.62, 0.1, 0.92),
                white(0.25),
            ),
            Look::Armed => (NSColor::colorWithWhite_alpha(0.25, 0.7), white(0.3)),
            Look::Success => (
                NSColor::colorWithSRGBRed_green_blue_alpha(0.2, 0.8, 0.4, 0.95),
                white(0.25),
            ),
            _ => (NSColor::colorWithWhite_alpha(0.06, 0.88), white(0.16)),
        };
        self.body.setFillColor(&fill);
        self.body.setBorderColor(&border);
    }
    fn place(&mut self) {
        let Some(screen) = NSScreen::mainScreen(self.panel.mtm()) else {
            return;
        };
        let visible = screen.visibleFrame();
        // Even widths keep the centre on a whole point while easing.
        let (width, height) = ((self.size.0 / 2.0).round() * 2.0, self.size.1.round());
        let frame = NSRect::new(
            NSPoint::new(
                (visible.origin.x + (visible.size.width - width) / 2.0).round(),
                (visible.origin.y + 9.0).round(),
            ),
            NSSize::new(width, height),
        );
        if frame != self.frame {
            self.frame = frame;
            self.panel.setFrame_display(frame, true);
            self.body
                .setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(width, height)));
            self.body.setCornerRadius(height / 2.0);
        }
    }
    fn render(&mut self, look: Look, now: u64) {
        if look != self.look {
            self.look = look;
            self.paint();
            self.content = false;
            for view in self.bars.iter().chain(&self.dots) {
                view.setHidden(true);
            }
            self.label.setHidden(true);
            self.panel.orderFrontRegardless();
        }
        let target = look.size(self.text_width);
        if self.size != target {
            self.size = (
                indicator::ease(self.size.0, target.0),
                indicator::ease(self.size.1, target.1),
            );
            self.place();
        }
        if !self.content && self.size == target {
            self.content = true;
            self.reveal();
        }
        if self.content {
            self.animate(now);
        }
    }
    fn reveal(&self) {
        let (width, height) = (self.frame.size.width, self.frame.size.height);
        match self.look {
            Look::Listening => self.bars.iter().for_each(|bar| bar.setHidden(false)),
            Look::Starting | Look::Processing => {
                let pitch = indicator::DOT_SIZE + indicator::DOT_GAP;
                let left = (width - indicator::DOTS as f64 * pitch + indicator::DOT_GAP) / 2.0;
                for (i, dot) in self.dots.iter().enumerate() {
                    let x = left + i as f64 * pitch;
                    dot.setFrame(NSRect::new(
                        NSPoint::new(x, (height - indicator::DOT_SIZE) / 2.0),
                        NSSize::new(indicator::DOT_SIZE, indicator::DOT_SIZE),
                    ));
                    dot.setAlphaValue(0.9);
                    dot.setHidden(false);
                }
            }
            Look::Notice => {
                // The label cell insets its text by 2pt on each side.
                let inset = indicator::NOTICE_PADDING - 2.0;
                let line = self.label.intrinsicContentSize().height.ceil();
                self.label.setFrame(NSRect::new(
                    NSPoint::new(inset, ((height - line) / 2.0).round()),
                    NSSize::new(width - 2.0 * inset, line),
                ));
                self.label.setHidden(false);
            }
            _ => {}
        }
    }
    fn animate(&self, now: u64) {
        let (width, height) = (self.frame.size.width, self.frame.size.height);
        match self.look {
            Look::Listening => {
                let pitch = indicator::BAR_WIDTH + indicator::BAR_GAP;
                let left =
                    (width - indicator::BARS as f64 * pitch + indicator::BAR_GAP).round() / 2.0;
                for (i, bar) in self.bars.iter().enumerate() {
                    let bar_height = indicator::bar_height(i, now);
                    bar.setFrame(NSRect::new(
                        NSPoint::new(left + i as f64 * pitch, (height - bar_height).round() / 2.0),
                        NSSize::new(indicator::BAR_WIDTH, bar_height),
                    ));
                }
            }
            Look::Starting => {
                for (i, dot) in self.dots.iter().enumerate() {
                    dot.setAlphaValue(indicator::dot_alpha(i, now));
                }
            }
            Look::Processing => {
                for (i, dot) in self.dots.iter().enumerate() {
                    let mut origin = dot.frame().origin;
                    origin.y = (height - indicator::DOT_SIZE) / 2.0 + indicator::dot_lift(i, now);
                    dot.setFrameOrigin(origin);
                }
            }
            _ => {}
        }
    }
}
struct Shell {
    core: Dictation,
    control: Arc<AtomicU8>,
    focus: Option<Focus>,
    input: mpsc::Receiver<Input>,
    sender: mpsc::SyncSender<Input>,
    tap: Option<EventTap>,
    result: mpsc::Receiver<Completion>,
    completed: mpsc::Sender<Completion>,
    _tray: TrayIcon,
    status: MenuItem,
    pill: Pill,
    indicator: Indicator,
    warning: Option<&'static str>,
    last: String,
    started: Instant,
    retry_at: Instant,
    smoke: bool,
    smoke_started: bool,
    launched: Instant,
    receiving: Arc<AtomicBool>,
    remote_mode: bool,
    remote_toggle: MenuItem,
    remote_profile: MenuItem,
    profile: crate::remote::Profile,
    review: crate::remote_review::Review,
    clipboard_lease: crate::remote_clipboard::Lease,
    local_lease: LocalLease,
    local_fixture: bool,
    fixture_target: Option<Focus>,
    clipboard_attempt: Option<crate::remote::Attempt>,
    selected_remote: crate::remote_review::Selection<RustDeskWindow>,
}
impl Shell {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        // Opt-in native integration fixture. No credentials or microphone are used.
        let remote_fixture = std::env::args().any(|arg| arg == "--remote-fixture");
        let local_fixture =
            cfg!(debug_assertions) && std::env::args().any(|arg| arg == "--local-fixture");
        let menu = Menu::new();
        let status = MenuItem::with_id("status", "Hold Control to dictate", false, None);
        let cancel = MenuItem::with_id("cancel", "Cancel dictation (Esc)", true, None);
        let start = MenuItem::with_id("start", "Start dictation", true, None);
        let stop = MenuItem::with_id("stop", "Stop dictation", true, None);
        let copy = MenuItem::with_id(
            "copy",
            "Copy Last Transcript (replaces clipboard)",
            true,
            None,
        );
        let setup = MenuItem::with_id("setup", "Set up permissions", true, None);
        let quit = MenuItem::with_id("quit", "Quit Murmur", true, None);
        let remote_toggle = MenuItem::with_id("remote_mode", "Remote review mode: Off", true, None);
        if remote_fixture {
            remote_toggle.set_text("Remote review mode: On");
        }
        let remote_review = MenuItem::with_id("remote_review", "Review for RustDesk…", true, None);
        let remote_profile = MenuItem::with_id(
            "remote_profile",
            "Remote paste shortcut: macOS ⌘V",
            true,
            None,
        );
        let restore =
            MenuItem::with_id("remote_restore", "Restore previous clipboard…", true, None);
        let fixture_paste =
            MenuItem::with_id("fixture_paste", "Insert synthetic test phrase", true, None);
        let fixture_capture = MenuItem::with_id(
            "fixture_capture",
            "Capture synthetic test target",
            true,
            None,
        );
        let fixture_send = MenuItem::with_id(
            "fixture_send",
            "Insert at captured synthetic target",
            true,
            None,
        );
        let fixture_complete = MenuItem::with_id(
            "fixture_complete",
            "Simulate synthetic dictation result",
            true,
            None,
        );
        let fixture_empty = MenuItem::with_id(
            "fixture_empty",
            "Simulate empty dictation result",
            true,
            None,
        );
        let fixture_duplicate = MenuItem::with_id(
            "fixture_duplicate",
            "Simulate duplicate dictation result",
            true,
            None,
        );
        let local_restore = MenuItem::with_id(
            "local_restore",
            "Restore clipboard from local paste…",
            true,
            None,
        );
        for item in [
            &status,
            &start,
            &stop,
            &cancel,
            &copy,
            &remote_toggle,
            &remote_profile,
            &remote_review,
            &restore,
            &local_restore,
            &setup,
            &quit,
        ] {
            menu.append(item).map_err(|e| e.to_string())?;
        }
        if local_fixture {
            menu.append(&fixture_paste).map_err(|e| e.to_string())?;
            menu.append(&fixture_capture).map_err(|e| e.to_string())?;
            menu.append(&fixture_send).map_err(|e| e.to_string())?;
            menu.append(&fixture_complete).map_err(|e| e.to_string())?;
            menu.append(&fixture_empty).map_err(|e| e.to_string())?;
            menu.append(&fixture_duplicate).map_err(|e| e.to_string())?;
        }
        let tray = TrayIconBuilder::new()
            .with_title("Control")
            .with_tooltip("Murmur")
            .with_menu(Box::new(menu.clone()))
            .build()
            .map_err(|e| e.to_string())?;
        let pill = Pill::new(mtm)?;
        let (sender, input) = mpsc::sync_channel(128);
        let tap = EventTap::new(sender.clone());
        eprintln!(
            "permissions accessibility={} input_monitoring={} keyboard_listener={}",
            unsafe { AXIsProcessTrusted() },
            unsafe { CGPreflightListenEventAccess() },
            tap.is_some()
        );
        let (completed, result) = mpsc::channel();
        Ok(Self {
            core: Dictation::default(),
            control: Arc::new(AtomicU8::new(2)),
            focus: None,
            input,
            sender,
            tap,
            result,
            completed,
            _tray: tray,
            status,
            pill,
            indicator: Indicator::default(),
            warning: None,
            last: if local_fixture {
                LOCAL_FIXTURE_TEXT.into()
            } else if remote_fixture {
                "Synthetic dictation test — Café 👋".into()
            } else {
                String::new()
            },
            started: Instant::now(),
            retry_at: Instant::now(),
            smoke: std::env::args().any(|a| a == "--smoke"),
            smoke_started: false,
            launched: Instant::now(),
            receiving: Arc::new(AtomicBool::new(false)),
            remote_mode: remote_fixture,
            remote_toggle,
            remote_profile,
            profile: crate::remote::Profile::Mac,
            review: Default::default(),
            clipboard_lease: Default::default(),
            local_lease: LocalLease::default(),
            local_fixture,
            fixture_target: None,
            clipboard_attempt: None,
            selected_remote: Default::default(),
        })
    }
    fn status(&self, message: &str) {
        eprintln!("status: {message}");
        self.status.set_text(message);
    }
    fn show(&mut self, message: &str) {
        self.status(message);
        if let Some(text) = self.indicator.message(message, self.now()) {
            self.pill.set_text(&text);
        }
    }
    fn now(&self) -> u64 {
        self.launched.elapsed().as_millis() as u64
    }
    fn check_keyboard(&mut self) {
        let warning = if self.tap.is_none() {
            Some("Keyboard access unavailable — use Start dictation in the Control menu")
        } else if !unsafe { CGPreflightListenEventAccess() } {
            Some("Keyboard permission needed — enable Input Monitoring in System Settings")
        } else {
            None
        };
        if warning != self.warning {
            self.warning = warning;
            if warning.is_some() || self.core.phase == Phase::Idle {
                self.status(self.ready());
            }
        }
    }
    fn ready(&self) -> &'static str {
        match self.warning {
            Some(warning) => warning,
            None if self.remote_mode => "Remote mode: use Start/Stop in menu",
            None => "Hold Control to dictate",
        }
    }
    fn cancel(&mut self) {
        self.review.cancel();
        self.control.store(2, Ordering::Release);
        if self.core.cancel() {
            self.focus = None;
            self.show("Cancelled");
        }
    }
    fn start(&mut self) {
        let focus = match self.preflight_focus() {
            Ok(focus) => focus,
            Err(reason) => {
                self.cancel();
                self.show(reason.message());
                return;
            }
        };
        let platform = fotw_audio::platform::macos::MacOsPlatform::new();
        if platform.permission(Permission::Microphone) != PermissionState::Granted {
            request_microphone();
            self.cancel();
            self.show("Use Set up permissions in the Control menu, then try again.");
            return;
        }
        self.begin_attempt(focus, "◌ Starting microphone…");
        let control = self.control.clone();
        let completed = self.completed.clone();
        let generation = self.core.generation();
        let receiving = self.receiving.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let credentials = crate::config::load()?;
                eprintln!("credentials loaded via {}", credentials.source);
                let platform = fotw_audio::platform::macos::MacOsPlatform::new();
                let tap = platform
                    .open_mic(&DeviceId::new("default"), FormatRequest::any())
                    .map_err(|_| "No microphone available".to_string())?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "Cannot start audio runtime".to_string())?;
                runtime.block_on(crate::capture::run_observed(
                    tap,
                    credentials.key.expose().into(),
                    control,
                    fotw_stt::DeepgramEndpoint::production(),
                    receiving,
                ))
            })();
            let _ = completed.send(Completion { generation, result });
        });
    }
    fn preflight_focus(&self) -> Result<Option<Focus>, DeliveryFailure> {
        match Focus::current() {
            Ok(f) if !f.target.secure && f.target.editable => Ok((!self.remote_mode).then_some(f)),
            Ok(f) if f.target.secure => Err(DeliveryFailure::SecureTarget),
            Ok(_) if !self.remote_mode => Err(DeliveryFailure::UnsupportedTarget),
            _ => Ok(None),
        }
    }
    fn begin_attempt(&mut self, focus: Option<Focus>, status: &str) {
        self.focus = focus;
        self.control = Arc::new(AtomicU8::new(0));
        self.receiving = Arc::new(AtomicBool::new(false));
        self.started = Instant::now();
        self.last.clear();
        self.indicator.clear();
        self.status(status);
    }
    fn simulate_completion(&mut self, text: &str, duplicate: bool) {
        if !self.local_fixture || self.remote_mode || !self.core.start_manual() {
            return;
        }
        let focus = match self.preflight_focus() {
            Ok(focus) => focus,
            Err(reason) => {
                self.cancel();
                self.show(reason.message());
                return;
            }
        };
        self.begin_attempt(focus, "Synthetic result queued");
        let generation = self.core.generation();
        self.core.stop();
        let _ = self.completed.send(Completion {
            generation,
            result: Ok(text.into()),
        });
        if duplicate {
            let _ = self.completed.send(Completion {
                generation,
                result: Ok("Late synthetic duplicate".into()),
            });
        }
    }
    fn finish(&mut self) {
        self.control.store(1, Ordering::Release);
        self.status("Finishing… · Esc to cancel");
    }
    fn pump(&mut self) {
        self.observe_remote_window();
        if self.smoke && !self.smoke_started && self.launched.elapsed() > Duration::from_secs(2) {
            self.smoke_started = true;
            if self.core.start_manual() {
                self.start();
            }
        }
        if self.smoke
            && self.core.phase == Phase::Recording
            && self.started.elapsed() > Duration::from_secs(10)
            && self.core.stop()
        {
            self.finish();
        }
        if let Some(tap) = &self.tap {
            tap._context.busy.set(self.core.phase == Phase::Processing);
            let action = tap
                ._context
                .space
                .borrow_mut()
                .tick(tap._context.origin.elapsed().as_millis() as u64);
            if let Some(command) = action.command {
                send_command(&tap._context, command);
            }
        }

        if self
            .tap
            .as_ref()
            .is_some_and(|tap| tap._context.dropped.swap(false, Ordering::AcqRel))
        {
            self.cancel();
            self.show("Input queue overflow; dictation cancelled");
        }
        while let Ok(event) = self.input.try_recv() {
            match event {
                Input::HotkeyDown | Input::HotkeyAbort if self.remote_mode => {}
                Input::Flags(_, _) if self.remote_mode => {
                    self.show("Remote mode: use Start/Stop in menu")
                }
                Input::HotkeyDown => {
                    let now = self.now();
                    self.indicator.arm(now)
                }
                Input::HotkeyAbort => self.indicator.disarm(),
                Input::Flags(down, other) => match self.core.flags(down, other) {
                    Some("start") => self.start(),
                    Some("finish") => self.finish(),
                    Some("cancel") => {
                        self.control.store(2, Ordering::Release);
                        self.focus = None;
                        self.show("Cancelled Control shortcut")
                    }
                    _ => {}
                },
                Input::Cancel => self.cancel(),
                Input::TapDisabled => {
                    self.cancel();
                    if let Some(tap) = &self.tap {
                        unsafe { CGEventTapEnable(tap.port.0, true) }
                    }
                }
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id.as_ref() {
                "remote_mode" => {
                    if self.local_lease.pending() || self.clipboard_lease.pending() {
                        self.show("Restore previous clipboard before changing modes");
                        continue;
                    }
                    self.cancel();
                    self.remote_mode = !self.remote_mode;
                    self.remote_toggle.set_text(if self.remote_mode {
                        "Remote review mode: On"
                    } else {
                        "Remote review mode: Off"
                    });
                    if self.remote_mode {
                        self.show("Remote mode: use Start/Stop in menu");
                    } else {
                        self.indicator.clear();
                        self.status(self.ready());
                    }
                }
                "remote_profile" => {
                    use crate::remote::Profile;
                    self.profile = match self.profile {
                        Profile::Mac => Profile::Linux,
                        Profile::Linux => Profile::LinuxTerminal,
                        Profile::LinuxTerminal => Profile::Mac,
                    };
                    self.remote_profile.set_text(format!(
                        "Remote paste shortcut: {}",
                        profile_name(self.profile)
                    ));
                }
                "remote_review" if self.core.phase == Phase::Idle && self.remote_mode => {
                    self.review_remote()
                }
                "remote_restore" if self.core.phase == Phase::Idle => {
                    self.restore_remote_clipboard()
                }
                "fixture_paste" if self.local_fixture && self.core.phase == Phase::Idle => {
                    let result = Focus::current()
                        .map_err(|_| DeliveryFailure::MissingTarget)
                        .and_then(|target| {
                            target.insert(LOCAL_FIXTURE_TEXT, &mut self.local_lease)
                        });
                    match result {
                        Ok(outcome) => self.show(outcome.message()),
                        Err(reason) => self.show(reason.message()),
                    }
                }
                "fixture_capture" if self.local_fixture && self.core.phase == Phase::Idle => {
                    match Focus::current() {
                        Ok(target) if target.target.secure => {
                            self.fixture_target = None;
                            self.show(DeliveryFailure::SecureTarget.message());
                        }
                        Ok(target) if !target.target.editable => {
                            self.fixture_target = None;
                            self.show(DeliveryFailure::UnsupportedTarget.message());
                        }
                        Ok(target) => {
                            self.fixture_target = Some(target);
                            self.show("Synthetic target captured");
                        }
                        Err(_) => {
                            self.fixture_target = None;
                            self.show(DeliveryFailure::MissingTarget.message());
                        }
                    }
                }
                "fixture_send" if self.local_fixture && self.core.phase == Phase::Idle => {
                    let result = self
                        .fixture_target
                        .take()
                        .ok_or(DeliveryFailure::MissingTarget)
                        .and_then(|target| {
                            target.insert(LOCAL_FIXTURE_TEXT, &mut self.local_lease)
                        });
                    match result {
                        Ok(outcome) => self.show(outcome.message()),
                        Err(reason) => self.show(reason.message()),
                    }
                }
                "fixture_complete" if self.local_fixture => {
                    self.simulate_completion(LOCAL_FIXTURE_TEXT, false)
                }
                "fixture_empty" if self.local_fixture => self.simulate_completion("", false),
                "fixture_duplicate" if self.local_fixture => {
                    self.simulate_completion(LOCAL_FIXTURE_TEXT, true)
                }
                "local_restore" if self.core.phase == Phase::Idle && !self.remote_mode => {
                    self.restore_local_clipboard();
                }
                "start" if self.core.start_manual() => {
                    self.start();
                }
                "stop" if self.core.stop() => {
                    self.finish();
                }
                "cancel" => self.cancel(),
                "copy" if self.remote_mode && self.core.phase == Phase::Idle => {
                    self.review_remote()
                }
                "copy" if !self.last.is_empty() => {
                    self.copy_local_transcript();
                }
                "setup" => {
                    self.cancel();
                    request_microphone();
                    let _=std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility").spawn();
                    self.show("Enable Accessibility. Keep Wispr Flow on Fn; hold Control here.");
                }
                "quit" => {
                    self.control.store(2, Ordering::Release);
                    std::process::exit(0)
                }
                _ => {}
            }
        }
        if self.core.phase == Phase::Recording && self.started.elapsed() >= Duration::from_secs(120)
        {
            self.core.stop();
            self.finish();
        }
        while let Ok(done) = self.result.try_recv() {
            let Some(effect) = self.core.complete(done.generation, done.result) else {
                continue;
            };
            match effect {
                CompletionEffect::Transcript(text) => {
                    eprintln!(
                        "transcription complete: {} characters",
                        text.chars().count()
                    );
                    self.last = text.clone();
                    if self.remote_mode {
                        self.show("Transcript ready · Review for RustDesk…");
                    } else {
                        let result = self
                            .focus
                            .as_ref()
                            .ok_or(DeliveryFailure::MissingTarget)
                            .and_then(|f| f.insert(&text, &mut self.local_lease));
                        match result {
                            Ok(outcome) => self.show(outcome.message()),
                            Err(reason) => self.show(reason.message()),
                        }
                    }
                }
                CompletionEffect::Empty => self.show("No speech detected"),
                CompletionEffect::Error(e) => self.show(&e),
            }
            self.focus = None;
        }
        if self.retry_at.elapsed() > Duration::from_secs(3) {
            if self.tap.is_none() {
                self.tap = EventTap::new(self.sender.clone());
            }
            self.retry_at = Instant::now();
            self.check_keyboard();
            self.pill.place();
        }
        let now = self.now();
        let receiving = self.receiving.load(Ordering::Acquire);
        let look = self
            .indicator
            .look(now, self.core.phase, receiving, self.warning.is_some());
        if self.pill.look == Look::Success
            && matches!(look, Look::Idle | Look::Warning | Look::Armed)
        {
            self.status(self.ready());
        }
        self.pill.render(look, now);
    }
}
fn profile_name(profile: crate::remote::Profile) -> &'static str {
    use crate::remote::Profile;
    match profile {
        Profile::Mac => "macOS ⌘V",
        Profile::Linux => "Linux Ctrl+V",
        Profile::LinuxTerminal => "Linux terminal Ctrl+Shift+V",
    }
}
/// This is a window observation, not proof of an active remote connection.
struct RustDeskWindow {
    element: Owned,
    token: crate::remote_review::Window,
    title: String,
}
impl RustDeskWindow {
    fn selected() -> Option<Self> {
        if !unsafe { AXIsProcessTrusted() } {
            return None;
        }
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        let app = attribute(system.0, "AXFocusedApplication")?;
        let mut process = 0;
        unsafe { AXUIElementGetPid(app.0, &mut process) };
        let bundle = NSRunningApplication::runningApplicationWithProcessIdentifier(process)?
            .bundleIdentifier()?;
        if !crate::remote_review::rustdesk_bundle(&bundle.to_string()) {
            return None;
        }
        let element = attribute(app.0, "AXFocusedWindow")?;
        let title = string(attribute(element.0, "AXTitle")?.0);
        if !title.ends_with(" - Remote Desktop - RustDesk") {
            return None;
        }
        let token = crate::remote_review::Window {
            process,
            element: unsafe { CFHash(element.0) } as u64,
        };
        Some(Self {
            element,
            token,
            title,
        })
    }
    fn revalidate(&self) -> Option<crate::remote_review::Window> {
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        let foreground = attribute(system.0, "AXFocusedApplication")?;
        let mut pid = 0;
        unsafe { AXUIElementGetPid(foreground.0, &mut pid) };
        if pid != self.token.process && pid != std::process::id() as i32 {
            return None;
        }
        let app = Owned(unsafe { AXUIElementCreateApplication(self.token.process) });
        let window = attribute(app.0, "AXFocusedWindow")?;
        let title = string(attribute(window.0, "AXTitle")?.0);
        (unsafe { CFEqual(window.0, self.element.0) } && title == self.title).then_some(self.token)
    }
}
impl Shell {
    fn observe_remote_window(&mut self) {
        use crate::remote_review::Foreground;
        if self.last.is_empty() {
            self.selected_remote.observe(Foreground::Other);
            return;
        }
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        let foreground = attribute(system.0, "AXFocusedApplication");
        let mut pid = 0;
        if let Some(app) = foreground {
            unsafe { AXUIElementGetPid(app.0, &mut pid) };
        }
        if pid == std::process::id() as i32 {
            self.selected_remote.observe(Foreground::ReviewUi);
        } else if let Some(window) = RustDeskWindow::selected() {
            self.selected_remote
                .observe(Foreground::Destination(window));
        } else {
            self.selected_remote.observe(Foreground::Other);
        }
    }
    fn review_remote(&mut self) {
        use crate::remote_review::Error;
        if self.clipboard_lease.pending() {
            self.show("Restore the previous clipboard before copying another transcript");
            return;
        }
        self.observe_remote_window();
        let selected = self.selected_remote.take();
        let id = match self
            .review
            .prepare(&self.last, selected.as_ref().map(|s| s.token))
        {
            Ok(id) => id,
            Err(Error::InvalidText) => {
                self.show(
                    "Record a single-line transcript first; remote copy rejects control characters",
                );
                return;
            }
            Err(_) => {
                remote_selection_diagnostic();
                self.show(
                    "Focus the intended RustDesk remote window, then choose Review for RustDesk",
                );
                return;
            }
        };
        let selected = selected.expect("review validated selected window");
        let mtm = MainThreadMarker::new().expect("shell main thread");
        let Some(confirmed) = review_dialog(&selected.title, &self.last, self.profile, mtm) else {
            self.review.cancel();
            self.show("Remote copy cancelled");
            return;
        };
        let text = match self.review.confirm(id, selected.revalidate(), confirmed) {
            Ok(text) => text,
            Err(Error::ConfirmationNeeded) => {
                self.review.cancel();
                self.show("Confirm the intended session before sharing; nothing copied");
                return;
            }
            Err(_) => {
                self.show("RustDesk window changed; review again before sharing");
                return;
            }
        };
        let attempt = crate::remote::Attempt(id.0);
        let mut clipboard =
            super::macos_clipboard::MacClipboard::new(NSPasteboard::generalPasteboard(), mtm);
        let result = self.clipboard_lease.share(&mut clipboard, attempt, &text);
        if self.clipboard_lease.pending() {
            self.clipboard_attempt = Some(attempt);
        }
        match result {
            Ok(()) => self.show("Copied · paste manually in RustDesk"),
            Err(crate::remote_clipboard::Error::Unsupported) => self.show("Clipboard format cannot be preserved; keep transcript and try after copying plain text"),
            Err(_) => self.show("Copy failed or clipboard changed; use Restore previous clipboard if available"),
        }
    }
    fn restore_local_clipboard(&mut self) {
        if !self.local_lease.pending() {
            self.show("No local clipboard recovery pending");
            return;
        }
        let main = MainThreadMarker::new().expect("app runs on main thread");
        let mut board = MacLocalBoard::new(NSPasteboard::generalPasteboard(), main, || false);
        match self.local_lease.restore(&mut board) {
            Ok(()) => self.show("Previous clipboard restored"),
            Err(crate::local_clipboard::Failure::Changed) => {
                self.show("Clipboard changed; newer copy kept")
            }
            Err(_) => self.show("Clipboard restore failed; retry from Control menu"),
        }
    }
    fn copy_local_transcript(&mut self) {
        let main = MainThreadMarker::new().expect("app runs on main thread");
        let mut board = MacLocalBoard::new(NSPasteboard::generalPasteboard(), main, || false);
        match crate::local_delivery::copy_transcript(
            &mut board,
            self.local_lease.pending(),
            &self.last,
        ) {
            Ok(()) => self.show("Transcript copied · paste manually"),
            Err(reason) => self.show(reason.message()),
        }
    }
    fn restore_remote_clipboard(&mut self) {
        use crate::remote_clipboard::{Recovery, RestoreReason};
        let Some(attempt) = self.clipboard_attempt else {
            self.show("No previous clipboard to restore");
            return;
        };
        let mtm = MainThreadMarker::new().expect("shell main thread");
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Restore previous clipboard?"));
        alert.setInformativeText(&NSString::from_str("Finish your manual paste first. This changes the local clipboard and may sync through RustDesk. A newer clipboard copy will be kept. Recovery is available only until this app quits."));
        alert.addButtonWithTitle(&NSString::from_str("Restore"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() != 1000 {
            return;
        }
        let mut clipboard =
            super::macos_clipboard::MacClipboard::new(NSPasteboard::generalPasteboard(), mtm);
        match self.clipboard_lease.recover(
            &mut clipboard,
            attempt,
            Some(RestoreReason::UserRequested),
        ) {
            Ok(Recovery::Restored) => self.show("Previous clipboard restored"),
            Ok(Recovery::OwnershipLost) => self.show("Newer clipboard kept"),
            Ok(_) => self.show("No previous clipboard to restore"),
            Err(_) => self.show("Restore failed; recovery retained for retry"),
        }
        if !self.clipboard_lease.pending() {
            self.clipboard_attempt = None;
        }
    }
}
fn remote_selection_diagnostic() {
    let system = Owned(unsafe { AXUIElementCreateSystemWide() });
    let key = cfstr("AXFocusedApplication");
    let mut raw = ptr::null();
    let result = unsafe { AXUIElementCopyAttributeValue(system.0, key.0, &mut raw) };
    let app = Owned(raw);
    if result != 0 || app.0.is_null() {
        eprintln!("remote_selection focused_app_error={result}");
        return;
    }
    let mut pid = 0;
    unsafe { AXUIElementGetPid(app.0, &mut pid) };
    let rustdesk = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|app| app.bundleIdentifier())
        .is_some_and(|id| crate::remote_review::rustdesk_bundle(&id.to_string()));
    let window = attribute(app.0, "AXFocusedWindow");
    let supported_title = window
        .as_ref()
        .and_then(|w| attribute(w.0, "AXTitle"))
        .is_some_and(|title| string(title.0).ends_with(" - Remote Desktop - RustDesk"));
    eprintln!(
        "remote_selection own_app={} rustdesk={} window={} supported_title={}",
        pid == std::process::id() as i32,
        rustdesk,
        window.is_some(),
        supported_title
    );
}
fn review_dialog(
    title: &str,
    transcript: &str,
    profile: crate::remote::Profile,
    mtm: MainThreadMarker,
) -> Option<bool> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Review for RustDesk"));
    alert.setInformativeText(&NSString::from_str(&format!(
            "Selected window: {}\n\nCopy shares text through RustDesk clipboard sync. Disconnect other sessions first. Connection, remote focus, and receipt cannot be verified here.\n\nAfter copying, focus the intended remote field and paste using {}. No paste shortcut or Enter will be sent. The remote clipboard may change.",
            title, profile_name(profile))));
    let content = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(480.0, 220.0)),
    );
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 45.0), NSSize::new(480.0, 175.0)),
    );
    scroll.setHasVerticalScroller(true);
    let text = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(455.0, 175.0)),
    );
    text.setString(&NSString::from_str(transcript));
    text.setEditable(false);
    text.setSelectable(true);
    text.setVerticallyResizable(true);
    scroll.setDocumentView(Some(&text));
    content.addSubview(&scroll);
    // SAFETY: This checkbox has no target or action; its state is read after the modal closes.
    let confirmation = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &NSString::from_str("Only my intended session is connected; its target field is safe."),
            None,
            None,
            mtm,
        )
    };
    confirmation.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(480.0, 40.0),
    ));
    content.addSubview(&confirmation);
    alert.setAccessoryView(Some(&content));
    alert.addButtonWithTitle(&NSString::from_str("Copy for manual paste"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    (alert.runModal() == 1000).then(|| confirmation.state() == 1)
}
/// Development-only UI smoke path: synthetic text, no microphone, credentials, or clipboard access.
pub fn preview_remote_review() -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("AppKit requires the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    let result = review_dialog(
        "Synthetic test window",
        "Café 👋 — This is a synthetic review fixture. No text will be copied or sent.",
        crate::remote::Profile::LinuxTerminal,
        mtm,
    );
    println!(
        "Review preview: {}",
        match result {
            Some(true) => "confirmed",
            Some(false) => "confirmation missing",
            None => "cancelled",
        }
    );
    Ok(())
}
fn request_microphone() {
    let handler = RcBlock::new(|_granted: objc2::runtime::Bool| {});
    if let Some(class) = objc2::runtime::AnyClass::get(c"AVCaptureDevice") {
        // AVFoundation owns a copied completion block. No UI is touched off-thread.
        unsafe {
            let _: () = objc2::msg_send![class,requestAccessForMediaType:&*NSString::from_str("soun"),completionHandler:&*handler];
        }
    }
}
pub fn run() -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("AppKit requires the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    let shell = Rc::new(RefCell::new(Shell::new(mtm)?));
    shell.borrow_mut().check_keyboard();
    if shell.borrow().remote_mode {
        shell
            .borrow_mut()
            .show("Synthetic fixture · Review for RustDesk");
    }
    let weak = Rc::downgrade(&shell);
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        if let Some(shell) = weak.upgrade()
            && let Ok(mut s) = shell.try_borrow_mut()
        {
            s.pump();
        }
    });
    let _timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.02, true, &block) };
    app.run();
    Ok(())
}
