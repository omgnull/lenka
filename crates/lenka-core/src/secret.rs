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
    ("GitHub token", "ghr_", 30),
    ("GitLab token", "glrt-", 16),
    ("GitLab token", "gldt-", 16),
    ("Slack token", "xoxe-", 16),
    ("Slack token", "xapp-", 16),
    ("Slack webhook", "hooks.slack.com/services/", 8),
    ("Stripe key", "rk_live_", 16),
    ("Hugging Face token", "hf_", 30),
    ("SendGrid key", "SG.", 16),
    ("Google OAuth token", "ya29.", 30),
    ("DigitalOcean token", "dop_v1_", 30),
];

/// Prefixes that also start normal words. A token after these prefixes must look random.
const WORD_PREFIXES: &[&str] = &["sk-", "npm_", "pypi-", "hf_"];

/// Prefixes of AWS keys. The rest of such a key has only uppercase letters and digits.
const AWS_PREFIXES: &[&str] = &["AKIA", "ASIA"];

/// Names of settings that often hold a secret value.
const ASSIGNMENT_NAMES: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "accountkey",
    "api-key",
    "auth_token",
    "access_token",
    "client_secret",
    "private_key",
    "token",
    "pwd",
];

/// The count of bytes that the check reads for the rest of a setting name, such as `_access_key`.
const MAX_NAME_SUFFIX_BYTES: usize = 64;

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// A token can follow `-` or `_`, as in `MY_` before a token.
fn is_word_start(text: &[u8], index: usize) -> bool {
    index == 0 || !text[index - 1].is_ascii_alphanumeric()
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
        } else if has_bearer_token(text) {
            Some("bearer token")
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
        let shape =
            *binary.get_or_insert_with(|| StreamShape::of(&buffer[..filled.min(BINARY_PROBE)]));
        let found = match shape {
            StreamShape::Text => find_pattern(&String::from_utf8_lossy(&buffer[..filled])),
            StreamShape::Binary => find_token(&String::from_utf8_lossy(&buffer[..filled])),
            StreamShape::Utf16 => {
                let bytes: Vec<u8> = buffer[..filled]
                    .iter()
                    .copied()
                    .filter(|byte| *byte != 0)
                    .collect();
                find_pattern(&String::from_utf8_lossy(&bytes))
            }
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

/// The kind of a stream, from the zero bytes near its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamShape {
    Text,
    /// Latin text in UTF-16 has a zero byte after or before each letter.
    Utf16,
    Binary,
}

impl StreamShape {
    fn of(probe: &[u8]) -> Self {
        let zeros = probe.iter().filter(|byte| **byte == 0).count();
        if zeros == 0 {
            return Self::Text;
        }
        let at = |offset: usize| {
            probe
                .iter()
                .skip(offset)
                .step_by(2)
                .filter(|byte| **byte == 0)
                .count()
        };
        let half = probe.len() / 2;
        if probe.starts_with(&[0xff, 0xfe])
            || probe.starts_with(&[0xfe, 0xff])
            || at(0).max(at(1)) * 10 >= half * 9
        {
            return Self::Utf16;
        }
        // A few zero bytes do not make a file binary, and they must not hide text.
        if zeros * 100 < probe.len() {
            Self::Text
        } else {
            Self::Binary
        }
    }
}

/// Tells if a token has the form of a key. A long normal name has only lowercase words.
fn looks_random(token: &[u8]) -> bool {
    token
        .iter()
        .any(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
}

/// Tells if a text after a prefix has the form of that prefix. It also gives the count of bytes
/// that it read.
///
/// The caller can skip these bytes after a failed test. A later prefix in the same run has a
/// shorter part of the same run after it, so that prefix also fails. This keeps the check linear
/// for a repeated prefix, such as `sk-sk-sk-`.
fn token_fits(prefix: &str, min_len: usize, rest: &[u8]) -> (bool, usize) {
    if AWS_PREFIXES.contains(&prefix) {
        let run = rest
            .iter()
            .take_while(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            .count();
        return (
            run >= min_len && rest[..run].iter().any(u8::is_ascii_digit),
            run,
        );
    }
    let len = token_len(rest);
    let fits = len >= min_len && (!WORD_PREFIXES.contains(&prefix) || looks_random(&rest[..len]));
    (fits, len)
}

/// A certificate can mention a private key in other text, so the two parts share one line. A PGP
/// key has the form `-----BEGIN PGP PRIVATE KEY BLOCK-----`.
fn has_private_key_block(text: &str) -> bool {
    text.lines().any(|line| {
        line.find("-----BEGIN ").is_some_and(|start| {
            let rest = &line[start..];
            rest.contains("PRIVATE KEY-----") || rest.contains("PRIVATE KEY BLOCK-----")
        })
    })
}

/// Gives the length of the run of `token68` characters at the start of the bytes (RFC 7235,
/// section 2.1). Base64 values and OAuth tokens use `+`, `/`, `=`, `.`, and `~`.
fn token68_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| {
            is_token_char(**byte) || matches!(**byte, b'.' | b'~' | b'+' | b'/' | b'=')
        })
        .count()
}

fn has_bearer_token(text: &str) -> bool {
    text.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        // The word `basic` is common in text, so this form needs the name of the header.
        let basic_form = lower.contains("authorization");
        ["bearer ", "basic "]
            .iter()
            .filter(|word| **word == "bearer " || basic_form)
            .any(|word| {
                let mut from = 0;
                while let Some(position) = lower[from..].find(word) {
                    let after = from + position + word.len();
                    from = after;
                    let len = token68_len(&line.as_bytes()[after..]);
                    if len >= 20
                        && looks_random(&line.as_bytes()[after..after + len])
                        && !is_placeholder(&line[after..after + len])
                    {
                        return true;
                    }
                }
                false
            })
    })
}

