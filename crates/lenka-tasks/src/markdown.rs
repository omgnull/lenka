// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The Markdown form of tasks. The three headings are fixed, and lenka keeps unknown headings
//! without change.

use lenka_plugin_api::{LenkaError, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The heading of the description.
pub const DESCRIPTION: &str = "Description";
/// The heading of the requirements.
pub const REQUIREMENTS: &str = "Requirements";
/// The heading of the acceptance criteria.
pub const ACCEPTANCE: &str = "Acceptance criteria";

const FENCE: &str = "+++";

/// The largest task text that lenka accepts.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// Refuses text with Git conflict markers.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidFile`] if a line is a conflict marker.
pub fn check_conflict_markers(what: &str, text: &str) -> Result<()> {
    let found = text.lines().any(|line| {
        line.starts_with("<<<<<<< ") || line.starts_with(">>>>>>> ") || line == "======="
    });
    if found {
        return Err(LenkaError::invalid_file(format!(
            "{what} has Git conflict markers. Resolve the conflict in the file, and run `lenka reindex`."
        )));
    }
    Ok(())
}

/// Checks the text of a fixed section. The section structure of a task must not depend on what an
/// author writes in a section, because `task.finish` reads the acceptance criteria from it.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidArgument`] if the text has a `## ` heading outside
/// a code fence, a code fence with no end, or a line that looks like a Git conflict marker.
pub fn check_field_text(what: &str, text: &str) -> Result<()> {
    check_line_text(what, text)?;
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_end();
        let marker = trimmed.trim_start();
        if let Some(open) = fence {
            if marker.starts_with(open) {
                fence = None;
            }
        } else if marker.starts_with("```") {
            fence = Some("```");
        } else if marker.starts_with("~~~") {
            fence = Some("~~~");
        } else if trimmed.starts_with("## ") {
            return Err(LenkaError::invalid_argument(format!(
                "{what} has a line that starts with `## `. Use `### ` for a heading in this text."
            )));
        }
    }
    if fence.is_some() {
        return Err(LenkaError::invalid_argument(format!(
            "{what} has a code fence with no end. Close the fence."
        )));
    }
    Ok(())
}

/// Checks text for a line that would make the file look like an unresolved merge.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidArgument`] if the text has such a line.
pub fn check_line_text(what: &str, text: &str) -> Result<()> {
    if check_conflict_markers(what, text).is_err() {
        return Err(LenkaError::invalid_argument(format!(
            "{what} has a line that looks like a Git conflict marker. Change that line."
        )));
    }
    Ok(())
}

/// Splits a document into its front matter and its body.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidFile`] if the fence or the TOML is not valid.
pub fn split_front<T: DeserializeOwned>(what: &str, text: &str) -> Result<(T, String)> {
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = normalized
        .strip_prefix(FENCE)
        .and_then(|rest| {
            rest.strip_prefix('\n')
                .or_else(|| rest.strip_prefix("\r\n"))
        })
        .ok_or_else(|| {
            LenkaError::invalid_file(format!("{what} has no +++ front matter at its start."))
        })?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == FENCE {
            let front: T = toml::from_str(&rest[..offset]).map_err(|err| {
                LenkaError::invalid_file(format!(
                    "{what} has invalid front matter: {}",
                    err.message()
                ))
            })?;
            return Ok((front, rest[offset + line.len()..].to_owned()));
        }
        offset += line.len();
    }
    Err(LenkaError::invalid_file(format!(
        "{what} has no closing +++ fence."
    )))
}

/// Joins front matter and a body into a document.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::InvalidFile`] if the value cannot be formatted.
pub fn join_front<T: Serialize>(front: &T, body: &str) -> Result<String> {
    let toml = toml::to_string(front)
        .map_err(|err| LenkaError::invalid_file(format!("Front matter: {err}")))?;
    Ok(format!("{FENCE}\n{toml}{FENCE}\n{body}"))
}

/// One part of a body. The text is exactly as in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    heading: Option<String>,
    /// The heading line exactly as in the file.
    heading_line: String,
    text: String,
}

/// A task body, as sections. `to_text` gives back the same bytes if nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Body {
    sections: Vec<Section>,
}

