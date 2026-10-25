//! Formats a server reply for display: picks a format from the `Content-Type`
//! (sniffing the body when it is missing) and renders sizes and durations.

use std::time::Duration;

/// Longest body text shown; a text view holding megabytes becomes unusable.
const MAX_DISPLAY_BYTES: usize = 512 * 1024;

/// How a response body is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    Json,
    Xml,
    Html,
    Text,
    Binary,
    Empty,
}

impl BodyFormat {
    /// Decides the format from the `Content-Type` header, or from the bytes when it is absent.
    pub fn detect(content_type: Option<&str>, body: &[u8]) -> Self {
        if body.is_empty() {
            return Self::Empty;
        }
        let Some(content_type) = content_type else {
            return Self::sniff(body);
        };
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if mime == "application/json" || mime.ends_with("+json") {
            Self::Json
        } else if mime == "text/html" {
            Self::Html
        } else if mime.ends_with("/xml") || mime.ends_with("+xml") {
            Self::Xml
        } else if std::str::from_utf8(body).is_ok() {
            Self::Text
        } else {
            Self::Binary
        }
    }

    /// Guesses the format of an unlabelled body: JSON if it parses, else text if UTF-8.
    fn sniff(body: &[u8]) -> Self {
        if serde_json::from_slice::<serde_json::Value>(body).is_ok() {
            Self::Json
        } else if std::str::from_utf8(body).is_ok() {
            Self::Text
        } else {
            Self::Binary
        }
    }

    /// Returns the short name shown next to the status.
    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Xml => "XML",
            Self::Html => "HTML",
            Self::Text => "Text",
            Self::Binary => "Binary",
            Self::Empty => "Empty",
        }
    }
}

/// Renders `body` as display text: JSON pretty-printed, binary summarised, long text cut.
pub fn render_body(format: BodyFormat, body: &[u8]) -> String {
    let text = match format {
        BodyFormat::Empty => return String::new(),
        BodyFormat::Binary => {
            return format!("Binary content ({}) is not shown.", format_size(body.len()));
        }
        // A server can label invalid JSON as JSON; show it as sent rather than nothing.
        BodyFormat::Json => serde_json::from_slice::<serde_json::Value>(body)
            .and_then(|value| serde_json::to_string_pretty(&value))
            .unwrap_or_else(|_| String::from_utf8_lossy(body).into_owned()),
        BodyFormat::Xml | BodyFormat::Html | BodyFormat::Text => {
            String::from_utf8_lossy(body).into_owned()
        }
    };
    truncate_for_display(text)
}

/// Cuts `text` to [`MAX_DISPLAY_BYTES`] on a character boundary, noting how much was hidden.
fn truncate_for_display(mut text: String) -> String {
    if text.len() <= MAX_DISPLAY_BYTES {
        return text;
    }
    let cut = text.floor_char_boundary(MAX_DISPLAY_BYTES);
    let hidden = text.len() - cut;
    text.truncate(cut);
    text.push_str(&format!("\n\n… {} more not shown", format_size(hidden)));
    text
}

/// Formats a byte count the way the response panel shows it, e.g. `512 B` or `1.5 KB`.
pub fn format_size(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = KB * 1024;
    let (unit_bytes, unit) = match bytes {
        0..KB => return format!("{bytes} B"),
        KB..MB => (KB, "KB"),
        _ => (MB, "MB"),
    };
    let tenths = bytes * 10 / unit_bytes;
    format!("{}.{} {unit}", tenths / 10, tenths % 10)
}

/// Formats a request duration, e.g. `87 ms` or `1.24 s`.
pub fn format_elapsed(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        format!("{millis} ms")
    } else {
        format!("{}.{:02} s", millis / 1000, millis % 1000 / 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_uses_content_type_with_parameters_and_case() {
        let body = b"x";
        assert_eq!(
            BodyFormat::detect(Some("Application/JSON; charset=utf-8"), body),
            BodyFormat::Json
        );
        assert_eq!(
            BodyFormat::detect(Some("application/problem+json"), body),
            BodyFormat::Json
        );
        assert_eq!(
            BodyFormat::detect(Some("text/html; charset=utf-8"), body),
            BodyFormat::Html
        );
        assert_eq!(
            BodyFormat::detect(Some("application/xml"), body),
            BodyFormat::Xml
        );
        assert_eq!(
            BodyFormat::detect(Some("image/svg+xml"), body),
            BodyFormat::Xml
        );
        assert_eq!(
            BodyFormat::detect(Some("text/plain"), body),
            BodyFormat::Text
        );
    }

    #[test]
    fn detect_treats_non_utf8_as_binary() {
        assert_eq!(
            BodyFormat::detect(Some("image/png"), &[0x89, 0xff, 0x00]),
            BodyFormat::Binary
        );
        assert_eq!(BodyFormat::detect(None, &[0xff, 0xfe]), BodyFormat::Binary);
    }

    #[test]
    fn detect_sniffs_unlabelled_bodies() {
        assert_eq!(BodyFormat::detect(None, br#"{"a":1}"#), BodyFormat::Json);
        assert_eq!(BodyFormat::detect(None, b"hello"), BodyFormat::Text);
        assert_eq!(
            BodyFormat::detect(Some("application/json"), b""),
            BodyFormat::Empty
        );
    }

    #[test]
    fn json_is_pretty_printed_in_original_key_order() {
        let rendered = render_body(BodyFormat::Json, br#"{"b":1,"a":[true]}"#);
        assert_eq!(rendered, "{\n  \"b\": 1,\n  \"a\": [\n    true\n  ]\n}");
    }

    #[test]
    fn invalid_json_is_shown_as_sent() {
        assert_eq!(render_body(BodyFormat::Json, b"{oops"), "{oops");
    }

    #[test]
    fn binary_is_summarised() {
        assert_eq!(
            render_body(BodyFormat::Binary, &[0; 2048]),
            "Binary content (2.0 KB) is not shown."
        );
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        // The leading "a" shifts every two-byte "é" so the limit falls mid-character.
        let body = format!("a{}", "é".repeat(MAX_DISPLAY_BYTES));
        let rendered = render_body(BodyFormat::Text, body.as_bytes());
        let (shown, note) = rendered.split_once("\n\n… ").unwrap();
        assert_eq!(shown.len(), MAX_DISPLAY_BYTES - 1);
        assert_eq!(note, "512.0 KB more not shown");
    }

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }

    #[test]
    fn durations_switch_to_seconds_at_one_second() {
        assert_eq!(format_elapsed(Duration::from_millis(87)), "87 ms");
        assert_eq!(format_elapsed(Duration::from_millis(999)), "999 ms");
        assert_eq!(format_elapsed(Duration::from_millis(1240)), "1.24 s");
    }
}
