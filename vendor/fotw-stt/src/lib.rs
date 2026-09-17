//! `fotw-stt` — the speech-to-text provider abstraction.
//!
//! This crate owns the canonical internal transcript format (spec 7.2) that
//! every provider adapter normalizes into, the capability descriptor the UI
//! reads to hide unsupported affordances (spec 7.3, STT-02), the shared error
//! taxonomy (STT-12), and the provider response normalizers.
//!
//! The crate splits along one seam, and the seam is load-bearing.
//!
//! **The normalization layer** — [`transcript`], [`clock`], [`speaker`],
//! [`store`], [`capabilities`], [`error`] and the provider normalizers such as
//! [`deepgram`] — is pure data and logic: no sockets, no async runtime, no clock
//! reads. That is what makes every rule in spec 7.2 testable from a checked-in
//! JSON fixture, with no network and no secrets.
//!
//! **The transport layer** — [`deepgram_stream`], plus the [`backoff`],
//! [`replay`], [`dedupe`] and [`stream`] pieces it is built from — owns the
//! WebSocket, the KeepAlive timer, the 30-second replay ring and the reconnect
//! loop. It is async, and it is tested against a local mock WebSocket server
//! rather than against a provider, for the same reason: CI has no API keys.
//!
//! Every piece of transport logic that can be pure is pure. The backoff
//! schedule takes its jitter draw and its clock reading as arguments, the ring
//! is an ordinary data structure, and the dedupe is a string function — so the
//! parts of STT-09 that are easy to get subtly wrong are assertable without a
//! runtime, and only the socket plumbing needs one.
//!
//! See `docs/REQUIREMENTS.md` §7.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backoff;
pub mod capabilities;
pub mod clock;
pub mod dedupe;
pub mod deepgram;
pub mod deepgram_stream;
pub mod deepgram_wire;
pub mod error;
pub mod replay;
pub mod speaker;
pub mod store;
pub mod stream;
pub mod transcript;

pub use backoff::{BackoffPolicy, FixedJitter, Jitter, ProcessJitter, ReconnectBudget};
pub use capabilities::{CustomVocabulary, FeatureAvailability, RetentionControl, SttCapabilities};
pub use clock::{ArrivalEstimator, SessionClock, TimeSpan, seconds_to_ms};
pub use dedupe::{TranscriptTail, normalize_tokens, trim_leading_tokens};
pub use deepgram_stream::{DeepgramStream, DeepgramStreamConfig, map_transport_error};
pub use deepgram_wire::{DeepgramEndpoint, DeepgramStreamParams, map_close, map_http_status};
pub use error::{FailoverPolicy, SttError, SttErrorClass};
pub use replay::{PcmRing, Replay, to_linear16_le};
pub use speaker::{SpeakerNormalizer, SpeakerRegistry};
pub use store::{
    CountingIdFactory, SegmentIdFactory, SegmentStore, StoreOutcome, UlidFactory, UtteranceTracker,
};
pub use stream::{StreamEvent, StreamState};
pub use transcript::{Source, TimestampSource, TranscriptSegment, Word};
