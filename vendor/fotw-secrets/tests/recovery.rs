//! Issue #38: the Recovery Key, tested as a cryptographic object.
//!
//! The library-level round trip — encrypt normally, destroy the keychain entry,
//! open with the Recovery Key alone — lives in `fotwd/tests/recovery.rs`,
//! because it needs SQLCipher. This file tests the primitive underneath it.
//!
//! # What this file is guarding against
//!
//! This codebase has repeatedly shipped tests that passed against
//! implementations doing nothing — including one that grepped an *encrypted*
//! database for a plaintext phrase. Crypto is the easiest place in the tree to
//! write that test, because "it round-tripped" is true of `fn wrap(x) { x }`.
//! So every positive assertion here is paired with a negative one that would
//! fail against a no-op:
//!
//! * unwrapping succeeds **and** a different Recovery Key fails;
//! * the blob round-trips **and** the sealed bytes differ from the master key;
//! * a new Recovery Key produces a different blob **and** the same one under a
//!   fresh salt does too, so "it changed" is not just the nonce moving.

use std::path::Path;

use fotw_secrets::recovery::{
    KdfParams, MasterKeyBytes, RECOVERY_KEY_BYTES, RecoveryKey, WrappedMasterKey, blob_path_for,
};

/// Cheap Argon2 parameters, for tests only.
///
/// The shipped defaults are 64 MiB × 3 passes, which is ~200 ms *per call*. A
/// suite that unwraps thirty times would spend six seconds in the KDF for no
/// added coverage: the cost parameters are an input to Argon2, not to the
/// wrapping logic, and the one test that cares about the real numbers asserts
/// them directly rather than paying for them.
fn fast_kdf() -> KdfParams {
    KdfParams {
        m_cost_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

fn master(fill: u8) -> MasterKeyBytes {
    MasterKeyBytes::new([fill; 32])
}

fn key(fill: u8) -> RecoveryKey {
    RecoveryKey::from_bytes([fill; RECOVERY_KEY_BYTES])
}

// ------------------------------------------------------------------ encoding

/// The property the whole encoding exists for: what is displayed is what can
/// be typed back.
#[test]
fn a_displayed_key_parses_back_to_the_same_bytes() {
    for seed in 0u8..32 {
        let original = RecoveryKey::from_bytes([seed.wrapping_mul(37); RECOVERY_KEY_BYTES]);
        let shown = original.display_string().unwrap();
        let typed_back = RecoveryKey::parse(shown.expose()).unwrap();
        assert!(
            original.ct_eq(&typed_back),
            "round trip lost the key at seed {seed}"
        );
    }
}

/// Randomly generated keys, not just constant fills — a byte-order bug in the
/// 8-to-5-bit conversion survives `[0xAB; 16]` and dies here.
#[test]
fn freshly_generated_keys_round_trip() {
    for _ in 0..64 {
        let original = RecoveryKey::generate().unwrap();
        let shown = original.display_string().unwrap();
        let back = RecoveryKey::parse(shown.expose()).unwrap();
        assert!(original.ct_eq(&back));
    }
}

#[test]
fn the_display_form_is_grouped_and_prefixed() {
    let shown = key(0x11).display_string().unwrap();
    let text = shown.expose();

    assert!(text.starts_with("fotw1-"), "no prefix: {text}");
    let groups: Vec<&str> = text.strip_prefix("fotw1-").unwrap().split('-').collect();
    assert_eq!(groups.len(), 8, "expected eight groups: {text}");
    for g in &groups {
        assert_eq!(g.len(), 4, "group is not four characters: {text}");
    }
    assert_eq!(text.len(), 5 + 8 * 5, "unexpected total length: {text}");
    assert_eq!(
        text.to_lowercase(),
        *text,
        "the canonical form must be lowercase: {text}"
    );
}

/// The alphabet is the point. `1`, `b`, `i` and `o` must never appear in the
/// body, because each is the confusable twin of a character that does.
#[test]
fn the_body_never_contains_a_confusable_character() {
    for _ in 0..200 {
        let shown = RecoveryKey::generate().unwrap().display_string().unwrap();
        let body: String = shown
            .expose()
            .strip_prefix("fotw1-")
            .unwrap()
            .replace('-', "");
        for bad in ['1', 'b', 'i', 'o'] {
            assert!(
                !body.contains(bad),
                "the encoding emitted a confusable `{bad}`: {}",
                shown.expose()
            );
        }
    }
}

/// Everything a human does to a written-down key between the card and the
/// keyboard.
#[test]
fn presentation_differences_are_forgiven() {
    let original = RecoveryKey::generate().unwrap();
    let shown = original.display_string().unwrap();
    let canonical = shown.expose();
    let body = canonical.strip_prefix("fotw1-").unwrap();

    let variants = [
        canonical.to_uppercase(),
        canonical.replace('-', " "),
        canonical.replace('-', ""),
        format!("  {canonical}\n"),
        format!("fotw1 {}", body.replace('-', " ")),
        canonical.replace('-', "_"),
    ];

    for v in &variants {
        let parsed = RecoveryKey::parse(v)
            .unwrap_or_else(|e| panic!("rejected a legitimate presentation {v:?}: {e}"));
        assert!(original.ct_eq(&parsed), "wrong bytes from {v:?}");
    }
}

/// The repair map. bech32's alphabet excludes `1`, `b`, `i` and `o`, so within
/// the body each of these characters has exactly one thing it could have been.
#[test]
fn confusable_characters_are_repaired_not_rejected() {
    let original = RecoveryKey::generate().unwrap();
    let shown = original.display_string().unwrap();
    let body: String = shown
        .expose()
        .strip_prefix("fotw1-")
        .unwrap()
        .replace('-', "");

    // `0` written down and read back as `O`; `l` read back as `1` or `I`.
    let mangled: String = body
        .chars()
        .map(|c| match c {
            '0' => 'O',
            'l' => '1',
            other => other,
        })
        .collect();

    if mangled == body {
        // Nothing to repair in this particular key; the next assertion below
        // still covers the mechanism.
        return;
    }

    let parsed = RecoveryKey::parse(&format!("fotw1{mangled}"))
        .expect("a key mistyped only in confusable characters must still parse");
    assert!(original.ct_eq(&parsed));
}

/// The same, against a fixed key so the test always exercises the repair
/// rather than depending on which characters the CSPRNG happened to produce.
#[test]
fn every_confusable_substitution_is_repaired() {
    // Find a key whose display form contains both a `0` and an `l`.
    let mut chosen = None;
    for seed in 0u8..=255 {
        let k = RecoveryKey::from_bytes([seed; RECOVERY_KEY_BYTES]);
        let s = k.display_string().unwrap();
        let body = s.expose().strip_prefix("fotw1-").unwrap().replace('-', "");
        if body.contains('0') && body.contains('l') {
            chosen = Some((k, body));
            break;
        }
    }
    let (k, body) = chosen.expect("no fill byte produced a body with both `0` and `l`");

    for (from, to) in [('0', 'O'), ('l', '1'), ('l', 'I'), ('0', 'o')] {
        let mangled = body.replace(from, &to.to_string());
        let parsed = RecoveryKey::parse(&format!("fotw1-{mangled}"))
            .unwrap_or_else(|e| panic!("`{from}` -> `{to}` was not repaired: {e}"));
        assert!(
            k.ct_eq(&parsed),
            "`{from}` -> `{to}` decoded to the wrong key"
        );
    }
}

/// A single wrong character must be *detected*. This is the entire argument for
/// a checksum: without one, a typo produces a plausible-looking key that
/// unwraps to garbage and then makes SQLCipher say "file is not a database".
#[test]
fn a_single_character_typo_is_always_caught() {
    // bech32's alphabet, minus nothing: any of these could be typed by mistake.
    const ALPHABET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

    let mut checked = 0usize;
    for seed in 0u8..8 {
        let k =
            RecoveryKey::from_bytes([seed.wrapping_mul(53).wrapping_add(7); RECOVERY_KEY_BYTES]);
        let shown = k.display_string().unwrap();
        let flat: String = shown.expose().replace('-', "");
        let body = &flat[5..];

        for pos in 0..body.len() {
            for &replacement in ALPHABET {
                let replacement = replacement as char;
                if body.as_bytes()[pos] as char == replacement {
                    continue;
                }
                let mut mangled: Vec<u8> = body.as_bytes().to_vec();
                mangled[pos] = replacement as u8;
                let candidate = format!("fotw1{}", String::from_utf8(mangled).unwrap());

                let err = RecoveryKey::parse(&candidate).expect_err(
                    "a one-character typo produced a key the checksum accepted; \
                     that key would unwrap to garbage and be reported as a corrupt database",
                );
                assert!(err.is_malformed(), "wrong error class for a typo: {err:?}");
                checked += 1;
            }
        }
    }
    assert!(
        checked > 5_000,
        "only checked {checked} single-character typos"
    );
}

/// Two swapped adjacent characters — the other classic transcription error.
#[test]
fn a_transposition_is_always_caught() {
    let mut checked = 0usize;
    for seed in 0u8..16 {
        let k =
            RecoveryKey::from_bytes([seed.wrapping_mul(29).wrapping_add(3); RECOVERY_KEY_BYTES]);
        let flat: String = k.display_string().unwrap().expose().replace('-', "");
        let body = flat.as_bytes()[5..].to_vec();

        for pos in 0..body.len() - 1 {
            if body[pos] == body[pos + 1] {
                continue;
            }
            let mut swapped = body.clone();
            swapped.swap(pos, pos + 1);
            let candidate = format!("fotw1{}", String::from_utf8(swapped).unwrap());
            let err = RecoveryKey::parse(&candidate)
                .expect_err("a transposition was accepted as a valid key");
            assert!(err.is_malformed(), "{err:?}");
            checked += 1;
        }
    }
    assert!(checked > 300, "only checked {checked} transpositions");
}

/// A dropped or added character changes the length, and the length check fires
/// before any crypto does — so the user is told they are short a character
/// rather than being told their key is wrong.
#[test]
fn a_dropped_or_duplicated_character_is_caught() {
    let k = RecoveryKey::generate().unwrap();
    let flat: String = k.display_string().unwrap().expose().replace('-', "");

    // The *message* is asserted, not just the error class. The checksum would
    // reject a wrong-length key anyway, so a test that only checked
    // `is_malformed` passes with the length check deleted — and the user is
    // then told "a character is wrong" when the truth is "a character is
    // missing", which are different things to go looking for on a hand-written
    // card.
    for (candidate, expected) in [
        (flat[..flat.len() - 1].to_owned(), "one is missing"),
        (format!("{flat}q"), "there is one too many"),
    ] {
        let err = RecoveryKey::parse(&candidate).expect_err("a length error was accepted");
        assert!(err.is_malformed(), "{err:?}");
        assert!(
            err.to_string().contains(expected),
            "expected the message to say `{expected}`, got: {err}"
        );
    }
}

#[test]
fn garbage_and_wrong_prefixes_are_rejected_as_malformed() {
    for candidate in [
        "",
        "   ",
        "hello world",
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        "fotw",
        "fotw1",
        "notfotw1qpzry9x8gf2tvdw0s3jn54khce6mua7lqqqq",
    ] {
        let err = RecoveryKey::parse(candidate)
            .err()
            .unwrap_or_else(|| panic!("{candidate:?} was accepted as a Recovery Key"));
        assert!(err.is_malformed(), "{candidate:?} gave {err:?}");
    }
}

/// The error text has to steer the reader away from "my database is broken".
#[test]
fn the_malformed_message_says_nothing_was_tried() {
    let err = RecoveryKey::parse("fotw1-zzzz-zzzz-zzzz-zzzz-zzzz-zzzz-zzzz-zzzz").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("Nothing was tried"), "unhelpful: {text}");
    assert!(!text.to_lowercase().contains("corrupt"), "alarming: {text}");
}

// ------------------------------------------------------- confirmation groups

#[test]
fn the_confirmation_challenge_accepts_the_right_group_and_rejects_others() {
    let k = RecoveryKey::generate().unwrap();
    let shown = k.display_string().unwrap();
    let groups: Vec<&str> = shown
        .expose()
        .strip_prefix("fotw1-")
        .unwrap()
        .split('-')
        .collect();

    for (i, g) in groups.iter().enumerate() {
        assert!(k.group_matches(i, g).unwrap(), "group {i} was rejected");
        assert!(
            k.group_matches(i, &g.to_uppercase()).unwrap(),
            "group {i} rejected in upper case"
        );
        assert!(
            k.group_matches(i, &format!("  {g} ")).unwrap(),
            "group {i} rejected with whitespace"
        );
        assert!(
            !k.group_matches(i, "zzzz").unwrap(),
            "group {i} accepted junk"
        );
    }

    // A group typed into the wrong slot must not pass — otherwise "confirm two
    // groups" degrades to "type any four characters from the key".
    let distinct: std::collections::BTreeSet<&&str> = groups.iter().collect();
    if distinct.len() == groups.len() {
        assert!(
            !k.group_matches(0, groups[1]).unwrap(),
            "group 1 was accepted as group 0"
        );
    }

    assert!(
        k.group_matches(8, "qqqq").is_err(),
        "index 8 does not exist"
    );
}

// ------------------------------------------------------------------ wrapping

/// The core property: seal, open, get the same 32 bytes back.
#[test]
fn a_wrapped_master_key_unwraps_to_the_same_bytes() {
    let mk = master(0x5A);
    let rk = RecoveryKey::generate().unwrap();

    let blob = WrappedMasterKey::wrap(&mk, &rk, fast_kdf()).unwrap();
    let out = blob.unwrap_master(&rk, Path::new("test.recovery")).unwrap();

    assert!(mk.ct_eq(&out), "unwrap did not return the master key");
}

/// …and the negative that makes the positive mean something. A no-op `wrap`
/// passes the test above and fails this one.
#[test]
fn a_different_recovery_key_does_not_unwrap_it() {
    let mk = master(0x5A);
    let blob = WrappedMasterKey::wrap(&mk, &key(0x01), fast_kdf()).unwrap();

    let err = blob
        .unwrap_master(&key(0x02), Path::new("test.recovery"))
        .expect_err("a foreign Recovery Key opened the blob");

    assert!(err.is_wrong_key(), "wrong error class: {err:?}");
    assert!(!err.is_malformed(), "a valid key was reported as a typo");
}

/// The message for a wrong key must not send the user hunting a corruption bug.
/// This is the exact failure mode issue #38 calls out: SQLCipher's own answer
/// to a bad key is "file is not a database".
#[test]
fn the_wrong_key_message_denies_that_the_database_is_corrupt() {
    let blob = WrappedMasterKey::wrap(&master(1), &key(1), fast_kdf()).unwrap();
    let err = blob
        .unwrap_master(&key(2), Path::new("/data/db.sqlite3.recovery"))
        .unwrap_err();
    let text = err.to_string();

    assert!(
        text.contains("NOT corrupt"),
        "the message does not rule out corruption: {text}"
    );
    assert!(
        text.contains("/data/db.sqlite3.recovery"),
        "the message does not name the file: {text}"
    );
    assert!(
        !text.contains("file is not a database"),
        "the message repeats SQLCipher's misleading wording: {text}"
    );
}

/// The sealed bytes must not *be* the master key. The cheapest possible fake
/// implementation stores the key verbatim and passes every round-trip test in
/// this file; this is the one that catches it.
#[test]
fn the_master_key_is_not_present_in_the_serialised_blob() {
    // A recognisable pattern, so a substring search is meaningful.
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = 0xE0 ^ (i as u8);
    }
    let mk = MasterKeyBytes::new(bytes);
    let rk = RecoveryKey::generate().unwrap();

    let json = WrappedMasterKey::wrap(&mk, &rk, fast_kdf())
        .unwrap()
        .to_json();

    // Both as raw bytes and as the hex the file is written in.
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        !json.contains(&hex),
        "the blob contains the master key in hex:\n{json}"
    );
    assert!(
        !contains(json.as_bytes(), &bytes),
        "the blob contains the master key verbatim"
    );

    // And the Recovery Key must not be in there either — the file would then
    // be a complete copy of the secret it is supposed to be sealed against.
    let rk_hex: String = rk.expose().iter().map(|b| format!("{b:02x}")).collect();
    assert!(
        !json.contains(&rk_hex),
        "the blob contains the Recovery Key"
    );
    assert!(!contains(json.as_bytes(), rk.expose()));

    // Positive control: the search would have found the key if it were there.
    let planted = format!("{json}{hex}");
    assert!(
        planted.contains(&hex),
        "the substring search is broken, so its negative result proves nothing"
    );
}

