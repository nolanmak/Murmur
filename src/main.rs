use murmur::vocabulary::ProfileFile as _;
fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let result = match command.as_str() {
        "--help" | "help" => {
            println!(
                "Murmur — macOS hold-Control dictation\n\nrun     Launch the menu-bar app (use the .app bundle)\ndoctor  Report provider configuration without printing secrets\nspeech  Select parakeet, whisper [URL], local [URL], deepgram or fallback [parakeet|whisper] [URL];\n        speech install-parakeet [--parakeet-from DIR] [--vad-from DIR] installs the Parakeet model\nlanguage CODE  Select an explicit transcription language (default en)\nvocabulary  Import/export/validate a versioned vocabulary profile\ncleanup  Configure a provider, then explicitly turn text cleanup on/off\nprivacy  Choose offline-all (local speech, no cleanup) or configured providers\n\nHold Control to dictate; release to insert; Esc cancels.\nRun scripts/build-app.sh and launch the resulting .app for capture."
            );
            Ok(())
        }
        "speech" => configure_speech(),
        "cleanup" => configure_cleanup(false),
        "privacy" => configure_cleanup(true),
        "vocabulary" => configure_vocabulary(),
        "language" => configure_language(),
        "doctor" => doctor(),
        #[cfg(target_os = "macos")]
        "review-preview" => murmur::platform::macos::preview_remote_review(),
        "run" => {
            #[cfg(target_os = "macos")]
            {
                murmur::platform::macos::run()
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err("Desktop dictation currently requires macOS 14.4+".to_string())
            }
        }
        _ => Err("Unknown command; use --help".to_string()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn configure_cleanup(privacy: bool) -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(2).collect();
    if !privacy
        && args
            .first()
            .is_some_and(|s| matches!(s.as_str(), "offline-all" | "configured"))
    {
        return Err("Use murmur privacy offline-all | configured".into());
    }
    if privacy && (args.len() != 1 || !matches!(args[0].as_str(), "offline-all" | "configured")) {
        return Err("Usage: murmur privacy offline-all | configured".into());
    }
    println!(
        "{}",
        murmur::cleanup_config::command(
            &args,
            &murmur::cleanup_config::Settings::path()?,
            &murmur::cleanup_config::NetworkPolicy::path()?
        )?
    );
    Ok(())
}

const SPEECH_USAGE: &str = "Usage: murmur speech parakeet | whisper [http://127.0.0.1:17863] | local [URL] | deepgram | fallback [parakeet|whisper] [URL] | install-parakeet [--parakeet-from DIR] [--vad-from DIR]";

fn configure_speech() -> Result<(), String> {
    use murmur::speech::{LocalEngine, Provider};
    let args: Vec<String> = std::env::args().skip(2).collect();
    if args.first().map(String::as_str) == Some("install-parakeet") {
        return install_parakeet(&args[1..]);
    }
    let path = murmur::speech::Settings::path()?;
    let mut settings = murmur::speech::Settings::load(&path)?;
    // A URL is the Whisper gateway's, so giving one means Whisper.
    let mut engine = settings.local_engine;
    let mut url = None;
    for arg in args.iter().skip(1).map(String::as_str) {
        match arg {
            "parakeet" => engine = LocalEngine::Parakeet,
            "whisper" => engine = LocalEngine::Whisper,
            url_arg if url.is_none() && url_arg.starts_with("http") => {
                url = Some(url_arg);
                engine = LocalEngine::Whisper;
            }
            _ => return Err(SPEECH_USAGE.into()),
        }
    }
    let provider = match args.first().map(String::as_str) {
        Some("parakeet") if args.len() == 1 => Provider::LocalParakeet,
        Some("whisper") => Provider::LocalWhisper,
        Some("local") => Provider::local(engine),
        Some("deepgram") if args.len() == 1 => Provider::Deepgram,
        Some("fallback") => Provider::backup(engine),
        _ => return Err(SPEECH_USAGE.into()),
    };
    if let Some(url) = url {
        if provider.local_engine() != Some(LocalEngine::Whisper) {
            return Err(SPEECH_USAGE.into());
        }
        settings.local_endpoint = url.into();
    }
    settings.select(provider);
    settings.save(&path)?;
    println!(
        "Saved {}; restart the running app to reload CLI changes.",
        murmur::engine_command::label(&settings.provider)
    );
    Ok(())
}

/// Explicit setup: the only CLI path that downloads a model.
fn install_parakeet(args: &[String]) -> Result<(), String> {
    let mut sources = murmur::local_parakeet::InstallSources::default();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let dir = iter.next().ok_or(SPEECH_USAGE)?;
        match flag.as_str() {
            "--parakeet-from" => sources.parakeet = Some(dir.into()),
            "--vad-from" => sources.vad = Some(dir.into()),
            _ => return Err(SPEECH_USAGE.into()),
        }
    }
    let service = murmur::local_parakeet::Endpoint::shared().service().clone();
    let progress: murmur::local_parakeet::Progress =
        std::sync::Arc::new(|line| eprintln!("{line}"));
    murmur::capture::block_on_detached(async move { service.install(sources, progress).await })??;
    println!(
        "Installed Parakeet Unified and Silero VAD in {}. Licenses: {}",
        murmur::local_parakeet::models_root()?.display(),
        murmur::local_parakeet::ATTRIBUTION
    );
    Ok(())
}
fn configure_language() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        return Err("Usage: murmur language CODE (for example en, es, fr-CA)".into());
    }
    let path = murmur::vocabulary::Profile::path()?;
    murmur::language::save_choice(&path, &args[2])?;
    println!(
        "Saved language for the next utterance. Provider/model support is checked before capture."
    );
    Ok(())
}

