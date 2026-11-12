use super::*;
use openlogi_assets::DeviceEntry;
use openlogi_core::device::DeviceTransports;
use std::collections::HashMap;

/// A resolver over `roots` — the bundle first when there are two, as in
/// production — with `index` already in memory instead of read from them.
fn resolver_over(roots: &[&Path], index: Option<Index>) -> AssetResolver {
    AssetResolver {
        read_roots: roots.iter().map(|root| root.to_path_buf()).collect(),
        write_root: roots[roots.len() - 1].to_path_buf(),
        has_bundle: roots.len() > 1,
        index,
        resolved: RefCell::default(),
    }
}

fn mx_master_3s_entry(model_ids: Vec<String>) -> DeviceEntry {
    DeviceEntry {
        model_id: "2b043".to_string(),
        model_ids,
        display_name: "MX Master 3S".to_string(),
        kind: "mouse".to_string(),
        asset_path: "assets/mx_master_3s/".to_string(),
        files: Vec::new(),
    }
}

fn index_of(depot: &str, entry: DeviceEntry) -> Index {
    let mut devices = HashMap::new();
    devices.insert(depot.to_string(), entry);
    Index {
        schema_version: 1,
        devices,
    }
}

/// The current registry: the 3S depot lists both bolt pids Logi ships for
/// it (`b043` via a Bolt receiver, `b034` over BTLE).
fn mx_master_3s_index() -> Index {
    index_of(
        "mx_master_3s",
        mx_master_3s_entry(vec!["2b043".into(), "2b034".into()]),
    )
}

/// A legacy index generated before `modelIds` existed: only the primary
/// pid `2b043` is listed, so the BTLE pid `b034` matches nothing.
fn legacy_mx_master_3s_index() -> Index {
    index_of("mx_master_3s", mx_master_3s_entry(Vec::new()))
}

/// An MX Master 3S connected over BTLE reports bolt pid `b034` / ext 1.
/// The strict `{ext}{pid}` key (`1b034`) matches no registry entry — the
/// depot lists `2b034`/`2b043` (ext 2) — so the suffix `b034` is what
/// bridges it.
fn btle_3s_model() -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports {
            btle: true,
            ..Default::default()
        },
        model_ids: [0xb034, 0, 0],
        extended_model_id: 0x01,
    }
}

#[test]
fn secondary_pid_resolves_btle_3s_without_codename() {
    // The fix: the depot lists `2b034` alongside `2b043`, so the suffix
    // match on `b034` resolves the BTLE 3S by pid — no codename needed.
    let index = mx_master_3s_index();
    let hit = resolve_in_index(&index, &btle_3s_model(), None);
    assert_eq!(hit.map(|(depot, _)| depot), Some("mx_master_3s"));
}

#[test]
fn legacy_index_misses_btle_3s_by_pid() {
    // Before `modelIds`: only `2b043` is listed, so neither strict nor
    // suffix pid matching finds the BTLE 3S (`b034`).
    let index = legacy_mx_master_3s_index();
    assert!(resolve_in_index(&index, &btle_3s_model(), None).is_none());
}

#[test]
fn codename_bridges_btle_3s_on_legacy_index() {
    // Back-compat: on a legacy index the firmware codename still bridges
    // to the depot via displayName.
    let index = legacy_mx_master_3s_index();
    let hit = resolve_in_index(&index, &btle_3s_model(), Some("MX Master 3S"));
    assert_eq!(hit.map(|(depot, _)| depot), Some("mx_master_3s"));
}

fn bare_model() -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports::default(),
        model_ids: [0; 3],
        extended_model_id: 0,
    }
}

/// Registry `files` entries for `names`; the hashes and sizes are irrelevant to
/// resolution, which only asks which names the depot publishes.
fn registry_files(names: &[&str]) -> Vec<openlogi_assets::FileEntry> {
    names
        .iter()
        .map(|name| openlogi_assets::FileEntry {
            name: (*name).to_string(),
            sha256: String::new(),
            bytes: 0,
        })
        .collect()
}

/// A 24-byte PNG: signature + an `IHDR` chunk header carrying only the
/// width/height — all `read_image_dimensions` reads of a PNG.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes
}

/// An old-schema depot (`metadata.json` + `front.png`, no `*_core`
/// names, no manifest) must still resolve — this is what makes the
/// MX Vertical and the older mice render.
#[test]
fn resolves_old_schema_depot_on_disk() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "mx_vertical";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("metadata.json"),
        r#"{"images":[
            {"key":"device_image","origin":{"width":100,"height":200}},
            {"key":"device_buttons_image","origin":{"width":100,"height":200},
             "assignments":[{"slotName":"SLOT_NAME_MIDDLE_BUTTON",
                             "marker":{"x":50,"y":50},"label":{"x":0,"y":0}}]}
        ]}"#,
    )
    .expect("write metadata.json");
    std::fs::write(dir.join("front.png"), png_header(100, 200)).expect("write front.png");

    let resolver = resolver_over(&[root.path()], None);
    let entry = DeviceEntry {
        model_id: "eb020".to_string(),
        model_ids: Vec::new(),
        display_name: "MX Vertical".to_string(),
        kind: "MOUSE".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: Vec::new(),
    };

    let asset = resolver
        .load_files(depot, &entry, bare_model().extended_model_id)
        .expect("old-schema depot should resolve");
    assert_eq!(
        asset.image_path.file_name().expect("image has a file name"),
        "front.png"
    );
    assert_eq!((asset.png_width, asset.png_height), (100, 200));
    assert_eq!(asset.metadata.assignments().count(), 1);
}

/// A depot whose variants are handed rather than coloured ships none of
/// [`METADATA_FILES`] — the Lift keys its metadata `core_metadata_left`
/// / `core_metadata_right` and names the right one in the manifest's
/// `image_metadata`. Resolving by well-known name alone skipped the
/// depot outright and rendered the generic silhouette.
#[test]
fn resolves_depot_whose_metadata_is_only_named_by_the_manifest() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "mx_vertical_mini";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"devices":[{"modelId":"b031_ext4","resources":[
            {"key":"image_metadata","src":"core_metadata_right.json"},
            {"key":"device_image","src":"front_ext_2.png"}
        ]}]}"#,
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("core_metadata_right.json"),
        r#"{"images":[
            {"key":"device_buttons_image","origin":{"width":860,"height":1256},
             "assignments":[{"slotName":"SLOT_NAME_MIDDLE_BUTTON",
                             "marker":{"x":50,"y":50},"label":{"x":0,"y":0}}]}
        ]}"#,
    )
    .expect("write variant metadata");
    std::fs::write(dir.join("front_ext_2.png"), png_header(860, 1256))
        .expect("write variant render");

    let resolver = resolver_over(&[root.path()], None);
    let entry = DeviceEntry {
        model_id: "b031".to_string(),
        model_ids: Vec::new(),
        display_name: "Lift".to_string(),
        kind: "MOUSE".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: Vec::new(),
    };
    let model = DeviceModelInfo {
        extended_model_id: 4,
        ..bare_model()
    };

    let asset = resolver
        .load_files(depot, &entry, model.extended_model_id)
        .expect("manifest-named metadata should resolve the depot");
    assert_eq!(
        asset.image_path.file_name().expect("image has a file name"),
        "front_ext_2.png"
    );
    assert_eq!(asset.metadata.assignments().count(), 1);
}

