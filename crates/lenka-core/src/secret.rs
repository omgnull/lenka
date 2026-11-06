// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The secret check. It finds common secret patterns in task text and text artifacts.
//!
//! A pattern check does not find all secrets. An error names the pattern and the location,
//! and never the value.

use lenka_plugin_api::{ErrorCode, LenkaError, Result};

/// A text to check, with a location that an error can show.
#[derive(Debug, Clone)]
pub struct TextCheck {
    /// The place of the text, such as `description`.
    pub location: String,
    /// The text.
    pub text: String,
}

impl TextCheck {
    /// Creates a check.
    pub fn new(location: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            text: text.into(),
        }
    }
}

/// What to do when the check finds a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecretPolicy {
    /// Reject the write.
    #[default]
    Enforce,
    /// Allow the write. Only a person can select this, through the CLI or the desktop app.
    PersonOverride,
}

/// Fixed prefixes with a minimum count of token characters after the prefix.
const PREFIXES: &[(&str, &str, usize)] = &[
    ("AWS access key", "AKIA", 16),
    ("AWS session key", "ASIA", 16),
    ("GitHub token", "ghp_", 30),
    ("GitHub token", "gho_", 30),
    ("GitHub token", "ghs_", 30),
    ("GitHub token", "ghu_", 30),
    ("GitHub token", "github_pat_", 30),
    ("GitLab token", "glpat-", 16),
    ("Slack token", "xoxb-", 16),
    ("Slack token", "xoxp-", 16),
    ("Slack token", "xoxa-", 16),
    ("Slack token", "xoxr-", 16),
    ("Anthropic key", "sk-ant-", 20),
    ("OpenAI key", "sk-proj-", 20),
    ("OpenAI key", "sk-", 32),
    ("Google API key", "AIza", 30),
    ("Stripe key", "sk_live_", 16),
    ("npm token", "npm_", 30),
    ("PyPI token", "pypi-", 40),
];

/// Names of settings that often hold a secret value.
const ASSIGNMENT_NAMES: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "auth_token",
    "access_token",
    "client_secret",
    "private_key",
];

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_word_start(text: &[u8], index: usize) -> bool {
    index == 0 || !is_token_char(text[index - 1])
}

fn token_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| is_token_char(**byte))
        .count()
}

/// Finds the name of the first pattern that the text matches.
#[must_use]
pub fn find_pattern(text: &str) -> Option<&'static str> {
    find_token(text).or_else(|| {
        if has_url_password(text) {
            Some("password in a URL")
        } else if has_secret_assignment(text) {
            Some("secret assignment")
        } else {
            None
        }
    })
}

/// The size of one read of a stream.
const STREAM_CHUNK: usize = 1024 * 1024;

/// The bytes that two reads of a stream share, so that a match cannot fall between them.
const STREAM_OVERLAP: usize = 16 * 1024;

/// The bytes at the start of a stream that show if the stream is binary.
const BINARY_PROBE: usize = 8 * 1024;

/// Checks a stream of any size, such as a file. Invalid UTF-8 does not stop the check.
///
/// A stream with a zero byte near its start is binary. The check of binary data looks only for
/// tokens with a fixed form, because text such as `password=` also occurs in normal programs.
///
/// # Errors
///
/// Returns [`ErrorCode::SecretDetected`] if a pattern matches, and [`ErrorCode::Io`] if a read
/// fails.
pub fn check_stream(location: &str, reader: &mut dyn std::io::Read) -> Result<()> {
    let mut buffer = vec![0_u8; STREAM_CHUNK + STREAM_OVERLAP];
    let mut kept = 0;
    let mut binary = None;
    loop {
        let count = reader
            .read(&mut buffer[kept..])
            .map_err(|err| LenkaError::io("Read data for the secret check", &err))?;
        if count == 0 && kept == 0 {
            return Ok(());
        }
        let filled = kept + count;
        let is_binary =
            *binary.get_or_insert_with(|| buffer[..filled.min(BINARY_PROBE)].contains(&0));
        let text = String::from_utf8_lossy(&buffer[..filled]);
        let found = if is_binary {
            find_token(&text)
        } else {
            find_pattern(&text)
        };
        if let Some(pattern) = found {
            return Err(LenkaError::new(
                ErrorCode::SecretDetected,
                format!("The {location} looks like it holds a secret: {pattern}. Remove it."),
            ));
        }
        if count == 0 {
            return Ok(());
        }
        kept = filled.min(STREAM_OVERLAP);
        buffer.copy_within(filled - kept..filled, 0);
    }
}

