//! Turning 16 bytes into something a person can copy onto a card at 2am, and
//! back again.
//!
//! # Why bech32m, and not hex, base64, BIP-39 or Crockford base32
//!
//! The encoding is chosen for one job: **a human reads a string off a screen,
//! writes it on paper, and types it back months later, under stress, possibly
//! from a photograph.** Every property below follows from that and from nothing
//! else. It is not a wire format; nothing but a person ever produces it.
//!
//! *Hex and base64 are out on the alphabet alone.* Base64 contains `0`/`O`,
//! `1`/`l`/`I`, `+`/`/` and is case-significant, which makes it about the worst
//! possible thing to hand-copy. Hex is unambiguous but takes 32 characters for
//! 16 bytes and carries **no checksum at all**, so a typo becomes a wrong key
//! that decodes fine — and a wrong key, further down, becomes SQLCipher saying
//! "file is not a database". That is the exact failure this whole feature
//! exists to prevent.
//!
//! *BIP-39 word lists* are genuinely good for transcription and are the serious
//! alternative. They lose on two counts here: 12 words is ~90 characters of
//! screen and paper against our 45, and an English word list embeds a 2 KB
//! table plus a language question we do not want to answer for a tool whose
//! users are not all anglophone. BIP-39's checksum is also weaker than what we
//! get below — 4 bits for 128 bits of entropy, so roughly 1 in 16 single-word
//! errors sails through.
//!
//! *Crockford base32* is the closest competitor and was the first choice. Its
//! alphabet is excellent (no `I`, `L`, `O`, `U`) and it defines confusable
//! folding. It loses on the checksum: Crockford's optional check symbol is a
//! single mod-37 character, which detects a single substitution but gives no
//! guarantee at all about two errors or a transposition — and transposition is
//! the single most common hand-copying mistake.
//!
//! **bech32m** (BIP-350) wins because its checksum is a BCH code over GF(32)
//! with a *proved* guarantee rather than a heuristic: for strings up to 89
//! characters it detects **any** pattern of four or fewer errors, always, and
//! misses longer patterns with probability below 2⁻³⁰. Its alphabet
//! `qpzry9x8gf2tvdw0s3jn54khce6mua7l` excludes `1`, `b`, `i` and `o`, which is
//! the property the repair map below depends on. And it is case-insensitive.
//!
//! bech32**m** and not bech32: BIP-350 exists because the original constant of
//! 1 made an error in the final character of the data part undetectable when
//! combined with length changes. Our strings are fixed-length, so we would
//! probably get away with it — "probably get away with it" is not a thing to
//! write into a data-recovery path.
//!
//! # The repair map
//!
//! Because `1`, `b`, `i` and `o` **cannot legally appear** in the body, each
//! classic confusable pair has exactly one member that can:
//!
//! | written | can only have meant |
//! |---|---|
//! | `O`, `o` | `0` |
//! | `I`, `i`, `1` | `l` |
//!
//! So substituting them is a repair, not a guess: there is no reading under
//! which information is lost. And if the repair is wrong for some reason we
//! have not thought of, the BCH checksum still rejects the result — the repair
//! can only ever turn a certain failure into a possible success.
//!
//! `1` is only repaired **inside the body**, never in the `fotw1` prefix, where
//! it is bech32's separator and legitimately a `1`.

use super::{GROUP_COUNT, GROUP_LEN, HRP, RECOVERY_KEY_BYTES, RecoveryError, ct_eq_bytes};

use bech32::{Bech32m, Hrp};

/// `fotw1`: the human-readable part plus bech32's separator.
const PREFIX: &str = "fotw1";

/// Characters after the prefix: 26 data + 6 checksum.
const BODY_LEN: usize = GROUP_COUNT * GROUP_LEN;

