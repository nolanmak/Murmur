//! Murmur #101: Parakeet Unified is the default local engine, Whisper stays
//! selectable, and both are named by voice.
use murmur::engine_command::{Decision, decide, parse};
use murmur::speech::{Provider, Settings};

#[test]
fn a_new_install_saves_parakeet_as_its_local_engine_and_accepts_local_parakeet() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("speech.json");
    // No speech.json yet: a new install.
    Settings::load(&path).unwrap().save(&path).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(
        saved.contains(r#""local_engine":"parakeet""#),
        "a new install must default its local engine to Parakeet: {saved}"
    );

    std::fs::write(
        &path,
        r#"{"provider":"local-parakeet","local_endpoint":"http://127.0.0.1:17863"}"#,
    )
    .unwrap();
    assert!(
        Settings::load(&path).is_ok(),
        "Local Parakeet must be a selectable provider: {:?}",
        Settings::load(&path)
    );
}

#[test]
fn an_existing_whisper_install_keeps_whisper_until_the_user_chooses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("speech.json");
    // Written by a build before Parakeet: no local_engine field.
    std::fs::write(
        &path,
        r#"{"provider":"local-whisper","local_endpoint":"http://127.0.0.1:17863"}"#,
    )
    .unwrap();
    let settings = Settings::load(&path).unwrap();
    assert_eq!(settings.provider, Provider::LocalWhisper);
    settings.save(&path).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(
        saved.contains(r#""provider":"local-whisper""#) && !saved.contains("parakeet"),
        "saving must not migrate a Whisper user: {saved}"
    );
}

#[test]
fn parakeet_and_whisper_are_named_by_voice() {
    for spoken in [
        "Murmur, use Parakeet.",
        "murmur switch to parakeet",
        "Murmur, use Whisper.",
        "Murmur, switch to whisper",
    ] {
        assert!(parse(spoken).is_some(), "{spoken:?} must be a command");
    }
    assert!(parse("Use parakeet for this one.").is_none());
}

#[test]
fn use_local_on_a_new_install_lands_on_local_parakeet() {
    let d = decide(
        parse("Murmur, use local").unwrap(),
        Some(&Provider::Deepgram),
        None,
        // A new install: no speech.json yet.
        Settings::default().local_engine,
        false,
        &Ok(()),
        &Ok(()),
    );
    assert!(
        matches!(&d, Decision::Switch { message, .. } if message.contains("Local Parakeet")),
        "{d:?}"
    );
}

// ------------------------------------------------------------ fake loader
//
// The helper is replaced by an in-process backend, so these run with no
// model, helper, microphone or network.

use murmur::local_parakeet::{
    Backend, Binding, BoxFuture, Endpoint, InstallSources, LoadError, Progress, Recognized,
    Service,
};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Fake {
    load: Mutex<Option<LoadError>>,
    hang_load: AtomicBool,
    loads: AtomicUsize,
    instance: AtomicU64,
    restart_during_speech: AtomicBool,
    regions: Mutex<Option<Vec<Range<usize>>>>,
    text: Mutex<String>,
    model: Mutex<Option<String>>,
    speech_calls: AtomicUsize,
    recognized: Mutex<Vec<usize>>,
    installs: AtomicUsize,
}
impl Fake {
    fn new() -> Arc<Self> {
        let fake = Self::default();
        fake.instance.store(1, Ordering::SeqCst);
        *fake.text.lock().unwrap() = "Fixture text.".into();
        *fake.model.lock().unwrap() = Some("parakeet-unified".into());
        Arc::new(fake)
    }
}
struct Handle(Arc<Fake>);
impl Backend for Handle {
    fn load(&self) -> BoxFuture<Result<Binding, LoadError>> {
        let fake = self.0.clone();
        Box::pin(async move {
            fake.loads.fetch_add(1, Ordering::SeqCst);
            if fake.hang_load.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if let Some(error) = fake.load.lock().unwrap().clone() {
                return Err(error);
            }
            Ok(Binding {
                instance: fake.instance.load(Ordering::SeqCst),
                model: "parakeet-unified".into(),
                revision: "d32e972dd4315f1dc3f6be28fb2aab0ab3e80358".into(),
            })
        })
    }
    fn instance(&self) -> Option<u64> {
        Some(self.0.instance.load(Ordering::SeqCst))
    }
    fn speech(&self, pcm: Arc<[i16]>) -> BoxFuture<Result<Vec<Range<usize>>, String>> {
        let fake = self.0.clone();
        Box::pin(async move {
            fake.speech_calls.fetch_add(1, Ordering::SeqCst);
            if fake.restart_during_speech.load(Ordering::SeqCst) {
                fake.instance.fetch_add(1, Ordering::SeqCst);
            }
            Ok(fake
                .regions
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| vec![0..pcm.len()]))
        })
    }
    fn recognize(&self, pcm: Arc<[i16]>) -> BoxFuture<Result<Recognized, String>> {
        let fake = self.0.clone();
        Box::pin(async move {
            fake.recognized.lock().unwrap().push(pcm.len());
            Ok(Recognized {
                text: fake.text.lock().unwrap().clone(),
                model: fake.model.lock().unwrap().clone(),
            })
        })
    }
    fn install(&self, _: InstallSources, progress: Progress) -> BoxFuture<Result<(), String>> {
        let fake = self.0.clone();
        Box::pin(async move {
            fake.installs.fetch_add(1, Ordering::SeqCst);
            progress("parakeet-unified: installed".into());
            *fake.load.lock().unwrap() = None;
            Ok(())
        })
    }
}
fn endpoint(fake: &Arc<Fake>) -> Endpoint {
    Endpoint::new(Arc::new(Service::new(Arc::new(Handle(fake.clone()))).unwrap()))
}

