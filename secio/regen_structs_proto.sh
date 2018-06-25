#!/bin/sh

# This script regenerates the `src/structs_proto.rs` file from `structs.proto`.

sudo docker run --rm -v `pwd`:/usr/code:z -w /usr/code rust /bin/bash -c " \
    apt-get update; \
    apt-get install -y protobuf-compiler; \
<<<<<<< HEAD
    cargo install protobuf; \
    protoc --rust_out . structs.proto; \
    protoc --rust_out . keys.proto"
=======
    cargo install --version 1 protobuf; \
    protoc --rust_out . structs.proto"
>>>>>>> c1e1cb6... The PeerId is the hash of the protobuf encoding

mv -f structs.rs ./src/structs_proto.rs
