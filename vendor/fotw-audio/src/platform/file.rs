//! [`FileAudioSource`] — an [`AudioTap`] backed by a WAV fixture.
//!
//! This is the single most load-bearing piece of test infrastructure in the
//! project. Device-dependent CI is close to unachievable here: GitHub macOS
//! runners have recurring null-audio-device regressions, and Core Audio taps
//! additionally need a signed binary plus a TCC grant that cannot be given
//! non-interactively. So roughly 90% of coverage has to come from replaying
//! fixtures through the real pipeline (docs/REQUIREMENTS.md 5.6).
//!
//! Replay honours [`ReplaySpeed`], so a 90-minute meeting fixture can be run
//! at 50× and finish inside a CI step while still exercising the same
//! callback cadence, timestamps and flags a real tap produces.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::clock;
use crate::error::TapError;
use crate::events::{EventBus, PlatformEvent};
use crate::format::{FormatRequest, StreamFormat};
use crate::frames::{CaptureTimestamp, FrameFlags, FrameSink};
use crate::ids::{AppInfo, DeviceId, DeviceInfo, TapId};
use crate::permission::{Permission, PermissionState, PlatformCaps};
use crate::tap::{AudioPlatform, AudioTap, BoxFuture, SystemScope};
use crate::wav::{self, WavData};

/// How fast to replay a fixture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReplaySpeed {
    /// Wall-clock pace, as a real device would.
    Realtime,
    /// `n`× real time. `50.0` turns 90 minutes of fixture into ~108 seconds.
    Multiplier(f32),
    /// No pacing at all — deliver every buffer as fast as the sink accepts it.
    Unpaced,
}

impl ReplaySpeed {
    /// Divisor applied to fixture time, or `None` for no pacing at all.
    fn divisor(self) -> Option<f64> {
        match self {
            Self::Realtime => Some(1.0),
            Self::Multiplier(m) if m > 0.0 => Some(f64::from(m)),
            // A non-positive multiplier is a caller bug; treat it as unpaced
            // rather than dividing by zero and hanging a test forever.
            Self::Multiplier(_) | Self::Unpaced => None,
        }
    }
}

/// Don't bother sleeping for less than this.
///
/// `thread::sleep` cannot resolve sub-millisecond waits — it rounds up to the
/// scheduler's granularity, which on a loaded machine is a few milliseconds.
/// Sleeping per 10 ms chunk therefore makes high multipliers a lie: at 50x the
/// per-chunk wait is 200 µs, so 200 chunks that should total 40 ms actually
/// take ~540 ms and the effective speed collapses to about 4x. Pacing against
/// a running deadline and skipping waits below this threshold keeps the
/// multiplier honest, which is what lets a 90-minute fixture finish inside a
/// CI step.
const MIN_SLEEP: Duration = Duration::from_millis(1);

/// Frames per delivered buffer. 10 ms at the fixture's rate, matching the unit
/// the rest of the pipeline is driven in (the AEC requires exactly 10 ms).
const CHUNK_MS: u64 = 10;

