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
use libp2p_core::upgrade::{self, toggleable::Toggleable};
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
pub struct KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Configuration for the Kademlia protocol.
    config: KademliaProtocolConfig,

    /// If true, we are trying to shut down the existing Kademlia substream.
    shutting_down: bool,

    /// If false, we always refuse incoming Kademlia substreams.
    allow_listening: bool,

    /// Next unique ID of a connection.
    next_connec_unique_id: UniqueConnecId,

    /// List of active substreams.
    substreams: Vec<SubstreamState<TSubstream>>,
}

/// State of an active substream, opened either by us or by the remote.
enum SubstreamState<TSubstream> {
    /// We haven't started opening the outgoing substream yet.
    OutPendingOpen(KadMsg, Option<TUserData>),
    /// We are waiting for the outgoing substream to be upgraded.
    OutPendingUpgrade(KadMsg, Option<TUserData>),
    /// Waiting to send a message to the remote.
    OutPendingSend(KadStreamSink<TSubstream>, KadMsg, Option<TUserData>),
    /// Waiting to send a message to the remote.
    OutPendingFlush(KadStreamSink<TSubstream>, KadMsg, Option<TUserData>),
    /// Waiting for an answer back from the remote.
    OutWaitingAnswer(KadStreamSink<TSubstream>, Option<TUserData>),
    /// Waiting for a request from the remote.
    InWaitingMessage(UniqueConnecId, KadStreamSink<TSubstream>),
    /// Waiting for a `KademliaHandlerIn` containing the response.
    InWaitingUser(UniqueConnecId, KadStreamSink<TSubstream>),
    /// Waiting to send an answer back to the remote.
    InPendingSend(UniqueConnecId, KadStreamSink<TSubstream>, KadMsg),
    /// Waiting to flush an answer back to the remote.
    InPendingFlush(UniqueConnecId, KadStreamSink<TSubstream>, KadMsg),
    /// The substream is being closed.
    Closing(KadStreamSink<TSubstream>),
}


impl<TSubstream> SubstreamState<TSubstream> {
    /// Consumes this state and produces the substream, if any exists.
    fn into_substream(self) -> Option<KadStreamSink<TSubstream>> {
        match self {
            SubstreamState::OutPendingOpen(_, _) => None,
            SubstreamState::OutPendingUpgrade(_, _) => None,
            SubstreamState::OutPendingSend(stream, _, _) => Some(stream),
            SubstreamState::OutPendingFlush(stream, _, _) => Some(stream),
            SubstreamState::OutWaitingAnswer(stream, _) => Some(stream),
            SubstreamState::InWaitingMessage(_, stream) => Some(stream),
            SubstreamState::InWaitingUser(_, stream) => Some(stream),
            SubstreamState::InPendingSend(_, stream, _) => Some(stream),
            SubstreamState::InPendingFlush(_, stream, _) => Some(stream),
            SubstreamState::Closing(stream) => Some(stream),
        }
    }
}

/// Event produced by the Kademlia handler.
#[derive(Debug)]
pub enum KademliaHandlerEvent<TUserData> {
    /// Request for the list of nodes whose IDs are the closest to `key`. The number of nodes
    /// returned is not specified, but should be around 20.
    FindNodeReq {
        /// Identifier of the node.
        key: PeerId,
        /// Identifier of the request. Needs to be passed back when answering.
        request_id: KademliaRequestId,
    },

    /// Response to an `KademliaHandlerIn::FindNodeReq`.
    FindNodeRes {
        /// Results of the request.
        closer_peers: Vec<KadPeer>,
        /// The user data passed to the `FindNodeReq`.
        user_data: TUserData,
    },

    /// Same as `FindNodeReq`, but should also return the entries of the local providers list for
    /// this key.
    GetProvidersReq {
        /// Identifier being searched.
        key: Multihash,
        /// Identifier of the request. Needs to be passed back when answering.
        request_id: KademliaRequestId,
    },

    /// Response to an `KademliaHandlerIn::GetProvidersReq`.
    GetProvidersRes {
        /// Nodes closest to the key.
        closer_peers: Vec<KadPeer>,
        /// Known providers for this key.
        provider_peers: Vec<KadPeer>,
        /// The user data passed to the `GetProvidersReq`.
        user_data: TUserData,
    },

    /// The remote indicates that this list of providers is known for this key.
    AddProvider {
        /// Key for which we should add providers.
        key: Multihash,
        /// Known provider for this key.
        provider_peer: KadPeer,
    },
}

