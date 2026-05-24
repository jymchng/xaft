//! Keybinding configuration parsing and registry.
//!
//! Key strings use the format `"[modifier+]key"`, e.g.:
//! - `"ctrl+n"` — Ctrl+N
//! - `"ctrl+shift+k"` — Ctrl+Shift+K
//! - `"alt+enter"` — Alt+Enter
//! - `"f1"` — F1
//! - `"space"` — Space bar

use std::collections::HashMap;

use crate::error::ConfigError;
use crate::types::{KeyAction, KeybindingConfig};

/// A parsed key event (modifier + code).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedKeyEvent {
    /// Key code.
    pub code: KeyCode,
    /// Modifier flags.
    pub modifiers: KeyModifiers,
}

/// Key code variants.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// A single character key.
    Char(char),
    /// Enter/Return.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Delete.
    Delete,
    /// Insert.
    Insert,
    /// Function key Fn where n ∈ [1, 12].
    F(u8),
}

impl std::fmt::Display for KeyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(c) => write!(f, "{c}"),
            Self::Enter => write!(f, "enter"),
            Self::Esc => write!(f, "esc"),
            Self::Tab => write!(f, "tab"),
            Self::Backspace => write!(f, "backspace"),
            Self::Up => write!(f, "up"),
            Self::Down => write!(f, "down"),
            Self::Left => write!(f, "left"),
            Self::Right => write!(f, "right"),
            Self::PageUp => write!(f, "pageup"),
            Self::PageDown => write!(f, "pagedown"),
            Self::Home => write!(f, "home"),
            Self::End => write!(f, "end"),
            Self::Delete => write!(f, "delete"),
            Self::Insert => write!(f, "insert"),
            Self::F(n) => write!(f, "f{n}"),
        }
    }
}

/// Modifier key flags.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers {
    /// Ctrl key held.
    pub ctrl: bool,
    /// Alt/Meta key held.
    pub alt: bool,
    /// Shift key held.
    pub shift: bool,
}

impl std::fmt::Display for ParsedKeyEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.modifiers.ctrl {
            write!(f, "ctrl+")?;
        }
        if self.modifiers.alt {
            write!(f, "alt+")?;
        }
        if self.modifiers.shift {
            write!(f, "shift+")?;
        }
        write!(f, "{}", self.code)
    }
}

/// Parser for key binding strings.
pub struct KeybindingParser;

impl KeybindingParser {
    /// Parse a key string like `"ctrl+shift+k"` into a `ParsedKeyEvent`.
    pub fn parse(s: &str) -> Result<ParsedKeyEvent, String> {
        let parts: Vec<&str> = s.split('+').collect();
        let mut modifiers = KeyModifiers::default();
        let mut code: Option<KeyCode> = None;

        for part in &parts {
            match part.to_lowercase().as_str() {
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" | "meta" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                key_str => {
                    if code.is_some() {
                        return Err(format!("multiple key codes in binding: '{s}'"));
                    }
                    code = Some(parse_key_code(key_str, s)?);
                }
            }
        }

        let code = code.ok_or_else(|| format!("no key code in binding: '{s}'"))?;
        Ok(ParsedKeyEvent { code, modifiers })
    }
}

fn parse_key_code(s: &str, original: &str) -> Result<KeyCode, String> {
    match s {
        "enter" | "return" => Ok(KeyCode::Enter),
        "esc" | "escape" => Ok(KeyCode::Esc),
        "tab" => Ok(KeyCode::Tab),
        "backspace" | "bs" => Ok(KeyCode::Backspace),
        "up" => Ok(KeyCode::Up),
        "down" => Ok(KeyCode::Down),
        "left" => Ok(KeyCode::Left),
        "right" => Ok(KeyCode::Right),
        "pageup" | "page_up" | "pgup" => Ok(KeyCode::PageUp),
        "pagedown" | "page_down" | "pgdn" | "pgdown" => Ok(KeyCode::PageDown),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        "delete" | "del" => Ok(KeyCode::Delete),
        "insert" | "ins" => Ok(KeyCode::Insert),
        "space" => Ok(KeyCode::Char(' ')),
        s if s.starts_with('f') => {
            let n: u8 = s[1..]
                .parse()
                .map_err(|_| format!("invalid function key in '{original}': '{s}'"))?;
            if !(1..=12).contains(&n) {
                return Err(format!("function key F{n} is out of range [F1-F12]"));
            }
            Ok(KeyCode::F(n))
        }
        s if s.len() == 1 => {
            let c = s.chars().next().unwrap();
            Ok(KeyCode::Char(c))
        }
        _ => Err(format!("unknown key '{s}' in binding '{original}'")),
    }
}

/// Registry mapping parsed key events to actions.
///
/// Built once from `KeybindingConfig` at startup and queried on each key press.
pub struct KeybindingRegistry {
    /// Forward map: key event → action.
    bindings: HashMap<ParsedKeyEvent, KeyAction>,
    /// Reverse map: action name → all bound keys.
    reverse: HashMap<String, Vec<ParsedKeyEvent>>,
}

