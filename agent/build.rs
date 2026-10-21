//! エージェントがログと画面に出す版とビルドの id（`lib.rs` の [`VERSION`]・[`BUILD_ID`]）。
//! リリースのワークフローが REMINEDOG_VERSION を、CI が REMINEDOG_BUILD_ID を入れる。
//! それ以外のビルドは crate の版と "local"。

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
