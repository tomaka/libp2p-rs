// Copyright 2017-2020 Parity Technologies (UK) Ltd.
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

use crate::{error::Error, socket, verifier, Upgrade};
use async_macros::ready;
use async_std::{net::SocketAddr, task::spawn};
use futures::{channel::mpsc, prelude::*};
use libp2p_core::{
    multiaddr::{host_addresses, Multiaddr, Protocol},
    transport::{ListenerEvent, TransportError},
    Transport,
};
use log::{debug, info, trace, warn};
use parking_lot::Mutex;
use quinn_proto::{Connection, ConnectionHandle};
use std::{
    collections::{hash_map::Entry, HashMap},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

/// Represents the configuration for a QUIC transport capability for libp2p.
#[derive(Debug, Clone)]
pub struct Config {
    /// The client configuration.  Quinn provides functions for making one.
    client_config: quinn_proto::ClientConfig,
    /// The server configuration.  Quinn provides functions for making one.
    server_config: Arc<quinn_proto::ServerConfig>,
    /// The endpoint configuration
    endpoint_config: Arc<quinn_proto::EndpointConfig>,
}

fn make_client_config(
    certificate: rustls::Certificate,
    key: rustls::PrivateKey,
) -> quinn_proto::ClientConfig {
    let mut transport = quinn_proto::TransportConfig::default();
    transport.stream_window_uni(0);
    transport.datagram_receive_buffer_size(None);
    transport.keep_alive_interval(Some(Duration::from_millis(1000)));
    let mut crypto = rustls::ClientConfig::new();
    crypto.versions = vec![rustls::ProtocolVersion::TLSv1_3];
    crypto.enable_early_data = true;
    crypto.set_single_client_cert(vec![certificate], key);
    let verifier = verifier::VeryInsecureRequireExactlyOneSelfSignedServerCertificate;
    crypto
        .dangerous()
        .set_certificate_verifier(Arc::new(verifier));
    quinn_proto::ClientConfig {
        transport: Arc::new(transport),
        crypto: Arc::new(crypto),
    }
}

fn make_server_config(
    certificate: rustls::Certificate,
    key: rustls::PrivateKey,
) -> quinn_proto::ServerConfig {
    let mut transport = quinn_proto::TransportConfig::default();
    transport.stream_window_uni(0);
    transport.datagram_receive_buffer_size(None);
    let mut crypto = rustls::ServerConfig::new(Arc::new(
        verifier::VeryInsecureRequireExactlyOneSelfSignedClientCertificate,
    ));
    crypto.versions = vec![rustls::ProtocolVersion::TLSv1_3];
    crypto
        .set_single_cert(vec![certificate], key)
        .expect("we are given a valid cert; qed");
    let mut config = quinn_proto::ServerConfig::default();
    config.transport = Arc::new(transport);
    config.crypto = Arc::new(crypto);
    config
}

impl Config {
    /// Creates a new configuration object for TCP/IP.
    pub fn new(keypair: &libp2p_core::identity::Keypair) -> Self {
        let cert = super::certificate::make_cert(&keypair);
        let (cert, key) = (
            rustls::Certificate(
                cert.serialize_der()
                    .expect("serialization of a valid cert will succeed; qed"),
            ),
            rustls::PrivateKey(cert.serialize_private_key_der()),
        );
        Self {
            client_config: make_client_config(cert.clone(), key.clone()),
            server_config: Arc::new(make_server_config(cert, key)),
            endpoint_config: Default::default(),
        }
    }
}

#[derive(Debug)]
pub enum EndpointMessage {
    Dummy,
    ConnectionAccepted,
    EndpointEvent {
        handle: ConnectionHandle,
        event: quinn_proto::EndpointEvent,
    },
}

#[derive(Debug)]
pub(super) struct EndpointInner {
    inner: quinn_proto::Endpoint,
    muxers: HashMap<ConnectionHandle, Arc<Mutex<super::connection::Muxer>>>,
    pending: socket::Pending,
    /// Used to receive events from connections
    event_receiver: mpsc::Receiver<EndpointMessage>,
    buffer: Vec<u8>,
}

impl EndpointInner {
    pub(super) fn handle_event(
        &mut self,
        handle: ConnectionHandle,
        event: quinn_proto::EndpointEvent,
    ) -> Option<quinn_proto::ConnectionEvent> {
        if event.is_drained() {
            let res = self.inner.handle_event(handle, event);
            self.muxers.remove(&handle);
            res
        } else {
            self.inner.handle_event(handle, event)
        }
    }

    fn drive_events(&mut self, cx: &mut Context<'_>) {
        while let Poll::Ready(e) = self.event_receiver.poll_next_unpin(cx).map(|e| e.unwrap()) {
            match e {
                EndpointMessage::ConnectionAccepted => {
                    debug!("accepting connection!");
                    self.inner.accept();
                }
                EndpointMessage::EndpointEvent { handle, event } => {
                    debug!("we have event {:?} from connection {:?}", event, handle);
                    if let Some(event) = self.handle_event(handle, event) {
                        self.send_connection_event(handle, event)
                    }
                }
                EndpointMessage::Dummy => {}
            }
        }
    }

    /// Send the given `ConnectionEvent` to the appropriate connection,
    /// and process any events it sends back.
    fn send_connection_event(
        &mut self,
        handle: ConnectionHandle,
        event: quinn_proto::ConnectionEvent,
    ) {
        let Self { inner, muxers, .. } = self;
        let entry = match muxers.entry(handle) {
            Entry::Vacant(_) => return,
            Entry::Occupied(occupied) => occupied,
        };
        let mut connection = entry.get().lock();
        connection.handle_event(event);
        let mut is_drained = false;
        while let Some(event) = connection.poll_endpoint_events() {
            is_drained |= event.is_drained();
            if let Some(event) = inner.handle_event(handle, event) {
                connection.handle_event(event)
            }
        }
        connection.process_app_events();
        connection.wake_driver();
        if is_drained {
            drop(connection);
            entry.remove();
        }
    }

    fn drive_receive(
        &mut self,
        socket: &socket::Socket,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(ConnectionHandle, Connection), Error>> {
        use quinn_proto::DatagramEvent;
        loop {
            let (bytes, peer) = ready!(socket.recv_from(cx, &mut self.buffer[..])?);
            let (handle, event) =
                match self
                    .inner
                    .handle(Instant::now(), peer, None, self.buffer[..bytes].into())
                {
                    Some(e) => e,
                    None => continue,
                };
            trace!("have an event!");
            match event {
                DatagramEvent::ConnectionEvent(event) => {
                    self.send_connection_event(handle, event);
                    continue;
                }
                DatagramEvent::NewConnection(connection) => {
                    debug!("new connection detected!");
                    break Poll::Ready(Ok((handle, connection)));
                }
            }
        }
    }

    fn poll_transmit_pending(
        &mut self,
        socket: &socket::Socket,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Error>> {
        let Self { inner, pending, .. } = self;
        pending
            .send_packet(cx, socket, &mut || inner.poll_transmit())
            .map_err(Error::IO)
    }
}

type ListenerResult = ListenerEvent<Upgrade, Error>;

#[derive(Debug)]
pub(super) struct EndpointData {
    /// The single UDP socket used for I/O
    socket: Arc<socket::Socket>,
    /// A `Mutex` protecting the QUIC state machine.
    inner: Mutex<EndpointInner>,
    /// The channel on which new connections are sent.  This is bounded in practice by the accept
    /// backlog.
    new_connections: mpsc::UnboundedSender<ListenerResult>,
    /// The channel used to receive new connections.
    receive_connections: Mutex<Option<mpsc::UnboundedReceiver<ListenerResult>>>,
    /// The `Multiaddr`
    address: Multiaddr,
    /// The configuration
    config: Config,
}

type Sender = mpsc::Sender<EndpointMessage>;

impl ConnectionEndpoint {
    pub fn socket(&self) -> &socket::Socket {
        &self.socket
    }

    pub fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<SendToken, mpsc::SendError>> {
        self.channel.poll_ready(cx).map_ok(SendToken)
    }

    pub fn send_message(
        &mut self,
        event: EndpointMessage,
        SendToken(()): SendToken,
    ) -> Result<(), mpsc::SendError> {
        self.channel.start_send(event)
    }
}

mod channel_ref {
    use super::{Arc, EndpointData, EndpointMessage::Dummy, Sender};
    #[derive(Debug, Clone)]
    pub struct Channel(Sender);

    impl Channel {
        pub fn new(s: Sender) -> Self {
            Self(s)
        }
    }

    impl std::ops::Deref for Channel {
        type Target = Sender;
        fn deref(&self) -> &Sender {
            &self.0
        }
    }

    impl std::ops::DerefMut for Channel {
        fn deref_mut(&mut self) -> &mut Sender {
            &mut self.0
        }
    }

    impl Drop for Channel {
        fn drop(&mut self) {
            let _ = self.0.start_send(Dummy);
        }
    }

    #[derive(Debug, Clone)]
    pub struct EndpointRef {
        pub(super) reference: Arc<EndpointData>,
        // MUST COME LATER so that the endpoint driver gets notified *after* its reference count is
        // dropped.
        pub(super) channel: Channel,
    }
}

pub(crate) use channel_ref::{Channel, EndpointRef};

/// A QUIC endpoint. Each endpoint has its own configuration and listening socket.
///
/// You generally need only one of these per process. Endpoints are [`Send`] and [`Sync`], so you
/// can share them among as many threads as you like. However, performance may be better if you
/// have one per CPU core, as this reduces lock contention. Most applications will not need to
/// worry about this. `Endpoint` tries to use fine-grained locking to reduce the overhead.
///
/// `Endpoint` wraps the underlying data structure in an [`Arc`], so cloning it just bumps the
/// reference count. All state is shared between the clones. For example, you can pass different
/// clones to [`Transport::listen_on`]. Each incoming connection will be received by exactly one of
/// them.
///
/// The **only** valid [`Multiaddr`] to pass to `listen_on` is the one used to create the
/// `QuicEndpoint`. You can obtain this via the `addr` method. If you pass a different one, you
/// will get [`TransportError::MultiaddrNotSupported`].
#[derive(Debug, Clone)]
pub struct Endpoint(EndpointRef);

#[derive(Debug)]
pub(crate) struct ConnectionEndpoint {
    channel: Channel,
    socket: Arc<socket::Socket>,
}

/// A handle that can be used to wait for the endpoint driver to finish.
pub type JoinHandle = async_std::task::JoinHandle<Result<(), Error>>;

#[derive(Debug)]
pub(crate) struct SendToken(());

impl Endpoint {
    /// Construct a `Endpoint` with the given `Config` and `Multiaddr`.
    pub fn new(
        config: Config,
        mut address: Multiaddr,
    ) -> Result<(Self, JoinHandle), TransportError<<Self as Transport>::Error>> {
        let socket_addr = if let Ok(sa) = multiaddr_to_socketaddr(&address) {
            sa
        } else {
            return Err(TransportError::MultiaddrNotSupported(address));
        };
        // NOT blocking, as per man:bind(2), as we pass an IP address.
        let socket = std::net::UdpSocket::bind(&socket_addr)
            .map_err(|e| TransportError::Other(Error::IO(e)))?;
        let port_is_zero = socket_addr.port() == 0;
        let socket_addr = socket.local_addr().expect("this socket is bound; qed");
        info!("bound socket to {:?}", socket_addr);
        if port_is_zero {
            assert_ne!(socket_addr.port(), 0);
            assert_eq!(address.pop(), Some(Protocol::Quic));
            assert_eq!(address.pop(), Some(Protocol::Udp(0)));
            address.push(Protocol::Udp(socket_addr.port()));
            address.push(Protocol::Quic);
        }
        let (new_connections, receive_connections) = mpsc::unbounded();
        let (event_sender, event_receiver) = mpsc::channel(0);
        if socket_addr.ip().is_unspecified() {
            info!("returning all local IPs for unspecified address");
            let suffixes = [Protocol::Udp(socket_addr.port()), Protocol::Quic];
            let local_addresses =
                host_addresses(&suffixes).map_err(|e| TransportError::Other(Error::IO(e)))?;
            for (_, _, address) in local_addresses {
                info!("sending address {:?}", address);
                new_connections
                    .unbounded_send(ListenerEvent::NewAddress(address))
                    .expect("we have a reference to the peer, so this will not fail; qed")
            }
        } else {
            info!("sending address {:?}", address);
            new_connections
                .unbounded_send(ListenerEvent::NewAddress(address.clone()))
                .expect("we have a reference to the peer, so this will not fail; qed");
        }
        let reference = Arc::new(EndpointData {
            socket: Arc::new(socket::Socket::new(socket.into())),
            inner: Mutex::new(EndpointInner {
                inner: quinn_proto::Endpoint::new(
                    config.endpoint_config.clone(),
                    Some(config.server_config.clone()),
                ),
                muxers: HashMap::new(),
                event_receiver,
                pending: Default::default(),
                buffer: vec![0; 0xFFFF],
            }),
            address: address.clone(),
            receive_connections: Mutex::new(Some(receive_connections)),
            new_connections,
            config,
        });
        let channel = Channel::new(event_sender);
        if socket_addr.ip().is_unspecified() {
            debug!("returning all local IPs for unspecified address");
            let local_addresses =
                host_addresses(&[Protocol::Udp(socket_addr.port()), Protocol::Quic])
                    .map_err(|e| TransportError::Other(Error::IO(e)))?;
            for i in local_addresses {
                info!("sending address {:?}", i.2);
                reference
                    .new_connections
                    .unbounded_send(ListenerEvent::NewAddress(i.2))
                    .expect("we have a reference to the peer, so this will not fail; qed")
            }
        } else {
            info!("sending address {:?}", address);
            reference
                .new_connections
                .unbounded_send(ListenerEvent::NewAddress(address))
                .expect("we have a reference to the peer, so this will not fail; qed");
        }
        let endpoint = EndpointRef { reference, channel };
        let join_handle = spawn(endpoint.clone());
        Ok((Self(endpoint), join_handle))
    }
}

fn create_muxer(
    endpoint: ConnectionEndpoint,
    connection: Connection,
    handle: ConnectionHandle,
    inner: &mut EndpointInner,
) -> Upgrade {
    super::connection::ConnectionDriver::spawn(endpoint, connection, handle, |weak| {
        inner.muxers.insert(handle, weak);
    })
}

fn accept_muxer(
    endpoint: &Arc<EndpointData>,
    connection: Connection,
    handle: ConnectionHandle,
    inner: &mut EndpointInner,
    channel: Channel,
) {
    let connection_endpoint = ConnectionEndpoint {
        socket: endpoint.socket.clone(),
        channel,
    };
    let upgrade = create_muxer(connection_endpoint, connection, handle, &mut *inner);
    if endpoint
        .new_connections
        .unbounded_send(ListenerEvent::Upgrade {
            upgrade,
            local_addr: endpoint.address.clone(),
            remote_addr: endpoint.address.clone(),
        })
        .is_err()
    {
        inner.inner.accept();
        inner.inner.reject_new_connections();
    }
}

impl Future for EndpointRef {
    type Output = Result<(), Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let Self { reference, channel } = self.get_mut();
        let mut inner = reference.inner.lock();
        trace!("driving events");
        inner.drive_events(cx);
        trace!("driving incoming packets");
        match inner.drive_receive(&reference.socket, cx) {
            Poll::Pending => {}
            Poll::Ready(Ok((handle, connection))) => {
                trace!("have a new connection");
                accept_muxer(reference, connection, handle, &mut *inner, channel.clone());
                trace!("connection accepted");
            }
            Poll::Ready(Err(e)) => {
                if reference
                    .new_connections
                    .unbounded_send(ListenerEvent::Error(e))
                    .is_err()
                {
                    inner.inner.reject_new_connections()
                }
            }
        }
        match inner.poll_transmit_pending(&reference.socket, cx)? {
            Poll::Pending | Poll::Ready(()) => {}
        }

        if !inner.muxers.is_empty() {
            Poll::Pending
        } else if Arc::strong_count(&reference) == 1 {
            debug!("All connections have closed. Closing QUIC endpoint driver.");
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }
}

/// A QUIC listener
#[derive(Debug)]
pub struct Listener {
    reference: Arc<EndpointData>,
    /// MUST COME AFTER `reference`!
    drop_notifier: Channel,
    channel: mpsc::UnboundedReceiver<ListenerResult>,
}

impl Stream for Listener {
    type Item = Result<ListenerResult, Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut()
            .channel
            .poll_next_unpin(cx)
            .map(|e| e.map(Ok))
    }
}

impl Transport for Endpoint {
    type Output = (libp2p_core::PeerId, super::Muxer);
    type Error = Error;
    type Listener = Listener;
    type ListenerUpgrade = Upgrade;
    type Dial = Upgrade;

    fn listen_on(self, addr: Multiaddr) -> Result<Self::Listener, TransportError<Self::Error>> {
        let Endpoint(EndpointRef {
            reference,
            channel: drop_notifier,
        }) = self;
        let socket_addr = if let Ok(sa) = multiaddr_to_socketaddr(&addr) {
            sa
        } else {
            return Err(TransportError::MultiaddrNotSupported(addr));
        };
        let own_socket_addr =
            multiaddr_to_socketaddr(&reference.address).expect("our own Multiaddr is valid; qed");
        if socket_addr.ip() != own_socket_addr.ip()
            || (socket_addr.port() != 0 && socket_addr.port() != own_socket_addr.port())
        {
            return Err(TransportError::MultiaddrNotSupported(addr));
        }
        let channel = reference
            .receive_connections
            .lock()
            .take()
            .ok_or_else(|| TransportError::Other(Error::AlreadyListening))?;
        Ok(Listener {
            reference,
            drop_notifier,
            channel,
        })
    }

    fn dial(self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let socket_addr = if let Ok(socket_addr) = multiaddr_to_socketaddr(&addr) {
            if socket_addr.port() == 0 || socket_addr.ip().is_unspecified() {
                debug!("Instantly refusing dialing {}, as it is invalid", addr);
                return Err(TransportError::MultiaddrNotSupported(addr));
            }
            socket_addr
        } else {
            return Err(TransportError::MultiaddrNotSupported(addr));
        };
        let Endpoint(endpoint) = self;
        let mut inner = endpoint.reference.inner.lock();
        let s: Result<(_, Connection), _> = inner
            .inner
            .connect(
                endpoint.reference.config.client_config.clone(),
                socket_addr,
                "l",
            )
            .map_err(|e| {
                warn!("Connection error: {:?}", e);
                TransportError::Other(Error::CannotConnect(e))
            });
        let (handle, conn) = s?;
        let socket = endpoint.reference.socket.clone();
        let endpoint = ConnectionEndpoint {
            socket,
            channel: endpoint.channel,
        };
        Ok(super::connection::ConnectionDriver::spawn(
            endpoint,
            conn,
            handle,
            |weak| {
                inner.muxers.insert(handle, weak);
            },
        ))
    }
}

// This type of logic should probably be moved into the multiaddr package
fn multiaddr_to_socketaddr(addr: &Multiaddr) -> Result<SocketAddr, ()> {
    let mut iter = addr.iter();
    let proto1 = iter.next().ok_or(())?;
    let proto2 = iter.next().ok_or(())?;
    let proto3 = iter.next().ok_or(())?;

    if iter.next().is_some() {
        return Err(());
    }

    match (proto1, proto2, proto3) {
        (Protocol::Ip4(ip), Protocol::Udp(port), Protocol::Quic) => {
            Ok(SocketAddr::new(ip.into(), port))
        }
        (Protocol::Ip6(ip), Protocol::Udp(port), Protocol::Quic) => {
            Ok(SocketAddr::new(ip.into(), port))
        }
        _ => Err(()),
    }
}

#[cfg(test)]
#[test]
fn multiaddr_to_udp_conversion() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use tracing_subscriber::{fmt::Subscriber, EnvFilter};
    let _ = Subscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();
    assert!(
        multiaddr_to_socketaddr(&"/ip4/127.0.0.1/udp/1234".parse::<Multiaddr>().unwrap()).is_err()
    );

    assert_eq!(
        multiaddr_to_socketaddr(
            &"/ip4/127.0.0.1/udp/12345/quic"
                .parse::<Multiaddr>()
                .unwrap()
        ),
        Ok(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            12345,
        ))
    );
    assert_eq!(
        multiaddr_to_socketaddr(
            &"/ip4/255.255.255.255/udp/8080/quic"
                .parse::<Multiaddr>()
                .unwrap()
        ),
        Ok(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)),
            8080,
        ))
    );
    assert_eq!(
        multiaddr_to_socketaddr(&"/ip6/::1/udp/12345/quic".parse::<Multiaddr>().unwrap()),
        Ok(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)),
            12345,
        ))
    );
    assert_eq!(
        multiaddr_to_socketaddr(
            &"/ip6/ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/udp/8080/quic"
                .parse::<Multiaddr>()
                .unwrap()
        ),
        Ok(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(
                65535, 65535, 65535, 65535, 65535, 65535, 65535, 65535,
            )),
            8080,
        ))
    );
}
