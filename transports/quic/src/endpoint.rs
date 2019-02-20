// Copyright 2019 Parity Technologies (UK) Ltd.
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

//! Actual QUIC state machine.
//!
//! The `QuicEndpoint` struct wraps around a `quinn_proto::Endpoint` and adds sockets and
//! everything related to I/O.

use bytes::BytesMut;
use fnv::FnvHashMap;
use futures::{future::{self, FutureResult}, prelude::*, sync::mpsc, sync::oneshot, task};
use log::{debug, trace, warn};
use multiaddr::{Multiaddr, Protocol};
use multihash::Multihash;
use parking_lot::Mutex;
use simple_asn1::oid;
use smallvec::SmallVec;
use std::{
    cmp,
    fmt, io, iter,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::{atomic, Arc, Weak},
    time::Duration, time::Instant,
};
use tokio_udp::UdpSocket;

/// Background task dedicated to processing QUIC connections.
pub struct QuicEndpoint {
    /// Sender for events from the public API. Processed in the background task.
    outside_events: mpsc::UnboundedSender<OutToInMessage>,

    /// If false, we need to start a background task that will process the sockets.
    /// The background task is passed an `Arc<QuicEndpoint>`.
    task_started: atomic::AtomicBool,

    /// The inner structure, behind a `Mutex`. Mainly locked from within the background task, but
    /// also sometimes from the outside.
    inner: Mutex<QuicEndpointInner>,
}

/// An active connection. Exposed in the public API of this module.
#[derive(Debug)]        // TODO: better Debug
pub struct QuicConnec {
    /// Identifier for this connection in the background task state machine.
    connection_handle: quinn_proto::ConnectionHandle,
}

/// An active substream. Exposed in the public API of this module.
#[derive(Debug)]        // TODO: better Debug
pub struct QuicSub {
    /// Identifier for this substream in the background task state machine.
    id: quinn_proto::StreamId,
}

/// Message sent from the the public API of the endpoint to the background task.
enum OutToInMessage {
    /// Start connecting to a remote.
    Connect {
        /// Address of the remote.
        target: SocketAddr,
        /// Channel used to send back the result once connected.
        send_back: oneshot::Sender<Result<QuicConnec, io::Error>>,
    },
    /// Open an outbound substream.
    OpenSubstream {
        /// Id of the connection to open a substream on.
        connection: quinn_proto::ConnectionHandle,
        /// Channel used to send back the result. Will return `None` if we have reached the maximum
        /// number of substreams.
        send_back: oneshot::Sender<Option<QuicSub>>,
    },
    /// Shuts down a substream.
    ShutdownSubstream {
        /// Id of the connection that owns the substream.
        connection: quinn_proto::ConnectionHandle,
        /// Id of the substream to shut down.
        stream: quinn_proto::StreamId,
    },
    /// Destroys a substream.
    // TODO: necessary?
    DestroySubstream {
        /// Id of the connection that owns the substream.
        connection: quinn_proto::ConnectionHandle,
        /// Id of the substream to destroy.
        stream: quinn_proto::StreamId,
    },
}

/// Internal handling of QUIC.
struct QuicEndpointInner {
    /// State machine that handles the protocol.
    quinn: quinn_proto::Endpoint,
    /// Receiver for messages sent through the public API.
    outside_events: mpsc::UnboundedReceiver<OutToInMessage>,
    /// List of active UDP sockets.
    sockets: SmallVec<[QuicSocket; 1]>,
    /// List of open connections.
    // TODO: something more efficient so that we can insert/remove connections in the middle?
    connections: Vec<QuicConnection>,
    /// Configuration for outbound connections.
    client_config: Arc<quinn_proto::ClientConfig>,
    /// Temporary buffer used for all readings. We always read inside that same buffer, so that we
    /// don't have to reallocate one every time.
    // Note: `QuicEndpointInner` is always in an `Arc`, so it's not a problem to use a large buffer.
    // TODO: is this buffer large enough? it seems that we silently fail if it isn't
    read_buffer: [u8; 2048],
}

