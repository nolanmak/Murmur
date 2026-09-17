//! Token-level word error rate.
//!
//! STT-09's acceptance criterion is a WER delta under 1 % between an
//! uninterrupted run and one whose socket was killed repeatedly. That is a
//! Levenshtein distance over word tokens divided by the reference length — about
//! twenty lines — so it is written here rather than pulled in as a dependency
//! whose only caller would be one assertion.

/// Split text the way a WER comparison should: lowercase, alphanumerics only.
///
/// Punctuation differences between the two runs are not transcription errors.
/// Deepgram's smart formatting is context-sensitive, so a comma landing
/// differently either side of a reconnect would otherwise register as two
/// substitutions and blow the 1 % budget on formatting alone.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|unit| {
            unit.chars()
                .filter(|character| character.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// Levenshtein distance between two token sequences.
pub fn edit_distance(reference: &[String], hypothesis: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0usize; hypothesis.len() + 1];

    for (i, reference_token) in reference.iter().enumerate() {
        current[0] = i + 1;
        for (j, hypothesis_token) in hypothesis.iter().enumerate() {
            let substitution = previous[j] + usize::from(reference_token != hypothesis_token);
            let deletion = previous[j + 1] + 1;
            let insertion = current[j] + 1;
            current[j + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[hypothesis.len()]
}

/// Word error rate of `hypothesis` against `reference`.
///
/// An empty reference with a non-empty hypothesis is 1.0 rather than a division
/// by zero — every word is an insertion.
pub fn word_error_rate(reference: &str, hypothesis: &str) -> f64 {
    let reference = tokenize(reference);
    let hypothesis = tokenize(hypothesis);
    if reference.is_empty() {
        return if hypothesis.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&reference, &hypothesis) as f64 / reference.len() as f64
}
