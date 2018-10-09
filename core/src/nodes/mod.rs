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

mod handled_node_tasks;

pub mod collection;
pub mod handled_node;
pub mod listeners;
pub mod node;
pub mod protocols_handler;
pub mod raw_swarm;
pub mod swarm;

pub use self::node::Substream;
pub use self::handled_node::{NodeHandlerEvent, NodeHandlerEndpoint};
pub use self::protocols_handler::{NodeHandlerWrapper, ProtocolsHandler, MapOutEvent};
pub use self::raw_swarm::{ConnectedPoint, HandlerFactory, Peer, RawSwarm, RawSwarmEvent};
pub use self::swarm::{NetworkBehavior, NetworkBehaviorAction, Swarm};
