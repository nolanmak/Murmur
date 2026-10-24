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
    failed: Arc<AtomicBool>,
    receiving: Arc<AtomicBool>,
}
impl FrameSink for Sink {
    fn on_frames(&mut self, pcm: &[f32], _: CaptureTimestamp, _: FrameFlags) {
        if !pcm.is_empty() {
            self.receiving.store(true, Ordering::Release);
        }
        for sample in pcm {
            if self.producer.push(*sample).is_err() {
                self.failed.store(true, Ordering::Release);
                break;
            }
        }
    }
    fn on_error(&mut self, _: TapError) {
        self.failed.store(true, Ordering::Release);
    }
}
struct Guard(Box<dyn AudioTap>);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}
/// Shared by the native shell across attempts. A blocked OS start retains its
/// lease until it returns and any abandoned microphone has been stopped.
#[derive(Clone, Default)]
pub struct MicrophoneStartup(Arc<AtomicBool>);
struct StartupLease(Arc<AtomicBool>);
impl Drop for StartupLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl MicrophoneStartup {
    fn acquire(&self) -> Result<StartupLease, String> {
        self.0.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Previous microphone startup is still pending; reconnect the input device or reopen Murmur".to_string())?;
        Ok(StartupLease(self.0.clone()))
    }
}

// Adapted from Murmur PR #53 (97fe25c). Carry the RAII Guard through the
// channel too: receiver cancellation after send must still close the tap.
async fn start_within_deadline(
    tap: Box<dyn AudioTap>,
    sink: Box<dyn FrameSink>,
    control: &AtomicU8,
    startup: &MicrophoneStartup,
    receiving: &AtomicBool,
) -> Result<(Guard, StreamFormat), String> {
    let lease = startup.acquire()?;
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("murmur-mic-start".into())
        .spawn(move || {
            let _lease = lease;
            let mut guard = Guard(tap);
            let started = guard.0.start(sink);
            // Failed delivery drops Guard on this worker, stopping a late mic.
            let _ = tx.send((guard, started));
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
        tokio::select! {
            started = &mut rx => {
                let (guard, format) = started.map_err(|_| "Microphone startup failed")?;
                if control.load(Ordering::Acquire) == 2 { return Err("Cancelled".into()); }
                let format = format.map_err(|_| "Cannot start microphone; check permission and input device")?;
                return Ok((guard, format));
            }
            _ = &mut deadline => return Err("Microphone did not start; check the input device and try again".into()),
            _ = tick.tick() => {}
        }
    }
}
/// Run one capture attempt without waiting for abandoned blocking DNS work.
/// Adapted from Murmur PR #52 (2e95a60). A timeout drops the lookup future,
/// but libc's resolver may keep running; normal runtime drop waits for it.
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
    run_provider_with_startup(
        tap,
        provider,
        control,
        receiving,
        progress,
        MicrophoneStartup::default(),
    )
    .await
}

/// Capture with a startup coordinator retained by the caller across attempts.
pub async fn run_provider_with_startup(
    tap: Box<dyn AudioTap>,
    provider: CaptureProvider,
    control: Arc<AtomicU8>,
    receiving: Arc<AtomicBool>,
    progress: crate::fallback::Progress,
    startup: MicrophoneStartup,
) -> Result<String, String> {
    if control.load(Ordering::Acquire) != 0 {
        return Err("Cancelled before microphone startup".into());
    }
    match &provider {
        CaptureProvider::Local(endpoint)
        | CaptureProvider::DeepgramThenLocal {
            local: endpoint, ..
        } => {
            crate::local_stt::check_ready(endpoint, Duration::from_secs(2)).await?;
            if control.load(Ordering::Acquire) != 0 {
                return Err("Dictation ended before local engine was ready; try again".into());
            }
        }
        CaptureProvider::Deepgram { .. } => {}
    }
    let failed = Arc::new(AtomicBool::new(false));
    let (producer, mut consumer) = RingBuffer::new(192_000);
    let sink = Box::new(Sink {
        producer,
        failed: failed.clone(),
        receiving: receiving.clone(),
    });
    let (mut guard, format) =
        start_within_deadline(tap, sink, &control, &startup, &receiving).await?;
    let mut resampler =
        Resampler16k::new(format.sample_rate_hz, 1).map_err(|_| "Unsupported microphone format")?;
    // Ten seconds of 10 ms chunks covers the bounded eight-second TLS handshake.
    // The previous 320 ms queue overflowed before a live connection could open.
    let (tx, rx) = tokio::sync::mpsc::channel(1024);
    let local = !matches!(&provider, CaptureProvider::Deepgram { .. });
    let network = async {
        match provider {
            CaptureProvider::Deepgram { key, endpoint } => {
                progress.set(crate::fallback::Stage::Cloud);
                crate::streaming::transcribe(key, rx, endpoint, Duration::from_secs(8)).await
            }
            CaptureProvider::DeepgramThenLocal {
                key,
                endpoint,
                local,
            } => {
                crate::fallback::transcribe_observed(
                    key,
                    rx,
                    endpoint,
                    local,
                    control.clone(),
                    crate::fallback::Deadlines::default(),
                    progress.clone(),
                )
                .await
            }
            CaptureProvider::Local(endpoint) => {
                progress.set(crate::fallback::Stage::Local);
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
             result=&mut network=>return result,
             _=tick.tick()=>{
              if control.load(Ordering::Acquire)==2 {return Err("Cancelled".into());}
        if finish_started.is_some_and(|at:std::time::Instant|at.elapsed()>Duration::from_secs(if local { 185 } else { 10 })){return Err("Finalization timed out; no text inserted".into());}
              if failed.load(Ordering::Acquire){return Err("Audio buffer overflow or microphone failure; no text inserted".into());}
              if !finishing {
               if control.load(Ordering::Acquire)==1 || started.elapsed()>=Duration::from_secs(120) {
                guard.0.stop().map_err(|_|"Microphone stop failed")?;finishing=true;finish_started=Some(std::time::Instant::now());
               }
               let mut captured=Vec::with_capacity(consumer.slots());
               while let Ok(sample)=consumer.pop(){captured.push(sample);}
               if !captured.is_empty(){last_frame=std::time::Instant::now();}
               if last_frame.elapsed()>Duration::from_secs(3)&&!finishing{return Err("Microphone stopped delivering audio; no text inserted".into());}
               let mut mono=Downmixer::to_mono(&captured,format.channels);
               if finishing {mono.extend(std::iter::repeat_n(0.0,format.sample_rate_hz as usize/5));}
               let samples=resampler.process_all(&mono).map_err(|_|"Audio conversion failed")?;
               if !samples.is_empty() && let Some(sender)=&sender {
                sender.try_send(Downmixer::to_i16(&samples)).map_err(|_|"Transcription cannot keep up; no text inserted")?;
               }
               if finishing {sender.take();}
              }
             }
            }
    }
}

/// Retain at most 120 seconds of actual capture, including on a late stop tick.
/// Once full, discard callback overshoot and the resampler's artificial drain tail.
/// The transport separately rejects oversized caller-provided input.
pub(crate) fn append_local_pcm(pcm: &mut Vec<i16>, chunk: Vec<i16>) {
    let remaining = (16000_usize * 120).saturating_sub(pcm.len());
    pcm.extend(chunk.into_iter().take(remaining));
}

#[cfg(test)]
mod local_buffer_tests {
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
