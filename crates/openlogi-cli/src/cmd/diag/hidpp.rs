//! `openlogi diag hidpp` — call one HID++ 2.0 feature with raw parameters and
//! print what it answers, to learn an undocumented feature's functions.
//!
//! Nothing here knows what a function does: a call can change the device as
//! easily as read it. Start with the functions a feature's family uses for
//! reads (function 0 is usually its capabilities).

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::{RAW_FEATURE_PARAMS, RawFeatureCall, RawFeatureReply};
use openlogi_ipc::client::ConnectError;

use crate::cmd::diag::select_device;

#[derive(Debug, Args)]
pub struct HidppArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of the first that offers the feature.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// The feature to call, by id (`0x1b05`).
    #[arg(long, value_name = "FEATURE_ID", value_parser = parse_feature_id)]
    pub feature: u16,

    /// One call: the function number (0–15), then up to sixteen hex
    /// parameter bytes (`"1 00 05"`); missing bytes are zero. Repeat for
    /// several calls in order. Without any, calls are asked for until `:q`.
    #[arg(long = "call", value_name = "FN [HEX]")]
    pub calls: Vec<String>,
}

pub async fn run(args: HidppArgs) -> Result<()> {
    // The agent holds the device's channel and may write to it at any time.
    if !matches!(
        crate::agent::connect().await,
        Err(ConnectError::Endpoint(_))
    ) {
        bail!("the OpenLogi agent is running; quit it, then run this again");
    }
    let (route, name) = select_device(args.device.as_deref(), &[args.feature]).await?;
    println!("device: {name} ({route})");

    if !args.calls.is_empty() {
        let calls = args
            .calls
            .iter()
            .map(|call| parse_call(call))
            .collect::<Result<Vec<_>>>()?;
        return call_and_print(&route, args.feature, &calls).await;
    }
    while let Some(line) = prompt("fn and params (`1 00 05`, :q to stop): ").await? {
        if line.trim() == ":q" {
            break;
        }
        match parse_call(&line) {
            Ok(call) => call_and_print(&route, args.feature, &[call]).await?,
            Err(error) => eprintln!("{error}"),
        }
    }
    Ok(())
}

async fn call_and_print(
    route: &openlogi_hid::DeviceRoute,
    feature: u16,
    calls: &[RawFeatureCall],
) -> Result<()> {
    let Some(answers) = openlogi_hid::raw_feature_calls(route, feature, calls)
        .await
        .with_context(|| format!("call feature {feature:#06x}"))?
    else {
        bail!("the device has no feature {feature:#06x}; run `diag features` for its list");
    };
    for (reply, call) in answers.replies.iter().zip(calls) {
        let answer = match reply {
            RawFeatureReply::Payload(payload) => hex(payload),
            RawFeatureReply::Error(error) => format!("error {error:?}"),
        };
        print_answer(feature, answers.index, call, &answer);
    }
    // The batch stopped at the call after the last answer.
    let Some(failure) = answers.failure else {
        return Ok(());
    };
    if let Some(call) = calls.get(answers.replies.len()) {
        print_answer(feature, answers.index, call, "no answer");
    }
    Err(failure).with_context(|| format!("call feature {feature:#06x}"))
}

fn print_answer(feature: u16, index: u8, call: &RawFeatureCall, answer: &str) {
    println!(
        "{feature:#06x}@{index} fn {:>2} {} -> {answer}",
        call.function(),
        hex(trimmed(call.params()))
    );
}

/// `params` without its trailing zero bytes, at least one byte, for a
/// shorter echo of what was sent.
fn trimmed(params: &[u8]) -> &[u8] {
    let len = params
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(1, |last| last + 1);
    &params[..len]
}

fn parse_feature_id(text: &str) -> Result<u16, String> {
    let digits = text.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(digits, 16).map_err(|error| format!("`{text}`: {error}"))
}

/// A function number, then hex parameter bytes with spaces anywhere.
fn parse_call(text: &str) -> Result<RawFeatureCall> {
    let text = text.trim();
    let (function, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let function: u8 = function
        .parse()
        .with_context(|| format!("`{function}` is not a function number"))?;
    let digits: String = rest.chars().filter(|c| !c.is_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        bail!("`{rest}`: an odd number of hex digits");
    }
    if digits.len() > RAW_FEATURE_PARAMS * 2 {
        bail!("`{rest}`: more than {RAW_FEATURE_PARAMS} bytes");
    }
    let mut params = [0; RAW_FEATURE_PARAMS];
    for (slot, pair) in params.iter_mut().zip(digits.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).context("hex digits")?;
        *slot = u8::from_str_radix(pair, 16).with_context(|| format!("`{pair}` is not hex"))?;
    }
    RawFeatureCall::new(function, params)
        .with_context(|| format!("function {function}: HID++ 2.0 functions run 0–15"))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Print `question` and read one line off the async runtime; `None` at the
/// end of input.
async fn prompt(question: &str) -> Result<Option<String>> {
    print!("{question}");
    std::io::stdout().flush()?;
    let line = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map(|read| (read > 0).then_some(line))
    })
    .await??;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_is_a_function_then_zero_padded_bytes() {
        let call = parse_call(" 1 00 0 5 ").expect("call parses");
        assert_eq!(call.function(), 1);
        assert_eq!(call.params()[..3], [0, 5, 0]);
        assert_eq!(
            parse_call("0").expect("bare function").params(),
            &[0; RAW_FEATURE_PARAMS]
        );
        parse_call("16").expect_err("function past 15");
        parse_call("1 abc").expect_err("odd digit count");
        parse_call("x").expect_err("not a number");
    }

    #[test]
    fn the_echo_drops_trailing_zeros_but_keeps_one_byte() {
        assert_eq!(trimmed(&[1, 0, 2, 0, 0]), [1, 0, 2]);
        assert_eq!(trimmed(&[0, 0]), [0]);
    }
}
