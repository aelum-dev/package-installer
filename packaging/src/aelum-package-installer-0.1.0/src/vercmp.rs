//! Arch version comparison: epoch split + rpmvercmp, as libalpm does it.
//!
//! Pure and unit-tested on purpose: pacman 7 no longer ships a `vercmp`
//! helper, and linking libalpm just to compare two strings is overkill.
//! The rules are rpm's: versions are sequences of alphanumeric segments
//! separated by anything else (separators are ignored); numeric segments
//! compare numerically (leading zeros not significant), alphabetic
//! segments compare in ASCII order; a version with more segments after
//! an equal prefix is newer; an `epoch:` prefix outranks everything.

use std::cmp::Ordering;

/// Compare two full Arch version strings (`epoch:name-version-rel`).
pub fn vercmp(a: &str, b: &str) -> Ordering {
    let (ea, va) = split_epoch(a);
    let (eb, vb) = split_epoch(b);
    ea.cmp(&eb).then_with(|| rpmvercmp(va, vb))
}

/// `1:1.2-3` → `(1, "1.2-3")`; no numeric epoch prefix → `(0, whole)`.
fn split_epoch(v: &str) -> (u64, &str) {
    if let Some(pos) = v.find(':')
        && pos > 0
        && v[..pos].bytes().all(|c| c.is_ascii_digit())
    {
        return (v[..pos].parse().unwrap_or(0), &v[pos + 1..]);
    }
    (0, v)
}

/// rpm's rpmvercmp over the epoch-free halves.
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    let (mut i, mut j) = (0usize, 0usize);
    let (ab, bb) = (a.as_bytes(), b.as_bytes());

    loop {
        // Skip separators; a segment starts at the next alphanumeric.
        while i < ab.len() && !ab[i].is_ascii_alphanumeric() {
            i += 1;
        }
        while j < bb.len() && !bb[j].is_ascii_alphanumeric() {
            j += 1;
        }
        // One side ran out: the other is newer only if it has a real
        // segment left — which the skips above just established.
        if i >= ab.len() || j >= bb.len() {
            break;
        }

        let numeric = ab[i].is_ascii_digit();
        let start_a = i;
        let start_b = j;
        if numeric {
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let ra = cmp_numeric(&a[start_a..i], &b[start_b..j]);
            if ra != Ordering::Equal {
                return ra;
            }
        } else {
            while i < ab.len() && ab[i].is_ascii_alphabetic() {
                i += 1;
            }
            while j < bb.len() && bb[j].is_ascii_alphabetic() {
                j += 1;
            }
            let ra = a[start_a..i].cmp(&b[start_b..j]);
            if ra != Ordering::Equal {
                return ra;
            }
        }
    }

    // Equal up to the shorter version's end: whichever still has a
    // segment is newer.
    has_segment(&a[i..]).cmp(&has_segment(&b[j..]))
}

/// True if anything alphanumeric remains.
fn has_segment(rest: &str) -> bool {
    rest.bytes().any(|c| c.is_ascii_alphanumeric())
}

/// Digit runs compared as numbers without parsing (versions exceed u64).
fn cmp_numeric(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn identical_versions_are_equal() {
        assert_eq!(vercmp("1.0-1", "1.0-1"), Equal);
        assert_eq!(vercmp("0.1.2-1", "0.1.2-1"), Equal);
    }

    #[test]
    fn release_bumps_are_newer() {
        assert_eq!(vercmp("1.0-2", "1.0-1"), Greater);
        assert_eq!(vercmp("1.0-10", "1.0-9"), Greater);
    }

    #[test]
    fn numeric_segments_compare_as_numbers() {
        assert_eq!(vercmp("1.10", "1.9"), Greater);
        assert_eq!(vercmp("1.00", "1.0"), Equal);
        // Past u64: still compared as numbers, not parsed.
        assert_eq!(vercmp("99999999999999999999999", "10000000000000000000000"), Greater);
        assert_eq!(vercmp("100000000000000000000000", "99999999999999999999999"), Greater);
    }

    #[test]
    fn epoch_outranks_the_version() {
        // `1:` beats any epoch-less version, however high — that is the
        // whole point of epochs.
        assert_eq!(vercmp("1:1.0", "2.0"), Greater);
        assert_eq!(vercmp("1:1.0", "1.0"), Greater);
        assert_eq!(vercmp("1:1.0", "2:0.1"), Less);
        assert_eq!(vercmp("1:1.0-1", "1:1.0-1"), Equal);
    }

    #[test]
    fn alphabetic_segments_compare_in_ascii_order() {
        assert_eq!(vercmp("1.0b", "1.0a"), Greater);
        assert_eq!(vercmp("1.0rc1", "1.0beta"), Greater); // 'r' > 'b'
        assert_eq!(vercmp("1.0", "1.0a"), Less); // more segments is newer
    }

    #[test]
    fn separators_are_ignored() {
        assert_eq!(vercmp("1_1", "1.1"), Equal);
        assert_eq!(vercmp("1-1", "1.1"), Equal);
        assert_eq!(vercmp("1.0.", "1.0"), Equal);
    }

    #[test]
    fn real_arch_shapes() {
        assert_eq!(vercmp("6.17.5.arch1-1", "6.17.9.arch1-1"), Less);
        assert_eq!(vercmp("2:1.20.1-1", "2:1.21.0-1"), Less);
        assert_eq!(vercmp("0.1.0-1", "0.1.1-1"), Less);
        assert_eq!(vercmp("20260817.0-2", "20260817.0-1"), Greater);
    }
}
