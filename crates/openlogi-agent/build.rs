//! Build script for openlogi-agent: embeds the Windows exe resources — the
//! app icon and a VERSIONINFO block — so Task Manager and Explorer identify
//! the background agent instead of showing a generic blank binary.
//!
//! Kept in sync with the twin in `crates/openlogi-gui/build.rs` — only the
//! description/filename strings differ. `embed-resource` is pinned to the
//! exact version already in Cargo.lock as gpui's own build-dependency, so it
//! adds an edge, not a crate, and cannot move the pinned gpui rev.

// A build script fails by panicking, so `expect` (with a message that surfaces
// in the build log) is the idiomatic error path here — exempt it from the
// workspace's strict runtime lints.
#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::{env, fs};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // rc.exe treats `\` in string literals as escapes; forward slashes are
    // accepted by every resource compiler embed-resource can drive.
    let icon = manifest_dir
        .join("../../design/icon/openlogi.ico")
        .display()
        .to_string()
        .replace('\\', "/");
    println!("cargo:rerun-if-changed={icon}");

    let (major, minor, patch) = (
        env::var("CARGO_PKG_VERSION_MAJOR").expect("CARGO_PKG_VERSION_MAJOR"),
        env::var("CARGO_PKG_VERSION_MINOR").expect("CARGO_PKG_VERSION_MINOR"),
        env::var("CARGO_PKG_VERSION_PATCH").expect("CARGO_PKG_VERSION_PATCH"),
    );
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "AprilNEA"
            VALUE "FileDescription", "OpenLogi Background Agent"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "openlogi-agent"
            VALUE "OriginalFilename", "openlogi-agent.exe"
            VALUE "ProductName", "OpenLogi"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_path = out.join("openlogi-agent.rc");
    fs::write(&rc_path, rc).expect("write generated .rc into OUT_DIR");
    // manifest_optional: a missing resource compiler downgrades to a cargo
    // warning (icon-less but working exe) instead of failing dev builds on
    // machines without the Windows SDK; release builds run on CI runners that
    // always carry rc.exe.
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("compile Windows resources");
}
