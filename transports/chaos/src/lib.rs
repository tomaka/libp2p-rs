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

use futures::{future, prelude::*, try_ready};
use libp2p_core::{Multiaddr, Transport, transport::ListenerEvent, transport::TransportError};
use rand::distributions::{Normal, Uniform, Distribution as _};
use std::io::{self, Read, Write};
use std::time::Duration;

mod delay;
mod disconnect;
mod latency;

/// Configuration for the chaos that is about to happen.
#[derive(Debug, Clone)]
pub struct ChaosConfig {
    /// Chances that dialing yields a future that never completes.
    ///
    /// The unit is a ratio of `u32::max_value()`. In other words, passing `u32::max_value()` means
    /// 100% chance, and passing `0` means 0% chance.
    pub dial_unresponsive: u32,

    /// Mean latency.
    pub mean_latency: Duration,

    /// The latency is using a normal distribution, and this is the deviation.
    pub latency_deviation: Duration,

    /// Chances per second that a connection created with this transport becomes unresponsive.
    ///
    /// The unit is a ratio of `u32::max_value()`. In other words, passing `u32::max_value()` means
    /// 100% chance, and passing `0` means 0% chance.
    pub random_disconnect_per_sec: u32,
}

impl ChaosConfig {
    /// Builds a configuration with no chaos.
    pub fn disabled() -> ChaosConfig {
        ChaosConfig {
            dial_unresponsive: 0,
            mean_latency: Duration::new(0, 0),
            latency_deviation: Duration::new(0, 0),
            random_disconnect_per_sec: 0,
        }
    }

    /// Builds a configuration approximating a real-life situation with a good western world
    /// Internet connection.
    pub fn real_life() -> ChaosConfig {
        ChaosConfig {
            dial_unresponsive: u32::max_value() / 50,
            mean_latency: Duration::from_millis(120),
            latency_deviation: Duration::from_millis(40),
            random_disconnect_per_sec: u32::max_value() / 3600,
        }
    }

    /// Builds a configuration for poor networking conditions.
    pub fn poor() -> ChaosConfig {
        ChaosConfig {
            dial_unresponsive: u32::max_value() / 20,
            mean_latency: Duration::from_millis(450),
            latency_deviation: Duration::from_millis(150),
            random_disconnect_per_sec: u32::max_value() / 600,
        }
    }

    /// Builds a configuration for extremely poor networking conditions. If you network holds with
    /// these conditions, then you know it's robust!
    pub fn very_poor() -> ChaosConfig {
        ChaosConfig {
            dial_unresponsive: u32::max_value() / 2,
            mean_latency: Duration::from_millis(1100),
            latency_deviation: Duration::from_millis(300),
            random_disconnect_per_sec: u32::max_value() / 60,
        }
    }
}

/// Equivalent to `ChaosConfig`, but with some processing.
#[derive(Debug, Clone)]
struct CookedConfig {
    /// See the equivalent field in `Config`.
    dial_unresponsive: u32,

    /// Normal distribution for latency. Produces values in milliseconds.
    latency_distribution: Normal,

    /// See the equivalent field in `Config`.
    mean_latency: Duration,

    /// See the equivalent field in `Config`.
    latency_deviation: Duration,

    /// See the equivalent field in `Config`.
    random_disconnect_per_sec: u32,
}

impl CookedConfig {
    /// Samples a value for latency.
    fn sample_latency(&self) -> Duration {
        let val = self.latency_distribution.sample(&mut rand::thread_rng());
        if val <= 0.0 {
            Duration::new(0, 0)
        } else {
            Duration::from_millis(val as u64)
        }
    }
}

impl From<ChaosConfig> for CookedConfig {
    fn from(config: ChaosConfig) -> CookedConfig {
        let latency_distribution = {
            let mean = config.mean_latency.as_millis() as f64;
            let std_dev = config.latency_deviation.as_millis() as f64;
            Normal::new(mean, std_dev)
        };

        CookedConfig {
            dial_unresponsive: config.dial_unresponsive,
            latency_distribution,
            mean_latency: config.mean_latency,
            latency_deviation: config.latency_deviation,
            random_disconnect_per_sec: config.random_disconnect_per_sec,
        }
    }
}

