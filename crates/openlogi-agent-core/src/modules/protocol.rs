//! The module protocol on the wire: one JSON object per line, UTF-8, over
//! the module's stdin (host to module) and stdout (module to host). The
//! messages are `docs/MODULES.md`'s tables.

use std::io;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use openlogi_core::binding::GridSlot;
use openlogi_core::module::{ModuleId, PROTOCOL};
use openlogi_ipc::check_key_image_size;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _};

use super::ModuleKey;
use crate::watchers::key_display::KeyImage;

/// The longest line a module may write. A longer one is dropped whole.
pub(super) const MAX_LINE: usize = 8 * 1024 * 1024;

/// What one [`read_line`] call found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LineRead {
    /// A line, now in the buffer without its line ending.
    Line,
    /// A line longer than the limit, read past and dropped; the buffer is
    /// empty.
    TooLong,
    /// The stream ended.
    Eof,
}

/// Read one `\n`-terminated line from `reader` into `buf`, holding at most
/// `max` bytes of it: a longer line is consumed to its end and reported as
/// [`LineRead::TooLong`] without ever being held whole. A `\r` before the
/// `\n` is dropped, and a last line without a newline is still a line.
pub(super) async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<LineRead> {
    buf.clear();
    let mut too_long = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if too_long {
                LineRead::TooLong
            } else if buf.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }
        let (chunk, used, ended) = match available.iter().position(|&byte| byte == b'\n') {
            Some(end) => (&available[..end], end + 1, true),
            None => (available, available.len(), false),
        };
        if !too_long {
            if buf.len() + chunk.len() > max {
                too_long = true;
                buf.clear();
            } else {
                buf.extend_from_slice(chunk);
            }
        }
        reader.consume(used);
        if ended {
            if too_long {
                return Ok(LineRead::TooLong);
            }
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(LineRead::Line);
        }
    }
}

/// A message from the host to a module.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HostMessage<'a> {
    Hello {
        protocol: u32,
        host: &'static str,
        host_version: &'static str,
        module_id: &'a ModuleId,
        settings: serde_json::Value,
    },
    Keys {
        keys: Vec<WireKey<'a>>,
    },
    Pressed {
        device: &'a str,
        slot: String,
    },
    Shutdown,
}

#[derive(Serialize)]
struct WireKey<'a> {
    device: &'a str,
    slot: String,
    size: u16,
}

/// One message as the line the host writes, newline included.
fn line(message: &HostMessage<'_>) -> String {
    // Every field is a string, a number, or a JSON value already, so this
    // cannot fail; an empty object stands in if it somehow did.
    let mut text = serde_json::to_string(message).unwrap_or_else(|_| "{}".into());
    text.push('\n');
    text
}

/// The `hello` line: the first thing a module reads.
pub(super) fn hello(module_id: &ModuleId, settings: &toml::Table) -> String {
    line(&HostMessage::Hello {
        protocol: PROTOCOL,
        host: "openlogi",
        host_version: env!("CARGO_PKG_VERSION"),
        module_id,
        settings: json_table(settings),
    })
}

/// The `keys` line for the full set `keys`.
pub(super) fn keys(keys: &[ModuleKey]) -> String {
    line(&HostMessage::Keys {
        keys: keys
            .iter()
            .map(|key| WireKey {
                device: &key.device,
                slot: key.slot.config_name().into_owned(),
                size: key.size,
            })
            .collect(),
    })
}

/// The `pressed` line for one key.
pub(super) fn pressed(device: &str, slot: GridSlot) -> String {
    line(&HostMessage::Pressed {
        device,
        slot: slot.config_name().into_owned(),
    })
}

/// The `shutdown` line.
pub(super) fn shutdown() -> String {
    line(&HostMessage::Shutdown)
}

/// `settings` as the JSON object a module reads: TOML's dates and times
/// become their TOML text, the one TOML value JSON has no form for.
fn json_table(settings: &toml::Table) -> serde_json::Value {
    serde_json::Value::Object(
        settings
            .iter()
            .map(|(key, value)| (key.clone(), json_value(value)))
            .collect(),
    )
}

