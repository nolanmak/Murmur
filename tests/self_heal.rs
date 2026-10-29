//! Murmur never leaves dictation locked behind microphone ownership it cannot
//! clear itself. Process exit is the one release Core Audio guarantees, so a
//! stuck lease relaunches the app — only while nothing is being dictated,
//! reviewed or installed, and at most twice in ten minutes.
use murmur::capture::MicrophoneState as Mic;
use murmur::core::Phase;
use murmur::self_heal::{Action, Policy, RESTARTING, RestartLog, SelfHeal};
use std::time::Duration;

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}
const WALL: u64 = 1_800_000_000;

fn log() -> (tempfile::TempDir, RestartLog) {
    let dir = tempfile::tempdir().unwrap();
    let log = RestartLog::load(dir.path().join("Murmur/microphone-restarts"));
    (dir, log)
}

/// Drives one stuck episode until it relaunches, giving up after `limit` ms.
fn run_until_action(
    heal: &mut SelfHeal,
    log: &mut RestartLog,
    state: Mic,
    busy: bool,
    from: u64,
    limit: u64,
) -> Vec<(u64, Action)> {
    let mut actions = Vec::new();
    for at in (from..=from + limit).step_by(20) {
        let action = heal.observe(ms(at), state, busy, log, WALL + at / 1000);
        if action != Action::Wait {
            actions.push((at, action));
        }
    }
    actions
}

#[test]
fn unconfirmed_or_failed_cleanup_announces_then_relaunches_when_idle() {
    for state in [Mic::Unconfirmed, Mic::CleanupFailed] {
        let (_dir, mut log) = log();
        let mut heal = SelfHeal::new(Policy::default());
        let actions = run_until_action(&mut heal, &mut log, state, false, 0, 3_000);
        assert_eq!(actions.first(), Some(&(0, Action::Announce)), "{state:?}");
        let relaunch = actions
            .iter()
            .find(|(_, action)| *action == Action::Relaunch)
            .expect("a stuck lease relaunches Murmur");
        assert!(
            relaunch.0 >= Policy::default().notice.as_millis() as u64,
            "the user sees the restart notice first"
        );
        assert_eq!(
            actions
                .iter()
                .filter(|(_, a)| *a == Action::Relaunch)
                .count(),
            1
        );
        assert!(heal.stuck());
        assert_eq!(heal.notice(state, Phase::Idle), Some(RESTARTING));
    }
}

#[test]
fn never_relaunches_during_dictation_review_or_install() {
    let (_dir, mut log) = log();
    let mut heal = SelfHeal::new(Policy::default());
    for state in [Mic::Unconfirmed, Mic::CleanupFailed, Mic::Stopping] {
        let actions = run_until_action(&mut heal, &mut log, state, true, 0, 120_000);
        assert!(actions.is_empty(), "{state:?} busy: {actions:?}");
        assert!(!heal.stuck(), "busy work keeps the pill click-through");
    }
    assert!(log.latest().is_none(), "nothing was recorded");
}

#[test]
fn busy_work_during_the_notice_cancels_that_relaunch() {
    let (_dir, mut log) = log();
    let mut heal = SelfHeal::new(Policy::default());
    assert_eq!(
        heal.observe(ms(0), Mic::Unconfirmed, false, &mut log, WALL),
        Action::Announce
    );
    // A review or install begins before the notice elapses.
    let busy = run_until_action(&mut heal, &mut log, Mic::Unconfirmed, true, 20, 5_000);
    assert!(busy.is_empty(), "{busy:?}");
    // Idle again: a fresh notice precedes any relaunch.
    let actions = run_until_action(&mut heal, &mut log, Mic::Unconfirmed, false, 6_000, 3_000);
    assert_eq!(actions.first(), Some(&(6_000, Action::Announce)));
    assert!(actions.iter().any(|(_, a)| *a == Action::Relaunch));
}

#[test]
fn pending_native_operations_relaunch_only_after_the_grace_period() {
    let grace = Policy::default().grace.as_millis() as u64;
    for state in [
        Mic::Preparing,
        Mic::Starting,
        Mic::Capturing,
        Mic::Stopping,
        Mic::CleaningUp,
        Mic::Inspecting,
    ] {
        let (_dir, mut log) = log();
        let mut heal = SelfHeal::new(Policy::default());
        let early = run_until_action(&mut heal, &mut log, state, false, 0, grace - 20);
        assert!(early.is_empty(), "{state:?} still within its deadline");
        assert!(!heal.stuck());
        assert_eq!(
            heal.notice(state, Phase::Idle),
            murmur::indicator::microphone_notice(state, Phase::Idle)
        );
        let late = run_until_action(&mut heal, &mut log, state, false, grace, 3_000);
        assert_eq!(late.first(), Some(&(grace, Action::Announce)), "{state:?}");
        assert!(late.iter().any(|(_, a)| *a == Action::Relaunch));
    }
}