#[test]
fn resolves_left_handed_lift_for_business_b033_ext6() {
    // #1170: BTLE reports b033/ext=06, but the index keys this depot on 2b033.
    // Both hands exist on disk; resolving the right-hand metadata is not enough.
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "mx_vertical_mini_for_business";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"devices":[
            {"modelId":"2b033","resources":[
                {"key":"image_metadata","src":"core_metadata_right.json"},
                {"key":"device_image","src":"front_ext_5.png"},
                {"key":"device_buttons_image","src":"front_ext_5.png"}]},
            {"modelId":"2b033_ext5","resources":[
                {"key":"image_metadata","src":"core_metadata_right.json"},
                {"key":"device_image","src":"front_ext_5.png"},
                {"key":"device_buttons_image","src":"front_ext_5.png"}]},
            {"modelId":"2b033_ext6","resources":[
                {"key":"image_metadata","src":"core_metadata_left.json"},
                {"key":"device_image","src":"front_ext_6.png"},
                {"key":"device_buttons_image","src":"front_ext_6.png"}]}
        ]}"#,
    )
    .expect("write manifest");
    for (hand, marker_x) in [("right", 24), ("left", 76)] {
        std::fs::write(
            dir.join(format!("core_metadata_{hand}.json")),
            format!(
                r#"{{"images":[
                    {{"key":"device_buttons_image","origin":{{"width":860,"height":1256}},
                     "assignments":[{{"slotName":"SLOT_NAME_BACK_BUTTON",
                                     "marker":{{"x":{marker_x},"y":31}}}}]}}
                ]}}"#,
            ),
        )
        .expect("write handed metadata");
    }
    for name in ["front_ext_5.png", "front_ext_6.png"] {
        std::fs::write(dir.join(name), png_header(860, 1256)).expect("write render");
    }
    let entry = DeviceEntry {
        model_id: "2b033".into(),
        model_ids: vec!["2b033".into()],
        display_name: "Lift for Business".into(),
        kind: "MOUSE".into(),
        asset_path: format!("v1/devices/{depot}/"),
        files: Vec::new(),
    };
    let resolver = resolver_over(&[root.path()], Some(index_of(depot, entry)));
    let model = DeviceModelInfo {
        transports: DeviceTransports {
            btle: true,
            ..Default::default()
        },
        model_ids: [0xb033, 0, 0],
        extended_model_id: 0x06,
        ..bare_model()
    };

    let asset = resolver
        .resolve(&model, None)
        .expect("left-handed Lift for Business should resolve without generic metadata");
    assert_eq!(asset.depot, depot);
    assert_eq!(asset.image_path, dir.join("front_ext_6.png"));
    assert_eq!(asset.hero_image_path, Some(dir.join("front_ext_6.png")));
    assert_eq!((asset.png_width, asset.png_height), (860, 1256));
    let mut assignments = asset.metadata.assignments();
    let back = assignments.next().expect("left-hand back button hotspot");
    assert_eq!(back.slot_name, "SLOT_NAME_BACK_BUTTON");
    assert_eq!(
        back.marker,
        openlogi_assets::metadata::Point { x: 76.0, y: 31.0 }
    );
    assert!(assignments.next().is_none());
}

#[test]
fn resolves_standalone_registry_model_without_synthetic_hidpp_info() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = root.path().join("litra_glow");
    std::fs::create_dir_all(&depot).expect("create depot dir");
    std::fs::write(
        depot.join("manifest.json"),
        r#"{"devices":[{"modelId":"8c900","resources":[{"key":"device_image","src":"front.png"}]}],"resources":[]}"#,
    )
    .expect("write manifest");
    std::fs::write(depot.join("front.png"), png_header(396, 396)).expect("write front");

    let index = index_of(
        "litra_glow",
        DeviceEntry {
            model_id: "8c900".into(),
            model_ids: vec![],
            display_name: "Litra Glow".into(),
            kind: "ILLUMINATION_LIGHT".into(),
            asset_path: "v1/devices/litra_glow/".into(),
            files: vec![],
        },
    );
    let resolver = resolver_over(&[root.path()], Some(index));

    let asset = resolver
        .resolve_registry_model("8c900")
        .expect("standalone registry model should resolve");
    assert_eq!(asset.display_name, "Litra Glow");
    assert_eq!(asset.kind, Some(DeviceKind::Light));
    assert_eq!(asset.image_path, depot.join("front.png"));
    assert_eq!((asset.png_width, asset.png_height), (396, 396));
}

#[test]
fn standalone_registry_lookup_does_not_cross_model_depots() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = root.path().join("litra_beam");
    std::fs::create_dir_all(&depot).expect("create depot dir");
    std::fs::write(
        depot.join("manifest.json"),
        r#"{"devices":[{"modelId":"8c901","resources":[{"key":"device_image","src":"front.png"}]}],"resources":[]}"#,
    )
    .expect("write manifest");
    std::fs::write(depot.join("front.png"), png_header(120, 240)).expect("write front");
    let index = Index {
        schema_version: 1,
        devices: HashMap::from([
            (
                "litra_glow".into(),
                DeviceEntry {
                    model_id: "8c900".into(),
                    model_ids: vec![],
                    display_name: "Litra Glow".into(),
                    kind: "ILLUMINATION_LIGHT".into(),
                    asset_path: "v1/devices/litra_glow/".into(),
                    files: vec![],
                },
            ),
            (
                "litra_beam".into(),
                DeviceEntry {
                    model_id: "8c901".into(),
                    model_ids: vec![],
                    display_name: "Litra Beam".into(),
                    kind: "ILLUMINATION_LIGHT".into(),
                    asset_path: "v1/devices/litra_beam/".into(),
                    files: vec![],
                },
            ),
        ]),
    };
    let resolver = resolver_over(&[root.path()], Some(index));

    assert!(resolver.resolve_registry_model("8c900").is_none());
    assert_eq!(
        resolver
            .resolve_registry_model("8c901")
            .expect("beam should resolve")
            .display_name,
        "Litra Beam"
    );
}

