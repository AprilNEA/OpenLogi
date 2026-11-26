//! Build script for openlogi-overlay: embeds the Windows exe resources — the
//! app icon and a VERSIONINFO block — so Task Manager and Explorer identify
//! the ring helper instead of showing a generic blank binary.
//!
//! Kept in sync with its twins in `crates/openlogi-agent/build.rs` and
//! `crates/openlogi-desktop/build.rs` — only the description/filename strings
//! differ. `embed-resource` is pinned to the exact version already in
//! Cargo.lock as gpui's own build-dependency, so it adds an edge, not a crate,
//! and cannot move the pinned gpui rev.

#![expect(
    clippy::expect_used,
    reason = "a build script fails by panicking, so expect — whose message surfaces in the build log — is the idiomatic error path"
)]

use std::path::PathBuf;
use std::{env, fs};

fn main() {
    // `rust_i18n::i18n!` in `main.rs` reads `../openlogi-ui/locales` at macro
    // expansion time, and Cargo does not track a proc macro's file reads. Without
    // this, editing a catalog leaves the embedded translations stale until an
    // unrelated change happens to recompile this crate.
    println!("cargo:rerun-if-changed=../openlogi-ui/locales");

    reserve_debug_main_stack("openlogi-overlay");

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
            VALUE "FileDescription", "OpenLogi Actions Ring Overlay"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "openlogi-overlay"
            VALUE "OriginalFilename", "openlogi-overlay.exe"
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
    let rc_path = out.join("openlogi-overlay.rc");
    fs::write(&rc_path, rc).expect("write generated .rc into OUT_DIR");
    // manifest_optional: a missing resource compiler downgrades to a cargo
    // warning (icon-less but working exe) instead of failing dev builds on
    // machines without the Windows SDK; release builds run on CI runners that
    // always carry rc.exe.
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("compile Windows resources");
}

/// Give a debug build's main thread a 2 MiB stack on Windows, where the OS
/// reserves 1 MiB and Rust gives every other thread 2 MiB.
///
/// `rust_i18n::i18n!` expands all of `../openlogi-ui/locales` into the
/// initializer of `_RUST_I18N_BACKEND`, one `HashMap` literal per catalog.
/// Unoptimized, that closure keeps every entry on the stack until the map
/// takes it: its prologue asks `__chkstk` for 0x1352C0 bytes (1.21 MiB) and
/// the first `t!` on the main thread dies with `STATUS_STACK_OVERFLOW`
/// before the ring opens. Release inlines the closure and keeps nothing on
/// the stack, so it is left alone. Only the named binary gets the flag; test
/// harnesses translate on their own 2 MiB threads and need none.
///
/// Kept in sync with the twin in `crates/openlogi-desktop/build.rs`.
fn reserve_debug_main_stack(binary: &str) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("PROFILE").as_deref() != Ok("debug")
    {
        return;
    }
    let arg = if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        "/STACK:2097152"
    } else {
        "-Wl,--stack,2097152"
    };
    println!("cargo:rustc-link-arg-bin={binary}={arg}");
}
