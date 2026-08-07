# Standalone device asset registry migration

## Purpose

This document plans the migration of standalone-device artwork from source-owned
embedded files to the existing OpenLogi asset registry pipeline.

The immediate case is Logitech Litra Glow. The same design must support Litra
Beam and future standalone devices without adding one `include_bytes!` branch per
product.

The migration described here is implemented. The design rationale, invariants,
milestones, and acceptance criteria remain as a review record for future
standalone models. The source-owned Litra artwork was removed only after the
registry resolver, cache/bundle path, renderer, and workspace checks were in
place.

Implementation status:

- Litra Glow and Beam carry driver-owned registry model ids (`8c900`/`8c901`).
- Standalone assets use the existing registry, mirror, cache, and bundle path.
- Physical standalone keys and HID++ asset resolution remain unchanged.
- The GUI bounds registry artwork to its gallery/detail slot and uses the
  generated visual when no verified asset is available.
- The former `crates/openlogi-gui/product-art/` tree no longer exists.

## Executive decision

Standalone devices should use the same asset registry, cache, mirror selection,
SHA-256 verification, bundle-time sync, and offline fallback as HID++ devices.

The device transport and asset identity remain separate concepts:

- the raw HID driver identifies and controls a physical device;
- the asset registry identifies a product model with a registry `modelId`;
- the persisted configuration identifies a physical unit, never just a model;
- the GUI consumes a resolved asset path and does not know how the asset was
  obtained.

The target architecture is therefore:

```text
raw HID driver
    -> StandaloneDevice.registry_model_id
    -> GUI asset target
    -> AssetRegistry / AssetResolver
    -> bundle Resources/assets or user cache
    -> ResolvedAsset
    -> generic standalone-light renderer
```

The existing `DeviceModelInfo` HID++ type must not be fabricated for a raw
device. A standalone registry lookup is a separate lookup operation keyed by
the driver-provided registry model id.

## Current state

### Registry data already available

The live registry currently contains both relevant Litra products:

| Product | Registry model id | Depot | Registry type | Asset path |
|---|---|---|---|---|
| Litra Glow | `8c900` | `litra_glow` | `ILLUMINATION_LIGHT` | `v1/devices/litra_glow/` |
| Litra Beam | `8c901` | `litra_beam` | `ILLUMINATION_LIGHT` | `v1/devices/litra_beam/` |

Litra Glow currently publishes at least:

```text
front.png
back.png
metadata.json
manifest.json
OnboardData.json
default_configurations.json
video_calls.png
browsers.png
editing.png
```

The registry `manifest.json` maps model `8c900` to `front.png` through the
`device_image` resource. The client uses that verified front image directly;
optional light-adjustment metadata is intentionally not consumed.

The registry entry is already sufficient for the first migration. No registry
publication is required for Litra Glow or Litra Beam as long as the live mirrors
continue to expose these entries.

Before implementation, re-check the live data rather than relying on this
document's snapshot:

```sh
curl -fsSL https://assets.openlogi.org/index.json | jq '.devices.litra_glow'
curl -fsSL https://assets.openlogi.org/v1/devices/litra_glow/manifest.json
curl -fsSL https://assets.openlogi.org/v1/devices/litra_glow/metadata.json
```

The registry file hash for the current Litra Glow `front.png` is recorded in
`index.json`; the client already verifies it before writing the file.

### Current OpenLogi asset pipeline

The pipeline has three layers:

1. `openlogi-assets` loads `index.json`, selects a mirror, constructs URLs, and
   verifies downloaded files against the registry SHA-256.
2. `openlogi-cli assets sync` downloads all bundle-required registry depots into
   `crates/openlogi-gui/assets/` for release packaging.
3. `openlogi-gui::asset` resolves device assets from the read-only app bundle
   first and the per-user cache second.

The built-in sources are defined in
`crates/openlogi-assets/src/source.rs`:

- `https://assets.openlogi.org`;
- the versioned Cloudflare Pages alias;
- the versioned jsDelivr catalog and its package routes;
- an explicit `OPENLOGI_ASSETS` / `--base` override.

The user cache is:

```text
~/.local/share/openlogi/assets/
```

The exact base directory can still be changed by the repository's XDG path
environment variables. The GUI Settings -> Assets page controls automatic
download, mirror preference, refresh, and cache clearing. It does not currently
distinguish HID++ and standalone assets, which is the desired end state.

The release bundle uses:

```toml
[package.metadata.bundle]
resources = ["assets/**/*"]
```

and the macOS bundle task runs `openlogi assets sync` when
`OPENLOGI_BUNDLE_ASSETS=1`.

### Current standalone path

The standalone device pipeline is already implemented for Litra control:

- `openlogi-hid/src/standalone.rs` enumerates raw HID nodes;
- `openlogi-hid/src/write/litra.rs` matches the full vendor/product/usage tuple
  and exposes the Litra model descriptor;
