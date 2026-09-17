//! macOS-only boundary: AppKit shell, Control event tap and accessibility insertion.
//! All retained CF/AX objects stay on the main thread. Workers receive no UI pointers.
use crate::{
    core::{Dictation, Phase},
    insertion::{Target, allowed},
};
use block2::RcBlock;
use fotw_audio::{AudioPlatform, DeviceId, FormatRequest, Permission, PermissionState};
use muda::{ContextMenu, Menu, MenuEvent, MenuItem};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor,
    NSFont, NSMenu, NSPanel, NSPasteboard, NSRunningApplication, NSScreen, NSScrollView,
    NSTextField, NSTextView, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
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
        let target = Target {
            pid,
            element: unsafe { CFHash(element.0) } as u64,
            secure,
            editable,
        };
        Ok(Self {
            element,
            target,
            paste_only,
        })
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
        if self.paste_only {
            return paste_text(text);
        }
        if unsafe { AXUIElementSetAttributeValue(now.element.0, key.0, value.0) } != 0 {
            return paste_text(text);
        }
        Ok(())
    }
}
fn paste_text(text: &str) -> Result<(), String> {
    let paste = NSPasteboard::generalPasteboard();
    if paste.pasteboardItems().is_some_and(|items| items.len() > 1) {
        return Err("Use Copy Last Transcript (clipboard has multiple items)".into());
    }
    let mut saved = Vec::new();
    if let Some(types) = paste.types() {
        for kind in types.iter() {
            let data = paste
                .dataForType(&kind)
                .ok_or("Cannot preserve clipboard; use Copy Last Transcript")?;
            saved.push((kind, data));
        }
    }
    let down = Owned(unsafe { CGEventCreateKeyboardEvent(ptr::null(), 9, true) });
    let up = Owned(unsafe { CGEventCreateKeyboardEvent(ptr::null(), 9, false) });
    if down.0.is_null() || up.0.is_null() {
        return Err("Cannot create paste shortcut".into());
    }
    paste.clearContents();
    paste.setString_forType(
        &NSString::from_str(text),
        &NSString::from_str("public.utf8-plain-text"),
    );
    let count = paste.changeCount();
    unsafe {
        CGEventSetFlags(down.0, 1 << 20);
        CGEventSetFlags(up.0, 1 << 20);
        CGEventPost(1, down.0);
        CGEventPost(1, up.0);
    }
    // Restore only if no other app or person has changed the clipboard meanwhile.
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        let paste = NSPasteboard::generalPasteboard();
        if paste.changeCount() == count {
            paste.clearContents();
            for (kind, data) in &saved {
                paste.setData_forType(Some(data), kind);
            }
        }
    });
    unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, false, &block);
    }
    Ok(())
}
#[derive(Clone, Copy)]
enum Input {
    Flags(bool, bool),
    HotkeyDown,
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
    clipboard_attempt: Option<crate::remote::Attempt>,
    selected_remote: crate::remote_review::Selection<RustDeskWindow>,
}
impl Shell {
    fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        // Opt-in native integration fixture. No credentials or microphone are used.
        let remote_fixture = std::env::args().any(|arg| arg == "--remote-fixture");
        let menu = Menu::new();
        let status = MenuItem::with_id("status", "Hold Control to dictate", false, None);
        let cancel = MenuItem::with_id("cancel", "Cancel dictation (Esc)", true, None);
        let start = MenuItem::with_id("start", "Start dictation", true, None);
        let stop = MenuItem::with_id("stop", "Stop dictation", true, None);
        let copy = MenuItem::with_id("copy", "Copy Last Transcript", true, None);
        let setup = MenuItem::with_id("setup", "Set up permissions", true, None);
        let quit = MenuItem::with_id("quit", "Quit Text-to-speech", true, None);
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
            &setup,
            &quit,
        ] {
            menu.append(item).map_err(|e| e.to_string())?;
        }
        let tray = TrayIconBuilder::new()
            .with_title("Control")
            .with_tooltip("Text-to-speech")
            .with_menu(Box::new(menu.clone()))
            .build()
            .map_err(|e| e.to_string())?;
        let screen = NSScreen::mainScreen(mtm).ok_or("No screen")?.visibleFrame();
        let frame = NSRect::new(
            NSPoint::new(
                screen.origin.x + (screen.size.width - 300.0) / 2.0,
                screen.origin.y + 38.0,
            ),
            NSSize::new(300.0, 44.0),
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
        panel.setIgnoresMouseEvents(false);
        panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
        let content = panel.contentView().ok_or("No panel content")?;
        // SAFETY: muda owns this NSMenu; AppKit retains the menu on the content view.
        unsafe {
            content.setMenu(Some(&*menu.ns_menu().cast::<NSMenu>()));
        }
        content.setToolTip(Some(&NSString::from_str(
            "Right-click for dictation and RustDesk controls",
        )));
        content.setWantsLayer(true);
        if let Some(layer) = content.layer() {
            unsafe {
                let _: () = objc2::msg_send![&*layer, setCornerRadius: 22.0f64];
            }
            layer.setMasksToBounds(true);
        }
        unsafe { panel.setReleasedWhenClosed(false) };
        let label = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Hold Control to dictate"),
            mtm,
        );
        label.setFrame(NSRect::new(
            NSPoint::new(18.0, 12.0),
            NSSize::new(264.0, 20.0),
        ));
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        label.setTextColor(Some(&NSColor::labelColor()));
        panel
            .contentView()
            .ok_or("No panel content")?
            .addSubview(&label);
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
            panel,
            label,
            last: if remote_fixture {
                "Synthetic dictation test — Café 👋".into()
            } else {
                String::new()
            },
            message: String::new(),
            started: Instant::now(),
            hide_at: None,
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
            clipboard_attempt: None,
            selected_remote: Default::default(),
        })
    }
    fn show(&mut self, message: &str, temporary: bool) {
        eprintln!("status: {message}");
        self.message = message.into();
        self.status.set_text(message);
        let short = if message.chars().count() > 42 {
            "⚠ Check Control menu for details"
        } else {
            message
        };
        self.label.setStringValue(&NSString::from_str(short));
        self.panel.orderFrontRegardless();
        self.hide_at = temporary.then(|| Instant::now() + Duration::from_secs(5));
    }
    fn cancel(&mut self) {
        self.review.cancel();
        self.control.store(2, Ordering::Release);
        if self.core.cancel() {
            self.focus = None;
            self.show("Cancelled", true);
        }
    }
    fn start(&mut self) {
        let focus = match Focus::current() {
            Ok(f) if !f.target.secure && f.target.editable => Some(f),
            Ok(f) if f.target.secure => {
                self.cancel();
                self.show(
                    "Choose a supported text field. Password fields are blocked.",
                    true,
                );
                return;
            }
            _ => None,
        };
        let platform = fotw_audio::platform::macos::MacOsPlatform::new();
        if platform.permission(Permission::Microphone) != PermissionState::Granted {
            request_microphone();
            self.cancel();
            self.show(
                "Use Set up permissions in the Control menu, then try again.",
                true,
            );
            return;
        }
        self.focus = if self.remote_mode { None } else { focus };
        self.control = Arc::new(AtomicU8::new(0));
        self.receiving = Arc::new(AtomicBool::new(false));
        self.started = Instant::now();
        self.last.clear();
        self.show("◌ Starting microphone…", false);
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
    fn finish(&mut self) {
        self.control.store(1, Ordering::Release);
        self.show("Finishing… · Esc to cancel", false);
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
        if self.core.phase == Phase::Recording && self.receiving.load(Ordering::Acquire) {
            let frames = ["▁ ▃ ▆ ▃ ▁", "▃ ▆ █ ▆ ▃", "▆ ▃ ▁ ▃ ▆", "▃ ▁ ▃ ▆ █"];
            let frame = frames[(self.started.elapsed().as_millis() / 160 % 4) as usize];
            self.label
                .setStringValue(&NSString::from_str(&format!("● Listening  {frame}")));
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
            self.show("Input queue overflow; dictation cancelled", true);
        }
        while let Ok(event) = self.input.try_recv() {
            match event {
                Input::HotkeyDown | Input::Flags(_, _) if self.remote_mode => {
                    self.show("Remote mode: use Start/Stop in menu", true);
                }
                Input::HotkeyDown => self.show("◌ Control held — keep holding", true),
                Input::Flags(down, other) => match self.core.flags(down, other) {
                    Some("start") => self.start(),
                    Some("finish") => self.finish(),
                    Some("cancel") => {
                        self.control.store(2, Ordering::Release);
                        self.focus = None;
                        self.show("Cancelled Control shortcut", true)
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
                    self.cancel();
                    self.remote_mode = !self.remote_mode;
                    self.remote_toggle.set_text(if self.remote_mode {
                        "Remote review mode: On"
                    } else {
                        "Remote review mode: Off"
                    });
                    self.show(
                        if self.remote_mode {
                            "Remote mode: use Start/Stop in menu"
                        } else {
                            "● Ready · hold Control"
                        },
                        false,
                    );
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
                "remote_review" if self.core.phase == Phase::Idle => self.review_remote(),
                "remote_restore" if self.core.phase == Phase::Idle => {
                    self.restore_remote_clipboard()
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
                        "Enable Accessibility. Keep Wispr Flow on Fn; hold Control here.",
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
                    eprintln!(
                        "transcription complete: {} characters",
                        text.chars().count()
                    );
                    self.last = text.clone();
                    if self.remote_mode {
                        self.show("Transcript ready · Review for RustDesk…", false);
                    } else {
                        let result = self
                            .focus
                            .as_ref()
                            .ok_or("Transcript ready — choose Copy Last Transcript".to_string())
                            .and_then(|f| f.insert(&text));
                        match result {
                            Ok(()) => self.show("Text inserted", true),
                            Err(e) => self.show(&e, true),
                        }
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
            if self.tap.is_none() && self.message.starts_with("Hold Control") {
                self.show(
                    "Keyboard access unavailable — use Start dictation in the Control menu",
                    false,
                );
            }
        }
        if self.hide_at.is_some_and(|time| Instant::now() >= time) {
            self.hide_at = None;
            let ready = if self.remote_mode {
                "Remote mode: use Start/Stop in menu"
            } else if unsafe { CGPreflightListenEventAccess() } {
                "● Ready · hold Control"
            } else {
                "⚠ Keyboard permission needed"
            };
            self.show(ready, false);
        }
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
        if bundle.to_string() != "com.carriez.rustdesk" {
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
            self.show(
                "Restore the previous clipboard before copying another transcript",
                true,
            );
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
                    true,
                );
                return;
            }
            Err(_) => {
                remote_selection_diagnostic();
                self.show(
                    "Focus the intended RustDesk remote window, then choose Review for RustDesk",
                    true,
                );
                return;
            }
        };
        let selected = selected.expect("review validated selected window");
        let mtm = MainThreadMarker::new().expect("shell main thread");
        let Some(confirmed) = review_dialog(&selected.title, &self.last, self.profile, mtm) else {
            self.review.cancel();
            self.show("Remote copy cancelled", true);
            return;
        };
        let text = match self.review.confirm(id, selected.revalidate(), confirmed) {
            Ok(text) => text,
            Err(Error::ConfirmationNeeded) => {
                self.review.cancel();
                self.show(
                    "Confirm the intended session before sharing; nothing copied",
                    true,
                );
                return;
            }
            Err(_) => {
                self.show("RustDesk window changed; review again before sharing", true);
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
            Ok(()) => self.show("Copied · paste manually in RustDesk",false),
            Err(crate::remote_clipboard::Error::Unsupported) => self.show("Clipboard format cannot be preserved; keep transcript and try after copying plain text",true),
            Err(_) => self.show("Copy failed or clipboard changed; use Restore previous clipboard if available",true),
        }
    }
    fn restore_remote_clipboard(&mut self) {
        use crate::remote_clipboard::{Recovery, RestoreReason};
        let Some(attempt) = self.clipboard_attempt else {
            self.show("No previous clipboard to restore", true);
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
            Ok(Recovery::Restored) => self.show("Previous clipboard restored", true),
            Ok(Recovery::OwnershipLost) => self.show("Newer clipboard kept", true),
            Ok(_) => self.show("No previous clipboard to restore", true),
            Err(_) => self.show("Restore failed; recovery retained for retry", true),
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
        .is_some_and(|id| id.to_string() == "com.carriez.rustdesk");
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
    if shell.borrow().remote_mode {
        shell
            .borrow_mut()
            .show("Synthetic fixture · Review for RustDesk", false);
    } else {
        shell.borrow_mut().show("● Ready · hold Control", true);
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
