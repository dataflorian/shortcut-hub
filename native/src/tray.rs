//! Native Windows system tray icon & menu for Shortcut Hub.

use eframe::egui;
use std::sync::mpsc;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::hotkey::UiCommand;

fn build_keyboard_icon_rgba() -> Vec<u8> {
    let size = 32usize;
    let mut buf = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let idx = (y * size + x) * 4;
            let in_box = (2..30).contains(&x) && (5..27).contains(&y);
            if !in_box {
                continue;
            }
            let is_border = x == 2 || x == 29 || y == 5 || y == 26;
            let is_keycap =
                ((6..=10).contains(&x) || (13..=18).contains(&x) || (21..=25).contains(&x))
                    && ((9..=13).contains(&y) || (16..=20).contains(&y));
            let is_space = (8..24).contains(&x) && (22..24).contains(&y);

            let (r, g, b, a) = if is_border {
                (137, 180, 250, 255) // Catppuccin Blue #89B4FA
            } else if is_keycap || is_space {
                (205, 214, 244, 255) // Catppuccin Text #CDD6F4
            } else {
                (30, 30, 46, 255) // Catppuccin Base #1E1E2E
            };
            buf[idx] = r;
            buf[idx + 1] = g;
            buf[idx + 2] = b;
            buf[idx + 3] = a;
        }
    }
    buf
}

pub struct TrayHandle {
    pub _tray: TrayIcon,
    toggle_item: MenuItem,
}

impl TrayHandle {
    pub fn update_hotkey_labels(&self, tooltip: &str, toggle_label: &str) {
        let _ = self._tray.set_tooltip(Some(tooltip));
        self.toggle_item.set_text(toggle_label);
    }
}

pub fn create_tray(
    tooltip: &str,
    toggle_label: &str,
    tx: mpsc::Sender<UiCommand>,
    ctx: egui::Context,
) -> Option<TrayHandle> {
    let menu = Menu::new();
    let item_toggle = MenuItem::new(toggle_label, true, None);
    let item_rescan = MenuItem::new("Auto-Scan & Sync to PowerToys", true, None);
    let item_folder = MenuItem::new("Open Project Folder", true, None);
    let item_quit = MenuItem::new("Quit", true, None);

    let toggle_item_clone = item_toggle.clone();
    let toggle_id: MenuId = item_toggle.id().clone();
    let rescan_id: MenuId = item_rescan.id().clone();
    let folder_id: MenuId = item_folder.id().clone();
    let quit_id: MenuId = item_quit.id().clone();

    let _ = menu.append(&item_toggle);
    let _ = menu.append(&item_rescan);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&item_folder);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&item_quit);

    let icon = Icon::from_rgba(build_keyboard_icon_rgba(), 32, 32).ok()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(tooltip)
        .with_icon(icon)
        .build()
        .ok()?;

    let tx_menu = tx.clone();
    let ctx_menu = ctx.clone();
    std::thread::spawn(move || {
        let rx = MenuEvent::receiver();
        while let Ok(ev) = rx.recv() {
            let cmd = if ev.id == toggle_id {
                Some(UiCommand::Toggle)
            } else if ev.id == rescan_id {
                Some(UiCommand::Rescan)
            } else if ev.id == folder_id {
                Some(UiCommand::OpenFolder)
            } else if ev.id == quit_id {
                Some(UiCommand::Quit)
            } else {
                None
            };
            if let Some(c) = cmd {
                let _ = tx_menu.send(c);
                ctx_menu.request_repaint();
            }
        }
    });

    std::thread::spawn(move || {
        let rx = TrayIconEvent::receiver();
        while let Ok(ev) = rx.recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = ev
            {
                let _ = tx.send(UiCommand::Toggle);
                ctx.request_repaint();
            }
        }
    });

    Some(TrayHandle {
        _tray: tray,
        toggle_item: toggle_item_clone,
    })
}
