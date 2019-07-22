// Copyright 2017-2018 Parity Technologies (UK) Ltd.
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

use crate::{
    transport::{Transport, TransportError, ListenerEvent},
    upgrade::{
        OutboundUpgrade,
        InboundUpgrade,
        apply_inbound,
        apply_outbound,
        UpgradeError,
        OutboundUpgradeApply,
        InboundUpgradeApply
    }
};
use futures::{future::Either, prelude::*};
use multiaddr::Multiaddr;
use std::{error, fmt, pin::Pin, task::Context, task::Poll};

#[derive(Debug, Copy, Clone)]
pub struct Upgrade<T, U> { inner: T, upgrade: U }

impl<T, U> Upgrade<T, U> {
    pub fn new(inner: T, upgrade: U) -> Self {
        Upgrade { inner, upgrade }
    }
}

impl<D, U, O, TUpgrErr> Transport for Upgrade<D, U>
where
    D: Transport,
    D::Output: AsyncRead + AsyncWrite + Unpin,
    D::Error: 'static,
    D::Dial: Unpin,
    D::Listener: Unpin,
    D::ListenerUpgrade: Unpin,
    U: InboundUpgrade<D::Output, Output = O, Error = TUpgrErr>,
    U: OutboundUpgrade<D::Output, Output = O, Error = TUpgrErr> + Clone,
    <U as InboundUpgrade<D::Output>>::Future: Unpin,
    <U as OutboundUpgrade<D::Output>>::Future: Unpin,
    TUpgrErr: std::error::Error + Send + Sync + 'static     // TODO: remove bounds
{
    type Output = O;
    type Error = TransportUpgradeError<D::Error, TUpgrErr>;
    type Listener = ListenerStream<D::Listener, U>;
    type ListenerUpgrade = ListenerUpgradeFuture<D::ListenerUpgrade, U>;
    type Dial = DialUpgradeFuture<D::Dial, U>;

    fn dial(self, addr: Multiaddr) -> Result<Self::Dial, TransportError<Self::Error>> {
        let outbound = self.inner.dial(addr.clone())
            .map_err(|err| err.map(TransportUpgradeError::Transport))?;
        Ok(DialUpgradeFuture {
            future: outbound,
            upgrade: Either::Left(Some(self.upgrade))
        })
    }

    fn listen_on(self, addr: Multiaddr) -> Result<Self::Listener, TransportError<Self::Error>> {
        let inbound = self.inner.listen_on(addr)
            .map_err(|err| err.map(TransportUpgradeError::Transport))?;
        Ok(ListenerStream { stream: inbound, upgrade: self.upgrade })
    }
}

/// Error produced by a transport upgrade.
#[derive(Debug)]
pub enum TransportUpgradeError<TTransErr, TUpgrErr> {
    /// Error in the transport.
    Transport(TTransErr),
    /// Error while upgrading to a protocol.
    Upgrade(UpgradeError<TUpgrErr>),
}

impl<TTransErr, TUpgrErr> fmt::Display for TransportUpgradeError<TTransErr, TUpgrErr>
where
    TTransErr: fmt::Display,
    TUpgrErr: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportUpgradeError::Transport(e) => write!(f, "Transport error: {}", e),
            TransportUpgradeError::Upgrade(e) => write!(f, "Upgrade error: {}", e),
        }
    }
}

impl<TTransErr, TUpgrErr> error::Error for TransportUpgradeError<TTransErr, TUpgrErr>
where
    TTransErr: error::Error + 'static,
    TUpgrErr: error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            TransportUpgradeError::Transport(e) => Some(e),
            TransportUpgradeError::Upgrade(e) => Some(e),
        }
    }
}

pub struct DialUpgradeFuture<T, U>
where
    T: TryFuture,
    T::Ok: AsyncRead + AsyncWrite + Unpin,
    U: OutboundUpgrade<T::Ok>
{
    future: T,
    upgrade: Either<Option<U>, OutboundUpgradeApply<T::Ok, U>>
}

impl<T, U> Unpin for DialUpgradeFuture<T, U>
where
    T: TryFuture,
    T::Ok: AsyncRead + AsyncWrite + Unpin,
    U: OutboundUpgrade<T::Ok>
{
}

