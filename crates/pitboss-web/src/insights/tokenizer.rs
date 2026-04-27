//! Drain-lite tokenizer. Masks high-cardinality runtime values (UUIDs,
//! paths, numbers, hex, IPs, URLs, quoted strings, timestamps) so that
//! failure messages with the same shape collapse to the same canonical
//! template. This is the second-tier clustering key paired with
//! `FailureReason::kind`.
//!
//! Reference: LogPAI/logparser Drain (He et al. 2017). We implement just
//! the masking step — no fixed-depth tree, no similarity threshold — on
//! the assumption that dispatcher failure messages have low surface
//! variability after masking. If clusters fragment in practice, we'd
//! reach for either (a) longer mask list or (b) Levenshtein folding on
//! canonical form.

use std::fmt::Write as _;

const MAX_TEMPLATE_LEN: usize = 500;

/// Mask runtime tokens in `msg` so semantically-identical messages
/// produce the same canonical template. Whitespace-tokenises, applies
/// per-token masks, re-joins with single spaces, and caps at
/// [`MAX_TEMPLATE_LEN`] bytes.
///
/// The cap is enforced *before* each push so the next token can't
/// straddle the boundary mid-codepoint — `String::truncate` panics if
/// the cut isn't on a UTF-8 char boundary, and failure messages
/// routinely contain non-ASCII (CJK paths, accented identifiers,
/// emoji from tool output).
pub fn canonicalize(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len().min(MAX_TEMPLATE_LEN));
    let mut first = true;
    for tok in msg.split_whitespace() {
        let sep_len = if first { 0 } else { 1 };
        let masked = mask_token(tok);
        if out.len() + sep_len + masked.len() > MAX_TEMPLATE_LEN {
            break;
        }
        if !first {
            out.push(' ');
        }
        first = false;
        out.push_str(&masked);
    }
    out
}

/// Mask a single whitespace-delimited token. Order matters: more
/// specific patterns (UUID, ISO timestamp, URL) before more general
/// ones (path, number).
fn mask_token(tok: &str) -> String {
    // Strip leading/trailing punctuation we want to preserve (commas,
    // colons, semicolons, parens, brackets) so the token's
    // classification isn't poisoned by adjacency.
    let (lead, core, trail) = strip_punct(tok);

    let masked = if core.is_empty() {
        String::new()
    } else if is_uuid(core) {
        "<UUID>".into()
    } else if is_iso_timestamp(core) {
        "<TS>".into()
    } else if is_url(core) {
        "<URL>".into()
    } else if is_quoted(core) {
        "<STR>".into()
    } else if is_ipv4(core) {
        "<IP>".into()
    } else if is_line_col(core) {
        "<LOC>".into()
    } else if is_hex_literal(core) {
        "<HEX>".into()
    } else if is_path(core) {
        "<PATH>".into()
    } else if is_long_hex(core) {
        "<HEX>".into()
    } else if is_number(core) {
        "<NUM>".into()
    } else {
        core.to_string()
    };

    let mut out = String::with_capacity(lead.len() + masked.len() + trail.len());
    out.push_str(lead);
    out.push_str(&masked);
    out.push_str(trail);
    out
}

/// Split a token into (leading punctuation, core, trailing punctuation).
/// Brackets, parens, commas, semicolons, colons, periods at start/end
/// are stripped so the core can be classified cleanly.
fn strip_punct(tok: &str) -> (&str, &str, &str) {
    const STRIP: &[char] = &['(', ')', '[', ']', '{', '}', ',', ';', ':', '.', '!', '?'];
    let lead_end = tok
        .char_indices()
        .find(|(_, c)| !STRIP.contains(c))
        .map(|(i, _)| i)
        .unwrap_or(tok.len());
    let trail_start = tok
        .char_indices()
        .rev()
        .find(|(_, c)| !STRIP.contains(c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    if trail_start <= lead_end {
        (tok, "", "")
    } else {
        (
            &tok[..lead_end],
            &tok[lead_end..trail_start],
            &tok[trail_start..],
        )
    }
}

fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if *b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

fn is_iso_timestamp(s: &str) -> bool {
    // YYYY-MM-DDTHH:MM:SS optionally followed by fractional + tz.
    // Be lenient on suffix; require the YYYY-MM-DD prefix and a `T`.
    if s.len() < 19 {
        return false;
    }
    let b = s.as_bytes();
    b[0..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[7] == b'-'
        && b[8..10].iter().all(|c| c.is_ascii_digit())
        && (b[10] == b'T' || b[10] == b' ')
        && b[11..13].iter().all(|c| c.is_ascii_digit())
        && b[13] == b':'
        && b[14..16].iter().all(|c| c.is_ascii_digit())
        && b[16] == b':'
        && b[17..19].iter().all(|c| c.is_ascii_digit())
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("file://")
        || s.starts_with("ws://")
        || s.starts_with("wss://")
}

fn is_quoted(s: &str) -> bool {
    s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
}

fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && p.len() <= 3
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
    })
}

fn is_line_col(s: &str) -> bool {
    let mut parts = s.split(':');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !a.is_empty()
        && !b.is_empty()
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
}

