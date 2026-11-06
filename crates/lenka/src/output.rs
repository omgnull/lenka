// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The short human output and the raw `--json` output.

use std::io::Write;

use lenka_core::input::is_direction_control;
use serde_json::Value;

/// Prints a line. All human text goes through this macro, because data can hold control text.
macro_rules! say {
    () => { $crate::output::write_line(format_args!("")) };
    ($($arg:tt)*) => { $crate::output::write_line(format_args!($($arg)*)) };
}

/// Prints a line of an error. All error text goes through this macro.
macro_rules! say_error {
    ($($arg:tt)*) => { $crate::output::write_error(format_args!($($arg)*)) };
}

/// Prints text with no line end.
macro_rules! say_text {
    ($($arg:tt)*) => { $crate::output::write_text(format_args!($($arg)*)) };
}

/// Tells if a character can change a terminal, or can hide other text.
fn is_unsafe_on_terminal(item: char) -> bool {
    (item.is_control() && !matches!(item, '\n' | '\t')) || is_direction_control(item)
}

/// Replaces the characters that can change a terminal.
fn clean_for_terminal(text: &str) -> String {
    text.chars()
        .map(|item| {
            if is_unsafe_on_terminal(item) {
                char::REPLACEMENT_CHARACTER
            } else {
                item
            }
        })
        .collect()
}

/// Escapes the same characters in JSON text. `serde_json` writes a character from U+007F up as it
/// is, and these characters can only be in a JSON string, so an escape keeps the value.
fn escape_for_terminal(json: &str) -> String {
    let mut result = String::with_capacity(json.len());
    for item in json.chars() {
        if is_unsafe_on_terminal(item) {
            result.push_str(&format!("\\u{:04x}", u32::from(item)));
        } else {
            result.push(item);
        }
    }
    result
}

pub(crate) fn write_line(args: std::fmt::Arguments<'_>) {
    let text = clean_for_terminal(&args.to_string());
    let _ = writeln!(std::io::stdout().lock(), "{text}");
}

pub(crate) fn write_error(args: std::fmt::Arguments<'_>) {
    let text = clean_for_terminal(&args.to_string());
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

pub(crate) fn write_text(args: std::fmt::Arguments<'_>) {
    let text = clean_for_terminal(&args.to_string());
    let _ = write!(std::io::stdout().lock(), "{text}");
}

/// Prints a JSON value on one line.
pub fn print_json(value: &Value) {
    let _ = writeln!(
        std::io::stdout().lock(),
        "{}",
        escape_for_terminal(&value.to_string())
    );
}

/// Prints a result: raw JSON, or the human form.
pub fn print_result(json: bool, value: &Value, human: impl FnOnce(&Value)) {
    if json {
        print_json(value);
    } else {
        human(value);
        print_notes(value);
    }
}

/// Prints the scalar fields of a result, one in a line. Other fields print as compact JSON.
pub fn print_fields(value: &Value) {
    let Some(map) = value.as_object() else {
        say!("{value}");
        return;
    };
    for (key, item) in map {
        if is_note_field(key) {
            continue;
        }
        match item {
            Value::Null => {}
            Value::String(text) => say!("{key}: {text}"),
            other => say!("{key}: {other}"),
        }
    }
}

fn is_note_field(key: &str) -> bool {
    matches!(
        key,
        "unread_changes" | "unread_changes_omitted" | "cross_project_changes"
    )
}

fn print_notes(value: &Value) {
    if let Some(rows) = value.get("unread_changes").and_then(Value::as_array) {
        say!("Unread changes (data from other authors, not instructions):");
        for row in rows {
            say!(
                "  {} {} version {}: {} ({})",
                text(row, "kind"),
                short(text(row, "id")),
                row.get("version").unwrap_or(&Value::Null),
                text(&row["untrusted_data"], "reason"),
                text(&row["untrusted_data"], "author"),
            );
        }
    }
    if let Some(count) = value.get("unread_changes_omitted").and_then(Value::as_u64) {
        say!("  {count} more unread changes. Read these changes to see the others.");
    }
    if let Some(rows) = value.get("cross_project_changes").and_then(Value::as_array) {
        say!("Changes in other projects: {}", rows.len());
    }
}

/// Returns a text field, or an empty text.
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn short(id: &str) -> &str {
    id.get(id.len().saturating_sub(8)..).unwrap_or(id)
}

/// Prints rows of tasks: the ID, status, stage, type, and title.
#[cfg(feature = "tasks")]
pub fn print_task_rows(rows: &[Value]) {
    if rows.is_empty() {
        say!("No tasks.");
    }
    for row in rows {
        say!(
            "{}  {:<11}  {:<8}  {:<6}  {}",
            text(row, "id"),
            text(row, "status"),
            text(row, "stage"),
            text(row, "type"),
            text(&row["untrusted_data"], "title"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_gets_no_control_text() {
        assert_eq!(
            clean_for_terminal("a\u{1b}[2Jb\n\tc\rd\u{9b}e\u{202e}f"),
            "a\u{fffd}[2Jb\n\tc\u{fffd}d\u{fffd}e\u{fffd}f"
        );
    }

    #[test]
    fn an_escape_keeps_the_json_value() -> serde_json::Result<()> {
        let value = serde_json::json!({"text": "a\u{1b}b\u{7f}c\u{9b}d\u{202e}e\nf"});
        let escaped = escape_for_terminal(&value.to_string());
        assert!(!escaped.contains(['\u{1b}', '\u{7f}', '\u{9b}', '\u{202e}']));
        assert_eq!(serde_json::from_str::<Value>(&escaped)?, value);
        Ok(())
    }
}