impl Body {
    /// Parses a body. A `## ` line inside a code fence is not a heading.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut sections = vec![Section {
            heading: None,
            heading_line: String::new(),
            text: String::new(),
        }];
        let mut fence: Option<&str> = None;
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_end();
            let marker = trimmed.trim_start();
            if let Some(open) = fence {
                if marker.starts_with(open) {
                    fence = None;
                }
            } else if marker.starts_with("```") {
                fence = Some("```");
            } else if marker.starts_with("~~~") {
                fence = Some("~~~");
            } else if let Some(heading) = trimmed.strip_prefix("## ") {
                sections.push(Section {
                    heading: Some(heading.trim().to_owned()),
                    heading_line: line.to_owned(),
                    text: String::new(),
                });
                continue;
            }
            if let Some(last) = sections.last_mut() {
                last.text.push_str(line);
            }
        }
        if sections.first().is_some_and(|first| first.text.is_empty()) {
            sections.remove(0);
        }
        Self { sections }
    }

    /// Formats the body.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for section in &self.sections {
            text.push_str(&section.heading_line);
            text.push_str(&section.text);
        }
        text
    }

    /// Returns the text of a section, with no blank lines at the ends.
    #[must_use]
    pub fn section(&self, heading: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|section| section.heading.as_deref() == Some(heading))
            .map(|section| section.text.trim_matches('\n'))
    }

    /// Returns the headings and the texts of the sections that are not one of the fixed three.
    #[must_use]
    pub fn other_sections(&self) -> Vec<(String, String)> {
        self.sections
            .iter()
            .filter_map(|section| {
                let heading = section.heading.as_deref()?;
                if [DESCRIPTION, REQUIREMENTS, ACCEPTANCE].contains(&heading) {
                    return None;
                }
                Some((
                    heading.to_owned(),
                    section.text.trim_matches('\n').to_owned(),
                ))
            })
            .collect()
    }

    /// Replaces the text of a fixed section, and adds the section in the fixed order if needed.
    /// An empty text removes the section.
    pub fn set_section(&mut self, heading: &str, text: &str) {
        let trimmed = text.trim_matches('\n');
        let position = self
            .sections
            .iter()
            .position(|section| section.heading.as_deref() == Some(heading));
        if trimmed.is_empty() {
            if let Some(position) = position {
                self.sections.remove(position);
            }
            return;
        }
        let stored = format!("{trimmed}\n\n");
        if let Some(position) = position {
            let section = &mut self.sections[position];
            if !section.heading_line.ends_with('\n') {
                section.heading_line.push('\n');
            }
            section.text = stored;
            return;
        }
        let rank = |name: &str| {
            [DESCRIPTION, REQUIREMENTS, ACCEPTANCE]
                .iter()
                .position(|item| *item == name)
        };
        let wanted = rank(heading).unwrap_or(usize::MAX);
        let insert_at = self
            .sections
            .iter()
            .position(|section| {
                section
                    .heading
                    .as_deref()
                    .and_then(rank)
                    .is_some_and(|value| value > wanted)
            })
            .unwrap_or(self.sections.len());
        self.sections.insert(
            insert_at,
            Section {
                heading: Some(heading.to_owned()),
                heading_line: format!("## {heading}\n"),
                text: stored,
            },
        );
    }

    /// Returns the acceptance criteria.
    #[must_use]
    pub fn criteria(&self) -> Vec<Criterion> {
        self.section(ACCEPTANCE)
            .map(|text| text.lines().filter_map(parse_criterion).collect())
            .unwrap_or_default()
    }

    /// Sets the mark of one criterion. An index out of range changes nothing, and gives `false`.
    pub fn mark_criterion(&mut self, index: usize, done: bool) -> bool {
        let Some(section) = self
            .sections
            .iter_mut()
            .find(|section| section.heading.as_deref() == Some(ACCEPTANCE))
        else {
            return false;
        };
        let mut count = 0;
        let mut changed = false;
        let mut result = String::with_capacity(section.text.len());
        for line in section.text.split_inclusive('\n') {
            if parse_criterion(line.trim_end()).is_some() {
                if count == index {
                    let mark = if done { "- [x] " } else { "- [ ] " };
                    let indent = line.len() - line.trim_start().len();
                    result.push_str(&line[..indent]);
                    result.push_str(mark);
                    result.push_str(&line[indent + 6..]);
                    changed = true;
                    count += 1;
                    continue;
                }
                count += 1;
            }
            result.push_str(line);
        }
        section.text = result;
        changed
    }
}

/// One acceptance criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Criterion {
    /// The text.
    pub text: String,
    /// Tells if the criterion is checked.
    pub done: bool,
}

