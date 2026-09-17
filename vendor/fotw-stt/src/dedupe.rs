//! Deduplicating replayed transcript on reconnect (spec 4.2, STT-09).
//!
//! Gapless replay re-feeds the provider audio it has already heard, so the first
//! finals after a reconnect restate words that are already in the transcript.
//! STT-09's rule is to compare **normalized leading text against the transcript
//! tail** and drop the overlap.
//!
//! "Normalized" is doing real work. The provider does not re-emit the replayed
//! audio verbatim: punctuation, casing and smart-formatting all shift with the
//! surrounding context, so `"Okay, so —"` and `"okay so"` are the same words and
//! a byte comparison says they are not. Stripping to lowercase alphanumerics
//! makes the comparison about the words, which is the only level at which the
//! two runs are actually expected to agree.

use std::collections::VecDeque;

use crate::TranscriptSegment;

/// How many tokens of transcript the tail keeps by default.
///
/// The overlap to remove is at most one utterance — the one that was in flight
/// when the socket died — so this only has to be comfortably longer than an
/// utterance, not longer than the meeting.
pub const DEFAULT_TAIL_TOKENS: usize = 120;

/// The longest overlap the matcher will consider.
///
/// Bounds the comparison and, more usefully, refuses to believe that two
/// minutes of identical text is a replay artifact rather than someone actually
/// repeating themselves.
pub const MAX_OVERLAP_TOKENS: usize = 64;

/// Split text into comparison tokens: lowercase, alphanumerics only.
///
/// One input whitespace-token yields at most one output token, which is what
/// lets [`trim_leading_tokens`] map a token count back onto whole words.
#[must_use]
pub fn normalize_tokens(text: &str) -> Vec<String> {
    text.split_whitespace().filter_map(normalize_one).collect()
}

/// Normalize a single whitespace-delimited unit, or `None` if nothing survives.
#[must_use]
fn normalize_one(unit: &str) -> Option<String> {
    let normalized: String = unit
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    (!normalized.is_empty()).then_some(normalized)
}

/// A bounded window of the most recently committed transcript text.
///
/// Only finals go in. A partial is by definition going to be restated, so
/// letting one into the tail would make the next final look like a duplicate of
/// text the user never saw settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptTail {
    tokens: VecDeque<String>,
    capacity: usize,
}

impl Default for TranscriptTail {
    fn default() -> Self {
        Self::new(DEFAULT_TAIL_TOKENS)
    }
}

impl TranscriptTail {
    /// A tail holding at most `capacity` tokens.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            tokens: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Append a committed segment's text.
    pub fn push_text(&mut self, text: &str) {
        for token in normalize_tokens(text) {
            self.tokens.push_back(token);
        }
        while self.tokens.len() > self.capacity {
            self.tokens.pop_front();
        }
    }

    /// How many tokens are retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether nothing has been committed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Forget everything. Used on failover, where the next provider's first
    /// output is not a replay of ours.
    pub fn clear(&mut self) {
        self.tokens.clear();
    }

    /// The longest suffix of the tail that is also a prefix of `incoming`.
    ///
    /// Longest rather than shortest on purpose: the shortest match is almost
    /// always a single common word (`"the"`), and trimming one word off a
    /// wholly-duplicated utterance leaves the other nine in the transcript
    /// twice.
    #[must_use]
    pub fn overlap_with(&self, incoming: &[String]) -> usize {
        let max = self
            .tokens
            .len()
            .min(incoming.len())
            .min(MAX_OVERLAP_TOKENS);
        for length in (1..=max).rev() {
            let tail_start = self.tokens.len() - length;
            if self
                .tokens
                .iter()
                .skip(tail_start)
                .zip(incoming.iter())
                .all(|(mine, theirs)| mine == theirs)
            {
                return length;
            }
        }
        0
    }

    /// The overlap between the tail and `text`, normalizing `text` first.
    #[must_use]
    pub fn overlap_with_text(&self, text: &str) -> usize {
        self.overlap_with(&normalize_tokens(text))
    }
}

/// Drop the first `tokens` words from `segment`, in place.
///
/// Returns `false` when nothing is left — the segment was wholly a replay and
/// the caller should drop it rather than emit an empty line.
///
/// Word timings move with the text: the trimmed segment starts at the first
/// surviving word, so a click-to-seek on it lands where the remaining audio
/// actually is rather than back inside the duplicated part.
pub fn trim_leading_tokens(segment: &mut TranscriptSegment, tokens: usize) -> bool {
    if tokens == 0 {
        return !segment.text.trim().is_empty() || !segment.words.is_empty();
    }

    if segment.words.is_empty() {
        return trim_text_only(segment, tokens);
    }

    let drop_count = units_covering(segment.words.iter().map(|word| word.text.as_str()), tokens);
    if drop_count >= segment.words.len() {
        segment.words.clear();
        segment.text.clear();
        return false;
    }

    segment.words.drain(..drop_count);
    segment.start_ms = segment.words[0].start_ms;
    segment.text = segment
        .words
        .iter()
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    true
}

fn trim_text_only(segment: &mut TranscriptSegment, tokens: usize) -> bool {
    let units: Vec<&str> = segment.text.split_whitespace().collect();
    let drop_count = units_covering(units.iter().copied(), tokens);
    if drop_count >= units.len() {
        segment.text.clear();
        return false;
    }
    segment.text = units[drop_count..].join(" ");
    true
}

/// How many leading units it takes to cover `tokens` normalized tokens.
///
/// A unit that normalizes to nothing (a lone dash from smart formatting) is
/// consumed for free rather than counted, or it would survive the trim and turn
/// up as punctuation floating at the start of a line.
fn units_covering<'a, I>(units: I, tokens: usize) -> usize
where
    I: IntoIterator<Item = &'a str>,
{
    let mut consumed = 0;
    let mut taken = 0;
    for unit in units {
        if consumed >= tokens {
            break;
        }
        taken += 1;
        if normalize_one(unit).is_some() {
            consumed += 1;
        }
    }
    taken
}
