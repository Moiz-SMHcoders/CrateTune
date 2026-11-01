//! Fractional positions for ordered lists (playlist entries): each entry carries a
//! string that sorts between its neighbours', so an entry can be put between two
//! others without renumbering the rest, and entries inserted on two devices at the
//! same time keep a well-defined order once merged (by position, then key).
//!
//! Positions are strings of base-36 digits (`0-9a-z`) read as a fraction after the
//! point: `"i"` = 18/36. [`between`] returns the shortest-ish string strictly between
//! two positions; an end can be open (`None`). Positions never end in `0`, so there
//! is always room on both sides.

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const BASE: u32 = 36;

fn digit(c: u8) -> u32 {
    DIGITS.iter().position(|d| *d == c).unwrap_or(0) as u32
}

/// Whether `pos` is a valid position (non-empty, only base-36 digits, not ending in `0`).
pub fn is_valid(pos: &str) -> bool {
    !pos.is_empty() && !pos.ends_with('0') && pos.bytes().all(|b| DIGITS.contains(&b))
}

/// A position strictly between `lo` and `hi` (`None`: the list's start / end). `lo`
/// must sort before `hi`.
pub fn between(lo: Option<&str>, hi: Option<&str>) -> String {
    let (lo, hi) = (lo.unwrap_or("").as_bytes(), hi.map(str::as_bytes));
    let mut out = Vec::new();
    let mut i = 0;
    // Walk down while the digits agree (or `hi` is still above with no room).
    loop {
        let a = lo.get(i).map_or(0, |c| digit(*c));
        let b = match hi {
            Some(hi) => hi.get(i).map_or(BASE, |c| digit(*c)),
            None => BASE,
        };
        if b > a + 1 {
            // Room between the digits: take the middle.
            out.push(DIGITS[((a + b) / 2) as usize]);
            break;
        }
        out.push(DIGITS[a as usize]);
        if b == a + 1 {
            // `hi` is just one above here: from now on only `lo` bounds us.
            i += 1;
            let rest = lo.get(i..).unwrap_or(&[]);
            let tail = between(Some(std::str::from_utf8(rest).unwrap_or("")), None);
            out.extend_from_slice(tail.as_bytes());
            break;
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| "i".into())
}

/// `n` increasing positions between `lo` and `hi` (for a run of new entries).
pub fn spread(lo: Option<&str>, hi: Option<&str>, n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(n);
    for _ in 0..n {
        let prev = out.last().map(String::as_str).or(lo);
        out.push(between(prev, hi));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_sort_between_their_bounds() {
        let cases: &[(Option<&str>, Option<&str>)] = &[
            (None, None),
            (None, Some("i")),
            (Some("i"), None),
            (Some("i"), Some("j")),
            (Some("i"), Some("i1")),
            (Some("i9"), Some("ia")),
            (Some("1"), Some("2")),
            (Some("zz"), None),
            (None, Some("01")),
            (Some("0001"), Some("0002")),
        ];
        for &(lo, hi) in cases {
            let p = between(lo, hi);
            assert!(is_valid(&p), "{lo:?} {hi:?} → {p:?}");
            if let Some(lo) = lo {
                assert!(lo < p.as_str(), "{lo:?} < {p:?}");
            }
            if let Some(hi) = hi {
                assert!(p.as_str() < hi, "{p:?} < {hi:?}");
            }
        }
        assert_eq!(between(None, None), "i");
    }

    #[test]
    fn many_inserts_stay_ordered_and_short() {
        // Always appending, always prepending, always into the same gap.
        let mut list = vec![between(None, None)];
        for _ in 0..200 {
            let p = between(list.last().map(String::as_str), None);
            list.push(p);
            let p = between(None, list.first().map(String::as_str));
            list.insert(0, p);
        }
        let (a, b) = (list[10].clone(), list[11].clone());
        let mut lo = a.clone();
        for _ in 0..100 {
            lo = between(Some(&lo), Some(&b));
            assert!(a < lo && lo < b);
        }
        let mut sorted = list.clone();
        sorted.sort();
        assert_eq!(sorted, list);
        assert!(list.iter().all(|p| is_valid(p) && p.len() < 60));
        let run = spread(Some("a"), Some("b"), 5);
        assert!(run.windows(2).all(|w| w[0] < w[1]) && run[0].as_str() > "a");
        assert!(run[4].as_str() < "b");
        assert!(!is_valid("") && !is_valid("a0") && !is_valid("A"));
    }
}
