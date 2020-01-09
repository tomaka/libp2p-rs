// Copyright 2020 Parity Technologies (UK) Ltd.
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

use crate::behaviour::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use crate::protocols_handler::{
    IntoProtocolsHandler, KeepAlive, ProtocolsHandler, ProtocolsHandlerEvent,
    ProtocolsHandlerUpgrErr, SubstreamProtocol,
};

use futures::prelude::*;
use libp2p_core::upgrade::{self, InboundUpgradeExt, OutboundUpgradeExt};
use libp2p_core::{nodes::ListenerId, ConnectedPoint, Multiaddr, Negotiated, PeerId};
use std::{any::Any, error, fmt, io, task::Context, task::Poll};

/// Implementation of the [`NetworkBehaviour`] trait that contains an opaque
/// [`NetworkBehaviour`].
pub struct BoxedNetworkBehaviour<TOutEv, TSubstream> {
    inner: Box<dyn AbstractBehaviour<OutEvent = TOutEv, Substream = TSubstream>>,
}

pub struct BoxedProtocolsHandler<TSubstream> {
    inner: Box<dyn AbstractProtocolsHandler<Substream = TSubstream>>,
}

impl<TOutEv, TSubstream> BoxedNetworkBehaviour<TOutEv, TSubstream> {
    /// Boxes a `NetworkBehaviour`, turning it into an abstract type.
    pub fn new<T>(inner: T) -> Self
    where
        T: NetworkBehaviour<OutEvent = TOutEv>
            + 'static
            + Send
            + AbstractBehaviour<OutEvent = TOutEv, Substream = TSubstream>, // TODO: put actual bounds?
    {
        BoxedNetworkBehaviour {
            inner: Box::new(inner),
        }
    }
}

impl<TOutEv, TSubstream> NetworkBehaviour for BoxedNetworkBehaviour<TOutEv, TSubstream>
where
    TSubstream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type ProtocolsHandler = BoxedProtocolsHandler<TSubstream>;
    type OutEvent = TOutEv;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        self.inner.new_handler()
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        self.inner.addresses_of_peer(peer_id)
    }

    fn inject_connected(&mut self, peer_id: PeerId, endpoint: ConnectedPoint) {
        self.inner.inject_connected(peer_id, endpoint)
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId, endpoint: ConnectedPoint) {
        self.inner.inject_disconnected(peer_id, endpoint)
    }

    fn inject_replaced(
        &mut self,
        peer_id: PeerId,
        closed_endpoint: ConnectedPoint,
        new_endpoint: ConnectedPoint,
    ) {
        self.inner
            .inject_replaced(peer_id, closed_endpoint, new_endpoint)
    }

    fn inject_node_event(&mut self, peer_id: PeerId, event: Box<dyn Any + Send>) {
        self.inner.inject_node_event(peer_id, event)
    }

    fn inject_addr_reach_failure(
        &mut self,
        peer_id: Option<&PeerId>,
        addr: &Multiaddr,
        error: &dyn error::Error,
    ) {
        self.inner.inject_addr_reach_failure(peer_id, addr, error)
    }

    fn inject_dial_failure(&mut self, peer_id: &PeerId) {
        self.inner.inject_dial_failure(peer_id)
    }

    fn inject_new_listen_addr(&mut self, addr: &Multiaddr) {
        self.inner.inject_new_listen_addr(addr)
    }

    fn inject_expired_listen_addr(&mut self, addr: &Multiaddr) {
        self.inner.inject_expired_listen_addr(addr)
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        self.inner.inject_new_external_addr(addr)
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        self.inner.inject_listener_error(id, err)
    }

    fn inject_listener_closed(&mut self, id: ListenerId) {
        self.inner.inject_listener_closed(id)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Box<dyn Any + Send>, Self::OutEvent>> {
        let event = match self.inner.poll(cx, params) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(e) => e,
        };

        Poll::Ready(match event {
            NetworkBehaviourAction::GenerateEvent(ev) => NetworkBehaviourAction::GenerateEvent(ev),
            NetworkBehaviourAction::DialAddress { address } => {
                NetworkBehaviourAction::DialAddress { address }
            }
            NetworkBehaviourAction::DialPeer { peer_id } => {
                NetworkBehaviourAction::DialPeer { peer_id }
            }
            NetworkBehaviourAction::SendEvent { peer_id, event } => {
                NetworkBehaviourAction::SendEvent { peer_id, event }
            }
            NetworkBehaviourAction::ReportObservedAddr { address } => {
                NetworkBehaviourAction::ReportObservedAddr { address }
            }
        })
    }
}

