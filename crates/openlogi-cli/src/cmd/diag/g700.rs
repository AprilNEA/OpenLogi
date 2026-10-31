//! `openlogi diag g700` — Logitech G700 onboard-profile diagnostics.
//!
//! `dump` reads the G700's settings registers and all of its onboard memory,
//! prints the profiles it finds, and can save every page to a JSON backup.
//! It only reads: no request it sends can change the mouse.

use std::fmt;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use atomic_write_file::AtomicWriteFile;
use clap::{Args, Subcommand};
use openlogi_hid::hidpp10_register::current_profile::CurrentProfile;
use openlogi_hid::hidpp10_register::onboard_memory::Page;
use openlogi_hid::hidpp10_register::onboard_memory::directory::{DIRECTORY_PAGE, ProfileDirectory};
use openlogi_hid::hidpp10_register::onboard_memory::g700::{
    self, Binding, DpiLed, DpiMode, G700Profile, Special,
};
use openlogi_hid::{G700MemoryDump, PageRead};
use serde_json::{Value, json};

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
}

#[derive(Debug, Args)]
pub struct DumpArgs {
    /// Also save every page, with its CRC check, to this JSON file.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

pub async fn run(args: G700Args) -> Result<()> {
    match args.command {
        G700Cmd::Dump(args) => dump(args).await,
    }
}

async fn dump(args: DumpArgs) -> Result<()> {
    let dump = openlogi_hid::read_g700_memory()
        .await
        .map_err(|error| anyhow!("{error}: is the G700 paired and awake, or on its cable?"))?;
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
        let directory = g700::parse_directory(directory_page);
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
            g700::PROFILE_COUNT
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
        match u8::try_from(self.0).ok().and_then(g700::dpi_from_raw) {
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
    json!({
        "format": "openlogi-g700-memory-dump",
        "version": 1,
        "created_unix": created_unix,
        "route": dump.route.to_string(),
        "device_index": dump.device_index,
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
    use openlogi_hid::hidpp10_register::onboard_memory::g700::{
        Binding, ButtonMask, KeyModifiers, MacroPointer, Special,
    };

    use super::{BindingDisplay, hex};

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
}
