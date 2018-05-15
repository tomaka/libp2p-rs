- The listening and observed addresses should be reportable separately because a node's listening
  address depends on its observed address.

- At which level is p2p-circuit used? Should it be reported in the Kademlia results? Should nodes
  automatically use p2p-circuit when failing to access a node?

- DHT polluted by many ports for each node.

- Protocol compatibility effort needs to be done.

- Precise specs should be written.

- The `p2p-circuit` protocol requires a Peer ID, and I don't think it should be necessary.