/// Encode as a grouped `fotw1-xxxx-…-xxxx` string.
///
/// Grouped because unbroken 32-character runs are where people lose their
/// place; four is the group size a phone number uses for the same reason.
pub(super) fn encode_grouped(key: &[u8; RECOVERY_KEY_BYTES]) -> Result<String, RecoveryError> {
    let hrp = Hrp::parse(HRP).map_err(|e| RecoveryError::Crypto(format!("bad hrp: {e}")))?;
    let flat = bech32::encode_lower::<Bech32m>(hrp, key)
        .map_err(|e| RecoveryError::Crypto(format!("bech32m encode failed: {e}")))?;

    // A layout change here would silently break every card already written, so
    // the invariant is asserted rather than assumed.
    let body = flat
        .strip_prefix(PREFIX)
        .ok_or_else(|| RecoveryError::Crypto("bech32m produced an unexpected prefix".to_owned()))?;
    if body.len() != BODY_LEN {
        return Err(RecoveryError::Crypto(format!(
            "bech32m produced {} body characters, expected {BODY_LEN}",
            body.len()
        )));
    }

    let mut out = String::with_capacity(PREFIX.len() + GROUP_COUNT * (GROUP_LEN + 1));
    out.push_str(PREFIX);
    for group in 0..GROUP_COUNT {
        out.push('-');
        out.push_str(&body[group * GROUP_LEN..(group + 1) * GROUP_LEN]);
    }
    Ok(out)
}

/// Decode what a human typed.
pub(super) fn decode(typed: &str) -> Result<[u8; RECOVERY_KEY_BYTES], RecoveryError> {
    let flat = flatten(typed);

    let body = flat.strip_prefix(PREFIX).ok_or_else(|| {
        RecoveryError::Malformed(
            "it does not start with `fotw1` — that prefix is part of the key".to_owned(),
        )
    })?;

    // Length before checksum, and before any derivation, so the commonest
    // mistakes get the most specific message. "you are one character short" is
    // actionable; "checksum failed" is not.
    if body.len() != BODY_LEN {
        return Err(RecoveryError::Malformed(format!(
            "expected {BODY_LEN} characters after `fotw1`, found {} — {}",
            body.len(),
            if body.len() < BODY_LEN {
                "one is missing"
            } else {
                "there is one too many"
            }
        )));
    }

    let repaired: String = body.chars().map(repair).collect();
    let candidate = format!("{PREFIX}{repaired}");

    let (_hrp, data) = bech32::decode(&candidate).map_err(|e| {
        RecoveryError::Malformed(format!(
            "the checksum rejected it, so at least one character is wrong ({e})"
        ))
    })?;

    // `bech32::decode` accepts bech32 *or* bech32m. We mint bech32m, so a
    // string that only validates under the older constant is not one of ours —
    // and silently accepting it would mean accepting the very
    // final-character weakness BIP-350 was written to remove.
    if bech32::primitives::decode::CheckedHrpstring::new::<Bech32m>(&candidate).is_err() {
        return Err(RecoveryError::Malformed(
            "the checksum rejected it, so at least one character is wrong".to_owned(),
        ));
    }

    // There is deliberately **no HRP check here**, and the reason is worth
    // stating because its absence looks like an omission. `candidate` is built
    // as `PREFIX + repaired`, and `repair` maps `1` to `l`, so `repaired`
    // cannot contain a `1`. bech32 splits on the *last* `1`, which is therefore
    // always the one in `fotw1` — the HRP is `fotw` by construction. A check
    // here would be unreachable code that no test can exercise and that a
    // future edit would quietly turn into a lie. The invariant it depends on is
    // pinned by `decoded_hrp_is_always_ours` below instead.
    debug_assert_eq!(_hrp.as_str(), HRP);

    data.as_slice().try_into().map_err(|_| {
        RecoveryError::Malformed(format!(
            "it carries {} bytes, and a Recovery Key is {RECOVERY_KEY_BYTES}",
            data.len()
        ))
    })
}

/// Whether `typed` is group `index` of this key.
pub(super) fn group_matches(
    key: &[u8; RECOVERY_KEY_BYTES],
    index: usize,
    typed: &str,
) -> Result<bool, RecoveryError> {
    if index >= GROUP_COUNT {
        return Err(RecoveryError::Malformed(format!(
            "there is no group {}; a Recovery Key has {GROUP_COUNT}",
            index + 1
        )));
    }
    let shown = encode_grouped(key)?;
    let body = shown
        .strip_prefix(PREFIX)
        .unwrap_or_default()
        .replace('-', "");
    let expected = &body[index * GROUP_LEN..(index + 1) * GROUP_LEN];

    // The typed answer gets the same flattening and the same repair map as a
    // whole key would, so `0` for `O` is accepted here too. Anything else and
    // the confirmation step would be stricter than the thing it is confirming.
    let answer: String = flatten(typed).chars().map(repair).collect();

    Ok(ct_eq_bytes(answer.as_bytes(), expected.as_bytes()))
}

