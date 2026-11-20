//! Win32 RegisterHotKey primary handler, WH_KEYBOARD_LL fallback, live hot-reload, actions, single-instance IPC, and focus helpers.

use eframe::egui;
use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    WAIT_OBJECT_0, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, CreateEventW, CreateMutexW, GetCurrentThreadId, SetEvent,
    WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, FindWindowW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, PeekMessageW, PostThreadMessageW, SetForegroundWindow,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG,
    PM_NOREMOVE, SW_SHOWNORMAL, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_USER,
};

const WM_RELOAD_HOTKEYS: u32 = WM_USER + 1;

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum UiCommand {
    Toggle,
    Show,
    Hide,
    Rescan,
    OpenFolder,
    SyncFinished(bool, String),
    HotkeysReloaded(bool, String),
    Quit,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleOverlay,
    OpenTarget(String),
    RunCommand(String),
}

#[derive(Debug, Clone)]
pub struct HotkeyBinding {
    pub spec: HotkeySpec,
    pub action: HotkeyAction,
    pub name: String,
}

#[derive(Clone)]
pub struct HotkeyController {
    thread_id: Arc<AtomicU32>,
    desired: Arc<Mutex<Vec<HotkeyBinding>>>,
    announce_next: Arc<AtomicBool>,
}

impl HotkeyController {
    fn reload_internal(&self, bindings: Vec<HotkeyBinding>, announce: bool) {
        if let Ok(mut guard) = self.desired.lock() {
            *guard = bindings;
        }
        self.announce_next.store(announce, Ordering::Release);
        let tid = self.thread_id.load(Ordering::Acquire);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_RELOAD_HOTKEYS, 0, 0);
            }
        }
    }

    pub fn reload(&self, bindings: Vec<HotkeyBinding>) {
        self.reload_internal(bindings, true);
    }

    pub fn pause_for_recording(&self) {
        self.reload_internal(Vec::new(), false);
    }
}

pub struct SendHandle(pub usize);

pub enum SingleInstanceOutcome {
    Primary {
        _mutex: SendHandle,
        toggle_event: SendHandle,
    },
    AlreadyRunningSignaled,
}

static LAST_TOGGLE_MS: AtomicU64 = AtomicU64::new(0);
static LAST_ACTION_MS: AtomicU64 = AtomicU64::new(0);

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn try_claim_debounce(slot: &AtomicU64, min_interval_ms: u64) -> bool {
    let now = now_epoch_ms();
    loop {
        let prev = slot.load(Ordering::Acquire);
        if now.saturating_sub(prev) < min_interval_ms {
            return false;
        }
        if slot
            .compare_exchange(prev, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return true;
        }
    }
}

/// Atomically claims a toggle slot if at least `min_interval_ms` elapsed since the previous toggle.
pub fn try_claim_toggle_debounce(min_interval_ms: u64) -> bool {
    try_claim_debounce(&LAST_TOGGLE_MS, min_interval_ms)
}

pub fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn acquire_single_instance() -> SingleInstanceOutcome {
    unsafe {
        let mutex_name = wide_null("Local\\ShortcutHubSingleInstanceMutex");
        let mutex: HANDLE = CreateMutexW(std::ptr::null(), 1, mutex_name.as_ptr());
        let already = GetLastError() == ERROR_ALREADY_EXISTS;

        let ev_name = wide_null("Local\\ShortcutHubToggleEvent");
        let ev: HANDLE = CreateEventW(std::ptr::null(), 0, 0, ev_name.as_ptr());

        if already {
            if !ev.is_null() {
                SetEvent(ev);
                CloseHandle(ev);
            }
            if !mutex.is_null() {
                CloseHandle(mutex);
            }
            return SingleInstanceOutcome::AlreadyRunningSignaled;
        }

        SingleInstanceOutcome::Primary {
            _mutex: SendHandle(mutex as usize),
            toggle_event: SendHandle(ev as usize),
        }
    }
}

pub fn spawn_event_listener(
    toggle_event: SendHandle,
    tx: mpsc::Sender<UiCommand>,
    ctx: egui::Context,
) {
    let raw_ev = toggle_event.0;
    if raw_ev == 0 {
        return;
    }
    std::thread::spawn(move || unsafe {
        let ev = raw_ev as HANDLE;
        loop {
            if WaitForSingleObject(ev, INFINITE) == WAIT_OBJECT_0 {
                let _ = tx.send(UiCommand::Toggle);
                ctx.request_repaint();
            }
        }
    });
}

