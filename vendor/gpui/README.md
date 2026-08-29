# GPUI numpad patch (fork)

OpenLogi patches three Zed GPUI platform crates so numpad digits keep their
physical identity in `Keystroke.key` (`kp6` instead of `6`).

The patch lives on a dedicated branch of a Zed fork — not in this directory
anymore:

**https://github.com/tournierjc/zed/tree/openlogi/numpad-digit-keys**

Root `Cargo.toml` `[patch."https://github.com/zed-industries/zed"]` and
`[patch.crates-io]` redirect `gpui`, `gpui_platform`, and the three platform
crates to that branch.

## Bumping GPUI

1. Rebase or cherry-pick `openlogi/numpad-digit-keys` onto the new zed commit
   OpenLogi pins in `Cargo.lock`.
2. `cargo update -p gpui --precise <rev>` (or the usual lock bump flow).
3. `cargo check -p openlogi-desktop`

Consider upstreaming to [zed-industries/zed](https://github.com/zed-industries/zed)
or [gpui-ce/gpui-ce](https://github.com/gpui-ce/gpui-ce) so the fork can go away.
