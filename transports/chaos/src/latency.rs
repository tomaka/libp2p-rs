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

use futures::prelude::*;
use rand::distributions::{Distribution as _, Normal};
use std::{collections::VecDeque, io, io::Read, io::Write, time::Duration, time::Instant};

/// Wraps around an `AsyncRead + AsyncWrite` and adds latency to it.
///
/// > **Note**: This only adds latency to the reading side, as we assume that it is the same as
/// >           having latency on both the writing and reading sides.
pub fn latency<T>(inner: T, mean_latency: Duration, latency_deviation: Duration) -> Latency<T> {
    let latency_distribution = {
        let mean = mean_latency.as_millis() as f64;
        let std_dev = latency_deviation.as_millis() as f64;
        Normal::new(mean, std_dev)
    };

    Latency {
        inner,
        latency_distribution,
        wait: None,
        buffer: VecDeque::with_capacity(1024),
    }
}

// TODO: the latency should increase when the trafic is high, and spikes should happen

/// Wraps around an `AsyncRead + AsyncWrite` and adds latency to it.
pub struct Latency<TInner> {
    /// Inner connection to add latency to.
    inner: TInner,
    /// Normal distribution for latency. Produces values in milliseconds.
    latency_distribution: Normal,
    /// If `Some`, waiting for this `Delay` to trigger before allowing reading from `buffer`.
    wait: Option<tokio_timer::Delay>,
    /// Buffer of data that the outside is allowed to read.
    buffer: VecDeque<u8>,
}

impl<TInner> Read for Latency<TInner>
    where TInner: Read
{
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(wait) = self.wait.as_mut() {
                match wait.poll() {
                    Ok(Async::Ready(())) => (),
                    Ok(Async::NotReady) => return Err(io::ErrorKind::WouldBlock.into()),
                    Err(err) => return Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
                }
            }
            self.wait = None;

            // Copying the existing content of `self.buffer` to `buf`.
            if !self.buffer.is_empty() {
                let mut n_copied = 0;
                for out_b in buf.iter_mut() {
                    if let Some(b) = self.buffer.pop_front() {
                        *out_b = b;
                        n_copied += 1;
                    } else {
                        break;
                    }
                }
                return Ok(n_copied);
            }

            // Reading the next buffer in `self.buffer`.
            debug_assert!(self.buffer.is_empty());
            self.buffer.resize(256, 0);
            let read_result = {
                let slices = self.buffer.as_mut_slices();
                if !slices.0.is_empty() {
                    self.inner.read(slices.0)
                } else {
                    self.inner.read(slices.1)
                }
            };

            match read_result {
                Ok(n) => self.buffer.truncate(n),
                Err(err) => {
                    self.buffer.clear();
                    return Err(err)
                }
            };

            // Start the latency.
            let latency = {
                let v = self.latency_distribution.sample(&mut rand::thread_rng());
                if v <= 0.0 {
                    Duration::new(0, 0)
                } else {
                    Duration::from_millis(v as u64)
                }
            };

            self.wait = Some(tokio_timer::Delay::new(Instant::now() + latency));
        }
    }
}

impl<TInner> tokio_io::AsyncRead for Latency<TInner>
    where TInner: tokio_io::AsyncRead
{
}

impl<TInner> Write for Latency<TInner>
    where TInner: Write
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<TInner> tokio_io::AsyncWrite for Latency<TInner>
    where TInner: tokio_io::AsyncWrite
{
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        self.inner.shutdown()
    }
}
