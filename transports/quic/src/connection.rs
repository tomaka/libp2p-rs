// Copyright 2020 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

/// A QUIC connection, either client or server.
///
/// QUIC opens streams lazily, so the peer is not notified that a stream has
/// been opened until data is written (either on this stream, or a
/// higher-numbered one). Therefore, reading on a stream that has not been
/// written to will deadlock, unless another stream is opened and written to
/// before the first read returns. Because this is not needed in practice, and
/// to ease debugging, [`<QuicMuxer as StreamMuxer>::read_substream`] returns an
/// error in this case.
#[derive(Debug, Clone)]
pub struct QuicMuxer(Arc<Mutex<Muxer>>);

impl QuicMuxer {
    /// Returns the underlying data structure, including all state.
    fn inner(&self) -> MutexGuard<'_, Muxer> {
        self.0.lock()
    }
}

#[derive(Debug)]
enum OutboundInner {
    /// The substream is fully set up
    Complete(Result<StreamId, Error>),
    /// We are waiting on a stream to become available
    Pending(oneshot::Receiver<StreamId>),
    /// We have already returned our substream
    Done,
}

/// An outbound QUIC substream. This will eventually resolve to either a
/// [`Substream`] or an [`Error`].
#[derive(Debug)]
pub struct Outbound(OutboundInner);

impl Future for Outbound {
    type Output = Result<Substream, Error>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = &mut *self;
        match this.0 {
            OutboundInner::Complete(_) => match replace(&mut this.0, OutboundInner::Done) {
                OutboundInner::Complete(e) => Poll::Ready(e.map(Substream::unwritten)),
                _ => unreachable!("we just checked that we have a `Complete`; qed"),
            },
            OutboundInner::Pending(ref mut receiver) => {
                let result = ready!(receiver.poll_unpin(cx))
                    .map(Substream::unwritten)
                    .map_err(|oneshot::Canceled| Error::ConnectionLost);
                this.0 = OutboundInner::Done;
                Poll::Ready(result)
            }
            OutboundInner::Done => panic!("polled after yielding Ready"),
        }
    }
}

impl StreamMuxer for QuicMuxer {
    type OutboundSubstream = Outbound;
    type Substream = Substream;
    type Error = Error;

    fn open_outbound(&self) -> Self::OutboundSubstream {
        let mut inner = self.inner();
        if let Some(ref e) = inner.close_reason {
            Outbound(OutboundInner::Complete(Err(Error::ConnectionError(
                e.clone(),
            ))))
        } else if let Some(id) = inner.get_pending_stream() {
            Outbound(OutboundInner::Complete(Ok(id)))
        } else {
            let (sender, receiver) = oneshot::channel();
            inner.connectors.push_front(sender);
            inner.wake_driver();
            Outbound(OutboundInner::Pending(receiver))
        }
    }

    fn destroy_outbound(&self, _: Outbound) {}

    fn destroy_substream(&self, substream: Self::Substream) {
        let mut inner = self.inner();
        if let Some(waker) = inner.writers.remove(&substream.id) {
            waker.wake();
        }
        if let Some(waker) = inner.readers.remove(&substream.id) {
            waker.wake();
        }
        if substream.is_live() && inner.close_reason.is_none() {
            if let Err(e) = inner.connection.finish(substream.id) {
                warn!("Error closing stream: {}", e);
            }
        }
        let _ = inner
            .connection
            .stop_sending(substream.id, Default::default());
    }

    fn is_remote_acknowledged(&self) -> bool {
        true
    }

    fn poll_inbound(&self, cx: &mut Context) -> Poll<Result<Self::Substream, Self::Error>> {
        debug!("being polled for inbound connections!");
        let mut inner = self.inner();
        if inner.connection.is_drained() {
            return Poll::Ready(Err(Error::ConnectionError(
                inner
                    .close_reason
                    .clone()
                    .expect("closed connections always have a reason; qed"),
            )));
        }
        inner.wake_driver();
        match inner.connection.accept(quinn_proto::Dir::Bi) {
            None => {
                if let Some(waker) = replace(
                    &mut inner.handshake_or_accept_waker,
                    Some(cx.waker().clone()),
                ) {
                    waker.wake()
                }
                Poll::Pending
            }
            Some(id) => {
                inner.finishers.insert(id, None);
                Poll::Ready(Ok(Substream::live(id)))
            }
        }
    }