- `openlogi-core::device::StandaloneDevice` crosses the agent/GUI boundary;
- `openlogi-agent-core::ipc::AgentSnapshot` carries `standalone` separately from
  HID++ receiver inventories;
- `openlogi-gui/src/state/devices.rs` adapts standalone records into the common
  GUI device list;
- the raw route and physical identity are already kept separate from HID++
  routes;
- `DeviceKind::Light` and `LightCapabilities` already exist;
- light settings and agent-owned light writes already exist.

The current asset-specific path is not registry-backed:

```text
crates/openlogi-gui/product-art/litra-glow/front.png
    -> app_assets.rs::LITRA_GLOW_BYTES
    -> app_assets.rs::standalone_artwork()
    -> DeviceRecord::standalone_artwork
    -> light_visual.rs::img(LITRA_GLOW)
```

The local image is embedded with `include_bytes!`, and the renderer has Litra
Glow-specific artwork and crop constants. Unknown standalone models use the
generated light visual.

### Current sync gap

The GUI runtime sync in `crates/openlogi-gui/src/asset/sync.rs` accepts only:

```rust
&[(DeviceModelInfo, Option<String>)]
```

`AppState::asset_models()` supplies only records with HID++ `model_info`.
Standalone records are therefore absent from targeted runtime downloads even
though their registry depots exist.

The command-line bundle sync is broader: it reads the complete registry and
downloads baseline files for every depot. Consequently, the current release
bundle can already contain the Litra registry asset when
`OPENLOGI_BUNDLE_ASSETS=1`, but the standalone GUI renderer cannot consume it.

This distinction is important:

- bundle population is already mostly covered by the standard sync;
- runtime standalone targeting and render-time resolution are not covered;
- the hardcoded local image hides the missing runtime path.

## Goals

### Required goals

- Resolve standalone artwork through the same `AssetRegistry` and
  `AssetResolver` used by HID++ assets.
- Use the driver-selected registry model id, not a guessed HID++ model object.
- Support both app-bundled and per-user cached assets.
- Preserve SHA-256 verification and safe path handling.
- Fetch only the standalone depot(s) needed at runtime.
- Keep all existing Settings -> Assets controls meaningful for standalone
  devices.
- Preserve the generated visual when an asset is unavailable, disabled, stale,
  not yet downloaded, or not registered.
- Preserve physical-device persistence and raw-HID identity semantics.
- Preserve the agent as the sole owner of device I/O.
- Make the renderer generic enough that Litra Beam does not require another
  embedded-artwork branch.
- Remove the source-owned Litra Glow PNG and its embedded `AppAssets` path only
  after the replacement has been verified.
- Keep release bundling deterministic when `OPENLOGI_BUNDLE_ASSETS=1`.

### Quality goals

- Avoid a second asset cache implementation for standalone devices.
- Avoid a second standalone-only network client.
- Avoid using configuration keys or serial numbers as asset cache keys.
- Avoid loading an asset before its registry hash has been verified.
- Avoid changing existing HID++ matching, inventory, or panel behavior.
- Keep the asset registry's product identity independent of GUI presentation.
- Keep future product additions localized to driver metadata and registry data.

### Non-goals

- Replacing the existing asset mirror infrastructure.
- Adding a plugin system.
- Moving device control out of the agent.
- Making the registry a source of driver capabilities. Capabilities remain owned
  by the driver and are used for control gating.
- Downloading every file in a depot at runtime.
- Adding support for unregistered devices without a generated fallback.
- Redesigning the Light panel.
- Changing the persisted physical-device key format unless implementation
  evidence shows it is necessary.
- Making remote assets a hard runtime dependency when bundled art or fallback
  art is available.

## Target contracts

### Registry model identity

Add a registry identity to the standalone driver descriptor. The driver already
knows the exact product variant after matching the raw HID tuple, so it is the
correct owner of this mapping.

Conceptually:

```rust
impl LitraModel {
    pub const fn registry_model_id(self) -> &'static str {
        match self {
            Self::Glow => "8c900",
            Self::Beam => "8c901",
        }
    }
}
```

The actual method name may differ, but the following rules are mandatory:

- the value is the exact registry `modelId` string;
- it is model-level, not physical-unit-level;
- it is not used as the persisted configuration key;
- an unsupported or unregistered driver variant returns `None` rather than a
  guessed id;
- the mapping is tested against a registry fixture.

For a general future driver, the same field belongs in its static model
descriptor, not in a GUI match table.

### `StandaloneDevice`