/// Rotating the Recovery Key must produce a genuinely different blob.
#[test]
fn a_new_recovery_key_produces_a_different_blob() {
    let mk = master(0x77);
    let a = WrappedMasterKey::wrap(&mk, &key(1), fast_kdf()).unwrap();
    let b = WrappedMasterKey::wrap(&mk, &key(2), fast_kdf()).unwrap();

    assert_ne!(a, b, "two different Recovery Keys sealed to the same blob");
    assert_ne!(a.to_json(), b.to_json());

    // Each opens under its own key and neither opens under the other's.
    let p = Path::new("x.recovery");
    assert!(mk.ct_eq(&a.unwrap_master(&key(1), p).unwrap()));
    assert!(mk.ct_eq(&b.unwrap_master(&key(2), p).unwrap()));
    assert!(a.unwrap_master(&key(2), p).is_err());
    assert!(b.unwrap_master(&key(1), p).is_err());
}

/// Wrapping the *same* key twice must also differ — salt and nonce are fresh
/// per call. Otherwise a blob would leak whether two libraries share a key.
#[test]
fn wrapping_the_same_pair_twice_is_not_deterministic() {
    let mk = master(0x33);
    let rk = key(9);
    let a = WrappedMasterKey::wrap(&mk, &rk, fast_kdf()).unwrap();
    let b = WrappedMasterKey::wrap(&mk, &rk, fast_kdf()).unwrap();

    assert_ne!(
        a, b,
        "wrap is deterministic, so salt or nonce is not random"
    );
    // ...and both still open.
    let p = Path::new("x.recovery");
    assert!(mk.ct_eq(&a.unwrap_master(&rk, p).unwrap()));
    assert!(mk.ct_eq(&b.unwrap_master(&rk, p).unwrap()));
}

