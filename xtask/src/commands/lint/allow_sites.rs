//! Finds the outer attributes in a source file that carry an `allow`, and the
//! edit that removes each one.
//!
//! The scan is lexical: it skips comments, string and char literals, and
//! finds `#[` … `]` by bracket depth, so an `allow(` inside a doc comment or a
//! `reason` string is never a site. Each attribute it finds is then parsed
//! with `syn` to see whether it is `allow(…)` or a `cfg_attr` holding one.
//! Byte offsets come from the scan, not from `syn` spans, so xtask does not
//! need `proc-macro2`'s `span-locations` feature — feature unification would
//! turn it on for every proc macro in a `--workspace` build.
//!
//! Inner `#![allow]` is out of scope: the `workspace-no-inner-allow` ast-grep
//! guard rejects it.

#[cfg(test)]
mod tests;

use std::ops::Range;

use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::{Meta, Token};

/// One outer attribute that carries an `allow`.
#[derive(Debug)]
pub(crate) struct AllowSite {
    /// 1-based line of the attribute's `#`.
    pub(crate) line: usize,
    /// 1-based column of the attribute's `#`, in chars.
    pub(crate) column: usize,
    /// Byte range of the whole attribute, `#` through `]`.
    pub(crate) span: Range<usize>,
    /// What replaces `span` to remove the `allow` and keep everything else.
    pub(crate) replacement: String,
    /// The lints the `allow` names, as written: `dead_code`, `clippy::foo`.
    pub(crate) lints: Vec<String>,
}

impl AllowSite {
    /// `source` with this site's `allow` removed.
    pub(crate) fn apply(&self, source: &str) -> String {
        let mut edited = String::with_capacity(source.len());
        edited.push_str(&source[..self.span.start]);
        edited.push_str(&self.replacement);
        edited.push_str(&source[self.span.end..]);
        edited
    }
}

/// Every outer attribute in `source` that carries an `allow`.
pub(crate) fn find(source: &str) -> Vec<AllowSite> {
    outer_attributes(source)
        .into_iter()
        .filter_map(|span| {
            // `#[` … `]`: the text between is the attribute's meta.
            let meta = source[span.start + 1..span.end]
                .trim_start()
                .strip_prefix('[')?
                .strip_suffix(']')?;
            if !meta.contains("allow") {
                return None;
            }
            let (replacement, lints) = classify(&syn::parse_str::<Meta>(meta).ok()?)?;
            let line_start = source[..span.start].rfind('\n').map_or(0, |at| at + 1);
            Some(AllowSite {
                line: source[..span.start].matches('\n').count() + 1,
                column: source[line_start..span.start].chars().count() + 1,
                span,
                replacement,
                lints,
            })
        })
        .collect()
}

/// For an `allow(…)` or a `cfg_attr(…)` holding one at any depth: the
/// replacement text and the lints it names. `None` for any other attribute.
fn classify(meta: &Meta) -> Option<(String, Vec<String>)> {
    let (rest, lints) = without_allows(meta)?;
    Some((
        rest.map_or_else(String::new, |rest| format!("#[{rest}]")),
        lints,
    ))
}

/// `meta` with every `allow` in it removed, inside nested `cfg_attr` too:
/// what is left (`None` when nothing is) and the lints the removed `allow`s
/// name. `None` when `meta` holds no `allow`.
fn without_allows(meta: &Meta) -> Option<(Option<String>, Vec<String>)> {
    let Meta::List(list) = meta else {
        return None;
    };
    if list.path.is_ident("allow") {
        return Some((None, lint_names(meta)));
    }
    if !list.path.is_ident("cfg_attr") {
        return None;
    }
    let args = list
        .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .ok()?;
    let mut args = args.into_iter();
    let predicate = args.next()?;
    let mut found = false;
    let mut kept = Vec::new();
    let mut lints = Vec::new();
    for attr in args {
        if let Some((rest, names)) = without_allows(&attr) {
            found = true;
            kept.extend(rest);
            lints.extend(names);
        } else {
            kept.push(attr.to_token_stream().to_string());
        }
    }
    if !found {
        return None;
    }
    let rest = (!kept.is_empty()).then(|| {
        format!(
            "cfg_attr({}, {})",
            predicate.to_token_stream(),
            kept.join(", ")
        )
    });
    Some((rest, lints))
}

