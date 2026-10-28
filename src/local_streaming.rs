//! Local streaming dictation (#100): Nemotron 3.5 through the shared
//! `fotw-speechd` helper, so words appear while Control is held.
//!
//! Murmur sends the microphone's 16 kHz PCM to one `nemotron-3.5` stream as
//! it is captured (FlyOnTheWall `docs/SPEECHD_PROTOCOL.md`). The helper's
//! partials are display text only: they go to the attempt's
//! [`Progress`](crate::fallback::Progress) for the indicator and are never
//! returned. On release the stream is finished and only its final comes back,
//! to run through the same command, snippet, vocabulary, cleanup, focus and
//! generation checks as every other engine.
//!
//! - [`Service`] owns the load state: the helper, the model loaded and
//!   warmed, and what each utterance is bound to. It is tested with a fake
//!   [`Backend`]; [`SpeechdBackend`] is the real one.
//! - Nothing here downloads during dictation. The model arrives only through
//!   [`Service::install`], from an explicit setup action.
//! - If the stream fails mid-utterance the audio captured so far is kept and
//!   returned with the failure, so the caller can retry it on the batch
//!   engine within the same attempt.
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::Shared;
use tokio::sync::mpsc;

/// The engine id in the helper.
pub const ENGINE: &str = "nemotron-3.5";
/// How to install the model; shown wherever a missing model is refused.
pub const INSTALL_HINT: &str = "run `murmur speech install-streaming`";
/// The attribution the model license asks for, for About/notices.
pub const ATTRIBUTION: &str = "Nemotron 3.5 ASR Streaming 0.6B (multilingual) by NVIDIA, Core ML conversion FluidInference/Nemotron-3.5-ASR-Streaming-Multilingual-0.6b-CoreML, OpenMDW-1.1 (https://openmdw.ai/license/). Run through FluidAudio (Apache-2.0) in fotw-speechd.";

const SAMPLE_RATE: usize = 16_000;
/// Same bound as the other local engines: 120 s of capture plus resampler drain.
const MAX_SAMPLES: usize = SAMPLE_RATE * 120 + 3200;
/// From `finish` to the final: one padded step of each model, ~50 ms on an M3.
const FINAL_DEADLINE: Duration = Duration::from_secs(10);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What one utterance was checked against before the mic opened. Its final
/// must come from the same helper instance and model, or nothing is typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The helper process serving the model; a restart changes it.
    pub instance: u64,
    pub model: String,
    pub revision: String,
    /// Language codes the engine has a prompt for.
    pub languages: Vec<String>,
}
impl Binding {
    /// Whether a final's `model` (`<model>@<revision>`) is this binding's.
    pub fn produced(&self, model: &str) -> bool {
        model
            .split_once('@')
            .is_some_and(|(id, revision)| id == self.model && revision == self.revision)
    }
}

/// Why the engine cannot take dictation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The model is not installed.
    Missing,
    /// The installed model fails the helper's checks.
    Invalid,
    /// The helper is not bundled or cannot run.
    Unavailable(String),
    /// Anything else, for example a helper crash during load.
    Failed(String),
}
impl LoadError {
    pub fn message(&self) -> String {
        match self {
            LoadError::Missing => {
                format!("Local streaming's model is not installed; {INSTALL_HINT}")
            }
            LoadError::Invalid => {
                format!("Local streaming's model failed verification; reinstall it: {INSTALL_HINT}")
            }
            LoadError::Unavailable(reason) | LoadError::Failed(reason) => {
                format!("Local streaming is unavailable: {reason}")
            }
        }
    }
}

/// One event from a live stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The running text; display only.
    Partial(String),
    /// The committed text and the model that produced it.
    Final { text: String, model: Option<String> },
    /// The stream failed; terminal.
    Failed(String),
}

/// One open stream in the helper. Dropping it cancels the stream, so nothing
/// more is computed or delivered for it.
pub trait LiveStream: Send {
    fn send<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, Result<(), String>>;
    /// No more audio: flush the last partial step into the final.
    fn finish(&mut self) -> BoxFuture<'_, Result<(), String>>;
    /// The next event, or `None` once the stream is over.
    fn next(&mut self) -> BoxFuture<'_, Option<Event>>;
}

