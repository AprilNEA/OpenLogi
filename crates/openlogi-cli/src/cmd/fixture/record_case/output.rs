//! Validated cassette-only atomic publication.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use atomic_write_file::AtomicWriteFile;
use openlogi_device::fixture::HidCassette;

pub(super) fn ensure_output_available(path: &Path, force: bool) -> Result<()> {
    if !force && path.try_exists().context("could not inspect output path")? {
        bail!(
            "output {} already exists; pass --force to replace it after validation",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn write_cassette_atomically(
    path: &Path,
    cassette: &HidCassette,
    force: bool,
) -> Result<()> {
    ensure_output_available(path, force)?;
    let mut json =
        serde_json::to_vec_pretty(cassette).context("could not serialize HID cassette")?;
    json.push(b'\n');
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).context("could not create cassette output directory")?;
    }
    ensure_output_available(path, force)?;
    let mut output =
        AtomicWriteFile::open(path).context("could not open atomic cassette output")?;
    output
        .write_all(&json)
        .context("could not write atomic cassette output")?;
    if let Err(error) = ensure_output_available(path, force) {
        output
            .discard()
            .context("could not discard refused cassette output")?;
        return Err(error);
    }
    output
        .commit()
        .context("could not commit atomic cassette output")
}

#[cfg(test)]
mod tests {
    use openlogi_device::fixture::{
        CassetteExchange, FIXTURE_SCHEMA_VERSION, ReportSupport, RequestMatch,
    };

    use super::*;

    #[test]
    fn refuses_existing_files_without_force() {
        let directory = tempfile::tempdir().expect("tempdir");
        let output = directory.path().join("case.json");
        std::fs::write(&output, "existing").expect("seed existing output");

        ensure_output_available(&output, false).expect_err("existing output must be refused");
        ensure_output_available(&output, true).expect("force permits replacement");
    }

    #[test]
    fn atomic_json_is_pretty_valid_and_newline_terminated() {
        let directory = tempfile::tempdir().expect("tempdir");
        let output = directory.path().join("nested/case.json");
        let cassette = HidCassette {
            schema_version: FIXTURE_SCHEMA_VERSION,
            name: "atomic-output".to_string(),
            channel: "direct".to_string(),
            report_support: ReportSupport::ShortAndLong,
            exchanges: vec![CassetteExchange {
                request_match: RequestMatch::Hidpp20,
                request: vec![0x10, 0xff, 0x00, 0x10, 0, 0, 0],
                response: Some(vec![0x10, 0xff, 0x00, 0x10, 4, 0, 0]),
                required: true,
            }],
        };

        write_cassette_atomically(&output, &cassette, false).expect("atomic write succeeds");

        let bytes = std::fs::read(&output).expect("read cassette");
        assert!(bytes.ends_with(b"\n"));
        assert!(bytes.windows(2).any(|window| window == b"  "));
        let decoded: HidCassette = serde_json::from_slice(&bytes).expect("valid cassette JSON");
        assert_eq!(decoded, cassette);
        write_cassette_atomically(&output, &cassette, false)
            .expect_err("a later non-force write must preserve the existing cassette");
    }
}
