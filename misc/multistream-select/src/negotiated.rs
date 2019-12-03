// Copyright 2019 Parity Technologies (UK) Ltd.
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

use crate::protocol::{Protocol, Message, Version, ProtocolError};
use futures::prelude::*;
use log::debug;
use std::{mem, io, error::Error, fmt, pin::Pin, task::Context, task::Poll};

/// An I/O stream that has settled on an (application-layer) protocol to use.
///
/// A `Negotiated` represents an I/O stream that has _settled_ on a protocol
/// to use. In particular, it is not implied that all of the protocol negotiation
/// frames have yet been sent and / or received, just that the selected protocol
/// is fully determined. This is to allow the last protocol negotiation frames
/// sent by a peer to be combined in a single write, possibly piggy-backing
/// data from the negotiated protocol on top.
///
/// Reading from a `Negotiated` I/O stream that still has pending negotiation
/// protocol data to send implicitly triggers flushing of all yet unsent data.
#[derive(Debug)]
pub struct Negotiated<TInner> {
    state: State<TInner>
}

impl<TInner> Negotiated<TInner> {
    /// Creates a `Negotiated` in state [`State::Complete`], possibly
    /// with `remaining` data to be sent.
    pub(crate) fn completed(io: TInner) -> Self {
        Negotiated { state: State::Completed { io } }
    }

    /// Creates a `Negotiated` in state [`State::Expecting`] that is still
    /// expecting confirmation of the given `protocol`.
    pub(crate) fn expecting(io: TInner, protocol: Protocol, version: Version) -> Self {
        Negotiated { state: State::Expecting { io, protocol, version } }
    }
}

impl<TInner> Negotiated<TInner>
where
    TInner: AsyncRead + AsyncWrite + Unpin
{
    /// Polls the `Negotiated` for completion.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), NegotiationError>> {
        // Flush any pending negotiation data.
        match self.poll_flush(cx) {
            Poll::Ready(Ok(())) => {},
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(e)) => {
                // If the remote closed the stream, it is important to still
                // continue reading the data that was sent, if any.
                if e.kind() != io::ErrorKind::WriteZero {
                    return Poll::Ready(Err(e.into()))
                }
            }
        }

        if let State::Completed { .. } = &mut self.state {
            return Poll::Ready(Ok(()))
        }

        // Read outstanding protocol negotiation messages.
        loop {
            match mem::replace(&mut self.state, State::Invalid) {
                State::Expecting { mut io, protocol, version } => {
                    let msg = match io.poll(cx) {       // TODO: read_one message
                        Poll::Ready(Some(Ok(msg))) => msg,
                        Poll::Pending => {
                            self.state = State::Expecting { io, protocol, version };
                            return Poll::Pending
                        }
                        Poll::Ready(None) => {
                            self.state = State::Expecting { io, protocol, version };
                            return Poll::Ready(Err(ProtocolError::IoError(io::ErrorKind::UnexpectedEof.into()).into()))
                        }
                        Poll::Ready(Some(Err(err))) => {
                            self.state = State::Expecting { io, protocol, version };
                            return Poll::Ready(Err(err.into()))
                        }
                    };

                    if let Message::Header(v) = &msg {
                        if v == &version {
                            self.state = State::Expecting { io, protocol, version };
                            continue
                        }
                    }

                    if let Message::Protocol(p) = &msg {
                        if p.as_ref() == protocol.as_ref() {
                            debug!("Negotiated: Received confirmation for protocol: {}", p);
                            self.state = State::Completed { io };
                            return Poll::Ready(Ok(()))
                        }
                    }

                    return Poll::Ready(Err(NegotiationError::Failed));
                }

                _ => panic!("Negotiated: Invalid state")
            }
        }
    }

    /// Returns a `NegotiatedComplete` future that waits for protocol
    /// negotiation to complete.
    pub async fn complete(mut self) -> Result<(), NegotiationError> {
        future::poll_fn(move |cx| {
            Pin::new(&mut self).poll(cx)
        }).await
    }
}

/// The states of a `Negotiated` I/O stream.
#[derive(Debug)]
enum State<R> {
    /// In this state, a `Negotiated` is still expecting to
    /// receive confirmation of the protocol it as settled on.
    Expecting {
        /// The underlying I/O stream.
        io: R,
        /// The expected protocol (i.e. name and version).
        protocol: Protocol,
        /// The expected multistream-select protocol version.
        version: Version
    },

    /// In this state, a protocol has been agreed upon and may
    /// only be pending the sending of the final acknowledgement,
    /// which is prepended to / combined with the next write for
    /// efficiency.
    Completed { io: R },

    /// Temporary state while moving the `io` resource from
    /// `Expecting` to `Completed`.
    Invalid,
}

