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

//! An HTTP server.

use bytes::Bytes;
use byteorder::{BigEndian, ByteOrder};
use futures::prelude::*;
use libp2p::{
    core::muxing::OneSubstreamMuxer,
    core::nodes::{RawSwarm, RawSwarmEvent},
    core::transport::Transport,
    tcp,
};
use std::{io, sync::atomic, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConnecId([u8; 8]);
impl AsRef<[u8]> for ConnecId {
    fn as_ref(&self) -> &[u8] { &self.0[..] }
}
impl libp2p::core::nodes::collection::ConnectionInfo for ConnecId {
    type PeerId = ConnecId;
    fn peer_id(&self) -> &Self::PeerId { &self }
}

fn main() {
    env_logger::init();

    let transport = tcp::TcpConfig::new()
        .and_then(move |out, endpoint| -> Result<_, io::Error> {
            static COUNTER: atomic::AtomicU64 = atomic::AtomicU64::new(1);
            let mut buf = [0; 8];
            BigEndian::write_u64(&mut buf, COUNTER.fetch_add(1, atomic::Ordering::Relaxed));
            Ok((ConnecId(buf), OneSubstreamMuxer::new(out, endpoint.into())))
        })
        .with_timeout(Duration::from_secs(20));

    let mut swarm = RawSwarm::new(transport, ConnecId([0; 8]));
    swarm.listen_on("/ip4/0.0.0.0/tcp/8000".parse().unwrap()).unwrap();
    tokio::run(futures::future::poll_fn(move || -> Result<_, ()> {
        loop {
            match swarm.poll() {
                Async::NotReady => break Ok(Async::NotReady),
                Async::Ready(RawSwarmEvent::IncomingConnection(ev)) => {
                    ev.accept(libp2p::http::HttpHandler::new());
                },
                Async::Ready(RawSwarmEvent::Connected { endpoint, .. }) => {
                    //println!("Connected to {:?}", endpoint);
                },
                Async::Ready(RawSwarmEvent::NodeEvent { conn_info, event: libp2p::http::Output::Request(rq), .. }) => {
                    //println!("Got request: {:?}", rq);
                    swarm.peer(conn_info).into_connected().unwrap()
                        .send_event(libp2p::http::Output::Response(build_reply(rq)));
                },
                Async::Ready(RawSwarmEvent::NodeClosed { endpoint, error, .. }) => {
                    //println!("Disconnected from {:?} because {:?}", endpoint, error);
                },
                Async::Ready(RawSwarmEvent::NewListenerAddress { .. }) => {},
                Async::Ready(ev) => {
                    unimplemented!("{:?}", ev)
                },
            }
        }
    }));
}

fn build_reply(rq: libp2p::http::Request<Bytes>) -> libp2p::http::Response<Bytes> {
    let mut response = libp2p::http::Response::builder();
    response.header("Foo", "Bar")
        .header("Content-Length", "11")
        .status(libp2p::http::http::StatusCode::OK);
    response.body(b"hello world".to_vec().into()).unwrap()
}
