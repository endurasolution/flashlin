/// Connection limiter — enforces `max_connections` and emits the 111 response
/// when the server is overloaded.
///
/// Design:
/// - A global atomic counter tracks live connections.
/// - `try_acquire()` either returns a `ConnectionSlot` (RAII guard that
///   decrements on drop) or `None` when at capacity.
/// - The listener calls `try_acquire` before spawning; on None it sends 111
///   and closes the socket immediately.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ─── Slot (RAII guard) ────────────────────────────────────────────────────────

/// Held for the lifetime of a single accepted connection.
/// Decrements the counter when dropped (even on panic).
pub struct ConnectionSlot {
    counter: Arc<AtomicUsize>,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

// ─── Limiter ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ConnectionLimiter {
    current: Arc<AtomicUsize>,
    /// 0 = unlimited
    max: usize,
}

impl ConnectionLimiter {
    pub fn new(max: usize) -> Self {
        ConnectionLimiter {
            current: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }

    /// Try to acquire a slot.
    /// Returns `Some(slot)` on success or `None` when at/above capacity.
    pub fn try_acquire(&self) -> Option<ConnectionSlot> {
        if self.max == 0 {
            // Unlimited — still track the count for metrics.
            self.current.fetch_add(1, Ordering::Relaxed);
            return Some(ConnectionSlot {
                counter: Arc::clone(&self.current),
            });
        }

        // Optimistic increment — roll back if over limit.
        let prev = self.current.fetch_add(1, Ordering::Relaxed);
        if prev < self.max {
            Some(ConnectionSlot {
                counter: Arc::clone(&self.current),
            })
        } else {
            self.current.fetch_sub(1, Ordering::Relaxed);
            None
        }
    }

    /// Current live connection count.
    pub fn active(&self) -> usize {
        self.current.load(Ordering::Relaxed)
    }

    pub fn max(&self) -> usize {
        self.max
    }
}
