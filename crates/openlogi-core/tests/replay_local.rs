//! Offline replay of raw `openlogi diag touchpad` captures against the real
//! recognizer, for local gesture-feel tuning. Runs only when
//! `OPENLOGI_REPLAY_DIR` is set, so CI never touches it:
//!
//! ```sh
//! OPENLOGI_REPLAY_DIR=/tmp/pinch-opt \
//!   cargo test -p openlogi-core --test replay_local -- --nocapture
//! ```
//!
//! Input: one `*.events` file per segment, one event per line —
//! `F <ts_us> <button> <id>:<x_um>:<y_um>,…` for frames, `DROP <n>`,
//! `CANCEL`, `END`. Events are replayed in file order with the live
//! pipeline semantics: frames feed `update`, `CANCEL` feeds `cancel`,
//! `END` feeds `end`. Gate metrics are recomputed alongside for diagnosis;
//! commit decisions come only from the recognizer under test.

#![expect(
    clippy::tests_outside_test_module,
    reason = "env-gated offline replay harness; CI never sets the variable"
)]
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "the harness input is a developer-provided capture file; panicking is the useful failure"
)]

use openlogi_core::binding::ButtonId;
use openlogi_core::touchpad::{
    GestureRecognition, TouchContact, TouchFrame, TouchpadGestureRecognizer,
};
use std::fs;
use std::path::{Path, PathBuf};

const PINCH_MIN_SPREAD_CHANGE_UM: u64 = 8_000;
const PINCH_MIN_SPREAD_PERCENT: u64 = 8;

#[test]
fn replay_local_captures() {
    let Some(dir) = std::env::var_os("OPENLOGI_REPLAY_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("replay dir readable")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "events"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no *.events files in {}", dir.display());
    for file in files {
        replay_one(&file);
    }
}

fn replay_one(path: &Path) {
    let text = fs::read_to_string(path).expect("events file readable");
    eprintln!("\n=== {} ===", path.file_name().unwrap().to_string_lossy());

    let mut recognizer = TouchpadGestureRecognizer::default();
    let mut stroke = None::<Diag>;
    let mut index = 0usize;
    let mut commits: Vec<(usize, String)> = Vec::new();
    let mut deaths: Vec<String> = Vec::new();

    for line in text.lines() {
        index += 1;
        let mut fields = line.split_whitespace();
        let kind = fields.next().unwrap_or("");
        if kind != "F" {
            // DroppedFrames carries its effect through the Cancel event that
            // follows it in the stream; everything else ends or no-ops.
            if kind == "CANCEL" {
                // OPENLOGI_REPLAY_IGNORE_CANCEL simulates the post-fix
                // stream: drop-induced Cancels vanish and the stroke
                // survives to the watchdog END.
                if std::env::var_os("OPENLOGI_REPLAY_IGNORE_CANCEL").is_none() {
                    if let Some(diag) = stroke.take() {
                        deaths.push(format!("cancel@{index}: {}", diag.summarize()));
                    }
                    recognizer.cancel();
                }
            } else if kind == "END" {
                if let Some(diag) = stroke.take() {
                    match recognizer.end() {
                        Some(tap) => deaths.push(format!(
                            "end-tap@{index}: {tap:?} over {}",
                            diag.summarize()
                        )),
                        None => deaths.push(format!("end-no-commit@{index}: {}", diag.summarize())),
                    }
                } else {
                    recognizer.end();
                }
            }
            continue;
        }

        let timestamp: u64 = fields.next().unwrap().parse().unwrap();
        let button = fields.next().unwrap() == "1";
        let contacts: Vec<TouchContact> = fields
            .next()
            .unwrap_or("")
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|triplet| {
                let mut it = triplet.split(':');
                TouchContact {
                    id: it.next().unwrap().parse().unwrap(),
                    x_um: it.next().unwrap().parse().unwrap(),
                    y_um: it.next().unwrap().parse().unwrap(),
                }
            })
            .collect();
        let Ok(frame) = TouchFrame::new(timestamp, button, contacts.clone()) else {
            continue;
        };
        if contacts.len() >= 2 && stroke.is_none() {
            stroke = Some(Diag::new(timestamp, &contacts));
        }
        if let Some(diag) = stroke.as_mut() {
            diag.record(timestamp, &contacts);
        }
        match recognizer.update(&frame) {
            GestureRecognition::Gesture(id) => {
                let diag = stroke.take();
                commits.push((index, describe_commit(id, timestamp, diag.as_ref())));
            }
            GestureRecognition::NativeScroll => {
                let diag = stroke.take();
                commits.push((
                    index,
                    format!(
                        "NATIVE-SCROLL @{}ms: {}",
                        elapsed_ms(timestamp, diag.as_ref()),
                        diag.map_or_else(|| "no stroke".to_string(), |d| d.summarize())
                    ),
                ));
            }
            GestureRecognition::Pending => {}
        }
    }

    eprintln!("commits ({}):", commits.len());
    for (at, what) in &commits {
        eprintln!("  line {at}: {what}");
    }
    eprintln!("non-committing strokes ({}):", deaths.len());
    for death in &deaths {
        eprintln!("  {death}");
    }
}