/// Finds a token with a fixed form.
fn find_token(text: &str) -> Option<&'static str> {
    if has_private_key_block(text) {
        return Some("private key block");
    }
    let bytes = text.as_bytes();
    for (name, prefix, min_len) in PREFIXES {
        let mut from = 0;
        while let Some(position) = text[from..].find(prefix) {
            let start = from + position;
            let after = start + prefix.len();
            from = after;
            if !is_word_start(bytes, start) {
                continue;
            }
            let (fits, read) = token_fits(prefix, *min_len, &bytes[after..]);
            if fits {
                return Some(name);
            }
            // The run has only ASCII bytes, so its end is a character boundary.
            from = after + read;
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
        // A later `eyJ` in the first part has the same parts after it, and a shorter first
        // part, so it also fails. This skip keeps the check linear for `eyJ-eyJ-eyJ-`.
        from = second_start;
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
        let Some((user_info, _host)) = authority.rsplit_once('@') else {
            continue;
        };
        match user_info.split_once(':') {
            Some((_user, password)) => {
                // Digits here are often a port, as in `http://host:8080@other`.
                let is_port = password.len() <= 5 && password.bytes().all(|b| b.is_ascii_digit());
                if password.len() >= 3 && !is_port && !is_placeholder(password) {
                    return true;
                }
            }
            // A long user part with no colon is a token, as in a Git URL.
            None if user_info.len() >= 32 && looks_random(user_info.as_bytes()) => return true,
            None => {}
        }
    }
    false
}

/// Tells if a value is a sample. A placeholder word counts only as a whole word of the value, so
/// that a real value such as `yourdomain2` is not a sample.
fn is_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.starts_with('<')
        || value.starts_with('$')
        || value.starts_with('{')
        || value.starts_with('%')
        || value.chars().all(|c| c == '*' || c == 'x' || c == 'X')
        || lower.split(|c: char| !c.is_alphanumeric()).any(|part| {
            [
                "example",
                "changeme",
                "password",
                "secret",
                "placeholder",
                "redacted",
                "your",
            ]
            .contains(&part)
        })
}

/// Tells if a value names the place of a secret, and is not the secret. A secret in Base64 can
/// hold `/`, so a path counts only at the start of the value.
fn is_reference(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let scheme = value
        .split_once(':')
        .is_some_and(|(name, _)| !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphabetic()));
    ["/", "./", "../", "~/"]
        .iter()
        .any(|start| value.starts_with(start))
        || scheme
        || lower.contains(".env.")
        || lower.contains("environ")
}

