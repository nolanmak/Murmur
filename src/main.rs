fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let result = match command.as_str() {
        "--help" | "help" => {
            println!(
                "Murmur — voice dictation\n\nrun     Launch the desktop app (Linux: experimental review/copy)\ndoctor  Report credential source without printing the key\n\nHold Control to dictate; release to insert; Esc cancels.\nmacOS: scripts/build-app.sh. Linux: scripts/install-linux.sh."
            );
            Ok(())
        }
        "doctor" => murmur::config::load().map(|c| {
            println!(
                "Deepgram credential available via {} (value hidden).",
                c.source
            )
        }),
        #[cfg(target_os = "macos")]
        "review-preview" => murmur::platform::macos::preview_remote_review(),
        #[cfg(target_os = "linux")]
        "linux-devices" => murmur::platform::linux::microphones()
            .map(|m| println!("{}", serde_json::to_string(&m).unwrap())),
        #[cfg(target_os = "linux")]
        "linux-session" => {
            murmur::platform::linux::session(&std::env::args().nth(2).unwrap_or_default())
        }
        "run" => {
            #[cfg(target_os = "macos")]
            {
                murmur::platform::macos::run()
            }
            #[cfg(target_os = "linux")]
            {
                murmur::platform::linux::run()
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
