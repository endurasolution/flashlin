/// Connection limiter — enforces `max_connections` and
/// `max_connections_per_ip`, and emits the 503 response when the server is
/// overloaded.
///
/// Design:
/// - A global atomic counter tracks live connections.
/// - With a per-client cap, a map counts live connections per client key
///   (IPv4 address or IPv6 /64, see `security::client_key`).
/// - `try_acquire()` either returns a `ConnectionSlot` (RAII guard that
///   decrements on drop) or the reason it was refused.
/// - The listener calls `try_acquire` before spawning; on a global refusal it
///   sends 503, on a per-client refusal it just closes the socket, so a flood
///   from one client costs as little as possible.
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

// ─── Slot (RAII guard) ────────────────────────────────────────────────────────

/// Held for the lifetime of a single accepted connection.
/// Decrements the counters when dropped (even on panic).
pub struct ConnectionSlot {
    counter: Arc<AtomicUsize>,
    per_client: Option<(Arc<DashMap<IpAddr, usize>>, IpAddr)>,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
        if let Some((map, key)) = &self.per_client {
            release(map, *key);
        }
    }
}

fn release(map: &DashMap<IpAddr, usize>, key: IpAddr) {
    if let dashmap::mapref::entry::Entry::Occupied(mut entry) = map.entry(key) {
        *entry.get_mut() -= 1;
        if *entry.get() == 0 {
            entry.remove();
        }
    }
}

/// Why a connection was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// `max_connections` reached.
    Server,
    /// `max_connections_per_ip` reached for this client.
    Client,
}

// ─── Limiter ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ConnectionLimiter {
    current: Arc<AtomicUsize>,
    /// 0 = unlimited
    max: usize,
    /// 0 = unlimited
    max_per_client: usize,
    per_client: Arc<DashMap<IpAddr, usize>>,
}

impl ConnectionLimiter {
    #[cfg(test)]
    pub fn new(max: usize) -> Self {
        Self::with_per_client(max, 0)
    }

    pub fn with_per_client(max: usize, max_per_client: usize) -> Self {
        ConnectionLimiter {
            current: Arc::new(AtomicUsize::new(0)),
            max,
            max_per_client,
            per_client: Arc::new(DashMap::new()),
        }
    }

    /// Try to admit a connection from `ip`.
    pub fn try_acquire(&self, ip: IpAddr) -> Result<ConnectionSlot, Refused> {
        let per_client = if self.max_per_client == 0 {
            None
        } else {
            let key = crate::security::client_key(ip);
            let mut count = self.per_client.entry(key).or_insert(0);
            if *count >= self.max_per_client {
                return Err(Refused::Client);
            }
            *count += 1;
            Some((Arc::clone(&self.per_client), key))
        };

        // Optimistic increment — roll back if over limit. Unlimited still
        // tracks the count for metrics and shutdown draining.
        let prev = self.current.fetch_add(1, Ordering::Relaxed);
        if self.max != 0 && prev >= self.max {
            self.current.fetch_sub(1, Ordering::Relaxed);
            if let Some((map, key)) = &per_client {
                release(map, *key);
            }
            return Err(Refused::Server);
        }
        Ok(ConnectionSlot {
            counter: Arc::clone(&self.current),
            per_client,
        })
    }

    /// Current live connection count.
    pub fn active(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    pub fn max(&self) -> usize {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn per_client_cap_is_independent_per_client_and_released_on_drop() {
        let limiter = ConnectionLimiter::with_per_client(0, 2);
        let a1 = limiter.try_acquire(ip("10.0.0.1")).unwrap();
        let _a2 = limiter.try_acquire(ip("::ffff:10.0.0.1")).unwrap();
        assert_eq!(
            limiter.try_acquire(ip("10.0.0.1")).err(),
            Some(Refused::Client)
        );
        let _b = limiter.try_acquire(ip("10.0.0.2")).unwrap();
        assert_eq!(limiter.active(), 3);

        drop(a1);
        let _a3 = limiter.try_acquire(ip("10.0.0.1")).unwrap();
        drop(_b);
        assert!(!limiter.per_client.contains_key(&ip("10.0.0.2")));
    }

    #[test]
    fn global_refusal_returns_the_client_slot() {
        let limiter = ConnectionLimiter::with_per_client(1, 5);
        let _first = limiter.try_acquire(ip("10.0.0.1")).unwrap();
        assert_eq!(
            limiter.try_acquire(ip("10.0.0.2")).err(),
            Some(Refused::Server)
        );
        assert!(!limiter.per_client.contains_key(&ip("10.0.0.2")));
        assert_eq!(limiter.active(), 1);
    }
}
