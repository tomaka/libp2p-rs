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

use either::EitherOutput;
use futures::prelude::*;
use nodes::handled_node::{NodeHandler, NodeHandlerEndpoint, NodeHandlerEvent};
use std::io::Error as IoError;
use tokio_io::{AsyncRead, AsyncWrite};
use upgrade::{choice::OrUpgrade, map::Map as UpgradeMap};
use ConnectionUpgrade;

/// Handler for a protocol.
pub trait ProtocolHandler<TSubstream> {
    /// Custom event that can be received from the outside.
    type InEvent;
    /// Custom event that can be produced by the handler and that will be returned by the swarm.
    type OutEvent;
    /// The protocol or protocols handled by this handler.
    type Protocol: ConnectionUpgrade<TSubstream>;
    /// Information about a substream. Can be sent to the handler through a `NodeHandlerEndpoint`,
    /// and will be passed back in `inject_substream` or `inject_outbound_closed`.
    type OutboundOpenInfo;

    /// Injects a fully-negotiated substream in the handler.
    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>);

    /// Injects an event coming from the outside in the handler.
    fn inject_event(&mut self, event: Self::InEvent);

    /// Indicates the handler that the inbound part of the muxer has been closed, and that
    /// therefore no more inbound substream will be produced.
    fn inject_inbound_closed(&mut self);

    /// Indicates the node that it should shut down. After that, it is expected that `poll()`
    /// returns `Ready(None)` as soon as possible.
    ///
    /// This method allows an implementation to perform a graceful shutdown of the substreams, and
    /// send back various events.
    fn shutdown(&mut self);

    /// Should behave like `Stream::poll()`. Should close if no more event can be produced and the
    /// node should be closed.
    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<Self::OutboundOpenInfo, Self::OutEvent>>, IoError>;

    /*/// Builds a `NodeHandlerWrapper`.
    #[inline]
    fn into_node_handler(self) -> NodeHandlerWrapper<Self>
    where Self: Sized
    {
        NodeHandlerWrapper {
            handler: self,
            negotiating_in: Vec::new(),
            negotiating_out: Vec::new(),
        }
    }*/
}

