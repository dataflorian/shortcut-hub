//! Native JSON/YAML store, Windows master parser, built-in fallback catalog, and PowerToys sync.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::hotkey::{parse_ahk_hotkey, parse_display_hotkey, HotkeyAction, HotkeyBinding};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeysConfig {
    pub toggle_overlay: String,
    pub toggle_overlay_display: String,
    pub toggle_overlay_alt: String,
    pub toggle_overlay_alt_display: String,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            toggle_overlay: "^!k".to_string(),
            toggle_overlay_display: "Ctrl+Alt+K".to_string(),
            toggle_overlay_alt: "#F7".to_string(),
            toggle_overlay_alt_display: "Win+F7".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerToysConfig {
    pub master_shell_yml: String,
    pub user_shortcuts_dir: String,
    pub index_generator_exe: String,
    pub custom_manifest_name: String,
    pub sync_enabled: bool,
}

impl Default for PowerToysConfig {
    fn default() -> Self {
        Self {
            master_shell_yml: "C:\\Program Files\\PowerToys\\WinUI3Apps\\Assets\\ShortcutGuide\\Manifests\\+WindowsNT.Shell.en-US.yml".to_string(),
            user_shortcuts_dir: "%LOCALAPPDATA%\\Microsoft\\WinGet\\KeyboardShortcuts".to_string(),
            index_generator_exe: "C:\\Program Files\\PowerToys\\WinUI3Apps\\PowerToys.ShortcutGuide.IndexYmlGenerator.exe".to_string(),
            custom_manifest_name: "+Custom.MyShortcuts.en-US.yml".to_string(),
            sync_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub hotkeys: HotkeysConfig,
    pub powertoys: PowerToysConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShortcutItem {
    pub id: String,
    pub category: String,
    pub keys: String,
    pub name: String,
    pub description: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action_target: String,
    #[serde(skip)]
    pub hidden: bool,
    #[serde(skip)]
    pub highlighted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuickNote {
    pub id: String,
    pub category: String,
    pub trigger: String,
    pub content: String,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserState {
    pub manual_shortcuts: Vec<ShortcutItem>,
    pub hidden_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlighted_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quick_notes: Vec<QuickNote>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scratchpad: String,
    pub discovered_cache: Vec<ShortcutItem>,
    pub last_scan_utc: String,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogStats {
    pub active_total: usize,
    pub hidden_total: usize,
    pub highlighted: usize,
    pub manual: usize,
    pub auto: usize,
    pub powertoys: usize,
    pub windows: usize,
    pub windows_hidden: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub active: Vec<ShortcutItem>,
    pub hidden: Vec<ShortcutItem>,
    pub custom_categories: Vec<String>,
    pub quick_notes: Vec<QuickNote>,
    pub scratchpad: String,
    pub stats: CatalogStats,
}

pub fn expand_env_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '%') {
                let var_name: String = chars[i + 1..i + 1 + rel_end].iter().collect();
                if !var_name.is_empty() {
                    if let Ok(val) = std::env::var(&var_name) {
                        out.push_str(&val);
                        i += rel_end + 2;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

pub fn resolve_project_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(|p| p.to_path_buf());
        while let Some(dir) = cur {
            if dir.join("config.json").is_file() || dir.join("config.local.json").is_file() {
                return dir;
            }
            cur = dir.parent().map(|p| p.to_path_buf());
        }
        if let Some(parent) = exe.parent() {
            return parent.to_path_buf();
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur = Some(cwd);
        while let Some(dir) = cur {
            if dir.join("config.json").is_file() || dir.join("config.local.json").is_file() {
                return dir;
            }
            cur = dir.parent().map(|p| p.to_path_buf());
        }
    }
    PathBuf::from(".")
}

fn merge_json_values(base: &mut Value, override_val: Value) {
    match (base, override_val) {
        (Value::Object(base_map), Value::Object(ovr_map)) => {
            for (k, v) in ovr_map {
                match base_map.get_mut(&k) {
                    Some(existing) if existing.is_object() && v.is_object() => {
                        merge_json_values(existing, v);
                    }
                    _ => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (base_slot, ovr) => {
            *base_slot = ovr;
        }
    }
}

pub fn load_config(project_dir: &Path) -> AppConfig {
    let mut merged = serde_json::Map::new();
    let base_path = project_dir.join("config.json");
    if let Ok(raw) = fs::read_to_string(&base_path) {
        let clean = raw.trim_start_matches('\u{feff}');
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(clean) {
            merged = map;
        }
    }

    let mut base_val = Value::Object(merged);
    let local_path = project_dir.join("config.local.json");
    if let Ok(raw) = fs::read_to_string(&local_path) {
        let clean = raw.trim_start_matches('\u{feff}');
        if let Ok(local_val) = serde_json::from_str::<Value>(clean) {
            merge_json_values(&mut base_val, local_val);
        }
    }

    serde_json::from_value::<AppConfig>(base_val).unwrap_or_default()
}

pub fn load_state(project_dir: &Path) -> UserState {
    let path = project_dir.join("state").join("user_shortcuts.json");
    if let Ok(raw) = fs::read_to_string(&path) {
        let clean = raw.trim_start_matches('\u{feff}');
        if let Ok(state) = serde_json::from_str::<UserState>(clean) {
            return state;
        }
    }
    UserState::default()
}

pub fn save_state(project_dir: &Path, state: &UserState) -> Result<(), String> {
    let state_dir = project_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let target = state_dir.join("user_shortcuts.json");
    let tmp = state_dir.join(format!("user_shortcuts.json.tmp.{}", std::process::id()));
    let json = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    fs::write(&tmp, format!("{json}\n")).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &target).map_err(|e| e.to_string())
}

fn unquote_yaml(raw: &str) -> String {
    let s = raw.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if (bytes[0] == b'"' && bytes[s.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[s.len() - 1] == b'\'')
        {
            let inner = &s[1..s.len() - 1];
            return if bytes[0] == b'"' {
                inner.replace("\\\"", "\"").replace("\\\\", "\\")
            } else {
                inner.replace("''", "'")
            };
        }
    }
    s.to_string()
}

fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_us = false;
    for ch in text.chars() {
        let low = ch.to_ascii_lowercase();
        if low.is_ascii_alphanumeric() {
            out.push(low);
            prev_us = false;
        } else if !prev_us && !out.is_empty() {
            out.push('_');
            prev_us = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "item".to_string()
    } else {
        out
    }
}

fn clean_section_title(raw: &str) -> String {
    let mut s = unquote_yaml(raw);
    if s.starts_with('<') {
        if let Some(end) = s.find('>') {
            s = s[end + 1..].trim().to_string();
        }
    }
    if s.is_empty() {
        "General".to_string()
    } else {
        s
    }
}

fn vk_to_key_name(code: u32) -> String {
    match code {
        8 => "Backspace".into(),
        9 => "Tab".into(),
        13 => "Enter".into(),
        19 => "Pause".into(),
        20 => "CapsLock".into(),
        27 => "Esc".into(),
        32 => "Space".into(),
        33 => "Page Up".into(),
        34 => "Page Down".into(),
        35 => "End".into(),
        36 => "Home".into(),
        37 => "Left".into(),
        38 => "Up".into(),
        39 => "Right".into(),
        40 => "Down".into(),
        44 => "PrtScr".into(),
        45 => "Insert".into(),
        46 => "Delete".into(),
        91 => "Win".into(),
        92 => "RWin".into(),
        186 => ";".into(),
        187 => "+".into(),
        188 => ",".into(),
        189 => "-".into(),
        190 => ".".into(),
        191 => "/".into(),
        192 => "`".into(),
        219 => "[".into(),
        220 => "\\".into(),
        221 => "]".into(),
        222 => "'".into(),
        226 => "<".into(),
        48..=57 => ((code as u8) as char).to_string(),
        65..=90 => ((code as u8) as char).to_string(),
        112..=135 => format!("F{}", code - 111),
        other => format!("VK_{other}"),
    }
}

fn format_key_token(raw: &str) -> String {
    let tok = unquote_yaml(raw);
    if tok.len() >= 2 && tok.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(num) = tok.parse::<u32>() {
            return vk_to_key_name(num);
        }
    }
    if tok.starts_with('<') && tok.ends_with('>') && tok.len() > 2 {
        return tok[1..tok.len() - 1].to_string();
    }
    tok
}

/// Fast lossless-ID-compatible parser for PowerToys `+WindowsNT.Shell.en-US.yml`.
pub fn parse_windows_master_yaml(path: &Path) -> Vec<ShortcutItem> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut items = Vec::with_capacity(96);
    let mut seen_ids: HashMap<String, usize> = HashMap::new();
    let mut in_shortcuts = false;
    let mut cur_section = String::from("General");

    let mut prop_active = false;
    let mut prop_name = String::new();
    let mut prop_desc = String::new();
    let mut combos: Vec<String> = Vec::new();
    let mut cur_mods = [false; 4]; // Win, Ctrl, Alt, Shift
    let mut cur_keys: Vec<String> = Vec::new();
    let mut in_shortcut = false;
    let mut in_keys = false;

    let flush_combo =
        |cur_mods: &mut [bool; 4], cur_keys: &mut Vec<String>, combos: &mut Vec<String>| {
            let mut parts: Vec<String> = Vec::new();
            for (idx, label) in ["Win", "Ctrl", "Alt", "Shift"].iter().enumerate() {
                if cur_mods[idx] {
                    parts.push((*label).to_string());
                }
            }
            parts.append(cur_keys);
            if !parts.is_empty() {
                let combo = parts.join("+");
                if !combos.contains(&combo) {
                    combos.push(combo);
                }
            }
            *cur_mods = [false; 4];
        };

    let flush_prop = |prop_active: &mut bool,
                      prop_name: &mut String,
                      prop_desc: &mut String,
                      combos: &mut Vec<String>,
                      cur_mods: &mut [bool; 4],
                      cur_keys: &mut Vec<String>,
                      cur_section: &str,
                      seen_ids: &mut HashMap<String, usize>,
                      items: &mut Vec<ShortcutItem>| {
        if !*prop_active {
            return;
        }
        flush_combo(cur_mods, cur_keys, combos);
        let keys_str = combos.join(" / ");
        if !keys_str.is_empty() {
            let base_id = format!(
                "win::{}::{}::{}",
                slug(cur_section),
                slug(prop_name),
                slug(&keys_str)
            );
            let cnt = seen_ids.entry(base_id.clone()).or_insert(0);
            *cnt += 1;
            let item_id = if *cnt == 1 {
                base_id
            } else {
                format!("{base_id}_{cnt}")
            };
            items.push(ShortcutItem {
                id: item_id,
                category: cur_section.to_string(),
                keys: keys_str,
                name: std::mem::take(prop_name),
                description: std::mem::take(prop_desc),
                source: "windows".to_string(),
                ..Default::default()
            });
        }
        prop_name.clear();
        prop_desc.clear();
        combos.clear();
        *prop_active = false;
    };

    for line in raw.lines() {
        let stripped = line.trim();
        if !in_shortcuts {
            if stripped == "Shortcuts:" {
                in_shortcuts = true;
            }
            continue;
        }
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }

        if let Some(rest) = stripped.strip_prefix("- SectionName:") {
            flush_prop(
                &mut prop_active,
                &mut prop_name,
                &mut prop_desc,
                &mut combos,
                &mut cur_mods,
                &mut cur_keys,
                &cur_section,
                &mut seen_ids,
                &mut items,
            );
            in_shortcut = false;
            in_keys = false;
            cur_section = clean_section_title(rest);
            continue;
        }

        let leading_spaces = line.len() - line.trim_start().len();
        if (4..=8).contains(&leading_spaces) {
            if let Some(rest) = stripped.strip_prefix("- Name:") {
                flush_prop(
                    &mut prop_active,
                    &mut prop_name,
                    &mut prop_desc,
                    &mut combos,
                    &mut cur_mods,
                    &mut cur_keys,
                    &cur_section,
                    &mut seen_ids,
                    &mut items,
                );
                prop_active = true;
                prop_name = unquote_yaml(rest);
                in_shortcut = false;
                in_keys = false;
                continue;
            }
        }

        if !prop_active {
            continue;
        }

        if let Some(rest) = stripped.strip_prefix("Description:") {
            prop_desc = unquote_yaml(rest);
            in_shortcut = false;
            in_keys = false;
            continue;
        }
        if stripped.starts_with("Recommended:") {
            in_shortcut = false;
            in_keys = false;
            continue;
        }
        if stripped == "Shortcut:" {
            in_shortcut = true;
            in_keys = false;
            continue;
        }

        if in_shortcut {
            if let Some(rest) = stripped.strip_prefix('-') {
                let r = rest.trim();
                if let Some((mod_name, val)) = parse_mod_kv(r) {
                    flush_combo(&mut cur_mods, &mut cur_keys, &mut combos);
                    in_keys = false;
                    set_mod(&mut cur_mods, mod_name, val);
                    continue;
                }
                if in_keys && !r.is_empty() {
                    cur_keys.push(format_key_token(r));
                    continue;
                }
            }
            if let Some((mod_name, val)) = parse_mod_kv(stripped) {
                in_keys = false;
                set_mod(&mut cur_mods, mod_name, val);
                continue;
            }
            if stripped == "Keys:" {
                in_keys = true;
            }
        }
    }

    flush_prop(
        &mut prop_active,
        &mut prop_name,
        &mut prop_desc,
        &mut combos,
        &mut cur_mods,
        &mut cur_keys,
        &cur_section,
        &mut seen_ids,
        &mut items,
    );
    items
}

fn parse_mod_kv(s: &str) -> Option<(&'static str, bool)> {
    let (k, v) = s.split_once(':')?;
    let val = match v.trim().to_ascii_lowercase().as_str() {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    match k.trim().to_ascii_lowercase().as_str() {
        "win" => Some(("Win", val)),
        "ctrl" => Some(("Ctrl", val)),
        "alt" => Some(("Alt", val)),
        "shift" => Some(("Shift", val)),
        _ => None,
    }
}

fn set_mod(mods: &mut [bool; 4], name: &str, val: bool) {
    match name {
        "Win" => mods[0] = val,
        "Ctrl" => mods[1] = val,
        "Alt" => mods[2] = val,
        "Shift" => mods[3] = val,
        _ => {}
    }
}

/// Built-in Windows 10/11 catalog used only when PowerToys Shortcut Guide manifests are absent.
pub fn builtin_windows_shortcuts() -> Vec<ShortcutItem> {
    const ENTRIES: &[(&str, &str, &str, &str)] = &[
        (
            "Desktop & Window Management",
            "Alt+Tab",
            "Switch open windows",
            "Cycle forward through active apps",
        ),
        (
            "Desktop & Window Management",
            "Alt+F4",
            "Close active window",
            "Quit the focused app or open shutdown dialog on desktop",
        ),
        (
            "Desktop & Window Management",
            "Win+Left",
            "Snap window left",
            "Tile the active window to the left half of the screen",
        ),
        (
            "Desktop & Window Management",
            "Win+Right",
            "Snap window right",
            "Tile the active window to the right half of the screen",
        ),
        (
            "Desktop & Window Management",
            "Win+Up",
            "Maximize window",
            "Expand the active window to fill the screen",
        ),
        (
            "Desktop & Window Management",
            "Win+Down",
            "Restore or minimize window",
            "Step window down from maximized to restored or minimized",
        ),
        (
            "Desktop & Window Management",
            "Win+Z",
            "Open Snap Layouts",
            "Choose a multi-pane snap grid for the current monitor",
        ),
        (
            "Desktop & Window Management",
            "Win+D",
            "Show or hide desktop",
            "Toggle all open windows minimized",
        ),
        (
            "Desktop & Window Management",
            "Win+M",
            "Minimize all windows",
            "Minimize every open desktop window",
        ),
        (
            "Desktop & Window Management",
            "Win+Shift+Left",
            "Move window to left monitor",
            "Send the active window across displays",
        ),
        (
            "Desktop & Window Management",
            "Win+Shift+Right",
            "Move window to right monitor",
            "Send the active window across displays",
        ),
        (
            "Desktop & Window Management",
            "Win+Home",
            "Minimize inactive windows",
            "Hide all desktop windows except the focused one",
        ),
        (
            "Virtual Desktops",
            "Win+Tab",
            "Open Task View",
            "View all open windows and virtual desktops",
        ),
        (
            "Virtual Desktops",
            "Win+Ctrl+D",
            "Create virtual desktop",
            "Add a clean new virtual desktop workspace",
        ),
        (
            "Virtual Desktops",
            "Win+Ctrl+Left",
            "Switch virtual desktop left",
            "Move to the previous virtual desktop",
        ),
        (
            "Virtual Desktops",
            "Win+Ctrl+Right",
            "Switch virtual desktop right",
            "Move to the next virtual desktop",
        ),
        (
            "Virtual Desktops",
            "Win+Ctrl+F4",
            "Close current virtual desktop",
            "Close the active virtual desktop and move its windows",
        ),
        (
            "System & Capture",
            "Win+V",
            "Open Clipboard History",
            "Browse and paste recent text and image clips",
        ),
        (
            "System & Capture",
            "Win+Shift+S",
            "Capture screen snip",
            "Take a rectangular, freeform, window, or full screenshot",
        ),
        (
            "System & Capture",
            "Win+L",
            "Lock PC",
            "Immediately lock the Windows session",
        ),
        (
            "System & Capture",
            "Win+I",
            "Open Settings",
            "Launch Windows Settings",
        ),
        (
            "System & Capture",
            "Win+X",
            "Open Quick Link menu",
            "Power-user menu for Terminal, Device Manager, and Disk Management",
        ),
        (
            "System & Capture",
            "Win+A",
            "Open Quick Settings",
            "Toggle Wi-Fi, Bluetooth, brightness, and audio",
        ),
        (
            "System & Capture",
            "Win+N",
            "Open Notification Center",
            "View notifications and calendar flyout",
        ),
        (
            "System & Capture",
            "Win+H",
            "Start Voice Typing",
            "Launch Windows speech-to-text dictation",
        ),
        (
            "System & Capture",
            "Win+.",
            "Open Emoji & Symbols",
            "Insert emoji, Kaomoji, and Unicode symbols",
        ),
        (
            "System & Capture",
            "Ctrl+Shift+Esc",
            "Open Task Manager",
            "Inspect running processes and performance",
        ),
        (
            "Taskbar & File Explorer",
            "Win+E",
            "Open File Explorer",
            "Launch a new File Explorer window",
        ),
        (
            "Taskbar & File Explorer",
            "Win+R",
            "Open Run dialog",
            "Execute a command, path, or control panel applet",
        ),
        (
            "Taskbar & File Explorer",
            "Win+S",
            "Open Windows Search",
            "Search apps, files, and settings",
        ),
        (
            "Taskbar & File Explorer",
            "Win+T",
            "Cycle taskbar apps",
            "Move keyboard focus across pinned and running taskbar apps",
        ),
        (
            "Taskbar & File Explorer",
            "Win+1",
            "Launch taskbar app 1-9",
            "Open or switch to apps pinned in slots 1 through 9",
        ),
    ];

    ENTRIES
        .iter()
        .map(|(cat, keys, name, desc)| ShortcutItem {
            id: format!("win::{}::{}::{}", slug(cat), slug(name), slug(keys)),
            category: (*cat).to_string(),
            keys: (*keys).to_string(),
            name: (*name).to_string(),
            description: (*desc).to_string(),
            source: "windows".to_string(),
            ..Default::default()
        })
        .collect()
}

pub fn load_catalog(project_dir: &Path) -> Catalog {
    let cfg = load_config(project_dir);
    let state = load_state(project_dir);

    let master_path = PathBuf::from(expand_env_vars(&cfg.powertoys.master_shell_yml));
    let backup_path = project_dir
        .join("state")
        .join("backup")
        .join("+WindowsNT.Shell.en-US.orig.yml");
    let user_shell = PathBuf::from(expand_env_vars(&cfg.powertoys.user_shortcuts_dir))
        .join("+WindowsNT.Shell.en-US.yml");

    let win_source = if master_path.is_file() {
        master_path
    } else if backup_path.is_file() {
        backup_path
    } else {
        user_shell
    };
    let parsed_windows = parse_windows_master_yaml(&win_source);
    let windows = if parsed_windows.is_empty() {
        builtin_windows_shortcuts()
    } else {
        parsed_windows
    };

    let hidden_set: HashSet<String> = state.hidden_ids.iter().cloned().collect();
    let highlighted_set: HashSet<String> = state.highlighted_ids.iter().cloned().collect();
    let mut active = Vec::with_capacity(
        state.manual_shortcuts.len() + state.discovered_cache.len() + windows.len(),
    );
    let mut hidden = Vec::with_capacity(hidden_set.len());
    let mut custom_categories = Vec::new();
    let mut stats = CatalogStats::default();

    let all_iter = state
        .manual_shortcuts
        .into_iter()
        .map(|mut s| {
            s.source = "manual".to_string();
            s
        })
        .chain(state.discovered_cache)
        .chain(windows);

    for mut item in all_iter {
        item.hidden = hidden_set.contains(&item.id);
        item.highlighted = highlighted_set.contains(&item.id);
        let cat = if item.category.is_empty() {
            "General".to_string()
        } else {
            item.category.clone()
        };
        if item.source != "windows"
            && item.source != "powertoys"
            && !custom_categories.contains(&cat)
        {
            custom_categories.push(cat);
        }

        if item.hidden {
            stats.hidden_total += 1;
            if item.source == "windows" {
                stats.windows_hidden += 1;
            }
            hidden.push(item);
        } else {
            stats.active_total += 1;
            if item.highlighted {
                stats.highlighted += 1;
            }
            match item.source.as_str() {
                "manual" => stats.manual += 1,
                "powertoys" => stats.powertoys += 1,
                "windows" => stats.windows += 1,
                _ => stats.auto += 1,
            }
            active.push(item);
        }
    }

    Catalog {
        active,
        hidden,
        custom_categories,
        quick_notes: state.quick_notes,
        scratchpad: state.scratchpad,
        stats,
    }
}

fn normalize_action(action_type: &str, action_target: &str) -> Result<(String, String), String> {
    let kind = action_type.trim().to_ascii_lowercase();
    let target = action_target.trim();
    match kind.as_str() {
        "" | "none" => Ok((String::new(), String::new())),
        "open" => {
            if target.is_empty() {
                Err("Action target (app, file, or URL) is required for Open bindings.".to_string())
            } else {
                Ok(("open".to_string(), target.to_string()))
            }
        }
        "command" => {
            if target.is_empty() {
                Err("PowerShell / CLI command is required for Run Command bindings.".to_string())
            } else {
                Ok(("command".to_string(), target.to_string()))
            }
        }
        _ => Err(format!("Unsupported action type '{kind}'.")),
    }
}

pub fn save_local_hotkeys(project_dir: &Path, hotkeys: &HotkeysConfig) -> Result<(), String> {
    let local_path = project_dir.join("config.local.json");
    let mut root_map = serde_json::Map::new();
    if let Ok(raw) = fs::read_to_string(&local_path) {
        let clean = raw.trim_start_matches('\u{feff}');
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(clean) {
            root_map = map;
        }
    }

    let hk_val = serde_json::to_value(hotkeys).map_err(|e| e.to_string())?;
    root_map.insert("hotkeys".to_string(), hk_val);

    let json = serde_json::to_string_pretty(&Value::Object(root_map)).map_err(|e| e.to_string())?;
    let tmp = project_dir.join(format!("config.local.json.tmp.{}", std::process::id()));
    fs::write(&tmp, format!("{json}\n")).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &local_path).map_err(|e| e.to_string())
}

pub fn collect_live_bindings(cfg: &AppConfig, catalog: &Catalog) -> Vec<HotkeyBinding> {
    let mut out = Vec::new();
    if let Some(spec) = parse_ahk_hotkey(
        &cfg.hotkeys.toggle_overlay,
        &cfg.hotkeys.toggle_overlay_display,
    ) {
        let label = spec.label.clone();
        out.push(HotkeyBinding {
            spec,
            action: HotkeyAction::ToggleOverlay,
            name: format!("Toggle Overlay ({label})"),
        });
    }
    if let Some(spec) = parse_ahk_hotkey(
        &cfg.hotkeys.toggle_overlay_alt,
        &cfg.hotkeys.toggle_overlay_alt_display,
    ) {
        let label = spec.label.clone();
        out.push(HotkeyBinding {
            spec,
            action: HotkeyAction::ToggleOverlay,
            name: format!("Toggle Overlay Alt ({label})"),
        });
    }

    for item in &catalog.active {
        if item.source != "manual" || item.hidden {
            continue;
        }
        let target = item.action_target.trim();
        if target.is_empty() {
            continue;
        }
        let action = match item.action_type.trim().to_ascii_lowercase().as_str() {
            "open" => HotkeyAction::OpenTarget(expand_env_vars(target)),
            "command" => HotkeyAction::RunCommand(target.to_string()),
            _ => continue,
        };
        if let Some(spec) = parse_display_hotkey(&item.keys) {
            out.push(HotkeyBinding {
                spec,
                action,
                name: item.name.clone(),
            });
        }
    }

    out
}

pub fn add_manual(
    project_dir: &Path,
    category: &str,
    keys: &str,
    name: &str,
    description: &str,
    action_type: &str,
    action_target: &str,
) -> Result<ShortcutItem, String> {
    let key_str = keys.trim();
    let title = name.trim();
    if key_str.is_empty() || title.is_empty() {
        return Err("Both shortcut keys and action name are required.".to_string());
    }
    let (norm_type, norm_target) = normalize_action(action_type, action_target)?;
    let cat = if category.trim().is_empty() {
        "Custom"
    } else {
        category.trim()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let item = ShortcutItem {
        id: format!("manual::{:x}", nanos & 0xffff_ffff_ffff),
        category: cat.to_string(),
        keys: key_str.to_string(),
        name: title.to_string(),
        description: description.trim().to_string(),
        source: "manual".to_string(),
        action_type: norm_type,
        action_target: norm_target,
        ..Default::default()
    };
    let mut state = load_state(project_dir);
    state.manual_shortcuts.push(item.clone());
    save_state(project_dir, &state)?;
    Ok(item)
}

#[allow(clippy::too_many_arguments)]
pub fn update_manual(
    project_dir: &Path,
    item_id: &str,
    category: &str,
    keys: &str,
    name: &str,
    description: &str,
    action_type: &str,
    action_target: &str,
) -> Result<(), String> {
    let key_str = keys.trim();
    let title = name.trim();
    if key_str.is_empty() || title.is_empty() {
        return Err("Both shortcut keys and action name are required.".to_string());
    }
    let (norm_type, norm_target) = normalize_action(action_type, action_target)?;
    let mut state = load_state(project_dir);
    let Some(entry) = state.manual_shortcuts.iter_mut().find(|x| x.id == item_id) else {
        return Err(format!("Shortcut '{item_id}' not found."));
    };
    entry.category = if category.trim().is_empty() {
        "Custom".to_string()
    } else {
        category.trim().to_string()
    };
    entry.keys = key_str.to_string();
    entry.name = title.to_string();
    entry.description = description.trim().to_string();
    entry.action_type = norm_type;
    entry.action_target = norm_target;
    save_state(project_dir, &state)
}

pub fn delete_manual(project_dir: &Path, item_id: &str) -> Result<(), String> {
    let mut state = load_state(project_dir);
    state.manual_shortcuts.retain(|x| x.id != item_id);
    state.hidden_ids.retain(|id| id != item_id);
    state.highlighted_ids.retain(|id| id != item_id);
    save_state(project_dir, &state)
}

pub fn hide_item(project_dir: &Path, item_id: &str) -> Result<(), String> {
    let mut state = load_state(project_dir);
    if !state.hidden_ids.iter().any(|x| x == item_id) {
        state.hidden_ids.push(item_id.to_string());
        save_state(project_dir, &state)?;
    }
    Ok(())
}

pub fn hide_items(project_dir: &Path, item_ids: &[String]) -> Result<usize, String> {
    let mut state = load_state(project_dir);
    let mut added = 0usize;
    for item_id in item_ids {
        if !state.hidden_ids.iter().any(|x| x == item_id) {
            state.hidden_ids.push(item_id.clone());
            added += 1;
        }
    }
    if added > 0 {
        save_state(project_dir, &state)?;
    }
    Ok(added)
}

pub fn toggle_highlight(project_dir: &Path, item_id: &str) -> Result<bool, String> {
    let mut state = load_state(project_dir);
    let now_highlighted = if let Some(pos) = state.highlighted_ids.iter().position(|x| x == item_id)
    {
        state.highlighted_ids.remove(pos);
        false
    } else {
        state.highlighted_ids.push(item_id.to_string());
        true
    };
    save_state(project_dir, &state)?;
    Ok(now_highlighted)
}

pub fn restore_item(project_dir: &Path, item_id: &str) -> Result<(), String> {
    let mut state = load_state(project_dir);
    state.hidden_ids.retain(|x| x != item_id);
    save_state(project_dir, &state)
}

pub fn restore_all_windows(project_dir: &Path) -> Result<Vec<String>, String> {
    let mut state = load_state(project_dir);
    let restored_ids: Vec<String> = state
        .hidden_ids
        .iter()
        .filter(|id| id.starts_with("win::"))
        .cloned()
        .collect();
    if !restored_ids.is_empty() {
        state.hidden_ids.retain(|id| !id.starts_with("win::"));
        save_state(project_dir, &state)?;
    }
    Ok(restored_ids)
}

pub fn add_quick_note(
    project_dir: &Path,
    category: &str,
    trigger: &str,
    content: &str,
) -> Result<QuickNote, String> {
    let trig = trigger.trim();
    let body = content.trim();
    if trig.is_empty() && body.is_empty() {
        return Err("Enter a command/label or note text.".to_string());
    }
    let cat = if category.trim().is_empty() {
        "AI Skills"
    } else {
        category.trim()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let note = QuickNote {
        id: format!("note::{:x}", nanos & 0xffff_ffff_ffff),
        category: cat.to_string(),
        trigger: trig.to_string(),
        content: body.to_string(),
        pinned: false,
    };
    let mut state = load_state(project_dir);
    state.quick_notes.push(note.clone());
    save_state(project_dir, &state)?;
    Ok(note)
}

pub fn update_quick_note(
    project_dir: &Path,
    note_id: &str,
    category: &str,
    trigger: &str,
    content: &str,
) -> Result<(), String> {
    let trig = trigger.trim();
    let body = content.trim();
    if trig.is_empty() && body.is_empty() {
        return Err("Enter a command/label or note text.".to_string());
    }
    let mut state = load_state(project_dir);
    let Some(entry) = state.quick_notes.iter_mut().find(|n| n.id == note_id) else {
        return Err(format!("Note '{note_id}' not found."));
    };
    entry.category = if category.trim().is_empty() {
        "AI Skills".to_string()
    } else {
        category.trim().to_string()
    };
    entry.trigger = trig.to_string();
    entry.content = body.to_string();
    save_state(project_dir, &state)
}

pub fn delete_quick_note(project_dir: &Path, note_id: &str) -> Result<(), String> {
    let mut state = load_state(project_dir);
    state.quick_notes.retain(|n| n.id != note_id);
    save_state(project_dir, &state)
}

pub fn toggle_quick_note_pin(project_dir: &Path, note_id: &str) -> Result<bool, String> {
    let mut state = load_state(project_dir);
    let Some(entry) = state.quick_notes.iter_mut().find(|n| n.id == note_id) else {
        return Err(format!("Note '{note_id}' not found."));
    };
    entry.pinned = !entry.pinned;
    let now_pinned = entry.pinned;
    save_state(project_dir, &state)?;
    Ok(now_pinned)
}

pub fn save_scratchpad(project_dir: &Path, text: &str) -> Result<(), String> {
    let mut state = load_state(project_dir);
    state.scratchpad = text.to_string();
    save_state(project_dir, &state)
}

fn find_python_exe_in_dir(dir: &Path) -> Option<PathBuf> {
    for exe in ["pythonw.exe", "python.exe"] {
        let cand = dir.join(exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn find_pythonw() -> Option<PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if let Some(found) = find_python_exe_in_dir(&dir) {
                return Some(found);
            }
        }
    }

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(local_app).join("Programs").join("Python"));
    }
    roots.push(PathBuf::from("C:\\"));

    let mut version_dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.to_ascii_lowercase().starts_with("python") {
                            version_dirs.push(path);
                        }
                    }
                }
            }
        }
    }
    // Prefer newest Python folder (lexicographically descending: Python313 > Python312 > Python311)
    version_dirs.sort();
    version_dirs.reverse();
    for dir in version_dirs {
        if let Some(found) = find_python_exe_in_dir(&dir) {
            return Some(found);
        }
    }

    None
}

pub fn spawn_powertoys_sync<F>(project_dir: PathBuf, rescan: bool, on_done: F)
where
    F: FnOnce(bool, String) + Send + 'static,
{
    std::thread::spawn(move || {
        let store_script = project_dir.join("store.py");
        if !store_script.is_file() {
            on_done(
                true,
                "Saved locally (install Python + PowerToys for sync)".to_string(),
            );
            return;
        }
        let Some(python_exe) = find_pythonw() else {
            on_done(
                true,
                "Saved locally (install Python + PowerToys for sync)".to_string(),
            );
            return;
        };

        let mut cmd = Command::new(python_exe);
        cmd.arg(&store_script)
            .arg("--project-dir")
            .arg(&project_dir)
            .arg("sync");
        if !rescan {
            cmd.arg("--no-rescan");
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        match cmd.output() {
            Ok(out) => {
                let ok = out.status.success();
                let msg = if ok {
                    if rescan {
                        "Auto-scan complete & synced to PowerToys".to_string()
                    } else {
                        "Synced with PowerToys Shortcut Guide".to_string()
                    }
                } else {
                    let err_txt = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    if err_txt.is_empty() {
                        "Saved locally (PowerToys sync skipped)".to_string()
                    } else {
                        err_txt
                    }
                };
                on_done(ok, msg);
            }
            Err(_) => on_done(
                true,
                "Saved locally (install Python + PowerToys for sync)".to_string(),
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_yaml_parser_matches_python_ids() {
        let dir = std::env::temp_dir().join(format!("sh_yaml_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let p = dir.join("shell.yml");
        fs::write(
            &p,
            "PackageName: +WindowsNT.Shell\nShortcuts:\n  - SectionName: Desktop Shortcuts\n    Properties:\n      - Name: Close active window\n        Shortcut:\n        - Win: false\n          Ctrl: false\n          Shift: false\n          Alt: true\n          Keys:\n            - F4\n  - SectionName: <TASKBAR1-9>Taskbar Shortcuts\n    Properties:\n      - Name: Open app\n        Description: Numbered item\n        Shortcut:\n        - Win: true\n          Ctrl: false\n          Shift: false\n          Alt: false\n          Keys:\n            - 1\n",
        )
        .unwrap();

        let items = parse_windows_master_yaml(&p);
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].id,
            "win::desktop_shortcuts::close_active_window::alt_f4"
        );
        assert_eq!(items[0].keys, "Alt+F4");
        assert_eq!(items[1].category, "Taskbar Shortcuts");
        assert_eq!(items[1].keys, "Win+1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crud_hide_and_restore_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sh_crud_test_{}", std::process::id()));
        let _ = fs::create_dir_all(dir.join("state"));
        fs::write(
            dir.join("config.json"),
            r#"{"powertoys":{"master_shell_yml":"","user_shortcuts_dir":""}}"#,
        )
        .unwrap();

        let baseline = load_catalog(&dir).active.len();
        let added = add_manual(
            &dir,
            "Notes",
            "Ctrl+Alt+N",
            "Quick Note",
            "Opens note",
            "open",
            "notepad.exe",
        )
        .unwrap();
        assert!(added.id.starts_with("manual::"));

        update_manual(
            &dir,
            &added.id,
            "Notes",
            "Ctrl+Alt+M",
            "Quick Note v2",
            "Updated",
            "command",
            "Write-Output ok",
        )
        .unwrap();
        let cat = load_catalog(&dir);
        assert_eq!(cat.active.len(), baseline + 1);
        assert_eq!(cat.active[0].keys, "Ctrl+Alt+M");
        assert_eq!(cat.active[0].action_type, "command");

        hide_item(&dir, &added.id).unwrap();
        assert_eq!(load_catalog(&dir).hidden.len(), 1);

        restore_item(&dir, &added.id).unwrap();
        assert_eq!(load_catalog(&dir).active.len(), baseline + 1);

        delete_manual(&dir, &added.id).unwrap();
        assert_eq!(load_catalog(&dir).active.len(), baseline);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_local_hotkeys_preserves_other_local_fields() {
        let dir = std::env::temp_dir().join(format!("sh_save_hk_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(
            dir.join("config.json"),
            r##"{"hotkeys":{"toggle_overlay":"^!k","toggle_overlay_display":"Ctrl+Alt+K","toggle_overlay_alt":"#F7","toggle_overlay_alt_display":"Win+F7"}}"##,
        )
        .unwrap();
        fs::write(
            dir.join("config.local.json"),
            r#"{"discovery":{"extra_watch_dirs":["C:\\CustomScripts"]}}"#,
        )
        .unwrap();

        let new_hk = HotkeysConfig {
            toggle_overlay: "^!p".to_string(),
            toggle_overlay_display: "Ctrl+Alt+P".to_string(),
            toggle_overlay_alt: String::new(),
            toggle_overlay_alt_display: String::new(),
        };
        save_local_hotkeys(&dir, &new_hk).unwrap();

        let reloaded_cfg = load_config(&dir);
        assert_eq!(reloaded_cfg.hotkeys.toggle_overlay, "^!p");
        assert_eq!(reloaded_cfg.hotkeys.toggle_overlay_display, "Ctrl+Alt+P");
        assert_eq!(reloaded_cfg.hotkeys.toggle_overlay_alt, "");

        let raw_local: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("config.local.json")).unwrap())
                .unwrap();
        assert_eq!(
            raw_local["discovery"]["extra_watch_dirs"][0],
            "C:\\CustomScripts"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_live_bindings_includes_active_actions_and_excludes_hidden() {
        let dir = std::env::temp_dir().join(format!("sh_live_bind_test_{}", std::process::id()));
        let _ = fs::create_dir_all(dir.join("state"));
        fs::write(
            dir.join("config.json"),
            r#"{"powertoys":{"master_shell_yml":"Z:\\missing.yml","user_shortcuts_dir":"Z:\\missing"}}"#,
        )
        .unwrap();

        let open_item = add_manual(
            &dir,
            "Apps",
            "Ctrl+Alt+O",
            "Open Notepad",
            "Launches Notepad",
            "open",
            "notepad.exe",
        )
        .unwrap();
        let ref_item = add_manual(
            &dir,
            "Refs",
            "Ctrl+Alt+R",
            "Reference Only",
            "Just a note",
            "",
            "",
        )
        .unwrap();
        assert!(!ref_item.id.is_empty());

        let cfg = load_config(&dir);
        let cat = load_catalog(&dir);
        let live = collect_live_bindings(&cfg, &cat);
        assert!(live
            .iter()
            .any(|b| matches!(&b.action, HotkeyAction::OpenTarget(t) if t == "notepad.exe")));
        assert!(!live.iter().any(|b| b.name == "Reference Only"));

        hide_item(&dir, &open_item.id).unwrap();
        let cat_hidden = load_catalog(&dir);
        let live_after_hide = collect_live_bindings(&cfg, &cat_hidden);
        assert!(!live_after_hide.iter().any(|b| b.name == "Open Notepad"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_local_overrides_base_config() {
        let dir = std::env::temp_dir().join(format!("sh_cfg_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(
            dir.join("config.json"),
            r##"{"hotkeys":{"toggle_overlay":"^!k","toggle_overlay_display":"Ctrl+Alt+K","toggle_overlay_alt":"#F7","toggle_overlay_alt_display":"Win+F7"},"powertoys":{"sync_enabled":true}}"##,
        )
        .unwrap();
        fs::write(
            dir.join("config.local.json"),
            r#"{"hotkeys":{"toggle_overlay":"^!m","toggle_overlay_display":"Ctrl+Alt+M"},"powertoys":{"sync_enabled":false}}"#,
        )
        .unwrap();

        let cfg = load_config(&dir);
        assert_eq!(cfg.hotkeys.toggle_overlay, "^!m");
        assert_eq!(cfg.hotkeys.toggle_overlay_display, "Ctrl+Alt+M");
        assert_eq!(cfg.hotkeys.toggle_overlay_alt, "#F7");
        assert!(!cfg.powertoys.sync_enabled);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_windows_fallback_loads_when_powertoys_absent() {
        let dir = std::env::temp_dir().join(format!("sh_builtin_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(
            dir.join("config.json"),
            r#"{"powertoys":{"master_shell_yml":"Z:\\nonexistent\\missing.yml","user_shortcuts_dir":"Z:\\nonexistent\\dir"}}"#,
        )
        .unwrap();

        let cat = load_catalog(&dir);
        assert!(cat.stats.windows >= 25);
        assert!(cat.active.iter().any(|x| x.keys == "Win+V"));
        assert!(cat.active.iter().any(|x| x.id.starts_with("win::")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn toggle_highlight_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sh_hl_test_{}", std::process::id()));
        let _ = fs::create_dir_all(dir.join("state"));
        fs::write(
            dir.join("config.json"),
            r#"{"powertoys":{"master_shell_yml":"Z:\\missing.yml","user_shortcuts_dir":"Z:\\missing"}}"#,
        )
        .unwrap();

        let item = add_manual(&dir, "Learn", "Ctrl+Alt+L", "Practice", "", "", "").unwrap();
        assert!(toggle_highlight(&dir, &item.id).unwrap());
        let cat1 = load_catalog(&dir);
        assert_eq!(cat1.stats.highlighted, 1);
        assert!(
            cat1.active
                .iter()
                .find(|x| x.id == item.id)
                .unwrap()
                .highlighted
        );

        assert!(!toggle_highlight(&dir, &item.id).unwrap());
        let cat2 = load_catalog(&dir);
        assert_eq!(cat2.stats.highlighted, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_all_windows_and_undo_hide_items_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sh_undo_test_{}", std::process::id()));
        let _ = fs::create_dir_all(dir.join("state"));
        fs::write(
            dir.join("config.json"),
            r#"{"powertoys":{"master_shell_yml":"Z:\\missing.yml","user_shortcuts_dir":"Z:\\missing"}}"#,
        )
        .unwrap();

        let cat = load_catalog(&dir);
        let win_ids: Vec<String> = cat
            .active
            .iter()
            .filter(|x| x.source == "windows")
            .take(2)
            .map(|x| x.id.clone())
            .collect();
        assert_eq!(hide_items(&dir, &win_ids).unwrap(), 2);
        assert_eq!(load_catalog(&dir).stats.windows_hidden, 2);

        let restored = restore_all_windows(&dir).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(load_catalog(&dir).stats.windows_hidden, 0);

        assert_eq!(hide_items(&dir, &restored).unwrap(), 2);
        assert_eq!(load_catalog(&dir).stats.windows_hidden, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_notes_and_scratchpad_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sh_notes_test_{}", std::process::id()));
        let _ = fs::create_dir_all(dir.join("state"));
        fs::write(
            dir.join("config.json"),
            r#"{"powertoys":{"master_shell_yml":"Z:\\missing.yml","user_shortcuts_dir":"Z:\\missing"}}"#,
        )
        .unwrap();

        let n = add_quick_note(&dir, "AI Skills", "/plan", "Enter step-by-step plan mode").unwrap();
        assert!(n.id.starts_with("note::"));
        assert!(toggle_quick_note_pin(&dir, &n.id).unwrap());

        update_quick_note(
            &dir,
            &n.id,
            "AI Skills",
            "/plan",
            "Careful architecture plan",
        )
        .unwrap();
        save_scratchpad(&dir, "Remember: /grill-me before big refactors").unwrap();

        let cat = load_catalog(&dir);
        assert_eq!(cat.quick_notes.len(), 1);
        assert_eq!(cat.quick_notes[0].trigger, "/plan");
        assert_eq!(cat.quick_notes[0].content, "Careful architecture plan");
        assert!(cat.quick_notes[0].pinned);
        assert_eq!(cat.scratchpad, "Remember: /grill-me before big refactors");

        delete_quick_note(&dir, &n.id).unwrap();
        assert!(load_catalog(&dir).quick_notes.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
