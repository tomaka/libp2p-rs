// Copyright 2020 Parity Technologies (UK) Ltd.
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

use crate::upgrade::{InboundUpgrade, OutboundUpgrade, ProtocolName, UpgradeInfo};
use futures::prelude::*;
use std::{any::Any, fmt, pin::Pin};

/// Wraps and abstracts around a type that implements `ProtocolName + Any`.
pub struct BoxedProtocolName {
    inner: Box<dyn AbstractProtocolName>,
}

/// Implementation detail. Implemented on any type that implements the `ProtocolName` and `Any`
/// traits.
trait AbstractProtocolName: Send {
    /// Clones the underlying object and returns a boxed version of it.
    fn clone_me(&self) -> Box<dyn AbstractProtocolName>;
    /// Same as [`ProtocolName::protocol_name`].
    fn protocol_name(&self) -> &[u8];
    /// Turns this `Box<dyn AbstractProtocolName>` into a `Box<dyn Any>`.
    fn into_box_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

impl<T: Clone + ProtocolName + Any + Send> AbstractProtocolName for T {
    fn clone_me(&self) -> Box<dyn AbstractProtocolName> {
        Box::new(self.clone())
    }

    fn protocol_name(&self) -> &[u8] {
        ProtocolName::protocol_name(self)
    }

    fn into_box_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(*self)
    }
}

impl ProtocolName for BoxedProtocolName {
    fn protocol_name(&self) -> &[u8] {
        self.inner.protocol_name()
    }
}

impl Clone for BoxedProtocolName {
    fn clone(&self) -> Self {
        BoxedProtocolName {
            inner: self.inner.clone_me(),
        }
    }
}

macro_rules! gen {
    ($trait_name:ident, $struct_name:ident, $trait_to_impl:ident, $method_to_impl:ident) => {
        /// Wraps and abstracts around a type that implements `InboundUpgrade + Clone`.
        pub struct $struct_name<TSubstream, TOut, TErr> {
            inner: Box<dyn $trait_name<TSubstream, Output = TOut, Error = TErr>>,
        }

        trait $trait_name<TSubstream>: fmt::Debug + Send {
            type Output;
            type Error;

            fn clone_me(
                &self,
            ) -> Box<dyn $trait_name<TSubstream, Output = Self::Output, Error = Self::Error>>;
            fn protocol_info(&self) -> Box<dyn Iterator<Item = BoxedProtocolName> + Send>;
            fn $method_to_impl(
                self: Box<Self>,
                sock: TSubstream,
                info: BoxedProtocolName,
            ) -> Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;
        }

        impl<T, TSubstream> $trait_name<TSubstream> for T
        where
            T: $trait_to_impl<TSubstream> + fmt::Debug + Clone + Send + 'static,
            <T::InfoIter as IntoIterator>::IntoIter: Send + 'static,
            T::Info: Send + 'static,
            T::Future: Send + 'static,
        {
            type Output = T::Output;
            type Error = T::Error;

            fn clone_me(
                &self,
            ) -> Box<dyn $trait_name<TSubstream, Output = Self::Output, Error = Self::Error>> {
                Box::new(self.clone())
            }

            fn protocol_info(&self) -> Box<dyn Iterator<Item = BoxedProtocolName> + Send> {
                let iter =
                    UpgradeInfo::protocol_info(self)
                        .into_iter()
                        .map(|name| BoxedProtocolName {
                            inner: Box::new(name),
                        });
                Box::new(iter)
            }

            fn $method_to_impl(
                self: Box<Self>,
                sock: TSubstream,
                info: BoxedProtocolName,
            ) -> Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>> {
                let info = info.inner.into_box_any().downcast().unwrap();
                $trait_to_impl::$method_to_impl(*self, sock, *info).boxed()
            }
        }

        impl<TSubstream, TOut, TErr> UpgradeInfo for $struct_name<TSubstream, TOut, TErr> {
            type Info = BoxedProtocolName;
            type InfoIter = Box<dyn Iterator<Item = Self::Info> + Send>;

            fn protocol_info(&self) -> Self::InfoIter {
                self.inner.protocol_info()
            }
        }

        impl<C, TOut, TErr> $trait_to_impl<C> for $struct_name<C, TOut, TErr>
        where
            C: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        {
            type Output = TOut;
            type Error = TErr;
            type Future = Pin<Box<dyn Future<Output = Result<TOut, TErr>> + Send>>;

            fn $method_to_impl(self, sock: C, info: Self::Info) -> Self::Future {
                self.inner.$method_to_impl(sock, info)
            }
        }

        impl<TSubstream, TOut, TErr> $struct_name<TSubstream, TOut, TErr> {
            /// Boxes the given upgrade.
            pub fn new<T>(upgrade: T) -> Self
            where
                T: $trait_to_impl<TSubstream, Output = TOut, Error = TErr>
                    + fmt::Debug
                    + Clone
                    + Send
                    + 'static,
                <T::InfoIter as IntoIterator>::IntoIter: Send + 'static,
                T::Info: Send + 'static,
                T::Future: Send + 'static,
            {
                $struct_name {
                    inner: Box::new(upgrade),
                }
            }
        }

        impl<TSubstream, TOut, TErr> fmt::Debug for $struct_name<TSubstream, TOut, TErr> {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                fmt::Debug::fmt(&self.inner, f)
            }
        }

        impl<TSubstream, TOut, TErr> Clone for $struct_name<TSubstream, TOut, TErr> {
            fn clone(&self) -> Self {
                $struct_name {
                    inner: self.inner.clone_me(),
                }
            }
        }
    };
}

gen!(
    AbstractInboundUpgrade,
    BoxedInboundUpgrade,
    InboundUpgrade,
    upgrade_inbound
);
gen!(
    AbstractOutboundUpgrade,
    BoxedOutboundUpgrade,
    OutboundUpgrade,
    upgrade_outbound
);
