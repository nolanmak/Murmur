//! Bounded, allocation-free handoff from FlyOnTheWall's microphone callback.
use crate::resample::{Downmixer, Resampler16k};
use fotw_audio::{AudioTap, CaptureTimestamp, FrameFlags, FrameSink, StreamFormat, TapError};
use rtrb::{Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use std::time::Duration;
/// Maximum wait for a microphone startup operation.
pub const MIC_START_DEADLINE: Duration = Duration::from_secs(4);
struct Sink {
    producer: Producer<f32>,
    failed: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
}
impl FrameSink for Sink {
    fn on_frames(&mut self, pcm: &[f32], _: CaptureTimestamp, flags: FrameFlags) {
        if self.failed.load(Ordering::Acquire) != 0 {
            return;
        }
        if flags.contains(FrameFlags::DISCONTINUITY) {
            self.fail(3);
            return;
        }
        if pcm.iter().any(|sample| !sample.is_finite()) {
            self.fail(4);
            return;
        }
        if !pcm.is_empty() {
            self.receiving.store(true, Ordering::Release);
        }
        for sample in pcm {
            if self.producer.push(*sample).is_err() {
                self.fail(1);
                break;
            }
        }
    }
    fn on_error(&mut self, error: TapError) {
        self.fail(match error {
            TapError::DeviceInvalidated(_) | TapError::DeviceNotFound(_) => 2,
            TapError::StreamFormatChanged | TapError::FormatChangedAfterStop => 6,
            _ => 5,
        });
    }
}
impl Sink {
    fn fail(&self, reason: u8) {
        // Preserve the first fault; subsequent or stale callbacks cannot heal it.
        let _ = self
            .failed
            .compare_exchange(0, reason, Ordering::AcqRel, Ordering::Acquire);
    }
}
fn audio_failure(failed: &AtomicU8) -> Option<&'static str> {
    match failed.load(Ordering::Acquire) {
        0 => None,
        1 => Some("Audio buffer overflow; no text inserted"),
        2 => Some("Microphone disconnected; no text inserted"),
        3 => Some("Microphone audio was interrupted; no text inserted"),
        4 => Some("Microphone delivered invalid samples; no text inserted"),
        6 => Some("Microphone format changed; start a new dictation; no text inserted"),
        _ => Some("Microphone failure; no text inserted"),
    }
}
/// Owned result of preparing one microphone and its device watcher.
pub struct PreparedInput {
    tap: Box<dyn AudioTap>,
    watcher: Option<Box<dyn fotw_audio::DeviceWatch>>,
}
impl PreparedInput {
    pub fn new(tap: Box<dyn AudioTap>) -> Self {
        Self { tap, watcher: None }
    }
    pub fn watched(tap: Box<dyn AudioTap>, watcher: Box<dyn fotw_audio::DeviceWatch>) -> Self {
        Self {
            tap,
            watcher: Some(watcher),
        }
    }
}
/// Deferred input setup, created without touching a native device.
pub struct InputPreparation(Box<dyn FnOnce() -> Result<PreparedInput, String> + Send>);
impl InputPreparation {
    pub fn new(prepare: impl FnOnce() -> Result<PreparedInput, String> + Send + 'static) -> Self {
        Self(Box::new(prepare))
    }
}