/*pub struct NodeHandlerWrapper<TProtoHandler> {
    handler: TProtoHandler,
    negotiating_in: Vec<???>,
    negotiating_out: Vec<???>,
}

impl<TProtoHandler> NodeHandler<TProtoHandler> for NodeHandlerWrapper<TProtoHandler>
where TProtoHandler: ProtocolHandler
{
    type InEvent = TProtoHandler::InEvent;
    type OutEvent = TProtoHandler::OutEvent;
    type OutboundOpenInfo = TProtoHandler::OutboundOpenInfo;

    fn inject_substream(&mut self, substream: TSubstream, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
		// For listeners, propose all the possible upgrades.
		if endpoint == NodeHandlerEndpoint::Listener {
			let listener_upgrade = listener_upgrade!(self);
			let upgrade = upgrade::apply(substream, listener_upgrade, Endpoint::Listener, &self.address);
			self.negotiating.push(Box::new(upgrade) as Box<_>);
			// Since we pushed to `upgrades_in_progress_listen`, we have to notify the task.
			if let Some(task) = self.to_notify.take() {
				task.notify();
			}
			return;
		}

		// If we're the dialer, we have to decide which upgrade we want.
		let purpose = if self.queued_dial_upgrades.is_empty() {
			// Since we sometimes remove elements from `queued_dial_upgrades` before they succeed
			// but after the outbound substream has started opening, it is possible that the queue
			// is empty when we receive a substream. This is not an error.
			// Example: we want to open a Kademlia substream, we start opening one, but in the
			// meanwhile the remote opens a Kademlia substream. When we receive the new substream,
			// we don't need it anymore.
			return;
		} else {
			self.queued_dial_upgrades.remove(0)
		};

		match purpose {
			UpgradePurpose::Custom(id) => {
				let wanted = if let Some(proto) = self.registered_custom.find_protocol(id) {
					// TODO: meh for cloning
					upgrade::map(proto.clone(), move |c| FinalUpgrade::Custom(c))
				} else {
					error!(target: "sub-libp2p", "Logic error: wrong custom protocol id for \
						opened substream");
					return;
				};

				// TODO: shouldn't be &self.address ; requires a change in libp2p
				let upgrade = upgrade::apply(substream, wanted, Endpoint::Dialer, &self.address);
				self.upgrades_in_progress_dial.push((purpose, Box::new(upgrade) as Box<_>));
			}
			UpgradePurpose::Kad => {
				let wanted = upgrade::map(KadConnecConfig::new(), move |(c, s)| FinalUpgrade::Kad(c, s));
				// TODO: shouldn't be &self.address ; requires a change in libp2p
				let upgrade = upgrade::apply(substream, wanted, Endpoint::Dialer, &self.address);
				self.upgrades_in_progress_dial.push((purpose, Box::new(upgrade) as Box<_>));
			}
			UpgradePurpose::Identify => {
				let wanted = upgrade::map(identify::IdentifyProtocolConfig, move |i| FinalUpgrade::from(i));
				// TODO: shouldn't be &self.address ; requires a change in libp2p
				let upgrade = upgrade::apply(substream, wanted, Endpoint::Dialer, &self.address);
				self.upgrades_in_progress_dial.push((purpose, Box::new(upgrade) as Box<_>));
			}
			UpgradePurpose::Ping => {
				let wanted = upgrade::map(ping::Ping::default(), move |p| FinalUpgrade::from(p));
				// TODO: shouldn't be &self.address ; requires a change in libp2p
				let upgrade = upgrade::apply(substream, wanted, Endpoint::Dialer, &self.address);
				self.upgrades_in_progress_dial.push((purpose, Box::new(upgrade) as Box<_>));
			}
		};

		// Since we pushed to `upgrades_in_progress_dial`, we have to notify the task.
		if let Some(task) = self.to_notify.take() {
			task.notify();
		}
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
        self.handler.inject_inbound_closed();
    }

    fn inject_outbound_closed(&mut self, user_data: Self::OutboundOpenInfo) {

    }

    #[inline]
    fn inject_event(&mut self, event: Self::InEvent) {
        self.handler.inject_event(event);
    }

    #[inline]
    fn shutdown(&mut self) {
        self.handler.shutdown();
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<Self::OutboundOpenInfo, Self::OutEvent>>, IoError> {
        // Poll the handler first.
        match self.handler.poll()? {
            Async::Ready(event) => return Ok(Async::Ready(event)),
            Async::NotReady => ()
        };

		// Continue negotiation of newly-opened substreams on the listening side.
		// We remove each element from `negotiating_in` one by one and add them back if not ready.
		for n in (0 .. self.negotiating_in.len()).rev() {
			let mut in_progress = self.negotiating_in.swap_remove(n);
			match in_progress.poll() {
				Ok(Async::Ready(upgrade)) => {
					if let Some(event) = self.handler.inject_fully_negotiated(upgrade) {
						return Ok(Async::Ready(Some(event)));
					}
				},
				Ok(Async::NotReady) => {
					self.negotiating_in.push(in_progress);
				},
				Err(err) => {
					return Ok(Async::Ready(Some(SubstrateOutEvent::SubstreamUpgradeFail(err))));
				},
			}
		}

		// Continue negotiation of newly-opened substreams.
		// We remove each element from `negotiating_out` one by one and add them back if not ready.
		for n in (0 .. self.negotiating_out.len()).rev() {
			let (purpose, mut in_progress) = self.negotiating_out.swap_remove(n);
			match in_progress.poll() {
				Ok(Async::Ready(upgrade)) => {
					if let Some(event) = self.inject_fully_negotiated(upgrade) {
						return Ok(Async::Ready(Some(event)));
					}
				},
				Ok(Async::NotReady) => {
					self.negotiating_out.push((purpose, in_progress));
                },
				Err(err) => {
					// TODO: dispatch depending on actual error ; right now we assume that
					// error == not supported, which is not necessarily true in theory
					if let UpgradePurpose::Custom(_) = purpose {
						return Ok(Async::Ready(Some(SubstrateOutEvent::Useless)));
					} else {
						let msg = format!("While upgrading to {:?}: {:?}", purpose, err);
						let err = IoError::new(IoErrorKind::Other, msg);
						return Ok(Async::Ready(Some(SubstrateOutEvent::SubstreamUpgradeFail(err))));
					}
				},
			}
		}
    }
}*/

