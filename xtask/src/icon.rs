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

/// The set itself is [`openlogi_core::config::AppIcon`]: the app persists the
/// user's choice, so the build and the running app have to agree on which icons
/// exist and what each is called. Packaging only adds how they are made — which
/// is the platform's business, not the type's: macOS compiles an Icon Composer
/// document, Windows would want a `.ico`. Each [`IconPipeline`] maps a variant
/// to its own source.
pub(crate) use openlogi_core::config::AppIcon;

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
