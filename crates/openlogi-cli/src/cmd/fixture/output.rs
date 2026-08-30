//! Validated fixture-only atomic publication.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use atomic_write_file::AtomicWriteFile;
use serde::Serialize;

pub(super) fn ensure_output_available(path: &Path, force: bool) -> Result<()> {
    if !force && path.try_exists().context("could not inspect output path")? {
        bail!(
            "output {} already exists; pass --force to replace it after validation",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn write_json_atomically<T: Serialize>(
    path: &Path,
    value: &T,
    force: bool,
    asset: &str,
) -> Result<()> {
    ensure_output_available(path, force)?;
    let mut json =
        serde_json::to_vec_pretty(value).with_context(|| format!("could not serialize {asset}"))?;
    json.push(b'\n');
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {asset} output directory"))?;
    }
    ensure_output_available(path, force)?;
    let mut output = AtomicWriteFile::open(path)
        .with_context(|| format!("could not open atomic {asset} output"))?;
    output
        .write_all(&json)
        .with_context(|| format!("could not write atomic {asset} output"))?;
    if let Err(error) = ensure_output_available(path, force) {
        output
            .discard()
            .with_context(|| format!("could not discard refused {asset} output"))?;
        return Err(error);
    }
    output
        .commit()
        .with_context(|| format!("could not commit atomic {asset} output"))
}

#[cfg(test)]
mod tests {
    use openlogi_device::fixture::{
        CassetteExchange, FIXTURE_SCHEMA_VERSION, HidCassette, ReportSupport, RequestMatch,
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

        write_json_atomically(&output, &cassette, false, "HID cassette")
            .expect("atomic write succeeds");

        let bytes = std::fs::read(&output).expect("read cassette");
        assert!(bytes.ends_with(b"\n"));
        assert!(bytes.windows(2).any(|window| window == b"  "));
        let decoded: HidCassette = serde_json::from_slice(&bytes).expect("valid cassette JSON");
        assert_eq!(decoded, cassette);
        write_json_atomically(&output, &cassette, false, "HID cassette")
            .expect_err("a later non-force write must preserve the existing cassette");
    }
}
