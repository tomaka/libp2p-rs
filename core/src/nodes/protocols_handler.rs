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
use std::io::{Error as IoError, ErrorKind as IoErrorKind};
use std::marker::PhantomData;
use tokio_io::{AsyncRead, AsyncWrite};
use upgrade::{self, apply::UpgradeApplyFuture, choice::OrUpgrade, map::Map as UpgradeMap};
use upgrade::{DeniedConnectionUpgrade, toggleable::Toggleable};
use void::Void;
use {ConnectionUpgrade, Endpoint};

/// Handler for a protocol.
// TODO: add upgrade timeout system
// TODO: add a "blocks connection closing" system, so that we can gracefully close a connection
//       when it's no longer needed, and so that for example the periodic pinging system does not
//       keep the connection alive forever
pub trait ProtocolsHandler {
    /// Custom event that can be received from the outside.
    type InEvent;
    /// Custom event that can be produced by the handler and that will be returned by the swarm.
    type OutEvent;
    /// The substream for raw data.
    type Substream: AsyncRead + AsyncWrite;
    /// The protocol or protocols handled by this handler.
    type Protocol: ConnectionUpgrade<Self::Substream>;
    /// Information about a substream. Can be sent to the handler through a `NodeHandlerEndpoint`,
    /// and will be passed back in `inject_substream` or `inject_outbound_closed`.
    type OutboundOpenInfo;

    /// Produces a `ConnectionUpgrade` for this protocol.
    fn listen_protocol(&self) -> Self::Protocol;

    /// Injects a fully-negotiated substream in the handler.
    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<Self::Substream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>);

    /// Injects an event coming from the outside in the handler.
    fn inject_event(&mut self, event: Self::InEvent);

    /// Indicates to the handler that upgrading a substream to the given protocol has failed.
    fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: &IoError);

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
    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, IoError>;

    /// Adds a closure that turns the output event into something else.
    #[inline]
    fn map_out_event<TMap, TNewOut>(self, map: TMap) -> MapOutEvent<Self, TMap>
    where Self: Sized,
          TMap: FnMut(Self::OutEvent) -> TNewOut
    {
        MapOutEvent {
            inner: self,
            map,
        }
    }

    /// Builds an implementation of `ProtocolsHandler` that combines this handler with another one.
    #[inline]
    fn select<TOther>(self, other: TOther) -> ProtocolsHandlerSelect<Self, TOther>
    where Self: Sized
    {
        ProtocolsHandlerSelect {
            proto1: self,
            proto2: other,
        }
    }

    /// Builds a `NodeHandlerWrapper`.
    #[inline]
    fn into_node_handler(self) -> NodeHandlerWrapper<Self>
    where Self: Sized
    {
        NodeHandlerWrapper {
            handler: self,
            negotiating_in: Vec::new(),
            negotiating_out: Vec::new(),
            queued_dial_upgrades: Vec::new(),
        }
    }
}

/// Implementation of `ProtocolsHandler` that doesn't handle anything.
pub struct DummyProtocolsHandler<TSubstream> {
    shutting_down: bool,
    marker: PhantomData<TSubstream>,
}

impl<TSubstream> Default for DummyProtocolsHandler<TSubstream> {
    #[inline]
    fn default() -> Self {
        DummyProtocolsHandler {
            shutting_down: false,
            marker: PhantomData,
        }
    }
}

impl<TSubstream> ProtocolsHandler for DummyProtocolsHandler<TSubstream>
where TSubstream: AsyncRead + AsyncWrite
{
    type InEvent = Void;
    type OutEvent = Void;
    type Substream = TSubstream;
    type Protocol = DeniedConnectionUpgrade;
    type OutboundOpenInfo = Void;

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        DeniedConnectionUpgrade
    }

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
    }

    fn inject_event(&mut self, event: Self::InEvent) {
    }

    fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: &IoError) {
    }

    fn inject_inbound_closed(&mut self) {
    }

    fn shutdown(&mut self) {
        self.shutting_down = true;
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, IoError> {
        if self.shutting_down {
            Ok(Async::Ready(None))
        } else {
            Ok(Async::NotReady)
        }
    }
}

/// Wrapper around a protocol handler that turns the output event into something else.
pub struct MapOutEvent<TProtoHandler, TMap> {
    inner: TProtoHandler,
    map: TMap,
}

