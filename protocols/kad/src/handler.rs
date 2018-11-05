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
use libp2p_core::nodes::{ProtocolsHandler, ProtocolsHandlerEvent, NodeHandlerEndpoint};
use libp2p_core::upgrade::{self, toggleable::Toggleable};
use multihash::Multihash;
use protocol::{KadMsg, KadPeer, KademliaProtocolConfig, KadStreamSink};
use std::io;
use tokio_io::{AsyncRead, AsyncWrite};

/// Protocol handler that handles Kademlia communications with the remote.
///
/// The handler will automatically open a Kademlia substream with the remote for each request we
/// make.
///
/// It also handles requests made by the remote.
pub struct KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Configuration for the Kademlia protocol.
    config: Toggleable<KademliaProtocolConfig>,

    /// If true, we are trying to shut down the existing Kademlia substream and should refuse any
    /// incoming connection.
    shutting_down: bool,

    /// If false, we always refuse incoming Kademlia substreams.
    allow_listening: bool,

    /// Next unique ID of a connection.
    next_connec_unique_id: UniqueConnecId,

    /// List of active substreams with the state they are in.
    substreams: Vec<SubstreamState<TSubstream, TUserData>>,
}

/// State of an active substream, opened either by us or by the remote.
enum SubstreamState<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// We haven't started opening the outgoing substream yet.
    /// Contains the request we want to send, and the user data if we expect an answer.
    OutPendingOpen(KadMsg, Option<TUserData>),
    /// We are waiting for the outgoing substream to be upgraded.
    /// Contains the request we want to send, and the user data if we expect an answer.
    OutPendingUpgrade(KadMsg, Option<TUserData>),
    /// Waiting to send a message to the remote.
    OutPendingSend(KadStreamSink<TSubstream>, KadMsg, Option<TUserData>),
    /// Waiting to send a message to the remote.
    /// Waiting to flush the substream so that the data arrives to the remote.
    OutPendingFlush(KadStreamSink<TSubstream>, Option<TUserData>),
    /// Waiting for an answer back from the remote.
    // TODO: add timeout
    OutWaitingAnswer(KadStreamSink<TSubstream>, TUserData),
    /// An error happened on the substream and we should report the error to the user.
    OutReportError(io::Error, TUserData),
    /// Waiting for a request from the remote.
    InWaitingMessage(UniqueConnecId, KadStreamSink<TSubstream>),
    /// Waiting for the user to send a `KademliaHandlerIn` event containing the response.
    InWaitingUser(UniqueConnecId, KadStreamSink<TSubstream>),
    /// Waiting to send an answer back to the remote.
    InPendingSend(UniqueConnecId, KadStreamSink<TSubstream>, KadMsg),
    /// Waiting to flush an answer back to the remote.
    InPendingFlush(UniqueConnecId, KadStreamSink<TSubstream>),
    /// The substream is being closed.
    Closing(KadStreamSink<TSubstream>),
}

