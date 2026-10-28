//! Murmur #96: switching the transcription engine by voice. A command is the
//! whole utterance or nothing, and a switch never lands on an engine that
//! cannot transcribe.
use murmur::engine_command::{Command, Decision, Target, decide, parse};
use murmur::speech::{LocalEngine, Provider};

#[test]
fn whole_utterance_commands_are_recognized_across_transcription_styles() {
    for spoken in [
        "Murmur, use local.",
        "murmur use local",
        "Murmur. Use local!",
        "  MURMUR,   USE   LOCAL?  ",
        "Murmer, switch to local.",
    ] {
        assert_eq!(parse(spoken), Some(Command::Use(Target::Local)), "{spoken}");
    }
    assert_eq!(
        parse("Murmur, use Deepgram."),
        Some(Command::Use(Target::Deepgram))
    );
    assert_eq!(
        parse("Murmur, use deep gram"),
        Some(Command::Use(Target::Deepgram))
    );
    assert_eq!(
        parse("Murmur, use cloud."),
        Some(Command::Use(Target::Deepgram))
    );
    assert_eq!(
        parse("Murmur, use backup."),
        Some(Command::Use(Target::Backup))
    );
    assert_eq!(
        parse("Murmur, use Deepgram with backup."),
        Some(Command::Use(Target::Backup))
    );
    assert_eq!(parse("Murmur, undo."), Some(Command::Undo));
    assert_eq!(parse("Murmur, which engine?"), Some(Command::Which));
}

#[test]
fn a_command_inside_ordinary_dictation_is_just_dictation() {
    for spoken in [
        "I told him to use local for the demo.",
        "Switch to local.",
        "use local",
        "Murmur, use local and then send the email.",
        "Can you ask Murmur to use local?",
        "Murmur, use local. Murmur, use local.",
        "Murmur",
        "",
        "Murmur, use the local one I think",
    ] {
        assert_eq!(
            parse(spoken),
            None,
            "{spoken:?} must be dictated, not obeyed"
        );
    }
}

fn ready() -> Result<(), String> {
    Ok(())
}
fn down() -> Result<(), String> {
    Err("Local Whisper is not ready".into())
}

#[test]
fn switching_to_a_ready_engine_saves_it_and_says_so() {
    let d = decide(
        Command::Use(Target::Local),
        Some(&Provider::DeepgramThenWhisper),
        None,
        LocalEngine::Whisper,
        false,
        &ready(),
        &ready(),
    );
    match d {
        Decision::Switch { to, message } => {
            assert_eq!(to, Provider::LocalWhisper);
            assert!(message.contains("Local"), "{message}");
        }
        other => panic!("expected a switch, got {other:?}"),
    }
}

#[test]
fn an_unready_local_engine_is_refused_and_the_current_choice_kept() {
    for target in [Target::Local, Target::Backup] {
        let d = decide(
            Command::Use(target),
            Some(&Provider::Deepgram),
            None,
            LocalEngine::Whisper,
            false,
            &down(),
            &ready(),
        );
        match d {
            Decision::Refuse(message) => {
                assert!(message.contains("staying on Deepgram"), "{message}")
            }
            other => panic!("{target:?}: expected a refusal, got {other:?}"),
        }
    }
    // Cloud-only needs nothing local.
    assert!(matches!(
        decide(
            Command::Use(Target::Deepgram),
            Some(&Provider::LocalWhisper),
            None,
            LocalEngine::Whisper,
            false,
            &down(),
            &ready()
        ),
        Decision::Switch {
            to: Provider::Deepgram,
            ..
        }
    ));
}

#[test]
fn offline_all_blocks_voice_switches_to_the_cloud() {
    for target in [Target::Deepgram, Target::Backup] {
        let d = decide(
            Command::Use(target),
            Some(&Provider::LocalWhisper),
            None,
            LocalEngine::Whisper,
            true,
            &ready(),
            &ready(),
        );
        assert!(
            matches!(d, Decision::Refuse(ref m) if m.contains("Offline")),
            "{target:?}: {d:?}"
        );
    }
    assert!(matches!(
        decide(
            Command::Use(Target::Local),
            Some(&Provider::Deepgram),
            None,
            LocalEngine::Whisper,
            true,
            &ready(),
            &ready()
        ),
        Decision::Switch {
            to: Provider::LocalWhisper,
            ..
        }
    ));
}

#[test]
fn undo_returns_to_the_previous_engine_and_obeys_the_same_rules() {
    assert!(matches!(
        decide(
            Command::Undo,
            Some(&Provider::LocalWhisper),
            Some(&Provider::Deepgram),
            LocalEngine::Whisper,
            false,
            &ready(),
            &ready()
        ),
        Decision::Switch {
            to: Provider::Deepgram,
            ..
        }
    ));
    assert!(matches!(
        decide(Command::Undo, Some(&Provider::Deepgram), None, LocalEngine::Whisper, false, &ready(),&ready()),
        Decision::Report(ref m) if m.contains("Nothing to undo")
    ));
    assert!(matches!(
        decide(
            Command::Undo,
            Some(&Provider::Deepgram),
            Some(&Provider::LocalWhisper),
            LocalEngine::Whisper,
            false,
            &down(),
            &ready()
        ),
        Decision::Refuse(_)
    ));
}

#[test]
fn which_and_already_selected_only_report() {
    assert!(matches!(
        decide(Command::Which, Some(&Provider::DeepgramThenWhisper), None, LocalEngine::Whisper, false, &ready(),&ready()),
        Decision::Report(ref m) if m.contains("Deepgram") && m.contains("Whisper backup")
    ));
    assert!(matches!(
        decide(Command::Use(Target::Local), Some(&Provider::LocalWhisper), None, LocalEngine::Whisper, false, &ready(),&ready()),
        Decision::Report(ref m) if m.contains("Already")
    ));
}

#[test]
fn a_cloud_switch_without_credentials_is_refused() {
    let missing: Result<(), String> = Err("No Deepgram key".into());
    let d = decide(
        Command::Use(Target::Deepgram),
        Some(&Provider::LocalWhisper),
        None,
        LocalEngine::Whisper,
        false,
        &ready(),
        &missing,
    );
    assert!(
        matches!(d, Decision::Refuse(ref m) if m.contains("Deepgram key") && m.contains("staying on Local Whisper")),
        "{d:?}"
    );

    // Undo back to Deepgram obeys the same rule.
    let d = decide(
        Command::Undo,
        Some(&Provider::LocalWhisper),
        Some(&Provider::Deepgram),
        LocalEngine::Whisper,
        false,
        &ready(),
        &missing,
    );
    assert!(matches!(d, Decision::Refuse(_)), "{d:?}");
    // Backup falls back to local without a key, so it is not blocked.
    assert!(matches!(
        decide(
            Command::Use(Target::Backup),
            Some(&Provider::LocalWhisper),
            None,
            LocalEngine::Whisper,
            false,
            &ready(),
            &missing
        ),
        Decision::Switch { .. }
    ));
}

#[test]
fn which_names_local_whisper_while_offline_all_overrides_the_saved_engine() {
    let d = decide(
        Command::Which,
        Some(&Provider::Deepgram),
        None,
        LocalEngine::Whisper,
        true,
        &ready(),
        &ready(),
    );
    assert!(
        matches!(d, Decision::Report(ref m) if m.starts_with("Using Local Whisper") && m.contains("Offline-all")),
        "{d:?}"
    );
}
