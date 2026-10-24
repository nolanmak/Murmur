fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let result = match command.as_str() {
        "--help" | "help" => {
            println!(
                "Murmur — macOS hold-Control dictation\n\nrun     Launch the menu-bar app (use the .app bundle)\ndoctor  Report provider configuration without printing secrets\nspeech  Select local [URL] or deepgram transcription\n\nHold Control to dictate; release to insert; Esc cancels.\nRun scripts/build-app.sh and launch the resulting .app for capture."
            );
            Ok(())
        }
        "speech" => configure_speech(),
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
        Some("deepgram") => settings.provider = murmur::speech::Provider::Deepgram,
        _ => return Err("Usage: murmur speech local [http://127.0.0.1:17863] | deepgram".into()),
    }
    settings.save(&path)?;
    println!("Saved transcription provider; restart running app to reload CLI changes.");
    Ok(())
}
fn doctor() -> Result<(), String> {
    let settings = murmur::speech::Settings::load(&murmur::speech::Settings::path()?)?;
    match settings.provider {
        murmur::speech::Provider::Deepgram => murmur::config::load().map(|c| {
            println!(
                "Deepgram credential available via {} (value hidden).",
                c.source
            )
        }),
        murmur::speech::Provider::LocalWhisper => {
            println!(
                "Local Whisper at {}. No cloud credentials required. Use scripts/local-whisper.py --app murmur status for engine readiness.",
                settings.local_endpoint
            );
            Ok(())
        }
    }
}
