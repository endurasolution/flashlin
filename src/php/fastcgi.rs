/// Full FastCGI protocol implementation over a TCP or Unix socket.
///
/// Supports PHP-FPM and any other FastCGI-compatible process manager.
use std::collections::HashMap;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

use super::errors::PhpError;

// ─── Record type constants ────────────────────────────────────────────────────
const FCGI_VERSION: u8 = 1;
const FCGI_BEGIN_REQUEST: u8 = 1;
const FCGI_ABORT_REQUEST: u8 = 2;
const FCGI_END_REQUEST: u8 = 3;
const FCGI_PARAMS: u8 = 4;
const FCGI_STDIN: u8 = 5;
const FCGI_STDOUT: u8 = 6;
const FCGI_STDERR: u8 = 7;
const FCGI_RESPONDER: u16 = 1;
const FCGI_REQUEST_COMPLETE: u8 = 0;
const REQUEST_ID: u16 = 1;

// ─── Record encoding ─────────────────────────────────────────────────────────

fn write_record(out: &mut BytesMut, rec_type: u8, content: &[u8]) {
    let content_len = content.len() as u16;
    // Pad content to 8-byte boundary.
    let padding = (8 - (content_len % 8)) % 8;
    out.put_u8(FCGI_VERSION);
    out.put_u8(rec_type);
    out.put_u16(REQUEST_ID);
    out.put_u16(content_len);
    out.put_u8(padding as u8);
    out.put_u8(0); // reserved
    out.put_slice(content);
    for _ in 0..padding {
        out.put_u8(0);
    }
}

fn encode_param_name_value(name: &str, value: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_len(&mut buf, name.len());
    encode_len(&mut buf, value.len());
    buf.extend_from_slice(name.as_bytes());
    buf.extend_from_slice(value.as_bytes());
    buf
}

fn encode_len(buf: &mut Vec<u8>, len: usize) {
    if len < 128 {
        buf.push(len as u8);
    } else {
        buf.push(((len >> 24) as u8) | 0x80);
        buf.push((len >> 16) as u8);
        buf.push((len >> 8) as u8);
        buf.push(len as u8);
    }
}

/// Encode all params into a single PARAMS record (or multiple if needed).
fn encode_params(params: &HashMap<String, String>) -> Vec<u8> {
    let mut buf = Vec::new();
    for (k, v) in params {
        buf.extend_from_slice(&encode_param_name_value(k, v));
    }
    buf
}

// ─── BEGIN_REQUEST body ───────────────────────────────────────────────────────

fn begin_request_body() -> [u8; 8] {
    let mut body = [0u8; 8];
    body[0] = (FCGI_RESPONDER >> 8) as u8;
    body[1] = (FCGI_RESPONDER & 0xff) as u8;
    body[2] = 0; // flags: 0 = no keep-conn (close after request)
    body
}

// ─── Record reader ────────────────────────────────────────────────────────────

struct FcgiRecord {
    rec_type: u8,
    content: Vec<u8>,
}

async fn read_record<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<FcgiRecord, PhpError> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).await?;
    let rec_type = header[1];
    let content_len = u16::from_be_bytes([header[4], header[5]]) as usize;
    let padding_len = header[6] as usize;

    let mut content = vec![0u8; content_len];
    if content_len > 0 {
        reader.read_exact(&mut content).await?;
    }
    // Drain padding.
    if padding_len > 0 {
        let mut pad = vec![0u8; padding_len];
        reader.read_exact(&mut pad).await?;
    }
    Ok(FcgiRecord { rec_type, content })
}

// ─── FastCGI response parser ──────────────────────────────────────────────────

pub struct FastCgiResponse {
    /// Parsed response headers (e.g. Status, Content-Type).
    pub headers: Vec<(String, String)>,
    /// HTTP status code extracted from the "Status:" header.
    pub status: u16,
    /// Response body.
    pub body: Bytes,
}

