//! `openlogi diag g700` — Logitech G700 onboard-profile diagnostics.
//!
//! Every subcommand finds the mouse the way `openlogi list` does — a probe of
//! this host's devices, which asks the G700's receiver to announce its paired
//! devices — and then addresses the G700 that probe lists.
//!
//! `dump` reads the G700's settings registers and all of its onboard memory,
//! prints the profiles it finds, and can save every page to a JSON backup.
//! None of its requests to the mouse change it.
//!
//! The other subcommands write, one setting at a time, and read back what
//! they wrote: `restore` puts pages back from a `dump` backup, `set-dpi` and
//! `set-rate --profile` change one onboard profile, and `set-rate`,
//! `set-resolution` and `set-profile` set the live registers.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use atomic_write_file::AtomicWriteFile;
use clap::{ArgGroup, Args, Subcommand};
use openlogi_hid::g700::{
    self as codec, Binding, CurrentProfile, DIRECTORY_PAGE, DpiLed, DpiMode, G700Profile,
    PAGE_SIZE, Page, ProfileDirectory, RAM_PAGE, ReportInterval, Resolution, Special,
};
use openlogi_hid::{
    BackupOrigin, DeviceRoute, G700Identity, G700MemoryDump, PageRead, ProfileDpiChange,
    ProfileRateChange, RestoreOutcome,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// The `format` field of a backup file.
const BACKUP_FORMAT: &str = "openlogi-g700-memory-dump";

/// The `version` field of a backup file this build writes. Version 2 adds the
/// `identity` block; version 1 carries the same facts in its `route`,
/// `device_index` and `pages` fields, and still restores.
const BACKUP_VERSION: u64 = 2;

#[derive(Debug, Args)]
pub struct G700Args {
    #[command(subcommand)]
    pub command: G700Cmd,
}

#[derive(Debug, Subcommand)]
pub enum G700Cmd {
    /// Read the settings registers and every onboard memory page (read-only)
    /// and print the profiles.
    Dump(DumpArgs),
    /// Put onboard memory pages back from a `dump --out` backup of this same
    /// mouse. Pages that already match are skipped; every write is read back.
    Restore(RestoreArgs),
    /// Change one DPI mode of one onboard profile. Rewrites the profile's
    /// flash page; every other byte of it stays as it was.
    SetDpi(SetDpiArgs),
    /// Set the report interval: of the mouse until it switches profile
    /// (register 0x64), or with `--profile` the one an onboard profile plays
    /// at (rewrites its flash page).
    SetRate(SetRateArgs),
    /// Set the resolution the sensor runs at (register 0x63). It lasts until
    /// the mouse switches DPI mode or profile.
    SetResolution(SetResolutionArgs),
    /// Select the onboard profile the mouse plays (register 0x0F).
    SetProfile(SetProfileArgs),
}

#[derive(Debug, Args)]
pub struct DumpArgs {
    /// Also save every page, with its CRC check, to this JSON file.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// Overwrite the `--out` file when it already exists.
    #[arg(long, requires = "out")]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// The backup `dump --out` wrote.
    #[arg(value_name = "BACKUP")]
    pub backup: PathBuf,
    /// Restore only this page; repeat for several. Default: every page the
    /// backup holds, except the RAM page 0.
    #[arg(long = "page", value_name = "N")]
    pub pages: Vec<u8>,
    /// Restore even when the mouse does not match the backup's identity —
    /// its route, device index, memory size and profile directory.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("value").required(true).args(["dpi", "raw"])))]
pub struct SetDpiArgs {
    /// The profile's index, as `dump` numbers them.
    #[arg(long, value_name = "INDEX")]
    pub profile: u8,
    /// The DPI mode, 0 to 4.
    #[arg(long, value_name = "MODE")]
    pub mode: usize,
    /// The DPI for both axes; the nearest sensor step is written.
    #[arg(long)]
    pub dpi: Option<u16>,
    /// The raw sensor step for both axes, 1 to 242, as `dump` prints it.
    #[arg(long, value_name = "STEP")]
    pub raw: Option<u8>,
}

#[derive(Debug, Args)]
pub struct SetRateArgs {
    /// The interval between reports in milliseconds: 1 is 1000 Hz, 4 is 250 Hz.
    #[arg(long, value_name = "MS")]
    pub interval_ms: u8,
    /// Change this onboard profile's interval instead, by its index as
    /// `dump` numbers them: 1, 2, 4 or 8 ms.
    #[arg(long, value_name = "INDEX")]
    pub profile: Option<u8>,
}

#[derive(Debug, Args)]
pub struct SetResolutionArgs {
    /// The raw sensor step, as `dump` prints it.
    #[arg(long, value_name = "STEP")]
    pub raw: u16,
    /// A different raw step for the Y axis. Default: the X step.
    #[arg(long, value_name = "STEP")]
    pub raw_y: Option<u16>,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("profile").required(true).args(["index", "factory"])))]
