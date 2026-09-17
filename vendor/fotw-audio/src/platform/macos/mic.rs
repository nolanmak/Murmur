//! The microphone leg.
//!
//! Deliberately a **separate device and a separate IOProc** from the system
//! tap, never a single aggregate containing both. Merging them is the most
//! commonly reported failure in this area: the two devices have independent
//! clocks, and a stale or mis-clocked aggregate silently produces drift or
//! stops delivering. They also change independently — the user can switch
//! output to AirPods without touching the input device, and a fused aggregate
//! has to be torn down for either event.
//!
//! Keeping them apart is also what makes "me vs them" attribution free: two
//! physically separate streams need no diarization to tell the local speaker
//! from everyone else (seam rule 2). The two are reconciled after the fact
//! through `host_ns`, which both legs stamp from the same monotonic clock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use cidre::{
    arc, blocks, cat,
    core_audio::{
        self as ca, Device, DeviceIoBlock, DeviceIoProcId, System, hardware::StartedDevice,
    },
    dispatch,
};

use crate::clock;
use crate::error::TapError;
use crate::format::{SampleFormat, StreamFormat};
use crate::frames::{CaptureTimestamp, FrameFlags, FrameSink};
use crate::ids::TapId;

struct IoState {
    sink: Mutex<Option<Box<dyn FrameSink>>>,
    device_frames: AtomicU64,
}

/// A microphone capture stream.
pub struct MicTap {
    id: TapId,
    hint: StreamFormat,
    running: Option<Running>,
    state: Arc<IoState>,
}

struct Running {
    format: StreamFormat,
    // Field order is teardown order, as in the system tap.
    _started: StartedDevice<Device>,
    _block: arc::R<DeviceIoBlock>,
    _queue: arc::R<dispatch::Queue>,
    _proc_id: DeviceIoProcId,
}

unsafe impl Send for Running {}

impl std::fmt::Debug for MicTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicTap")
            .field("id", &self.id)
            .field("running", &self.running.is_some())
            .finish()
    }
}

impl MicTap {
    /// Prepare a tap on the default input device.
    pub fn default_input() -> Result<Self, TapError> {
        let device = System::default_input_device()
            .map_err(|e| TapError::platform(format!("no default input device: {e:?}")))?;
        let uid = device
            .uid()
            .map(|u| u.to_string())
            .unwrap_or_else(|_| "default".to_string());
        Ok(Self {
            id: TapId::mic(uid),
            hint: StreamFormat::new(48_000, 1, SampleFormat::F32),
            running: None,
            state: Arc::new(IoState {
                sink: Mutex::new(None),
                device_frames: AtomicU64::new(0),
            }),
        })
    }

    fn build(&mut self, sink: Box<dyn FrameSink>) -> Result<Running, TapError> {
        let device = System::default_input_device()
            .map_err(|e| TapError::platform(format!("no default input device: {e:?}")))?;

        // Read the format every time. A Bluetooth headset engaging HFP drops
        // the input to 16 or 24 kHz mid-session, and a converter configured
        // once at session start would then be silently wrong (CAP-07).
        let probed = device
            .nominal_sample_rate()
            .ok()
            .map(|rate| StreamFormat::new(rate as u32, 1, SampleFormat::F32));
        let format = probed
            .filter(StreamFormat::is_plausible)
            .unwrap_or(self.hint);

        self.state.device_frames.store(0, Ordering::Release);
        {
            let mut guard = self
                .state
                .sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *guard = Some(sink);
        }

        let queue = dispatch::Queue::serial_with_ar_pool();
        let state = Arc::clone(&self.state);
        let channels = u64::from(format.channels.max(1));

        let mut block = blocks::EscBlock::new5(
            move |_now: &cat::AudioTimeStamp,
                  input: &cat::AudioBufList<1>,
                  _input_time: &cat::AudioTimeStamp,
                  _output: &mut cat::AudioBufList<1>,
                  _output_time: &cat::AudioTimeStamp| {
                on_io(&state, input, channels);
            },
        );

        let proc_id = device
            .create_io_proc_id_with_block(Some(&queue), &mut block)
            .map_err(|e| {
                TapError::platform(format!(
                    "mic AudioDeviceCreateIOProcIDWithBlock failed: {e:?}"
                ))
            })?;

        let started = ca::device_start(device, Some(proc_id))
            .map_err(|e| TapError::platform(format!("mic AudioDeviceStart failed: {e:?}")))?;

        Ok(Running {
            format,
            _started: started,
            _block: block,
            _queue: queue,
            _proc_id: proc_id,
        })
    }
}

fn on_io(state: &IoState, input: &cat::AudioBufList<1>, channels: u64) {
    let Ok(mut guard) = state.sink.try_lock() else {
        return;
    };
    let Some(sink) = guard.as_mut() else {
        return;
    };
    let buf = &input.buffers[0];
    if buf.data.is_null() || buf.data_bytes_size == 0 {
        return;
    }
    let samples = buf.data_bytes_size as usize / std::mem::size_of::<f32>();
    // SAFETY: Core Audio guarantees `data` is `data_bytes_size` bytes of
    // interleaved f32, valid for this callback only. We copy and do not retain.
    let pcm = unsafe { std::slice::from_raw_parts(buf.data as *const f32, samples) };

    let frames = samples as u64 / channels.max(1);
    let before = state.device_frames.fetch_add(frames, Ordering::AcqRel);

    let mut flags = FrameFlags::empty();
    flags.set(FrameFlags::SILENT, pcm.iter().all(|s| *s == 0.0));

    sink.on_frames(pcm, CaptureTimestamp::new(before, clock::host_ns()), flags);
}

impl crate::tap::AudioTap for MicTap {
    fn id(&self) -> &TapId {
        &self.id
    }

    fn format(&self) -> StreamFormat {
        self.running.as_ref().map_or(self.hint, |r| r.format)
    }

    fn format_is_authoritative(&self) -> bool {
        self.running.is_some()
    }

    fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        if self.running.is_some() {
            return Err(TapError::platform("mic tap is already started"));
        }
        let running = self.build(sink)?;
        let format = running.format;
        self.running = Some(running);
        Ok(format)
    }

    fn stop(&mut self) -> Result<(), TapError> {
        self.running = None;
        let mut guard = self
            .state
            .sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *guard = None;
        Ok(())
    }
}
