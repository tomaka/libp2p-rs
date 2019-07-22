use futures::prelude::*;
use std::{io, pin::Pin, task::{Context, Poll}};

/// Extracts the successful type of a `Poll<T>`.
///
/// This macro bakes in propagation of `Pending` signals by returning early.
#[macro_export]
macro_rules! ready {
    ($e:expr $(,)?) => (match $e {
        Poll::Ready(t) => t,
        Poll::Pending => return Poll::Pending,
    })
}

/// Future for the [`close`](super::AsyncWriteExt::close) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Close<W> {
    writer: Option<W>,
}

impl<W: Unpin> Unpin for Close<W> {}

impl<W: AsyncWrite + Unpin> Close<W> {
    pub(super) fn new(writer: W) -> Self {
        Close { writer: Some(writer) }
    }
}

impl<W: AsyncWrite + Unpin> Future for Close<W> {
    type Output = io::Result<W>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.writer.as_mut().expect("Future already finished")).poll_close(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) =>
                Poll::Ready(Ok(self.writer.take().expect("We know the writer is Some; QED"))),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
        }
    }
}

/// Future for the [`write_all`](super::AsyncWriteExt::write_all) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct WriteAll<W, B> {
    writer: Option<W>,
    buf: Option<futures::io::Window<B>>,
}

impl<W: Unpin, B> Unpin for WriteAll<W, B> {}

impl<W: AsyncWrite + Unpin, B: AsRef<[u8]>> WriteAll<W, B> {
    pub(super) fn new(writer: W, buf: B) -> Self {
        WriteAll { writer: Some(writer), buf: Some(futures::io::Window::new(buf)) }
    }
}

impl<W: AsyncWrite + Unpin, B: AsRef<[u8]>> Future for WriteAll<W, B> {
    type Output = io::Result<(W, B)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<(W, B)>> {
        let this = &mut *self;
        while !this.buf.as_ref().expect("Future is finished").as_ref().is_empty() {
            let n = ready!(Pin::new(&mut this.writer.as_mut().unwrap()).poll_write(cx, this.buf.as_ref().unwrap().as_ref()))?;
            let start = this.buf.as_ref().unwrap().start();
            this.buf.as_mut().unwrap().set((start + n)..);
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()))
            }
        }

        Poll::Ready(Ok((this.writer.take().unwrap(), this.buf.take().unwrap().into_inner())))
    }
}

/// Future for the [`read_exact`](super::AsyncReadExt::read_exact) method.
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct ReadExact<R, B> {
    reader: Option<R>,
    buf: Option<futures::io::Window<B>>,
}

impl<R: Unpin, B> Unpin for ReadExact<R, B> {}

impl<R: AsyncRead + Unpin, B: AsRef<[u8]> + AsMut<[u8]>> ReadExact<R, B> {
    pub(super) fn new(reader: R, buf: B) -> Self {
        ReadExact { reader: Some(reader), buf: Some(futures::io::Window::new(buf)) }
    }
}

impl<R: AsyncRead + Unpin, B: AsRef<[u8]> + AsMut<[u8]>> Future for ReadExact<R, B> {
    type Output = io::Result<(R, B)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        while !this.buf.as_mut().expect("Future is finished").as_mut().is_empty() {
            let n = ready!(Pin::new(&mut this.reader.as_mut().unwrap()).poll_read(cx, this.buf.as_mut().unwrap().as_mut()))?;
            let start = this.buf.as_ref().unwrap().start();
            this.buf.as_mut().unwrap().set((start + n)..);
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()))
            }
        }
        Poll::Ready(Ok((this.reader.take().unwrap(), this.buf.take().unwrap().into_inner())))
    }
}

