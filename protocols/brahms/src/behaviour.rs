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

use crate::handler::{BrahmsHandler, BrahmsHandlerEvent, BrahmsHandlerIn};
use crate::sampler::Sampler;
use crate::topology::BrahmsTopology;
use fnv::FnvHashSet;
use futures::prelude::*;
use libp2p_core::swarm::{
    ConnectedPoint, NetworkBehaviour, NetworkBehaviourAction, PollParameters,
};
use libp2p_core::{Multiaddr, PeerId};
use rand::distributions::{Distribution, Range};
use smallvec::SmallVec;
use std::{cmp, iter, marker::PhantomData, mem, time::Duration, time::Instant};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_timer::Interval;

/// Configuration for the Brahms discovery mechanism.
#[derive(Debug, Copy, Clone)]
pub struct BrahmsConfig {
    /// Configuration for the size of the view.
    pub view_size: BrahmsViewSize,

    /// Duration of a round. Should be at least twice the maximum expected latency between two
    /// nodes, plus the time to generate `alpha` proofs of work.
    pub round_duration: Duration,

    /// Number of samplers. The more the better, but the more CPU-intensive the algorithm is.
    pub num_samplers: u32,

    /// Number of leading zero bytes that are required in the proof-of-work calculation used to
    /// prevent DDoS attacks. The higher the value, the more resistant we are, but the more
    /// CPU-intensive the code is.
    ///
    /// The correct value to use here depends on the round duration, the size of the view, and the
    /// speed of CPUs. We need to generate `alpha` proofs in less than `round_duration - latency`.
    ///
    /// As an example, on a modern machine, with a value of 16 it takes around 1ms to create a
    /// proof.
    pub difficulty: u8,
}

/// Configuration for the view size of the Brahms discovery mechanism.
///
/// The maximum number of elements in the view will be `alpha + beta + gamma`.
///
/// Note that if a node receives receives more than `alpha` pushes, it will assume that the network
/// is under attack. It is therefore important that the configuration is the same throughout the
/// network, most notably the value of `alpha`.
#[derive(Debug, Copy, Clone)]
pub struct BrahmsViewSize {
    /// Number of elements in the view that are the result of pushes. Must not be 0. Typically
    /// 45% of the size of the view.
    pub alpha: u32,

    /// Number of elements in the view that are the result of pulls. Must not be 0. Typically
    /// 45% of the size of the view.
    pub beta: u32,

    /// Number of elements in the view that are the result of sampling. Typically 10% of the size
    /// of the view.
    pub gamma: u32,
}

impl Default for BrahmsConfig {
    #[inline]
    fn default() -> Self {
        BrahmsConfig {
            view_size: BrahmsViewSize {
                alpha: 14,
                beta: 14,
                gamma: 4,
            },
            round_duration: Duration::from_secs(10),
            num_samplers: 32,
            difficulty: 10,
        }
    }
}

/// Brahms discovery algorithm behaviour.
pub struct Brahms<TSubstream> {
    /// The way the algorithm is configured.
    config: BrahmsConfig,
    /// Configuration to use for the next round. Copied into `config` at the beginning of each
    /// round.
    next_round_view_size: BrahmsViewSize,
    /// PeerId of the local node.
    // TODO: field can be removed after a rework of NetworkBehaviour
    local_peer_id: PeerId,

    /// List of elements to add to the topology as soon as we have access to it.
    add_to_topology: SmallVec<[(PeerId, Multiaddr); 32]>,

    /// The view contains our local view of the whole network. This is public.
    view: FnvHashSet<PeerId>,

    /// Contains the peers we know about.
    sampler: Sampler<PeerIdAdapter>,

    /// Interval to advance round.
    round_advance: Interval,

    /// List of values that other nodes have spontaneously pushed to us.
    push_list: FnvHashSet<PeerId>,
    /// List of values that we pulled from other nodes.
    pull_list: FnvHashSet<PeerId>,