fn configure_vocabulary() -> Result<(), String> {
    use murmur::vocabulary::{Profile, Snippet};
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("Usage: murmur vocabulary import|export|validate|example FILE".into());
    }
    let file = std::path::Path::new(&args[3]);
    match args[2].as_str() {
        "import" => Profile::read(file)?.save(&Profile::path()?)?,
        "export" => Profile::load(&Profile::path()?)?.save(file)?,
        "validate" => {
            Profile::read(file)?;
        }
        "example" => Profile {
            version: 1,
            language: "en".into(),
            recognition_hints: vec!["Project Acorn".into()],
            snippets: vec![Snippet {
                name: "sign off".into(),
                text: "Thanks,\nExample team".into(),
            }],
        }
        .save(file)?,
        _ => return Err("Usage: murmur vocabulary import|export|validate|example FILE".into()),
    }
    println!(
        "Vocabulary profile processed. Changes apply to the next utterance; recognition hints are not supported by local Whisper."
    );
    Ok(())
}

fn doctor() -> Result<(), String> {
    let vocabulary = murmur::vocabulary::Profile::load(&murmur::vocabulary::Profile::path()?)?;
    println!(
        "Vocabulary: {} recognition hints, {} snippets (contents withheld). Local engines ignore recognition hints; snippets work with every provider.",
        vocabulary.recognition_hints.len(),
        vocabulary.snippets.len()
    );
    let settings = murmur::speech::Settings::load(&murmur::speech::Settings::path()?)?;
    println!(
        "Selected language: {} (checked against provider/model before capture).",
        vocabulary.language
    );
    let privacy = murmur::cleanup_config::NetworkPolicy::load(
        &murmur::cleanup_config::NetworkPolicy::path()?,
    )?;
    let local = settings.effective_local_engine();
    match local {
        murmur::speech::LocalEngine::Parakeet => println!(
            "Local engine: Local Parakeet (helper {}, models in {}). Install with `murmur speech install-parakeet`; nothing downloads during dictation.",
            fotw_speechd_client::locate_helper().map_or("not bundled in this build".into(), |p| p
                .display()
                .to_string()),
            murmur::local_parakeet::models_root()?.display()
        ),
        murmur::speech::LocalEngine::Whisper => println!(
            "Local engine: Local Whisper at {}.",
            settings.local_endpoint
        ),
    }
    if privacy.offline_all {
        println!("Offline-all: local speech only, cleanup disabled; cloud credentials not read.");
        return Ok(());
    }
    match settings.provider {
        murmur::speech::Provider::Deepgram => murmur::config::load().map(|c| {
            println!(
                "Deepgram credential available via {} (value hidden).",
                c.source
            )
        }),
        murmur::speech::Provider::DeepgramThenWhisper
        | murmur::speech::Provider::DeepgramThenParakeet => {
            println!(
                "Deepgram → {} fallback. Install the local engine before relying on offline recovery. No model downloads occur during capture.",
                local.label()
            );
            Ok(())
        }
        murmur::speech::Provider::LocalParakeet => {
            println!("Local Parakeet. No cloud credentials required.");
            Ok(())
        }
        murmur::speech::Provider::LocalWhisper => {
            println!(
                "Local Whisper at {}. No cloud credentials required. Use scripts/local-whisper.py --app murmur status for engine readiness.",
                settings.local_endpoint
            );
            Ok(())
        }
    }
}
