//! Workspace sleep notifications, owned and removed by the native shell.
use block2::RcBlock;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use std::{
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

pub(super) struct SleepEvents {
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    pending: Arc<AtomicBool>,
    active: Arc<Mutex<Option<Arc<AtomicU8>>>>,
}
impl SleepEvents {
    pub(super) fn new() -> Self {
        Self::with_center(NSWorkspace::sharedWorkspace().notificationCenter())
    }
    fn with_center(center: Retained<NSNotificationCenter>) -> Self {
        let pending = Arc::new(AtomicBool::new(false));
        let active: Arc<Mutex<Option<Arc<AtomicU8>>>> = Arc::new(Mutex::new(None));
        let mut observers = Vec::new();
        // These names are immutable framework constants. The callbacks own
        // only shared Rust state, with no UI pointers or real-time work.
        for name in unsafe {
            [
                NSWorkspaceWillSleepNotification,
                NSWorkspaceDidWakeNotification,
            ]
        } {
            let signal = pending.clone();
            let current = active.clone();
            let block = RcBlock::new(move |_: NonNull<NSNotification>| {
                signal.store(true, Ordering::Release);
                if let Some(control) = current.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    control.store(2, Ordering::Release);
                }
            });
            let observer = unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
            };
            observers.push(observer);
        }
        Self {
            center,
            observers,
            pending,
            active,
        }
    }
    pub(super) fn track(&self, control: Arc<AtomicU8>) {
        let mut current = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if self.pending.load(Ordering::Acquire) {
            control.store(2, Ordering::Release);
        }
        *current = Some(control);
    }
    pub(super) fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}
impl Drop for SleepEvents {
    fn drop(&mut self) {
        for observer in &self.observers {
            // Tokens came from this center and are retained through removal.
            unsafe {
                self.center.removeObserver(observer.as_ref());
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sleep_events_fire_on_private_center_and_are_removed_on_drop() {
        let center = NSNotificationCenter::new();
        let events = SleepEvents::with_center(center.clone());
        assert!(!events.take());
        for name in unsafe {
            [
                NSWorkspaceWillSleepNotification,
                NSWorkspaceDidWakeNotification,
            ]
        } {
            let control = Arc::new(AtomicU8::new(0));
            events.track(control.clone());
            unsafe {
                center.postNotificationName_object(name, None);
            }
            assert_eq!(control.load(Ordering::Acquire), 2);
            let raced = Arc::new(AtomicU8::new(0));
            events.track(raced.clone());
            assert_eq!(raced.load(Ordering::Acquire), 2);
            assert!(events.take());
            assert!(!events.take());
        }
        let pending = events.pending.clone();
        drop(events);
        unsafe {
            center.postNotificationName_object(NSWorkspaceDidWakeNotification, None);
        }
        assert!(!pending.load(Ordering::Acquire));
    }
}
