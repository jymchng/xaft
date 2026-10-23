//! Project and user CLI plugins declared in `cli.toml`.
//!
//! Port of the TOML half of agenthicc's CLI registry
//! (`agenthicc.cli.registry._load_toml_commands`, PRD-79): a `cli.toml` file
//! declares extra top-level commands, each of which runs a program.
//!
//! ```toml
//! [[command]]
//! path = ["deploy", "staging"]
//! help = "Deploy the current branch to staging"
//! run  = "./scripts/deploy.sh {region} --dry-run={dry_run}"
//! args = [
//!   { name = "region", default = "eu-west-1" },   # --region VALUE
//!   { name = "dry_run", type = "bool" },          # --dry-run
//!   { name = "ticket" },                          # positional TICKET
//! ]
//! ```
//!
//! Two files are read, in agenthicc's precedence order:
//!
//! 1. **user** — `~/.config/xaft/cli.toml`, next to the user's `xaft.toml`.
//!    Trusted by location: only the user writes there.
//! 2. **project** — `.xaft/cli.toml` in the current directory. A repository
//!    can ship this file, and its commands run with the user's permissions,
//!    so it is loaded **only when trusted** through `xaft trust` (see
//!    [`crate::trust`]). This is what makes `xaft trust` a real control rather
//!    than a manifest nothing reads: an untrusted table is reported on stderr
//!    and its commands do not exist.
//!
//! A project command with the same path as a user command wins, with a
//! warning; `[plugins] strict_cli_shadow = true` turns the warning into an
//! error, as in agenthicc.
//!
//! # Argument inference (agenthicc's rules)
//!
//! * `type = "bool"` → a `--flag` (store-true; hyphenated from `snake_case`);
//! * any other arg **with** a `default` → an `--option VALUE`;
//! * any other arg **without** a default → a required positional.
//!
//! # Deliberate divergences
//!
//! * **Python plugins are not loaded.** agenthicc also imports
//!   `.agenthicc/cli/*.py`; xaft runs no Python, so only the TOML form exists.
//! * **The template is split before it is filled.** agenthicc formats the
//!   `run` string and then `shlex.split`s the result, so a value containing a
//!   space becomes two arguments and a value containing a quote aborts the
//!   command. xaft splits the template first and substitutes inside each
//!   token, so every value is exactly one argument and cannot inject flags.
//!   Booleans still render as `True` / `False`, Python's spelling, so a
//!   template written for agenthicc produces the same argv.
//! * **Built-in commands cannot be shadowed.** agenthicc lets a later
//!   registration replace a built-in path. Here a plugin whose first path
//!   element is a built-in command (`run`, `mcp`, `trust`, …) is ignored with
//!   a warning — a cloned repository must never be able to redefine
//!   `xaft run`, trusted or not.
//! * **The child's exit status is the command's.** agenthicc raises
//!   `CalledProcessError` (a traceback, exit 1) for any non-zero status.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command};

use crate::trust::{ExtensionTrust, PROJECT_CLI_PLUGINS, user_config_dir};

/// Where a plugin command came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    /// `~/.config/xaft/cli.toml`.
    User,
    /// `.xaft/cli.toml` in the current directory.
    Project,
}

impl PluginSource {
    /// `user` or `project`, the badge `xaft --help` shows.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// One argument of a plugin command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginArg {
    /// The `{name}` placeholder and the clap id.
    pub name: String,
    /// `type = "bool"`: a store-true `--flag`.
    pub flag: bool,
    /// The default, rendered as a string; `None` makes a string arg positional.
    pub default: Option<String>,
}

/// One `[[command]]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    /// Command path, e.g. `["deploy", "staging"]`.
    pub path: Vec<String>,
    /// One-line help.
    pub help: String,
    /// The program template.
    pub run: String,
    /// Declared arguments.
    pub args: Vec<PluginArg>,
    /// Which file declared it.
    pub source: PluginSource,
    /// That file.
    pub file: PathBuf,
}

/// The commands that survived trust, parsing and shadowing checks.
#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    commands: Vec<PluginCommand>,
}

/// The result of discovery: usable commands plus what the user must be told.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    /// Commands to add to the parser.
    pub registry: PluginRegistry,
    /// Warnings for stderr (untrusted file, bad table, shadowing).
    pub warnings: Vec<String>,
}

/// A parsed plugin invocation, ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInvocation {
    /// The command path that matched.
    pub path: Vec<String>,
    /// The program and its arguments.
    pub argv: Vec<String>,
}