fn map_vk_name(key: &str) -> Option<u32> {
    let trimmed = key.trim();
    if trimmed.len() == 1 {
        let ch = trimmed.chars().next()?.to_ascii_uppercase();
        if ch.is_ascii_alphanumeric() {
            return Some(ch as u32);
        }
    }
    let low = trimmed.to_ascii_lowercase();
    if let Some(rest) = low.strip_prefix('f') {
        if let Ok(num) = rest.parse::<u32>() {
            if (1..=24).contains(&num) {
                return Some(0x70 + num - 1);
            }
        }
    }
    match low.as_str() {
        "space" => Some(0x20),
        "enter" | "return" => Some(0x0D),
        "tab" => Some(0x09),
        "esc" | "escape" => Some(0x1B),
        "backspace" | "bs" => Some(0x08),
        "delete" | "del" => Some(0x2E),
        "insert" | "ins" => Some(0x2D),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pgup" | "pageup" | "page up" => Some(0x21),
        "pgdn" | "pagedown" | "page down" => Some(0x22),
        "up" | "arrowup" => Some(0x26),
        "down" | "arrowdown" => Some(0x28),
        "left" | "arrowleft" => Some(0x25),
        "right" | "arrowright" => Some(0x27),
        _ => None,
    }
}

pub fn vk_to_display_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 => ((vk as u8) as char).to_string(),
        0x41..=0x5A => ((vk as u8) as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x20 => "Space".to_string(),
        0x0D => "Enter".to_string(),
        0x09 => "Tab".to_string(),
        0x1B => "Esc".to_string(),
        0x08 => "Backspace".to_string(),
        0x2E => "Delete".to_string(),
        0x2D => "Insert".to_string(),
        0x24 => "Home".to_string(),
        0x23 => "End".to_string(),
        0x21 => "Page Up".to_string(),
        0x22 => "Page Down".to_string(),
        0x26 => "Up".to_string(),
        0x28 => "Down".to_string(),
        0x25 => "Left".to_string(),
        0x27 => "Right".to_string(),
        other => format!("VK_{other}"),
    }
}

fn vk_to_ahk_key_token(vk: u32) -> String {
    match vk {
        0x30..=0x39 => ((vk as u8) as char).to_string(),
        0x41..=0x5A => ((vk as u8) as char).to_ascii_lowercase().to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x20 => "Space".to_string(),
        0x0D => "Enter".to_string(),
        0x09 => "Tab".to_string(),
        0x1B => "Esc".to_string(),
        0x08 => "Backspace".to_string(),
        0x2E => "Delete".to_string(),
        0x2D => "Insert".to_string(),
        0x24 => "Home".to_string(),
        0x23 => "End".to_string(),
        0x21 => "PgUp".to_string(),
        0x22 => "PgDn".to_string(),
        0x26 => "Up".to_string(),
        0x28 => "Down".to_string(),
        0x25 => "Left".to_string(),
        0x27 => "Right".to_string(),
        other => format!("vk{other:X}"),
    }
}

pub fn spec_to_display(spec: &HotkeySpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    if spec.win {
        parts.push("Win".to_string());
    }
    if spec.ctrl {
        parts.push("Ctrl".to_string());
    }
    if spec.alt {
        parts.push("Alt".to_string());
    }
    if spec.shift {
        parts.push("Shift".to_string());
    }
    parts.push(vk_to_display_name(spec.vk));
    parts.join("+")
}

pub fn spec_to_ahk(spec: &HotkeySpec) -> String {
    let mut out = String::new();
    if spec.win {
        out.push('#');
    }
    if spec.ctrl {
        out.push('^');
    }
    if spec.alt {
        out.push('!');
    }
    if spec.shift {
        out.push('+');
    }
    out.push_str(&vk_to_ahk_key_token(spec.vk));
    out
}

pub fn parse_display_hotkey(raw: &str) -> Option<HotkeySpec> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if !s.contains('+') && s.starts_with(['^', '!', '+', '#', '$']) {
        return parse_ahk_hotkey(s, s);
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut win = false;
    let mut key_vk: Option<u32> = None;

    for token in s.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "menu" => alt = true,
            "shift" => shift = true,
            "win" | "super" | "meta" | "windows" => win = true,
            _ => {
                if key_vk.is_some() {
                    return None;
                }
                key_vk = map_vk_name(token);
                key_vk?;
            }
        }
    }

    let vk = key_vk?;
    let mut spec = HotkeySpec {
        ctrl,
        alt,
        shift,
        win,
        vk,
        label: String::new(),
    };
    spec.label = spec_to_display(&spec);
    Some(spec)
}