// TODO: remove pub
pub trait AbstractProtocolsHandler: Send {
    type Substream;
    fn listen_protocol(
        &self,
    ) -> SubstreamProtocol<
        upgrade::BoxedInboundUpgrade<Negotiated<Self::Substream>, Box<dyn Any + Send>, io::Error>,
    >;
    fn inject_fully_negotiated_inbound(&mut self, protocol: Box<dyn Any + Send>);
    fn inject_fully_negotiated_outbound(
        &mut self,
        protocol: Box<dyn Any + Send>,
        info: Box<dyn Any + Send>,
    );
    fn inject_event(&mut self, event: Box<dyn Any + Send>);
    fn inject_dial_upgrade_error(
        &mut self,
        info: Box<dyn Any + Send>,
        error: ProtocolsHandlerUpgrErr<io::Error>,
    );
    fn connection_keep_alive(&self) -> KeepAlive;
    fn poll(
        &mut self,
        cx: &mut Context,
    ) -> Poll<
        ProtocolsHandlerEvent<
            upgrade::BoxedOutboundUpgrade<
                Negotiated<Self::Substream>,
                Box<dyn Any + Send>,
                io::Error,
            >,
            Box<dyn Any + Send>,
            Box<dyn Any + Send>,
            io::Error,
        >,
    >;
}