/// Parse one `cli.toml`, returning its valid commands and a warning per
/// rejected table.
pub fn parse_cli_table(
    text: &str,
    file: &Path,
    source: PluginSource,
) -> (Vec<PluginCommand>, Vec<String>) {
    let table: toml::Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(e) => {
            return (
                Vec::new(),
                vec![format!("bad cli.toml at {}: {e}", file.display())],
            );
        }
    };
    let Some(entries) = table.get("command") else {
        return (Vec::new(), Vec::new());
    };
    let Some(entries) = entries.as_array() else {
        return (
            Vec::new(),
            vec![format!(
                "bad cli.toml at {}: `command` must be an array of tables",
                file.display()
            )],
        );
    };

    let mut commands = Vec::new();
    let mut warnings = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match parse_command(entry, file, source) {
            Ok(Some(command)) => commands.push(command),
            Ok(None) => {}
            Err(reason) => warnings.push(format!(
                "ignoring command #{} in {}: {reason}",
                index + 1,
                file.display()
            )),
        }
    }
    (commands, warnings)
}

fn parse_command(
    entry: &toml::Value,
    file: &Path,
    source: PluginSource,
) -> Result<Option<PluginCommand>, String> {
    let table = entry.as_table().ok_or("each command must be a table")?;
    let path: Vec<String> = match table.get("path") {
        // agenthicc skips a table with no path silently.
        None => return Ok(None),
        Some(value) => value
            .as_array()
            .ok_or("`path` must be a list of strings")?
            .iter()
            .map(|p| p.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or("`path` must be a list of strings")?,
    };
    if path.is_empty() {
        return Ok(None);
    }
    for part in &path {
        if !valid_command_name(part) {
            return Err(format!("invalid command name {part:?}"));
        }
    }
    let help = table
        .get("help")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .to_string();
    let run = table
        .get("run")
        .and_then(|r| r.as_str())
        .ok_or("`run` must be a string")?
        .to_string();
    if run.trim().is_empty() {
        return Err("`run` must not be empty".into());
    }

    let mut args = Vec::new();
    if let Some(raw) = table.get("args") {
        let raw = raw.as_array().ok_or("`args` must be a list of tables")?;
        for arg in raw {
            let arg = arg.as_table().ok_or("`args` must be a list of tables")?;
            // agenthicc skips an arg without a string name.
            let Some(name) = arg.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if !valid_arg_name(name) {
                return Err(format!("invalid argument name {name:?}"));
            }
            let flag = arg.get("type").and_then(|t| t.as_str()) == Some("bool");
            let default = arg.get("default").map(render_default);
            args.push(PluginArg {
                name: name.to_string(),
                flag,
                default,
            });
        }
    }

    // Reject a template that cannot be rendered now, not when it is run.
    let known: BTreeMap<String, String> = args
        .iter()
        .map(|a| (a.name.clone(), String::new()))
        .collect();
    render_template(&run, &known)?;

    Ok(Some(PluginCommand {
        path,
        help,
        run,
        args,
        source,
        file: file.to_path_buf(),
    }))
}

fn render_default(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Boolean(b) => python_bool(*b).to_string(),
        other => other.to_string(),
    }
}

fn python_bool(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

fn valid_command_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_arg_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Split a command line the way POSIX `shlex.split` does.
///
/// Whitespace separates words; single quotes are literal; double quotes allow
/// `\` to escape `\`, `"`, `$`, `` ` `` and newline; a backslash outside quotes
/// escapes the next character. An unterminated quote is an error.
pub fn shell_split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some(&next) if matches!(next, '\\' | '"' | '$' | '`' | '\n') => {
                                current.push(next);
                                chars.next();
                            }
                            _ => current.push('\\'),
                        },
                        Some(c) => current.push(c),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(next) => current.push(next),
                    None => return Err("No escaped character".into()),
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Render a `run` template into argv.
///
/// The template is split first, then each `{name}` placeholder is replaced
/// inside its word — see the module docs for why. `{{` and `}}` are literal
/// braces, as in Python's `str.format`. An unknown placeholder, a positional
/// `{}` or a format spec is an error.
pub fn render_template(
    template: &str,
    values: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let words = shell_split(template)?;
    if words.is_empty() {
        return Err("`run` renders to an empty command".into());
    }
    words.iter().map(|word| substitute(word, values)).collect()
}

