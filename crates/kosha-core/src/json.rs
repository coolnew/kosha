// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Just enough JSON to write it correctly.
//!
//! Two crates emit JSON — the audit log and the exporter — and both write it by
//! hand rather than take a serialisation dependency for a handful of object
//! shapes. What neither can do by hand is string escaping: paths contain
//! backslashes on Windows, quotes are legal in filenames on Unix, and control
//! characters are legal in both. A single unescaped byte turns a whole export
//! into a parse error somebody else has to diagnose.
//!
//! This is the one implementation. It escapes exactly what RFC 8259 requires
//! and nothing more, so ordinary text stays readable in the output.

/// Write `s` as a quoted, escaped JSON string into `out`.
///
/// Appends rather than returning, so writing a large document does not
/// allocate a String per field.
pub fn write_escaped(out: &mut String, s: &str) {
    out.reserve(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // RFC 8259 requires escaping everything below 0x20, and the short
            // forms above cover the ones with names. The rest go as \u00XX.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `s` as a quoted, escaped JSON string.
pub fn escaped(s: &str) -> String {
    let mut out = String::new();
    write_escaped(&mut out, s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_backslashes_survive() {
        assert_eq!(escaped(r"C:\Users\me"), r#""C:\\Users\\me""#);
        assert_eq!(escaped(r#"say "hi""#), r#""say \"hi\"""#);
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(escaped("a\nb"), r#""a\nb""#);
        assert_eq!(escaped("a\tb"), r#""a\tb""#);
        assert_eq!(escaped("a\rb"), r#""a\rb""#);
        // No short form, so the \u form.
        assert_eq!(escaped("a\u{1}b"), r#""a\u0001b""#);
        assert_eq!(escaped("a\u{7f}b"), "\"a\u{7f}b\"", "DEL needs no escape");
    }

    #[test]
    fn ordinary_text_stays_readable() {
        // Escaping more than required would make every export harder to read
        // for no gain.
        assert_eq!(
            escaped("Vidéos/2024 — copy (1).mp4"),
            "\"Vidéos/2024 — copy (1).mp4\""
        );
        assert_eq!(escaped(""), "\"\"");
    }

    #[test]
    fn appending_does_not_disturb_what_is_there() {
        let mut out = String::from("[");
        write_escaped(&mut out, "a");
        out.push(',');
        write_escaped(&mut out, "b");
        out.push(']');
        assert_eq!(out, r#"["a","b"]"#);
    }
}
