//! The provider capability descriptor (spec 7.3, STT-02).
//!
//! The UI reads this to grey out diarization- and timestamp-dependent features
//! rather than discovering at runtime that the provider does not have them. It
//! is therefore a UI contract, and its wire shape matches the TypeScript type in
//! the spec.

use serde::{Deserialize, Serialize};

/// Which modes a feature is available in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeatureAvailability {
    /// Streaming and batch.
    Both,
    /// Streaming only.
    Streaming,
    /// Batch only. OpenAI's word timestamps, for one.
    Batch,
    /// Not available at all.
    None,
}

impl FeatureAvailability {
    /// Whether the feature is available on a live stream.
    #[must_use]
    pub fn in_streaming(&self) -> bool {
        matches!(self, Self::Both | Self::Streaming)
    }

    /// Whether the feature is available on a batch transcription.
    #[must_use]
    pub fn in_batch(&self) -> bool {
        matches!(self, Self::Both | Self::Batch)
    }
}

/// How the provider takes custom vocabulary (STT-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustomVocabulary {
    /// Deepgram `keyterm=` on nova-3.
    Keyterm,
    /// A keyword list.
    Keywords,
    /// Free-text prompt biasing, as OpenAI takes.
    Prompt,
    /// Unsupported.
    None,
}

/// How the provider lets us opt out of training/abuse retention (spec 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetentionControl {
    /// A request parameter, e.g. Deepgram `mip_opt_out=true`.
    Param,
    /// A request header.
    Header,
    /// Contract only — ElevenLabs' `enable_logging=false` is enterprise-gated,
    /// which is a materially weaker guarantee and must be surfaced as such.
    Contract,
    /// No control offered.
    None,
}

/// What a provider can and cannot do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttCapabilities {
    /// Live streaming transcription.
    pub streaming: bool,
    /// Whole-file transcription.
    pub batch: bool,
    /// Per-word start/end times.
    pub word_timestamps: FeatureAvailability,
    /// Speaker labels.
    pub diarization: FeatureAvailability,
    /// Automatic language identification.
    pub language_detection: bool,
    /// Custom vocabulary mechanism.
    pub custom_vocabulary: CustomVocabulary,
    /// Largest accepted batch upload, in bytes.
    pub max_file_bytes: u64,
    /// Longest accepted batch upload, in seconds.
    pub max_file_seconds: u64,
    /// Sample rates the provider takes without us resampling.
    pub native_rates: Vec<u32>,
    /// How retention opt-out is expressed.
    pub retention_control: RetentionControl,
    /// Whether the provider accepts audio faster than wall-clock.
    ///
    /// Decides the stall-recovery strategy (STT-09). Replaying a 30 s PCM ring
    /// into a fresh connection only closes the gap if the provider will swallow
    /// it faster than realtime; if it will not, recovery takes as long as the
    /// outage did and the honest fallback is to re-transcribe from the local
    /// recording afterwards (STT-10).
    pub supports_replay_faster_than_realtime: bool,
}

impl SttCapabilities {
    /// Whether a stalled stream can actually be caught up by replaying PCM.
    ///
    /// Needs both a stream to replay into and a provider that will take the
    /// audio faster than wall-clock.
    #[must_use]
    pub fn can_replay_to_catch_up(&self) -> bool {
        self.streaming && self.supports_replay_faster_than_realtime
    }

    /// Deepgram `nova-3` (spec 7.1, 7.4).
    ///
    /// `max_file_seconds` is derived, not documented: Deepgram publishes a 2 GB
    /// pre-recorded upload cap and no duration cap, so this is that cap
    /// expressed in seconds of our canonical 16 kHz 16-bit mono PCM
    /// (2e9 / 32000 = 62_500 s, about 17 hours).
    #[must_use]
    pub fn deepgram() -> Self {
        Self {
            streaming: true,
            batch: true,
            word_timestamps: FeatureAvailability::Both,
            // Available in both modes, but not the same model: `diarize_model=v1`
            // on streaming, v2 batch-only. v2 on a stream is a validation error.
            diarization: FeatureAvailability::Both,
            language_detection: true,
            custom_vocabulary: CustomVocabulary::Keyterm,
            max_file_bytes: 2_000_000_000,
            max_file_seconds: 62_500,
            native_rates: vec![8_000, 16_000, 24_000, 48_000],
            retention_control: RetentionControl::Param,
            supports_replay_faster_than_realtime: true,
        }
    }
}
