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
use handler::{KademliaHandler, OutEvent};
use libp2p_core::{ConnectionUpgrade, PeerId, nodes::protocols_handler::ProtocolsHandler};
use libp2p_core::nodes::protocols_handler::{ProtocolsHandlerSelect, Either as ProtoHdlerEither};
use libp2p_core::nodes::raw_swarm::{ConnectedPoint, RawSwarmEvent};
use libp2p_core::nodes::raw_swarm::{SwarmLayer, PollOutcome};
use libp2p_core::Transport;
use multihash::Multihash;
use protocol::KadPeer;
use std::collections::VecDeque;
use tokio_io::{AsyncRead, AsyncWrite};

/// Layer that automatically handles Kademlia.
pub struct KademliaLayer<TInner> {
    inner: TInner,
    pending_events: VecDeque<KademliaLayerEvent>,
}

impl<TInner> KademliaLayer<TInner> {
    /// Creates a layer that handles Kademlia in the network.
    #[inline]
    pub fn new(inner: TInner) -> Self {
        KademliaLayer {
            inner,
            pending_events: VecDeque::new(),
        }
    }
}

impl<TInner, TTrans, TSubstream, TOutEvent> SwarmLayer<TTrans, TOutEvent> for KademliaLayer<TInner>
where TInner: SwarmLayer<TTrans, TOutEvent>,
      TOutEvent: From<KademliaLayerEvent>,
      // TODO: too many bounds
      TInner::Handler: ProtocolsHandler<Substream = TSubstream>,
      <TInner::Handler as ProtocolsHandler>::Protocol: ConnectionUpgrade<TSubstream>,
      <<TInner::Handler as ProtocolsHandler>::Protocol as ConnectionUpgrade<TSubstream>>::Future: Send + 'static,
      <<TInner::Handler as ProtocolsHandler>::Protocol as ConnectionUpgrade<TSubstream>>::Output: Send + 'static,
      TTrans: Transport,
      TSubstream: AsyncRead + AsyncWrite + Send + 'static,
{
    type Handler = ProtocolsHandlerSelect<KademliaHandler<TSubstream>, TInner::Handler>;
    type NodeHandlerOutEvent = ProtoHdlerEither<OutEvent, TInner::NodeHandlerOutEvent>;

    fn new_handler(&self, connected_point: ConnectedPoint) -> Self::Handler {
        KademliaHandler::new().select(self.inner.new_handler(connected_point))
    }

    fn inject_swarm_event(&mut self, event: RawSwarmEvent<TTrans, <Self::Handler as ProtocolsHandler>::OutEvent>) {
        let inner_event = event
            .filter_map_out_event(|peer_id, event| {
                match event {
                    ProtoHdlerEither::First(OutEvent::FindNodeReq { key }) => {
                        let ev = KademliaLayerEvent::FindNodeRequest {
                            peer_id: peer_id.clone(),
                            key,
                            request_identifier: KademliaRequestId {},
                        };

                        self.pending_events.push_back(ev);
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::AddProvider { key, provider_peer }) => {
                        let ev = KademliaLayerEvent::AddProvider {
                            peer_id: peer_id.clone(),
                            key,
                            provider_peer,
                        };

                        self.pending_events.push_back(ev);
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::FindNodeRes { .. }) => {
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::GetProvidersReq { .. }) => {
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::GetProvidersRes { .. }) => {
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::Open) => {
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::IgnoredIncoming) => {
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::Closed(_)) => {
                        None
                    },
                    ProtoHdlerEither::Second(ev) => Some(ev),
                }
            });

        if let Some(inner_event) = inner_event {
            self.inner.inject_swarm_event(inner_event);
        }
    }

    fn poll(&mut self) -> Async<PollOutcome<<Self::Handler as ProtocolsHandler>::InEvent, TOutEvent>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Async::Ready(PollOutcome::GenerateEvent(event.into()));
        }

        self.inner.poll()
            .map(|ev| ev.map_in_event(|_, ev| ProtoHdlerEither::Second(ev)))
    }
}

/// Opaque structure to pass back when answering a Kademlia query.
// Doesn't implement Clone on purpose, so that the user can only answer a request once.
#[derive(Debug)]
pub struct KademliaRequestId {

}

/// Event generated by the `KademliaLayer`.
#[derive(Debug)]
pub enum KademliaLayerEvent {
    /// A node performs a FIND_NODE request towards us, and we should answer it.
    FindNodeRequest {
        /// The node that made the request.
        peer_id: PeerId,
        /// The searched key.
        key: PeerId,
        /// Identifier to pass back when answering the query.
        request_identifier: KademliaRequestId,
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

