//! Validating-writer server (docs/research/server.md) and its client `HttpBackend`.
//!
//! The server fronts any [`vcs_remote::Backend`] over a small HTTP/1.1 API (hand-rolled on
//! `std::net`, one thread per connection, no async runtime), authenticates bearer tokens,
//! applies roles and a per-zone write policy, and validates every write of the manifest key
//! (locks, zone policy) before the compare-and-swap. It keeps no state between requests.
//!
//! TLS is available behind the `tls` cargo feature (rustls); without it the server speaks plain
//! HTTP and must sit on loopback, a private network or behind a TLS-terminating proxy (docs/server.md).

pub mod audit;
pub mod auth;
pub mod backend_url;
pub mod client;
pub mod glob;
pub mod http;
pub mod policy;
pub mod ratelimit;
pub mod readguard;
pub mod server;
#[cfg(feature = "tls")]
pub mod tls;
pub mod validate;

pub use audit::AuditLog;
pub use auth::{Principal, Role, TokenTable};
pub use client::HttpBackend;
pub use policy::Policy;
pub use ratelimit::RateLimit;
pub use server::{Config, Handle, Server};
pub use validate::ValidatorConfig;
