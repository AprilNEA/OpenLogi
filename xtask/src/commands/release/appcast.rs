//! The Sparkle appcasts: one feed per macOS architecture, each offering that
//! architecture's notarized DMG of this release, signed with Sparkle's EdDSA
//! key.
//!
//! Runs in the publish job after the DMGs are stapled — stapling rewrites the
//! file, so a signature made before it would not match what users download.
//! The bundles read these feeds from the URLs `support::update_feed` gives
//! them, and the publish job uploads them there.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use clap::Parser;
use ed25519_compact::{KeyPair, PublicKey, Seed, Signature};
use strum::VariantArray as _;
use time::{OffsetDateTime, format_description::well_known::Rfc2822};

use crate::support::update_feed::{self, MacArch};

/// The base64 private key: the 32-byte seed Sparkle's `generate_keys -x`
/// exports. Read only from the environment, so it never shows up in a
/// process listing.
const PRIVATE_KEY_ENV: &str = "OPENLOGI_SPARKLE_PRIVATE_ED_KEY";

#[derive(Parser)]
pub(crate) struct Args {
    /// Directory holding the stapled `…-macos-<arch>.dmg` release artifacts.
    #[arg(long, default_value = "dist")]
    dist: PathBuf,
    /// Directory the `appcast-<arch>.xml` feeds are written to. Keep it out
    /// of `--dist`, whose contents are uploaded as release assets.
    #[arg(long, default_value = ".release/appcast")]
    output_dir: PathBuf,
    /// Release tag, for example `v0.8.14`.
    #[arg(long, env = "GITHUB_REF_NAME")]
    tag: String,
    /// Public update base URL, for example `https://updates.openlogi.org`.
    #[arg(long, env = update_feed::BASE_URL_ENV)]
    base_url: String,
    /// The public key the bundles embed (`SUPublicEDKey`). Signing fails unless
    /// the private key belongs to it: a feed signed with any other key is one
    /// every installed app rejects.
    #[arg(long, env = update_feed::SPARKLE_PUBLIC_KEY_ENV)]
    public_key: String,
    /// Markdown release notes for Sparkle's update window.
    #[arg(long)]
    release_notes: Option<PathBuf>,
}

/// One architecture's offer: the DMG, its size, and its EdDSA signature.
struct Enclosure {
    url: String,
    length: u64,
    signature: String,
}

/// Everything an appcast item says about the release besides its enclosure.
struct Release<'a> {
    /// `CFBundleVersion` of the bundles in the DMGs, which Sparkle compares
    /// against the running app's. `macos package` stamps the crate version
    /// there in place of cargo-bundle's build timestamp.
    version: &'a str,
    pub_date: String,
    page_url: String,
    notes: Option<String>,
}

pub(crate) fn run(args: &Args) -> Result<()> {
    let private_key = std::env::var(PRIVATE_KEY_ENV).with_context(|| {
        format!("set {PRIVATE_KEY_ENV} to the base64 key `generate_keys -x` exports")
    })?;
    write_appcasts(args, &private_key)
}

fn write_appcasts(args: &Args, private_key: &str) -> Result<()> {
    let version = args.tag.strip_prefix('v').unwrap_or(&args.tag);
    ensure_sparkle_orders(version)?;
    let key = signing_key(private_key)?;
    let public_key = decode_public_key(&args.public_key)?;
    ensure!(
        key.pk == public_key,
        "{PRIVATE_KEY_ENV} does not belong to the public key the bundles embed"
    );

    let notes = args
        .release_notes
        .as_deref()
        .map(|path| {
            fs_err::read_to_string(path)
                .with_context(|| format!("could not read release notes {}", path.display()))
        })
        .transpose()?;
    let release = Release {
        version,
        pub_date: OffsetDateTime::now_utc()
            .format(&Rfc2822)
            .context("could not format the publication date")?,
        page_url: format!(
            "https://github.com/AprilNEA/OpenLogi/releases/tag/{}",
            args.tag
        ),
        notes,
    };
    let release_base = format!(
        "{}/releases/{}",
        args.base_url.trim_end_matches('/'),
        args.tag
    );

    fs_err::create_dir_all(&args.output_dir)
        .with_context(|| format!("could not create {}", args.output_dir.display()))?;
    for (arch, dmg) in release_dmgs(&args.dist)? {
        let bytes =
            fs_err::read(&dmg).with_context(|| format!("could not read {}", dmg.display()))?;
        let name = dmg
            .file_name()
            .and_then(|name| name.to_str())
            .context("release DMG name is not UTF-8")?;
        let enclosure = Enclosure {
            url: format!("{release_base}/{name}"),
            length: bytes.len() as u64,
            signature: sign(&key, &bytes)?,
        };
        let feed = args.output_dir.join(arch.appcast_file_name());
        let feed_url = update_feed::appcast_url(&args.base_url, arch);
        fs_err::write(&feed, appcast(&feed_url, &release, &enclosure))
            .with_context(|| format!("could not write {}", feed.display()))?;
        println!("{} → {}", feed.display(), feed_url);
    }
    Ok(())
}