/// The operations Murmur needs from the helper. The ordinary tests replace it
/// with a fake; [`SpeechdBackend`] is the real one.
pub trait Backend: Send + Sync + 'static {
    /// Start the helper if needed, then load and warm the engine from local
    /// files. Never downloads.
    fn load(&self) -> BoxFuture<'static, Result<Binding, LoadError>>;
    /// The helper instance serving now, if one is running.
    fn instance(&self) -> Option<u64>;
    /// Open one dictation stream; `language` `None` lets the model detect it.
    fn open(
        &self,
        language: Option<String>,
    ) -> BoxFuture<'static, Result<Box<dyn LiveStream>, String>>;
    /// Explicit setup: install the model, downloading unless `from` is given.
    fn install(&self, from: Option<PathBuf>) -> BoxFuture<'static, Result<(), String>>;
}

type SharedLoad = Shared<BoxFuture<'static, Result<Binding, LoadError>>>;

/// A runtime that outlives every dictation attempt. Each attempt runs on its
/// own short-lived runtime, but the helper's reader, writer and supervisor
/// tasks must keep running between them.
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

impl Service {
    pub fn new(backend: Arc<dyn Backend>) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("murmur-streaming")
            .enable_all()
            .build()
            .map_err(|_| "Cannot start the local streaming runtime".to_string())?;
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

    /// The current load, starting one if none is running, the last failed, or
    /// it loaded into a helper that has since been replaced.
    fn current_load(&self) -> SharedLoad {
        let mut load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        let reusable = load.as_ref().is_some_and(|current| match current.peek() {
            None => true,
            Some(Ok(binding)) => self.backend.instance() == Some(binding.instance),
            Some(Err(_)) => false,
        });
        if !reusable {
            let task = self.handle().spawn(self.backend.load());
            let shared = async move {
                task.await
                    .unwrap_or_else(|_| Err(LoadError::Failed("the load stopped".into())))
            }
            .boxed()
            .shared();
            // Drive it whether or not anyone waits, so warming needs no waiter.
            let poll = shared.clone();
            drop(self.handle().spawn(async move {
                let _ = poll.await;
            }));
            *load = Some(shared);
        }
        load.clone().expect("load just set")
    }

    /// Load and warm in the background: at launch, when streaming is chosen,
    /// and after a helper restart (the next `ready` notices the new instance).
    pub fn warm(&self) {
        if !self.installing.load(Ordering::Acquire) {
            drop(self.current_load());
        }
    }

    /// The finished load's outcome, without starting one.
    pub fn state(&self) -> Option<Result<Binding, LoadError>> {
        let load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        load.as_ref().and_then(|l| l.peek().cloned())
    }

    /// Check before the mic opens that the model is loaded and warm in the
    /// helper serving now. Waits at most `deadline` for a load in progress.
    pub async fn ready(&self, deadline: Duration) -> Result<Binding, String> {
        if self.installing.load(Ordering::Acquire) {
            return Err("Local streaming's model is installing".into());
        }
        let binding = match tokio::time::timeout(deadline, self.current_load()).await {
            Ok(Ok(binding)) => binding,
            Ok(Err(error)) => return Err(error.message()),
            Err(_) => return Err("Local streaming is still loading its model".into()),
        };
        if self.backend.instance() != Some(binding.instance) {
            // The helper restarted after this load; the next attempt reloads.
            self.warm();
            return Err("Local streaming's helper restarted and is warming up again".into());
        }
        Ok(binding)
    }

    /// Explicit setup only; never called during dictation.
    pub async fn install(&self, from: Option<PathBuf>) -> Result<(), String> {
        if self.installing.swap(true, Ordering::AcqRel) {
            return Err("A streaming model install is already running".into());
        }
        let result = self
            .handle()
            .spawn(self.backend.install(from))
            .await
            .unwrap_or_else(|_| Err("the install stopped".into()));
        *self.load.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.installing.store(false, Ordering::Release);
        if result.is_ok() {
            self.warm();
        }
        result
    }

