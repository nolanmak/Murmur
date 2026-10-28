//! Local Parakeet (#101): Parakeet Unified 0.6B (English) through the shared
//! `fotw-speechd` helper, Murmur's default local engine.
//!
//! Murmur spawns and owns the helper (FlyOnTheWall #243, protocol in its
//! `docs/SPEECHD_PROTOCOL.md`), loads and warms both models when Parakeet is
//! chosen or the app starts with it, and sends each utterance as one batch
//! stream after release. Silero VAD in the same helper gates and trims the
//! utterance first, so silence and noise never reach the recognizer.
//!
//! - [`Service`] holds the load state and the policy (refuse before the mic
//!   opens, per-utterance binding, VAD trim, one line of text). It is tested
//!   with a fake [`Backend`].
//! - [`SpeechdBackend`] is the thin adapter onto `fotw-speechd-client`.
//! - Nothing here downloads during dictation. Models arrive only through
//!   [`Service::install`], from an explicit menu action or CLI command.
use std::future::Future;
use std::ops::Range;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::Shared;

/// The recognizer's engine id in the helper.
pub const ASR_ENGINE: &str = "parakeet-unified";
/// The speech gate's engine id in the helper.
pub const VAD_ENGINE: &str = "silero-vad";
/// How to install the models; shown wherever a missing model is refused.
pub const INSTALL_HINT: &str = "choose Transcription → Install Parakeet model… in the menu, or run `murmur speech install-parakeet`";
/// The attribution the licenses require, for About/notices.
pub const ATTRIBUTION: &str = "Parakeet Unified 0.6B (English) by NVIDIA, Core ML conversion FluidInference/parakeet-unified-en-0.6b-coreml, CC-BY-4.0 (https://creativecommons.org/licenses/by/4.0/). Silero VAD by Silero Team, Core ML conversion FluidInference/silero-vad-coreml, MIT. Run through FluidAudio (Apache-2.0) in fotw-speechd.";

const SAMPLE_RATE: usize = 16_000;
/// Keep this much audio around the detected speech, so trimming never clips
/// a soft first or last word.
const SPEECH_PAD: usize = SAMPLE_RATE * 3 / 10;
/// Same bound as the Whisper path: 120 s of capture plus resampler drain.
const MAX_SAMPLES: usize = SAMPLE_RATE * 120 + 3200;
/// The gate's bar for speech. Silero starts a region at 0.5, which is right
/// for where speech begins, but MUSAN noise and instrumental music reach
/// 0.54-0.87 for a step and Parakeet then hears "Uh" or "Okay."; real speech,
/// even a lone "Thank you.", peaks at 1.0 (#101 corpus run).
const SPEECH_PEAK: f32 = 0.9;
/// Audio frames to the helper: 256 ms, Silero's step.
const CHUNK: usize = 4096;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// What one utterance was checked against before the mic opened. Inference
/// must come from the same helper instance and model, or nothing is typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The helper process serving the model; a restart changes it.
    pub instance: u64,
    pub model: String,
    pub revision: String,
}
impl Binding {
    /// Whether a final's `model` names this binding. The helper always reports
    /// `<model>@<revision>`; anything else, a bare id included, is refused.
    pub fn produced(&self, model: &str) -> bool {
        model
            .split_once('@')
            .is_some_and(|(id, revision)| id == self.model && revision == self.revision)
    }
}

/// Why the engine cannot take dictation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// A model is not installed.
    Missing(String),
    /// An installed model fails the helper's checks.
    Invalid(String),
    /// The helper is not bundled or cannot run.
    Unavailable(String),
    /// Anything else, for example a helper crash during load.
    Failed(String),
}
impl LoadError {
    pub fn message(&self) -> String {
        match self {
            LoadError::Missing(model) => format!(
                "Local Parakeet is not installed ({model} model missing); {INSTALL_HINT}. Nothing was recorded"
            ),
            LoadError::Invalid(model) => format!(
                "Local Parakeet's {model} model failed verification; reinstall it: {INSTALL_HINT}. Nothing was recorded"
            ),
            LoadError::Unavailable(reason) | LoadError::Failed(reason) => {
                format!("Local Parakeet is unavailable: {reason}. Nothing was recorded")
            }
        }
    }
}

