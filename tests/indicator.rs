use murmur::core::Phase;
use murmur::indicator::*;
#[test]
fn shell_messages_map_to_success_cancel_or_notice() {
    for m in ["Text inserted", "Transcript copied"] {
        assert_eq!(classify(m), Kind::Success, "{m}");
    }
    for m in [
        "Cancelled",
        "Cancelled Control shortcut",
        "Remote copy cancelled",
    ] {
        assert_eq!(classify(m), Kind::Cancelled, "{m}");
    }
    for m in [
        "Cancelled before microphone startup",
        "No speech detected",
        "Transcript ready — choose Copy Last Transcript",
        "Insertion blocked: focus changed or field unsupported. Use Copy Last Transcript.",
        "Input queue overflow; dictation cancelled",
        "Choose a supported text field. Password fields are blocked.",
        "Use Set up permissions in the Control menu, then try again.",
        "Enable Accessibility. Keep Wispr Flow on Fn; hold Control here.",
        "Microphone stopped delivering audio; no text inserted",
        "Remote mode: use Start/Stop in menu",
        "Transcript ready · Review for RustDesk…",
        "Copied · paste manually in RustDesk",
        "Restore failed; recovery retained for retry",
    ] {
        assert_eq!(classify(m), Kind::Notice, "{m}");
    }
}
#[test]
fn idle_pill_is_far_smaller_than_active_looks() {
    let (w, h) = Look::Idle.size(0.0);
    assert!(w <= 40.0 && h <= 8.0);
    assert_eq!(Look::Warning.size(0.0), (w, h));
    for look in [Look::Armed, Look::Success] {
        let (aw, ah) = look.size(0.0);
        assert!(aw > w && ah > h && aw <= 48.0 && ah <= 8.0, "{look:?}");
    }
    for look in [
        Look::Starting,
        Look::Listening,
        Look::Processing,
        Look::Notice,
    ] {
        let (aw, ah) = look.size(80.0);
        assert!(aw * ah >= 4.0 * w * h, "{look:?}");
    }
    assert_eq!(Look::Notice.size(1000.0).0, NOTICE_MAX_WIDTH);
    assert_eq!(Look::Notice.size(40.5), (65.0, NOTICE_HEIGHT));
}
#[test]
fn listening_bars_stay_in_bounds_and_move() {
    for i in 0..BARS {
        let heights: Vec<f64> = (0..200).map(|t| bar_height(i, t * 20)).collect();
        assert!(heights.iter().all(|h| (BAR_MIN..=BAR_MAX).contains(h)));
        let (lo, hi) = heights
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(*h), hi.max(*h)));
        assert!(hi - lo > 4.0, "bar {i} barely moves");
    }
    assert_ne!(bar_height(0, 500), bar_height(1, 500));
    // Half-point steps land on device pixels at 2x.
    assert!((0..500).all(|t| (bar_height(3, t) * 2.0).fract() == 0.0));
}
#[test]
fn dots_pulse_within_visible_range() {
    for i in 0..DOTS {
        for t in (0..3000).step_by(20) {
            assert!((0.3..=1.0).contains(&dot_alpha(i, t)));
            assert!(dot_lift(i, t).abs() <= 1.5);
        }
    }
    assert_ne!(dot_alpha(0, 100), dot_alpha(2, 100));
}
#[test]
fn easing_converges_and_snaps_without_overshoot() {
    for (from, to) in [(36.0, 64.0), (300.0, 36.0), (6.0, 22.0)] {
        let mut size = from;
        let mut ticks = 0;
        while size != to {
            let next = ease(size, to);
            assert!((next - to).abs() < (size - to).abs());
            assert!((from..=to).contains(&next) || (to..=from).contains(&next));
            size = next;
            ticks += 1;
            assert!(ticks < 20, "did not settle");
        }
    }
    assert_eq!(ease(22.0, 22.0), 22.0);
}
#[test]
fn armed_hint_waits_for_a_hold_and_expires_unless_dictation_starts() {
    let mut ind = Indicator::default();
    ind.arm(1000);
    assert_eq!(ind.look(1000, Phase::Idle, false, false), Look::Idle);
    assert_eq!(
        ind.look(1000 + ARMED_DELAY_MS, Phase::Idle, false, false),
        Look::Armed
    );
    assert_eq!(
        ind.look(1000 + ARMED_MS - 1, Phase::Idle, false, true),
        Look::Armed
    );
    assert_eq!(
        ind.look(1000 + ARMED_MS, Phase::Idle, false, false),
        Look::Idle
    );
    assert_eq!(
        ind.look(1000 + ARMED_MS, Phase::Idle, false, true),
        Look::Warning
    );
    assert_eq!(
        ind.look(1100, Phase::Recording, false, false),
        Look::Starting
    );
    ind.arm(5000);
    ind.disarm();
    assert_eq!(
        ind.look(5000 + ARMED_DELAY_MS, Phase::Idle, false, false),
        Look::Idle
    );
}
#[test]
fn session_phases_and_flashes_choose_the_look() {
    let mut ind = Indicator::default();
    assert_eq!(ind.look(0, Phase::Recording, true, false), Look::Listening);
    assert_eq!(
        ind.look(0, Phase::Processing, true, false),
        Look::Processing
    );
    assert_eq!(ind.message("Text inserted", 100), None);
    assert_eq!(ind.look(100, Phase::Idle, false, true), Look::Success);
    assert_eq!(
        ind.look(100 + SUCCESS_MS, Phase::Idle, false, false),
        Look::Idle
    );
    assert_eq!(
        ind.message("No speech detected", 2000).as_deref(),
        Some("No speech detected")
    );
    assert_eq!(ind.look(2000, Phase::Recording, true, false), Look::Notice);
    assert_eq!(
        ind.look(2000 + NOTICE_MS, Phase::Recording, true, false),
        Look::Listening
    );
    ind.message("No speech detected", 3000);
    assert_eq!(ind.message("Cancelled", 3100), None);
    assert_eq!(ind.look(3100, Phase::Idle, false, false), Look::Idle);
    ind.message("No speech detected", 4000);
    ind.clear();
    assert_eq!(
        ind.look(4000, Phase::Recording, false, false),
        Look::Starting
    );
}
#[test]
fn notice_text_fits_whole_or_falls_back_to_the_first_clause() {
    for m in [
        "Finalization timed out; no text inserted",
        "Input queue overflow; dictation cancelled",
        "Remote mode: use Start/Stop in menu",
        "Transcript ready · Review for RustDesk…",
    ] {
        assert_eq!(shorten(m), m);
    }
    assert_eq!(
        shorten("Choose a supported text field. Password fields are blocked."),
        "Choose a supported text field"
    );
    assert_eq!(
        shorten("Record a single-line transcript first; remote copy rejects control characters"),
        "Record a single-line transcript first"
    );
    let long = "Deepgram rejected the streaming session because the configured credential expired";
    let short = shorten(long);
    assert!(short.ends_with('…') && long.starts_with(short.trim_end_matches('…')));
    for m in [
        long,
        "x".repeat(200).as_str(),
        "",
        "Unicode — ✓ ünïcödé ".repeat(9).as_str(),
        "Audio buffer overflow or microphone failure; no text inserted",
        "Provider disconnected before finalization; no text inserted",
    ] {
        assert!(shorten(m).chars().count() <= NOTICE_CHARS, "{m}");
    }
}
#[test]
fn notices_keep_the_recovery_hint() {
    assert_eq!(
        shorten("Insertion blocked: focus changed or field unsupported. Use Copy Last Transcript."),
        "Insertion blocked · Copy Last Transcript"
    );
    assert_eq!(
        shorten("Transcript ready — choose Copy Last Transcript"),
        "Transcript ready — choose Copy Last Transcript"
    );
    assert_eq!(
        shorten("Cannot preserve clipboard; use Copy Last Transcript"),
        "Cannot preserve clipboard · Copy Last Transcript"
    );
    assert_eq!(
        shorten("Copy failed or clipboard changed; use Restore previous clipboard if available"),
        "Copy failed · Restore previous clipboard"
    );
    for m in [
        "Cannot preserve clipboard; use Copy Last Transcript",
        "Use Copy Last Transcript (clipboard has multiple items)",
        "Deepgram took far too long to answer the finalize request. Use Copy Last Transcript.",
    ] {
        let short = shorten(m);
        assert!(short.contains("Copy Last Transcript"), "{m} -> {short}");
        assert!(short.chars().count() <= NOTICE_CHARS, "{short}");
    }
}
