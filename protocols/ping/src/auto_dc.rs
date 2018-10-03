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
use libp2p_core::{ConnectionUpgrade, PeerId, nodes::protocols_handler::ProtocolsHandler};
use libp2p_core::nodes::protocols_handler::{ProtocolsHandlerSelect, Either as ProtoHdlerEither};
use libp2p_core::nodes::raw_swarm::{ConnectedPoint, SwarmEvent};
use libp2p_core::nodes::raw_swarm::{SwarmLayer, PollOutcome};
use libp2p_core::Transport;
use std::collections::VecDeque;
use std::time::Duration;
use tokio_io::{AsyncRead, AsyncWrite};
use {PeriodicPingHandler, OutEvent};

/// Layer that automatically disconnects nodes if they don't respond to a periodic ping.
pub struct AutoDcLayer<TInner> {
    inner: TInner,
    ping_time: VecDeque<(PeerId, Duration)>,
    to_disconnect: VecDeque<PeerId>,
}

impl<TInner> AutoDcLayer<TInner> {
    /// Creates an automatic disconnection layer on top of the given layer.
    pub fn new(inner: TInner) -> Self {
        AutoDcLayer {
            inner: inner,
            ping_time: VecDeque::with_capacity(2),
            to_disconnect: VecDeque::with_capacity(2),
        }
    }
}

impl<TInner, TTrans, TSubstream, TOutEvent> SwarmLayer<TTrans, TOutEvent> for AutoDcLayer<TInner>
where TInner: SwarmLayer<TTrans, TOutEvent>,
      TOutEvent: From<AutoDcLayerEvent>,
      // TODO: too many bounds
      TInner::Handler: ProtocolsHandler<Substream = TSubstream>,
      <TInner::Handler as ProtocolsHandler>::Protocol: ConnectionUpgrade<TSubstream>,
      <<TInner::Handler as ProtocolsHandler>::Protocol as ConnectionUpgrade<TSubstream>>::Future: Send + 'static,
      <<TInner::Handler as ProtocolsHandler>::Protocol as ConnectionUpgrade<TSubstream>>::Output: Send + 'static,
      TTrans: Transport,
      TSubstream: AsyncRead + AsyncWrite + Send + 'static,  // TODO: useless bounds
{
    type Handler = ProtocolsHandlerSelect<PeriodicPingHandler<TSubstream>, TInner::Handler>;
    type NodeHandlerOutEvent = ProtoHdlerEither<OutEvent, TInner::NodeHandlerOutEvent>;

    fn new_handler(&self, connected_point: ConnectedPoint) -> Self::Handler {
        PeriodicPingHandler::new()
            .select(self.inner.new_handler(connected_point))
    }

    fn inject_swarm_event(&mut self, event: SwarmEvent<TTrans, <Self::Handler as ProtocolsHandler>::OutEvent>) {
        let inner_event = event
            .filter_map_out_event(|peer_id, event| {
                match event {
                    ProtoHdlerEither::First(OutEvent::Unresponsive) => {
                        self.to_disconnect.push_back(peer_id.clone());
                        None
                    },
                    ProtoHdlerEither::First(OutEvent::PingStart) => None,
                    ProtoHdlerEither::First(OutEvent::PingSuccess(duration)) => {
                        self.ping_time.push_back((peer_id.clone(), duration));
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
        if let Some(to_disconnect) = self.to_disconnect.pop_front() {
            return Async::Ready(PollOutcome::Disconnect(to_disconnect));
        }

        if let Some((peer_id, ping_time)) = self.ping_time.pop_front() {
            let event = AutoDcLayerEvent::PingSuccess { peer_id, ping_time };
            return Async::Ready(PollOutcome::GenerateEvent(event.into()));
        }

        self.inner
            .poll()
            .map(|ev| ev.map_in_event(|_, ev| ProtoHdlerEither::Second(ev)))
    }
}

/// Event generated by the `AutoDcLayer`.
#[derive(Debug, Clone)]
pub enum AutoDcLayerEvent {
    /// We have successfully pinged the given peer.
    PingSuccess {
        peer_id: PeerId,
        ping_time: Duration,
    },
}