pub struct SetProfileArgs {
    /// The profile's index, as `dump` numbers them.
    #[arg(long, value_name = "INDEX")]
    pub index: Option<u8>,
    /// The firmware's built-in profile, which also stops the profile button
    /// from switching.
    #[arg(long)]
    pub factory: bool,
}

pub async fn run(args: G700Args) -> Result<()> {
    match args.command {
        G700Cmd::Dump(args) => dump(args).await,
        G700Cmd::Restore(args) => restore(args).await,
        G700Cmd::SetDpi(args) => set_dpi(args).await,
        G700Cmd::SetRate(args) => set_rate(args).await,
        G700Cmd::SetResolution(args) => set_resolution(args).await,
        G700Cmd::SetProfile(args) => set_profile(args).await,
    }
}

/// The hint every command adds when the mouse cannot be reached.
const UNREACHABLE_HINT: &str = "is the G700 paired and awake, or on its cable?";

/// The route of the G700 a probe of this host lists.
async fn g700_route() -> Result<DeviceRoute> {
    let inventories = openlogi_hid::enumerate()
        .await
        .map_err(|error| anyhow!("probing this host's devices failed: {error}"))?;
    openlogi_hid::find_g700_route(&inventories)
        .ok_or_else(|| anyhow!("no G700 found: {UNREACHABLE_HINT}"))
}

async fn dump(args: DumpArgs) -> Result<()> {
    if let Some(out) = &args.out
        && out.exists()
        && !args.force
    {
        bail!(
            "{} exists; pass --force to overwrite it, or pick another file",
            out.display()
        );
    }
    let route = g700_route().await?;
    let dump = openlogi_hid::read_g700_memory(&route)
        .await
        .map_err(|error| anyhow!("{error}: {UNREACHABLE_HINT}"))?;
    let route = &dump.route;
    println!("device: G700 ({route})");

    print!("{}", Summary(&dump));

    if let Some(out) = &args.out {
        let backup = backup_json(&dump, unix_now());
        let mut text = serde_json::to_vec_pretty(&backup)?;
        text.push(b'\n');
        // Atomic, so an interrupted run never leaves a truncated backup.
        let mut file = AtomicWriteFile::open(out)
            .with_context(|| format!("open the backup {}", out.display()))?;
        file.write_all(&text)
            .and_then(|()| file.commit())
            .with_context(|| format!("write the backup to {}", out.display()))?;
        println!("backup: {}", out.display());
    }
    Ok(())
}

async fn restore(args: RestoreArgs) -> Result<()> {
    let backup = read_backup(&args.backup)?;
    let origin = if args.force {
        BackupOrigin::Forced
    } else {
        BackupOrigin::Identified(backup.identity)
    };
    let pages = select_pages(backup.pages, &args.pages)?;
    let numbers: Vec<u8> = pages.iter().map(|(page, _)| *page).collect();
    println!(
        "restoring pages {} from {}",
        PageList(&numbers),
        args.backup.display()
    );
    let route = g700_route().await?;
    let restored = openlogi_hid::restore_g700_pages(&route, &origin, pages)
        .await
        .map_err(|error| anyhow!("{error}\n  {error:?}"))?;
    for page in &restored {
        let outcome = match page.outcome {
            RestoreOutcome::Identical => "already identical, skipped",
            RestoreOutcome::Erased => "erased, verified",
            RestoreOutcome::Written => "written, verified",
        };
        println!("  page {}: {outcome}", page.page);
    }
    Ok(())
}

