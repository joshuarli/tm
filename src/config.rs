use crate::keys::{self, KeyCode};

pub struct Config {
    pub prefix: KeyCode,
    pub escape_time: u64,
    pub mouse: bool,
    pub history_limit: u32,
    pub base_index: u32,
    pub renumber_windows: bool,
    pub focus_events: bool,
    pub status_position: StatusPosition,
    pub status_bg: crate::grid::Color,
    pub status_fg: crate::grid::Color,
    pub repeat_time: u64,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusPosition {
    Bottom,
    Top,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub key: KeyCode,
    pub action: Action,
    pub repeat: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Detach,
    NewWindow,
    RenameWindow,
    NextWindow,
    PrevWindow,
    SwapWindowLeft,
    SwapWindowRight,
    SelectWindow(u8),
    SplitH,
    SplitV,
    KillPane,
    ZoomPane,
    FocusUp,
    FocusDown,
    FocusLeft,
    FocusRight,
    SelectPane(u8),
    MovePaneToWindow,
    BreakPane,
    CopyMode,
    CommandPrompt,
    ReloadConfig,
    SendPrefix,
}

impl Config {
    pub fn default_config() -> Self {
        let mut c = Self {
            prefix: KeyCode::ctrl('a'),
            escape_time: 0,
            mouse: true,
            history_limit: 10000,
            base_index: 1,
            renumber_windows: true,
            focus_events: true,
            status_position: StatusPosition::Bottom,
            status_bg: crate::grid::Color::Default,
            status_fg: crate::grid::Color::Default,
            repeat_time: 500,
            bindings: Vec::new(),
        };
        c.bindings = default_bindings();
        c
    }

    /// Load config from file, falling back to defaults.
    pub fn load() -> Self {
        let mut config = Self::default_config();

        let path = config_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return config,
        };

        parse_config(&content, &mut config);
        config
    }

    /// Reload: reset to defaults, then re-apply config file.
    pub fn reload(&mut self) {
        *self = Self::default_config();
        let path = config_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            parse_config(&content, self);
        }
    }

    pub fn find_binding(&self, key: KeyCode) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.key == key)
    }
}

fn config_path() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".config/tm/tm.conf")
    } else {
        std::path::PathBuf::from("/dev/null")
    }
}

fn default_bindings() -> Vec<Binding> {
    use Action::*;
    vec![
        bind(KeyCode(KeyCode::ENTER), ReloadConfig, false),
        bind(KeyCode::char('d'), Detach, false),
        bind(KeyCode::ctrl('s'), SendPrefix, false),
        bind(KeyCode::char('/'), CommandPrompt, false),
        bind(KeyCode::char('n'), NewWindow, false),
        bind(KeyCode::char('r'), RenameWindow, false),
        bind(KeyCode(KeyCode::LEFT | KeyCode::CTRL), PrevWindow, true),
        bind(KeyCode(KeyCode::RIGHT | KeyCode::CTRL), NextWindow, true),
        bind(KeyCode(KeyCode::LEFT | KeyCode::SHIFT), SwapWindowLeft, true),
        bind(KeyCode(KeyCode::RIGHT | KeyCode::SHIFT), SwapWindowRight, true),
        bind(KeyCode::char('\\'), SplitH, false),
        bind(KeyCode::char('-'), SplitV, false),
        bind(KeyCode::char('z'), ZoomPane, false),
        bind(KeyCode::char('k'), KillPane, false),
        bind(KeyCode::char('m'), MovePaneToWindow, false),
        bind(KeyCode::ctrl('n'), BreakPane, true),
        bind(KeyCode(KeyCode::UP), FocusUp, false),
        bind(KeyCode(KeyCode::DOWN), FocusDown, false),
        bind(KeyCode(KeyCode::LEFT), FocusLeft, false),
        bind(KeyCode(KeyCode::RIGHT), FocusRight, false),
        // Window selection 1-9
        bind(KeyCode::char('1'), SelectWindow(1), false),
        bind(KeyCode::char('2'), SelectWindow(2), false),
        bind(KeyCode::char('3'), SelectWindow(3), false),
        bind(KeyCode::char('4'), SelectWindow(4), false),
        bind(KeyCode::char('5'), SelectWindow(5), false),
        bind(KeyCode::char('6'), SelectWindow(6), false),
        bind(KeyCode::char('7'), SelectWindow(7), false),
        bind(KeyCode::char('8'), SelectWindow(8), false),
        bind(KeyCode::char('9'), SelectWindow(9), false),
    ]
}

fn bind(key: KeyCode, action: Action, repeat: bool) -> Binding {
    Binding { key, action, repeat }
}

