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
use rand::distributions::{Uniform, Distribution as _};
use std::{fmt, io, io::Read, io::Write, time::Duration, time::Instant};

// # How it works
//
// The user can specify the chance per second of a disconnect. Assuming that the `RandomDisconnect`
// lives forever, and that the chance is not zero, the disconnect **will** happen at some point.
// What we do is simply calculate in advance when it will happen.

/// Creates a wrapper around `AsyncRead + AsyncWrite` that has a random chance of disconnecting the
/// remote.
///
/// The `chance_per_sec` is the chance per sec on a ratio of `u32::max_value()`. In other words,
/// passing `u32::max_value()` means 100% chance, and passing `0` means 0% chance.
pub fn random_disconnect<T>(inner: T, chance_per_sec: u32) -> RandomDisconnect<T> {
    let state = if chance_per_sec == 0 {
        State::Disabled
    } else if chance_per_sec == u32::max_value() {
        State::Disconnected
    } else {
        let inverse = u64::from(u32::max_value()) / u64::from(chance_per_sec);
        let secs = Uniform::new(0, inverse).sample(&mut rand::thread_rng());
        let will_happen = Instant::now() + Duration::from_secs(secs);
        State::WillHappen(will_happen)
    };

    RandomDisconnect {
        inner,
        state,
    }
}

/// Wraps around an `AsyncRead + AsyncWrite` and adds a random chance of disconnect.
pub struct RandomDisconnect<TInner> {
    /// The underlying stream. Note that we keep it alive even after the fake disconnect, so as to
    /// not inform the remote of the disconnection.
    inner: TInner,

    /// Inner state.
    state: State,
}

#[derive(Debug)]
enum State {
    /// We are already disconnected.
    Disconnected,
    /// Chance of disconnect is 0.
    Disabled,
    /// Disconnect will happen at the given `Instant` calculated in advance.
    WillHappen(Instant),
}

impl<TInner> RandomDisconnect<TInner> {
    /// Checks whether we're disconnected. Returns the `TInner` if we're still connected.
    fn update(&mut self) -> Option<&mut TInner> {
        match self.state {
            State::Disconnected => return None,
            State::Disabled => return Some(&mut self.inner),
            State::WillHappen(when) if when > Instant::now() => return Some(&mut self.inner),
            State::WillHappen(_) => ()
        };

        self.state = State::Disconnected;
        None
    }
}

impl<TInner> Read for RandomDisconnect<TInner>
    where TInner: Read
{
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(inner) = self.update() {
            inner.read(buf)
        } else {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "simulated connection timeout"))
        }
    }
}

impl<TInner> tokio_io::AsyncRead for RandomDisconnect<TInner>
    where TInner: tokio_io::AsyncRead
{
}

impl<TInner> Write for RandomDisconnect<TInner>
    where TInner: Write
{
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(inner) = self.update() {
            inner.write(buf)
        } else {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "simulated connection timeout"))
        }
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        if let Some(inner) = self.update() {
            inner.flush()
        } else {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "simulated connection timeout"))
        }
    }
}

impl<TInner> tokio_io::AsyncWrite for RandomDisconnect<TInner>
    where TInner: tokio_io::AsyncWrite
{
    #[inline]
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        if let Some(inner) = self.update() {
            inner.shutdown()
        } else {
            Ok(Async::NotReady)
        }
    }
}

impl<TInner> fmt::Debug for RandomDisconnect<TInner>
where
    TInner: fmt::Debug
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let is_connected = match self.state {
            State::Disconnected => false,
            State::Disabled => true,
            State::WillHappen(when) if when > Instant::now() => true,
            State::WillHappen(_) => false,
        };

        if is_connected {
            fmt::Debug::fmt(&self.inner, f)
        } else {
            f.debug_tuple("Disconnected").field(&self.inner).finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::random_disconnect;
    use std::io::{self, Cursor, Read};
    use std::{thread, time::Duration};

    #[test]
    fn random_dc_full_chance() {
        let mut stream = random_disconnect(Cursor::new(vec![1, 2, 3, 4]), u32::max_value());
        match stream.read(&mut vec![0; 10]) {
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => (),
            _ => panic!()
        }
    }

    #[test]
    fn random_dc_half_chance() {
        let mut stream = random_disconnect(Cursor::new(vec![1, 2, 3, 4]), u32::max_value() / 2);
        thread::sleep(Duration::from_secs(3));
        match stream.read(&mut vec![0; 10]) {
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => (),
            _ => panic!()
        }
    }

    #[test]
    fn random_dc_zero_chance() {
        let mut stream = random_disconnect(Cursor::new(vec![1, 2, 3, 4]), 0);
        match stream.read(&mut vec![0; 10]) {
            Ok(_) => (),
            _ => panic!()
        }
    }
}
