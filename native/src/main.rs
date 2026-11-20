#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod data;
mod hotkey;
mod tray;

use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use crate::app::{ScreenshotOpts, ShortcutHubApp};
use crate::data::{collect_live_bindings, load_catalog, load_config, resolve_project_dir};
use crate::hotkey::{
    acquire_single_instance, spawn_event_listener, spawn_global_hotkeys, SingleInstanceOutcome,
    UiCommand,
};
use crate::tray::create_tray;

#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn run_bench(project_dir: &std::path::Path) -> i32 {
    let mut load_us_total: u128 = 0;
    let mut last_count = 0usize;
    for _ in 0..50 {
        let t0 = Instant::now();
        let cat = load_catalog(project_dir);
        load_us_total += t0.elapsed().as_micros();
        last_count = cat.stats.active_total;
    }
    let avg_load_ms = (load_us_total as f64 / 50.0) / 1000.0;

    let cat = load_catalog(project_dir);
    let queries = ["clipboard", "virtual desktop", "ctrl alt", "window"];
    let t1 = Instant::now();
    let mut matches = 0usize;
    for iter in 0..1000 {
        let q = queries[iter % queries.len()];
        let toks: Vec<&str> = q.split_whitespace().collect();
        for item in &cat.active {
            let hay = format!(
                "{} {} {} {}",
                item.keys, item.name, item.description, item.category
            )
            .to_ascii_lowercase();
            if toks.iter().all(|tok| hay.contains(tok)) {
                matches += 1;
            }
        }
    }
    let avg_filter_us = t1.elapsed().as_micros() as f64 / 1000.0;

    println!(
        "BENCH_OK active={last_count} avg_catalog_load_ms={avg_load_ms:.3} avg_filter_us={avg_filter_us:.1} (sample_hits={matches})"
    );
    0
}

fn main() -> eframe::Result<()> {
    let mut start_hidden = false;
    let mut bench_mode = false;
    let mut smoke_test = false;
    let mut screenshot_path: Option<PathBuf> = None;
    let mut initial_filter: Option<String> = None;
    let mut initial_search: Option<String> = None;
    let mut open_hotkeys_modal = false;
    let mut project_dir_override: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--hidden" => start_hidden = true,
            "--bench" => bench_mode = true,
            "--smoke-test" => smoke_test = true,
            "--open-hotkeys" => open_hotkeys_modal = true,
            "--screenshot" => {
                if let Some(p) = args.next() {
                    screenshot_path = Some(PathBuf::from(p));
                }
            }
            "--filter" => {
                if let Some(f) = args.next() {
                    initial_filter = Some(f);
                }
            }
            "--search" => {
                if let Some(s) = args.next() {
                    initial_search = Some(s);
                }
            }
            "--project-dir" => {
                if let Some(p) = args.next() {
                    project_dir_override = Some(PathBuf::from(p));
                }
            }
            _ => {}
        }
    }

    if bench_mode || smoke_test || screenshot_path.is_some() {
        #[cfg(windows)]
        attach_parent_console();
    }

    let project_dir = project_dir_override.unwrap_or_else(resolve_project_dir);

    if bench_mode {
        std::process::exit(run_bench(&project_dir));
    }

    let automated = smoke_test || screenshot_path.is_some();
    let single = if automated {
        None
    } else {
        match acquire_single_instance() {
            SingleInstanceOutcome::AlreadyRunningSignaled => return Ok(()),
            SingleInstanceOutcome::Primary {
                _mutex,
                toggle_event,
            } => Some((_mutex, toggle_event)),
        }
    };

    let cfg = load_config(&project_dir);
    let initial_catalog = load_catalog(&project_dir);
    let initial_bindings = collect_live_bindings(&cfg, &initial_catalog);

    let mut labels: Vec<String> = Vec::new();
    if !cfg.hotkeys.toggle_overlay_display.trim().is_empty() {
        labels.push(cfg.hotkeys.toggle_overlay_display.trim().to_string());
    }
    if !cfg.hotkeys.toggle_overlay_alt_display.trim().is_empty() {
        labels.push(cfg.hotkeys.toggle_overlay_alt_display.trim().to_string());
    }
    let hotkeys_label = if labels.is_empty() {
        "Ctrl+Alt+K / Win+F7".to_string()
    } else {
        labels.join(" / ")
    };

    let screenshot_failed = Arc::new(AtomicBool::new(false));
    let screenshot_opts = screenshot_path.map(|out_path| ScreenshotOpts {
        out_path,
        initial_filter,
        initial_search,
        open_hotkeys_modal,
        failed_flag: Arc::clone(&screenshot_failed),
    });

    let start_visible = if automated { true } else { !start_hidden };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Shortcut Hub")
            .with_inner_size([920.0, 620.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_visible(start_visible)
            .with_taskbar(false),
        ..Default::default()
    };

    let run_res = eframe::run_native(
        "Shortcut Hub",
        options,
        Box::new(move |cc| {
            let (tx, rx) = mpsc::channel::<UiCommand>();
            let tray = if automated {
                None
            } else {
                let tooltip = format!("Shortcut Hub ({hotkeys_label})");
                let toggle_lbl = format!("Toggle Shortcut Hub ({hotkeys_label})");
                create_tray(&tooltip, &toggle_lbl, tx.clone(), cc.egui_ctx.clone())
            };

            let hotkey_ctrl = if !automated {
                let ctrl = spawn_global_hotkeys(initial_bindings, tx.clone(), cc.egui_ctx.clone());
                if let Some((_mutex_handle, ev_handle)) = single {
                    // SendHandle intentionally does not close the Win32 mutex HANDLE on drop
                    spawn_event_listener(ev_handle, tx.clone(), cc.egui_ctx.clone());
                }
                Some(ctrl)
            } else {
                None
            };

            Ok(Box::new(ShortcutHubApp::new(
                &cc.egui_ctx,
                project_dir,
                cfg,
                initial_catalog,
                start_visible,
                smoke_test,
                screenshot_opts,
                hotkey_ctrl,
                rx,
                tx,
                tray,
            )))
        }),
    );

    if screenshot_failed.load(Ordering::Acquire) {
        std::process::exit(1);
    }

    run_res
}
