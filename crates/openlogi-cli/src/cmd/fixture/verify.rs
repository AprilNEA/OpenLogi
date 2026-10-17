//! Strict on-disk fixture corpus verification.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use openlogi_fixture::fs::Fixture;

/// Arguments for strict fixture corpus verification.
#[derive(Args, Debug)]
pub struct VerifyArgs {
    /// Fixture directory containing manifest.json, profile.json, and declared cases.
    #[arg(value_name = "DIRECTORY")]
    pub directory: PathBuf,
}

pub fn run(args: &VerifyArgs) -> Result<()> {
    let fixture = Fixture::load(&args.directory)?;
    println!("verified fixture {}", fixture.manifest().id);
    println!(
        "  schema: manifest, profile, and {} cassette(s) are valid",
        fixture.cassettes().len()
    );
    println!("  privacy: exact synthetic identity ledger matched");
    println!("  relationships: profile and every declared case matched");
    println!(
        "  replay: {} declared cassette(s) passed framing and supported protocol validation",
        fixture.cassettes().len()
    );
    println!("  hardware: not exercised; semantic correctness is not established");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_fixture::{CANONICAL_DEVICE_PROFILE_JSON, CANONICAL_FIXTURE_MANIFEST_JSON};

    #[test]
    fn packaged_canonical_fixture_is_complete_and_valid() {
        let temp = tempfile::tempdir().expect("fixture directory");
        let directory = temp.path().join("openlogi-canonical-synthetic-001");
        std::fs::create_dir(&directory).expect("create specimen");
        std::fs::write(
            directory.join("profile.json"),
            CANONICAL_DEVICE_PROFILE_JSON,
        )
        .expect("write embedded profile");
        std::fs::write(
            directory.join("manifest.json"),
            CANONICAL_FIXTURE_MANIFEST_JSON,
        )
        .expect("write embedded manifest");
        run(&VerifyArgs { directory })
            .expect("CLI verifies packaged assets without workspace paths");
    }

    #[test]
    fn contributed_fixture_corpus_is_complete_and_valid() {
        openlogi_fixture::fs::repository_corpus()
            .expect("every contributed fixture must pass strict verification");
    }
}