impl<T> AbstractProtocolsHandler for T
where
    T: ProtocolsHandler + Send,
    T::Error: Send + Sync + 'static,
    T::InEvent: Send + 'static,
    T::OutEvent: Send + 'static,
    T::OutboundOpenInfo: Send + 'static,
    T::InboundProtocol:
        upgrade::InboundUpgrade<Negotiated<T::Substream>> + fmt::Debug + Clone + Send + 'static,
    T::OutboundProtocol:
        upgrade::OutboundUpgrade<Negotiated<T::Substream>> + fmt::Debug + Clone + Send + 'static,
    <<T::InboundProtocol as upgrade::UpgradeInfo>::InfoIter as IntoIterator>::IntoIter:
        Send + 'static,
    <T::InboundProtocol as upgrade::UpgradeInfo>::Info: Send + 'static,
    <<T::OutboundProtocol as upgrade::UpgradeInfo>::InfoIter as IntoIterator>::IntoIter:
        Send + 'static,
    <T::OutboundProtocol as upgrade::UpgradeInfo>::Info: Send + 'static,
    <T::InboundProtocol as upgrade::InboundUpgrade<Negotiated<T::Substream>>>::Output:
        Send + 'static,
    <T::InboundProtocol as upgrade::InboundUpgrade<Negotiated<T::Substream>>>::Error:
        error::Error + Send + Sync + 'static,
    <T::InboundProtocol as upgrade::InboundUpgrade<Negotiated<T::Substream>>>::Future:
        Send + 'static,
    <T::OutboundProtocol as upgrade::OutboundUpgrade<Negotiated<T::Substream>>>::Output:
        Send + 'static,
    <T::OutboundProtocol as upgrade::OutboundUpgrade<Negotiated<T::Substream>>>::Error:
        error::Error + Send + Sync + 'static,
    <T::OutboundProtocol as upgrade::OutboundUpgrade<Negotiated<T::Substream>>>::Future:
        Send + 'static,
{
    type Substream = T::Substream;

    fn listen_protocol(
        &self,
    ) -> SubstreamProtocol<
        upgrade::BoxedInboundUpgrade<Negotiated<T::Substream>, Box<dyn Any + Send>, io::Error>,
    > {
        ProtocolsHandler::listen_protocol(self).map_upgrade(|upgr| {
            let upgr = upgr
                .map_inbound(|out| Box::new(out) as Box<_>)
                .map_inbound_err(|err| io::Error::new(io::ErrorKind::Other, err));
            upgrade::BoxedInboundUpgrade::new(upgr)
        })
    }

    fn inject_fully_negotiated_inbound(&mut self, protocol: Box<dyn Any + Send>) {
        let protocol = *protocol
            .downcast()
            .expect("Any is always of the right type, by convention; qed");
        ProtocolsHandler::inject_fully_negotiated_inbound(self, protocol)
    }

    fn inject_fully_negotiated_outbound(
        &mut self,
        protocol: Box<dyn Any + Send>,
        info: Box<dyn Any + Send>,
    ) {
        let protocol = *protocol
            .downcast()
            .expect("Any is always of the right type, by convention; qed");
        let info = *info
            .downcast()
            .expect("Any is always of the right type, by convention; qed");
        ProtocolsHandler::inject_fully_negotiated_outbound(self, protocol, info)
    }

    fn inject_event(&mut self, event: Box<dyn Any + Send>) {
        let event = *event
            .downcast()
            .expect("Any is always of the right type, by convention; qed");
        ProtocolsHandler::inject_event(self, event)
    }

    fn inject_dial_upgrade_error(
        &mut self,
        info: Box<dyn Any + Send>,
        error: ProtocolsHandlerUpgrErr<io::Error>,
    ) {
        let info = *info
            .downcast()
            .expect("Any is always of the right type, by convention; qed");
        let error = match error {
            ProtocolsHandlerUpgrErr::Timeout => ProtocolsHandlerUpgrErr::Timeout,
            ProtocolsHandlerUpgrErr::Timer => ProtocolsHandlerUpgrErr::Timer,
            ProtocolsHandlerUpgrErr::Upgrade(upgrade::UpgradeError::Select(error)) => {
                ProtocolsHandlerUpgrErr::Upgrade(upgrade::UpgradeError::Select(error))
            }
            ProtocolsHandlerUpgrErr::Upgrade(upgrade::UpgradeError::Apply(error)) => {
                let error = *error
                    .into_inner()
                    .expect("Substream IO errors are always created with io::Error::new; qed")
                    .downcast()
                    .expect("Substream errors are always created from the original error; qed");
                ProtocolsHandlerUpgrErr::Upgrade(upgrade::UpgradeError::Apply(error))
            }
        };
        ProtocolsHandler::inject_dial_upgrade_error(self, info, error)
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        ProtocolsHandler::connection_keep_alive(self)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
    ) -> Poll<
        ProtocolsHandlerEvent<
            upgrade::BoxedOutboundUpgrade<Negotiated<T::Substream>, Box<dyn Any + Send>, io::Error>,
            Box<dyn Any + Send>,
            Box<dyn Any + Send>,
            io::Error,
        >,
    > {
        let event = match ProtocolsHandler::poll(self, cx) {
            Poll::Ready(ev) => ev,
            Poll::Pending => return Poll::Pending,
        };

        Poll::Ready(match event {
            ProtocolsHandlerEvent::OutboundSubstreamRequest { protocol, info } => {
                ProtocolsHandlerEvent::OutboundSubstreamRequest {
                    info: Box::new(info) as Box<_>,
                    protocol: protocol.map_upgrade(|upgr| {
                        let upgr = upgr
                            .map_outbound(|out| Box::new(out) as Box<_>)
                            .map_outbound_err(|err| io::Error::new(io::ErrorKind::Other, err));
                        upgrade::BoxedOutboundUpgrade::new(upgr)
                    }),
                }
            }
            ProtocolsHandlerEvent::Close(err) => {
                ProtocolsHandlerEvent::Close(io::Error::new(io::ErrorKind::Other, err))
            }
            ProtocolsHandlerEvent::Custom(ev) => ProtocolsHandlerEvent::Custom(Box::new(ev)),
        })
    }
}