/// What the speech gate found in one utterance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detected {
    /// Speech regions, in samples from the first.
    pub regions: Vec<Range<usize>>,
    /// The highest speech probability of any 256 ms step.
    pub peak: f32,
}

/// The recognizer's answer for one utterance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recognized {
    pub text: String,
    /// The model that produced it, when the helper says.
    pub model: Option<String>,
}

/// The operations Murmur needs from the helper. The ordinary tests replace it
/// with a fake loader; [`SpeechdBackend`] is the real one.
pub trait Backend: Send + Sync + 'static {
    /// Start the helper if needed, then load and warm both engines from local
    /// files. Never downloads.
    fn load(&self) -> BoxFuture<Result<Binding, LoadError>>;
    /// The helper instance serving now, if one is running.
    fn instance(&self) -> Option<u64>;
    /// Where the speech is, and how sure the gate is of it.
    fn speech(&self, pcm: Arc<[i16]>) -> BoxFuture<Result<Detected, String>>;
    /// One batch stream over the whole (trimmed) utterance.
    fn recognize(&self, pcm: Arc<[i16]>) -> BoxFuture<Result<Recognized, String>>;
    /// Explicit setup: install both models, downloading unless a local copy
    /// is given, then drop any loaded copy so the next load reads the new one.
    fn install(&self, from: InstallSources, progress: Progress) -> BoxFuture<Result<(), String>>;
}

/// Local copies to import instead of downloading.
#[derive(Clone, Debug, Default)]
pub struct InstallSources {
    pub parakeet: Option<PathBuf>,
    pub vad: Option<PathBuf>,
}

/// Install progress, as short status lines.
pub type Progress = Arc<dyn Fn(String) + Send + Sync>;

type SharedLoad = Shared<BoxFuture<Result<Binding, LoadError>>>;

/// A runtime that outlives every dictation attempt. Each attempt runs on its
/// own short-lived runtime, but the helper's reader, writer and reaper tasks
/// must keep running between them.
struct Worker(Option<tokio::runtime::Runtime>);
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// Owns the load state for one backend.
pub struct Service {
    backend: Arc<dyn Backend>,
    worker: Worker,
    load: Mutex<Option<SharedLoad>>,
    installing: AtomicBool,
}
impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

