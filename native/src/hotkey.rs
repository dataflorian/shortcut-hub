//! Win32 RegisterHotKey primary handler, WH_KEYBOARD_LL fallback, single-instance IPC, and focus helpers.

use eframe::egui;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

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
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, FindWindowW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, SetForegroundWindow, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum UiCommand {
    Toggle,
    Show,
    Hide,
    Rescan,
    OpenFolder,
    SyncFinished(bool, String),
    Quit,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
    pub label: String,
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

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Atomically claims a toggle slot if at least `min_interval_ms` elapsed since the previous toggle.
pub fn try_claim_toggle_debounce(min_interval_ms: u64) -> bool {
    let now = now_epoch_ms();
    loop {
        let prev = LAST_TOGGLE_MS.load(Ordering::Acquire);
        if now.saturating_sub(prev) < min_interval_ms {
            return false;
        }
        if LAST_TOGGLE_MS
            .compare_exchange(prev, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return true;
        }
    }
}

fn wide_null(s: &str) -> Vec<u16> {
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
        "pgup" | "pageup" => Some(0x21),
        "pgdn" | "pagedown" => Some(0x22),
        "up" => Some(0x26),
        "down" => Some(0x28),
        "left" => Some(0x25),
        "right" => Some(0x27),
        _ => None,
    }
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

/// Checks if the given hotkey was pressed inside the focused egui frame with matching modifiers.
pub fn matches_egui_hotkey(input: &egui::InputState, spec: &HotkeySpec) -> bool {
    let Some(egui_key) = vk_to_egui_key(spec.vk) else {
        return false;
    };
    if !input.key_pressed(egui_key) {
        return false;
    }
    let win_down =
        unsafe { GetAsyncKeyState(VK_LWIN as i32) < 0 || GetAsyncKeyState(VK_RWIN as i32) < 0 };
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
    Some(HotkeySpec {
        ctrl,
        alt,
        shift,
        win,
        vk,
        label: label.to_string(),
    })
}

struct HookShared {
    hotkeys: Vec<HotkeySpec>,
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

                    if let Some(matched) = shared.hotkeys.iter().find(|spec| {
                        spec.vk == kb.vkCode
                            && spec.ctrl == ctrl
                            && spec.alt == alt
                            && spec.shift == shift
                            && spec.win == win
                    }) {
                        let now = now_epoch_ms();
                        let prev_vk = shared.pressed_vk.load(Ordering::Relaxed);
                        let prev_ms = shared.last_pressed_ms.load(Ordering::Relaxed);
                        // Suppress keyboard auto-repeat, auto-expiring after 600ms if keyup dropped
                        if prev_vk == kb.vkCode && now.saturating_sub(prev_ms) <= 600 {
                            return 1;
                        }
                        shared.pressed_vk.store(kb.vkCode, Ordering::Relaxed);
                        shared.last_pressed_ms.store(now, Ordering::Relaxed);

                        if !try_claim_toggle_debounce(180) {
                            return 1;
                        }

                        if matched.win {
                            send_mask_key_vk_e8();
                        }
                        let _ = shared.tx.send(UiCommand::Toggle);
                        shared.ctx.request_repaint();
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

pub fn spawn_global_hotkeys(
    hotkeys: Vec<HotkeySpec>,
    tx: mpsc::Sender<UiCommand>,
    ctx: egui::Context,
) {
    if hotkeys.is_empty() {
        return;
    }

    std::thread::spawn(move || unsafe {
        let mut registered_ids: Vec<i32> = Vec::new();
        let mut fallback_specs: Vec<HotkeySpec> = Vec::new();

        for (idx, spec) in hotkeys.into_iter().enumerate() {
            let id = (idx + 1) as i32;
            let mods = win32_modifiers_for_spec(&spec);
            if RegisterHotKey(std::ptr::null_mut(), id, mods, spec.vk) != 0 {
                registered_ids.push(id);
            } else {
                fallback_specs.push(spec);
            }
        }

        let hook = if !fallback_specs.is_empty() {
            let _ = HOOK_STATE.set(HookShared {
                hotkeys: fallback_specs,
                tx: tx.clone(),
                ctx: ctx.clone(),
                pressed_vk: AtomicU32::new(0),
                last_pressed_ms: AtomicU64::new(0),
                mod_mask: AtomicU32::new(0),
            });
            let h = SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(ll_keyboard_proc),
                std::ptr::null_mut(),
                0,
            );
            if h.is_null() {
                None
            } else {
                Some(h)
            }
        } else {
            None
        };

        if registered_ids.is_empty() && hook.is_none() {
            return;
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_HOTKEY && try_claim_toggle_debounce(180) {
                let _ = tx.send(UiCommand::Toggle);
                ctx.request_repaint();
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        for id in registered_ids {
            UnregisterHotKey(std::ptr::null_mut(), id);
        }
        if let Some(h) = hook {
            UnhookWindowsHookEx(h);
        }
    });
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
