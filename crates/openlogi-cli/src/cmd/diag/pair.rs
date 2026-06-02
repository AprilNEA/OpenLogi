//! `openlogi diag pair` - run a real receiver pairing session.

use anyhow::{Result, bail};
use clap::Args;
use openlogi_hid::{
    DiscoveredDevice, PairingCommand, PairingError, PairingEvent, PairingReceiver, PasskeyMethod,
    ReceiverFamily, ReceiverSelector,
};
use tokio::sync::mpsc;

#[derive(Debug, Args)]
pub struct PairArgs {
    /// Only list pairing-capable receivers and exit.
    #[arg(long)]
    pub list_receivers: bool,

    /// Target a specific Bolt receiver unique id.
    #[arg(long)]
    pub bolt_uid: Option<String>,

    /// Stop after discovery; do not automatically pair the first found device.
    #[arg(long)]
    pub discover_only: bool,

    /// Number of discovery attempts before giving up. Use 0 to keep trying.
    #[arg(long, default_value_t = 1)]
    pub attempts: u32,

    /// Seconds to wait between retry attempts.
    #[arg(long, default_value_t = 2)]
    pub retry_delay_secs: u64,
}

pub async fn run(args: PairArgs) -> Result<()> {
    let receivers = openlogi_hid::list_pairing_receivers().await?;
    if receivers.is_empty() {
        bail!("no supported pairing-capable receiver found");
    }

    println!("pairing receivers:");
    for (idx, receiver) in receivers.iter().enumerate() {
        println!("  {}. {}", idx + 1, format_receiver(receiver));
    }

    if args.list_receivers {
        return Ok(());
    }

    let target = args
        .bolt_uid
        .map_or(ReceiverSelector::First, ReceiverSelector::BoltUid);
    println!("put the Logitech device into pairing mode now");

    let mut attempt = 1;
    loop {
        let attempt_label = if args.attempts == 0 {
            format!("{attempt}")
        } else {
            format!("{attempt}/{}", args.attempts)
        };
        println!("starting pairing session: {target:?} (attempt {attempt_label})");
        match run_attempt(target.clone(), args.discover_only).await {
            Ok(true) => return Ok(()),
            Ok(false) => bail!("pairing session ended without a paired event"),
            Err(error) if should_retry(args.attempts, attempt, &error) => {
                println!(
                    "pairing attempt timed out; retrying in {}s",
                    args.retry_delay_secs
                );
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(args.retry_delay_secs)).await;
            }
            Err(error) => bail!("pairing session failed: {error}"),
        }
    }
}

async fn run_attempt(
    target: ReceiverSelector,
    discover_only: bool,
) -> std::result::Result<bool, PairingError> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel();
    let mut session = Box::pin(openlogi_hid::run_pairing(target, cmd_rx, evt_tx));
    let mut auto_pair_sent = false;
    let mut paired = false;

    loop {
        tokio::select! {
            result = &mut session => {
                match result {
                    Ok(()) => {
                        if paired {
                            println!("pairing session completed");
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                    Err(error) => return Err(error),
                }
            }
            event = evt_rx.recv() => {
                let Some(event) = event else {
                    return Err(PairingError::Hid("pairing event stream closed".into()));
                };
                match event {
                    PairingEvent::Searching => {
                        println!("receiver is searching");
                    }
                    PairingEvent::DeviceFound(device) => {
                        print_device_found(&device);
                        if !discover_only && !auto_pair_sent {
                            let name = device.name.clone();
                            cmd_tx
                                .send(PairingCommand::Pair(device))
                                .map_err(|e| PairingError::Hid(e.to_string()))?;
                            auto_pair_sent = true;
                            println!("pairing first discovered device: {name}");
                        }
                    }
                    PairingEvent::Passkey(method) => {
                        print_passkey(method);
                    }
                    PairingEvent::Paired { slot } => {
                        paired = true;
                        println!("paired to slot {slot}");
                    }
                    PairingEvent::WindowsSearching
                    | PairingEvent::WindowsDeviceFound(_)
                    | PairingEvent::WindowsPairing { .. }
                    | PairingEvent::WindowsPaired { .. } => {}
                    PairingEvent::Failed(error) => {
                        println!("pairing failed event: {error}");
                    }
                }
            }
        }
    }
}

fn should_retry(attempts: u32, attempt: u32, error: &PairingError) -> bool {
    matches!(error, PairingError::Timeout) && (attempts == 0 || attempt < attempts)
}

fn format_receiver(receiver: &PairingReceiver) -> String {
    let family = match receiver.family {
        ReceiverFamily::Bolt => "Bolt",
        ReceiverFamily::Unifying => "Unifying",
    };
    let uid = receiver.uid.as_deref().unwrap_or("-");
    format!(
        "{family} receiver pid={:04x} uid={uid}",
        receiver.product_id
    )
}

fn print_device_found(device: &DiscoveredDevice) {
    println!(
        "found: {} kind={:?} address={} auth={:#04x}",
        device.name,
        device.kind,
        format_address(device.address),
        device.authentication
    );
}

fn format_address(address: [u8; 6]) -> String {
    address
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn print_passkey(method: PasskeyMethod) {
    match method {
        PasskeyMethod::Keyboard(passkey) => {
            println!("type passkey on the new keyboard, then press Enter: {passkey}");
        }
        PasskeyMethod::Pointer { clicks, passkey } => {
            let sequence = clicks
                .into_iter()
                .map(|click| match click {
                    openlogi_hid::Click::Left => "L",
                    openlogi_hid::Click::Right => "R",
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!("mouse passkey {passkey}: click {sequence}, then press both buttons together");
        }
    }
}
