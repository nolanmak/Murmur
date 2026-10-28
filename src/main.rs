use murmur::vocabulary::ProfileFile as _;
fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let result = match command.as_str() {
        "--help" | "help" => {
            println!(
                "Murmur — macOS hold-Control dictation\n\nrun     Launch the menu-bar app (use the .app bundle)\ndoctor  Report provider configuration without printing secrets\nspeech  Select local [URL], deepgram or fallback [URL] transcription\nlanguage CODE  Select an explicit transcription language (default en)\nvocabulary  Import/export/validate a versioned vocabulary profile\ncleanup  Configure a provider, then explicitly turn text cleanup on/off\nprivacy  Choose offline-all (local speech, no cleanup) or configured providers\n\nHold Control to dictate; release to insert; Esc cancels.\nRun scripts/build-app.sh and launch the resulting .app for capture."
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

fn configure_speech() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let path = murmur::speech::Settings::path()?;
    let mut settings = murmur::speech::Settings::load(&path)?;
    match args.get(2).map(String::as_str) {
        Some("local") => {
            settings.provider = murmur::speech::Provider::LocalWhisper;
            if let Some(url) = args.get(3) {
                settings.local_endpoint = url.clone();
            }
        }
        Some("streaming") => {
            settings.provider = murmur::speech::Provider::LocalStreaming;
            if let Some(url) = args.get(3) {
                settings.local_endpoint = url.clone();
            }
        }
        Some("install-streaming") => return install_streaming(&args[3..]),
        Some("deepgram") => settings.provider = murmur::speech::Provider::Deepgram,
        Some("fallback") => {
            settings.provider = murmur::speech::Provider::DeepgramThenLocal;
            if let Some(url) = args.get(3) { settings.local_endpoint = url.clone(); }
        },
        _ => return Err("Usage: murmur speech local [http://127.0.0.1:17863] | streaming [http://127.0.0.1:17863] | install-streaming [--from DIR] | deepgram | fallback [http://127.0.0.1:17863]".into()),
    }
    settings.save(&path)?;
    println!("Saved transcription provider; restart running app to reload CLI changes.");
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
            corrections: vec![murmur::vocabulary::Correction {
                word: "open telemetry".into(),
                replacement: Some("OpenTelemetry".into()),
                matching_threshold: None,
            }],
        }
        .save(file)?,
        _ => return Err("Usage: murmur vocabulary import|export|validate|example FILE".into()),
    }
    println!(
        "Vocabulary profile processed. Changes apply to the next utterance; local Whisper text is corrected from recognition hints and corrections."
    );
    Ok(())
}

fn doctor() -> Result<(), String> {
    let vocabulary = murmur::vocabulary::Profile::load(&murmur::vocabulary::Profile::path()?)?;
    println!(
        "Vocabulary: {} recognition hints, {} corrections, {} snippets (contents withheld). Deepgram receives hints as keyterms; local Whisper text gets {} vocabulary corrections instead. Snippets work with either provider.",
        vocabulary.recognition_hints.len(),
        vocabulary.corrections.len(),
        vocabulary.snippets.len(),
        murmur::vocabulary::VocabularyMatcher::new(&vocabulary).len()
    );
    let settings = murmur::speech::Settings::load(&murmur::speech::Settings::path()?)?;
    println!(
        "Selected language: {} (checked against provider/model before capture).",
        vocabulary.language
    );
    let privacy = murmur::cleanup_config::NetworkPolicy::load(
        &murmur::cleanup_config::NetworkPolicy::path()?,
    )?;
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
        murmur::speech::Provider::DeepgramThenLocal => {
            println!(
                "Deepgram → local fallback at {}. Install/start the local engine before relying on offline recovery. No model downloads occur during capture.",
                settings.local_endpoint
            );
            Ok(())
        }
        murmur::speech::Provider::LocalWhisper => {
            println!(
                "Local Whisper at {}. No cloud credentials required. Use scripts/local-whisper.py --app murmur status for engine readiness.",
                settings.local_endpoint
            );
            Ok(())
        }
        murmur::speech::Provider::LocalStreaming => {
            println!(
                "Local streaming (Nemotron 3.5 through fotw-speechd), falling back to Local Whisper at {}. No cloud credentials required. Install the model with `murmur speech install-streaming`.",
                settings.local_endpoint
            );
            Ok(())
        }
    }
}

/// Explicit setup: the only path that downloads the streaming model.
fn install_streaming(args: &[String]) -> Result<(), String> {
    let from = match args {
        [] => None,
        [flag, dir] if flag == "--from" => Some(std::path::PathBuf::from(dir)),
        _ => return Err("Usage: murmur speech install-streaming [--from DIR]".into()),
    };
    let endpoint = murmur::local_streaming::Endpoint::shared();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot start runtime".to_string())?;
    println!("Installing the streaming model (about 1.2 GB unless --from is given)…");
    runtime.block_on(endpoint.service().install(from))?;
    println!("Installed. {}", murmur::local_streaming::ATTRIBUTION);
    Ok(())
}