/// The previous test only proves *something* changed. This one names what:
/// both the salt and the nonce must be drawn fresh, individually.
///
/// A fixed nonce with a random salt still produces different blobs, so the
/// assertion above survives it — which is precisely the mutation this test
/// exists to kill. A fixed nonce under a *reused* KEK is a stream-cipher
/// keystream reuse, and rotation re-derives the same KEK whenever the salt is
/// also fixed.
#[test]
fn every_wrap_draws_a_fresh_salt_and_a_fresh_nonce() {
    let mk = master(0x21);
    let rk = key(11);
    let a = WrappedMasterKey::wrap(&mk, &rk, fast_kdf())
        .unwrap()
        .to_json();
    let b = WrappedMasterKey::wrap(&mk, &rk, fast_kdf())
        .unwrap()
        .to_json();

    assert_ne!(
        field(&a, "salt"),
        field(&b, "salt"),
        "the salt is fixed, so every install derives the same KEK from the same key"
    );
    assert_ne!(
        field(&a, "nonce"),
        field(&b, "nonce"),
        "the nonce is fixed, so re-wrapping under one KEK reuses a keystream"
    );

    // ...and neither is a constant that merely happens to differ from the other
    // field, e.g. a zero-filled buffer.
    for name in ["salt", "nonce"] {
        let v = field(&a, name);
        assert!(v.chars().any(|c| c != '0'), "{name} is all zeroes: {v}");
        assert!(v.chars().any(|c| c != 'f'), "{name} is all ones: {v}");
    }
}