/// An [`AudioTap`] that replays a fixture.
#[derive(Debug)]
pub struct FileAudioSource {
    id: TapId,
    data: Arc<WavData>,
    speed: ReplaySpeed,
    hint: StreamFormat,
    started: bool,
    stop: Arc<AtomicBool>,
    delivered: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl FileAudioSource {
    /// Build a source from already-decoded fixture audio.
    #[must_use]
    pub fn from_wav(id: TapId, data: WavData, speed: ReplaySpeed) -> Self {
        // Before start() the tap may only echo a hint. Using the fixture's own
        // format as that hint is the honest choice, and the test that asserts
        // "format is not authoritative before start" still holds because
        // authority is tracked separately from the value.
        let hint = data.format;
        Self {
            id,
            data: Arc::new(data),
            speed,
            hint,
            started: false,
            stop: Arc::new(AtomicBool::new(false)),
            delivered: Arc::new(AtomicU64::new(0)),
            worker: None,
        }
    }

    /// Build a source from a WAV file on disk.
    pub fn open(id: TapId, path: impl AsRef<Path>, speed: ReplaySpeed) -> Result<Self, TapError> {
        Ok(Self::from_wav(id, wav::read(path)?, speed))
    }

    /// Total frames in the fixture.
    #[must_use]
    pub fn total_frames(&self) -> usize {
        self.data.frames()
    }

    /// Frames delivered to the sink so far.
    #[must_use]
    pub fn delivered_frames(&self) -> u64 {
        self.delivered.load(Ordering::Acquire)
    }

    /// Block until the fixture has been fully replayed or the tap is stopped.
    ///
    /// Tests use this instead of sleeping, so they are deterministic rather
    /// than timing-dependent.
    pub fn wait_for_completion(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl AudioTap for FileAudioSource {
    fn id(&self) -> &TapId {
        &self.id
    }

    fn format(&self) -> StreamFormat {
        if self.started {
            self.data.format
        } else {
            self.hint
        }
    }

    fn format_is_authoritative(&self) -> bool {
        self.started
    }

    fn start(&mut self, mut sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        if self.started {
            return Err(TapError::platform("FileAudioSource is already started"));
        }
        let format = self.data.format;
        if !format.is_plausible() {
            return Err(TapError::platform(format!(
                "fixture declares an implausible format: {format}"
            )));
        }

        self.stop.store(false, Ordering::Release);
        self.delivered.store(0, Ordering::Release);

        let data = Arc::clone(&self.data);
        let stop = Arc::clone(&self.stop);
        let delivered = Arc::clone(&self.delivered);
        let speed = self.speed;
        let channels = usize::from(format.channels);
        let chunk_frames = ((u64::from(format.sample_rate_hz) * CHUNK_MS) / 1_000).max(1) as usize;
        let chunk_samples = chunk_frames * channels;

        let divisor = speed.divisor();

        self.worker = Some(thread::spawn(move || {
            // Pacing only, never stamping. This tap's schedule is relative to
            // its own start — the fixture begins when the worker does — but its
            // *timestamps* are not: see the stamp below for what timing them
            // from here cost (#89).
            let paced_from = Instant::now();
            let mut device_frames: u64 = 0;
            let mut flags = FrameFlags::empty();

            for chunk in data.samples.chunks(chunk_samples) {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let frames = (chunk.len() / channels) as u64;
                device_frames += frames;

                // Pace against a running deadline derived from total fixture
                // time, not per-chunk sleeps: rounding error and sleep
                // granularity would otherwise accumulate across thousands of
                // chunks and dominate the schedule entirely.
                if let Some(divisor) = divisor {
                    let target_ns = format.frames_to_ns(device_frames) as f64 / divisor;
                    let deadline = Duration::from_nanos(target_ns as u64);
                    if let Some(wait) = deadline.checked_sub(paced_from.elapsed())
                        && wait >= MIN_SLEEP
                    {
                        thread::sleep(wait);
                    }
                }

                // Stamp at the boundary from the one process-wide clock, as a
                // real backend must (seam rule 3) — `clock::host_ns`, exactly
                // what the macOS taps stamp from, and not this worker's own
                // start. Timing it from `paced_from` read identically within a
                // tap and was exactly backwards across two: both legs stamped
                // their first buffer at ≈0 however far apart they really began,
                // so `fotwd`'s cross-leg anchoring (#86) saw every fixture
                // session as two taps that woke together and declined to anchor
                // (#89). `device_frames` counts what came BEFORE this buffer,
                // hence the subtraction.
                let ts = CaptureTimestamp::new(device_frames - frames, clock::host_ns());

                // A fixture that is entirely zero is legitimately silent, and
                // reporting that is how the layer above learns to distinguish
                // "quiet room" from "tap died" (CAP-05).
                let silent = chunk.iter().all(|s| *s == 0.0);
                flags.set(FrameFlags::SILENT, silent);

                sink.on_frames(chunk, ts, flags);
                delivered.store(device_frames, Ordering::Release);
            }
        }));

        self.started = true;
        Ok(format)
    }

    fn stop(&mut self) -> Result<(), TapError> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.started = false;
        Ok(())
    }
}

impl Drop for FileAudioSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// An [`AudioPlatform`] that serves [`FileAudioSource`] taps.
///
/// Lets the whole pipeline be driven from fixtures through the ordinary
/// platform interface, with no device, no GUI and no TCC grant.
#[derive(Debug)]
pub struct FilePlatform {
    mic: Option<WavData>,
    system: Option<WavData>,
    speed: ReplaySpeed,
    bus: EventBus,
}

impl FilePlatform {
    /// A platform with no fixtures loaded.
    #[must_use]
    pub fn new(speed: ReplaySpeed) -> Self {
        Self {
            mic: None,
            system: None,
            speed,
            bus: EventBus::new(),
        }
    }

    /// Serve `data` as the microphone leg.
    #[must_use]
    pub fn with_mic(mut self, data: WavData) -> Self {
        self.mic = Some(data);
        self
    }

    /// Serve `data` as the system-audio leg.
    #[must_use]
    pub fn with_system(mut self, data: WavData) -> Self {
        self.system = Some(data);
        self
    }

    /// Publish a platform event, so tests can drive device-change handling.
    pub fn emit(&self, event: PlatformEvent) {
        self.bus.emit(event);
    }
}

impl AudioPlatform for FilePlatform {
    fn caps(&self) -> PlatformCaps {
        PlatformCaps {
            system_mix: true,
            app_scoped: false,
            exclude_scope: false,
            // A fixture always has samples to deliver, including silent ones.
            emits_silence_when_idle: true,
            needs_consent_for_system: false,
        }
    }

    fn permission(&self, _permission: Permission) -> PermissionState {
        PermissionState::NotApplicable
    }

    fn request_permission(&self, _permission: Permission) -> BoxFuture<'static, PermissionState> {
        Box::pin(async { PermissionState::NotApplicable })
    }

    fn mics(&self) -> Vec<DeviceInfo> {
        self.mic
            .as_ref()
            .map(|d| {
                vec![
                    DeviceInfo::new(DeviceId::new("fixture-mic"), "Fixture microphone", true)
                        .with_nominal_format(d.format),
                ]
            })
            .unwrap_or_default()
    }

    fn capturable_apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }

    fn open_mic(
        &self,
        _device: &DeviceId,
        _hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        let data = self
            .mic
            .clone()
            .ok_or_else(|| TapError::unsupported("FilePlatform has no mic fixture"))?;
        Ok(Box::new(FileAudioSource::from_wav(
            TapId::mic("fixture-mic"),
            data,
            self.speed,
        )))
    }

    fn open_system(
        &self,
        scope: SystemScope,
        _hint: FormatRequest,
    ) -> Result<Box<dyn AudioTap>, TapError> {
        if !matches!(scope, SystemScope::DefaultOutputMix) {
            return Err(TapError::unsupported(
                "FilePlatform serves only the default output mix",
            ));
        }
        let data = self
            .system
            .clone()
            .ok_or_else(|| TapError::unsupported("FilePlatform has no system fixture"))?;
        Ok(Box::new(FileAudioSource::from_wav(
            TapId::system_default(),
            data,
            self.speed,
        )))
    }

    fn events(&self) -> Receiver<PlatformEvent> {
        self.bus.subscribe()
    }
}
