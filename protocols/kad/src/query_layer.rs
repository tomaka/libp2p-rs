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

use fnv::FnvHashSet;
use futures::prelude::*;
use handler::{KademliaHandler, InEvent, KademliaHandlerEvent};
use raw_layer::{KademliaRawBehaviour, KademliaRawBehaviourEvent};
use libp2p_core::{PeerId, StreamMuxer, Transport};
use libp2p_core::nodes::{ProtocolsHandler, Substream, RawSwarmEvent, NetworkBehavior, NetworkBehaviorAction};
use multihash::Multihash;
use protocol::KadPeer;
use smallvec::SmallVec;
use std::collections::VecDeque;
use std::io;
use std::marker::PhantomData;

/// Layer that propagates Kademlia messages to the outside.
pub struct KademliaQueryBehaviour<TTrans, TUserData> {
    /// The messages handling behaviour.
    raw_layer: KademliaRawBehaviour<TTrans>,

    /// List of queries currently being performed.
    queries_in_progress: Vec<QueryState<TUserData>>,

    /// Events waiting to be propagated out through polling.
    pending_events: VecDeque<NetworkBehaviorAction<InEvent, KademliaQueryBehaviourEvent<TUserData>>>,
    /// Marker to pin the generics.
    marker: PhantomData<TTrans>,
}

/// State of an individual query.
struct QueryState<TUserData> {
    /// The data passed by the user.
    user_data: TUserData,
    /// At which stage we are.
    stage: Stage,
    /// Final output of the iteration.
    result: Vec<PeerId>,
    /// Nodes that need to be attempted.
    pending_nodes: Vec<PeerId>,
    /// Peers that we tried to contact but failed.
    failed_to_contact: FnvHashSet<PeerId>,
}

/// General stage of the state.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Stage {
    // We are still in the first step of the algorithm where we try to find the closest node.
    FirstStep,
    // We are contacting the k closest nodes in order to fill the list with enough results.
    SecondStep,
    // The results are complete, and the next stream iteration will produce the outcome.
    FinishingNextIter,
    // We are finished and the stream shouldn't return anything anymore.
    Finished,
}

