//! The backend conformance suites over HTTP against a `vcs-server` somebody else started (the Docker image on
//! a directory or an S3 backend, a systemd host; docs/deploy.md "Verified"). Gated on `VCS_TEST_SERVER_URL`;
//! the server needs an admin token `VCS_TEST_SERVER_ADMIN_TOKEN` (default `tok-root`). Each run uses fresh
//! keys under the server's own namespace, so the remote does not have to be empty.
use vcs_remote::backend::{conformance, conformance_gc};
use vcs_remote::Backend;
use vcs_server::HttpBackend;

fn admin() -> Option<HttpBackend> {
    let Ok(url) = std::env::var("VCS_TEST_SERVER_URL") else {
        eprintln!("skipping: VCS_TEST_SERVER_URL not set (see docs/deploy.md)");
        return None;
    };
    let tok = std::env::var("VCS_TEST_SERVER_ADMIN_TOKEN").unwrap_or_else(|_| "tok-root".into());
    Some(HttpBackend::new(&url, &tok).unwrap())
}

#[test]
fn external_server_passes_conformance() {
    let Some(b) = admin() else { return };
    conformance(&b);
}

#[test]
fn external_server_passes_conformance_gc_when_its_backend_can_collect() {
    let Some(b) = admin() else { return };
    if !b.supports_gc() {
        eprintln!("server backend cannot be collected; skipping conformance_gc");
        return;
    }
    conformance_gc(&b);
}