/// Tells if a value is a normal name with words between hyphens, such as `my-service-account-v2`.
fn is_hyphen_name(value: &str) -> bool {
    value.split('-').count() >= 3
        && value
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_lowercase() || b.is_ascii_digit())
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
    // A name can continue, as in `SECRET_ACCESS_KEY`.
    let suffix = token_len(&rest.as_bytes()[..rest.len().min(MAX_NAME_SUFFIX_BYTES)]);
    let trimmed = rest[suffix..].trim_start_matches(['"', '\'', ' ', '\t']);
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
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().any(|c| c.is_ascii_alphabetic())
        && !is_placeholder(value)
        && !is_reference(value)
        && !is_hyphen_name(value)
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
                    "The text in {}{line} matches the pattern \"{pattern}\". Remove the value and refer to it by name. Only a person can override this check, in the desktop app or with `--allow-secret` where a command has this option.",
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
    fn a_basic_header_and_an_account_key_are_secrets() {
        let value = ["dXNlcjpw", "YXNzd29y", "ZDEyMzQ1Njc4OTA"].concat();
        assert!(find_pattern(&format!("Authorization: Basic {value}")).is_some());
        assert!(find_pattern(&format!("AccountKey={value}")).is_some());
        // The word `basic` is common in text, so this form needs the name of the header.
        assert_eq!(find_pattern(&format!("The basic {value} form.")), None);
    }

    #[test]
    fn a_header_token_with_base64_or_url_characters_is_a_secret() {
        // These characters are in the first 20 characters, so a shorter run was too short.
        let basic = ["dXNlcjpw", "/YXNzd29y", "ZDEyMzQ1Njc4OTA="].concat();
        let bearer = ["abc+def/ghi", "12345678901234567890"].concat();
        let oauth = ["a1B2.c3D4", "~e5F6-g7H8_i9J0.k1L2"].concat();
        for text in [
            format!("Authorization: Basic {basic}"),
            format!("Authorization: Bearer {bearer}"),
            format!("Bearer {oauth}"),
        ] {
            let label = text.split_at(16).0.to_owned();
            assert!(find_pattern(&text).is_some(), "no match: {label}");
        }
        assert_eq!(
            find_pattern("The bearer docs/api/authentication.html page."),
            None
        );
    }

    #[test]
    fn a_pgp_private_key_block_is_a_secret() {
        let word = "PRIVATE";
        let block = format!(
            "-----BEGIN PGP {word} KEY BLOCK-----\n\nlQOYBF1x9mEBCADm2Yq7\n-----END PGP {word} KEY BLOCK-----"
        );
        assert_eq!(find_pattern(&block), Some("private key block"));
        assert_eq!(
            find_pattern("-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQENBF1x9mEBCADm2Yq7"),
            None
        );
    }

    #[test]
    fn a_repeated_name_stays_fast() {
        // The cost was quadratic, and this text took 50 seconds in a release build.
        for unit in ["secret=", "password=", "password:a"] {
            let text = unit.repeat(1024 * 1024 / unit.len());
            let started = std::time::Instant::now();
            std::hint::black_box(find_pattern(&text));
            let seconds = started.elapsed().as_secs();
            assert!(seconds < 10, "{unit}: {seconds} s");
        }
    }

    #[test]
    fn a_repeated_prefix_stays_fast() {
        // The cost was quadratic, and the `sk-` text of 1 MiB took 210 seconds in a release
        // build. Each text of 1 MiB now takes less than 0.1 seconds in a debug build. The sizes
        // increase, so that a quadratic cost fails the test in seconds and not in an hour.
        for size in [16, 64, 256, 1024].map(|kib| kib * 1024) {
            for unit in [
                "sk-", "npm_", "hf_", "pypi-", "eyJ-", "eyJ_", "AKIA", "AKIA_",
            ] {
                let text = unit.repeat(size / unit.len());
                let started = std::time::Instant::now();
                assert_eq!(find_pattern(&text), None, "{unit}");
                let elapsed = started.elapsed();
                assert!(elapsed.as_secs_f64() < 2.0, "{unit} {size}: {elapsed:?}");
            }
        }
    }

    #[test]
    fn a_token_after_a_repeated_prefix_is_still_found() {
        // The check skips a run after a failed prefix. A token at the end of the run must count.
        for unit in ["sk-", "npm_", "hf_", "pypi-"] {
            let text = format!("{} {}", unit.repeat(100), fake(unit, 40));
            assert!(find_pattern(&text).is_some(), "{unit}");
        }
        let jwt = format!("eyJ{0}.eyJ{0}.{0}", "abcdefghijkl");
        assert!(find_pattern(&["eyJ-".repeat(100), jwt.clone()].concat()).is_some());
        assert!(find_pattern(&format!("eyJ{}.ab.{jwt}", "abcdefghijkl")).is_some());
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
            format!("AKIA{}", "A1B2".repeat(4)),
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

    /// Builds a value with no secret form in the source, so that a scan of this file stays clean.
    fn value() -> String {
        "a1B2c3D4".repeat(5)
    }

    #[test]
    fn finds_common_forms_of_settings_and_tokens() {
        let long = value();
        let cases = [
            format!("AWS_SECRET_ACCESS_KEY={long}"),
            format!("aws_{}_access_key = {long}", "secret"),
            format!("SECRET_KEY = '{long}'"),
            format!("password_prod={long}"),
            format!("token={long}"),
            format!("pwd={long}"),
            format!("Authorization: Bearer {long}"),
            format!("password = yourdomain{}", "12345"),
            format!("api_key = secret{}", "12345678"),
            format!("postgres://u:yourpass{}@db/x", 9),
            format!("https://{long}@github.com/a/b"),
            // A secret in Base64 can hold a slash.
            format!("{}={}/{long}", "aws_secret_access_key", "wJalr"),
            fake("ghr_", 36),
            fake("xoxe-", 20),
            fake("xapp-", 20),
            fake("rk_live_", 20),
            fake("hf_", 36),
            fake("SG.", 20),
            fake("glrt-", 20),
            fake("gldt-", 20),
            fake("ya29.", 36),
            fake("dop_v1_", 36),
            format!("https://hooks.slack.com/services/{long}"),
            format!("token-{}", fake("ghp_", 36)),
            format!("MY_{}", fake("ghp_", 36)),
        ];
        for text in cases {
            let label = text.split_at(8.min(text.len())).0.to_owned();
            assert!(find_pattern(&text).is_some(), "no match: {label}");
        }
    }

    #[test]
    fn normal_text_with_the_form_of_a_setting_passes() {
        for text in [
            "ASIA_PACIFIC_NORTHEAST_REGION_1",
            "ASIAPACIFICNORTHEASTREGION",
            "sk-learn-pipeline-migration-notes-for-the-team",
            "npm_config_registry_cache_directory_override",
            "pypi-simple-index-mirror-configuration-notes-long-enough-to-count",
            "-----BEGIN CERTIFICATE-----\nThe PRIVATE KEY stays on another machine.",
            "secret: my-service-account-v2",
            "secret = /etc/ssl/app2/key.pem",
            "api_key: process.env.API_KEY_2",
            "client_secret=vault:kv/app/client2",
            "http://foo:8080@bar",
            "Bearer <token>",
            "The bearer token goes in the header.",
            "max_tokens: 4096",
        ] {
            assert_eq!(find_pattern(text), None, "{text}");
        }
    }

    #[test]
    fn a_stream_in_utf16_or_with_a_leading_zero_byte_is_still_checked() {
        let text = format!("password={}\n", value());
        let wide: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(stream_error(&wide), Some(ErrorCode::SecretDetected));
        let mut with_mark = vec![0xff, 0xfe];
        with_mark.extend_from_slice(&wide);
        assert_eq!(stream_error(&with_mark), Some(ErrorCode::SecretDetected));
        let mut with_zero = vec![0_u8];
        with_zero.extend_from_slice(text.repeat(4).as_bytes());
        assert_eq!(stream_error(&with_zero), Some(ErrorCode::SecretDetected));
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
