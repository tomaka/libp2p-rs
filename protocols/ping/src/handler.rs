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
use libp2p_core::{ConnectionUpgrade, nodes::protocol_handler::ProtocolHandler};
use libp2p_core::nodes::handled_node::{NodeHandlerEvent, NodeHandlerEndpoint};
use std::io;
use std::time::{Duration, Instant};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_timer::Delay;
use void::Void;
use {Ping, PingOutput, PingDialer, PingListener};

/// Duration after which we consider that a ping failed.
const PING_TIMEOUT: Duration = Duration::from_secs(30);
/// After a ping succeeded, wait this long before the next ping.
const DELAY_TO_NEXT_PING: Duration = Duration::from_secs(15);
/// Delay between the moment we connect and the first time we ping.
const DELAY_TO_FIRST_PING: Duration = Duration::from_secs(5);

pub struct PeriodicPingHandler<TSubstream> {
    /// Configuration for the ping protocol.
    ping_config: Ping<Instant>,

    /// True if we're upgrading a dialing ping substream.
    upgrading: bool,
    /// Substream open for sending pings, if any.
    ping_out_substream: Option<PingDialer<TSubstream, Instant>>,
    /// Active pinging attempt with the moment it expires.
    active_ping_out: Option<Delay>,
    /// Substreams open for receiving pings.
    ping_in_substreams: Vec<PingListener<TSubstream>>,
    /// Future that fires when we need to ping the node again.
    ///
    /// Every time we receive a pong, we reset the timer to the next time.
    next_ping: Delay,
}

/// Event produced by the periodic pinger.
#[derive(Debug, Copy, Clone)]
pub enum OutEvent {
    /// The node has been determined to be unresponsive.
    Unresponsive,

    /// Started pinging the remote. This can be used to print a diagnostic message in the logs.
    PingStart,

    /// The node has successfully responded to a ping.
    PingSuccess(Duration),
}

impl<TSubstream> PeriodicPingHandler<TSubstream> {
    pub fn new() -> PeriodicPingHandler<TSubstream> {
        PeriodicPingHandler {
            ping_config: Default::default(),
            ping_in_substreams: Vec::with_capacity(1),
            ping_out_substream: None,
            active_ping_out: None,
            next_ping: Delay::new(Instant::now() + DELAY_TO_FIRST_PING),
            upgrading: false,
        }
    }
}

impl<TSubstream> ProtocolHandler<TSubstream> for PeriodicPingHandler<TSubstream>
where TSubstream: AsyncRead + AsyncWrite,
{
    type InEvent = Void;
    type OutEvent = OutEvent;
    type Protocol = Ping<Instant>;
    type OutboundOpenInfo = ();

    #[inline]
    fn protocol(&self) -> Self::Protocol {
        self.ping_config
    }

    fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ConnectionUpgrade<TSubstream>>::Output, _endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        match protocol {
            PingOutput::Pinger(mut dialer) => {
                // TODO: self.cancel_dial_upgrade(&UpgradePurpose::Ping); ???
                // We always open the ping substream for a reason, which is to immediately ping.
                self.upgrading = false;
                if let Some(_) = self.active_ping_out.take() {
                    dialer.ping(Instant::now());
                }
                self.ping_out_substream = Some(dialer);
            },
            PingOutput::Ponger(listener) => {
                self.ping_in_substreams.push(listener);
            },
        }
    }

    fn inject_event(&mut self, _: Self::InEvent) {
    }

    fn inject_inbound_closed(&mut self) {
    }

    #[inline]
    fn inject_dial_upgrade_error(&mut self, _: Self::OutboundOpenInfo, _: &io::Error) {
        // We notify the task in order to try reopen a substream.
        self.upgrading = false;
    }

    fn shutdown(&mut self) {
        if let Some(ping_out_substream) = self.ping_out_substream.as_mut() {
            ping_out_substream.shutdown();
        }

        for ping in self.ping_in_substreams.iter_mut() {
            ping.shutdown();
        }
    }

    fn poll(&mut self) -> Poll<Option<NodeHandlerEvent<Self::OutboundOpenInfo, Self::OutEvent>>, io::Error> {
        // Open a ping substream if necessary.
        if self.ping_out_substream.is_none() && !self.upgrading {
            if self.active_ping_out.is_none() {
                let future = Delay::new(Instant::now() + PING_TIMEOUT);
                self.active_ping_out = Some(future);
            }
            self.upgrading = true;
            return Ok(Async::Ready(Some(NodeHandlerEvent::OutboundSubstreamRequest(()))));
        }

        // Poll the future that fires when we need to ping the node again.
        match self.next_ping.poll() {
            Ok(Async::NotReady) => {},
            Ok(Async::Ready(())) => {
                // We reset `next_ping` to a very long time in the future so that we can poll
                // it again without having an accident.
                self.next_ping.reset(Instant::now() + Duration::from_secs(5 * 60));
                if let Some(ref mut ping) = self.ping_out_substream {
                    ping.ping(Instant::now());
                    return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(OutEvent::PingStart))));
                }
            },
            Err(err) => {
                warn!(target: "sub-libp2p", "Ping timer errored: {:?}", err);
                return Err(io::Error::new(io::ErrorKind::Other, err));
            }
        }

        // Poll for answering pings.
        for n in (0 .. self.ping_in_substreams.len()).rev() {
            let mut ping = self.ping_in_substreams.swap_remove(n);
            match ping.poll() {
                Ok(Async::Ready(())) => {},
                Ok(Async::NotReady) => self.ping_in_substreams.push(ping),
                Err(err) => warn!(target: "sub-libp2p", "Remote ping substream errored:  {:?}", err),
            }
        }

        // Poll the ping substream.
        if let Some(mut ping_dialer) = self.ping_out_substream.take() {
            match ping_dialer.poll() {
                Ok(Async::Ready(Some(started))) => {
                    self.active_ping_out = None;
                    self.next_ping.reset(Instant::now() + DELAY_TO_NEXT_PING);
                    let ev = OutEvent::PingSuccess(started.elapsed());
                    return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(ev))));
                },
                Ok(Async::NotReady) => self.ping_out_substream = Some(ping_dialer),
                Ok(Async::Ready(None)) | Err(_) => {},
            }
        }

        // Poll the active ping attempt.
        if let Some(mut deadline) = self.active_ping_out.take() {
            match deadline.poll() {
                Ok(Async::Ready(())) => {
                    self.ping_out_substream = None;
                    return Ok(Async::Ready(Some(NodeHandlerEvent::Custom(OutEvent::Unresponsive))))
                },
                Ok(Async::NotReady) => self.active_ping_out = Some(deadline),
                Err(err) => {
                    warn!(target: "sub-libp2p", "Active ping deadline errored: {:?}", err);
                    return Err(io::Error::new(io::ErrorKind::Other, err));
                },
            }
        }

        Ok(Async::NotReady)
    }
}