/// Event to send to the handler.
pub enum KademliaHandlerIn<TUserData> {
    /// Request for the list of nodes whose IDs are the closest to `key`. The number of nodes
    /// returned is not specified, but should be around 20.
    FindNodeReq {
        /// Identifier of the node.
        key: PeerId,
        /// Custom user data. Passed back in the out event when the results arrive.
        user_data: TUserData,
    },

    /// Response to a `FindNodeReq`.
    FindNodeRes {
        /// Results of the request.
        closer_peers: Vec<KadPeer>,
        /// Identifier of the request that was made by the remote.
        ///
        /// It is a logic error to use an id of the handler of a different node.
        request_id: KademliaRequestId,
    },

    /// Same as `FindNodeReq`, but should also return the entries of the local providers list for
    /// this key.
    GetProvidersReq {
        /// Identifier being searched.
        key: Multihash,
        /// Custom user data. Passed back in the out event when the results arrive.
        user_data: TUserData,
    },

    /// Response to a `FindNodeReq`.
    GetProvidersRes {
        /// Nodes closest to the key.
        closer_peers: Vec<KadPeer>,
        /// Known providers for this key.
        provider_peers: Vec<KadPeer>,
        /// Identifier of the request that was made by the remote.
        ///
        /// It is a logic error to use an id of the handler of a different node.
        request_id: KademliaRequestId,
    },

    /// Indicates that this list of providers is known for this key.
    AddProvider {
        /// Key for which we should add providers.
        key: Multihash,
        /// Known provider for this key.
        provider_peer: KadPeer,
    },
}

/// Unique identifier for a request.
///
/// We don't implement `Clone` on purpose, in order to prevent users from answering the same
/// request twice.
#[derive(Debug, PartialEq, Eq)]
pub struct KademliaRequestId {
    /// Unique identifier for an incoming connection.
    connec_unique_id: UniqueConnecId,
}

/// Unique identifier for a connection.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct UniqueConnecId(u64);

impl<TUserData> Into<KadMsg> for KademliaHandlerIn<TUserData> {
    fn into(self) -> KadMsg {
        match self {
            KademliaHandlerIn::FindNodeReq { key } => {
                KadMsg::FindNodeReq { key }
            },
            KademliaHandlerIn::FindNodeRes { closer_peers } => {
                KadMsg::FindNodeRes { closer_peers }
            },
            KademliaHandlerIn::GetProvidersReq { key } => {
                KadMsg::GetProvidersReq { key }
            },
            KademliaHandlerIn::GetProvidersRes { closer_peers, provider_peers } => {
                KadMsg::GetProvidersRes { closer_peers, provider_peers }
            },
            KademliaHandlerIn::AddProvider { key, provider_peer } => {
                KadMsg::AddProvider { key, provider_peer }
            },
        }
    }
}

impl<TSubstream, TUserData> KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Create a new `KademliaHandler`.
    pub fn new() -> Self {
        KademliaHandler {
            config: upgrade::toggleable(Default::default()),
        }
    }
}

