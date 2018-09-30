// Copyright 2017 Parity Technologies (UK) Ltd.
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

extern crate bytes;
extern crate env_logger;
extern crate futures;
extern crate libp2p;
extern crate tokio;

use futures::{Future, Stream};
use std::env;
use libp2p::core::Transport;
use libp2p::core::{nodes::protocol_handler::ProtocolHandler, upgrade};
use libp2p::tcp::TcpConfig;

fn main() {
    env_logger::init();

    // Determine which address to dial.
    let target_addr = env::args()
        .nth(1)
        .unwrap_or("/ip4/127.0.0.1/tcp/4001".to_owned());

    // We start by creating a `TcpConfig` that indicates that we want TCP/IP.
    let transport = TcpConfig::new()

        // On top of TCP/IP, we will use either the plaintext protocol or the secio protocol,
        // depending on which one the remote supports.
        .with_upgrade({
            let private_key = include_bytes!("test-rsa-private-key.pk8");
            let public_key = include_bytes!("test-rsa-public-key.der").to_vec();
            libp2p::secio::SecioConfig::new(
                libp2p::secio::SecioKeyPair::rsa_from_pkcs8(private_key, public_key).unwrap()
            )
        })

        .and_then(move |out, endpoint| {
            let peer_id = out.remote_key.into_peer_id();
            let upgrade = upgrade::map(libp2p::mplex::MplexConfig::new(), move |muxer| (peer_id, muxer));
            upgrade::apply(out.stream, upgrade, endpoint)
        });

    let mut swarm = libp2p::core::nodes::swarm::Swarm::with_handler_builder(transport, |_| {
        libp2p::ping::PeriodicPingHandler::new().into_node_handler()
    });

    swarm.listen_on("/ip4/127.0.0.1/tcp/5050".parse().unwrap()).unwrap();

    // We now use the controller to dial to the address.
    swarm
        .dial(target_addr.parse().expect("invalid multiaddr"))
        // If the multiaddr protocol exists but is not supported, then we get an error containing
        // the original multiaddress.
        .expect("unsupported multiaddr");

    // `swarm_future` is a future that contains all the behaviour that we want, but nothing has
    // actually started yet. Because we created the `TcpConfig` with tokio, we need to run the
    // future through the tokio core.
    tokio::run(
        swarm.for_each(|event| {
            match event {
                libp2p::core::nodes::swarm::SwarmEvent::NodeEvent { event, .. } => {
                    match event {
                        libp2p::ping::OutEvent::Unresponsive => println!("unresponsive"),
                        libp2p::ping::OutEvent::PingSuccess(duration) => println!("{:?}", duration),
                        _ => (),
                    }
                },
                _ => (),
            };

            Ok(())
        })
            .map_err(|_| ()),
    );
}