impl<TSubstream, TUserData> SubstreamState<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Consumes this state and produces the substream, if relevant.
    fn into_substream(self) -> Option<KadStreamSink<TSubstream>> {
        match self {
            SubstreamState::OutPendingOpen(_, _) => None,
            SubstreamState::OutPendingUpgrade(_, _) => None,
            SubstreamState::OutPendingSend(stream, _, _) => Some(stream),
            SubstreamState::OutPendingFlush(stream, _) => Some(stream),
            SubstreamState::OutWaitingAnswer(stream, _) => Some(stream),
            SubstreamState::OutReportError(_, _) => None,
            SubstreamState::InWaitingMessage(_, stream) => Some(stream),
            SubstreamState::InWaitingUser(_, stream) => Some(stream),
            SubstreamState::InPendingSend(_, stream, _) => Some(stream),
            SubstreamState::InPendingFlush(_, stream) => Some(stream),
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

    /// An error happened when performing a  query.
    QueryError {
        /// The error that happened.
        error: io::Error,
        /// The user data passed to the query.
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

    /// Response to a `GetProvidersReq`.
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

    /// Indicates that this provider is known for this key.
    ///
    /// The API of the handler doesn't expose any event that allows you to know whether this
    /// succeeded.
    AddProvider {
        /// Key for which we should add providers.
        key: Multihash,
        /// Known provider for this key.
        provider_peer: KadPeer,
    },
}

/// Unique identifier for a request. Must be passed back in order to answer a request from
/// the remote.
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

impl<TSubstream, TUserData> KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    /// Create a `KademliaHandler` that only allows sending messages to the remote but denying
    /// incoming connections.
    #[inline]
    pub fn dial_only() -> Self {
        KademliaHandler::with_allow_listening(false)
    }

    /// Create a `KademliaHandler` that only allows sending messages but also receive incoming
    /// requests.
    ///
    /// The `Default` trait implementation wraps around this function.
    #[inline]
    pub fn dial_and_listen() -> Self {
        KademliaHandler::with_allow_listening(true)
    }

    fn with_allow_listening(allow_listening: bool) -> Self {
        KademliaHandler {
            config: upgrade::toggleable(Default::default()),
            shutting_down: false,
            allow_listening,
            next_connec_unique_id: UniqueConnecId(0),
            substreams: Vec::new(),
        }
    }
}

impl<TSubstream, TUserData> Default for KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite,
{
    #[inline]
    fn default() -> Self {
        KademliaHandler::dial_and_listen()
    }
}

impl<TSubstream, TUserData> ProtocolsHandler for KademliaHandler<TSubstream, TUserData>
where TSubstream: AsyncRead + AsyncWrite + 'static,
      TUserData: Clone,
{
    type InEvent = KademliaHandlerIn<TUserData>;
    type OutEvent = KademliaHandlerEvent<TUserData>;
    type Substream = TSubstream;
    type Protocol = Toggleable<KademliaProtocolConfig>;
    // Message of the request to send to the remote, and user data if we expect an answer.
    type OutboundOpenInfo = (KadMsg, Option<TUserData>);

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        let mut config = self.config;
        if !self.allow_listening {
            config.disable();
        }
        config
    }

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        if self.shutting_down {
            return;
        }

        match endpoint {
            NodeHandlerEndpoint::Dialer((msg, user_data)) => {
                self.substreams.push(SubstreamState::OutPendingSend(protocol, msg, user_data));
            },
            NodeHandlerEndpoint::Listener => {
                debug_assert!(self.allow_listening);
                let connec_unique_id = self.next_connec_unique_id;
                self.next_connec_unique_id.0 += 1;
                self.substreams.push(SubstreamState::InWaitingMessage(connec_unique_id, protocol));
            },
        }
    }

    #[inline]
    fn inject_event(&mut self, message: KademliaHandlerIn<TUserData>) {
        match message {
            KademliaHandlerIn::FindNodeReq { key, user_data } => {
                let msg = KadMsg::FindNodeReq { key: key.clone() };
                self.substreams.push(SubstreamState::OutPendingOpen(msg, Some(user_data.clone())));
            },
            KademliaHandlerIn::FindNodeRes { closer_peers, request_id } => {
                let pos = self.substreams.iter().position(|state| {
                    match state {
                        SubstreamState::InWaitingUser(ref conn_id, _)
                            if conn_id == &request_id.connec_unique_id => true,
                        _ => false
                    }
                });

                if let Some(pos) = pos {
                    let (conn_id, substream) = match self.substreams.remove(pos) {
                        SubstreamState::InWaitingUser(conn_id, substream) => (conn_id, substream),
                        _ => unreachable!()
                    };

                    let msg = KadMsg::FindNodeRes { closer_peers: closer_peers.clone() };
                    self.substreams.push(SubstreamState::InPendingSend(conn_id, substream, msg));
                }
            },
            KademliaHandlerIn::GetProvidersReq { key, user_data } => {
                let msg = KadMsg::GetProvidersReq { key: key.clone() };
                self.substreams.push(SubstreamState::OutPendingOpen(msg, Some(user_data.clone())));
            },
            KademliaHandlerIn::GetProvidersRes { closer_peers, provider_peers, request_id } => {
                let pos = self.substreams.iter().position(|state| {
                    match state {
                        SubstreamState::InWaitingUser(ref conn_id, _)
                            if conn_id == &request_id.connec_unique_id => true,
                        _ => false
                    }
                });

                if let Some(pos) = pos {
                    let (conn_id, substream) = match self.substreams.remove(pos) {
                        SubstreamState::InWaitingUser(conn_id, substream) => (conn_id, substream),
                        _ => unreachable!()
                    };

                    let msg = KadMsg::GetProvidersRes { closer_peers: closer_peers.clone(), provider_peers: provider_peers.clone() };
                    self.substreams.push(SubstreamState::InPendingSend(conn_id, substream, msg));
                }
            },
            KademliaHandlerIn::AddProvider { key, provider_peer } => {
                let msg = KadMsg::AddProvider { key: key.clone(), provider_peer: provider_peer.clone() };
                self.substreams.push(SubstreamState::OutPendingOpen(msg, None));
            },
        }
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
    }

    #[inline]
    fn inject_dial_upgrade_error(&mut self, (_, user_data): Self::OutboundOpenInfo, error: io::Error) {
        // TODO: cache the fact that the remote doesn't support kademlia at all, so that we don't
        //       continue trying
        if let Some(user_data) = user_data {
            self.substreams.push(SubstreamState::OutReportError(error, user_data));
        }
    }

    #[inline]
    fn shutdown(&mut self) {
        self.shutting_down = true;
    }

    fn poll(&mut self) -> Poll<Option<ProtocolsHandlerEvent<Self::Protocol, Self::OutboundOpenInfo, Self::OutEvent>>, io::Error> {
        // Special case if shutting down.
        if self.shutting_down {
            for n in (0..self.substreams.len()).rev() {
                if let Some(mut stream) = self.substreams.swap_remove(n).into_substream() {
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

        // We remove each element from `substreams` one by one and add them back.
        for n in (0..self.substreams.len()).rev() {
            let mut substream = self.substreams.swap_remove(n);

            loop {
                match advance_substream(substream, self.config) {
                    (Some(new_state), Some(event), _) => {
                        self.substreams.push(new_state);
                        return Ok(Async::Ready(Some(event)));
                    },
                    (None, Some(event), _) => {
                        return Ok(Async::Ready(Some(event)));
                    },
                    (Some(new_state), None, false) => {
                        self.substreams.push(new_state);
                        break;
                    },
                    (Some(new_state), None, true) => {
                        substream = new_state;
                        continue;
                    },
                    (None, None, _) => {
                        break;
                    }
                }
            }
        }

        Ok(Async::NotReady)
    }
}

/// Advances one substream.
///
/// Returns the new state for that substream, an event to generate, and whether the substream
/// should be polled again.
fn advance_substream<TSubstream, TUserData>(state: SubstreamState<TSubstream, TUserData>, upgrade: Toggleable<KademliaProtocolConfig>)
    -> (Option<SubstreamState<TSubstream, TUserData>>, Option<ProtocolsHandlerEvent<Toggleable<KademliaProtocolConfig>, (KadMsg, Option<TUserData>), KademliaHandlerEvent<TUserData>>>, bool)
where TSubstream: AsyncRead + AsyncWrite,
{
    match state {
        SubstreamState::OutPendingOpen(msg, user_data) => {
            let ev = ProtocolsHandlerEvent::OutboundSubstreamRequest { upgrade, info: (msg, user_data) };
            (None, Some(ev), false)
        },
        SubstreamState::OutPendingUpgrade(msg, user_data) => {
            (Some(SubstreamState::OutPendingUpgrade(msg, user_data)), None, false)
        },
        SubstreamState::OutPendingSend(mut substream, msg, user_data) => {
            match substream.start_send(msg) {
                Ok(AsyncSink::Ready) => {
                    (Some(SubstreamState::OutPendingFlush(substream, user_data)), None, true)
                },
                Ok(AsyncSink::NotReady(msg)) => {
                    (Some(SubstreamState::OutPendingSend(substream, msg, user_data)), None, false)
                },
                Err(error) => {
                    let event = if let Some(user_data) = user_data {
                        let ev = KademliaHandlerEvent::QueryError { error, user_data };
                        Some(ProtocolsHandlerEvent::Custom(ev))
                    } else {
                        None
                    };

                    (None, event, false)
                },
            }
        },
        SubstreamState::OutPendingFlush(mut substream, user_data) => {
            match substream.poll_complete() {
                Ok(Async::Ready(())) => {
                    if let Some(user_data) = user_data {
                        (Some(SubstreamState::OutWaitingAnswer(substream, user_data)), None, true)
                    } else {
                        (Some(SubstreamState::Closing(substream)), None, true)
                    }
                },
                Ok(Async::NotReady) => {
                    (Some(SubstreamState::OutPendingFlush(substream, user_data)), None, false)
                },
                Err(error) => {
                    let event = if let Some(user_data) = user_data {
                        let ev = KademliaHandlerEvent::QueryError { error, user_data };
                        Some(ProtocolsHandlerEvent::Custom(ev))
                    } else {
                        None
                    };

                    (None, event, false)
                },
            }
        },
        SubstreamState::OutWaitingAnswer(mut substream, user_data) => {
            match substream.poll() {
                Ok(Async::Ready(Some(msg))) => {
                    let new_state = SubstreamState::Closing(substream);
                    let event = process_kad_response(msg, user_data);
                    (Some(new_state), Some(ProtocolsHandlerEvent::Custom(event)), true)
                },
                Ok(Async::NotReady) => {
                    (Some(SubstreamState::OutWaitingAnswer(substream, user_data)), None, false)
                },
                Err(error) => {
                    let event = KademliaHandlerEvent::QueryError { error, user_data };
                    (None, Some(ProtocolsHandlerEvent::Custom(event)), false)
                },
                Ok(Async::Ready(None)) => {
                    let error = io::Error::new(io::ErrorKind::Other, "unexpected EOF");
                    let event = KademliaHandlerEvent::QueryError { error, user_data };
                    (None, Some(ProtocolsHandlerEvent::Custom(event)), false)
                },
            }
        },
        SubstreamState::OutReportError(error, user_data) => {
            let event = KademliaHandlerEvent::QueryError { error, user_data };
            (None, Some(ProtocolsHandlerEvent::Custom(event)), false)
        },
        SubstreamState::InWaitingMessage(id, mut substream) => {
            match substream.poll() {
                Ok(Async::Ready(Some(msg))) => {
                    if let Ok(ev) = process_kad_request(msg, id) {
                        (Some(SubstreamState::InWaitingUser(id, substream)), Some(ProtocolsHandlerEvent::Custom(ev)), false)
                    } else {
                        (Some(SubstreamState::Closing(substream)), None, true)
                    }
                },
                Ok(Async::NotReady) => {
                    (Some(SubstreamState::InWaitingMessage(id, substream)), None, false)
                },
                Ok(Async::Ready(None)) | Err(_) => (None, None, false),
            }
        },
        SubstreamState::InWaitingUser(id, substream) => {
            (Some(SubstreamState::InWaitingUser(id, substream)), None, false)
        },
        SubstreamState::InPendingSend(id, mut substream, msg) => {
            match substream.start_send(msg) {
                Ok(AsyncSink::Ready) => {
                    (Some(SubstreamState::InPendingFlush(id, substream)), None, true)
                },
                Ok(AsyncSink::NotReady(msg)) => {
                    (Some(SubstreamState::InPendingSend(id, substream, msg)), None, false)
                },
                Err(_) => (None, None, false),
            }
        },
        SubstreamState::InPendingFlush(id, mut substream) => {
            match substream.poll_complete() {
                Ok(Async::Ready(())) => {
                    (Some(SubstreamState::InWaitingMessage(id, substream)), None, true)
                },
                Ok(Async::NotReady) => {
                    (Some(SubstreamState::InPendingFlush(id, substream)), None, false)
                },
                Err(_) => (None, None, false),
            }
        },
        SubstreamState::Closing(mut stream) => {
            match stream.close() {
                Ok(Async::Ready(())) => (None, None, false),
                Ok(Async::NotReady) => (Some(SubstreamState::Closing(stream)), None, false),
                Err(_) => (None, None, false),
            }
        },
    }
}

/// Processes a Kademlia message that's expected to be a request from a remote.
fn process_kad_request<TUserData>(event: KadMsg, connec_unique_id: UniqueConnecId) -> Result<KademliaHandlerEvent<TUserData>, io::Error> {
    match event {
        KadMsg::Ping => {
            // TODO: implement
            Err(io::Error::new(io::ErrorKind::InvalidData, "the PING Kademlia message is notimplemented"))
        },
        KadMsg::PutValue { .. } => {
            // TODO: implement
            Err(io::Error::new(io::ErrorKind::InvalidData, "the PUT_VALUE Kademlia message is not implemented"))
        },

        KadMsg::FindNodeReq { key } => {
            Ok(KademliaHandlerEvent::FindNodeReq {
                key,
                request_id: KademliaRequestId { connec_unique_id }
            })
        },
        KadMsg::GetValueReq { .. } => {
            // TODO: implement
            Err(io::Error::new(io::ErrorKind::InvalidData, "the GET_VALUE Kademlia message is not implemented"))
        },
        KadMsg::GetProvidersReq { key } => {
            Ok(KademliaHandlerEvent::GetProvidersReq {
                key,
                request_id: KademliaRequestId { connec_unique_id }
            })
        },
        KadMsg::AddProvider { key, provider_peer } => {
            Ok(KademliaHandlerEvent::AddProvider { key, provider_peer })
        },

        KadMsg::Pong |
        KadMsg::GetValueRes { .. } |
        KadMsg::FindNodeRes { .. } |
        KadMsg::GetProvidersRes { .. } => {
            // TODO: eventually we could rework protocol.rs to split between dialer and listener.
            panic!("The code of the protocol can only generate request messages when we listen \
                    on a connection from the remote");
        },
    }
}

/// Process a Kademlia message that's supposed to be a response to one of our requests.
fn process_kad_response<TUserData>(event: KadMsg, user_data: TUserData)
    -> KademliaHandlerEvent<TUserData>
{
    // TODO: must check that the response corresponds to the request
    match event {
        KadMsg::Pong => {
            // We never send out pings.
            let err = io::Error::new(io::ErrorKind::InvalidData, "received unexpected PONG message");
            KademliaHandlerEvent::QueryError { error: err, user_data }
        },
        KadMsg::GetValueRes { .. } => {
            // TODO: implement
            let err = io::Error::new(io::ErrorKind::InvalidData, "received unexpected GET_VALUE response");
            KademliaHandlerEvent::QueryError { error: err, user_data }
        },
        KadMsg::FindNodeRes { closer_peers } => {
            KademliaHandlerEvent::FindNodeRes { closer_peers, user_data }
        },
        KadMsg::GetProvidersRes { closer_peers, provider_peers } => {
            KademliaHandlerEvent::GetProvidersRes { closer_peers, provider_peers, user_data }
        },
        KadMsg::PutValue { .. } |
        KadMsg::Ping |
        KadMsg::GetValueReq { .. } |
        KadMsg::FindNodeReq { .. } |
        KadMsg::GetProvidersReq { .. } |
        KadMsg::AddProvider { .. } => {
            // TODO: eventually we could rework protocol.rs to split between dialer and listener.
            panic!("The code of the protocol can only generate response messages when we dial \
                    on a connection with the remote");
        },
    }
}