/// Abort the spawned work when the caller gives up (Esc, deadline), so a
/// cancelled utterance's stream is cancelled in the helper too.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Service {
    pub fn new(backend: Arc<dyn Backend>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("murmur-speechd")
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the Local Parakeet runtime".to_string())?;
        Ok(Self {
            backend,
            worker: Worker(Some(runtime)),
            load: Mutex::new(None),
            installing: AtomicBool::new(false),
        })
    }

    fn handle(&self) -> &tokio::runtime::Handle {
        self.worker.0.as_ref().expect("live runtime").handle()
    }

    /// Run `work` on the long-lived runtime; dropping the returned future
    /// aborts it.
    fn run<T: Send + 'static>(
        &self,
        work: BoxFuture<T>,
    ) -> impl Future<Output = Result<T, String>> + Send + 'static {
        let task = AbortOnDrop(self.handle().spawn(work));
        async move {
            let mut task = task;
            (&mut task.0)
                .await
                .map_err(|_| "Local Parakeet stopped unexpectedly".to_string())
        }
    }

    /// The current load, starting one if none is running or the last failed.
    fn load(&self) -> SharedLoad {
        let mut load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        let reusable = load.as_ref().is_some_and(|current| match current.peek() {
            None => true,
            // Only for the helper instance it loaded into. A replacement helper
            // replays loads, but a replay that fails is dropped silently, and
            // a replay reads whatever revision is installed now; so load again
            // (idempotent in the helper) and bind to what that reports.
            Some(Ok(binding)) => self.backend.instance() == Some(binding.instance),
            // A failed load is retried, so installing a missing model, even
            // from the CLI while the app runs, needs no restart.
            Some(Err(_)) => false,
        });
        if !reusable {
            let work = self.run(self.backend.load());
            let shared: SharedLoad =
                async move { work.await.unwrap_or_else(|e| Err(LoadError::Failed(e))) }
                    .boxed()
                    .shared();
            // The load runs on the worker whether or not anyone awaits it, so
            // warming at launch needs no waiter.
            let poll = shared.clone();
            drop(self.handle().spawn(async move {
                let _ = poll.await;
            }));
            *load = Some(shared);
        }
        load.clone().expect("load just set")
    }

    /// Load and warm in the background, at launch or when Parakeet is chosen.
    pub fn warm(&self) {
        if !self.installing.load(Ordering::Acquire) {
            drop(self.load());
        }
    }

    /// The finished load's outcome, without starting one: `None` while it is
    /// running or before the first.
    pub fn state(&self) -> Option<Result<Binding, LoadError>> {
        let load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        load.as_ref().and_then(|l| l.peek().cloned())
    }

    /// Check before the mic opens: the models are installed, valid, loaded and
    /// warm. Waits at most `deadline` for a load in progress.
    pub async fn ready(&self, deadline: Duration) -> Result<Binding, String> {
        if self.installing.load(Ordering::Acquire) {
            return Err(
                "Local Parakeet's model is installing; dictate when it finishes. Nothing was recorded"
                    .into(),
            );
        }
        let load = self.load();
        let binding = match tokio::time::timeout(deadline, load).await {
            Ok(Ok(binding)) => binding,
            Ok(Err(error)) => return Err(error.message()),
            Err(_) => {
                return Err(
                    "Local Parakeet is still loading its model; try again in a moment. Nothing was recorded"
                        .into(),
                );
            }
        };
        // A load from an instance that has since been replaced proves nothing
        // about the one serving now; the next attempt loads into it.
        if self.backend.instance() != Some(binding.instance) {
            return Err(
                "Local Parakeet's helper restarted; it is reloading, so try again. Nothing was recorded"
                    .into(),
            );
        }
        Ok(binding)
    }

    /// Why no dictation may start now, with any engine: the model install
    /// the menu promised dictation would wait for.
    pub fn dictation_hold(&self) -> Option<&'static str> {
        self.installing.load(Ordering::Acquire).then_some(
            "The Parakeet model install is running; dictate when it finishes. Nothing was recorded",
        )
    }

    /// Explicit setup only; never called during dictation.
    pub async fn install(&self, from: InstallSources, progress: Progress) -> Result<(), String> {
        if self.installing.swap(true, Ordering::AcqRel) {
            return Err("A Parakeet model install is already running".into());
        }
        let result = self
            .run(self.backend.install(from, progress))
            .await
            .and_then(|r| r);
        *self.load.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.installing.store(false, Ordering::Release);
        if result.is_ok() {
            self.warm();
        }
        result
    }

    /// Transcribe one utterance bound at [`ready`](Self::ready).
    async fn transcribe(&self, binding: &Binding, pcm: &[i16]) -> Result<String, String> {
        const CHANGED: &str =
            "Local Parakeet restarted or changed model after the mic opened; no text inserted";
        if self.backend.instance() != Some(binding.instance) {
            return Err(CHANGED.into());
        }
        let pcm: Arc<[i16]> = pcm.into();
        let detected = self
            .run(self.backend.speech(pcm.clone()))
            .await?
            .map_err(|e| format!("Local Parakeet speech gate failed: {e}; no text inserted"))?;
        // Silence, noise and music without speech never reach the recognizer.
        if detected.peak < SPEECH_PEAK {
            return Ok(String::new());
        }
        let Some(span) = speech_span(&detected.regions, pcm.len()) else {
            return Ok(String::new());
        };
        let trimmed: Arc<[i16]> = if span == (0..pcm.len()) {
            pcm
        } else {
            pcm[span].into()
        };
        let recognized = self
            .run(self.backend.recognize(trimmed))
            .await?
            .map_err(|e| format!("Local Parakeet failed: {e}; no text inserted"))?;
        if self.backend.instance() != Some(binding.instance)
            || !recognized
                .model
                .as_deref()
                .is_some_and(|model| binding.produced(model))
        {
            return Err(CHANGED.into());
        }
        Ok(one_line(&recognized.text))
    }
}

/// From the first speech to the last, padded and clamped. `None` when no
/// region holds any sample.
fn speech_span(regions: &[Range<usize>], len: usize) -> Option<Range<usize>> {
    let start = regions
        .iter()
        .filter(|r| r.start < r.end && r.start < len)
        .map(|r| r.start)
        .min()?;
    let end = regions
        .iter()
        .filter(|r| r.start < r.end && r.start < len)
        .map(|r| r.end.min(len))
        .max()?;
    Some(start.saturating_sub(SPEECH_PAD)..(end + SPEECH_PAD).min(len))
}