    /// List of all nodes we're connected to.
    connected_peers: FnvHashSet<PeerId>,
    /// Nodes we want to connect to because we want to put them in `pending_pushes`.
    push_pending_connects: SmallVec<[PeerId; 32]>,
    /// Nodes we want to connect to because we want to put them in `pending_pulls`.
    pull_pending_connects: SmallVec<[PeerId; 32]>,
    /// List of peers we want to send a push to.
    pending_pushes: SmallVec<[PeerId; 32]>,
    /// List of peers we want to send a pull request to.
    pending_pulls: SmallVec<[PeerId; 32]>,
    /// List of pull requests we received and that need to be answered.
    pull_requests_to_respond: SmallVec<[PeerId; 32]>,

    /// Marker to pin the generic.
    marker: PhantomData<TSubstream>,
}

/// Event that happened in the Brahms behaviour.
#[derive(Debug, Clone)]
pub enum BrahmsEvent {
    /// The view has been modified.
    ViewChanged,
}

/// Wraps around a `PeerId` so that it implements `AsRef<[u8]>`.
#[derive(Debug, Clone)]
struct PeerIdAdapter(PeerId);

impl AsRef<[u8]> for PeerIdAdapter {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl<TSubstream> Brahms<TSubstream> {
    /// Initializes the Brahms state.
    pub fn new(config: BrahmsConfig, local_peer_id: PeerId) -> Self {
        let sampler = Sampler::with_len(config.num_samplers);
        let round_advance = Interval::new(Instant::now(), config.round_duration);

        Brahms {
            config,
            next_round_view_size: config.view_size,
            local_peer_id,
            add_to_topology: SmallVec::new(),
            sampler,
            round_advance,
            // We set the capacity right before filling the view.
            view: Default::default(),
            push_list: FnvHashSet::with_capacity_and_hasher(
                config.view_size.alpha as usize,
                Default::default(),
            ),
            pull_list: FnvHashSet::default(),
            connected_peers: FnvHashSet::default(),
            push_pending_connects: SmallVec::new(),
            pull_pending_connects: SmallVec::new(),
            pending_pushes: SmallVec::new(),
            pending_pulls: SmallVec::new(),
            pull_requests_to_respond: SmallVec::new(),
            marker: PhantomData,
        }
    }

    /// Modifies the configuration of Brahms. Only starts applying after the next round.
    pub fn set_config(&mut self, config: BrahmsViewSize) {
        self.next_round_view_size = config;

        if self.push_list.len() < config.alpha as usize {
            self.push_list.reserve(config.alpha as usize - self.push_list.len());
        }
    }

    /// Returns the local view of the network.
    ///
    /// Contains a maximum of `alpha + beta + gamma` elements (where `alpha`, `beta` and `gamma`
    /// come from the configuration). Never contains any duplicate.
    ///
    /// Assuming that the whole network uses Brahms, there should be a direct or indirect link
    /// between every single node of the network and at least one of the nodes returned by
    /// `view()`.
    ///
    /// The list of nodes that are returned can change after you call `poll()`.
    #[inline]
    pub fn view(&self) -> impl Iterator<Item = &PeerId> {
        self.view.iter()
    }