/// Rotation does not touch the master key — that is what makes it cheap.
#[test]
fn rotation_preserves_the_master_key_so_the_database_is_never_re_encrypted() {
    let mk = master(0x42);
    let old = WrappedMasterKey::wrap(&mk, &key(1), fast_kdf()).unwrap();
    let recovered = old.unwrap_master(&key(1), Path::new("x")).unwrap();

    let new_rk = RecoveryKey::generate().unwrap();
    let new = WrappedMasterKey::wrap(&recovered, &new_rk, fast_kdf()).unwrap();

    let out = new.unwrap_master(&new_rk, Path::new("x")).unwrap();
    assert!(
        mk.ct_eq(&out),
        "rotating the Recovery Key changed the master key, which would require \
         re-encrypting the whole library"
    );
}

// --------------------------------------------------------------- the on-disk

#[test]
fn the_blob_round_trips_through_json() {
    let blob = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf()).unwrap();
    let back = WrappedMasterKey::from_json(&blob.to_json(), Path::new("x")).unwrap();
    assert_eq!(blob, back);
    assert!(master(3).ct_eq(&back.unwrap_master(&key(4), Path::new("x")).unwrap()));
}

/// The file has to explain itself. Someone finding it in a backup three years
/// from now must not conclude it is their Recovery Key.
#[test]
fn the_file_says_what_it_is_and_what_it_is_not() {
    let json = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf())
        .unwrap()
        .to_json();
    let lower = json.to_lowercase();
    assert!(lower.contains("recovery key"), "no explanation:\n{json}");
    assert!(
        lower.contains("cannot open") || lower.contains("not your recovery key"),
        "the note does not say the file is useless on its own:\n{json}"
    );
    assert!(lower.contains("argon2id"), "the KDF is not named:\n{json}");
}

