//! The shared error taxonomy (spec 4.2, STT-12).
//!
//! Every adapter maps its provider's failures onto one of ten classes, and the
//! supervisor above reacts to the class rather than to a provider-specific
//! status code. That mapping is the whole point: the failover chain (STT-11)
//! must not need to know what a Deepgram `NET-0001` is.

use serde::{Deserialize, Serialize};

/// What the stream supervisor should do about an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailoverPolicy {
    /// Demote this provider and move down the failover chain (STT-11).
    Failover,
    /// Retry the same provider after a backoff delay.
    Backoff,
    /// Reconnect immediately and transparently. Expected, not a fault.
    Reconnect,
    /// Show the user; neither retrying nor failing over would help.
    Surface,
}

/// The ten error classes every adapter normalizes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttErrorClass {
    /// The key is missing, malformed, revoked, or lacks the needed scope.
    Auth,
    /// The account is out of credit or past a hard billing cap.
    Quota,
    /// Requests per unit time exceeded. Clears on its own.
    RateLimit,
    /// Too many simultaneous streams. Deepgram counts these per *project*, not
    /// per key, and the two-stream default consumes two per meeting.
    Concurrency,
    /// We sent something the provider rejected. Our bug.
    BadRequest,
    /// The provider does not offer what was asked for, e.g. streaming on a
    /// batch-only backend.
    Unsupported,
    /// Transport failed: DNS, TLS, reset, timeout, proxy.
    Network,
    /// The provider returned a 5xx or closed with an internal error.
    Server,
    /// The audio itself was rejected: wrong encoding, rate, or channel count.
    AudioFormat,
    /// The provider's own maximum session duration elapsed. Routine on long
    /// meetings; ElevenLabs guarantees it.
    SessionLimit,
}

impl SttErrorClass {
    /// Every class, in spec order.
    ///
    /// Exhaustive matches below plus this constant mean a class added later
    /// cannot silently inherit somebody else's policy.
    pub const ALL: [Self; 10] = [
        Self::Auth,
        Self::Quota,
        Self::RateLimit,
        Self::Concurrency,
        Self::BadRequest,
        Self::Unsupported,
        Self::Network,
        Self::Server,
        Self::AudioFormat,
        Self::SessionLimit,
    ];

    /// What the supervisor should do (STT-12).
    ///
    /// **Only `auth` and `quota` trigger failover.** Everything else either
    /// clears on its own or would fail identically on the next provider.
    ///
    /// Note the tension with STT-11's "demote on … a non-retryable error": a
    /// non-retryable `bad_request` is our own defect, and demoting on it would
    /// quietly move the user to a worse provider to hide our bug. STT-12 is the
    /// narrower and more specific rule, so it wins here; the supervisor may
    /// still demote after a `Surface` error has been reported, but that is its
    /// decision, not this function's.
    #[must_use]
    pub fn failover_policy(&self) -> FailoverPolicy {
        match self {
            // The key is bad or the money is gone. No amount of retrying fixes
            // either, and the next provider in the chain has its own key.
            Self::Auth | Self::Quota => FailoverPolicy::Failover,

            // Self-clearing pressure. Back off and come back.
            Self::RateLimit | Self::Concurrency | Self::Network | Self::Server => {
                FailoverPolicy::Backoff
            }

            // Expected on any long meeting. Reconnect without penalty.
            Self::SessionLimit => FailoverPolicy::Reconnect,

            // Our request or our audio is wrong. Every provider rejects it the
            // same way, so failing over would only hide the defect.
            Self::BadRequest | Self::Unsupported | Self::AudioFormat => FailoverPolicy::Surface,
        }
    }

