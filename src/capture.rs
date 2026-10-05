//! Bounded, allocation-free handoff from FlyOnTheWall's microphone callback.
use crate::resample::{Downmixer, Resampler16k};
use fotw_audio::{AudioTap, CaptureTimestamp, FrameFlags, FrameSink, StreamFormat, TapError};
use rtrb::{Producer, RingBuffer};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use std::time::Duration;
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
/// Longest wait for the microphone to start. CoreAudio's `AudioDeviceStart` can
/// block forever when its IO thread never reaches the running state; a healthy
/// built-in or Bluetooth input starts well inside this.
pub const MIC_START_DEADLINE: Duration = Duration::from_secs(4);
/// Starts the tap off the async thread so a wedged CoreAudio start cannot pin
/// the attempt in its loading state, and Esc still cancels while it waits.
async fn start_within_deadline(
    mut tap: Box<dyn AudioTap>,
    sink: Box<dyn FrameSink>,
    control: &AtomicU8,
) -> Result<(Guard, Result<StreamFormat, TapError>), String> {
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let started = tap.start(sink);
        // The attempt already gave up: close a microphone that opened late.
        if let Err((mut tap, _)) = tx.send((tap, started)) {
            let _ = tap.stop();
        }
    });
    let deadline = tokio::time::sleep(MIC_START_DEADLINE);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tokio::select! {
            started = &mut rx => {
                let (tap, started) = started.map_err(|_| "Microphone startup failed")?;
                return Ok((Guard(tap), started));
            }
            _ = &mut deadline => {
                return Err("Microphone did not start; check the input device and try again".into());
            }
            _ = tick.tick() => {
                if control.load(Ordering::Acquire) == 2 {
                    return Err("Cancelled".into());
                }
            }
        }
    }
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
    if control.load(Ordering::Acquire) != 0 {
        return Err("Cancelled before microphone startup".into());
    }
    let failed = Arc::new(AtomicBool::new(false));
    let (producer, mut consumer) = RingBuffer::new(192_000);
    let sink = Box::new(Sink {
        producer,
        failed: failed.clone(),
        receiving,
    });
    let (mut guard, started) = start_within_deadline(tap, sink, &control).await?;
    let format =
        started.map_err(|_| "Cannot start microphone; check permission and input device")?;
    let mut resampler =
        Resampler16k::new(format.sample_rate_hz, 1).map_err(|_| "Unsupported microphone format")?;
    // Ten seconds of 10 ms chunks covers the bounded eight-second TLS handshake.
    // The previous 320 ms queue overflowed before a live connection could open.
    let (tx, rx) = tokio::sync::mpsc::channel(1024);
    let network = crate::streaming::transcribe(key, rx, endpoint, Duration::from_secs(8));
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
        if finish_started.is_some_and(|at:std::time::Instant|at.elapsed()>Duration::from_secs(10)){return Err("Finalization timed out; no text inserted".into());}
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