/// Finds a token with a fixed form.
fn find_token(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN ") && text.contains("PRIVATE KEY") {
        return Some("private key block");
    }
    let bytes = text.as_bytes();
    for (name, prefix, min_len) in PREFIXES {
        let mut from = 0;
        while let Some(position) = text[from..].find(prefix) {
            let start = from + position;
            let after = start + prefix.len();
            if is_word_start(bytes, start) && token_len(&bytes[after..]) >= *min_len {
                return Some(name);
            }
            from = after;
        }
    }
    has_jwt(text).then_some("JSON web token")
}

fn has_jwt(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(position) = text[from..].find("eyJ") {
        let start = from + position;
        from = start + 3;
        if !is_word_start(bytes, start) {
            continue;
        }
        let first = token_len(&bytes[start..]);
        let second_start = start + first;
        if first < 10 || bytes.get(second_start) != Some(&b'.') {
            continue;
        }
        let second = token_len(&bytes[second_start + 1..]);
        let third_start = second_start + 1 + second;
        if second >= 10
            && bytes.get(third_start) == Some(&b'.')
            && token_len(&bytes[third_start + 1..]) >= 10
        {
            return true;
        }
    }
    false
}

fn has_url_password(text: &str) -> bool {
    let mut from = 0;
    while let Some(position) = text[from..].find("://") {
        let start = from + position + 3;
        from = start;
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '"' | '\'' | '`' | ')'))
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        if let Some((user_info, _host)) = authority.rsplit_once('@')
            && let Some((_user, password)) = user_info.split_once(':')
            && password.len() >= 3
            && !is_placeholder(password)
        {
            return true;
        }
    }
    false
}

fn is_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with('<')
        || value.starts_with('$')
        || value.starts_with('{')
        || value.starts_with('%')
        || value.chars().all(|c| c == '*' || c == 'x' || c == 'X')
        || [
            "example",
            "changeme",
            "password",
            "secret",
            "placeholder",
            "redacted",
            "your",
        ]
        .iter()
        .any(|word| lower.contains(word))
}

/// The count of bytes of an assignment value that the check reads.
const MAX_ASSIGNMENT_VALUE_BYTES: usize = 256;

fn has_secret_assignment(text: &str) -> bool {
    text.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        ASSIGNMENT_NAMES.iter().any(|name| {
            let mut from = 0;
            while let Some(position) = lower[from..].find(name) {
                let after = from + position + name.len();
                from = after;
                if assignment_value_is_secret(&line[after..]) {
                    return true;
                }
            }
            false
        })
    })
}

fn assignment_value_is_secret(rest: &str) -> bool {
    let trimmed = rest.trim_start_matches(['"', '\'', ' ', '\t']);
    let Some(value) = trimmed.strip_prefix(['=', ':']) else {
        return false;
    };
    let value = value.trim_start().trim_start_matches(['"', '\'']);
    // The start of a value is enough, and this limit keeps the check linear for repeated names.
    let value = crate::input::clip(value, MAX_ASSIGNMENT_VALUE_BYTES);
    let end = value
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | '`'))
        .unwrap_or(value.len());
    let value = &value[..end];
    // A real secret value is long and mixes character kinds. This keeps normal prose out.
    value.len() >= 12
        && !is_placeholder(value)
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().any(|c| c.is_ascii_alphabetic())
}