fn json_value(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(text) => serde_json::Value::String(text.clone()),
        toml::Value::Integer(number) => serde_json::Value::from(*number),
        toml::Value::Float(number) => serde_json::Number::from_f64(*number)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        toml::Value::Boolean(flag) => serde_json::Value::Bool(*flag),
        toml::Value::Datetime(when) => serde_json::Value::String(when.to_string()),
        toml::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(json_value).collect())
        }
        toml::Value::Table(table) => json_table(table),
    }
}

/// A message from a module, parsed but with any image still encoded:
/// decoding is for the blocking pool ([`decode_image`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Incoming {
    /// The reply to `hello`.
    Ready {
        /// The protocol the module speaks.
        protocol: u32,
    },
    /// An image for one key.
    Image {
        /// The keypad, as the `keys` message named it.
        device: String,
        /// The key.
        slot: GridSlot,
        /// The image, base64.
        data: String,
    },
    /// An image for every key the module owns.
    ImageAll {
        /// The image, base64.
        data: String,
    },
    /// The module glyph again on one key.
    Clear {
        /// The keypad.
        device: String,
        /// The key.
        slot: GridSlot,
    },
    /// A line for the agent's log.
    Log {
        /// How the agent logs it.
        level: LogLevel,
        /// The text.
        message: String,
    },
    /// A message of a type this host does not know: ignored, so a module
    /// written for a later protocol still runs.
    Unknown(String),
}

/// The level of a module's `log` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// The messages a module sends, as they are written.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModuleMessage {
    Ready {
        protocol: u32,
    },
    Image {
        device: String,
        slot: String,
        #[serde(flatten)]
        data: ImageField,
    },
    ImageAll {
        #[serde(flatten)]
        data: ImageField,
    },
    Clear {
        device: String,
        slot: String,
    },
    Log {
        level: String,
        message: String,
    },
}

/// An image's `png` or `jpeg` field: exactly one of them.
#[derive(Deserialize)]
struct ImageField {
    #[serde(default)]
    png: Option<String>,
    #[serde(default)]
    jpeg: Option<String>,
}

impl ImageField {
    fn into_data(self) -> Result<String, ProtocolError> {
        match (self.png, self.jpeg) {
            (Some(data), None) | (None, Some(data)) => Ok(data),
            (None, None) => Err(ProtocolError::Malformed(
                "an image needs a png or jpeg field".into(),
            )),
            (Some(_), Some(_)) => Err(ProtocolError::Malformed(
                "an image has both png and jpeg fields".into(),
            )),
        }
    }
}

/// Just the `type` of a line, to tell a message this host does not know
/// from one it knows but cannot read.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
}

/// The message types this host knows.
const KNOWN_TYPES: [&str; 5] = ["ready", "image", "image_all", "clear", "log"];

/// Why a module's line was dropped.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum ProtocolError {
    /// The line is not JSON, not an object with a `type`, or a known type
    /// with a field missing or of the wrong kind.
    #[error("not a valid message: {0}")]
    Malformed(String),
}

/// Parse one line a module wrote.
pub(super) fn parse(line: &[u8]) -> Result<Incoming, ProtocolError> {
    let message = match serde_json::from_slice::<ModuleMessage>(line) {
        Ok(message) => message,
        Err(error) => {
            return match serde_json::from_slice::<Envelope>(line) {
                Ok(envelope) if !KNOWN_TYPES.contains(&envelope.kind.as_str()) => {
                    Ok(Incoming::Unknown(envelope.kind))
                }
                _ => Err(ProtocolError::Malformed(error.to_string())),
            };
        }
    };
    Ok(match message {
        ModuleMessage::Ready { protocol } => Incoming::Ready { protocol },
        ModuleMessage::Image { device, slot, data } => Incoming::Image {
            device,
            slot: parse_slot(&slot)?,
            data: data.into_data()?,
        },
        ModuleMessage::ImageAll { data } => Incoming::ImageAll {
            data: data.into_data()?,
        },
        ModuleMessage::Clear { device, slot } => Incoming::Clear {
            device,
            slot: parse_slot(&slot)?,
        },
        ModuleMessage::Log { level, message } => Incoming::Log {
            level: match level.as_str() {
                "debug" => LogLevel::Debug,
                "warn" => LogLevel::Warn,
                "error" => LogLevel::Error,
                _ => LogLevel::Info,
            },
            message,
        },
    })
}

