use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    Top,
    Bottom,
    NextView,
    PrevView,
    ToggleLayout,
    Open,
    Search,
    Filter,
    Refresh,
    OpenBrowser,
    Help,
    Back,
    Quit,
    PickBoard,
    ToggleGroup,
    ToggleRaw,
    LoadOlder,
}

impl Action {
    pub const ALL: [Action; 21] = [
        Action::Up,
        Action::Down,
        Action::Left,
        Action::Right,
        Action::Top,
        Action::Bottom,
        Action::NextView,
        Action::PrevView,
        Action::ToggleLayout,
        Action::Open,
        Action::Search,
        Action::Filter,
        Action::Refresh,
        Action::OpenBrowser,
        Action::Help,
        Action::Back,
        Action::Quit,
        Action::PickBoard,
        Action::ToggleGroup,
        Action::ToggleRaw,
        Action::LoadOlder,
    ];

    /// The name used in `[keys]` of config.toml.
    pub fn name(&self) -> &'static str {
        match self {
            Action::Up => "up",
            Action::Down => "down",
            Action::Left => "left",
            Action::Right => "right",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::NextView => "next_view",
            Action::PrevView => "prev_view",
            Action::ToggleLayout => "toggle_layout",
            Action::Open => "open",
            Action::Search => "search",
            Action::Filter => "filter",
            Action::Refresh => "refresh",
            Action::OpenBrowser => "open_browser",
            Action::Help => "help",
            Action::Back => "back",
            Action::Quit => "quit",
            Action::PickBoard => "pick_board",
            Action::ToggleGroup => "toggle_group",
            Action::ToggleRaw => "toggle_raw",
            Action::LoadOlder => "load_older",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Action::Up => "move up",
            Action::Down => "move down",
            Action::Left => "previous column",
            Action::Right => "next column",
            Action::Top => "first row or card",
            Action::Bottom => "last row or card",
            Action::NextView => "next view (next link in detail)",
            Action::PrevView => "previous view (previous link in detail)",
            Action::ToggleLayout => "switch table / board",
            Action::Open => "open item detail, collapse group, follow link",
            Action::Search => "quick search",
            Action::Filter => "add a filter on top of the view's GitHub filter",
            Action::Refresh => "refresh now",
            Action::OpenBrowser => "open in the browser",
            Action::Help => "this help",
            Action::Back => "back / cancel",
            Action::Quit => "quit",
            Action::PickBoard => "switch board",
            Action::ToggleGroup => "collapse or expand the group",
            Action::ToggleRaw => "detail: rendered / raw text",
            Action::LoadOlder => "detail: load older comments",
        }
    }

    fn from_name(name: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeySpec {
    /// Parses `j`, `J`, `?`, `enter`, `esc`, `tab`, `shift+tab`, `space`, `home`, `end`,
    /// `up`/`down`/`left`/`right`, `pageup`/`pagedown`, `backspace`, and `ctrl+`/`alt+` prefixes.
    pub fn parse(s: &str) -> Result<KeySpec, String> {
        let mut mods = KeyModifiers::NONE;
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("ctrl+") {
                mods |= KeyModifiers::CONTROL;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("alt+") {
                mods |= KeyModifiers::ALT;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("shift+") {
                mods |= KeyModifiers::SHIFT;
                rest = r;
            } else {
                break;
            }
        }
        let code = match rest {
            "enter" => KeyCode::Enter,
            "esc" => KeyCode::Esc,
            "tab" if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "backspace" => KeyCode::Backspace,
            single if single.chars().count() == 1 => KeyCode::Char(single.chars().next().unwrap()),
            _ => return Err(format!("unknown key `{s}`")),
        };
        Ok(KeySpec {
            code,
            mods: mods & (KeyModifiers::CONTROL | KeyModifiers::ALT),
        })
    }

    /// Characters match by the character itself (terminals differ on reporting SHIFT for `J`);
    /// Ctrl and Alt must match exactly.
    pub fn matches(&self, key: &KeyEvent) -> bool {
        let relevant = |m: KeyModifiers| m & (KeyModifiers::CONTROL | KeyModifiers::ALT);
        self.code == key.code && relevant(self.mods) == relevant(key.modifiers)
    }

    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "enter".into(),
            KeyCode::Esc => "esc".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::BackTab => "shift+tab".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::PageUp => "pageup".into(),
            KeyCode::PageDown => "pagedown".into(),
            KeyCode::Backspace => "backspace".into(),
            other => format!("{other:?}").to_lowercase(),
        };
        let mut label = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            label.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            label.push_str("alt+");
        }
        label + &base
    }
}

pub struct Keymap {
    bindings: Vec<(KeySpec, Action)>,
}