pub fn validate_hotkey_spec(spec: &HotkeySpec) -> Result<(), String> {
    let has_mod = spec.ctrl || spec.alt || spec.shift || spec.win;
    if spec.vk == 0x1B && !has_mod {
        return Err("Bare Escape cannot be assigned as a global hotkey.".to_string());
    }
    let is_fn_key = (0x70..=0x87).contains(&spec.vk);
    if !has_mod && !is_fn_key {
        return Err("Global hotkeys require at least one modifier (Ctrl, Alt, Shift, or Win) or an F1-F24 key.".to_string());
    }
    Ok(())
}

fn vk_to_egui_key(vk: u32) -> Option<egui::Key> {
    match vk {
        0x30 => Some(egui::Key::Num0),
        0x31 => Some(egui::Key::Num1),
        0x32 => Some(egui::Key::Num2),
        0x33 => Some(egui::Key::Num3),
        0x34 => Some(egui::Key::Num4),
        0x35 => Some(egui::Key::Num5),
        0x36 => Some(egui::Key::Num6),
        0x37 => Some(egui::Key::Num7),
        0x38 => Some(egui::Key::Num8),
        0x39 => Some(egui::Key::Num9),
        0x41 => Some(egui::Key::A),
        0x42 => Some(egui::Key::B),
        0x43 => Some(egui::Key::C),
        0x44 => Some(egui::Key::D),
        0x45 => Some(egui::Key::E),
        0x46 => Some(egui::Key::F),
        0x47 => Some(egui::Key::G),
        0x48 => Some(egui::Key::H),
        0x49 => Some(egui::Key::I),
        0x4A => Some(egui::Key::J),
        0x4B => Some(egui::Key::K),
        0x4C => Some(egui::Key::L),
        0x4D => Some(egui::Key::M),
        0x4E => Some(egui::Key::N),
        0x4F => Some(egui::Key::O),
        0x50 => Some(egui::Key::P),
        0x51 => Some(egui::Key::Q),
        0x52 => Some(egui::Key::R),
        0x53 => Some(egui::Key::S),
        0x54 => Some(egui::Key::T),
        0x55 => Some(egui::Key::U),
        0x56 => Some(egui::Key::V),
        0x57 => Some(egui::Key::W),
        0x58 => Some(egui::Key::X),
        0x59 => Some(egui::Key::Y),
        0x5A => Some(egui::Key::Z),
        0x70 => Some(egui::Key::F1),
        0x71 => Some(egui::Key::F2),
        0x72 => Some(egui::Key::F3),
        0x73 => Some(egui::Key::F4),
        0x74 => Some(egui::Key::F5),
        0x75 => Some(egui::Key::F6),
        0x76 => Some(egui::Key::F7),
        0x77 => Some(egui::Key::F8),
        0x78 => Some(egui::Key::F9),
        0x79 => Some(egui::Key::F10),
        0x7A => Some(egui::Key::F11),
        0x7B => Some(egui::Key::F12),
        0x7C => Some(egui::Key::F13),
        0x7D => Some(egui::Key::F14),
        0x7E => Some(egui::Key::F15),
        0x7F => Some(egui::Key::F16),
        0x80 => Some(egui::Key::F17),
        0x81 => Some(egui::Key::F18),
        0x82 => Some(egui::Key::F19),
        0x83 => Some(egui::Key::F20),
        0x84 => Some(egui::Key::F21),
        0x85 => Some(egui::Key::F22),
        0x86 => Some(egui::Key::F23),
        0x87 => Some(egui::Key::F24),
        0x20 => Some(egui::Key::Space),
        0x0D => Some(egui::Key::Enter),
        0x09 => Some(egui::Key::Tab),
        0x1B => Some(egui::Key::Escape),
        0x08 => Some(egui::Key::Backspace),
        0x2E => Some(egui::Key::Delete),
        0x2D => Some(egui::Key::Insert),
        0x24 => Some(egui::Key::Home),
        0x23 => Some(egui::Key::End),
        0x21 => Some(egui::Key::PageUp),
        0x22 => Some(egui::Key::PageDown),
        0x26 => Some(egui::Key::ArrowUp),
        0x28 => Some(egui::Key::ArrowDown),
        0x25 => Some(egui::Key::ArrowLeft),
        0x27 => Some(egui::Key::ArrowRight),
        _ => None,
    }
}