#[test]
fn released_ownership_resets_the_episode() {
    let (_dir, mut log) = log();
    let mut heal = SelfHeal::new(Policy::default());
    run_until_action(&mut heal, &mut log, Mic::Stopping, false, 0, 20_000);
    // Cleanup finished on its own: no relaunch, and the grace starts over.
    assert_eq!(
        heal.observe(ms(20_020), Mic::Idle, false, &mut log, WALL),
        Action::Wait
    );
    assert!(!heal.stuck());
    let actions = run_until_action(&mut heal, &mut log, Mic::Stopping, false, 20_040, 20_000);
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn at_most_two_automatic_relaunches_in_ten_minutes_then_manual() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("microphone-restarts");
    let policy = Policy::default();
    let relaunch_once = |wall: u64| {
        // Each launch is a new process reading the persisted log.
        let mut log = RestartLog::load(path.clone());
        let mut heal = SelfHeal::new(policy);
        let mut seen = Vec::new();
        for at in (0..=3_000).step_by(20) {
            let action = heal.observe(ms(at), Mic::Unconfirmed, false, &mut log, wall);
            if action != Action::Wait {
                seen.push(action);
            }
        }
        (seen, heal)
    };
    assert_eq!(relaunch_once(WALL).0, [Action::Announce, Action::Relaunch]);
    assert_eq!(
        relaunch_once(WALL + 60).0,
        [Action::Announce, Action::Relaunch]
    );
    let (third, heal) = relaunch_once(WALL + 120);
    assert_eq!(third, [Action::Manual], "the loop guard stops relaunching");
    assert!(
        heal.stuck(),
        "the manual click-to-restart path stays offered"
    );
    assert!(
        heal.notice(Mic::Unconfirmed, Phase::Idle)
            .unwrap()
            .contains("click to restart Murmur")
    );
    // Outside the ten-minute window the budget is available again.
    assert_eq!(
        relaunch_once(WALL + 60 + 601).0,
        [Action::Announce, Action::Relaunch]
    );
}

#[test]
fn an_unrecordable_restart_falls_back_to_manual_instead_of_risking_a_loop() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the parent directory must be: the record cannot persist.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"").unwrap();
    let mut log = RestartLog::load(blocker.join("microphone-restarts"));
    let mut heal = SelfHeal::new(Policy::default());
    let actions = run_until_action(&mut heal, &mut log, Mic::CleanupFailed, false, 0, 3_000);
    assert!(
        !actions.iter().any(|(_, a)| *a == Action::Relaunch),
        "{actions:?}"
    );
    assert_eq!(actions.last().map(|(_, a)| *a), Some(Action::Manual));
}

#[test]
fn corrupt_or_future_history_cannot_disable_or_bypass_the_guard() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("microphone-restarts");
    // Garbage lines are ignored; future timestamps (clock moved back) still count.
    std::fs::write(&path, format!("junk\n{}\n{}\n", WALL + 5, WALL + 9)).unwrap();
    let mut log = RestartLog::load(path);
    let mut heal = SelfHeal::new(Policy::default());
    let actions = run_until_action(&mut heal, &mut log, Mic::Unconfirmed, false, 0, 3_000);
    assert_eq!(actions, [(0, Action::Manual)]);
}

#[test]
fn a_recent_automatic_restart_is_reported_after_relaunch() {
    let (_dir, mut log) = log();
    let mut heal = SelfHeal::new(Policy::default());
    run_until_action(&mut heal, &mut log, Mic::Unconfirmed, false, 0, 3_000);
    let recorded = log.latest().expect("relaunch was recorded before exit");
    let reopened = RestartLog::load(log.path().to_path_buf());
    assert!(reopened.restarted_recently(recorded + 5));
    assert!(!reopened.restarted_recently(recorded + 120));
}

#[test]
fn stuck_notices_offer_a_click_and_stay_honest() {
    let heal = SelfHeal::new(Policy::default());
    let unconfirmed = heal.notice(Mic::Unconfirmed, Phase::Idle).unwrap();
    assert!(unconfirmed.contains("may be recording"), "{unconfirmed}");
    assert!(unconfirmed.contains("click to restart Murmur"));
    let failed = heal.notice(Mic::CleanupFailed, Phase::Idle).unwrap();
    assert!(failed.contains("stopped") && !failed.contains("may be recording"));
    assert!(failed.contains("click to restart Murmur"));
    for notice in [unconfirmed, failed, RESTARTING] {
        assert!(
            notice.chars().count() <= murmur::indicator::NOTICE_CHARS,
            "the whole pill text fits: {notice}"
        );
        assert_eq!(murmur::indicator::shorten(notice), notice);
    }
}