/// Wraps around a `Transport` and adds chaos to the underlying transport.
#[derive(Clone)]
pub struct ChaosTransport<TInner> {
    /// The underlying transport to build connections from.
    inner: TInner,
    /// Configuration that was passed on creation.
    config: CookedConfig,
}

impl<TInner> ChaosTransport<TInner> {
    /// Creates a new `ChaosTransport` around the transport.
    pub fn new(inner: TInner, config: ChaosConfig) -> Self {
        ChaosTransport {
            inner,
            config: config.into(),
        }
    }
}

impl<TInner> Transport for ChaosTransport<TInner>
where
    TInner: Transport,
{
    type Output = ChaosStream<TInner::Output>;
    type Error = TInner::Error;
    type Listener = ChaosListener<TInner::Listener>;
    type ListenerUpgrade = ChaosFuture<TInner::ListenerUpgrade>;
    type Dial = future::Either<ChaosFuture<TInner::Dial>, future::Empty<Self::Output, Self::Error>>;

    fn listen_on(self, addr: Multiaddr) -> Result<Self::Listener, TransportError<Self::Error>> {
        let config = self.config;
        self.inner
            .listen_on(addr)
            .map(|inner| ChaosListener { inner, config })
    }

    fn dial(self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        if Uniform::new_inclusive(0, u32::max_value()).sample(&mut rand::thread_rng()) < self.config.dial_unresponsive {
            return Ok(future::Either::B(future::empty()));
        }

        let latency = self.config.sample_latency();
        let config = self.config;
        self.inner
            .dial(addr)
            .map(move |inner| future::Either::A(ChaosFuture {
                inner: delay::delay(inner, latency),
                config,
            }))
    }
}

/// Listener for the `ChaosTransport`.
pub struct ChaosListener<TInner> {
    inner: TInner,
    config: CookedConfig,
}

impl<TInner, TConn> Stream for ChaosListener<TInner>
where
    TInner: Stream<Item = ListenerEvent<TConn>>,
    TConn: Future,
{
    type Item = ListenerEvent<ChaosFuture<TConn>>;
    type Error = TInner::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let (upgrade, listen_addr, remote_addr) = match try_ready!(self.inner.poll()) {
            Some(ListenerEvent::Upgrade { upgrade, listen_addr, remote_addr }) =>
                (upgrade, listen_addr, remote_addr),
            Some(ListenerEvent::NewAddress(addr)) =>
                return Ok(Async::Ready(Some(ListenerEvent::NewAddress(addr)))),
            Some(ListenerEvent::AddressExpired(addr)) =>
                return Ok(Async::Ready(Some(ListenerEvent::AddressExpired(addr)))),
            None => return Ok(Async::Ready(None))
        };

        let fut = ChaosFuture {
            // We don't put any additional latency when yielding a connection, as this is not a
            // network-related operation.
            inner: delay::delay(upgrade, Duration::new(0, 0)),
            config: self.config.clone(),
        };

        Ok(Async::Ready(Some(ListenerEvent::Upgrade {
            upgrade: fut,
            listen_addr,
            remote_addr
        })))
    }
}

/// Connection establishment future for the `ChaosTransport`.
pub struct ChaosFuture<TInner>
where
    TInner: Future
{
    inner: delay::Delay<TInner>,
    config: CookedConfig,
}

impl<TInner> Future for ChaosFuture<TInner>
    where TInner: Future,
{
    type Item = ChaosStream<TInner::Item>;
    type Error = TInner::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let inner = try_ready!(self.inner.poll());
        let inner = latency::latency(inner, self.config.mean_latency, self.config.latency_deviation);
        let inner = disconnect::random_disconnect(inner, self.config.random_disconnect_per_sec);

        Ok(Async::Ready(ChaosStream {
            inner,
        }))
    }
}

/// Wraps around an `AsyncRead + AsyncWrite` and adds chaos to it.
pub struct ChaosStream<TInner> {
    inner: disconnect::RandomDisconnect<latency::Latency<TInner>>,
}

impl<TInner> Read for ChaosStream<TInner>
where
    TInner: Read
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<TInner> tokio_io::AsyncRead for ChaosStream<TInner>
where
    TInner: tokio_io::AsyncRead
{
}

impl<TInner> Write for ChaosStream<TInner>
where
    TInner: Write
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<TInner> tokio_io::AsyncWrite for ChaosStream<TInner>
where
    TInner: tokio_io::AsyncWrite
{
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.inner.shutdown()
    }
}