#[test]
fn resolves_depot_with_named_manifest_and_non_standard_render() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "pro_keyboard_ext1";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"devices":[
            {"modelId":"pro_keyboard_ext1","resources":[{"key":"device_image","src":"front_mx.png"}]},
            {"modelId":"pro_keyboard_ext7","resources":[{"key":"device_image","src":"front_kda.png"}]}
        ],"resources":[]}"#,
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("metadata.json"),
        r#"{"images":[{"key":"device_image","origin":{"width":500,"height":300}}]}"#,
    )
    .expect("write metadata");
    std::fs::write(dir.join("front_mx.png"), png_header(500, 300)).expect("write front_mx.png");
    std::fs::write(dir.join("front_kda.png"), png_header(500, 300)).expect("write front_kda.png");

    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: None,
        resolved: RefCell::default(),
    };
    let entry = DeviceEntry {
        model_id: "c339".to_string(),
        model_ids: vec!["c339".to_string()],
        display_name: "PRO".to_string(),
        kind: "KEYBOARD".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: Vec::new(),
    };

    // Base model (ext == 0) resolves front_mx.png via pro_keyboard_ext1
    let asset = resolver
        .load_files(depot, &entry, bare_model().extended_model_id)
        .expect("pro keyboard base variant should resolve");
    assert_eq!(
        asset.image_path.file_name().expect("filename"),
        "front_mx.png"
    );

    // KDA variant (ext == 7) resolves front_kda.png via pro_keyboard stem + _ext7
    let mut kda_model = bare_model();
    kda_model.extended_model_id = 7;
    let kda_asset = resolver
        .load_files(depot, &entry, kda_model.extended_model_id)
        .expect("pro keyboard KDA variant should resolve");
    assert_eq!(
        kda_asset.image_path.file_name().expect("filename"),
        "front_kda.png"
    );
}

#[test]
fn unsafe_standalone_manifest_filename_is_rejected() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = root.path().join("litra_glow");
    std::fs::create_dir_all(&depot).expect("create depot dir");
    std::fs::write(
        depot.join("manifest.json"),
        r#"{"devices":[{"modelId":"8c900","resources":[{"key":"device_image","src":"../front.png"}]}],"resources":[]}"#,
    )
    .expect("write manifest");
    std::fs::write(root.path().join("front.png"), png_header(1, 1)).expect("write escape");
    let resolver = resolver_over(
        &[root.path()],
        Some(index_of(
            "litra_glow",
            DeviceEntry {
                model_id: "8c900".into(),
                model_ids: vec![],
                display_name: "Litra Glow".into(),
                kind: "ILLUMINATION_LIGHT".into(),
                asset_path: "v1/devices/litra_glow/".into(),
                files: vec![openlogi_assets::FileEntry {
                    name: "front.png".into(),
                    sha256: String::new(),
                    bytes: 0,
                }],
            },
        )),
    );
    assert!(resolver.resolve_registry_model("8c900").is_none());
}

#[test]
fn standalone_resolution_prefers_the_first_read_root() {
    let roots = [
        tempfile::tempdir().expect("create bundle root"),
        tempfile::tempdir().expect("create cache root"),
    ];
    for (root, dimensions) in roots.iter().zip([(10, 10), (20, 20)]) {
        let depot = root.path().join("litra_glow");
        std::fs::create_dir_all(&depot).expect("create depot dir");
        std::fs::write(
            depot.join("front.png"),
            png_header(dimensions.0, dimensions.1),
        )
        .expect("write front");
    }
    let resolver = resolver_over(
        &[roots[0].path(), roots[1].path()],
        Some(index_of(
            "litra_glow",
            DeviceEntry {
                model_id: "8c900".into(),
                model_ids: vec![],
                display_name: "Litra Glow".into(),
                kind: "ILLUMINATION_LIGHT".into(),
                asset_path: "v1/devices/litra_glow/".into(),
                files: vec![openlogi_assets::FileEntry {
                    name: "front.png".into(),
                    sha256: String::new(),
                    bytes: 0,
                }],
            },
        )),
    );

    let asset = resolver
        .resolve_registry_model("8c900")
        .expect("bundle asset should resolve");
    assert_eq!((asset.png_width, asset.png_height), (10, 10));
    assert_eq!(
        asset.image_path,
        roots[0].path().join("litra_glow/front.png")
    );
}

/// A Signature M650 (plain) model, matching the config.toml quoted in
/// issue #1332: `model_ids = [0xb02a, 0, 0]`, `extended_model_id = 8`.
fn m650_plain_model() -> DeviceModelInfo {
    DeviceModelInfo {
        entity_count: 0,
        serial_number: None,
        unit_id: [0; 4],
        transports: DeviceTransports {
            btle: true,
            ..Default::default()
        },
        model_ids: [0xb02a, 0, 0],
        extended_model_id: 8,
    }
}

/// The catalog's single entry for `2b02a`: the Signature M650 *L* depot,
/// whose one `displayName` covers every extended-model-id variant.
fn m650_l_depot_entry() -> DeviceEntry {
    DeviceEntry {
        model_id: "2b02a".to_string(),
        model_ids: Vec::new(),
        display_name: "Signature M650 L".to_string(),
        kind: "mouse".to_string(),
        asset_path: "assets/signature_m650/".to_string(),
        files: Vec::new(),
    }
}

fn m650_index() -> Index {
    index_of("signature_m650", m650_l_depot_entry())
}

#[test]
fn cached_m650_assets_keep_each_devices_firmware_name() {
    let root = tempfile::tempdir().expect("create asset root");
    let depot = root.path().join("signature_m650");
    std::fs::create_dir_all(&depot).unwrap();
    std::fs::write(
        depot.join("metadata.json"),
        r#"{"images":[{"key":"device_image","origin":{"width":100,"height":200}}]}"#,
    )
    .unwrap();
    std::fs::write(depot.join("front.png"), png_header(100, 200)).unwrap();
    let resolver = resolver_over(&[root.path()], Some(m650_index()));
    let model = m650_plain_model();

    // Resolve without a firmware name first, then reuse the same cached
    // artwork for both names. Neither a cache hit nor a previous device may
    // decide the next device's display name.
    for (codename, expected) in [
        (None, "Signature M650 L"),
        (Some("Signature M650 Mouse"), "Signature M650"),
        (Some("Signature M650 L"), "Signature M650 L"),
        (Some("Signature M650 Mouse"), "Signature M650"),
    ] {
        let asset = resolver.resolve(&model, codename).expect("resolve M650");
        assert_eq!(asset.display_name, expected);
        assert_eq!(asset.image_path, depot.join("front.png"));
    }
    assert_eq!(resolver.resolved.borrow().len(), 1, "artwork stays shared");
}