/// The stored parameters must be the ones actually used, or raising the cost
/// later would silently fail to raise it.
#[test]
fn the_kdf_parameters_are_stored_and_honoured() {
    let params = KdfParams {
        m_cost_kib: 256,
        t_cost: 2,
        p_cost: 1,
    };
    let blob = WrappedMasterKey::wrap(&master(3), &key(4), params).unwrap();
    assert_eq!(blob.kdf(), params);

    let back = WrappedMasterKey::from_json(&blob.to_json(), Path::new("x")).unwrap();
    assert_eq!(back.kdf(), params);
    assert!(master(3).ct_eq(&back.unwrap_master(&key(4), Path::new("x")).unwrap()));
}

/// The shipped defaults, pinned. Lowering them is a security change and should
/// look like one in a diff.
#[test]
fn the_default_parameters_are_the_ones_we_argued_for() {
    let d = KdfParams::default();
    assert_eq!(d.m_cost_kib, 65_536, "64 MiB, per RFC 9106's second option");
    assert_eq!(d.t_cost, 3);
    assert_eq!(
        d.p_cost, 1,
        "p>1 costs us wall-clock and costs a parallel attacker nothing, because \
         this argon2 build is single-threaded"
    );
}

/// A flipped bit anywhere in the sealed key must fail, not decrypt to garbage.
#[test]
fn a_bit_flip_in_the_sealed_key_is_detected() {
    let blob = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf()).unwrap();
    let json = blob.to_json();
    let sealed = field(&json, "sealed_key");

    let mut flipped = 0usize;
    for pos in 0..sealed.len() {
        // Flip one hex digit -> at least one bit of the ciphertext or tag.
        let mut chars: Vec<char> = sealed.chars().collect();
        chars[pos] = if chars[pos] == '0' { '1' } else { '0' };
        let mangled: String = chars.into_iter().collect();
        if mangled == sealed {
            continue;
        }
        let tampered = json.replace(&sealed, &mangled);

        let err = match WrappedMasterKey::from_json(&tampered, Path::new("x")) {
            Ok(b) => b
                .unwrap_master(&key(4), Path::new("x"))
                .expect_err("a tampered blob decrypted successfully")
                .to_string(),
            // Caught even earlier, by the integrity digest. Also fine.
            Err(e) => e.to_string(),
        };
        assert!(!err.is_empty());
        flipped += 1;
    }
    assert!(flipped > 90, "only tried {flipped} bit flips");
}