    async fn open(&self, language: Option<String>) -> Result<Box<dyn LiveStream>, String> {
        self.handle()
            .spawn(self.backend.open(language))
            .await
            .unwrap_or_else(|_| Err("the stream did not open".into()))
    }
}

/// The streaming transcriber for one utterance: the shared service plus what
/// preflight bound it to.
#[derive(Clone)]
pub struct Endpoint {
    service: Arc<Service>,
    binding: Option<Binding>,
    language: Option<String>,
}
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

static SHARED: OnceLock<Arc<Service>> = OnceLock::new();

/// Why a stream produced no final: the reason, and every sample captured in
/// this attempt so the caller can retry it on the batch engine.
#[derive(Debug)]
pub struct StreamFailure {
    pub reason: String,
    pub pcm: Vec<i16>,
}

impl Endpoint {
    pub fn new(service: Arc<Service>) -> Self {
        Self {
            service,
            binding: None,
            language: None,
        }
    }

    /// The app's one helper-backed service, created on first use. Creating it
    /// starts nothing; the helper is spawned by the first load.
    pub fn shared() -> Self {
        Self::new(
            SHARED
                .get_or_init(|| {
                    Arc::new(
                        Service::new(Arc::new(SpeechdBackend::from_environment()))
                            .expect("local streaming runtime"),
                    )
                })
                .clone(),
        )
    }

    pub fn service(&self) -> &Arc<Service> {
        &self.service
    }

    /// Refuse before the mic opens unless the engine is loaded and warm and
    /// takes `language`; bind the utterance to what was checked.
    pub async fn prepare(&self, language: &str, deadline: Duration) -> Result<Self, String> {
        crate::language::validate_tag(language)?;
        let binding = self.service.ready(deadline).await?;
        let base = language.split('-').next().unwrap_or(language);
        let language = match language {
            "multi" | "auto" => None,
            _ if binding.languages.iter().any(|l| l == language || l == base) => {
                Some(language.to_owned())
            }
            _ => {
                return Err(format!(
                    "Local streaming does not transcribe the selected language ({language})"
                ));
            }
        };
        Ok(Self {
            service: self.service.clone(),
            binding: Some(binding),
            language,
        })
    }