/// A wired G512 as issue #2402's live inventory reports it:
/// `model_ids=[c33c,0000,0000] ext=06 transports=usb`.
fn g512_model() -> DeviceModelInfo {
    DeviceModelInfo {
        transports: DeviceTransports {
            usb: true,
            ..Default::default()
        },
        model_ids: [0xc33c, 0, 0],
        extended_model_id: 6,
        ..bare_model()
    }
}

/// The catalog's single entry for `c33c`: the `g513` depot, whose
/// `displayName` is `G513` for every variant, the G512 ones included.
fn g513_index() -> Index {
    index_of(
        "g513",
        DeviceEntry {
            model_id: "c33c".to_string(),
            model_ids: vec!["c33c".to_string()],
            display_name: "G513".to_string(),
            kind: "KEYBOARD".to_string(),
            asset_path: "v1/devices/g513/".to_string(),
            files: Vec::new(),
        },
    )
}

#[test]
fn g512_in_the_shared_g513_depot_shows_its_own_name() {
    let root = tempfile::tempdir().expect("create asset root");
    let depot = root.path().join("g513");
    std::fs::create_dir_all(&depot).unwrap();
    std::fs::write(depot.join("front.png"), png_header(100, 200)).unwrap();
    std::fs::write(depot.join("front_ext6.png"), png_header(300, 100)).unwrap();
    std::fs::write(
        depot.join("manifest.json"),
        r#"{"devices":[{"modelId":"g512_ext6","resources":[{"key":"device_image","src":"front_ext6.png"},{"key":"image_metadata","src":"metadata_g512.json"}]}]}"#,
    )
    .unwrap();
    std::fs::write(
        depot.join("metadata_g512.json"),
        r#"{"images":[{"key":"device_image","origin":{"width":300,"height":100}}]}"#,
    )
    .unwrap();
    let resolver = resolver_over(&[root.path()], Some(g513_index()));
    let model = g512_model();

    // The same cached extended-model render serves both firmware names.
    for (codename, expected) in [
        (None, "G513"),
        (Some("G512 RGB MECHANICAL GAMING KEYBOARD"), "G512"),
        (Some("G513 RGB MECHANICAL GAMING KEYBOARD"), "G513"),
        (Some("G512 RGB MECHANICAL GAMING KEYBOARD"), "G512"),
    ] {
        let asset = resolver.resolve(&model, codename).expect("resolve c33c");
        assert_eq!(asset.display_name, expected, "codename={codename:?}");
        assert_eq!(asset.depot, "g513");
        assert_eq!(asset.image_path, depot.join("front_ext6.png"));
        assert_eq!(asset.hero_image_path, Some(depot.join("front_ext6.png")));
        assert_eq!((asset.png_width, asset.png_height), (300, 100));
        assert_eq!(asset.metadata.origin().expect("G512 metadata").width, 300);
    }
    assert_eq!(resolver.resolved.borrow().len(), 1, "artwork stays shared");
}

#[test]
fn variant_display_name_preserves_matching_and_whitespace_rules() {
    // None means leave the catalog name untouched, including its whitespace.
    for (catalog, codename, correction) in [
        (
            "Signature M650 L",
            Some("Signature M650 Mouse"),
            Some("Signature M650"),
        ),
        ("Signature M650 L", Some("Signature M650 L"), None),
        ("Signature M650 L", None, None),
        ("MX Master 3S", Some("M3S"), None),
        ("MX Master 3S", Some("MX Master"), None),
        ("MX Master X", Some("MX Master"), None),
        (
            "Signature M650 L LEFT",
            Some("signature m650"),
            Some("Signature M650"),
        ),
        ("Signature M650 L Pro", Some("Signature M650"), None),
        ("Signature M650", Some("Signature M650 L"), None),
        ("Other M650 L", Some("Signature M650"), None),
        ("Signature M650 L", Some(" \t"), None),
        ("Signature M650 L", Some("Mouse Keyboard Trackball"), None),
        ("", Some("Signature M650"), None),
        (
            " \tSignature\u{2003}M650  l \n",
            Some("signature\tM650 mouse"),
            Some("Signature M650"),
        ),
        (" MX\tMaster 3S ", Some("MX Master"), None),
        ("Élan L", Some("Élan Mouse"), Some("Élan")),
        // Characterize existing behavior, not a new tail-only removal policy.
        (
            "Signature M650 L",
            Some("Signature Mouse M650"),
            Some("Signature M650"),
        ),
        (
            "Signature M650 L",
            Some("Signature M650 \u{212a}eyboard"),
            Some("Signature M650"),
        ),
        // A sibling product sharing the depot (#2402), matched on the
        // firmware's leading model word and only within its own depot.
        (
            "G513",
            Some("G512 RGB MECHANICAL GAMING KEYBOARD"),
            Some("G512"),
        ),
        (" g513 ", Some("g512 rgb mechanical gaming"), Some("G512")),
        ("G513", Some("Keyboard G512"), Some("G512")),
        ("G513", Some("G513 RGB MECHANICAL GAMING KEYBOARD"), None),
        ("G513", Some("G512X RGB"), None),
        ("G513", Some("RGB G512"), None),
        ("G513", Some("Keyboard"), None),
        ("G513", None, None),
        ("G915", Some("G512 RGB MECHANICAL GAMING KEYBOARD"), None),
        ("G512", Some("G512 RGB MECHANICAL GAMING KEYBOARD"), None),
    ] {
        assert_eq!(
            variant_display_name_override(catalog, codename).as_deref(),
            correction,
            "catalog={catalog:?}, codename={codename:?}"
        );
    }
}

#[test]
fn cleanup_removes_only_legacy_glow_pngs() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = root.path().join("g513");
    std::fs::create_dir_all(&depot).expect("create depot dir");
    std::fs::write(depot.join("glow-ff9500.png"), b"x").expect("write glow png");
    std::fs::write(depot.join("glow-af52de.png.tmp"), b"x").expect("write glow tmp");
    std::fs::write(depot.join("front.png"), b"x").expect("write front render");
    std::fs::write(depot.join("metadata.json"), b"{}").expect("write metadata");

    cleanup_glow_pngs_in(root.path());

    assert!(
        !depot.join("glow-ff9500.png").exists() && !depot.join("glow-af52de.png.tmp").exists(),
        "legacy glow files must be deleted"
    );
    assert!(
        depot.join("front.png").exists() && depot.join("metadata.json").exists(),
        "real assets must be left untouched"
    );
}