/// Maps an `egui::Key` back to its canonical Win32 VK display token for key recording.
pub fn egui_key_to_display_name(key: egui::Key) -> Option<&'static str> {
    match key {
        egui::Key::Num0 => Some("0"),
        egui::Key::Num1 => Some("1"),
        egui::Key::Num2 => Some("2"),
        egui::Key::Num3 => Some("3"),
        egui::Key::Num4 => Some("4"),
        egui::Key::Num5 => Some("5"),
        egui::Key::Num6 => Some("6"),
        egui::Key::Num7 => Some("7"),
        egui::Key::Num8 => Some("8"),
        egui::Key::Num9 => Some("9"),
        egui::Key::A => Some("A"),
        egui::Key::B => Some("B"),
        egui::Key::C => Some("C"),
        egui::Key::D => Some("D"),
        egui::Key::E => Some("E"),
        egui::Key::F => Some("F"),
        egui::Key::G => Some("G"),
        egui::Key::H => Some("H"),
        egui::Key::I => Some("I"),
        egui::Key::J => Some("J"),
        egui::Key::K => Some("K"),
        egui::Key::L => Some("L"),
        egui::Key::M => Some("M"),
        egui::Key::N => Some("N"),
        egui::Key::O => Some("O"),
        egui::Key::P => Some("P"),
        egui::Key::Q => Some("Q"),
        egui::Key::R => Some("R"),
        egui::Key::S => Some("S"),
        egui::Key::T => Some("T"),
        egui::Key::U => Some("U"),
        egui::Key::V => Some("V"),
        egui::Key::W => Some("W"),
        egui::Key::X => Some("X"),
        egui::Key::Y => Some("Y"),
        egui::Key::Z => Some("Z"),
        egui::Key::F1 => Some("F1"),
        egui::Key::F2 => Some("F2"),
        egui::Key::F3 => Some("F3"),
        egui::Key::F4 => Some("F4"),
        egui::Key::F5 => Some("F5"),
        egui::Key::F6 => Some("F6"),
        egui::Key::F7 => Some("F7"),
        egui::Key::F8 => Some("F8"),
        egui::Key::F9 => Some("F9"),
        egui::Key::F10 => Some("F10"),
        egui::Key::F11 => Some("F11"),
        egui::Key::F12 => Some("F12"),
        egui::Key::F13 => Some("F13"),
        egui::Key::F14 => Some("F14"),
        egui::Key::F15 => Some("F15"),
        egui::Key::F16 => Some("F16"),
        egui::Key::F17 => Some("F17"),
        egui::Key::F18 => Some("F18"),
        egui::Key::F19 => Some("F19"),
        egui::Key::F20 => Some("F20"),
        egui::Key::F21 => Some("F21"),
        egui::Key::F22 => Some("F22"),
        egui::Key::F23 => Some("F23"),
        egui::Key::F24 => Some("F24"),
        egui::Key::Space => Some("Space"),
        egui::Key::Enter => Some("Enter"),
        egui::Key::Tab => Some("Tab"),
        egui::Key::Escape => Some("Esc"),
        egui::Key::Backspace => Some("Backspace"),
        egui::Key::Delete => Some("Delete"),
        egui::Key::Insert => Some("Insert"),
        egui::Key::Home => Some("Home"),
        egui::Key::End => Some("End"),
        egui::Key::PageUp => Some("Page Up"),
        egui::Key::PageDown => Some("Page Down"),
        egui::Key::ArrowUp => Some("Up"),
        egui::Key::ArrowDown => Some("Down"),
        egui::Key::ArrowLeft => Some("Left"),
        egui::Key::ArrowRight => Some("Right"),
        _ => None,
    }
}

/// Checks if the Win key is physically down via Win32 async key state.
pub fn is_win_key_down() -> bool {
    unsafe { GetAsyncKeyState(VK_LWIN as i32) < 0 || GetAsyncKeyState(VK_RWIN as i32) < 0 }
}