impl KeybindingRegistry {
    /// Build a registry from a `KeybindingConfig`.
    ///
    /// Returns `Err` if any key string fails to parse.
    pub fn from_config(config: &KeybindingConfig) -> Result<Self, ConfigError> {
        let mut bindings = HashMap::new();
        let mut reverse: HashMap<String, Vec<ParsedKeyEvent>> = HashMap::new();

        for (key_str, action) in &config.bindings {
            let event = KeybindingParser::parse(key_str).map_err(|e| ConfigError::KeyParse {
                key: key_str.clone(),
                reason: e,
            })?;

            reverse
                .entry(action.action_name().to_string())
                .or_default()
                .push(event.clone());

            bindings.insert(event, action.clone());
        }

        Ok(Self { bindings, reverse })
    }

    /// Return the action bound to `key`, if any.
    pub fn lookup(&self, key: &ParsedKeyEvent) -> Option<&KeyAction> {
        self.bindings.get(key)
    }

    /// Return all keys bound to `action`.
    pub fn keys_for_action(&self, action: &str) -> &[ParsedKeyEvent] {
        self.reverse
            .get(action)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Total number of registered bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Return `true` if no bindings are registered.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::KeyAction;

    #[test]
    fn parse_ctrl_n() {
        let ev = KeybindingParser::parse("ctrl+n").unwrap();
        assert!(ev.modifiers.ctrl);
        assert!(!ev.modifiers.alt);
        assert_eq!(ev.code, KeyCode::Char('n'));
    }

    #[test]
    fn parse_ctrl_shift_k() {
        let ev = KeybindingParser::parse("ctrl+shift+k").unwrap();
        assert!(ev.modifiers.ctrl);
        assert!(ev.modifiers.shift);
        assert_eq!(ev.code, KeyCode::Char('k'));
    }

    #[test]
    fn parse_enter() {
        let ev = KeybindingParser::parse("enter").unwrap();
        assert_eq!(ev.code, KeyCode::Enter);
        assert!(!ev.modifiers.ctrl);
    }

    #[test]
    fn parse_alt_enter() {
        let ev = KeybindingParser::parse("alt+enter").unwrap();
        assert!(ev.modifiers.alt);
        assert_eq!(ev.code, KeyCode::Enter);
    }

    #[test]
    fn parse_f1_through_f12() {
        for n in 1u8..=12 {
            let ev = KeybindingParser::parse(&format!("f{n}")).unwrap();
            assert_eq!(ev.code, KeyCode::F(n));
        }
    }

    #[test]
    fn parse_f13_rejected() {
        assert!(KeybindingParser::parse("f13").is_err());
    }

    #[test]
    fn parse_unknown_key_rejected() {
        assert!(KeybindingParser::parse("ctrl+xyz_unknown_key").is_err());
    }

    #[test]
    fn parse_space() {
        let ev = KeybindingParser::parse("space").unwrap();
        assert_eq!(ev.code, KeyCode::Char(' '));
    }

    #[test]
    fn parse_pageup_variants() {
        assert_eq!(
            KeybindingParser::parse("pageup").unwrap().code,
            KeyCode::PageUp
        );
        assert_eq!(
            KeybindingParser::parse("pgup").unwrap().code,
            KeyCode::PageUp
        );
    }

    #[test]
    fn registry_lookup() {
        let mut bindings = std::collections::HashMap::new();
        bindings.insert("ctrl+q".to_string(), KeyAction::Single("quit".to_string()));
        let config = KeybindingConfig { bindings };

        let registry = KeybindingRegistry::from_config(&config).unwrap();
        let ev = KeybindingParser::parse("ctrl+q").unwrap();
        let action = registry.lookup(&ev).unwrap();
        assert_eq!(action.action_name(), "quit");
    }

    #[test]
    fn registry_reverse_lookup() {
        let mut bindings = std::collections::HashMap::new();
        bindings.insert("ctrl+q".to_string(), KeyAction::Single("quit".to_string()));
        bindings.insert(
            "ctrl+alt+q".to_string(),
            KeyAction::Single("quit".to_string()),
        );
        let config = KeybindingConfig { bindings };

        let registry = KeybindingRegistry::from_config(&config).unwrap();
        let keys = registry.keys_for_action("quit");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn registry_invalid_key_errors() {
        let mut bindings = std::collections::HashMap::new();
        bindings.insert(
            "ctrl+BADKEY".to_string(),
            KeyAction::Single("quit".to_string()),
        );
        let config = KeybindingConfig { bindings };

        assert!(KeybindingRegistry::from_config(&config).is_err());
    }

    #[test]
    fn key_event_display() {
        let ev = ParsedKeyEvent {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        };
        assert_eq!(ev.to_string(), "ctrl+n");
    }
}
