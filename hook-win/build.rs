//! Puts the version into reminedog.dll's version resource (Explorer's Properties > Details),
//! since the file keeps its name across versions. The release workflow sets
//! REMINEDOG_VERSION; other builds use the crate's version.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=REMINEDOG_VERSION");
    println!("cargo:rerun-if-env-changed=REMINEDOG_BUILD_ID");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env::var("REMINEDOG_VERSION")
        .unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").expect("set by cargo"));
    let build = env::var("REMINEDOG_BUILD_ID").unwrap_or_else(|_| "local".into());
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
    // Without a resource compiler (a GNU build lacking windres) the DLL just has no version
    // resource; the version still shows in the log.
    if let Err(e) =
        embed_resource::compile_for_cdylib(&path, embed_resource::NONE).manifest_optional()
    {
        println!("cargo:warning=no version resource: {e}");
    }
}

/// `1.2.3-beta.1` → `1,2,3,0` (the numeric version a version resource holds).
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

/// A string for the resource script: `"` doubled, nothing else special.
fn quoted(text: &str) -> String {
    text.replace('"', "\"\"")
}
