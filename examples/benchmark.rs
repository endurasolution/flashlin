//! Bounded-memory HTTP/1 closed-loop load generator. Not a production certification.
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, Uri};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::time::{Duration, Instant};

struct Stats {
    ok: u64,
    errors: u64,
    bytes: u64,
    histogram: [u64; 1024],
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            ok: 0,
            errors: 0,
            bytes: 0,
            histogram: [0; 1024],
        }
    }
}
impl Stats {
    fn percentile(&self, p: f64) -> f64 {
        let target = ((self.ok + self.errors) as f64 * p).ceil() as u64;
        if target == 0 {
            return 0.0;
        }
        let mut count = 0;
        for (i, n) in self.histogram.iter().enumerate() {
            count += n;
            if count >= target {
                return 1.02_f64.powi(i as i32) / 1000.0;
            }
        }
        0.0
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() == 4,
        "Usage: benchmark URL SECONDS CONCURRENCY EXPECTED_BYTES (HTTP/1, identity encoding)"
    );
    let uri: Uri = args[0].parse()?;
    anyhow::ensure!(
        uri.scheme_str() == Some("http"),
        "This baseline runner supports plain HTTP only"
    );
    let seconds: u64 = args[1].parse()?;
    let concurrency: usize = args[2].parse()?;
    let expected_bytes: u64 = args[3].parse()?;
    anyhow::ensure!(
        (1..=3600).contains(&seconds) && (1..=4096).contains(&concurrency),
        "Invalid duration/concurrency"
    );
    let mut connector = hyper_util::client::legacy::connect::HttpConnector::new();
    connector.set_nodelay(true);
    connector.set_connect_timeout(Some(Duration::from_secs(5)));
    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(concurrency)
        .retry_canceled_requests(false)
        .build(connector);
    // Prime files, page cache and persistent connections. Discard this phase.
    run(client.clone(), uri.clone(), 3, concurrency, expected_bytes).await?;
    let started = Instant::now();
    let result = run(client, uri, seconds, concurrency, expected_bytes).await?;
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({
            "url": args[0], "protocol": "http/1.1", "encoding": "identity", "warmup_seconds": 3,
            "requested_seconds": seconds, "elapsed_seconds_including_drain": elapsed,
            "concurrency": concurrency, "expected_bytes": expected_bytes,
            "successes": result.ok, "errors": result.errors, "bytes": result.bytes,
            "successful_requests_per_second": result.ok as f64 / elapsed,
            "p50_ms": result.percentile(0.50), "p95_ms": result.percentile(0.95), "p99_ms": result.percentile(0.99),
            "latency_histogram_relative_error_max": 0.02,
            "load_model": "closed-loop; includes full response body; no coordinated-omission correction"
        })
    );
    anyhow::ensure!(
        result.ok > 0 && result.errors == 0,
        "Benchmark contained failures; do not treat it as a clean capacity result"
    );
    Ok(())
}

async fn run(
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Empty<Bytes>>,
    uri: Uri,
    seconds: u64,
    concurrency: usize,
    expected_bytes: u64,
) -> anyhow::Result<Stats> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..concurrency {
        let client = client.clone();
        let uri = uri.clone();
        tasks.spawn(async move {
            let mut stats = Stats::default();
            while Instant::now() < deadline {
                let start = Instant::now();
                let result = tokio::time::timeout(Duration::from_secs(10), async {
                    let req = Request::builder()
                        .uri(uri.clone())
                        .header("accept-encoding", "identity")
                        .body(Empty::new())?;
                    let response = client.request(req).await?;
                    anyhow::ensure!(response.status() == 200, "Unexpected status");
                    let mut body = response.into_body();
                    let mut bytes = 0_u64;
                    while let Some(frame) = body.frame().await {
                        if let Some(data) = frame?.data_ref() {
                            bytes += data.len() as u64;
                        }
                    }
                    anyhow::ensure!(bytes == expected_bytes, "Unexpected body length");
                    Ok::<u64, anyhow::Error>(bytes)
                })
                .await;
                let micros = start.elapsed().as_secs_f64() * 1_000_000.0;
                let bin = (micros.max(1.0).ln() / 1.02_f64.ln()).ceil() as usize;
                stats.histogram[bin.min(1023)] += 1;
                match result {
                    Ok(Ok(bytes)) => {
                        stats.ok += 1;
                        stats.bytes += bytes;
                    }
                    _ => stats.errors += 1,
                }
            }
            stats
        });
    }
    let mut total = Stats::default();
    while let Some(stats) = tasks.join_next().await {
        let stats = stats?;
        total.ok += stats.ok;
        total.errors += stats.errors;
        total.bytes += stats.bytes;
        for (total, count) in total.histogram.iter_mut().zip(stats.histogram) {
            *total += count;
        }
    }
    Ok(total)
}
