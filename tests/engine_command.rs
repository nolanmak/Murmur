//! Murmur #96: switching the transcription engine by voice. A command is the
//! whole utterance or nothing, and a switch never lands on an engine that
//! cannot transcribe.
use murmur::engine_command::{Command, Decision, Target, decide, parse};
use murmur::speech::Provider;

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
        Some(&Provider::DeepgramThenLocal),
        None,
        false,
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
            false,
            &down(),
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
            false,
            &down()
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
            true,
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
            true,
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
            false,
            &ready()
        ),
        Decision::Switch {
            to: Provider::Deepgram,
            ..
        }
    ));
    assert!(matches!(
        decide(Command::Undo, Some(&Provider::Deepgram), None, false, &ready()),
        Decision::Report(ref m) if m.contains("Nothing to undo")
    ));
    assert!(matches!(
        decide(
            Command::Undo,
            Some(&Provider::Deepgram),
            Some(&Provider::LocalWhisper),
            false,
            &down()
        ),
        Decision::Refuse(_)
    ));
}

#[test]
fn which_and_already_selected_only_report() {
    assert!(matches!(
        decide(Command::Which, Some(&Provider::DeepgramThenLocal), None, false, &ready()),
        Decision::Report(ref m) if m.contains("Deepgram") && m.contains("local")
    ));
    assert!(matches!(
        decide(Command::Use(Target::Local), Some(&Provider::LocalWhisper), None, false, &ready()),
        Decision::Report(ref m) if m.contains("Already")
    ));
}
