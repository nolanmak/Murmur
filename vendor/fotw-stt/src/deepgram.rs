//! Normalizing Deepgram streaming responses (spec 7.4, STT-03).
//!
//! Pure data in, canonical segments out. There is no socket here on purpose:
//! the WebSocket client, the KeepAlive timer and the reconnect loop all live
//! above this module, so every normalization rule in spec 7.2 is testable from a
//! checked-in JSON fixture with no network and no runtime.
//!
//! Two Deepgram flags do different jobs and the difference is load-bearing:
//! `is_final` finalizes a *chunk* of audio — its text will not change — while
//! `speech_final` marks the end of an *utterance* at an endpoint. Treating the
//! first as the second chops one sentence into several transcript lines at
//! arbitrary boundaries, so a finalized chunk that is not `speech_final` is
//! committed and carried forward into the next partial instead.

use serde::{Deserialize, Serialize};

use crate::{
    ArrivalEstimator, SegmentIdFactory, SessionClock, Source, SpeakerNormalizer, SttError,
    SttErrorClass, TimeSpan, TranscriptSegment, UlidFactory, UtteranceTracker, Word, seconds_to_ms,
};

/// The provider id recorded on every segment this module produces.
pub const PROVIDER: &str = "deepgram";

/// The default streaming model (spec 7.1).
pub const DEFAULT_MODEL: &str = "nova-3";

// ---------------------------------------------------------------------------
// Deepgram's wire shape
// ---------------------------------------------------------------------------

/// One message from the Deepgram streaming socket.
///
/// Every field is optional-with-a-default because the same socket also carries
/// `Metadata`, `SpeechStarted` and `UtteranceEnd` frames, and a normalizer that
/// panics on one of those would take the meeting down.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeepgramResult {
    /// `Results`, `Metadata`, `SpeechStarted`, `UtteranceEnd`, …
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    /// This chunk's transcript is settled and will not be revised.
    #[serde(default)]
    pub is_final: bool,
    /// Deepgram's endpointer decided the utterance ended here.
    #[serde(default)]
    pub speech_final: bool,
    /// Chunk start, in seconds from the connection's own zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f64>,
    /// Chunk duration in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// The `Results` channel object, or `None` on any other frame.
    ///
    /// Read through [`channel_object_only`] because `channel` is not one
    /// shape: `Results` sends the object holding `alternatives`, while
    /// `SpeechStarted` and `UtteranceEnd` send an array of channel indices,
    /// `[0,1]`.
    #[serde(
        default,
        deserialize_with = "channel_object_only",
        skip_serializing_if = "Option::is_none"
    )]
    pub channel: Option<DeepgramChannel>,
}

/// Accept the object form of `channel` and ignore every other shape.
///
/// # The bug this exists to prevent
///
/// Typed as `Option<DeepgramChannel>` alone, serde read `SpeechStarted`'s
/// `"channel":[0,1]` as the struct — arrays deserialise into structs
/// positionally — mapped element `0` onto `alternatives`, and failed with
/// "invalid type: integer `0`, expected a sequence".
///
/// That was fatal rather than cosmetic. `vad_events=true` is in the spec
/// parameter set, so `SpeechStarted` is the *first* frame Deepgram sends: the
/// reader errored before any `Results` frame arrived, the driver closed the
/// stream, and `session::run` — which consumes only `StreamEvent::Final` —
/// reported nothing at all. Every recording came back with an empty
/// transcript that was indistinguishable from a meeting where nobody spoke.
fn channel_object_only<'de, D>(deserializer: D) -> Result<Option<DeepgramChannel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_object() {
        serde_json::from_value(value)
            .map(Some)
            .map_err(serde::de::Error::custom)
    } else {
        // An array of channel indices, or null. Neither carries alternatives,
        // and neither is a reason to drop the connection.
        Ok(None)
    }
}

/// The `channel` object of a `Results` frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeepgramChannel {
    /// Ranked hypotheses. We read `[0]`; the rest are not worth the storage.
    #[serde(default)]
    pub alternatives: Vec<DeepgramAlternative>,
}

