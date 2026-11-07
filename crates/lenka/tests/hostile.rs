// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The hostile data suite. A remote or a hand edit can write each file in a data directory.
//!
//! The suite finds the fields itself, so a new field in a data file gets the same checks:
//!
//! - A marker text in each field reaches an agent only in `untrusted_data`.
//! - An escape sequence in each field does not reach the terminal of a person.
//! - Each file can be damaged, a named pipe, or a symlink, and no call hangs or fails the server.

#![expect(
    clippy::disallowed_methods,
    reason = "A test reads files that it wrote, and it needs no guard."
)]

#[expect(dead_code, reason = "this test binary uses only a part of the helpers")]
mod common;
#[expect(dead_code, reason = "this test binary uses only a part of the helpers")]
#[path = "common/mcp.rs"]
mod mcp_client;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Machine, TestResult, text};
use mcp_client::{Era, connect, params, result_text};
use serde_json::{Value, json};

const MARKER: &str = "HOSTILE MARKER";
const ESCAPE: &str = "\\u001b[31mHOSTILE";
const CALL_LIMIT: Duration = Duration::from_secs(20);

/// Files that hold no field that lenka shows.
const NO_FIELDS: [&str; 4] = [".gitignore", ".gitattributes", "lock", "pending.json"];

/// A project with each kind of file, and a copy of its data to restore after each case.
struct Sample {
    _temp: tempfile::TempDir,
    machine: Machine,
    repo: PathBuf,
    data: PathBuf,
    template: PathBuf,
    first: String,
    second: String,
    third: String,
}

fn agent_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    full
}

fn sample() -> TestResult<Sample> {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let first_task = machine.json(&repo, &["task", "create", "First"])?;
    let first = text(&first_task, "task").to_owned();
    let second_task = machine.json(&repo, &["task", "create", "Second", "--relates", &first])?;
    let second = text(&second_task, "task").to_owned();
    let third_task = machine.json(&repo, &["task", "create", "Third"])?;
    let third = text(&third_task, "task").to_owned();

    fs::write(repo.join("source.txt"), "Artifact text.\n")?;
    let artifact = machine.json(&repo, &["artifact", "create", "source.txt", "--name", "a"])?;
    let link = json!({
        "task": third, "version": 1, "artifacts_add": [text(&artifact, "artifact")],
        "blocked_by_add": [second], "plan": "A plan.", "reason": "A reason.",
    })
    .to_string();
    machine.json(&repo, &["call", "task.update", &link])?;
    let entry = json!({"task": third, "kind": "comment", "text": "A comment."}).to_string();
    machine.json(&repo, &["call", "task.entry.add", &entry])?;
    machine.json(
        &repo,
        &["memory", "add", "A memory text.", "--tag", "topic"],
    )?;

    let claim = agent_args(&["task", "claim", &first]);
    let output = machine.with_input(&repo, &claim, "", &[("LENKA_AGENT", "a")])?;
    assert!(output.status.success(), "{output:?}");
    let change = json!({
        "task": second, "version": 1, "description": "New text.", "reason": "Another reason.",
    })
    .to_string();
    let update = agent_args(&["call", "task.update", &change]);
    let output = machine.with_input(&repo, &update, "", &[("LENKA_AGENT", "b")])?;
    assert!(output.status.success(), "{output:?}");

    let data = repo.join(".git/lenka-data");
    let template = temp.path().join("template");
    copy_tree(&data, &template)?;
    Ok(Sample {
        _temp: temp,
        machine,
        repo,
        data,
        template,
        first,
        second,
        third,
    })
}

fn copy_tree(from: &Path, to: &Path) -> TestResult {
    fs::create_dir_all(to)?;
    for item in fs::read_dir(from)? {
        let item = item?;
        let target = to.join(item.file_name());
        if item.file_type()?.is_dir() {
            copy_tree(&item.path(), &target)?;
        } else {
            fs::copy(item.path(), target)?;
        }
    }
    Ok(())
}

impl Sample {
    fn restore(&self) -> TestResult {
        fs::remove_dir_all(&self.data)?;
        copy_tree(&self.template, &self.data)
    }

