use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Lightweight atomic counters — a full Prometheus exporter can be layered on top.
#[derive(Default)]
pub struct Metrics {
    pub requests_total: AtomicU64,
    pub requests_2xx: AtomicU64,
    pub requests_4xx: AtomicU64,
    pub requests_5xx: AtomicU64,
    pub bytes_sent: AtomicU64,
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        Arc::new(Metrics::default())
    }

    pub fn record(&self, status: u16, bytes: u64) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        match status {
            200..=299 => {
                self.requests_2xx.fetch_add(1, Ordering::Relaxed);
            }
            400..=499 => {
                self.requests_4xx.fetch_add(1, Ordering::Relaxed);
            }
            500..=599 => {
                self.requests_5xx.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Render a minimal Prometheus text exposition format.
    pub fn render(&self) -> String {
        format!(
            "# HELP flashlin_requests_total Total requests served\n\
             # TYPE flashlin_requests_total counter\n\
             flashlin_requests_total {}\n\
             # HELP flashlin_responses_2xx 2xx responses\n\
             # TYPE flashlin_responses_2xx counter\n\
             flashlin_responses_2xx {}\n\
             # HELP flashlin_responses_4xx 4xx responses\n\
             # TYPE flashlin_responses_4xx counter\n\
             flashlin_responses_4xx {}\n\
             # HELP flashlin_responses_5xx 5xx responses\n\
             # TYPE flashlin_responses_5xx counter\n\
             flashlin_responses_5xx {}\n\
             # HELP flashlin_bytes_sent Total bytes sent\n\
             # TYPE flashlin_bytes_sent counter\n\
             flashlin_bytes_sent {}\n",
            self.requests_total.load(Ordering::Relaxed),
            self.requests_2xx.load(Ordering::Relaxed),
            self.requests_4xx.load(Ordering::Relaxed),
            self.requests_5xx.load(Ordering::Relaxed),
            self.bytes_sent.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn renders_recorded_counters() {
        let metrics = Metrics::new();
        metrics.record(200, 24);
        metrics.record(404, 0);
        metrics.record(502, 0);

        let rendered = metrics.render();
        assert!(rendered.contains("flashlin_requests_total 3"));
        assert!(rendered.contains("flashlin_responses_2xx 1"));
        assert!(rendered.contains("flashlin_responses_4xx 1"));
        assert!(rendered.contains("flashlin_responses_5xx 1"));
        assert!(rendered.contains("flashlin_bytes_sent 24"));
    }
}
