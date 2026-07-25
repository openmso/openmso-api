// SPDX-License-Identifier: Apache-2.0
//! OCP (OpenMSO Capture Protocol) reference bindings for Rust.
//!
//! NDJSON+binary framing, the capture-server serve loop, and the capture-client
//! used by frontends to launch or connect to a server. See `docs/protocol.md`
//! for the normative spec.

pub mod client;
pub mod framing;
pub mod server;

pub const PROTOCOL_VERSION: i64 = 0;
