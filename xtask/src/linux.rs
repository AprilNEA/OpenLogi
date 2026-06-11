use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use anyhow::Result;
use clap::Parser;

use crate::util::{absolutize, ensure_command, ensure_file, repo_root, run};

#[derive(Parser)]
pub(crate) struct PackageLinux {
    /// Output directory for .deb and .rpm packages (default: target/release).
    #[arg(long, default_value = "target/release")]
    output: PathBuf,
    /// Skip the cargo build step (binaries must already exist in target/release).
    #[arg(long)]
    no_build: bool,
}

pub(crate) fn package_linux(args: &PackageLinux) -> Result<()> {
    let root = repo_root()?;

    if !args.no_build {
        println!("==> build release binaries");
        run(ProcessCommand::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                "openlogi",
                "-p",
                "openlogi-gui",
                "-p",
                "openlogi-agent",
            ])
            .current_dir(&root))?;
    }

    for bin in ["openlogi", "openlogi-gui", "openlogi-agent"] {
        ensure_file(&root.join("target/release").join(bin))?;
    }

    ensure_command("nfpm")?;

    let output = absolutize(&root, &args.output);
    let config = root.join("packaging/linux/nfpm.yaml");

    for packager in ["deb", "rpm"] {
        println!("==> nfpm {packager}");
        run(ProcessCommand::new("nfpm")
            .args(["package", "--packager", packager, "--config"])
            .arg(&config)
            .arg("--target")
            .arg(&output)
            .env("VERSION", env!("CARGO_PKG_VERSION"))
            .current_dir(&root))?;
    }

    println!();
    println!("Linux packages written to {}", output.display());
    Ok(())
}