impl<TInner> AsyncRead for Negotiated<TInner>
where
    TInner: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context, buf: &mut [u8])
        -> Poll<Result<usize, io::Error>>
    {
        loop {
            if let State::Completed { io } = &mut self.state {
                // If protocol negotiation is complete and there is no
                // remaining data to be flushed, commence with reading.
                return Pin::new(io).poll_read(cx, buf);
            }

            // Poll the `Negotiated`, driving protocol negotiation to completion,
            // including flushing of any remaining data.
            match Pin::new(&mut self).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(())) => {},
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
            }
        }
    }
}

impl<TInner> AsyncWrite for Negotiated<TInner>
where
    TInner: AsyncWrite + AsyncRead + Unpin
{
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
        match &mut self.state {
            State::Completed { io } => Pin::new(io).poll_write(cx, buf),
            State::Expecting { io, .. } => Pin::new(io).poll_write(cx, buf),
            State::Invalid => panic!("Negotiated: Invalid state")
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        match &mut self.state {
            State::Completed { io } => Pin::new(io).poll_flush(cx),
            State::Expecting { io, .. } => Pin::new(io).poll_flush(cx),
            State::Invalid => panic!("Negotiated: Invalid state")
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), io::Error>> {
        // Ensure all data has been flushed and expected negotiation messages
        // have been received.
        match self.poll(cx) {
            Poll::Ready(Ok(())) => {},
            Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
            Poll::Pending => return Poll::Pending,
        }

        // Continue with the shutdown of the underlying I/O stream.
        match &mut self.state {
            State::Completed { io, .. } => Pin::new(io).poll_close(cx),
            State::Expecting { io, .. } => Pin::new(io).poll_close(cx),
            State::Invalid => panic!("Negotiated: Invalid state")
        }
    }
}

/// Error that can happen when negotiating a protocol with the remote.
#[derive(Debug)]
pub enum NegotiationError {
    /// A protocol error occurred during the negotiation.
    ProtocolError(ProtocolError),

    /// Protocol negotiation failed because no protocol could be agreed upon.
    Failed,
}

impl From<ProtocolError> for NegotiationError {
    fn from(err: ProtocolError) -> NegotiationError {
        NegotiationError::ProtocolError(err)
    }
}

impl From<io::Error> for NegotiationError {
    fn from(err: io::Error) -> NegotiationError {
        ProtocolError::from(err).into()
    }
}

impl Into<io::Error> for NegotiationError {
    fn into(self) -> io::Error {
        if let NegotiationError::ProtocolError(e) = self {
            return e.into()
        }
        io::Error::new(io::ErrorKind::Other, self)
    }
}

impl Error for NegotiationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            NegotiationError::ProtocolError(err) => Some(err),
            _ => None,
        }
    }
}

impl fmt::Display for NegotiationError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        match self {
            NegotiationError::ProtocolError(p) =>
                fmt.write_fmt(format_args!("Protocol error: {}", p)),
            NegotiationError::Failed =>
                fmt.write_str("Protocol negotiation failed.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::*;
    use std::io::Write;

    /// An I/O resource with a fixed write capacity (total and per write op).
    struct Capped { buf: Vec<u8>, step: usize }

    impl io::Write for Capped {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.buf.len() + buf.len() > self.buf.capacity() {
                return Err(io::ErrorKind::WriteZero.into())
            }
            self.buf.write(&buf[.. usize::min(self.step, buf.len())])
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl AsyncWrite for Capped {
        fn shutdown(&mut self) -> Poll<(), io::Error> {
            Ok(().into())
        }
    }

    #[test]
    fn write_remaining() {
        fn prop(rem: Vec<u8>, new: Vec<u8>, free: u8) -> TestResult {
            let cap = rem.len() + free as usize;
            let buf = Capped { buf: Vec::with_capacity(cap), step: free as usize };
            let mut rem = BytesMut::from(rem);
            let mut io = Negotiated::completed(buf, rem.clone());
            let mut written = 0;
            loop {
                // Write until `new` has been fully written or the capped buffer is
                // full (in which case the buffer should remain unchanged from the
                // last successful write).
                match io.write(&new[written..]) {
                    Ok(n) =>
                        if let State::Completed { remaining, .. } = &io.state {
                            if n == rem.len() + new[written..].len() {
                                assert!(remaining.is_empty())
                            } else {
                                assert!(remaining.len() <= rem.len());
                            }
                            written += n;
                            if written == new.len() {
                                return TestResult::passed()
                            }
                            rem = remaining.clone();
                        } else {
                            return TestResult::failed()
                        }
                    Err(_) =>
                        if let State::Completed { remaining, .. } = &io.state {
                            assert!(rem.len() + new[written..].len() > cap);
                            assert_eq!(remaining, &rem);
                            return TestResult::passed()
                        } else {
                            return TestResult::failed()
                        }
                }
            }
        }
        quickcheck(prop as fn(_,_,_) -> _)
    }
}

