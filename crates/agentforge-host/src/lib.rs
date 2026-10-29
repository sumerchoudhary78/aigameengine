//! # agentforge-host
//!
//! The engine host: everything between the deterministic core and a client process.
//!
//! * [`protocol`] defines the versioned wire contract: request/response envelopes, stable
//!   error codes, typed operation arguments and results, the operation catalog and the JSON
//!   Schemas generated from those Rust types.
//! * [`session::Host`] owns the authoritative state of one session and is the single path
//!   through which it changes. Its [`protocol::Role`] is fixed when the launcher creates it;
//!   requests cannot change it.
//! * [`storage`] reads and atomically writes project and replay files.
//! * [`server`] runs a host over any line-oriented reader/writer pair (JSON Lines on
//!   stdin/stdout in the `agentforge` binary).
#![warn(missing_docs)]
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod protocol;
pub mod server;
pub mod session;
pub mod storage;

pub use protocol::Role;
pub use session::Host;