/// State of a socket managed by the background task.
struct QuicSocket {
    /// The actual socket object.
    socket: UdpSocket,
    /// If Some, we are "listening" on this socket and thus it shouldn't be closed. If None,
    /// we also deny any new incoming connection on this socket.
    listening: Option<mpsc::Sender<(QuicConnec, SocketAddr)>>,
    /// Address we are binded to.
    addr: SocketAddr,
}

/// An active connection in the backgroud task state machine.
struct QuicConnection {
    /// Identifier of the connection within the global state machine.
    handle: quinn_proto::ConnectionHandle,
    /// Addr the socket is operating on. This is used to match the element in `QuicSocket`.
    socket_addr: SocketAddr,
    /// List of substreams that have been opened on the connection.
    substreams: FnvHashMap<quinn_proto::StreamId, QuicSubstream>,
    /// List of timers that have been started.
    timers: SmallVec<[(quinn_proto::Timer, tokio_timer::Delay); 4]>,
    /// Sender to trigger when the connection is established.
    send_back: Option<oneshot::Sender<Result<QuicConnec, io::Error>>>,
}

struct QuicSubstream {
    read_notify: Option<task::Task>,
    write_notify: Option<task::Task>,
}

impl QuicEndpoint {
    /// Initializes a new `QuicEndpoint`.
    pub fn new(keypair: libp2p_core::identity::Keypair) -> Result<Arc<QuicEndpoint>, quinn_proto::EndpointError> {
        let (tx, rx) = mpsc::unbounded();

        let (certificates, private_key) = {
            let key = openssl::rsa::Rsa::generate(2048).unwrap();

            let mut certif = openssl::x509::X509Builder::new().unwrap();        // TODO:
            certif.set_version(2).unwrap();
            let mut serial: [u8; 20] = rand::random();
            certif.set_serial_number(&openssl::asn1::Asn1Integer::from_bn(&openssl::bn::BigNum::from_slice(&serial[..]).unwrap()).unwrap()).unwrap();
            let mut x509_name = openssl::x509::X509NameBuilder::new().unwrap();
            x509_name.append_entry_by_text("C", "US").unwrap();
            x509_name.append_entry_by_text("ST", "CA").unwrap();
            x509_name.append_entry_by_text("O", "Some organization").unwrap();
            x509_name.append_entry_by_text("CN", "libp2p.io").unwrap();
            let x509_name = x509_name.build();

            certif.set_issuer_name(&x509_name).unwrap();
            certif.set_subject_name(&x509_name).unwrap();
            // TODO: libp2p specs says we shouldn't have these date fields
            certif.set_not_before(&openssl::asn1::Asn1Time::days_from_now(0).unwrap()).unwrap();
            certif.set_not_after(&openssl::asn1::Asn1Time::days_from_now(365).unwrap()).unwrap();
            certif.set_pubkey(openssl::pkey::PKey::from_rsa(key.clone()).unwrap().as_ref()).unwrap();        // TODO: unwrap

            let ext = openssl::x509::extension::BasicConstraints::new()
                .critical()
                //.ca()
                .build().unwrap();
            certif.append_extension(ext).unwrap();

            let ext = openssl::x509::extension::SubjectKeyIdentifier::new()
                .build(&certif.x509v3_context(None, None)).unwrap();
            certif.append_extension(ext).unwrap();

            let ext = openssl::x509::extension::AuthorityKeyIdentifier::new()
                .issuer(true)
                .keyid(true)
                .build(&certif.x509v3_context(None, None)).unwrap();
            certif.append_extension(ext).unwrap();

            let ext = openssl::x509::extension::SubjectAlternativeName::new()
                .dns("libp2p.io")    // TODO: must match the domain name in the QUIC requests being made
                .build(&certif.x509v3_context(None, None)).unwrap();
            certif.append_extension(ext).unwrap();

            // TODO:
            /*{
                let ext = format!("publicKey={}", bs58::encode(keypair.public().into_protobuf_encoding()).into_string());  // TODO: signature
                certif.append_extension(openssl::x509::X509Extension::new(None, None, "1.3.6.1.4.1.53594.1.1", &ext).unwrap());
            }*/

            certif.sign(&openssl::pkey::PKey::from_rsa(key.clone()).unwrap(), openssl::hash::MessageDigest::sha256()).unwrap();
            let certif_gen = certif.build();
            debug_assert!(certif_gen.verify(&openssl::pkey::PKey::from_rsa(key.clone()).unwrap()).unwrap());
            let pkey_bytes = key.private_key_to_der().unwrap();
            (vec![rustls::Certificate(certif_gen.to_der().unwrap())], rustls::PrivateKey(pkey_bytes))
        };

        let client_config = {
            struct DummyVerifier;//(Mutex<Option<PeerId>>);
            impl rustls::ServerCertVerifier for DummyVerifier {
                fn verify_server_cert(&self,
                    _: &rustls::RootCertStore,
                    certs: &[rustls::Certificate],
                    _: webpki::DNSNameRef,
                    _ocsp_response: &[u8]) -> Result<rustls::ServerCertVerified, rustls::TLSError>
                {
                    println!("blocks: {:?}", simple_asn1::from_der(&certs[0].0));
                    Ok(rustls::ServerCertVerified::assertion())
                }
            }

            let mut cfg = quinn_proto::ClientConfig::new();
            cfg.versions = vec![rustls::ProtocolVersion::TLSv1_3];
            cfg.enable_sni = false;
            cfg.enable_early_data = true;
            cfg.key_log = Arc::new(rustls::KeyLogFile::new());
            cfg.set_single_client_cert(certificates.clone(), private_key.clone());
            cfg.dangerous().set_certificate_verifier(Arc::new(DummyVerifier));
            Arc::new(cfg)
        };

        let server_config = {
            let mut rustls_cfg = rustls::ServerConfig::new(rustls::NoClientAuth::new());    // TODO: no, authenticate client
            rustls_cfg.versions = vec![rustls::ProtocolVersion::TLSv1_3];
            rustls_cfg.key_log = Arc::new(rustls::KeyLogFile::new());
            rustls_cfg.set_single_cert(certificates, private_key).unwrap();

            let mut cfg = quinn_proto::ServerConfig::default();
            cfg.tls_config = Arc::new(rustls_cfg);
            cfg
        };

        Ok(Arc::new(QuicEndpoint {
            outside_events: tx,
            inner: Mutex::new(QuicEndpointInner {
                quinn: quinn_proto::Endpoint::new(
                    slog::Logger::root(slog::Discard, slog::o!()),
                    quinn_proto::Config::default(),
                    Some(server_config),
                )?,
                outside_events: rx,
                connections: Vec::with_capacity(32),
                sockets: SmallVec::new(),
                client_config,
                read_buffer: [0; 2048],
            }),
            task_started: atomic::AtomicBool::new(false),
        }))
    }