/// Makes progress on a query if possible.
fn advance_query<TUserData>(query: &mut QueryState<TUserData>) {
    /*match query.stage {
        Stage::FinishingNextIter => {
            let result = mem::replace(&mut state.result, Vec::new());
            debug!("Query finished with {} results", result.len());
            query.stage = Stage::Finished;
            let future = future::ok((Some(QueryEvent::Finished(result)), state));
            return Some(future::Either::A(future));
        },
        Stage::Finished => {
            return None;
        },
        _ => ()
    };

    let searched_key = searched_key.clone();
    let find_node_rpc = query_params.find_node.clone();

    // Find out which nodes to contact at this iteration.
    let to_contact = {
        let wanted_len = if state.stage == Stage::FirstStep {
            parallelism.saturating_sub(state.current_attempts_fut.len())
        } else {
            num_results.saturating_sub(state.current_attempts_fut.len())
        };
        let mut to_contact = SmallVec::<[_; 16]>::new();
        while to_contact.len() < wanted_len && !state.pending_nodes.is_empty() {
            // Move the first element of `pending_nodes` to `to_contact`, but ignore nodes that
            // are already part of the results or of a current attempt or if we failed to
            // contact it before.
            let peer = state.pending_nodes.remove(0);
            if state.result.iter().any(|p| p == &peer) {
                continue;
            }
            if state.current_attempts_addrs.iter().any(|p| p == &peer) {
                continue;
            }
            if state.failed_to_contact.iter().any(|p| p == &peer) {
                continue;
            }
            to_contact.push(peer);
        }
        to_contact
    };

    debug!("New query round ; {} queries in progress ; contacting {} new peers",
            state.current_attempts_fut.len(),
            to_contact.len());

    // For each node in `to_contact`, start an RPC query and a corresponding entry in the two
    // `state.current_attempts_*` fields.
    for peer in to_contact {
        let multiaddr: Multiaddr = Protocol::P2p(peer.clone().into_bytes()).into();

        let searched_key2 = searched_key.clone();
        let current_attempt = find_node_rpc(multiaddr.clone(), searched_key2); // TODO: suboptimal
        state.current_attempts_addrs.push(peer.clone());
        state
            .current_attempts_fut
            .push(Box::new(current_attempt) as Box<_>);
    }
    debug_assert_eq!(
        state.current_attempts_addrs.len(),
        state.current_attempts_fut.len()
    );

    // Extract `current_attempts_fut` so that we can pass it to `select_all`. We will push the
    // values back when inside the loop.
    let current_attempts_fut = mem::replace(&mut state.current_attempts_fut, Vec::new());
    if current_attempts_fut.is_empty() {
        // If `current_attempts_fut` is empty, then `select_all` would panic. It happens
        // when we have no additional node to query.
        debug!("Finishing query early because no additional node available");
        state.stage = Stage::FinishingNextIter;
        let future = future::ok((None, state));
        return Some(future::Either::A(future));
    }

    // This is the future that continues or breaks the `loop_fn`.
    let future = future::select_all(current_attempts_fut.into_iter()).then(move |result| {
        let (message, trigger_idx, other_current_attempts) = match result {
            Err((err, trigger_idx, other_current_attempts)) => {
                (Err(err), trigger_idx, other_current_attempts)
            }
            Ok((message, trigger_idx, other_current_attempts)) => {
                (Ok(message), trigger_idx, other_current_attempts)
            }
        };

        // Putting back the extracted elements in `state`.
        let remote_id = state.current_attempts_addrs.remove(trigger_idx);
        debug_assert!(state.current_attempts_fut.is_empty());
        state.current_attempts_fut = other_current_attempts;

        // `message` contains the reason why the current future was woken up.
        let closer_peers = match message {
            Ok(msg) => msg,
            Err(err) => {
                trace!("RPC query failed for {:?}: {:?}", remote_id, err);
                state.failed_to_contact.insert(remote_id);
                return future::ok((None, state));
            }
        };

        // Inserting the node we received a response from into `state.result`.
        // The code is non-trivial because `state.result` is ordered by distance and is limited
        // by `num_results` elements.
        if let Some(insert_pos) = state.result.iter().position(|e| {
            e.distance_with(&searched_key) >= remote_id.distance_with(&searched_key)
        }) {
            if state.result[insert_pos] != remote_id {
                if state.result.len() >= num_results {
                    state.result.pop();
                }
                state.result.insert(insert_pos, remote_id);
            }
        } else if state.result.len() < num_results {
            state.result.push(remote_id);
        }

        // The loop below will set this variable to `true` if we find a new element to put at
        // the top of the result. This would mean that we have to continue looping.
        let mut local_nearest_node_updated = false;

        // Update `state` with the actual content of the message.
        let mut new_known_multiaddrs = Vec::with_capacity(closer_peers.len());
        for mut peer in closer_peers {
            // Update the peerstore with the information sent by
            // the remote.
            {
                let multiaddrs = mem::replace(&mut peer.multiaddrs, Vec::new());
                trace!("Reporting multiaddresses for {:?}: {:?}", peer.node_id, multiaddrs);
                new_known_multiaddrs.push((peer.node_id.clone(), multiaddrs));
            }

            if peer.node_id.distance_with(&searched_key)
                <= state.result[0].distance_with(&searched_key)
            {
                local_nearest_node_updated = true;
            }

            if state.result.iter().any(|ma| ma == &peer.node_id) {
                continue;
            }

            // Insert the node into `pending_nodes` at the right position, or do not
            // insert it if it is already in there.
            if let Some(insert_pos) = state.pending_nodes.iter().position(|e| {
                e.distance_with(&searched_key) >= peer.node_id.distance_with(&searched_key)
            }) {
                if state.pending_nodes[insert_pos] != peer.node_id {
                    state.pending_nodes.insert(insert_pos, peer.node_id.clone());
                }
            } else {
                state.pending_nodes.push(peer.node_id.clone());
            }
        }

        if state.result.len() >= num_results
            || (state.stage != Stage::FirstStep && state.current_attempts_fut.is_empty())
        {
            state.stage = Stage::FinishingNextIter;

        } else {
            if !local_nearest_node_updated {
                trace!("Loop didn't update closer node ; jumping to step 2");
                state.stage = Stage::SecondStep;
            }
        }

        future::ok((Some(QueryEvent::PeersReported(new_known_multiaddrs)), state))
    });

    Some(future::Either::B(future))*/
}

impl<TTrans, TUserData> KademliaQueryBehaviour<TTrans, TUserData> {
    /// Creates a new `KademliaQueryBehaviour`.
    #[inline]
    pub fn new() -> Self {
        KademliaQueryBehaviour {
            raw_layer: KademliaRawBehaviour::new(),
            queries_in_progress: Vec::new(),
            pending_events: VecDeque::with_capacity(1),
            marker: PhantomData,
        }
    }

    /// Starts a `FIND_NODE` query on the network.
    pub fn find_node(&mut self, user_data: TUserData, searched_key: PeerId, num_results: usize) {
        let initial_state = QueryState {
            user_data,
            stage: Stage::FirstStep,
            result: Vec::with_capacity(num_results),
            current_attempts_fut: Vec::new(),
            current_attempts_addrs: SmallVec::new(),
            pending_nodes: {
                let kbuckets_find_closest = query_params.kbuckets_find_closest.clone();
                kbuckets_find_closest(searched_key)
            },
            failed_to_contact: Default::default(),
        };

        self.queries_in_progress.push(initial_state);
    }

