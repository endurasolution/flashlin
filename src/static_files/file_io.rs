//! Bounded, demand-driven disk reads. No filesystem work runs on an IO worker.
use std::{
    fs::File,
    io::{self, Read},
    sync::{Arc, Mutex, OnceLock},
};

use bytes::Bytes;

use crate::http::response::ResponseBody;

// Amortize blocking-pool handoffs for large transfers while bounding each
// active body's read buffer independently of the file's size.
const STREAM_CHUNK: usize = 1024 * 1024;
const MEDIUM_CHUNK: usize = 64 * 1024;

// A whole-file read at or below this size completes inside the single
// blocking job that already resolved and opened the file, instead of
// returning to the connection layer and re-entering the blocking pool for
// every additional STREAM_CHUNK. Each re-entry is a scheduling round trip
// through the executor, which is small on its own but is significant next to
// the memcpy it wraps. Larger transfers keep streaming with a bounded
// per-chunk buffer, so memory use stays independent of file size.
const INLINE_MAX: u64 = 8 * 1024 * 1024;

// Retain 32 MiB of large buffers plus 4 MiB of medium buffers. Active buffers
// remain owned by Bytes and cannot be reused until their final clone is dropped.
const MAX_IDLE_BUFFERS: usize = 32;

struct BufferPool {
    idle: Mutex<Vec<Vec<u8>>>,
    limit: usize,
    buffer_size: usize,
}

impl BufferPool {
    fn new(limit: usize, buffer_size: usize) -> Self {
        Self {
            idle: Mutex::new(Vec::new()),
            limit,
            buffer_size,
        }
    }

    fn checkout(self: &Arc<Self>) -> ReadBuffer {
        // Never wait for another transfer's pool lock on a networking worker.
        let bytes = self
            .idle
            .try_lock()
            .ok()
            .and_then(|mut idle| idle.pop())
            .unwrap_or_else(|| vec![0; self.buffer_size]);
        ReadBuffer {
            bytes,
            length: self.buffer_size,
            pool: self.clone(),
        }
    }
}

struct ReadBuffer {
    bytes: Vec<u8>,
    length: usize,
    pool: Arc<BufferPool>,
}

impl AsRef<[u8]> for ReadBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

impl Drop for ReadBuffer {
    fn drop(&mut self) {
        if let Ok(mut idle) = self.pool.idle.try_lock() {
            if idle.len() < self.pool.limit {
                idle.push(std::mem::take(&mut self.bytes));
            }
        }
    }
}

fn read_chunk(file: &mut File, length: usize) -> io::Result<Bytes> {
    // Size classes reuse common medium responses without allocating a 1 MiB
    // buffer for them. Tiny responses keep the cheaper direct allocation path.
    if (16 * 1024 + 1..=MEDIUM_CHUNK).contains(&length) {
        static MEDIUM_POOL: OnceLock<Arc<BufferPool>> = OnceLock::new();
        let pool = MEDIUM_POOL.get_or_init(|| Arc::new(BufferPool::new(64, MEDIUM_CHUNK)));
        return read_pooled_chunk(file, pool, length);
    }
    if length < STREAM_CHUNK {
        return read_exact_bytes(file, length);
    }
    debug_assert_eq!(length, STREAM_CHUNK);
    static POOL: OnceLock<Arc<BufferPool>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Arc::new(BufferPool::new(MAX_IDLE_BUFFERS, STREAM_CHUNK)));
    read_pooled_chunk(file, pool, length)
}

fn read_pooled_chunk(file: &mut File, pool: &Arc<BufferPool>, length: usize) -> io::Result<Bytes> {
    let mut buffer = pool.checkout();
    // A reused buffer may contain an earlier response. Publish it only after
    // every byte has been overwritten; partial reads return an error instead.
    file.read_exact(&mut buffer.bytes[..length])?;
    buffer.length = length;
    Ok(Bytes::from_owner(buffer))
}

pub(super) fn read_exact_bytes(file: &mut File, length: usize) -> io::Result<Bytes> {
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(Bytes::from(bytes))
}

