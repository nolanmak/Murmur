//! Fakes shared with the crates above the seam.
//!
//! Everything here exists so the pipeline, STT and storage layers can be
//! tested with no audio device, no GUI and no TCC grant. Shipped in the crate
//! proper (not behind `#[cfg(test)]`) precisely so downstream crates can use
//! it (docs/REQUIREMENTS.md 5.6).

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use crate::activity::{ActivityProbe, ActivitySnapshot};
use crate::clock::Clock;
use crate::error::TapError;
use crate::events::{EventBus, PlatformEvent};
use crate::format::{FormatRequest, StreamFormat};
use crate::frames::{CaptureTimestamp, FrameFlags, FrameSink};
use crate::ids::{AppInfo, DeviceId, DeviceInfo, TapId};
use crate::permission::{Permission, PermissionState, PlatformCaps};
use crate::tap::{AudioPlatform, AudioTap, BoxFuture, SystemScope};

/// Drive a future to completion on the current thread.
///
/// The seam's futures are all immediately ready, so this needs no reactor. It
/// exists to keep an async runtime out of the crate's dependency graph.
/// # Panics
///
/// Panics if the future is still pending after [`BLOCK_ON_POLL_LIMIT`] polls.
/// The waker is a no-op, so a future that genuinely needs waking would
/// otherwise spin forever and hang CI with no diagnostic — a bounded panic is
/// a far better failure than a wedged runner.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    for _ in 0..BLOCK_ON_POLL_LIMIT {
        if let Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        std::hint::spin_loop();
    }
    panic!(
        "block_on: future still pending after {BLOCK_ON_POLL_LIMIT} polls. \
         This helper drives immediately-ready futures only; it has no reactor \
         and its waker does nothing."
    );
}

/// How many times [`block_on`] polls before giving up.
pub const BLOCK_ON_POLL_LIMIT: usize = 1_000_000;

/// A [`Clock`] a test moves by hand.
///
/// The stall watchdog's thresholds are 5 s and 8 s. Testing it against the
/// real clock would mean a suite that sleeps for thirteen seconds per case,
/// which is a suite nobody runs and therefore a watchdog nobody trusts. With
/// this, a 45-minute meeting is exercised in a few milliseconds.
///
/// Interior mutability so it can sit behind the `Arc<dyn Clock>` the
/// supervisor holds while the test still moves it.
#[derive(Debug, Default)]
pub struct ManualClock {
    ns: AtomicU64,
}

impl ManualClock {
    /// A clock at zero, ready to share with whatever is being tested.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Move forward. Never backwards: the seam's whole timing model assumes a
    /// monotonic clock, and a test that could violate it would be testing
    /// something that cannot happen.
    pub fn advance(&self, by: Duration) {
        self.ns.fetch_add(by.as_nanos() as u64, Ordering::Relaxed);
    }

    /// The current reading.
    #[must_use]
    pub fn now_ns(&self) -> u64 {
        self.ns.load(Ordering::Relaxed)
    }
}

impl Clock for ManualClock {
    fn now_ns(&self) -> u64 {
        Self::now_ns(self)
    }
}

/// Something that happened to a [`FakeTap`].
///
/// Recovery correctness is an *ordering* property — stop, destroy, only then
/// create — and ordering is invisible to a counter and to the type system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapEvent {
    /// A tap object was created.
    Opened(TapId),
    /// It began delivering, at this format.
    Started(StreamFormat),
    /// It was stopped.
    Stopped,
    /// It was dropped — on macOS, where the aggregate device and the process
    /// tap are actually destroyed.
    Dropped,
}

/// A shared journal of what happened to a series of [`FakeTap`]s.
///
/// Shared and `Arc`-backed so it survives the taps themselves: a supervisor
/// drops the tap it is replacing, and "was it dropped before the replacement
/// was created?" is exactly the question worth asking.
#[derive(Debug, Clone, Default)]
pub struct TapLog {
    events: Arc<Mutex<Vec<TapEvent>>>,
}

