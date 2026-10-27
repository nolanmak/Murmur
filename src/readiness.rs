//! Metadata-only recovery episodes. No capture, provider or insertion capability.
use crate::capture::MicrophoneStartup;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

/// Background probes are limited to three starts, at least fifteen seconds apart.
pub const INTERVAL: Duration = Duration::from_secs(15);
pub const DEADLINE: Duration = Duration::from_secs(4);
pub const MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Inactive,
    Waiting,
    Checking,
    Ready,
    Exhausted,
}

struct Worker<T> {
    ticket: Ticket,
    result: Receiver<Result<Option<T>, ()>>,
}

/// Owns the result channel until the native metadata worker has really returned.
/// A timeout or cancellation revokes its ticket, but cannot spawn a replacement.
pub struct Monitor<T> {
    episode: Episode<T>,
    worker: Option<Worker<T>>,
}
impl<T: Eq + Send + 'static> Monitor<T> {
    pub fn new(health: Duration) -> Self {
        Self {
            episode: Episode::new(health),
            worker: None,
        }
    }
    pub fn status(&self) -> Status {
        self.episode.status()
    }
    pub fn attempts(&self) -> u8 {
        self.episode.attempts()
    }
    pub fn fault(&mut self, now: Duration) {
        self.episode.fault(now);
    }
    pub fn cancel(&mut self) {
        self.episode.cancel();
    }
    pub fn pending(&self) -> bool {
        self.worker.is_some()
    }
    /// Status never claims live audio or resumes a previous dictation.
    pub fn message(&self) -> Option<&'static str> {
        match self.status() {
            Status::Inactive => None,
            Status::Waiting => {
                Some("Microphone readiness recovering — no recording; start dictation to retry")
            }
            Status::Checking => Some("Checking microphone availability — no recording"),
            Status::Ready => Some("Microphone available — start a new dictation"),
            Status::Exhausted => {
                Some("Microphone readiness checks exhausted — start dictation to retry")
            }
        }
    }

    /// Tick only on the controller thread, after input/cancellation processing.
    /// `allowed` excludes capture, permission/modal work and sleep transitions.
    pub fn tick(
        &mut self,
        now: Duration,
        allowed: bool,
        startup: &MicrophoneStartup,
        inspect: impl FnOnce() -> Option<T> + Send + 'static,
    ) {
        if !allowed {
            self.cancel();
        }
        self.episode.expire(now);
        if let Some(worker) = &self.worker {
            let result = match worker.result.try_recv() {
                Ok(result) => Some(result.ok().flatten()),
                Err(TryRecvError::Disconnected) => Some(None),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                self.episode.observe(worker.ticket, now, result);
                self.worker = None;
            }
        }
        if !allowed || self.worker.is_some() {
            return;
        }
        if let Some(ticket) = self.episode.next(now) {
            match startup.inspect(inspect) {
                Ok(result) => self.worker = Some(Worker { ticket, result }),
                Err(_) => self.episode.observe(ticket, now, None),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    epoch: u64,
    attempt: u8,
}

/// Clock-injected policy; an observation alone cannot start or resume dictation.
pub struct Episode<T> {
    health: Duration,
    epoch: u64,
    attempts: u8,
    status: Status,
    next: Duration,
    pending: Option<(Ticket, Duration)>,
    healthy: Option<(T, Duration, Duration)>,
}
impl<T: Eq> Episode<T> {
    /// Require a positive sustained-health window, configurable by the caller.
    pub fn new(health: Duration) -> Self {
        assert!(!health.is_zero());
        Self {
            health,
            epoch: 0,
            attempts: 0,
            status: Status::Inactive,
            next: Duration::ZERO,
            pending: None,
            healthy: None,
        }
    }
    pub fn status(&self) -> Status {
        self.status
    }
    pub fn attempts(&self) -> u8 {
        self.attempts
    }
    pub fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending = None;
        self.healthy = None;
        self.status = Status::Inactive;
    }
    /// Repeated faults belong to the same budget until deliberate cancellation.
    pub fn fault(&mut self, now: Duration) {
        if !matches!(self.status, Status::Inactive | Status::Ready) {
            self.healthy = None;
            return;
        }
        self.cancel();
        self.attempts = 0;
        self.next = now;
        self.status = Status::Waiting;
    }
    pub fn next(&mut self, now: Duration) -> Option<Ticket> {
        if self.pending.is_some() || self.status != Status::Waiting || now < self.next {
            return None;
        }
        self.attempts += 1;
        let ticket = Ticket {
            epoch: self.epoch,
            attempt: self.attempts,
        };
        self.next = now.saturating_add(INTERVAL);
        self.pending = Some((ticket, now.saturating_add(DEADLINE)));
        self.status = Status::Checking;
        Some(ticket)
    }
    pub fn observe(&mut self, ticket: Ticket, now: Duration, result: Option<T>) {
        let Some((current, deadline)) = self.pending else {
            return;
        };
        if current != ticket {
            return;
        }
        self.pending = None;
        // Space completed observations too: a slow first probe must not spend
        // the three-attempt budget before the sustained-health window elapses.
        // This only lengthens backoff; start spacing and health stay unchanged.
        self.next = self.next.max(now.saturating_add(INTERVAL));
        let result = result.filter(|_| now < deadline);
        self.healthy = match (result, self.healthy.take()) {
            (Some(value), Some((previous, since, last)))
                if value == previous && now.saturating_sub(last) <= INTERVAL + DEADLINE =>
            {
                Some((value, since, now))
            }
            (Some(value), _) => Some((value, now, now)),
            (None, _) => None,
        };
        self.status = if self
            .healthy
            .as_ref()
            .is_some_and(|(_, since, _)| now.saturating_sub(*since) >= self.health)
        {
            Status::Ready
        } else if self.attempts >= MAX_ATTEMPTS {
            Status::Exhausted
        } else {
            Status::Waiting
        };
    }
    pub fn expire(&mut self, now: Duration) {
        if let Some((ticket, deadline)) = self.pending
            && now >= deadline
        {
            self.observe(ticket, now, None);
        }
    }
}