/// The lint paths inside one `allow(…)`, skipping `reason = "…"`.
fn lint_names(allow: &Meta) -> Vec<String> {
    let Meta::List(list) = allow else {
        return Vec::new();
    };
    list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
        .map(|args| {
            args.iter()
                .filter_map(|arg| match arg {
                    Meta::Path(path) => Some(path.to_token_stream().to_string().replace(' ', "")),
                    Meta::List(_) | Meta::NameValue(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Byte ranges of every outer attribute, `#` through its closing `]`.
fn outer_attributes(source: &str) -> Vec<Range<usize>> {
    let marks = code_marks(source);
    let mut attributes = Vec::new();
    for (index, &(position, byte)) in marks.iter().enumerate() {
        if byte != b'#' {
            continue;
        }
        let Some(&(open, b'[')) = marks.get(index + 1) else {
            continue;
        };
        if !source[position + 1..open].trim().is_empty() {
            continue;
        }
        let mut depth = 0_usize;
        for &(close, mark) in &marks[index + 1..] {
            match mark {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        attributes.push(position..close + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    attributes
}

/// Positions of `#`, `!`, `[` and `]` that are code: not inside a comment, a
/// string, or a char literal. `!` is kept so `#![` never reads as `#[`.
fn code_marks(source: &str) -> Vec<(usize, u8)> {
    let bytes = source.as_bytes();
    let mut marks = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at = source[at..].find('\n').map_or(bytes.len(), |end| at + end);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => at = block_comment_end(bytes, at),
            b'"' => at = string_end(bytes, at + 1),
            b'r' if starts_raw_string(bytes, at) => at = raw_string_end(bytes, at + 1),
            b'\'' => at = char_literal_end(source, at),
            mark @ (b'#' | b'!' | b'[' | b']') => {
                marks.push((at, mark));
                at += 1;
            }
            _ => at += 1,
        }
    }
    marks
}

/// Index just past a (possibly nested) block comment opening at `start`.
fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0_usize;
    let mut at = start;
    while at + 1 < bytes.len() {
        match (bytes[at], bytes[at + 1]) {
            (b'/', b'*') => {
                depth += 1;
                at += 2;
            }
            (b'*', b'/') => {
                depth -= 1;
                at += 2;
                if depth == 0 {
                    return at;
                }
            }
            _ => at += 1,
        }
    }
    bytes.len()
}

/// Index just past the `"` that closes a string whose body starts at `at`.
fn string_end(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// Whether the `r` at `at` opens a raw string (`r"`, `r#"`, `br"` …) rather
/// than ending an identifier such as `for` or `ptr`.
fn starts_raw_string(bytes: &[u8], at: usize) -> bool {
    let before = at.checked_sub(1).map(|i| bytes[i]);
    let prefix_ok = match before {
        Some(b'b' | b'c') => at < 2 || !is_ident_byte(bytes[at - 2]),
        Some(byte) => !is_ident_byte(byte),
        None => true,
    };
    let hashes = bytes[at + 1..].iter().take_while(|&&b| b == b'#').count();
    prefix_ok && bytes.get(at + 1 + hashes) == Some(&b'"')
}

/// Index just past a raw string whose `#`s (if any) start at `at`.
fn raw_string_end(bytes: &[u8], at: usize) -> usize {
    let hashes = bytes[at..].iter().take_while(|&&b| b == b'#').count();
    let mut cursor = at + hashes + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes[cursor + 1..]
                .iter()
                .take(hashes)
                .filter(|&&b| b == b'#')
                .count()
                == hashes
        {
            return cursor + 1 + hashes;
        }
        cursor += 1;
    }
    bytes.len()
}

/// Index past a char literal opening at `at`, or just past the `'` when it
/// starts a lifetime or label instead.
fn char_literal_end(source: &str, at: usize) -> usize {
    let rest = &source[at + 1..];
    match rest.chars().next() {
        // Every escape starts with `\` and one ASCII byte (`\'`, `\n`, `\u{…}`),
        // so the closing quote is the first `'` after those two bytes.
        Some('\\') => rest
            .get(2..)
            .and_then(|tail| tail.find('\''))
            .map_or(source.len(), |end| at + 1 + 2 + end + 1),
        Some(ch) if rest[ch.len_utf8()..].starts_with('\'') => at + 1 + ch.len_utf8() + 1,
        _ => at + 1,
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
