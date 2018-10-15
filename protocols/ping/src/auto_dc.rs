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
use libp2p_core::{PeerId, Transport, nodes::protocols_handler::ProtocolsHandler};
use libp2p_core::muxing::StreamMuxer;
use libp2p_core::nodes::{Substream, RawSwarmEvent, SwarmBehaviourEvent, NetworkBehavior, NetworkBehaviorAction};
use std::collections::VecDeque;
use std::io;
use std::marker::PhantomData;
use std::time::Duration;
use {PeriodicPingHandler, OutEvent};

/// Network behaviour that automatically disconnects nodes if they don't respond to a periodic ping.
pub struct AutoDcBehaviour<TTrans> {
    /// Pending `PingSuccess` events to generate.
    ping_time: VecDeque<(PeerId, Duration)>,
    /// Pending peer IDs to disconnect from the network because they are unresponsive.
    unresponsive: VecDeque<PeerId>,
    /// Marker to pin the generics.
    marker: PhantomData<TTrans>,
}

impl<TTrans> AutoDcBehaviour<TTrans> {
    /// Creates an `AutoDcBehaviour`.
    pub fn new() -> Self {
        AutoDcBehaviour {
            ping_time: VecDeque::with_capacity(2),
            unresponsive: VecDeque::with_capacity(2),
            marker: PhantomData,
        }
    }
}

impl<TTrans, TMuxer> NetworkBehavior for AutoDcBehaviour<TTrans>
where TTrans: Transport<Output = (PeerId, TMuxer)> + Clone,
      TMuxer: StreamMuxer,
{
    type ProtocolsHandler = PeriodicPingHandler<Substream<TMuxer>>;
    type Transport = TTrans;
    type OutEvent = AutoDcBehaviourEvent;

    fn inject_event(
        &mut self,
        event: &SwarmBehaviourEvent<Self::Transport, Self::ProtocolsHandler>,
    ) {
        match event {
            RawSwarmEvent::NodeEvent { peer_id, event: OutEvent::Unresponsive } => {
                self.unresponsive.push_back(peer_id.clone());
            },
            &RawSwarmEvent::NodeEvent { ref peer_id, event: OutEvent::PingSuccess(duration) } => {
                self.ping_time.push_back((peer_id.clone(), duration));
            },
            RawSwarmEvent::NodeEvent { event: OutEvent::PingStart, .. } => (),
            _ => ()
        }
    }

    fn poll(&mut self) -> Poll<Option<NetworkBehaviorAction<<Self::ProtocolsHandler as ProtocolsHandler>::InEvent, Self::OutEvent>>, io::Error> {
        // TODO: merge self.unresponsive and self.ping_time

        if let Some(peer_id) = self.unresponsive.pop_front() {
            let event = AutoDcBehaviourEvent::Unresponsive { peer_id };
            return Ok(Async::Ready(Some(NetworkBehaviorAction::GenerateEvent(event.into()))));
        }

        if let Some((peer_id, ping_time)) = self.ping_time.pop_front() {
            let event = AutoDcBehaviourEvent::PingSuccess { peer_id, ping_time };
            return Ok(Async::Ready(Some(NetworkBehaviorAction::GenerateEvent(event.into()))));
        }

        Ok(Async::NotReady)
    }
}

/// Event generated by the `AutoDcBehaviour`.
#[derive(Debug, Clone)]
pub enum AutoDcBehaviourEvent {
    /// We have successfully pinged the given peer.
    PingSuccess {
        /// Peer that has been successfully pinged.
        peer_id: PeerId,
        /// Duration of the ping.
        ping_time: Duration,
    },

    /// The given node has been determined unresponsive and is shutting down.
    // TODO: is this a good idea?
    Unresponsive {
        /// Peer that is unresponsive.
        peer_id: PeerId,
    },
}
