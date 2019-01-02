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
use libp2p_core::Endpoint;
use std::{collections::VecDeque, io};
use tokio_io::AsyncWrite;
use unsigned_varint::encode;

/// Holds the state of the writing side of the multiplexer.
///
/// This struct has "virtual ownership" of the writing part of the underlying stream. It doesn't
/// technically have the stream as a field, but it is expected that the same stream is passed to
/// every single method of the `WriterState` and that nothing else but the `WriterState` touches
/// the stream. Failing to do so will likely result in logic errors.
pub struct WriterState {
    /// Data waiting to be sent on the underlying stream.
    // TODO: try a Vec instead and see if it performs better
    pending_writes: VecDeque<u8>,
}

impl WriterState {
    /// Initializes a `WriterState`.
    ///
    /// `max_data_len` is the maximum size of the data we will pass to `write_substream_data`.
    /// Failing to conform will result in decreased performances.
    pub fn new(max_data_len: usize) -> Self {
        // Calculate the number of bytes required to represent `max_data_len`.
        let max_data_len_len = {
            let mut dummy_buf = encode::usize_buffer();
            let dummy_bytes = encode::usize(max_data_len, &mut dummy_buf);
            dummy_bytes.len()
        };

        // Capacity to allocate for `pending_writes`.
        // Contains one byte for the header, plus the bytes for the length of the data,
        // plus the data.
        let buf_cap = 1 + max_data_len_len + max_data_len;

        WriterState {
            pending_writes: VecDeque::with_capacity(buf_cap),
        }
    }

    /// Flushes `out`. Makes sure that everything we sent will eventually arrive to the remote.
    pub fn poll_flush<TStream>(&mut self, out: &mut TStream) -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        try_ready!(self.poll_flush_pending(out));
        out.poll_flush()
    }

    /// Writes the content of the pending buffer, if any, to the underlying stream.
    ///
    /// Return `NotReady` if we couldn't write everything.
    fn poll_flush_pending<TStream>(&mut self, out: &mut TStream) -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        loop {
            if self.pending_writes.is_empty() {
                return Ok(Async::Ready(()));
            }

            let written = {
                let (slice, _) = self.pending_writes.as_slices();
                assert!(!slice.is_empty());
                match out.poll_write(&slice) {
                    Ok(Async::Ready(n)) if n == 0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(Async::Ready(n)) => n,
                    Ok(Async::NotReady) => return Ok(Async::NotReady),
                    Err(ref err) if err.kind() == io::ErrorKind::Interrupted => 0,
                    Err(err) => return Err(err),
                }
            };

            for _ in 0 .. written {
                // TODO: is there a more optimized version of this method?
                self.pending_writes.pop_front();
            }
        }
    }

    /// Tries to send a packet to `out` that opens a certain substream.
    pub fn write_substream_open<TStream>(&mut self, out: &mut TStream, substream_id: u32)
        -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        try_ready!(self.poll_flush_pending(out));

        let header = u64::from(substream_id) << 3;
        let mut header_buf = encode::u64_buffer();
        let header_bytes = encode::u64(header, &mut header_buf);
        try_ready!(self.write_raw(out, header_bytes, false));
        self.write_raw(out, &[0], true)
    }

    /// Tries to send a packet to `out` that closes a certain substream.
    pub fn write_substream_close<TStream>(&mut self, out: &mut TStream, substream: (u32, Endpoint))
        -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        try_ready!(self.poll_flush_pending(out));

        let header = match substream {
            (substream_id, Endpoint::Listener) => {
                (u64::from(substream_id) << 3) | 3
            },
            (substream_id, Endpoint::Dialer) => {
                (u64::from(substream_id) << 3) | 4
            },
        };

        let mut header_buf = encode::u64_buffer();
        let header_bytes = encode::u64(header, &mut header_buf);
        try_ready!(self.write_raw(out, header_bytes, false));
        self.write_raw(out, &[0], true)
    }

    /// Tries to send a packet to `out` that resets a certain substream.
    pub fn write_substream_reset<TStream>(&mut self, out: &mut TStream, substream: (u32, Endpoint))
        -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        try_ready!(self.poll_flush_pending(out));

        let header = match substream {
            (substream_id, Endpoint::Listener) => {
                (u64::from(substream_id) << 3) | 5
            },
            (substream_id, Endpoint::Dialer) => {
                (u64::from(substream_id) << 3) | 6
            },
        };

        let mut header_buf = encode::u64_buffer();
        let header_bytes = encode::u64(header, &mut header_buf);
        try_ready!(self.write_raw(out, header_bytes, false));
        self.write_raw(out, &[0], true)
    }

    /// Tries to send a packet to `out` that appends data to a certain substream.
    pub fn write_substream_data<TStream>(&mut self, out: &mut TStream, substream: (u32, Endpoint), substream_data: &[u8])
        -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        let _pending_buf_cap = self.pending_writes.capacity();

        try_ready!(self.poll_flush_pending(out));

        let header = match substream {
            (substream_id, Endpoint::Listener) => {
                (u64::from(substream_id) << 3) | 1
            },
            (substream_id, Endpoint::Dialer) => {
                (u64::from(substream_id) << 3) | 2
            },
        };

        let mut header_buf = encode::u64_buffer();
        let header_bytes = encode::u64(header, &mut header_buf);

        let mut len_buf = encode::usize_buffer();
        let len_bytes = encode::usize(substream_data.len(), &mut len_buf);

        try_ready!(self.write_raw(out, &header_bytes, false));
        self.write_raw(out, len_bytes, true)?;
        self.write_raw(out, substream_data, true)?;

        debug_assert_eq!(self.pending_writes.capacity(), _pending_buf_cap);
        Ok(Async::Ready(()))
    }

    /// Writes raw data to `out`.
    ///
    /// If `always_ready` is true, then this method never returns `NotReady`. If the underlying
    /// stream is not ready, the extra data is put in the pending buffer.
    ///
    /// If `always_ready` is false, then this method returns `NotReady` only if zero bytes could
    /// be written. If one or more byte could be written, then `Ready` is returned and the extra
    /// is put in the pending buffer.
    ///
    /// If an error is returned, no further guarantee about the state can be made. This is
    /// essentially a "start panicking" mode.
    fn write_raw<TStream>(&mut self, out: &mut TStream, data: &[u8], always_ready: bool)
        -> Poll<(), io::Error>
    where TStream: AsyncWrite
    {
        let mut total_written = 0;

        loop {
            if total_written == data.len() {
                break;
            }

            match out.poll_write(&data[total_written..]) {
                Ok(Async::Ready(n)) if n == 0 => break,
                Ok(Async::Ready(n)) => total_written += n,
                Ok(Async::NotReady) => {
                    if !always_ready && total_written == 0 {
                        return Ok(Async::NotReady);
                    } else {
                        break;
                    }
                },
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {},
                Err(err) => return Err(err),
            }
        }

        // TODO: is this fast?
        self.pending_writes.extend(&data[total_written..]);
        Ok(Async::Ready(()))
    }
}