Append an optional registry model id to `openlogi_core::device::StandaloneDevice`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub registry_model_id: Option<String>,
```

The field must be appended, not inserted, because `StandaloneDevice` is inside
the IPC snapshot and bincode field order is wire-sensitive.

The field is populated by `openlogi-hid` from the driver descriptor. It is not
derived by the GUI from `vendor_id`, `product_id`, or `driver_id`.

Semantics:

- `Some("8c900")`: the driver has a registry-backed model identity;
- `None`: the driver is supported but has no registry asset, so use generated
  fallback art;
- a missing registry entry is not an enumeration error;
- a malformed registry id must not be accepted from remote JSON as a filesystem
  path. The id is used only for an index lookup.

### Persisted `DeviceIdentity`

Append the same optional model-level identity to
`openlogi_core::config::DeviceIdentity`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub registry_model_id: Option<String>,
```

This lets an offline standalone card resolve bundled or cached artwork after a
restart without a live raw-HID descriptor.

It must not replace:

- `config_key`, which identifies a physical device;
- `driver_id`, which identifies the control implementation;
- the raw route identity, which identifies the current interface.

Old TOML files must deserialize with `None`. New identity persistence must not
write serial numbers or raw OS node ids into the model-level field.

### GUI asset target

Replace the HID++-only `asset_models()` concept with an asset target abstraction.
The exact type can be private to `openlogi-gui`, for example:

```rust
enum AssetTarget {
    Hidpp {
        model: DeviceModelInfo,
        codename: Option<String>,
    },
    Standalone {
        registry_model_id: String,
    },
}
```

Requirements:

- deduplicate by registry/model lookup identity, not physical serial;
- preserve the existing HID++ matching and variant behavior;
- include persisted offline standalone identities when present;
- never construct a fake `DeviceModelInfo` for a raw device;
- expose a stable sync bookkeeping key such as
  `standalone:model:8c900`;
- keep `OPENLOGI_ASSETS` precedence and Settings source selection unchanged.

If a model is present in both a live standalone record and an offline identity,
the live record wins for display and route, but only one asset sync target is
created.

### `AssetResolver`

Add a standalone lookup method that finds a registry entry directly by its model
id. Conceptually:

```rust
pub fn resolve_registry_model(&self, registry_model_id: &str) -> Option<ResolvedAsset>
```

The implementation should share the existing safe file-loading code with the
HID++ resolver. It must not create a synthetic HID++ model to reuse the current
method.

The shared resolver should:

1. load the index from the first valid read root;
2. call `Index::find_by_model_id` using exact case-insensitive model matching;
3. select the depot from the index entry, never from a GUI-built path;
4. use the manifest's `device_image` resource where present;
5. fall back to the depot's standard front filename list;
6. require a parseable metadata file when the current `ResolvedAsset` contract
   requires metadata;
7. return the actual local `PathBuf` for the image;
8. return `None` when any required local file is absent or invalid;
9. leave the generated light visual as the caller fallback.

The result should remain a `ResolvedAsset` or a compatible shared type, not a
new standalone-only result with duplicated fields. It should contain enough
information for:

- display name;
- registry depot;
- front/image path;
- metadata;
- image dimensions;
- optional shared metadata used by existing HID++ renderers.

For a standalone light, the registry `front.png` is the primary visual. The
current mouse-specific side-image behavior must not be applied to a light.

### `DeviceRecord`

Remove `standalone_artwork: Option<&'static str>` and use the resolved asset
field for both HID++ and standalone registry assets. If keeping one field would
make the renderer ambiguous, add an explicitly named `standalone_asset`, but do
not keep both a dynamic registry asset and a permanent Litra-specific static
artwork field.

The preferred design is one resolved asset field with render dispatch based on
the device capability/kind:

- HID++ mouse/keyboard paths consume mouse metadata and image fields;
- standalone light paths consume the verified front image;
- a missing asset uses the existing generated fallback.

When a standalone asset resolves, use its registry display name in the card if
the driver/OS name is empty or less specific. The driver-provided name remains
the fallback. The `DeviceKind::Light` identity and `LightCapabilities` remain
driver-owned and must not be inferred solely from registry metadata.

### Asset metadata for lights

The client deliberately does not interpret product-specific light-adjustment
metadata. A verified registry front image is sufficient for the generic light
card, while power, brightness, and temperature remain driver-owned controls.
Ignoring optional adjustment fields keeps the shared metadata parser focused on
the fields used by existing HID++ renderers and avoids guessing the registry's
coordinate system for a visual effect that is not required for device control.

## Target data flow

### Development build

```text
OpenLogi GUI starts
    -> raw watcher finds Litra Glow
    -> StandaloneDevice carries registry_model_id = "8c900"
    -> GUI builds AssetTarget::Standalone("8c900")
    -> background sync fetches index.json
    -> background sync resolves 8c900 -> litra_glow
    -> baseline files are fetched and hash-checked
    -> ~/.local/share/openlogi/assets/litra_glow/* is written atomically
    -> AssetResolver is rebuilt
    -> GUI refreshes the existing device record
    -> light renderer uses the cached front.png
```

When the first render occurs before the sync completes, the generated visual is
expected. The successful sync must mark assets dirty and force a device-list
rebuild, as the existing HID++ path already does.

