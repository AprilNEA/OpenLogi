use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum MonitorCmd {
    /// List DDC/CI physical monitors and reported input values.
    List,
    /// Set one monitor's VCP 0x60 input value.
    Set(SetArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Monitor id printed by `openlogi monitor list`.
    #[arg(long)]
    monitor: String,
    /// Input value, decimal or hex such as 0x11.
    #[arg(long, value_parser = parse_input)]
    input: u32,
}

impl MonitorCmd {
    pub fn run(self) -> Result<()> {
        match self {
            Self::List => list(),
            Self::Set(args) => set(&args),
        }
    }
}

fn list() -> Result<()> {
    let monitors = openlogi_monitor::list_monitors()?;
    if monitors.is_empty() {
        println!("No DDC/CI monitors were discovered.");
        return Ok(());
    }
    for monitor in monitors {
        println!(
            "{}\n  name: {}\n  display: {}\n  description: {}\n  current: {}",
            monitor.id,
            monitor.friendly_name,
            monitor.display_name,
            monitor.description,
            monitor
                .current_input
                .map_or_else(|| "unknown".to_string(), format_input)
        );
        if monitor.inputs.is_empty() {
            println!("  inputs: unknown");
        } else {
            println!("  inputs:");
            for input in monitor.inputs {
                println!("    {}  {}", format_input(input.value), input.label);
            }
        }
    }
    Ok(())
}

fn set(args: &SetArgs) -> Result<()> {
    openlogi_monitor::set_monitor_input(&args.monitor, args.input)?;
    println!("Set {} to {}", args.monitor, format_input(args.input));
    Ok(())
}

fn parse_input(value: &str) -> Result<u32, String> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        trimmed.parse::<u32>().map_err(|error| error.to_string())
    }
}

fn format_input(value: u32) -> String {
    format!("0x{value:02x}")
}
