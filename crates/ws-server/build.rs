//! Build-time identity for `ws-server`.
//!
//! `src/provenance.rs` reads `STOCKSPOTTER_COMMIT` through `option_env!`, which
//! resolves at *compile* time. Cargo does not know that, so without the
//! directive below it would happily reuse an object file compiled with a
//! previous commit's stamp and the binary would confidently report the wrong
//! identity -- worse than reporting none at all.
//!
//! Nothing else belongs here. This is not a place to compute a commit: the
//! value is supplied by whoever runs the build (`ops/vps/deploy.sh` for a
//! deployment), so that a build cannot invent an identity for itself.

fn main() {
    println!("cargo:rerun-if-env-changed=STOCKSPOTTER_COMMIT");
}