    /// Answers a `FIND_NODE` query made by a remote.
    pub fn answer_find_node(&mut self, id: KademliaQueryBehaviourId) {

    }
}

impl<TTrans, TMuxer, TUserData> NetworkBehavior for KademliaQueryBehaviour<TTrans, TUserData>
where TTrans: Transport<Output = (PeerId, TMuxer)> + Clone,
      TMuxer: StreamMuxer + 'static,
{
    type ProtocolsHandler = KademliaHandler<Substream<TMuxer>>;
    type Transport = TTrans;
    type OutEvent = KademliaQueryBehaviourEvent<TUserData>;

    fn inject_event(&mut self, event: &RawSwarmEvent<Self::Transport, <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent>) {
        self.raw_layer.inject_event(event);
    }

    fn poll(&mut self) -> Poll<Option<NetworkBehaviorAction<InEvent, Self::OutEvent>>, io::Error> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Async::Ready(Some(event)));
        }

        loop {
            match self.raw_layer.poll()? {
                /// Event generated by the `KademliaRawBehaviour`.
                #[derive(Debug)]
                pub enum KademliaRawBehaviourEvent {
                    /// A node performs a FIND_NODE request towards us, and we should answer it.
                    FindNodeRequest {
                        /// The node that made the request.
                        peer_id: PeerId,
                        /// The searched key.
                        key: PeerId,
                        /// Identifier to pass back when answering the query.
                        request_identifier: KademliaRequestId,
                    },

                Async::Ready(KademliaRawBehaviourEvent::FindNodeRequest { peer_id, key, request_identifier }) => {
                    return Ok(Async::Ready(KademliaQueryBehaviourEvent::AddProvider { peer_id, key, provider_peer }));
                },
                Async::Ready(KademliaRawBehaviourEvent::AddProvider { peer_id, key, provider_peer }) => {
                    return Ok(Async::Ready(KademliaQueryBehaviourEvent::AddProvider { peer_id, key, provider_peer }));
                },
                Async::Ready(None) => return Ok(Async::Ready(None)),
                Async::NotReady => break,
            }
        }

        for query in self.queries_in_progress.iter_mut() {
            advance_query(query);
        }

        Ok(Async::NotReady)
    }
}

/// Event generated by the `KademliaQueryBehaviour`.
#[derive(Debug)]
pub enum KademliaQueryBehaviourEvent<TUserData> {
    /// The user should ensure that the swarm attempts to connect to the given peer.
    ConnectToRequest {
        /// Peer that we try to reach.
        target: PeerId,
    },

    /// A `ConnectToRequest` event has been previously emitted, but we no longer need connecting.
    ConnectToCancel {
        /// Peer that we no longer need connecting to.
        target: PeerId,
    },

    /// A find node query has finished.
    FindNodeResult {
        /// The searched key.
        key: PeerId,
        /// The peers closest to the searched key, according to the network.
        results: Vec<KadPeer>,
        /// Value that was passed by the userwhen starting a query.
        request_identifier: TUserData,
    },

    /// Request for the list of nodes whose IDs are the closest to `key`. The number of nodes
    /// returned is not specified, but should be around 20.
    FindNodeRequest {
        /// Node which sent the request.
        sender: PeerId,
        /// Identifier of the node.
        key: PeerId,
        /// Identifier of the request. This id has to be passed later when answer the request.
        request_identifier: KademliaQueryBehaviourId,
    },

    /// Same as `FindNodeReq`, but should also return the entries of the local providers list for
    /// this key.
    GetProvidersRequest {
        /// Node which sent the request.
        sender: PeerId,
        /// Identifier being searched.
        key: Multihash,
        /// Identifier of the request. This id has to be passed later when answer the request.
        request_identifier: KademliaQueryBehaviourId,
    },

    /// Indicates that this list of providers is known for this key.
    AddProvider {
        /// Node which sent the request.
        sender: PeerId,
        /// Key for which we should add providers.
        key: Multihash,
        /// Known provider for this key.
        provider_peer: KadPeer,
    },
}

/// Identifier for a request. Must be passed back when answering.
// Note that we purposefully don't implement Clone, so that the user cannot answer the same
// request twice.
#[derive(Debug)]
pub struct KademliaQueryBehaviourId {
    /// Peer which sent the original query.
    query_origin: PeerId,
    // TODO: request type with strong typing, so that there's no mismatch
}
