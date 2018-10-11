// Copyright 2018 Parity Technologies (UK) Ltd.
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

use futures::prelude::*;
use libp2p_core::{ConnectionUpgrade, PeerId};
use libp2p_core::nodes::{ProtocolsHandler, NodeHandlerEvent, NodeHandlerEndpoint};
use multihash::Multihash;
use protocol::{KadMsg, KadPeer, KademliaProtocolConfig, KadStreamSink};
use std::{collections::VecDeque, io};
use tokio_io::{AsyncRead, AsyncWrite};

/// Protocol handler that handles Kademlia communications with the remote.
///
/// The handler will automatically open a Kademlia substream with the remote the first time we
/// try to send a message to the remote.
///
/// The handler will only allow one Kademlia substream at a time. Any further open substream will
/// automatically get closed.
pub struct KademliaHandler<TSubstream>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Configuration for the Kademlia protocol.
    config: KademliaProtocolConfig,

    /// If true, we are trying to shut down the existing Kademlia substream.
    shutting_down: bool,

    /// The already-open Kademlia substream, if any.
    kademlia_substream: Option<KadStreamSink<TSubstream>>,

    /// If true, we are in the process of opening a Kademlia substream on the dialing side.
    upgrading: bool,

    /// Queue of messages to send to the Kademlia substream.
    ///
    /// If an entry contains `NotReady`, that means we are waiting for an `InEvent` to insert
    /// there and we shouldn't send any further element in the queue.
    send_queue: VecDeque<Async<KadMsg>>,
}

/// Event produced by the Kademlia handler.
#[derive(Debug)]
pub enum KademliaHandlerEvent {
    /// Opened a new Kademlia substream.
    Open,

    /// Ignoring an incoming Kademlia substream because we already have one.
    IgnoredIncoming,

    /// Closed an existing Kademlia substream.
    Closed(Result<(), io::Error>),

    /// Request for the list of nodes whose IDs are the closest to `key`. The number of nodes
    /// returned is not specified, but should be around 20.
    FindNodeReq {
        /// Identifier of the node.
        key: PeerId,
    },

    /// Response to a `FindNodeReq`.
    FindNodeRes {
        /// Results of the request.
        closer_peers: Vec<KadPeer>,
    },

    /// Same as `FindNodeReq`, but should also return the entries of the local providers list for
    /// this key.
    GetProvidersReq {
        /// Identifier being searched.
        key: Multihash,
    },

    /// Response to a `FindNodeReq`.
    GetProvidersRes {
        /// Nodes closest to the key.
        closer_peers: Vec<KadPeer>,
        /// Known providers for this key.
        provider_peers: Vec<KadPeer>,
    },

    /// Indicates that this list of providers is known for this key.
    AddProvider {
        /// Key for which we should add providers.
        key: Multihash,
        /// Known provider for this key.
        provider_peer: KadPeer,
    },
}

impl<TSubstream> KademliaHandler<TSubstream>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Create a new `KademliaHandler`.
    pub fn new() -> KademliaHandler<TSubstream> {
        KademliaHandler {
            config: Default::default(),
            kademlia_substream: None,
            send_queue: VecDeque::new(),
            shutting_down: false,
            upgrading: false,
        }
    }
}

/// Event to send to the handler.
pub enum InEvent {
    /// Request for the list of nodes whose IDs are the closest to `key`. The number of nodes
    /// returned is not specified, but should be around 20.
    FindNodeReq {
        /// Identifier of the node.
        key: PeerId,
    },

    /// Response to a `FindNodeReq`.
    FindNodeRes {
        /// Results of the request.
        closer_peers: Vec<KadPeer>,
    },

    /// Same as `FindNodeReq`, but should also return the entries of the local providers list for
    /// this key.
    GetProvidersReq {
        /// Identifier being searched.
        key: Multihash,
    },

    /// Response to a `FindNodeReq`.
    GetProvidersRes {
        /// Nodes closest to the key.
        closer_peers: Vec<KadPeer>,
        /// Known providers for this key.
        provider_peers: Vec<KadPeer>,
    },

    /// Indicates that this list of providers is known for this key.
    AddProvider {
        /// Key for which we should add providers.
        key: Multihash,
        /// Known provider for this key.
        provider_peer: KadPeer,
    },
}

impl Into<KadMsg> for InEvent {
    fn into(self) -> KadMsg {
        match self {
            InEvent::FindNodeReq { key } => {
                KadMsg::FindNodeReq { key }
            },
            InEvent::FindNodeRes { closer_peers } => {
                KadMsg::FindNodeRes { closer_peers }
            },
            InEvent::GetProvidersReq { key } => {
                KadMsg::GetProvidersReq { key }
            },
            InEvent::GetProvidersRes { closer_peers, provider_peers } => {
                KadMsg::GetProvidersRes { closer_peers, provider_peers }
            },
            InEvent::AddProvider { key, provider_peer } => {
                KadMsg::AddProvider { key, provider_peer }
            },
        }
    }
}

