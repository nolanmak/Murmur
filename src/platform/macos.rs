//! macOS-only boundary: AppKit shell, Control event tap and accessibility insertion.
//! All retained CF/AX objects stay on the main thread. Workers receive no UI pointers.
use crate::vocabulary::ProfileFile as _;
use crate::{
    core::{CompletionEffect, Dictation, Phase},
    indicator::{self, Indicator, Look},
    insertion::{
        Preflight, Probe, Target, allowed, preflight, prewarm_attributes, same_window, settle_focus,
    },
    local_clipboard::Lease as LocalLease,
    local_delivery::{Failure as DeliveryFailure, Outcome as DeliveryOutcome},
    permissions::{self, Access, Grant, Onboarding, Request, Route, Snapshot},
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
    fn CGRequestListenEventAccess() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CF) -> bool;
    static kAXTrustedCheckOptionPrompt: CF;
    fn CGEventCreateKeyboardEvent(source: CF, key: u16, down: bool) -> CF;
    fn CGEventSetFlags(event: CF, flags: u64);
    fn CGEventPost(tap: u32, event: CF);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CF);
    static kCFBooleanTrue: CF;
    fn CFEqual(a: CF, b: CF) -> bool;
    fn CFHash(value: CF) -> usize;
    fn CFStringCreateWithCString(allocator: CF, text: *const c_char, encoding: u32) -> CF;
    fn CFStringGetCString(value: CF, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFMachPortCreateRunLoopSource(allocator: CF, port: CF, order: isize) -> CF;
    fn CFRunLoopGetMain() -> CF;
    fn CFRunLoopAddSource(runloop: CF, source: CF, mode: CF);
    fn CFRunLoopRemoveSource(runloop: CF, source: CF, mode: CF);
    static kCFRunLoopCommonModes: CF;
    fn CFDictionaryCreate(
        allocator: CF,
        keys: *const CF,
        values: *const CF,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CF;
    fn CFDictionaryGetValue(dictionary: CF, key: CF) -> CF;
    fn CFNumberGetValue(number: CF, kind: isize, value: *mut c_void) -> bool;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
}
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopySelf(flags: u32, code: *mut CF) -> i32;
    fn SecCodeCopySigningInformation(code: CF, flags: u32, information: *mut CF) -> i32;
    static kSecCodeInfoFlags: CF;
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
}
impl Focus {
    fn current() -> Result<Self, String> {
        if !unsafe { AXIsProcessTrusted() } {
            return Err("Enable Accessibility in System Settings, then relaunch".into());
        }
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        let focused_app = attribute(system.0, "AXFocusedApplication");
        if let Some(app) = focused_app.as_ref() {
            // Chromium/Electron may not expose focused editors until requested.
            // Try capabilities rather than maintaining an app-name allowlist.
            // Unsupported attributes are ignored; a real focused input is still required.
            for name in ["AXEnhancedUserInterface", "AXManualAccessibility"] {
                let key = cfstr(name);
                unsafe { AXUIElementSetAttributeValue(app.0, key.0, kCFBooleanTrue) };
            }
        }
        let element = attribute(system.0, "AXFocusedUIElement")
            .or_else(|| {
                focused_app
                    .as_ref()
                    .and_then(|app| attribute(app.0, "AXFocusedUIElement"))
            })
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
        let editable = crate::insertion::native_paste_surface(&bundle, &role, &subrole);
        eprintln!("focus role={role} editable={editable} secure={secure} paste={editable}");
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
        paste_text(text, lease, self)
    }
}
fn probe(focus: &Focus) -> Probe {
    match (focus.target.secure, focus.target.editable) {
        (true, _) => Probe::Secure,
        (_, true) => Probe::Editable,
        _ => Probe::NotEditable,
    }
}
fn focused_app_pid() -> i32 {
    let system = Owned(unsafe { AXUIElementCreateSystemWide() });
    let mut pid = 0;
    if let Some(app) = attribute(system.0, "AXFocusedApplication") {
        unsafe { AXUIElementGetPid(app.0, &mut pid) };
    }
    pid
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
/// A spoken engine command and what the worker learned before the UI applies it.
struct SpokenCommand {
    command: crate::engine_command::Command,
    local: Result<(), String>,
    cloud: Result<(), String>,
    offline_all: bool,
}
enum Spoken {
    Text(crate::cleanup::Outcome),
    Command(
        String,
        crate::engine_command::Command,
        Result<(), String>,
        Result<(), String>,
    ),
}
enum InstallEvent {
    Progress(String),
    Done(Result<(), String>),
}
struct Completion {
    command: Option<SpokenCommand>,
    cleanup: Option<crate::cleanup::Disposition>,
    input_failed: bool,
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
#[derive(Clone, Copy)]
enum LoginRetry {
    // Re-read the saved preference; startup may not have been able to inspect it.
    Sync,
    SetEnabled(bool),
}

struct Shell {
    core: Dictation,
    sleep_events: super::macos_sleep::SleepEvents,
    control: Arc<AtomicU8>,
    focus: Option<Focus>,
    input: mpsc::Receiver<Input>,
    sender: mpsc::SyncSender<Input>,
    tap: Option<EventTap>,
    result: mpsc::Receiver<Completion>,
    completed: mpsc::Sender<Completion>,
    _tray: TrayIcon,
    status: MenuItem,
    speech: crate::speech::Selection,
    transcription_menu: super::macos_transcription::TranscriptionMenu,
    /// Status lines from an explicit Parakeet model install.
    install_events: Option<mpsc::Receiver<InstallEvent>>,
    /// The engine before the last switch, for "Murmur, undo".
    previous_provider: Option<crate::speech::Provider>,
    language_menu: super::macos_language::LanguageMenu,
    vocabulary_menu: super::macos_vocabulary::VocabularyMenu,
    cleanup_menu: super::macos_cleanup::CleanupMenu,
    cleanup_service: crate::cleanup_http::Service,
    _native_menu: Menu,
    pill: Pill,
    indicator: Indicator,
    warning: Option<String>,
    permissions: Snapshot,
    onboarding: Onboarding,
    prompted_at: Instant,
    last: crate::transcript::Latest,
    started: Instant,
    released: Instant,
    retry_at: Instant,
    smoke: bool,
    smoke_started: bool,
    launched: Instant,
    receiving: Arc<AtomicBool>,
    microphone_startup: crate::capture::MicrophoneStartup,
    microphone_notice: Option<&'static str>,
    microphone_readiness: crate::readiness::Monitor<fotw_audio::platform::macos::InputSnapshot>,
    readiness_status: crate::readiness::Status,
    readiness_focus: i32,
    readiness_reason: Option<String>,
    speech_progress: crate::fallback::Progress,
    speech_stage: crate::fallback::Stage,
    remote_mode: bool,
    recording_remote: bool,
    recording_remote_target: Option<RustDeskWindow>,
    remote_toggle: MenuItem,
    remote_profile: MenuItem,
    /// Present only when launched from an app bundle; `cargo run` never installs.
    login: Option<(crate::login_item::LoginItem, std::path::PathBuf)>,
    login_toggle: MenuItem,
    login_retry: Option<LoginRetry>,
    profile: crate::remote::Profile,
    review: crate::remote_review::Review,
    clipboard_lease: crate::remote_clipboard::Lease,
    local_lease: LocalLease,
    local_fixture: bool,
    fixture_target: Option<Focus>,
    clipboard_attempt: Option<crate::remote::Attempt>,
    selected_remote: crate::remote_review::Selection<RustDeskWindow>,
    /// Last app asked to expose accessibility, when, and when focus was last checked.
    prewarmed: (i32, Instant),
    prewarm_checked: Instant,
}
impl Shell {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        // Opt-in native integration fixture. No credentials or microphone are used.
        let remote_fixture = std::env::args().any(|arg| arg == "--remote-fixture");
        let local_fixture =
            cfg!(debug_assertions) && std::env::args().any(|arg| arg == "--local-fixture");
        let menu = Menu::new();
        let status = MenuItem::with_id("status", "Hold Control to dictate", false, None);
        let speech = crate::speech::Selection::load(crate::speech::Settings::path());
        if speech.error().is_some() {
            status.set_text(INVALID_SPEECH);
        }
        let transcription_menu = super::macos_transcription::TranscriptionMenu::new()?;
        let language_menu =
            super::macos_language::LanguageMenu::new(crate::vocabulary::Profile::path())?;
        let vocabulary_menu =
            super::macos_vocabulary::VocabularyMenu::new(crate::vocabulary::Profile::path())?;
        let cleanup_menu = super::macos_cleanup::CleanupMenu::new(
            crate::cleanup_config::Settings::path(),
            crate::cleanup_config::NetworkPolicy::path(),
        )?;
        transcription_menu.refresh(
            speech.provider(),
            cleanup_menu.speech_label(
                speech_label(speech.provider()),
                offline_label(speech.settings()),
            ),
        );
        let native_menu = Menu::new();
        let application_menu = muda::Submenu::new("Murmur", true);
        application_menu
            .append(&muda::PredefinedMenuItem::hide(None))
            .map_err(|e| e.to_string())?;
        native_menu
            .append(&application_menu)
            .map_err(|e| e.to_string())?;
        native_menu
            .append(&super::macos_vocabulary::edit_menu()?)
            .map_err(|e| e.to_string())?;
        native_menu.init_for_nsapp();
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
        let reopen = MenuItem::with_id("reopen", "Reopen Murmur", true, None);
        let quit = MenuItem::with_id("quit", "Quit Murmur", true, None);
        let login = std::env::current_exe()
            .ok()
            .and_then(|exe| crate::login_item::app_bundle(&exe))
            .zip(crate::login_item::LoginItem::for_user())
            .map(|(bundle, item)| (item, bundle));
        let login_retry = login.as_ref().and_then(|(item, bundle)| {
            if let Err(error) = item.sync(bundle) {
                eprintln!("open at login: cannot update LaunchAgent: {error}");
                Some(LoginRetry::Sync)
            } else {
                None
            }
        });
        let login_toggle = MenuItem::with_id(
            "open_at_login",
            if login_retry.is_some() {
                "Open at login: Setup failed — click to retry"
            } else {
                login_label(
                    login
                        .as_ref()
                        .is_some_and(|(item, bundle)| item.is_enabled(bundle)),
                )
            },
            login.is_some(),
            None,
        );
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
        menu.append(&status).map_err(|e| e.to_string())?;
        menu.append(&transcription_menu.menu)
            .map_err(|e| e.to_string())?;
        menu.append(&language_menu.menu)
            .map_err(|e| e.to_string())?;
        menu.append(&vocabulary_menu.menu)
            .map_err(|e| e.to_string())?;
        menu.append(&cleanup_menu.menu).map_err(|e| e.to_string())?;
        for item in [
            &start,
            &stop,
            &cancel,
            &copy,
            &remote_toggle,
            &remote_profile,
            &remote_review,
            &restore,
            &local_restore,
            &login_toggle,
            &setup,
            &reopen,
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
        let permissions = permission_snapshot(signed_ad_hoc());
        eprintln!("{}", permissions.report(tap.is_some()));
        let (completed, result) = mpsc::channel();
        Ok(Self {
            core: Dictation::default(),
            sleep_events: super::macos_sleep::SleepEvents::new(),
            control: Arc::new(AtomicU8::new(2)),
            focus: None,
            input,
            sender,
            tap,
            result,
            completed,
            _tray: tray,
            status,
            speech,
            transcription_menu,
            install_events: None,
            previous_provider: None,
            language_menu,
            vocabulary_menu,
            cleanup_menu,
            cleanup_service: Default::default(),
            _native_menu: native_menu,
            pill,
            indicator: Indicator::default(),
            warning: None,
            permissions,
            onboarding: Onboarding::default(),
            prompted_at: Instant::now(),
            last: if local_fixture {
                LOCAL_FIXTURE_TEXT.into()
            } else if remote_fixture {
                "Synthetic dictation test — Café 👋".into()
            } else {
                crate::transcript::Latest::default()
            },
            started: Instant::now(),
            released: Instant::now(),
            retry_at: Instant::now(),
            smoke: std::env::args().any(|a| a == "--smoke"),
            smoke_started: false,
            launched: Instant::now(),
            receiving: Arc::new(AtomicBool::new(false)),
            microphone_startup: Default::default(),
            microphone_notice: None,
            microphone_readiness: crate::readiness::Monitor::new(Duration::from_secs(30)),
            readiness_status: crate::readiness::Status::Inactive,
            readiness_focus: 0,
            readiness_reason: None,
            speech_progress: Default::default(),
            speech_stage: Default::default(),
            remote_mode: remote_fixture,
            recording_remote: remote_fixture,
            recording_remote_target: None,
            remote_toggle,
            remote_profile,
            login,
            login_toggle,
            login_retry,
            profile: crate::remote::Profile::Mac,
            review: Default::default(),
            clipboard_lease: Default::default(),
            local_lease: LocalLease::default(),
            local_fixture,
            fixture_target: None,
            clipboard_attempt: None,
            selected_remote: Default::default(),
            prewarmed: (0, Instant::now()),
            prewarm_checked: Instant::now(),
        })
    }
    fn status(&self, message: &str) {
        eprintln!("status: {message}");
        self.status.set_text(message);
    }
    fn refresh_speech_menu(&mut self) {
        let title = self.cleanup_menu.speech_label(
            speech_label(self.speech.provider()),
            offline_label(self.speech.settings()),
        );
        self.transcription_menu
            .refresh(self.speech.provider(), title);
    }
    /// Load and warm Parakeet in the background when the next dictation may
    /// use it, so the first utterance pays no model load or Core ML compile.
    fn warm_local_engine(&self) {
        let Some(settings) = self.speech.settings() else {
            return;
        };
        let engine = if self.cleanup_menu.offline_all() {
            Some(settings.effective_local_engine())
        } else {
            settings.provider.local_engine()
        };
        if engine == Some(crate::speech::LocalEngine::Parakeet) {
            crate::local_parakeet::Endpoint::shared().service().warm();
        }
    }
    fn handle_transcription(&mut self, action: super::macos_transcription::Action) {
        use super::macos_transcription::Action;
        if self.core.phase != Phase::Idle || self.install_events.is_some() {
            self.refresh_speech_menu();
            self.show("Finish dictation or the model install before changing transcription");
            return;
        }
        match action {
            Action::Choose(provider) => {
                let before = self.speech.provider().cloned();
                match crate::speech::Settings::path()
                    .and_then(|path| self.speech.choose(provider, &path))
                {
                    Ok(()) => {
                        if before.as_ref() != self.speech.provider() {
                            self.previous_provider = before;
                        }
                        self.refresh_speech_menu();
                        self.warm_local_engine();
                        self.show("Transcription engine saved for the next recording");
                    }
                    Err(error) => {
                        self.refresh_speech_menu();
                        self.show(&error);
                    }
                }
            }
            Action::InstallParakeet => self.install_parakeet(),
        }
    }
    /// The only path that downloads a model in the app: an explicit menu
    /// action while no dictation runs. Dictation refuses until it finishes.
    fn install_parakeet(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.install_events = Some(rx);
        self.transcription_menu
            .set_install_text("Installing Parakeet model…");
        self.show("Installing the Parakeet model; dictation waits until it finishes");
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let progress: crate::local_parakeet::Progress = Arc::new(move |line: String| {
                let _ = progress_tx.send(InstallEvent::Progress(line));
            });
            let service = crate::local_parakeet::Endpoint::shared().service().clone();
            let result = crate::capture::block_on_detached(async move {
                service
                    .install(crate::local_parakeet::InstallSources::default(), progress)
                    .await
            })
            .and_then(|r| r);
            let _ = tx.send(InstallEvent::Done(result));
        });
    }
    fn drain_install_events(&mut self) {
        let Some(events) = &self.install_events else {
            return;
        };
        let mut last = None;
        let mut done = None;
        loop {
            match events.try_recv() {
                Ok(InstallEvent::Progress(line)) => last = Some(line),
                Ok(InstallEvent::Done(result)) => {
                    done = Some(result);
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = Some(Err("The Parakeet model install stopped unexpectedly".into()));
                    break;
                }
            }
        }
        if let Some(line) = last {
            self.status(&format!("Installing Parakeet model · {line}"));
        }
        if let Some(result) = done {
            self.install_events = None;
            self.transcription_menu
                .set_install_text("Install Parakeet model…");
            match result {
                Ok(()) => self.show("Parakeet model installed · loading it for the next dictation"),
                Err(error) => self.show(&error),
            }
        }
    }
    fn apply_engine_command(&mut self, spoken: &SpokenCommand) {
        use crate::engine_command::Decision;
        let current = self.speech.provider().cloned();
        match crate::engine_command::decide(
            spoken.command,
            current.as_ref(),
            self.previous_provider.as_ref(),
            self.speech.local_engine(),
            spoken.offline_all,
            &spoken.local,
            &spoken.cloud,
        ) {
            Decision::Switch { to, message } => {
                match crate::speech::Settings::path().and_then(|path| self.speech.choose(to, &path))
                {
                    Ok(()) => {
                        self.previous_provider = current;
                        self.refresh_speech_menu();
                        self.warm_local_engine();
                        self.show(&message);
                    }
                    Err(error) => self.show(&error),
                }
            }
            Decision::Refuse(message) | Decision::Report(message) => self.show(&message),
        }
    }
    fn show(&mut self, message: &str) {
        // A fresh result owns the transient notice. If native cleanup is still
        // pending, the next pump reinstates that higher-priority explanation.
        self.microphone_notice = None;
        self.status(message);
        if let Some(text) = self.indicator.message(message, self.now()) {
            self.pill.set_text(&text);
        }
    }
    fn now(&self) -> u64 {
        self.launched.elapsed().as_millis() as u64
    }
    /// Re-reads every grant, walks launch onboarding and refreshes the named warning.
    fn check_permissions(&mut self) {
        let snapshot = permission_snapshot(self.permissions.ad_hoc);
        if permissions::newly_granted(&self.permissions, &snapshot)
            .contains(&Grant::InputMonitoring)
        {
            // A listener created before the grant may never receive events.
            self.tap = EventTap::new(self.sender.clone());
        }
        self.permissions = snapshot;
        if self.onboarding.waiting() && self.prompted_at.elapsed() >= PROMPT_WAIT {
            self.onboarding.expire_wait(&snapshot);
        }
        if self.core.phase == Phase::Idle
            && self.microphone_startup.state() == crate::capture::MicrophoneState::Idle
            && let Some(grant) = self.onboarding.next(&snapshot)
        {
            self.prompted_at = Instant::now();
            perform(permissions::request(grant, &snapshot));
        }
        let warning = snapshot.warning().or_else(|| {
            self.tap.is_none().then(|| {
                "Keyboard access unavailable — use Start dictation in the Control menu".into()
            })
        });
        if warning != self.warning {
            let show = warning.is_some() || self.core.phase == Phase::Idle;
            self.warning = warning;
            if show {
                self.status(&self.ready());
            }
        }
    }
    /// Refuses a dictation that cannot be delivered, naming the grant and opening its pane.
    fn permitted(&mut self, route: Route) -> bool {
        self.permissions = permission_snapshot(self.permissions.ad_hoc);
        let Err(grant) = self.permissions.gate(route) else {
            return true;
        };
        self.cancel();
        fix(grant, &self.permissions);
        self.show(&permissions::blocked_message(grant, &self.permissions));
        false
    }
    fn ready(&self) -> String {
        if let Some(message) =
            indicator::microphone_notice(self.microphone_startup.state(), self.core.phase)
        {
            return message.into();
        }
        if self.cleanup_menu.network_error() {
            return "Privacy settings invalid · choose Offline-all to repair".into();
        }
        if self.speech.error().is_some() {
            return INVALID_SPEECH.into();
        }
        match &self.warning {
            Some(warning) => warning.clone(),
            None if self.microphone_readiness.message().is_some() => {
                let message = self.microphone_readiness.message().unwrap();
                match &self.readiness_reason {
                    Some(reason)
                        if self.microphone_readiness.status()
                            != crate::readiness::Status::Ready =>
                    {
                        format!("{reason} · {message}")
                    }
                    _ => message.into(),
                }
            }
            None if self.remote_mode => "Hold Control to record for remote review".into(),
            None => "Hold Control to dictate".into(),
        }
    }
    fn cancel(&mut self) {
        self.microphone_readiness.cancel();
        self.review.cancel();
        self.control.store(2, Ordering::Release);
        if self.core.cancel() {
            self.focus = None;
            self.show("Cancelled");
        }
    }
    fn start(&mut self) {
        self.microphone_readiness.cancel();
        if self.microphone_startup.state() != crate::capture::MicrophoneState::Idle {
            self.last.begin();
            self.cancel();
            self.show(&self.ready());
            return;
        }
        if self.speech.error().is_some() || self.cleanup_menu.network_error() {
            self.cancel();
            self.show(&self.ready());
            return;
        }
        self.recording_remote_target = RustDeskWindow::selected();
        self.recording_remote = crate::remote_review::recording_destination(
            self.remote_mode,
            self.recording_remote_target.is_some(),
        );
        // A new user attempt supersedes any old remote-review/copy action.
        // Clear it before preflight so a failed attempt cannot leave a stale
        // "Transcript ready" action that appears to belong to this attempt.
        self.last.begin();
        let route = if self.recording_remote && self.recording_remote_target.is_none() {
            Route::RemoteReview
        } else {
            Route::Local
        };
        // Check before recording: without Accessibility a local dictation can only
        // end as "Transcript ready · choose Copy Last Transcript".
        if !self.permitted(route) {
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
        self.begin_attempt(focus, "◌ Starting microphone…");
        let control = self.control.clone();
        let completed = self.completed.clone();
        let generation = self.core.generation();
        let receiving = self.receiving.clone();
        let startup = self.microphone_startup.clone();
        let progress = self.speech_progress.clone();
        let speech = self.speech.clone();
        let previous = self.previous_provider.clone();
        let remote_cleanup = self.recording_remote;
        let cleanup_service = self.cleanup_service.clone();
        std::thread::spawn(move || {
            let fault = Arc::new(AtomicBool::new(false));
            let mut cleanup_notice = None;
            let mut spoken_command = None;
            let result = crate::core::contain_worker(|| {
                let vocabulary =
                    crate::vocabulary::Profile::load(&crate::vocabulary::Profile::path()?)?;
                let policy = crate::cleanup_config::NetworkPolicy::load(
                    &crate::cleanup_config::NetworkPolicy::path()?,
                )?;
                let privacy = if policy.offline_all {
                    crate::cleanup::Privacy::OfflineAll
                } else {
                    crate::cleanup::Privacy::Configured
                };
                let settings = crate::cleanup_config::Settings::path()
                    .and_then(|p| crate::cleanup_config::Settings::load(&p));
                let provider = speech
                    .capture_provider_with_privacy(policy.offline_all, crate::config::load)?;
                let changes = fotw_audio::device_change::DeviceChangeSignal::new();
                let input_changes = changes.clone();
                let input = crate::capture::InputPreparation::new(move || {
                    let platform = fotw_audio::platform::macos::MacOsPlatform::new();
                    let watch = platform
                        .watch_devices(input_changes)
                        .map_err(|_| "Cannot monitor microphone changes; try again".to_string())?;
                    let tap = platform
                        .open_mic(&DeviceId::new("default"), FormatRequest::any())
                        .map_err(|_| "No microphone available".to_string())?;
                    Ok(crate::capture::PreparedInput::watched(tap, watch))
                })
                .with_fault_signal(fault.clone());
                let outcome = crate::capture::block_on_detached(async {
                    let raw = crate::capture::run_provider_raw_with_preparation(
                        input,
                        provider,
                        control.clone(),
                        receiving,
                        progress,
                        startup,
                        changes,
                        vocabulary.clone(),
                    )
                    .await?;
                    // A whole-utterance engine command is never cleaned up,
                    // expanded or inserted; it is applied by the UI instead.
                    if let Some(command) = crate::engine_command::parse(&raw) {
                        // Probe the local engine this command would land on.
                        let engine = crate::engine_command::target(
                            command,
                            previous.as_ref(),
                            speech.local_engine(),
                        )
                        .and_then(|target| target.local_engine());
                        let local = match (engine, speech.settings()) {
                            (None, _) => Ok(()),
                            (Some(_), None) => Err("Speech settings are invalid".to_string()),
                            (Some(engine), Some(settings)) => {
                                match settings.local_transcriber(engine) {
                                    Ok(local) => local
                                        .prepare("en", Duration::from_secs(2))
                                        .await
                                        .map(|_| ()),
                                    Err(error) => Err(error),
                                }
                            }
                        };
                        let cloud = if command.may_need_cloud() {
                            crate::config::load().map(|_| ())
                        } else {
                            Ok(())
                        };
                        return Ok::<_, String>(Spoken::Command(raw, command, local, cloud));
                    }
                    let outcome = cleanup_service
                        .run(
                            &raw,
                            &vocabulary,
                            settings.as_ref().map_err(String::as_str),
                            privacy,
                            control,
                            crate::config::load_named,
                        )
                        .await?;
                    Ok::<_, String>(Spoken::Text(outcome.for_destination(&raw, remote_cleanup)))
                })??;
                match outcome {
                    Spoken::Text(outcome) => {
                        cleanup_notice = Some(outcome.disposition);
                        Ok(outcome.text)
                    }
                    Spoken::Command(raw, command, local, cloud) => {
                        spoken_command = Some(SpokenCommand {
                            command,
                            local,
                            cloud,
                            offline_all: policy.offline_all,
                        });
                        Ok(raw)
                    }
                }
            });
            let _ = completed.send(Completion {
                command: spoken_command,
                cleanup: cleanup_notice,
                input_failed: fault.load(Ordering::Acquire),
                generation,
                result,
            });
        });
    }
    fn preflight_focus(&mut self) -> Result<Option<Focus>, DeliveryFailure> {
        if self.recording_remote_target.is_some() {
            return Ok(None);
        }
        let focus = if self.remote_mode {
            Focus::current().ok()
        } else {
            settle_focus(|| Focus::current().ok(), probe, std::thread::sleep)
        };
        match preflight(focus.as_ref().map(probe), self.remote_mode) {
            Preflight::Use => Ok(focus),
            Preflight::ReviewOnly => Ok(None),
            Preflight::Refuse(failure) => Err(failure),
        }
    }
    /// Asks the frontmost app to build its accessibility tree before Control is held.
    fn prewarm(&mut self) {
        if self.prewarm_checked.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.prewarm_checked = Instant::now();
        let pid = focused_app_pid();
        let (last, at) = self.prewarmed;
        // Chrome can drop the request later, so repeat it periodically.
        if pid <= 0 || (pid == last && at.elapsed() < Duration::from_secs(3)) {
            return;
        }
        let bundle = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .and_then(|app| app.bundleIdentifier())
            .map(|id| id.to_string())
            .unwrap_or_default();
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        for name in prewarm_attributes(&bundle) {
            let key = cfstr(name);
            unsafe { AXUIElementSetAttributeValue(app.0, key.0, kCFBooleanTrue) };
        }
        self.prewarmed = (pid, Instant::now());
    }
    fn begin_attempt(&mut self, focus: Option<Focus>, status: &str) {
        self.focus = focus;
        self.control = Arc::new(AtomicU8::new(0));
        self.sleep_events.track(self.control.clone());
        self.receiving = Arc::new(AtomicBool::new(false));
        self.speech_progress = Default::default();
        self.speech_stage = Default::default();
        self.started = Instant::now();
        self.last.begin();
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
            command: None,
            cleanup: None,
            input_failed: false,
            generation,
            result: Ok(text.into()),
        });
        if duplicate {
            let _ = self.completed.send(Completion {
                command: None,
                cleanup: None,
                input_failed: false,
                generation,
                result: Ok("Late synthetic duplicate".into()),
            });
        }
    }
    fn finish(&mut self) {
        self.microphone_readiness.cancel();
        self.released = Instant::now();
        self.control.store(1, Ordering::Release);
        self.status("Finishing… · Esc to cancel");
    }
    fn suspend_for_sleep(&mut self) {
        self.microphone_readiness.cancel();
        self.control.store(2, Ordering::Release);
        let interrupted = self.core.cancel();
        self.review.cancel();
        self.focus = None;
        self.indicator.disarm();
        if let Some(tap) = &self.tap {
            tap._context.space.borrow_mut().suspend();
        }
        let mut tap_disabled = false;
        for event in self.input.try_iter() {
            tap_disabled |= matches!(event, Input::TapDisabled);
        }
        if tap_disabled && let Some(tap) = &self.tap {
            // Discard stale shortcuts without losing monitor recovery.
            unsafe { CGEventTapEnable(tap.port.0, true) }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            // Explicit app exit wins; stale dictation/menu commands do not replay.
            if event.id.as_ref() == "quit" {
                std::process::exit(0);
            }
        }
        self.smoke_started = true;
        if interrupted {
            self.show("Sleep interrupted dictation; release the shortcut and try again");
        }
    }
    fn poll_microphone_readiness(&mut self) {
        use crate::{capture::MicrophoneState as Mic, permissions::Access};
        if self.readiness_focus != focused_app_pid() {
            self.microphone_readiness.cancel();
        }
        let allowed = self.core.phase == Phase::Idle
            && self.permissions.microphone == Access::Granted
            && !self.onboarding.waiting();
        let native_available = self.microphone_startup.state() == Mic::Idle
            || (self.microphone_startup.state() == Mic::Inspecting
                && self.microphone_readiness.pending());
        // A failed capture can still be finishing cleanup. Preserve the episode,
        // but never compete with or bypass that native owner.
        if allowed && !native_available {
            return;
        }
        self.microphone_readiness.tick(
            self.launched.elapsed(),
            allowed,
            &self.microphone_startup,
            || fotw_audio::platform::macos::default_input_snapshot().ok(),
        );
        let status = self.microphone_readiness.status();
        if status != self.readiness_status {
            self.readiness_status = status;
            // Metadata completions have no recording, focus or insertion action.
            // Only an idle status may change, after cancellation/input handling.
            if self.core.phase == Phase::Idle {
                self.status(&self.ready());
            }
        }
    }
    fn pump(&mut self) {
        if self.sleep_events.take() {
            self.suspend_for_sleep();
            return;
        }
        self.observe_remote_window();
        if self.core.phase == Phase::Idle
            && self.microphone_startup.state() == crate::capture::MicrophoneState::Idle
        {
            self.prewarm();
        }
        if self.cleanup_menu.refresh_if_due() {
            self.refresh_speech_menu();
        }
        self.transcription_menu
            .set_enabled(self.core.phase == Phase::Idle && self.install_events.is_none());
        self.drain_install_events();
        self.cleanup_menu
            .menu
            .set_enabled(self.core.phase == Phase::Idle);
        self.language_menu.refresh_if_due();
        self.vocabulary_menu.refresh_if_due();
        self.vocabulary_menu
            .menu
            .set_enabled(self.core.phase == Phase::Idle);
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
            tap._context.busy.set(
                self.core.phase == Phase::Processing
                    || (self.core.phase == Phase::Idle
                        && self.microphone_startup.state()
                            != crate::capture::MicrophoneState::Idle),
            );
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
            if matches!(event, Input::Flags(false, _) | Input::HotkeyAbort) {
                self.microphone_readiness.cancel();
            }
            match event {
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
            if super::macos_cleanup::CleanupMenu::handles(event.id.as_ref()) {
                if self.core.phase != Phase::Idle {
                    self.show("Finish dictation before changing cleanup or privacy");
                    continue;
                }
                if let Some(result) = self.cleanup_menu.handle(event.id.as_ref()) {
                    self.refresh_speech_menu();
                    self.warm_local_engine();
                    match result {
                        Ok(message) => self.show(&message),
                        Err(error) => self.show(&error),
                    }
                }
                continue;
            }
            if super::macos_vocabulary::VocabularyMenu::handles(event.id.as_ref()) {
                if self.core.phase != Phase::Idle {
                    self.show("Finish dictation before editing vocabulary");
                    continue;
                }
                // A modal editor must not collect shortcuts for replay when it
                // closes. Keep ordinary editing keys available to AppKit.
                self.core.cancel();
                self.microphone_readiness.cancel();
                self.indicator.disarm();
                if let Some(tap) = &self.tap {
                    tap._context.busy.set(true);
                    tap._context.space.borrow_mut().suspend();
                }
                let result = self.vocabulary_menu.handle(event.id.as_ref());
                if let Some(tap) = &self.tap {
                    tap._context.space.borrow_mut().suspend();
                    tap._context.busy.set(
                        self.core.phase != Phase::Idle
                            || self.microphone_startup.state()
                                != crate::capture::MicrophoneState::Idle,
                    );
                }
                let mut tap_disabled = false;
                for event in self.input.try_iter() {
                    tap_disabled |= matches!(event, Input::TapDisabled);
                }
                if tap_disabled && let Some(tap) = &self.tap {
                    // Preserve monitor recovery while discarding stale editor shortcuts.
                    unsafe { CGEventTapEnable(tap.port.0, true) }
                }
                // Commands made behind a modal must not start recording after
                // it closes; the next action must be a fresh deliberate command.
                while MenuEvent::receiver().try_recv().is_ok() {}
                match result {
                    Ok(message) => self.show(&message),
                    Err(error) => self.show(&error),
                }
                continue;
            }
            if let Some(result) = self.language_menu.handle(event.id.as_ref()) {
                match result {
                    Ok(message) => self.show(&message),
                    Err(error) => self.show(&error),
                }
                continue;
            }
            if let Some(action) = self.transcription_menu.handle(event.id.as_ref()) {
                self.handle_transcription(action);
                continue;
            }
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
                        self.show("Hold Control to record for remote review");
                    } else {
                        self.indicator.clear();
                        self.status(&self.ready());
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
                "open_at_login" => {
                    if let Some((item, bundle)) = &self.login {
                        let action = self
                            .login_retry
                            .unwrap_or_else(|| LoginRetry::SetEnabled(!item.is_enabled(bundle)));
                        let result = match action {
                            LoginRetry::Sync => item.sync(bundle).map(|_| ()),
                            LoginRetry::SetEnabled(enabled) => item.set_enabled(enabled, bundle),
                        };
                        match result {
                            Ok(()) => {
                                self.login_retry = None;
                                self.login_toggle
                                    .set_text(login_label(item.is_enabled(bundle)));
                            }
                            Err(error) => {
                                eprintln!("open at login: cannot update LaunchAgent: {error}");
                                self.login_retry = Some(action);
                                self.login_toggle
                                    .set_text("Open at login: Setup failed — click to retry");
                                self.show("Open at login failed — check Library/LaunchAgents and Murmur Application Support permissions, then retry");
                            }
                        }
                    }
                }
                "remote_review" if self.core.phase == Phase::Idle && self.recording_remote => {
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
                "copy" if self.recording_remote && self.core.phase == Phase::Idle => {
                    self.review_remote()
                }
                "copy" if !self.last.is_empty() => {
                    self.copy_local_transcript();
                }
                "setup" => {
                    self.cancel();
                    self.permissions = permission_snapshot(self.permissions.ad_hoc);
                    match self.permissions.missing().first() {
                        Some(&grant) => {
                            fix(grant, &self.permissions);
                            self.show(&permissions::blocked_message(grant, &self.permissions));
                        }
                        None => self.show("All permissions granted"),
                    }
                }
                "reopen" => {
                    // Input Monitoring grants apply only to a newly launched process.
                    let bundle = std::env::current_exe()
                        .ok()
                        .and_then(|exe| permissions::app_bundle(&exe));
                    match bundle {
                        Some(bundle) => {
                            self.control.store(2, Ordering::Release);
                            let _ = std::process::Command::new("/bin/sh")
                                .args(["-c", "sleep 1; /usr/bin/open \"$0\""])
                                .arg(bundle)
                                .spawn();
                            std::process::exit(0)
                        }
                        None => self.show("Reopen needs Murmur.app; quit and relaunch it"),
                    }
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
        if self.core.phase == Phase::Processing
            && crate::core::processing_overdue(self.released.elapsed())
        {
            self.cancel();
            self.show("Transcription did not finish; no text inserted");
        }
        while let Ok(done) = self.result.try_recv() {
            if self.sleep_events.take() {
                self.suspend_for_sleep();
                return;
            }
            if self.control.load(Ordering::Acquire) == 2 {
                self.core.cancel();
                self.focus = None;
                self.indicator.disarm();
                continue;
            }
            let Some(effect) = self.core.complete(done.generation, done.result) else {
                continue;
            };
            match effect {
                CompletionEffect::Transcript(_) if done.command.is_some() => {
                    if let Some(spoken) = done.command.as_ref() {
                        self.apply_engine_command(spoken);
                    }
                }
                CompletionEffect::Transcript(text) => {
                    let cleanup_suffix = match done.cleanup {
                        Some(crate::cleanup::Disposition::Fallback) => {
                            " · raw text; cleanup unavailable"
                        }
                        Some(crate::cleanup::Disposition::Offline) => " · Offline-all",
                        _ => "",
                    };
                    eprintln!(
                        "transcription complete: {} characters",
                        text.chars().count()
                    );
                    self.last.set(text.clone());
                    if !self.recording_remote && text.contains('\n') {
                        self.show("Formatted text ready · choose Copy Last Transcript");
                    } else if self.recording_remote_target.is_some() {
                        self.copy_automatic_remote(&text);
                    } else if self.recording_remote {
                        self.show(&self.speech_progress.stage().completion_message(&format!(
                            "Transcript ready · Review for RustDesk…{cleanup_suffix}"
                        )));
                    } else {
                        let result = self
                            .focus
                            .as_ref()
                            .ok_or(DeliveryFailure::MissingTarget)
                            .and_then(|f| f.insert(&text, &mut self.local_lease));
                        match result {
                            Ok(outcome) => {
                                self.show(&self.speech_progress.stage().completion_message(
                                    &format!("{}{cleanup_suffix}", outcome.message()),
                                ))
                            }
                            Err(reason) => self.show(reason.message()),
                        }
                    }
                }
                CompletionEffect::Empty => self.show(crate::core::NO_SPEECH),
                CompletionEffect::Error(e) => {
                    if done.input_failed {
                        self.readiness_reason = Some(e.clone());
                        self.readiness_focus = focused_app_pid();
                        self.microphone_readiness.fault(self.launched.elapsed());
                    }
                    self.show(&e);
                }
            }
            self.focus = None;
        }
        if self.retry_at.elapsed() > Duration::from_secs(3) {
            if self.tap.is_none() {
                self.tap = EventTap::new(self.sender.clone());
            }
            self.retry_at = Instant::now();
            self.check_permissions();
            self.pill.place();
        }
        self.poll_microphone_readiness();
        let now = self.now();
        let stage = self.speech_progress.stage();
        if stage != self.speech_stage
            && matches!(self.core.phase, Phase::Recording | Phase::Processing)
        {
            self.speech_stage = stage;
            let verb = if self.core.phase == Phase::Recording {
                "Recording"
            } else {
                "Transcribing"
            };
            self.status(&format!("{verb} · {} · Esc to cancel", stage.label()));
        }
        let receiving = self.receiving.load(Ordering::Acquire);
        let microphone = self.microphone_startup.state();
        let notice = indicator::microphone_notice(microphone, self.core.phase);
        if let Some(message) = notice {
            if notice != self.microphone_notice {
                self.indicator.clear();
                self.pill.set_text(&indicator::shorten(message));
            }
            // Other completion/permission messages cannot hide unresolved input
            // ownership. Keep the full explanation available in the menu.
            self.status.set_text(message);
        } else if self.microphone_notice.is_some() {
            self.indicator.clear();
            if self.core.phase == Phase::Idle {
                self.status(&self.ready());
            } else if self.core.phase == Phase::Processing {
                self.status(&format!("Transcribing · {} · Esc to cancel", stage.label()));
            }
        }
        self.microphone_notice = notice;
        let look = self.indicator.look_with_microphone(
            now,
            self.core.phase,
            receiving,
            self.warning.is_some(),
            microphone,
        );
        if self.pill.look == Look::Success
            && matches!(look, Look::Idle | Look::Warning | Look::Armed)
        {
            self.status(&self.ready());
        }
        self.pill.render(look, now);
    }
}
fn login_label(enabled: bool) -> &'static str {
    if enabled {
        "Open at login: On"
    } else {
        "Open at login: Off"
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
    fn copy_automatic_remote(&mut self, text: &str) {
        let Some(target) = self.recording_remote_target.take() else {
            return;
        };
        if target.revalidate() != Some(target.token) {
            self.show("Transcript ready · RustDesk focus changed. Review for RustDesk");
            return;
        }
        let mtm = MainThreadMarker::new().expect("shell main thread");
        let mut clipboard =
            super::macos_clipboard::MacClipboard::new(NSPasteboard::generalPasteboard(), mtm);
        let attempt = crate::remote::Attempt(self.core.generation());
        let result = self.clipboard_lease.share(&mut clipboard, attempt, text);
        if self.clipboard_lease.pending() {
            self.clipboard_attempt = Some(attempt);
        }
        match result {
            Ok(()) => self.show(
                &self
                    .speech_progress
                    .stage()
                    .completion_message("Fresh transcript copied · paste in RustDesk"),
            ),
            Err(reason) => {
                eprintln!("remote_copy reason={reason:?}");
                self.show(reason.message());
            }
        }
    }
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
            Err(reason) => {
                eprintln!("remote_copy reason={reason:?}");
                self.show(reason.message());
            }
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
/// How long a dismissed system prompt blocks the next onboarding step.
const PROMPT_WAIT: Duration = Duration::from_secs(20);
fn microphone_access() -> Access {
    let platform = fotw_audio::platform::macos::MacOsPlatform::new();
    match platform.permission(Permission::Microphone) {
        PermissionState::Granted => Access::Granted,
        PermissionState::NotDetermined => Access::Undetermined,
        _ => Access::Denied,
    }
}
fn permission_snapshot(ad_hoc: bool) -> Snapshot {
    Snapshot {
        microphone: microphone_access(),
        accessibility: unsafe { AXIsProcessTrusted() },
        input_monitoring: unsafe { CGPreflightListenEventAccess() },
        ad_hoc,
    }
}
/// Reads this process's own signature; no special access is required.
fn signed_ad_hoc() -> bool {
    let mut code = ptr::null();
    if unsafe { SecCodeCopySelf(0, &mut code) } != 0 || code.is_null() {
        return false;
    }
    let code = Owned(code);
    let mut info = ptr::null();
    // kSecCSSigningInformation
    if unsafe { SecCodeCopySigningInformation(code.0, 1 << 1, &mut info) } != 0 || info.is_null() {
        return false;
    }
    let info = Owned(info);
    let value = unsafe { CFDictionaryGetValue(info.0, kSecCodeInfoFlags) };
    let mut flags = 0u32;
    // kCFNumberSInt32Type
    !value.is_null()
        && unsafe { CFNumberGetValue(value, 3, (&raw mut flags).cast()) }
        && permissions::is_ad_hoc(flags)
}
/// Explicit user action: the system prompt when one can still appear, otherwise Settings.
fn fix(grant: Grant, snapshot: &Snapshot) {
    match permissions::request(grant, snapshot) {
        Request::MicrophonePrompt => request_microphone(),
        _ => open_settings(grant),
    }
}
fn perform(request: Request) {
    match request {
        Request::MicrophonePrompt => request_microphone(),
        Request::AccessibilityPrompt => prompt_accessibility(),
        Request::InputMonitoringPrompt => {
            unsafe { CGRequestListenEventAccess() };
        }
        Request::OpenSettings(grant) => open_settings(grant),
    }
}
fn open_settings(grant: Grant) {
    let _ = std::process::Command::new("open")
        .arg(grant.settings_url())
        .spawn();
}
fn prompt_accessibility() {
    let keys = [unsafe { kAXTrustedCheckOptionPrompt }];
    let values = [unsafe { kCFBooleanTrue }];
    let options = Owned(unsafe {
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
            (&raw const kCFTypeDictionaryValueCallBacks).cast(),
        )
    });
    unsafe { AXIsProcessTrustedWithOptions(options.0) };
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
    shell.borrow_mut().check_permissions();
    shell.borrow().warm_local_engine();
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

const INVALID_SPEECH: &str =
    "Speech settings invalid — choose an engine in the Transcription menu to repair";

fn speech_label(provider: Option<&crate::speech::Provider>) -> &'static str {
    use crate::speech::Provider;
    match provider {
        Some(Provider::Deepgram) => "Transcription: Deepgram",
        Some(Provider::LocalParakeet) => "Transcription: Local Parakeet",
        Some(Provider::LocalWhisper) => "Transcription: Local Whisper",
        Some(Provider::DeepgramThenParakeet) => "Transcription: Deepgram → Parakeet",
        Some(Provider::DeepgramThenWhisper) => "Transcription: Deepgram → Whisper",
        None => "Transcription: invalid settings — choose an engine",
    }
}

/// What Offline-all runs instead of the saved choice.
fn offline_label(settings: Option<&crate::speech::Settings>) -> &'static str {
    match settings.map(crate::speech::Settings::effective_local_engine) {
        Some(crate::speech::LocalEngine::Whisper) => "Transcription: Local Whisper (Offline-all)",
        _ => "Transcription: Local Parakeet (Offline-all)",
    }
}

/// Render only the owned indicator views; no Shell, capture, permissions,
/// keyboard hook, clipboard or other application's window is accessed.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn preview_microphone_cleanup(output: &std::path::Path) -> Result<(), String> {
    use crate::capture::MicrophoneState as Mic;
    use objc2_app_kit::NSBitmapImageFileType;
    use objc2_foundation::NSDictionary;
    let mtm = MainThreadMarker::new().ok_or("Preview must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    std::fs::create_dir_all(output).map_err(|e| e.to_string())?;
    for (name, state) in [
        ("pending", Mic::Stopping),
        ("stopped", Mic::CleaningUp),
        ("unconfirmed", Mic::Unconfirmed),
        ("cleanup-failed", Mic::CleanupFailed),
    ] {
        let mut pill = Pill::new(mtm)?;
        // Window opacity does not affect caching its own content view. Keep
        // this fixture invisible to the desktop while exercising native paint.
        pill.panel.setAlphaValue(0.0);
        let message =
            indicator::microphone_notice(state, Phase::Idle).ok_or("Missing cleanup notice")?;
        pill.set_text(&indicator::shorten(message));
        for tick in 0..40 {
            pill.render(Look::Notice, tick * 20);
        }
        let view = pill.panel.contentView().ok_or("Missing preview view")?;
        view.layoutSubtreeIfNeeded();
        view.displayIfNeeded();
        let bounds = view.bounds();
        let bitmap = view
            .bitmapImageRepForCachingDisplayInRect(bounds)
            .ok_or("Cannot cache preview")?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);
        // Empty, correctly typed properties; no arbitrary Objective-C values.
        let data = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }
        .ok_or("Cannot encode preview")?;
        let path = output.join(format!("{name}.png"));
        if !data.writeToFile_atomically(&NSString::from_str(&path.to_string_lossy()), true) {
            return Err("Cannot save preview".into());
        }
        pill.panel.orderOut(None);
        println!("{name}: {message}");
    }
    Ok(())
}
