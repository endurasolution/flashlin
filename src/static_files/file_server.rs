use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::http::response::ResponseBody;
use bytes::Bytes;
use hyper::header::{self};
use hyper::{Response, StatusCode};
use mime_guess::MimeGuess;
use tokio::fs;
use tracing::debug;

use super::cache_headers::{apply_cache_headers, is_not_modified, make_etag};
use super::path_security::{safe_join, validate_url_path};
use crate::cache::{CachedEntry, ResponseCache};
use crate::config::ServerConfig;
use crate::error::{FlashError, Result};
use crate::http::response::HyperResponse;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryIndex {
    Php(PathBuf),
    Static(PathBuf),
    Missing,
}

pub fn resolve_directory_index(
    url_path: &str,
    root: &str,
    index_files: &[&str],
    prefer_php: bool,
) -> Result<Option<DirectoryIndex>> {
    validate_url_path(url_path)?;
    let fs_path = safe_join(root, url_path)?;
    // Already known to be a regular file, so there is no directory index to
    // resolve and the stat below would only repeat what serving it will do.
    if is_verified_file(&fs_path) {
        return Ok(None);
    }
    let meta = match std::fs::metadata(&fs_path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(FlashError::AccessDenied),
        Err(e) => return Err(FlashError::Io(e)),
    };

    if !meta.is_dir() {
        return Ok(None);
    }

    if prefer_php {
        if let Some(index) = metadata_file(fs_path.join("index.php"))? {
            return Ok(Some(DirectoryIndex::Php(index)));
        }
    }

    for idx in index_files {
        if let Some(index) = metadata_file(fs_path.join(idx))? {
            let is_php = index
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("php"))
                .unwrap_or(false);
            return Ok(Some(if is_php {
                DirectoryIndex::Php(index)
            } else {
                DirectoryIndex::Static(index)
            }));
        }
    }

    if !prefer_php {
        if let Some(index) = metadata_file(fs_path.join("index.php"))? {
            return Ok(Some(DirectoryIndex::Php(index)));
        }
    }

    if let Some(index) = metadata_file(fs_path.join("index.html"))? {
        return Ok(Some(DirectoryIndex::Static(index)));
    }

    Ok(Some(DirectoryIndex::Missing))
}

/// Serve a static file with in-memory response cache.
/// Falls back to disk on cache miss and populates the cache for future requests.
pub async fn serve_static_cached(
    url_path: &str,
    root: &str,
    index_files: &[&str],
    cache_control: Option<&str>,
    directory_listing: bool,
    config: &Arc<ServerConfig>,
    req_headers: Option<&hyper::HeaderMap>,
    cache: &Arc<ResponseCache>,
    method: &hyper::Method,
) -> Result<HyperResponse> {
    let headers = req_headers.unwrap_or_else(|| empty_headers());
    let fresh_key = fresh_response_key(
        url_path,
        root,
        index_files,
        cache_control,
        headers,
        cache,
        method,
    );
    if fresh_lookup_allowed(headers) {
        if let Some(entry) = fresh_key.as_deref().and_then(|key| cache.get(key)) {
            if let Some(stored_headers) = &entry.headers {
                validate_url_path(url_path)?;
                let mut response =
                    Response::new(ResponseBody::new(if method == hyper::Method::HEAD {
                        Bytes::new()
                    } else {
                        entry.body.clone()
                    }));
                *response.headers_mut() = stored_headers.clone();
                response
                    .headers_mut()
                    .insert("x-cache", header::HeaderValue::from_static("HIT"));
                return Ok(response);
            }
        }
    }
    if config.server.direct_static_io && likely_small_file(url_path, root) {
        // Fast path: resolve, open and read directly on this async worker,
        // skipping the blocking-pool handoff entirely. Bounded to files
        // recently observed to be small: a synchronous read blocks this
        // worker thread for its full duration, and measurement showed that
        // for multi-KiB+ files under concurrency this starves the other
        // connections sharing the (few) async worker threads far more than
        // the blocking-pool handoff it saves — the blocking pool's extra OS
        // threads let the kernel time-slice those reads fairly instead of
        // serializing them onto worker-thread count. See `likely_small_file`
        // and the `direct_static_io` config doc.
        return prepare_static(
            url_path,
            root,
            index_files,
            cache_control,
            directory_listing,
            config,
            req_headers,
            cache,
            method,
            fresh_key.as_deref(),
        );
    }

    // File APIs are blocking on the supported platforms. Batch resolution,
    // open, metadata and the first read into one blocking-pool job, instead of
    // scheduling and waking the request once for each filesystem operation.
    let url_path = url_path.to_owned();
    let root = root.to_owned();
    let index_files: Vec<String> = index_files.iter().map(|s| (*s).to_owned()).collect();
    let cache_control = cache_control.map(str::to_owned);
    let config = Arc::clone(config);
    let headers = req_headers.cloned();
    let cache = Arc::clone(cache);
    let method = method.clone();
    tokio::task::spawn_blocking(move || {
        let indexes: Vec<&str> = index_files.iter().map(String::as_str).collect();
        prepare_static(
            &url_path,
            &root,
            &indexes,
            cache_control.as_deref(),
            directory_listing,
            &config,
            headers.as_ref(),
            &cache,
            &method,
            fresh_key.as_deref(),
        )
    })
    .await
    .map_err(|error| FlashError::Io(std::io::Error::other(error)))?
}

