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

use crate::muxing::StreamMuxer;
use crate::nodes::node::{NodeEvent, NodeStream, Substream};
use futures::prelude::*;
use std::{error, fmt, io};

mod tests;

/// Handler for the substreams of a node.
// TODO: right now it is possible for a node handler to be built, then shut down right after if we
//       realize we dialed the wrong peer for example; this could be surprising and should either
//       be documented or changed (favouring the "documented" right now)
pub trait NodeHandler {
    /// Custom event that can be received from the outside.
    type InEvent;
    /// Custom event that can be produced by the handler and that will be returned by the swarm.
    type OutEvent;
    /// Error that can happen during the processing of the node.
    type Error;
    /// The type of the substream containing the data.
    type Substream;
    /// Information about a substream. Can be sent to the handler through a `NodeHandlerEndpoint`,
    /// and will be passed back in `inject_substream` or `inject_outbound_closed`.
    type OutboundOpenInfo;

    /// Sends a new substream to the handler.
    ///
    /// The handler is responsible for upgrading the substream to whatever protocol it wants.
    ///
    /// # Panic
    ///
    /// Implementations are allowed to panic in the case of dialing if the `user_data` in
    /// `endpoint` doesn't correspond to what was returned earlier when polling, or is used
    /// multiple times.
    fn inject_substream(&mut self, substream: Self::Substream, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>);

    /// Indicates to the handler that the inbound part of the muxer has been closed, and that
    /// therefore no more inbound substream will be produced.
    fn inject_inbound_closed(&mut self);

    /// Indicates to the handler that an outbound substream failed to open because the outbound
    /// part of the muxer has been closed.
    ///
    /// # Panic
    ///
    /// Implementations are allowed to panic if `user_data` doesn't correspond to what was returned
    /// earlier when polling, or is used multiple times.
    fn inject_outbound_closed(&mut self, user_data: Self::OutboundOpenInfo);

    /// Injects an event coming from the outside into the handler.
    fn inject_event(&mut self, event: Self::InEvent);

    /// Indicates to the node that it should shut down. After that, it is expected that `poll()`
    /// returns `Ready(NodeHandlerEvent::Shutdown)` as soon as possible.
    ///
    /// This method allows an implementation to perform a graceful shutdown of the substreams, and
    /// send back various events.
    fn shutdown(&mut self);

    /// Should behave like `Stream::poll()`. Should close if no more event can be produced and the
    /// node should be closed.
    fn poll(&mut self) -> Poll<NodeHandlerEvent<Self::OutboundOpenInfo, Self::OutEvent>, Self::Error>;
}

/// Endpoint for a received substream.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NodeHandlerEndpoint<TOutboundOpenInfo> {
    Dialer(TOutboundOpenInfo),
    Listener,
}

impl<TOutboundOpenInfo> NodeHandlerEndpoint<TOutboundOpenInfo> {
    /// Returns true for `Dialer`.
    #[inline]
    pub fn is_dialer(&self) -> bool {
        match self {
            NodeHandlerEndpoint::Dialer(_) => true,
            NodeHandlerEndpoint::Listener => false,
        }
    }

    /// Returns true for `Listener`.
    #[inline]
    pub fn is_listener(&self) -> bool {
        match self {
            NodeHandlerEndpoint::Dialer(_) => false,
            NodeHandlerEndpoint::Listener => true,
        }
    }
}

/// Event produced by a handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeHandlerEvent<TOutboundOpenInfo, TCustom> {
    /// Require a new outbound substream to be opened with the remote.
    OutboundSubstreamRequest(TOutboundOpenInfo),

    /// Gracefully shut down the connection to the node.
    Shutdown(GracefulClose),

    /// Other event.
    Custom(TCustom),
}

/// Event produced by a handler.
impl<TOutboundOpenInfo, TCustom> NodeHandlerEvent<TOutboundOpenInfo, TCustom> {
    /// If this is `OutboundSubstreamRequest`, maps the content to something else.
    #[inline]
    pub fn map_outbound_open_info<F, I>(self, map: F) -> NodeHandlerEvent<I, TCustom>
    where F: FnOnce(TOutboundOpenInfo) -> I
    {
        match self {
            NodeHandlerEvent::OutboundSubstreamRequest(val) => {
                NodeHandlerEvent::OutboundSubstreamRequest(map(val))
            },
            NodeHandlerEvent::Shutdown(reason) => NodeHandlerEvent::Shutdown(reason),
            NodeHandlerEvent::Custom(val) => NodeHandlerEvent::Custom(val),
        }
    }