fn parse_fcgi_stdout(stdout: &[u8]) -> Result<FastCgiResponse, PhpError> {
    // PHP-FPM sends "Header: Value\r\n\r\nBody".
    let sep = stdout
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| stdout.windows(2).position(|w| w == b"\n\n"))
        .ok_or_else(|| PhpError::InvalidResponse("No header/body separator".into()))?;

    let header_part = &stdout[..sep];
    let body_start = if stdout[sep..].starts_with(b"\r\n\r\n") {
        sep + 4
    } else {
        sep + 2
    };
    let body = Bytes::copy_from_slice(&stdout[body_start..]);

    let mut headers = Vec::new();
    let mut status = 200u16;

    for line in header_part.split(|&b| b == b'\n') {
        let line = line.trim_ascii_end(); // strip \r
        if line.is_empty() {
            continue;
        }
        if let Some(pos) = line.iter().position(|&b| b == b':') {
            let name = String::from_utf8_lossy(&line[..pos]).trim().to_string();
            let value = String::from_utf8_lossy(&line[pos + 1..]).trim().to_string();
            if name.eq_ignore_ascii_case("status") {
                status = value
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(200);
            } else {
                headers.push((name, value));
            }
        }
    }

    Ok(FastCgiResponse {
        headers,
        status,
        body,
    })
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Send a FastCGI request over a TCP connection and return the parsed response.
pub async fn send_fastcgi_request(
    address: &str,
    params: HashMap<String, String>,
    body: &[u8],
    timeout_secs: u64,
) -> Result<FastCgiResponse, PhpError> {
    let connect = crate::server::net::connect_tcp(address);
    let mut stream = timeout(Duration::from_secs(timeout_secs), connect)
        .await
        .map_err(|_| PhpError::Timeout)?
        .map_err(|e| PhpError::Connection(e))?;

    send_request_on_stream(&mut stream, params, body, timeout_secs).await
}

/// Send over a Unix socket (Linux/macOS).  On Windows, TCP is used instead.
#[cfg(unix)]
pub async fn send_fastcgi_unix(
    socket_path: &str,
    params: HashMap<String, String>,
    body: &[u8],
    timeout_secs: u64,
) -> Result<FastCgiResponse, PhpError> {
    use tokio::net::UnixStream;
    let connect = UnixStream::connect(socket_path);
    let mut stream = timeout(Duration::from_secs(timeout_secs), connect)
        .await
        .map_err(|_| PhpError::Timeout)?
        .map_err(|e| PhpError::Connection(e))?;

    send_request_on_stream(&mut stream, params, body, timeout_secs).await
}

// Windows does not have Unix sockets accessible via tokio in the same way;
// fall back to TCP on Windows.
#[cfg(windows)]
pub async fn send_fastcgi_unix(
    _socket_path: &str,
    _params: HashMap<String, String>,
    _body: &[u8],
    _timeout_secs: u64,
) -> Result<FastCgiResponse, PhpError> {
    Err(PhpError::Protocol(
        "Unix sockets not supported on Windows; use TCP address instead".into(),
    ))
}

async fn send_request_on_stream<S>(
    stream: &mut S,
    params: HashMap<String, String>,
    body: &[u8],
    timeout_secs: u64,
) -> Result<FastCgiResponse, PhpError>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut out = BytesMut::with_capacity(4096);

    // BEGIN_REQUEST
    write_record(&mut out, FCGI_BEGIN_REQUEST, &begin_request_body());

    // PARAMS (split into ≤65535-byte chunks if needed)
    let params_data = encode_params(&params);
    for chunk in params_data.chunks(65535) {
        write_record(&mut out, FCGI_PARAMS, chunk);
    }
    write_record(&mut out, FCGI_PARAMS, &[]); // empty PARAMS = end of params

    // STDIN (request body)
    for chunk in body.chunks(65535) {
        write_record(&mut out, FCGI_STDIN, chunk);
    }
    write_record(&mut out, FCGI_STDIN, &[]); // empty STDIN = end of body

    // Send everything at once.
    let send = stream.write_all(&out);
    timeout(Duration::from_secs(timeout_secs), send)
        .await
        .map_err(|_| PhpError::Timeout)? // timeout error
        .map_err(|e| PhpError::Connection(e))?; // io::Error → PhpError::Connection

    // Read response records.
    let mut stdout_buf = Vec::new();
    let mut _stderr_buf = Vec::new();

    loop {
        // read_record returns Result<FcgiRecord, PhpError>; the inner ? uses From<PhpError>.
        let record = timeout(Duration::from_secs(timeout_secs), read_record(stream))
            .await
            .map_err(|_| PhpError::Timeout)? // timeout error
            ?; // PhpError passthrough

        match record.rec_type {
            FCGI_STDOUT => {
                if record.content.is_empty() {
                    // Empty STDOUT signals end of response body.
                } else {
                    stdout_buf.extend_from_slice(&record.content);
                }
            }
            FCGI_STDERR => {
                _stderr_buf.extend_from_slice(&record.content);
                if !record.content.is_empty() {
                    let msg = String::from_utf8_lossy(&record.content);
                    tracing::warn!("PHP stderr: {}", msg.trim());
                }
            }
            FCGI_END_REQUEST => {
                break;
            }
            _ => {}
        }
    }

    parse_fcgi_stdout(&stdout_buf)
}
