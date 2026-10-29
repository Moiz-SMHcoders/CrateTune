//! Semantic versions (<https://semver.org>) as release tags spell them, and which
//! releases a running version is offered.

use std::cmp::Ordering;
use std::fmt;

/// One dot-separated pre-release identifier (`beta`, `2`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Identifier {
    Numeric(u64),
    Alpha(String),
}

impl Ord for Identifier {
    /// SemVer §11.4: numeric identifiers compare numerically and sort before
    /// alphanumeric ones, which compare in ASCII order.
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Identifier::Numeric(a), Identifier::Numeric(b)) => a.cmp(b),
            (Identifier::Numeric(_), Identifier::Alpha(_)) => Ordering::Less,
            (Identifier::Alpha(_), Identifier::Numeric(_)) => Ordering::Greater,
            (Identifier::Alpha(a), Identifier::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Identifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Identifier::Numeric(n) => write!(f, "{n}"),
            Identifier::Alpha(s) => f.write_str(s),
        }
    }
}

/// `MAJOR.MINOR.PATCH[-PRE][+BUILD]`. Parsing accepts a leading `v` and a missing
/// minor / patch (`v1.2` = `1.2.0`); build metadata is dropped (it has no
/// precedence).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Empty for a release.
    pub pre: Vec<Identifier>,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version {
            major,
            minor,
            patch,
            pre: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() > 3 {
            return None;
        }
        let mut numbers = [0u64; 3];
        for (slot, part) in numbers.iter_mut().zip(&parts) {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *slot = part.parse().ok()?;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    {
                        None
                    } else if id.bytes().all(|b| b.is_ascii_digit()) {
                        id.parse().ok().map(Identifier::Numeric)
                    } else {
                        Some(Identifier::Alpha(id.to_owned()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Version {
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
            pre,
        })
    }

    /// Has a pre-release part (`1.0.0-beta.1`).
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The default of "Include pre-releases" for a running version: on for a
    /// pre-release, and for `0.x` (SemVer §4: initial development, where every
    /// release so far has been published as a GitHub pre-release); off from 1.0.
    pub fn wants_prereleases_by_default(&self) -> bool {
        self.is_prerelease() || self.major == 0
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                // A release sorts after its pre-releases (1.0.0-rc.1 < 1.0.0).
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                // Identifier by identifier; a shorter list that is a prefix sorts first.
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            write!(f, "{id}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn parses_tags() {
        assert_eq!(v("v1.2.3"), Version::new(1, 2, 3));
        assert_eq!(v("1.2"), Version::new(1, 2, 0));
        assert_eq!(v(" V7 "), Version::new(7, 0, 0));
        assert_eq!(v("1.2.3+build.5"), Version::new(1, 2, 3));
        let beta = v("0.2.0-beta.11");
        assert_eq!(
            beta.pre,
            vec![Identifier::Alpha("beta".into()), Identifier::Numeric(11)]
        );
        assert_eq!(beta.to_string(), "0.2.0-beta.11");
        assert_eq!(v("1.0.0-rc-1").to_string(), "1.0.0-rc-1");
        for bad in [
            "",
            "v",
            "1..2",
            "1.2.3.4",
            "1.x",
            "1.0.0-",
            "1.0.0-a..b",
            "1.0.0-ä",
            "latest",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn orders_like_semver() {
        // The example chain from SemVer §11.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "1.10.0",
            "2.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert!(v("0.1.10") > v("0.1.9"));
        assert_eq!(v("1.0").cmp(&v("v1.0.0")), Ordering::Equal);
        assert_eq!(v("1.0.0+a").cmp(&v("1.0.0+b")), Ordering::Equal);
    }

    #[test]
    fn prerelease_default() {
        assert!(v("0.1.0").wants_prereleases_by_default());
        assert!(v("1.2.0-beta.1").wants_prereleases_by_default());
        assert!(!v("1.0.0").wants_prereleases_by_default());
    }
}