/// Runs the check on all texts.
///
/// # Errors
///
/// Returns [`ErrorCode::SecretDetected`] with the pattern and the location, and never the value.
pub fn check(texts: &[TextCheck], policy: SecretPolicy) -> Result<()> {
    if policy == SecretPolicy::PersonOverride {
        return Ok(());
    }
    for item in texts {
        if let Some(pattern) = find_pattern(&item.text) {
            let line = item
                .text
                .lines()
                .position(|line| find_pattern(line).is_some())
                .map_or(String::new(), |index| format!(", line {}", index + 1));
            return Err(LenkaError::new(
                ErrorCode::SecretDetected,
                format!(
                    "The text in {}{line} matches the pattern \"{pattern}\". Remove the value and refer to it by name. Only a person can override this check, through the CLI.",
                    item.location
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_name_stays_fast() {
        // The cost was quadratic, and this text took 50 seconds in a release build.
        for unit in ["secret=", "password=", "password:a"] {
            let text = unit.repeat(1024 * 1024 / unit.len());
            let started = std::time::Instant::now();
            let _ = find_pattern(&text);
            let seconds = started.elapsed().as_secs();
            assert!(seconds < 10, "{unit}: {seconds} s");
        }
    }

    #[test]
    fn a_long_value_is_still_found() {
        let value = "a1".repeat(50_000);
        let text = ["pass", "word=", &value].concat();
        assert!(find_pattern(&text).is_some());
    }

    fn token() -> String {
        format!("ghp_{}", "a1b2c3d4".repeat(5))
    }

    fn stream_error(bytes: &[u8]) -> Option<ErrorCode> {
        let mut reader = bytes;
        check_stream("file", &mut reader)
            .err()
            .map(|err| err.code())
    }

    #[test]
    fn a_stream_finds_a_token_in_any_position() {
        let found = Some(ErrorCode::SecretDetected);
        assert_eq!(stream_error(b"plain text"), None);
        let invalid = [b"text \xff ".as_slice(), token().as_bytes()].concat();
        assert_eq!(stream_error(&invalid), found);
        let large = [
            vec![b'x'; 3 * STREAM_CHUNK],
            b" ".to_vec(),
            token().into_bytes(),
        ]
        .concat();
        assert_eq!(stream_error(&large), found);
        // The token crosses the end of the first read.
        let mut across = vec![b' '; STREAM_CHUNK + STREAM_OVERLAP - 10];
        across.extend(token().into_bytes());
        across.extend(vec![b' '; STREAM_CHUNK]);
        assert_eq!(stream_error(&across), found);
    }

    #[test]
    fn binary_data_needs_a_token_with_a_fixed_form() {
        assert_eq!(stream_error(b"\0\x01password = hunter222222"), None);
        let binary = [b"\0\x01 ".as_slice(), token().as_bytes()].concat();
        assert_eq!(stream_error(&binary), Some(ErrorCode::SecretDetected));
        assert_eq!(
            stream_error(b"password = hunter222222"),
            Some(ErrorCode::SecretDetected)
        );
    }

    /// Builds test values at run time, so that the source holds no text that looks like a secret.
    fn fake(prefix: &str, len: usize) -> String {
        format!("{prefix}{}", "a1B2".repeat(len / 4 + 1))
    }

    #[test]
    fn finds_common_tokens() {
        let private_key = format!("-----BEGIN RSA {} KEY-----", "PRIVATE");
        let url = format!("postgres://admin:{}@db.internal/app", "s3cr3tValue");
        let assignment = format!("api_key = \"{}\"", fake("Zz9", 20));
        for text in [
            fake("AKIA", 16),
            fake("ghp_", 36),
            fake("sk-ant-", 40),
            fake("glpat-", 20),
            format!("eyJ{0}.eyJ{0}.{0}", "abcdefghijkl"),
            private_key,
            url,
            assignment,
        ] {
            assert!(find_pattern(&text).is_some(), "no match for pattern kind");
        }
    }

    #[test]
    fn normal_prose_passes() {
        for text in [
            "The password must be at least 12 characters.",
            "Set password = <value from the vault>.",
            "See https://example.com/docs:8080/a",
            "postgres://user:$PASSWORD@localhost/app",
            "Task about AKIA prefix documentation",
            "The sk-learn package is a dependency.",
            "secret: rotate after each release",
            "desk-1234 is not a key, and task-abcdefghijklmnopqrstuvwxyz0123456789abcd is not",
        ] {
            assert_eq!(find_pattern(text), None, "false match on: {text}");
        }
    }

    #[test]
    fn the_error_names_the_location_and_hides_the_value() {
        let value = fake("ghp_", 36);
        let texts = [TextCheck::new("plan", format!("first\nkey {value}"))];
        let Err(error) = check(&texts, SecretPolicy::Enforce) else {
            panic!("the check must fail");
        };
        assert_eq!(error.code(), ErrorCode::SecretDetected);
        assert!(error.message().contains("plan, line 2"));
        assert!(!error.message().contains(&value));
        assert!(check(&texts, SecretPolicy::PersonOverride).is_ok());
    }
}