/// The pages of `backup` to restore: those `requested`, or every page but
/// the RAM page when none is. A request for the RAM page itself goes on to
/// the device layer, which refuses it before finding the mouse.
fn select_pages(backup: Vec<(u8, Page)>, requested: &[u8]) -> Result<Vec<(u8, Page)>> {
    if requested.is_empty() {
        return Ok(backup
            .into_iter()
            .filter(|(page, _)| *page != RAM_PAGE)
            .collect());
    }
    let mut selected = Vec::with_capacity(requested.len());
    for &number in requested {
        if selected.iter().any(|(page, _)| *page == number) {
            continue;
        }
        let Some(page) = backup.iter().find(|(page, _)| *page == number) else {
            bail!("the backup holds no bytes for page {number}");
        };
        selected.push(page.clone());
    }
    Ok(selected)
}

async fn set_dpi(args: SetDpiArgs) -> Result<()> {
    let raw = args
        .raw
        .or_else(|| args.dpi.map(codec::raw_from_dpi))
        .ok_or_else(|| anyhow!("pass --dpi or --raw"))?;
    let route = g700_route().await?;
    let write = openlogi_hid::set_g700_profile_dpi(
        &route,
        ProfileDpiChange {
            profile: args.profile,
            mode: args.mode,
            x: raw,
            y: raw,
        },
    )
    .await
    .map_err(|error| anyhow!("{error}\n  {error:?}"))?;
    println!(
        "profile {} (page {}), DPI {}: {} -> {}; written, verified",
        args.profile,
        write.page,
        args.mode,
        DpiModeDisplay(write.before),
        DpiModeDisplay(write.after)
    );
    Ok(())
}

async fn set_rate(args: SetRateArgs) -> Result<()> {
    let interval = ReportInterval::try_from(args.interval_ms)
        .map_err(|_| anyhow!("the interval must be at least 1 ms"))?;
    let route = g700_route().await?;
    if let Some(profile) = args.profile {
        let write = openlogi_hid::set_g700_profile_report_interval(
            &route,
            ProfileRateChange { profile, interval },
        )
        .await
        .map_err(|error| anyhow!("{error}\n  {error:?}"))?;
        println!(
            "profile {profile} (page {}), report interval: {} ms -> {} ms = {} Hz; written, verified",
            write.page,
            write.before,
            write.after.millis(),
            write.after.rate_hz()
        );
        return Ok(());
    }
    let actual = openlogi_hid::set_g700_report_interval(&route, interval)
        .await
        .map_err(|error| anyhow!("{error}\n  {error:?}"))?;
    println!(
        "report rate (0x64): {} ms = {} Hz, read back",
        actual.millis(),
        actual.rate_hz()
    );
    Ok(())
}

async fn set_resolution(args: SetResolutionArgs) -> Result<()> {
    let requested = Resolution {
        x: args.raw,
        y: args.raw_y.unwrap_or(args.raw),
    };
    let route = g700_route().await?;
    let actual = openlogi_hid::set_g700_resolution(&route, requested)
        .await
        .map_err(|error| anyhow!("{error}\n  {error:?}"))?;
    println!(
        "current resolution (0x63): raw {}x{} = {}x{} dpi, read back",
        actual.x,
        actual.y,
        RawDpi(actual.x),
        RawDpi(actual.y)
    );
    Ok(())
}

async fn set_profile(args: SetProfileArgs) -> Result<()> {
    let requested = match args.index {
        Some(index) => CurrentProfile::Index(index),
        None => CurrentProfile::Factory,
    };
    let route = g700_route().await?;
    let actual = openlogi_hid::set_g700_current_profile(&route, requested)
        .await
        .map_err(|error| anyhow!("{error}\n  {error:?}"))?;
    println!("current profile (0x0F): {actual:?}, read back");
    Ok(())
}

/// One page entry of a backup file.
#[derive(Debug, Deserialize)]
struct BackupPage {
    page: u8,
    /// The page as hex; absent for a page that did not read.
    bytes: Option<String>,
    /// Whether the dump found the CRC valid.
    crc_valid: Option<bool>,
}

/// The `identity` block of a version 2 backup (see [`G700Identity`]).
#[derive(Debug, Deserialize)]
struct BackupIdentity {
    route: String,
    device_index: u8,
    readable_pages: usize,
    directory: Vec<(u8, u8)>,
}

