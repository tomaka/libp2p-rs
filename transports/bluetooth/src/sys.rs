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

use futures::{prelude::*, try_ready};
use std::io;

#[cfg(unix)]
#[path = "sys/unix.rs"]
mod platform;

pub struct BluetoothStream {
    inner: platform::BluetoothStream,
}

impl BluetoothStream {
    pub fn connect(addr: [u8; 6], port: u8) -> io::Result<BluetoothStream> {
        Ok(BluetoothStream {
            inner: platform::BluetoothStream::connect(addr, port)?
        })
    }
}

pub struct BluetoothListener {
    inner: platform::BluetoothListener,
}

impl BluetoothListener {
    pub fn bind(addr: [u8; 6], port: u8) -> io::Result<BluetoothListener> {
        Ok(BluetoothListener {
            inner: platform::BluetoothListener::bind(addr, port)?
        })
    }
}

impl Stream for BluetoothListener {
    type Item = BluetoothStream;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let socket = try_ready!(self.inner.poll());
        Ok(Async::Ready(socket.map(|i| BluetoothStream { inner: i })))
    }
}
