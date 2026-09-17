//! Platform change notifications — seam rule 5.
//!
//! `events()` exists from day one deliberately. Without it the macOS code
//! assumes a static device and breaks the first time someone connects AirPods
//! mid-meeting, and Windows cannot express `AUDCLNT_E_DEVICE_INVALIDATED` at
//! all. See docs/REQUIREMENTS.md 6.5.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::ids::TapId;

/// Something changed underneath a tap.
///
/// The session recorder subscribes and transparently rebuilds the affected
/// tap, logging a gap rather than ending the session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlatformEvent {
    /// The system default output device changed.
    DefaultOutputChanged,
    /// A specific tap's underlying device went away.
    DeviceInvalidated(TapId),
    /// A tap's stream format changed. Treat as a full rebuild trigger, not a
    /// converter reconfiguration: a Bluetooth headset engaging HFP changes the
    /// rate on both legs at once.
    FormatChanged(TapId),
    /// The set of processes producing audio changed.
    AppListChanged,
}

/// Fan-out for [`PlatformEvent`], one `Sender` per live subscriber.
///
/// Subscribers that hang up are pruned on the next emit, so a dropped
/// receiver can never wedge the publisher — which matters because the
/// publisher is often a Core Audio property listener that must not block.
#[derive(Debug, Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<Sender<PlatformEvent>>>,
}

impl EventBus {
    /// An empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscriber.
    pub fn subscribe(&self) -> Receiver<PlatformEvent> {
        let (tx, rx) = channel();
        // A poisoned lock here would mean a panic inside a listener; keep
        // delivering rather than propagating it into an audio callback.
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.push(tx);
        rx
    }

    /// Broadcast to every live subscriber, dropping the dead ones.
    pub fn emit(&self, event: PlatformEvent) {
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.retain(|tx| tx.send(event.clone()).is_ok());
    }

    /// Number of live subscribers as of the last emit.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_subscriber_does_not_wedge_the_publisher() {
        let bus = EventBus::new();
        let a = bus.subscribe();
        let b = bus.subscribe();
        drop(b);

        bus.emit(PlatformEvent::AppListChanged);
        assert_eq!(a.recv().unwrap(), PlatformEvent::AppListChanged);
        assert_eq!(bus.subscriber_count(), 1, "the dead subscriber was pruned");
    }
}