    fn write_substream(
        &self,
        cx: &mut Context,
        substream: &mut Self::Substream,
        buf: &[u8],
    ) -> Poll<Result<usize, Self::Error>> {
        use quinn_proto::WriteError;
        if !substream.is_live() {
            error!(
                "The application used stream {:?} after it was no longer live",
                substream.id
            );
            return Poll::Ready(Err(Error::ExpiredStream));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let mut inner = self.inner();
        debug_assert!(
            inner.finishers.get(&substream.id).is_some(),
            "no entry in finishers map for write stream"
        );
        inner.wake_driver();
        if let Some(ref e) = inner.close_reason {
            return Poll::Ready(Err(Error::ConnectionError(e.clone())));
        }
        match inner.connection.write(substream.id, buf) {
            Ok(bytes) => Poll::Ready(Ok(bytes)),
            Err(WriteError::Blocked) => {
                if let Some(ref e) = inner.close_reason {
                    return Poll::Ready(Err(Error::ConnectionError(e.clone())));
                }
                if let Some(w) = inner.writers.insert(substream.id, cx.waker().clone()) {
                    w.wake();
                }
                Poll::Pending
            }
            Err(WriteError::UnknownStream) => {
                if let Some(e) = &inner.close_reason {
                    error!(
                        "The application used a connection that is already being \
                        closed. This is a bug in the application or in libp2p."
                    );
                    Poll::Ready(Err(Error::ConnectionError(e.clone())))
                } else {
                    error!(
                        "The application used a stream that has already been \
                        closed. This is a bug in the application or in libp2p."
                    );
                    Poll::Ready(Err(Error::ExpiredStream))
                }
            }
            Err(WriteError::Stopped(e)) => {
                inner.finishers.remove(&substream.id);
                if let Some(w) = inner.writers.remove(&substream.id) {
                    w.wake()
                }
                substream.status = SubstreamStatus::Finished;
                Poll::Ready(Err(Error::Stopped(e)))
            }
        }
    }

    fn poll_outbound(
        &self,
        cx: &mut Context,
        substream: &mut Self::OutboundSubstream,
    ) -> Poll<Result<Self::Substream, Self::Error>> {
        substream.poll_unpin(cx)
    }

    /// Try to from a substream. This will return an error if the substream has
    /// not yet been written to.
    fn read_substream(
        &self,
        cx: &mut Context,
        substream: &mut Self::Substream,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Self::Error>> {
        use quinn_proto::ReadError;
        let mut inner = self.inner();
        inner.wake_driver();
        match inner.connection.read(substream.id, buf) {
            Ok(Some(bytes)) => Poll::Ready(Ok(bytes)),
            Ok(None) => Poll::Ready(Ok(0)),
            Err(ReadError::Blocked) => {
                if let Some(error) = &inner.close_reason {
                    Poll::Ready(Err(Error::ConnectionError(error.clone())))
                } else if let SubstreamStatus::Unwritten = substream.status {
                    Poll::Ready(Err(Error::CannotReadFromUnwrittenStream))
                } else {
                    trace!(
                        "Blocked on reading stream {:?} with side {:?}",
                        substream.id,
                        inner.connection.side()
                    );
                    if let Some(w) = inner.readers.insert(substream.id, cx.waker().clone()) {
                        w.wake()
                    }
                    Poll::Pending
                }
            }
            Err(ReadError::UnknownStream) => {
                error!(
                    "The application used a stream that has already been closed. This is a bug."
                );
                Poll::Ready(Err(Error::ExpiredStream))
            }
            Err(ReadError::Reset(e)) => {
                if let Some(w) = inner.readers.remove(&substream.id) {
                    w.wake()
                }
                Poll::Ready(Err(Error::Reset(e)))
            }
        }
    }

    fn shutdown_substream(
        &self,
        cx: &mut Context,
        substream: &mut Self::Substream,
    ) -> Poll<Result<(), Self::Error>> {
        match substream.status {
            SubstreamStatus::Finished => return Poll::Ready(Ok(())),
            SubstreamStatus::Finishing(ref mut channel) => {
                self.inner().wake_driver();
                return channel
                    .poll_unpin(cx)
                    .map_err(|e| Error::IO(io::Error::new(io::ErrorKind::ConnectionAborted, e)));
            }
            SubstreamStatus::Unwritten | SubstreamStatus::Live => {}
        }
        let mut inner = self.inner();
        inner.wake_driver();
        inner.connection.finish(substream.id).map_err(|e| match e {
            quinn_proto::FinishError::UnknownStream => Error::ConnectionClosing,
            quinn_proto::FinishError::Stopped(e) => Error::Stopped(e),
        })?;
        let (sender, mut receiver) = oneshot::channel();
        assert!(
            receiver.poll_unpin(cx).is_pending(),
            "we haven’t written to the peer yet"
        );
        substream.status = SubstreamStatus::Finishing(receiver);
        match inner.finishers.insert(substream.id, Some(sender)) {
            Some(None) => {}
            _ => unreachable!(
                "We inserted a None value earlier; and haven’t touched this entry since; qed"
            ),
        }
        Poll::Pending
    }

    /// Flush pending data on this stream. libp2p-quic sends out data as soon as
    /// possible, so this method does nothing.
    fn flush_substream(
        &self,
        _cx: &mut Context,
        _substream: &mut Self::Substream,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    /// Flush all pending data to the peer. libp2p-quic sends out data as soon
    /// as possible, so this method does nothing.
    fn flush_all(&self, _cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    /// Close the connection. Once this function is called, it is a logic error
    /// to call other methods on this object.
    fn close(&self, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        trace!("close() called");
        let mut inner = self.inner();
        if inner.connection.is_closed() || inner.close_reason.is_some() {
            return Poll::Ready(Ok(()));
        } else if inner.finishers.is_empty() {
            inner.shutdown(0);
            inner.close_reason = Some(quinn_proto::ConnectionError::LocallyClosed);
            drop(inner.driver().poll_unpin(cx));
            return Poll::Ready(Ok(()));
        }
        if let Some(waker) = replace(&mut inner.close_waker, Some(cx.waker().clone())) {
            waker.wake();
            return Poll::Pending;
        }
        let Muxer {
            finishers,
            connection,
            ..
        } = &mut *inner;
        for (id, channel) in finishers {
            if channel.is_none() {
                match connection.finish(*id) {
                    Ok(()) => {}
                    Err(error) => warn!("Finishing stream {:?} failed: {}", id, error),
                }
            }
        }
        Poll::Pending
    }
}