/// The depot [`mx_master_3s_index`] maps the MX Master 3S to, with one render
/// of its own width per colour variant. Returns the depot directory.
fn write_variant_depot(root: &Path, variants: &[(u8, u32)]) -> PathBuf {
    let depot = "mx_master_3s";
    let dir = root.join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    let devices: Vec<String> = variants
        .iter()
        .map(|(ext, _)| {
            format!(
                r#"{{"modelId":"2b034_ext{ext}","resources":[
                    {{"key":"device_buttons_image","src":"side_ext_{ext}.png"}}]}}"#
            )
        })
        .collect();
    std::fs::write(
        dir.join("manifest.json"),
        format!(r#"{{"devices":[{}]}}"#, devices.join(",")),
    )
    .expect("write manifest");
    std::fs::write(dir.join("core_metadata.json"), r#"{"images":[]}"#).expect("write metadata");
    for (ext, width) in variants {
        std::fs::write(
            dir.join(format!("side_ext_{ext}.png")),
            png_header(*width, 10),
        )
        .expect("write render");
    }
    dir
}

fn mx_master_3s(extended_model_id: u8) -> DeviceModelInfo {
    DeviceModelInfo {
        model_ids: [0xb034, 0, 0],
        extended_model_id,
        ..bare_model()
    }
}

/// The device list is rebuilt on every agent snapshot, so a resolver must not
/// go back to disk for an asset it has already found. Deleting the depot
/// between two resolves is the proof: the second answer can only come from
/// memory. A new resolver is the one way to see the change, which is what the
/// runtime builds when a download lands or the cache is cleared.
#[test]
fn a_found_asset_is_answered_from_memory_until_the_resolver_is_replaced() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_variant_depot(root.path(), &[(2, 100)]);
    let resolver = resolver_over(&[root.path()], Some(mx_master_3s_index()));

    let found = resolver
        .resolve(&mx_master_3s(2), None)
        .expect("the depot is on disk");
    std::fs::remove_dir_all(&dir).expect("remove the depot");

    assert_eq!(resolver.resolve(&mx_master_3s(2), None), Some(found));
    assert_eq!(
        resolver_over(&[root.path()], Some(mx_master_3s_index())).resolve(&mx_master_3s(2), None),
        None,
        "a replacement resolver reads the disk again"
    );
}

#[test]
fn a_found_standalone_asset_is_answered_from_memory_too() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = root.path().join("litra_glow");
    std::fs::create_dir_all(&depot).expect("create depot dir");
    std::fs::write(depot.join("front.png"), png_header(396, 396)).expect("write front");
    let index = index_of(
        "litra_glow",
        DeviceEntry {
            model_id: "8c900".into(),
            model_ids: vec![],
            display_name: "Litra Glow".into(),
            kind: "ILLUMINATION_LIGHT".into(),
            asset_path: "v1/devices/litra_glow/".into(),
            files: vec![openlogi_assets::FileEntry {
                name: "front.png".into(),
                sha256: String::new(),
                bytes: 0,
            }],
        },
    );
    let resolver = resolver_over(&[root.path()], Some(index));

    let found = resolver
        .resolve_registry_model("8c900")
        .expect("the depot is on disk");
    std::fs::remove_dir_all(&depot).expect("remove the depot");

    assert_eq!(resolver.resolve_registry_model("8c900"), Some(found));
}

/// Only finds are remembered. A depot that is not on disk yet is downloaded in
/// the background, and partial writes can land before the download that
/// replaces the resolver settles; the next resolve has to see them.
#[test]
fn a_depot_that_lands_after_a_miss_is_found_by_the_same_resolver() {
    let root = tempfile::tempdir().expect("create temp dir");
    let resolver = resolver_over(&[root.path()], Some(mx_master_3s_index()));
    assert_eq!(resolver.resolve(&mx_master_3s(2), None), None);

    write_variant_depot(root.path(), &[(2, 100)]);

    assert!(resolver.resolve(&mx_master_3s(2), None).is_some());
}

/// Two colours of one model share a depot and nothing else: the second must
/// not be handed the render remembered for the first.
#[test]
fn colour_variants_of_one_depot_are_remembered_apart() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_variant_depot(root.path(), &[(2, 100), (12, 200)]);
    let resolver = resolver_over(&[root.path()], Some(mx_master_3s_index()));

    let graphite = resolver
        .resolve(&mx_master_3s(2), None)
        .expect("graphite resolves");
    let pale_grey = resolver
        .resolve(&mx_master_3s(12), None)
        .expect("pale grey resolves");

    assert_eq!(graphite.image_path, dir.join("side_ext_2.png"));
    assert_eq!(pale_grey.image_path, dir.join("side_ext_12.png"));
    assert_eq!((graphite.png_width, pale_grey.png_width), (100, 200));
}

/// A camera depot (the C922 / StreamCam family) ships `front.png` and a
/// manifest but none of the hotspot metadata files — its `image_metadata` is a
/// per-PID `metadata_<pid>.json` nobody reads. The render alone must resolve,
/// or every webcam falls back to the gallery glyph.
#[test]
fn resolves_camera_depot_without_hotspot_metadata() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "c922";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"devices":[{"modelId":"085c","resources":[
            {"key":"device_camera_image","src":"front.png"},
            {"key":"image_metadata","src":"metadata_085c.json"}]}],
          "resources":[]}"#,
    )
    .expect("write manifest.json");
    std::fs::write(dir.join("front.png"), png_header(300, 150)).expect("write front.png");

    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: None,
        resolved: RefCell::default(),
    };
    let entry = DeviceEntry {
        model_id: "085c".to_string(),
        model_ids: vec!["0883".to_string(), "0894".to_string(), "085c".to_string()],
        display_name: "C922".to_string(),
        kind: "CAMERA".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: registry_files(&["front.png", "manifest.json", "metadata_085c.json"]),
    };
    let mut model = bare_model();
    model.model_ids = [0x085c, 0, 0];

    let asset = resolver
        .load_files(depot, &entry, model.extended_model_id)
        .expect("render-only camera depot should resolve");
    assert_eq!(
        asset.image_path.file_name().expect("image has a file name"),
        "front.png"
    );
    assert_eq!(
        asset.hero_image_path.as_deref(),
        Some(asset.image_path.as_path())
    );
    assert_eq!((asset.png_width, asset.png_height), (300, 150));
    assert_eq!(asset.kind, Some(DeviceKind::Camera));
    assert_eq!(asset.metadata.assignments().count(), 0);
}

/// A depot that *publishes* hotspot metadata (every mouse and keyboard) but has
/// none in this root is a stale or half-synced cache, not a metadata-free
/// device. It must keep missing so the next root or the synthetic fallback
/// serves the device instead of a render with no hotspots.
#[test]
fn metadata_publishing_depot_without_cached_metadata_still_misses() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "mx_master_4";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(dir.join("front_core.png"), png_header(100, 200)).expect("write render");
    std::fs::write(dir.join("side_core.png"), png_header(100, 200)).expect("write render");

    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: None,
        resolved: RefCell::default(),
    };
    let entry = DeviceEntry {
        model_id: "2b042".to_string(),
        model_ids: Vec::new(),
        display_name: "MX Master 4".to_string(),
        kind: "MOUSE".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: registry_files(&[
            "core_metadata.json",
            "front_core.png",
            "manifest.json",
            "side_core.png",
        ]),
    };
    assert!(
        resolver
            .load_files(depot, &entry, bare_model().extended_model_id)
            .is_none()
    );
}

