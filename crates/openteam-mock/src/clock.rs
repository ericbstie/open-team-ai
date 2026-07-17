//! The mock's own minimal injected clock seam (ADR 0019).
//!
//! The mock cannot depend on `openteam-core` (ADR 0013), so it carries its own
//! one-method clock trait. It exists solely to fill the chat response's
//! `created` field: the system clock in production, a frozen clock in the
//! exact-envelope contract tests (ADR 0025).

use std::time::{SystemTime, UNIX_EPOCH};

/// One-method clock seam: the unix-seconds source for the response `created`
/// field (ADR 0019 — "`created` from the injected `Clock`, frozen in tests").
pub trait MockClock: Send + Sync {
    fn unix_seconds(&self) -> u64;
}

/// The production clock: wall-clock unix seconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl MockClock for SystemClock {
    fn unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    }
}

/// A frozen clock for byte-identical envelope assertions (ADR 0025 Tier 3).
#[derive(Debug, Clone, Copy)]
pub struct FrozenClock(pub u64);

impl MockClock for FrozenClock {
    fn unix_seconds(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_clock_returns_its_instant() {
        assert_eq!(FrozenClock(1_752_710_400).unix_seconds(), 1_752_710_400);
    }

    #[test]
    fn system_clock_is_past_2020() {
        assert!(SystemClock.unix_seconds() > 1_577_836_800);
    }
}
