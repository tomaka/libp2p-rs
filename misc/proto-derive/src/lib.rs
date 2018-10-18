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

#![recursion_limit = "256"]

extern crate proc_macro;
#[macro_use]
extern crate syn;
#[macro_use]
extern crate quote;

use self::proc_macro::TokenStream;
use syn::{DeriveInput, Data, DataStruct, Ident};

/// The interface that satisfies Rust.
#[proc_macro_derive(ProtocolsHandler)]
pub fn hello_macro_derive(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    build(&ast)
}

/// The actual implementation.
fn build(ast: &DeriveInput) -> TokenStream {
    match ast.data {
        Data::Struct(ref s) => build_struct(ast, s),
        Data::Enum(_) => unimplemented!(),       // TODO:
        Data::Union(_) => unimplemented!(),     // TODO:
    }
}

/// The version for structs
fn build_struct(ast: &DeriveInput, data_struct: &DataStruct) -> TokenStream {
    let name = &ast.ident;
    let (_, ty_generics, in_where_clause) = ast.generics.split_for_impl();
    let trait_to_impl = quote!{::libp2p::core::nodes::protocols_handler::ProtocolsHandler};
    let either_ident = quote!{::libp2p::core::nodes::protocols_handler::Either};
    let either_output_ident = quote!{::libp2p::core::either::EitherOutput};
    let proto_handler_event = quote!{::libp2p::core::nodes::protocols_handler::ProtocolsHandlerEvent};
    let node_handler_endpoint = quote!{::libp2p::core::nodes::handled_node::NodeHandlerEndpoint};

    // Name of the type parameter that represents the substream.
    let substream_generic = {
        let mut n = "TSubstream".to_string();
        // Avoid collisions.
        while ast.generics.type_params().any(|tp| tp.ident.to_string() == n) {
            n.push('1');
        }
        let n = Ident::new(&n, name.span());
        quote!{#n}
    };

    // Name of the type parameter that represents the output event.
    let out_event_generic = {
        let mut n = "TOutEvent".to_string();
        // Avoid collisions.
        while ast.generics.type_params().any(|tp| tp.ident.to_string() == n) {
            n.push('1');
        }
        let n = Ident::new(&n, name.span());
        quote!{#n}
    };

    // Build the generics.
    let impl_generics = {
        let tp = ast.generics.type_params();
        let lf = ast.generics.lifetimes();
        let cst = ast.generics.const_params();
        quote!{<#(#lf,)* #(#tp,)* #(#cst,)* #substream_generic, #out_event_generic>}
    };

    // Build the `where ...` clause of the trait implementation.
    let where_clause = {
        let mut additional = data_struct.fields.iter().flat_map(|field| {
            let ty = &field.ty;
            vec![
                quote!{#ty: #trait_to_impl<Substream = #substream_generic, OutEvent = #out_event_generic>},
                // TODO: are these required?
                quote!{<#ty as #trait_to_impl>::Protocol: ::libp2p::ConnectionUpgrade<#substream_generic>},
                quote!{<<#ty as #trait_to_impl>::Protocol as ::libp2p::ConnectionUpgrade<#substream_generic>>::Future: Send + 'static},
                quote!{<<#ty as #trait_to_impl>::Protocol as ::libp2p::ConnectionUpgrade<#substream_generic>>::Output: 'static},
                quote!{<#ty as #trait_to_impl>::Substream: ::libp2p::tokio_io::AsyncRead + ::libp2p::tokio_io::AsyncWrite},
            ]
        }).collect::<Vec<_>>();

        additional.push(quote!{
            #substream_generic: ::libp2p::tokio_io::AsyncRead + ::libp2p::tokio_io::AsyncWrite
        });
        additional.push(quote!{
            #out_event_generic: 'static
        });

        if let Some(in_where_clause) = in_where_clause {
            // TODO: correct with the coma?
            Some(quote!{#in_where_clause, #(#additional),*})
        } else {
            Some(quote!{where #(#additional),*})
        }
    };

    // Build the list of statements to put in the body of `inject_inbound_closed()`.
    let inject_inbound_closed_stmts = data_struct.fields.iter().enumerate().map(|(field_n, field)| {
        match field.ident {
            Some(ref i) => quote!{ self.#i.inject_inbound_closed(); },
            None => quote!{ self.#field_n.inject_inbound_closed(); },
        }
    });

    // Build the list of statements to put in the body of `shutdown()`.
    let shutdown_stmts = data_struct.fields.iter().enumerate().map(|(field_n, field)| {
        match field.ident {
            Some(ref i) => quote!{ self.#i.shutdown(); },
            None => quote!{ self.#field_n.shutdown(); },
        }
    });

    // Build the list of variants to put in the body of `inject_event()`.
    //
    // The event type is a construction of nested `#either_ident`s of the events of the children.
    // We call `inject_event` on the corresponding child.
    let inject_event_variants = data_struct.fields.iter().enumerate().map(|(field_n, field)| {
        let mut elem = if field_n != 0 {
            quote!{ #either_ident::Second(ev) }
        } else {
            quote!{ ev }
        };

        for _ in 0 .. data_struct.fields.iter().count() - 1 - field_n {
            elem = quote!{ #either_ident::First(#elem) };
        }

        match field.ident {
            Some(ref i) => quote!{ #elem => self.#i.inject_event(ev) },
            None => quote!{ #elem => self.#field_n.inject_event(ev) },
        }
    });

    // Build the list of variants to put in the body of `inject_dial_upgrade_error()`.
    //
    // The info type is a construction of nested `#either_ident`s of the info of the children.
    // We call `inject_*` only on the corresponding child.
    let inject_dial_upgrade_error_variants = data_struct.fields.iter().enumerate().map(|(field_n, field)| {
        let mut elem = if field_n != 0 {
            quote!{ #either_ident::Second(info) }
        } else {
            quote!{ info }
        };

        for _ in 0 .. data_struct.fields.iter().count() - 1 - field_n {
            elem = quote!{ #either_ident::First(#elem) };
        }

        match field.ident {
            Some(ref i) => quote!{ #elem => self.#i.inject_dial_upgrade_error(info, error) },
            None => quote!{ #elem => self.#field_n.inject_dial_upgrade_error(info, error) },
        }
    });

    // Build the list of variants to put in the body of `inject_fully_negotiated()`.
    let inject_fully_negotiated_variants = data_struct.fields.iter().enumerate().flat_map(|(field_n, field)| {
        let (mut either_output, mut either) = if field_n != 0 {
            (quote!{ #either_output_ident::Second(protocol) }, quote!{ #either_ident::Second(info) })
        } else {
            (quote!{ protocol }, quote!{ info })
        };

        for _ in 0 .. data_struct.fields.iter().count() - 1 - field_n {
            either_output = quote!{ #either_output_ident::First(#either_output) };
            either = quote!{ #either_ident::First(#either) };
        }

        let field_ident = match field.ident {
            Some(ref i) => quote!{ self.#i },
            None => quote!{ self.#field_n },
        };

        let dialer = quote!{
            (#either_output, #node_handler_endpoint::Dialer(#either)) => {
                #field_ident.inject_fully_negotiated(protocol, #node_handler_endpoint::Dialer(info));
            }
        };

        let listener = quote!{
            (#either_output, #node_handler_endpoint::Listener) => {
                #field_ident.inject_fully_negotiated(protocol, #node_handler_endpoint::Listener);
            }
        };

        vec![dialer, listener]
    });

    // The `InEvent` associated type.
    let in_event = {
        let mut in_event = None;
        for field in data_struct.fields.iter() {
            let ty = &field.ty;
            let field_in = quote!{ <#ty as #trait_to_impl>::InEvent };
            match in_event {
                Some(ev) => in_event = Some(quote!{ #either_ident<#ev, #field_in> }),
                ref mut a @ None => *a = Some(field_in),
            }
        }
        in_event.unwrap_or(quote!{()})     // TODO: `!` instead
    };

    // Final output of the `Protocol` associated type.
    let protocol_ty_output = {
        let mut ty_out = None;

        for field in data_struct.fields.iter() {
            let field_out = {
                let ty = &field.ty;
                quote!{
                    <<#ty as #trait_to_impl>::Protocol as ::libp2p::ConnectionUpgrade<#substream_generic>>::Output
                }
            };

            match ty_out {
                Some(o) => ty_out = Some(quote!{
                    #either_output_ident<#o, #field_out>
                }),
                ref mut o @ None => *o = Some(field_out),
            }
        }

        ty_out.unwrap_or(quote!{()})        // TODO: use !
    };

    // The `Protocol` associated type.
    let protocol_ty = {
        let mut protocol_ty = None;
        for field in data_struct.fields.iter() {
            let ty = &field.ty;
            let field_proto = quote!{
                ::libp2p::core::upgrade::toggleable::Toggleable<
                    ::libp2p::core::upgrade::map::Map<
                        <#ty as #trait_to_impl>::Protocol,
                        fn(<<#ty as #trait_to_impl>::Protocol as ::libp2p::ConnectionUpgrade<#substream_generic>>::Output) -> #protocol_ty_output
                    >
                >
            };

            match protocol_ty {
                Some(ev) => protocol_ty = Some(quote!{
                    ::libp2p::core::upgrade::OrUpgrade<#ev, #field_proto>
                }),
                ref mut ev @ None => *ev = Some(field_proto),
            }
        }
        protocol_ty.unwrap_or(quote!{::libp2p::core::upgrade::DummyProtocolUpgrade})
    };

    // The `OutboundOpenInfo` associated type.
    let out_info = {
        let mut out_info = None;
        for field in data_struct.fields.iter() {
            let ty = &field.ty;
            let field_info = quote!{ <#ty as #trait_to_impl>::OutboundOpenInfo };
            match out_info {
                Some(ev) => out_info = Some(quote!{ #either_ident<#ev, #field_info> }),
                ref mut ev @ None => *ev = Some(field_info),
            }
        }
        out_info.unwrap_or(quote!{()})     // TODO: `!` instead
    };

    // The content of `listen_protocol()`.
    let listen_protocol = {
        let mut out_listen = None;

        for (field_n, field) in data_struct.fields.iter().enumerate() {
            let field_name = match field.ident {
                Some(ref i) => quote!{ self.#i },
                None => quote!{ self.#field_n },
            };

            let mut wrapped_out = if field_n != 0 {
                quote!{ #either_output_ident::Second(p) }
            } else {
                quote!{ p }
            };
            for _ in 0 .. data_struct.fields.iter().count() - 1 - field_n {
                wrapped_out = quote!{ #either_output_ident::First(#wrapped_out) };
            }

            let wrapped_proto = quote! {
                upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(#field_name.listen_protocol(), |p| #wrapped_out))
            };

            match out_listen {
                Some(li) => out_listen = Some(quote!{ upgrade::or(#li, #wrapped_proto) }),
                ref mut li @ None => *li = Some(wrapped_proto),
            }
        }

        out_listen.unwrap_or(quote!{()})     // TODO: incorrect
    };

    // List of statements to put in `poll()`.
    //
    // We poll each child one by one and wrap around the output.
    let poll_stmts = data_struct.fields.iter().enumerate().map(|(field_n, field)| {
        let field_name = match field.ident {
            Some(ref i) => quote!{ self.#i },
            None => quote!{ self.#field_n },
        };

        let mut wrapped_info = if field_n != 0 {
            quote!{ #either_ident::Second(info) }
        } else {
            quote!{ info }
        };
        for _ in 0 .. data_struct.fields.iter().count() - 1 - field_n {
            wrapped_info = quote!{ #either_ident::First(#wrapped_info) };
        }

        let proto_build = {
            let mut out_proto_build = None;

            for (in_field_n, field) in data_struct.fields.iter().enumerate() {
                let field_name = match field.ident {
                    Some(ref i) => quote!{ self.#i },
                    None => quote!{ self.#in_field_n },
                };

                let mut wrapped_out = if in_field_n != 0 {
                    quote!{ #either_output_ident::Second(p) }
                } else {
                    quote!{ p }
                };
                for _ in 0 .. data_struct.fields.iter().count() - 1 - in_field_n {
                    wrapped_out = quote!{ #either_output_ident::First(#wrapped_out) };
                }

                let wrapped_proto = if in_field_n != field_n {
                    // TODO: should use `listen_protocol()` ; instead have some sort of None
                    quote!{{
                        let mut upgr = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(#field_name.listen_protocol(), |p| #wrapped_out));
                        upgr.disable();
                        upgr
                    }}
                } else {
                    quote!{upgrade}
                };

                match out_proto_build {
                    Some(li) => out_proto_build = Some(quote!{ upgrade::or(#li, #wrapped_proto) }),
                    ref mut li @ None => *li = Some(wrapped_proto),
                }
            }

            out_proto_build.unwrap_or(quote!{()})     // TODO: incorrect
        };

        quote!{
            match #field_name.poll()? {
                Async::Ready(Some(#proto_handler_event::Custom(a))) => {
                    return Ok(Async::Ready(Some(#proto_handler_event::Custom(a))));
                },
                Async::Ready(Some(#proto_handler_event::OutboundSubstreamRequest { upgrade, info })) => {
                    let upgrade = #proto_build;
                    return Ok(Async::Ready(Some(#proto_handler_event::OutboundSubstreamRequest {
                        upgrade,
                        info: #wrapped_info,
                    })));
                },
                Async::Ready(None) => return Ok(Async::Ready(None)),
                Async::NotReady => ()
            };
        }
    });

    // Now the magic happens.
    let final_quote = quote!{
        impl #impl_generics #trait_to_impl for #name #ty_generics
        #where_clause
        {
            type InEvent = #in_event;
            type OutEvent = #out_event_generic;
            type Substream = #substream_generic;
            type Protocol = #protocol_ty;
            type OutboundOpenInfo = #out_info;

            #[inline]
            fn listen_protocol(&self) -> Self::Protocol {
                use libp2p::core::upgrade;
                #listen_protocol
            }

            #[inline]
            fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ::libp2p::ConnectionUpgrade<#substream_generic>>::Output, endpoint: #node_handler_endpoint<Self::OutboundOpenInfo>) {
                match (protocol, endpoint) {
                    #(#inject_fully_negotiated_variants,)*
                    _ => panic!("The passed OutboundOpenInfo don't match the expected protocol output")
                }
            }

            #[inline]
            fn inject_event(&mut self, event: Self::InEvent) {
                match event {
                    #(#inject_event_variants),*
                }
            }

            #[inline]
            fn inject_inbound_closed(&mut self) {
                #(#inject_inbound_closed_stmts);*
            }

            #[inline]
            fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: ::std::io::Error) {
                match info {
                    #(#inject_dial_upgrade_error_variants),*
                }
            }

            #[inline]
            fn shutdown(&mut self) {
                #(#shutdown_stmts);*
            }

            fn poll(&mut self) -> ::libp2p::futures::Poll<Option<#proto_handler_event<Self::Protocol, Self::OutboundOpenInfo, Self::OutEvent>>, ::std::io::Error> {
                use libp2p::futures::prelude::*;
                use libp2p::core::upgrade;
                #(#poll_stmts)*
                Ok(Async::NotReady)
            }
        }
    };

    final_quote.into()
}
