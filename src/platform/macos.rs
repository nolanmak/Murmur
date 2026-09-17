//! macOS-only boundary: AppKit shell, Space event tap and accessibility insertion.
//! All retained CF/AX objects stay on the main thread. Workers receive no UI pointers.
use crate::{
    core::{Dictation, Phase},
    insertion::{Target, allowed},
};
use block2::RcBlock;
use fotw_audio::{AudioPlatform, DeviceId, FormatRequest, Permission, PermissionState};
use muda::{Menu, MenuEvent, MenuItem};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont, NSPanel,
    NSPasteboard, NSScreen, NSTextField, NSWindowCollectionBehavior, NSWindowStyleMask,
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
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateSystemWide() -> CF;
    fn AXUIElementCopyAttributeValue(element: CF, attribute: CF, value: *mut CF) -> i32;
    fn AXUIElementSetAttributeValue(element: CF, attribute: CF, value: CF) -> i32;
    fn AXUIElementIsAttributeSettable(element: CF, attribute: CF, settable: *mut u8) -> i32;
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
    fn CGEventCreateCopy(event: CF) -> CF;
    fn CGEventTapPostEvent(proxy: CF, event: CF);
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
        unsafe { CFRelease(self.0) }
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
    target: Target,
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
        let name = cfstr("AXSelectedText");
        let mut settable = 0;
        let editable = unsafe { AXUIElementIsAttributeSettable(element.0, name.0, &mut settable) }
            == 0
            && settable != 0
            && matches!(role.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox");
        let target = Target {
            pid,
            element: unsafe { CFHash(element.0) } as u64,
            secure,
            editable,
        };
        Ok(Self { element, target })
    }
    fn insert(&self, text: &str) -> Result<(), String> {
        let now = Self::current()?;
        if !allowed(&self.target, &now.target, text)
            || !unsafe { CFEqual(self.element.0, now.element.0) }
        {
            return Err(
                "Insertion blocked: focus changed or field unsupported. Use Copy Last Transcript."
                    .into(),
            );
        }
        let key = cfstr("AXSelectedText");
        let value = cfstr(text);
        if unsafe { AXUIElementSetAttributeValue(now.element.0, key.0, value.0) } != 0 {
            return Err("This field does not accept text. Use Copy Last Transcript.".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Input {
    Flags(bool, bool),
    Cancel,
    TapDisabled,
}
struct TapContext {
    sender: mpsc::SyncSender<Input>,
    dropped: AtomicBool,
    space: RefCell<crate::space_hotkey::HoldSpace>,
    pending: RefCell<Option<Owned>>,
    origin: Instant,
    busy: std::cell::Cell<bool>,
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
unsafe extern "C" fn callback(proxy: CF, kind: u32, event: CF, info: *mut c_void) -> CF {
    use crate::space_hotkey::Key;
    let context = unsafe { &*(info as *const TapContext) };
    if matches!(kind, 0xffff_fffe | 0xffff_ffff) {
        let _ = context.sender.try_send(Input::TapDisabled);
        return event;
    }
    let flags = unsafe { CGEventGetFlags(event) };
    let modified = flags & ((1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | (1 << 23)) != 0;
    let code = unsafe { CGEventGetIntegerValueField(event, 9) };
    let repeat = unsafe { CGEventGetIntegerValueField(event, 8) } != 0;
    let key = match (kind, code) {
        (10, 49) if repeat => Key::Repeat,
        (10, 49) if modified || context.busy.get() => Key::ModifiedDown,
        (10, 49) => Key::Down,
        (11, 49) => Key::Up,
        (10, 53) => Key::Escape,
        (10, _) => Key::Other,
        (12, _) if modified => Key::Other,
        _ => return event,
    };
    let Ok(mut space) = context.space.try_borrow_mut() else {
        return event;
    };
    let action = space.key(key, context.origin.elapsed().as_millis() as u64);
    drop(space);
    if matches!(key, Key::Down) && action.consume {
        // Keep one native event so a short tap can be delivered unchanged.
        let copy = unsafe { CGEventCreateCopy(event) };
        if copy.is_null() {
            *context.space.borrow_mut() = Default::default();
            return event;
        }
        *context.pending.borrow_mut() = Some(Owned(copy));
    }
    if action.replay
        && let Some(saved) = context.pending.borrow_mut().take()
    {
        // Posts downstream in this same event-tap callback, before the current
        // key-up or next character. This preserves fast typing rollover order.
        unsafe { CGEventTapPostEvent(proxy, saved.0) };
    }
    if let Some(command) = action.command {
        send_command(context, command);
    }
    if matches!(key, Key::Up) {
        context.pending.borrow_mut().take();
    }
    if action.consume { ptr::null() } else { event }
}
impl EventTap {
    fn new(sender: mpsc::SyncSender<Input>) -> Option<Self> {
        let mut context = Box::new(TapContext {
            sender,
            dropped: AtomicBool::new(false),
            space: RefCell::new(Default::default()),
            pending: RefCell::new(None),
            origin: Instant::now(),
            busy: std::cell::Cell::new(false),
        });
        let port = unsafe {
            CGEventTapCreate(
                1,
                0,
                0,
                (1 << 12) | (1 << 10) | (1 << 11),
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
    panel: Retained<NSPanel>,
    label: Retained<NSTextField>,
    last: String,
    message: String,
    started: Instant,
    hide_at: Option<Instant>,
    retry_at: Instant,
}
impl Shell {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        let menu = Menu::new();
        let status = MenuItem::with_id("status", "Hold Space to dictate", false, None);
        let cancel = MenuItem::with_id("cancel", "Cancel dictation (Esc)", true, None);
        let copy = MenuItem::with_id("copy", "Copy Last Transcript", true, None);
        let setup = MenuItem::with_id("setup", "Set up permissions", true, None);
        let quit = MenuItem::with_id("quit", "Quit Text-to-speech", true, None);
        for item in [&status, &cancel, &copy, &setup, &quit] {
            menu.append(item).map_err(|e| e.to_string())?;
        }
        let tray = TrayIconBuilder::new()
            .with_title("Space")
            .with_tooltip("Text-to-speech")
            .with_menu(Box::new(menu))
            .build()
            .map_err(|e| e.to_string())?;
        let screen = NSScreen::mainScreen(mtm).ok_or("No screen")?.visibleFrame();
        let frame = NSRect::new(
            NSPoint::new(
                screen.origin.x + (screen.size.width - 480.0) / 2.0,
                screen.origin.y + 38.0,
            ),
            NSSize::new(480.0, 54.0),
        );
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
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
        panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
        unsafe { panel.setReleasedWhenClosed(false) };
        let label =
            NSTextField::wrappingLabelWithString(&NSString::from_str("Hold Space to dictate"), mtm);
        label.setFrame(NSRect::new(
            NSPoint::new(16.0, 10.0),
            NSSize::new(448.0, 34.0),
        ));
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        label.setTextColor(Some(&NSColor::labelColor()));
        panel
            .contentView()
            .ok_or("No panel content")?
            .addSubview(&label);
        let (sender, input) = mpsc::sync_channel(128);
        let tap = EventTap::new(sender.clone());
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
            panel,
            label,
            last: String::new(),
            message: String::new(),
            started: Instant::now(),
            hide_at: None,
            retry_at: Instant::now(),
        })
    }
    fn show(&mut self, message: &str, temporary: bool) {
        self.message = message.into();
        self.status.set_text(message);
        self.label.setStringValue(&NSString::from_str(message));
        self.panel.orderFrontRegardless();
        self.hide_at = temporary.then(|| Instant::now() + Duration::from_secs(5));
    }
    fn cancel(&mut self) {
        self.control.store(2, Ordering::Release);
        if self.core.cancel() {
            self.focus = None;
            self.show("Cancelled", true);
        }
    }
    fn start(&mut self) {
        let focus = match Focus::current() {
            Ok(f) if !f.target.secure && f.target.editable => f,
            Ok(_) => {
                self.cancel();
                self.show(
                    "Choose a supported text field. Password fields are blocked.",
                    true,
                );
                return;
            }
            Err(e) => {
                self.cancel();
                self.show(&e, true);
                return;
            }
        };
        let platform = fotw_audio::platform::macos::MacOsPlatform::new();
        if platform.permission(Permission::Microphone) != PermissionState::Granted {
            self.cancel();
            self.show(
                "Use Set up permissions in the Space menu, then try again.",
                true,
            );
            return;
        }
        self.focus = Some(focus);
        self.control = Arc::new(AtomicU8::new(0));
        self.started = Instant::now();
        self.last.clear();
        self.show("Listening — release Space to insert · Esc to cancel", false);
        let control = self.control.clone();
        let completed = self.completed.clone();
        let generation = self.core.generation();
        std::thread::spawn(move || {
            let result = (|| {
                let credentials = crate::config::load()?;
                let platform = fotw_audio::platform::macos::MacOsPlatform::new();
                let tap = platform
                    .open_mic(&DeviceId::new("default"), FormatRequest::any())
                    .map_err(|_| "No microphone available".to_string())?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| "Cannot start audio runtime".to_string())?;
                runtime.block_on(crate::capture::run(
                    tap,
                    credentials.key.expose().into(),
                    control,
                ))
            })();
            let _ = completed.send(Completion { generation, result });
        });
    }
    fn finish(&mut self) {
        self.control.store(1, Ordering::Release);
        self.show("Finishing… · Esc to cancel", false);
    }
    fn pump(&mut self) {
        if let Some(tap) = &self.tap {
            tap._context.busy.set(self.core.phase == Phase::Processing);
            let action = tap
                ._context
                .space
                .borrow_mut()
                .tick(tap._context.origin.elapsed().as_millis() as u64);
            if let Some(command) = action.command {
                tap._context.pending.borrow_mut().take();
                send_command(&tap._context, command);
            }
        }

        if self
            .tap
            .as_ref()
            .is_some_and(|tap| tap._context.dropped.swap(false, Ordering::AcqRel))
        {
            self.cancel();
            self.show("Input queue overflow; dictation cancelled", true);
        }
        while let Ok(event) = self.input.try_recv() {
            match event {
                Input::Flags(down, other) => match self.core.flags(down, other) {
                    Some("start") => self.start(),
                    Some("finish") => self.finish(),
                    Some("cancel") => {
                        self.control.store(2, Ordering::Release);
                        self.focus = None;
                        self.show("Cancelled Space shortcut", true)
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
                "cancel" => self.cancel(),
                "copy" if !self.last.is_empty() => {
                    let paste = NSPasteboard::generalPasteboard();
                    paste.clearContents();
                    paste.setString_forType(
                        &NSString::from_str(&self.last),
                        &NSString::from_str("public.utf8-plain-text"),
                    );
                    self.show("Transcript copied", true);
                }
                "setup" => {
                    self.cancel();
                    request_microphone();
                    let _=std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility").spawn();
                    self.show(
                        "Enable Accessibility. Keep Wispr Flow on Fn; hold Space here.",
                        true,
                    );
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
            if done.generation != self.core.generation() {
                continue;
            }
            let accepts = self.core.accepts(done.generation);
            match done.result {
                Ok(text) if accepts && !text.trim().is_empty() => {
                    self.last = text.clone();
                    let result = self
                        .focus
                        .as_ref()
                        .ok_or("Target was lost".to_string())
                        .and_then(|f| f.insert(&text));
                    match result {
                        Ok(()) => self.show("Text inserted", true),
                        Err(e) => self.show(&e, true),
                    }
                }
                Ok(_) => self.show("No speech detected", true),
                Err(e) => self.show(&e, true),
            }
            self.core.finish();
            self.focus = None;
        }
        if self.tap.is_none() && self.retry_at.elapsed() > Duration::from_secs(3) {
            self.tap = EventTap::new(self.sender.clone());
            self.retry_at = Instant::now();
            if self.tap.is_none() {
                self.status.set_text("Enable Accessibility, then relaunch");
            }
        }
        if self.hide_at.is_some_and(|time| Instant::now() >= time) {
            self.panel.orderOut(None);
            self.hide_at = None;
            self.status.set_text("Hold Space to dictate");
        }
    }
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
    let shell = Rc::new(RefCell::new(Shell::new(mtm)?));
    shell.borrow_mut().show(
        "Hold Space to dictate. First run: use Set up permissions in the Space menu.",
        true,
    );
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