/// Damage to the sealed bytes must be reported as **damage**, not as a wrong
/// key. The AEAD tag alone cannot tell those apart — which is exactly why the
/// key-independent integrity digest exists, and this is the test that says the
/// digest is actually consulted.
#[test]
fn a_damaged_sealed_key_is_reported_as_damage_before_any_key_is_tried() {
    let json = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf())
        .unwrap()
        .to_json();
    let sealed = field(&json, "sealed_key");
    let mut chars: Vec<char> = sealed.chars().collect();
    chars[10] = if chars[10] == '0' { '1' } else { '0' };
    let tampered = json.replace(&sealed, &chars.into_iter().collect::<String>());

    let err = WrappedMasterKey::from_json(&tampered, Path::new("/d/db.sqlite3.recovery"))
        .expect_err("a damaged sealed key was loaded without complaint");
    assert!(
        err.is_corrupt_blob(),
        "damage was not caught before the key was tried: {err:?}"
    );
    assert!(
        err.to_string().contains("not with your"),
        "the message does not rule out the Recovery Key and the database: {err}"
    );
}

/// The same for the salt and the nonce — every field the digest claims to
/// cover has to actually be covered.
#[test]
fn damage_to_any_stored_field_is_caught() {
    for name in ["salt", "nonce", "m_cost_kib", "t_cost", "p_cost"] {
        let json = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf())
            .unwrap()
            .to_json();
        let original = field(&json, name);
        let replacement = if original.chars().all(|c| c.is_ascii_digit()) {
            format!("{}7", &original)
        } else {
            let mut chars: Vec<char> = original.chars().collect();
            chars[0] = if chars[0] == 'a' { 'b' } else { 'a' };
            chars.into_iter().collect()
        };
        let tampered = json.replace(
            &format!("\"{name}\": \"{original}\""),
            &format!("\"{name}\": \"{replacement}\""),
        );
        let tampered = tampered.replace(
            &format!("\"{name}\": {original}"),
            &format!("\"{name}\": {replacement}"),
        );
        assert_ne!(tampered, json, "the fixture for {name} did not change");

        let err = WrappedMasterKey::from_json(&tampered, Path::new("x"))
            .expect_err(&format!("damage to {name} was accepted"));
        assert!(err.is_corrupt_blob(), "{name} gave {err:?}");
    }
}