impl<T, U> Future for DialUpgradeFuture<T, U>
where
    T: TryFuture + Unpin,
    T::Ok: AsyncRead + AsyncWrite + Unpin,
    U: OutboundUpgrade<T::Ok>,
    U::Future: Unpin,
    U::Error: std::error::Error + Send + Sync + 'static
{
    type Output = Result<U::Output, TransportUpgradeError<T::Error, U::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        // We use a `this` because the compiler isn't smart enough to allow mutably borrowing
        // multiple different fields from the `Pin` at the same time.
        let mut this = &mut *self;

        loop {
            let next = match this.upgrade {
                Either::Left(ref mut up) => {
                    let x = match TryFuture::try_poll(Pin::new(&mut this.future), cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(err)) =>
                            return Poll::Ready(Err(TransportUpgradeError::Transport(err))),
                        Poll::Ready(Ok(v)) => v,
                    };
                    let u = up.take().expect("DialUpgradeFuture is constructed with Either::Left(Some).");
                    Either::Right(apply_outbound(x, u))
                }
                Either::Right(ref mut up) => return Future::poll(Pin::new(up), cx).map_err(TransportUpgradeError::Upgrade)
            };
            this.upgrade = next
        }
    }
}

pub struct ListenerStream<T, U> {
    stream: T,
    upgrade: U
}

impl<T, U> Unpin for ListenerStream<T, U> {
}

impl<T, U, F> Stream for ListenerStream<T, U>
where
    T: TryStream<Ok = ListenerEvent<F>> + Unpin,
    F: TryFuture,
    F::Ok: AsyncRead + AsyncWrite + Unpin,
    U: InboundUpgrade<F::Ok> + Clone,
    U::Future: Unpin,
{
    type Item = Result<ListenerEvent<ListenerUpgradeFuture<F, U>>, TransportUpgradeError<T::Error, U::Error>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match TryStream::try_poll_next(Pin::new(&mut self.stream), cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(TransportUpgradeError::Transport(err)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(event))) => {
                let event = event.map(move |x| {
                    ListenerUpgradeFuture {
                        future: x,
                        upgrade: Either::Left(Some(self.upgrade.clone()))
                    }
                });

                Poll::Ready(Some(Ok(event)))
            }
        }
    }
}

pub struct ListenerUpgradeFuture<T, U>
where
    T: TryFuture,
    T::Ok: AsyncRead + AsyncWrite + Unpin,
    U: InboundUpgrade<T::Ok>
{
    future: T,
    upgrade: Either<Option<U>, InboundUpgradeApply<T::Ok, U>>
}

impl<T, U> Unpin for ListenerUpgradeFuture<T, U>
where
    T: TryFuture,
    T::Ok: AsyncRead + AsyncWrite + Unpin,
    U: InboundUpgrade<T::Ok>
{
}

impl<T, U> Future for ListenerUpgradeFuture<T, U>
where
    T: TryFuture + Unpin,
    T::Ok: AsyncRead + AsyncWrite + Unpin,
    U: InboundUpgrade<T::Ok>,
    U::Future: Unpin,
    U::Error: std::error::Error + Send + Sync + 'static
{
    type Output = Result<U::Output, TransportUpgradeError<T::Error, U::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        // We use a `this` because the compiler isn't smart enough to allow mutably borrowing
        // multiple different fields from the `Pin` at the same time.
        let mut this = &mut *self;

        loop {
            let next = match this.upgrade {
                Either::Left(ref mut up) => {
                    let x = match TryFuture::try_poll(Pin::new(&mut this.future), cx) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(err)) =>
                            return Poll::Ready(Err(TransportUpgradeError::Transport(err))),
                        Poll::Ready(Ok(v)) => v,
                    };
                    let u = up.take().expect("ListenerUpgradeFuture is constructed with Either::Left(Some).");
                    Either::Right(apply_inbound(x, u))
                }
                Either::Right(ref mut up) => return Future::poll(Pin::new(up), cx).map_err(TransportUpgradeError::Upgrade)
            };
            this.upgrade = next;
        }
    }
}

