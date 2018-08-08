# Libp2p

This document contains the specifications of libp2p.

## Overview

TODO

## Protobuf

In this document, the term **protobuf** designates the **protocol buffers** format whose
specifications can be found [here](https://developers.google.com/protocol-buffers/docs/reference/overview).

## Variable-length integers

The libp2p protocols often uses **variable-length integers**.

Variable-length integers are explained [here](https://developers.google.com/protocol-buffers/docs/encoding).

In order to turn an integer of any size into a variable-length integer:

- Split the integer in groups of 7 bits in a little-endian way.
- Each group of 7 bits is turned into a byte form the least significant bits of a byte, and the
  most significant bit is set to 1.
- Set the most significant bit of the last byte to 0.

## Multiaddress

A *multiaddress* is a way to represent a network path to a node and the protocols to use to reach
this node.

A multiaddress is composed of a stack of protocols such as `IPv4`, `TCP`, `WebSockets`, etc.
Each protocol in the stack can accept one optional parameter.

Example:

* `/ip4/80.164.9.232/tcp/2300` indicates a remote which can be reached by using the TCP/IP
  protocols stack with the IP address 80.164.9.232 and the TCP port 2300.

Note that no assumption is made about the validity of a multiaddress when it comes to its stack
of protocols. For example `/tcp/5/tcp/0/tcp/2500` is valid multiaddress, even though attempting
to reach it will fail.

TODO the [] syntax for string reprs

### Dialing and listening

An implementation of libp2p can either *dial* or *listen* on a multiaddress, the meaning of which
depends on the protocols being used.

### Protocol requirements

The stack of protocols of a multiaddress must satisfy the following requirements:

- Reliability: all the packets sent by one side must be received by the other side.
- Ordering: the packets must be received in the same order as they are sent.

If the stack of protocols of a multiaddress doesn't satisfy them, then dialing or listening on that
multiaddress must fail.

### Locality of multiaddresses

The protocols of a multiaddress indicate how to reach a remote. However it must be noted that
the stack of protocols only makes sense in the context of a specific machine.

In other words, if two different machines try to reach the same multiaddress, it is possible that
one of them fails but the other succeeds, or that they both succeed but reach two different peers.

For example `/ip4/192.168.1.28/tcp/50000` will reach a different machine or no machine at all,
depending on where it is being dialed from.

Also note that the same node may also be reachable through multiple different multiaddresses.

### String and binary representations

A multiaddress has two representations: a string representation (like in the example above), or a
binary representation. The string representation is generally used in end user interfaces, while
the binary representation is generally used when a multiaddress needs to be transmitted over the
network.

In its binary representation, a multiaddress is composed of the binary representations of each
of its protocol, adjacent in memory.

The binary representation of a protocol is composed of a variable-length integer representing
the code of the protocol, as defined
[here](https://github.com/multiformats/multiaddr/blob/master/protocols.csv), optionally followed
with the binary representation of the protocol's parameter if the protocol accepts one.
If the protocol's parameter has a constant binary size, then the binary representation of the
parameter immediately follows the protocol code in memory. If the protocol's parameter doesn't
have a fixed size, then a variable-length integer indicating the size of the parameter is inserted
between the protocol's code and the parameter's binary representation.

### Multiaddress protocols

The following sections describe each possible protocol of a multiaddress.

### Multiaddress protocol: IPv4, IPv6

* String representation: `ip4`, `ip6`
* Protocol code: 4 for IPv4, 41 for IPv6
* Parameter: the IPv4 or IPv6 address
* Binary size of the parameter: 4 bytes for IPv4, 16 bytes for IPv6

Example string representations:

 - `/ip4/87.94.1.243`
 - `/ip4/0.0.0.0`
 - `/ip4/127.0.0.1`
 - `/ip6/[::1]`
 - `/ip6/[FE80::0202:B3FF:FE1E:8329]`
 - `/ip6/[fe80:0000:0000:0000:0202:b3ff:fe1e:8329]`
 - `/ip6/[::]`

The binary representation of the parameter is the four bytes of the IPv4 address, or the sixteen
bytes of the IPv6 address.
For example the IPv4 address `127.0.0.1` is represented by `[127, 0, 0, 1]` in memory.

Designates the standard IPv4 and IPv6 protocols of the TCP/IP stack. Listening on an IP address
consists in accepting connections whose destination IP matches the one we're listening on.
Listening on the addresses `/ip4/0.0.0.0` or `/ip6/[::]` means accepting all connections that reach
this machine.

Note that implementations are typically not capable of using the IP protocol by itself, as a
protocol such as TCP or UDP must be used on top of it. Implementations are not expected to split a
multiaddress into each individual components, but instead can take multiaddresses as a whole in
order to determine how to behave.

### Multiaddress protocol: TCP

* String representation: `tcp`
* Protocol code: 6
* Parameter: the TCP port to use
* Binary size of the parameter: 2 bytes

Example string representations:
 - `/tcp/2500`
 - `/tcp/0`

The binary representation of the parameter is the 2-bytes number of the port in big endian.

Designates the standard TCP protocol of the TCP/IP stack.
Generally used on top of IPv4 or IPv6.

### Multiaddress protocol: UDP

* String representation: `udp`
* Protocol code: 17
* Parameter: the UDP port to use
* Binary size of the parameter: 2 bytes

Example string representations:
 - `/udp/19874`
 - `/udp/0`

The binary representation of the parameter is the 2-bytes number of the port in big endian.

Designates the standard UDP protocol of the TCP/IP stack.
Generally used on top of IPv4 or IPv6.

Note that a multiaddress that ends with `/udp` doesn't respect the robustness expectations of
connections. A multiaddress that contains `/udp` should have another protocol negotiated on top,
such as UTP or QUIC for example.

### Multiaddress protocol: DCCP

TODO group with TCP, UDP, SCTP?

* String representation: `dccp`
* Protocol code: 33
* Parameter: the DCCP port to use
* Binary size of the parameter: 2 bytes

Example string representations:
 - `/dccp/4900`
 - `/dccp/0`

The binary representation of the parameter is the 2-bytes number of the port in big endian.

Designates the standard DCCP protocol, as described by
[RFC 4340](https://tools.ietf.org/html/rfc4340).
Generally used on top of IPv4 or IPv6.

### Multiaddress protocol: IPv6

* String representation: `ip6`
* Protocol code: 41
* Parameter: the IPv6 address
* Binary size of the parameter: 16 bytes

Example string representations:

The binary representation of the parameter is the sixteen bytes of the IPv6 address.

Designates the standard IPv6 protocol of the TCP/IP stack.

Note that implementations are typically not capable of using the IP protocol by itself, as a
protocol such as TCP or UDP must be used on top of it. Implementations are not expected to split a
multiaddress into each individual components, but instead can take multiaddresses as a whole in
order to determine how to behave.

### Multiaddress protocol: DNS4, DNS6, DNSAddr

* String representation: `dns4`, `dns6`, `dnsaddr`
* Protocol code: 54 for DNS4, 55 for DNS6, 56 for DNSAddr
* Parameter: the domain name
* Binary size of the parameter: variable

Example string representations:
 - `/dns4/example.com`
 - `/dns4/localhost`
 - `/dns6/example.com`
 - `/dns6/localhost`
 - `/dnsaddr/example.com`
 - `/dnsaddr/localhost`

The binary representation of the parameter is the UTF-8 representation of the domain name.
Since the length of the parameter is variable, a variable-length integer must be prefixed, as
explained in the `Mutiaddress` section.

Designates the standard DNS protocol. Reaching this endpoint is done by performing a name
resolution.
When using DNS4, an implementation must use an IPv4 stored in an `A` entry of the results, or fail
if there is none.
When using DNS6, an implementation must use an IPv6 stored in an `AAAA` entry of the results, or
fail if there is none.
When using DNSAddr, an implementation can decide whether to use IPv4 or IPv6.

It is not valid to listen on an address that uses DNS4, DNS6 or DNSAddr.

### Multiaddress protocol: SCTP

TODO group with TCP, UDP, DCCP?

* String representation: `sctp`
* Protocol code: 132
* Parameter: the SCTP port to use
* Binary size of the parameter: 2 bytes

Example string representations:
 - `/sctp/16900`
 - `/sctp/0`

The binary representation of the parameter is the 2-bytes number of the port in big endian.

Designates the standard SCTP protocol, as described by
[RFC 4960](https://tools.ietf.org/html/rfc4960).
Generally used on top of IPv4 or IPv6.

### Multiaddress protocol: UDT, UTP

* String representation: `udt`, `utp`
* Protocol code: 301 for `udt`, 302 for `utp`
* Parameter: none
* Binary size of the parameter: N/A

Example string representations:
 - `/utp` (only valid possible representation)
 - `/udt` (only valid possible representation)

Designates the non-standard UDT and µTP protocols.
See [this document](http://bittorrent.org/beps/bep_0029.html) for µTP.

If this protocol is not used immediately after `/udp`, then dialing or listening must fail.
Otherwise, when used after `/udp`, we assume that a newly-opened connection on the underlying
multiaddress expects respectively UTP or µTP.

### Multiaddress protocol: Unix

* String representation: `unix`
* Protocol code: 400
* Parameter: the path to the UNIX domain socket
* Binary size of the parameter: variable

TODO the [] syntax for string repr

Example string representations:
 - `/unix/[/var/foo]` (designates the path `/var/foo` on the file system)
 - `/unix/foo` (designates the path `/foo` on the file system)

The path to the file is always absolute.

The binary representation of the parameter is the UTF-8 representation of the path.
Since the length of the parameter is variable, a variable-length integer must be prefixed, as
explained in the `Mutiaddress` section.

Designates Unix Domain Sockets. Note that recent versions of Windows also support Unix
Domain Sockets.

### Multiaddress protocol: P2P

* String representation: `p2p`
* Protocol code: 421
* Parameter: a multihash whose meaning is context-dependant
* Binary size of the parameter: variable

Example string representations:
 - `/p2p/QmRMGcQh69t8a8YwzHkofVo9SFr7ffggUwhAYjVSTChmrd`
 - `/p2p/QmWCnXrhM1in1qPqVT3rDXQEJHedAzbPDMimdjqy2P9fGn`

This protocol used to be known as `IPFS` in earlier versions, and its string representation
was `/ipfs/...`.

The string representation of the parameter is the base58 encoding of the binary representation of
the multihash.

The binary representation of the parameter is the binary representation of the multihash of the
parameter.
Since the length of the parameter is variable, a variable-length integer must be prefixed, as
explained in the `Mutiaddress` section.

The implementation is expected to determine how to reach the node passed as parameter in an
implementation-defined way, or fail if it doesn't know how to reach the node.

While the meaning of the multihash depends on the context, it is very often the SHA-256 hash of
a protobuf structure describing the public key of a node. See the peer ID section for more
information.

### Multiaddress protocol: WS and WSS

* String representation: `ws` and `wss`
* Protocol code: 477 (for WS) and 478 (for WSS)
* Parameter: none
* Binary size of the parameter: N/A

Example string representations:
 - `/ws`, `/wss` (only valid possible representations)

Example usage:
 - `/ip4/49.85.4.39/tcp/6000/ws`

Designates the standard WebSockets protocol, as defined by
[RFC 6455](https://tools.ietf.org/html/rfc6455).

Dialing an address that starts with `/ws` or `/wss` doesn't make sense and must fail.

Dialing an address that contains `/ws` or `/wss` consists in dialing the underlying address (the
part at the left of `/ws` or `/wss`) and performing on the stream an HTTP requests that negotiates
the WebSockets protocol. For `/wss`, secure WebSockets has to be used.

### Multiaddress protocol P2P-circuit

* String representation: `p2p-circuit`
* Protocol code: 290
* Parameter: none
* Binary size of the parameter: N/A

Example string representations:
 - `/p2p-circuit` (only valid possible representation)

Example usage:
 - `/ip4/90.34.68.2/tcp/5000/p2p-circuit/ip4/12.98.230.28/udp/2500/utp`

Designates the `relay` protocol of libp2p. See the relevant section below.

Dialing an address that contains `p2p-circuit` consists in dialing the underlying multiaddress
(the part at the left of `p2p-circuit`) and negotiating the `relay` protocol in order to ask
the remote to reach the follow-up multiaddress (the part at the right of `p2p-circuit`) and relay
all communications on this stream to it.

Listening on an address that contains `p2p-circuit` must fail.

TODO what if multiple p2p-circuit in same addr?

As explained in the `Multiaddress` section, a multiaddress is always relative to a specific
machine. The right part of the address here is relative to the machine designated by the left part
of the address.

## Multihash

A multihash is a data structure that contains a digest and a code representing the hashing
algorithm that was used.

The binary representation of a multihash is composed of a variable-length integer representing the
code of the hashing algorithm, a variable-length integer containing the length of the digest, and
the digest, all adjacent in memory.

The hash codes can be found [here](https://github.com/multiformats/multihash/blob/master/hashtable.csv).

A multihash doesn't have a string representation.

## Peer ID

A node can identified by a public key that it generates locally.
Considering the number of possible keys, it is highly improbable that two nodes randomly generate
the same key.

When using libp2p, you are strongly encouraged to use an encryption layer (such as secio, see
below) in order to make sure that a node is capable of decoding communication encrypted with their
public key and thus is indeed in posession of the corresponding private key.

A **peer ID** is a multihash using the SHA-256 algorithm of the binary encoding of the protobuf
`PublicKey` structure defined like this:

```protobuf
enum KeyType {
    RSA = 0;
    Ed25519 = 1;
    Secp256k1 = 2;
}

message PublicKey {
    required KeyType Type = 1;
    required bytes Data = 2;
}
```

The binary representation of a **peer ID** is the binary representation of the multihash.
The string representation of a **peer ID** is the base58 encoding of its binary representation.

## Multistream-select

**multistream-select** is a protocol whose role is to negotiate a protocol on a stream.

Once a node dials a multiaddress and successfully reaches another node, the protocol that is
expected to be used on the newly-opened stream is *multistream-select*.

*The dialer* designates the node that initiated the connection, and *the listener* designates the
node that received the connection.

Each message in the *multistream-select* protocol is prefixed with a variable-length integer
indicating its length.

### Handshake

The first thing that happens when a connection is initiated is the *multistream-select handshake*.
The dialer sends the ASCII representation of `/multistream/1.0.0\n` (prefixed with its length (19),
as explained above), to which the listener responds by sending `/multistream/1.0.0\n` (prefixed
as well).

The dialer doesn't have to wait for the handshake response in order to continue.

This initial handshake message makes it possible to release new versions of the multistream-select
protocol in the future, such that nodes can support multiple versions at once.

After the handshake has been performed, the dialer has two options:

- Request a specific protocol.
- Request the list of all the protocols the remote would accept.

### Choosing a protocol

Once the handshake is performed, the dialer can send a message containing the name of a protocol
followed with the `\n` character (prefixed with its length, as explained above, including the
`\n`). This means that the dialer requests to use the protocol whose name has been sent.

If the listener accepts this protocol request, it must send back the same name (followed with `\n`
and prefixed with its length). Once this is done, the *multistream-select* protocol ends and the
stream or connection is considered as using the newly-negotiated protocol.

If the listener refuses the protocol request (for example becsupportsause it doesn't support this
protocol, or if this protocol is invalid in this context), then it must send back `na\n` (prefixed
with its length). The dialer can then request the same or another protocol.

### Querying the list of supported protocols

Additionally, the dialer can also send `ls\n` (prefixed with its length), which asks the listener
for a list of the protocols it supports.

The listener must then answer by sending a variable-length integer indicating the number of
protocols it will send back, followed with the list of protocols. The entire message must also
be length-prefixed.

TODO document which delimiter between protocols in the return list ; it seems weird

Once the listener sent back its response, the dialer can continue by requesting a protocol as
explained above.

## Plaintext

The `/plaintext/1.0.0` protocol is a protocol that nodes can optionally support and that can be
negotiated with *multistream-select*.

Plaintext is a dummy protocol. Once it has been negotiated, the dialer is expected to restart
the process of selecting a protocol with *multistream-select* on the same stream.

## Secio

The `/secio/1.0.0` protocol is a protocol that nodes can optionally support and that can be
negotiated with *multistream-select*.

Secio makes it possible for nodes to agree on a encryption key by performing a Diffie-Hellman key
exchange.

After the secio handshake process has been performed, the dialer is expected to restart the
process of selecting a protocol with *multistream-select* on the now-encrypted stream.

TODO ouch that's going to be long

## Mplex

The `/mplex/6.7.0` protocol is a protocol that nodes can optionally support and that can be
negotiated with *multistream-select*.

In the following section, the *dialer* refers to the side that opened the connection and negotiated
the *mplex* protocol, while the *listener* refers to the other side.

*Mplex* is a multiplexing protocol. Once negotiated, the connection consists of multiple substreams
multiplexed together. Both the dialer and the listener can open a new substream. Every time a
new substream is open, the *multistream-select* protocol is used to negotiate the protocol to use
in this substream in particular. In each individual substream, the side which opened the substream
is considered as the dialer for the purpose of *multistream-select* and higher level protocols.

Each packet in the mplex protocol is composed of a variable-length integer called the *header*,
a variable-length integer indicating the size in bytes of the data, then the data.

The three least-significant bits of the header are a number indicating which kind of operation
this header describes. The other bits are an integer indicating which substream is concerned.

The three least-significant bits of the header can have the following value:

- `0`: the node which sent this packet wants to open the given substream. The data is an arbitrary
  name to give to the substream, that can be used for debugging purposes.
- `1`: the listener wants to transmit data on the given substream.
- `2`: the dialer wants to transmit data on the given substream.
- `3`: the listener closed its side of the given substream and will no longer send any
  data on it anymore.
- `4`: the dialer closed its side of the given substream and will no longer send any
  data on it anymore.
- `5`: the listener wants to completely close the given substream.
- `6`: the dialer wants to completely close the given substream.

The dialer is only allowed to open substreams with uneven numbers, while the listener is only
allowed to open substreams with even numbers. Opening a substream doesn't need to be confirmed,
in other words as soon as an open message is sent, the substream is considered open.

## Yamux

The `/yamux/1.0.0` protocol is a protocol that nodes can optionally support and that can be
negotiated with *multistream-select*.

The *yamux* protocol is specified [here](https://github.com/hashicorp/yamux/blob/master/spec.md).

*Yamux* is a multiplexing protocol. Once negotiated, the connection consists of multiple substreams
multiplexed together. Both the dialer and the listener can open a new substream. Every time a
new substream is open, the *multistream-select* protocol is used to negotiate the protocol to use
in this substream in particular. In each individual substream, the side which opened the substream
is considered as the dialer for the purpose of *multistream-select* and higher level protocols.

## Ping

The `/ipfs/ping/1.0.0` protocol is a protocol that nodes can optionally support and that can be
negotiated with *multistream-select*.

Once the protocol has been negotiated, the dialer can send packets of 32 bytes of data to which
the listener must answer by sending back the exact same data.

The data doesn't have any meaning and is should be randomly generated.

This protocol is typically used to determine is an existing connection is still alive, and/or how
much time it takes for the remote to send back an answer.

Note that the ping protocol is unidirectional. In other words, only the dialer can send pings,
and all the listener is allowed to do is respond to incoming pings. If the listener wants to ping
back the dialer, it should do so in a different multiplexed substream or in a different connection.

## Identify

The `/ipfs/id/1.0.0` protocol is a protocol that nodes can optionally support and that can be
negotiated with *multistream-select*.

Once the protocol has been negotiated, the listener must send its *identification information*
to the dialer and immediately close the substream or connection. The dialer must not send anything.

The *identification information* consists in a protobuf message with the following definition:

```protobuf
message Identify {
    optional string protocolVersion = 5;
    optional string agentVersion = 6;
    optional bytes publicKey = 1;
    repeated bytes listenAddrs = 2;
    optional bytes observedAddr = 4;
    repeated string protocols = 3;
}
```

The message is prefixed with a variable-length integer containing its size in bytes.

The `protocolVersion` field indicates the . TODO

The `agentVersion` field indicates the name and version of the listener's software. It should be
in the format `name/version` (eg. `foo/1.2.0`). This is similar to the `User-Agent` field of the
HTTP protocol and should be used only for debugging or statistics purposes.

The `publicKey` field is the protobuf encoding of a peer ID. See the peer ID section above.

The `listenAddrs` field is a list of the binary representation of the multiaddresses where the
listener can be reached by the dialer.

The `observedAddr` field is the binary representation of the multiaddress of the dialer that the
listener can used to reach the dialer. TODO this is underspecified I think, and maybe wrong

The `protocols` field is a list of protocols that the listener supports.

## Relay

All the messages exchanged through the `relay` protocol are the binary representation of the
`CircuitRelay` struct defined here:

```protobuf
message CircuitRelay {
    enum Status {
        SUCCESS                    = 100;
        HOP_SRC_ADDR_TOO_LONG      = 220;
        HOP_DST_ADDR_TOO_LONG      = 221;
        HOP_SRC_MULTIADDR_INVALID  = 250;
        HOP_DST_MULTIADDR_INVALID  = 251;
        HOP_NO_CONN_TO_DST         = 260;
        HOP_CANT_DIAL_DST          = 261;
        HOP_CANT_OPEN_DST_STREAM   = 262;
        HOP_CANT_SPEAK_RELAY       = 270;
        HOP_CANT_RELAY_TO_SELF     = 280;
        STOP_SRC_ADDR_TOO_LONG     = 320;
        STOP_DST_ADDR_TOO_LONG     = 321;
        STOP_SRC_MULTIADDR_INVALID = 350;
        STOP_DST_MULTIADDR_INVALID = 351;
        STOP_RELAY_REFUSED         = 390;
        MALFORMED_MESSAGE          = 400;
    }

    enum Type {
        HOP = 1;
        STOP = 2;
        STATUS = 3;
        CAN_HOP = 4;
    }

    message Peer {
        required bytes id = 1;
        repeated bytes addrs = 2;
    }

    optional Type type = 1;
    optional Peer srcPeer = 2;
    optional Peer dstPeer = 3;
    optional Status code = 4;
}
```

All messages are prefixed with a variable-length integer indicating its size in bytes.

With the *relay* protocol, the dialer (denoted `A`) can ask the listener (denoted `B`) to dial
another a third node and relay all communications to it (denoted `C`).

The protocol works in the following steps:

- *A* asks *B* with the *relay* protocol.


TODO

## Kademlia

TODO

## Floodsub

TODO