    /// Stream `audio` until its sender closes (release), publishing partials
    /// to `progress`, then return the final. Dropping the future (Esc)
    /// cancels the stream; nothing is published after it ends.
    pub async fn transcribe(
        &self,
        mut audio: mpsc::Receiver<Vec<i16>>,
        progress: crate::fallback::Progress,
        control: Arc<AtomicU8>,
    ) -> Result<String, StreamFailure> {
        // Whatever happens, this attempt shows no partial after it ends.
        let _done = progress.live_guard();
        let mut pcm: Vec<i16> = Vec::new();
        let keep = |pcm: &mut Vec<i16>, chunk: &[i16]| {
            let room = MAX_SAMPLES.saturating_sub(pcm.len());
            pcm.extend(chunk.iter().take(room));
        };
        let Some(binding) = self.binding.clone() else {
            return Err(drain(audio, pcm, "it was not checked before recording".into()).await);
        };
        if self.service.backend.instance() != Some(binding.instance) {
            return Err(drain(
                audio,
                pcm,
                "the helper restarted after the mic opened".into(),
            )
            .await);
        }
        let mut stream = match self.service.open(self.language.clone()).await {
            Ok(stream) => stream,
            Err(reason) => return Err(drain(audio, pcm, reason).await),
        };
        // While held: forward audio, show partials.
        loop {
            tokio::select! {
                biased;
                event = stream.next() => match event {
                    Some(Event::Partial(text)) => progress.set_live(&text),
                    Some(Event::Final { .. }) => {}
                    Some(Event::Failed(reason)) => return Err(drain(audio, pcm, reason).await),
                    None => return Err(drain(audio, pcm, "the stream ended early".into()).await),
                },
                chunk = audio.recv() => match chunk {
                    Some(chunk) => {
                        keep(&mut pcm, &chunk);
                        if let Err(reason) = stream.send(&chunk).await {
                            return Err(drain(audio, pcm, reason).await);
                        }
                    }
                    None => break,
                },
            }
        }
        if control.load(Ordering::Acquire) == 2 {
            return Err(StreamFailure {
                reason: "Cancelled".into(),
                pcm,
            });
        }
        // Released: flush the last step and take the final.
        if let Err(reason) = stream.finish().await {
            return Err(StreamFailure { reason, pcm });
        }
        let outcome = tokio::time::timeout(FINAL_DEADLINE, async {
            loop {
                match stream.next().await {
                    Some(Event::Partial(text)) => progress.set_live(&text),
                    Some(Event::Final { text, model }) => return Ok((text, model)),
                    Some(Event::Failed(reason)) => return Err(reason),
                    None => return Err("the stream ended without a final".to_string()),
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err("no final within 10 s".into()));
        let (text, model) = match outcome {
            Ok(done) => done,
            Err(reason) => return Err(StreamFailure { reason, pcm }),
        };
        if self.service.backend.instance() != Some(binding.instance)
            || !model.as_deref().is_some_and(|m| binding.produced(m))
        {
            return Err(StreamFailure {
                reason: "the helper restarted or changed model during the utterance".into(),
                pcm,
            });
        }
        Ok(one_line(&text))
    }
}

/// Keep collecting the attempt's audio until release, for the batch retry.
async fn drain(
    mut audio: mpsc::Receiver<Vec<i16>>,
    mut pcm: Vec<i16>,
    reason: String,
) -> StreamFailure {
    while let Some(chunk) = audio.recv().await {
        let room = MAX_SAMPLES.saturating_sub(pcm.len());
        pcm.extend(chunk.into_iter().take(room));
    }
    StreamFailure { reason, pcm }
}

/// Typed text is one line (#98).
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------ helper

use fotw_speechd_client::message::ErrorCode;
use fotw_speechd_client::{ClientError, SpeechdClient, SpeechdConfig, StreamConfig, StreamEvent};

/// Where installed models live; `MURMUR_SPEECHD_MODELS` overrides it.
pub fn models_root() -> Result<PathBuf, String> {
    if let Some(root) = std::env::var_os("MURMUR_SPEECHD_MODELS").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(root));
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or("Cannot locate home directory")?)
            .join("Library/Application Support/Murmur/models"),
    )
}

/// The real backend: `fotw-speechd` through `fotw-speechd-client`.
pub struct SpeechdBackend {
    inner: Arc<Inner>,
}
struct Inner {
    config: Result<SpeechdConfig, String>,
    engine: String,
    client: tokio::sync::Mutex<Option<SpeechdClient>>,
    current: Mutex<Option<SpeechdClient>>,
}

impl SpeechdBackend {
    /// The bundled helper (or `FOTW_SPEECHD_PATH`) and Murmur's models root.
    pub fn from_environment() -> Self {
        let config = match (fotw_speechd_client::locate_helper(), models_root()) {
            (Some(program), Ok(root)) => Ok(SpeechdConfig::new(program, root)),
            (None, _) => Err(
                "this build does not include the fotw-speechd helper; rebuild with MURMUR_SPEECHD=1 ./scripts/build-app.sh"
                    .to_string(),
            ),
            (_, Err(error)) => Err(error),
        };
        Self::new(config, ENGINE)
    }

    /// `engine` is `nemotron-3.5`, or a fake one in tests.
    pub fn new(config: Result<SpeechdConfig, String>, engine: &str) -> Self {
        let config = config.map(|mut config| {
            config.client_name = concat!("murmur/", env!("CARGO_PKG_VERSION")).into();
            config
        });
        Self {
            inner: Arc::new(Inner {
                config,
                engine: engine.into(),
                client: tokio::sync::Mutex::new(None),
                current: Mutex::new(None),
            }),
        }
    }
}