    /// Whether retrying the same provider can succeed.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::RateLimit
            | Self::Concurrency
            | Self::Network
            | Self::Server
            | Self::SessionLimit => true,
            Self::Auth | Self::Quota | Self::BadRequest | Self::Unsupported | Self::AudioFormat => {
                false
            }
        }
    }

    /// Whether this error should count toward the reconnect-failure budget that
    /// STT-11 demotes on.
    ///
    /// A session limit is the provider working as documented, so it must not.
    /// Otherwise any meeting long enough to hit the limit three times would
    /// demote a healthy provider on a timer.
    #[must_use]
    pub fn counts_against_failure_budget(&self) -> bool {
        !matches!(self, Self::SessionLimit)
    }

    /// A short, provider-neutral explanation for the UI.
    ///
    /// Adapters supply the provider's own wording in
    /// [`SttError::message`]; this is the fallback when that wording is jargon.
    #[must_use]
    pub fn user_hint(&self) -> &'static str {
        match self {
            Self::Auth => "The API key was rejected. Check it in Settings.",
            Self::Quota => "This account is out of transcription credit.",
            Self::RateLimit => "The provider is rate-limiting us. Retrying shortly.",
            Self::Concurrency => {
                "Too many transcriptions running at once on this account. Retrying shortly."
            }
            Self::BadRequest => "The transcription request was rejected. This is a bug in the app.",
            Self::Unsupported => "This provider does not support that.",
            Self::Network => "Could not reach the transcription provider.",
            Self::Server => "The transcription provider had an internal error.",
            Self::AudioFormat => "The provider rejected the audio format.",
            Self::SessionLimit => "The provider's session length limit was reached. Reconnecting.",
        }
    }
}

/// A normalized transcription error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{provider}: {message}")]
pub struct SttError {
    /// Which class of failure this is.
    pub class: SttErrorClass,
    /// The provider that produced it, e.g. `deepgram`.
    pub provider: String,
    /// A user-facing message. Adapters put the provider's own wording here when
    /// it is intelligible, and [`SttErrorClass::user_hint`] when it is not.
    pub message: String,
    /// Whether retrying this provider can succeed.
    ///
    /// Seeded from the class. An adapter may narrow it (see
    /// [`not_retryable`](Self::not_retryable)) but never widen it, and it never
    /// affects [`failover_policy`](Self::failover_policy).
    pub retryable: bool,
    /// How long the provider asked us to wait, when it said.
    ///
    /// `None` means it did not say — the caller uses its own backoff schedule
    /// rather than inventing a number.
    pub retry_after_ms: Option<u64>,
    /// Diagnostic detail for the log. Not shown to the user.
    pub detail: Option<String>,
}

impl SttError {
    /// A new error with `retryable` seeded from the class.
    #[must_use]
    pub fn new(
        class: SttErrorClass,
        provider: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            class,
            provider: provider.into(),
            message: message.into(),
            retryable: class.is_retryable(),
            retry_after_ms: None,
            detail: None,
        }
    }

    /// An error whose message is the class's generic hint.
    #[must_use]
    pub fn from_class(class: SttErrorClass, provider: impl Into<String>) -> Self {
        Self::new(class, provider, class.user_hint())
    }

    /// Record the provider's requested wait.
    #[must_use]
    pub fn with_retry_after_ms(mut self, retry_after_ms: u64) -> Self {
        self.retry_after_ms = Some(retry_after_ms);
        self
    }

    /// Attach diagnostic detail for the log.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Mark an ordinarily-retryable error as permanent.
    ///
    /// For cases the class cannot express — a 500 that is really a retired model
    /// id. Narrowing only: this cannot make a non-retryable class retryable, and
    /// it does not change the failover policy.
    #[must_use]
    pub fn not_retryable(mut self, detail: impl Into<String>) -> Self {
        self.retryable = false;
        self.detail = Some(detail.into());
        self
    }

    /// What the supervisor should do. Determined by the class alone.
    #[must_use]
    pub fn failover_policy(&self) -> FailoverPolicy {
        self.class.failover_policy()
    }

    /// Whether this should count toward the STT-11 demotion budget.
    #[must_use]
    pub fn counts_against_failure_budget(&self) -> bool {
        self.class.counts_against_failure_budget()
    }
}