impl<TSubstream, TUserData> ProtocolsHandler for KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite + 'static,
{
    type KademliaHandlerIn = KademliaHandlerIn;
    type OutEvent = KademliaHandlerEvent;
    type Substream = TSubstream;
    type Protocol = Toggleable<KademliaProtocolConfig>;
    type OutboundOpenInfo = (KadMsg, TUserData);

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        let mut config = self.config;
        if !self.allow_listening {
            config.disable();
        }
        config
    }

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        match endpoint {
            NodeHandlerEndpoint::Dialer((msg, user_data)) => {
                self.substreams.push(SubstreamState::OutPendingSend(protocol, msg, user_data));
            },
            NodeHandlerEndpoint::Listener => {
                let connec_unique_id = self.next_connec_unique_id;
                self.next_connec_unique_id.0 += 1;
                self.substreams.push(SubstreamState::InWaitingMessage(connec_unique_id, protocol));
            },
        }
    }

    #[inline]
    fn inject_event(&mut self, message: KademliaHandlerIn) {
        match message {
            KademliaHandlerIn::FindNodeReq { key, user_data } => {
                let msg = KadMsg::FindNodeReq { key };
                self.substreams.push(SubstreamState::OutPendingOpen(msg, Some(user_data)));
            },
            KademliaHandlerIn::FindNodeRes { closer_peers, request_id } => {
                let pos = self.substreams.iter().position(|state| {
                    match state {
                        SubstreamState::InWaitingUser(ref conn_id, _)
                            if conn_id == request_id.connec_unique_id => true,
                        _ => false
                    }
                });

                if let Some(pos) = pos {
                    let (conn_id, substream) = match self.substreams.remove(pos) {
                        SubstreamState::InWaitingUser(conn_id, substream) => (conn_id, substream),
                        _ => unreachable!()
                    };

                    let msg = KadMsg::FindNodeRes { closer_peers };
                    self.substreams.push(SubstreamState::InPendingSend(conn_id, substream, msg));
                }
            },
            KademliaHandlerIn::GetProvidersReq { key, user_data } => {
                let msg = KadMsg::GetProvidersReq { key };
                self.substreams.push(SubstreamState::OutPendingOpen(msg, Some(user_data)));
            },
            KademliaHandlerIn::GetProvidersRes { closer_peers, provider_peers, request_id } => {
                let pos = self.substreams.iter().position(|state| {
                    match state {
                        SubstreamState::InWaitingUser(ref conn_id, _)
                            if conn_id == request_id.connec_unique_id => true,
                        _ => false
                    }
                });

                if let Some(pos) = pos {
                    let (conn_id, substream) = match self.substreams.remove(pos) {
                        SubstreamState::InWaitingUser(conn_id, substream) => (conn_id, substream),
                        _ => unreachable!()
                    };

                    let msg = KadMsg::GetProvidersRes { closer_peers, provider_peers };
                    self.substreams.push(SubstreamState::InPendingSend(conn_id, substream, msg));
                }
            },
            KademliaHandlerIn::AddProvider { key, provider_peer } => {
                let msg = KadMsg::AddProvider { key, provider_peer };
                self.substreams.push(SubstreamState::OutPendingOpen(msg, None));
            },
        }
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
    }

    #[inline]
    fn inject_dial_upgrade_error(&mut self, _: Self::OutboundOpenInfo, _: &io::Error) {
        // FIXME: report as event that a request errored
    }

    #[inline]
    fn shutdown(&mut self) {
        self.shutting_down = true;
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, io::Error> {
        // Special case if shutting down.
        if self.shutting_down {
            for n in (0..self.substreams.len()).rev() {
                if let Some(stream) = self.substreams.swap_remove(n).into_substream() {
                    match stream.close() {
                        Ok(Async::Ready(())) | Err(_) => (),
                        Ok(Async::NotReady) => self.substreams.push(SubstreamState::Closing(stream)),
                    }
                }
            }

            if self.substreams.is_empty() {
                return Ok(Async::Ready(None));
            } else {
                return Ok(Async::NotReady);
            }
        }

        // Open a Kademlia substream if necessary.
        if !self.pending_open.is_empty() {
            let (msg, user_data) = self.pending_open.remove(0);
            let ev = NodeHandlerEvent::OutboundSubstreamRequest((self.config, (msg, user_data)));
            return Ok(Async::Ready(Some(ev)));
        }

        // We remove each element from `substreams` one by one and add them back.
        for n in (0..self.kademlia_substream_in.len()).rev() {
            let (connec_id, mut substream) = self.kademlia_substream_in.swap_remove(n);
            match substream.poll() {
                Ok(Async::NotReady) => {
                    self.kademlia_substream_in.push((connec_id, substream));
                }
                Ok(Async::Ready(Some((upgrade, send_back_addr)))) => {
                    let listen_addr = listener.address.clone();
                    self.listeners.push(listener);
                    return Async::Ready(Some(ListenersEvent::Incoming {
                        upgrade,
                        listen_addr,
                        send_back_addr,
                    }));
                }
                Ok(Async::Ready(Some(KadMsg::Ping))) => {

                },
                Ok(Async::Ready(Some(KadMsg::Pong))) => {
                },
                Ok(Async::Ready(Some(KadMsg::PutValue { key, record }))) => {
                },

                Ok(Async::Ready(Some(KadMsg::GetValueReq { key }))) => {
                },

                Ok(Async::Ready(Some(KadMsg::GetValueRes { record, closer_peers }))) => {
                },
                Ok(Async::Ready(Some(KadMsg::FindNodeReq { key }))) => {
                },

                Ok(Async::Ready(Some(KadMsg::FindNodeRes { closer_peers }))) => {
                },
                Ok(Async::Ready(Some(KadMsg::GetProvidersReq { key }))) => {
                },
                Ok(Async::Ready(Some(KadMsg::GetProvidersRes { closer_peers, provider_peers }))) => {
                },
                Ok(Async::Ready(Some(KadMsg::AddProvider { key, provider_peer }))) => {
                },
                Ok(Async::Ready(None)) | Err(_) => {}
            }
        }

        Ok(Async::NotReady)
    }
}