impl<TProtoHandler, TMap, TNewOut> ProtocolsHandler for MapOutEvent<TProtoHandler, TMap>
where TProtoHandler: ProtocolsHandler,
      TMap: FnMut(TProtoHandler::OutEvent) -> TNewOut
{
    type InEvent = TProtoHandler::InEvent;
    type OutEvent = TNewOut;
    type Substream = TProtoHandler::Substream;
    type Protocol = TProtoHandler::Protocol;
    type OutboundOpenInfo = TProtoHandler::OutboundOpenInfo;

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        self.inner.listen_protocol()
    }

    #[inline]
    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<Self::Substream>>::Output, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        self.inner.inject_fully_negotiated(protocol, endpoint)
    }

    #[inline]
    fn inject_event(&mut self, event: Self::InEvent) {
        self.inner.inject_event(event)
    }

    #[inline]
    fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: &IoError) {
        self.inner.inject_dial_upgrade_error(info, error)
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
        self.inner.inject_inbound_closed()
    }

    #[inline]
    fn shutdown(&mut self) {
        self.inner.shutdown()
    }

    #[inline]
    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, IoError> {
        Ok(self.inner.poll()?.map(|ev| {
            ev.map(|ev| {
                match ev {
                    NodeHandlerEvent::Custom(ev) => NodeHandlerEvent::Custom((self.map)(ev)),
                    NodeHandlerEvent::OutboundSubstreamRequest(info) => {
                        NodeHandlerEvent::OutboundSubstreamRequest(info)
                    },
                }
            })
        }))
    }
}

/// Wraps around an implementation of `ProtocolsHandler`, and implements `NodeHandler`.
pub struct NodeHandlerWrapper<TProtoHandler>
where TProtoHandler: ProtocolsHandler
{
    handler: TProtoHandler,
    negotiating_in: Vec<UpgradeApplyFuture<TProtoHandler::Substream, TProtoHandler::Protocol>>,
    negotiating_out: Vec<(TProtoHandler::OutboundOpenInfo, UpgradeApplyFuture<TProtoHandler::Substream, TProtoHandler::Protocol>)>,
    // For each outbound substream request, how to upgrade it.
    queued_dial_upgrades: Vec<TProtoHandler::Protocol>,
}

impl<TProtoHandler> NodeHandler for NodeHandlerWrapper<TProtoHandler>
where TProtoHandler: ProtocolsHandler,
      <TProtoHandler::Protocol as ConnectionUpgrade<TProtoHandler::Substream>>::NamesIter: Clone,
{
    type InEvent = TProtoHandler::InEvent;
    type OutEvent = TProtoHandler::OutEvent;
    type Substream = TProtoHandler::Substream;
    type OutboundOpenInfo = TProtoHandler::OutboundOpenInfo;

    fn inject_substream(&mut self, substream: Self::Substream, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        match endpoint {
            NodeHandlerEndpoint::Listener => {
                let protocol = self.handler.listen_protocol();
                let upgrade = upgrade::apply(substream, protocol, Endpoint::Listener);
                self.negotiating_in.push(upgrade);
            },
            NodeHandlerEndpoint::Dialer(upgr_info) => {
                // If we're the dialer, we have to decide which upgrade we want.
                let proto_upgrade = if self.queued_dial_upgrades.is_empty() {
                    // TODO: is that an error?
                    return;
                } else {
                    self.queued_dial_upgrades.remove(0)
                };

                let upgrade = upgrade::apply(substream, proto_upgrade, Endpoint::Dialer);
                self.negotiating_out.push((upgr_info, upgrade));
            },
        }
    }

    #[inline]
    fn inject_inbound_closed(&mut self) {
        self.handler.inject_inbound_closed();
    }

    fn inject_outbound_closed(&mut self, user_data: Self::OutboundOpenInfo) {
        unimplemented!()        // FIXME:
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
        // Continue negotiation of newly-opened substreams on the listening side.
        // We remove each element from `negotiating_in` one by one and add them back if not ready.
        for n in (0 .. self.negotiating_in.len()).rev() {
            let mut in_progress = self.negotiating_in.swap_remove(n);
            match in_progress.poll() {
                Ok(Async::Ready(upgrade)) => {
                    self.handler.inject_fully_negotiated(upgrade, NodeHandlerEndpoint::Listener);
                },
                Ok(Async::NotReady) => {
                    self.negotiating_in.push(in_progress);
                },
                Err(err) => {
                    // TODO: what to do?
                },
            }
        }

        // Continue negotiation of newly-opened substreams.
        // We remove each element from `negotiating_out` one by one and add them back if not ready.
        for n in (0 .. self.negotiating_out.len()).rev() {
            let (upgr_info, mut in_progress) = self.negotiating_out.swap_remove(n);
            match in_progress.poll() {
                Ok(Async::Ready(upgrade)) => {
                    let endpoint = NodeHandlerEndpoint::Dialer(upgr_info);
                    self.handler.inject_fully_negotiated(upgrade, endpoint);
                },
                Ok(Async::NotReady) => {
                    self.negotiating_out.push((upgr_info, in_progress));
                },
                Err(err) => {
                    // TODO: dispatch depending on actual error ; right now we assume that
                    // error == not supported, which is not necessarily true in theory
                    let msg = format!("Error while upgrading: {:?}", err);
                    let err = IoError::new(IoErrorKind::Other, msg);
                    self.handler.inject_dial_upgrade_error(upgr_info, &err);
                    // TODO: what to do?
                },
            }
        }

        // Poll the handler at the end so that we see the consequences of the method calls on
        // `self.handler`.
        match self.handler.poll()? {
            Async::Ready(Some(NodeHandlerEvent::Custom(event))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(event))));
            },
            Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest((proto, rq)))) => {
                self.queued_dial_upgrades.push(proto);
                return Ok(Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest(rq))));
            },
            Async::Ready(None) => return Ok(Async::Ready(None)),
            Async::NotReady => ()
        };

        Ok(Async::NotReady)
    }
}