/// One transcription hypothesis.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeepgramAlternative {
    /// The whole chunk as text.
    #[serde(default)]
    pub transcript: String,
    /// Chunk-level confidence in `0.0..=1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Per-word detail. Empty when word timings were not requested.
    #[serde(default)]
    pub words: Vec<DeepgramWord>,
}

/// One word from Deepgram, with its own timings in **seconds**.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeepgramWord {
    /// The raw token: lowercased and unpunctuated.
    #[serde(default)]
    pub word: String,
    /// The display form, present when `smart_format`/`punctuate` are on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub punctuated_word: Option<String>,
    /// Start, in seconds from the connection's own zero.
    #[serde(default)]
    pub start: f64,
    /// End, in seconds from the connection's own zero.
    #[serde(default)]
    pub end: f64,
    /// Per-word confidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// Diarization index, present only when `diarize` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
}

impl DeepgramWord {
    /// The display text, preferring `punctuated_word` (spec 7.4).
    ///
    /// The raw `word` is lowercased and unpunctuated, which reads badly in the
    /// transcript and breaks quote matching against the summary.
    #[must_use]
    pub fn display_text(&self) -> &str {
        match &self.punctuated_word {
            Some(punctuated) if !punctuated.is_empty() => punctuated,
            _ => &self.word,
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What the normalizer needs to know that the wire does not tell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepgramConfig {
    /// The recording session these segments belong to.
    pub session_id: String,
    /// Which capture stream this connection is transcribing.
    pub source: Source,
    /// The model id, recorded on every segment.
    pub model: String,
    /// The language, when it was pinned rather than detected.
    pub language: Option<String>,
    /// Whether `diarize` was requested on this connection.
    pub diarization_enabled: bool,
}

impl DeepgramConfig {
    /// The default configuration for a stream.
    ///
    /// Diarization defaults on for the system stream and off for the mic: under
    /// the two-stream default (spec 7.5) the mic stream is one known person, so
    /// diarizing it is a paid add-on that can only introduce error.
    #[must_use]
    pub fn new(session_id: impl Into<String>, source: Source) -> Self {
        Self {
            session_id: session_id.into(),
            source,
            model: DEFAULT_MODEL.to_string(),
            language: None,
            diarization_enabled: source == Source::System,
        }
    }
}

// ---------------------------------------------------------------------------
// The normalizer
// ---------------------------------------------------------------------------

/// Text and words committed by `is_final` chunks of the utterance in progress.
#[derive(Debug, Clone, Default)]
struct Committed {
    text: String,
    words: Vec<Word>,
    /// Word-count-weighted confidence, so a long chunk counts for more than a
    /// two-word one.
    confidence_sum: f64,
    confidence_weight: f64,
}

impl Committed {
    fn is_empty(&self) -> bool {
        self.text.is_empty() && self.words.is_empty()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Turns Deepgram streaming frames into canonical transcript segments.
///
/// Stateful across a connection — it owns the utterance ids, the speaker
/// registry and the clock offset — but deterministic: the same frames through a
/// freshly constructed normalizer always produce the same segments, which is
/// what makes the golden-file test meaningful.
#[derive(Debug)]
pub struct DeepgramNormalizer<F = UlidFactory> {
    config: DeepgramConfig,
    clock: SessionClock,
    speakers: SpeakerNormalizer,
    tracker: UtteranceTracker<F>,
    estimator: ArrivalEstimator,
    committed: Committed,
    /// The most recent emission of the open utterance, so a stream that ends
    /// mid-utterance can still be closed with a final.
    open: Option<TranscriptSegment>,
}

impl DeepgramNormalizer<UlidFactory> {
    /// A normalizer minting ULID segment ids.
    #[must_use]
    pub fn new(config: DeepgramConfig) -> Self {
        Self::with_id_factory(config, UlidFactory)
    }

    /// [`new`](Self::new), for a capture leg whose own audio zero is
    /// `anchor_ms` into the session (#86).
    ///
    /// The anchor is a property of the leg, not of the connection: it is
    /// applied at connection zero and survives every reconnect. `0` is
    /// [`new`](Self::new).
    #[must_use]
    pub fn anchored_at(config: DeepgramConfig, anchor_ms: u64) -> Self {
        let mut normalizer = Self::with_id_factory(config, UlidFactory);
        normalizer.clock = SessionClock::anchored_at(anchor_ms);
        normalizer.estimator = ArrivalEstimator::starting_at(anchor_ms);
        normalizer
    }

    /// A normalizer minting `prefix-0`, `prefix-1`, … instead of ULIDs.
    ///
    /// For fixtures and golden files, where a random id would make the expected
    /// output unwritable.
    #[must_use]
    pub fn with_test_ids(
        config: DeepgramConfig,
        prefix: impl Into<String>,
    ) -> DeepgramNormalizer<crate::CountingIdFactory> {
        DeepgramNormalizer::with_id_factory(config, crate::CountingIdFactory::new(prefix))
    }
}

impl<F: SegmentIdFactory> DeepgramNormalizer<F> {
    /// A normalizer minting ids from `factory`.
    pub fn with_id_factory(config: DeepgramConfig, factory: F) -> Self {
        let speakers = SpeakerNormalizer::new(config.source, config.diarization_enabled);
        Self {
            config,
            clock: SessionClock::new(),
            speakers,
            tracker: UtteranceTracker::with_id_factory(factory),
            estimator: ArrivalEstimator::new(),
            committed: Committed::default(),
            open: None,
        }
    }

    /// Where this connection's zero sits on the session clock.
    #[must_use]
    pub fn clock(&self) -> SessionClock {
        self.clock
    }

    /// Normalize a batch of frames.
    ///
    /// On a freshly constructed normalizer this is a pure function of
    /// `(config, messages)`.
    pub fn normalize_all(&mut self, messages: &[DeepgramResult]) -> Vec<TranscriptSegment> {
        self.normalize_at(messages, None)
    }

    /// Normalize a batch of frames that all arrived at `arrival_session_ms`.
    ///
    /// The arrival position is only consulted for frames carrying no timings at
    /// all; see [`push_message_at`](Self::push_message_at).
    pub fn normalize_at(
        &mut self,
        messages: &[DeepgramResult],
        arrival_session_ms: Option<u64>,
    ) -> Vec<TranscriptSegment> {
        messages
            .iter()
            .filter_map(|message| self.push_message_at(message, arrival_session_ms))
            .collect()
    }

    /// Normalize one frame.
    pub fn push_message(&mut self, message: &DeepgramResult) -> Option<TranscriptSegment> {
        self.push_message_at(message, None)
    }

    /// Normalize one raw JSON frame off the socket.
    ///
    /// A frame we cannot parse is classified `server` and retryable: the socket
    /// itself is fine, we just could not read that message, and dropping the
    /// provider over one bad frame would cost the user their transcript.
    pub fn push_json(&mut self, raw: &str) -> Result<Option<TranscriptSegment>, SttError> {
        let message: DeepgramResult = serde_json::from_str(raw).map_err(|source| {
            SttError::new(
                SttErrorClass::Server,
                PROVIDER,
                SttErrorClass::Server.user_hint(),
            )
            .with_detail(format!("could not parse a Deepgram frame: {source}"))
        })?;
        Ok(self.push_message(&message))
    }

    /// Normalize one frame, with the session-clock position at which it arrived.
    ///
    /// `arrival_session_ms` is a fallback, not the normal path: Deepgram reports
    /// real timings, and a synthesized time is always marked
    /// [`TimestampSource::Estimated`] so the UI can tell the difference.
    pub fn push_message_at(
        &mut self,
        message: &DeepgramResult,
        arrival_session_ms: Option<u64>,
    ) -> Option<TranscriptSegment> {
        let alternative = message.primary_alternative()?;

        // Deepgram emits empty interims throughout silence. They carry no
        // information; turning them into segments would put blank lines in the
        // transcript and bump the revision for nothing.
        if alternative.transcript.trim().is_empty() && alternative.words.is_empty() {
            return None;
        }

        let words: Vec<Word> = alternative
            .words
            .iter()
            .map(|word| self.normalize_word(word))
            .collect();
        let span = self.span_for(message, &words, arrival_session_ms);

        // `speech_final` ends the utterance. `is_final` alone only settles this
        // chunk, so the segment stays open and the chunk is carried forward.
        let closes_utterance = message.speech_final;
        let (id, revision) = if closes_utterance {
            self.tracker.next_final()
        } else {
            self.tracker.next_partial()
        };

        let segment = self.build_segment(id, revision, closes_utterance, alternative, &words, span);

        if closes_utterance {
            self.committed.clear();
            self.open = None;
        } else {
            if message.is_final {
                self.commit(alternative, words);
            }
            self.open = Some(segment.clone());
        }

        Some(segment)
    }

    /// Close the open utterance, if any, with a final.
    ///
    /// Called on `close()` and before a reconnect. Without it the last partial
    /// before the socket died stays non-final forever, and the conformance
    /// property "every partial is eventually superseded by a final" (spec 7.3)
    /// fails at end of session.
    pub fn finish(&mut self) -> Option<TranscriptSegment> {
        let mut segment = self.open.take()?;
        let (id, revision) = self.tracker.next_final();
        // The tracker's open utterance and `self.open` are set and cleared
        // together, so these always agree. Keep the segment's own id rather than
        // the tracker's, so a future divergence orphans a minted id instead of
        // renaming a segment the store has already seen.
        debug_assert_eq!(id, segment.id, "finish() must not mint a new utterance id");
        segment.revision = revision;
        segment.is_final = true;
        self.committed.clear();
        Some(segment)
    }

    /// Rebase onto a new connection after a reconnect (spec 7.2 rule 1).
    ///
    /// `leg_ms` is the position of the first audio sample fed to the new
    /// connection on *this leg's own* audio clock — with STT-09's gapless
    /// replay that is where the replay ring resumed, not the moment the socket
    /// opened. This leg's session anchor (#86) sits underneath it and survives.
    ///
    /// Returns a final for whatever utterance was open, because the replayed
    /// audio will be re-transcribed from scratch by the new connection. The
    /// resulting overlap is deduplicated on normalized leading text above this
    /// crate (STT-09); reusing the id here instead would append the replayed
    /// text to the utterance it duplicates.
    pub fn reconnected_at(&mut self, leg_ms: u64) -> Option<TranscriptSegment> {
        let dangling = self.finish();
        self.clock.reconnected_at(leg_ms);
        // The estimator works in session milliseconds — everything it is
        // compared against does — so it takes the anchored position rather than
        // the leg-relative one it was handed.
        self.estimator.reconnected_at(self.clock.to_session_ms(0));
        dangling
    }

    fn normalize_word(&mut self, word: &DeepgramWord) -> Word {
        let start_ms = self.clock.seconds_to_session_ms(word.start);
        Word {
            text: word.display_text().to_string(),
            start_ms,
            // Clamped so an inverted range from the provider cannot underflow
            // the word's duration.
            end_ms: self.clock.seconds_to_session_ms(word.end).max(start_ms),
            confidence: word.confidence,
            speaker: self.speakers.normalize_index(word.speaker),
        }
    }

    /// The segment's span: word timings first, then the chunk's own
    /// `start`/`duration`, and only then our own clock.
    fn span_for(
        &mut self,
        message: &DeepgramResult,
        words: &[Word],
        arrival_session_ms: Option<u64>,
    ) -> TimeSpan {
        let all_words = self.committed.words.iter().chain(words.iter());
        let bounds = all_words.fold(None::<(u64, u64)>, |bounds, word| match bounds {
            None => Some((word.start_ms, word.end_ms)),
            Some((start, end)) => Some((start.min(word.start_ms), end.max(word.end_ms))),
        });

        if let Some((start_ms, end_ms)) = bounds {
            self.estimator.advance_to(end_ms);
            return TimeSpan::provided(start_ms, end_ms);
        }

        if let Some(start_seconds) = message.start {
            let start_ms = self.clock.seconds_to_session_ms(start_seconds);
            let end_ms = start_ms + seconds_to_ms(message.duration.unwrap_or(0.0));
            self.estimator.advance_to(end_ms);
            return TimeSpan::provided(start_ms, end_ms);
        }

        // Nothing to go on but our own audio clock. Rule 3.
        match arrival_session_ms {
            Some(arrival) => self.estimator.next_span(arrival),
            None => {
                let at = self.estimator.last_end_ms();
                TimeSpan::estimated(at, at)
            }
        }
    }

    fn build_segment(
        &self,
        id: String,
        revision: u32,
        is_final: bool,
        alternative: &DeepgramAlternative,
        words: &[Word],
        span: TimeSpan,
    ) -> TranscriptSegment {
        let text = join_text(&self.committed.text, alternative.transcript.trim());
        let all_words: Vec<Word> = self
            .committed
            .words
            .iter()
            .cloned()
            .chain(words.iter().cloned())
            .collect();
        let speaker = self.segment_speaker(&all_words);
        let confidence = self.segment_confidence(alternative, words.len());

        TranscriptSegment {
            id,
            session_id: self.config.session_id.clone(),
            source: self.config.source,
            speaker,
            text,
            start_ms: span.start_ms,
            end_ms: span.end_ms,
            words: all_words,
            confidence,
            language: self.config.language.clone(),
            is_final,
            revision,
            provider: PROVIDER.to_string(),
            model: self.config.model.clone(),
            timestamp_source: span.source,
        }
    }

    /// The segment's speaker, or `None` when its words disagree.
    ///
    /// Deepgram can put two speakers inside one alternative. Picking either
    /// would attribute somebody's words to the wrong person, which is worse than
    /// admitting we do not know — the label is what "rename once, apply
    /// retroactively" (STT-15) acts on.
    fn segment_speaker(&self, words: &[Word]) -> Option<String> {
        if self.speakers.is_forced_me() {
            return Some(crate::speaker::ME.to_string());
        }

        let mut labelled = words.iter().filter_map(|word| word.speaker.as_deref());
        let first = labelled.next()?;
        if labelled.all(|speaker| speaker == first) {
            Some(first.to_string())
        } else {
            None
        }
    }

    /// Word-count-weighted mean confidence across the chunks of the utterance.
    ///
    /// Rounded to four decimals: it is display data, and unrounded floating
    /// point makes golden files unwritable and diffs unreadable.
    fn segment_confidence(
        &self,
        alternative: &DeepgramAlternative,
        word_count: usize,
    ) -> Option<f64> {
        let chunk_confidence = alternative.confidence?;
        let chunk_weight = weight_of(word_count);
        let sum = self.committed.confidence_sum + chunk_confidence * chunk_weight;
        let weight = self.committed.confidence_weight + chunk_weight;
        if weight == 0.0 {
            return Some(round4(chunk_confidence));
        }
        Some(round4(sum / weight))
    }

    fn commit(&mut self, alternative: &DeepgramAlternative, words: Vec<Word>) {
        let weight = weight_of(words.len());
        if let Some(confidence) = alternative.confidence {
            self.committed.confidence_sum += confidence * weight;
            self.committed.confidence_weight += weight;
        }
        self.committed.text = join_text(&self.committed.text, alternative.transcript.trim());
        self.committed.words.extend(words);
    }

    /// Whether an utterance is currently open.
    #[must_use]
    pub fn has_open_utterance(&self) -> bool {
        !self.committed.is_empty() || self.open.is_some()
    }
}

impl DeepgramResult {
    /// The top-ranked hypothesis, if this frame is a `Results` frame at all.
    ///
    /// `Metadata`, `SpeechStarted` and `UtteranceEnd` arrive on the same socket
    /// and are not transcripts.
    #[must_use]
    pub fn primary_alternative(&self) -> Option<&DeepgramAlternative> {
        if let Some(message_type) = &self.message_type
            && message_type != "Results"
        {
            return None;
        }
        self.channel.as_ref()?.alternatives.first()
    }
}

fn join_text(committed: &str, chunk: &str) -> String {
    match (committed.is_empty(), chunk.is_empty()) {
        (true, _) => chunk.to_string(),
        (false, true) => committed.to_string(),
        (false, false) => format!("{committed} {chunk}"),
    }
}

/// A chunk with no words still counts once, or its confidence would vanish.
fn weight_of(word_count: usize) -> f64 {
    word_count.max(1) as f64
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}
