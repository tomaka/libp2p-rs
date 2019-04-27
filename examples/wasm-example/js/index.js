import { ws } from 'rust-libp2p-ext-ws';

import("../crate/pkg")
    .then(module => module.start(ws()))
    .then(results => console.log(results));
