//! `openlogi mic` — read and set a USB microphone's controls.
//!
//! Exercises the `openlogi-audio` path the agent uses, straight against the
//! host audio stack, so it works with no agent running. A change lands in the
//! device's feature unit, so the system's sound settings and every other app
//! see it too.

use std::fmt::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use openlogi_audio::{GainStage, Microphone, MicrophoneChange, MilliDecibels};

#[derive(Debug, Args)]
pub struct MicArgs {
    #[command(subcommand)]
    pub cmd: Option<MicCmd>,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// The microphone to use, by the id `openlogi mic` prints (default: the
    /// only one connected).
    #[arg(long, global = true)]
    pub id: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum MicCmd {
    /// Show every connected microphone and its controls (the default action).
    Get,
    /// Change one control, then show the microphone as it reads back.
    Set {
        control: MicControl,
        /// Decibels for a level (`-6`, `12.5`), or on/off for a switch.
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
}

/// A control `openlogi mic set` can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MicControl {
    Gain,
    Mute,
    HeadphoneVolume,
    HeadphoneMute,
    Monitoring,
    MonitoringLevel,
}

pub fn run(args: MicArgs) -> Result<()> {
    let microphones = openlogi_audio::microphones().context("reading microphones")?;
    match args.cmd.unwrap_or(MicCmd::Get) {
        MicCmd::Get => {
            let shown: Vec<Microphone> = match &args.id {
                Some(id) => vec![target(&microphones, Some(id))?.clone()],
                None => microphones,
            };
            print(&shown, args.json)
        }
        MicCmd::Set { control, value } => {
            let id = target(&microphones, args.id.as_deref())?.id.clone();
            let change = change_for(control, &value)?;
            let microphone = openlogi_audio::apply(&id, change)
                .with_context(|| format!("setting {control:?} on {id}"))?;
            print(&[microphone], args.json)
        }
    }
}

fn print(microphones: &[Microphone], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(microphones)?);
    } else if microphones.is_empty() {
        println!("no supported microphone is connected");
    } else {
        for microphone in microphones {
            print!("{}", describe(microphone));
        }
    }
    Ok(())
}

/// The microphone `id` names, or the only one connected.
fn target<'a>(microphones: &'a [Microphone], id: Option<&str>) -> Result<&'a Microphone> {
    if let Some(id) = id {
        return microphones
            .iter()
            .find(|microphone| microphone.id == id)
            .ok_or_else(|| anyhow!("no connected microphone has the id {id:?}"));
    }
    match microphones {
        [] => bail!("no supported microphone is connected"),
        [only] => Ok(only),
        several => {
            let ids: Vec<&str> = several.iter().map(|m| m.id.as_str()).collect();
            bail!(
                "several microphones are connected; pick one with --id: {}",
                ids.join(", ")
            )
        }
    }
}

fn change_for(control: MicControl, value: &str) -> Result<MicrophoneChange> {
    Ok(match control {
        MicControl::Gain => MicrophoneChange::Gain(level(value)?),
        MicControl::Mute => MicrophoneChange::Muted(switch(value)?),
        MicControl::HeadphoneVolume => MicrophoneChange::HeadphoneVolume(level(value)?),
        MicControl::HeadphoneMute => MicrophoneChange::HeadphonesMuted(switch(value)?),
        MicControl::Monitoring => MicrophoneChange::Monitoring(switch(value)?),
        MicControl::MonitoringLevel => MicrophoneChange::MonitoringLevel(level(value)?),
    })
}

fn level(value: &str) -> Result<MilliDecibels> {
    let decibels: f64 = value
        .trim()
        .trim_end_matches("dB")
        .trim()
        .parse()
        .map_err(|_| anyhow!("{value:?} is not a level in decibels"))?;
    MilliDecibels::from_decibels(decibels).ok_or_else(|| anyhow!("{value:?} is out of range"))
}

fn switch(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Ok(true),
        "off" | "false" | "0" | "no" => Ok(false),
        _ => bail!("{value:?} is not on or off"),
    }
}