    /// Internal function. Called at a regular interval. Updates the view with the previous round's
    /// responses and advances to the next round.
    fn advance_round<TTopology>(&mut self, parameters: &mut PollParameters<TTopology>)
    where
        TTopology: BrahmsTopology,
    {
        // Update the view if necessary.
        if !self.push_list.is_empty() &&
            // !self.pull_list.is_empty() &&        // TODO: figure out bootstrapping
            self.push_list.len() <= self.config.view_size.alpha as usize
        {
            // Clear `self.view` and put the former view in `old_view`.
            let old_view = {
                // We don't just copy the capacity of `self.view` because the view size can be
                // changed by the user.
                let view_size = self.config.view_size.alpha + self.config.view_size.beta + self.config.view_size.gamma;
                mem::replace(&mut self.view, FnvHashSet::with_capacity_and_hasher(view_size as usize, Default::default()))
            };

            // Move elements from `push_list` to `self.view`.
            let mut push_to_view = FnvHashSet::default();
            let push_range = Range::new(0, self.push_list.len());
            let desired_push_to_view_len =
                cmp::min(self.config.view_size.alpha as usize, self.push_list.len());
            while push_to_view.len() < desired_push_to_view_len {
                let index = push_range.sample(&mut rand::thread_rng());
                let elem = self
                    .push_list
                    .iter()
                    .nth(index)
                    .expect("The index is always valid; QED");
                push_to_view.insert(elem.clone());
            }
            for elem in push_to_view.drain() {
                // It is possible that a node incorrectly pushes our own identity to us.
                if &elem != parameters.local_peer_id() {
                    self.view.insert(elem.clone());
                }
            }

            // Move elements from `pull_list` to `self.view`.
            if !self.pull_list.is_empty() {
                let mut pull_to_view = FnvHashSet::default();
                let pull_range = Range::new(0, self.pull_list.len());
                let desired_pull_to_view_len =
                    cmp::min(self.config.view_size.beta as usize, self.pull_list.len());
                while pull_to_view.len() < desired_pull_to_view_len {
                    let index = pull_range.sample(&mut rand::thread_rng());
                    let elem = self
                        .pull_list
                        .iter()
                        .nth(index)
                        .expect("The index is always valid; QED");
                    pull_to_view.insert(elem.clone());
                }
                for elem in pull_to_view.drain() {
                    if &elem != parameters.local_peer_id() {
                        self.view.insert(elem.clone());
                    }
                }
            }

            // Move elements from the sampler.
            for _ in 0..self.config.view_size.gamma {
                if let Some(elem) = self.sampler.sample() {
                    self.view.insert(elem.0.clone());
                }
            }

            // TODO: for each element in `self.view` not in `old_view`, we send a "keep-alive enable"
            // message.
            for peer in self.view.iter() {
                if !old_view.contains(peer) {

                }
            }
        }

        // Reset for next round.
        self.push_list.clear();
        self.pull_list.clear();
        self.config.view_size = self.next_round_view_size;

        // Since we can't do anything if the view is empty, set it to something.
        // Normally this should only ever happen once at initialization, but if `initial_view()`
        // returns nothing then the view will remain empty and we will call this again every round
        // until something is returned.
        if self.view.is_empty() {
            let max = (self.config.view_size.alpha + self.config.view_size.beta + self.config.view_size.gamma) as usize;
            self.view = parameters.topology().initial_view(max).take(max).collect();
        }

        // Update the samplers.
        for elem in self.view.iter() {
            self.sampler.insert(PeerIdAdapter(elem.clone()));
        }

        if !self.view.is_empty() {
            let range = Range::new(0, self.view.len());

            // Send push requests.
            let wanted_push_len = cmp::min(self.config.view_size.alpha as usize, self.view.len());
            while self.pending_pushes.len() + self.push_pending_connects.len() < wanted_push_len {
                let index = range.sample(&mut rand::thread_rng());
                let elem = self
                    .view
                    .iter()
                    .nth(index)
                    .expect("The index is always valid; QED");
                if self.connected_peers.contains(elem) {
                    if !self.pending_pushes.contains(elem) {
                        self.pending_pushes.push(elem.clone());
                    }
                } else if !self.push_pending_connects.contains(elem) {
                    self.push_pending_connects.push(elem.clone());
                }
            }

            // Send pull requests.
            let wanted_pull_len = cmp::min(self.config.view_size.beta as usize, self.view.len());
            while self.pending_pulls.len() + self.pull_pending_connects.len() < wanted_pull_len {
                let index = range.sample(&mut rand::thread_rng());
                let elem = self
                    .view
                    .iter()
                    .nth(index)
                    .expect("The index is always valid; QED");
                if self.connected_peers.contains(elem) {
                    if !self.pending_pulls.contains(elem) {
                        self.pending_pulls.push(elem.clone());
                    }
                } else if !self.pull_pending_connects.contains(elem) {
                    self.pull_pending_connects.push(elem.clone());
                }
            }
        }
    }
}

impl<TSubstream, TTopology> NetworkBehaviour<TTopology> for Brahms<TSubstream>
where
    TSubstream: AsyncRead + AsyncWrite,
    TTopology: BrahmsTopology,
{
    type ProtocolsHandler = BrahmsHandler<TSubstream>;
    type OutEvent = BrahmsEvent;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        BrahmsHandler::new(self.local_peer_id.clone(), self.config.difficulty)
    }