/// An unsynced depot directory — no render at all — must still miss, so the
/// metadata relaxation above doesn't turn an empty cache into a broken image.
#[test]
fn depot_without_any_render_still_misses() {
    let root = tempfile::tempdir().expect("create temp dir");
    let depot = "c922";
    let dir = root.path().join(depot);
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(dir.join("manifest.json"), b"{}").expect("write manifest.json");

    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: None,
        resolved: RefCell::default(),
    };
    let entry = DeviceEntry {
        model_id: "085c".to_string(),
        model_ids: Vec::new(),
        display_name: "C922".to_string(),
        kind: "CAMERA".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: Vec::new(),
    };
    assert!(
        resolver
            .load_files(depot, &entry, bare_model().extended_model_id)
            .is_none()
    );
}

/// The head of a G HUB encrypted asset: a magic, the header length, then a
/// JSON header naming the key. No image decoder reads past it.
fn ghub_encrypted_blob() -> Vec<u8> {
    let mut bytes = b"!\x05! \x82\0\0\0".to_vec();
    bytes.extend_from_slice(br#"{"key-id": "ad3e4085-55be-40ae-8b4a-afeedb479a3d"}"#);
    bytes.extend_from_slice(&[0x34, 0x4d, 0x6d, 0x00, 0x60, 0x28]);
    bytes
}

#[test]
fn renders_are_recognised_by_their_bytes() {
    let root = tempfile::tempdir().expect("create temp dir");
    let write = |name: &str, bytes: &[u8]| {
        let path = root.path().join(name);
        std::fs::write(&path, bytes).expect("write file");
        path
    };
    let png = write("45a9d07d-91e8-47d6-92b3-f3827d951246", &png_header(10, 10));
    let webp = write("front_mihoyo.webp", b"RIFF\x10\0\0\0WEBPVP8 ");
    let blob = write(
        "58ee56b0-5ca6-4e7b-b764-2d1746094b78",
        &ghub_encrypted_blob(),
    );
    assert!(images::is_drawable_image(&png));
    assert!(images::is_drawable_image(&webp));
    assert!(!images::is_drawable_image(&blob));
    assert!(!images::is_drawable_image(&write("empty.png", b"")));
    assert!(!images::is_drawable_image(&root.path().join("missing.png")));
}

/// The head of a WebP file: the `RIFF` header, then the first chunk's `tag`
/// and the start of its `data`. The reader skips both lengths, so they stay
/// zero.
fn webp_header(tag: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
    bytes.extend_from_slice(&tag);
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(data);
    bytes
}

/// An extended-format WebP head, the shape of the catalog's WebP renders: the
/// alpha flag and reserved bits, then the canvas width and height minus one,
/// each 24-bit little-endian.
fn webp_vp8x_header(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0x10, 0, 0, 0];
    data.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    data.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    webp_header(*b"VP8X", &data)
}

/// #2460: the size behind each render's hotspots, read from the header of
/// either format the catalog ships.
#[test]
fn render_sizes_are_read_from_png_and_webp_headers() {
    let root = tempfile::tempdir().expect("create temp dir");
    let size_of = |bytes: &[u8]| {
        let path = root.path().join("1b909bb1-911c-42b7-bf07-b60be1655de5");
        std::fs::write(&path, bytes).expect("write render");
        images::read_image_dimensions(&path).ok()
    };
    // Lossy: a key-frame tag and the start code, then 640 with the scale bits
    // set, and 480.
    let lossy = [0x30, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x80, 0x42, 0xe0, 0x01];
    // Lossless: the signature, then 300 and 200 minus one with the alpha bit.
    let mut lossless = vec![0x2f];
    lossless.extend_from_slice(&((300 - 1) | ((200 - 1) << 14) | (1_u32 << 28)).to_le_bytes());

    assert_eq!(size_of(&png_header(1391, 2700)), Some((1391, 2700)));
    assert_eq!(
        size_of(&webp_vp8x_header(1157, 2800)),
        Some((1157, 2800)),
        "the G502 X PLUS ext2 side render"
    );
    assert_eq!(size_of(&webp_header(*b"VP8 ", &lossy)), Some((640, 480)));
    assert_eq!(size_of(&webp_header(*b"VP8L", &lossless)), Some((300, 200)));
}

#[test]
fn a_render_without_a_readable_size_has_none() {
    let root = tempfile::tempdir().expect("create temp dir");
    let size_of = |bytes: &[u8]| {
        let path = root.path().join("render");
        std::fs::write(&path, bytes).expect("write render");
        images::read_image_dimensions(&path).ok()
    };
    let no_start_code = [0x30, 0x01, 0x00, 0, 0, 0, 0x80, 0x02, 0xe0, 0x01];

    assert_eq!(
        size_of(b"RIFF\x10\0\0\0WEBPVP8 "),
        None,
        "a WebP head cut short"
    );
    assert_eq!(size_of(&webp_vp8x_header(1157, 2800)[..28]), None);
    assert_eq!(size_of(&png_header(1391, 2700)[..20]), None);
    assert_eq!(size_of(&webp_header(*b"VP8 ", &no_start_code)), None);
    assert_eq!(
        size_of(&webp_header(*b"VP8L", &[0; 5])),
        None,
        "no lossless signature"
    );
    assert_eq!(size_of(&webp_header(*b"ANIM", &[0; 10])), None);
    assert_eq!(size_of(&ghub_encrypted_blob()), None);
    assert_eq!(size_of(b""), None);
    assert_eq!(
        images::read_image_dimensions(&root.path().join("missing")).ok(),
        None
    );
}

