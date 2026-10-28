//! Murmur #100 review: with local streaming chosen, Local Parakeet is only a
//! fallback. When a fallback loads it, it is released again after a quiet
//! spell, so both 0.6B models (~1.85 GB together) don't stay resident.
use murmur::local_parakeet::{
    Backend, Binding, BoxFuture, Detected, InstallSources, LoadError, Progress, Recognized, Service,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Default)]
struct Counts {
    loads: AtomicUsize,
    unloads: AtomicUsize,
}

struct Fake(Arc<Counts>);
impl Backend for Fake {
    fn load(&self) -> BoxFuture<Result<Binding, LoadError>> {
        self.0.loads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(Binding {
                instance: 1,
                model: "parakeet-unified".into(),
                revision: "r".into(),
            })
        })
    }
    fn instance(&self) -> Option<u64> {
        Some(1)
    }
    fn speech(&self, _: Arc<[i16]>) -> BoxFuture<Result<Detected, String>> {
        Box::pin(async { Ok(Detected::default()) })
    }
    fn recognize(&self, _: Arc<[i16]>) -> BoxFuture<Result<Recognized, String>> {
        Box::pin(async {
            Ok(Recognized {
                text: String::new(),
                model: None,
            })
        })
    }
    fn install(&self, _: InstallSources, _: Progress) -> BoxFuture<Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn unload(&self) -> BoxFuture<Result<(), String>> {
        self.0.unloads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

fn service() -> (Arc<Service>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let service = Arc::new(Service::new(Arc::new(Fake(counts.clone()))).unwrap());
    (service, counts)
}

#[tokio::test]
async fn a_fallback_parakeet_is_released_after_a_quiet_spell() {
    let (service, counts) = service();
    service.ready(Duration::from_secs(2)).await.unwrap();
    service.release_after(Duration::from_millis(100));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(counts.unloads.load(Ordering::SeqCst), 1, "released");
    assert!(service.state().is_none(), "no load is kept after release");
    // The next fallback loads it again.
    service.ready(Duration::from_secs(2)).await.unwrap();
    assert_eq!(counts.loads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn using_parakeet_again_during_the_quiet_spell_keeps_it() {
    let (service, counts) = service();
    service.ready(Duration::from_secs(2)).await.unwrap();
    service.release_after(Duration::from_millis(300));
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Another fallback: the clock starts again from this use.
    service.ready(Duration::from_secs(2)).await.unwrap();
    service.release_after(Duration::from_millis(300));
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        counts.unloads.load(Ordering::SeqCst),
        0,
        "the first release was cancelled by the second use"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(counts.unloads.load(Ordering::SeqCst), 1);
}
