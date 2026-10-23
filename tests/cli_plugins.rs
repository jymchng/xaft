//! End-to-end tests for `xaft trust` and the CLI plugins it gates (parity
//! matrix row 47).
//!
//! `xaft trust` is only real if something obeys it. The consumer is the
//! `cli.toml` plugin loader, which runs inside `xaft_cli::run` *before* clap
//! parses — so the only honest test is the binary itself: these tests spawn
//! `xaft`, nothing is mocked, and the assertion is on what a user's shell sees.
//!
//! Each test gets a scratch `HOME`, `XDG_CONFIG_HOME`, `XAFT_HOME` (where the
//! trust store lives) and data directory, so neither the operator's config nor
//! their trust records can leak in or be touched.
//!
//! Unix-only: the plugin commands exercised here run `echo` and `sh`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch {
    root: PathBuf,
    project: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("xaft-cli-plugins-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".xaft")).unwrap();
        Self { root, project }
    }

    fn xaft(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xaft"))
            .args(args)
            .current_dir(&self.project)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XAFT_HOME", self.root.join("xaft-home"))
            .env("XAFT_CORE__DATA_DIR", self.root.join("xaft-data"))
            .output()
            .expect("run xaft")
    }

    fn project_table(&self, body: &str) -> PathBuf {
        let path = self.project.join(".xaft/cli.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn user_table(&self, body: &str) {
        let dir = self.root.join("config/xaft");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cli.toml"), body).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const HELLO: &str = r#"
[[command]]
path = ["hello"]
help = "Say hello"
run = "echo hello {name}"
args = [{ name = "name", default = "world" }]
"#;

#[test]
fn an_untrusted_project_command_does_not_exist_and_says_why() {
    let s = Scratch::new("untrusted");
    let table = s.project_table(HELLO);
    let output = s.xaft(&["hello"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "clap rejects an unknown subcommand"
    );
    let err = stderr(&output);
    assert!(err.contains("skipping untrusted cli extension"), "{err}");
    assert!(
        err.contains(&format!("xaft trust add {}", canonical(&table))),
        "{err}"
    );
}

#[test]
fn trusting_the_table_makes_its_commands_run_until_the_file_changes() {
    let s = Scratch::new("lifecycle");
    let table = s.project_table(HELLO);

    let trusted = s.xaft(&["trust", "add"]);
    assert!(trusted.status.success(), "{}", stderr(&trusted));
    assert!(
        stdout(&trusted).contains("  trusted cli "),
        "{}",
        stdout(&trusted)
    );
    assert!(
        s.root.join("xaft-home/trust.json").is_file(),
        "the trust store lives in XAFT_HOME, never in the repository"
    );

    let hello = s.xaft(&["hello", "--name", "xaft"]);
    assert!(hello.status.success(), "{}", stderr(&hello));
    assert_eq!(stdout(&hello), "hello xaft\n");

    let default = s.xaft(&["hello"]);
    assert_eq!(stdout(&default), "hello world\n");

    // Editing the reviewed file revokes the trust on its own.
    std::fs::write(&table, format!("{HELLO}\n# edited after review\n")).unwrap();
    let edited = s.xaft(&["hello"]);
    assert_eq!(edited.status.code(), Some(2));
    assert!(
        stderr(&edited).contains("changed since it was trusted"),
        "{}",
        stderr(&edited)
    );

    let listing = s.xaft(&["trust", "list"]);
    assert!(
        stdout(&listing).contains("changed since it was trusted"),
        "{}",
        stdout(&listing)
    );
}

#[test]
fn a_users_own_table_is_trusted_by_location() {
    let s = Scratch::new("user");
    s.user_table("[[command]]\npath = [\"greet\"]\nrun = \"echo hi from user\"\n");
    let output = s.xaft(&["greet"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "hi from user\n");
}

#[test]
fn a_plugin_cannot_redefine_a_built_in_command() {
    let s = Scratch::new("shadow");
    s.user_table("[[command]]\npath = [\"version\"]\nrun = \"echo pwned\"\n");
    let output = s.xaft(&["version"]);
    assert!(output.status.success());
    assert!(!stdout(&output).contains("pwned"), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("is a built-in xaft command"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn the_plugins_exit_status_is_the_commands() {
    let s = Scratch::new("status");
    s.user_table("[[command]]\npath = [\"fail\"]\nrun = \"sh -c 'exit 7'\"\n");
    assert_eq!(s.xaft(&["fail"]).status.code(), Some(7));
}

fn canonical(path: &Path) -> String {
    path.canonicalize().unwrap().display().to_string()
}