### Bundled release

With:

```sh
OPENLOGI_BUNDLE_ASSETS=1 cargo run -p xtask -- macos package
```

the existing CLI bundle sync downloads registry baseline files for `litra_glow`
and any other registry depot into:

```text
crates/openlogi-gui/assets/litra_glow/
```

`cargo bundle` copies them into:

```text
OpenLogi.app/Contents/Resources/assets/litra_glow/
```

At runtime, `AssetResolver` must prefer this bundle root over the user cache.
The release app should therefore show Litra Glow artwork without a network
request when the bundle contains the registry index and required files.

With `OPENLOGI_BUNDLE_ASSETS` disabled, the release app may fetch the targeted
standalone depot if automatic downloads are enabled. If downloads are disabled,
it must show the generated visual and remain usable.

### Clear cache

The existing behavior should remain:

- clearing the user cache removes only writable downloaded assets;
- bundled assets remain available;
- the resolver is rebuilt immediately;
- the GUI falls back to bundled artwork if present, otherwise generated art;
- a later manual refresh can repopulate the user cache.

### Mirror selection

Standalone downloads must use the same source selection as HID++ downloads:

1. `OPENLOGI_ASSETS`, when set;
2. the persisted Settings source preference;
3. automatic mirror race.

Do not add a Litra-specific URL, a second CDN, or a GUI-only endpoint.

## Sync design

### Runtime sync API

Refactor the GUI sync input from HID++ tuples to the target abstraction described
above. Keep the lower-level `AssetClient::fetch_entry_if_stale` unchanged unless
the implementation finds a concrete shared bug.

For each standalone target:

1. load the selected index;
2. find the registry entry by `registry_model_id`;
3. if no entry exists, log a debug/info-level fallback event and consider the
   target handled for the current attempt so it does not retry forever;
4. fetch the entry's baseline files;
5. fetch any explicitly required light files only if a later UI feature needs
   them;
6. do not fetch `back.png`, demo GIFs, or unrelated application images for the
   current Light panel;
7. preserve per-file hash validation and atomic replacement.

The baseline currently consists of metadata, manifest when listed, and a front
render. Confirm that `front.png`, `metadata.json`, and `manifest.json` are
selected for the Litra entries with a unit test.

### Bundle sync API

The CLI `openlogi assets sync` currently processes every registry entry. Keep
that behavior for the offline bundle; it gives releases a complete registry
snapshot and avoids trying to enumerate hardware in the packaging job.

Add tests or assertions proving that the Litra depot is not accidentally pruned
or skipped because its type is `ILLUMINATION_LIGHT` rather than `MOUSE` or
`KEYBOARD`.

Do not make the CLI bundle sync depend on a connected Litra. Packaging must work
on a machine with no Logitech hardware attached.

### Fetch bookkeeping

The existing GUI bookkeeping tracks model targets that have been synced in the
current process. Extend it to stable target keys:

```text
hidpp:<model key>:<extended id>:<codename>
standalone:model:8c900
```

Do not use the raw serial as the sync key. Two identical Litra Glow units share
one depot and should cause one asset fetch, while their physical configuration
keys remain distinct.

The following transitions must re-arm or force sync exactly as the existing
HID++ path does:

- a new standalone model appears;
- a manual Refresh is requested;
- Clear cache is followed by Refresh;
- a failed sync becomes eligible for retry;
- the asset source preference changes;
- a successful sync lands and the resolver must be rebuilt.

## Renderer migration

### Remove the hardcoded asset dependency

After the dynamic resolver is working, remove the Litra-specific path from:

- `crates/openlogi-gui/src/app_assets.rs`:
  - `LITRA_GLOW`;
  - `LITRA_GLOW_BYTES`;
  - `standalone_artwork()`;
  - the `AppAssets::load()` special case;
  - related tests.
- `crates/openlogi-gui/src/components/light_visual.rs`:
  - the static `LITRA_GLOW` import;
  - the fixed `include_bytes!`-derived artwork source;
  - Litra-specific crop or mask constants;
  - `uses_glow_artwork()` and its model-specific test.
- `crates/openlogi-gui/src/state/devices.rs`:
  - `standalone_artwork` field and lookup;
  - offline artwork reconstruction based only on VID/PID.
- `crates/openlogi-gui/src/app/home.rs` and `app/detail.rs`:
  - pass a resolved registry asset to the light visual instead of a static
    GPUI asset path.

Do not remove the embedded OpenLogi logo or action icons from `AppAssets`.

### Generic light visual input

Change the light visual API to receive the resolved asset, conceptually:

```rust
fn gallery(
    asset: Option<&ResolvedAsset>,
    online: bool,
    enabled: bool,
    settings: LightSettings,
    palette: Palette,
) -> AnyElement
```

The renderer should:

