//! Link flags for a faster process start (docs/research/startup-cost.md in the PR that added this):
//! a non-PIE executable has no relative relocations for the loader to apply (the binary has ~19k)
//! and lazy binding keeps the loader from resolving every libc symbol before `main`.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "linux" && env == "gnu" && std::env::var_os("VCS_PIE").is_none() {
        for a in ["-no-pie", "-Wl,-z,lazy", "-Wl,-z,norelro"] {
            println!("cargo:rustc-link-arg-bins={a}");
        }
    }
}