fn describe(microphone: &Microphone) -> String {
    let controls = &microphone.controls;
    let mut text = format!(
        "{}  ({:04x}:{:04x})\n",
        microphone.name, microphone.vendor_id, microphone.product_id
    );
    let rows = [
        ("id", microphone.id.clone()),
        ("mic gain", stage(controls.gain.as_ref())),
        ("muted", yes_no(controls.muted, "yes", "no")),
        (
            "headphone volume",
            stage(controls.headphone_volume.as_ref()),
        ),
        (
            "headphones muted",
            yes_no(controls.headphones_muted, "yes", "no"),
        ),
        ("monitoring", yes_no(controls.monitoring, "on", "off")),
        (
            "monitoring level",
            stage(controls.monitoring_level.as_ref()),
        ),
    ];
    for (label, value) in rows {
        let _ = writeln!(text, "  {label:<17} {value}");
    }
    text
}

fn stage(stage: Option<&GainStage>) -> String {
    stage.map_or_else(
        || "—".to_owned(),
        |stage| {
            format!(
                "{:+.1} dB  ({:+.1} … {:+.1} dB)",
                stage.level.decibels(),
                stage.min.decibels(),
                stage.max.decibels()
            )
        },
    )
}

fn yes_no(value: Option<bool>, on: &str, off: &str) -> String {
    match value {
        Some(true) => on.to_owned(),
        Some(false) => off.to_owned(),
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use openlogi_audio::{
        GainStage, Microphone, MicrophoneChange, MicrophoneControls, MilliDecibels,
    };

    use super::{MicControl, change_for, describe, level, stage, switch, target};

    fn microphone(id: &str) -> Microphone {
        Microphone {
            id: id.into(),
            vendor_id: 0x046d,
            product_id: 0x0ab7,
            name: "Yeti".into(),
            registry_model_id: Some("0ab7".into()),
            controls: MicrophoneControls::default(),
        }
    }

    #[test]
    fn switch_values_parse_either_way() {
        for on in ["on", "ON", "true", "1", "yes", " on "] {
            assert!(switch(on).unwrap(), "{on}");
        }
        for off in ["off", "False", "0", "no"] {
            assert!(!switch(off).unwrap(), "{off}");
        }
        for bad in ["", "maybe", "2"] {
            switch(bad).expect_err(bad);
        }
    }

    #[test]
    fn levels_parse_as_decibels() {
        assert_eq!(level("-6").unwrap(), MilliDecibels::new(-6_000));
        assert_eq!(level("12.5 dB").unwrap(), MilliDecibels::new(12_500));
        for bad in ["", "loud", "NaN", "inf", "1e9"] {
            level(bad).expect_err(bad);
        }
    }

    #[test]
    fn every_control_builds_its_change() {
        let db = MilliDecibels::new(3_000);
        let cases = [
            (MicControl::Gain, "3", MicrophoneChange::Gain(db)),
            (MicControl::Mute, "on", MicrophoneChange::Muted(true)),
            (
                MicControl::HeadphoneVolume,
                "3",
                MicrophoneChange::HeadphoneVolume(db),
            ),
            (
                MicControl::HeadphoneMute,
                "off",
                MicrophoneChange::HeadphonesMuted(false),
            ),
            (
                MicControl::Monitoring,
                "on",
                MicrophoneChange::Monitoring(true),
            ),
            (
                MicControl::MonitoringLevel,
                "3",
                MicrophoneChange::MonitoringLevel(db),
            ),
        ];
        for (control, value, change) in cases {
            assert_eq!(change_for(control, value).unwrap(), change, "{control:?}");
        }
        change_for(MicControl::Mute, "3").unwrap_err();
        change_for(MicControl::Gain, "on").unwrap_err();
    }

    #[test]
    fn the_target_is_the_named_or_only_microphone() {
        let one = [microphone("a")];
        let two = [microphone("a"), microphone("b")];

        target(&[], None).unwrap_err();
        assert_eq!(target(&one, None).unwrap().id, "a");
        target(&two, None).unwrap_err();
        assert_eq!(target(&two, Some("b")).unwrap().id, "b");
        target(&two, Some("c")).unwrap_err();
    }

    #[test]
    fn stages_and_absent_controls_format() {
        let gain = GainStage {
            level: MilliDecibels::new(12_000),
            min: MilliDecibels::new(-12_000),
            max: MilliDecibels::new(21_000),
        };

        assert_eq!(stage(Some(&gain)), "+12.0 dB  (-12.0 … +21.0 dB)");
        assert_eq!(stage(None), "—");
        let text = describe(&microphone("a"));
        assert!(text.starts_with("Yeti  (046d:0ab7)\n"), "{text}");
        assert!(text.contains("  monitoring        —\n"), "{text}");
    }
}