fn is_hex_literal(s: &str) -> bool {
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"));
    matches!(s, Some(rest) if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_path(s: &str) -> bool {
    // Absolute or relative path containing at least one separator.
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return s.contains('/');
    }
    // Heuristic: contains a `/` and at least one non-numeric segment.
    s.contains('/')
        && s.split('/')
            .any(|seg| !seg.is_empty() && seg.bytes().any(|b| b.is_ascii_alphabetic()))
}

fn is_long_hex(s: &str) -> bool {
    s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_number(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let s = s.strip_prefix('-').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    let mut seen_dot = false;
    for b in s.bytes() {
        match b {
            b'0'..=b'9' => {}
            b'.' if !seen_dot => seen_dot = true,
            _ => return false,
        }
    }
    true
}

/// Render a template string with mask placeholders into a sequence of
/// (literal, mask) tokens — used by the SPA pill renderer. We expose a
/// minimal helper here so the same parser drives both the test
/// expectations and any backend rendering.
#[allow(dead_code)]
pub fn split_masks(template: &str) -> Vec<TemplatePart> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut tag = String::from("<");
            for nc in chars.by_ref() {
                tag.push(nc);
                if nc == '>' {
                    break;
                }
            }
            if is_known_mask(&tag) {
                if !buf.is_empty() {
                    out.push(TemplatePart::Literal(std::mem::take(&mut buf)));
                }
                out.push(TemplatePart::Mask(tag));
            } else {
                buf.push_str(&tag);
            }
        } else {
            buf.push(c);
        }
    }
    if !buf.is_empty() {
        out.push(TemplatePart::Literal(buf));
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TemplatePart {
    Literal(String),
    Mask(String),
}

fn is_known_mask(tag: &str) -> bool {
    matches!(
        tag,
        "<UUID>" | "<TS>" | "<URL>" | "<STR>" | "<IP>" | "<LOC>" | "<HEX>" | "<PATH>" | "<NUM>"
    )
}

// `Write` import silences unused-warning when MAX_TEMPLATE_LEN truncation
// path doesn't fire in the binary build. Keep the import since the
// truncation is hot once dispatcher messages get large.
#[allow(dead_code)]
fn _writer_marker() {
    let mut s = String::new();
    let _ = write!(&mut s, "{}", 0u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_uuid() {
        let t = canonicalize("worker 0190ab12-3456-7890-abcd-ef0123456789 crashed");
        assert_eq!(t, "worker <UUID> crashed");
    }

    #[test]
    fn masks_path_and_number() {
        let t = canonicalize("exit 137 in /tmp/abc/script.sh");
        assert_eq!(t, "exit <NUM> in <PATH>");
    }

    #[test]
    fn masks_iso_timestamp() {
        let t = canonicalize("error at 2026-04-27T12:34:56Z while processing");
        assert_eq!(t, "error at <TS> while processing");
    }

    #[test]
    fn masks_url_and_quoted() {
        let t = canonicalize("POST https://api.example.com/v1 returned \"forbidden\"");
        assert_eq!(t, "POST <URL> returned <STR>");
    }

    #[test]
    fn masks_ipv4() {
        let t = canonicalize("connection refused 192.168.1.10");
        assert_eq!(t, "connection refused <IP>");
    }

    #[test]
    fn masks_line_col() {
        let t = canonicalize("syntax error at 42:17 in source");
        assert_eq!(t, "syntax error at <LOC> in source");
    }

    #[test]
    fn masks_hex_literal_and_long_hex() {
        let t = canonicalize("addr 0xdeadbeef commit a1b2c3d4e5f60718");
        assert_eq!(t, "addr <HEX> commit <HEX>");
    }

    #[test]
    fn preserves_word_punctuation() {
        let t = canonicalize("(failed: code 137, retrying)");
        assert_eq!(t, "(failed: code <NUM>, retrying)");
    }

    #[test]
    fn similar_messages_collapse_to_same_template() {
        let a = canonicalize("worker 0190ab12-3456-7890-abcd-ef0123456789 exit 137 in /tmp/a.sh");
        let b = canonicalize("worker 0190de00-aaaa-7bbb-cccc-ddeeeeffeeee exit 1 in /var/x.py");
        assert_eq!(a, b, "two same-shape messages must yield the same template");
    }

    #[test]
    fn truncates_overlong_input() {
        let big = "x ".repeat(2000);
        let t = canonicalize(&big);
        assert!(t.len() <= MAX_TEMPLATE_LEN);
    }

    /// Regression: a multi-byte UTF-8 sequence straddling
    /// `MAX_TEMPLATE_LEN` must NOT panic. `String::truncate` panics on
    /// non-char-boundary cuts, so the cap is checked before each push.
    /// Worst case is a long ASCII prefix that brings `out.len()` close
    /// to the cap, then a multi-byte token that would land mid-codepoint
    /// — that token is now skipped instead of crashing the request.
    #[test]
    fn does_not_panic_on_multibyte_utf8_at_boundary() {
        // 249 × "x " = 498 bytes, then "日" (3 bytes) would push to 501
        // and previously straddled the truncate at byte 500. Now it's
        // simply dropped; the (sub-)cap output is preserved.
        let mut input = String::new();
        for _ in 0..249 {
            input.push_str("x ");
        }
        input.push('日');
        let t = canonicalize(&input);
        assert!(t.len() <= MAX_TEMPLATE_LEN);
        // Critically: result is a valid Rust String (no half-codepoint).
        // The .chars() iterator would panic if it weren't.
        let _: usize = t.chars().count();
    }

    #[test]
    fn does_not_panic_on_emoji_tokens_at_boundary() {
        // 4-byte UTF-8 codepoints (emoji) are the worst case for a
        // byte-cut to land mid-codepoint.
        let mut input = String::new();
        for _ in 0..245 {
            input.push_str("y ");
        }
        input.push('🚀');
        let t = canonicalize(&input);
        assert!(t.len() <= MAX_TEMPLATE_LEN);
    }

    #[test]
    fn split_masks_round_trips() {
        let parts = split_masks("exit <NUM> in <PATH>");
        assert_eq!(
            parts,
            vec![
                TemplatePart::Literal("exit ".into()),
                TemplatePart::Mask("<NUM>".into()),
                TemplatePart::Literal(" in ".into()),
                TemplatePart::Mask("<PATH>".into()),
            ]
        );
    }
}