    fn inject_connected(&mut self, peer_id: PeerId, _: ConnectedPoint) {
        let wasnt_in = self.connected_peers.insert(peer_id);
        debug_assert!(wasnt_in);
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId, _: ConnectedPoint) {
        let was_in = self.connected_peers.remove(peer_id);
        debug_assert!(was_in);
    }

    fn inject_node_event(&mut self, peer_id: PeerId, event: BrahmsHandlerEvent) {
        match event {
            BrahmsHandlerEvent::Push { addresses, .. } => {
                for addr in addresses {
                    self.add_to_topology.push((peer_id.clone(), addr));
                }
                self.push_list.insert(peer_id);
            }
            BrahmsHandlerEvent::PullRequest => {
                self.pull_requests_to_respond.push(peer_id);
            }
            BrahmsHandlerEvent::PullResult { list } => {
                for (peer, addresses) in list {
                    for addr in addresses {
                        self.add_to_topology.push((peer.clone(), addr));
                    }
                    self.pull_list.insert(peer);
                }
            }
        }
    }

    fn poll(
        &mut self,
        parameters: &mut PollParameters<TTopology>,
    ) -> Async<NetworkBehaviourAction<BrahmsHandlerIn, Self::OutEvent>> {
        // TODO: we need to send BrahmsHandlerIn::Enable/DisableKeepAlive

        for (peer_id, addr) in self.add_to_topology.drain() {
            if &peer_id == parameters.local_peer_id() {
                continue;
            }

            parameters
                .topology()
                .add_brahms_discovered_address(peer_id, addr);
        }

        if !self.pull_requests_to_respond.is_empty() {
            let peer_id = self.pull_requests_to_respond.remove(0);
            let external_addresses = parameters.external_addresses().collect::<Vec<_>>();
            let local_addresses = parameters
                .listened_addresses()
                .cloned()
                .chain(external_addresses.into_iter())
                .collect();
            let local_peer_id = parameters.local_peer_id().clone();
            let response = self
                .view
                .iter()
                .map(|p| (p.clone(), parameters.topology().addresses_of_peer(p)))
                .chain(iter::once((local_peer_id, local_addresses)))
                .collect();
            return Async::Ready(
                NetworkBehaviourAction::SendEvent {
                    peer_id,
                    event: BrahmsHandlerIn::Event(BrahmsHandlerEvent::PullResult { list: response }),
                },
            );
        }

        if !self.push_pending_connects.is_empty() {
            let peer_id = self.push_pending_connects.remove(0);
            self.pending_pushes.push(peer_id.clone());
            return Async::Ready(NetworkBehaviourAction::DialPeer { peer_id });
        }

        if !self.pull_pending_connects.is_empty() {
            let peer_id = self.pull_pending_connects.remove(0);
            self.pending_pulls.push(peer_id.clone());
            return Async::Ready(NetworkBehaviourAction::DialPeer { peer_id });
        }

        if !self.pending_pushes.is_empty() {
            let peer_id = self.pending_pushes.remove(0);
            let external_addresses = parameters.external_addresses().collect::<Vec<_>>();
            return Async::Ready(NetworkBehaviourAction::SendEvent {
                peer_id: peer_id.clone(),
                event: BrahmsHandlerIn::Event(BrahmsHandlerEvent::Push {
                    addresses: parameters
                        .listened_addresses()
                        .cloned()
                        .chain(external_addresses)
                        .collect(),
                    local_peer_id: parameters.local_peer_id().clone(),
                    remote_peer_id: peer_id.clone(),
                    pow_difficulty: self.config.difficulty,
                }),
            });
        }

        if !self.pending_pulls.is_empty() {
            let peer_id = self.pending_pulls.remove(0);
            return Async::Ready(NetworkBehaviourAction::SendEvent {
                peer_id,
                event: BrahmsHandlerIn::Event(BrahmsHandlerEvent::PullRequest),
            });
        }

        match self.round_advance.poll() {
            Ok(Async::NotReady) => (),
            Ok(Async::Ready(Some(_))) => {
                self.advance_round(parameters);
                return Async::Ready(NetworkBehaviourAction::GenerateEvent(
                    BrahmsEvent::ViewChanged,
                ));
            }
            Ok(Async::Ready(None)) | Err(_) => (),
        }

        Async::NotReady
    }
}
