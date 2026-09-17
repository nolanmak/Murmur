//! Spec 7.2, normalization rule 2.
//!
//! > `speaker` normalizes to `S0…Sn`. When `source === 'mic'` and diarization is
//! > off, `speaker` is forced to `me`.
//!
//! Providers label speakers however they like — Deepgram sends integers,
//! ElevenLabs sends strings, and neither guarantees they start at zero or stay
//! dense. Downstream (rename-once-apply-retroactively, the augment prompt, the
//! export) assumes `S0..Sn`, so the mapping happens here, once.

use fotw_stt::{Source, SpeakerNormalizer, SpeakerRegistry};

#[test]
fn provider_labels_map_to_dense_s_indices_in_first_appearance_order() {
    let mut registry = SpeakerRegistry::new();

    assert_eq!(registry.label_for("0"), "S0");
    assert_eq!(registry.label_for("1"), "S1");
    // Stable: the same provider label always yields the same normalized label.
    assert_eq!(registry.label_for("0"), "S0");
    assert_eq!(registry.label_for("1"), "S1");
    assert_eq!(registry.label_for("2"), "S2");
    assert_eq!(registry.len(), 3);
}

#[test]
fn sparse_and_non_numeric_provider_labels_still_come_out_dense() {
    // Deepgram can start a reconnected stream at speaker 3; ElevenLabs sends
    // opaque strings. Neither may leak into the transcript.
    let mut registry = SpeakerRegistry::new();

    assert_eq!(registry.label_for("7"), "S0");
    assert_eq!(registry.label_for("speaker_a"), "S1");
    assert_eq!(registry.label_for("3"), "S2");
    assert_eq!(registry.label_for("speaker_a"), "S1");

    assert_eq!(registry.labels(), &["7", "speaker_a", "3"]);
}

#[test]
fn the_system_stream_keeps_diarized_labels() {
    let mut speakers = SpeakerNormalizer::new(Source::System, true);

    assert_eq!(speakers.normalize(Some("0")).as_deref(), Some("S0"));
    assert_eq!(speakers.normalize(Some("1")).as_deref(), Some("S1"));
    assert_eq!(speakers.normalize(Some("0")).as_deref(), Some("S0"));
}

#[test]
fn the_mic_stream_without_diarization_is_forced_to_me() {
    // The two-stream default (spec 7.5) exists precisely so the mic stream can
    // skip diarization: whoever is on this microphone is the user, definitionally.
    let mut speakers = SpeakerNormalizer::new(Source::Mic, false);

    assert_eq!(speakers.normalize(None).as_deref(), Some("me"));

    // Even if the provider *did* send a label, "me" wins. A diarizer that
    // decides the user is two people must not fragment their own transcript.
    assert_eq!(speakers.normalize(Some("0")).as_deref(), Some("me"));
    assert_eq!(speakers.normalize(Some("4")).as_deref(), Some("me"));
}

#[test]
fn the_mic_stream_with_diarization_on_keeps_provider_labels() {
    // Mixed-mono mode and speakerphone-in-a-room mode both put more than one
    // human on the microphone, so "me" would be a lie there.
    let mut speakers = SpeakerNormalizer::new(Source::Mic, true);

    assert_eq!(speakers.normalize(Some("0")).as_deref(), Some("S0"));
    assert_eq!(speakers.normalize(Some("1")).as_deref(), Some("S1"));
}

#[test]
fn an_absent_provider_label_stays_none_when_it_is_not_the_mic() {
    // OpenAI streaming reports no speaker at all. `None` must survive as `None`
    // rather than being invented into "S0".
    let mut speakers = SpeakerNormalizer::new(Source::System, false);
    assert_eq!(speakers.normalize(None), None);

    let mut diarized = SpeakerNormalizer::new(Source::System, true);
    assert_eq!(diarized.normalize(None), None);
}

#[test]
fn integer_provider_labels_normalize_the_same_as_their_string_form() {
    // Deepgram's `speaker` is a JSON number; the adapter must not have to
    // stringify it by hand and risk `0` and `"0"` becoming two speakers.
    let mut speakers = SpeakerNormalizer::new(Source::System, true);

    assert_eq!(speakers.normalize_index(Some(0)).as_deref(), Some("S0"));
    assert_eq!(speakers.normalize(Some("0")).as_deref(), Some("S0"));
    assert_eq!(speakers.normalize_index(Some(1)).as_deref(), Some("S1"));
    assert_eq!(speakers.registry().len(), 2);
}

#[test]
fn the_forced_me_label_is_the_exact_spec_string() {
    // Downstream matches on this literally; "Me" or "you" would break the
    // me-vs-them split that the whole two-stream design pays for.
    assert_eq!(fotw_stt::speaker::ME, "me");
}
