//! Whole-utterance arbitration for explicitly selected cloud-to-local dictation.
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64},
};
use std::time::Duration;
use tokio::sync::mpsc;

/// Per-attempt progress; each capture owns a fresh instance so late updates
/// cannot overwrite a later dictation's UI.
#[derive(Clone, Default)]
pub struct Progress(Arc<AtomicU64>);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Preparing,
    Cloud,
    Retrying,
    Local,
    /// Local engine text gets this many vocabulary correction targets (#102).
    LocalCorrections(u32),
}
impl Progress {
    pub fn stage(&self) -> Stage {
        let packed = self.0.load(std::sync::atomic::Ordering::Acquire);
        match packed & 0xff {
            1 => Stage::Cloud,
            2 => Stage::Retrying,
            3 => Stage::Local,
            4 => Stage::LocalCorrections((packed >> 8) as u32),
            _ => Stage::Preparing,
        }
    }
    pub(crate) fn set(&self, stage: Stage) {
        let packed = match stage {
            Stage::Preparing => 0,
            Stage::Cloud => 1,
            Stage::Retrying => 2,
            Stage::Local => 3,
            Stage::LocalCorrections(count) => 4 | u64::from(count) << 8,
        };
        self.0.store(packed, std::sync::atomic::Ordering::Release);
    }
}
impl Stage {
    /// Keep local vocabulary behavior visible even when a fast local result
    /// completes between two native UI progress polls.
    pub fn completion_message(self, message: &str) -> String {
        if let Self::LocalCorrections(_) = self {
            format!("{message} · {}", self.label())
        } else {
            message.to_owned()
        }
    }
    pub fn label(self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Preparing => "Checking speech engine".into(),
            Self::Cloud => "Deepgram".into(),
            Self::Retrying => "Retrying Deepgram".into(),
            Self::Local => "Local Whisper".into(),
            Self::LocalCorrections(count) => format!(
                "Local: {count} vocabulary correction{} available",
                if count == 1 { "" } else { "s" }
            )
            .into(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Deadlines {
    pub connect: Duration,
    pub cloud_final: Duration,
    pub local: Duration,
}
impl Default for Deadlines {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            cloud_final: Duration::from_secs(3),
            local: Duration::from_secs(180),
        }
    }
}
pub async fn transcribe(
    key: String,
    audio: mpsc::Receiver<Vec<i16>>,
    cloud: fotw_stt::DeepgramEndpoint,
    local: crate::local_stt::LocalEndpoint,
    control: Arc<AtomicU8>,
    deadlines: Deadlines,
) -> Result<String, String> {
    transcribe_observed(
        key,
        audio,
        cloud,
        local,
        control,
        deadlines,
        Progress::default(),
    )
    .await
}

pub async fn transcribe_observed(
    key: String,
    audio: mpsc::Receiver<Vec<i16>>,
    cloud: fotw_stt::DeepgramEndpoint,
    local: crate::local_stt::LocalEndpoint,
    control: Arc<AtomicU8>,
    deadlines: Deadlines,
    progress: Progress,
) -> Result<String, String> {
    transcribe_with_vocabulary(key, audio, cloud, local, control, deadlines, progress, &[]).await
}

#[allow(
    clippy::too_many_arguments,
    reason = "preserve the injected transport/deadline seam while sharing immutable utterance hints"
)]
pub async fn transcribe_with_vocabulary(
    key: String,
    audio: mpsc::Receiver<Vec<i16>>,
    cloud: fotw_stt::DeepgramEndpoint,
    local: crate::local_stt::LocalEndpoint,
    control: Arc<AtomicU8>,
    deadlines: Deadlines,
    progress: Progress,
    hints: &[String],
) -> Result<String, String> {
    let profile = crate::vocabulary::Profile {
        recognition_hints: hints.to_vec(),
        ..Default::default()
    };
    transcribe_with_profile(
        key, audio, cloud, local, control, deadlines, progress, &profile,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "attempt-owned transport and immutable recognition profile"
)]
pub async fn transcribe_with_profile(
    key: String,
    mut audio: mpsc::Receiver<Vec<i16>>,
    cloud: fotw_stt::DeepgramEndpoint,
    local: crate::local_stt::LocalEndpoint,
    control: Arc<AtomicU8>,
    deadlines: Deadlines,
    progress: Progress,
    profile: &crate::vocabulary::Profile,
) -> Result<String, String> {
    profile.validate()?;
    crate::language::validate_deepgram(&profile.language)?;
    if local.language() != profile.language {
        return Err("Local language preflight does not match this utterance".into());
    }
    progress.set(Stage::Cloud);
    use std::sync::atomic::Ordering;
    let (tx, rx) = mpsc::channel(1024);
    let mut sender = Some(tx);
    let mut remote = Some(Box::pin(crate::streaming::transcribe_with_profile(
        key.clone(),
        rx,
        cloud.clone(),
        deadlines.connect,
        deadlines.cloud_final,
        profile,
    )));
    let mut pcm = Vec::new();
    let mut ended = false;
    let mut final_started = None;
    let mut retry_at = None;
    let mut retried = false;
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    let corrections = crate::vocabulary::VocabularyMatcher::new(profile);
    loop {
        if control.load(Ordering::Acquire) == 2 {
            return Err("Cancelled".into());
        }
        if remote.is_none() && retry_at.is_none() {
            progress.set(local_vocabulary_stage(&corrections));
            if ended {
                break;
            }
        }
        tokio::select! {
            biased;
            _ = tick.tick() => {
                if retry_at.is_some_and(|at| tokio::time::Instant::now() >= at) {
                    retry_at = None;
                    progress.set(Stage::Cloud);
                    let (tx, rx) = mpsc::channel(1024);
                    for chunk in pcm.chunks(16_000) {
                        tx.try_send(chunk.to_vec()).map_err(|_| "Cannot replay bounded dictation")?;
                    }
                    remote = Some(Box::pin(crate::streaming::transcribe_with_profile(
                        key.clone(), rx, cloud.clone(), deadlines.connect, deadlines.cloud_final, profile,
                    )));
                    sender = if ended { None } else { Some(tx) };
                }
                if final_started.is_some_and(|at: tokio::time::Instant| at.elapsed() >= deadlines.cloud_final) {
                    // Drop the future/socket before local inference. No detached cloud
                    // task can publish a late result or compete for insertion.
                    remote = None;
                    sender = None;
                    retry_at = None;
                }
            }
            result = async { remote.as_mut().expect("guarded remote").await }, if remote.is_some() => {
                remote = None;
                sender = None;
                if let Err(error) = &result
                    && error.failover_policy() == fotw_stt::FailoverPolicy::Surface
                { return Err(error.message.clone()); }
                if let Ok(text) = &result {
                    if control.load(Ordering::Acquire) == 2 { return Err("Cancelled".into()); }
                    if ended { return Ok(text.clone()); }
                    return Err("Provider completed before capture finished; no text inserted".into());
                }
                if let Err(error) = result
                    && error.retryable && !retried
                {
                    let wait = Duration::from_millis(error.retry_after_ms.unwrap_or(100).max(100));
                    // Never hammer a rate-limited provider. If its requested wait
                    // exceeds the retry budget, use local instead of retrying early.
                    if wait < deadlines.connect {
                        retry_at = Some(tokio::time::Instant::now() + wait);
                        retried = true;
                        progress.set(Stage::Retrying);
                    }
                }
                // Keep consuming microphone audio after failure, until release.
                // Local inference receives the whole utterance, not an uncertain
                // splice between cloud text and an unaligned audio tail.
            }
            chunk = audio.recv(), if !ended => match chunk {
                Some(chunk) => {
                    crate::capture::append_local_pcm(&mut pcm, chunk.clone());
                    if let Some(tx) = &sender && tx.try_send(chunk).is_err() {
                        // Slow cloud must not backpressure capture or lose the local
                        // recording. Demote and drain only into the bounded buffer.
                        remote = None;
                        sender = None;
                    }
                }
                None => {
                    ended = true;
                    sender = None;
                    final_started = Some(tokio::time::Instant::now());
                }
            }
        }
    }
    crate::local_stt::transcribe(&local, &pcm, control, deadlines.local)
        .await
        .map(|text| corrections.apply(&text))
        .map_err(|e| format!("Cloud transcription unavailable; local fallback: {e}"))
}

/// Local engines receive no recognition hints; their text is corrected from
/// the vocabulary afterwards instead. Cloud text never is: Deepgram already
/// received the hints as keyterms.
pub(crate) fn local_vocabulary_stage(corrections: &crate::vocabulary::VocabularyMatcher) -> Stage {
    match u32::try_from(corrections.len()) {
        Ok(0) => Stage::Local,
        Ok(count) => Stage::LocalCorrections(count),
        Err(_) => Stage::LocalCorrections(u32::MAX),
    }
}
