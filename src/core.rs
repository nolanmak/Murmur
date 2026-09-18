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
        if id != self.generation {
            return None;
        }
        let accepts = self.accepts(id);
        let effect = match result {
            Ok(text) if accepts && !text.trim().is_empty() => CompletionEffect::Transcript(text),
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