    /// The paths of all files with fields, relative to the data directory.
    fn files(&self) -> TestResult<Vec<PathBuf>> {
        let mut found = Vec::new();
        let mut open = vec![self.template.clone()];
        while let Some(dir) = open.pop() {
            for item in fs::read_dir(dir)? {
                let path = item?.path();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                let name = name.unwrap_or_default();
                if path.is_dir() {
                    if name != "index" {
                        open.push(path);
                    }
                } else if !NO_FIELDS.contains(&name.as_str()) {
                    found.push(path.strip_prefix(&self.template)?.to_path_buf());
                }
            }
        }
        found.sort();
        Ok(found)
    }

    /// Calls each tool that shows data to an agent, and gives back each answer.
    async fn agent_answers(&self) -> TestResult<Vec<(String, Value)>> {
        let client = tokio::time::timeout(
            CALL_LIMIT,
            connect(&self.machine, &self.repo, Some("a"), Era::Initialize),
        )
        .await
        .map_err(|_| "the connection hangs")??;
        let parts = json!(["plan", "entries", "artifacts", "versions"]);
        let mut calls: Vec<(&'static str, Value)> = vec![
            ("project.list", json!({})),
            ("task.list", json!({})),
            ("task.list", json!({"ready": true})),
            ("task.list", json!({"blocked": true})),
            ("task.list", json!({"text": "First"})),
            ("memory.search", json!({"text": "memory"})),
            ("memory.search", json!({"tag": "topic"})),
        ];
        for id in [&self.first, &self.second, &self.third] {
            calls.push(("task.show", json!({"task": id, "include": parts})));
            calls.push(("task.show", json!({"task": id, "version": 1})));
            calls.push(("task.related", json!({"task": id})));
        }
        calls.push(("task.claim", json!({"task": self.second})));
        calls.push((
            "task.finish",
            json!({"task": self.first, "outcome": "released"}),
        ));
        let mut answers = Vec::new();
        for (name, args) in calls {
            let call = client.call_tool(params(name, &args));
            let result = tokio::time::timeout(CALL_LIMIT, call)
                .await
                .map_err(|_| format!("{name} {args} hangs"))??;
            let body = result_text(&result);
            let value = serde_json::from_str(&body).unwrap_or(Value::String(body));
            answers.push((format!("{name} {args}"), value));
        }
        client.cancel().await?;
        Ok(answers)
    }