pub enum Either<A, B> {
    First(A),
    Second(B),
}

#[derive(Debug, Clone)]
pub struct ProtocolHandlerSelect<TProto1, TProto2> {
    proto1: TProto1,
    proto2: TProto2,
}

impl<TSubstream, TProto1, TProto2, TProto1Out, TProto2Out>
    ProtocolHandler<TSubstream> for ProtocolHandlerSelect<TProto1, TProto2>
where TProto1: ProtocolHandler<TSubstream>,
      TProto2: ProtocolHandler<TSubstream>,
      TSubstream: AsyncRead + AsyncWrite,
      TProto1::Protocol: ConnectionUpgrade<TSubstream, Output = TProto1Out>,
      TProto2::Protocol: ConnectionUpgrade<TSubstream, Output = TProto2Out>,
      TProto1Out: Send + 'static,
      TProto2Out: Send + 'static,
      <TProto1::Protocol as ConnectionUpgrade<TSubstream>>::Future: Send + 'static,
      <TProto2::Protocol as ConnectionUpgrade<TSubstream>>::Future: Send + 'static,
{
    type InEvent = Either<TProto1::InEvent, TProto2::InEvent>;
    type OutEvent = Either<TProto1::OutEvent, TProto2::OutEvent>;
    type Protocol = OrUpgrade<UpgradeMap<TProto1::Protocol, fn(TProto1Out) -> EitherOutput<TProto1Out, TProto2Out>>, UpgradeMap<TProto2::Protocol, fn(TProto2Out) -> EitherOutput<TProto1Out, TProto2Out>>>;
    type OutboundOpenInfo = Either<TProto1::OutboundOpenInfo, TProto2::OutboundOpenInfo>;

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        match (protocol, endpoint) {
            (EitherOutput::First(protocol), NodeHandlerEndpoint::Dialer(Either::First(info))) => {
                self.proto1.inject_fully_negotiated(protocol, NodeHandlerEndpoint::Dialer(info));
            },
            (EitherOutput::Second(protocol), NodeHandlerEndpoint::Dialer(Either::Second(info))) => {
                self.proto2.inject_fully_negotiated(protocol, NodeHandlerEndpoint::Dialer(info));
            },
            (EitherOutput::First(_), NodeHandlerEndpoint::Dialer(Either::Second(_))) => {
                panic!()
            },
            (EitherOutput::Second(_), NodeHandlerEndpoint::Dialer(Either::First(_))) => {
                panic!()
            },
            (EitherOutput::First(protocol), NodeHandlerEndpoint::Listener) => {
                self.proto1.inject_fully_negotiated(protocol, NodeHandlerEndpoint::Listener);
            },
            (EitherOutput::Second(protocol), NodeHandlerEndpoint::Listener) => {
                self.proto2.inject_fully_negotiated(protocol, NodeHandlerEndpoint::Listener);
            },
        }
    }

    #[inline]
    fn inject_event(&mut self, event: Self::InEvent) {
        match event {
            Either::First(event) => self.proto1.inject_event(event),
            Either::Second(event) => self.proto2.inject_event(event),
        }
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
        self.proto1.inject_inbound_closed();
        self.proto2.inject_inbound_closed();
    }

    #[inline]
    fn shutdown(&mut self) {
        self.proto1.shutdown();
        self.proto2.shutdown();
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<Self::OutboundOpenInfo, Self::OutEvent>>, IoError> {
        match self.proto1.poll()? {
            Async::Ready(Some(NodeHandlerEvent::Custom(event))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(Either::First(event)))));
            },
            Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest(rq))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest(Either::First(rq)))));
            },
            Async::Ready(None) => return Ok(Async::Ready(None)),
            Async::NotReady => ()
        };

        match self.proto2.poll()? {
            Async::Ready(Some(NodeHandlerEvent::Custom(event))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(Either::Second(event)))));
            },
            Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest(rq))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest(Either::Second(rq)))));
            },
            Async::Ready(None) => return Ok(Async::Ready(None)),
            Async::NotReady => ()
        };

        Ok(Async::NotReady)
    }
}
