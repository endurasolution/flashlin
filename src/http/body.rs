use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

type Frames = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, io::Error>> + Send>>;

/// Buffered small responses and backpressured streams share one response type.
pub struct ResponseBody {
    pub(crate) buffered: Option<Bytes>,
    stream: Option<Frames>,
    remaining: Option<u64>,
}

impl ResponseBody {
    pub fn new(bytes: Bytes) -> Self {
        Self {
            remaining: Some(bytes.len() as u64),
            buffered: Some(bytes),
            stream: None,
        }
    }

    pub fn stream(
        stream: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
        length: Option<u64>,
    ) -> Self {
        Self {
            buffered: None,
            stream: Some(Box::pin(stream.map(|item| item.map(Frame::data)))),
            remaining: length,
        }
    }

    pub fn incoming(body: Incoming, idle: Duration) -> Self {
        let length = body.size_hint().exact();
        let stream = futures_util::stream::unfold(Some(body), move |state| async move {
            let mut body = state?;
            match tokio::time::timeout(idle, body.frame()).await {
                Ok(Some(Ok(frame))) => Some((Ok(frame), Some(body))),
                Ok(Some(Err(error))) => Some((Err(io::Error::other(error)), None)),
                Ok(None) => None,
                Err(_) => Some((
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "upstream body idle timeout",
                    )),
                    None,
                )),
            }
        });
        Self {
            buffered: None,
            stream: Some(Box::pin(stream)),
            remaining: length,
        }
    }
}

impl Body for ResponseBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if let Some(bytes) = self.buffered.take() {
            self.remaining = Some(0);
            return Poll::Ready(if bytes.is_empty() {
                None
            } else {
                Some(Ok(Frame::data(bytes)))
            });
        }
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(None);
        };
        let result = stream.as_mut().poll_next(cx);
        if let Poll::Ready(Some(Ok(frame))) = &result {
            if let (Some(left), Some(data)) = (self.remaining.as_mut(), frame.data_ref()) {
                *left = left.saturating_sub(data.len() as u64);
            }
        }
        if matches!(result, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self.stream = None;
            self.remaining = Some(0);
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.stream.is_none() && self.buffered.as_ref().is_none_or(Bytes::is_empty)
    }

    fn size_hint(&self) -> SizeHint {
        self.remaining.map(SizeHint::with_exact).unwrap_or_default()
    }
}
