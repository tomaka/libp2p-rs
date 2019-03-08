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

//! Allows adding a delay before a future completes.

use futures::prelude::*;
use std::{mem, time::Duration, time::Instant};

/// Adds a delay to the given future.
pub fn delay<T>(inner: T, latency: Duration) -> Delay<T>
where
    T: Future,
{
    Delay {
        inner: DelayInner::Inner(inner, latency),
    }
}

/// Wraps around a `Future` and adds a delay before the value is yielded.
pub struct Delay<TInner> where TInner: Future {
    inner: DelayInner<TInner>,
}

enum DelayInner<TInner> where TInner: Future {
    Inner(TInner, Duration),
    Waiting(tokio_timer::Delay, TInner::Item),
    Poisoned,
}

impl<TInner> Future for Delay<TInner>
where
    TInner: Future
{
    type Item = TInner::Item;
    type Error = TInner::Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            match mem::replace(&mut self.inner, DelayInner::Poisoned) {
                DelayInner::Inner(mut inner, duration) => {
                    match inner.poll()? {
                        Async::Ready(item) => {
                            let delay = tokio_timer::Delay::new(Instant::now() + duration);
                            self.inner = DelayInner::Waiting(delay, item);
                        }
                        Async::NotReady => {
                            self.inner = DelayInner::Inner(inner, duration);
                            break Ok(Async::NotReady);
                        }
                    }
                }

                DelayInner::Waiting(mut delay, item) => {
                    match delay.poll().unwrap() {       // TODO: don't unwrap
                        Async::Ready(()) => break Ok(Async::Ready(item)),
                        Async::NotReady => {
                            self.inner = DelayInner::Waiting(delay, item);
                            break Ok(Async::NotReady);
                        }
                    }
                }

                DelayInner::Poisoned => panic!("Future has already finished or errored"),
            }
        }
    }
}

