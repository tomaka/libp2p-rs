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
        dial: dial,
        listen_on: (addr) => {
            throw "Listening on WebSockets is not possible from within a browser";
        },
    };
}

/// Turns a string multiaddress into a WebSockets string URL.
const multiaddr_to_ws = (addr) => {
    let parsed = addr.match(/^\/ip(4|6)\/(.*?)\/tcp\/(.*?)\/(ws|wss)$/);
    if (parsed != null) {
        if (parsed[1] == '6') {
            return parsed[4] + "://[" + parsed[2] + "]:" + parsed[3];
        } else {
            return parsed[4] + "://" + parsed[2] + ":" + parsed[3];
        }
    }

    throw "Address not supported: " + addr;
}

/// Attempt to dial a multiaddress.
const dial = (addr) => {
    let ws = new WebSocket(multiaddr_to_ws(addr));
    let reader = read_queue();

    return new Promise((resolve, reject) => {
        // TODO: handle ws.onerror properly after dialing has happened
        ws.onerror = (ev) => reject(ev);
        ws.onmessage = (ev) => reader.inject_blob(ev.data);
        ws.onopen = () => resolve({
            read: () => reader.next(),
            write: (data) => {
                ws.send(data);
                return promise_when_ws_finished(ws);
            },
            shutdown: () => {},
            close: () => ws.close()
        });
    });
}

/// Takes a WebSocket object and returns a Promise that resolves when bufferedAmount is 0.
const promise_when_ws_finished = (ws) => {
    if (ws.bufferedAmount == 0) {
        return Promise.resolve();
    }

    return new Promise((resolve, reject) => {
        setTimeout(function check() {
            if (ws.bufferedAmount == 0) {
                resolve();
            } else {
                setTimeout(check, 100);
            }
        }, 2);
    })
}

// Creates a reading system with the `inject_blob` and `next` methods.
const read_queue = () => {
    // Array of promises resolving to ArrayBuffers, that haven't been transmitted back with
    // `next` yet.
    let queue = new Array();
    // If `resolve` isn't null, it is a function.
    let pending_read = { resolve: null };

    return {
        // Inserts a new Blob in the queue.
        inject_blob: (blob) => {
            if (pending_read.resolve != null) {
                var resolve = pending_read.resolve;
                pending_read.resolve = null;

                var reader = new FileReader();
                reader.addEventListener("loadend", () => resolve(reader.result));
                reader.readAsArrayBuffer(blob);
            } else {
                queue.push(new Promise((resolve, reject) => {
                    var reader = new FileReader();
                    reader.addEventListener("loadend", () => resolve(reader.result));
                    reader.readAsArrayBuffer(blob);
                }));
            }
        },

        // Returns a Promise that yields the next entry as an ArrayBuffer.
        next: () => {
            if (queue.length != 0) {
                return queue.shift(0);
            } else {
                if (pending_read.resolve !== null)
                    throw "Internal error: already have a pending promise";
                return new Promise((resolve, reject) => {
                    pending_read.resolve = resolve;
                });
            }
        }
    };
};