    /// Ensures that the QUIC background task has properly started. Starts it if it hasn't.
    fn ensure_task_started(self: Arc<Self>) {
        if self.task_started.swap(true, atomic::Ordering::Relaxed) {
            let me = Arc::downgrade(&self);
            tokio_executor::spawn(future::poll_fn(move || {
                if let Some(me) = me.upgrade() {
                    me.inner.lock().poll();
                    Ok(Async::NotReady)
                } else {
                    Ok(Async::Ready(()))
                }
            }));
        }
    }

    /// Start listening on an address. Returns a channel that will automatically produce
    /// connections.
    ///
    /// Dropping the receiver will close the listening.
    pub(crate) fn start_listening(self: Arc<Self>, addr: SocketAddr)
        -> io::Result<(mpsc::Receiver<(QuicConnec, SocketAddr)>, SocketAddr)>
    {
        self.clone().ensure_task_started();

        let (tx, rx) = mpsc::channel(2);

        let mut inner = self.inner.lock();
        if let Some(socket) = inner.sockets.iter_mut().find(|s| s.addr == addr) {
            if socket.listening.as_ref().map(|tx| !tx.is_closed()).unwrap_or(false) {
                return Err(io::ErrorKind::AddrInUse.into());
            } else {
                socket.listening = Some(tx);
                return Ok((rx, socket.addr));
            }
        }

        let socket = UdpSocket::bind(&addr)?;
        let actual_addr = socket.local_addr()?;

        inner.sockets.push(QuicSocket {
            socket,
            listening: Some(tx),
            addr: actual_addr.clone(),
        });

        Ok((rx, actual_addr))
    }

