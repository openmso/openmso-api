// SPDX-License-Identifier: Apache-2.0
//! OpenMSO Capture Protocol v1 — Rust bindings.
//!
//! The generated message types live in [`proto`]; [`server`] and [`client`]
//! are the two ends of the nng transport.

pub mod client;
pub mod encoding;
pub mod manifest;
pub mod server;
pub mod transport;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/omso.capture.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/omso.capture.v1.serde.rs"));
}

use proto::ErrorCode;

/// OCP major version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// What a plugin's request handlers return: the error arm is the `Error`
/// message that goes on the wire verbatim.
pub type Reply<T> = std::result::Result<T, proto::Error>;

pub type Result<T> = std::result::Result<T, Error>;

impl proto::Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        proto::Error { code: code as i32, message: message.into() }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ErrorInvalidRequest, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ErrorUnsupported, message)
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ErrorBusy, message)
    }

    pub fn device(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ErrorDevice, message)
    }
}

/// A local failure: transport, codec, or child process. Distinct from
/// [`proto::Error`], which is a well-formed refusal from the far end.
#[derive(Debug)]
pub enum Error {
    Nng(nng::Error),
    Decode(prost::DecodeError),
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The peer sent something the protocol does not allow here.
    Protocol(String),
    Remote(proto::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Nng(e) => write!(f, "nng: {e}"),
            Error::Decode(e) => write!(f, "malformed message: {e}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "bad manifest JSON: {e}"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Remote(e) => {
                let code = ErrorCode::try_from(e.code).unwrap_or(ErrorCode::Unspecified);
                write!(f, "{}: {}", code.as_str_name(), e.message)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<nng::Error> for Error {
    fn from(e: nng::Error) -> Self {
        Error::Nng(e)
    }
}

impl From<prost::DecodeError> for Error {
    fn from(e: prost::DecodeError) -> Self {
        Error::Decode(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}
