use crate::{
    config::ServerConfig,
    http::{
        request::encoding_quality,
        response::{HyperResponse, ResponseBody},
    },
};
use bytes::Bytes;
use flate2::{write::GzEncoder, Compression};
use hyper::{
    header::{self, HeaderValue},
    Response,
};
use std::{
    io::Write,
    sync::{Arc, OnceLock},
};
use tokio::sync::Semaphore;

pub fn vary_encoding(headers: &mut hyper::HeaderMap) {
    let present = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim() == "*" || v.trim().eq_ignore_ascii_case("accept-encoding"));
    if !present {
        headers.append(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
}

/// Bounded CPU work, off the async workers. Large/streaming responses use
/// precompressed files or pass through without dynamic compression.
pub async fn maybe_compress(
    mut response: HyperResponse,
    headers: &hyper::HeaderMap,
    config: &Arc<ServerConfig>,
) -> HyperResponse {
    if response.headers().contains_key(header::CONTENT_ENCODING)
        || response.status() != hyper::StatusCode::OK
        || response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|v| v.trim().eq_ignore_ascii_case("no-transform"))
            })
    {
        return response;
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ![
        "text/",
        "application/json",
        "application/javascript",
        "application/xml",
        "image/svg",
    ]
    .iter()
    .any(|t| content_type.starts_with(t))
    {
        return response;
    }
    let settings = config.compression.as_ref();
    if !settings.map(|c| c.gzip || c.brotli).unwrap_or(true) {
        return response;
    }
    vary_encoding(response.headers_mut());
    let Some(bytes) = response.body().buffered.as_ref() else {
        return response;
    };
    if bytes.len() < settings.map(|c| c.min_size as usize).unwrap_or(1024)
        || bytes.len() > 1024 * 1024
    {
        return response;
    }
    let br = if settings.map(|c| c.brotli).unwrap_or(true) {
        encoding_quality(headers, "br")
    } else {
        0
    };
    let gz = if settings.map(|c| c.gzip).unwrap_or(true) {
        encoding_quality(headers, "gzip")
    } else {
        0
    };
    if br == 0 && gz == 0 {
        return response;
    }
    let encoding = if br >= gz { "br" } else { "gzip" };
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let slots = SLOTS.get_or_init(|| {
        Arc::new(Semaphore::new(
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1)
                .min(8),
        ))
    });
    let Ok(permit) = slots.clone().try_acquire_owned() else {
        return response;
    };
    let original_size = bytes.len();
    let input = bytes.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let _permit = permit;
        if encoding == "gzip" {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&input)?;
            encoder.finish()
        } else {
            let mut output = Vec::new();
            let params = brotli::enc::BrotliEncoderParams {
                quality: 4,
                ..Default::default()
            };
            brotli::enc::BrotliCompress(&mut std::io::Cursor::new(input), &mut output, &params)?;
            Ok(output)
        }
    })
    .await;
    let Ok(Ok(compressed)) = result else {
        return response;
    };
    if compressed.len() >= original_size {
        return response;
    }
    let (mut parts, _) = response.into_parts();
    parts
        .headers
        .insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
    parts
        .headers
        .insert(header::CONTENT_LENGTH, compressed.len().into());
    if let Some(etag) = parts
        .headers
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
    {
        if !etag.starts_with("W/") {
            if let Ok(weak) = format!("W/{etag}").parse() {
                parts.headers.insert(header::ETAG, weak);
            }
        }
    }
    parts.headers.remove(header::ACCEPT_RANGES);
    Response::from_parts(parts, ResponseBody::new(Bytes::from(compressed)))
}