impl<TSubstream> ProtocolsHandler for BoxedProtocolsHandler<TSubstream>
where
    TSubstream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type InEvent = Box<dyn Any + Send>;
    type OutEvent = Box<dyn Any + Send>;
    type Error = io::Error;
    type Substream = TSubstream;
    type InboundProtocol =
        upgrade::BoxedInboundUpgrade<Negotiated<TSubstream>, Box<dyn Any + Send>, io::Error>;
    type OutboundProtocol =
        upgrade::BoxedOutboundUpgrade<Negotiated<TSubstream>, Box<dyn Any + Send>, io::Error>;
    type OutboundOpenInfo = Box<dyn Any + Send>;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        self.inner.listen_protocol()
    }

    fn inject_fully_negotiated_inbound(&mut self, protocol: Box<dyn Any + Send>) {
        self.inner.inject_fully_negotiated_inbound(protocol)
    }

    fn inject_fully_negotiated_outbound(
        &mut self,
        protocol: Box<dyn Any + Send>,
        info: Box<dyn Any + Send>,
    ) {
        self.inner.inject_fully_negotiated_outbound(protocol, info)
    }

    fn inject_event(&mut self, event: Box<dyn Any + Send>) {
        self.inner.inject_event(event)
    }

    fn inject_dial_upgrade_error(
        &mut self,
        info: Box<dyn Any + Send>,
        error: ProtocolsHandlerUpgrErr<io::Error>,
    ) {
        self.inner.inject_dial_upgrade_error(info, error)
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        self.inner.connection_keep_alive()
    }

    fn poll(
        &mut self,
        cx: &mut Context,
    ) -> Poll<
        ProtocolsHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::OutEvent,
            Self::Error,
        >,
    > {
        self.inner.poll(cx)
    }
}

/// Implemented on all types that implement [`PollParameters`].
// TODO: remove pub
pub trait AbstractPollParameters {
    fn supported_protocols(&self) -> std::vec::IntoIter<Vec<u8>>;
    fn listened_addresses(&self) -> std::vec::IntoIter<Multiaddr>;
    fn external_addresses(&self) -> std::vec::IntoIter<Multiaddr>;
    fn local_peer_id(&self) -> &PeerId;
}

impl<T> AbstractPollParameters for T
where
    T: ?Sized + PollParameters,
{
    fn supported_protocols<'a>(&'a self) -> std::vec::IntoIter<Vec<u8>> {
        PollParameters::supported_protocols(self)
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn listened_addresses<'a>(&'a self) -> std::vec::IntoIter<Multiaddr> {
        PollParameters::listened_addresses(self)
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn external_addresses<'a>(&'a self) -> std::vec::IntoIter<Multiaddr> {
        PollParameters::external_addresses(self)
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn local_peer_id(&self) -> &PeerId {
        PollParameters::local_peer_id(self)
    }
}

impl<'a> PollParameters for &'a mut dyn AbstractPollParameters {
    type SupportedProtocolsIter = std::vec::IntoIter<Vec<u8>>;
    type ListenedAddressesIter = std::vec::IntoIter<Multiaddr>;
    type ExternalAddressesIter = std::vec::IntoIter<Multiaddr>;

    fn supported_protocols(&self) -> Self::SupportedProtocolsIter {
        AbstractPollParameters::supported_protocols(self)
    }

    fn listened_addresses(&self) -> Self::ListenedAddressesIter {
        AbstractPollParameters::listened_addresses(self)
    }

    fn external_addresses(&self) -> Self::ExternalAddressesIter {
        AbstractPollParameters::external_addresses(self)
    }

    fn local_peer_id(&self) -> &PeerId {
        AbstractPollParameters::local_peer_id(self)
    }
}

/// Implemented on all types that implement [`NetworkBehaviour`].
// TODO: remove pub
pub trait AbstractBehaviour: Send {
    type OutEvent;
    type Substream;
    fn new_handler(&mut self) -> BoxedProtocolsHandler<Self::Substream>;
    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr>;
    fn inject_connected(&mut self, peer_id: PeerId, endpoint: ConnectedPoint);
    fn inject_disconnected(&mut self, peer_id: &PeerId, endpoint: ConnectedPoint);
    fn inject_replaced(
        &mut self,
        peer_id: PeerId,
        closed_endpoint: ConnectedPoint,
        new_endpoint: ConnectedPoint,
    );
    fn inject_node_event(&mut self, peer_id: PeerId, event: Box<dyn Any + Send>);
    fn inject_addr_reach_failure(
        &mut self,
        peer_id: Option<&PeerId>,
        addr: &Multiaddr,
        error: &dyn error::Error,
    );
    fn inject_dial_failure(&mut self, peer_id: &PeerId);
    fn inject_new_listen_addr(&mut self, addr: &Multiaddr);
    fn inject_expired_listen_addr(&mut self, addr: &Multiaddr);
    fn inject_new_external_addr(&mut self, addr: &Multiaddr);
    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static));
    fn inject_listener_closed(&mut self, id: ListenerId);
    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut dyn AbstractPollParameters,
    ) -> Poll<NetworkBehaviourAction<Box<dyn Any + Send>, Self::OutEvent>>;
}