/// Checks if the given hotkey was pressed inside the focused egui frame with matching modifiers.
pub fn matches_egui_hotkey(input: &egui::InputState, spec: &HotkeySpec) -> bool {
    let Some(egui_key) = vk_to_egui_key(spec.vk) else {
        return false;
    };
    if !input.key_pressed(egui_key) {
        return false;
    }
    let win_down = is_win_key_down();
    input.modifiers.ctrl == spec.ctrl
        && input.modifiers.alt == spec.alt
        && input.modifiers.shift == spec.shift
        && win_down == spec.win
}

pub fn parse_ahk_hotkey(raw: &str, label: &str) -> Option<HotkeySpec> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut win = false;
    let mut idx = 0;
    let bytes = s.as_bytes();
    while idx < bytes.len() {
        match bytes[idx] as char {
            '$' | '~' | '*' | '<' | '>' => idx += 1,
            '^' => {
                ctrl = true;
                idx += 1;
            }
            '!' => {
                alt = true;
                idx += 1;
            }
            '+' => {
                shift = true;
                idx += 1;
            }
            '#' => {
                win = true;
                idx += 1;
            }
            _ => break,
        }
    }
    let vk = map_vk_name(&s[idx..])?;
    let mut spec = HotkeySpec {
        ctrl,
        alt,
        shift,
        win,
        vk,
        label: String::new(),
    };
    spec.label = if label.trim().is_empty() {
        spec_to_display(&spec)
    } else {
        label.to_string()
    };
    Some(spec)
}

struct HookShared {
    hotkeys: Mutex<Vec<HotkeyBinding>>,
    tx: mpsc::Sender<UiCommand>,
    ctx: egui::Context,
    pressed_vk: AtomicU32,
    last_pressed_ms: AtomicU64,
    mod_mask: AtomicU32,
}

static HOOK_STATE: OnceLock<HookShared> = OnceLock::new();
const MASK_VK_E8: u32 = 0xE8;

const MOD_BIT_LCTRL: u32 = 1 << 0;
const MOD_BIT_RCTRL: u32 = 1 << 1;
const MOD_BIT_LALT: u32 = 1 << 2;
const MOD_BIT_RALT: u32 = 1 << 3;
const MOD_BIT_LSHIFT: u32 = 1 << 4;
const MOD_BIT_RSHIFT: u32 = 1 << 5;
const MOD_BIT_LWIN: u32 = 1 << 6;
const MOD_BIT_RWIN: u32 = 1 << 7;

fn modifier_bit_for_vk(vk: u32) -> u32 {
    if vk == VK_LCONTROL as u32 || vk == VK_CONTROL as u32 {
        MOD_BIT_LCTRL
    } else if vk == VK_RCONTROL as u32 {
        MOD_BIT_RCTRL
    } else if vk == VK_LMENU as u32 || vk == VK_MENU as u32 {
        MOD_BIT_LALT
    } else if vk == VK_RMENU as u32 {
        MOD_BIT_RALT
    } else if vk == VK_LSHIFT as u32 || vk == VK_SHIFT as u32 {
        MOD_BIT_LSHIFT
    } else if vk == VK_RSHIFT as u32 {
        MOD_BIT_RSHIFT
    } else if vk == VK_LWIN as u32 {
        MOD_BIT_LWIN
    } else if vk == VK_RWIN as u32 {
        MOD_BIT_RWIN
    } else {
        0
    }
}

unsafe fn send_mask_key_vk_e8() {
    let inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: MASK_VK_E8 as u16,
                    wScan: 0,
                    dwFlags: 0,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: MASK_VK_E8 as u16,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];
    SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
}

pub fn execute_hotkey_action(action: HotkeyAction) {
    match action {
        HotkeyAction::ToggleOverlay => {}
        HotkeyAction::OpenTarget(target) => {
            std::thread::spawn(move || unsafe {
                let verb = wide_null("open");
                let file = wide_null(&target);
                ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                );
            });
        }
        HotkeyAction::RunCommand(cmd) => {
            std::thread::spawn(move || {
                let mut child = Command::new("powershell.exe");
                child.args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &cmd]);
                #[cfg(windows)]
                child.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                let _ = child.spawn();
            });
        }
    }
}

