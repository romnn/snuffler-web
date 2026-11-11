//! Generates the typed `BridgeServiceClient` through prost's service-generator hook.
//!
//! The hook is the same one tonic-build uses, so the client lands in the same file and
//! module as the prost messages it names.
//! The client is transport-agnostic: each method encodes its request, hands it to a
//! `BridgeTransport` implemented by the frontend over the wasm worker, and unwraps the
//! `BridgeResult` envelope into the typed response.
//! The compiler therefore checks the binding of method, request type and response type,
//! and that binding cannot drift from the `.proto`.

use std::collections::HashMap;

use color_eyre::eyre::{self, OptionExt as _, WrapErr as _, bail, eyre};
use heck::ToUpperCamelCase as _;
use proc_macro2::{Ident, TokenStream};
use prost_build::{Method, Service, ServiceGenerator};
use prost_reflect::{DescriptorPool, Kind};
use quote::{format_ident, quote};

/// Fully qualified name of the response envelope.
const BRIDGE_RESULT: &str = "snuffler_web_proto.BridgeResult";

/// Proto type of requests that carry no data; their client methods take no argument.
const EMPTY_REQUEST: &str = ".snuffler_web_proto.Empty";

/// Proto type of the one response whose samples travel beside the envelope.
///
/// The worker hands the trace samples back as a separate `Float32Array` instead of encoding
/// them into the protobuf payload, so these methods call `unary_with_buffer`.
const BUFFERED_RESPONSE: &str = ".snuffler_web_proto.ViewMeta";

/// Maps each response message to the `BridgeResult` oneof variant that carries it.
///
/// Keys are fully qualified message names without the leading dot, and values are the
/// variant names prost generates for the oneof.
///
/// # Errors
///
/// Fails when the envelope is missing, when one of its fields is not a message, or when two
/// fields carry the same message, which would make the response ambiguous.
pub fn response_variants(descriptors: &DescriptorPool) -> eyre::Result<HashMap<String, Ident>> {
    let envelope = descriptors
        .get_message_by_name(BRIDGE_RESULT)
        .ok_or_eyre("BridgeResult descriptor is missing")?;

    let mut variants = HashMap::new();
    for field in envelope.fields() {
        let Kind::Message(response) = field.kind() else {
            bail!("BridgeResult.{} does not carry a message", field.name());
        };

        // prost names a oneof variant after its field in upper camel case.
        let variant = format_ident!("{}", field.name().to_upper_camel_case());
        if variants
            .insert(response.full_name().to_owned(), variant)
            .is_some()
        {
            bail!(
                "BridgeResult carries {} more than once",
                response.full_name()
            );
        }
    }
    Ok(variants)
}

/// Emits the transport trait, the envelope helpers and the client for `BridgeService`.
pub struct ClientGenerator {
    /// The output of [`response_variants`].
    pub response_variants: HashMap<String, Ident>,
}

impl ServiceGenerator for ClientGenerator {
    fn generate(&mut self, service: Service, buf: &mut String) {
        // The hook cannot return an error, so a schema problem fails the crate's compilation
        // instead, with the same message the build script would have printed.
        let tokens = render_client(&service, &self.response_variants).unwrap_or_else(|err| {
            let message = format!("{err:#}");
            quote!(compile_error!(#message);)
        });

        // prost formats the whole output file afterwards, so unformatted tokens suffice.
        buf.push_str(&tokens.to_string());
    }
}

fn render_client(
    service: &Service,
    response_variants: &HashMap<String, Ident>,
) -> eyre::Result<TokenStream> {
    // The canonical gRPC service path, `package.Service`.
    // The generated Python dispatch derives the same string from the descriptor, so the two
    // generated ends agree by construction; the JS worker only relays it.
    let service_path = format!("{}.{}", service.package, service.proto_name);
    let methods = service
        .methods
        .iter()
        .map(|method| render_method(method, &service_path, response_variants))
        .collect::<eyre::Result<Vec<_>>>()?;

    Ok(quote! {
        /// Transport for the generated `BridgeServiceClient`.
        ///
        /// It marshals an encoded request across the bridge boundary and returns the encoded
        /// `BridgeResult`, plus the out-of-band `ys` samples for buffer-carrying methods.
        /// The frontend implements it over the wasm worker; native builds never call it.
        ///
        /// `method` is `&'static str` because every method id is a compile-time constant
        /// generated from the `.proto`, so nothing can name an rpc that is not in the schema.
        pub trait BridgeTransport {
            #[expect(
                async_fn_in_trait,
                reason = "the generated client is internal to this workspace"
            )]
            async fn unary(
                &self,
                operation: Operation,
                method: &'static str,
                request: Vec<u8>,
            ) -> Result<Vec<u8>, crate::TransportError>;

            #[expect(
                async_fn_in_trait,
                reason = "the generated client is internal to this workspace"
            )]
            async fn unary_with_buffer(
                &self,
                operation: Operation,
                method: &'static str,
                request: Vec<u8>,
            ) -> Result<(Vec<u8>, Vec<f32>), crate::TransportError>;
        }

