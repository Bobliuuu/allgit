//! Validating-writer server (docs/research/server.md) and its client `HttpBackend`.
//!
//! The server fronts any [`vcs_remote::Backend`] over a small HTTP/1.1 API (hand-rolled on
//! `std::net`, one thread per connection, no async runtime), authenticates bearer tokens,
//! applies roles and a per-zone write policy, and validates every write of the manifest key
//! (locks, zone policy) before the compare-and-swap. It keeps no state between requests.
//!
//! **There is no TLS here.** Bearer tokens travel in clear text; run the server on loopback
//! or a private network behind a TLS-terminating proxy (docs/server.md).

pub mod auth;
pub mod client;
pub mod http;
pub mod server;
pub mod validate;

pub use auth::{Principal, Role, TokenTable};
pub use client::HttpBackend;
pub use server::{Config, Server};
