//! reminedog.dll のバージョン情報のリソース（エクスプローラーのプロパティ > 詳細）に版を入れる。
//! ファイルは版が変わっても同じ名前のため。リリースのワークフローが REMINEDOG_VERSION を
//! （CI が REMINEDOG_BUILD_ID を）入れる。それ以外のビルドは、`reminedog_agent::VERSION` と
//! `BUILD_ID` と同じく crate の版を使う。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=REMINEDOG_VERSION");
    println!("cargo:rerun-if-env-changed=REMINEDOG_BUILD_ID");
    let version = env::var("REMINEDOG_VERSION")
        .unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").expect("set by cargo"));
    let build = env::var("REMINEDOG_BUILD_ID").unwrap_or_else(|_| "local".into());
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let numbers = numeric_version(&version);
    let rc = format!(
        r#"1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x2
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "reminedog (Minecraft in-game tools)"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "reminedog"
      VALUE "OriginalFilename", "reminedog.dll"
      VALUE "ProductName", "reminedog"
      VALUE "ProductVersion", "{version} ({build})"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        version = quoted(&version),
        build = quoted(&build),
    );
    let path = PathBuf::from(env::var("OUT_DIR").expect("set by cargo")).join("version.rc");
    fs::write(&path, rc).expect("write version.rc");
    if let Err(e) =
        embed_resource::compile_for_cdylib(&path, embed_resource::NONE).manifest_optional()
    {
        // MSVC は自分のリソースコンパイラを持つ。GNU のビルドには windres がないことがある：その DLL には
        // バージョン情報のリソースがないだけ（版はログには出る）。
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            panic!("cannot put the version resource in: {e}");
        }
        println!("cargo:warning=no version resource: {e}");
    }
}

/// `1.2.3-beta.1` → `1,2,3,0`（バージョン情報のリソースが持つ数の版）。
fn numeric_version(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let mut parts: Vec<u16> = core
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    parts.resize(4, 0);
    parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// リソースのスクリプトのための文字列：`"` を 2 つにする。ほかに特別なものはない。
fn quoted(text: &str) -> String {
    text.replace('"', "\"\"")
}
