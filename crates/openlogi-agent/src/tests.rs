/// Mirror of the overlay's guard: the `i18n!` in `main.rs` reaches the shared
/// catalog by relative path, and a wrong path does **not** fail the build:
/// `rust_i18n` compiles it to an empty catalog, and every tray string renders
/// as its semantic key. Resolve one tray key so that breakage is loud without
/// pinning any translation wording.
#[test]
fn the_shared_catalog_is_wired_up() {
    const KEY: &str = "app.show_main_window";
    assert_ne!(rust_i18n::t!(KEY), KEY);
}
