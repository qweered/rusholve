//! "Did you mean?" suggestions for unresolved command names.
//!
//! When an external command isn't in `--inputs`, scan the input
//! directories for any file whose name is close to the misspelled one
//! and surface it as a hint. We use bounded Levenshtein distance — if
//! nothing is within the threshold, no suggestion. Better silence than
//! a misleading guess.

/// Threshold rule, tiered to balance catching common typos against
/// false positives on very short names:
///
/// - len ≤ 3:  threshold 1   (`git` → `got` matches, but not `git` → `foo`)
/// - len ≤ 5:  threshold 2   (`gerp` → `grep`, `cwurl` → `curl`)
/// - len > 5:  threshold len/3 + 1
fn distance_threshold(name: &str) -> usize {
    let len = name.chars().count();
    if len <= 3 {
        1
    } else if len <= 5 {
        2
    } else {
        len / 3 + 1
    }
}

/// Return the closest candidate to `name` if one exists within the
/// threshold. Ties broken by alphabetical order for determinism.
pub fn nearest<'a, I>(name: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let threshold = distance_threshold(name);
    let mut best: Option<(usize, &str)> = None;
    for cand in candidates {
        if cand == name {
            // Exact match — wouldn't be unresolved in the first place,
            // but skip so we don't suggest the user "did mean" their own typo.
            continue;
        }
        let d = levenshtein(name, cand);
        if d > threshold {
            continue;
        }
        match best {
            Some((bd, _)) if d > bd => {}
            Some((bd, bs)) if d == bd && cand >= bs => {}
            _ => best = Some((d, cand)),
        }
    }
    best.map(|(_, s)| s.to_string())
}

/// Simple O(n*m) Levenshtein. Inputs are short command names, so the
/// quadratic factor is fine.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basic_cases() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("git", "got"), 1);
        assert_eq!(levenshtein("foo", "foo"), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn nearest_returns_close_match() {
        let cands = ["git", "grep", "gawk"];
        assert_eq!(nearest("got", cands).as_deref(), Some("git"));
    }

    #[test]
    fn nearest_returns_none_when_too_far() {
        let cands = ["git", "grep"];
        assert!(nearest("xyzzy", cands).is_none());
    }

    #[test]
    fn nearest_skips_exact_match() {
        // Exact match — not a typo. (Caller would be calling `nearest`
        // for an unresolved name, so this is mostly defensive.)
        let cands = ["git", "grep"];
        assert!(nearest("git", cands).is_none());
    }

    #[test]
    fn nearest_picks_lowest_distance() {
        let cands = ["foo", "bar", "git", "github"];
        // From "git" (len 3, threshold 1): everything that's not exact is too far.
        assert!(nearest("git", cands).is_none());
        // From "fit" (len 3, threshold 1): git is dist 1 → match.
        assert_eq!(nearest("fit", cands).as_deref(), Some("git"));
    }

    #[test]
    fn nearest_catches_transposition_on_short_name() {
        // `gerp` → `grep` is a transposition: Levenshtein 2, len 4,
        // threshold 2 → match.
        let cands = ["grep", "git"];
        assert_eq!(nearest("gerp", cands).as_deref(), Some("grep"));
    }

    #[test]
    fn nearest_threshold_scales_with_length() {
        // "ripgrep" has length 7 → threshold 2. "ripgrip" is dist 1 → match.
        let cands = ["ripgrep"];
        assert_eq!(nearest("ripgrip", cands).as_deref(), Some("ripgrep"));
    }

    #[test]
    fn ties_broken_alphabetically() {
        // "abc" / "abd" both dist 1 from "abe" — alphabetical pick wins.
        let cands = ["abc", "abd"];
        assert_eq!(nearest("abe", cands).as_deref(), Some("abc"));
    }
}
