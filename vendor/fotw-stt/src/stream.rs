//! The `SttStream` event surface (spec 7.3).
//!
//! The interface in the spec is an `EventEmitter` with four channels —
//! `partial`, `final`, `error`, `state`. Rust has no such thing, so the four
//! collapse into one [`StreamEvent`] enum delivered on a single channel. That is
//! not just an encoding convenience: a single ordered channel is what makes
//! "this partial arrived *before* the reconnect" a fact the UI can rely on,
//! where four independent emitters would let a `state` event overtake the
//! transcript it explains.
//!
//! [`StreamState`] exists so the UI can show a subtle "reconnecting" indicator
//! instead of an error. A dropped socket on a two-hour meeting is expected
//! traffic (STT-09), and presenting it as a failure trains users to distrust a
//! recorder that is actually working.

use serde::{Deserialize, Serialize};

use crate::{SttError, TranscriptSegment};

/// Where a stream is in its lifecycle.
///
/// Wire shape is lowercase to match the TypeScript union in spec 7.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamState {
    /// The first socket has not been established yet.
    Connecting,
    /// The socket is up and audio is flowing.
    Open,
    /// The socket dropped and we are backing off before trying again. Audio is
    /// still being buffered into the replay ring, so nothing is lost yet.
    Reconnecting,
    /// Terminal. No further events will arrive on this stream.
    Closed,
}

impl StreamState {
    /// Whether this is the terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed)
    }

    /// The lowercase tag used on the wire and in logs.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Open => "open",
            Self::Reconnecting => "reconnecting",
            Self::Closed => "closed",
        }
    }
}

/// One event from a live transcription stream.
///
/// `Error` is not necessarily terminal: a malformed frame or a dropped socket
/// produces an error *and* the stream keeps going. Callers decide what to do
/// from [`SttError::failover_policy`], and learn that the stream is really over
/// only from [`StreamState::Closed`].
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A revision of an utterance still in progress.
    Partial(TranscriptSegment),
    /// The settled text of an utterance. No further revision of this id will
    /// arrive.
    Final(TranscriptSegment),
    /// A normalized failure. See [`SttError::failover_policy`].
    Error(SttError),
    /// A lifecycle transition.
    State(StreamState),
}

impl StreamEvent {
    /// The segment, for `Partial` and `Final`.
    #[must_use]
    pub fn segment(&self) -> Option<&TranscriptSegment> {
        match self {
            Self::Partial(segment) | Self::Final(segment) => Some(segment),
            _ => None,
        }
    }

    /// The segment, but only if this is a `Final`.
    #[must_use]
    pub fn final_segment(&self) -> Option<&TranscriptSegment> {
        match self {
            Self::Final(segment) => Some(segment),
            _ => None,
        }
    }

    /// The error, if this is an `Error`.
    #[must_use]
    pub fn error(&self) -> Option<&SttError> {
        match self {
            Self::Error(error) => Some(error),
            _ => None,
        }
    }

    /// The state, if this is a `State`.
    #[must_use]
    pub fn state(&self) -> Option<StreamState> {
        match self {
            Self::State(state) => Some(*state),
            _ => None,
        }
    }
}