fn parse_criterion(line: &str) -> Option<Criterion> {
    let line = line.trim_start();
    let (done, text) = if let Some(text) = line.strip_prefix("- [ ] ") {
        (false, text)
    } else {
        let text = line
            .strip_prefix("- [x] ")
            .or_else(|| line.strip_prefix("- [X] "))?;
        (true, text)
    };
    Some(Criterion {
        text: text.trim_end().to_owned(),
        done,
    })
}

/// Tells if two acceptance sections differ in more than their check marks.
#[must_use]
pub fn criteria_text_differs(old: Option<&str>, new: Option<&str>) -> bool {
    fn normal(text: Option<&str>) -> String {
        text.unwrap_or("")
            .trim()
            .replace("- [x] ", "- [ ] ")
            .replace("- [X] ", "- [ ] ")
    }
    normal(old) != normal(new)
}

/// Creates the text of a new body.
#[must_use]
pub fn new_body(description: &str, requirements: &str, acceptance: &str) -> String {
    let mut body = Body::default();
    body.set_section(DESCRIPTION, description);
    body.set_section(REQUIREMENTS, requirements);
    body.set_section(ACCEPTANCE, acceptance);
    body.to_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Preamble.

## Description
Let only one agent claim a task.

## Notes
Unknown heading stays.
```text
## Not a heading
```

## Requirements
- A claim is atomic.

## Acceptance criteria
- [ ] Two parallel claims give one winner.
- [x] Done item.
  - [ ] Nested item.
";

    #[test]
    fn round_trip_keeps_all_bytes() {
        assert_eq!(Body::parse(SAMPLE).to_text(), SAMPLE);
        for odd in ["", "\n", "no newline", "## a", "## a\n\n\n## b\n"] {
            assert_eq!(Body::parse(odd).to_text(), odd, "text: {odd:?}");
        }
    }

    #[test]
    fn snapshot_of_a_new_body() {
        insta::assert_snapshot!(new_body("Short text.", "- One", "- [ ] Check"), @r"
        ## Description
        Short text.

        ## Requirements
        - One

        ## Acceptance criteria
        - [ ] Check
        ");
    }

    #[test]
    fn sections_read_and_change() {
        let mut body = Body::parse(SAMPLE);
        assert_eq!(
            body.section(DESCRIPTION),
            Some("Let only one agent claim a task.")
        );
        assert_eq!(body.other_sections().len(), 1);
        body.set_section(DESCRIPTION, "New text.");
        assert_eq!(body.section(DESCRIPTION), Some("New text."));
        assert!(body.to_text().contains("## Not a heading"));
        assert!(body.to_text().contains("Unknown heading stays."));
        body.set_section(REQUIREMENTS, "");
        assert_eq!(body.section(REQUIREMENTS), None);
    }

    #[test]
    fn a_new_section_goes_in_the_fixed_order() {
        let mut body = Body::parse("## Acceptance criteria\n- [ ] A\n");
        body.set_section(DESCRIPTION, "D");
        let text = body.to_text();
        let d = text.find("## Description");
        let a = text.find("## Acceptance");
        assert!(d < a, "{text}");
    }

    #[test]
    fn criteria_marks() {
        let mut body = Body::parse(SAMPLE);
        let items = body.criteria();
        assert_eq!(items.len(), 3);
        assert!(!items[0].done && items[1].done && !items[2].done);
        assert!(body.mark_criterion(2, true));
        assert!(body.criteria()[2].done);
        assert!(body.to_text().contains("  - [x] Nested item."));
        assert!(!body.mark_criterion(9, true));
    }

    #[test]
    fn check_marks_are_not_a_text_change() {
        assert!(!criteria_text_differs(Some("- [ ] A"), Some("- [x] A")));
        assert!(criteria_text_differs(Some("- [ ] A"), Some("- [ ] B")));
    }

    #[test]
    fn conflict_markers_are_found() {
        assert!(check_conflict_markers("f", "a\n<<<<<<< HEAD\nb").is_err());
        assert!(check_conflict_markers("f", "a\n=======\nb").is_err());
        assert!(check_conflict_markers("f", "a\n====\nb").is_ok());
        assert!(check_field_text("f", "a\n```\n## in a fence\n```\n### b\n").is_ok());
        for bad in [
            "## Requirements\nFAKE",
            "```\ncode",
            "~~~\ncode",
            "a\n=======\nb",
        ] {
            assert!(check_field_text("f", bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_bad_front_matter_gives_a_clear_error() {
        for text in ["no front", "+++\nnot toml ===\n+++\n", "+++\nid = 1\n"] {
            let result: Result<(toml::Table, String)> = split_front("task.md", text);
            assert!(result.is_err(), "{text}");
        }
    }
}
