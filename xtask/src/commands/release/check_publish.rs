use std::collections::HashMap;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use xshell::{Shell, cmd};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    publish: Option<Vec<String>>,
    dependencies: Vec<Dependency>,
}

impl Package {
    fn is_publishable(&self) -> bool {
        self.publish
            .as_ref()
            .is_none_or(|registries| !registries.is_empty())
    }
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    req: String,
    path: Option<String>,
    kind: Option<String>,
}

pub(crate) fn run() -> Result<()> {
    let sh = Shell::new()?;
    let metadata = cmd!(sh, "cargo metadata --format-version 1 --no-deps")
        .read()
        .context("could not read Cargo workspace metadata")?;
    let publishable = validate(&metadata)?;
    println!("publish closure valid ({publishable} publishable workspace packages)");
    Ok(())
}

fn validate(text: &str) -> Result<usize> {
    let metadata: Metadata =
        serde_json::from_str(text).context("could not parse Cargo workspace metadata")?;
    let packages: HashMap<&str, &Package> = metadata
        .packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect();
    let publishable = metadata
        .packages
        .iter()
        .filter(|package| package.is_publishable())
        .count();
    let mut violations = Vec::new();

    for package in metadata
        .packages
        .iter()
        .filter(|package| package.is_publishable())
    {
        for dependency in package.dependencies.iter().filter(|dependency| {
            dependency.path.is_some() && dependency.kind.as_deref() != Some("dev")
        }) {
            if dependency.req == "*" {
                violations.push(format!(
                    "`{}` path dependency on `{}` must declare a registry version",
                    package.name, dependency.name
                ));
            }
            match packages.get(dependency.name.as_str()) {
                Some(target) if !target.is_publishable() => violations.push(format!(
                    "`{}` depends on unpublished path package `{}`",
                    package.name, dependency.name
                )),
                None => violations.push(format!(
                    "`{}` path dependency `{}` is not a workspace package",
                    package.name, dependency.name
                )),
                Some(_) => {}
            }
        }
    }

    if !violations.is_empty() {
        bail!("invalid publish closure:\n- {}", violations.join("\n- "));
    }
    Ok(publishable)
}

#[cfg(test)]
mod tests;