/// Truncation is a *file damage* error, not a wrong-key error: a user whose
/// backup was cut short must be told to restore the file, not to look for
/// another card.
#[test]
fn a_truncated_blob_is_reported_as_damage_not_as_a_wrong_key() {
    let json = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf())
        .unwrap()
        .to_json();
    let sealed = field(&json, "sealed_key");
    let short = json.replace(&sealed, &sealed[..sealed.len() - 4]);

    let err = WrappedMasterKey::from_json(&short, Path::new("/d/db.sqlite3.recovery"))
        .expect_err("a truncated sealed key was accepted");
    assert!(err.is_corrupt_blob(), "wrong error class: {err:?}");
    assert!(!err.is_wrong_key());
}

/// Editing the KDF parameters must not silently produce a different-but-valid
/// unwrap, and must not be reported as "you typed it wrong".
#[test]
fn editing_the_stored_parameters_is_detected() {
    let json = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf())
        .unwrap()
        .to_json();
    let downgraded = json.replace("\"t_cost\": 1", "\"t_cost\": 9");
    assert_ne!(downgraded, json, "the fixture did not actually change");

    let err = match WrappedMasterKey::from_json(&downgraded, Path::new("x")) {
        Ok(b) => b
            .unwrap_master(&key(4), Path::new("x"))
            .expect_err("a parameter-edited blob unwrapped"),
        Err(e) => e,
    };
    assert!(
        err.is_corrupt_blob() || err.is_wrong_key(),
        "a parameter edit produced {err:?}"
    );
}