fn prepare_static(
    url_path: &str,
    root: &str,
    index_files: &[&str],
    cache_control: Option<&str>,
    directory_listing: bool,
    config: &Arc<ServerConfig>,
    req_headers: Option<&hyper::HeaderMap>,
    cache: &ResponseCache,
    method: &hyper::Method,
    fresh_key: Option<&str>,
) -> Result<HyperResponse> {
    use std::fs;
    use std::io::Seek;
    validate_url_path(url_path)?;
    let base_path = safe_join(root, url_path)?;
    debug!(path = %base_path.display(), "Serving static");

    // Block direct PHP file access (PHP is dispatched by the router, not here).
    if is_php_path(&base_path) {
        return Err(FlashError::AccessDenied);
    }

    let canonical_root = super::path_security::canonical_root(root);
    let mut direct_file = true;

    // ── Resolve directory → concrete file ───────────────────────────────────
    // A path recently confirmed to be a symlink-free regular file inside the
    // root needs neither the preliminary stat nor realpath again. The opened
    // file is checked below, so replacing a warmed path with a symlink cannot
    // use this optimization to escape the document root.
    let mut selected = if is_verified_file(&base_path) {
        base_path
    } else {
        let base_meta = match fs::metadata(&base_path) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                return Err(FlashError::AccessDenied)
            }
            Err(e) => return Err(FlashError::Io(e)),
        };

        let (fs_path, meta) = if base_meta.as_ref().is_some_and(|m| m.is_dir()) {
            direct_file = false;
            let mut found: Option<(PathBuf, std::fs::Metadata)> = None;
            for idx in index_files {
                let candidate = base_path.join(idx);
                match fs::metadata(&candidate) {
                    Ok(m) if m.is_file() => {
                        found = Some((candidate, m));
                        break;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                        return Err(FlashError::AccessDenied);
                    }
                    Err(e) => return Err(FlashError::Io(e)),
                }
            }
            match found {
                Some((path, meta)) => (path, Some(meta)),
                None => {
                    if directory_listing {
                        return render_directory_listing(&base_path, url_path);
                    }
                    return Err(FlashError::NotFound);
                }
            }
        } else {
            (base_path, base_meta)
        };

        if is_php_path(&fs_path) {
            return Err(FlashError::AccessDenied);
        }

        // Symlinks must resolve to somewhere inside the document root.
        let canonical = fs::canonicalize(&fs_path).map_err(map_io_error)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(FlashError::AccessDenied);
        }
        // Only a symlink makes the canonical path differ; then the metadata
        // above described the link target's path rather than this one.
        let meta = match meta {
            Some(meta) if canonical == fs_path => meta,
            _ => fs::metadata(&canonical).map_err(map_io_error)?,
        };
        if !meta.is_file() {
            return Err(FlashError::NotFound);
        }
        if canonical == fs_path {
            remember_verified_file(&fs_path);
        }
        canonical
    };

    let headers: &hyper::HeaderMap = match req_headers {
        Some(headers) => headers,
        None => empty_headers(),
    };

    let (mut file, mut meta) = open_file_within_root(&selected, &canonical_root)?;
    // Content type follows the requested resource, not a compressed sidecar.
    let mime = mime_header(&selected);
    let source_modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

    let mut encoding = None;
    // Range requests use the identity representation. Sidecars must be at least
    // as new as the source to avoid serving a previous deployment's bytes.
    if !headers.contains_key(header::RANGE) && !cache_control.unwrap_or("").contains("no-transform")
    {
        let settings = config.compression.as_ref();
        let mut candidates = [
            ("br", settings.map(|c| c.brotli).unwrap_or(true)),
            ("gzip", settings.map(|c| c.gzip).unwrap_or(true)),
        ];
        candidates.sort_by_key(|(name, _)| {
            std::cmp::Reverse(crate::http::request::encoding_quality(headers, name))
        });
        for (name, enabled) in candidates {
            if !enabled || crate::http::request::encoding_quality(headers, name) == 0 {
                continue;
            }
            let mut sidecar = selected.as_os_str().to_os_string();
            sidecar.push(if name == "br" { ".br" } else { ".gz" });
            if let Ok(path) = fs::canonicalize(PathBuf::from(sidecar)) {
                if !path.starts_with(&canonical_root) {
                    continue;
                }
                if let Ok(sidecar_meta) = fs::metadata(&path) {
                    if sidecar_meta.is_file()
                        && sidecar_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
                            >= source_modified
                    {
                        if let Ok((sidecar_file, opened_meta)) =
                            open_file_within_root(&path, &canonical_root)
                        {
                            // Freshness must describe the descriptor we read,
                            // even if a deployment replaced the sidecar after stat.
                            if opened_meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
                                >= source_modified
                            {
                                selected = path;
                                file = sidecar_file;
                                meta = opened_meta;
                                encoding = Some(name);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    let size = meta.len();
    remember_size(&selected, size);
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let etag = make_etag(size, modified);
    let cc = cache_control.unwrap_or("public, max-age=3600");
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, size)
        .header(header::ACCEPT_RANGES, "bytes")
        .body(ResponseBody::new(Bytes::new()))
        .unwrap();
    apply_cache_headers(response.headers_mut(), &etag, modified, Some(cc));
    crate::middleware::compression::vary_encoding(response.headers_mut());
    if let Some(encoding) = encoding {
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, encoding.parse().unwrap());
        // Even equal-size, equal-mtime sidecars are distinct representations.
        response.headers_mut().insert(
            header::ETAG,
            format!(
                "W/\"{}-{}\"",
                etag.trim_start_matches("W/").trim_matches('"'),
                encoding
            )
            .parse()
            .unwrap(),
        );
    }
    let representation_etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_string();
    if is_not_modified(headers, &representation_etag, modified) {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response.headers_mut().remove(header::CONTENT_LENGTH);
        return Ok(response);
    }

    let range = if method == hyper::Method::GET
        && if_range_matches(headers, &representation_etag, modified)
    {
        parse_range(
            headers.get(header::RANGE).and_then(|h| h.to_str().ok()),
            size,
        )
    } else {
        Ok(None)
    };
    let range = match range {
        Ok(range) => range,
        Err(()) => {
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes */{size}").parse().unwrap(),
            );
            response
                .headers_mut()
                .insert(header::CONTENT_LENGTH, 0.into());
            return Ok(response);
        }
    };
    if method == hyper::Method::HEAD {
        return Ok(response);
    }
    if let Some((start, end)) = range {
        file.seek(std::io::SeekFrom::Start(start))
            .map_err(map_io_error)?;
        let length = end - start + 1;
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{size}").parse().unwrap(),
        );
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length.into());
        *response.body_mut() = super::file_io::read_body(file, length).map_err(map_io_error)?;
        return Ok(response);
    }

    let cacheable = cache.can_cache(size)
        && !cache_directive(cc, "no-store")
        && !request_cache_directive(headers, "no-store");
    if cacheable {
        // Built only when it will actually be used; formatting a key for a
        // disabled cache was pure overhead on every request.
        let fresh_key = fresh_key.filter(|_| direct_file);
        let cache_key = fresh_key
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}:{}:{}", selected.display(), representation_etag, cc));
        if let Some(entry) = cache.get(&cache_key).filter(|entry| {
            entry.headers.as_ref().is_none_or(|stored| {
                stored.get(header::ETAG) == response.headers().get(header::ETAG)
            })
        }) {
            response
                .headers_mut()
                .insert("x-cache", "HIT".parse().unwrap());
            *response.body_mut() = ResponseBody::new(entry.body.clone());
            return Ok(response);
        }
        let data =
            super::file_io::read_exact_bytes(&mut file, size as usize).map_err(map_io_error)?;
        let entry = if fresh_key.is_some() {
            CachedEntry::new(data.clone(), cache.revalidate_after())
                .with_headers(response.headers().clone(), cache_key.len())
        } else {
            CachedEntry::new(data.clone(), cache.default_ttl())
        };
        cache.insert(cache_key, entry);
        response
            .headers_mut()
            .insert("x-cache", "MISS".parse().unwrap());
        *response.body_mut() = ResponseBody::new(data);
    } else {
        response
            .headers_mut()
            .insert("x-cache", "BYPASS".parse().unwrap());
        *response.body_mut() = super::file_io::read_body(file, size).map_err(map_io_error)?;
    }
    Ok(response)
}

