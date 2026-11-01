//! Re-encoding the payload of the packages nfpm writes.
//!
//! The four packaged binaries share most of their code, but they sit tens of
//! megabytes apart in the package's tar stream. nfpm compresses a `.deb` with
//! gzip, whose 32 KiB window cannot reach from one binary back to the next, and
//! an Arch package with zstd's default window. A window that spans the whole
//! tar finds the shared code: for an amd64 build of 0.8.20 the `.deb` drops
//! from 43.5 MB to 20.5 MB and the Arch package from 41.6 MB to 22.2 MB.
//! nfpm exposes the codec but not the window or preset, and its own
//! `xz`/`zstd` settings save about 16%, so the packages are rewritten after
//! nfpm instead. The `.rpm` is left as nfpm writes it.

use std::io::{Read as _, Write as _};
use std::num::NonZeroUsize;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

/// The members nfpm writes into a `.deb`, in the order dpkg requires.
const NFPM_DEB_MEMBERS: [&[u8]; 3] = [b"debian-binary", b"control.tar.gz", b"data.tar.gz"];

/// The data member's name once it is xz.
const XZ_DATA_MEMBER: &[u8] = b"data.tar.xz";

/// xz preset for a `.deb`'s data member. Level 9 is the 64 MiB dictionary that
/// spans the tar; `--extreme` on top of it saved another 0.08% for 15% more
/// encode time.
const DEB_XZ_PRESET: u32 = 9;

/// zstd level for an Arch package.
const ARCH_ZSTD_LEVEL: i32 = 19;

/// log2 of the Arch package's zstd window, 128 MiB. That is the largest window
/// a zstd decoder accepts without being told to (`ZSTD_WINDOWLOG_LIMIT_DEFAULT`),
/// so pacman, through libarchive's default settings, reads the package without
/// any option. A larger window would need `--long` on every decoder.
const ARCH_ZSTD_WINDOW_LOG: u32 = 27;

/// Rewrite the `.deb` at `path` with its data member compressed as xz.
pub(super) fn recompress_deb(path: &Path) -> Result<()> {
    let deb = fs_err::read(path)?;
    let rewritten =
        deb_with_xz_data(&deb).with_context(|| format!("recompressing {}", path.display()))?;
    fs_err::write(path, rewritten)?;
    Ok(())
}

/// Rewrite the Arch package at `path` with a zstd window that spans its tar.
pub(super) fn recompress_arch(path: &Path) -> Result<()> {
    let package = fs_err::read(path)?;
    let tar = zstd::stream::decode_all(package.as_slice())
        .with_context(|| format!("decompressing {}", path.display()))?;
    let rewritten = zstd_long(&tar).with_context(|| format!("recompressing {}", path.display()))?;
    fs_err::write(path, rewritten)?;
    Ok(())
}

/// `deb` with its gzip data member replaced by the same tar as xz.
///
/// Every other byte stays nfpm's: the members keep their order, names and ar
/// headers, and the tar inside keeps its owners, modes and mtimes because it is
/// re-encoded, not re-archived.
fn deb_with_xz_data(deb: &[u8]) -> Result<Vec<u8>> {
    let mut archive = ar::Archive::new(deb);
    let mut builder = ar::Builder::new(Vec::new());
    let mut members = Vec::new();
    while let Some(entry) = archive.next_entry() {
        let mut entry = entry?;
        let mut body = Vec::new();
        entry.read_to_end(&mut body)?;
        let original = entry.header();
        members.push(original.identifier().to_vec());
        let (identifier, body) = if original.identifier() == NFPM_DEB_MEMBERS[2] {
            let mut tar = Vec::new();
            flate2::read::GzDecoder::new(body.as_slice()).read_to_end(&mut tar)?;
            (
                XZ_DATA_MEMBER.to_vec(),
                liblzma::encode_all(tar.as_slice(), DEB_XZ_PRESET)?,
            )
        } else {
            (original.identifier().to_vec(), body)
        };
        let mut header = ar::Header::new(identifier, u64::try_from(body.len())?);
        header.set_mtime(original.mtime());
        header.set_uid(original.uid());
        header.set_gid(original.gid());
        header.set_mode(original.mode());
        builder.append(&header, body.as_slice())?;
    }
    ensure!(
        members == NFPM_DEB_MEMBERS,
        "expected the .deb members nfpm writes by default ({}), found {}; \
         leave `deb.compression` unset in nfpm.yaml",
        member_list(&NFPM_DEB_MEMBERS),
        member_list(&members),
    );
    Ok(builder.into_inner()?)
}

/// `tar` as a level-19 zstd frame with a long window, encoded on every core.
fn zstd_long(tar: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), ARCH_ZSTD_LEVEL)?;
    encoder.long_distance_matching(true)?;
    encoder.window_log(ARCH_ZSTD_WINDOW_LOG)?;
    encoder.include_checksum(true)?;
    encoder.set_pledged_src_size(Some(u64::try_from(tar.len())?))?;
    let workers = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    encoder.multithread(u32::try_from(workers)?)?;
    encoder.write_all(tar)?;
    Ok(encoder.finish()?)
}

/// Member names for an error message.
fn member_list(members: &[impl AsRef<[u8]>]) -> String {
    members
        .iter()
        .map(|name| String::from_utf8_lossy(name.as_ref()).into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;