/// The `g502_wireless` depot as the catalog serves it: the black base model
/// in PNG, the purple `ext2` variant as `variant_render` with metadata
/// calibrated for it, everything keyed on the depot name.
fn write_g502_lightspeed_depot(root: &Path, variant_render: &[u8]) -> PathBuf {
    let dir = root.join("g502_wireless");
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"devices":[
            {"modelId":"g502_wireless","resources":[
                {"key":"device_image","src":"front.png"},
                {"key":"device_side","src":"side.png"},
                {"key":"image_metadata","src":"metadata.json"}]},
            {"modelId":"g502_wireless_ext2","resources":[
                {"key":"device_image","src":"58ee56b0-5ca6-4e7b-b764-2d1746094b78"},
                {"key":"device_side","src":"642c5810-f750-48db-99b1-9dc46f750908"},
                {"key":"image_metadata","src":"metadata_ext2.json"}]}
        ]}"#,
    )
    .expect("write manifest");
    for (name, g1_x) in [("metadata.json", 538), ("metadata_ext2.json", 535)] {
        std::fs::write(
            dir.join(name),
            format!(
                r#"{{"images":[
                    {{"key":"device_image","origin":{{"width":1391,"height":2700}},
                     "assignments":[{{"slotId":"g502wireless_g1_m1",
                                     "marker":{{"x":{g1_x},"y":614}}}}]}},
                    {{"key":"device_side","origin":{{"width":936,"height":2700}},
                     "assignments":[{{"slotId":"g502wireless_g4_m1",
                                     "marker":{{"x":580,"y":1800}}}}]}}
                ]}}"#,
            ),
        )
        .expect("write metadata");
    }
    std::fs::write(dir.join("front.png"), png_header(1391, 2700)).expect("write front");
    std::fs::write(dir.join("side.png"), png_header(936, 2700)).expect("write side");
    for name in [
        "58ee56b0-5ca6-4e7b-b764-2d1746094b78",
        "642c5810-f750-48db-99b1-9dc46f750908",
    ] {
        std::fs::write(dir.join(name), variant_render).expect("write variant render");
    }
    dir
}

fn g502_lightspeed_index() -> Index {
    index_of(
        "g502_wireless",
        DeviceEntry {
            model_id: "407f".into(),
            model_ids: vec!["407f".into()],
            display_name: "G502 Lightspeed".into(),
            kind: "MOUSE".into(),
            asset_path: "v1/devices/g502_wireless/".into(),
            files: Vec::new(),
        },
    )
}

fn g502_lightspeed(extended_model_id: u8) -> DeviceModelInfo {
    DeviceModelInfo {
        model_ids: [0x407f, 0xc08d, 0x0000],
        extended_model_id,
        ..bare_model()
    }
}

fn first_front_marker_x(asset: &ResolvedAsset) -> Option<f32> {
    let front = asset.metadata.image("device_image")?;
    front.assignments.first().map(|a| a.marker.x)
}

/// #2375: the purple G502 LIGHTSPEED's renders are encrypted. It drew the blob
/// as its main view, which renders as nothing, beside the black side view and
/// under hotspots calibrated for the purple renders.
#[test]
fn an_encrypted_colour_variant_is_drawn_as_the_base_model() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_g502_lightspeed_depot(root.path(), &ghub_encrypted_blob());
    let resolver = resolver_over(&[root.path()], Some(g502_lightspeed_index()));

    let asset = resolver
        .resolve(&g502_lightspeed(2), None)
        .expect("the purple G502 LIGHTSPEED resolves");
    assert_eq!(asset.image_path, dir.join("front.png"));
    assert_eq!(asset.hero_image_path, Some(dir.join("front.png")));
    assert_eq!((asset.png_width, asset.png_height), (1391, 2700));
    let side = asset.side_view.as_ref().expect("the side view");
    assert_eq!(side.image_path, dir.join("side.png"));
    assert_eq!(
        first_front_marker_x(&asset),
        Some(538.0),
        "the base model's hotspots"
    );
}

#[test]
fn a_drawable_colour_variant_keeps_its_renders_and_metadata() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_g502_lightspeed_depot(root.path(), &png_header(1391, 2700));
    let resolver = resolver_over(&[root.path()], Some(g502_lightspeed_index()));

    let asset = resolver
        .resolve(&g502_lightspeed(2), None)
        .expect("the purple G502 LIGHTSPEED resolves");
    assert_eq!(
        asset.image_path,
        dir.join("58ee56b0-5ca6-4e7b-b764-2d1746094b78")
    );
    let side = asset.side_view.as_ref().expect("the side view");
    assert_eq!(
        side.image_path,
        dir.join("642c5810-f750-48db-99b1-9dc46f750908"),
        "the variant's own side render, not the base model's"
    );
    assert_eq!(first_front_marker_x(&asset), Some(535.0));
}

const G502X_PLUS_EXT2_FRONT: &str = "3253a30b-28e6-4704-85ea-a14a2e5b5df6";
const G502X_PLUS_EXT2_SIDE: &str = "1b909bb1-911c-42b7-bf07-b60be1655de5";

/// A depot whose colour variant ships its renders as WebP and no `side.png`,
/// sized like the G502 X PLUS `ext2` (#2460). Its metadata origins are smaller
/// than the renders, as in the catalog. The catalog keys the variant on the
/// depot name under `images/device/`; this one keys it on the registry model
/// id with flat names, so only the size check decides the side view.
fn write_webp_variant_depot(root: &Path) -> PathBuf {
    let dir = root.join("g502x_plus");
    std::fs::create_dir_all(&dir).expect("create depot dir");
    std::fs::write(
        dir.join("manifest.json"),
        format!(
            r#"{{"devices":[
                {{"modelId":"c095_ext2","resources":[
                    {{"key":"device_image","src":"{G502X_PLUS_EXT2_FRONT}"}},
                    {{"key":"device_side","src":"{G502X_PLUS_EXT2_SIDE}"}}]}}
            ]}}"#,
        ),
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("metadata.json"),
        r#"{"images":[
            {"key":"device_image","origin":{"width":1391,"height":2700},
             "assignments":[{"slotId":"g502x_plus_g1_m1",
                             "marker":{"x":538,"y":614}}]},
            {"key":"device_side","origin":{"width":936,"height":2700},
             "assignments":[{"slotId":"g502x_plus_g4_m1",
                             "marker":{"x":580,"y":1800}}]}
        ]}"#,
    )
    .expect("write metadata");
    std::fs::write(
        dir.join(G502X_PLUS_EXT2_FRONT),
        webp_vp8x_header(1662, 2800),
    )
    .expect("write front render");
    std::fs::write(dir.join(G502X_PLUS_EXT2_SIDE), webp_vp8x_header(1157, 2800))
        .expect("write side render");
    dir
}

/// #2460: a WebP side render got no side view, and a WebP main view took the
/// metadata origin for its size, so its hotspots were scaled wrong.
#[test]
fn a_webp_colour_variant_gets_both_views_at_their_own_size() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_webp_variant_depot(root.path());
    let index = index_of(
        "g502x_plus",
        DeviceEntry {
            model_id: "c095".into(),
            model_ids: vec!["c095".into(), "4099".into()],
            display_name: "G502 X PLUS".into(),
            kind: "MOUSE".into(),
            asset_path: "v1/devices/g502x_plus/".into(),
            files: Vec::new(),
        },
    );
    let resolver = resolver_over(&[root.path()], Some(index));
    let model = DeviceModelInfo {
        model_ids: [0x4099, 0xc095, 0x0000],
        extended_model_id: 2,
        ..bare_model()
    };

    let asset = resolver
        .resolve(&model, None)
        .expect("the G502 X PLUS ext2 resolves");
    let side = asset.side_view.as_ref().expect("the WebP side view");
    assert_eq!(side.image_path, dir.join(G502X_PLUS_EXT2_SIDE));
    assert_eq!((side.png_width, side.png_height), (1157, 2800));
    assert_eq!(asset.image_path, dir.join(G502X_PLUS_EXT2_FRONT));
    assert_eq!((asset.png_width, asset.png_height), (1662, 2800));
}