/// Called inside the static response's blocking job. Read the initial chunk
/// here; subsequent reads happen only when the transport polls for more data.
pub(super) fn read_body(mut file: File, length: u64) -> io::Result<ResponseBody> {
    if length > STREAM_CHUNK as u64 && length <= INLINE_MAX {
        // Read the whole file here, inside the blocking job that already
        // opened it, instead of returning to the connection layer and
        // re-entering the blocking pool for every additional STREAM_CHUNK.
        // Chunks still come from the same pooled buffers as the streaming
        // path below (via `read_chunk`): one large unpooled allocation per
        // request measured worse under concurrency than the handoffs it
        // replaced, most likely from repeated mmap/munmap of multi-MiB
        // regions contending on the process-wide mmap lock.
        let mut frames = Vec::with_capacity((length as usize).div_ceil(STREAM_CHUNK));
        let mut remaining = length;
        while remaining > 0 {
            let chunk_len = remaining.min(STREAM_CHUNK as u64) as usize;
            frames.push(read_chunk(&mut file, chunk_len)?);
            remaining -= chunk_len as u64;
        }
        return Ok(ResponseBody::stream(
            futures_util::stream::iter(frames.into_iter().map(Ok)),
            Some(length),
        ));
    }
    let first = read_chunk(&mut file, length.min(STREAM_CHUNK as u64) as usize)?;
    let remaining = length - first.len() as u64;
    if remaining == 0 {
        return Ok(ResponseBody::new(first));
    }
    let stream = futures_util::stream::unfold(
        (Some(file), remaining, Some(first)),
        |(file, remaining, first)| async move {
            if let Some(first) = first {
                return Some((Ok(first), (file, remaining, None)));
            }
            if remaining == 0 {
                return None;
            }
            let mut file = file?;
            let length = remaining.min(STREAM_CHUNK as u64) as usize;
            let result = tokio::task::spawn_blocking(move || {
                let bytes = read_chunk(&mut file, length);
                (file, bytes)
            })
            .await;
            match result {
                Ok((file, Ok(bytes))) => {
                    Some((Ok(bytes), (Some(file), remaining - length as u64, None)))
                }
                Ok((_, Err(error))) => Some((Err(error), (None, 0, None))),
                Err(error) => Some((Err(io::Error::other(error)), (None, 0, None))),
            }
        },
    );
    Ok(ResponseBody::stream(stream, Some(length)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use std::io::{Seek, Write};

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(data: &[u8]) -> Self {
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("file-io-{}", uuid::Uuid::new_v4()));
            std::fs::write(&path, data).unwrap();
            Self(path)
        }
        fn open(&self) -> File {
            File::open(&self.0).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn buffers_are_reused_only_after_the_last_transport_reference_drops() {
        let pool = Arc::new(BufferPool::new(1, STREAM_CHUNK));
        let fixture = Fixture::new(&vec![7; STREAM_CHUNK]);
        let bytes = read_pooled_chunk(&mut fixture.open(), &pool, STREAM_CHUNK).unwrap();
        let pointer = bytes.as_ptr();
        let slice = bytes.slice(3..19);
        drop(bytes);
        assert!(pool.idle.lock().unwrap().is_empty());
        assert_eq!(slice.as_ref(), &[7; 16]);
        drop(slice);
        assert_eq!(pool.idle.lock().unwrap().len(), 1);
        let bytes = read_pooled_chunk(&mut fixture.open(), &pool, STREAM_CHUNK).unwrap();
        assert_eq!(bytes.as_ptr(), pointer);
        assert!(bytes.iter().all(|byte| *byte == 7));
    }

    #[test]
    fn idle_memory_is_bounded_and_short_reads_never_publish_recycled_bytes() {
        let pool = Arc::new(BufferPool::new(1, STREAM_CHUNK));
        let first = pool.checkout();
        let second = pool.checkout();
        drop(first);
        drop(second);
        assert_eq!(pool.idle.lock().unwrap().len(), 1);
        let short = Fixture::new(b"short");
        assert_eq!(
            read_pooled_chunk(&mut short.open(), &pool, STREAM_CHUNK)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        let fixture = Fixture::new(&vec![9; STREAM_CHUNK]);
        let bytes = read_pooled_chunk(&mut fixture.open(), &pool, STREAM_CHUNK).unwrap();
        assert!(bytes.iter().all(|byte| *byte == 9));
    }

    #[tokio::test]
    async fn empty_small_and_multichunk_bodies_preserve_every_byte() {
        for length in [
            0,
            1024,
            16384,
            16385,
            65535,
            65536,
            65537,
            STREAM_CHUNK,
            2 * STREAM_CHUNK + 17,
            INLINE_MAX as usize,
            INLINE_MAX as usize + 2 * STREAM_CHUNK + 17,
        ] {
            let data: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
            let fixture = Fixture::new(&data);
            let mut body = read_body(fixture.open(), length as u64).unwrap();
            // Every frame stays bounded at STREAM_CHUNK regardless of total
            // size, whether it came from the inline whole-file path or the
            // still-lazy streaming path above INLINE_MAX.
            let mut received = Vec::new();
            while let Some(frame) = body.frame().await {
                let bytes = frame.unwrap().into_data().unwrap();
                assert!(bytes.len() <= STREAM_CHUNK);
                received.extend_from_slice(&bytes);
            }
            assert_eq!(received, data);
        }
    }

    #[test]
    fn medium_reuse_exposes_only_the_new_file_length() {
        let pool = Arc::new(BufferPool::new(1, MEDIUM_CHUNK));
        let original = Fixture::new(&vec![7; MEDIUM_CHUNK]);
        let bytes = read_pooled_chunk(&mut original.open(), &pool, MEDIUM_CHUNK).unwrap();
        let pointer = bytes.as_ptr();
        drop(bytes);
        let replacement = Fixture::new(&vec![3; 20001]);
        let shorter = read_pooled_chunk(&mut replacement.open(), &pool, 20001).unwrap();
        assert_eq!(shorter.as_ptr(), pointer);
        assert_eq!(shorter.len(), 20001);
        assert!(shorter.iter().all(|byte| *byte == 3));
        let retained = shorter.clone();
        drop(shorter);
        let concurrent = read_pooled_chunk(&mut original.open(), &pool, MEDIUM_CHUNK).unwrap();
        assert_ne!(concurrent.as_ptr(), pointer);
        assert!(retained.iter().all(|byte| *byte == 3));
    }

    #[tokio::test]
    async fn range_reads_start_at_offset_and_stop_at_declared_length() {
        let data: Vec<u8> = (0..3 * STREAM_CHUNK).map(|i| (i % 251) as u8).collect();
        let fixture = Fixture::new(&data);
        let mut file = fixture.open();
        file.seek(io::SeekFrom::Start(31)).unwrap();
        let length = STREAM_CHUNK + 7;
        let body = read_body(file, length as u64).unwrap();
        assert_eq!(
            body.collect().await.unwrap().to_bytes(),
            &data[31..31 + length]
        );
    }

    #[tokio::test]
    async fn streaming_is_lazy_and_stops_at_original_length_after_growth() {
        // Above INLINE_MAX so this still exercises the lazy streaming path
        // instead of the whole-file inline read.
        let total = INLINE_MAX as usize + 16;
        let fixture = Fixture::new(&vec![1; total]);
        let mut body = read_body(fixture.open(), total as u64).unwrap();
        assert_eq!(
            body.frame()
                .await
                .unwrap()
                .unwrap()
                .data_ref()
                .unwrap()
                .len(),
            STREAM_CHUNK
        );
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.0)
            .unwrap();
        writer
            .seek(io::SeekFrom::Start(STREAM_CHUNK as u64))
            .unwrap();
        writer.write_all(&[2; 32]).unwrap();
        let rest = body.collect().await.unwrap().to_bytes();
        let mut expected = vec![2_u8; 32];
        expected.extend(std::iter::repeat_n(1_u8, total - STREAM_CHUNK - 32));
        assert_eq!(rest.as_ref(), expected.as_slice());
    }

    #[tokio::test]
    async fn truncation_returns_an_error_instead_of_a_short_success() {
        // Above INLINE_MAX so this still exercises the lazy streaming path
        // instead of the whole-file inline read.
        let total = INLINE_MAX as usize + 16;
        let fixture = Fixture::new(&vec![1; total]);
        let mut body = read_body(fixture.open(), total as u64).unwrap();
        body.frame().await.unwrap().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.0)
            .unwrap()
            .set_len(STREAM_CHUNK as u64)
            .unwrap();
        assert_eq!(
            body.frame().await.unwrap().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(body.frame().await.is_none());
        // Below INLINE_MAX, a short file now fails inside read_body itself
        // instead of lazily during streaming; either way it must never
        // succeed with fewer bytes than declared.
        match read_body(fixture.open(), (STREAM_CHUNK + 1) as u64) {
            Ok(body) => assert!(body.collect().await.is_err()),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof),
        }
        let short = Fixture::new(b"short");
        assert!(
            matches!(read_body(short.open(), 10), Err(e) if e.kind() == io::ErrorKind::UnexpectedEof)
        );
    }
}
