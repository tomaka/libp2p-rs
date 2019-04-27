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
use libp2p::{
    NetworkBehaviour,
    core::Transport, core::upgrade::InboundUpgradeExt, core::upgrade::OutboundUpgradeExt,
    core::swarm::{NetworkBehaviourAction, NetworkBehaviourEventProcess},
};
use log::debug;
use std::time::Duration;
use wasm_bindgen::prelude::*;

#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[wasm_bindgen]
pub fn start(transport: libp2p::wasm_ext::ffi::Transport) -> JsValue {
    // Setup logging.
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Debug).unwrap();

    // Create a random key for ourselves.
    let local_key = libp2p::core::identity::Keypair::generate_ed25519();
    let local_public = local_key.public();
    let local_peer_id = local_key.public().into_peer_id();

    let transport = libp2p::wasm_ext::ExtTransport::new(transport)
        .with_upgrade(libp2p::secio::SecioConfig::new(local_key))
        .and_then(move |out, endpoint| {
            let peer_id = out.remote_key.into_peer_id();
            let peer_id2 = peer_id.clone();
            let upgrade = libp2p::core::upgrade::SelectUpgrade::new(libp2p::yamux::Config::default(), libp2p::mplex::MplexConfig::new())
                // TODO: use a single `.map` instead of two maps
                .map_inbound(move |muxer| (peer_id, muxer))
                .map_outbound(move |muxer| (peer_id2, muxer));

            libp2p::core::upgrade::apply(out.stream, upgrade, endpoint)
                .map(|(id, muxer)| (id, libp2p::core::muxing::StreamMuxerBox::new(muxer)))
        })
        .with_timeout(Duration::from_secs(20));

    // Create a swarm to manage peers and events.
    let mut swarm = {
        #[derive(NetworkBehaviour)]
        #[behaviour(out_event = "libp2p::kad::KademliaOut", poll_method = "poll")]
        struct Behaviour<TSubstream> {
            kad: libp2p::kad::Kademlia<TSubstream>,
            ping: libp2p::ping::Ping<TSubstream>,
            identify: libp2p::identify::Identify<TSubstream>,

            #[behaviour(ignore)]
            events: Vec<libp2p::kad::KademliaOut>,
        }

        impl<TSubstream> Behaviour<TSubstream> {
            fn poll<TEv>(&mut self) -> Async<NetworkBehaviourAction<TEv, libp2p::kad::KademliaOut>> {
                if !self.events.is_empty() {
                    return Async::Ready(NetworkBehaviourAction::GenerateEvent(self.events.remove(0)))
                }

                Async::NotReady
            }
        }

        impl<TSubstream> NetworkBehaviourEventProcess<libp2p::kad::KademliaOut> for Behaviour<TSubstream> {
            fn inject_event(&mut self, event: libp2p::kad::KademliaOut) {
                self.events.push(event);
            }
        }

        impl<TSubstream> NetworkBehaviourEventProcess<libp2p::ping::PingEvent> for Behaviour<TSubstream> {
            fn inject_event(&mut self, _event: libp2p::ping::PingEvent) {}
        }

        impl<TSubstream> NetworkBehaviourEventProcess<libp2p::identify::IdentifyEvent> for Behaviour<TSubstream> {
            fn inject_event(&mut self, _event: libp2p::identify::IdentifyEvent) {
            }
        }

        let mut kad = libp2p::kad::Kademlia::new(local_peer_id.clone());
        kad.add_connected_address(
            &"QmSY3MkoNvxRmsbwqfEqzCwiYv2A5wVw2MNWUSX3wrSxBu".parse().unwrap(),
            "/ip4/127.0.0.1/tcp/30333/ws".parse().unwrap()
        );

        let behaviour = Behaviour {
            kad,
            ping: libp2p::ping::Ping::new(libp2p::ping::PingConfig::new()),
            identify: libp2p::identify::Identify::new(
                "substrate-mock/1.0.0".to_string(),
                "substrate-mock/1.0.0".to_string(),
                local_public
            ),
            events: Vec::new(),
        };

        libp2p::core::Swarm::new(transport, behaviour, local_peer_id)
    };

    // Order Kademlia to search for a peer.
    swarm.kad.find_node(libp2p::PeerId::random());

    // Kick it off!
    let future = futures::future::poll_fn(move || -> Result<_, JsValue> {
        loop {
            match swarm.poll().expect("Error while polling swarm") {
                Async::Ready(Some(ev @ libp2p::kad::KademliaOut::FindNodeResult { .. })) => {
                    let out = format!("Result: {:#?}", ev);
                    debug!("finished result");
                    return Ok(Async::Ready(JsValue::from_str(&out)));
                },
                Async::Ready(Some(_)) => (),
                Async::Ready(None) | Async::NotReady => break,
            }
        }

        Ok(Async::NotReady)
    });

    wasm_bindgen_futures::future_to_promise(future).into()
}
