use super::*;

/// A tar-sized stand-in: the rewrite never looks inside the tar.
const TAR: &[u8] = b"usr/bin/openlogi\0usr/bin/openlogi-agent\0usr/bin/openlogi-desktop\0";

const CONTROL: &[u8] = b"control.tar.gz bytes, kept as they are";

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
