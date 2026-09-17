//! The canonical internal transcript format (spec 7.2, STT-01).
//!
//! Every provider adapter normalizes into [`TranscriptSegment`]. Nothing above
//! this crate ever sees a provider's own JSON shape, which is what makes
//! "re-transcribe this meeting with a different provider" a data operation
//! rather than a rewrite.
//!
//! The wire shape is camelCase with lowercase enum tags so it matches the
//! TypeScript interface in the spec byte for byte.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};

thread_local! {
    /// A monotonic ULID generator.
    ///
    /// Plain `Ulid::new()` is only sortable at millisecond granularity: two ids
    /// minted in the same millisecond get independent random tails and can come
    /// out in either order. Partials arrive many per second, so that is not a
    /// theoretical case — it would scramble the transcript's ordering key.
    static ULID_GENERATOR: RefCell<ulid::Generator> = const { RefCell::new(ulid::Generator::new()) };
}

/// Mint the next segment ULID, monotonic within this thread.
///
/// Falls back to a non-monotonic ULID if the generator's random component
/// overflows inside a single millisecond, which needs ~2^80 ids in one
/// millisecond to happen. A slightly out-of-order id beats a panic.
#[must_use]
pub fn next_segment_id() -> String {
    ULID_GENERATOR
        .with(|generator| generator.borrow_mut().generate())
        .unwrap_or_else(|_| ulid::Ulid::new())
        .to_string()
}

/// Which capture stream a segment came from.
///
/// The two-stream default (spec 7.5) is what gives us a diarization-free
/// me-vs-them split, so this is structural information, not a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The local microphone: definitionally the user.
    Mic,
    /// The system output tap: everybody else on the call.
    System,
}

/// Where a segment's timestamps came from.
///
/// `Estimated` means we synthesized the times from our own audio-clock position
/// when the delta arrived, because the provider did not send any (spec 7.2 rule
/// 3). The UI uses this to decide whether click-to-seek is trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimestampSource {
    /// The provider reported the times and we only rebased them onto our clock.
    #[default]
    Provider,
    /// We synthesized the times from arrival position on our own audio clock.
    Estimated,
}

/// One word of a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    /// The word as it should be displayed — punctuated and cased where the
    /// provider offers that (Deepgram's `punctuated_word`).
    pub text: String,
    /// Milliseconds from session t0 on **our** clock. Never provider-relative.
    pub start_ms: u64,
    /// Milliseconds from session t0 on **our** clock. Never provider-relative.
    pub end_ms: u64,
    /// `None` when the provider reports no confidence (OpenAI streaming).
    pub confidence: Option<f64>,
    /// Normalized speaker label (`S0..Sn`, or `me`), never the provider's own id.
    pub speaker: Option<String>,
}

impl Word {
    /// Duration of the word in milliseconds, saturating at zero.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// One contiguous chunk of transcript from one capture stream.
///
/// Partials and the final that supersedes them share an `id` and differ by
/// `revision` (spec 7.2 rule 4); see [`crate::SegmentStore`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    /// ULID. Stable across every revision of the same utterance.
    pub id: String,
    /// The recording session this belongs to.
    pub session_id: String,
    /// Which capture stream produced it.
    pub source: Source,
    /// Normalized speaker label, `None` when unknown or mixed.
    pub speaker: Option<String>,
    /// The display text for the whole segment.
    pub text: String,
    /// Milliseconds from session t0 on **our** clock.
    pub start_ms: u64,
    /// Milliseconds from session t0 on **our** clock.
    pub end_ms: u64,
    /// Per-word detail. `[]` is legal — OpenAI streaming returns none.
    pub words: Vec<Word>,
    /// Segment-level confidence, `None` when the provider reports none.
    pub confidence: Option<f64>,
    /// BCP-47-ish language tag as the provider reports it, or `None`.
    pub language: Option<String>,
    /// `true` once no further revision of this `id` will arrive.
    pub is_final: bool,
    /// Monotonically increasing per `id`. The store keeps only the newest.
    pub revision: u32,
    /// Provider id, e.g. `deepgram`. Recorded per segment because a session can
    /// fail over mid-meeting (STT-11).
    pub provider: String,
    /// Model id, e.g. `nova-3`.
    pub model: String,
    /// Whether the times above were reported or synthesized.
    pub timestamp_source: TimestampSource,
}

impl TranscriptSegment {
    /// A fresh segment with a newly minted ULID and everything else empty.
    ///
    /// Adapters fill in the rest. Revision starts at 0 and `is_final` at false,
    /// which is the correct state for the first partial of an utterance.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        source: Source,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: next_segment_id(),
            session_id: session_id.into(),
            source,
            speaker: None,
            text: String::new(),
            start_ms: 0,
            end_ms: 0,
            words: Vec::new(),
            confidence: None,
            language: None,
            is_final: false,
            revision: 0,
            provider: provider.into(),
            model: model.into(),
            timestamp_source: TimestampSource::Provider,
        }
    }

    /// Duration of the segment in milliseconds, saturating at zero.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// Whether `other` is a newer revision of the same utterance.
    #[must_use]
    pub fn supersedes(&self, other: &Self) -> bool {
        self.id == other.id && self.revision > other.revision
    }
}
