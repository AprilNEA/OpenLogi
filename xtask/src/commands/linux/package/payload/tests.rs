use super::*;

/// A tar-sized stand-in: the rewrite never looks inside the tar.
const TAR: &[u8] = b"usr/bin/openlogi\0usr/bin/openlogi-agent\0usr/bin/openlogi-desktop\0";

const CONTROL: &[u8] = b"control.tar.gz bytes, kept as they are";

/// Size of the block [`far_repeat`] writes twice.
const BLOCK: usize = 256 * 1024;

/// Zeros between the two copies. 9 MiB is farther than the 8 MiB that zstd
/// level 19 and xz preset 6 reach by default, so only the configured window
/// finds the second copy.
const GAP: usize = 9 * 1024 * 1024;

/// Incompressible bytes from a fixed xorshift seed.
fn noise(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// A block, [`GAP`] zeros, then the same block: the shape of two binaries
/// that share code megabytes apart.
fn far_repeat() -> Vec<u8> {
    let block = noise(BLOCK);
    let mut tar = block.clone();
    tar.resize(BLOCK + GAP, 0);
    tar.extend_from_slice(&block);
    tar
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// An ar archive with nfpm's member headers: root-owned, 0644, one mtime.
fn deb(members: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut builder = ar::Builder::new(Vec::new());
    for (identifier, body) in members {
        let mut header = ar::Header::new(identifier.to_vec(), u64::try_from(body.len()).unwrap());
        header.set_mtime(1_793_534_385);
        header.set_mode(0o100_644);
        builder.append(&header, *body).unwrap();
    }
    builder.into_inner().unwrap()
}

/// Every member's header and body, in archive order.
fn members(deb: &[u8]) -> Vec<(ar::Header, Vec<u8>)> {
    let mut archive = ar::Archive::new(deb);
    let mut members = Vec::new();
    while let Some(entry) = archive.next_entry() {
        let mut entry = entry.unwrap();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        members.push((entry.header().clone(), body));
    }
    members
}

#[test]
fn deb_data_member_becomes_xz_of_the_same_tar() {
    let original = deb(&[
        (b"debian-binary", b"2.0\n"),
        (b"control.tar.gz", CONTROL),
        (b"data.tar.gz", &gzip(TAR)),
    ]);

    let rewritten = members(&deb_with_xz_data(&original).unwrap());
    let before = members(&original);

    let names: Vec<&[u8]> = rewritten
        .iter()
        .map(|(header, _)| header.identifier())
        .collect();
    assert_eq!(
        names,
        [&b"debian-binary"[..], b"control.tar.gz", b"data.tar.xz"]
    );
    assert_eq!(rewritten[0].1, b"2.0\n");
    assert_eq!(rewritten[1].1, CONTROL);
    assert_eq!(liblzma::decode_all(rewritten[2].1.as_slice()).unwrap(), TAR);
    for ((old, _), (new, _)) in before.iter().zip(&rewritten) {
        assert_eq!(
            (old.mtime(), old.uid(), old.gid(), old.mode()),
            (new.mtime(), new.uid(), new.gid(), new.mode()),
            "{} lost its ar header",
            String::from_utf8_lossy(new.identifier())
        );
    }
}

#[test]
fn a_deb_nfpm_did_not_gzip_is_refused() {
    let already_xz = deb(&[
        (b"debian-binary", b"2.0\n"),
        (b"control.tar.gz", CONTROL),
        (b"data.tar.xz", &liblzma::encode_all(TAR, 0).unwrap()),
    ]);

    let error = deb_with_xz_data(&already_xz).unwrap_err().to_string();
    assert!(error.contains("deb.compression"), "{error}");
}

#[test]
fn arch_package_decodes_to_the_same_tar() {
    let encoded = zstd_long(TAR).unwrap();

    assert_eq!(zstd::stream::decode_all(encoded.as_slice()).unwrap(), TAR);
}

#[test]
fn arch_window_reaches_a_copy_beyond_the_default_window() {
    let tar = far_repeat();

    let encoded = zstd_long(&tar).unwrap();

    assert_eq!(zstd::stream::decode_all(encoded.as_slice()).unwrap(), tar);
    // Both copies cost about 2 * BLOCK; a window that reaches back costs one.
    assert!(
        encoded.len() < BLOCK * 3 / 2,
        "{} bytes: the second copy was not matched",
        encoded.len()
    );
}

#[test]
fn deb_xz_reaches_a_copy_beyond_the_default_dictionary() {
    let tar = far_repeat();
    let original = deb(&[
        (b"debian-binary", b"2.0\n"),
        (b"control.tar.gz", CONTROL),
        (b"data.tar.gz", &gzip(&tar)),
    ]);

    let rewritten = members(&deb_with_xz_data(&original).unwrap());

    let data = &rewritten[2].1;
    assert_eq!(liblzma::decode_all(data.as_slice()).unwrap(), tar);
    assert!(
        data.len() < BLOCK * 3 / 2,
        "{} bytes: the second copy was not matched",
        data.len()
    );
}
