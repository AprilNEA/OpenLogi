use super::*;
use openlogi_assets::DeviceEntry;
use openlogi_core::device::DeviceTransports;
use std::collections::HashMap;

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

/// A 24-byte PNG: signature + an `IHDR` chunk header carrying only the
/// width/height — all `read_png_dimensions` actually reads.
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

    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: None,
    };
    let entry = DeviceEntry {
        model_id: "eb020".to_string(),
        model_ids: Vec::new(),
        display_name: "MX Vertical".to_string(),
        kind: "MOUSE".to_string(),
        asset_path: format!("v1/devices/{depot}/"),
        files: Vec::new(),
    };

    let asset = resolver
        .load_files(depot, &entry, &bare_model())
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

    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: None,
    };
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
        .load_files(depot, &entry, &model)
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
    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: Some(index_of(depot, entry)),
    };
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
    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: Some(index),
    };

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
    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: Some(index),
    };

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
    let resolver = AssetResolver {
        read_roots: vec![root.path().to_path_buf()],
        write_root: root.path().to_path_buf(),
        has_bundle: false,
        index: Some(index_of(
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
    };
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
    let resolver = AssetResolver {
        read_roots: roots.iter().map(|root| root.path().to_path_buf()).collect(),
        write_root: roots[1].path().to_path_buf(),
        has_bundle: true,
        index: Some(index_of(
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
    };

    let asset = resolver
        .resolve_registry_model("8c900")
        .expect("bundle asset should resolve");
    assert_eq!((asset.png_width, asset.png_height), (10, 10));
    assert_eq!(
        asset.image_path,
        roots[0].path().join("litra_glow/front.png")
    );
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
