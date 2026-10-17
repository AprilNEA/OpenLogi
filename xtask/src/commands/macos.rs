pub(crate) mod bundle;
pub(crate) mod dev_bundle;
pub(crate) mod dmg;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::icon::IconPipeline as _;
use crate::icon::macos::AppBundle;
use crate::support::update_feed;
use bundle::identity::Channel;

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Compile the macOS app icon from its Icon Composer document.
    Icon,
    /// Build the OpenLogi.app bundle.
    Bundle(BundleArgs),
    /// Wrap a freshly built desktop binary in `target/dev/OpenLogi.app`.
    /// Driven by the cargo runner; not normally run by hand.
    DevBundle(dev_bundle::Args),
    /// Create the branded macOS DMG from an existing app bundle.
    Dmg(dmg::Args),
    /// Build the app bundle for distribution, optionally sign it, and package
    /// the branded DMG.
    Package(PackageArgs),
}

#[derive(Parser)]
pub(crate) struct BundleArgs {
    /// Identity family to stamp into the bundle. Defaults to `dev` so a local
    /// build can never claim the installed app's permission grants or config;
    /// the shipped bundle comes from `macos package`.
    #[arg(long, value_enum, default_value_t = Channel::Dev)]
    channel: Channel,
}

#[derive(Parser)]
pub(crate) struct PackageArgs {
    #[command(flatten)]
    dmg: dmg::Args,
    /// Rust target to package. Omit it for the host architecture.
    #[arg(long, value_enum)]
    target: Option<bundle::DistributionTarget>,
    /// Sparkle's EdDSA public key (`SUPublicEDKey`, as `generate_keys`
    /// prints it). Given one, the bundle embeds Sparkle.framework and updates
    /// through it; without one it updates through the built-in engine.
    #[arg(long, env = update_feed::SPARKLE_PUBLIC_KEY_ENV)]
    sparkle_public_key: Option<String>,
    /// Update host the bundle's Sparkle feed is served from. Required with
    /// `--sparkle-public-key`.
    #[arg(long, env = update_feed::BASE_URL_ENV)]
    update_base_url: Option<String>,
}

impl PackageArgs {
    /// The Sparkle feed to embed, or `None` without a public key. An empty
    /// value counts as none: CI passes unset secrets as empty strings.
    fn sparkle_feed(&self) -> Result<Option<bundle::sparkle::Feed>> {
        let Some(public_key) = self
            .sparkle_public_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
        else {
            return Ok(None);
        };
        let Some(base_url) = self
            .update_base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
        else {
            bail!(
                "set {} too: the bundle's Sparkle feed lives on the update host",
                update_feed::BASE_URL_ENV
            );
        };
        let arch = bundle::DistributionTarget::arch(self.target)?;
        bundle::sparkle::Feed::new(public_key, base_url, arch).map(Some)
    }
}

pub(crate) fn run(command: Command) -> Result<()> {
    match command {
        Command::Icon => AppBundle.compile(),
        Command::Bundle(args) => bundle::run(args.channel),
        Command::DevBundle(args) => dev_bundle::run(&args),
        Command::Dmg(args) => dmg::run(&args),
        Command::Package(args) => {
            let feed = args.sparkle_feed()?;
            bundle::run_for_distribution(
                args.dmg.sign_identity.as_deref(),
                args.target,
                feed.as_ref(),
            )?;
            dmg::run(&args.dmg)
        }
    }
}