fn elapsed_ms(timestamp: u64, diag: Option<&Diag>) -> u64 {
    diag.map_or(0, |d| timestamp.saturating_sub(d.t0_us) / 1_000)
}

fn describe_commit(id: ButtonId, timestamp: u64, diag: Option<&Diag>) -> String {
    format!("{id:?} @{}ms: {}", elapsed_ms(timestamp, diag), {
        match diag {
            Some(diag) => diag.summarize(),
            None => "no stroke".to_string(),
        }
    })
}

/// Mirrors the recognizer's per-segment geometry for diagnosis only.
struct Diag {
    t0_us: u64,
    last_us: u64,
    fingers: u8,
    frames: u32,
    rebases: u32,
    start_spread_um: u64,
    starts: Vec<(u8, u32, u32)>,
    ids: Vec<u8>,
    max_spread_change_um: u64,
    centroid_at_max_um: u64,
    latest: Vec<TouchContact>,
}

impl Diag {
    fn new(timestamp: u64, contacts: &[TouchContact]) -> Self {
        let ids = contacts.iter().map(|c| c.id).collect();
        let starts = contacts.iter().map(|c| (c.id, c.x_um, c.y_um)).collect();
        Self {
            t0_us: timestamp,
            last_us: timestamp,
            fingers: u8::try_from(contacts.len()).unwrap_or(u8::MAX),
            frames: 0,
            rebases: 0,
            start_spread_um: spread(contacts),
            starts,
            ids,
            max_spread_change_um: 0,
            centroid_at_max_um: 0,
            latest: contacts.to_vec(),
        }
    }

    fn record(&mut self, timestamp: u64, contacts: &[TouchContact]) {
        self.last_us = timestamp;
        self.frames += 1;
        let new_ids: Vec<u8> = contacts.iter().map(|c| c.id).collect();
        if new_ids != self.ids {
            // The recognizer restarts the segment on any finger-set change.
            self.rebases += 1;
            self.ids = new_ids;
            self.starts = contacts.iter().map(|c| (c.id, c.x_um, c.y_um)).collect();
            self.start_spread_um = spread(contacts);
        }
        self.latest = contacts.to_vec();
        let current = spread(&self.latest);
        let change = current.abs_diff(self.start_spread_um);
        if change > self.max_spread_change_um {
            self.max_spread_change_um = change;
            self.centroid_at_max_um = centroid_travel(&self.starts, &self.latest);
        }
    }

    fn threshold_um(&self) -> u64 {
        PINCH_MIN_SPREAD_CHANGE_UM.max(
            self.start_spread_um
                .saturating_mul(PINCH_MIN_SPREAD_PERCENT)
                / 100,
        )
    }

    fn summarize(&self) -> String {
        format!(
            "{}f {}ms {}frm {}rebase Δspread {}/{}mm centroid {}mm",
            self.fingers,
            self.duration_ms(),
            self.frames,
            self.rebases,
            self.max_spread_change_um / 1_000,
            self.threshold_um() / 1_000,
            self.centroid_at_max_um / 1_000,
        )
    }

    fn duration_ms(&self) -> u64 {
        self.last_us.saturating_sub(self.t0_us) / 1_000
    }
}

fn spread(contacts: &[TouchContact]) -> u64 {
    let count = i64::try_from(contacts.len()).unwrap_or(1).max(1);
    let sum_x: i64 = contacts.iter().map(|c| i64::from(c.x_um)).sum();
    let sum_y: i64 = contacts.iter().map(|c| i64::from(c.y_um)).sum();
    let lengths: u64 = contacts
        .iter()
        .map(|c| {
            length(
                i64::from(c.x_um) - sum_x / count,
                i64::from(c.y_um) - sum_y / count,
            )
        })
        .sum();
    lengths / count.unsigned_abs()
}

fn centroid_travel(starts: &[(u8, u32, u32)], latest: &[TouchContact]) -> u64 {
    let count = i64::try_from(latest.len()).unwrap_or(1).max(1);
    let mut dx = 0_i64;
    let mut dy = 0_i64;
    for contact in latest {
        if let Some((_, start_x, start_y)) = starts.iter().find(|(id, _, _)| *id == contact.id) {
            dx += i64::from(contact.x_um) - i64::from(*start_x);
            dy += i64::from(contact.y_um) - i64::from(*start_y);
        }
    }
    length(dx / count, dy / count)
}

fn length(dx: i64, dy: i64) -> u64 {
    dx.unsigned_abs()
        .saturating_pow(2)
        .saturating_add(dy.unsigned_abs().saturating_pow(2))
        .isqrt()
}