/// Refuse a version Sparkle would order wrongly. Its comparator
/// (`SUStandardVersionComparator`) stops at the first `-`, so it reads
/// `1.2.0-rc.1` as `1.2.0`: whoever took the pre-release from the stable feed
/// would never be offered the final 1.2.0.
fn ensure_sparkle_orders(version: &str) -> Result<()> {
    let parts: Vec<_> = version.split('.').collect();
    ensure!(
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
        "the stable appcast only carries MAJOR.MINOR.PATCH releases, not {version:?}"
    );
    Ok(())
}

/// The key pair behind Sparkle's exported private key: base64 of the 32-byte
/// Ed25519 seed. Sparkle 1's 96-byte export is an expanded key that no
/// longer yields its seed; re-export it with Sparkle 2's `generate_keys -x`.
fn signing_key(encoded: &str) -> Result<KeyPair> {
    let bytes = BASE64
        .decode(encoded.trim())
        .with_context(|| format!("{PRIVATE_KEY_ENV} is not base64"))?;
    match bytes.len() {
        Seed::BYTES => Ok(KeyPair::from_seed(Seed::from_slice(&bytes)?)),
        96 => bail!(
            "{PRIVATE_KEY_ENV} is a legacy 96-byte Sparkle key; export the seed with Sparkle 2's `generate_keys -x`"
        ),
        len => bail!("{PRIVATE_KEY_ENV} must be a 32-byte Ed25519 seed, not {len} bytes"),
    }
}

fn decode_public_key(encoded: &str) -> Result<PublicKey> {
    let bytes = BASE64
        .decode(encoded.trim())
        .context("the Sparkle public key is not base64")?;
    PublicKey::from_slice(&bytes).context("the Sparkle public key is not an Ed25519 public key")
}

/// `sparkle:edSignature` over `bytes`: a plain (deterministic) Ed25519
/// signature of the whole file, as Sparkle's `sign_update` makes it, checked
/// before it is written.
fn sign(key: &KeyPair, bytes: &[u8]) -> Result<String> {
    let signature: Signature = key.sk.sign(bytes, None);
    key.pk
        .verify(bytes, &signature)
        .context("the EdDSA signature does not verify")?;
    Ok(BASE64.encode(signature.as_ref()))
}

/// The release DMG of every macOS architecture in `dist`, in [`MacArch`]
/// order. Both are required: the publish gate guarantees them, and a feed
/// left behind would keep offering that architecture the previous release.
fn release_dmgs(dist: &Path) -> Result<Vec<(MacArch, PathBuf)>> {
    let mut dmgs: Vec<(MacArch, PathBuf)> = Vec::new();
    for entry in fs_err::read_dir(dist)
        .with_context(|| format!("could not read artifact directory {}", dist.display()))?
    {
        let path = entry?.path();
        let Some(arch) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(MacArch::of_dmg)
        else {
            continue;
        };
        if let Some((_, other)) = dmgs.iter().find(|(seen, _)| *seen == arch) {
            bail!(
                "two {} DMGs: {} and {}",
                arch.as_str(),
                other.display(),
                path.display()
            );
        }
        dmgs.push((arch, path));
    }
    for arch in MacArch::VARIANTS {
        ensure!(
            dmgs.iter().any(|(seen, _)| seen == arch),
            "no {} DMG in {}",
            arch.as_str(),
            dist.display()
        );
    }
    dmgs.sort_by_key(|(arch, _)| MacArch::VARIANTS.iter().position(|a| a == arch));
    Ok(dmgs)
}

/// The appcast at `feed_url`: one item, offering `enclosure`.
fn appcast(feed_url: &str, release: &Release<'_>, enclosure: &Enclosure) -> String {
    let notes = release.notes.as_deref().map_or_else(String::new, |notes| {
        format!(
            "      <description sparkle:format=\"markdown\"><![CDATA[{}]]></description>\n",
            notes.replace("]]>", "]]]]><![CDATA[>")
        )
    });
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>OpenLogi</title>
    <link>{feed_url}</link>
    <description>OpenLogi updates ({channel} channel)</description>
    <item>
      <title>OpenLogi {version}</title>
      <pubDate>{pub_date}</pubDate>
      <sparkle:version>{version}</sparkle:version>
      <sparkle:shortVersionString>{version}</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>{minimum_os}</sparkle:minimumSystemVersion>
      <link>{page_url}</link>
      <sparkle:fullReleaseNotesLink>{page_url}</sparkle:fullReleaseNotesLink>
{notes}      <enclosure url="{url}" length="{length}" type="application/x-apple-diskimage" sparkle:edSignature="{signature}"/>
    </item>
  </channel>
</rss>
"#,
        feed_url = escape(feed_url),
        channel = update_feed::CHANNEL,
        version = escape(release.version),
        pub_date = escape(&release.pub_date),
        minimum_os = update_feed::MACOS_MINIMUM_OS_VERSION,
        page_url = escape(&release.page_url),
        url = escape(&enclosure.url),
        length = enclosure.length,
        signature = escape(&enclosure.signature),
    )
}

/// `text` escaped for an XML attribute value or element text.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests;