fn cache_directive(value: &str, name: &str) -> bool {
    value.split(',').any(|part| {
        part.trim()
            .split('=')
            .next()
            .is_some_and(|directive| directive.trim().eq_ignore_ascii_case(name))
    })
}

fn request_cache_directive(headers: &hyper::HeaderMap, name: &str) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| cache_directive(value, name))
}

fn fresh_lookup_allowed(headers: &hyper::HeaderMap) -> bool {
    ![
        header::RANGE,
        header::IF_RANGE,
        header::IF_MATCH,
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
        header::IF_UNMODIFIED_SINCE,
    ]
    .iter()
    .any(|name| headers.contains_key(name))
        && !request_cache_directive(headers, "no-cache")
        && !request_cache_directive(headers, "no-store")
        && !headers
            .get_all(header::PRAGMA)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| cache_directive(v, "no-cache"))
        && !headers
            .get_all(header::CACHE_CONTROL)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|part| {
                part.trim().split_once('=').is_some_and(|(name, value)| {
                    name.trim().eq_ignore_ascii_case("max-age")
                        && value.trim().trim_matches('"').parse::<u64>() == Ok(0)
                })
            })
}

fn fresh_response_key(
    url_path: &str,
    root: &str,
    indexes: &[&str],
    cache_control: Option<&str>,
    headers: &hyper::HeaderMap,
    cache: &ResponseCache,
    method: &hyper::Method,
) -> Option<String> {
    if cache.revalidate_after().is_zero()
        || (method != hyper::Method::GET && method != hyper::Method::HEAD)
        || cache_directive(cache_control.unwrap_or(""), "no-store")
        || cache_directive(cache_control.unwrap_or(""), "no-cache")
        || request_cache_directive(headers, "no-store")
    {
        return None;
    }
    // Debug string quoting keeps separators in user-controlled paths unambiguous.
    Some(format!(
        "fresh:{root:?}:{url_path:?}:{indexes:?}:{cache_control:?}:br{}:gzip{}",
        crate::http::request::encoding_quality(headers, "br"),
        crate::http::request::encoding_quality(headers, "gzip")
    ))
}