const DEFAULTS: &[(&str, Action)] = &[
    ("k", Action::Up),
    ("up", Action::Up),
    ("j", Action::Down),
    ("down", Action::Down),
    ("h", Action::Left),
    ("left", Action::Left),
    ("l", Action::Right),
    ("right", Action::Right),
    ("home", Action::Top),
    ("end", Action::Bottom),
    ("tab", Action::NextView),
    ("shift+tab", Action::PrevView),
    ("L", Action::ToggleLayout),
    ("enter", Action::Open),
    ("/", Action::Search),
    ("f", Action::Filter),
    ("r", Action::Refresh),
    ("o", Action::OpenBrowser),
    ("?", Action::Help),
    ("esc", Action::Back),
    ("q", Action::Quit),
    ("B", Action::PickBoard),
    ("z", Action::ToggleGroup),
    ("m", Action::ToggleRaw),
    ("P", Action::LoadOlder),
];

impl Keymap {
    /// Spec section 6 defaults. None of them uses a Ctrl chord.
    pub fn defaults() -> Self {
        Self {
            bindings: DEFAULTS
                .iter()
                .map(|(k, a)| (KeySpec::parse(k).expect("default key parses"), *a))
                .collect(),
        }
    }

    /// Each `[keys]` entry replaces every default key of that action. Unknown actions and
    /// unparseable keys are reported and skipped.
    pub fn with_overrides(overrides: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut map = Self::defaults();
        let mut warnings = Vec::new();
        for (name, key) in overrides {
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("[keys] {name}: no such action"));
                continue;
            };
            match KeySpec::parse(key) {
                Ok(spec) => {
                    map.bindings.retain(|(_, a)| *a != action);
                    if let Some(old) = map.bindings.iter().find(|(k, _)| *k == spec).map(|b| b.1) {
                        map.bindings.retain(|(k, _)| *k != spec);
                        warnings.push(format!(
                            "[keys] {name}: {} was bound to {}; it now triggers {name}",
                            spec.label(),
                            old.name()
                        ));
                    }
                    map.bindings.push((spec, action));
                }
                Err(e) => warnings.push(format!("[keys] {name}: {e}")),
            }
        }
        (map, warnings)
    }

    pub fn action(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(spec, _)| spec.matches(key))
            .map(|(_, a)| *a)
    }

    pub fn keys_for(&self, action: Action) -> Vec<KeySpec> {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(k, _)| *k)
            .collect()
    }

    pub fn bindings(&self) -> &[(KeySpec, Action)] {
        &self.bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn defaults_have_no_ctrl_chords() {
        assert!(
            Keymap::defaults()
                .bindings()
                .iter()
                .all(|(k, _)| !k.mods.contains(KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn shifted_letters_match_with_or_without_the_shift_flag() {
        let km = Keymap::defaults();
        assert_eq!(
            km.action(&ev(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            Some(Action::ToggleLayout)
        );
        assert_eq!(
            km.action(&ev(KeyCode::Char('L'), KeyModifiers::NONE)),
            Some(Action::ToggleLayout)
        );
        assert_eq!(
            km.action(&ev(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(Action::Right)
        );
        assert_eq!(
            km.action(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Action::PrevView)
        );
        assert_eq!(
            km.action(&ev(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn overrides_replace_defaults_and_report_problems() {
        let mut o = BTreeMap::new();
        o.insert("next_view".to_string(), "]".to_string());
        o.insert("teleport".to_string(), "t".to_string());
        o.insert("quit".to_string(), "ctrl+".to_string());
        let (km, warnings) = Keymap::with_overrides(&o);
        assert_eq!(
            km.keys_for(Action::NextView),
            vec![KeySpec::parse("]").unwrap()]
        );
        assert_eq!(km.action(&ev(KeyCode::Tab, KeyModifiers::NONE)), None);
        assert_eq!(
            km.keys_for(Action::Quit).len(),
            1,
            "a bad override leaves the default"
        );
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn override_stealing_a_default_key_takes_effect_with_a_warning() {
        let mut o = BTreeMap::new();
        o.insert("help".to_string(), "k".to_string());
        let (km, warnings) = Keymap::with_overrides(&o);
        assert_eq!(
            km.action(&ev(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(Action::Help)
        );
        assert_eq!(
            km.action(&ev(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::Up)
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn esc_goes_back_and_q_quits() {
        let km = Keymap::defaults();
        assert_eq!(
            km.action(&ev(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Action::Back)
        );
        assert_eq!(
            km.action(&ev(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn labels_round_trip_through_parse() {
        for (spec, _) in Keymap::defaults().bindings() {
            if matches!(
                spec.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
            ) {
                continue;
            }
            assert_eq!(KeySpec::parse(&spec.label()).unwrap(), *spec);
        }
    }
}