    /// Start connecting to the given target. Returns a future that resolves when we finish
    /// connecting.
    pub(crate) fn connect(self: Arc<Self>, target: SocketAddr)
        -> oneshot::Receiver<Result<QuicConnec, io::Error>>
    {
        self.clone().ensure_task_started();

        // TODO: have a config option for the local port to use when dialing

        let (send_back, rx) = oneshot::channel();
        self.outside_events.unbounded_send(OutToInMessage::Connect { target, send_back })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
        rx
    }

    /// Open a substream with the target.
    pub(crate) fn open_substream(&self, connection: &QuicConnec)
        -> oneshot::Receiver<Option<QuicSub>>
    {
        let (send_back, rx) = oneshot::channel();
        self.outside_events.unbounded_send(OutToInMessage::OpenSubstream { connection: connection.connection_handle, send_back })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
        rx
    }

    /// Try to read data from a substream.
    ///
    /// The API is similar to the one of `StreamMuxer::read_substream`.
    pub(crate) fn read_substream(&self, connection: &QuicConnec, stream: &QuicSub, buf: &mut [u8]) -> Poll<usize, io::Error> {
        let mut inner = self.inner.lock();
        match inner.quinn.read(connection.connection_handle, stream.id, buf) {
            Ok(n) => Ok(Async::Ready(n)),
            Err(quinn_proto::ReadError::Blocked) => {
                Ok(Async::NotReady)   // FIXME: notify task
            },
            Err(quinn_proto::ReadError::Finished) => Ok(Async::Ready(0)),
            Err(quinn_proto::ReadError::Reset { .. }) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Try to write data to a substream.
    ///
    /// The API is similar to the one of `StreamMuxer::write_substream`.
    pub(crate) fn write_substream(&self, connection: &QuicConnec, stream: &QuicSub, data: &[u8]) -> Poll<usize, io::Error> {
        let mut inner = self.inner.lock();
        match inner.quinn.write(connection.connection_handle, stream.id, data) {
            Ok(n) => Ok(Async::Ready(n)),
            Err(quinn_proto::WriteError::Blocked) => {
                Ok(Async::NotReady)   // FIXME: notify task
            },
            Err(quinn_proto::WriteError::Stopped { .. }) => Err(io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Shuts down the substream.
    ///
    /// Has no effect is the substream has already been shut down or destroyed.
    pub(crate) fn shutdown_substream(&self, connection: &QuicConnec, stream: &QuicSub) {
        self.outside_events
            .unbounded_send(OutToInMessage::ShutdownSubstream { connection: connection.connection_handle, stream: stream.id })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
    }

    /// Destroys the given substream.
    ///
    /// Has no effect is the substream has already been destroyed.
    pub(crate) fn destroy_substream(&self, connection: &QuicConnec, stream: QuicSub) {
        self.outside_events
            .unbounded_send(OutToInMessage::DestroySubstream { connection: connection.connection_handle, stream: stream.id })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
    }
}

impl fmt::Debug for QuicEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("QuicEndpoint")
            // TODO:
            .finish()
    }
}

impl QuicEndpointInner {
    /// Processes the QUIC connection. The future never ends and never produces a result. In other
    /// words, the result of this method should be considered as `Ok(Async::NotReady)`.
    fn poll(&mut self) {
        loop {
            // If we set this variable to `true` in the loop, we're going to `continue`.
            let mut need_repolling = false;

            let now = now();

            loop {
                match self.outside_events.poll() {
                    Ok(Async::Ready(Some(OutToInMessage::Connect { target, send_back }))) => {
                        let socket_addr = match self.assign_dialing_socket(&target) {
                            Ok(p) => p,
                            Err(err) => { let _ = send_back.send(Err(err)); continue; }
                        };

                        match self.quinn.connect(target, &self.client_config, "libp2p.io") {        // TODO: Must match the certificate
                            Ok(connection_id) => {
                                self.connections.push(QuicConnection {
                                    handle: connection_id,
                                    socket_addr,
                                    substreams: Default::default(),
                                    timers: SmallVec::new(),
                                    send_back: Some(send_back),
                                });
                            },
                            Err(err) => {
                                panic!("{:?}", err);        // TODO:
                                //let _ = send_back.send(Err(err));
                            }
                        }
                    },
                    Ok(Async::Ready(Some(OutToInMessage::OpenSubstream { connection, send_back }))) => {
                        let result = self.quinn.open(connection, quinn_proto::Directionality::Bi);
                        if let Some(stream) = result {
                            self.connections
                                .iter_mut().find(|c| c.handle == connection)
                                .expect("The list of connections is kept in sync with the external API; QED")
                                .substreams
                                .insert(stream, QuicSubstream {
                                    read_notify: None,
                                    write_notify: None,
                                });
                        }
                        let _ = send_back.send(result.map(|id| QuicSub { id }));
                    },
                    Ok(Async::Ready(Some(OutToInMessage::ShutdownSubstream { connection, stream }))) => {
                        self.quinn.finish(connection, stream);
                    },
                    Ok(Async::Ready(Some(OutToInMessage::DestroySubstream { connection, stream }))) => {
                        // TODO: call reset? not sure
                        if let Some(connection) = self.connections.iter_mut().find(|c| c.handle == connection) {
                            connection.substreams.remove(&stream);
                        }
                    },
                    Ok(Async::NotReady) => break,
                    Ok(Async::Ready(None)) | Err(_) => {
                        unreachable!("The sender is in the QuicEndpoint, which we know is alive \
                                      because we have a borrow one of its fields; QED")
                    },
                }
            }

            for socket in self.sockets.iter_mut() {
                // TODO: call quinn.close on error
                while let Async::Ready((num_read, from)) = socket.socket.poll_recv_from(&mut self.read_buffer).unwrap() {  // TODO: don't unwrap :-/
                    let data = &self.read_buffer[..num_read];
                    self.quinn.handle(now, from, None, data.into());
                }

                // TODO: obviously bad
                loop {
                    // TODO: don't unwrap
                    if let Async::NotReady = socket.socket.poll_write_ready().unwrap() {
                        break;
                    }

                    if let Some(transmit) = self.quinn.poll_transmit(now) {
                        match socket.socket.poll_send_to(&transmit.packet, &transmit.destination) {
                            Ok(Async::Ready(n)) => assert_eq!(n, transmit.packet.len()),
                            other => panic!("{:?}", other)       // TODO: no
                        }
                    } else {
                        break;
                    }
                }
            }

            while let Some((connection_id, event)) = self.quinn.poll() {
                println!("event: {:?} {:?}", connection_id, event);

                let connection = if let quinn_proto::Event::Handshaking = event {
                    debug_assert!(self.connections.iter().all(|c| c.handle != connection_id));
                    let socket = self.sockets.iter_mut().next().unwrap();       // TODO: no
                    if let Some(tx) = socket.listening.as_mut() {
                        // TODO: so wrong
                        let _ = tx.start_send((QuicConnec {
                            connection_handle: connection_id,
                        }, socket.addr));       // TODO: socket.addr is wrong; has to be the remote addr
                        let _ = tx.poll_complete();
                    }

                    self.connections.push(QuicConnection {
                        handle: connection_id,
                        socket_addr: socket.addr,
                        substreams: Default::default(),
                        timers: SmallVec::new(),
                        send_back: None,        // TODO: store something actually, and don't use a FutureResult in lib.rs
                    });
                    self.connections.last_mut().expect("we just pushed to this list; QED")

                } else {
                    self.connections.iter_mut().find(|c| c.handle == connection_id)
                        .expect("self.connections is always tracking the currently active connections; QED")
                };

                match event {
                    quinn_proto::Event::Handshaking => {
                        self.quinn.accept();
                        need_repolling = true;
                    },
                    quinn_proto::Event::Connected => {
                        // TODO: assert that it's there, once the listening side also uses that
                        if let Some(send_back) = connection.send_back.take() {
                            let _ = send_back.send(Ok(QuicConnec { connection_handle: connection.handle }));
                        }
                    },
                    quinn_proto::Event::ConnectionLost { reason } => {
                        panic!("lost connec {:?}", reason);
                    },
                    quinn_proto::Event::StreamOpened => {
                        let stream = self.quinn.accept_stream(connection_id)
                            .expect("accept_stream is guaranteed to return Some after StreamOpened; QED");
                        connection.substreams.insert(stream, QuicSubstream {
                            read_notify: None,
                            write_notify: None,
                        });
                        // TODO: notify
                    },
                    quinn_proto::Event::StreamReadable { stream } => {
                        let substream = connection.substreams
                            .get_mut(&stream)
                            .expect("connection.substreams always follow the open substreams; QED");
                        if let Some(task) = substream.read_notify.take() {
                            task.notify();
                        }
                    },
                    quinn_proto::Event::StreamWritable { stream } => {
                        let substream = connection.substreams
                            .get_mut(&stream)
                            .expect("connection.substreams always follow the open substreams; QED");
                        if let Some(task) = substream.write_notify.take() {
                            task.notify();
                        }
                    },
                    quinn_proto::Event::StreamFinished { stream } => {
                        let substream = connection.substreams
                            .get_mut(&stream)
                            .expect("connection.substreams always follow the open substreams; QED");
                        if let Some(task) = substream.read_notify.take() {
                            task.notify();
                        }
                        if let Some(task) = substream.write_notify.take() {
                            task.notify();
                        }
                    },
                    quinn_proto::Event::StreamAvailable { .. } => {},
                }
            }

            while let Some((connection_id, timer_update)) = self.quinn.poll_timers() {
                self.connections.iter_mut()
                    .find(|c| c.handle == connection_id)
                    .expect("connections always follows the quinn state machine; QED")
                    .inject_update(timer_update);
            }

            for connection in self.connections.iter_mut() {
                while let Async::Ready(timer) = connection.poll_timeouts() {
                    self.quinn.timeout(now, connection.handle, timer);
                    need_repolling = true;
                }
            }

            if !need_repolling {
                break;
            }
        }
    }

    /// Chooses a socket to use to dial the target address and returns its local address, or
    /// creates a new socket.
    fn assign_dialing_socket(&mut self, target: &SocketAddr) -> Result<SocketAddr, io::Error> {
        // TODO: IPv4 must only be assigned to IPv4 sockets and same for IPv6
        Ok(self.sockets.iter().next().unwrap().addr.clone())     // TODO: no
    }
}

impl QuicConnection {
    /// Modifies the timers to take the update into account.
    fn inject_update(&mut self, update: quinn_proto::TimerUpdate) {
        match update.update {
            quinn_proto::TimerSetting::Start(expiration) => {
                let delay = tokio_timer::Delay::new(*EPOCH + Duration::from_millis(expiration));
                self.timers.push((update.timer, delay));
            },
            quinn_proto::TimerSetting::Stop => {
                self.timers.retain(|(t, _)| *t != update.timer);
            },
        }
    }

    /// Polls any of the timers to see if there's a timeout.
    fn poll_timeouts(&mut self) -> Async<quinn_proto::Timer> {
        let pos_finished = self.timers.iter_mut().position(|(_, delay)| {
            match delay.poll() {
                Ok(Async::Ready(())) => true,
                Ok(Async::NotReady) => false,
                Err(_) => true,
            }
        });

        if let Some(pos_finished) = pos_finished {
            Async::Ready(self.timers.remove(pos_finished).0)
        } else {
            Async::NotReady
        }
    }
}

lazy_static::lazy_static! {
    /// Arbitrary EPOCH from which `now()` is being calculated.
    static ref EPOCH: Instant = Instant::now();
}

/// Returns the number of milliseconds that have passed since an arbitrary EPOCH.
fn now() -> u64 {
    let elapsed = EPOCH.elapsed();
    elapsed.as_secs().saturating_mul(1_000).saturating_add(u64::from(elapsed.subsec_millis()))
}
