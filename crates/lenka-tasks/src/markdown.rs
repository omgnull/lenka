// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The Markdown form of tasks. The three headings are fixed, and lenka keeps unknown headings
//! without change.

use std::collections::BTreeMap;

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
        line.starts_with("<<<<<<< ")
            || line.starts_with(">>>>>>> ")
            || line.trim_end_matches('\r') == "======="
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
            .find(|section| has_heading(section, heading))
            .map(|section| section.text.trim_matches(['\n', '\r']))
    }

    /// Returns the headings and the texts of the sections that are not one of the fixed three.
    #[must_use]
    pub fn other_sections(&self) -> Vec<(String, String)> {
        self.sections
            .iter()
            .filter_map(|section| {
                let heading = section.heading.as_deref()?;
                if [DESCRIPTION, REQUIREMENTS, ACCEPTANCE]
                    .iter()
                    .any(|name| heading.eq_ignore_ascii_case(name))
                {
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
            .position(|section| has_heading(section, heading));
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
        // A file can end with no newline, and a heading must start a line.
        if let Some(previous) = insert_at
            .checked_sub(1)
            .map(|index| &mut self.sections[index])
        {
            let end = if previous.text.is_empty() {
                &mut previous.heading_line
            } else {
                &mut previous.text
            };
            if !end.is_empty() && !end.ends_with('\n') {
                end.push('\n');
            }
        }
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
        // A hand edit can repeat the section, and a criterion there must also count.
        self.sections
            .iter()
            .filter(|section| has_heading(section, ACCEPTANCE))
            .flat_map(|section| {
                section
                    .text
                    .split_inclusive('\n')
                    .zip(fenced_lines(&section.text))
            })
            .filter(|(_, fenced)| !fenced)
            .filter_map(|(line, _)| parse_criterion(line.trim_end_matches(['\n', '\r'])))
            .collect()
    }

    /// Sets the mark of one criterion. An index out of range changes nothing, and gives `false`.
    pub fn mark_criterion(&mut self, index: usize, done: bool) -> bool {
        self.mark_criteria(&BTreeMap::from([(index, done)]))
            .is_none()
    }

    /// Sets the marks of criteria in one pass over the text. The result is the first position
    /// that the task does not have.
    pub fn mark_criteria(&mut self, marks: &BTreeMap<usize, bool>) -> Option<usize> {
        let mut count = 0;
        for section in &mut self.sections {
            if !has_heading(section, ACCEPTANCE) {
                continue;
            }
            let mut result = String::with_capacity(section.text.len());
            let fenced = fenced_lines(&section.text);
            for (line, in_fence) in section.text.split_inclusive('\n').zip(fenced) {
                if let Some(position) =
                    mark_position(line.trim_end_matches(['\n', '\r'])).filter(|_| !in_fence)
                {
                    if let Some(done) = marks.get(&count) {
                        result.push_str(&line[..position]);
                        result.push(if *done { 'x' } else { ' ' });
                        result.push_str(&line[position + 1..]);
                        count += 1;
                        continue;
                    }
                    count += 1;
                }
                result.push_str(line);
            }
            section.text = result;
        }
        marks.keys().find(|index| **index >= count).copied()
    }
}

fn has_heading(section: &Section, name: &str) -> bool {
    section
        .heading
        .as_deref()
        .is_some_and(|heading| heading.eq_ignore_ascii_case(name))
}

/// One acceptance criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Criterion {
    /// The text.
    pub text: String,
    /// Tells if the criterion is checked.
    pub done: bool,
}

/// Finds the place of the mark in a line of a task list, with any Markdown list marker.
///
/// The finish gate counts these lines, so this function accepts each form that a Markdown renderer
/// shows as a check box: a block quote, and one to four spaces or tabs after the marker.
fn mark_position(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let is_gap = |byte: &u8| matches!(byte, b' ' | b'\t');
    let mut start = 0;
    loop {
        start += bytes[start..]
            .iter()
            .take_while(|byte| is_gap(byte))
            .count();
        if bytes.get(start) != Some(&b'>') {
            break;
        }
        start += 1;
    }
    let rest = &bytes[start..];
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let marker = if digits > 0 {
        matches!(rest.get(digits), Some(b'.' | b')')).then_some(digits + 1)?
    } else {
        matches!(rest.first(), Some(b'-' | b'*' | b'+')).then_some(1)?
    };
    let gap = rest[marker..]
        .iter()
        .take_while(|byte| is_gap(byte))
        .count();
    if !(1..=4).contains(&gap) {
        return None;
    }
    let open = rest.get(marker + gap..)?;
    let has_mark = open.len() >= 3
        && open[0] == b'['
        && matches!(open[1], b' ' | b'x' | b'X')
        && open[2] == b']'
        && open.get(3).is_none_or(is_gap);
    has_mark.then_some(start + marker + gap + 1)
}

fn parse_criterion(line: &str) -> Option<Criterion> {
    let position = mark_position(line)?;
    Some(Criterion {
        text: line.get(position + 2..).unwrap_or("").trim().to_owned(),
        done: line.as_bytes()[position] != b' ',
    })
}

/// Tells for each line of a section if it is inside a code fence. A fence with no end hides
/// nothing, because a hand edit must not hide a criterion from the finish gate.
fn fenced_lines(text: &str) -> Vec<bool> {
    let mut fence: Option<&str> = None;
    let mut result = Vec::new();
    for line in text.split_inclusive('\n') {
        let marker = line.trim();
        if let Some(open) = fence {
            result.push(true);
            if marker.starts_with(open) {
                fence = None;
            }
        } else if marker.starts_with("```") {
            fence = Some("```");
            result.push(true);
        } else if marker.starts_with("~~~") {
            fence = Some("~~~");
            result.push(true);
        } else {
            result.push(false);
        }
    }
    if fence.is_some() {
        result.fill(false);
    }
    result
}

/// Tells if two acceptance sections differ in more than their check marks.
#[must_use]
pub fn criteria_text_differs(old: Option<&str>, new: Option<&str>) -> bool {
    fn normal(text: Option<&str>) -> Vec<String> {
        text.unwrap_or("")
            .trim()
            .lines()
            .map(|line| {
                let mut line = line.trim_end().to_owned();
                if let Some(position) = mark_position(&line) {
                    line.replace_range(position..=position, " ");
                }
                line
            })
            .collect()
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

    #[test]
    fn each_form_of_a_check_box_is_a_criterion() {
        for line in [
            "- [ ] x",
            "-   [ ] x",
            "-\t[ ] x",
            "- [ ]\tx",
            "> - [ ] x",
            ">> * [ ] x",
            "1.  [ ] x",
            "  + [ ] x",
        ] {
            let found = parse_criterion(line).unwrap_or_else(|| panic!("no criterion: {line:?}"));
            assert_eq!(found.text, "x", "{line:?}");
            assert!(!found.done, "{line:?}");
        }
        for line in [
            "- [x]not",
            "-[ ] x",
            "text [ ] x",
            "-      [ ] code",
            "- [y] x",
        ] {
            assert!(parse_criterion(line).is_none(), "{line:?}");
        }
    }

    #[test]
    fn a_check_of_an_empty_criterion_is_not_a_text_change() {
        assert!(!criteria_text_differs(
            Some("- [ ] \n- [ ] B"),
            Some("- [x] \n- [ ] B")
        ));
    }

    #[test]
    fn a_fenced_example_is_not_a_criterion() {
        let text = "## Acceptance criteria\n\n- [ ] real\n```\n- [ ] example\n```\n- [ ] last\n";
        let mut body = Body::parse(text);
        assert_eq!(body.criteria().len(), 2);
        assert!(body.mark_criterion(1, true));
        assert!(body.to_text().contains("- [ ] example"));
        assert!(body.to_text().contains("- [x] last"));
    }

    #[test]
    fn a_fence_with_no_end_does_not_hide_a_criterion() {
        let body = Body::parse("## Acceptance criteria\n\n```\n- [ ] hidden\n");
        assert_eq!(body.criteria().len(), 1);
    }

    #[test]
    fn a_new_section_after_text_with_no_final_newline_starts_a_line() {
        let mut body = Body::parse("Do the work");
        body.set_section(ACCEPTANCE, "- [ ] Tests pass");
        let again = Body::parse(&body.to_text());
        assert_eq!(again.criteria().len(), 1, "{}", body.to_text());
    }

    #[test]
    fn an_empty_criterion_does_not_shift_a_mark() {
        let mut body = Body::parse("## Acceptance criteria\n- [ ] A\n- [ ] \n- [ ] B\n- [ ] C\n");
        assert_eq!(body.criteria().len(), 4);
        assert!(body.mark_criterion(2, true));
        let marks: Vec<bool> = body.criteria().iter().map(|item| item.done).collect();
        assert_eq!(marks, [false, false, true, false]);
    }

    #[test]
    fn criteria_in_other_forms_of_a_hand_edit_still_count() {
        for text in [
            "## acceptance criteria\n- [ ] A\n",
            "## Acceptance criteria\n* [ ] A\n",
            "## Acceptance criteria\n+ [ ] A\n",
            "## Acceptance criteria\n1. [ ] A\n",
            "## Acceptance criteria\n\n## Notes\nText\n\n## Acceptance criteria\n- [ ] A\n",
        ] {
            let mut body = Body::parse(text);
            let items = body.criteria();
            assert_eq!(items.len(), 1, "{text}");
            assert!(!items[0].done, "{text}");
            assert!(body.mark_criterion(0, true), "{text}");
            assert!(body.criteria()[0].done, "{text}");
            assert_eq!(body.criteria()[0].text, "A", "{text}");
        }
    }

    #[test]
    fn a_mark_in_the_middle_of_a_line_is_a_text_change() {
        assert!(criteria_text_differs(
            Some("- [ ] Check - [ ] this"),
            Some("- [ ] Check - [x] this")
        ));
        assert!(!criteria_text_differs(Some("- [ ] A"), Some("- [x] A")));
    }

    #[test]
    fn a_section_has_no_line_end_characters_at_its_ends() {
        let body = Body::parse("## Description\r\nLine\r\n\r\n");
        assert_eq!(body.section(DESCRIPTION), Some("Line"));
    }

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
