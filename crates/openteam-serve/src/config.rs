//! `ServeConfig` — the constructor-injectable timing knobs (ADR 0030).
//!
//! The knobs are never on the CLI (whose surface is exactly `serve --dir
//! --port`, ADR 0027): the binary wires the pinned production defaults, while
//! tests construct the router/state directly with fast values (~5 ms poll,
//! tiny broadcast capacity) so torn-line and disconnect-on-lag are provable
//! without wall-clock sleeps or 1024+ events.

use std::time::Duration;

/// Timing/capacity knobs consumed by `build_router`/`serve` (pins §9).
#[derive(Debug, Clone)]
pub struct ServeConfig {
    /// Tail poll interval — how often the tailer re-reads `events.jsonl`.
    pub poll_interval: Duration,
    /// SSE keep-alive comment interval (`:` comment lines).
    pub keep_alive: Duration,
    /// The SSE `retry:` reconnection hint sent to `EventSource`, in ms.
    pub retry_ms: u64,
    /// Per-live-run bounded broadcast channel capacity (events).
    pub broadcast_capacity: usize,
}

impl ServeConfig {
    /// The pinned production defaults (pins §9): **100 ms / 15 s / 2000 ms /
    /// 1024** — what `openteam serve` wires; the CLI never overrides them.
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
    pub const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(15);
    pub const DEFAULT_RETRY_MS: u64 = 2000;
    pub const DEFAULT_BROADCAST_CAPACITY: usize = 1024;
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            poll_interval: Self::DEFAULT_POLL_INTERVAL,
            keep_alive: Self::DEFAULT_KEEP_ALIVE,
            retry_ms: Self::DEFAULT_RETRY_MS,
            broadcast_capacity: Self::DEFAULT_BROADCAST_CAPACITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_pinned_values() {
        let config = ServeConfig::default();
        assert_eq!(config.poll_interval, Duration::from_millis(100));
        assert_eq!(config.keep_alive, Duration::from_secs(15));
        assert_eq!(config.retry_ms, 2000);
        assert_eq!(config.broadcast_capacity, 1024);
    }
}
