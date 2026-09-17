//! [`TapSession`] — a tap plus the format history around it.
//!
//! CAP-07's acceptance criterion is that a sample-rate change between two
//! starts is absorbed *by the live session* rather than forcing the caller to
//! tear everything down. A session therefore owns the "re-read the format on
//! every rebuild, never assume 48 kHz" rule so no backend has to remember it.

use crate::error::TapError;
use crate::format::StreamFormat;
use crate::frames::FrameSink;
use crate::tap::AudioTap;

/// A tap and the formats it has reported across restarts.
#[derive(Debug)]
pub struct TapSession<T: AudioTap> {
    tap: T,
    observed: Vec<StreamFormat>,
    restarts: usize,
    running: bool,
}

impl<T: AudioTap> TapSession<T> {
    /// Wrap `tap`. Nothing is started yet.
    pub fn new(tap: T) -> Self {
        Self {
            tap,
            observed: Vec::new(),
            restarts: 0,
            running: false,
        }
    }

    /// Start the tap and record the format it reports.
    pub fn start(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        let format = self.tap.start(sink)?;
        self.observed.push(format);
        self.running = true;
        Ok(format)
    }

    /// Stop and start again, recording the new format.
    ///
    /// This is the device-change path. The caller keeps the same session
    /// object — and therefore the same downstream ring, WAL and STT stream —
    /// across a rebuild, so a mid-meeting AirPods switch costs a gap marker
    /// rather than a lost meeting.
    pub fn restart(&mut self, sink: Box<dyn FrameSink>) -> Result<StreamFormat, TapError> {
        if self.running {
            self.tap.stop()?;
            self.running = false;
        }
        self.restarts += 1;
        self.start(sink)
    }

    /// Stop the tap. Idempotent.
    pub fn stop(&mut self) -> Result<(), TapError> {
        if self.running {
            self.tap.stop()?;
            self.running = false;
        }
        Ok(())
    }

    /// The most recent authoritative format, if the tap has ever started.
    #[must_use]
    pub fn format(&self) -> Option<StreamFormat> {
        self.observed.last().copied()
    }

    /// Every format this session has been handed, oldest first.
    #[must_use]
    pub fn observed_formats(&self) -> Vec<StreamFormat> {
        self.observed.clone()
    }

    /// How many times the format actually *changed* between consecutive
    /// starts. Restarting at the same rate is not a change.
    #[must_use]
    pub fn format_changes(&self) -> usize {
        self.observed.windows(2).filter(|w| w[0] != w[1]).count()
    }

    /// How many times the tap has been restarted.
    #[must_use]
    pub const fn restarts(&self) -> usize {
        self.restarts
    }

    /// Whether the tap is currently running.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// The underlying tap.
    pub const fn tap(&self) -> &T {
        &self.tap
    }

    /// The underlying tap, mutably — used by tests to drive a fake.
    pub const fn tap_mut(&mut self) -> &mut T {
        &mut self.tap
    }
}
