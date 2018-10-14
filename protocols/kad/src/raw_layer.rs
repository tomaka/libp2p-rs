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
use handler::{KademliaHandler, InEvent, KademliaHandlerEvent};
use libp2p_core::{PeerId, StreamMuxer, Transport};
use libp2p_core::nodes::{ProtocolsHandler, Substream, RawSwarmEvent, NetworkBehavior, NetworkBehaviorAction};
use multihash::Multihash;
use protocol::KadPeer;
use std::collections::VecDeque;
use std::io;
use std::marker::PhantomData;

/// Layer that propagates Kademlia messages to the outside and matches requests with responses.
pub struct KademliaRawBehaviour<TTrans, TUserData> {
    /// Events waiting to be propagated out through polling.
    pending_events: VecDeque<NetworkBehaviorAction<InEvent, KademliaRawBehaviourEvent<TUserData>>>,
    /// Marker to pin the generics.
    marker: PhantomData<(TTrans, TUserData)>,       // TODO: remove TUserData
}

impl<TInner, TUserData> KademliaRawBehaviour<TInner, TUserData> {
    /// Creates a layer that handles Kademlia in the network.
    #[inline]
    pub fn new() -> Self {
        KademliaRawBehaviour {
            pending_events: VecDeque::with_capacity(1),
            marker: PhantomData,
        }
    }

    /// Performs a `FIND_NODE` RPC request to a single node.
    ///
    /// The user data will be returned as part of the response.
    // TODO: what to do if we're not connected?
    pub fn find_node(&mut self, user_data: TUserData, target: &PeerId, searched: &PeerId) {
        
    }
}

impl<TTrans, TUserData> KademliaRawBehaviour<TTrans, TUserData> {
    /// Responds to a `FIND_NODE` request.
    pub fn respond_find_node<TPeers>(&mut self, id: KademliaRequestId, closer_peers: TPeers)
        where TPeers: IntoIterator<Item = KadPeer>
    {
        let list: Vec<KadPeer> = closer_peers.into_iter().collect();
        let ev = InEvent::FindNodeRes { closer_peers: list };
        self.pending_events.push_back(NetworkBehaviorAction::SendEventIfExists(id.target, ev));
    }
}

impl<TTrans, TMuxer, TUserData> NetworkBehavior for KademliaRawBehaviour<TTrans, TUserData>
where TTrans: Transport<Output = (PeerId, TMuxer)> + Clone,
      TMuxer: StreamMuxer + 'static,
{
    type ProtocolsHandler = KademliaHandler<Substream<TMuxer>, TUserData>;
    type Transport = TTrans;
    type OutEvent = KademliaRawBehaviourEvent<TUserData>;

    fn inject_event(&mut self, event: &RawSwarmEvent<Self::Transport, <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent>) {
        match event {
            RawSwarmEvent::NodeEvent { peer_id, event: KademliaHandlerEvent::FindNodeReq { key } } => {
                let ev = KademliaRawBehaviourEvent::FindNodeRequest {
                    peer_id: peer_id.clone(),
                    key: key.clone(),
                    request_identifier: KademliaRequestId {
                        target: peer_id.clone(),
                    },
                };

                self.pending_events.push_back(NetworkBehaviorAction::GenerateEvent(ev));
            },
            RawSwarmEvent::NodeEvent { peer_id, event: KademliaHandlerEvent::FindNodeRes { closer_peers } } => {
                let ev = KademliaRawBehaviourEvent::FindNodeRequest {
                    peer_id: peer_id.clone(),
                    key: key.clone(),
                    request_identifier: KademliaRequestId {
                        target: peer_id.clone(),
                    },
                };

                self.pending_events.push_back(NetworkBehaviorAction::GenerateEvent(ev));
            },
            RawSwarmEvent::NodeEvent { peer_id, event: KademliaHandlerEvent::AddProvider { key, provider_peer } } => {
                let ev = KademliaRawBehaviourEvent::AddProvider {
                    peer_id: peer_id.clone(),
                    key: key.clone(),
                    provider_peer: provider_peer.clone(),
                };

                self.pending_events.push_back(NetworkBehaviorAction::GenerateEvent(ev));
            },
            _ => ()
        }
    }

    fn poll(&mut self) -> Poll<Option<NetworkBehaviorAction<InEvent, Self::OutEvent>>, io::Error> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Async::Ready(Some(event)));
        }

        Ok(Async::NotReady)
    }
}

/// Opaque structure to pass back when answering a Kademlia query.
// Doesn't implement Clone on purpose, so that the user can only answer a request once.
#[derive(Debug)]
pub struct KademliaRequestId {
    /// Node that made the request.
    target: PeerId,
}

/// Event generated by the `KademliaRawBehaviour`.
#[derive(Debug)]
pub enum KademliaRawBehaviourEvent<TUserData> {
    /// Request the multiaddresses that should be attempt to reach a node.
    // TODO: what if already connected? -_-'
    PeersMultiaddressesRequest {
        peer_id: PeerId,
    },

    /// A node performs a FIND_NODE request towards us, and we should answer it.
    FindNodeRequest {
        /// The node that made the request.
        peer_id: PeerId,
        /// The searched key.
        key: PeerId,
        /// Identifier to pass back when answering the query.
        request_identifier: KademliaRequestId,
    },

    /// A node performs a FIND_NODE request towards us, and we should answer it.
    FindNodeResult {
        /// The node that made the request.
        peer_id: PeerId,
        /// The searched key.
        key: PeerId,
        /// Value passed when called `find_node()`.
        user_data: TUserData,
    },

    /*GetProviders {

    },*/

    /// A node registers a provider for the given key.
    AddProvider {
        peer_id: PeerId,
        key: Multihash,
        provider_peer: KadPeer,
    },
}