fn dispatch_binding(binding: &HotkeyBinding, tx: &mpsc::Sender<UiCommand>, ctx: &egui::Context) {
    match &binding.action {
        HotkeyAction::ToggleOverlay => {
            if try_claim_toggle_debounce(180) {
                let _ = tx.send(UiCommand::Toggle);
                ctx.request_repaint();
            }
        }
        HotkeyAction::OpenTarget(_) | HotkeyAction::RunCommand(_) => {
            if try_claim_debounce(&LAST_ACTION_MS, 220) {
                execute_hotkey_action(binding.action.clone());
            }
        }
    }
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        if let Some(shared) = HOOK_STATE.get() {
            let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
            // Ignore only our own vkE8 Win-menu mask key so OSK / injected keys still work
            if kb.vkCode != MASK_VK_E8 {
                let msg = wparam as u32;
                let mod_bit = modifier_bit_for_vk(kb.vkCode);

                if msg == WM_KEYUP || msg == WM_SYSKEYUP {
                    if mod_bit != 0 {
                        shared.mod_mask.fetch_and(!mod_bit, Ordering::Relaxed);
                    }
                    if shared.pressed_vk.load(Ordering::Relaxed) == kb.vkCode {
                        shared.pressed_vk.store(0, Ordering::Relaxed);
                    }
                } else if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                    if mod_bit != 0 {
                        shared.mod_mask.fetch_or(mod_bit, Ordering::Relaxed);
                    }

                    let mask = shared.mod_mask.load(Ordering::Relaxed);
                    let ctrl = (mask & (MOD_BIT_LCTRL | MOD_BIT_RCTRL)) != 0
                        || GetAsyncKeyState(VK_CONTROL as i32) < 0;
                    let alt = (mask & (MOD_BIT_LALT | MOD_BIT_RALT)) != 0
                        || GetAsyncKeyState(VK_MENU as i32) < 0;
                    let shift = (mask & (MOD_BIT_LSHIFT | MOD_BIT_RSHIFT)) != 0
                        || GetAsyncKeyState(VK_SHIFT as i32) < 0;
                    let win = (mask & (MOD_BIT_LWIN | MOD_BIT_RWIN)) != 0
                        || GetAsyncKeyState(VK_LWIN as i32) < 0
                        || GetAsyncKeyState(VK_RWIN as i32) < 0;

                    let matched = shared.hotkeys.try_lock().ok().and_then(|bindings| {
                        bindings
                            .iter()
                            .find(|b| {
                                b.spec.vk == kb.vkCode
                                    && b.spec.ctrl == ctrl
                                    && b.spec.alt == alt
                                    && b.spec.shift == shift
                                    && b.spec.win == win
                            })
                            .cloned()
                    });

                    if let Some(binding) = matched {
                        let now = now_epoch_ms();
                        let prev_vk = shared.pressed_vk.load(Ordering::Relaxed);
                        let prev_ms = shared.last_pressed_ms.load(Ordering::Relaxed);
                        // Suppress keyboard auto-repeat, auto-expiring after 600ms if keyup dropped
                        if prev_vk == kb.vkCode && now.saturating_sub(prev_ms) <= 600 {
                            return 1;
                        }
                        shared.pressed_vk.store(kb.vkCode, Ordering::Relaxed);
                        shared.last_pressed_ms.store(now, Ordering::Relaxed);

                        if binding.spec.win {
                            send_mask_key_vk_e8();
                        }
                        dispatch_binding(&binding, &shared.tx, &shared.ctx);
                        return 1;
                    }
                }
            }
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

fn win32_modifiers_for_spec(spec: &HotkeySpec) -> u32 {
    let mut mods = MOD_NOREPEAT;
    if spec.ctrl {
        mods |= MOD_CONTROL;
    }
    if spec.alt {
        mods |= MOD_ALT;
    }
    if spec.shift {
        mods |= MOD_SHIFT;
    }
    if spec.win {
        mods |= MOD_WIN;
    }
    mods
}

unsafe fn apply_bindings(
    desired: &Arc<Mutex<Vec<HotkeyBinding>>>,
    registered_map: &mut HashMap<i32, HotkeyBinding>,
    hook_handle: &mut HHOOK,
    tx: &mpsc::Sender<UiCommand>,
    ctx: &egui::Context,
) -> (usize, Vec<String>) {
    for (id, _) in registered_map.drain() {
        UnregisterHotKey(std::ptr::null_mut(), id);
    }
    if !hook_handle.is_null() {
        UnhookWindowsHookEx(*hook_handle);
        *hook_handle = std::ptr::null_mut();
    }

    let bindings = desired.lock().map(|g| g.clone()).unwrap_or_default();

    let mut fallback_hooks: Vec<HotkeyBinding> = Vec::new();
    let mut conflicts: Vec<String> = Vec::new();

    for (idx, binding) in bindings.into_iter().enumerate() {
        let id = (idx + 1) as i32;
        let mods = win32_modifiers_for_spec(&binding.spec);
        if RegisterHotKey(std::ptr::null_mut(), id, mods, binding.spec.vk) != 0 {
            registered_map.insert(id, binding);
        } else if binding.action == HotkeyAction::ToggleOverlay {
            fallback_hooks.push(binding);
        } else {
            conflicts.push(format!(
                "{} ({})",
                spec_to_display(&binding.spec),
                binding.name
            ));
        }
    }

    let shared = HOOK_STATE.get_or_init(|| HookShared {
        hotkeys: Mutex::new(Vec::new()),
        tx: tx.clone(),
        ctx: ctx.clone(),
        pressed_vk: AtomicU32::new(0),
        last_pressed_ms: AtomicU64::new(0),
        mod_mask: AtomicU32::new(0),
    });

    if !fallback_hooks.is_empty() {
        let hook = SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(ll_keyboard_proc),
            std::ptr::null_mut(),
            0,
        );
        if hook.is_null() {
            for b in fallback_hooks {
                conflicts.push(format!("{} ({})", spec_to_display(&b.spec), b.name));
            }
            if let Ok(mut g) = shared.hotkeys.lock() {
                g.clear();
            }
        } else {
            let fallback_count = fallback_hooks.len();
            if let Ok(mut g) = shared.hotkeys.lock() {
                *g = fallback_hooks;
            }
            *hook_handle = hook;
            return (registered_map.len() + fallback_count, conflicts);
        }
    } else if let Ok(mut g) = shared.hotkeys.lock() {
        g.clear();
    }

    (registered_map.len(), conflicts)
}