- use the verified registry front image when `asset` exists;
- apply online/offline opacity consistently;
- render the current power, brightness, and temperature state independently;
- use the generated visual if no asset exists;
- never assume that every light image has the same dimensions;
- never choose an image solely from `driver_id`.

No product-specific overlay is rendered. This is intentional: the registry
front image remains the source of truth, and the generic generated visual is
used only when no verified asset is available. A future overlay feature would
need an explicit registry contract for the displayed image frame before being
reintroduced.

### Asset path handling in GPUI

The embedded `AppAssets` source is appropriate for source-owned static artwork,
but not for a registry file that can come from the bundle or user cache. The
light renderer should use the local resolved `PathBuf` with the same image path
loading convention already used by the device render path.

The implementation must be checked in both contexts:

- release bundle path under `Contents/Resources/assets`;
- user-cache path under `~/.local/share/openlogi/assets`.

Do not convert a verified filesystem asset back into an unverified URL at render
time.

## Persistence and IPC impact

Adding `registry_model_id` to `StandaloneDevice` is an IPC wire change.

Required protocol work:

- append the field at the end of `StandaloneDevice`;
- bump `openlogi-agent-core::ipc::PROTOCOL_VERSION`;
- update the version history comment;
- update `crates/openlogi-agent-core/tests/wire_format.rs` golden fixtures;
- add a standalone Glow fixture with `registry_model_id = Some("8c900")`;
- add a fixture for a supported standalone model with `None`;
- verify existing HID++ and pre-existing standalone fields retain their order;
- run the full agent/GUI protocol tests.

Do not reorder tarpc service methods. This task does not require a new service
method or new device-I/O RPC. Asset resolution stays in the GUI and packaging
layers, so the agent should not gain asset-fetch responsibilities.

Adding `registry_model_id` to `DeviceIdentity` is a config-schema change but is
backward-compatible through `#[serde(default)]`.

Persistence requirements:

- old config files load without the field;
- an online standalone device persists the registry model id together with the
  existing identity fields;
- an offline standalone identity can resolve its asset from the saved field;
- the physical raw key remains the key for `LightSettings`;
- transient raw OS-node identities remain non-persistent;
- the registry model id never becomes a physical identity key;
- serial numbers remain excluded from model-level identity serialization where
  the current privacy rules require that.

## Display and identity rules

Use this precedence for standalone display data:

1. resolved registry display name, when the asset is present and the registry
   entry has a non-empty name;
2. driver/HID display name;
3. a generic driver/model fallback.

Use this precedence for control data:

1. live `LightCapabilities` from the driver;
2. persisted `DeviceIdentity.light_capabilities` while offline;
3. no light controls when neither exists.

Never infer power, brightness, temperature, color, or zones from registry asset
metadata. The registry image may describe a product but cannot prove that the
current driver supports a control.

Use this precedence for artwork:

1. verified bundled registry asset;
2. verified user-cache registry asset;
3. generated light visual.

The renderer must not show the Litra Glow image for Litra Beam, an unknown Litra
product, or an unknown driver.

## Files likely to change

The exact split may differ, but the handoff should expect changes in these
areas.

### Shared contracts

- `crates/openlogi-core/src/device.rs`
  - append `StandaloneDevice.registry_model_id`;
  - update tests and documentation.
- `crates/openlogi-core/src/config/device.rs`
  - append `DeviceIdentity.registry_model_id`;
  - update config fixtures.
- `crates/openlogi-hid/src/write/litra.rs`
  - expose the registry model id from the model descriptor;
  - add Glow/Beam mapping tests.
- `crates/openlogi-hid/src/standalone.rs`
  - populate the new field.

### Registry and resolver

- `crates/openlogi-assets/src/index.rs`
  - add/extend model-id lookup tests if needed.
- `crates/openlogi-assets/src/manifest.rs`
  - verify standalone `device_image` resolution with a fixture.
- `crates/openlogi-gui/src/asset.rs`
  - add direct registry-model resolution and shared light asset loading.
- `crates/openlogi-gui/src/asset/sync.rs`
  - accept HID++ and standalone targets.
- `crates/openlogi-gui/src/state.rs`
  - include standalone targets in asset sync and persistence.
- `crates/openlogi-gui/src/state/devices.rs`
  - attach the resolved asset to standalone records and offline records.

### GUI renderer

- `crates/openlogi-gui/src/components/light_visual.rs`
  - consume `ResolvedAsset` and the verified registry image.
- `crates/openlogi-gui/src/app/home.rs`
  - pass the dynamic light asset.
- `crates/openlogi-gui/src/app/detail.rs`
  - pass the dynamic light asset.
- `crates/openlogi-gui/src/app_assets.rs`
  - remove Litra-specific embedded artwork while retaining logo/icons.

### IPC and tests

- `crates/openlogi-agent-core/src/ipc.rs`
  - protocol version and documentation only; no new RPC is expected.