/// One second of a loud tone at 16 kHz, delivered after the mic starts.
struct ToneTap {
    id: fotw_audio::TapId,
    control: Arc<AtomicU8>,
}
impl fotw_audio::AudioTap for ToneTap {
    fn id(&self) -> &fotw_audio::TapId {
        &self.id
    }
    fn format(&self) -> fotw_audio::StreamFormat {
        fotw_audio::StreamFormat::new(16_000, 1, fotw_audio::SampleFormat::F32)
    }
    fn format_is_authoritative(&self) -> bool {
        true
    }
    fn start(
        &mut self,
        mut sink: Box<dyn fotw_audio::FrameSink>,
    ) -> Result<fotw_audio::StreamFormat, fotw_audio::TapError> {
        let tone: Vec<f32> = (0..16_000).map(|i| ((i % 40) as f32 / 40.0) - 0.5).collect();
        sink.on_frames(
            &tone,
            fotw_audio::CaptureTimestamp::new(0, 0),
            fotw_audio::FrameFlags::empty(),
        );
        self.control.store(1, Ordering::Release);
        Ok(self.format())
    }
    fn stop(&mut self) -> Result<(), fotw_audio::TapError> {
        Ok(())
    }
}

async fn dictate(endpoint: &Endpoint) -> (Result<String, String>, usize) {
    let control = Arc::new(AtomicU8::new(0));
    let opened = Arc::new(AtomicUsize::new(0));
    let (seen, tap_control) = (opened.clone(), control.clone());
    let input = murmur::capture::InputPreparation::new(move || {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(murmur::capture::PreparedInput::new(Box::new(ToneTap {
            id: fotw_audio::TapId::mic("fixture"),
            control: tap_control,
        })))
    });
    let result = murmur::capture::run_provider_with_preparation(
        input,
        murmur::capture::CaptureProvider::Local(endpoint.clone().into()),
        control,
        Arc::new(AtomicBool::new(false)),
        Default::default(),
        Default::default(),
        fotw_audio::device_change::DeviceChangeSignal::new(),
        murmur::vocabulary::Profile::default(),
    )
    .await;
    (result, opened.load(Ordering::SeqCst))
}

