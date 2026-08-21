//! Derives `openlogi_hidpp`'s `Feature` / `CreatableFeature` boilerplate for
//! feature structs whose `new` is purely mechanical.
//!
//! This crate is a private implementation detail of `openlogi-hidpp` (lib
//! name `hidpp`): the generated code references that crate's internal
//! `crate::channel`/`crate::feature` paths directly, so the derive only makes
//! sense invoked from inside it.
//!
//! ```ignore
//! #[derive(Feature)]
//! #[creatable(id = 0x1000, version = 0)]
//! pub struct BatteryStatusFeature {
//!     endpoint: FeatureEndpoint,
//! }
//! ```
//!
//! Two field shapes are recognised:
//!
//! - `{ endpoint: FeatureEndpoint }` — a non-emitting feature.
//! - `{ endpoint: FeatureEndpoint, events: EventSource<SomeEvent> }` — an
//!   emitting feature; this shape also derives
//!   `EmittingFeature<SomeEvent>`, delegating to the `events` field.
//!
//! `version` defaults to `0` when omitted. Any other field layout is a
//! compile error rather than a silently wrong implementation — this derive
//! only covers the mechanical shapes; a feature whose construction does
//! anything else keeps a hand-written `impl CreatableFeature`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Data, DeriveInput, Fields, GenericArgument, LitInt, PathArguments, Type, parse_macro_input,
};

/// See the crate-level docs.
#[proc_macro_derive(Feature, attributes(creatable))]
pub fn derive_feature(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(Feature)] does not support generic structs",
        ));
    }

    let ident = &input.ident;
    let (id, version) = parse_feature_attr(input)?;
    // `Some(event_ty)` is the `{ endpoint, events: EventSource<event_ty> }`
    // shape; `None` is the plain `{ endpoint }` shape.
    let event_ty = parse_shape(input)?;

    let new_fn = if event_ty.is_some() {
        quote! {
            fn new(
                chan: ::std::sync::Arc<crate::channel::HidppChannel>,
                device_index: u8,
                feature_index: u8,
            ) -> Self {
                let events =
                    crate::feature::EventSource::attach(&chan, device_index, feature_index);
                Self {
                    endpoint: crate::feature::FeatureEndpoint::new(chan, device_index, feature_index),
                    events,
                }
            }
        }
    } else {
        quote! {
            fn new(
                chan: ::std::sync::Arc<crate::channel::HidppChannel>,
                device_index: u8,
                feature_index: u8,
            ) -> Self {
                Self {
                    endpoint: crate::feature::FeatureEndpoint::new(chan, device_index, feature_index),
                }
            }
        }
    };

    let emitting_impl = event_ty.map(|event_ty| {
        quote! {
            impl crate::feature::EmittingFeature<#event_ty> for #ident {
                fn listen(&self) -> async_channel::Receiver<#event_ty> {
                    self.events.listen()
                }
            }
        }
    });

    Ok(quote! {
        impl crate::feature::CreatableFeature for #ident {
            const ID: u16 = #id;
            const STARTING_VERSION: u8 = #version;

            #new_fn
        }

        impl crate::feature::Feature for #ident {}

        #emitting_impl
    })
}

/// Parses `#[creatable(id = 0x1000, version = 0)]`. `version` defaults to `0`.
fn parse_feature_attr(input: &DeriveInput) -> syn::Result<(u16, u8)> {
    let mut id = None;
    let mut version = 0u8;

    for attr in &input.attrs {
        if !attr.path().is_ident("creatable") {
            continue;
        }

        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("id") {
                id = Some(meta.value()?.parse::<LitInt>()?.base10_parse::<u16>()?);
                Ok(())
            } else if meta.path.is_ident("version") {
                version = meta.value()?.parse::<LitInt>()?.base10_parse::<u8>()?;
                Ok(())
            } else {
                Err(meta.error("expected `id` or `version`"))
            }
        })?;
    }

    id.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "#[derive(Feature)] requires #[creatable(id = <feature id>)]",
        )
    })
    .map(|id| (id, version))
}

