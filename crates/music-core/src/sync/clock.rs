//! A hybrid logical clock: Unix milliseconds in the upper 48 bits, a counter in the
//! lower 16. Stamps from one device only ever grow (two edits in the same
//! millisecond, or a clock set back, still get increasing stamps), and a device
//! that has seen a stamp stamps its next change later, so "the later change wins"
//! follows cause and effect even when the devices' clocks disagree a little.

/// Bits of the counter.
const COUNTER_BITS: u32 = 16;
/// A peer's clock this far ahead of ours isn't followed (its stamps still count;
/// only our own clock doesn't jump to a wrong date).
const MAX_AHEAD_MS: u64 = 24 * 3600 * 1000;

/// The stamp of `ms` with counter 0.
pub fn from_ms(ms: u64) -> u64 {
    ms << COUNTER_BITS
}

/// The milliseconds of a stamp.
pub fn to_ms(stamp: u64) -> u64 {
    stamp >> COUNTER_BITS
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clock {
    last: u64,
}

impl Clock {
    pub fn new(last: u64) -> Self {
        Self { last }
    }

    pub fn last(&self) -> u64 {
        self.last
    }

    /// A new stamp at wall time `now_ms`, later than every stamp seen so far.
    pub fn tick(&mut self, now_ms: u64) -> u64 {
        self.last = (self.last + 1).max(from_ms(now_ms));
        self.last
    }

    /// A stamp from another device was seen.
    pub fn observe(&mut self, stamp: u64, now_ms: u64) {
        if to_ms(stamp) <= now_ms.saturating_add(MAX_AHEAD_MS) {
            self.last = self.last.max(stamp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_grow_and_follow_what_was_seen() {
        let mut c = Clock::default();
        let a = c.tick(1_000);
        let b = c.tick(1_000);
        assert!(b > a && to_ms(b) == 1_000);
        // The wall clock went back: still later.
        let d = c.tick(900);
        assert!(d > b);
        // A peer 5 s ahead: our next stamp is later than its.
        c.observe(from_ms(6_000), 1_000);
        assert!(c.tick(1_001) > from_ms(6_000));
        // A peer a year ahead isn't followed.
        let before = c.last();
        c.observe(from_ms(1_000 + 400 * 24 * 3600 * 1000), 1_000);
        assert_eq!(c.last(), before);
        assert_eq!(to_ms(from_ms(123)), 123);
    }
}