/// Maximum wait for native Stop and resource destruction after key release.
pub const MIC_STOP_DEADLINE: Duration = Duration::from_secs(4);
const CANCEL_CLEANUP_GRACE: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MicrophoneState {
    Idle = 0,
    Preparing = 1,
    Starting = 2,
    Capturing = 3,
    Stopping = 4,
    CleaningUp = 5,
    Unconfirmed = 6,
    CleanupFailed = 7,
}
/// One native owner across attempts. Only confirmed resource cleanup releases it.
#[derive(Clone, Default)]
pub struct MicrophoneStartup(Arc<AtomicU8>);
struct StartupLease {
    state: Arc<AtomicU8>,
    cleaned: bool,
}
impl StartupLease {
    fn phase(&self, state: MicrophoneState) {
        self.state.store(state as u8, Ordering::Release);
    }
}
impl Drop for StartupLease {
    fn drop(&mut self) {
        let state = if self.cleaned
            || self.state.load(Ordering::Acquire) == MicrophoneState::Preparing as u8
        {
            MicrophoneState::Idle
        } else if self.state.load(Ordering::Acquire) == MicrophoneState::CleaningUp as u8 {
            MicrophoneState::CleanupFailed
        } else {
            MicrophoneState::Unconfirmed
        };
        self.state.store(state as u8, Ordering::Release);
    }
}
impl MicrophoneStartup {
    pub fn state(&self) -> MicrophoneState {
        match self.0.load(Ordering::Acquire) {
            0 => MicrophoneState::Idle,
            1 => MicrophoneState::Preparing,
            2 => MicrophoneState::Starting,
            3 => MicrophoneState::Capturing,
            4 => MicrophoneState::Stopping,
            5 => MicrophoneState::CleaningUp,
            7 => MicrophoneState::CleanupFailed,
            _ => MicrophoneState::Unconfirmed,
        }
    }
    fn acquire(&self) -> Result<StartupLease, String> {
        self.0.compare_exchange(0, MicrophoneState::Preparing as u8, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Previous microphone operation is still pending; reconnect the input device or reopen Murmur".to_string())?;
        Ok(StartupLease {
            state: self.0.clone(),
            cleaned: false,
        })
    }
}

/// Lives only on the native owner thread. Drop cannot block the async caller.
struct NativeInput {
    tap: Option<Box<dyn AudioTap>>,
    watcher: Option<Box<dyn fotw_audio::DeviceWatch>>,
    stopped: Arc<AtomicBool>,
    lease: StartupLease,
}
impl NativeInput {
    fn close(&mut self) -> Result<(), String> {
        let Some(mut tap) = self.tap.take() else {
            return Ok(());
        };
        self.lease.phase(MicrophoneState::Stopping);
        let result = tap.stop_observed(&mut || {
            // Native Stop can succeed before registration/resource release.
            // Route notifications after this boundary cannot invalidate audio
            // already captured, even while cleanup still owns the lease.
            self.stopped.store(true, Ordering::Release);
            self.lease.phase(MicrophoneState::CleaningUp);
        });
        let evidence = result.as_ref().err().and_then(TapError::cleanup_state);
        if result.is_ok() || evidence.is_some_and(|state| state.capture_stopped()) {
            self.stopped.store(true, Ordering::Release);
            self.lease.phase(MicrophoneState::CleaningUp);
        } else if !self.stopped.load(Ordering::Acquire) {
            self.lease.phase(MicrophoneState::Unconfirmed);
        }
        let released = result.is_ok() || evidence.is_some_and(|state| state.resources_released());
        let format_changed = matches!(&result, Err(TapError::FormatChangedAfterStop));
        let result = result.map_err(|_| {
            if format_changed {
                "Microphone format changed; start a new dictation; no text inserted"
            } else if released {
                "Microphone stopped; native cleanup reported an error; no text inserted; try again"
            } else if self.stopped.load(Ordering::Acquire) {
                "Microphone stopped but cleanup failed; no text inserted; reopen Murmur"
            } else {
                "Microphone stop unconfirmed; no text inserted; reopen Murmur"
            }
            .to_string()
        });
        drop(tap);
        drop(self.watcher.take());
        self.lease.cleaned = released;
        result
    }
}
impl Drop for NativeInput {
    fn drop(&mut self) {
        if self.tap.is_some() {
            let _ = self.close();
        }
    }
}
/// Async-side ownership is just a command channel and completion receiver.
/// Dropping it requests cleanup; all native destruction stays on the owner.
struct Guard {
    stop: Option<std::sync::mpsc::SyncSender<()>>,
    closed: tokio::sync::oneshot::Receiver<Result<(), String>>,
    stopped: Arc<AtomicBool>,
    result: Option<Result<(), String>>,
}
impl Guard {
    async fn close(
        &mut self,
        control: &AtomicU8,
        changes: &fotw_audio::device_change::DeviceChangeSignal,
    ) -> Result<(), String> {
        self.stop.take();
        if let Some(result) = &self.result {
            return result.clone();
        }
        let deadline = tokio::time::sleep(MIC_STOP_DEADLINE);
        tokio::pin!(deadline);
        let mut cancelled_at = None;
        let mut tick = tokio::time::interval(Duration::from_millis(5));
        let result = loop {
            if control.load(Ordering::Acquire) == 2 {
                let began = cancelled_at.get_or_insert_with(std::time::Instant::now);
                if began.elapsed() >= CANCEL_CLEANUP_GRACE {
                    break Err("Cancelled".into());
                }
            }
            if !self.stopped.load(Ordering::Acquire)
                && let Some(error) = device_failure(changes)
            {
                break Err(error.to_string());
            }
            tokio::select! {
                biased;
                closed = &mut self.closed => break closed.unwrap_or_else(|_| Err(if self.stopped.load(Ordering::Acquire) {
                    "Microphone stopped but cleanup failed; no text inserted; reopen Murmur"
                } else {
                    "Microphone cleanup unconfirmed; no text inserted; reopen Murmur"
                }.into())),
                _ = &mut deadline => break Err("Microphone cleanup timed out; no text inserted; cleanup is still pending".into()),
                _ = tick.tick() => {}
            }
        };
        self.result = Some(result.clone());
        result
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.take();
    }
}

// Adapted from Murmur PR #53 (97fe25c). Carry the RAII Guard through the
// channel too: receiver cancellation after send must still close the tap.
async fn start_within_deadline(
    input: InputPreparation,
    sink: Box<dyn FrameSink>,
    control: &Arc<AtomicU8>,
    startup: &MicrophoneStartup,
    receiving: &AtomicBool,
    changes: &Arc<fotw_audio::device_change::DeviceChangeSignal>,
) -> Result<(Guard, StreamFormat), String> {
    if let Some(error) = device_failure(changes) {
        return Err(error.into());
    }
    let lease = startup.acquire()?;
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel::<()>(0);
    let (closed_tx, closed) = tokio::sync::oneshot::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let guard = Guard {
        stop: Some(stop_tx),
        closed,
        stopped: stopped.clone(),
        result: None,
    };
    let worker_control = control.clone();
    let worker_changes = changes.clone();
    std::thread::Builder::new()
        .name("murmur-mic-owner".into())
        .spawn(move || {
            let early_error = if tx.is_closed() || worker_control.load(Ordering::Acquire) != 0 {
                Some("Dictation ended before microphone was ready; try again".to_string())
            } else {
                device_failure(&worker_changes).map(str::to_string)
            };
            if let Some(error) = early_error {
                // Captured prebuilt inputs, if any, also drop on this owner
                // before its lease is released. Deferred native setup is skipped.
                drop(input);
                drop(lease);
                let _ = tx.send(Err(error));
                let _ = closed_tx.send(Ok(()));
                return;
            }
            let prepared = match (input.0)() {
                Ok(prepared) => prepared,
                Err(error) => {
                    drop(lease);
                    let _ = tx.send(Err(error));
                    let _ = closed_tx.send(Ok(()));
                    return;
                }
            };
            let mut native = NativeInput {
                tap: Some(prepared.tap),
                watcher: prepared.watcher,
                stopped,
                lease,
            };
            let start = if tx.is_closed() || worker_control.load(Ordering::Acquire) != 0 {
                Err("Dictation ended before microphone was ready; try again".to_string())
            } else if let Some(error) = device_failure(&worker_changes) {
                Err(error.to_string())
            } else {
                native.lease.phase(MicrophoneState::Starting);
                native
                    .tap
                    .as_mut()
                    .expect("owned tap")
                    .start(sink)
                    .map_err(|_| {
                        "Cannot start microphone; check permission and input device".to_string()
                    })
            };
            match start {
                Ok(format) => {
                    native.lease.phase(MicrophoneState::Capturing);
                    if tx.send(Ok(format)).is_ok() {
                        // Disconnecting the async handle requests Stop, without
                        // running a native destructor on its runtime thread.
                        let _ = stop_rx.recv();
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                }
            }
            let result = native.close();
            drop(native); // drop the lease before acknowledging complete cleanup
            let _ = closed_tx.send(result);
        })
        .map_err(|_| "Cannot start microphone worker".to_string())?;
    let deadline = tokio::time::sleep(MIC_START_DEADLINE);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        match control.load(Ordering::Acquire) {
            2 => return Err("Cancelled".into()),
            1 if !receiving.load(Ordering::Acquire) => {
                return Err("Dictation ended before microphone was ready; try again".into());
            }
            _ => {}
        }
        if let Some(error) = device_failure(changes) {
            return Err(error.into());
        }
        tokio::select! {
            started = &mut rx => {
                let format = started.map_err(|_| "Microphone startup failed")??;
                if control.load(Ordering::Acquire) == 2 { return Err("Cancelled".into()); }
                return Ok((guard, format));
            }
            _ = &mut deadline => return Err("Microphone did not start; check the input device and try again".into()),
            _ = tick.tick() => {}
        }
    }
}
/// Runs one attempt on a private runtime and returns as soon as it resolves.
/// Offline, a timed-out connect leaves getaddrinfo blocking on a pool thread for
/// tens of seconds; dropping the runtime would wait for it and keep the
/// indicator loading, so shutdown abandons that thread instead.
pub fn block_on_detached<F: std::future::Future>(attempt: F) -> Result<F::Output, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start audio runtime".to_string())?;
    let output = runtime.block_on(attempt);
    runtime.shutdown_background();
    Ok(output)
}
/// Control: 0 recording, 1 finish, 2 cancel. No audio or transcript touches disk.
pub async fn run(
    tap: Box<dyn AudioTap>,
    key: String,
    control: Arc<AtomicU8>,
) -> Result<String, String> {
    run_with_endpoint(tap, key, control, fotw_stt::DeepgramEndpoint::production()).await
}
/// Injectable endpoint for fixture tests; production always uses Deepgram TLS.
pub async fn run_with_endpoint(
    tap: Box<dyn AudioTap>,
    key: String,
    control: Arc<AtomicU8>,
    endpoint: fotw_stt::DeepgramEndpoint,
) -> Result<String, String> {
    run_observed(
        tap,
        key,
        control,
        endpoint,
        Arc::new(AtomicBool::new(false)),
    )
    .await
}
pub async fn run_observed(
    tap: Box<dyn AudioTap>,
    key: String,
    control: Arc<AtomicU8>,
    endpoint: fotw_stt::DeepgramEndpoint,
    receiving: Arc<AtomicBool>,
) -> Result<String, String> {
    run_provider_observed(
        tap,
        CaptureProvider::Deepgram { key, endpoint },
        control,
        receiving,
    )
    .await
}
#[derive(Clone)]
pub enum CaptureProvider {
    Deepgram {
        key: String,
        endpoint: fotw_stt::DeepgramEndpoint,
    },
    Local(crate::local_stt::LocalEndpoint),
    DeepgramThenLocal {
        key: String,
        endpoint: fotw_stt::DeepgramEndpoint,
        local: crate::local_stt::LocalEndpoint,
    },
}
pub async fn run_provider_observed(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
) -> Result<String, String> {
    run_provider_with_progress(
        tap,
        provider,
        control,
        receiving,
        crate::fallback::Progress::default(),
    )
    .await
}