impl<TSubstream> ProtocolsHandler for KademliaHandler<TSubstream>
where TSubstream: AsyncRead + AsyncWrite + 'static,
{
    type InEvent = InEvent;
    type OutEvent = KademliaHandlerEvent;
    type Substream = TSubstream;
    type Protocol = KademliaProtocolConfig;
    type OutboundOpenInfo = ();

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        self.config
    }

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        if let NodeHandlerEndpoint::Dialer(_) = endpoint {
            self.upgrading = false;
        }

        // If we already have a Kademlia substream, drop the incoming one.
        if self.kademlia_substream.is_none() {
            self.kademlia_substream = Some(protocol);
            // TODO: self.report_kad_open = true;
        } else {
            // TODO: report
        }
    }

    #[inline]
    fn inject_event(&mut self, message: Self::InEvent) {
        let message = message.into();
        if let Some(pos) = self.send_queue.iter().position(|e| e == &Async::NotReady) {
            self.send_queue[pos] = Async::Ready(message);
        } else {
            self.send_queue.push_back(Async::Ready(message));
        }
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
    }

    #[inline]
    fn inject_dial_upgrade_error(&mut self, _: Self::OutboundOpenInfo, _: &io::Error) {
        self.upgrading = false;
        // TODO: report as event
    }

    #[inline]
    fn shutdown(&mut self) {
        self.shutting_down = true;
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, io::Error> {
        // Special case if shutting down.
        if self.shutting_down {
            if let Some(kad) = self.kademlia_substream.as_mut() {
                return Ok(kad.close()?.map(|_| None));
            } else {
                return Ok(Async::Ready(None));
            }
        }

        // Open a Kademlia substream if necessary.
        if self.kademlia_substream.is_none() && !self.send_queue.is_empty() && !self.upgrading {
            self.upgrading = true;
            let ev = NodeHandlerEvent::OutboundSubstreamRequest((self.config, ()));
            return Ok(Async::Ready(Some(ev)));
        }

        // Poll for Kademlia events.
        if let Some(mut stream) = self.kademlia_substream.take() {
            loop {
                // Try to flush the send queue.
                while let Some(message) = self.send_queue.pop_front() {
                    match message {
                        Async::Ready(msg) => {
                            match stream.start_send(msg)? {
                                AsyncSink::Ready => (),
                                AsyncSink::NotReady(msg) => {
                                    self.send_queue.push_front(Async::Ready(msg));
                                    break;
                                }
                            }
                        },
                        Async::NotReady => {
                            self.send_queue.push_front(Async::NotReady);
                            break;
                        }
                    }
                }

                // TODO: flush?

                match stream.poll() {
                    Ok(Async::Ready(Some(KadMsg::FindNodeReq { key }))) => {
                        self.kademlia_substream = Some(stream);
                        self.send_queue.push_back(Async::NotReady);
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::FindNodeReq { key });
                        return Ok(Async::Ready(Some(ev)));
                    },
                    Ok(Async::Ready(Some(KadMsg::FindNodeRes { closer_peers }))) => {
                        self.kademlia_substream = Some(stream);
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::FindNodeRes { closer_peers });
                        return Ok(Async::Ready(Some(ev)));
                    },
                    Ok(Async::Ready(Some(KadMsg::GetProvidersReq { key }))) => {
                        self.kademlia_substream = Some(stream);
                        self.send_queue.push_back(Async::NotReady);
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::GetProvidersReq { key });
                        return Ok(Async::Ready(Some(ev)));
                    },
                    Ok(Async::Ready(Some(KadMsg::GetProvidersRes { closer_peers, provider_peers }))) => {
                        self.kademlia_substream = Some(stream);
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::GetProvidersRes { closer_peers, provider_peers });
                        return Ok(Async::Ready(Some(ev)));
                    },
                    Ok(Async::Ready(Some(KadMsg::AddProvider { key, provider_peer }))) => {
                        self.kademlia_substream = Some(stream);
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::AddProvider { key, provider_peer });
                        return Ok(Async::Ready(Some(ev)));
                    },
                    Ok(Async::Ready(Some(KadMsg::Ping))) => {
                        self.send_queue.push_back(Async::Ready(KadMsg::Pong));
                    },
                    Ok(Async::Ready(Some(KadMsg::Pong))) => {
                        // We never send pings, so this should never be received.
                        let err = io::Error::new(io::ErrorKind::Other, "Received PONG");
                        return Err(err);
                    },
                    Ok(Async::Ready(Some(KadMsg::PutValue { .. }))) => {
                        let err = io::Error::new(io::ErrorKind::Other, "PUT_VALUE not implemented");
                        return Err(err);
                    },
                    Ok(Async::Ready(Some(KadMsg::GetValueReq { .. }))) => {
                        let err = io::Error::new(io::ErrorKind::Other, "GET_VALUE not implemented");
                        return Err(err);
                    },
                    Ok(Async::Ready(Some(KadMsg::GetValueRes { .. }))) => {
                        let err = io::Error::new(io::ErrorKind::Other, "GET_VALUE not implemented");
                        return Err(err);
                    },
                    Ok(Async::NotReady) => {
                        self.kademlia_substream = Some(stream);
                        break;
                    },
                    Ok(Async::Ready(None)) => {
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::Closed(Ok(())));
                        return Ok(Async::Ready(Some(ev)));
                    },
                    Err(err) => {
                        let ev = NodeHandlerEvent::Custom(KademliaHandlerEvent::Closed(Err(err)));
                        return Ok(Async::Ready(Some(ev)));
                    },
                }
            }
        }

        Ok(Async::NotReady)
    }
}
