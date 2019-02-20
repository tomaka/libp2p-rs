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

//! Background task whose job is to wake up whenever something happens on any of the sockets, and
//! to notify the right substream.

use bytes::BytesMut;
use fnv::FnvHashMap;
use futures::{future::{self, FutureResult}, prelude::*, sync::mpsc, sync::oneshot, task};
use log::{debug, trace, warn};
use multiaddr::{Multiaddr, Protocol};
use multihash::Multihash;
use parking_lot::Mutex;
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
    task_started: atomic::AtomicBool,
    /// The inner structure, behind a `Mutex`.
    inner: Mutex<QuicEndpointInner>,
}

/// Message sent from the the public API of the endpoint to the background task.
enum OutToInMessage {
    /// Start connecting to a remote.
    Connect {
        /// Address of the remote.
        target: SocketAddr,
        /// Channel used to send back the result once connected.
        send_back: oneshot::Sender<Result<(), io::Error>>,
    },
    /// Open an outbound substream.
    OpenSubstream {
        /// Id of the connection to open a substream on.
        connection: quinn_proto::ConnectionHandle,
        /// Channel used to send back the result. Will return `None` if we have reached the maximum
        /// number of substreams.
        send_back: oneshot::Sender<Option<quinn_proto::StreamId>>,
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
    /// We always read inside that buffer, so that we don't have to reallocate one every time.
    // Note: `QuicEndpointInner` is always in an `Arc`, so it's not a problem to use a large buffer.
    read_buffer: [u8; 1024],
}

struct QuicSocket {
    /// The actual socket object.
    socket: UdpSocket,
    /// Port we are binded to.
    port: u16,
    /// Buffer of data waiting to be written on `socket` as soon as it is ready.
    pending_write: BytesMut,
}

struct QuicConnection {
    /// Identifier of the connection within the global state machine.
    handle: quinn_proto::ConnectionHandle,
    /// Port the socket is operating on. This is used to match the element in `QuicSocket`.
    socket_port: u16,
    /// List of substreams that have been opened on the connection.
    substreams: FnvHashMap<quinn_proto::StreamId, QuicSubstream>,
    /// List of timers that have been started.
    timers: SmallVec<[(quinn_proto::Timer, tokio_timer::Delay); 4]>,
}

struct QuicSubstream {
    read_notify: Option<task::Task>,
    write_notify: Option<task::Task>,
}

impl QuicEndpoint {
    /// Initializes a new `QuicEndpoint`.
    pub fn new() -> Result<Arc<QuicEndpoint>, quinn_proto::EndpointError> {
        let (tx, rx) = mpsc::unbounded();

        Ok(Arc::new(QuicEndpoint {
            outside_events: tx,
            inner: Mutex::new(QuicEndpointInner {
                quinn: quinn_proto::Endpoint::new(
                    slog::Logger::root(slog::Discard, slog::o!()),
                    quinn_proto::Config::default(),
                    Some(quinn_proto::ServerConfig::default()),
                )?,
                outside_events: rx,
                connections: Vec::with_capacity(32),
                sockets: SmallVec::new(),
                client_config: Arc::new(quinn_proto::ClientConfig::new()),
                read_buffer: [0; 1024],
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

    /// Start connecting to the given target. Returns a future that resolves when we finish
    /// connecting.
    pub(crate) fn connect(self: Arc<Self>, target: SocketAddr)
        -> oneshot::Receiver<Result<quinn_proto::ConnectionHandle, io::Error>>
    {
        self.ensure_task_started();

        // TODO: have a config option for the port to dial when dialing

        let (send_back, rx) = oneshot::channel();
        self.outside_events.unbounded_send(OutToInMessage::Connect { target, send_back })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
        rx
    }

    /// Open a substream with the target.
    pub(crate) fn open_substream(&self, connection: quinn_proto::ConnectionHandle)
        -> oneshot::Receiver<Option<quinn_proto::StreamId>>
    {
        let (send_back, rx) = oneshot::channel();
        self.outside_events.unbounded_send(OutToInMessage::OpenSubstream { connection, send_back })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
        rx
    }

    /// Try to read data from a substream.
    ///
    /// The API is similar to the one of `StreamMuxer::read_substream`.
    pub(crate) fn read_substream(&self, connection: quinn_proto::ConnectionHandle, stream: quinn_proto::StreamId, buf: &mut [u8]) -> Poll<usize, io::Error> {
        match self.inner.lock().quinn.read(connection, stream, buf) {
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
    pub(crate) fn write_substream(&self, connection: quinn_proto::ConnectionHandle, stream: quinn_proto::StreamId, data: &[u8]) -> Poll<usize, io::Error> {
        match self.inner.lock().quinn.write(connection, stream, data) {
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
    pub(crate) fn shutdown_substream(&self, connection: quinn_proto::ConnectionHandle, stream: quinn_proto::StreamId) {
        self.outside_events.unbounded_send(OutToInMessage::ShutdownSubstream { connection, stream })
            .expect("The receiver is stored in self as well, therefore the sender can never \
                     disconnect; QED");
    }

    /// Destroys the given substream.
    ///
    /// Has no effect is the substream has already been destroyed.
    pub(crate) fn destroy_substream(&self, connection: quinn_proto::ConnectionHandle, stream: quinn_proto::StreamId) {
        self.outside_events.unbounded_send(OutToInMessage::DestroySubstream { connection, stream })
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
                        let socket_port = match self.assign_dialing_socket(&target) {
                            Ok(p) => p,
                            Err(err) => { let _ = send_back.send(Err(err)); continue; }
                        };

                        match self.quinn.connect(target, &self.client_config, "") {
                            Ok(connection_id) => {
                                self.connections.push(QuicConnection {
                                    handle: connection_id,
                                    socket_port,
                                    timers: SmallVec::new(),
                                    // TODO: store send_back here
                                });
                            },
                            Err(err) => {
                                let _ = send_back.send(Err(err));
                            }
                        }
                    },
                    Ok(Async::Ready(Some(OutToInMessage::OpenSubstream { connection, send_back }))) => {
                        let result = self.quinn.open(connection, quinn_proto::Directionality::Bi);
                        if let Ok(stream) = result {
                            connection.substreams.insert(stream, QuicSubstream {
                                read_notify: None,
                                write_notify: None,
                            });
                        }
                        let _ = send_back.send(result);
                    },
                    Ok(Async::Ready(Some(OutToInMessage::ShutdownSubstream { connection, stream }))) => {
                        self.quinn.finish(connection, stream);
                    },
                    Ok(Async::Ready(Some(OutToInMessage::DestroySubstream { connection, stream }))) => {
                        // TODO: call reset? not sure
                        if let Some(connection) = match self.connections.iter_mut().find(|c| c.handle == connection) {
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
            }

            if self.sockets.iter().all(|s| s.is_ready_to_write()) {
                while let Some(transmit) = self.quinn.poll_transmit(now) {
                    // TODO: polling
                    // TODO: quinn supports congestion control bits, but we can't use them in a cross-platform way
                    self.sockets[0].socket.poll_send_to(&transmit.packet, transmit.destination);
                }
            }

            while let Some((connection_id, event)) = self.quinn.poll() {
                let connection = self.connections.iter_mut().find(|c| c.handle == connection_id)
                    .expect("self.connections is always tracking the currently active connections; QED");

                match event {
                    quinn_proto::Event::Handshaking => {
                        self.quinn.accept();
                        need_repolling = true;
                    },
                    quinn_proto::Event::Connected => {

                    },
                    quinn_proto::Event::ConnectionLost { reason } => {

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
                    self.quinn.timeout(now(), connection.handle, timer);
                    need_repolling = true;
                }
            }

            if !need_repolling {
                break;
            }
        }
    }

    /// Chooses a socket to use to dial the target address and returns its local port, or creates
    /// a new socket.
    fn assign_dialing_socket(&mut self, target: &SocketAddr) -> Result<u16, io::Error> {
        unimplemented!()
    }
}

impl QuicSocket {
    /// Returns true if the socket is ready to be written to.
    fn is_ready_to_write(&self) -> bool {
        unimplemented!()
    }

    fn write(&mut self, data: &[u8]) {
        unimplemented!()
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