pub fn spawn_global_hotkeys(
    initial_bindings: Vec<HotkeyBinding>,
    tx: mpsc::Sender<UiCommand>,
    ctx: egui::Context,
) -> HotkeyController {
    let thread_id = Arc::new(AtomicU32::new(0));
    let desired = Arc::new(Mutex::new(initial_bindings));
    let announce_next = Arc::new(AtomicBool::new(false));

    let tid_worker = Arc::clone(&thread_id);
    let desired_worker = Arc::clone(&desired);
    let announce_worker = Arc::clone(&announce_next);

    std::thread::spawn(move || unsafe {
        let mut msg: MSG = std::mem::zeroed();
        // Force creation of this thread's Win32 message queue before exposing its thread ID
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        tid_worker.store(GetCurrentThreadId(), Ordering::Release);

        let mut registered_map: HashMap<i32, HotkeyBinding> = HashMap::new();
        let mut hook_handle: HHOOK = std::ptr::null_mut();

        let (_cnt, init_conflicts) = apply_bindings(
            &desired_worker,
            &mut registered_map,
            &mut hook_handle,
            &tx,
            &ctx,
        );
        if !init_conflicts.is_empty() {
            let _ = tx.send(UiCommand::HotkeysReloaded(
                false,
                format!("Hotkey conflict: {}", init_conflicts.join(", ")),
            ));
            ctx.request_repaint();
        }

        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_RELOAD_HOTKEYS {
                let (cnt, conflicts) = apply_bindings(
                    &desired_worker,
                    &mut registered_map,
                    &mut hook_handle,
                    &tx,
                    &ctx,
                );
                if announce_worker.swap(false, Ordering::AcqRel) || !conflicts.is_empty() {
                    if conflicts.is_empty() {
                        let _ = tx.send(UiCommand::HotkeysReloaded(
                            true,
                            format!("{cnt} active global hotkey(s) registered"),
                        ));
                    } else {
                        let _ = tx.send(UiCommand::HotkeysReloaded(
                            false,
                            format!("In use by another app: {}", conflicts.join(", ")),
                        ));
                    }
                    ctx.request_repaint();
                }
                continue;
            }

            if msg.message == WM_HOTKEY {
                let id = msg.wParam as i32;
                if let Some(binding) = registered_map.get(&id).cloned() {
                    dispatch_binding(&binding, &tx, &ctx);
                }
                continue;
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        for (id, _) in registered_map.drain() {
            UnregisterHotKey(std::ptr::null_mut(), id);
        }
        if !hook_handle.is_null() {
            UnhookWindowsHookEx(hook_handle);
        }
    });

    HotkeyController {
        thread_id,
        desired,
        announce_next,
    }
}

pub fn active_monitor_centered_pos(logical_w: f32, logical_h: f32, ppp: f32) -> (f32, f32) {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 {
            return (200.0, 120.0);
        }
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        if GetMonitorInfoW(hmon, &mut mi) == 0 {
            return (200.0, 120.0);
        }
        let scale = if ppp > 0.0 { ppp } else { 1.0 };
        let wl = mi.rcWork.left as f32 / scale;
        let wt = mi.rcWork.top as f32 / scale;
        let wr = mi.rcWork.right as f32 / scale;
        let wb = mi.rcWork.bottom as f32 / scale;

        let x = (wl + (wr - wl - logical_w) * 0.5).max(wl);
        let y = (wt + (wb - wt - logical_h) * 0.34).max(wt);
        (x, y)
    }
}

