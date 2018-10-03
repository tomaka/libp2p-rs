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

use proc_macro::TokenStream;
use syn::{DeriveInput, Data, DataStruct, Ident};

#[proc_macro_derive(ProtocolHandler)]
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
    let trait_to_impl = quote!{::libp2p::core::nodes::protocol_handler::ProtocolHandler};
    let either_ident = quote!{::libp2p::core::nodes::protocol_handler::Either};
    let either_output_ident = quote!{::libp2p::core::either::EitherOutput};
    let node_handler_event = quote!{::libp2p::core::nodes::handled_node::NodeHandlerEvent};
    let node_handler_endpoint = quote!{::libp2p::core::nodes::handled_node::NodeHandlerEndpoint};

    // Name of the type parameter that represents the substream.
    let substream_generic = {
        let mut n = "TSubstream".to_string();
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
        quote!{<#(#lf,)* #(#tp,)* #(#cst,)* #substream_generic>}
    };

    // Build the `where ...` clause of the trait implementation.
    let where_clause = {
        let additional = data_struct.fields.iter().map(|field| {
            let ty = &field.ty;
            quote!{#ty: #trait_to_impl<Substream = #substream_generic>}
        }).collect::<Vec<_>>();

        if let Some(in_where_clause) = in_where_clause {
            // TODO: correct with the coma?
            Some(quote!{#in_where_clause, #(#additional),*})
        } else if !additional.is_empty() {
            Some(quote!{where #(#additional),*})
        } else {
            None
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

    // The `OutEvent` associated type.
    let out_event = {
        let mut out_event = None;
        for field in data_struct.fields.iter() {
            let ty = &field.ty;
            let field_out = quote!{ <#ty as #trait_to_impl>::OutEvent };
            match out_event {
                Some(ev) => out_event = Some(quote!{ #either_ident<#ev, #field_out> }),
                ref mut ev @ None => *ev = Some(field_out),
            }
        }
        out_event.unwrap_or(quote!{()})     // TODO: `!` instead
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
                        fn(<<#ty as #trait_to_impl>::Protocol as ::libp2p::ConnectionUpgrade<#substream_generic>>::Output) -> #either_output_ident<TProto1Out, TProto2Out>
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
        protocol_ty.unwrap_or(quote!{::libp2p::upgrade::DummyProtocolUpgrade})
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

    // List of statements to put in `poll()`.
    let poll_stmts = data_struct.fields.iter().enumerate().map(|(field_n, field)| {
        let field_name = match field.ident {
            Some(ref i) => quote!{ self.#i },
            None => quote!{ self.#field_n },
        };

        let mut var = if field_n != 0 {
            quote!{ #either_ident::Second(a) }
        } else {
            quote!{ a }
        };

        for _ in 0 .. data_struct.fields.iter().count() - 1 - field_n {
            var = quote!{ #either_ident::First(#var) };
        }

        // TODO:
        /*let proto_build = {
            let mut build = quote!{ let proto = ; };
            for (field_n2, field2) in data_struct.fields.iter().enumerate() {
                let local_build = quote!{ upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(proto, #either_output_ident::First)) };
                build = quote!{ let proto = upgrade::or(proto, proto2); };
            }
            quote!{
                let proto1 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(proto, #either_output_ident::First));
                let mut proto2 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto2.listen_protocol(), #either_output_ident::Second));
                proto2.disable();
                upgrade::or(proto1, proto2)
            }
        };*/

        quote!{
            match #field_name.poll()? {
                Async::Ready(Some(#node_handler_event::Custom(a))) => {
                    return Ok(Async::Ready(Some(#node_handler_event::Custom(#var))));
                },
                Async::Ready(Some(#node_handler_event::OutboundSubstreamRequest((proto, a)))) => {
                    unimplemented!()
                    /*let proto = { #proto_build };
                    return Ok(Async::Ready(Some(#node_handler_event::OutboundSubstreamRequest((proto, #var)))));*/
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
            type OutEvent = #out_event;
            type Substream = #substream_generic;
            type Protocol = #protocol_ty;
            type OutboundOpenInfo = #out_info;

            #[inline]
            fn listen_protocol(&self) -> Self::Protocol {
                unimplemented!()    // TODO:
                /*let proto1 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto1.listen_protocol(), #either_output_ident::First));
                let proto2 = upgrade::toggleable(upgrade::map::<_, fn(_) -> _>(self.proto2.listen_protocol(), #either_output_ident::Second));
                upgrade::or(proto1, proto2)*/
            }

            #[inline]
            fn inject_fully_negotiated(&mut self, protocol: <Self::Protocol as ::libp2p::ConnectionUpgrade<TSubstream>>::Output, endpoint: #node_handler_endpoint<Self::OutboundOpenInfo>) {
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
            fn inject_dial_upgrade_error(&mut self, info: Self::OutboundOpenInfo, error: &::std::io::Error) {
                match info {
                    #(#inject_dial_upgrade_error_variants),*
                }
            }

            #[inline]
            fn shutdown(&mut self) {
                #(#shutdown_stmts);*
            }

            fn poll(&mut self) -> ::libp2p::futures::Poll<Option<#node_handler_event<(Self::Protocol, Self::OutboundOpenInfo), Self::OutEvent>>, ::std::io::Error> {
                use libp2p::futures::prelude::*;
                #(#poll_stmts)*
                Ok(Async::NotReady)
            }
        }
    };

    final_quote.into()
}