pub async fn run_provider_with_progress(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    progress: crate::fallback::Progress,
) -> Result<String, String> {
    run_provider_with_startup_and_progress(
        tap,
        provider,
        control,
        receiving,
        progress,
        MicrophoneStartup::default(),
    )
    .await
}

/// Capture with one startup coordinator retained across dictation attempts.
pub async fn run_provider_with_startup(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    startup: MicrophoneStartup,
) -> Result<String, String> {
    run_provider_with_startup_and_progress(
        tap,
        provider,
        control,
        receiving,
        crate::fallback::Progress::default(),
        startup,
    )
    .await
}

/// Capture with independent provider progress and bounded microphone startup.
pub async fn run_provider_with_startup_and_progress(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    progress: crate::fallback::Progress,
    startup: MicrophoneStartup,
) -> Result<String, String> {
    run_provider_with_events(
        tap,
        provider,
        control,
        receiving,
        progress,
        startup,
        fotw_audio::device_change::DeviceChangeSignal::new(),
    )
    .await
}

/// Capture with notifications owned by this attempt; do not share the signal
/// with another attempt or a consumer that clears it.
pub async fn run_provider_with_events(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    progress: crate::fallback::Progress,
    startup: MicrophoneStartup,
    changes: Arc<fotw_audio::device_change::DeviceChangeSignal>,
) -> Result<String, String> {
    run_provider_with_vocabulary(
        tap,
        provider,
        control,
        receiving,
        progress,
        startup,
        changes,
        crate::vocabulary::Profile::default(),
    )
    .await
}

