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
use libp2p_core::{ConnectionUpgrade, nodes::protocol_handler::ProtocolHandler};
use libp2p_core::nodes::handled_node::{NodeHandlerEvent, NodeHandlerEndpoint};
use protocol::{KadMsg, KademliaProtocolConfig, KadStreamSink};
use std::io;
use std::time::{Duration, Instant};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_timer::Delay;
use void::Void;

/// Protocol handler that handles Kademlia communications with the remote.
pub struct KademliaHandler<TSubstream> {
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
#[derive(Debug, Clone)]
pub enum OutEvent {
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

impl<TSubstream> KademliaHandler<TSubstream> {
    pub fn new() -> KademliaHandler<TSubstream> {
        KademliaHandler {
            config: Default::default(),
            kademlia_substream: None,
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
        unimplemented!()
    }
}

impl<TSubstream> ProtocolHandler<TSubstream> for KademliaHandler<TSubstream>
where TSubstream: AsyncRead + AsyncWrite,
{
    type InEvent = InEvent;
    type OutEvent = OutEvent;
    type Protocol = KademliaProtocolConfig;
    type OutboundOpenInfo = ();

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        self.config
    }

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        if endpont.is_dialer() {
            self.upgrading = false;
        }

        // If we already have a Kademlia substream, drop the incoming one.
        if self.kademlia_substream.is_none() {
            self.kademlia_substream = Some(protocol);
            self.report_kad_open = true;
        } else {
            // TODO: report
        }
    }

    #[inline]
    fn inject_event(&mut self, message: Self::InEvent) {
        self.send_queue.push_back(Async::Ready(message.into()));
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
                return kad.close()?.map(|_| None);
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
        if let Some(stream) = self.kademlia_substream.take() {
            loop {
                // Try to flush the send queue.
                while let Some(message) = self.send_queue.pop_front() {
                    match message {
                        Async::Ready(msg) => {
                            match stream.start_send(msg) {
                                AsyncSink::Ready => (),
                                AsyncSink::NotReady(msg) => {
                                    self.send_queue.push_front(msg);
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
                        return Ok(Async::Ready(Some(OutEvent::FindNodeReq { key })));
                    },
                    Ok(Async::Ready(Some(KadMsg::Ping))) => {
                        // We never send pings, so whenever we receive a ping through Kademlia
                        // we should answer with another ping.
                        self.send_queue.push_back(Async::Ready(KadMsg::Ping));
                    },
                    Ok(Async::NotReady) => {
                        self.kademlia_substream = Some(stream);
                        break;
                    },
                    Ok(Async::Ready(None)) => {
                        return Ok(Async::Ready(Some(OutEvent::KadClosed(Ok(())))));
                    },
                    Err(err) => {
                        return Ok(Async::Ready(Some(OutEvent::KadClosed(Err(err)))));
                    },
                }
            }
        }

        Ok(Async::NotReady)
    }
}
