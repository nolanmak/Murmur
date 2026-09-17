fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let result = match command.as_str() {
        "--help" | "help" => {
            println!(
                "Text-to-speech — macOS hold-Control dictation\n\nrun     Launch the menu-bar app (use the .app bundle)\ndoctor  Report credential source without printing the key\n\nHold Control to dictate; release to insert; Esc cancels.\nRun scripts/build-app.sh and launch the resulting .app for capture."
            );
            Ok(())
        }
        "doctor" => text_to_speech::config::load().map(|c| {
            println!(
                "Deepgram credential available via {} (value hidden).",
                c.source
            )
        }),
        #[cfg(target_os = "macos")]
        "review-preview" => text_to_speech::platform::macos::preview_remote_review(),
        "run" => {
            #[cfg(target_os = "macos")]
            {
                text_to_speech::platform::macos::run()
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
