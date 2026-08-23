//! The icons OpenLogi ships, and what a platform's packaging does with them.
//!
//! [`AppIcon`] is the set: one entry per icon the app can wear, independent of
//! how any platform stores it. [`IconPipeline`] is what a platform has to be
//! able to do with that set — compile it into whatever its packages read, put
//! the result inside a package, and prove it arrived.
//!
//! macOS is the pipeline that exists ([`macos::AppBundle`]). The other two get
//! their icons without a build step today: Windows embeds `design/icon/
//! openlogi.ico` into each executable through its build script, and Linux
//! installs `design/icon/openlogi.png` from `packaging/linux/nfpm.yaml`. When
//! either grows one — a per-variant `.ico`, a hicolor tree — it implements this
//! trait rather than inventing its own vocabulary.

pub(crate) mod macos;

use std::path::Path;

use anyhow::Result;
use strum::{Display, VariantArray};

/// An icon the app can wear.
///
/// The set lives here because every pass over it — compiling the sources,
/// putting the alternates where the app can find them, checking they arrived —
/// has to cover a newly added icon without anyone remembering to extend a list.
/// `Display` renders the name the icon is known by outside the build: the file
/// it ships as, and the value the app persists once a user picks it.
///
/// What an icon is *made of* is the platform's business, not this type's: macOS
/// compiles an Icon Composer document, Windows would want a `.ico`. Each
/// [`IconPipeline`] maps a variant to its own source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, VariantArray)]
#[strum(serialize_all = "kebab-case")]
pub(crate) enum AppIcon {
    /// What the app wears out of the box, and the only one a platform draws
    /// without being told to.
    Openlogi,
    /// The dark alternate.
    Midnight,
}

impl AppIcon {
    /// The icon a package wears until something switches it.
    pub(crate) const DEFAULT: Self = Self::Openlogi;

    /// Whether this is the icon the package already wears, which is the one
    /// case a pipeline never has to install anywhere: going back to it clears
    /// the override instead of applying a file.
    pub(crate) fn is_default(self) -> bool {
        self == Self::DEFAULT
    }
}

/// What one platform's packaging does with [`AppIcon`].
pub(crate) trait IconPipeline {
    /// Compile every icon in the set into build outputs this platform's
    /// packaging can consume.
    fn compile(&self) -> Result<()>;

    /// Put whatever the packaged app reads at runtime inside `package`.
    fn install(&self, package: &Path) -> Result<()>;

    /// Fail unless `package` carries everything [`Self::install`] promised, so
    /// a picker offering an icon can never point at a file that is not there.
    fn verify(&self, package: &Path) -> Result<()>;
}