- `crates/openlogi-agent-core/tests/wire_format.rs`
  - update append-only golden fixtures.
- relevant `openlogi-core`, `openlogi-hid`, `openlogi-agent-core`, and GUI tests.

### Documentation and asset cleanup

- `docs/DEVELOPMENT.md`
  - document that `OPENLOGI_BUNDLE_ASSETS=1` includes standalone registry
    assets too.
- `docs/USAGE.md`
  - document that Settings and runtime sync cover standalone device artwork.
- `crates/openlogi-gui/product-art/litra-glow/front.png`
  - delete only in the final cleanup commit.
- `crates/openlogi-gui/product-art/litra-glow/README.md`
  - delete or replace with a short registry/provenance note, depending on the
    final redistribution decision.
- any references to `LITRA_GLOW`, `standalone_artwork`, or the old crop must be
  removed or intentionally retained with a documented reason.

## Test plan

### Registry fixture tests

Add small checked-in JSON fixtures or inline fixtures based on the live schema.
Do not make normal unit tests depend on the public network.

Cover:

- `index.json` finds `8c900` and `8c901`;
- `ILLUMINATION_LIGHT` is accepted as a light registry type if kind conversion
  is used for the resolved asset;
- `manifest.json` maps `device_image` to `front.png`;
- `metadata.json` with a front-image origin parses without light-specific
  fields;
- optional light-adjustment metadata is ignored without rejecting the depot;
- malformed required metadata still causes a resolver miss rather than a panic.

### Resolver tests

Use a temporary directory containing:

```text
index.json
litra_glow/
  front.png
  manifest.json
  metadata.json
```

Cover:

- direct registry-model resolution returns a `ResolvedAsset`;
- returned image path is inside the supplied root;
- model `8c900` resolves only `litra_glow`;
- model `8c901` does not resolve to `litra_glow`;
- missing front image returns `None`;
- missing metadata returns `None` if metadata is required by the current
  resolved-asset contract;
- bundle root is preferred over user cache;
- cache is used when bundle root is absent;
- malformed index falls back without panicking;
- a registry component containing separators cannot escape the root.

### Sync tests

Use a local test HTTP server or existing HTTP test helpers. Do not hit
`assets.openlogi.org` from the normal test suite.

Cover:

- standalone target `8c900` is included in a runtime sync;
- the sync downloads `index.json`, `front.png`, `manifest.json`, and
  `metadata.json` for the Litra depot;
- the sync does not download unrelated Litra files for the current Light panel;
- the file hash is checked before the file is committed;
- a checksum mismatch leaves the previous file untouched;
- two physical Glow units create one model-level fetch target;
- an unregistered model id logs/returns a fallback outcome without a retry loop;
- `OPENLOGI_ASSETS` overrides the saved source preference;
- manual Refresh works when automatic download is disabled;
- a completed sync rebuilds the resolver and refreshes the UI record;
- Clear cache leaves bundled assets usable.

### Core and config tests

Cover:

- old `StandaloneDevice` wire/config fixtures deserialize with no registry id;
- new fixtures round-trip `registry_model_id`;
- old `DeviceIdentity` TOML remains semantically unchanged;
- an online standalone identity persists the registry id;
- an offline standalone identity can resolve registry artwork;
- transient raw identities are still not persisted;
- physical keys for two serial-bearing identical lights remain distinct;
- the registry model id does not affect the physical config key.

### IPC tests

Cover:

- protocol version is the intended new value;
- golden bytes for `StandaloneDevice` include the appended field;
- existing field order and existing fixture bytes before the appended field do
  not move;
- snapshot serialization still carries receiver inventory and standalone
  inventory independently;
- GUI and agent reject mismatched protocol versions as before.

### GUI tests

Cover:

- a live Glow record receives a dynamic resolved asset, not a static app asset;
- a live Beam record receives Beam artwork, not Glow artwork;
- an unknown standalone model uses generated art;
- an unregistered but supported driver remains controllable with generated art;
- a registry miss does not hide the Light tab when `LightCapabilities` exists;
- only driver-advertised controls are rendered;
- offline Glow records resolve bundled/cache art from persisted model identity;
- cache refresh changes a silhouette to the registry image without restarting;
- a cache clear falls back correctly;
- existing HID++ mouse and keyboard render paths are unchanged;
- keyboard RGB still uses `Lighting`, not the standalone Light panel;
- light renderer works with arbitrary image dimensions and no hardcoded Glow crop.

### Packaging tests

Run with no physical device connected:

```sh
OPENLOGI_BUNDLE_ASSETS=1 cargo run -p xtask -- macos bundle
```

Verify:

```sh
test -f target/release/bundle/osx/OpenLogi.app/Contents/Resources/assets/index.json
test -f target/release/bundle/osx/OpenLogi.app/Contents/Resources/assets/litra_glow/front.png
test -f target/release/bundle/osx/OpenLogi.app/Contents/Resources/assets/litra_glow/manifest.json
test -f target/release/bundle/osx/OpenLogi.app/Contents/Resources/assets/litra_glow/metadata.json
```