fn substitute(word: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("unterminated `{` in run template".into()),
                    }
                }
                if name.is_empty() {
                    return Err("positional `{}` is not supported in run templates".into());
                }
                if !valid_arg_name(&name) {
                    return Err(format!(
                        "unsupported placeholder {{{name}}}; use {{argument_name}}"
                    ));
                }
                let value = values
                    .get(&name)
                    .ok_or_else(|| format!("unknown placeholder {{{name}}}"))?;
                out.push_str(value);
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '}' => return Err("single `}` encountered in run template".into()),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Discover plugins for this process: user table, then trusted project table.
///
/// `builtins` are the top-level command names (and aliases) a plugin may not
/// take. `strict_shadow` is `[plugins] strict_cli_shadow`.
pub fn discover(
    cwd: &Path,
    user_dir: Option<&Path>,
    trust: &ExtensionTrust,
    builtins: &[String],
    strict_shadow: bool,
) -> Result<Discovery, String> {
    let mut discovery = Discovery::default();
    let mut found: Vec<PluginCommand> = Vec::new();

    let mut sources: Vec<(PathBuf, PluginSource)> = Vec::new();
    if let Some(dir) = user_dir {
        sources.push((dir.join("cli.toml"), PluginSource::User));
    }
    sources.push((cwd.join(PROJECT_CLI_PLUGINS), PluginSource::Project));

    for (file, source) in sources {
        if !file.is_file() {
            continue;
        }
        if let Err(refusal) = trust.permits(&file, "cli") {
            discovery.warnings.push(format!(
                "skipping untrusted {} extension {} ({}); review it, then run `xaft trust add {}`",
                refusal.kind,
                refusal.path.display(),
                refusal.reason,
                refusal.path.display()
            ));
            continue;
        }
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) => {
                discovery
                    .warnings
                    .push(format!("could not read {}: {e}", file.display()));
                continue;
            }
        };
        let (commands, warnings) = parse_cli_table(&text, &file, source);
        discovery.warnings.extend(warnings);
        found.extend(commands);
    }

    let mut accepted: Vec<PluginCommand> = Vec::new();
    for command in found {
        let head = &command.path[0];
        if builtins.iter().any(|b| b == head) {
            discovery.warnings.push(format!(
                "ignoring CLI plugin `{}` from {}: `{head}` is a built-in xaft command",
                command.path.join(" "),
                command.file.display()
            ));
            continue;
        }
        if let Some(existing) = accepted.iter().position(|c| c.path == command.path) {
            let shadowed = &accepted[existing];
            if shadowed.source == PluginSource::User && command.source == PluginSource::Project {
                let message = format!(
                    "{} (project) shadows {} (user) for command path {:?}",
                    command.file.display(),
                    shadowed.file.display(),
                    command.path
                );
                if strict_shadow {
                    return Err(message);
                }
                discovery.warnings.push(message);
            }
            // The later layer wins, as in agenthicc.
            accepted[existing] = command;
            continue;
        }
        accepted.push(command);
    }
    discovery.registry = PluginRegistry { commands: accepted };
    Ok(discovery)
}

/// Discover with the user's real trust store, user directory and config.
pub fn discover_default(cwd: &Path, builtins: &[String]) -> Result<Discovery, String> {
    let strict = xaft_config::ConfigLoader::load(&xaft_config::CliOverrides::default())
        .map(|config| config.plugins.strict_cli_shadow)
        .unwrap_or(false);
    discover(
        cwd,
        user_config_dir().as_deref(),
        &ExtensionTrust::for_user(),
        builtins,
        strict,
    )
}

/// Every top-level name `command` reserves: subcommands, their aliases, and
/// clap's own `help`.
pub fn builtin_names(command: &Command) -> Vec<String> {
    let mut names = vec!["help".to_string()];
    for sub in command.get_subcommands() {
        names.push(sub.get_name().to_string());
        names.extend(sub.get_all_aliases().map(str::to_string));
    }
    names
}

/// clap (without its `string` feature) wants `'static` names. The handful of
/// plugin strings are built once per process, so leaking them is the simple,
/// bounded answer.
fn leak(value: &str) -> &'static str {
    Box::leak(value.to_string().into_boxed_str())
}

/// A node of the plugin command tree.
#[derive(Default)]
struct Node {
    entry: Option<usize>,
    children: BTreeMap<String, Node>,
}

impl PluginRegistry {
    /// The usable commands.
    pub fn commands(&self) -> &[PluginCommand] {
        &self.commands
    }

