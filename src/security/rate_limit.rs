use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::config::{parse_rate_limit, ServerConfig};

/// Token-bucket rate limiter keyed by IP address (or any string key).
pub struct RateLimiter {
    /// Maximum tokens per window.
    capacity: u32,
    /// Tokens added per second (capacity / window_secs).
    refill_per_sec: f64,
    /// Burst headroom above capacity.
    burst: u32,
    /// Per-key state.
    buckets: Arc<DashMap<String, BucketState>>,
    checks_since_cleanup: AtomicUsize,
}

struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Construct a limiter directly (useful in tests).
    pub fn new(capacity: u32, refill_per_sec: f64, burst: u32) -> Self {
        RateLimiter {
            capacity,
            refill_per_sec,
            burst,
            buckets: Arc::new(DashMap::new()),
            checks_since_cleanup: AtomicUsize::new(0),
        }
    }

    pub fn from_config(config: &ServerConfig) -> Self {
        let (capacity, window_secs, burst) = config
            .security
            .as_ref()
            .and_then(|s| s.rate_limit.as_ref())
            .and_then(|r| {
                parse_rate_limit(&r.limit).map(|(c, w)| {
                    let b = r.burst.unwrap_or(c / 5 + 1);
                    (c, w, b)
                })
            })
            .unwrap_or((1000, 60, 50)); // safe default: 1000/min

        let refill_per_sec = capacity as f64 / window_secs as f64;

        RateLimiter {
            capacity,
            refill_per_sec,
            burst,
            buckets: Arc::new(DashMap::new()),
            checks_since_cleanup: AtomicUsize::new(0),
        }
    }

    /// Returns true if the request should be allowed, false if rate-limited.
    pub fn check(&self, key: &str) -> bool {
        // Bound state created by one-off client addresses without a dedicated
        // housekeeping task. The periodic scan is deliberately infrequent.
        if self.checks_since_cleanup.fetch_add(1, Ordering::Relaxed) % 1024 == 0 {
            let refill_horizon =
                ((self.capacity as f64 + self.burst as f64) / self.refill_per_sec).ceil() as u64;
            self.evict_stale((15 * 60).max(refill_horizon));
        }
        let now = Instant::now();
        let max_tokens = (self.capacity + self.burst) as f64;

        let mut entry = self
            .buckets
            .entry(key.to_string())
            .or_insert_with(|| BucketState {
                tokens: self.capacity as f64,
                last_refill: now,
            });

        // Refill tokens based on elapsed time.
        let elapsed = now.duration_since(entry.last_refill).as_secs_f64();
        entry.tokens = (entry.tokens + elapsed * self.refill_per_sec).min(max_tokens);
        entry.last_refill = now;

        if entry.tokens >= 1.0 {
            entry.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Periodically evict stale entries to prevent unbounded memory growth.
    /// Call this from a background task every few minutes.
    pub fn evict_stale(&self, idle_secs: u64) {
        let threshold = Duration::from_secs(idle_secs);
        let now = Instant::now();
        self.buckets
            .retain(|_, v| now.duration_since(v.last_refill) < threshold);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_within_limit() {
        let limiter = RateLimiter::new(10, 10.0 / 60.0, 5);
        for _ in 0..10 {
            assert!(limiter.check("127.0.0.1"));
        }
    }

    #[test]
    fn blocks_after_burst() {
        let limiter = RateLimiter::new(3, 0.0, 0);
        assert!(limiter.check("10.0.0.1"));
        assert!(limiter.check("10.0.0.1"));
        assert!(limiter.check("10.0.0.1"));
        assert!(!limiter.check("10.0.0.1")); // 4th request should be blocked
    }
}