Inspect the finished bundle and confirm no old embedded Litra path is required
by the renderer. The source file should be gone only after the code no longer
references it.

### Full local gate

Use the repository's standard commands:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

For a macOS UI check:

```sh
cargo run -p openlogi-gui
```

Quit an existing OpenLogi instance before judging a new GUI bundle. A plain
`cargo build` does not refresh the development app bundle.

### Hardware matrix

Hardware is required only after automated tests pass. Do not record serial
numbers in commits or test reports.

| Scenario | Expected result |
|---|---|
| Glow connected, automatic downloads on | registry image appears after sync |
| Glow connected, automatic downloads off, no bundle | generated visual remains, controls work |
| Glow connected, bundled assets present | image appears without network |
| Glow disconnected after first run | offline card retains cached/bundled image |
| Glow unplug/replug | same physical config key, image and controls return |
| Beam connected | Beam asset is selected, Glow asset is not used |
| Unknown/unsupported product tuple | no standalone record or generic fallback per driver behavior |
| Registry endpoint unavailable | existing local asset remains; otherwise generated visual |
| Asset hash mismatch | bad file is not used; fallback remains visible |
| Settings -> Refresh | targeted standalone depot is fetched |
| Settings -> Clear | bundle remains usable; user cache is repopulated only on refresh/sync |

## Rollout milestones

### Milestone 0: registry and provenance gate

Before code changes:

1. Confirm `8c900` and `8c901` are present in the live index.
2. Confirm the files required by the current UI exist and match the manifest.
3. Confirm the public mirror and bundled release use of the registry artwork are
   allowed by the asset repository's provenance/redistribution policy.
4. Record the source and license decision in documentation. The existing local
   product-art README explicitly says Logitech artwork is outside the
   MIT/Apache-2.0 code license; removing the local file must not erase that
   provenance requirement.
5. Capture a local fixture from the schema, without adding a large generated
   asset cache to Git.

Exit criterion: the data and redistribution path are acceptable, and the
implementation can be tested offline with fixtures.

### Milestone 1: model identity propagation

1. Add the driver-level registry model id.
2. Append `StandaloneDevice.registry_model_id`.
3. Populate Glow and Beam descriptors.
4. Add config identity support with serde defaults.
5. Update IPC version and wire goldens.
6. Add focused tests before touching rendering.

Exit criterion: a live standalone snapshot carries `8c900` for Glow and `8c901`
for Beam, while an unknown/unregistered model carries `None`.

### Milestone 2: standalone resolver

1. Add direct registry-model lookup to `AssetResolver`.
2. Share the existing `ResolvedAsset` loading and path safety logic.
3. Add temporary-root tests for bundle/cache resolution.

Exit criterion: a fixture-backed resolver returns a valid local Litra asset with
no HID++ model object and returns `None` safely when files are unavailable.

### Milestone 3: runtime sync integration

1. Introduce unified HID++/standalone sync targets.
2. Include live and persisted standalone registry ids in `AppState` targets.
3. Fetch Litra baseline files using the existing mirror selection and hash
   verification.
4. Preserve retry, manual Refresh, Clear, and resolver rebuild behavior.
5. Add local-server sync tests.

Exit criterion: a development run with a connected Glow downloads
`litra_glow` into the standard user cache and resolves it without restarting.

### Milestone 4: registry-backed Light renderer

1. Pass `ResolvedAsset` to the Light visual.
2. Render the resolved front image from bundle/cache.
3. Keep generated fallback and online/offline treatment.
4. Add Glow/Beam/unknown GUI fixtures.

Exit criterion: the GUI displays the correct registry image for each registered
model, and no model-specific embedded path is required.

### Milestone 5: packaging and offline behavior

1. Verify the CLI bundle sync includes Litra depots.
2. Build a bundle with `OPENLOGI_BUNDLE_ASSETS=1` without hardware.
3. Launch with network disabled and verify bundled rendering.
4. Launch without bundled assets and verify runtime cache/fallback behavior.
5. Verify Clear cache never removes bundled art.

Exit criterion: both release modes work and the no-network bundled mode shows
the registry Light image.

### Milestone 6: remove source-owned artwork

Only after all previous exit criteria:

1. Remove the Litra-specific `AppAssets` constants and loader branch.
2. Remove `DeviceRecord::standalone_artwork` and all static lookup code.
3. Remove the old crop/mask constants and static byte loader.
4. Delete `product-art/litra-glow/front.png`.
5. Delete or replace its README with the final registry provenance note.
6. Search the repository for all old symbols and paths.
7. Re-run the full workspace gate and packaging test.

Exit criterion: no production path references the deleted file, and a clean
checkout can build with registry fixtures/remote assets only.