    /// Whether there is nothing to add.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn tree(&self) -> BTreeMap<String, Node> {
        let mut roots: BTreeMap<String, Node> = BTreeMap::new();
        for (index, command) in self.commands.iter().enumerate() {
            let mut level = &mut roots;
            let last = command.path.len() - 1;
            for (depth, part) in command.path.iter().enumerate() {
                let node = level.entry(part.clone()).or_default();
                if depth == last {
                    node.entry = Some(index);
                }
                level = &mut node.children;
            }
        }
        roots
    }

    /// Add every plugin command to `command` as a subcommand tree.
    pub fn augment(&self, mut command: Command) -> Command {
        for (name, node) in self.tree() {
            command = command.subcommand(self.build(&name, &node));
        }
        command
    }

    fn build(&self, name: &str, node: &Node) -> Command {
        let mut command = Command::new(leak(name));
        if let Some(index) = node.entry {
            let plugin = &self.commands[index];
            let about = if plugin.help.is_empty() {
                format!("[{}]", plugin.source.as_str())
            } else {
                format!("{}  [{}]", plugin.help, plugin.source.as_str())
            };
            command = command.about(leak(&about));
            for arg in &plugin.args {
                command = command.arg(build_arg(arg));
            }
        }
        for (child, grandchild) in &node.children {
            command = command.subcommand(self.build(child, grandchild));
        }
        if !node.children.is_empty() {
            command = if node.entry.is_some() {
                command.args_conflicts_with_subcommands(true)
            } else {
                command
                    .subcommand_required(true)
                    .arg_required_else_help(true)
            };
        }
        command
    }

    /// If `matches` selected a plugin command, render its argv.
    ///
    /// `Err` carries a template error, which can only happen for a value the
    /// parse-time check could not see; it is reported rather than run.
    pub fn resolve(&self, matches: &ArgMatches) -> Option<Result<PluginInvocation, String>> {
        let tree = self.tree();
        let (name, mut sub) = matches.subcommand()?;
        let mut node = tree.get(name)?;
        let mut path = vec![name.to_string()];
        while let Some((child, child_matches)) = sub.subcommand() {
            let Some(next) = node.children.get(child) else {
                break;
            };
            node = next;
            sub = child_matches;
            path.push(child.to_string());
        }
        let plugin = &self.commands[node.entry?];
        let mut values = BTreeMap::new();
        for arg in &plugin.args {
            let value = if arg.flag {
                python_bool(sub.get_flag(&arg.name)).to_string()
            } else {
                sub.get_one::<String>(&arg.name)
                    .cloned()
                    .unwrap_or_default()
            };
            values.insert(arg.name.clone(), value);
        }
        Some(render_template(&plugin.run, &values).map(|argv| PluginInvocation { path, argv }))
    }
}

fn build_arg(arg: &PluginArg) -> Arg {
    let id = leak(&arg.name);
    // agenthicc: one trailing `_` lets a name dodge a keyword (`global_`
    // becomes `--global`); the rest of `snake_case` is hyphenated.
    let long = leak(
        &arg.name
            .strip_suffix('_')
            .unwrap_or(&arg.name)
            .replace('_', "-"),
    );
    if arg.flag {
        let mut built = Arg::new(id).long(long).action(ArgAction::SetTrue);
        if arg.default.as_deref() == Some("True") {
            built = built.default_value("true");
        }
        return built;
    }
    let value_name = leak(&arg.name.to_ascii_uppercase());
    match &arg.default {
        Some(default) => Arg::new(id)
            .long(long)
            .value_name(value_name)
            .default_value(leak(default))
            .action(ArgAction::Set),
        None => Arg::new(id)
            .value_name(value_name)
            .required(true)
            .action(ArgAction::Set),
    }
}

impl PluginInvocation {
    /// Run the program with inherited stdio; return its exit status.
    pub fn run(&self) -> i32 {
        let Some((program, args)) = self.argv.split_first() else {
            return 1;
        };
        match std::process::Command::new(program).args(args).status() {
            Ok(status) => exit_status_code(status),
            Err(e) => {
                eprintln!(
                    "xaft: could not run `{}` for plugin command `{}`: {e}",
                    program,
                    self.path.join(" ")
                );
                127
            }
        }
    }
}

#[cfg(unix)]
fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

#[cfg(not(unix))]
fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
[[command]]
path = ["deploy", "staging"]
help = "Deploy to staging"
run = "echo deploying {region} {ticket} dry={dry_run}"
args = [
  { name = "region", default = "eu-west-1" },
  { name = "dry_run", type = "bool" },
  { name = "ticket" },
]