pub fn force_focus_window(title: &str) {
    unsafe {
        let wide = wide_null(title);
        let hwnd: HWND = FindWindowW(std::ptr::null(), wide.as_ptr());
        if hwnd.is_null() {
            return;
        }
        let fg = GetForegroundWindow();
        let fg_thread = if !fg.is_null() {
            GetWindowThreadProcessId(fg, std::ptr::null_mut())
        } else {
            0
        };
        let cur_thread = GetCurrentThreadId();
        let attached = fg_thread != 0
            && fg_thread != cur_thread
            && AttachThreadInput(cur_thread, fg_thread, 1) != 0;

        SetForegroundWindow(hwnd);

        if attached {
            AttachThreadInput(cur_thread, fg_thread, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ahk_hotkeys() {
        let k = parse_ahk_hotkey("$^!k", "Ctrl+Alt+K").unwrap();
        assert!(k.ctrl && k.alt && !k.shift && !k.win);
        assert_eq!(k.vk, b'K' as u32);

        let f7 = parse_ahk_hotkey("#F7", "Win+F7").unwrap();
        assert!(f7.win && !f7.ctrl && !f7.alt);
        assert_eq!(f7.vk, 0x76);
    }

    #[test]
    fn parses_display_and_formats_roundtrip() {
        let s1 = parse_display_hotkey("Ctrl+Alt+K").unwrap();
        assert_eq!(spec_to_ahk(&s1), "^!k");
        assert_eq!(spec_to_display(&s1), "Ctrl+Alt+K");

        let s2 = parse_display_hotkey("Win+Shift+F7").unwrap();
        assert_eq!(spec_to_ahk(&s2), "#+F7");
        assert_eq!(spec_to_display(&s2), "Win+Shift+F7");

        let s3 = parse_display_hotkey("ctrl + space").unwrap();
        assert_eq!(spec_to_ahk(&s3), "^Space");
        assert_eq!(spec_to_display(&s3), "Ctrl+Space");
    }

    #[test]
    fn validates_hotkey_specs() {
        let bare_a = parse_display_hotkey("A").unwrap();
        assert!(validate_hotkey_spec(&bare_a).is_err());

        let bare_esc = parse_display_hotkey("Esc").unwrap();
        assert!(validate_hotkey_spec(&bare_esc).is_err());

        let ctrl_alt_k = parse_display_hotkey("Ctrl+Alt+K").unwrap();
        assert!(validate_hotkey_spec(&ctrl_alt_k).is_ok());

        let fn_f9 = parse_display_hotkey("F9").unwrap();
        assert!(validate_hotkey_spec(&fn_f9).is_ok());
    }

    #[test]
    fn register_hotkey_win32_roundtrip() {
        unsafe {
            let mods = MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT;
            let vk = 0x86u32; // VK_F23
            let id = 0x5150i32;
            let ok = RegisterHotKey(std::ptr::null_mut(), id, mods, vk);
            assert_ne!(
                ok, 0,
                "RegisterHotKey should succeed for Ctrl+Alt+Shift+F23"
            );
            let unreg = UnregisterHotKey(std::ptr::null_mut(), id);
            assert_ne!(unreg, 0, "UnregisterHotKey should succeed");
        }
    }

    #[test]
    fn toggle_debounce_guards_double_fire() {
        LAST_TOGGLE_MS.store(0, Ordering::Release);
        assert!(try_claim_toggle_debounce(180));
        assert!(!try_claim_toggle_debounce(180));
    }
}
