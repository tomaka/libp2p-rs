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

use core::Transport;
use core::nodes::swarm::SwarmLayer;
use ping;

/// Trait automatically implemented on all objects that implement `SwarmLayer`. Provides some
/// additional utilities.
pub trait SwarmLayerExt<TTrans, TFinalOutEvent>: SwarmLayer<TTrans, TFinalOutEvent>
where TTrans: Transport
{
    /// Appends a layer that automatically disconnects nodes if they stop responding to a
    /// periodic ping.
    #[inline]
    fn with_ping_auto_disconnect(self) -> ping::AutoDcLayer<Self>
    where Self:Sized {
        ping::AutoDcLayer::new(self)
    }
}

impl<TLayer, TTrans, TFinalOutEvent> SwarmLayerExt<TTrans, TFinalOutEvent> for TLayer
where TLayer: SwarmLayer<TTrans, TFinalOutEvent>,
      TTrans: Transport,
{}