fn parse_config(content: &str, config: &mut Config) {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, ' ').collect();
        match parts.first().copied() {
            Some("set") if parts.len() >= 3 => {
                parse_set(parts[1], parts[2], config);
            }
            Some("bind") => {
                parse_bind(&parts[1..], config);
            }
            _ => {}
        }
    }
}

fn parse_set(key: &str, value: &str, config: &mut Config) {
    match key {
        "prefix" => {
            if let Some(k) = keys::parse_key_name(value) {
                config.prefix = k;
            }
        }
        "escape-time" => {
            if let Ok(v) = value.parse() {
                config.escape_time = v;
            }
        }
        "mouse" => config.mouse = value == "on",
        "history-limit" => {
            if let Ok(v) = value.parse() {
                config.history_limit = v;
            }
        }
        "base-index" => {
            if let Ok(v) = value.parse() {
                config.base_index = v;
            }
        }
        "renumber-windows" => config.renumber_windows = value == "on",
        "focus-events" => config.focus_events = value == "on",
        "status-position" => {
            config.status_position = if value == "top" {
                StatusPosition::Top
            } else {
                StatusPosition::Bottom
            };
        }
        "status-bg" => config.status_bg = parse_color(value),
        "status-fg" => config.status_fg = parse_color(value),
        "repeat-time" => {
            if let Ok(v) = value.parse() {
                config.repeat_time = v;
            }
        }
        _ => {}
    }
}

fn parse_bind(parts: &[&str], config: &mut Config) {
    if parts.is_empty() {
        return;
    }

    let mut repeat = false;
    let mut idx = 0;

    if parts.get(idx) == Some(&"-r") {
        repeat = true;
        idx += 1;
    }

    let key_name = match parts.get(idx) {
        Some(k) => *k,
        None => return,
    };
    idx += 1;

    let action_name = match parts.get(idx) {
        Some(a) => *a,
        None => return,
    };

    let key = match keys::parse_key_name(key_name) {
        Some(k) => k,
        None => return,
    };

    let action = match parse_action(action_name) {
        Some(a) => a,
        None => return,
    };

    // Remove existing binding for this key
    config.bindings.retain(|b| b.key != key);
    config.bindings.push(Binding { key, action, repeat });
}

fn parse_action(name: &str) -> Option<Action> {
    use Action::*;
    match name {
        "detach" => Some(Detach),
        "new-window" => Some(NewWindow),
        "rename-window" => Some(RenameWindow),
        "next-window" => Some(NextWindow),
        "prev-window" => Some(PrevWindow),
        "swap-window-left" => Some(SwapWindowLeft),
        "swap-window-right" => Some(SwapWindowRight),
        "split-h" => Some(SplitH),
        "split-v" => Some(SplitV),
        "kill-pane" => Some(KillPane),
        "zoom-pane" => Some(ZoomPane),
        "focus-up" => Some(FocusUp),
        "focus-down" => Some(FocusDown),
        "focus-left" => Some(FocusLeft),
        "focus-right" => Some(FocusRight),
        "move-pane" => Some(MovePaneToWindow),
        "break-pane" => Some(BreakPane),
        "copy-mode" => Some(CopyMode),
        "command-prompt" => Some(CommandPrompt),
        "reload-config" => Some(ReloadConfig),
        "send-prefix" => Some(SendPrefix),
        _ => None,
    }
}

fn parse_color(s: &str) -> crate::grid::Color {
    match s {
        "default" => crate::grid::Color::Default,
        "black" => crate::grid::Color::Palette(0),
        "red" => crate::grid::Color::Palette(1),
        "green" => crate::grid::Color::Palette(2),
        "yellow" => crate::grid::Color::Palette(3),
        "blue" => crate::grid::Color::Palette(4),
        "magenta" => crate::grid::Color::Palette(5),
        "cyan" => crate::grid::Color::Palette(6),
        "white" => crate::grid::Color::Palette(7),
        _ => {
            if let Ok(n) = s.parse::<u8>() {
                crate::grid::Color::Palette(n)
            } else {
                crate::grid::Color::Default
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default_config();
        assert_eq!(config.prefix, KeyCode::ctrl('a'));
        assert!(config.mouse);
        assert_eq!(config.history_limit, 10000);
        assert!(!config.bindings.is_empty());
    }

    #[test]
    fn test_parse_config() {
        let mut config = Config::default_config();
        parse_config(
            "set prefix C-b\nset mouse off\nbind x detach\n",
            &mut config,
        );
        assert_eq!(config.prefix, KeyCode::ctrl('b'));
        assert!(!config.mouse);
        assert!(config.find_binding(KeyCode::char('x')).is_some());
    }
}
