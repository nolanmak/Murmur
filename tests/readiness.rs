use murmur::readiness::{Episode, Status};
use std::time::Duration;
fn s(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

#[test]
fn one_success_cannot_heal_and_matching_metadata_must_remain_stable_for_health_window() {
    let mut recovery = Episode::new(s(30));
    recovery.fault(s(0));
    for at in [0, 15] {
        let ticket = recovery.next(s(at)).unwrap();
        recovery.observe(ticket, s(at), Some("route-a"));
        assert_eq!(
            recovery.status(),
            Status::Waiting,
            "one metadata success cannot heal an episode"
        );
        assert!(recovery.next(s(at + 14)).is_none());
    }
    let ticket = recovery.next(s(30)).unwrap();
    recovery.observe(ticket, s(30), Some("route-a"));
    assert_eq!(recovery.status(), Status::Ready);
    assert_eq!(recovery.attempts(), 3);
    assert!(recovery.next(s(1000)).is_none());
}

#[test]
fn repeated_faults_and_single_healthy_buffer_cannot_refill_retry_budget() {
    let mut recovery = Episode::new(s(30));
    recovery.fault(s(0));
    for at in [0, 15, 30] {
        let ticket = recovery.next(s(at)).unwrap();
        recovery.observe(ticket, s(at), None::<&str>);
        recovery.fault(s(at + 1));
        assert!(
            recovery.next(s(at + 1)).is_none(),
            "flapping cannot bypass backoff or exhaustion"
        );
    }
    assert_eq!(recovery.status(), Status::Exhausted);
    assert_eq!(recovery.attempts(), 3);
    assert!(recovery.next(s(10000)).is_none());
    recovery.cancel();
    recovery.fault(s(10001));
    assert!(
        recovery.next(s(10001)).is_some(),
        "explicit retry can begin a fresh episode"
    );
}

#[test]
fn changed_route_failure_and_unobserved_gap_do_not_count_as_sustained_health() {
    for observations in [
        [Some("a"), Some("b"), Some("b")],
        [Some("a"), None, Some("a")],
    ] {
        let mut recovery = Episode::new(s(30));
        recovery.fault(s(0));
        for (at, result) in [0, 15, 30].into_iter().zip(observations) {
            let ticket = recovery.next(s(at)).unwrap();
            recovery.observe(ticket, s(at), result);
        }
        assert_eq!(recovery.status(), Status::Exhausted);
    }
    let mut recovery = Episode::new(s(30));
    recovery.fault(s(0));
    for at in [0, 15, 300] {
        let ticket = recovery.next(s(at)).unwrap();
        recovery.observe(ticket, s(at), Some("a"));
    }
    assert_eq!(recovery.status(), Status::Exhausted);
}

#[test]
fn late_success_and_cancelled_generation_never_heal_or_affect_fresh_episode() {
    let mut recovery = Episode::new(s(15));
    recovery.fault(s(0));
    let late = recovery.next(s(0)).unwrap();
    recovery.observe(late, s(4), Some("a"));
    assert_eq!(recovery.status(), Status::Waiting);
    recovery.cancel();
    recovery.fault(s(15));
    let fresh = recovery.next(s(15)).unwrap();
    recovery.observe(late, s(15), Some("a"));
    assert_eq!(recovery.status(), Status::Checking);
    recovery.observe(fresh, s(15), Some("a"));
    let fresh = recovery.next(s(30)).unwrap();
    recovery.observe(fresh, s(30), Some("a"));
    assert_eq!(recovery.status(), Status::Ready);
}

#[test]
fn timed_out_cancelled_worker_cannot_multiply_or_heal_a_new_episode() {
    use murmur::{
        capture::{MicrophoneStartup, MicrophoneState},
        readiness::Monitor,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    let startup = MicrophoneStartup::default();
    let mut monitor = Monitor::new(s(15));
    let (release, gate) = mpsc::channel();
    let (entered, began) = mpsc::channel();
    monitor.fault(s(0));
    monitor.tick(s(0), true, &startup, move || {
        entered.send(()).unwrap();
        gate.recv().unwrap();
        Some("stale")
    });
    began.recv_timeout(s(2)).unwrap();
    monitor.tick(s(4), true, &startup, || {
        panic!("no replacement while OS read is blocked")
    });
    assert_eq!(monitor.status(), Status::Waiting);
    monitor.cancel();
    monitor.fault(s(15));
    let starts = Arc::new(AtomicUsize::new(0));
    for at in [15, 30, 100] {
        let starts = starts.clone();
        monitor.tick(s(at), true, &startup, move || {
            starts.fetch_add(1, Ordering::SeqCst);
            Some("fresh")
        });
    }
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(monitor.attempts(), 0);
    assert_eq!(startup.state(), MicrophoneState::Inspecting);
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + s(2);
    while startup.state() != MicrophoneState::Idle && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    // Cancel again before consuming the stale completion. No fresh probe may start.
    let deadline = std::time::Instant::now() + s(2);
    while monitor.pending() && std::time::Instant::now() < deadline {
        monitor.tick(s(101), false, &startup, || panic!("cancel wins"));
        std::thread::yield_now();
    }
    assert!(!monitor.pending());
    assert_eq!(monitor.status(), Status::Inactive);
    monitor.fault(s(102));
    monitor.tick(s(102), true, &startup, || Some("fresh"));
    let deadline = std::time::Instant::now() + s(2);
    while monitor.pending() && std::time::Instant::now() < deadline {
        monitor.tick(s(102), true, &startup, || panic!("not due"));
        std::thread::yield_now();
    }
    assert_eq!(monitor.status(), Status::Waiting);
    monitor.tick(s(117), true, &startup, || Some("fresh"));
    let deadline = std::time::Instant::now() + s(2);
    while monitor.pending() && std::time::Instant::now() < deadline {
        monitor.tick(s(117), true, &startup, || panic!("not due"));
        std::thread::yield_now();
    }
    assert_eq!(monitor.status(), Status::Ready);
    assert_eq!(startup.state(), MicrophoneState::Idle);
}
