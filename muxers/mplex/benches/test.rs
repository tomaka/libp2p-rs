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

#![feature(test)]
extern crate test;

use futures::prelude::*;
use libp2p_core::{muxing, Transport};
use std::sync::Arc;
use tokio::{
    codec::length_delimited::Builder,
    runtime::current_thread::Runtime
};

fn connect_and_send_data(bench: &mut test::Bencher, data: &[u8]) {
    let transport =
        libp2p_tcp::TcpConfig::new().with_upgrade(libp2p_mplex::MplexConfig::new());

    bench.iter(move || {
        let (listener, addr) = transport
            .clone()
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();

        let listener_side = listener
            .into_future()
            .map_err(|(err, _)| err)
            .and_then(|(client, _)| client.unwrap().0)
            .and_then(|client| muxing::outbound_from_ref_and_wrap(Arc::new(client)))
            .map(|client| Builder::new().new_read(client.unwrap()))
            .and_then(|client| {
                client
                    .into_future()
                    .map_err(|(err, _)| err)
                    .map(|(msg, _)| msg)
            })
            .and_then(|msg| {
                let msg = msg.unwrap();
                assert_eq!(msg, data);
                Ok(())
            });

        let dialer_side = transport
            .clone()
            .dial(addr)
            .unwrap()
            .and_then(|client| muxing::inbound_from_ref_and_wrap(Arc::new(client)))
            .map(|server| Builder::new().new_write(server.unwrap()))
            .and_then(|server| server.send(data.into()))
            .map(|_| ());

        let combined = listener_side.select(dialer_side)
            .map_err(|(err, _)| panic!("{:?}", err))
            .map(|_| ());
        let mut rt = Runtime::new().unwrap();
        rt.block_on(combined).unwrap();
    })
}

#[bench]
fn connect_and_send_hello(bench: &mut test::Bencher) {
    connect_and_send_data(bench, b"hello world")
}

#[bench]
fn connect_and_send_one_kb(bench: &mut test::Bencher) {
    let data = (0 .. 1024).map(|_| rand::random::<u8>()).collect::<Vec<_>>();
    connect_and_send_data(bench, &data)
}

#[bench]
fn connect_and_send_one_mb(bench: &mut test::Bencher) {
    let data = (0 .. 1024 * 1024).map(|_| rand::random::<u8>()).collect::<Vec<_>>();
    connect_and_send_data(bench, &data)
}

#[bench]
fn connect_and_send_two_mb(bench: &mut test::Bencher) {
    let data = (0 .. 2 * 1024 * 1024).map(|_| rand::random::<u8>()).collect::<Vec<_>>();
    connect_and_send_data(bench, &data)
}