/// Existing callers can supply an already-created tap; the native shell uses
/// deferred preparation so watching/opening also run inside the startup lease.
#[allow(clippy::too_many_arguments)]
pub async fn run_provider_with_vocabulary(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    progress: crate::fallback::Progress,
    startup: MicrophoneStartup,
    changes: Arc<fotw_audio::device_change::DeviceChangeSignal>,
    profile: crate::vocabulary::Profile,
) -> Result<String, String> {
    run_provider_with_preparation(
        InputPreparation::new(move || Ok(PreparedInput::new(tap))),
        provider,
        control,
        receiving,
        progress,
        startup,
        changes,
        profile,
    )
    .await
}

/// One immutable profile for preflight, cloud retries, local fallback and final expansion.
#[allow(
    clippy::too_many_arguments,
    reason = "attempt-owned audio, cancellation and notification seams remain explicit"
)]
pub async fn run_provider_with_preparation(
    input: InputPreparation,
    mut provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    progress: crate::fallback::Progress,
    startup: MicrophoneStartup,
    changes: Arc<fotw_audio::device_change::DeviceChangeSignal>,
    profile: crate::vocabulary::Profile,
) -> Result<String, String> {
    profile.validate()?;
    if control.load(Ordering::Acquire) != 0 {
        return Err("Cancelled before microphone startup".into());
    }
    if let Some(error) = device_failure(&changes) {
        return Err(error.into());
    }
    if !matches!(&provider, CaptureProvider::Local(_)) {
        crate::language::validate_deepgram(&profile.language)?;
    }
    match &mut provider {
        CaptureProvider::Local(endpoint)
        | CaptureProvider::DeepgramThenLocal {
            local: endpoint, ..
        } => {
            let prepared = {
                let readiness =
                    crate::local_stt::prepare(endpoint, &profile.language, Duration::from_secs(2));
                tokio::pin!(readiness);
                let mut tick = tokio::time::interval(Duration::from_millis(5));
                loop {
                    if control.load(Ordering::Acquire) == 2 {
                        return Err("Cancelled".into());
                    }
                    if control.load(Ordering::Acquire) != 0 {
                        return Err(
                            "Dictation ended before local engine was ready; try again".into()
                        );
                    }
                    if let Some(error) = device_failure(&changes) {
                        return Err(error.into());
                    }
                    tokio::select! {
                        biased;
                        _ = tick.tick() => {}
                        result = &mut readiness => break result?,
                    }
                }
            };
            *endpoint = prepared;
        }
        CaptureProvider::Deepgram { .. } => {}
    }
    let failed = Arc::new(AtomicU8::new(0));
    let (producer, mut consumer) = RingBuffer::new(192_000);
    let sink = Box::new(Sink {
        producer,
        failed: failed.clone(),
        receiving: receiving.clone(),
    });
    let started_capture =
        start_within_deadline(input, sink, &control, &startup, &receiving, &changes).await;
    let (mut guard, format) = match started_capture {
        Ok(started) => started,
        Err(error) => {
            if control.load(Ordering::Acquire) == 2 {
                return Err("Cancelled".into());
            }
            if let Some(error) = device_failure(&changes) {
                return Err(error.into());
            }
            if let Some(error) = audio_failure(&failed) {
                return Err(error.into());
            }
            return Err(error);
        }
    };
    let result = async {
        if control.load(Ordering::Acquire) == 2 { return Err("Cancelled".into()); }
        if let Some(error) = device_failure(&changes) { return Err(error.into()); }
        if let Some(error) = audio_failure(&failed) { return Err(error.into()); }
    let mut converter = CaptureConverter::new(format)?;
    // Ten seconds of 10 ms chunks covers the bounded eight-second TLS handshake.
    // The previous 320 ms queue overflowed before a live connection could open.
    let (tx, rx) = tokio::sync::mpsc::channel(1024);
    let local = !matches!(&provider, CaptureProvider::Deepgram { .. });
    let network = async {
        match provider {
            CaptureProvider::Deepgram { key, endpoint } => {
                progress.set(crate::fallback::Stage::Cloud);
                crate::streaming::transcribe_with_profile(
                    key,
                    rx,
                    endpoint,
                    Duration::from_secs(8),
                    Duration::from_secs(8),
                    &profile,
                )
                .await
                .map_err(|e| e.message)
            }
            CaptureProvider::DeepgramThenLocal {
                key,
                endpoint,
                local,
            } => {
                crate::fallback::transcribe_with_profile(
                    key,
                    rx,
                    endpoint,
                    local,
                    control.clone(),
                    crate::fallback::Deadlines::default(),
                    progress.clone(),
                    &profile,
                )
                .await
            }
            CaptureProvider::Local(endpoint) => {
                progress.set(if profile.recognition_hints.is_empty() {
                    crate::fallback::Stage::Local
                } else {
                    crate::fallback::Stage::LocalWithoutHints
                });
                let mut rx = rx;
                let mut pcm = Vec::new();
                while let Some(chunk) = rx.recv().await {
                    append_local_pcm(&mut pcm, chunk);
                }
                crate::local_stt::transcribe(
                    &endpoint,
                    &pcm,
                    control.clone(),
                    Duration::from_secs(180),
                )
                .await
            }
        }
    };
    tokio::pin!(network);
    let started = std::time::Instant::now();
    let mut last_frame = started;
    let mut finishing = false;
    let mut finish_started = None;
    let mut sender = Some(tx);
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {
             result=&mut network=>{
                 if control.load(Ordering::Acquire)==2 {return Err("Cancelled".into());}
                 if !finishing && let Some(error)=device_failure(&changes) {return Err(error.into());}
                 if let Some(error)=audio_failure(&failed) {return Err(error.into());}
                 if result.is_ok() && !finishing {return Err("Transcription ended before microphone stopped; no text inserted".into());}
                 return result.map(|text| profile.expand(&text));
             },
             _=tick.tick()=>{
              if control.load(Ordering::Acquire)==2 {return Err("Cancelled".into());}
        if finish_started.is_some_and(|at:std::time::Instant|at.elapsed()>Duration::from_secs(if local { 185 } else { 10 })){return Err("Finalization timed out; no text inserted".into());}
              if !finishing && let Some(error)=device_failure(&changes) {return Err(error.into());}
                 if let Some(error)=audio_failure(&failed){return Err(error.into());}
              if !finishing {
               if control.load(Ordering::Acquire)==1 || started.elapsed()>=Duration::from_secs(120) {
                guard.close(&control, &changes).await?;
                // Successful stop closes the capture epoch. Later route notifications
                // cannot invalidate audio already collected, but a callback fault
                // reported during stop must still reject this utterance.
                finishing=true;finish_started=Some(std::time::Instant::now());
                 if let Some(error)=audio_failure(&failed){return Err(error.into());}
               }
               let mut captured=Vec::with_capacity(consumer.slots());
               while let Ok(sample)=consumer.pop(){captured.push(sample);}
               if !captured.is_empty(){last_frame=std::time::Instant::now();}
               if last_frame.elapsed()>Duration::from_secs(3)&&!finishing{return Err("Microphone stopped delivering audio; no text inserted".into());}
               let samples=converter.process(&captured,finishing)?;
               if !samples.is_empty() && let Some(sender)=&sender {
                sender.try_send(Downmixer::to_i16(&samples)).map_err(|_|"Transcription cannot keep up; no text inserted")?;
               }
               if finishing {sender.take();}
              }
             }
            }
    }
    }.await;
    let cleanup = guard.close(&control, &changes).await;
    if control.load(Ordering::Acquire) == 2 {
        return Err("Cancelled".into());
    }
    if let Some(error) = audio_failure(&failed) {
        return Err(error.into());
    }
    cleanup?;
    result
}

