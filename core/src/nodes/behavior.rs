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

// users select the behaviours they want, and "merges" them

// substrate behaviours list:
// - periodic random kademlia queries ; FinalEvent = Kademlia query result
// - periodic ping queries for keep alive ; FinalEvent = ping durations, disconnects
// - pings answering => trivial ; no FinalEvent
// - answer identification requests ; NAT traversal ; FinalEvent = external address
// - periodic identification requests ; FinalEvent = node infos
// - topology (custom for substrate) ; based on top of periodic id request and kademlia and ping answering
// - limit number of incoming connections
// - auto-out-connect on top of topology

// handler combination produces event => dispatch to swarm layer
// then poll layer


pub struct SubstrateTopologyBehaviour {
    periodic_id_req: PeriodicIdRequestBehaviour,
}


pub struct Swarm<TBehaviour> {
    pub fn poll(&mut self) -> TBehaviour::FinalEvent {
        let event: RawSwarmEvent<TBehaviour::ProtocolsHandler::OutEvent> = self.inner.poll()?;
        self.behaviour.layer.inject(&event, swarm.limited_capabilities());
        self.behaviour.poll(&self.behaviour.layer)
    }
}


pub trait NetworkBehavior {
    /// Combination of all the protocol handlers
    type ProtocolsHandler: ProtocolsHandler<OutEvent = Self::IntermediaryEvent>;
    type SwarmLayer: SwarmLayer<NodeOutEvent = Self::IntermediaryEvent>;
    type IntermediaryEvent;

    type FinalEvent;

    fn join<TOther>(self, other: TOther)
    where Self: Sized,
          TOther: NetworkBehavior,
    {}
}

pub trait SwarmLayer {
    type NodeOutEvent;

    fn inject_event(&mut self, ev: Self::NodeOutEvent);

    fn poll(&mut self) -> Poll<> {

    }
}