impl TapLog {
    /// An empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an event.
    pub fn record(&self, event: TapEvent) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }

    /// Everything recorded, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<TapEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Forget everything, so a test can ignore setup noise.
    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

#[derive(Debug, Default)]
struct SinkState {
    calls: usize,
    samples: Vec<f32>,
    timestamps: Vec<CaptureTimestamp>,
    flags: Vec<FrameFlags>,
    errors: Vec<String>,
}

/// A recording [`FrameSink`] whose observations survive the sink being moved
/// into a tap.
#[derive(Debug, Clone, Default)]
pub struct SinkHandle {
    state: Arc<Mutex<SinkState>>,
}

impl SinkHandle {
    /// A fresh handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A sink that records into this handle. May be called more than once; all
    /// sinks share the handle's state, which is what lets a test observe a tap
    /// across a restart.
    #[must_use]
    pub fn sink(&self) -> Box<dyn FrameSink> {
        Box::new(RecordingSink {
            state: Arc::clone(&self.state),
        })
    }

    fn read<T>(&self, f: impl FnOnce(&SinkState) -> T) -> T {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&guard)
    }

    /// How many `on_frames` callbacks arrived.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.read(|s| s.calls)
    }

    /// Every sample delivered, concatenated.
    #[must_use]
    pub fn samples(&self) -> Vec<f32> {
        self.read(|s| s.samples.clone())
    }

    /// Every timestamp delivered.
    #[must_use]
    pub fn timestamps(&self) -> Vec<CaptureTimestamp> {
        self.read(|s| s.timestamps.clone())
    }

    /// Every flag set delivered.
    #[must_use]
    pub fn flags(&self) -> Vec<FrameFlags> {
        self.read(|s| s.flags.clone())
    }

    /// Errors reported through `on_error`.
    #[must_use]
    pub fn errors(&self) -> Vec<String> {
        self.read(|s| s.errors.clone())
    }

    /// True when every consecutive pair of timestamps advances on both clocks.
    ///
    /// A failure means a backend is stamping from more than one clock, which
    /// silently corrupts two-stream alignment — the bug that would quietly
    /// break "me vs them" attribution.
    #[must_use]
    pub fn timestamps_are_monotonic(&self) -> bool {
        self.read(|s| {
            s.timestamps
                .windows(2)
                .all(|w| w[1].is_monotonic_after(&w[0]))
        })
    }
}

#[derive(Debug)]
struct RecordingSink {
    state: Arc<Mutex<SinkState>>,
}

impl FrameSink for RecordingSink {
    fn on_frames(&mut self, interleaved_f32: &[f32], ts: CaptureTimestamp, flags: FrameFlags) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.calls += 1;
        s.samples.extend_from_slice(interleaved_f32);
        s.timestamps.push(ts);
        s.flags.push(flags);
    }

    fn on_error(&mut self, e: TapError) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.errors.push(e.to_string());
    }
}

/// A tap whose behaviour a test scripts directly.
pub struct FakeTap {
    id: TapId,
    hint: StreamFormat,
    /// Formats to report from successive `start()` calls. The last one repeats
    /// once exhausted, so a test only lists the transitions it cares about.
    start_formats: Vec<StreamFormat>,
    starts: usize,
    current: Option<StreamFormat>,
    sink: Option<Box<dyn FrameSink>>,
    fail_next_start: Option<TapError>,
    delivered_frames: u64,
    log: Option<TapLog>,
}

impl Drop for FakeTap {
    fn drop(&mut self) {
        if let Some(log) = &self.log {
            log.record(TapEvent::Dropped);
        }
    }
}

// Hand-written because `dyn FrameSink` is not `Debug` — and making it so would
// put a formatting requirement on a trait whose implementations sit on the
// real-time path, where nothing may allocate.
impl std::fmt::Debug for FakeTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeTap")
            .field("id", &self.id)
            .field("hint", &self.hint)
            .field("start_formats", &self.start_formats)
            .field("starts", &self.starts)
            .field("current", &self.current)
            .field("delivered_frames", &self.delivered_frames)
            .field("sink_attached", &self.sink.is_some())
            .finish()
    }
}

