// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The limits and the checks of text from a caller. Each plugin runs them where the text enters.

use lenka_plugin_api::{LenkaError, Result};

/// The largest reason of a change. A reason goes into each notice, so it must stay small.
pub const MAX_REASON_BYTES: usize = 4 * 1024;

/// The largest short value, such as a label or a media type.
pub const MAX_SHORT_BYTES: usize = 256;

/// Tells if a character can be in a text with line breaks.
fn is_text_char(item: char) -> bool {
    !item.is_control() || matches!(item, '\n' | '\r' | '\t')
}

/// Refuses control characters in a text. Line breaks and tabs stay valid. A control character can
/// change the terminal of a person, so it must stay out of the data.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidArgument`] if the text has a control character.
pub fn check_text(what: &str, text: &str) -> Result<()> {
    if text.chars().all(is_text_char) {
        Ok(())
    } else {
        Err(LenkaError::invalid_argument(format!(
            "{what} has a control character. Remove it."
        )))
    }
}

/// Refuses a value that is not one short line of text.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidArgument`] if the value is too long, or if it has
/// a line break or another control character.
pub fn check_short(what: &str, text: &str) -> Result<()> {
    if text.len() > MAX_SHORT_BYTES {
        return Err(LenkaError::invalid_argument(format!(
            "{what} is larger than the limit of {MAX_SHORT_BYTES} bytes."
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(LenkaError::invalid_argument(format!(
            "{what} must be one line with no control characters."
        )));
    }
    Ok(())
}

/// Refuses a reason that is too long, or that has a control character.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidArgument`] for a bad reason.
pub fn check_reason(what: &str, text: &str) -> Result<()> {
    if text.len() > MAX_REASON_BYTES {
        return Err(LenkaError::invalid_argument(format!(
            "{what} is larger than the limit of {MAX_REASON_BYTES} bytes. Put the long text in an entry."
        )));
    }
    check_text(what, text)
}

/// Returns the start of a text, with at most this many bytes, and whole characters only.
#[must_use]
pub fn clip(text: &str, max_bytes: usize) -> &str {
    &text[..text.floor_char_boundary(max_bytes)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_keeps_whole_characters() {
        assert_eq!(clip("aé", 2), "a");
        assert_eq!(clip("aé", 3), "aé");
        assert_eq!(clip("aé", 9), "aé");
    }

    #[test]
    fn a_short_value_is_one_line() {
        assert!(check_short("x", "text/plain; charset=utf-8").is_ok());
        for bad in ["a\nb", "a\tb", "a\u{1b}b", "a\u{9b}b"] {
            assert!(check_short("x", bad).is_err(), "{bad:?}");
        }
        assert!(check_short("x", &"a".repeat(MAX_SHORT_BYTES + 1)).is_err());
    }

    #[test]
    fn a_text_can_have_line_breaks() {
        assert!(check_text("x", "a\r\nb\n\tc").is_ok());
        assert!(check_text("x", "a\u{1b}[2J").is_err());
        assert!(check_reason("x", &"a".repeat(MAX_REASON_BYTES + 1)).is_err());
    }
}
