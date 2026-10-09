//! Services between plugins (plan §5.8.2): payloads are CBOR maps with named
//! fields, so new optional fields keep old readers working.
//!
//! ```ignore
//! pumbo_sdk::service! {
//!     /// example:counter@1.0
//!     pub service Counter("example:counter", 1, 0) provider CounterApi {
//!         fn increment(Increment) -> Count;
//!         fn get(Get) -> Count;
//!     }
//! }
//! // consumer (only in async handlers):
//! let n = Counter::client().increment(&Increment { by: 1 }).await?;
//! // inside a provider handler, keep the chain context:
//! let n = Counter::within(&call).get(&Get {}).await?;
//! // provider:
//! async fn on_service_call(&self, c: ServiceCall) -> Result<Vec<u8>, CallReject> {
//!     Counter::dispatch(self, c).await
//! }
//! ```

use crate::bindings::pumbo::prox::services::ServiceError;
use crate::bindings::pumbo::prox::types::Text;

pub fn to_cbor<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn from_cbor<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, String> {
    ciborium::from_reader(b).map_err(|e| e.to_string())
}

/// `check_offline` → `check-offline`.
pub fn method_name(ident: &str) -> String {
    ident.replace('_', "-")
}

/// Error of a typed client call.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientError {
    Service(ServiceError),
    /// The payload could not be encoded or the answer decoded.
    Codec(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Service(e) => write!(f, "service error: {e:?}"),
            ClientError::Codec(e) => write!(f, "codec error: {e}"),
        }
    }
}

/// Fail-closed helper: any service error becomes a refusal for the player.
pub fn require<T>(r: Result<T, ClientError>) -> Result<T, Text> {
    r.map_err(|_| {
        crate::text::mini(
            "<err>A service is temporarily unavailable. Please try again in a moment.",
        )
    })
}

/// Declares a service: constants, a typed client and a dispatcher for the
/// provider trait.
#[macro_export]
macro_rules! service {
    (
        $(#[$meta:meta])*
        $vis:vis service $name:ident($svc:literal, $major:literal, $minor:literal) provider $api:ident {
            $( $(#[$mmeta:meta])* fn $method:ident($req:ty) -> $resp:ty; )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default)]
        $vis struct $name {
            pub ctx: ::core::option::Option<u64>,
            pub player: ::core::option::Option<$crate::PlayerId>,
            pub timeout_ms: ::core::option::Option<u32>,
        }

        #[allow(async_fn_in_trait)]
        $vis trait $api {
            $( $(#[$mmeta])* async fn $method(&self, call: &$crate::ServiceCall, req: $req) -> ::core::result::Result<$resp, ::std::string::String>; )*
        }

        #[allow(dead_code)]
        impl $name {
            pub const NAME: &'static str = $svc;
            pub const MAJOR: u16 = $major;
            pub const MINOR: u16 = $minor;

            pub fn versioned() -> ::std::string::String {
                ::std::format!("{}@{}.{}", $svc, $major, $minor)
            }

            pub fn client() -> Self {
                Self::default()
            }

            /// A client inside a provider handler: keeps the chain context.
            pub fn within(call: &$crate::ServiceCall) -> Self {
                Self { ctx: ::core::option::Option::Some(call.ctx), player: call.player, timeout_ms: ::core::option::Option::None }
            }

            $(
                $(#[$mmeta])*
                pub async fn $method(&self, req: &$req) -> ::core::result::Result<$resp, $crate::service::ClientError> {
                    let payload = $crate::service::to_cbor(req).map_err($crate::service::ClientError::Codec)?;
                    let opts = $crate::services::CallOptions { timeout_ms: self.timeout_ms, player: self.player, ctx: self.ctx };
                    let out = $crate::services::call(
                        ::std::string::String::from($svc),
                        $crate::service::method_name(::core::stringify!($method)),
                        payload,
                        opts,
                    )
                    .await
                    .map_err($crate::service::ClientError::Service)?;
                    $crate::service::from_cbor(&out).map_err($crate::service::ClientError::Codec)
                }
            )*

            /// Routes a call to the provider trait.
            pub async fn dispatch<P: $api>(p: &P, call: $crate::ServiceCall) -> ::core::result::Result<::std::vec::Vec<u8>, $crate::CallReject> {
                if call.service != $svc {
                    return ::core::result::Result::Err($crate::CallReject::UnknownMethod);
                }
                if call.major != $major {
                    return ::core::result::Result::Err($crate::CallReject::Rejected(::std::format!("major version {} not provided", call.major)));
                }
                $(
                    if call.method == $crate::service::method_name(::core::stringify!($method)) {
                        let req: $req = $crate::service::from_cbor(&call.payload).map_err($crate::CallReject::Rejected)?;
                        let resp = p.$method(&call, req).await.map_err($crate::CallReject::Rejected)?;
                        return $crate::service::to_cbor(&resp).map_err($crate::CallReject::Rejected);
                    }
                )*
                ::core::result::Result::Err($crate::CallReject::UnknownMethod)
            }
        }
    };
}