    /// Runs a CLI command with a time limit, and gives back all its output.
    fn person_output(&self, args: &[&str]) -> TestResult<Vec<u8>> {
        let mut child = self
            .machine
            .command(&self.repo, args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let start = Instant::now();
        while child.try_wait()?.is_none() {
            if start.elapsed() > CALL_LIMIT {
                child.kill()?;
                return Err(format!("lenka {args:?} hangs").into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = child.wait_with_output()?;
        let mut all = output.stdout;
        all.extend_from_slice(&output.stderr);
        assert!(
            !String::from_utf8_lossy(&all).contains("panicked at"),
            "lenka {args:?}: {}",
            String::from_utf8_lossy(&all)
        );
        Ok(all)
    }

    fn person_outputs(&self) -> TestResult<Vec<(String, Vec<u8>)>> {
        let mut answers = Vec::new();
        let mut calls = vec![
            vec!["task", "list"],
            vec!["artifact", "list"],
            vec!["memory", "search", "memory"],
            vec!["project", "list"],
            vec!["doctor"],
        ];
        for id in [&self.first, &self.second, &self.third] {
            calls.push(vec!["task", "show", id]);
            calls.push(vec!["task", "related", id]);
        }
        for args in calls {
            answers.push((args.join(" "), self.person_output(&args)?));
        }
        Ok(answers)
    }
}

/// Lists the positions of a text outside `untrusted_data`.
fn outside_untrusted(value: &Value, path: &str, marker: &str, found: &mut Vec<String>) {
    match value {
        Value::String(text) if text.contains(marker) => found.push(path.to_owned()),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                outside_untrusted(item, &format!("{path}[{index}]"), marker, found);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if key.contains(marker) {
                    found.push(format!("{path}.<key>"));
                }
                if key != "untrusted_data" {
                    outside_untrusted(item, &format!("{path}.{key}"), marker, found);
                }
            }
        }
        _ => {}
    }
}

/// A text with one field of a file changed, for each field of the file.
fn with_each_field(file: &Path, value: &str) -> TestResult<Vec<(String, String)>> {
    let source = fs::read_to_string(file)?;
    if file.extension().is_some_and(|ext| ext == "jsonl") {
        return jsonl_cases(&source, value);
    }
    let mut cases = Vec::new();
    let is_markdown = file.extension().is_some_and(|ext| ext == "md");
    let mut in_front = !is_markdown;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if is_markdown && line.trim_end() == "+++" {
            in_front = index == 0;
            continue;
        }
        if !in_front {
            continue;
        }
        let Some((key, rest)) = line.split_once(" = ") else {
            continue;
        };
        let replacement = if rest.trim_start().starts_with('[') {
            format!("{key} = [\"{value}\"]\n")
        } else {
            format!("{key} = \"{value}\"\n")
        };
        let mut changed = lines.clone();
        changed[index] = &replacement;
        cases.push((key.to_owned(), changed.concat()));
    }
    Ok(cases)
}

fn jsonl_cases(source: &str, value: &str) -> TestResult<Vec<(String, String)>> {
    let mut cases = Vec::new();
    // A TOML escape and a JSON escape have one form here, so the raw text works in both.
    let plain: Value = serde_json::from_str(&format!("\"{value}\""))?;
    for (index, line) in source.lines().enumerate() {
        let parsed: Value = serde_json::from_str(line)?;
        let mut paths = Vec::new();
        string_paths(&parsed, &mut Vec::new(), &mut paths);
        for path in paths {
            let mut changed = parsed.clone();
            let mut target = &mut changed;
            for part in &path {
                target = match part {
                    Ok(key) => &mut target[key.as_str()],
                    Err(position) => &mut target[*position],
                };
            }
            *target = plain.clone();
            let mut rows: Vec<String> = source.lines().map(str::to_owned).collect();
            rows[index] = changed.to_string();
            let name = format!("line {index} {path:?}");
            cases.push((name, rows.join("\n") + "\n"));
        }
    }
    Ok(cases)
}

type JsonPath = Vec<Result<String, usize>>;

fn string_paths(value: &Value, path: &mut JsonPath, found: &mut Vec<JsonPath>) {
    match value {
        Value::String(_) => found.push(path.clone()),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(Err(index));
                string_paths(item, path, found);
                path.pop();
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                path.push(Ok(key.clone()));
                string_paths(item, path, found);
                path.pop();
            }
        }
        _ => {}
    }
}

/// A marker in any field of any file reaches an agent only in `untrusted_data`.
#[tokio::test]
async fn a_marker_in_any_field_stays_in_untrusted_data() -> TestResult {
    let sample = sample()?;
    let mut leaks = Vec::new();
    let mut count = 0;
    for file in sample.files()? {
        for (field, changed) in with_each_field(&sample.template.join(&file), MARKER)? {
            count += 1;
            fs::write(sample.data.join(&file), changed)?;
            match sample.agent_answers().await {
                Ok(answers) => {
                    for (call, answer) in answers {
                        let mut found = Vec::new();
                        outside_untrusted(&answer, "", MARKER, &mut found);
                        for place in found {
                            leaks.push(format!("{}: {field}: {call}: {place}", file.display()));
                        }
                    }
                }
                Err(error) => leaks.push(format!("{}: {field}: {error}", file.display())),
            }
            sample.restore()?;
        }
    }
    assert!(count > 40, "the suite found only {count} fields");
    assert!(
        leaks.is_empty(),
        "{} leaks:\n{}",
        leaks.len(),
        leaks.join("\n")
    );
    Ok(())
}

