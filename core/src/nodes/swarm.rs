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
use muxing::StreamMuxer;
use nodes::handled_node::NodeHandler;
use nodes::node::Substream;
use nodes::protocols_handler::{NodeHandlerWrapper, ProtocolsHandler};
use nodes::raw_swarm::{RawSwarm, RawSwarmEvent, ConnectedPoint};
use std::{io, marker::PhantomData};
use {ConnectionUpgrade, Multiaddr, PeerId, Transport};

/// Contains the state of the network, plus the way it should behave.
pub struct Swarm<TBehaviour>
where TBehaviour: NetworkBehavior,
      TBehaviour::ProtocolsHandler: Default,        // TODO: this is a hack because of the builder
{
    raw_swarm: RawSwarm<
        <TBehaviour as NetworkBehavior>::Transport,
        <<TBehaviour as NetworkBehavior>::ProtocolsHandler as ProtocolsHandler>::InEvent,
        <<TBehaviour as NetworkBehavior>::ProtocolsHandler as ProtocolsHandler>::OutEvent,
        NodeHandlerWrapper<TBehaviour::ProtocolsHandler>,
    >,

    behaviour: TBehaviour,
}

impl<TBehaviour, TMuxer> Swarm<TBehaviour>
where TBehaviour: NetworkBehavior,
      TMuxer: StreamMuxer + Send + Sync + 'static,
      <TMuxer as StreamMuxer>::OutboundSubstream: Send + 'static,
      <TMuxer as StreamMuxer>::Substream: Send + 'static,
      TBehaviour::Transport: Transport<Output = (PeerId, TMuxer)> + Clone,
      <TBehaviour::Transport as Transport>::Dial: Send + 'static,
      <TBehaviour::Transport as Transport>::Listener: Send + 'static,
      <TBehaviour::Transport as Transport>::ListenerUpgrade: Send + 'static,
      TBehaviour::ProtocolsHandler: ProtocolsHandler<Substream = Substream<TMuxer>> + Send + 'static + Default,        // TODO: Default is a hack because of the builder
      <TBehaviour::ProtocolsHandler as ProtocolsHandler>::InEvent: Send + 'static,
      <TBehaviour::ProtocolsHandler as ProtocolsHandler>::OutEvent: Send + 'static,
      <TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol: ConnectionUpgrade<Substream<TMuxer>> + Send + 'static,
      <<TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol as ConnectionUpgrade<Substream<TMuxer>>>::Future: Send + 'static,
      <<TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol as ConnectionUpgrade<Substream<TMuxer>>>::NamesIter: Clone + Send + 'static,
      <<TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol as ConnectionUpgrade<Substream<TMuxer>>>::UpgradeIdentifier: Send + 'static,
      <NodeHandlerWrapper<TBehaviour::ProtocolsHandler> as NodeHandler>::OutboundOpenInfo: Send + 'static, // TODO: shouldn't be necessary
{
    /// Builds a new `Swarm`.
    #[inline]
    pub fn new(transport: <TBehaviour as NetworkBehavior>::Transport, behaviour: TBehaviour) -> Self {
        let raw_swarm = RawSwarm::new(transport);
        Swarm {
            raw_swarm,
            behaviour,
        }
    }

    /// Returns the transport passed when building this object.
    #[inline]
    pub fn transport(&self) -> &TBehaviour::Transport {
        self.raw_swarm.transport()
    }

    /// Returns an iterator that produces the list of addresses we're listening on.
    #[inline]
    pub fn listeners(&self) -> impl Iterator<Item = &Multiaddr> {
        RawSwarm::listeners(&self.raw_swarm)
    }

    /// Returns a reference to the network behaviour.
    #[inline]
    pub fn behaviour(&mut self) -> &TBehaviour {
        &self.behaviour
    }

    /// Returns the network behaviour. Allows giving instructions.
    ///
    /// You should call `poll()` on the `Swarm` after modifying the behaviour.
    // TODO: we could add a method on `NetworkBehaviour` that gets fed a `&mut RawSwarm` and
    //       returns an object, and return this object which the user then interacts with ; this
    //       way the behaviour implementation could directly operate on the raw swarm
    #[inline]
    pub fn behaviour_mut(&mut self) -> &mut TBehaviour {
        &mut self.behaviour
    }

    /// Polls the swarm for events. The kind of event depends on the implementation of
    /// `NetworkBehaviour`.
    pub fn poll(&mut self) -> Poll<Option<TBehaviour::OutEvent>, io::Error> {
        loop {
            let mut raw_swarm_not_ready = false;

            match self.raw_swarm.poll() {
                Async::Ready(event) => self.behaviour.inject_event(&event),
                Async::NotReady => raw_swarm_not_ready = true,
            };

            match self.behaviour.poll()? {
                Async::Ready(Some(NetworkBehaviorAction::GenerateEvent(event))) => {
                    return Ok(Async::Ready(Some(event)));
                },
                Async::Ready(Some(NetworkBehaviorAction::DialAddress(addr))) => {
                    // TODO: if the address is not supported, this should produce an error in the
                    //       stream of events ; alternatively, we could mention that the address
                    //       is ignored if not supported in the docs of DialAddress
                    let handler = ProtocolsHandler::into_node_handler(Default::default());
                    let _ = self.raw_swarm.dial(addr, handler);
                },
                Async::Ready(Some(NetworkBehaviorAction::DialPeer(peer_id))) => {
                    // TODO: how to even do that
                    unimplemented!()
                },
                Async::Ready(Some(NetworkBehaviorAction::SendEventOrDial(peer_id, event))) => {
                    if let Some(mut peer) = self.raw_swarm.peer(peer_id).as_connected() {
                        peer.send_event(event);
                    }
                },
                Async::Ready(None) => return Ok(Async::Ready(None)),
                Async::NotReady => {
                    if raw_swarm_not_ready {
                        return Ok(Async::NotReady);
                    }
                }
            }
        }
    }
}

