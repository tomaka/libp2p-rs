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

//! Implementation of the libp2p `Transport` trait for QUIC.

use crate::error::QuicError;
use futures::{future::{self, FutureResult}, prelude::*, sync::mpsc, sync::oneshot, try_ready};
use libp2p_core::{muxing::Shutdown, PeerId, StreamMuxer, Transport, transport::TransportError};
use log::{debug, trace, warn};
use multiaddr::{Multiaddr, Protocol};
use multihash::Multihash;
use parking_lot::Mutex;
use std::{fmt, io, iter, net::SocketAddr, sync::Arc};
use tokio_udp::UdpSocket;

mod endpoint;

pub use crate::endpoint::QuicEndpoint;
// We re-export the error from `quinn_proto`.
#[doc(inline)]
pub use quinn_proto::EndpointError;

/// Represents the configuration for a QUIC transport capability for libp2p.
#[derive(Debug, Clone)]
pub struct QuicConfig {
    /// Endpoint to use.
    pub endpoint: Arc<QuicEndpoint>,
}

impl From<Arc<QuicEndpoint>> for QuicConfig {
    fn from(endpoint: Arc<QuicEndpoint>) -> QuicConfig {
        QuicConfig {
            endpoint,
        }
    }
}

impl Transport for QuicConfig {
    type Output = (PeerId, QuicMuxer);
    type Error = QuicError;
    type Listener = QuicListenStream;
    type ListenerUpgrade = FutureResult<Self::Output, QuicError>;
    type Dial = QuicDialFut;

    fn listen_on(self, addr: Multiaddr) -> Result<(Self::Listener, Multiaddr), TransportError<Self::Error>> {
        unimplemented!()
        /*let listen_addr =
            if let Ok((sa, None)) = multiaddr_to_socketaddr(&addr) {
                sa
            } else {
                return Err(TransportError::MultiaddrNotSupported(addr))
            };

        let mut inner = self.endpoint.inner.lock();

        if listen_addr.port() == 0 || inner.sockets.iter().all(|(_, port)| port != listen_addr.port()) {
            let new_socket = UdpSocket::bind(listen_addr).unwrap();  // TODO: don't unwrap
            inner.sockets.push((new_socket, listen_addr.port()));
        }

        let actual_addr = socket_addr_to_quic(context.local_addr());
        debug!("Listening on {}; actual_addr = {}", listen_addr, actual_addr);

        Ok((QuicListenStream { inner: context, public_keys }, actual_addr))*/
    }

    fn dial(self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let (target_addr, hash) =
            match multiaddr_to_socketaddr(&addr) {
                Ok(val) => val,
                Err(_) => return Err(TransportError::MultiaddrNotSupported(addr))
            };

        // As an optimization, we check that the address is not of the form `0.0.0.0`.
        // If so, we instantly refuse dialing instead of going through a lot of fuss.
        if target_addr.port() == 0 || target_addr.ip().is_unspecified() {
            debug!("Instantly refusing dialing {}, as it is invalid", addr);
            return Err(TransportError::MultiaddrNotSupported(addr))
        }

        debug!("Dialing {}", target_addr);
        Ok(QuicDialFut {
            inner: self.endpoint.connect(target_addr)
        })
    }

    fn nat_traversal(&self, _server: &Multiaddr, _observed: &Multiaddr) -> Option<Multiaddr> {
        // TODO: implement after https://github.com/libp2p/rust-libp2p/pull/550
        None
    }
}

/// An open connection. Implements `StreamMuxer`.
pub struct QuicMuxer {
    /// Reference to the state machine.
    endpoint: Arc<QuicEndpoint>,
    /// Identifier of the connection we handle in the quinn state machine.
    connection_id: quinn_proto::ConnectionHandle,
    /// Channel over which incoming substreams are being sent.
    incoming_substreams: mpsc::Receiver<quinn_proto::StreamId>,
}

impl fmt::Debug for QuicMuxer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("QuicMuxer")
            .field(&(&*self.endpoint as *const QuicEndpoint as usize))
            .field(&self.connection_id)
            .finish()
    }
}

/// A QUIC substream.
pub struct QuicMuxerSubstream(quinn_proto::StreamId);

impl fmt::Debug for QuicMuxerSubstream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("QuicMuxerSubstream").field(&self.0).finish()
    }
}

/// A QUIC outbound substream.
pub struct QuicMuxerOutboundSubstream(oneshot::Receiver<Option<quinn_proto::StreamId>>);

impl fmt::Debug for QuicMuxerOutboundSubstream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("QuicMuxerOutboundSubstream").finish()
    }
}

impl StreamMuxer for QuicMuxer {
    type Substream = QuicMuxerSubstream;
    type OutboundSubstream = QuicMuxerOutboundSubstream;

    fn poll_inbound(&self) -> Poll<Option<Self::Substream>, io::Error> {
        match self.incoming_substreams.poll() {
            Ok(Async::Ready(Some(substream))) => Ok(Async::Ready(Some(substream))),
            Ok(Async::Ready(None)) => Err(io::ErrorKind::ConnectionReset.into()),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(_) => panic!("The QUIC background task has panicked")
        }
    }

    fn open_outbound(&self) -> Self::OutboundSubstream {
        QuicMuxerOutboundSubstream(self.endpoint.open_substream(self.connection_id))
    }