/// The parts of a backup file a restore reads.
#[derive(Debug, Deserialize)]
struct BackupFile {
    format: String,
    version: u64,
    route: String,
    device_index: u8,
    identity: Option<BackupIdentity>,
    pages: Vec<BackupPage>,
}

/// A backup, checked: its pages and the mouse it was taken from.
#[derive(Debug, PartialEq)]
struct Backup {
    identity: G700Identity,
    pages: Vec<(u8, Page)>,
}

/// The backup at `path`. The file is checked against itself first: every
/// page must decode to a full page whose CRC verdict matches the one the
/// dump recorded, so a damaged file is refused before anything reaches the
/// mouse, and a version 2 identity must agree with the pages it describes.
fn read_backup(path: &Path) -> Result<Backup> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read the backup {}", path.display()))?;
    parse_backup(&text).with_context(|| format!("the backup {}", path.display()))
}

fn parse_backup(text: &str) -> Result<Backup> {
    let file: BackupFile = serde_json::from_str(text)?;
    if file.format != BACKUP_FORMAT || !(1..=BACKUP_VERSION).contains(&file.version) {
        bail!(
            "format {:?} version {} is not a {BACKUP_FORMAT} backup of version 1 to {BACKUP_VERSION}",
            file.format,
            file.version
        );
    }
    let mut pages = Vec::new();
    for entry in file.pages {
        let Some(hex_text) = entry.bytes else {
            continue;
        };
        let bytes = unhex(&hex_text)
            .and_then(|bytes| <[u8; PAGE_SIZE]>::try_from(bytes).ok())
            .ok_or_else(|| anyhow!("page {}: not {PAGE_SIZE} bytes of hex", entry.page))?;
        let page = Page::from_bytes(bytes);
        if entry.crc_valid != Some(page.has_valid_crc()) {
            bail!(
                "page {}: the recorded CRC verdict {:?} does not match the bytes",
                entry.page,
                entry.crc_valid
            );
        }
        if pages.iter().any(|(number, _)| *number == entry.page) {
            bail!("page {} appears twice", entry.page);
        }
        pages.push((entry.page, page));
    }
    let identity = G700Identity::of_pages(
        file.route,
        file.device_index,
        pages.iter().map(|(number, page)| (*number, page)),
    );
    if let Some(recorded) = file.identity {
        let recorded = G700Identity {
            route: recorded.route,
            device_index: recorded.device_index,
            readable_pages: recorded.readable_pages,
            directory: recorded.directory,
        };
        if recorded != identity {
            bail!("the identity {recorded:?} contradicts the pages, which say {identity:?}");
        }
    }
    Ok(Backup { identity, pages })
}

/// Decodes [`hex`]'s output.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The page `number` of the first full read, if it read.
fn page(dump: &G700MemoryDump, number: u8) -> Option<&Page> {
    dump.pages
        .iter()
        .find(|read| read.page == number)
        .and_then(|read| read.result.as_ref().ok())
}

/// The human-readable report of a dump.
struct Summary<'a>(&'a G700MemoryDump);