impl Inner {
    async fn client(&self) -> Result<SpeechdClient, LoadError> {
        let mut client = self.client.lock().await;
        if let Some(existing) = client.as_ref() {
            if !existing.status().failed {
                return Ok(existing.clone());
            }
            // The restart budget was spent; an explicit retry resets it.
            existing.reset().await;
            if !existing.status().failed {
                return Ok(existing.clone());
            }
        }
        let config = self.config.clone().map_err(LoadError::Unavailable)?;
        let started = SpeechdClient::start(config).await.map_err(|e| match e {
            ClientError::Spawn(reason) => LoadError::Unavailable(reason),
            ClientError::VersionMismatch { .. } => {
                LoadError::Unavailable(format!("{e}; rebuild Murmur so its helper matches"))
            }
            other => LoadError::Failed(other.to_string()),
        })?;
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(started.clone());
        *client = Some(started.clone());
        Ok(started)
    }

    async fn load(&self) -> Result<Binding, LoadError> {
        let client = self.client().await?;
        let info = client
            .capabilities()
            .and_then(|c| c.engine(&self.engine).cloned())
            .ok_or_else(|| {
                LoadError::Unavailable(format!(
                    "the speech helper does not offer {}; rebuild Murmur",
                    self.engine
                ))
            })?;
        if info.model.as_ref().is_some_and(|model| !model.installed) {
            return Err(LoadError::Missing);
        }
        let map = |error: ClientError| match error.code() {
            ErrorCode::ModelMissing => LoadError::Missing,
            ErrorCode::ModelInvalid => LoadError::Invalid,
            _ => LoadError::Failed(error.to_string()),
        };
        let loaded = client.load(&self.engine).await.map_err(map)?;
        // One silent step, so the first dictation pays no Core ML compile.
        // After a crash the client replays this load and warmup itself.
        client.warmup(&self.engine).await.map_err(map)?;
        Ok(Binding {
            instance: client.status().generation,
            model: loaded.model,
            revision: loaded.revision,
            languages: info.languages,
        })
    }

    fn instance(&self) -> Option<u64> {
        let client = self.current.lock().unwrap_or_else(|e| e.into_inner());
        client
            .as_ref()
            .map(SpeechdClient::status)
            .filter(|status| !status.failed)
            .map(|status| status.generation)
    }
}

impl Backend for SpeechdBackend {
    fn load(&self) -> BoxFuture<'static, Result<Binding, LoadError>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.load().await })
    }

    fn instance(&self) -> Option<u64> {
        self.inner.instance()
    }

    fn open(
        &self,
        language: Option<String>,
    ) -> BoxFuture<'static, Result<Box<dyn LiveStream>, String>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let client = inner.client().await.map_err(|e| e.message())?;
            let mut config = StreamConfig::new(inner.engine.as_str(), SAMPLE_RATE as u32);
            config.options.language = language;
            let stream = client
                .open_stream(config)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Box::new(HelperStream(stream)) as Box<dyn LiveStream>)
        })
    }

    fn install(&self, from: Option<PathBuf>) -> BoxFuture<'static, Result<(), String>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let client = inner.client().await.map_err(|e| e.message())?;
            client
                .install(ENGINE, from.as_deref(), None)
                .await
                .map_err(|e| e.to_string())?;
            // A loaded copy predates the install; the next load reads the new one.
            let _ = client.unload(&inner.engine).await;
            Ok(())
        })
    }
}

/// A helper stream; dropping it cancels it, even after `finish`, so an Esc
/// while the final is computed frees the engine for the next dictation.
struct HelperStream(fotw_speechd_client::SpeechStream);
impl Drop for HelperStream {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
impl LiveStream for HelperStream {
    fn send<'a>(&'a mut self, pcm: &'a [i16]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.send_audio(pcm).await.map_err(|e| e.to_string()) })
    }
    fn finish(&mut self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.0.finish().await.map_err(|e| e.to_string()) })
    }
    fn next(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(async move {
            loop {
                return match self.0.next_event().await? {
                    StreamEvent::Partial { text, .. } => Some(Event::Partial(text)),
                    StreamEvent::Final(f) => Some(Event::Final {
                        text: f.text,
                        model: f.model,
                    }),
                    StreamEvent::Failed(failure) => Some(Event::Failed(format!(
                        "{} ({:?})",
                        failure.message, failure.code
                    ))),
                    StreamEvent::Closed(_) => None,
                    _ => continue,
                };
            }
        })
    }
}
