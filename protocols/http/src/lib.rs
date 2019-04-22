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

use bytes::{Bytes, BytesMut};
use futures::prelude::*;
use libp2p_core::nodes::handled_node::{NodeHandler, NodeHandlerEndpoint, NodeHandlerEvent};
use std::{fmt, io, marker::PhantomData};
use tokio_codec::Framed;
use tokio_io::{AsyncRead, AsyncWrite};

pub use error::Error;
pub use http;
pub use http::{Request, Response};

mod client;
mod error;
mod server;

pub struct HttpHandler<TSubstream> {
    inner: Option<Framed<TSubstream, HttpCodec>>,
    marker: PhantomData<TSubstream>,
}

impl<TSubstream> HttpHandler<TSubstream> {
    pub fn new() -> Self {
        HttpHandler {
            inner: None,
            marker: PhantomData,
        }
    }
}

impl<TSubstream> NodeHandler for HttpHandler<TSubstream>
where
    TSubstream: AsyncRead + AsyncWrite,
{
    type InEvent = Output;
    type OutEvent = Output;
    type Error = Error;
    type Substream = TSubstream;
    type OutboundOpenInfo = http::Request<Bytes>;

    fn inject_substream(&mut self, substream: Self::Substream, endpoint: NodeHandlerEndpoint<Self::OutboundOpenInfo>) {
        let framed = Framed::new(substream, HttpCodec::Server(server::HttpCodec::new(8192)));     // TODO:
        self.inner = Some(framed);
    }

    fn inject_event(&mut self, event: Self::InEvent) {
        if let Some(inner) = self.inner.as_mut() {
            inner.start_send(event);        // TODO: check result
        } else {
            unimplemented!()        // TODO:
        }
    }

    fn poll(&mut self) -> Poll<NodeHandlerEvent<Self::OutboundOpenInfo, Self::OutEvent>, Self::Error> {
        let inner = match self.inner.as_mut() {
            Some(i) => i,
            None => return Ok(Async::NotReady),
        };

        match inner.poll() {
            Ok(Async::NotReady) => (),
            Ok(Async::Ready(Some(out))) => return Ok(Async::Ready(NodeHandlerEvent::Custom(out))),
            Ok(Async::Ready(None)) => return Err(io::Error::from(io::ErrorKind::Other).into()),
            Err(err) => return Err(err),
        }

        match inner.poll_complete() {
            Ok(Async::Ready(())) => (),
            Ok(Async::NotReady) => (),
            Err(err) => return Err(err),
        }

        Ok(Async::NotReady)
    }
}

enum HttpCodec {
    Client(client::HttpCodec),
    Server(server::HttpCodec),
}

#[derive(Debug)]
pub enum Output {
    Request(http::Request<Bytes>),
    Response(http::Response<Bytes>),
}

impl tokio_codec::Encoder for HttpCodec {
    type Item = Output;
    type Error = Error;

    fn encode(&mut self, item: Output, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match self {
            HttpCodec::Client(ref mut inner) => {
                if let Output::Request(r) = item {
                    inner.encode(r, dst)
                } else {
                    panic!()
                }
            },
            HttpCodec::Server(ref mut inner) => {
                if let Output::Response(r) = item {
                    inner.encode(r, dst)
                } else {
                    panic!()
                }
            },
        }
    }
}

impl tokio_codec::Decoder for HttpCodec {
    type Item = Output;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Output>, Self::Error> {
        match self {
            HttpCodec::Client(inner) => inner.decode(src).map(|o| o.map(Output::Response)),
            HttpCodec::Server(inner) => inner.decode(src).map(|o| o.map(Output::Request)),
        }
    }
}