/// Paths recently confirmed to be symlink-free regular files inside their root.
///
/// Resolving a static path cost a `stat` plus a `realpath` on every request.
/// Both answers are stable for any given file, so they are remembered briefly.
/// Only paths that canonicalise to themselves are recorded — a path that
/// resolves through a symlink always takes the full check — and the entry only
/// suppresses preliminary lookups. Every open still verifies containment and
/// obtains metadata from the opened file, so replacement, deletion and symlink
/// changes take effect immediately rather than after the TTL.
static VERIFIED_FILES: std::sync::OnceLock<dashmap::DashMap<PathBuf, Instant>> =
    std::sync::OnceLock::new();

const VERIFIED_TTL: Duration = Duration::from_secs(5);

pub fn is_verified_file(path: &Path) -> bool {
    let cache = VERIFIED_FILES.get_or_init(dashmap::DashMap::new);
    cache
        .get(path)
        .is_some_and(|seen| seen.elapsed() < VERIFIED_TTL)
}

pub(crate) fn remember_verified_file(path: &Path) {
    let cache = VERIFIED_FILES.get_or_init(dashmap::DashMap::new);
    if cache.len() >= 8192 && !cache.contains_key(path) {
        cache.retain(|_, seen| seen.elapsed() < VERIFIED_TTL);
        if cache.len() >= 8192 {
            return;
        }
    }
    cache.insert(path.to_path_buf(), Instant::now());
}