impl<TBehaviour, TMuxer> Stream for Swarm<TBehaviour>
where TBehaviour: NetworkBehavior,
      TMuxer: StreamMuxer + Send + Sync + 'static,
      <TMuxer as StreamMuxer>::OutboundSubstream: Send + 'static,
      <TMuxer as StreamMuxer>::Substream: Send + 'static,
      TBehaviour::Transport: Transport<Output = (PeerId, TMuxer)> + Clone,
      <TBehaviour::Transport as Transport>::Dial: Send + 'static,
      <TBehaviour::Transport as Transport>::Listener: Send + 'static,
      <TBehaviour::Transport as Transport>::ListenerUpgrade: Send + 'static,
      TBehaviour::ProtocolsHandler: ProtocolsHandler<Substream = Substream<TMuxer>> + Send + 'static + Default,        // TODO: Default is a hack because of the builder
      <TBehaviour::ProtocolsHandler as ProtocolsHandler>::InEvent: Send + 'static,
      <TBehaviour::ProtocolsHandler as ProtocolsHandler>::OutEvent: Send + 'static,
      <TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol: ConnectionUpgrade<Substream<TMuxer>> + Send + 'static,
      <<TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol as ConnectionUpgrade<Substream<TMuxer>>>::Future: Send + 'static,
      <<TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol as ConnectionUpgrade<Substream<TMuxer>>>::NamesIter: Clone + Send + 'static,
      <<TBehaviour::ProtocolsHandler as ProtocolsHandler>::Protocol as ConnectionUpgrade<Substream<TMuxer>>>::UpgradeIdentifier: Send + 'static,
      <NodeHandlerWrapper<TBehaviour::ProtocolsHandler> as NodeHandler>::OutboundOpenInfo: Send + 'static, // TODO: shouldn't be necessary
{
    type Item = TBehaviour::OutEvent;
    type Error = io::Error;

    #[inline]
    fn poll(&mut self) -> Poll<Option<TBehaviour::OutEvent>, io::Error> {
        // TODO: remove the code in the inherent `poll()` and directly move it here?
        self.poll()
    }
}

/// How the network behaves. Allows customizing the swarm.
pub trait NetworkBehavior {
    /// Handler for all the protocols the network supports.
    type ProtocolsHandler: ProtocolsHandler;
    /// The transport for this behaviour.
    type Transport: Transport + Clone;

    /// Event generated by the network.
    type OutEvent;

    /// Indicates to the behaviour that an event happened on the raw swarm.
    // TODO: should be a different event type
    fn inject_event(
        &mut self,
        ev: &RawSwarmEvent<Self::Transport, <Self::ProtocolsHandler as ProtocolsHandler>::InEvent, <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent, NodeHandlerWrapper<Self::ProtocolsHandler>>,
    );

    /// Polls for things that swarm should do.
    ///
    /// This API mimics the API of the `Stream` trait.
    // TODO: debate about the `Option`
    fn poll(&mut self) -> Poll<Option<NetworkBehaviorAction<<Self::ProtocolsHandler as ProtocolsHandler>::InEvent, Self::OutEvent>>, io::Error>;
}

/// Action to perform.
#[derive(Debug, Clone)]
pub enum NetworkBehaviorAction<TInEvent, TOutEvent> {
    /// Generate an event for the outside.
    GenerateEvent(TOutEvent),

    /// Instructs the swarm to dial the given multiaddress without any expectation of a peer id.
    DialAddress(Multiaddr),

    /// Instructs the swarm to try reach the given peer.
    DialPeer(PeerId),

    /// If we're connected to the given peer, sends a message to the protocol handler.
    ///
    /// If we're not connected to this peer, attempts to open a connection to it, then sends the
    /// message.
    // TODO: ^ meh ; should somehow ensure delivery or something
    SendEventOrDial(PeerId, TInEvent),
}
