//! Pure on-screen indicator model: looks, sizes, motion curves and message classes.
use crate::core::Phase;
use std::f64::consts::TAU;
pub const ARMED_DELAY_MS: u64 = 150;
pub const ARMED_MS: u64 = 450;
pub const SUCCESS_MS: u64 = 1000;
pub const NOTICE_MS: u64 = 3500;
pub const NOTICE_CHARS: usize = 48;
pub const NOTICE_HEIGHT: f64 = 22.0;
pub const NOTICE_PADDING: f64 = 12.0;
pub const NOTICE_MAX_WIDTH: f64 = 300.0;
pub const BARS: usize = 7;
pub const BAR_WIDTH: f64 = 2.5;
pub const BAR_GAP: f64 = 2.5;
pub const BAR_MIN: f64 = 4.0;
pub const BAR_MAX: f64 = 12.0;
pub const DOTS: usize = 3;
pub const DOT_SIZE: f64 = 4.0;
pub const DOT_GAP: f64 = 4.0;
pub const EASE: f64 = 0.4;
pub const SNAP: f64 = 0.5;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Idle,
    Warning,
    Armed,
    Starting,
    Listening,
    Processing,
    Success,
    Notice,
}
impl Look {
    /// Target (width, height); `text_width` only sizes the notice pill.
    pub fn size(self, text_width: f64) -> (f64, f64) {
        match self {
            Look::Idle | Look::Warning => (36.0, 6.0),
            Look::Armed | Look::Success => (44.0, 8.0),
            Look::Starting | Look::Processing => (50.0, 20.0),
            Look::Listening => (64.0, 22.0),
            Look::Notice => (
                (text_width + 2.0 * NOTICE_PADDING)
                    .ceil()
                    .clamp(44.0, NOTICE_MAX_WIDTH),
                NOTICE_HEIGHT,
            ),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Success,
    Cancelled,
    Notice,
}
pub fn classify(message: &str) -> Kind {
    match message {
        "Text inserted" | "Transcript copied" => Kind::Success,
        "Cancelled" | "Cancelled Control shortcut" | "Remote copy cancelled" => Kind::Cancelled,
        _ => Kind::Notice,
    }
}
const RECOVERY: [&str; 2] = ["Copy Last Transcript", "Restore previous clipboard"];
/// Whole message when it fits; otherwise its first clause, keeping any recovery hint.
pub fn shorten(message: &str) -> String {
    let whole = message.trim().trim_end_matches('.');
    if whole.chars().count() <= NOTICE_CHARS {
        return whole.into();
    }
    let head = [". ", "; ", ": ", " (", " — "]
        .iter()
        .filter_map(|sep| message.find(sep))
        .min()
        .map_or(message, |end| &message[..end])
        .trim()
        .trim_end_matches('.');
    if let Some(hint) = RECOVERY
        .iter()
        .find(|hint| message.contains(*hint) && !head.contains(*hint))
    {
        let suffix = format!(" · {hint}");
        let limit = NOTICE_CHARS - suffix.chars().count();
        let head = match head.split_once(" or ") {
            Some((first, _)) if head.chars().count() > limit => first,
            _ => head,
        };
        return fit(head, limit) + &suffix;
    }
    fit(head, NOTICE_CHARS)
}
fn fit(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.into();
    }
    let cut: String = text.chars().take(limit - 1).collect();
    let cut = match cut.rfind(' ') {
        Some(space) if space > 0 => &cut[..space],
        _ => &cut,
    };
    format!("{}…", cut.trim_end_matches([' ', ',', ':', '—']))
}
#[derive(Debug, Default)]
pub struct Indicator {
    armed: Option<u64>,
    flash: Option<(Look, u64)>,
}
impl Indicator {
    pub fn arm(&mut self, now: u64) {
        self.armed = Some(now);
    }
    /// Control was released or became a shortcut before dictation started.
    pub fn disarm(&mut self) {
        self.armed = None;
    }
    /// Records a status message; returns the pill text when it needs one.
    pub fn message(&mut self, message: &str, now: u64) -> Option<String> {
        match classify(message) {
            Kind::Success => self.flash = Some((Look::Success, now + SUCCESS_MS)),
            Kind::Cancelled => self.flash = None,
            Kind::Notice => {
                self.flash = Some((Look::Notice, now + NOTICE_MS));
                return Some(shorten(message));
            }
        }
        None
    }
    pub fn clear(&mut self) {
        self.flash = None;
    }
    pub fn look(&self, now: u64, phase: Phase, receiving: bool, warning: bool) -> Look {
        let flash = self.flash.filter(|(_, until)| now < *until).map(|f| f.0);
        match (flash, phase) {
            (Some(Look::Notice), _) => Look::Notice,
            (_, Phase::Recording) if receiving => Look::Listening,
            (_, Phase::Recording) => Look::Starting,
            (_, Phase::Processing) => Look::Processing,
            (Some(look), _) => look,
            _ if self
                .armed
                .is_some_and(|at| (at + ARMED_DELAY_MS..at + ARMED_MS).contains(&now)) =>
            {
                Look::Armed
            }
            _ if warning => Look::Warning,
            _ => Look::Idle,
        }
    }
}
/// One easing step toward `target`; snaps once close so motion settles exactly.
pub fn ease(current: f64, target: f64) -> f64 {
    let next = current + (target - current) * EASE;
    if (target - next).abs() < SNAP {
        target
    } else {
        next
    }
}
pub fn bar_height(index: usize, elapsed_ms: u64) -> f64 {
    let t = elapsed_ms as f64 / 1000.0;
    let i = index as f64;
    let wave = 0.6 * (t * TAU * 1.3 + i * 0.9).sin() + 0.4 * (t * TAU * 2.1 + i * 1.7).sin();
    let centre = 1.0 - 0.3 * (i - (BARS as f64 - 1.0) / 2.0).abs() / 3.0;
    let height = BAR_MIN + (BAR_MAX - BAR_MIN) * (0.5 + 0.5 * wave) * centre;
    (height * 2.0).round() / 2.0
}
/// Sequential pulse used while the microphone starts.
pub fn dot_alpha(index: usize, elapsed_ms: u64) -> f64 {
    let phase = elapsed_ms as f64 / 900.0 - index as f64 / 6.0;
    0.3 + 0.7 * (phase * TAU).sin().max(0.0)
}
/// Gentle vertical wave used while finishing.
pub fn dot_lift(index: usize, elapsed_ms: u64) -> f64 {
    (3.0 * ((elapsed_ms as f64 / 1100.0 - index as f64 / 5.0) * TAU).sin()).round() / 2.0
}