impl<T, TSubstream> AbstractBehaviour for T
where
    T: NetworkBehaviour + Send,
    // TODO: wrong; we have to add an intermediate AbstractIntoProtocolsHandler trait
    T::ProtocolsHandler:
        IntoProtocolsHandler + AbstractProtocolsHandler<Substream = TSubstream> + Send + 'static,
    <T::ProtocolsHandler as IntoProtocolsHandler>::Handler:
        AbstractProtocolsHandler + Send + 'static,
    <<T::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::InEvent:
        Send + 'static,
{
    type OutEvent = T::OutEvent;
    type Substream = TSubstream;

    fn new_handler(&mut self) -> BoxedProtocolsHandler<Self::Substream> {
        BoxedProtocolsHandler {
            inner: Box::new(NetworkBehaviour::new_handler(self)),
        }
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        NetworkBehaviour::addresses_of_peer(self, peer_id)
    }

    fn inject_connected(&mut self, peer_id: PeerId, endpoint: ConnectedPoint) {
        NetworkBehaviour::inject_connected(self, peer_id, endpoint)
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId, endpoint: ConnectedPoint) {
        NetworkBehaviour::inject_disconnected(self, peer_id, endpoint)
    }

    fn inject_replaced(
        &mut self,
        peer_id: PeerId,
        closed_endpoint: ConnectedPoint,
        new_endpoint: ConnectedPoint,
    ) {
        NetworkBehaviour::inject_replaced(self, peer_id, closed_endpoint, new_endpoint)
    }

    fn inject_node_event(&mut self, peer_id: PeerId, event: Box<dyn Any + Send>) {
        let event = event
            .downcast::<<<T::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent>()
            .expect("The Any must always contain this type by convention; qed");
        NetworkBehaviour::inject_node_event(self, peer_id, *event)
    }

    fn inject_addr_reach_failure(
        &mut self,
        peer_id: Option<&PeerId>,
        addr: &Multiaddr,
        error: &dyn error::Error,
    ) {
        NetworkBehaviour::inject_addr_reach_failure(self, peer_id, addr, error)
    }

    fn inject_dial_failure(&mut self, peer_id: &PeerId) {
        NetworkBehaviour::inject_dial_failure(self, peer_id)
    }

    fn inject_new_listen_addr(&mut self, addr: &Multiaddr) {
        NetworkBehaviour::inject_new_listen_addr(self, addr)
    }

    fn inject_expired_listen_addr(&mut self, addr: &Multiaddr) {
        NetworkBehaviour::inject_expired_listen_addr(self, addr)
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        NetworkBehaviour::inject_new_external_addr(self, addr)
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        NetworkBehaviour::inject_listener_error(self, id, err)
    }

    fn inject_listener_closed(&mut self, id: ListenerId) {
        NetworkBehaviour::inject_listener_closed(self, id)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        mut params: &mut dyn AbstractPollParameters,
    ) -> Poll<NetworkBehaviourAction<Box<dyn Any + Send>, Self::OutEvent>> {
        let event = match NetworkBehaviour::poll(self, cx, &mut params) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(e) => e,
        };

        Poll::Ready(match event {
            NetworkBehaviourAction::GenerateEvent(ev) => NetworkBehaviourAction::GenerateEvent(ev),
            NetworkBehaviourAction::DialAddress { address } => {
                NetworkBehaviourAction::DialAddress { address }
            }
            NetworkBehaviourAction::DialPeer { peer_id } => {
                NetworkBehaviourAction::DialPeer { peer_id }
            }
            NetworkBehaviourAction::SendEvent { peer_id, event } => {
                let event = Box::new(event) as Box<dyn Any + Send>;
                NetworkBehaviourAction::SendEvent { peer_id, event }
            }
            NetworkBehaviourAction::ReportObservedAddr { address } => {
                NetworkBehaviourAction::ReportObservedAddr { address }
            }
        })
    }
}