fn parse_slot(name: &str) -> Result<GridSlot, ProtocolError> {
    GridSlot::from_config_name(name)
        .ok_or_else(|| ProtocolError::Malformed(format!("{name:?} names no keypad key")))
}

/// Decode an image a module sent: base64, then at most
/// [`MAX_KEY_IMAGE_BYTES`](openlogi_ipc::MAX_KEY_IMAGE_BYTES), then a PNG or
/// JPEG within [`KeyImage::decode`]'s limits. Blocking work: run it on the
/// blocking pool.
pub(super) fn decode_image(data: &str) -> Result<KeyImage, String> {
    // Refuse an oversized image before allocating its bytes.
    check_key_image_size(data.len() / 4 * 3).map_err(|error| error.to_string())?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|error| format!("the image is not base64: {error}"))?;
    check_key_image_size(bytes.len()).map_err(|error| error.to_string())?;
    KeyImage::decode(&bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) mod tests {
    use openlogi_core::binding::{GridKey, KeypadPage};
    use tokio::io::BufReader;

    use super::*;

    /// A `side`-square PNG of one colour.
    pub(in crate::modules) fn solid_png(side: u32, rgb: [u8; 3]) -> Vec<u8> {
        use image::ImageEncoder as _;
        let pixels: Vec<u8> = (0..side * side).flat_map(|_| rgb).collect();
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, side, side, image::ExtendedColorType::Rgb8)
            .unwrap();
        png
    }

    async fn lines(input: &[u8], max: usize, capacity: usize) -> Vec<(LineRead, String)> {
        let mut reader = BufReader::with_capacity(capacity, input);
        let mut buf = Vec::new();
        let mut out = Vec::new();
        loop {
            let read = read_line(&mut reader, &mut buf, max).await.unwrap();
            let text = String::from_utf8(buf.clone()).unwrap();
            let eof = read == LineRead::Eof;
            out.push((read, text));
            if eof {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn lines_split_on_newlines_and_drop_carriage_returns() {
        for capacity in [1, 3, 64] {
            assert_eq!(
                lines(b"one\r\ntwo\n\nlast", 16, capacity).await,
                [
                    (LineRead::Line, "one".into()),
                    (LineRead::Line, "two".into()),
                    (LineRead::Line, String::new()),
                    (LineRead::Line, "last".into()),
                    (LineRead::Eof, String::new()),
                ],
                "buffer capacity {capacity}"
            );
        }
    }

    #[tokio::test]
    async fn an_oversized_line_is_dropped_whole_and_the_next_one_still_reads() {
        for capacity in [2, 5, 64] {
            assert_eq!(
                lines(
                    b"short\nmuch too long here\nnext\nalso too long",
                    8,
                    capacity
                )
                .await,
                [
                    (LineRead::Line, "short".into()),
                    (LineRead::TooLong, String::new()),
                    (LineRead::Line, "next".into()),
                    (LineRead::TooLong, String::new()),
                    (LineRead::Eof, String::new()),
                ],
                "buffer capacity {capacity}"
            );
        }
        assert_eq!(
            lines(b"12345678\n", 8, 4).await[0],
            (LineRead::Line, "12345678".into()),
            "a line of exactly the limit is kept"
        );
    }

    fn slot(page: u8, key: GridKey) -> GridSlot {
        GridSlot {
            page: KeypadPage::new(page).unwrap(),
            key,
        }
    }

    #[test]
    fn module_messages_parse() {
        assert_eq!(
            parse(br#"{"type":"ready","protocol":1}"#),
            Ok(Incoming::Ready { protocol: 1 })
        );
        assert_eq!(
            parse(br#"{"type":"image","device":"unit:1","slot":"page2:gridTopLeft","png":"AAAA","extra":1}"#),
            Ok(Incoming::Image {
                device: "unit:1".into(),
                slot: slot(2, GridKey::TopLeft),
                data: "AAAA".into(),
            }),
            "an unknown field is ignored"
        );
        assert_eq!(
            parse(br#"{"type":"image_all","jpeg":"BBBB"}"#),
            Ok(Incoming::ImageAll {
                data: "BBBB".into()
            })
        );
        assert_eq!(
            parse(br#"{"type":"clear","device":"unit:1","slot":"gridMiddle"}"#),
            Ok(Incoming::Clear {
                device: "unit:1".into(),
                slot: slot(1, GridKey::Middle),
            })
        );
        assert_eq!(
            parse(br#"{"type":"log","level":"warn","message":"hi"}"#),
            Ok(Incoming::Log {
                level: LogLevel::Warn,
                message: "hi".into()
            })
        );
        assert_eq!(
            parse(br#"{"type":"log","level":"trace","message":"hi"}"#),
            Ok(Incoming::Log {
                level: LogLevel::Info,
                message: "hi".into()
            }),
            "a level this host does not know logs at info"
        );
    }

    #[test]
    fn an_unknown_type_is_told_apart_from_a_broken_message() {
        assert_eq!(
            parse(br#"{"type":"vibrate","ms":20}"#),
            Ok(Incoming::Unknown("vibrate".into()))
        );
        for line in [
            &b"not json"[..],
            br#"{"protocol":1}"#,
            br#"["ready"]"#,
            br#"{"type":"ready"}"#,
            br#"{"type":"ready","protocol":"1"}"#,
            br#"{"type":"image","device":"unit:1","slot":"gridMiddle"}"#,
            br#"{"type":"image","device":"unit:1","slot":"gridMiddle","png":"A","jpeg":"B"}"#,
            br#"{"type":"image","device":"unit:1","slot":"page1:gridMiddle","png":"A"}"#,
            br#"{"type":"clear","device":"unit:1","slot":"gridNowhere"}"#,
            br#"{"type":"log","message":"no level"}"#,
        ] {
            assert!(
                matches!(parse(line), Err(ProtocolError::Malformed(_))),
                "{}",
                String::from_utf8_lossy(line)
            );
        }
    }

    /// The host's lines are single JSON objects with the documented fields.
    #[test]
    fn host_messages_are_one_json_object_per_line() {
        let id = ModuleId::try_new("com.example.weather".to_owned()).unwrap();
        let mut settings = toml::Table::new();
        settings.insert("city".into(), "Lynchburg".into());
        settings.insert("refresh".into(), 300.into());
        settings.insert(
            "since".into(),
            toml::Value::Datetime("1979-05-27".parse().unwrap()),
        );
        let hello = hello(&id, &settings);
        assert!(hello.ends_with('\n') && hello.matches('\n').count() == 1);
        let hello: serde_json::Value = serde_json::from_str(&hello).unwrap();
        assert_eq!(
            hello,
            serde_json::json!({
                "type": "hello",
                "protocol": 1,
                "host": "openlogi",
                "host_version": env!("CARGO_PKG_VERSION"),
                "module_id": "com.example.weather",
                "settings": {"city": "Lynchburg", "refresh": 300, "since": "1979-05-27"},
            })
        );

        let keys = keys(&[
            ModuleKey {
                device: "unit:1".into(),
                slot: slot(1, GridKey::Middle),
                size: 118,
            },
            ModuleKey {
                device: "unit:1".into(),
                slot: slot(2, GridKey::TopLeft),
                size: 118,
            },
        ]);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&keys).unwrap(),
            serde_json::json!({"type": "keys", "keys": [
                {"device": "unit:1", "slot": "gridMiddle", "size": 118},
                {"device": "unit:1", "slot": "page2:gridTopLeft", "size": 118},
            ]})
        );
        assert_eq!(
            super::keys(&[]),
            "{\"type\":\"keys\",\"keys\":[]}\n",
            "no keys is an empty list"
        );
        assert_eq!(
            pressed("unit:1", slot(1, GridKey::BottomRight)),
            "{\"type\":\"pressed\",\"device\":\"unit:1\",\"slot\":\"gridBottomRight\"}\n"
        );
        assert_eq!(shutdown(), "{\"type\":\"shutdown\"}\n");
    }

    #[test]
    fn an_image_decodes_from_base64_within_the_size_cap() {
        let image = decode_image(&STANDARD.encode(solid_png(4, [255, 0, 0]))).unwrap();
        assert_eq!(image, KeyImage::solid(4, [255, 0, 0]));
        assert!(decode_image("not base64!").unwrap_err().contains("base64"));
        assert!(
            decode_image(&STANDARD.encode(b"plain text"))
                .unwrap_err()
                .contains("PNG or JPEG")
        );
        let oversized = "A".repeat(6 * 1024 * 1024);
        assert!(
            decode_image(&oversized)
                .unwrap_err()
                .contains("larger than")
        );
    }
}