impl FakeTap {
    /// A tap that will echo `hint` until started.
    #[must_use]
    pub fn new(id: TapId, hint: StreamFormat) -> Self {
        Self {
            id,
            hint,
            start_formats: Vec::new(),
            starts: 0,
            current: None,
            sink: None,
            fail_next_start: None,
            delivered_frames: 0,
            log: None,
        }
    }

    /// Journal this tap's lifecycle into a shared [`TapLog`].
    ///
    /// Records [`TapEvent::Opened`] immediately, because for a supervisor
    /// "when was the replacement created" is half the ordering question.
    #[must_use]
    pub fn with_log(mut self, log: TapLog) -> Self {
        log.record(TapEvent::Opened(self.id.clone()));
        self.log = Some(log);
        self
    }

    /// Script the format each successive `start()` reports.
    #[must_use]
    pub fn with_start_formats(mut self, formats: impl IntoIterator<Item = StreamFormat>) -> Self {
        self.start_formats = formats.into_iter().collect();
        self
    }

    /// Make the next `start()` fail, so error paths can be exercised.
    #[must_use]
    pub fn failing_next_start(mut self, error: TapError) -> Self {
        self.fail_next_start = Some(error);
        self
    }

    /// Deliver `samples` to whatever sink is currently attached.
    ///
    /// Silently does nothing when stopped, mirroring a real tap: a stopped tap
    /// does not deliver, and it does not panic about it either.
    pub fn emit(&mut self, samples: &[f32]) {
        self.emit_with(samples, FrameFlags::empty());
    }

    /// Deliver `samples` with explicit flags.
    pub fn emit_with(&mut self, samples: &[f32], flags: FrameFlags) {
        let format = match self.current {
            Some(f) => f,
            None => return,
        };
        let frames = format.frames_in(samples.len()).unwrap_or(0) as u64;
        // `device_frames` counts frames delivered BEFORE this buffer, so it is
        // a running total, not this buffer's length. Stamping the length here
        // would look monotonic for buffers of increasing size and then silently
        // stall — two equal buffers in a row would report the same position,
        // and the alignment layer would place them on top of each other.
        let ts = CaptureTimestamp::new(self.delivered_frames, crate::clock::host_ns());
        self.delivered_frames += frames;
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        sink.on_frames(samples, ts, flags);
    }

    /// Total frames this tap has delivered since it was created.
    #[must_use]
    pub const fn delivered_frames(&self) -> u64 {
        self.delivered_frames
    }

    /// Report an error through the attached sink.
    pub fn emit_error(&mut self, error: TapError) {
        if let Some(sink) = self.sink.as_mut() {
            sink.on_error(error);
        }
    }

    /// How many times this tap has been started.
    #[must_use]
    pub const fn start_count(&self) -> usize {
        self.starts
    }
}

impl AudioTap for FakeTap {
    fn id(&self) -> &TapId {
        &self.id
    }

    fn format(&self) -> StreamFormat {
        self.current.unwrap_or(self.hint)
    }

    fn format_is_authoritative(&self) -> bool {
        self.current.is_some()
    }

    fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        if let Some(e) = self.fail_next_start.take() {
            return Err(e);
        }
        let format = self
            .start_formats
            .get(self.starts)
            .copied()
            .or_else(|| self.start_formats.last().copied())
            .unwrap_or(self.hint);
        self.starts += 1;
        self.current = Some(format);
        self.sink = Some(sink);
        if let Some(log) = &self.log {
            log.record(TapEvent::Started(format));
        }
        Ok(format)
    }

    fn stop(&mut self) -> Result<(), TapError> {
        self.current = None;
        self.sink = None;
        if let Some(log) = &self.log {
            log.record(TapEvent::Stopped);
        }
        Ok(())
    }
}

