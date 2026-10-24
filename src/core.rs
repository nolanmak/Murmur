//! Pure recording controller. Native callbacks contain no lifecycle decisions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Recording,
    Processing,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CompletionEffect {
    Transcript(String),
    Empty,
    Error(String),
}
#[derive(Debug, Default)]
pub struct Dictation {
    pub phase: Phase,
    down: bool,
    generation: u64,
}
impl Dictation {
    pub fn start_manual(&mut self) -> bool {
        if self.phase != Phase::Idle {
            return false;
        }
        self.down = false;
        self.generation += 1;
        self.phase = Phase::Recording;
        true
    }
    pub fn flags(&mut self, key_down: bool, other_modifier: bool) -> Option<&'static str> {
        let rising = key_down && !self.down;
        let falling = !key_down && self.down;
        self.down = key_down;
        if other_modifier && self.phase == Phase::Recording {
            self.cancel();
            return Some("cancel");
        }
        if rising && !other_modifier && self.phase == Phase::Idle {
            self.generation += 1;
            self.phase = Phase::Recording;
            return Some("start");
        }
        if falling && self.phase == Phase::Recording {
            self.phase = Phase::Processing;
            return Some("finish");
        }
        None
    }
    pub fn cancel(&mut self) -> bool {
        if self.phase == Phase::Idle {
            return false;
        }
        self.generation += 1;
        self.phase = Phase::Idle;
        true
    }
    pub fn finish(&mut self) {
        self.phase = Phase::Idle;
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn accepts(&self, id: u64) -> bool {
        self.generation == id && self.phase == Phase::Processing
    }
    pub fn complete(
        &mut self,
        id: u64,
        result: Result<String, String>,
    ) -> Option<CompletionEffect> {
        // A worker can end before release (mic stall, connection failure). It
        // will never report again, so ending the attempt here is the only way
        // out; dropping it left Processing waiting forever after release.
        if self.generation == id && self.phase == Phase::Recording {
            self.finish();
            return Some(CompletionEffect::Error(match result {
                Err(error) => error,
                Ok(_) => "Dictation ended before release; no text inserted".into(),
            }));
        }
        if !self.accepts(id) {
            return None;
        }
        let effect = match result {
            Ok(text) if !text.trim().is_empty() => CompletionEffect::Transcript(text),
            Ok(_) => CompletionEffect::Empty,
            Err(error) => CompletionEffect::Error(error),
        };
        self.finish();
        Some(effect)
    }
    pub fn stop(&mut self) -> bool {
        if self.phase == Phase::Recording {
            self.phase = Phase::Processing;
            true
        } else {
            false
        }
    }
}
/// UI-side limit on Processing after release. Every worker path has its own
/// deadline (longest: 3 s cloud final + 180 s local inference); this only
/// catches a worker that never reports back, so the pill cannot spin forever.
pub const PROCESSING_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(200);
pub fn processing_overdue(since_release: std::time::Duration) -> bool {
    since_release >= PROCESSING_BACKSTOP
}
/// Runs a dictation worker so a panic still yields a completion for the UI.
pub fn contain_worker(worker: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(worker))
        .unwrap_or_else(|_| Err("Transcription crashed; no text inserted".into()))
}