[[command]]
path = ["hello"]
run = "echo hi"
"#;

    fn parsed() -> Vec<PluginCommand> {
        let (commands, warnings) =
            parse_cli_table(TABLE, Path::new("/p/.xaft/cli.toml"), PluginSource::Project);
        assert!(warnings.is_empty(), "{warnings:?}");
        commands
    }

    fn registry() -> PluginRegistry {
        PluginRegistry { commands: parsed() }
    }

    #[test]
    fn tables_parse_with_agenthicc_argument_inference() {
        let commands = parsed();
        assert_eq!(commands.len(), 2);
        let deploy = &commands[0];
        assert_eq!(deploy.path, vec!["deploy", "staging"]);
        assert_eq!(
            deploy.args[0],
            PluginArg {
                name: "region".into(),
                flag: false,
                default: Some("eu-west-1".into())
            }
        );
        assert!(deploy.args[1].flag);
        assert_eq!(deploy.args[2].default, None);
    }

    #[test]
    fn a_bad_table_is_reported_not_fatal() {
        let (commands, warnings) =
            parse_cli_table("not = [toml", Path::new("/x/cli.toml"), PluginSource::User);
        assert!(commands.is_empty());
        assert!(warnings[0].starts_with("bad cli.toml at /x/cli.toml"));

        let (commands, warnings) = parse_cli_table(
            "[[command]]\npath = [\"x\"]\nrun = \"echo {missing}\"\n",
            Path::new("/x/cli.toml"),
            PluginSource::User,
        );
        assert!(commands.is_empty());
        assert!(
            warnings[0].contains("unknown placeholder {missing}"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_table_without_a_path_is_skipped_silently() {
        let (commands, warnings) = parse_cli_table(
            "[[command]]\nrun = \"echo\"\n",
            Path::new("/x"),
            PluginSource::User,
        );
        assert!(commands.is_empty());
        assert!(warnings.is_empty());
    }

    #[test]
    fn shell_split_follows_posix_quoting() {
        assert_eq!(
            shell_split(r#"a 'b c' "d \"e\"" f\ g"#).unwrap(),
            vec!["a", "b c", "d \"e\"", "f g"]
        );
        assert_eq!(shell_split("  ").unwrap(), Vec::<String>::new());
        assert_eq!(shell_split("''").unwrap(), vec![""]);
        assert!(shell_split("'open").is_err());
    }

    #[test]
    fn a_value_is_always_exactly_one_argument() {
        let mut values = BTreeMap::new();
        values.insert("msg".to_string(), "hello world; rm -rf /".to_string());
        let argv = render_template("echo {msg}", &values).unwrap();
        assert_eq!(argv, vec!["echo", "hello world; rm -rf /"]);
    }

    #[test]
    fn braces_follow_python_format_escapes() {
        let values = BTreeMap::new();
        assert_eq!(
            render_template("echo {{literal}}", &values).unwrap(),
            vec!["echo", "{literal}"]
        );
        assert!(render_template("echo {}", &values).is_err());
        assert!(render_template("echo }", &values).is_err());
        assert!(render_template("echo {x!r}", &values).is_err());
    }

    fn cli() -> Command {
        Command::new("xaft").subcommand(Command::new("run"))
    }

    #[test]
    fn plugin_commands_parse_and_render_through_clap() {
        let command = registry().augment(cli());
        let matches = command
            .try_get_matches_from(["xaft", "deploy", "staging", "T-1", "--dry-run"])
            .unwrap();
        let invocation = registry().resolve(&matches).unwrap().unwrap();
        assert_eq!(invocation.path, vec!["deploy", "staging"]);
        assert_eq!(
            invocation.argv,
            vec!["echo", "deploying", "eu-west-1", "T-1", "dry=True"]
        );

        let matches = registry()
            .augment(cli())
            .try_get_matches_from(["xaft", "deploy", "staging", "T-2", "--region", "us"])
            .unwrap();
        let invocation = registry().resolve(&matches).unwrap().unwrap();
        assert_eq!(invocation.argv[2], "us");
        assert_eq!(invocation.argv[4], "dry=False");
    }

    #[test]
    fn a_missing_positional_is_a_clap_usage_error() {
        let err = registry()
            .augment(cli())
            .try_get_matches_from(["xaft", "deploy", "staging"])
            .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_builtin_invocation_is_not_a_plugin() {
        let matches = registry()
            .augment(cli())
            .try_get_matches_from(["xaft", "run"])
            .unwrap();
        assert!(registry().resolve(&matches).is_none());
    }

    #[test]
    fn the_group_badge_names_the_source() {
        let command = registry().augment(cli());
        let hello = command.find_subcommand("hello").unwrap();
        assert_eq!(hello.get_about().unwrap().to_string(), "[project]");
    }

    fn trusted_policy(dir: &Path) -> ExtensionTrust {
        ExtensionTrust::new(dir.join("home/trust.json"), vec![dir.join("home")])
    }

    fn write_project_table(dir: &Path, body: &str) -> PathBuf {
        let file = dir.join(PROJECT_CLI_PLUGINS);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, body).unwrap();
        file
    }

    #[test]
    fn an_untrusted_project_table_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        write_project_table(dir.path(), TABLE);
        let discovery =
            discover(dir.path(), None, &trusted_policy(dir.path()), &[], false).unwrap();
        assert!(discovery.registry.is_empty());
        assert!(discovery.warnings[0].starts_with("skipping untrusted cli extension"));
        assert!(discovery.warnings[0].contains("(not trusted)"));
    }

    #[test]
    fn a_trusted_project_table_is_loaded_until_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_project_table(dir.path(), TABLE);
        let trust = trusted_policy(dir.path());
        trust.trust(&file, "cli").unwrap();

        let discovery = discover(dir.path(), None, &trust, &[], false).unwrap();
        assert_eq!(discovery.registry.commands().len(), 2);
        assert!(discovery.warnings.is_empty());

        std::fs::write(&file, format!("{TABLE}\n# edited")).unwrap();
        let discovery = discover(dir.path(), None, &trust, &[], false).unwrap();
        assert!(discovery.registry.is_empty());
        assert!(discovery.warnings[0].contains("changed since it was trusted"));
    }

    #[test]
    fn a_user_table_is_trusted_by_location() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("home");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("cli.toml"), TABLE).unwrap();
        let discovery = discover(
            dir.path(),
            Some(&user),
            &trusted_policy(dir.path()),
            &[],
            false,
        )
        .unwrap();
        assert_eq!(discovery.registry.commands().len(), 2);
        assert_eq!(discovery.registry.commands()[0].source, PluginSource::User);
    }

    #[test]
    fn a_builtin_name_cannot_be_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("home");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("cli.toml"),
            "[[command]]\npath = [\"run\"]\nrun = \"echo pwned\"\n",
        )
        .unwrap();
        let builtins = builtin_names(&cli());
        let discovery = discover(
            dir.path(),
            Some(&user),
            &trusted_policy(dir.path()),
            &builtins,
            false,
        )
        .unwrap();
        assert!(discovery.registry.is_empty());
        assert!(discovery.warnings[0].contains("is a built-in xaft command"));
    }

    #[test]
    fn a_project_command_shadows_a_user_command_with_a_warning_or_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("home");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            user.join("cli.toml"),
            "[[command]]\npath = [\"hello\"]\nrun = \"echo user\"\n",
        )
        .unwrap();
        let file = write_project_table(
            dir.path(),
            "[[command]]\npath = [\"hello\"]\nrun = \"echo project\"\n",
        );
        let trust = trusted_policy(dir.path());
        trust.trust(&file, "cli").unwrap();

        let discovery = discover(dir.path(), Some(&user), &trust, &[], false).unwrap();
        assert_eq!(discovery.registry.commands().len(), 1);
        assert_eq!(discovery.registry.commands()[0].run, "echo project");
        assert!(discovery.warnings[0].contains("shadows"));

        let err = discover(dir.path(), Some(&user), &trust, &[], true).unwrap_err();
        assert!(err.contains("(project) shadows"));
    }

    #[cfg(unix)]
    #[test]
    fn a_plugin_runs_and_reports_the_child_exit_status() {
        let ok = PluginInvocation {
            path: vec!["t".into()],
            argv: vec!["true".into()],
        };
        assert_eq!(ok.run(), 0);
        let fail = PluginInvocation {
            path: vec!["t".into()],
            argv: vec!["sh".into(), "-c".into(), "exit 7".into()],
        };
        assert_eq!(fail.run(), 7);
        let missing = PluginInvocation {
            path: vec!["t".into()],
            argv: vec!["/definitely/not/a/program".into()],
        };
        assert_eq!(missing.run(), 127);
    }
}