    /// If this is `Custom`, maps the content to something else.
    #[inline]
    pub fn map_custom<F, I>(self, map: F) -> NodeHandlerEvent<TOutboundOpenInfo, I>
    where F: FnOnce(TCustom) -> I
    {
        match self {
            NodeHandlerEvent::OutboundSubstreamRequest(val) => {
                NodeHandlerEvent::OutboundSubstreamRequest(val)
            },
            NodeHandlerEvent::Shutdown(reason) => NodeHandlerEvent::Shutdown(reason),
            NodeHandlerEvent::Custom(val) => NodeHandlerEvent::Custom(map(val)),
        }
    }
}

/// A node combined with an implementation of `NodeHandler`.
pub struct HandledNode<TMuxer, THandler>
where
    TMuxer: StreamMuxer,
    THandler: NodeHandler<Substream = Substream<TMuxer>>,
{
    /// Node that handles the muxing.
    node: NodeStream<TMuxer, THandler::OutboundOpenInfo>,
    /// Handler that processes substreams.
    handler: THandler,
    /// State of the node.
    state: ShutdownState,
}

/// State of the handled node.
///
/// In order to not conflict with the state of the node.
#[derive(Debug)]
enum ShutdownState {
    /// Normal state of operation.
    Normal,

    /// Waiting for both the handler to finish, and maybe the node as well. Contains the original
    /// reason why we are shutting down.
    ShuttingDown(GracefulClose),

    /// The handler is finished. If the node is finished, then we are done. Contains the original
    /// reason why we are shutting down.
    HandlerFinished(GracefulClose),
}

impl ShutdownState {
    /// Returns true for `HandlerFinished`, false otherwise.
    fn handler_is_done(&self) -> bool {
        match *self {
            ShutdownState::Normal => false,
            ShutdownState::ShuttingDown(_) => false,
            ShutdownState::HandlerFinished(_) => true,
        }
    }
}

impl<TMuxer, THandler> fmt::Debug for HandledNode<TMuxer, THandler>
where
    TMuxer: StreamMuxer,
    THandler: NodeHandler<Substream = Substream<TMuxer>> + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("HandledNode")
            .field("node", &self.node)
            .field("handler", &self.handler)
            .field("state", &self.state)
            .finish()
    }
}

impl<TMuxer, THandler> HandledNode<TMuxer, THandler>
where
    TMuxer: StreamMuxer,
    THandler: NodeHandler<Substream = Substream<TMuxer>>,
{
    /// Builds a new `HandledNode`.
    #[inline]
    pub fn new(muxer: TMuxer, handler: THandler) -> Self {
        HandledNode {
            node: NodeStream::new(muxer),
            handler,
            state: ShutdownState::Normal,
        }
    }

    /// Returns a reference to the `NodeHandler`
    pub fn handler(&self) -> &THandler {
        &self.handler
    }

    /// Returns a mutable reference to the `NodeHandler`
    pub fn handler_mut(&mut self) -> &mut THandler {
        &mut self.handler
    }

    /// Injects an event to the handler.
    #[inline]
    pub fn inject_event(&mut self, event: THandler::InEvent) {
        self.handler.inject_event(event);
    }

    /// Returns true if the inbound channel of the muxer is open.
    ///
    /// If `true` is returned, more inbound substream will be received.
    #[inline]
    pub fn is_inbound_open(&self) -> bool {
        self.node.is_inbound_open()
    }

    /// Returns true if the outbound channel of the muxer is open.
    ///
    /// If `true` is returned, more outbound substream will be opened.
    #[inline]
    pub fn is_outbound_open(&self) -> bool {
        self.node.is_outbound_open()
    }

    /// Returns true if the handled node is in the process of shutting down or has finished
    /// shutting down.
    #[inline]
    pub fn is_shutting_down(&self) -> bool {
        match self.state {
            ShutdownState::Normal => false,
            ShutdownState::ShuttingDown(_) => true,
            ShutdownState::HandlerFinished(_) => true,
        }
    }

    /// Indicates to the handled node that it must shut down. After calling this method, the
    /// `Stream` will end in the not-so-distant future with a `Close` event containing the reason
    /// passed as parameter.
    ///
    /// After this method returns, `is_shutting_down()` will return true.
    pub fn shutdown(&mut self, reason: GracefulClose) {
        if let ShutdownState::Normal = self.state {
            self.state = ShutdownState::ShuttingDown(reason);
        } else {
            return;
        }

        self.node.shutdown_all();
        for user_data in self.node.cancel_outgoing() {
            self.handler.inject_outbound_closed(user_data);
        }
        self.handler.shutdown();
    }
}

/// Event emitted when polling the handled node.
#[derive(Debug)]
pub enum HandledNodeOutEvent<THandlerEv> {
    /// Event from the handler.
    HandlerEvent(THandlerEv),
    /// The node has been gracefully closed. This is the last event that is emitted by the handler.
    /// Any attempt to further poll the handled node will lead to a panic.
    Close(GracefulClose),
}