/// Segment breaks are not the speaker's formatting and would stop the text
/// being typed (#98). `<unk>` is the recognizer's unknown-token marker, not
/// text.
fn one_line(text: &str) -> String {
    text.replace("<unk>", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The local transcriber for one utterance: the shared service plus what
/// preflight bound it to.
#[derive(Clone)]
pub struct Endpoint {
    service: Arc<Service>,
    binding: Option<Binding>,
}
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

static SHARED: OnceLock<Arc<Service>> = OnceLock::new();

impl Endpoint {
    pub fn new(service: Arc<Service>) -> Self {
        Self {
            service,
            binding: None,
        }
    }

    /// The app's one helper-backed service, created on first use. Creating it
    /// starts nothing; the helper is spawned by the first load.
    pub fn shared() -> Self {
        Self::new(Self::shared_service())
    }

    fn shared_service() -> Arc<Service> {
        SHARED
            .get_or_init(|| {
                let backend = Arc::new(SpeechdBackend::from_environment());
                // Building a one-thread runtime fails only if no thread can be
                // spawned; the dictation worker contains that panic.
                Arc::new(Service::new(backend).expect("Local Parakeet runtime"))
            })
            .clone()
    }

    pub fn service(&self) -> &Arc<Service> {
        &self.service
    }

    /// Parakeet Unified takes no recognition hints.
    pub fn vocabulary_capability(&self) -> fotw_stt::CustomVocabulary {
        fotw_stt::CustomVocabulary::None
    }

    /// The preflight: refuse before the mic opens unless the models are
    /// installed, valid and warm, and bind this utterance to them.
    pub async fn prepare(&self, language: &str, deadline: Duration) -> Result<Self, String> {
        crate::language::validate_tag(language)?;
        if language != "en" {
            return Err(format!(
                "Local Parakeet transcribes English only; choose English, or Local Whisper for {language}"
            ));
        }
        let binding = self.service.ready(deadline).await?;
        Ok(Self {
            service: self.service.clone(),
            binding: Some(binding),
        })
    }

    /// The whole utterance after release, as one line of text; "" when the
    /// speech gate found no speech.
    pub async fn transcribe(
        &self,
        pcm: &[i16],
        control: Arc<AtomicU8>,
        deadline: Duration,
    ) -> Result<String, String> {
        if control.load(Ordering::Acquire) == 2 {
            return Err("Cancelled".into());
        }
        if pcm.len() > MAX_SAMPLES {
            return Err("Local audio exceeds 120-second capture limit".into());
        }
        let binding = self
            .binding
            .as_ref()
            .ok_or("Local Parakeet was not checked before recording; no text inserted")?;
        // Digital silence must never reach a recognizer.
        if pcm.is_empty() || pcm.iter().all(|v| v.unsigned_abs() < 16) {
            return Ok(String::new());
        }
        let work = self.service.transcribe(binding, pcm);
        tokio::pin!(work);
        let overall = tokio::time::sleep(deadline);
        tokio::pin!(overall);
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                result = &mut work => {
                    if control.load(Ordering::Acquire) == 2 {
                        return Err("Cancelled".into());
                    }
                    return result;
                }
                _ = &mut overall => {
                    return Err(if control.load(Ordering::Acquire) == 2 {
                        "Cancelled"
                    } else {
                        "Local inference timed out; no text inserted"
                    }
                    .into());
                }
                _ = tick.tick() => {
                    if control.load(Ordering::Acquire) == 2 {
                        return Err("Cancelled".into());
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ helper

use fotw_speechd_client::message::{ErrorCode, ProgressPhase, VadKind};
use fotw_speechd_client::{ClientError, SpeechdClient, SpeechdConfig, StreamConfig, StreamEvent};

/// Where Murmur keeps its own copy of the models.
pub fn models_root() -> Result<PathBuf, String> {
    crate::speechd::models_root()
}

/// `fotw-speechd` through `fotw-speechd-client`.
#[derive(Clone)]
pub struct SpeechdBackend {
    inner: Arc<SpeechdInner>,
}
struct SpeechdInner {
    helper: Arc<crate::speechd::Helper>,
    asr: String,
    vad: Option<String>,
}

impl SpeechdBackend {
    /// The app's shared helper (`crate::speechd::Helper::shared`), which
    /// local streaming (#100) uses too.
    pub fn from_environment() -> Self {
        Self::with_helper(
            crate::speechd::Helper::shared(),
            ASR_ENGINE,
            Some(VAD_ENGINE),
        )
    }

    /// Any helper and engines; tests use the fake helper's engines.
    pub fn new(config: Result<SpeechdConfig, String>, asr: &str, vad: Option<&str>) -> Self {
        Self::with_helper(crate::speechd::Helper::new(config), asr, vad)
    }

    /// Engines on a helper process another backend may share.
    pub fn with_helper(helper: Arc<crate::speechd::Helper>, asr: &str, vad: Option<&str>) -> Self {
        Self {
            inner: Arc::new(SpeechdInner {
                helper,
                asr: asr.into(),
                vad: vad.map(Into::into),
            }),
        }
    }
}

impl SpeechdInner {
    async fn client(&self) -> Result<SpeechdClient, LoadError> {
        self.helper.client().await.map_err(|e| match e {
            crate::speechd::StartError::Unavailable(reason) => LoadError::Unavailable(reason),
            crate::speechd::StartError::Failed(reason) => LoadError::Failed(reason),
        })
    }

    fn engines(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.asr.as_str()).chain(self.vad.as_deref())
    }

    async fn load(&self) -> Result<Binding, LoadError> {
        let client = self.client().await?;
        let capabilities = client
            .capabilities()
            .ok_or_else(|| LoadError::Failed("the speech helper is not running".into()))?;
        for engine in self.engines() {
            let info = capabilities.engine(engine).ok_or_else(|| {
                LoadError::Unavailable(format!(
                    "the speech helper does not offer {engine}; rebuild Murmur"
                ))
            })?;
            if info.model.as_ref().is_some_and(|model| !model.installed) {
                return Err(LoadError::Missing(engine.into()));
            }
        }
        let map = |engine: &str, error: ClientError| match error.code() {
            ErrorCode::ModelMissing => LoadError::Missing(engine.into()),
            ErrorCode::ModelInvalid => LoadError::Invalid(engine.into()),
            _ => LoadError::Failed(error.to_string()),
        };
        let loaded = client
            .load(&self.asr)
            .await
            .map_err(|e| map(&self.asr, e))?;
        if let Some(vad) = &self.vad {
            client.load(vad).await.map_err(|e| map(vad, e))?;
        }
        // One silent chunk each, so the first dictation pays no Core ML compile.
        for engine in self.engines() {
            client.warmup(engine).await.map_err(|e| map(engine, e))?;
        }
        Ok(Binding {
            instance: client.status().generation,
            model: loaded.model,
            revision: loaded.revision,
        })
    }

    fn instance(&self) -> Option<u64> {
        self.helper.instance()
    }

    async fn stream(
        &self,
        config: StreamConfig,
        pcm: &[i16],
    ) -> Result<fotw_speechd_client::SpeechStream, String> {
        let client = self.client().await.map_err(|e| e.message())?;
        let mut stream = client
            .open_stream(config)
            .await
            .map_err(|e| e.to_string())?;
        for chunk in pcm.chunks(CHUNK) {
            stream.send_audio(chunk).await.map_err(|e| e.to_string())?;
        }
        stream.finish().await.map_err(|e| e.to_string())?;
        Ok(stream)
    }

    async fn speech(&self, pcm: Arc<[i16]>) -> Result<Detected, String> {
        let Some(vad) = &self.vad else {
            return Ok(Detected {
                regions: std::iter::once(0..pcm.len()).collect(),
                peak: 1.0,
            });
        };
        let mut config = StreamConfig::new(vad.as_str(), SAMPLE_RATE as u32);
        // Every 256 ms step's probability, for the gate's peak.
        config.options.probabilities = true;
        let mut stream = self.stream(config, &pcm).await?;
        let mut regions = Vec::new();
        let mut open = None;
        let mut peak = 0f32;
        loop {
            match stream.next_event().await {
                Some(StreamEvent::Vad {
                    event,
                    sample,
                    probability,
                }) => {
                    peak = peak.max(probability);
                    let sample = usize::try_from(sample).unwrap_or(usize::MAX).min(pcm.len());
                    match event {
                        VadKind::SpeechStart => {
                            open.get_or_insert(sample);
                        }
                        VadKind::SpeechEnd => {
                            if let Some(start) = open.take() {
                                regions.push(start..sample);
                            }
                        }
                        VadKind::Probability => {}
                    }
                }
                Some(StreamEvent::Closed(_)) | None => break,
                Some(StreamEvent::Failed(failure)) => return Err(failure.message),
                Some(_) => {}
            }
        }
        if let Some(start) = open {
            regions.push(start..pcm.len());
        }
        Ok(Detected { regions, peak })
    }

    async fn recognize(&self, pcm: Arc<[i16]>) -> Result<Recognized, String> {
        let mut stream = self
            .stream(
                StreamConfig::new(self.asr.as_str(), SAMPLE_RATE as u32),
                &pcm,
            )
            .await?;
        let mut recognized = None;
        loop {
            match stream.next_event().await {
                Some(StreamEvent::Final(text)) => {
                    let part = recognized.get_or_insert(Recognized {
                        text: String::new(),
                        model: text.model.clone(),
                    });
                    if !part.text.is_empty() {
                        part.text.push(' ');
                    }
                    part.text.push_str(&text.text);
                }
                Some(StreamEvent::Closed(_)) | None => break,
                Some(StreamEvent::Failed(failure)) => return Err(failure.message),
                Some(_) => {}
            }
        }
        recognized.ok_or_else(|| "the recognizer closed without a result".into())
    }

    async fn install(&self, from: InstallSources, progress: Progress) -> Result<(), String> {
        let client = self.client().await.map_err(|e| e.message())?;
        let models = [
            (self.asr.as_str(), from.parakeet.as_deref()),
            (self.vad.as_deref().unwrap_or_default(), from.vad.as_deref()),
        ];
        for (model, source) in models.into_iter().filter(|(m, _)| !m.is_empty()) {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let report = progress.clone();
            let name = model.to_owned();
            let reporter = tokio::spawn(async move {
                while let Some(update) = rx.recv().await {
                    let fotw_speechd_client::Progress {
                        phase,
                        bytes,
                        total,
                    } = update;
                    let phase = match phase {
                        ProgressPhase::Download => "downloading",
                        ProgressPhase::Verify => "verifying",
                        _ => "preparing",
                    };
                    report(match (bytes, total) {
                        (Some(done), Some(total)) if total > 0 => {
                            format!("{name}: {phase} {}%", done * 100 / total)
                        }
                        _ => format!("{name}: {phase}"),
                    });
                }
            });
            let result = client.install(model, source, Some(tx)).await;
            let _ = reporter.await;
            let revision = result.map_err(|e| format!("Installing {model} failed: {e}"))?;
            progress(format!("{model}: installed revision {revision}"));
            // A loaded copy is the old revision; the next load reads the new one.
            let _ = client.unload(model).await;
        }
        Ok(())
    }
}

impl Backend for SpeechdBackend {
    fn load(&self) -> BoxFuture<Result<Binding, LoadError>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.load().await })
    }
    fn instance(&self) -> Option<u64> {
        self.inner.instance()
    }
    fn speech(&self, pcm: Arc<[i16]>) -> BoxFuture<Result<Detected, String>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.speech(pcm).await })
    }
    fn recognize(&self, pcm: Arc<[i16]>) -> BoxFuture<Result<Recognized, String>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.recognize(pcm).await })
    }
    fn install(&self, from: InstallSources, progress: Progress) -> BoxFuture<Result<(), String>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.install(from, progress).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_span_covers_first_to_last_speech_with_padding_inside_the_audio() {
        assert_eq!(speech_span(&[], 100_000), None);
        let empty = 5..5;
        assert_eq!(speech_span(std::slice::from_ref(&empty), 100_000), None);
        assert_eq!(
            speech_span(&[(20_000..30_000), (50_000..60_000)], 100_000),
            Some(20_000 - SPEECH_PAD..60_000 + SPEECH_PAD)
        );
        let almost_all = 100..99_000;
        assert_eq!(
            speech_span(std::slice::from_ref(&almost_all), 100_000),
            Some(0..100_000)
        );
    }

    #[test]
    fn segment_breaks_become_spaces() {
        assert_eq!(
            one_line(" Hello there.\n Second line.\n"),
            "Hello there. Second line."
        );
    }
}