/// A scriptable [`AudioPlatform`].
#[derive(Debug)]
pub struct MockPlatform {
    caps: PlatformCaps,
    permissions: Vec<(Permission, PermissionState, PermissionState)>,
    apps: Vec<AppInfo>,
    mics: Vec<DeviceInfo>,
    bus: EventBus,
}

impl MockPlatform {
    /// A platform shaped like Windows endpoint loopback.
    ///
    /// The interesting cases in one object: system audio needs no consent at
    /// all ([`PermissionState::NotApplicable`]), the mic does, nothing is
    /// delivered while the endpoint is idle, and app scoping is unavailable.
    #[must_use]
    pub fn windows_loopback() -> Self {
        Self {
            caps: PlatformCaps {
                system_mix: true,
                app_scoped: false,
                exclude_scope: false,
                emits_silence_when_idle: false,
                needs_consent_for_system: false,
            },
            permissions: vec![
                (
                    Permission::SystemAudio,
                    PermissionState::NotApplicable,
                    PermissionState::NotApplicable,
                ),
                (
                    Permission::Microphone,
                    PermissionState::NotDetermined,
                    PermissionState::Granted,
                ),
            ],
            apps: Vec::new(),
            mics: Vec::new(),
            bus: EventBus::new(),
        }
    }

    /// A platform shaped like macOS Core Audio taps: system audio requires a
    /// grant that cannot be observed at all.
    #[must_use]
    pub fn macos_taps() -> Self {
        Self {
            caps: PlatformCaps {
                system_mix: true,
                app_scoped: true,
                exclude_scope: true,
                emits_silence_when_idle: true,
                needs_consent_for_system: true,
            },
            permissions: vec![
                (
                    Permission::SystemAudio,
                    PermissionState::Unobservable,
                    PermissionState::Unobservable,
                ),
                (
                    Permission::Microphone,
                    PermissionState::NotDetermined,
                    PermissionState::Granted,
                ),
            ],
            apps: Vec::new(),
            mics: Vec::new(),
            bus: EventBus::new(),
        }
    }

    /// Replace the advertised capabilities.
    #[must_use]
    pub const fn with_caps(mut self, caps: PlatformCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Advertise capturable applications.
    #[must_use]
    pub fn with_apps(mut self, apps: Vec<AppInfo>) -> Self {
        self.apps = apps;
        self
    }

    /// Advertise microphones.
    #[must_use]
    pub fn with_mics(mut self, mics: Vec<DeviceInfo>) -> Self {
        self.mics = mics;
        self
    }

    /// Publish a platform event to every subscriber.
    pub fn emit(&self, event: PlatformEvent) {
        self.bus.emit(event);
    }

    fn lookup(&self, permission: Permission) -> (PermissionState, PermissionState) {
        self.permissions
            .iter()
            .find(|(p, _, _)| *p == permission)
            .map(|(_, now, after)| (*now, *after))
            .unwrap_or((PermissionState::NotDetermined, PermissionState::Denied))
    }
}

impl AudioPlatform for MockPlatform {
    fn caps(&self) -> PlatformCaps {
        self.caps
    }

    fn permission(&self, permission: Permission) -> PermissionState {
        self.lookup(permission).0
    }

    fn request_permission(&self, permission: Permission) -> BoxFuture<'static, PermissionState> {
        let after = self.lookup(permission).1;
        Box::pin(async move { after })
    }

    fn mics(&self) -> Vec<DeviceInfo> {
        self.mics.clone()
    }

    fn capturable_apps(&self) -> Vec<AppInfo> {
        self.apps.clone()
    }

    fn open_mic(
        &self,
        device: &DeviceId,
        hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        let format = hint.resolve(StreamFormat::new(
            48_000,
            1,
            crate::format::SampleFormat::F32,
        ));
        Ok(Box::new(FakeTap::new(TapId::mic(device.as_str()), format)))
    }