/// Reason why the node has been closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GracefulClose {
    /// The node is closing because the remote has gracefully closed the connection.
    RemoteGraceful,
}

impl<TMuxer, THandler> Stream for HandledNode<TMuxer, THandler>
where
    TMuxer: StreamMuxer,
    THandler: NodeHandler<Substream = Substream<TMuxer>>,
{
    type Item = HandledNodeOutEvent<THandler::OutEvent>;
    type Error = HandledNodeError<THandler::Error>;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        loop {
            if let ShutdownState::HandlerFinished(_) = self.state {
                if self.node.is_finished() {
                    panic!("HandledNode::poll() called after it has been finished");
                }
            }

            let mut node_not_ready = false;
            if !self.node.is_finished() {
                match self.node.poll().map_err(HandledNodeError::Node)? {
                    Async::NotReady => node_not_ready = true,
                    Async::Ready(Some(NodeEvent::InboundSubstream { substream })) => {
                        self.handler.inject_substream(substream, NodeHandlerEndpoint::Listener)
                    }
                    Async::Ready(Some(NodeEvent::OutboundSubstream { user_data, substream })) => {
                        let endpoint = NodeHandlerEndpoint::Dialer(user_data);
                        self.handler.inject_substream(substream, endpoint)
                    }
                    Async::Ready(None) => {
                        match self.state {
                            ShutdownState::Normal => {
                                self.handler.shutdown();
                                self.state = ShutdownState::ShuttingDown(GracefulClose::RemoteGraceful);
                            },
                            ShutdownState::ShuttingDown(_) => {},
                            ShutdownState::HandlerFinished(ref cause) => {
                                return Ok(Async::Ready(Some(HandledNodeOutEvent::Close(cause.clone()))));
                            },
                        }
                    }
                    Async::Ready(Some(NodeEvent::OutboundClosed { user_data })) => {
                        self.handler.inject_outbound_closed(user_data)
                    }
                    Async::Ready(Some(NodeEvent::InboundClosed)) => {
                        self.handler.inject_inbound_closed()
                    }
                }
            } else {
                node_not_ready = true;
            }

            let mut handler_not_ready = false;
            if !self.state.handler_is_done() {
                match self.handler.poll().map_err(HandledNodeError::Handler)? {
                    Async::NotReady => handler_not_ready = true,
                    Async::Ready(NodeHandlerEvent::OutboundSubstreamRequest(user_data)) => {
                        if self.node.is_outbound_open() {
                            match self.node.open_substream(user_data) {
                                Ok(()) => (),
                                Err(user_data) => {
                                    self.handler.inject_outbound_closed(user_data)
                                },
                            }
                        } else {
                            self.handler.inject_outbound_closed(user_data);
                        }
                    }
                    Async::Ready(NodeHandlerEvent::Custom(event)) => {
                        return Ok(Async::Ready(Some(HandledNodeOutEvent::HandlerEvent(event))));
                    }
                    Async::Ready(NodeHandlerEvent::Shutdown(reason)) => {
                        match self.state {
                            ShutdownState::Normal => {
                                self.state = ShutdownState::ShuttingDown(reason);
                                debug_assert!(!self.node.is_finished());
                                self.node.cancel_outgoing();
                                self.node.shutdown_all();
                            },
                            ShutdownState::ShuttingDown(ref cause) => {
                                let cause = cause.clone();
                                self.state = ShutdownState::HandlerFinished(cause.clone());
                                if self.node.is_finished() {
                                    return Ok(Async::Ready(Some(HandledNodeOutEvent::Close(cause))));
                                }
                            },
                            ShutdownState::HandlerFinished(_) =>
                                unreachable!("We have a if for this variant above; QED")
                        }
                    }
                }
            } else {
                handler_not_ready = true;
            }

            if node_not_ready && handler_not_ready {
                return Ok(Async::NotReady);
            }
        }
    }
}

/// Error that can happen when polling a `HandledNode`.
#[derive(Debug)]
pub enum HandledNodeError<THandlerErr> {
    /// An error happend in the stream muxer.
    // TODO: eventually this should also be a custom error
    Node(io::Error),
    /// An error happened in the handler of the connection to the node.
    Handler(THandlerErr),
}

impl<THandlerErr> fmt::Display for HandledNodeError<THandlerErr>
where THandlerErr: fmt::Display
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HandledNodeError::Node(err) => write!(f, "{}", err),
            HandledNodeError::Handler(err) => write!(f, "{}", err),
        }
    }
}

impl<THandlerErr> error::Error for HandledNodeError<THandlerErr>
where THandlerErr: error::Error + 'static
{
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            HandledNodeError::Node(err) => Some(err),
            HandledNodeError::Handler(err) => Some(err),
        }
    }
}
