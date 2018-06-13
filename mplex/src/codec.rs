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

use std::cmp;
use std::io::{Error as IoError, ErrorKind as IoErrorKind};
use std::mem;
use bytes::{BufMut, BytesMut};
use core::Endpoint;
use tokio_io::codec::{Decoder, Encoder};
use varint;

#[derive(Debug, Clone)]
pub enum Elem {
    Open { substream_id: u32 },
    Data { substream_id: u32, data: BytesMut },
    Close { substream_id: u32 },
    Reset { substream_id: u32 },
}

pub struct Codec {
    endpoint: Endpoint,
    varint_decoder: varint::VarintDecoder<u32>,
    decoder_state: CodecDecodeState,
}

#[derive(Debug, Clone)]
enum CodecDecodeState {
    Begin,
    HasHeader(u32),
    HasHeaderAndLen(u32, usize, BytesMut),
    Poisoned,
}

impl Codec {
    pub fn new(endpoint: Endpoint) -> Codec {
        Codec {
            endpoint,
            varint_decoder: varint::VarintDecoder::new(),
            decoder_state: CodecDecodeState::Begin,
        }
    }
}

impl Decoder for Codec {
    type Item = Elem;
    type Error = IoError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match mem::replace(&mut self.decoder_state, CodecDecodeState::Poisoned) {
                CodecDecodeState::Begin => {
                    match self.varint_decoder.decode(src)? {
                        Some(header) => {
                            self.decoder_state = CodecDecodeState::HasHeader(header);
                        },
                        None => {
                            self.decoder_state = CodecDecodeState::Begin;
                            return Ok(None);
                        },
                    }
                },
                CodecDecodeState::HasHeader(header) => {
                    match self.varint_decoder.decode(src)? {
                        Some(len) => {
                            if len > 4096 {     // TODO: arbitrary
                                return Err(IoErrorKind::InvalidData.into());
                            }

                            self.decoder_state = CodecDecodeState::HasHeaderAndLen(header, len as usize, BytesMut::with_capacity(len as usize));
                        },
                        None => {
                            self.decoder_state = CodecDecodeState::HasHeader(header);
                            return Ok(None);
                        },
                    }
                },
                CodecDecodeState::HasHeaderAndLen(header, len, mut buf) => {
                    debug_assert!(len == 0 || buf.len() < len);
                    let to_transfer = cmp::min(src.len(), len - buf.len());

                    buf.put(src.split_to(to_transfer));    // TODO: more optimal?

                    if buf.len() < len {
                        self.decoder_state = CodecDecodeState::HasHeaderAndLen(header, len, buf);
                        return Ok(None);
                    }

                    self.decoder_state = CodecDecodeState::Begin;
                    let substream_id = (header >> 3) as u32;
                    let out = match header & 7 {
                        0 => Elem::Open { substream_id },
                        1 | 2 => Elem::Data { substream_id, data: buf },
                        3 | 4 => Elem::Close { substream_id },
                        5 | 6 => Elem::Reset { substream_id },
                        _ => return Err(IoErrorKind::InvalidData.into()),
                    };

                    return Ok(Some(out));
                },

                CodecDecodeState::Poisoned => {
                    return Err(IoErrorKind::InvalidData.into());
                }
            }
        }
    }
}

impl Encoder for Codec {
    type Item = Elem;
    type Error = IoError;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let (header, data) = match (item, self.endpoint) {
            (Elem::Open { substream_id }, _) => {
                ((substream_id as u64) << 3, BytesMut::new())
            },
            (Elem::Data { substream_id, data }, Endpoint::Listener) => {
                ((substream_id as u64) << 3 | 1, data)
            },
            (Elem::Data { substream_id, data }, Endpoint::Dialer) => {
                ((substream_id as u64) << 3 | 2, data)
            },
            (Elem::Close { substream_id }, Endpoint::Listener) => {
                ((substream_id as u64) << 3 | 3, BytesMut::new())
            },
            (Elem::Close { substream_id }, Endpoint::Dialer) => {
                ((substream_id as u64) << 3 | 4, BytesMut::new())
            },
            (Elem::Reset { substream_id }, Endpoint::Listener) => {
                ((substream_id as u64) << 3 | 5, BytesMut::new())
            },
            (Elem::Reset { substream_id }, Endpoint::Dialer) => {
                ((substream_id as u64) << 3 | 6, BytesMut::new())
            },
        };

        let header_bytes = varint::encode(header);
        let data_len = data.as_ref().len();
        let data_len_bytes = varint::encode(data_len);

        dst.reserve(header_bytes.len() + data_len_bytes.len() + data_len);
        dst.put(header_bytes);
        dst.put(data_len_bytes);
        dst.put(data);
        Ok(())
    }
}
