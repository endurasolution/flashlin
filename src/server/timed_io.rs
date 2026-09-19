use std::future::Future;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::Sleep,
};

/// Timeout stalled socket operations without buffering the response.
pub struct TimedIo<T> {
    inner: T,
    read_idle: Duration,
    write_idle: Duration,
    read_timer: Option<Pin<Box<Sleep>>>,
    write_timer: Option<Pin<Box<Sleep>>>,
}

impl<T> TimedIo<T> {
    pub fn new(inner: T, read_idle: Duration, write_idle: Duration) -> Self {
        Self {
            inner,
            read_idle,
            write_idle,
            read_timer: None,
            write_timer: None,
        }
    }
}

fn deadline(
    timer: &mut Option<Pin<Box<Sleep>>>,
    idle: Duration,
    cx: &mut Context<'_>,
) -> Poll<io::Result<()>> {
    let timer = timer.get_or_insert_with(|| Box::pin(tokio::time::sleep(idle)));
    if timer.as_mut().poll(cx).is_ready() {
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "socket idle timeout",
        )))
    } else {
        Poll::Pending
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for TimedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                self.read_timer = None;
                Poll::Ready(result)
            }
            Poll::Pending => {
                let idle = self.read_idle;
                deadline(&mut self.read_timer, idle, cx)
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TimedIo<T> {
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write_vectored(cx, bufs) {
            Poll::Ready(result) => {
                self.write_timer = None;
                Poll::Ready(result)
            }
            Poll::Pending => {
                let idle = self.write_idle;
                deadline(&mut self.write_timer, idle, cx).map(|result| result.map(|_| 0))
            }
        }
    }

    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(result) => {
                self.write_timer = None;
                Poll::Ready(result)
            }
            Poll::Pending => {
                let idle = self.write_idle;
                deadline(&mut self.write_timer, idle, cx).map(|result| result.map(|_| 0))
            }
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Ready(result) => {
                self.write_timer = None;
                Poll::Ready(result)
            }
            Poll::Pending => {
                let idle = self.write_idle;
                deadline(&mut self.write_timer, idle, cx)
            }
        }
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_shutdown(cx) {
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => {
                let idle = self.write_idle;
                deadline(&mut self.write_timer, idle, cx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn vectored_writes_preserve_bytes_and_idle_deadline() {
        let (socket, mut peer) = tokio::io::duplex(4);
        let supported = socket.is_write_vectored();
        let idle = Duration::from_millis(20);
        let mut io = TimedIo::new(socket, idle, idle);
        assert_eq!(io.is_write_vectored(), supported);
        let buffers = [std::io::IoSlice::new(b"ab"), std::io::IoSlice::new(b"cd")];
        let count = io.write_vectored(&buffers).await.unwrap();
        let mut received = vec![0; count];
        peer.read_exact(&mut received).await.unwrap();
        assert_eq!(received, b"abcd"[..count]);
        io.write_all(b"full").await.unwrap();
        assert_eq!(
            io.write_vectored(&buffers).await.unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }
    #[tokio::test]
    async fn stalled_reads_and_writes_expire() {
        let (socket, _peer) = tokio::io::duplex(1);
        let idle = Duration::from_millis(20);
        let mut io = TimedIo::new(socket, idle, idle);
        let mut buf = [0; 1];
        assert_eq!(
            io.read(&mut buf).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            io.write_all(b"larger than capacity")
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