    fn open_system(
        &self,
        scope: SystemScope,
        hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        match &scope {
            SystemScope::Apps(_) if !self.caps.app_scoped => {
                return Err(TapError::unsupported(
                    "this platform cannot scope system capture to specific applications",
                ));
            }
            SystemScope::AllExcept(_) if !self.caps.exclude_scope => {
                return Err(TapError::unsupported(
                    "this platform cannot exclude applications from system capture",
                ));
            }
            _ => {}
        }
        let format = hint.resolve(StreamFormat::new(
            48_000,
            2,
            crate::format::SampleFormat::F32,
        ));
        Ok(Box::new(FakeTap::new(TapId::system_default(), format)))
    }

    fn events(&self) -> Receiver<PlatformEvent> {
        self.bus.subscribe()
    }
}

/// An [`ActivityProbe`] that replays a fixed answer.
///
/// This is what lets meeting detection be tested with **no conferencing app
/// installed and no audio device present** — the whole point of putting the
/// probe behind a trait. A test constructs the machine it wants to reason
/// about (Zoom holding the mic, AirPods as the default input, a probe that
/// fails) and the detector never learns it is not on a real Mac.
#[derive(Debug, Clone)]
pub struct FixedActivityProbe {
    snapshot: Arc<Mutex<Result<ActivitySnapshot, String>>>,
}

impl FixedActivityProbe {
    /// A probe that always reports `snapshot`.
    #[must_use]
    pub fn new(snapshot: ActivitySnapshot) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(Ok(snapshot))),
        }
    }

    /// A probe that reports an empty machine: nothing is using audio.
    #[must_use]
    pub fn quiet() -> Self {
        Self::new(ActivitySnapshot::default())
    }

    /// A probe that always fails.
    #[must_use]
    pub fn failing(reason: impl Into<String>) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(Err(reason.into()))),
        }
    }

    /// Replace what the probe reports from now on, so one test can walk a
    /// machine through a whole meeting.
    pub fn set(&self, snapshot: ActivitySnapshot) {
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = Ok(snapshot);
    }

    /// Make the probe start failing.
    pub fn fail(&self, reason: impl Into<String>) {
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = Err(reason.into());
    }
}

impl ActivityProbe for FixedActivityProbe {
    fn snapshot(&self) -> Result<ActivitySnapshot, TapError> {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map_err(TapError::platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SampleFormat;

    fn mono16k() -> StreamFormat {
        StreamFormat::new(16_000, 1, SampleFormat::I16)
    }

    /// Regression: `device_frames` is a running total of frames delivered
    /// *before* the current buffer, not the current buffer's length.
    ///
    /// Stamping the length looks correct for buffers of increasing size and
    /// then silently stalls — two equal buffers in a row report the same
    /// position, so the alignment layer stacks them on top of each other and
    /// "me vs them" attribution drifts with no error anywhere.
    #[test]
    fn equal_sized_buffers_still_advance_the_frame_counter() {
        let handle = SinkHandle::new();
        let mut tap = FakeTap::new(TapId::system_default(), mono16k());
        tap.start(handle.sink()).unwrap();

        tap.emit(&[0.1; 160]);
        tap.emit(&[0.2; 160]);
        tap.emit(&[0.3; 160]);

        let frames: Vec<u64> = handle
            .timestamps()
            .iter()
            .map(|t| t.device_frames)
            .collect();
        assert_eq!(
            frames,
            vec![0, 160, 320],
            "each buffer must start where the previous one ended"
        );
        assert_eq!(tap.delivered_frames(), 480);
        assert!(handle.timestamps_are_monotonic());
    }

    #[test]
    fn a_stopped_tap_delivers_nothing_and_does_not_panic() {
        let handle = SinkHandle::new();
        let mut tap = FakeTap::new(TapId::system_default(), mono16k());
        tap.start(handle.sink()).unwrap();
        tap.emit(&[0.1; 160]);
        tap.stop().unwrap();
        tap.emit(&[0.2; 160]);

        assert_eq!(handle.calls(), 1, "the post-stop emit was dropped");
    }

    #[test]
    fn block_on_resolves_an_immediately_ready_future() {
        assert_eq!(block_on(async { 7u8 }), 7);
    }
}
