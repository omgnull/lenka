// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Test helpers. Each process gets isolated user directories and an isolated Git configuration,
//! so that the tests never read or change the real files of the user.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// One machine: isolated user directories in a temporary directory.
pub struct Machine {
    pub root: PathBuf,
}

impl Machine {
    pub fn new(root: &Path) -> TestResult<Self> {
        fs::create_dir_all(root.join("home"))?;
        fs::create_dir_all(root.join("config"))?;
        fs::write(
            root.join("home/gitconfig"),
            "[user]\n\tname = Test Person\n\temail = person@example.invalid\n[init]\n\tdefaultBranch = main\n",
        )?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn apply_env(&self, command: &mut Command) {
        command
            .env("LENKA_CONFIG_DIR", self.root.join("config"))
            .env("LENKA_DATA_DIR", self.root.join("data"))
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", self.root.join("home/gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("USER", "tester")
            .env_remove("LENKA_AGENT")
            .env_remove("LENKA_PROJECT")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
    }

    pub fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lenka"));
        self.apply_env(&mut command);
        command.args(args).current_dir(dir).stdin(Stdio::null());
        command
    }

    /// Runs `lenka`, and gives back the raw output.
    pub fn run(&self, dir: &Path, args: &[&str]) -> TestResult<Output> {
        Ok(self.command(dir, args).output()?)
    }

    /// Runs `lenka`, and requires success. The result is the standard output.
    pub fn ok(&self, dir: &Path, args: &[&str]) -> TestResult<String> {
        let output = self.run(dir, args)?;
        assert!(
            output.status.success(),
            "lenka {args:?} failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs `lenka --json`, and requires success.
    pub fn json(&self, dir: &Path, args: &[&str]) -> TestResult<Value> {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        Ok(serde_json::from_str(&self.ok(dir, &full)?)?)
    }

    /// Runs `lenka --json`, requires a failure, and returns the error code.
    pub fn fails(&self, dir: &Path, args: &[&str]) -> TestResult<String> {
        let mut full = vec!["--json"];
        full.extend_from_slice(args);
        let output = self.run(dir, &full)?;
        assert!(!output.status.success(), "lenka {args:?} must fail");
        let value: Value = serde_json::from_slice(&output.stdout)?;
        Ok(value["code"].as_str().unwrap_or_default().to_owned())
    }

    /// Runs `lenka` with text on the standard input, with extra environment settings.
    pub fn with_input(
        &self,
        dir: &Path,
        args: &[&str],
        input: &str,
        env: &[(&str, &str)],
    ) -> TestResult<Output> {
        let mut command = self.command(dir, args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input.as_bytes())?;
        }
        Ok(child.wait_with_output()?)
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> TestResult<String> {
        let mut command = Command::new("git");
        self.apply_env(&mut command);
        let output = command.args(args).current_dir(dir).output()?;
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Creates a repository with one commit.
    pub fn repo(&self, name: &str) -> TestResult<PathBuf> {
        let repo = self.root.join(name);
        fs::create_dir_all(&repo)?;
        self.git(&repo, &["init", "-q"])?;
        self.git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"])?;
        Ok(repo)
    }

    /// Creates a repository with a project in the default data directory.
    pub fn project(&self, name: &str) -> TestResult<PathBuf> {
        let repo = self.repo(name)?;
        self.ok(&repo, &["init"])?;
        Ok(repo)
    }
}

/// Returns a text field of a JSON result.
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}