/// A sync can bring a hostile field in the current file and in each old version. The current file
/// is then invalid, and lenka shows the last valid record, which holds the same hostile field.
#[tokio::test]
async fn a_marker_in_all_copies_of_a_task_stays_in_untrusted_data() -> TestResult {
    let sample = sample()?;
    let mut leaks = Vec::new();
    for item in fs::read_dir(sample.template.join("tasks"))? {
        let id = item?.file_name();
        let root = sample.template.join("tasks").join(&id);
        let mut copies = vec![PathBuf::from("task.md")];
        for item in fs::read_dir(root.join("versions"))? {
            copies.push(
                Path::new("versions")
                    .join(item?.file_name())
                    .join("task.md"),
            );
        }
        let current = with_each_field(&root.join("task.md"), MARKER)?;
        for (field, _) in &current {
            for copy in &copies {
                let cases = with_each_field(&root.join(copy), MARKER)?;
                let changed = cases.iter().find(|(name, _)| name == field);
                if let Some((_, text)) = changed {
                    fs::write(sample.data.join("tasks").join(&id).join(copy), text)?;
                }
            }
            for (call, answer) in sample.agent_answers().await? {
                let mut found = Vec::new();
                outside_untrusted(&answer, "", MARKER, &mut found);
                for place in found {
                    leaks.push(format!("{field}: {call}: {place}"));
                }
            }
            sample.restore()?;
        }
    }
    assert!(
        leaks.is_empty(),
        "{} leaks:\n{}",
        leaks.len(),
        leaks.join("\n")
    );
    Ok(())
}

/// An escape sequence in any field does not reach the terminal of a person.
#[tokio::test]
async fn an_escape_sequence_in_any_field_does_not_reach_a_terminal() -> TestResult {
    let sample = sample()?;
    let mut leaks = Vec::new();
    for file in sample.files()? {
        for (field, changed) in with_each_field(&sample.template.join(&file), ESCAPE)? {
            fs::write(sample.data.join(&file), changed)?;
            for (call, output) in sample.person_outputs()? {
                if output.contains(&0x1b) {
                    leaks.push(format!("{}: {field}: lenka {call}", file.display()));
                }
            }
            sample.restore()?;
        }
    }
    assert!(
        leaks.is_empty(),
        "{} leaks:\n{}",
        leaks.len(),
        leaks.join("\n")
    );
    Ok(())
}

/// The name of a kind of damage, and the function that makes it.
type Damage = (&'static str, fn(&Path) -> TestResult);

fn damages() -> Vec<Damage> {
    #[cfg_attr(not(unix), expect(unused_mut, reason = "Only Unix adds more kinds."))]
    let mut list: Vec<Damage> = vec![
        ("removed", |path| Ok(fs::remove_file(path)?)),
        ("empty", |path| Ok(fs::write(path, "")?)),
        ("not UTF-8", |path| Ok(fs::write(path, [0xff, 0xfe, 0x00])?)),
        ("not the format", |path| {
            Ok(fs::write(path, "[[[ {{{ +++\n")?)
        }),
        ("a directory", |path| {
            fs::remove_file(path)?;
            Ok(fs::create_dir(path)?)
        }),
    ];
    #[cfg(unix)]
    {
        list.push(("a named pipe", |path| {
            fs::remove_file(path)?;
            let made = std::process::Command::new("mkfifo").arg(path).status()?;
            assert!(made.success());
            Ok(())
        }));
        list.push(("a symlink out of the data", |path| {
            let outside = path.with_extension("outside");
            fs::write(&outside, MARKER)?;
            fs::remove_file(path)?;
            Ok(std::os::unix::fs::symlink(outside, path)?)
        }));
    }
    list
}

/// No damage to a file makes a call hang, or stops the server of another agent.
#[tokio::test]
async fn a_damaged_file_does_not_stop_the_tools() -> TestResult {
    let sample = sample()?;
    let mut problems = Vec::new();
    for file in sample.files()? {
        for (name, damage) in damages() {
            damage(&sample.data.join(&file))?;
            let place = format!("{} is {name}", file.display());
            match sample.agent_answers().await {
                Ok(answers) => {
                    for (call, answer) in answers {
                        let mut found = Vec::new();
                        outside_untrusted(&answer, "", MARKER, &mut found);
                        if !found.is_empty() {
                            problems.push(format!("{place}: {call}: {found:?}"));
                        }
                    }
                }
                Err(error) => {
                    // A project with a damaged `lenka.toml` cannot open, but it must not hang.
                    let text = error.to_string();
                    if text.contains("hangs") {
                        problems.push(format!("{place}: {text}"));
                    }
                }
            }
            if let Err(error) = sample.person_outputs() {
                problems.push(format!("{place}: {error}"));
            }
            sample.restore()?;
        }
    }
    assert!(
        problems.is_empty(),
        "{} problems:\n{}",
        problems.len(),
        problems.join("\n")
    );
    Ok(())
}