## Failure and fallback policy

The following are expected, non-fatal conditions:

- registry is unreachable;
- selected mirror is unhealthy;
- index does not contain a driver model id;
- depot is missing a non-required optional file;
- cached file hash is stale and the replacement cannot be downloaded;
- user disabled automatic downloads;
- release was built without bundled assets.

For all of these, the GUI must preserve the device record and controls and use
the generated visual when no verified image is available.

The following are implementation errors or security failures and must be
surfaced in logs/tests:

- using an unverified file after a checksum mismatch;
- accepting a registry path with separators or traversal components;
- resolving Glow artwork for Beam or an unknown model id;
- persisting a transient raw OS-node identity as a physical key;
- creating a fake HID++ `DeviceModelInfo` for a standalone device;
- hiding the Light panel because artwork is unavailable;
- allowing an asset sync failure to erase a last-known device record;
- changing existing HID++ asset matching as a side effect of standalone support.

## Security and privacy requirements

- Keep the existing SHA-256-before-write verification.
- Keep atomic writes so readers never observe partial files.
- Keep `safe_component_path` for depot and file names.
- Never log serial numbers, unit ids, or raw node identities in normal asset
  messages.
- Do not use a serial number as an asset URL or registry lookup key.
- Do not include serial numbers in the registry model identity.
- Preserve the existing explicit `OPENLOGI_ASSETS` override behavior.
- Treat a mirror as untrusted input for path and content purposes; a valid index
  entry still does not permit arbitrary filesystem writes.

## Performance requirements

- Runtime sync must fetch one depot per registry model, not once per physical
  unit.
- Runtime sync must fetch only baseline files needed by the current renderer.
- Image loading must not block the GUI event loop for every inventory poll.
- Rebuild the resolver only when sync state changes, matching current behavior.
- Keep the existing early render fallback so startup is not blocked on HTTP.

## Open questions for a future visual-adjustment feature

These decisions should be answered in the PR or handoff thread, not guessed in
code:

1. **Artwork provenance:** Is the registry's Litra artwork approved for bundled
   redistribution in OpenLogi releases, or is it runtime-download-only?
2. **Metadata strictness:** If a future visual adjustment is introduced, should
   it require `metadata.json`, or should a valid `front.png` remain enough for a
   generic light card? The current renderer intentionally needs no adjustment
   metadata.
3. **Beam controls:** Is Litra Beam already fully supported by the current
   driver, or should its registry artwork be added now but remain generated until
   its control descriptor is implemented?
5. **Registry release pin:** The current client understands asset release
   `0.1.0`. If the registry mapping changes in a future release, should this
   task include a coordinated asset version update, or should it remain a
   follow-up?
6. **Display naming:** Should registry display names always override raw HID
   names when present, or only fill missing/less-specific names?

Recommended answers for the first implementation:

- approve bundled use if the asset repository policy permits it;
- keep front-only rendering and make light adjustment metadata optional;
- add Beam asset resolution only if the driver emits `8c901`; otherwise keep the
  generic fallback for unsupported Beam control;
- do not change the asset release pin in this task;
- use registry display name when non-empty, with the driver name as fallback.

## Definition of done

The task is complete only when all of the following are true:

- Litra Glow resolves through `AssetResolver` by registry model id `8c900`.
- Litra Beam, when emitted by the driver, resolves by `8c901` and never uses
  Glow artwork.
- Runtime sync includes standalone targets and writes to the normal user cache.
- Bundle sync includes the Litra depot in an offline release bundle.
- The GUI prefers bundle, then cache, then generated fallback.
- Settings -> Assets controls standalone asset refresh and cache behavior.
- The renderer has no hardcoded Litra image path or fixed Glow crop dependency.
- Existing keyboard RGB and HID++ device rendering are unchanged.
- Driver capabilities, not registry artwork, gate Light controls.
- Physical raw-device configuration keys remain stable and distinct.
- IPC changes are append-only, versioned, and covered by wire goldens.
- Old config files load without migration errors.
- No unverified or unsafe asset path can be rendered.
- The old `product-art/litra-glow/front.png` file is deleted only after the new
  path passes the bundle, cache, offline, and hardware checks.
- `cargo fmt --all -- --check` passes.
- `cargo test --workspace` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- Manual hardware results are recorded without serial numbers.

## Suggested commit sequence

Keep commits independently reviewable:

1. `feat(assets): carry standalone registry model identities`
2. `feat(assets): resolve standalone registry depots`
3. `feat(gui): sync standalone asset targets`
4. `feat(gui): render standalone lights from resolved assets`
5. `test(assets): cover standalone bundle and cache behavior`
6. `docs(assets): document standalone registry flow`
7. `refactor(gui): remove embedded litra artwork`

Do not combine the image deletion with the first resolver change. The deletion
should be the final, independently reviewable cleanup after the new path has
been exercised.
