//! Command templates that are split into argv *before* they are filled.
//!
//! Both xaft's CLI plugins (`.xaft/cli.toml`, `run = "..."`) and its
//! persistent custom tools (`.xaft/tools/<name>.toml`, `command = "..."`)
//! declare a program as one templated line such as
//! `git log -n {count} -- {path}`. agenthicc formats such a string and then
//! `shlex.split`s the result, so a value containing a space becomes two
//! arguments and a value containing a quote aborts the command. Here the
//! template is split first, POSIX-shell style, and each `{name}` placeholder is
//! replaced inside its word: **every value is exactly one argument** and can
//! never inject a flag or a second command. No shell is involved at any point.
//!
//! ```
//! use std::collections::BTreeMap;
//! use xaft_tools::command_template::render_argv;
//!
//! let values = BTreeMap::from([("msg".to_string(), "a b; rm -rf /".to_string())]);
//! let argv = render_argv("echo {msg}", &values, "command").unwrap();
//! assert_eq!(argv, ["echo", "a b; rm -rf /"]);
//! ```

use std::collections::BTreeMap;

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

/// Whether `name` can be a `{placeholder}`: an identifier.
pub fn valid_placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Render a template into argv.
///
/// The template is split first, then each `{name}` placeholder is replaced
/// inside its word. `{{` and `}}` are literal braces, as in Python's
/// `str.format`. An unknown placeholder, a positional `{}` or a format spec is
/// an error. `key` names the template's TOML key (`run`, `command`) in the
/// error messages.
pub fn render_argv(
    template: &str,
    values: &BTreeMap<String, String>,
    key: &str,
) -> Result<Vec<String>, String> {
    let words = shell_split(template)?;
    if words.is_empty() {
        return Err(format!("`{key}` renders to an empty command"));
    }
    words
        .iter()
        .map(|word| substitute(word, values, key))
        .collect()
}

/// The `{placeholder}` names in one word of a template, in order.
pub fn placeholders(word: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut chars = word.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("unterminated `{`".into()),
                    }
                }
                names.push(name);
            }
            _ => {}
        }
    }
    Ok(names)
}

fn substitute(word: &str, values: &BTreeMap<String, String>, key: &str) -> Result<String, String> {
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
                        None => return Err(format!("unterminated `{{` in {key} template")),
                    }
                }
                if name.is_empty() {
                    return Err(format!(
                        "positional `{{}}` is not supported in {key} templates"
                    ));
                }
                if !valid_placeholder_name(&name) {
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
            '}' => return Err(format!("single `}}` encountered in {key} template")),
            c => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let values = BTreeMap::from([("msg".to_string(), "two words; ls".to_string())]);
        let argv = render_argv("echo --text={msg} {msg}", &values, "command").unwrap();
        assert_eq!(argv, ["echo", "--text=two words; ls", "two words; ls"]);
    }

    #[test]
    fn braces_follow_python_format_escapes_and_name_the_key() {
        let values = BTreeMap::new();
        assert_eq!(
            render_argv("echo {{literal}}", &values, "run").unwrap(),
            ["echo", "{literal}"]
        );
        assert!(render_argv("echo {}", &values, "run").is_err());
        assert!(render_argv("echo }", &values, "run").is_err());
        assert!(render_argv("echo {x!r}", &values, "run").is_err());
        assert_eq!(
            render_argv("  ", &values, "command").unwrap_err(),
            "`command` renders to an empty command"
        );
        assert_eq!(
            render_argv("echo {missing}", &values, "command").unwrap_err(),
            "unknown placeholder {missing}"
        );
    }

    #[test]
    fn placeholders_are_listed_per_word() {
        assert_eq!(
            placeholders("--from={a}..{b}").unwrap(),
            ["a".to_string(), "b".to_string()]
        );
        assert!(placeholders("{{x}}").unwrap().is_empty());
        assert!(placeholders("{open").is_err());
    }
}