impl fmt::Display for Summary<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dump = self.0;
        writeln!(f, "  device index: {}", dump.device_index)?;
        match &dump.battery {
            Ok(reading) => writeln!(
                f,
                "  battery (0x07): {:?}, {:?}",
                reading.level, reading.charging
            )?,
            Err(error) => writeln!(f, "  battery (0x07): read failed: {error:?}")?,
        }
        match &dump.resolution {
            Ok(resolution) => writeln!(
                f,
                "  current resolution (0x63): raw {}x{} = {}x{} dpi",
                resolution.x,
                resolution.y,
                RawDpi(resolution.x),
                RawDpi(resolution.y)
            )?,
            Err(error) => writeln!(f, "  current resolution (0x63): read failed: {error:?}")?,
        }
        match &dump.report_interval {
            Ok(interval) => writeln!(
                f,
                "  report rate (0x64): {} ms = {} Hz",
                interval.millis(),
                interval.rate_hz()
            )?,
            Err(error) => writeln!(f, "  report rate (0x64): read failed: {error:?}")?,
        }

        let Some(directory_page) = page(dump, DIRECTORY_PAGE) else {
            writeln!(f, "  profile directory (page {DIRECTORY_PAGE}): not read")?;
            return Ok(());
        };
        let directory = codec::parse_directory(directory_page);
        let active = active_index(&dump.current_profile, &directory);
        match &dump.current_profile {
            Ok(current) => writeln!(f, "  current profile (0x0F): {current:?}")?,
            Err(error) => writeln!(f, "  current profile (0x0F): read failed: {error:?}")?,
        }
        writeln!(
            f,
            "  profile directory (page {DIRECTORY_PAGE}, CRC {}): {} of {} profiles enabled",
            CrcDisplay(directory_page.has_valid_crc()),
            directory.entries().len(),
            codec::PROFILE_COUNT
        )?;

        for (index, entry) in directory.entries().iter().enumerate() {
            let marker = if active == Some(index) {
                "  [active]"
            } else {
                ""
            };
            let Some(profile_page) = page(dump, entry.page) else {
                writeln!(f, "  profile {index}: page {} not read{marker}", entry.page)?;
                continue;
            };
            let profile = G700Profile::parse(profile_page);
            writeln!(
                f,
                "  profile {index}: page {}, offset {}, LED mask {:#04x}, CRC {}{marker}",
                entry.page,
                entry.offset,
                entry.led_mask,
                CrcDisplay(profile_page.has_valid_crc())
            )?;
            write!(f, "{}", ProfileDisplay(&profile))?;
        }

        write!(f, "{}", MemoryDisplay(dump))
    }
}

/// The decoded fields of one profile, below its header line.
struct ProfileDisplay<'a>(&'a G700Profile);

impl fmt::Display for ProfileDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.name() {
            Some(name) => writeln!(f, "    name: {name:?}")?,
            None => writeln!(f, "    name: (no LGS02 metadata)")?,
        }
        match self.0.report_interval() {
            Ok(interval) => writeln!(
                f,
                "    report rate: {} Hz ({} ms)",
                interval.rate_hz(),
                interval.millis()
            )?,
            Err(_) => writeln!(
                f,
                "    report rate: invalid interval byte {}",
                self.0.report_interval_millis
            )?,
        }
        writeln!(f, "    default DPI mode: {}", self.0.default_dpi_mode)?;
        for (mode, dpi) in self.0.dpi_modes.iter().enumerate() {
            writeln!(f, "    DPI {mode}: {}", DpiModeDisplay(*dpi))?;
        }
        for (button, binding) in self.0.bindings.iter().enumerate() {
            writeln!(f, "    button {button:>2}: {}", BindingDisplay(*binding))?;
        }
        Ok(())
    }
}

/// The page-read totals and the re-read verdict of a dump.
struct MemoryDisplay<'a>(&'a G700MemoryDump);

impl fmt::Display for MemoryDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dump = self.0;
        let failed: Vec<u8> = dump
            .pages
            .iter()
            .filter(|read| read.result.is_err())
            .map(|read| read.page)
            .collect();
        let valid = dump
            .pages
            .iter()
            .filter(|read| read.result.as_ref().is_ok_and(Page::has_valid_crc))
            .count();
        writeln!(
            f,
            "  memory: {} pages read, {valid} with a valid CRC, failed: {}",
            dump.pages.len() - failed.len(),
            PageList(&failed)
        )?;
        let unstable: Vec<u8> = dump
            .rereads
            .iter()
            .filter(|reread| !reread_matches(dump, reread))
            .map(|reread| reread.page)
            .collect();
        writeln!(
            f,
            "  re-read of pages {}: {}",
            PageList(
                &dump
                    .rereads
                    .iter()
                    .map(|read| read.page)
                    .collect::<Vec<_>>()
            ),
            if unstable.is_empty() {
                "identical".to_owned()
            } else {
                format!("DIFFERENT on {}", PageList(&unstable))
            }
        )
    }
}

/// Whether a second read of a page returned the bytes of the first.
fn reread_matches(dump: &G700MemoryDump, reread: &PageRead) -> bool {
    match (page(dump, reread.page), &reread.result) {
        (Some(first), Ok(second)) => first == second,
        _ => false,
    }
}

