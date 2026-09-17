//! Speaker label normalization (spec 7.2 rule 2).
//!
//! Provider speaker ids are opaque, provider-specific, and not guaranteed to be
//! dense or to start at zero. Everything above this crate — rename-once-apply-
//! retroactively (STT-15), the augment prompt, the Markdown export — assumes
//! `S0..Sn`, so the mapping is done here exactly once.

use std::collections::HashMap;

/// The reserved label for the local microphone stream.
///
/// Downstream matches on this literally to build the me-vs-them split that the
/// two-stream capture default (spec 7.5) pays double STT cost for.
pub const ME: &str = "me";

/// Maps opaque provider speaker ids onto dense `S0..Sn` labels.
///
/// Assignment is by first appearance, which makes the labels stable for a given
/// input stream and reproducible in golden tests.
///
/// Scope note: this is stable **within a connection**. Provider speaker ids reset
/// on reconnect, so re-anchoring the registry across a reconnect using the
/// mic/system channel split as a prior is STT-15, and is not implemented here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeakerRegistry {
    /// Provider labels in first-appearance order; the index is the `S` number.
    order: Vec<String>,
    /// Provider label to index, so lookup does not go quadratic on long meetings.
    index: HashMap<String, usize>,
}

impl SpeakerRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The normalized `Sn` label for a provider label, assigning one if new.
    pub fn label_for(&mut self, provider_label: &str) -> String {
        let next = self.order.len();
        let index = *self
            .index
            .entry(provider_label.to_string())
            .or_insert_with(|| {
                self.order.push(provider_label.to_string());
                next
            });
        format!("S{index}")
    }

    /// How many distinct speakers have been seen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether no speaker has been seen yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The provider labels in first-appearance order, i.e. `S0`, `S1`, …
    #[must_use]
    pub fn labels(&self) -> &[String] {
        &self.order
    }
}

/// Applies rule 2 for one capture stream.
///
/// Holds the stream's [`Source`](crate::Source) and whether diarization is
/// enabled on it, because those two together decide whether a provider label is
/// even consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerNormalizer {
    source: crate::Source,
    diarization_enabled: bool,
    registry: SpeakerRegistry,
}

impl SpeakerNormalizer {
    /// A normalizer for one capture stream.
    #[must_use]
    pub fn new(source: crate::Source, diarization_enabled: bool) -> Self {
        Self {
            source,
            diarization_enabled,
            registry: SpeakerRegistry::new(),
        }
    }

    /// Normalize a provider speaker label.
    ///
    /// Returns `Some("me")` for an undiarized mic stream regardless of what the
    /// provider said, `Some("Sn")` when a label is present, and `None` when the
    /// provider reports no speaker — `None` is never invented into `S0`, because
    /// "unknown speaker" and "the first speaker" are different facts.
    pub fn normalize(&mut self, provider_label: Option<&str>) -> Option<String> {
        if self.is_forced_me() {
            return Some(ME.to_string());
        }
        provider_label.map(|label| self.registry.label_for(label))
    }

    /// Normalize a numeric provider speaker id, as Deepgram sends.
    ///
    /// Goes through the same registry as [`normalize`](Self::normalize) so `0`
    /// and `"0"` can never become two different speakers.
    pub fn normalize_index(&mut self, provider_index: Option<u32>) -> Option<String> {
        if self.is_forced_me() {
            return Some(ME.to_string());
        }
        let label = provider_index?.to_string();
        Some(self.registry.label_for(&label))
    }

    /// Whether this stream's labels are forced to `me` by rule 2.
    #[must_use]
    pub fn is_forced_me(&self) -> bool {
        self.source == crate::Source::Mic && !self.diarization_enabled
    }

    /// The stream this normalizer belongs to.
    #[must_use]
    pub fn source(&self) -> crate::Source {
        self.source
    }

    /// Whether diarization is enabled on this stream.
    #[must_use]
    pub fn diarization_enabled(&self) -> bool {
        self.diarization_enabled
    }

    /// The underlying registry, for inspection and for STT-15 re-anchoring.
    #[must_use]
    pub fn registry(&self) -> &SpeakerRegistry {
        &self.registry
    }
}