/// Sizes observed for recently served files, used only to decide whether
/// `direct_static_io`'s worker-thread fast path is worth its tradeoff for a
/// given file without an extra `stat`. Independent of `VERIFIED_FILES`,
/// which exists to skip path/symlink resolution, not to size-gate anything.
/// A stale or missing entry just falls back to the always-safe blocking pool.
static RECENT_SIZES: std::sync::OnceLock<dashmap::DashMap<PathBuf, (Instant, u64)>> =
    std::sync::OnceLock::new();

/// A synchronous read below this size costs at most tens of microseconds
/// even from a cold page-cache line, so running it inline on an async worker
/// is worth skipping the blocking-pool handoff. Measurement on the Ubuntu
/// benchmark harness showed 64 KiB reads already serializing badly across
/// only a couple of worker threads under concurrency, so this stays well
/// under that: it deliberately matches the medium-buffer size class's lower
/// bound in `file_io.rs`, the point where that module itself stops treating
/// a read as "small".
const DIRECT_IO_MAX_BYTES: u64 = 16 * 1024;

fn remember_size(path: &Path, size: u64) {
    let cache = RECENT_SIZES.get_or_init(dashmap::DashMap::new);
    if cache.len() >= 8192 && !cache.contains_key(path) {
        cache.retain(|_, (seen, _)| seen.elapsed() < VERIFIED_TTL);
        if cache.len() >= 8192 {
            return;
        }
    }
    cache.insert(path.to_path_buf(), (Instant::now(), size));
}

/// Best-effort only: a fresh cache miss (first request for a path, or one
/// whose entry aged out) routes through the blocking pool regardless of this
/// function's answer, so it never needs a `stat` of its own.
fn likely_small_file(url_path: &str, root: &str) -> bool {
    let Ok(path) = safe_join(root, url_path) else {
        return false;
    };
    RECENT_SIZES
        .get_or_init(dashmap::DashMap::new)
        .get(&path)
        .is_some_and(|entry| entry.0.elapsed() < VERIFIED_TTL && entry.1 <= DIRECT_IO_MAX_BYTES)
}

/// Content type for a path, interned by extension.
///
/// `MimeGuess` built a fresh `String` and re-parsed it into a header value on
/// every request; the set of extensions actually served is small and fixed.
pub(crate) fn mime_header(path: &Path) -> header::HeaderValue {
    static MIME_CACHE: std::sync::OnceLock<dashmap::DashMap<String, header::HeaderValue>> =
        std::sync::OnceLock::new();
    let cache = MIME_CACHE.get_or_init(dashmap::DashMap::new);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if let Some(found) = cache.get(ext) {
        return found.clone();
    }
    let guessed = MimeGuess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let value = header::HeaderValue::from_str(&guessed)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));
    if cache.len() < 1024 {
        cache.insert(ext.to_owned(), value.clone());
    }
    value
}

fn empty_headers() -> &'static hyper::HeaderMap {
    static EMPTY: std::sync::OnceLock<hyper::HeaderMap> = std::sync::OnceLock::new();
    EMPTY.get_or_init(hyper::HeaderMap::new)
}

