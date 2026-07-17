//! The injectable `Clock` seam (ADR 0019/0022).
//!
//! Event timestamps come from an injected clock so golden tests and the mock
//! server can freeze time. The `at` an event carries is **informational only**
//! — `EventId` is the single ordering key (ADR 0022).

use jiff::Timestamp;

/// The time source injected everywhere a wall-clock breadcrumb is stamped.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// The production clock — reads the system time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock frozen at a fixed instant, for tests and golden logs (ADR 0019).
#[derive(Debug, Clone, Copy)]
pub struct FrozenClock(Timestamp);

impl FrozenClock {
    pub const fn new(at: Timestamp) -> Self {
        Self(at)
    }
}

impl Default for FrozenClock {
    /// Frozen at the Unix epoch.
    fn default() -> Self {
        Self(Timestamp::UNIX_EPOCH)
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_clock_always_returns_its_instant() {
        let at: Timestamp = "2026-07-17T00:00:00Z".parse().unwrap();
        let clock = FrozenClock::new(at);
        assert_eq!(clock.now(), at);
        assert_eq!(clock.now(), at);
        assert_eq!(clock.now().to_string(), "2026-07-17T00:00:00Z");
        assert_eq!(FrozenClock::default().now(), Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn system_clock_is_monotone_enough_for_a_breadcrumb() {
        let clock = SystemClock;
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a);
    }
}