    fn poll_outbound(&self, sub: &mut Self::OutboundSubstream) -> Poll<Option<Self::Substream>, io::Error> {
        match sub.0.poll() {
            Ok(Async::Ready(Some(stream_id))) => Ok(Async::Ready(QuicMuxerSubstream(stream_id))),
            Ok(Async::Ready(None)) => Err(io::Error::new("too many substreams open", io::ErrorKind::ConnectionRefused)),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(_) => panic!("The QUIC background task has panicked")
        }
    }

    fn destroy_outbound(&self, _: Self::OutboundSubstream) {}

    fn read_substream(&self, substream: &mut Self::Substream, buf: &mut [u8]) -> Poll<usize, io::Error> {
        self.endpoint.read_substream(self.connection_id, substream.0, buf)
    }

    fn write_substream(&self, substream: &mut Self::Substream, data: &[u8]) -> Poll<usize, io::Error> {
        self.endpoint.write_substream(self.connection_id, substream.0, data)
    }

    fn flush_substream(&self, substream: &mut Self::Substream) -> Poll<(), io::Error> {
        // Substreams are always flushed.
        Ok(Async::Ready(()))
    }

    fn shutdown_substream(&self, substream: &mut Self::Substream, _: Shutdown) -> Poll<(), io::Error> {
        self.endpoint.shutdown_substream(self.connection_id, substream.0);
        Ok(Async::Ready(()))
    }

    fn destroy_substream(&self, substream: Self::Substream) {
        self.endpoint.destroy_substream(self.connection_id, substream.0);
    }

    fn is_remote_acknowledged(&self) -> bool {
        true        // FIXME:
    }

    fn shutdown(&self, _: Shutdown) -> Poll<(), io::Error> {
        Ok(Async::Ready(()))
    }

    fn flush_all(&self) -> Poll<(), io::Error> {
        // Everything is always flushed.
        Ok(Async::Ready(()))
    }
}

/// If `addr` is a QUIC address, returns the corresponding `SocketAddr`.
fn multiaddr_to_socketaddr(addr: &Multiaddr) -> Result<(SocketAddr, Option<Multihash>), ()> {
    let mut iter = addr.iter();
    let proto1 = iter.next().ok_or(())?;
    let proto2 = iter.next().ok_or(())?;
    let proto3 = iter.next().ok_or(())?;
    let proto4 = iter.next();

    if iter.next().is_some() {
        return Err(());
    }

    match (proto1, proto2, proto3, proto4) {
        (Protocol::Ip4(ip), Protocol::Udp(port), Protocol::Quic, None) => {
            Ok((SocketAddr::new(ip.into(), port), None))
        }
        (Protocol::Ip6(ip), Protocol::Udp(port), Protocol::Quic, None) => {
            Ok((SocketAddr::new(ip.into(), port), None))
        }
        (Protocol::Ip4(ip), Protocol::Udp(port), Protocol::Quic, Some(Protocol::P2p(hash))) => {
            Ok((SocketAddr::new(ip.into(), port), Some(hash)))
        }
        (Protocol::Ip6(ip), Protocol::Udp(port), Protocol::Quic, Some(Protocol::P2p(hash))) => {
            Ok((SocketAddr::new(ip.into(), port), Some(hash)))
        }
        _ => Err(()),
    }
}

/// Converts a `SocketAddr` into a QUIC multiaddr.
fn socket_addr_to_quic(addr: SocketAddr) -> Multiaddr {
    iter::once(Protocol::from(addr.ip()))
        .chain(iter::once(Protocol::Udp(addr.port())))
        .chain(iter::once(Protocol::Quic))
        .collect()
}

/// Future that dials an address.
#[must_use = "futures do nothing unless polled"]
pub struct QuicDialFut {
    /// Channel that will contain the result of this future.
    inner: oneshot::Receiver<Result<(), io::Error>>,
}

impl fmt::Debug for QuicDialFut {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("QuicDialFut").finish()
    }
}

impl Future for QuicDialFut {
    type Item = (PeerId, QuicMuxer);
    type Error = QuicError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let result = try_ready!(Ok(self.inner.poll()
            .expect("The sender is only ever be dropped if the background task panics; QED")));
        return Ok(Async::Ready());
    }
}

/*
/// Stream that listens on an TCP/IP address.
#[must_use = "futures do nothing unless polled"]
pub struct QuicListenStream {
    /// The QUIC protocol state machine.
    endpoint: Arc<QuicEndpoint>,
}

impl fmt::Debug for QuicListenStream {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("QuicListenStream")
            // TODO: expand?
            .finish()
    }
}

impl Stream for QuicListenStream {
    type Item = (FutureResult<(PeerId, QuicMuxer), QuicError>, Multiaddr);
    type Error = QuicError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.udp_socket.poll() {
            Ok(Async::Ready(Some(stream))) => {
                let public_key = self.public_keys.lock()
                    .remove(&stream.id())
                    .expect("picoquic calls certificate validator which saves the public key");
                let peer_id = public_key.into_peer_id();
                trace!("incoming connection to {:?}", peer_id);
                let addr = socket_addr_to_quic(stream.peer_addr());
                let muxer = QuicMuxer {
                    _context: None,
                    inner: Mutex::new(stream)
                };
                Ok(Async::Ready(Some((future::ok((peer_id, muxer)), addr))))
            }
            Ok(Async::Ready(None)) => Ok(Async::Ready(None)),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Err(e) => {
                warn!("listen error: {}", e);
                Err(e.into())
            }
        }
    }
}
*/