/// Multiple ranges are intentionally ignored (serve 200); this avoids multipart
/// amplification. Malformed syntax is ignored; valid unsatisfiable ranges get 416.
fn parse_range(value: Option<&str>, size: u64) -> std::result::Result<Option<(u64, u64)>, ()> {
    let Some(value) = value.and_then(|s| s.strip_prefix("bytes=")) else {
        return Ok(None);
    };
    if value.contains(',') {
        return Ok(None);
    }
    let Some((start, end)) = value.trim().split_once('-') else {
        return Ok(None);
    };
    if start.is_empty() {
        let Ok(suffix) = end.parse::<u64>() else {
            return Ok(None);
        };
        if suffix == 0 || size == 0 {
            return Err(());
        }
        return Ok(Some((size.saturating_sub(suffix), size - 1)));
    }
    let Ok(start) = start.parse::<u64>() else {
        return Ok(None);
    };
    let end = if end.is_empty() {
        size.saturating_sub(1)
    } else {
        let Ok(end) = end.parse::<u64>() else {
            return Ok(None);
        };
        if end < start {
            return Ok(None);
        }
        end.min(size.saturating_sub(1))
    };
    if start >= size {
        return Err(());
    }
    Ok(Some((start, end)))
}

fn if_range_matches(headers: &hyper::HeaderMap, etag: &str, modified: SystemTime) -> bool {
    let Some(value) = headers.get(header::IF_RANGE) else {
        return true;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    if value.starts_with('"') {
        return !etag.starts_with("W/") && value == etag;
    }
    httpdate::parse_http_date(value).ok().is_some_and(|date| {
        modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            <= date
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
    })
}

// ─── Legacy helpers (used by tests and the original serve path) ───────────────

pub async fn serve_static(
    url_path: &str,
    root: &str,
    index_files: &[&str],
    cache_control: Option<&str>,
    directory_listing: bool,
    config: &Arc<ServerConfig>,
) -> Result<HyperResponse> {
    serve_static_with_headers(
        url_path,
        root,
        index_files,
        cache_control,
        directory_listing,
        config,
        None,
    )
    .await
}

pub async fn serve_static_with_headers(
    url_path: &str,
    root: &str,
    index_files: &[&str],
    cache_control: Option<&str>,
    directory_listing: bool,
    _config: &Arc<ServerConfig>,
    req_headers: Option<&hyper::HeaderMap>,
) -> Result<HyperResponse> {
    validate_url_path(url_path)?;
    let fs_path = safe_join(root, url_path)?;
    debug!(path = %fs_path.display(), "Serving static");

    let fs_meta = match fs::metadata(&fs_path).await {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(FlashError::AccessDenied),
        Err(e) => return Err(FlashError::Io(e)),
    };

    let fs_path = if fs_meta.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
        let mut found = None;
        for idx in index_files {
            let candidate = fs_path.join(idx);
            match fs::metadata(&candidate).await {
                Ok(meta) if meta.is_file() => {
                    found = Some(candidate);
                    break;
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                    return Err(FlashError::AccessDenied);
                }
                Err(e) => return Err(FlashError::Io(e)),
            }
        }
        match found {
            Some(p) => p,
            None => {
                if directory_listing {
                    return render_directory_listing(&fs_path, url_path);
                }
                return Err(FlashError::NotFound);
            }
        }
    } else {
        fs_path
    };

    if is_php_path(&fs_path) {
        return Err(FlashError::AccessDenied);
    }

    let meta = fs::metadata(&fs_path).await.map_err(map_io_error)?;
    if !meta.is_file() {
        return Err(FlashError::NotFound);
    }

    let size = meta.len();
    let modified: SystemTime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let etag = make_etag(size, modified);
    let mime = MimeGuess::from_path(&fs_path)
        .first_or_octet_stream()
        .to_string();

    if let Some(rh) = req_headers {
        if is_not_modified(rh, &etag, modified) {
            let mut resp = Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .body(ResponseBody::new(Bytes::new()))
                .unwrap();
            apply_cache_headers(resp.headers_mut(), &etag, modified, cache_control);
            return Ok(resp);
        }
    }

    let body = fs::read(&fs_path).await.map_err(map_io_error)?;

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &mime)
        .header(header::CONTENT_LENGTH, size.to_string())
        .body(ResponseBody::new(Bytes::from(body)))
        .unwrap();
    apply_cache_headers(resp.headers_mut(), &etag, modified, cache_control);

    Ok(resp)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

pub(crate) fn is_php_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("php"))
        .unwrap_or(false)
}

