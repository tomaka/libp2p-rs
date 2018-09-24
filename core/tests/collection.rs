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

extern crate futures;
extern crate libp2p_core;
extern crate libp2p_mplex;
extern crate libp2p_tcp_transport;
extern crate rand;
extern crate tokio;

use futures::{stream, prelude::*};
use libp2p_core::nodes::collection::{CollectionEvent, CollectionNodeAccept, CollectionStream};
use libp2p_core::nodes::handled_node::IdleHandler;
use libp2p_core::nodes::swarm::{Swarm, SwarmEvent};
use libp2p_core::{Endpoint, PeerId, PublicKey, Transport};
use libp2p_mplex::MplexConfig;
use libp2p_tcp_transport::TcpConfig;

#[test]
fn collection_basic_working() {
    let mut collec = CollectionStream::<(), ()>::new();

    let listener_peer_id = PeerId::from_public_key(PublicKey::Secp256k1(rand::random::<[u8; 32]>().to_vec()));
    let dialer_peer_id = PeerId::from_public_key(PublicKey::Secp256k1(rand::random::<[u8; 32]>().to_vec()));

    let transport = TcpConfig::default()
        .with_upgrade(MplexConfig::default())
        .map({
            let dialer_peer_id = dialer_peer_id.clone();
            let listener_peer_id = listener_peer_id.clone();
            move |muxer, endpoint| {
                match endpoint {
                    Endpoint::Dialer => (listener_peer_id, muxer),
                    Endpoint::Listener => (dialer_peer_id, muxer),
                }
            }
        });

    // We use a higher level API for the listening.
    let mut listener = Swarm::<_, _, _, fn(_) -> IdleHandler<(), ()>>::new(transport.clone());
    let listened_addr = listener.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();

    let future = transport.clone()
        .dial(listened_addr.clone()).unwrap_or_else(|_| panic!());
    let reach_id = collec.add_reach_attempt(future, IdleHandler::default());

    let dialer_peer_id2 = dialer_peer_id.clone();
    let dialer_stream = stream::poll_fn(move || -> Poll<Option<()>, ()> {
        loop {
            let should_close = {
                let event = match collec.poll() {
                    Async::Ready(ev) => ev,
                    Async::NotReady => return Ok(Async::NotReady),
                };

                match event {
                    Some(CollectionEvent::NodeReached(node)) => {
                        assert_eq!(node.reach_attempt_id(), reach_id);
                        assert!(!node.would_replace());
                        let (accept, peer_id) = node.accept();
                        assert_eq!(accept, CollectionNodeAccept::NewEntry);
                        assert_eq!(peer_id, listener_peer_id);
                        false
                    },
                    Some(CollectionEvent::NodeClosed { .. }) => true,
                    Some(CollectionEvent::NodeError { .. }) => panic!(),
                    Some(CollectionEvent::ReachError { .. }) => panic!(),
                    Some(CollectionEvent::NodeEvent { .. }) => panic!(),
                    None => panic!(),
                }
            };

            if should_close {
                assert!(collec.peer_mut(&dialer_peer_id2).is_none());
                assert!(!collec.has_connection(&dialer_peer_id2));
                assert_eq!(collec.connections().count(), 0);
            } else {
                assert!(collec.has_connection(&dialer_peer_id2));
                let _ = collec.peer_mut(&dialer_peer_id2).unwrap();
                assert!(collec.connections().cloned().collect::<Vec<_>>(), vec![dialer_peer_id2.clone()]);
            }
        }
    });

    let listener_future = listener
        .map(move |event| {
            match event {
                SwarmEvent::Connected { peer_id, endpoint } => {
                    assert_eq!(Endpoint::from(endpoint), Endpoint::Listener);
                    assert_eq!(peer_id, dialer_peer_id);
                    false
                },
                _ => true
            }
        })
        .take_while(|&out| Ok(out))
        .for_each(|_| Ok(()))
        .map_err(|_| ());

    let dialer_future = dialer_stream.for_each(|_| Ok(()));

    let final_future = dialer_future.select(listener_future).map(|((), _)| ()).map_err(|_| ());
    tokio::run(final_future);
}

// TODO:
// - replace existing peer
// - deny peer
// - interrupt attempt
// - broadcast event
// - close peer
// - send event to peer
