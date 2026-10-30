//! Short room codes (`ABCD-EFGH`) and the per-address limit on resolving them.
//!
//! A code is [`CODE_LEN`] characters from [`CODE_ALPHABET`] (digits and capital
//! letters without the look-alikes 0 / O / 1 / I): 32⁸ ≈ 1.1 × 10¹² codes. With
//! the relay full (100 rooms) a guess hits some room with a chance of ~1 in 10¹⁰;
//! at the default 10 resolves per minute per address that is about 2,000 years
//! of guessing per address (an IPv6 /64 counts as one address). And a code only
//! gets someone as far as the session's key: the host still lets guests in when
//! the room asks for approval.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// 32 symbols, so one random byte (`& 31`) picks one without bias.
pub const CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub const CODE_LEN: usize = 8;

/// A fresh random code (not checked against the live ones; the caller does that).
pub fn new_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    getrandom::fill(&mut bytes).expect("the OS random number generator");
    bytes
        .iter()
        .map(|b| CODE_ALPHABET[usize::from(b & 31)] as char)
        .collect()
}

/// `abcd-efgh`, ` ABCD EFGH ` → `ABCDEFGH`; `None` if it can't be a code.
pub fn normalize_code(raw: &str) -> Option<String> {
    let code: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && !is_dash(*c))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LEN && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '‐' | '‑' | '‒' | '–' | '—' | '−')
}

/// One bucket per address: `per_minute` attempts at once, refilled evenly over a
/// minute. IPv6 addresses count per /64 (what one subscriber usually gets).
pub struct RateLimiter {
    per_minute: f64,
    buckets: HashMap<IpAddr, (f64, Instant)>,
}

/// Forget full buckets once this many addresses are tracked.
const PRUNE_AT: usize = 10_000;

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute: f64::from(per_minute.max(1)),
            buckets: HashMap::new(),
        }
    }

    fn refill(&self, (tokens, last): (f64, Instant), now: Instant) -> f64 {
        let rate = self.per_minute / 60.0;
        (tokens + now.saturating_duration_since(last).as_secs_f64() * rate).min(self.per_minute)
    }

    /// Takes one attempt for `ip`, or says how long until the next one is allowed.
    pub fn check(&mut self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        if self.buckets.len() >= PRUNE_AT {
            let full = self.per_minute;
            let buckets = std::mem::take(&mut self.buckets);
            self.buckets = buckets
                .into_iter()
                .filter(|(_, b)| self.refill(*b, now) < full)
                .collect();
        }
        let key = bucket_key(ip);
        let tokens = match self.buckets.get(&key) {
            Some(b) => self.refill(*b, now),
            None => self.per_minute,
        };
        if tokens >= 1.0 {
            self.buckets.insert(key, (tokens - 1.0, now));
            Ok(())
        } else {
            self.buckets.insert(key, (tokens, now));
            let rate = self.per_minute / 60.0;
            Err(Duration::from_secs_f64((1.0 - tokens) / rate))
        }
    }
}

fn bucket_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::from([s[0], s[1], s[2], s[3], 0, 0, 0, 0])
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn codes_are_unambiguous_and_random() {
        let mut seen = HashSet::new();
        for _ in 0..2_000 {
            let code = new_code();
            assert_eq!(code.len(), CODE_LEN);
            assert!(!code.contains(['0', 'O', '1', 'I']), "look-alike in {code}");
            assert_eq!(normalize_code(&code).as_deref(), Some(code.as_str()));
            seen.insert(code);
        }
        // 2,000 draws from 32⁸: a repeat would mean the generator is broken.
        assert_eq!(seen.len(), 2_000);
        // Every symbol shows up (no bias towards a part of the alphabet).
        let used: HashSet<u8> = seen.iter().flat_map(|c| c.bytes()).collect();
        assert_eq!(used.len(), CODE_ALPHABET.len());
    }

    #[test]
    fn normalization() {
        for (raw, want) in [
            ("ABCD-EFGH", Some("ABCDEFGH")),
            ("abcd efgh", Some("ABCDEFGH")),
            ("  aBcD–eFgH \n", Some("ABCDEFGH")),
            ("A-B-C-D-E-F-G-H", Some("ABCDEFGH")),
            ("23456789", Some("23456789")),
            ("ABCDEFG", None),   // too short
            ("ABCDEFGHJ", None), // too long
            ("ABCD-EFG0", None), // 0 isn't in the alphabet
            ("OBCD-EFGH", None), // nor O
            ("IBCD-EFGH", None), // nor I
            ("1BCD-EFGH", None), // nor 1
            ("ÄBCD-EFGH", None), // nor anything else
            ("ABCD_EFGH", None), // underscores aren't dashes
            ("", None),
        ] {
            assert_eq!(normalize_code(raw).as_deref(), want, "{raw:?}");
        }
    }

    #[test]
    fn rate_limit_per_address() {
        let mut limiter = RateLimiter::new(10);
        let start = Instant::now();
        let a: IpAddr = "203.0.113.7".parse().unwrap();
        let b: IpAddr = "203.0.113.8".parse().unwrap();
        for _ in 0..10 {
            assert!(limiter.check(a, start).is_ok());
        }
        // The 11th within the minute is refused, with a sensible wait.
        let wait = limiter.check(a, start).unwrap_err();
        assert!(
            wait > Duration::from_secs(5) && wait <= Duration::from_secs(6),
            "{wait:?}"
        );
        // Others aren't affected.
        assert!(limiter.check(b, start).is_ok());
        // One attempt comes back every 6 s; a full minute refills all ten.
        assert!(limiter.check(a, start + Duration::from_secs(3)).is_err());
        assert!(limiter.check(a, start + Duration::from_secs(7)).is_ok());
        assert!(limiter.check(a, start + Duration::from_secs(8)).is_err());
        let later = start + Duration::from_secs(120);
        for _ in 0..10 {
            assert!(limiter.check(a, later).is_ok());
        }
        assert!(limiter.check(a, later).is_err());
    }

    #[test]
    fn ipv6_counts_per_slash_64() {
        let mut limiter = RateLimiter::new(2);
        let now = Instant::now();
        let one: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let same_net: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let other_net: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert!(limiter.check(one, now).is_ok());
        assert!(limiter.check(same_net, now).is_ok());
        assert!(limiter.check(one, now).is_err());
        assert!(limiter.check(other_net, now).is_ok());
        // A v4-mapped address is the v4 address.
        let v4: IpAddr = "198.51.100.1".parse().unwrap();
        let mapped: IpAddr = "::ffff:198.51.100.1".parse().unwrap();
        assert!(limiter.check(v4, now).is_ok());
        assert!(limiter.check(mapped, now).is_ok());
        assert!(limiter.check(v4, now).is_err());
    }

    #[test]
    fn prunes_idle_addresses() {
        let mut limiter = RateLimiter::new(10);
        let now = Instant::now();
        for i in 0..PRUNE_AT as u32 {
            let _ = limiter.check(IpAddr::from(i.to_be_bytes()), now);
        }
        assert_eq!(limiter.buckets.len(), PRUNE_AT);
        // A minute later every bucket is full again and goes.
        let _ = limiter.check("192.0.2.1".parse().unwrap(), now + Duration::from_secs(61));
        assert_eq!(limiter.buckets.len(), 1);
    }
}