pub enum Either<A, B> {
    First(A),
    Second(B),
}

#[derive(Debug, Clone)]
pub struct ProtocolsHandlerSelect<TProto1, TProto2> {
    proto1: TProto1,
    proto2: TProto2,
}

impl<TSubstream, TProto1, TProto2, TProto1Out, TProto2Out, TOutEvent>
    ProtocolsHandler for ProtocolsHandlerSelect<TProto1, TProto2>
where TProto1: ProtocolsHandler<Substream = TSubstream, OutEvent = TOutEvent>,
      TProto2: ProtocolsHandler<Substream = TSubstream, OutEvent = TOutEvent>,
      TSubstream: AsyncRead + AsyncWrite,
      TProto1::Protocol: ConnectionUpgrade<TSubstream, Output = TProto1Out>,
      TProto2::Protocol: ConnectionUpgrade<TSubstream, Output = TProto2Out>,
      TProto1Out: Send + 'static,
      TProto2Out: Send + 'static,
      <TProto1::Protocol as ConnectionUpgrade<TSubstream>>::Future: Send + 'static,
      <TProto2::Protocol as ConnectionUpgrade<TSubstream>>::Future: Send + 'static,
{
    type InEvent = Either<TProto1::InEvent, TProto2::InEvent>;
    type OutEvent = TOutEvent;
    type Substream = TSubstream;
    type Protocol = OrUpgrade<Toggleable<UpgradeMap<TProto1::Protocol, fn(TProto1Out) -> EitherOutput<TProto1Out, TProto2Out>>>, Toggleable<UpgradeMap<TProto2::Protocol, fn(TProto2Out) -> EitherOutput<TProto1Out, TProto2Out>>>>;
    type OutboundOpenInfo = Either<TProto1::OutboundOpenInfo, TProto2::OutboundOpenInfo>;

    #[inline]
    fn listen_protocol(&self) -> Self::Protocol {
        let proto1 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto1.listen_protocol(), EitherOutput::First));
        let proto2 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto2.listen_protocol(), EitherOutput::Second));
        upgrade::or(proto1, proto2)
    }

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
    fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: &IoError) {
        match info {
            Either::First(info) => self.proto1.inject_dial_upgrade_error(info, error),
            Either::Second(info) => self.proto2.inject_dial_upgrade_error(info, error),
        }
    }

    #[inline]
    fn shutdown(&mut self) {
        self.proto1.shutdown();
        self.proto2.shutdown();
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, IoError> {
        match self.proto1.poll()? {
            Async::Ready(Some(NodeHandlerEvent::Custom(event))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(event))));
            },
            Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest((proto, rq)))) => {
                let proto = {
                    let proto1 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(proto, EitherOutput::First));
                    let mut proto2 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto2.listen_protocol(), EitherOutput::Second));
                    proto2.disable();
                    upgrade::or(proto1, proto2)
                };

                return Ok(Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest((proto, Either::First(rq))))));
            },
            // TODO: should wait for the other handler to close as well?
            Async::Ready(None) => return Ok(Async::Ready(None)),
            Async::NotReady => ()
        };

        match self.proto2.poll()? {
            Async::Ready(Some(NodeHandlerEvent::Custom(event))) => {
                return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(event))));
            },
            Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest((proto, rq)))) => {
                let proto = {
                    let mut proto1 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto1.listen_protocol(), EitherOutput::First));
                    proto1.disable();
                    let proto2 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(proto, EitherOutput::Second));
                    upgrade::or(proto1, proto2)
                };

                return Ok(Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest((proto, Either::Second(rq))))));
            },
            // TODO: should wait for the other handler to close as well?
            Async::Ready(None) => return Ok(Async::Ready(None)),
            Async::NotReady => ()
        };

        Ok(Async::NotReady)
    }
}