/// Strip presentation: whitespace, the group separators, and case.
///
/// Deliberately generous. A user pasting from a password manager, retyping from
/// a photo, or reading a card aloud to somebody else will produce spaces where
/// we printed dashes and vice versa, and rejecting that is rejecting a correct
/// key for a reason the user cannot see.
fn flatten(typed: &str) -> String {
    typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Map a character onto the only alphabet member it could have meant.
///
/// See the module docs: this is total and information-preserving *because*
/// bech32 excludes the other member of each pair.
fn repair(c: char) -> char {
    match c {
        'o' => '0',
        'i' | '1' => 'l',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The assumption the repair map rests on. If bech32's alphabet ever
    /// gained one of these, `repair` would start destroying information and
    /// this test is what says so.
    #[test]
    fn the_alphabet_excludes_every_character_the_repair_map_writes_over() {
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0u8..=255 {
            let s = encode_grouped(&[seed; RECOVERY_KEY_BYTES]).unwrap();
            for c in s
                .strip_prefix(PREFIX)
                .unwrap()
                .chars()
                .filter(|c| *c != '-')
            {
                seen.insert(c);
            }
        }
        for excluded in ['1', 'b', 'i', 'o'] {
            assert!(
                !seen.contains(&excluded),
                "`{excluded}` appeared in a body, so repairing it loses information"
            );
        }
        // ...and the survivors of each pair really are reachable, or the
        // exclusion above is vacuous.
        assert!(seen.contains(&'0'), "`0` never appears; is this base32?");
        assert!(seen.contains(&'l'), "`l` never appears");
    }

    /// The invariant that makes an HRP check in [`decode`] unreachable: the
    /// only `1` in a candidate string is bech32's separator, so the parsed HRP
    /// is always `fotw`. If `repair` ever stopped folding `1`, this fails and
    /// the comment in `decode` stops being true.
    #[test]
    fn decoded_hrp_is_always_ours() {
        assert_eq!(repair('1'), 'l', "a `1` could now reach the data part");
        for seed in 0u8..=255 {
            let shown = encode_grouped(&[seed; RECOVERY_KEY_BYTES]).unwrap();
            let flat = flatten(&shown);
            let body: String = flat
                .strip_prefix(PREFIX)
                .unwrap()
                .chars()
                .map(repair)
                .collect();
            assert!(!body.contains('1'), "a `1` survived repair: {body}");
            let (hrp, _) = bech32::decode(&format!("{PREFIX}{body}")).unwrap();
            assert_eq!(hrp.as_str(), HRP);
        }
    }

    #[test]
    fn flatten_removes_presentation_only() {
        assert_eq!(flatten("  FOTW1-ab_cd ef\n"), "fotw1abcdef");
    }

    #[test]
    fn repair_is_the_identity_on_the_alphabet() {
        for c in "qpzry9x8gf2tvdw0s3jn54khce6mua7l".chars() {
            assert_eq!(repair(c), c, "repair mangled a legal character: {c}");
        }
    }

    /// bech32 (the BIP-173 constant) must not be accepted where bech32m is
    /// expected. Without the explicit check this is exactly the kind of thing
    /// that passes every round-trip test and quietly reinstates the weakness
    /// BIP-350 removed.
    #[test]
    fn a_bech32_string_is_not_accepted_as_bech32m() {
        let hrp = Hrp::parse(HRP).unwrap();
        let old = bech32::encode_lower::<bech32::Bech32>(hrp, &[7u8; RECOVERY_KEY_BYTES]).unwrap();
        let new = encode_grouped(&[7u8; RECOVERY_KEY_BYTES]).unwrap();
        assert_ne!(
            old,
            new.replace('-', ""),
            "the two checksum variants agree, so this test proves nothing"
        );
        let err = decode(&old).expect_err("a bech32 (not -m) string was accepted");
        assert!(err.is_malformed(), "{err:?}");
    }
}
