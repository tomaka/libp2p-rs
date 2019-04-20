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

const multiaddr = require("multiaddr");

export default () => {
    return {
        listen_on: (addr) => {
            throw "Listening on WebSockets is not possible from within a browser";
        },
        dial: (addr) => {
            // TODO: handle ip6
            let parsed_addr = addr.match(/^\/ip4\/(.*?)\/tcp\/(.*?)\/ws$/);
            if (parsed_addr == null)
                throw "Address not supported: " + addr;
            let ws = new WebSocket("ws://" + parsed_addr[1] + ":" + parsed_addr[2]);
            let read_data = new Array();
            let pending_read = { promise: null };
            return new Promise((resolve, reject) => {
                // TODO: handle ws.onerror properly after dialing has happened
                ws.onerror = (ev) => reject(ev);
                ws.onmessage = (ev) => {
                    if (pending_read.promise !== null) {
                        var reader = new FileReader();
                        reader.addEventListener("loadend", function() {
                            pending_read.promise(reader.result);
                            pending_read.promise = null;
                        });
                        reader.readAsArrayBuffer(ev.data);
                    } else {
                        read_data.push(ev.data);
                    }
                };
                ws.onopen = () => resolve({
                    read: () => {
                        if (read_data.length != 0) {
                            return new Promise((resolve, reject) => {
                                var reader = new FileReader();
                                reader.addEventListener("loadend", function() {
                                    resolve(reader.result);
                                });
                                reader.readAsArrayBuffer(read_data.shift());
                            });
                        } else {
                            return new Promise((resolve, reject) => {
                                if (pending_read.promise !== null)
                                    throw "Already have a pending promise; broken contract";
                                pending_read.promise = resolve;
                            });
                        }
                    },
                    write: (data) => {
                        ws.send(data);
                        // TODO: use setTimeout and bufferedAmount instead of succeeding instantly
                        return Promise.resolve();
                    },
                    shutdown: () => {},
                    close: () => ws.close()
                });
            });
        }
    };
}
