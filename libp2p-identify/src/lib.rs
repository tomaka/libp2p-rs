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

//! Implementation of the `/ipfs/id/1.0.0` protocol. Allows a node A to query another node B which
//! multiaddresses B knows about A.
//!
//! When two nodes connect to each other, the listening half sends a message to the dialing half,
//! indicating the public key, multiaddresses, and listening addresses.

extern crate bytes;
extern crate futures;
extern crate libp2p_peerstore;
extern crate libp2p_swarm;
extern crate protobuf;
extern crate tokio_io;

use bytes::Bytes;
use futures::{Future, Stream, Sink};
use libp2p_peerstore::Peerstore;
use libp2p_swarm::{ConnectionUpgrade, ConnectionUpgradeTy};
use protobuf::Message as ProtobufMessage;
use protobuf::core::parse_from_bytes as protobuf_parse_from_bytes;
use protobuf::repeated::RepeatedField;
use std::io::Error as IoError;
use std::iter;
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_io::codec::length_delimited;

mod structs_proto;

/// Prototype for an upgrade to the identity protocol.
#[derive(Debug, Clone)]
pub struct IdentifyProtocol<P> {
    peer_store: P,
    public_key: Vec<u8>,
    protocol_version: String,
    agent_version: String,
}

impl<P> IdentifyProtocol<P> {
    /// Creates a `IdentifyProtocol` struct. The peer store passed as parameter will be used
    /// whenever to determine which multiaddresses to report back whenever a query is received on
    /// the connection.
    #[inline]
    pub fn new(peer_store: P, public_key: Vec<u8>) -> IdentifyProtocol<P> {
        IdentifyProtocol {
            peer_store: peer_store,
            public_key: public_key,
            protocol_version: "n/a".to_owned(),                 // TODO:
            agent_version: "rust-libp2p/1.0.0".to_owned(),      // TODO:
        }
    }
}

impl<P, C> ConnectionUpgrade<C> for IdentifyProtocol<P>
    where C: AsyncRead + AsyncWrite + 'static
{
	type NamesIter = iter::Once<(Bytes, Self::UpgradeIdentifier)>;
	type UpgradeIdentifier = ();

    #[inline]
	fn protocol_names(&self) -> Self::NamesIter {
        iter::once((Bytes::from("/ipfs/id/1.0.0"), ()))
    }

	type Output = ();
	type Future = Box<Future<Item = (), Error = IoError>>;

	fn upgrade(self, socket: C, _: (), ty: ConnectionUpgradeTy) -> Self::Future {
        // TODO: use jack's varint library instead
        let socket = length_delimited::Builder::new()
            .length_field_length(1)
            .new_framed(socket);

        match ty {
            ConnectionUpgradeTy::Dialer => {
                let future = socket
                    .into_future().map(|(msg, _)| msg).map_err(|(err, _)| err)
                    .and_then(|msg| {
                        if let Some(msg) = msg {
                            if let Ok(msg) = protobuf_parse_from_bytes::<structs_proto::Identify>(&msg) {
                                //let peer = self.peerstore.peer_or_create();
                                // TODO:
                            }
                        }

                        Ok(())
                    });

                Box::new(future) as Box<_>
            },
            ConnectionUpgradeTy::Listener => {
                let mut message = structs_proto::Identify::new();
                message.set_publicKey(self.public_key.clone());
                message.set_agentVersion(self.agent_version.clone());
                message.set_protocolVersion(self.protocol_version.clone());
                // TODO: rest

                let future = socket.send(message.write_to_bytes().expect("we control the protobuf message"))
                    .map(|_| ());
                Box::new(future) as Box<_>
            },
        }
    }
}