/// The directory index of the profile register `0x0F` names.
fn active_index(
    current: &Result<CurrentProfile, impl fmt::Debug>,
    directory: &ProfileDirectory,
) -> Option<usize> {
    match current.as_ref().ok()? {
        CurrentProfile::Index(index) => Some(usize::from(*index)),
        CurrentProfile::Address { page, offset } => directory
            .entries()
            .iter()
            .position(|entry| entry.page == *page && entry.offset == *offset),
        CurrentProfile::Factory => None,
    }
}

/// A raw DPI step as DPI, or `-` for a step without a table entry.
struct RawDpi(u16);

impl fmt::Display for RawDpi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match u8::try_from(self.0).ok().and_then(codec::dpi_from_raw) {
            Some(dpi) => write!(f, "{dpi}"),
            None => write!(f, "-"),
        }
    }
}

struct DpiModeDisplay(DpiMode);

impl fmt::Display for DpiModeDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = self.0;
        match (mode.x_dpi(), mode.y_dpi()) {
            (Some(x), Some(y)) => write!(f, "{x}x{y} dpi")?,
            _ => write!(f, "disabled")?,
        }
        write!(f, " (raw {:#04x}/{:#04x}, LEDs ", mode.x, mode.y)?;
        for led in 1..=4 {
            let symbol = match mode.led(led) {
                Some(DpiLed::On) => '*',
                Some(DpiLed::Off) => '.',
                _ => '?',
            };
            write!(f, "{symbol}")?;
        }
        write!(f, ")")
    }
}

/// A binding in ratbagctl's words where it has them, so the two compare.
struct BindingDisplay(Binding);

impl fmt::Display for BindingDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Binding::MouseButtons(mask) => {
                let buttons: Vec<String> = mask.buttons().map(|n| n.to_string()).collect();
                if buttons.is_empty() {
                    write!(f, "no button")
                } else {
                    write!(f, "button {}", buttons.join("+"))
                }
            }
            Binding::Key { modifiers, usage } => {
                write!(f, "key usage {usage:#04x}")?;
                if !modifiers.is_empty() {
                    write!(f, " with {modifiers:?}")?;
                }
                Ok(())
            }
            Binding::Special(special) => write!(f, "{}", special_name(special)),
            Binding::Consumer(usage) => write!(f, "consumer usage {usage:#06x}"),
            Binding::Disabled => write!(f, "disabled"),
            Binding::Macro(pointer) => write!(
                f,
                "macro at page {} word {:#04x}",
                pointer.page, pointer.word_offset
            ),
            Binding::Unknown([a, b, c]) => write!(f, "unknown {a:02x} {b:02x} {c:02x}"),
        }
    }
}

fn special_name(special: Special) -> String {
    match special {
        Special::WheelLeft => "wheel-left".into(),
        Special::WheelRight => "wheel-right".into(),
        Special::BatteryLevel => "battery-level".into(),
        Special::ResolutionUp => "resolution-up".into(),
        Special::ResolutionCycleUp => "resolution-cycle-up".into(),
        Special::ResolutionDown => "resolution-down".into(),
        Special::ResolutionCycleDown => "resolution-cycle-down".into(),
        Special::ProfileUp => "profile-up".into(),
        Special::ProfileCycleUp => "profile-cycle-up".into(),
        Special::ProfileDown => "profile-down".into(),
        Special::ProfileCycleDown => "profile-cycle-down".into(),
        Special::Unknown(code) => format!("special {code:#06x}"),
    }
}

struct CrcDisplay(bool);

impl fmt::Display for CrcDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0 { "ok" } else { "BAD" })
    }
}

struct PageList<'a>(&'a [u8]);

impl fmt::Display for PageList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        let pages: Vec<String> = self.0.iter().map(u8::to_string).collect();
        f.write_str(&pages.join(", "))
    }
}

