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
use libp2p_core::{Multiaddr, PeerId, Transport, nodes::protocols_handler::ProtocolsHandler};
use libp2p_core::muxing::StreamMuxer;
use libp2p_core::nodes::{Substream, RawSwarmEvent, SwarmBehaviourEvent, NetworkBehavior, NetworkBehaviorAction};
use std::collections::VecDeque;
use std::io;
use std::marker::PhantomData;
use {PeriodicIdentification, PeriodicIdentificationEvent, IdentifyInfo};

/// Network behaviour that automatically identifies nodes periodically, and returns information
/// about them.
pub struct PeriodicIdentifyBehaviour<TTrans> {
    /// Events that need to be produced outside when polling..
    events: VecDeque<PeriodicIdentifyBehaviourEvent>,
    /// Marker to pin the generics.
    marker: PhantomData<TTrans>,
}

impl<TTrans> PeriodicIdentifyBehaviour<TTrans> {
    /// Creates a `PeriodicIdentifyBehaviour`.
    pub fn new() -> Self {
        PeriodicIdentifyBehaviour {
            events: VecDeque::new(),
            marker: PhantomData,
        }
    }
}

impl<TTrans, TMuxer> NetworkBehavior for PeriodicIdentifyBehaviour<TTrans>
where TTrans: Transport<Output = (PeerId, TMuxer)> + Clone,
      TMuxer: StreamMuxer + Send + Sync + 'static,
      TMuxer::Substream: Send + Sync + 'static,
{
    type ProtocolsHandler = PeriodicIdentification<Substream<TMuxer>>;
    type Transport = TTrans;
    type OutEvent = PeriodicIdentifyBehaviourEvent;

    fn inject_event(
        &mut self,
        event: &SwarmBehaviourEvent<Self::Transport, Self::ProtocolsHandler>,
    ) {
        match event {
            RawSwarmEvent::NodeEvent { peer_id, event: PeriodicIdentificationEvent::Identified { info, observed_addr } } => {
                self.events.push_back(PeriodicIdentifyBehaviourEvent::Identified {
                    peer_id: peer_id.clone(),
                    info: info.clone(),
                    observed_addr: observed_addr.clone(),
                });
            },
            _ => ()
        }
    }

    fn poll(&mut self) -> Poll<Option<NetworkBehaviorAction<<Self::ProtocolsHandler as ProtocolsHandler>::InEvent, Self::OutEvent>>, io::Error> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Async::Ready(Some(NetworkBehaviorAction::GenerateEvent(event))));
        }

        Ok(Async::NotReady)
    }
}

/// Event generated by the `PeriodicIdentifyBehaviour`.
#[derive(Debug, Clone)]
pub enum PeriodicIdentifyBehaviourEvent {
    /// We obtained identification information from the remote
    Identified {
        /// Peer that has been successfully identified.
        peer_id: PeerId,
        /// Information of the remote.
        info: IdentifyInfo,
        /// Address the remote observes us as.
        observed_addr: Multiaddr,
    },
}
