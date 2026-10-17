//! The version and build id the code logs (`agent::VERSION`, `agent::BUILD_ID`): the release
//! workflow sets REMINEDOG_VERSION and CI REMINEDOG_BUILD_ID; other builds use the crate's
//! version and "local".

use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=REMINEDOG_VERSION");
    println!("cargo:rerun-if-env-changed=REMINEDOG_BUILD_ID");
    let version = env::var("REMINEDOG_VERSION")
        .unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").expect("set by cargo"));
    let build = env::var("REMINEDOG_BUILD_ID").unwrap_or_else(|_| "local".into());
    println!("cargo:rustc-env=REMINEDOG_VERSION={version}");
    println!("cargo:rustc-env=REMINEDOG_BUILD_ID={build}");
}