#[test]
fn a_blob_that_is_not_json_at_all_is_damage() {
    for junk in ["", "{", "not json", "{\"fotw_recovery\": 1}"] {
        let err = WrappedMasterKey::from_json(junk, Path::new("/d/x.recovery"))
            .expect_err("junk was accepted as a recovery file");
        assert!(err.is_corrupt_blob(), "{junk:?} gave {err:?}");
    }
}

// ------------------------------------------------------------------ the file

#[test]
fn the_blob_writes_and_reads_back_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db.sqlite3");
    let path = blob_path_for(&db);

    let blob = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf()).unwrap();
    blob.write_to(&path).unwrap();

    assert!(path.exists(), "nothing was written to {}", path.display());
    let back = WrappedMasterKey::read_from(&path).unwrap();
    assert_eq!(blob, back);
}

#[test]
fn a_missing_file_is_its_own_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite3.recovery");
    let err = WrappedMasterKey::read_from(&path).unwrap_err();
    assert!(err.is_missing_blob(), "{err:?}");
    assert!(!err.is_corrupt_blob());
    assert!(
        err.to_string().contains("db.sqlite3"),
        "the error does not say where to put the file: {err}"
    );
}

/// The blob is not a secret in the "must never touch disk" sense — that is the
/// entire design — but it is also not something to leave world-readable.
#[cfg(unix)]
#[test]
fn the_file_is_written_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite3.recovery");
    WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf())
        .unwrap()
        .write_to(&path)
        .unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "recovery file is mode {mode:o}");
}

/// Rewriting over an existing file must leave a complete file, never a
/// half-written one: a crash mid-write would otherwise destroy the only
/// recovery path at the exact moment the user was securing it.
#[test]
fn rewriting_replaces_the_file_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite3.recovery");

    let first = WrappedMasterKey::wrap(&master(3), &key(4), fast_kdf()).unwrap();
    first.write_to(&path).unwrap();
    let second = WrappedMasterKey::wrap(&master(3), &key(5), fast_kdf()).unwrap();
    second.write_to(&path).unwrap();

    assert_eq!(WrappedMasterKey::read_from(&path).unwrap(), second);
    // No temp file left behind.
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "db.sqlite3.recovery")
        .collect();
    assert!(strays.is_empty(), "left temp files behind: {strays:?}");
}

// ------------------------------------------------------------------- helpers

/// Pull a field out of the JSON without a JSON parser, so the test does not
/// depend on the same serialiser it is checking. Handles quoted (hex) and bare
/// (numeric) values, and returns the value with its quotes, so that a caller
/// can substitute it back verbatim.
fn field(json: &str, name: &str) -> String {
    let needle = format!("\"{name}\": ");
    let start = json
        .find(&needle)
        .unwrap_or_else(|| panic!("no {name} in {json}"))
        + needle.len();
    let rest = &json[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        return quoted[..quoted.find('"').unwrap()].to_owned();
    }
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].to_owned()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}
