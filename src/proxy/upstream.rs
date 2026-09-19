use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub struct UpstreamGroup {
    next: AtomicUsize,
    unavailable_until: Mutex<Vec<Option<Instant>>>,
}

impl UpstreamGroup {
    pub fn shared(upstreams: &[String], config: &crate::config::ServerConfig) -> Arc<Self> {
        config
            .state
            .upstreams
            .entry(upstreams.to_vec())
            .or_insert_with(|| {
                Arc::new(Self {
                    next: AtomicUsize::new(0),
                    unavailable_until: Mutex::new(vec![None; upstreams.len()]),
                })
            })
            .clone()
    }

    pub fn candidates(&self, length: usize) -> Vec<usize> {
        let start = self.next.fetch_add(1, Ordering::Relaxed) % length;
        let failed = self
            .unavailable_until
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut candidates: Vec<_> = (0..length)
            .map(|offset| (start + offset) % length)
            .filter(|&index| failed[index].is_none_or(|until| until <= Instant::now()))
            .collect();
        // When all backends are cooling down, probe one to permit recovery.
        if candidates.is_empty() {
            candidates.push(start);
        }
        candidates
    }

    pub fn record(&self, index: usize, failed: bool) {
        self.unavailable_until
            .lock()
            .unwrap_or_else(|e| e.into_inner())[index] = if failed {
            Some(Instant::now() + Duration::from_secs(5))
        } else {
            None
        };
    }
}