/// Recognises the struct's field layout as one of the two shapes this derive
/// supports, returning the `events` field's event type for the emitting
/// shape (`None` for the plain shape), or a compile error naming what was
/// found instead.
fn parse_shape(input: &DeriveInput) -> syn::Result<Option<Type>> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Feature)] only supports structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "#[derive(Feature)] requires named fields (`{ endpoint: FeatureEndpoint, .. }`)",
        ));
    };

    let shape_error = || {
        syn::Error::new_spanned(
            fields,
            "#[derive(Feature)] only supports `{ endpoint: FeatureEndpoint }` or \
             `{ endpoint: FeatureEndpoint, events: EventSource<Event> }`",
        )
    };

    let mut endpoint_present = false;
    let mut event_ty = None;

    for field in &fields.named {
        let Some(name) = &field.ident else {
            return Err(shape_error());
        };
        if name == "endpoint" {
            if !is_type_named(&field.ty, "FeatureEndpoint") {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "the `endpoint` field must be of type `FeatureEndpoint`",
                ));
            }
            endpoint_present = true;
        } else if name == "events" {
            event_ty = Some(event_source_arg(&field.ty).ok_or_else(|| {
                syn::Error::new_spanned(
                    &field.ty,
                    "the `events` field must be of type `EventSource<Event>`",
                )
            })?);
        } else {
            return Err(shape_error());
        }
    }

    if !endpoint_present {
        return Err(shape_error());
    }

    Ok(event_ty)
}

/// Whether `ty`'s last path segment is named `name`, ignoring any qualifying
/// path (fields are always written unqualified, but this is robust either
/// way).
fn is_type_named(ty: &Type, name: &str) -> bool {
    let Type::Path(path) = ty else { return false };
    path.path
        .segments
        .last()
        .is_some_and(|seg| seg.ident == name)
}

/// Extracts `Event` from a field typed `EventSource<Event>`.
fn event_source_arg(ty: &Type) -> Option<Type> {
    let Type::Path(path) = ty else { return None };
    let seg = path.path.segments.last()?;
    if seg.ident != "EventSource" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        GenericArgument::Type(ty) => Some(ty.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use quote::quote;

    use super::*;

    fn derive_input(tokens: proc_macro2::TokenStream) -> DeriveInput {
        syn::parse2(tokens).expect("test fixture must parse as an item")
    }

    #[test]
    fn plain_shape_has_no_event_type_and_version_defaults_to_zero() {
        let input = derive_input(quote! {
            #[creatable(id = 0x1000)]
            struct Foo {
                endpoint: FeatureEndpoint,
            }
        });

        assert_eq!(parse_feature_attr(&input).unwrap(), (0x1000, 0));
        assert!(parse_shape(&input).unwrap().is_none());
    }

    #[test]
    fn emitting_shape_extracts_the_event_source_type_argument() {
        let input = derive_input(quote! {
            #[creatable(id = 0x4600, version = 2)]
            struct Foo {
                endpoint: FeatureEndpoint,
                events: EventSource<CrownEvent>,
            }
        });

        assert_eq!(parse_feature_attr(&input).unwrap(), (0x4600, 2));
        let event_ty = parse_shape(&input).unwrap().expect("emitting shape");
        assert_eq!(
            quote!(#event_ty).to_string(),
            quote!(CrownEvent).to_string()
        );
    }

    #[test]
    fn field_order_does_not_affect_shape_detection() {
        let input = derive_input(quote! {
            #[creatable(id = 0x4600)]
            struct Foo {
                events: EventSource<CrownEvent>,
                endpoint: FeatureEndpoint,
            }
        });

        assert!(parse_shape(&input).unwrap().is_some());
    }

    #[test]
    fn missing_id_is_an_error() {
        let input = derive_input(quote! {
            struct Foo {
                endpoint: FeatureEndpoint,
            }
        });

        parse_feature_attr(&input).unwrap_err();
    }

    #[test]
    fn unknown_attribute_key_is_an_error() {
        let input = derive_input(quote! {
            #[creatable(id = 0x1000, bogus = 1)]
            struct Foo {
                endpoint: FeatureEndpoint,
            }
        });

        parse_feature_attr(&input).unwrap_err();
    }

    #[test]
    fn struct_without_an_endpoint_field_is_an_error() {
        let input = derive_input(quote! {
            struct Foo {
                events: EventSource<CrownEvent>,
            }
        });

        parse_shape(&input).unwrap_err();
    }

    #[test]
    fn wrong_endpoint_field_type_is_an_error() {
        let input = derive_input(quote! {
            struct Foo {
                endpoint: NotAnEndpoint,
            }
        });

        parse_shape(&input).unwrap_err();
    }

    #[test]
    fn unrecognised_field_name_is_an_error() {
        let input = derive_input(quote! {
            struct Foo {
                endpoint: FeatureEndpoint,
                extra: u8,
            }
        });

        parse_shape(&input).unwrap_err();
    }

    #[test]
    fn non_struct_input_is_an_error() {
        let input = derive_input(quote! {
            enum Foo {
                A,
                B,
            }
        });

        parse_shape(&input).unwrap_err();
    }

    #[test]
    fn generic_struct_is_rejected_before_shape_parsing() {
        let input = derive_input(quote! {
            #[creatable(id = 0x1000)]
            struct Foo<T> {
                endpoint: FeatureEndpoint,
                _marker: std::marker::PhantomData<T>,
            }
        });

        expand(&input).unwrap_err();
    }
}