const G502X_PLUS_WHITE_FRONT: &str = "images/device/e0e9dacc-b5c0-4434-8b8d-410a7aa81d2f";
const G502X_PLUS_WHITE_SIDE: &str = "images/device/22c9ac7a-43ed-48b7-8771-72ca4ae2a6bb";

/// The `g502x_plus` depot as the catalog serves it (#2457): only `front.png`
/// at the depot root, every manifest render under `images/device/`, and the
/// variants keyed on the depot name rather than a model id. The white `ext1`
/// renders are PNG, the `ext2` ones WebP.
fn write_g502x_plus_depot(root: &Path, white_side_src: &str) -> PathBuf {
    let dir = root.join("g502x_plus");
    std::fs::create_dir_all(dir.join("images").join("device")).expect("create depot dirs");
    std::fs::write(
        dir.join("manifest.json"),
        format!(
            r#"{{"devices":[
                {{"modelId":"g502x_plus","resources":[
                    {{"key":"device_image","src":"images/device/85641870-0a46-49dc-ab7a-9a152e74d77a"}},
                    {{"key":"device_side","src":"images/device/55d70c6f-0d74-4045-aed4-e450baa4eeff"}}]}},
                {{"modelId":"g502x_plus_ext1","resources":[
                    {{"key":"device_image","src":"{G502X_PLUS_WHITE_FRONT}"}},
                    {{"key":"device_side","src":"{white_side_src}"}}]}},
                {{"modelId":"g502x_plus_ext2","resources":[
                    {{"key":"device_image","src":"images/device/{G502X_PLUS_EXT2_FRONT}"}},
                    {{"key":"device_side","src":"images/device/{G502X_PLUS_EXT2_SIDE}"}}]}}
            ]}}"#
        ),
    )
    .expect("write manifest");
    std::fs::write(
        dir.join("metadata.json"),
        r#"{"images":[
            {"key":"device_image","origin":{"width":1662,"height":2800},
             "assignments":[{"slotId":"g502xplus_g1_m1","marker":{"x":540,"y":620}}]},
            {"key":"device_side","origin":{"width":1157,"height":2800},
             "assignments":[{"slotId":"g502xplus_g4_m1","marker":{"x":580,"y":1800}}]}
        ]}"#,
    )
    .expect("write metadata");
    std::fs::write(dir.join("front.png"), png_header(1662, 2800)).expect("write front");
    for (name, width) in [
        (G502X_PLUS_WHITE_FRONT, 1661),
        (G502X_PLUS_WHITE_SIDE, 1153),
    ] {
        std::fs::write(dir.join(name), png_header(width, 2800)).expect("write variant render");
    }
    let device = dir.join("images").join("device");
    for (name, width) in [(G502X_PLUS_EXT2_FRONT, 1662), (G502X_PLUS_EXT2_SIDE, 1157)] {
        std::fs::write(device.join(name), webp_vp8x_header(width, 2800))
            .expect("write variant render");
    }
    dir
}

fn g502x_plus_index() -> Index {
    index_of(
        "g502x_plus",
        DeviceEntry {
            model_id: "c095".into(),
            model_ids: vec!["c095".into(), "4099".into()],
            display_name: "G502 X PLUS".into(),
            kind: "MOUSE".into(),
            asset_path: "v1/devices/g502x_plus/".into(),
            files: Vec::new(),
        },
    )
}

fn g502x_plus(extended_model_id: u8) -> DeviceModelInfo {
    DeviceModelInfo {
        model_ids: [0x4099, 0x0000, 0x0000],
        extended_model_id,
        ..bare_model()
    }
}

/// #2457: the white G502 X PLUS drew the black `front.png` and no side view,
/// because its renders sit under `images/device/` and its variants are keyed
/// on the depot name.
#[test]
fn a_colour_variant_draws_its_renders_from_a_depot_sub_directory() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_g502x_plus_depot(root.path(), G502X_PLUS_WHITE_SIDE);
    let resolver = resolver_over(&[root.path()], Some(g502x_plus_index()));

    let asset = resolver
        .resolve(&g502x_plus(1), None)
        .expect("the white G502 X PLUS resolves");

    assert_eq!(
        asset.hero_image_path,
        Some(dir.join(G502X_PLUS_WHITE_FRONT))
    );
    assert_eq!(asset.image_path, dir.join(G502X_PLUS_WHITE_FRONT));
    let side = asset.side_view.as_ref().expect("the side view");
    assert_eq!(side.image_path, dir.join(G502X_PLUS_WHITE_SIDE));
    assert_eq!((side.png_width, side.png_height), (1153, 2800));
}

/// The catalog's `ext2` layout end to end: WebP renders (#2460) under
/// `images/device/`, keyed on the depot name.
#[test]
fn a_webp_colour_variant_draws_its_renders_from_a_depot_sub_directory() {
    let root = tempfile::tempdir().expect("create temp dir");
    let dir = write_g502x_plus_depot(root.path(), G502X_PLUS_WHITE_SIDE);
    let device = dir.join("images").join("device");
    let resolver = resolver_over(&[root.path()], Some(g502x_plus_index()));

    let asset = resolver
        .resolve(&g502x_plus(2), None)
        .expect("the G502 X PLUS ext2 resolves");

    assert_eq!(asset.image_path, device.join(G502X_PLUS_EXT2_FRONT));
    assert_eq!((asset.png_width, asset.png_height), (1662, 2800));
    let side = asset.side_view.as_ref().expect("the WebP side view");
    assert_eq!(side.image_path, device.join(G502X_PLUS_EXT2_SIDE));
    assert_eq!((side.png_width, side.png_height), (1157, 2800));
}

#[test]
fn a_manifest_sub_path_cannot_reach_outside_its_depot() {
    let root = tempfile::tempdir().expect("create temp dir");
    write_g502x_plus_depot(root.path(), "images/../../side.png");
    std::fs::write(root.path().join("side.png"), png_header(1153, 2800)).expect("write escape");
    let resolver = resolver_over(&[root.path()], Some(g502x_plus_index()));

    let asset = resolver
        .resolve(&g502x_plus(1), None)
        .expect("the white G502 X PLUS resolves");

    assert_eq!(asset.side_view, None);
}