        /// Decodes a `BridgeResult` envelope into its success payload.
        ///
        /// Python never raises across the boundary, so this is the single place where a
        /// remote error, a cancellation or a stale generation becomes an `Err` in Rust.
        fn bridge_ok(
            bytes: &[u8],
            method: &'static str,
        ) -> Result<bridge_result::Result, crate::CallError> {
            let envelope = <BridgeResult as ::prost::Message>::decode(bytes)
                .map_err(|source| crate::CallError::Decode { method, source })?;
            match envelope.result {
                Some(bridge_result::Result::Error(err)) => {
                    Err(crate::RemoteError::from(err).into())
                }
                Some(bridge_result::Result::Cancelled(_)) => {
                    Err(crate::CallError::Cancelled { method })
                }
                Some(bridge_result::Result::Stale(stale)) => Err(crate::CallError::Stale {
                    method,
                    expected_generation: stale.expected_generation,
                    actual_generation: stale.actual_generation,
                }),
                Some(payload) => Ok(payload),
                None => Err(crate::CallError::Empty { method }),
            }
        }

        /// Typed client for `BridgeService`, generated from the `.proto`.
        ///
        /// Each rpc gets one method that encodes the request, calls the transport and
        /// unwraps the matching response variant.
        /// Add or change rpcs in the `.proto`; this type is never edited by hand.
        pub struct BridgeServiceClient<T> {
            transport: T,
        }

        impl<T: BridgeTransport> BridgeServiceClient<T> {
            pub fn new(transport: T) -> Self {
                Self { transport }
            }

            #(#methods)*
        }
    })
}

fn render_method(
    method: &Method,
    service_path: &str,
    response_variants: &HashMap<String, Ident>,
) -> eyre::Result<TokenStream> {
    let name = format_ident!("{}", method.name);
    let operation = format_ident!("{}", method.proto_name);
    let wire_method = format!("/{service_path}/{}", method.proto_name);

    // Resolve the response type and the envelope variant that carries it
    let response = parse_type(&method.output_type)?;
    let output_message = method.output_proto_type.trim_start_matches('.');
    let variant = response_variants.get(output_message).ok_or_else(|| {
        eyre!(
            "BridgeService.{}: response {output_message} has no BridgeResult variant",
            method.proto_name
        )
    })?;

    // Empty requests become argument-free methods
    let (parameter, request) = if method.input_proto_type == EMPTY_REQUEST {
        (quote!(), quote!(Empty::default()))
    } else {
        let request_type = parse_type(&method.input_type)?;
        (quote!(, request: #request_type), quote!(request))
    };

    let tokens = if method.output_proto_type == BUFFERED_RESPONSE {
        // Buffered response
        quote! {
            pub async fn #name(
                &self #parameter
            ) -> Result<(#response, Vec<f32>), crate::CallError> {
                let (bytes, ys) = self
                    .transport
                    .unary_with_buffer(
                        Operation::#operation,
                        #wire_method,
                        ::prost::Message::encode_to_vec(&#request),
                    )
                    .await?;
                match bridge_ok(&bytes, #wire_method)? {
                    bridge_result::Result::#variant(response) => Ok((response, ys)),
                    _ => Err(crate::CallError::UnexpectedPayload { method: #wire_method }),
                }
            }
        }
    } else {
        // Plain response
        quote! {
            pub async fn #name(&self #parameter) -> Result<#response, crate::CallError> {
                let bytes = self
                    .transport
                    .unary(
                        Operation::#operation,
                        #wire_method,
                        ::prost::Message::encode_to_vec(&#request),
                    )
                    .await?;
                match bridge_ok(&bytes, #wire_method)? {
                    bridge_result::Result::#variant(response) => Ok(response),
                    _ => Err(crate::CallError::UnexpectedPayload { method: #wire_method }),
                }
            }
        }
    };
    Ok(tokens)
}

/// Parses a Rust type path as prost resolved it, such as `ViewParams` or `super::Empty`.
fn parse_type(path: &str) -> eyre::Result<syn::Type> {
    syn::parse_str(path).wrap_err_with(|| format!("prost type path {path:?} is not a Rust type"))
}
