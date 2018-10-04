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
use nodes::raw_swarm::{RawSwarm, RawSwarmEvent, ConnectedPoint, HandlerFactory};
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
        LocalBuilder<TBehaviour::ProtocolsHandler>,
    >,

    behaviour: TBehaviour,
}

// TODO: document
pub struct LocalBuilder<TProtocolsHandler>(PhantomData<TProtocolsHandler>);
impl<TProtocolsHandler> HandlerFactory for LocalBuilder<TProtocolsHandler>
where TProtocolsHandler: ProtocolsHandler + Default,
{
    type Handler = NodeHandlerWrapper<TProtocolsHandler>;
    fn new_handler(&self, _: ConnectedPoint) -> Self::Handler {
        TProtocolsHandler::default().into_node_handler()
    }
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
    #[inline]
    pub fn behaviour_mut(&mut self) -> &mut TBehaviour {
        &mut self.behaviour
    }

    /// Polls the swarm for events. The kind of event depends on the implementation of
    /// `NetworkBehaviour`.
    pub fn poll(&mut self) -> Poll<Option<TBehaviour::OutEvent>, io::Error> {
        loop {
            match self.raw_swarm.poll() {
                Async::Ready(Some(event)) => {
                    self.behaviour.inject_event(&event, &mut self.raw_swarm)
                },
                Async::Ready(None) => unreachable!(),       // TODO:
                Async::NotReady => break,
            }
        }

        self.behaviour.poll(&mut self.raw_swarm)
    }
}

// TODO: implement the futures::Swarm trait

/// How the network behaves. Allows customizing the swarm.
pub trait NetworkBehavior {
    /// Handler for all the protocols the network supports.
    type ProtocolsHandler: ProtocolsHandler;
    /// The transport for this behaviour.
    type Transport: Transport + Clone;

    /// Event generated by the network.
    type OutEvent;

    /// Indicates the behaviour that an event happened on the raw swarm.
    ///
    /// The implementation can modify its internal state, and/or perform actions on the underlying
    /// swarm.
    fn inject_event(
        &mut self,
        ev: &RawSwarmEvent<Self::Transport, <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent>,
        raw_swarm: &mut RawSwarm<
            Self::Transport,
            <Self::ProtocolsHandler as ProtocolsHandler>::InEvent,
            <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
            LocalBuilder<Self::ProtocolsHandler>
        >,
    );

    /// Polls for final events.
    fn poll(
        &mut self,
        raw_swarm: &mut RawSwarm<
            Self::Transport,
            <Self::ProtocolsHandler as ProtocolsHandler>::InEvent,
            <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
            LocalBuilder<Self::ProtocolsHandler>
        >
    ) -> Poll<Option<Self::OutEvent>, io::Error>;
}