/// Verify the file actually opened, not only a pathname checked before open.
/// Linux exposes the descriptor's resolved path through procfs in one readlink
/// syscall; unlike a cached realpath, it detects replaced leaf and parent links.
pub(crate) fn open_file_within_root(
    path: &Path,
    canonical_root: &Path,
) -> Result<(std::fs::File, std::fs::Metadata)> {
    #[cfg(target_os = "linux")]
    let (file, opened_path) = {
        use std::os::fd::AsRawFd;
        let file = std::fs::File::open(path).map_err(map_io_error)?;
        let opened_path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(map_io_error)?;
        (file, opened_path)
    };
    #[cfg(not(target_os = "linux"))]
    let (file, opened_path) = {
        // Platforms without procfs must revalidate every request rather than
        // trusting the five-second preliminary-path cache for containment.
        let opened_path = std::fs::canonicalize(path).map_err(map_io_error)?;
        if !opened_path.starts_with(canonical_root) || is_php_path(&opened_path) {
            return Err(FlashError::AccessDenied);
        }
        let file = std::fs::File::open(&opened_path).map_err(map_io_error)?;
        (file, opened_path)
    };
    if !opened_path.starts_with(canonical_root) || is_php_path(&opened_path) {
        return Err(FlashError::AccessDenied);
    }
    let meta = file.metadata().map_err(map_io_error)?;
    if !meta.is_file() {
        return Err(FlashError::NotFound);
    }
    Ok((file, meta))
}

fn metadata_file(path: PathBuf) -> Result<Option<PathBuf>> {
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(Some(path)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => Err(FlashError::AccessDenied),
        Err(e) => Err(FlashError::Io(e)),
    }
}

fn map_io_error(e: std::io::Error) -> FlashError {
    match e.kind() {
        ErrorKind::NotFound => FlashError::NotFound,
        ErrorKind::PermissionDenied => FlashError::AccessDenied,
        _ => FlashError::Io(e),
    }
}

// ─── Directory listing ────────────────────────────────────────────────────────

fn render_directory_listing(dir: &Path, url_path: &str) -> Result<HyperResponse> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(FlashError::Io)?
        .filter_map(|e| e.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            (name, is_dir)
        })
        .collect::<Vec<_>>();

    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut html = format!(
        "<!DOCTYPE html><html><head><title>Index of {url_path}</title>\
         <style>body{{font-family:monospace;padding:1em}}a{{display:block;padding:2px}}</style>\
         </head><body><h1>Index of {url_path}</h1><hr>"
    );

    if url_path != "/" {
        html.push_str("<a href=\"../\">[Parent Directory]</a>");
    }

    for (name, is_dir) in &entries {
        let display = if *is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        let href = if *is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        html.push_str(&format!("<a href=\"{href}\">{display}</a>"));
    }

    html.push_str("<hr></body></html>");

    Ok(Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(ResponseBody::new(Bytes::from(html)))
        .unwrap())
}