/// Retain at most 120 seconds of actual capture, including on a late stop tick.
/// Once full, discard callback overshoot and the resampler's artificial drain tail.
/// The transport separately rejects oversized caller-provided input.
pub(crate) fn append_local_pcm(pcm: &mut Vec<i16>, chunk: Vec<i16>) {
    let remaining = (16000_usize * 120).saturating_sub(pcm.len());
    pcm.extend(chunk.into_iter().take(remaining));
}

struct CaptureConverter {
    format: StreamFormat,
    resampler: Resampler16k,
}
impl CaptureConverter {
    fn new(format: StreamFormat) -> Result<Self, String> {
        Ok(Self {
            format,
            resampler: Resampler16k::new(format.sample_rate_hz, format.channels)
                .map_err(|_| "Unsupported microphone format")?,
        })
    }
    fn process(&mut self, pcm: &[f32], finishing: bool) -> Result<Vec<f32>, String> {
        let mut samples = self
            .resampler
            .process_all(pcm)
            .map_err(|_| "Audio conversion failed")?;
        if finishing {
            let padding = vec![
                0.0;
                self.format.sample_rate_hz as usize / 5
                    * usize::from(self.format.channels)
            ];
            samples.extend(
                self.resampler
                    .process_all(&padding)
                    .map_err(|_| "Audio conversion failed")?,
            );
        }
        Ok(Downmixer::to_mono(&samples, self.format.channels))
    }
}