/// The backup file: every page as hex with its CRC check, plus the registers.
fn backup_json(dump: &G700MemoryDump, created_unix: u64) -> Value {
    fn error_or<T>(result: &Result<T, impl fmt::Debug>, ok: impl FnOnce(&T) -> Value) -> Value {
        match result {
            Ok(value) => ok(value),
            Err(error) => json!({ "error": format!("{error:?}") }),
        }
    }
    let page_json = |read: &PageRead| match &read.result {
        Ok(page) => json!({
            "page": read.page,
            "crc_valid": page.has_valid_crc(),
            "stored_crc": format!("{:04x}", page.stored_crc()),
            "computed_crc": format!("{:04x}", page.computed_crc()),
            "bytes": hex(page.as_bytes()),
        }),
        Err(error) => json!({ "page": read.page, "error": format!("{error:?}") }),
    };
    let identity = dump.identity();
    json!({
        "format": BACKUP_FORMAT,
        "version": BACKUP_VERSION,
        "created_unix": created_unix,
        "route": dump.route.to_string(),
        "device_index": dump.device_index,
        "identity": {
            "route": identity.route,
            "device_index": identity.device_index,
            "readable_pages": identity.readable_pages,
            "directory": identity.directory,
        },
        "registers": {
            "current_profile_0x0f": error_or(&dump.current_profile, |current| {
                json!({ "decoded": format!("{current:?}"), "encoded": hex(&current.to_register()) })
            }),
            "current_resolution_0x63": error_or(&dump.resolution, |resolution| {
                json!({ "x": resolution.x, "y": resolution.y })
            }),
            "report_interval_0x64": error_or(&dump.report_interval, |interval| {
                json!({ "millis": interval.millis(), "rate_hz": interval.rate_hz() })
            }),
            "battery_0x07": error_or(&dump.battery, |reading| {
                json!({ "decoded": format!("{reading:?}") })
            }),
        },
        "pages": dump.pages.iter().map(page_json).collect::<Vec<_>>(),
        "rereads": dump.rereads.iter().map(|reread| json!({
            "page": reread.page,
            "matches_first_read": reread_matches(dump, reread),
        })).collect::<Vec<_>>(),
    })
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use openlogi_hid::g700::{Binding, ButtonMask, KeyModifiers, MacroPointer, Page, Special};

    use openlogi_hid::G700Identity;

    use super::{Backup, BindingDisplay, hex, parse_backup, select_pages, unhex};

    #[test]
    fn bindings_print_in_ratbagctl_words() {
        for (binding, text) in [
            (Binding::MouseButtons(ButtonMask(0x0001)), "button 1"),
            (Binding::MouseButtons(ButtonMask(0x0400)), "button 11"),
            (Binding::MouseButtons(ButtonMask(0x0003)), "button 1+2"),
            (Binding::Special(Special::BatteryLevel), "battery-level"),
            (
                Binding::Special(Special::ProfileCycleUp),
                "profile-cycle-up",
            ),
            (Binding::Special(Special::Unknown(0x0006)), "special 0x0006"),
            (
                Binding::Key {
                    modifiers: KeyModifiers::LEFT_CTRL,
                    usage: 0x04,
                },
                "key usage 0x04 with KeyModifiers(LEFT_CTRL)",
            ),
            (Binding::Consumer(0x00e9), "consumer usage 0x00e9"),
            (Binding::Disabled, "disabled"),
            (
                Binding::Macro(MacroPointer {
                    page: 5,
                    word_offset: 0x10,
                }),
                "macro at page 5 word 0x10",
            ),
            (Binding::Unknown([0x80, 0x01, 0x02]), "unknown 80 01 02"),
        ] {
            assert_eq!(BindingDisplay(binding).to_string(), text);
        }
    }

    #[test]
    fn hex_is_lowercase_and_unseparated() {
        assert_eq!(hex(&[0x00, 0xab, 0x10]), "00ab10");
        assert_eq!(hex(&[]), "");
    }

    fn backup_text(pages: &[(u8, &Page)]) -> String {
        let pages: Vec<_> = pages
            .iter()
            .map(|(number, page)| {
                serde_json::json!({
                    "page": number,
                    "crc_valid": page.has_valid_crc(),
                    "bytes": hex(page.as_bytes()),
                })
            })
            .collect();
        serde_json::json!({
            "format": "openlogi-g700-memory-dump",
            "version": 1,
            "route": "slot 1 on receiver 500F9BB6",
            "device_index": 1,
            "pages": pages,
        })
        .to_string()
    }

    fn identity(readable_pages: usize, directory: Vec<(u8, u8)>) -> G700Identity {
        G700Identity {
            route: "slot 1 on receiver 500F9BB6".into(),
            device_index: 1,
            readable_pages,
            directory,
        }
    }

    /// A directory listing profiles on pages 2 and 3.
    fn directory() -> Page {
        let mut bytes = [0xff; 512];
        bytes[..6].copy_from_slice(&[0x02, 0x00, 0x01, 0x03, 0x00, 0x03]);
        let mut page = Page::from_bytes(bytes);
        page.update_crc();
        page
    }

    fn valid_page(fill: u8) -> Page {
        let mut page = Page::from_bytes([fill; 512]);
        page.update_crc();
        page
    }

    #[test]
    fn unhex_reverses_hex() {
        assert_eq!(unhex("00ab10"), Some(vec![0x00, 0xab, 0x10]));
        assert_eq!(unhex(""), Some(vec![]));
        assert_eq!(unhex("abc"), None, "odd length");
        assert_eq!(unhex("zz"), None);
    }

    #[test]
    fn a_backup_reads_back_the_pages_a_dump_wrote() {
        let text = backup_text(&[(1, &directory()), (8, &Page::erased())]);

        let backup = parse_backup(&text).unwrap();

        assert_eq!(
            backup,
            Backup {
                identity: identity(2, vec![(2, 0), (3, 0)]),
                pages: vec![(1, directory()), (8, Page::erased())],
            },
            "a version 1 backup's identity comes from its route and pages"
        );
    }

    #[test]
    fn a_version_2_identity_must_agree_with_its_pages() {
        let v2 = |readable_pages: usize| {
            let mut file: serde_json::Value =
                serde_json::from_str(&backup_text(&[(1, &directory())])).unwrap();
            file["version"] = 2.into();
            file["identity"] = serde_json::json!({
                "route": "slot 1 on receiver 500F9BB6",
                "device_index": 1,
                "readable_pages": readable_pages,
                "directory": [[2, 0], [3, 0]],
            });
            file.to_string()
        };

        assert_eq!(
            parse_backup(&v2(1)).unwrap().identity,
            identity(1, vec![(2, 0), (3, 0)])
        );
        let error = parse_backup(&v2(27)).unwrap_err();
        assert!(error.to_string().contains("contradicts"), "{error}");
    }

    #[test]
    fn a_page_that_did_not_read_is_left_out() {
        let text = serde_json::json!({
            "format": "openlogi-g700-memory-dump",
            "version": 1,
            "route": "slot 1 on receiver 500F9BB6",
            "device_index": 1,
            "pages": [{ "page": 27, "error": "RegisterAccess(InvalidParamValue)" }],
        })
        .to_string();

        let backup = parse_backup(&text).unwrap();
        assert_eq!(backup.pages, []);
        assert_eq!(backup.identity.readable_pages, 0);
    }

    #[test]
    fn a_backup_whose_bytes_contradict_its_crc_verdict_is_refused() {
        let mut bytes = *valid_page(0x02).as_bytes();
        bytes[3] ^= 0x01;
        let damaged = Page::from_bytes(bytes);
        let text =
            backup_text(&[(2, &damaged)]).replace("\"crc_valid\":false", "\"crc_valid\":true");

        let error = parse_backup(&text).unwrap_err();

        assert!(error.to_string().contains("page 2"), "{error}");
    }

    #[test]
    fn another_format_is_refused() {
        let text = backup_text(&[]).replace("openlogi-g700-memory-dump", "something-else");

        parse_backup(&text).unwrap_err();
    }

    #[test]
    fn the_default_selection_is_every_page_but_ram() {
        let backup = vec![(0, valid_page(0)), (1, valid_page(1)), (8, Page::erased())];

        let selected: Vec<u8> = select_pages(backup.clone(), &[])
            .unwrap()
            .into_iter()
            .map(|(page, _)| page)
            .collect();
        assert_eq!(selected, [1, 8]);

        let ram = select_pages(backup.clone(), &[0]).unwrap();
        assert_eq!(
            ram,
            [(0, valid_page(0))],
            "an explicit RAM page is the device layer's to refuse"
        );
        select_pages(backup.clone(), &[9]).unwrap_err();
        let one = select_pages(backup, &[8, 8]).unwrap();
        assert_eq!(one, [(8, Page::erased())], "a repeated page once");
    }
}