#[cfg(test)]
mod performance_regressions {
    use super::*;
    use http_body_util::BodyExt;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("static-perf-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        async fn get(
            &self,
            config: &Arc<ServerConfig>,
            cache: &Arc<ResponseCache>,
            cc: Option<&str>,
        ) -> Result<HyperResponse> {
            serve_static_cached(
                "/asset.bin",
                self.0.to_str().unwrap(),
                &["index.html"],
                cc,
                false,
                config,
                None,
                cache,
                &hyper::Method::GET,
            )
            .await
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn configured_large_file_cache_limits_and_no_store_are_respected() {
        let fixture = Fixture::new();
        let data: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::fs::write(fixture.0.join("asset.bin"), &data).unwrap();
        let mut cfg = crate::default_config();
        cfg.cache.as_mut().unwrap().max_file_size = "4MB".into();
        cfg.cache.as_mut().unwrap().max_size = "8MB".into();
        let cfg = Arc::new(cfg);
        let cache = Arc::new(ResponseCache::from_config(&cfg));
        for expected in ["MISS", "HIT"] {
            let response = fixture.get(&cfg, &cache, None).await.unwrap();
            assert_eq!(response.headers()["x-cache"], expected);
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                data
            );
        }
        assert_eq!(cache.stats().1, data.len());
        let response = fixture.get(&cfg, &cache, Some("no-store")).await.unwrap();
        assert_eq!(response.headers()["x-cache"], "BYPASS");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            data
        );
        let mut limited = (*cfg).clone();
        limited.cache.as_mut().unwrap().max_file_size = "2MB".into();
        let limited = Arc::new(limited);
        let cache = Arc::new(ResponseCache::from_config(&limited));
        let response = fixture.get(&limited, &cache, None).await.unwrap();
        assert_eq!(response.headers()["x-cache"], "BYPASS");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            data
        );
        assert_eq!(cache.stats().1, 0);
    }

    #[tokio::test]
    async fn warmed_files_observe_replacement_and_deletion() {
        let fixture = Fixture::new();
        let path = fixture.0.join("asset.bin");
        std::fs::write(&path, b"original").unwrap();
        let cfg = Arc::new(crate::default_config());
        let cache = Arc::new(ResponseCache::from_config(&cfg));
        let response = fixture.get(&cfg, &cache, None).await.unwrap();
        let old_etag = response.headers()[header::ETAG].clone();
        response.into_body().collect().await.unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"replacement content").unwrap();
        let response = fixture.get(&cfg, &cache, None).await.unwrap();
        assert_ne!(response.headers()[header::ETAG], old_etag);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "replacement content"
        );
        std::fs::remove_file(path).unwrap();
        assert!(matches!(
            fixture.get(&cfg, &cache, None).await,
            Err(FlashError::NotFound)
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn warmed_file_symlink_cannot_escape_document_root() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let path = fixture.0.join("asset.bin");
        let secret = outside.0.join("secret.bin");
        std::fs::write(&path, b"public").unwrap();
        std::fs::write(&secret, b"private").unwrap();
        let cfg = Arc::new(crate::default_config());
        let cache = Arc::new(ResponseCache::from_config(&cfg));
        fixture
            .get(&cfg, &cache, None)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap();
        assert!(is_verified_file(&path));
        std::fs::remove_file(&path).unwrap();
        symlink(&secret, &path).unwrap();
        assert!(matches!(
            fixture.get(&cfg, &cache, None).await,
            Err(FlashError::AccessDenied)
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn warmed_parent_symlink_cannot_escape_document_root() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let nested = fixture.0.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("asset.bin"), b"public").unwrap();
        std::fs::write(outside.0.join("asset.bin"), b"private").unwrap();
        let cfg = Arc::new(crate::default_config());
        let cache = Arc::new(ResponseCache::from_config(&cfg));
        let root = fixture.0.to_str().unwrap();
        let response = serve_static_cached(
            "/nested/asset.bin",
            root,
            &["index.html"],
            None,
            false,
            &cfg,
            None,
            &cache,
            &hyper::Method::GET,
        )
        .await
        .unwrap();
        response.into_body().collect().await.unwrap();
        assert!(is_verified_file(&nested.join("asset.bin")));
        std::fs::rename(&nested, fixture.0.join("original-nested")).unwrap();
        symlink(&outside.0, &nested).unwrap();
        assert!(matches!(
            serve_static_cached(
                "/nested/asset.bin",
                root,
                &["index.html"],
                None,
                false,
                &cfg,
                None,
                &cache,
                &hyper::Method::GET,
            )
            .await,
            Err(FlashError::AccessDenied)
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn warmed_symlink_allows_public_files_inside_root_but_blocks_php() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let path = fixture.0.join("asset.bin");
        let inside = fixture.0.join("other.bin");
        let php = fixture.0.join("secret.php");
        std::fs::write(&path, b"original").unwrap();
        std::fs::write(&inside, b"replacement public content").unwrap();
        std::fs::write(&php, b"<?php $secret = 'private';").unwrap();
        let cfg = Arc::new(crate::default_config());
        let cache = Arc::new(ResponseCache::from_config(&cfg));
        fixture
            .get(&cfg, &cache, None)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&inside, &path).unwrap();
        assert_eq!(
            fixture
                .get(&cfg, &cache, None)
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
            "replacement public content"
        );
        std::fs::remove_file(&path).unwrap();
        symlink(&php, &path).unwrap();
        assert!(matches!(
            fixture.get(&cfg, &cache, None).await,
            Err(FlashError::AccessDenied)
        ));
    }
}
