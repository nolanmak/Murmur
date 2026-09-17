//! What the mock provider "hears" at each point on the session clock.

/// One word, positioned on the session clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptWord {
    /// The display form, as `punctuated_word` would carry it.
    pub text: String,
    /// Session milliseconds.
    pub start_ms: u64,
    /// Session milliseconds.
    pub end_ms: u64,
    /// Diarization index.
    pub speaker: u32,
}

/// One utterance: the unit Deepgram closes with `speech_final`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptUtterance {
    /// Its words, in order.
    pub words: Vec<ScriptWord>,
}

impl ScriptUtterance {
    /// Session position of the first word.
    pub fn start_ms(&self) -> u64 {
        self.words.first().map_or(0, |word| word.start_ms)
    }

    /// Session position of the last word.
    pub fn end_ms(&self) -> u64 {
        self.words.last().map_or(0, |word| word.end_ms)
    }

    /// The utterance as Deepgram would transcribe it.
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A whole meeting, laid out on the session clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptScript {
    /// Utterances in chronological order, non-overlapping.
    pub utterances: Vec<ScriptUtterance>,
}

impl TranscriptScript {
    /// Lay `sentences` out end to end, `word_ms` per word and `gap_ms` of
    /// silence between utterances.
    ///
    /// The gaps are load-bearing: a reconnect that lands inside one has no
    /// dangling partial to deduplicate, and a reconnect that lands mid-utterance
    /// does. Both cases have to occur in the same fixture or the chaos test only
    /// covers whichever one it happened to hit.
    pub fn from_sentences(sentences: &[&str], word_ms: u64, gap_ms: u64) -> Self {
        let mut cursor = gap_ms;
        let mut utterances = Vec::new();
        for (index, sentence) in sentences.iter().enumerate() {
            let mut words = Vec::new();
            for token in sentence.split_whitespace() {
                words.push(ScriptWord {
                    text: token.to_string(),
                    start_ms: cursor,
                    end_ms: cursor + word_ms,
                    speaker: (index % 2) as u32,
                });
                cursor += word_ms;
            }
            utterances.push(ScriptUtterance { words });
            cursor += gap_ms;
        }
        Self { utterances }
    }

    /// A fixture long enough for several reconnects to land in different places.
    ///
    /// Every word is distinct so a deduplicator cannot pass by luck: with a
    /// vocabulary of "yes"/"okay"/"right" a wrong overlap length still produces
    /// plausible text, and the test would go green on a broken matcher.
    pub fn fixture() -> Self {
        let sentences = [
            "opening remarks about quarterly revenue targets",
            "marketing spent heavily on paid acquisition channels",
            "engineering shipped the audio capture rewrite",
            "support tickets dropped nineteen percent since March",
            "legal flagged three outstanding vendor agreements",
            "recruiting closed two senior infrastructure roles",
            "finance wants headcount forecasts before Thursday",
            "design proposed a lighter onboarding flow",
            "data showed weekend usage climbing steadily",
            "security completed the annual penetration review",
            "operations renegotiated the colocation contract terms",
            "product deferred the collaboration features again",
            "partnerships signed a reseller in Singapore",
            "customers asked repeatedly for offline transcription",
            "leadership approved additional storage budget",
            "documentation lagged behind the shipped functionality",
            "analytics revealed churn concentrated among trials",
            "infrastructure migrated logging to cheaper storage",
            "hiring managers requested clearer interview rubrics",
            "everyone agreed to reconvene next Tuesday morning",
        ];
        Self::from_sentences(&sentences, 300, 700)
    }

    /// Session position just past the final word.
    pub fn total_ms(&self) -> u64 {
        self.utterances.last().map_or(0, ScriptUtterance::end_ms)
    }

    /// The transcript an uninterrupted run should produce.
    pub fn expected_text(&self) -> String {
        self.utterances
            .iter()
            .map(ScriptUtterance::text)
            .collect::<Vec<_>>()
            .join(" ")
    }
}