fn device_failure(changes: &fotw_audio::device_change::DeviceChangeSignal) -> Option<&'static str> {
    use fotw_audio::device_change::DeviceChangeKind;
    let pending = changes.peek();
    if pending.contains(DeviceChangeKind::DefaultInput) {
        Some("Microphone route changed; no text inserted. Start a new dictation.")
    } else if pending.contains(DeviceChangeKind::StreamFormat) {
        Some("Audio format changed; no text inserted. Start a new dictation.")
    } else if pending.contains(DeviceChangeKind::WillSleep)
        || pending.contains(DeviceChangeKind::DidWake)
    {
        Some("Computer sleep interrupted dictation; no text inserted. Start a new dictation.")
    } else {
        None
    }
}

#[cfg(test)]
mod local_buffer_tests {
    #[test]
    fn stereo_conversion_preserves_frames_split_across_ring_drains() {
        use super::*;
        let format = StreamFormat::new(48000, 2, fotw_audio::SampleFormat::F32);
        let pcm: Vec<f32> = (0..12000)
            .flat_map(|i| {
                let x = (i % 23) as f32 / 23.0;
                [x, -x * 0.5]
            })
            .collect();
        let mut whole = CaptureConverter::new(format).unwrap();
        let mut expected = whole.process(&pcm, false).unwrap();
        expected.extend(whole.process(&[], true).unwrap());
        let mut split = CaptureConverter::new(format).unwrap();
        let mut actual = Vec::new();
        for chunk in pcm.chunks(319) {
            actual.extend(split.process(chunk, false).unwrap());
        }
        actual.extend(split.process(&[], true).unwrap());
        assert!(!expected.is_empty());
        assert_eq!(
            actual.len(),
            expected.len(),
            "ring boundaries must not drop sample frames"
        );
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 1e-6),
            "ring boundaries changed the stereo waveform"
        );
    }
    #[test]
    fn maximum_capture_clips_scheduler_overshoot_and_resampler_padding() {
        let mut pcm = vec![1; 16000 * 120 - 160];
        // Last 10 ms of speech, a late 10 ms callback, then the 200 ms drain.
        let mut final_chunk = vec![2; 320];
        final_chunk.extend(vec![0; 3200]);
        super::append_local_pcm(&mut pcm, final_chunk);
        assert_eq!(pcm.len(), 16000 * 120);
        assert_eq!(&pcm[pcm.len() - 160..], &[2; 160]);
        super::append_local_pcm(&mut pcm, vec![3; 160]);
        assert_eq!(pcm.len(), 16000 * 120);
        assert_eq!(pcm.last(), Some(&2));
    }
}