#[tokio::test]
async fn a_missing_or_corrupt_model_is_refused_before_the_microphone_opens_with_an_install_hint() {
    for error in [
        LoadError::Missing("parakeet-unified".into()),
        LoadError::Invalid("silero-vad".into()),
    ] {
        let fake = Fake::new();
        *fake.load.lock().unwrap() = Some(error.clone());
        let (result, opened) = dictate(&endpoint(&fake)).await;
        let message = result.unwrap_err();
        assert!(
            message.contains("Install Parakeet model") && message.contains("install-parakeet"),
            "{error:?}: {message}"
        );
        assert_eq!(opened, 0, "{error:?}: the microphone must not open");
        assert_eq!(fake.speech_calls.load(Ordering::SeqCst), 0);
        assert!(fake.recognized.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn an_explicit_install_makes_the_next_dictation_work_without_a_restart() {
    let fake = Fake::new();
    *fake.load.lock().unwrap() = Some(LoadError::Missing("parakeet-unified".into()));
    let endpoint = endpoint(&fake);
    assert!(dictate(&endpoint).await.0.is_err());
    let lines = Arc::new(Mutex::new(Vec::new()));
    let seen = lines.clone();
    endpoint
        .service()
        .install(
            InstallSources::default(),
            Arc::new(move |line| seen.lock().unwrap().push(line)),
        )
        .await
        .unwrap();
    assert_eq!(fake.installs.load(Ordering::SeqCst), 1);
    assert!(!lines.lock().unwrap().is_empty());
    assert_eq!(dictate(&endpoint).await.0.unwrap(), "Fixture text.");
}

#[tokio::test]
async fn a_model_still_loading_is_refused_instead_of_recording() {
    let fake = Fake::new();
    fake.hang_load.store(true, Ordering::SeqCst);
    let (result, opened) = dictate(&endpoint(&fake)).await;
    assert!(result.unwrap_err().contains("still loading"));
    assert_eq!(opened, 0);
}

#[tokio::test]
async fn the_whole_utterance_is_gated_trimmed_recognized_once_and_typed_as_one_line() {
    let fake = Fake::new();
    *fake.text.lock().unwrap() = " Hello there.\n Second segment.\n".into();
    // Speech only in the middle of the ~1.2 s utterance.
    *fake.regions.lock().unwrap() = Some(vec![8_000..9_000]);
    let endpoint = endpoint(&fake);
    let (result, opened) = dictate(&endpoint).await;
    assert_eq!(result.unwrap(), "Hello there. Second segment.");
    assert_eq!(opened, 1);
    assert_eq!(fake.speech_calls.load(Ordering::SeqCst), 1);
    let recognized = fake.recognized.lock().unwrap().clone();
    // 300 ms of padding on each side of the speech, inside the audio.
    assert_eq!(recognized, vec![1_000 + 2 * 4_800]);
    // Loaded once, at the first preflight; the next utterance reuses it.
    assert_eq!(dictate(&endpoint).await.0.unwrap(), "Hello there. Second segment.");
    assert_eq!(fake.loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn no_speech_types_nothing_and_never_reaches_the_recognizer() {
    let fake = Fake::new();
    *fake.regions.lock().unwrap() = Some(Vec::new());
    let (result, opened) = dictate(&endpoint(&fake)).await;
    assert_eq!(result.unwrap(), "");
    assert_eq!(opened, 1);
    assert!(fake.recognized.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_engine_is_bound_per_utterance() {
    // The helper restarted between preflight and inference.
    let fake = Fake::new();
    fake.restart_during_speech.store(true, Ordering::SeqCst);
    let endpoint = endpoint(&fake);
    let error = dictate(&endpoint).await.0.unwrap_err();
    assert!(error.contains("no text inserted"), "{error}");
    // The next utterance binds to the instance serving now.
    fake.restart_during_speech.store(false, Ordering::SeqCst);
    assert_eq!(dictate(&endpoint).await.0.unwrap(), "Fixture text.");

    // Text from a different model than preflight checked is not typed.
    *fake.model.lock().unwrap() = Some("another-model".into());
    let error = dictate(&endpoint).await.0.unwrap_err();
    assert!(error.contains("no text inserted"), "{error}");
}

#[tokio::test]
async fn parakeet_is_english_only_and_says_so_before_recording() {
    let fake = Fake::new();
    let error = endpoint(&fake)
        .prepare("fr", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert!(error.contains("English") && error.contains("Local Whisper"), "{error}");
}

#[test]
fn each_choice_runs_its_own_local_engine_and_offline_all_follows_the_default() {
    use murmur::capture::CaptureProvider;
    use murmur::local_stt::LocalTranscriber;
    let engine = |provider: Provider, local_engine, offline_all| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speech.json");
        Settings {
            provider,
            local_engine,
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        match murmur::speech::Selection::load(Ok(path))
            .capture_provider_with_privacy(offline_all, || Err("no key".into()))
            .unwrap()
        {
            CaptureProvider::Local(LocalTranscriber::Parakeet(_)) => "parakeet",
            CaptureProvider::Local(LocalTranscriber::Whisper(_)) => "whisper",
            _ => "cloud",
        }
    };
    use murmur::speech::LocalEngine::{Parakeet, Whisper};
    assert_eq!(engine(Provider::LocalParakeet, Parakeet, false), "parakeet");
    assert_eq!(engine(Provider::LocalWhisper, Whisper, false), "whisper");
    // Without a key the backup runs alone.
    assert_eq!(engine(Provider::DeepgramThenParakeet, Parakeet, false), "parakeet");
    assert_eq!(engine(Provider::DeepgramThenWhisper, Whisper, false), "whisper");
    assert_eq!(engine(Provider::Deepgram, Parakeet, true), "parakeet");
    assert_eq!(engine(Provider::Deepgram, Whisper, true), "whisper");
}

#[test]
fn choosing_an_engine_by_name_makes_it_the_default_local_engine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("speech.json");
    std::fs::write(
        &path,
        r#"{"provider":"local-whisper","local_endpoint":"http://127.0.0.1:17863"}"#,
    )
    .unwrap();
    let mut selection = murmur::speech::Selection::load(Ok(path.clone()));
    assert_eq!(selection.local_engine(), murmur::speech::LocalEngine::Whisper);
    selection.choose(Provider::LocalParakeet, &path).unwrap();
    selection.choose(Provider::Deepgram, &path).unwrap();
    let reloaded = Settings::load(&path).unwrap();
    assert_eq!(reloaded.local_engine, murmur::speech::LocalEngine::Parakeet);
    // "use local" now lands on Parakeet; "use whisper" still reaches Whisper.
    let d = decide(
        parse("Murmur, use whisper").unwrap(),
        Some(&Provider::Deepgram),
        None,
        reloaded.local_engine,
        false,
        &Ok(()),
        &Ok(()),
    );
    assert!(matches!(d, Decision::Switch { to: Provider::LocalWhisper, .. }), "{d:?}");
    let d = decide(
        parse("Murmur, use backup").unwrap(),
        Some(&Provider::Deepgram),
        None,
        reloaded.local_engine,
        false,
        &Ok(()),
        &Ok(()),
    );
    assert!(
        matches!(d, Decision::Switch { to: Provider::DeepgramThenParakeet, .. }),
        "{d:?}"
    );
}
