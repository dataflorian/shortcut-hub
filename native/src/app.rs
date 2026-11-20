//! GPU-accelerated egui 0.37 overlay UI and shortcut customizer.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2, ViewportCommand};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use crate::data::{
    add_manual, delete_manual, hide_item, load_catalog, restore_all_windows, restore_item,
    spawn_powertoys_sync, update_manual, Catalog, ShortcutItem,
};
use crate::hotkey::{
    active_monitor_centered_pos, force_focus_window, matches_egui_hotkey,
    try_claim_toggle_debounce, HotkeySpec, UiCommand,
};
use crate::tray::TrayHandle;

const BG_MAIN: Color32 = Color32::from_rgb(30, 30, 46); // #1E1E2E
const BG_HEADER: Color32 = Color32::from_rgb(24, 24, 37); // #181825
const BG_CARD: Color32 = Color32::from_rgb(42, 43, 60); // #2A2B3C
const BG_CARD_ALT: Color32 = Color32::from_rgb(49, 50, 68); // #313244
const BG_KEYCAP: Color32 = Color32::from_rgb(69, 71, 90); // #45475A
const BORDER_KEYCAP: Color32 = Color32::from_rgb(88, 91, 112); // #585B70
const FG_TEXT: Color32 = Color32::from_rgb(205, 214, 244); // #CDD6F4
const FG_MUTED: Color32 = Color32::from_rgb(166, 173, 200); // #A6ADC8
const FG_ACCENT: Color32 = Color32::from_rgb(137, 180, 250); // #89B4FA
const FG_GREEN: Color32 = Color32::from_rgb(166, 227, 161); // #A6E3A1
const FG_RED: Color32 = Color32::from_rgb(243, 139, 168); // #F38BA8
const FG_YELLOW: Color32 = Color32::from_rgb(249, 226, 175); // #F9E2AF
const FG_PURPLE: Color32 = Color32::from_rgb(203, 166, 247); // #CBA6F7
const FG_TEAL: Color32 = Color32::from_rgb(148, 226, 213); // #94E2D5
const FG_DARK: Color32 = Color32::from_rgb(17, 17, 27); // #11111B

#[derive(Debug, Clone)]
pub struct ScreenshotOpts {
    pub out_path: PathBuf,
    pub initial_filter: Option<String>,
    pub initial_search: Option<String>,
    pub failed_flag: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CategoryFilter {
    All,
    CustomAndAuto,
    Named(String),
    Windows,
    Hidden,
}

impl CategoryFilter {
    fn from_cli(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "all" => Self::All,
            "custom" | "custom & auto" => Self::CustomAndAuto,
            "windows" => Self::Windows,
            "hidden" | "hidden / restore" => Self::Hidden,
            _ => Self::Named(raw.trim().to_string()),
        }
    }
}

#[derive(Debug, Clone)]
struct EditorModal {
    editing_id: Option<String>,
    category: String,
    keys: String,
    name: String,
    description: String,
    recording_key: bool,
    error: Option<String>,
}

fn save_png_from_color_image(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let file = File::create(path).map_err(|e| e.to_string())?;
    let w = u32::try_from(image.width()).map_err(|e| e.to_string())?;
    let h = u32::try_from(image.height()).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(image.as_raw())
        .map_err(|e| e.to_string())
}

pub struct ShortcutHubApp {
    project_dir: PathBuf,
    catalog: Catalog,
    filter: CategoryFilter,
    search: String,
    visible: bool,
    quitting: bool,
    smoke_test: bool,
    smoke_frames: u32,
    screenshot: Option<ScreenshotOpts>,
    screenshot_frames: u32,
    screenshot_requested: bool,
    focus_search_frames: u8,
    pending_native_focus: bool,
    status_banner: Option<(String, bool)>,
    editor: Option<EditorModal>,
    hotkeys: Vec<HotkeySpec>,
    hotkeys_footer_label: String,
    cmd_rx: mpsc::Receiver<UiCommand>,
    cmd_tx: mpsc::Sender<UiCommand>,
    _tray: Option<TrayHandle>,
}

impl ShortcutHubApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: &egui::Context,
        project_dir: PathBuf,
        start_visible: bool,
        smoke_test: bool,
        screenshot: Option<ScreenshotOpts>,
        hotkeys: Vec<HotkeySpec>,
        hotkeys_footer_label: String,
        cmd_rx: mpsc::Receiver<UiCommand>,
        cmd_tx: mpsc::Sender<UiCommand>,
        tray: Option<TrayHandle>,
    ) -> Self {
        Self::apply_theme(ctx);
        let catalog = load_catalog(&project_dir);
        let automated = smoke_test || screenshot.is_some();
        let focus_search_frames = if start_visible && !automated { 3 } else { 0 };
        if start_visible && !automated {
            let (px, py) = active_monitor_centered_pos(920.0, 620.0, ctx.pixels_per_point());
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::pos2(px, py)));
        }

        let initial_filter = screenshot
            .as_ref()
            .and_then(|s| s.initial_filter.as_deref())
            .map(CategoryFilter::from_cli)
            .unwrap_or(CategoryFilter::All);
        let initial_search = screenshot
            .as_ref()
            .and_then(|s| s.initial_search.clone())
            .unwrap_or_default();

        Self {
            project_dir,
            catalog,
            filter: initial_filter,
            search: initial_search,
            visible: start_visible,
            quitting: false,
            smoke_test,
            smoke_frames: 0,
            screenshot,
            screenshot_frames: 0,
            screenshot_requested: false,
            focus_search_frames,
            pending_native_focus: false,
            status_banner: None,
            editor: None,
            hotkeys,
            hotkeys_footer_label,
            cmd_rx,
            cmd_tx,
            _tray: tray,
        }
    }

    fn apply_theme(ctx: &egui::Context) {
        ctx.set_theme(egui::Theme::Dark);
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG_MAIN;
        visuals.window_fill = BG_MAIN;
        visuals.extreme_bg_color = BG_CARD_ALT;
        visuals.faint_bg_color = BG_CARD;
        visuals.widgets.noninteractive.bg_fill = BG_CARD;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, FG_TEXT);
        visuals.widgets.inactive.bg_fill = BG_CARD_ALT;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, FG_TEXT);
        visuals.widgets.hovered.bg_fill = BG_KEYCAP;
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, FG_ACCENT);
        visuals.widgets.active.bg_fill = FG_ACCENT;
        visuals.widgets.active.fg_stroke = Stroke::new(1.0, FG_DARK);
        visuals.selection.bg_fill = FG_ACCENT.linear_multiply(0.35);
        visuals.window_corner_radius = CornerRadius::same(10);
        visuals.window_stroke = Stroke::new(1.0, BG_KEYCAP);
        ctx.set_visuals(visuals);
    }

    fn show_overlay(&mut self, ctx: &egui::Context) {
        self.catalog = load_catalog(&self.project_dir);
        let (px, py) = active_monitor_centered_pos(920.0, 620.0, ctx.pixels_per_point());
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::pos2(px, py)));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.visible = true;
        self.focus_search_frames = 3;
        self.pending_native_focus = true;
        ctx.request_repaint();
    }

    fn hide_overlay(&mut self, ctx: &egui::Context) {
        self.visible = false;
        self.editor = None;
        self.pending_native_focus = false;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        ctx.request_repaint();
    }

    fn trigger_sync(&mut self, ctx: &egui::Context, rescan: bool, pending_msg: &str) {
        self.catalog = load_catalog(&self.project_dir);
        self.status_banner = Some((pending_msg.to_string(), true));
        let tx = self.cmd_tx.clone();
        let ctx_clone = ctx.clone();
        spawn_powertoys_sync(self.project_dir.clone(), rescan, move |ok, msg| {
            let _ = tx.send(UiCommand::SyncFinished(ok, msg));
            ctx_clone.request_repaint();
        });
    }

    fn process_commands(&mut self, ctx: &egui::Context) {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                UiCommand::Toggle => {
                    if self.visible {
                        self.hide_overlay(ctx);
                    } else {
                        self.show_overlay(ctx);
                    }
                }
                UiCommand::Show => {
                    if !self.visible {
                        self.show_overlay(ctx);
                    }
                }
                UiCommand::Hide => {
                    if self.visible {
                        self.hide_overlay(ctx);
                    }
                }
                UiCommand::Rescan => {
                    self.trigger_sync(ctx, true, "Auto-scanning system shortcuts...");
                }
                UiCommand::OpenFolder => {
                    let _ = Command::new("explorer.exe").arg(&self.project_dir).spawn();
                }
                UiCommand::SyncFinished(ok, msg) => {
                    self.catalog = load_catalog(&self.project_dir);
                    self.status_banner = Some((msg, ok));
                    ctx.request_repaint();
                }
                UiCommand::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide_overlay(ctx);
        }
    }

    fn filtered_items(&self) -> Vec<&ShortcutItem> {
        let is_hidden = self.filter == CategoryFilter::Hidden;
        let pool = if is_hidden {
            &self.catalog.hidden
        } else {
            &self.catalog.active
        };

        let tokens: Vec<String> = self
            .search
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();

        pool.iter()
            .filter(|item| {
                if !is_hidden {
                    match &self.filter {
                        CategoryFilter::All => {}
                        CategoryFilter::CustomAndAuto => {
                            if item.source == "windows" || item.source == "powertoys" {
                                return false;
                            }
                        }
                        CategoryFilter::Windows => {
                            if item.source != "windows" {
                                return false;
                            }
                        }
                        CategoryFilter::Named(cat) => {
                            if &item.category != cat {
                                return false;
                            }
                        }
                        CategoryFilter::Hidden => {}
                    }
                }
                if tokens.is_empty() {
                    return true;
                }
                let hay = format!(
                    "{} {} {} {}",
                    item.keys, item.name, item.description, item.category
                )
                .to_ascii_lowercase();
                tokens.iter().all(|tok| hay.contains(tok))
            })
            .collect()
    }

    fn source_badge(source: &str) -> (&'static str, Color32) {
        match source {
            "manual" => ("Manual", FG_GREEN),
            "ahk" => ("AHK", FG_ACCENT),
            "ahk-config" => ("Auto", FG_ACCENT),
            "powertoys" => ("PowerToys", FG_YELLOW),
            "copyq" => ("CopyQ", FG_PURPLE),
            "vscode" => ("IDE", FG_TEAL),
            "windows" => ("Windows", FG_MUTED),
            _ => ("Auto", FG_ACCENT),
        }
    }

    fn render_header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let header_frame = egui::Frame::new()
            .fill(BG_HEADER)
            .inner_margin(Margin::symmetric(14, 10));

        let resp = header_frame
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Shortcut Hub")
                            .size(17.0)
                            .strong()
                            .color(FG_ACCENT),
                    );
                    ui.add_space(8.0);
                    if let Some((msg, ok)) = &self.status_banner {
                        ui.label(RichText::new(msg).size(12.0).color(if *ok {
                            FG_GREEN
                        } else {
                            FG_RED
                        }));
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{} active  ·  {} hidden",
                                self.catalog.stats.active_total, self.catalog.stats.hidden_total
                            ))
                            .size(12.0)
                            .color(FG_MUTED),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let close_btn = egui::Button::new(
                            RichText::new(" X ").size(13.0).strong().color(FG_MUTED),
                        )
                        .fill(BG_HEADER)
                        .frame(false);
                        if ui.add(close_btn).clicked() {
                            self.hide_overlay(ctx);
                        }

                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Restore Windows")
                                        .size(12.0)
                                        .strong()
                                        .color(FG_GREEN),
                                )
                                .fill(BG_CARD_ALT)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            if let Ok(cnt) = restore_all_windows(&self.project_dir) {
                                self.trigger_sync(
                                    ctx,
                                    false,
                                    &format!("Restored {cnt} Windows shortcut(s) — syncing..."),
                                );
                            }
                        }

                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Auto-Scan")
                                        .size(12.0)
                                        .strong()
                                        .color(FG_TEXT),
                                )
                                .fill(BG_CARD_ALT)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            self.trigger_sync(ctx, true, "Auto-scanning system shortcuts...");
                        }

                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("+ Add Shortcut")
                                        .size(12.0)
                                        .strong()
                                        .color(FG_DARK),
                                )
                                .fill(FG_ACCENT)
                                .corner_radius(CornerRadius::same(6)),
                            )
                            .clicked()
                        {
                            self.editor = Some(EditorModal {
                                editing_id: None,
                                category: "Custom".to_string(),
                                keys: String::new(),
                                name: String::new(),
                                description: String::new(),
                                recording_key: false,
                                error: None,
                            });
                        }
                    });
                });
            })
            .response;

        if resp.interact(egui::Sense::drag()).drag_started() {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
    }

    fn render_search_and_chips(&mut self, ui: &mut egui::Ui, shown_count: usize) {
        egui::Frame::new()
            .fill(BG_CARD_ALT)
            .stroke(Stroke::new(1.0, BG_KEYCAP))
            .corner_radius(CornerRadius::same(7))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Search:")
                            .size(13.0)
                            .strong()
                            .color(FG_ACCENT),
                    );
                    let search_resp = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .desired_width(ui.available_width() - 95.0)
                            .frame(egui::Frame::NONE)
                            .text_color(FG_TEXT)
                            .font(egui::FontId::proportional(14.5))
                            .hint_text("Filter by key, action, app, or category..."),
                    );
                    if self.focus_search_frames > 0 && self.editor.is_none() {
                        search_resp.request_focus();
                        self.focus_search_frames -= 1;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{shown_count} shown"))
                                .size(11.5)
                                .color(FG_MUTED),
                        );
                    });
                });
            });

        ui.add_space(7.0);

        let mut chips: Vec<(CategoryFilter, String)> = vec![
            (CategoryFilter::All, "All".to_string()),
            (CategoryFilter::CustomAndAuto, "Custom & Auto".to_string()),
        ];
        for cat in &self.catalog.custom_categories {
            chips.push((CategoryFilter::Named(cat.clone()), cat.clone()));
        }
        if self.catalog.stats.powertoys > 0 {
            chips.push((
                CategoryFilter::Named("PowerToys".to_string()),
                "PowerToys".to_string(),
            ));
        }
        chips.push((CategoryFilter::Windows, "Windows".to_string()));
        chips.push((
            CategoryFilter::Hidden,
            format!("Hidden / Restore ({})", self.catalog.stats.hidden_total),
        ));

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);
            for (target, label) in chips {
                let selected = self.filter == target;
                let btn = egui::Button::new(
                    RichText::new(label).size(11.5).strong().color(if selected {
                        FG_DARK
                    } else {
                        FG_TEXT
                    }),
                )
                .fill(if selected { FG_ACCENT } else { BG_CARD_ALT })
                .corner_radius(CornerRadius::same(6));
                if ui.add(btn).clicked() {
                    self.filter = target;
                }
            }
        });
    }

    fn render_keycaps(ui: &mut egui::Ui, keys: &str) {
        let primary_variant = keys.split(" / ").next().unwrap_or(keys);
        let tokens: Vec<&str> = primary_variant
            .split('+')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();

        ui.allocate_ui_with_layout(
            Vec2::new(210.0, 26.0),
            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
            |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for (idx, tok) in tokens.iter().enumerate() {
                    if idx > 0 {
                        ui.label(RichText::new("+").size(11.0).color(FG_MUTED));
                    }
                    egui::Frame::new()
                        .fill(BG_KEYCAP)
                        .stroke(Stroke::new(1.0, BORDER_KEYCAP))
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(7, 2))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(*tok)
                                    .monospace()
                                    .size(11.5)
                                    .strong()
                                    .color(FG_TEXT),
                            );
                        });
                }
            },
        );
    }

    fn render_list(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        filtered: &[ShortcutItem],
        max_h: f32,
    ) {
        let is_hidden_tab = self.filter == CategoryFilter::Hidden;
        let mut grouped: BTreeMap<String, Vec<&ShortcutItem>> = BTreeMap::new();
        let mut cat_order: Vec<String> = Vec::new();
        for item in filtered {
            let cat = if item.category.is_empty() {
                "General".to_string()
            } else {
                item.category.clone()
            };
            if !grouped.contains_key(&cat) {
                cat_order.push(cat.clone());
            }
            grouped.entry(cat).or_default().push(item);
        }

        let mut hide_id: Option<String> = None;
        let mut restore_id: Option<String> = None;
        let mut delete_id: Option<String> = None;
        let mut edit_item: Option<ShortcutItem> = None;

        egui::ScrollArea::vertical()
            .max_height(max_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.add_space(40.0);
                    ui.centered_and_justified(|ui| {
                        let msg = if is_hidden_tab {
                            "No hidden shortcuts. Click 'Hide' on any shortcut to remove it from your overview."
                        } else {
                            "No shortcuts match your filter."
                        };
                        ui.label(RichText::new(msg).size(13.0).color(FG_MUTED));
                    });
                    return;
                }

                for cat in &cat_order {
                    let Some(items) = grouped.get(cat) else {
                        continue;
                    };
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{}  ({})", cat.to_ascii_uppercase(), items.len()))
                            .size(11.5)
                            .strong()
                            .color(FG_ACCENT),
                    );
                    ui.add_space(3.0);

                    for item in items {
                        egui::Frame::new()
                            .fill(BG_CARD)
                            .corner_radius(CornerRadius::same(7))
                            .inner_margin(Margin::symmetric(10, 7))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    Self::render_keycaps(ui, &item.keys);

                                    ui.add_space(6.0);
                                    ui.vertical(|ui| {
                                        ui.set_max_width(
                                            (ui.available_width() - 165.0).max(180.0),
                                        );
                                        ui.label(
                                            RichText::new(&item.name)
                                                .size(13.0)
                                                .strong()
                                                .color(FG_TEXT),
                                        );
                                        if !item.description.is_empty() {
                                            ui.label(
                                                RichText::new(&item.description)
                                                    .size(11.5)
                                                    .color(FG_MUTED),
                                            );
                                        }
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if is_hidden_tab {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("Restore")
                                                                .size(11.5)
                                                                .strong()
                                                                .color(FG_DARK),
                                                        )
                                                        .fill(FG_GREEN)
                                                        .corner_radius(CornerRadius::same(5)),
                                                    )
                                                    .clicked()
                                                {
                                                    restore_id = Some(item.id.clone());
                                                }
                                            } else {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("Hide")
                                                                .size(11.5)
                                                                .color(FG_MUTED),
                                                        )
                                                        .fill(BG_CARD_ALT)
                                                        .corner_radius(CornerRadius::same(5)),
                                                    )
                                                    .clicked()
                                                {
                                                    hide_id = Some(item.id.clone());
                                                }

                                                if item.source == "manual" {
                                                    if ui
                                                        .add(
                                                            egui::Button::new(
                                                                RichText::new("Del")
                                                                    .size(11.5)
                                                                    .color(FG_RED),
                                                            )
                                                            .fill(BG_CARD_ALT)
                                                            .corner_radius(CornerRadius::same(5)),
                                                        )
                                                        .clicked()
                                                    {
                                                        delete_id = Some(item.id.clone());
                                                    }
                                                    if ui
                                                        .add(
                                                            egui::Button::new(
                                                                RichText::new("Edit")
                                                                    .size(11.5)
                                                                    .color(FG_TEXT),
                                                            )
                                                            .fill(BG_CARD_ALT)
                                                            .corner_radius(CornerRadius::same(5)),
                                                        )
                                                        .clicked()
                                                    {
                                                        edit_item = Some((*item).clone());
                                                    }
                                                }
                                            }

                                            let (src_lbl, src_col) =
                                                Self::source_badge(&item.source);
                                            ui.add_space(6.0);
                                            ui.label(
                                                RichText::new(src_lbl)
                                                    .size(11.0)
                                                    .strong()
                                                    .color(src_col),
                                            );
                                        },
                                    );
                                });
                            });
                        ui.add_space(3.0);
                    }
                }
            });

        if let Some(id) = hide_id {
            if hide_item(&self.project_dir, &id).is_ok() {
                self.trigger_sync(ctx, false, "Shortcut hidden — syncing PowerToys...");
            }
        } else if let Some(id) = restore_id {
            if restore_item(&self.project_dir, &id).is_ok() {
                self.trigger_sync(ctx, false, "Shortcut restored — syncing PowerToys...");
            }
        } else if let Some(id) = delete_id {
            if delete_manual(&self.project_dir, &id).is_ok() {
                self.trigger_sync(ctx, false, "Custom shortcut deleted — syncing...");
            }
        } else if let Some(item) = edit_item {
            self.editor = Some(EditorModal {
                editing_id: Some(item.id),
                category: item.category,
                keys: item.keys,
                name: item.name,
                description: item.description,
                recording_key: false,
                error: None,
            });
        }
    }

    fn render_editor_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.editor.take() else {
            return;
        };

        let mut close_modal = false;
        let mut save_requested = false;
        let title = if modal.editing_id.is_some() {
            "Edit Custom Shortcut"
        } else {
            "Add Custom Shortcut"
        };

        if modal.recording_key {
            let captured: Option<String> = ctx.input(|i| {
                for ev in &i.events {
                    if let egui::Event::Key {
                        key, pressed: true, ..
                    } = ev
                    {
                        let mut parts: Vec<String> = Vec::new();
                        if i.modifiers.ctrl {
                            parts.push("Ctrl".to_string());
                        }
                        if i.modifiers.alt {
                            parts.push("Alt".to_string());
                        }
                        if i.modifiers.shift {
                            parts.push("Shift".to_string());
                        }
                        if i.modifiers.mac_cmd || i.modifiers.command && !i.modifiers.ctrl {
                            parts.push("Win".to_string());
                        }
                        parts.push(key.name().to_string());
                        return Some(parts.join("+"));
                    }
                }
                None
            });
            if let Some(combo) = captured {
                modal.keys = combo;
                modal.recording_key = false;
            }
        }

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(430.0, 290.0))
            .frame(
                egui::Frame::new()
                    .fill(BG_MAIN)
                    .stroke(Stroke::new(1.5, FG_ACCENT))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Category (pick or type new):")
                        .size(12.0)
                        .color(FG_MUTED),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut modal.category)
                            .text_color(FG_TEXT)
                            .desired_width(250.0),
                    );
                    egui::ComboBox::from_id_salt("cat_pick")
                        .selected_text("Presets")
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut modal.category,
                                "Custom".to_string(),
                                "Custom",
                            );
                            for cat in &self.catalog.custom_categories {
                                ui.selectable_value(&mut modal.category, cat.clone(), cat);
                            }
                        });
                });

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Shortcut Keys (e.g. Ctrl+Alt+N):")
                        .size(12.0)
                        .color(FG_MUTED),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut modal.keys)
                            .text_color(FG_TEXT)
                            .desired_width(305.0),
                    );
                    let rec_lbl = if modal.recording_key {
                        "Press Keys..."
                    } else {
                        "Record"
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(rec_lbl).size(11.5).strong().color(
                                if modal.recording_key {
                                    FG_DARK
                                } else {
                                    FG_TEXT
                                },
                            ))
                            .fill(if modal.recording_key {
                                FG_YELLOW
                            } else {
                                BG_KEYCAP
                            })
                            .corner_radius(CornerRadius::same(5)),
                        )
                        .clicked()
                    {
                        modal.recording_key = !modal.recording_key;
                    }
                });

                ui.add_space(8.0);
                ui.label(RichText::new("Action Name:").size(12.0).color(FG_MUTED));
                ui.add(
                    egui::TextEdit::singleline(&mut modal.name)
                        .text_color(FG_TEXT)
                        .desired_width(f32::INFINITY),
                );

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Description (optional):")
                        .size(12.0)
                        .color(FG_MUTED),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut modal.description)
                        .text_color(FG_TEXT)
                        .desired_width(f32::INFINITY),
                );

                if let Some(err) = &modal.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(err).size(11.5).color(FG_RED));
                }

                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Save & Sync")
                                    .size(12.5)
                                    .strong()
                                    .color(FG_DARK),
                            )
                            .fill(FG_ACCENT)
                            .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        save_requested = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Cancel").size(12.5).color(FG_TEXT))
                                .fill(BG_CARD_ALT)
                                .corner_radius(CornerRadius::same(6)),
                        )
                        .clicked()
                    {
                        close_modal = true;
                    }
                });
            });

        if save_requested {
            let res = if let Some(edit_id) = &modal.editing_id {
                update_manual(
                    &self.project_dir,
                    edit_id,
                    &modal.category,
                    &modal.keys,
                    &modal.name,
                    &modal.description,
                )
            } else {
                add_manual(
                    &self.project_dir,
                    &modal.category,
                    &modal.keys,
                    &modal.name,
                    &modal.description,
                )
                .map(|_| ())
            };
            match res {
                Ok(()) => {
                    self.trigger_sync(ctx, false, "Saved & synced with PowerToys");
                    return;
                }
                Err(e) => {
                    modal.error = Some(e);
                }
            }
        }

        if !close_modal {
            self.editor = Some(modal);
        }
    }
}

impl eframe::App for ShortcutHubApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.process_commands(&ctx);

        if self.smoke_test {
            self.smoke_frames += 1;
            if self.smoke_frames >= 2 {
                println!(
                    "SMOKE_TEST_OK active={} hidden={} ppp={:.2}",
                    self.catalog.stats.active_total,
                    self.catalog.stats.hidden_total,
                    ctx.pixels_per_point()
                );
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }

        if self.pending_native_focus {
            if self.focus_search_frames <= 2 {
                force_focus_window("Shortcut Hub");
                self.pending_native_focus = false;
            } else {
                ctx.request_repaint();
            }
        }

        if self.visible && self.editor.is_none() {
            let hotkey_pressed =
                ctx.input(|i| self.hotkeys.iter().any(|spec| matches_egui_hotkey(i, spec)));
            if hotkey_pressed && try_claim_toggle_debounce(180) {
                self.hide_overlay(&ctx);
            } else if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                if !self.search.trim().is_empty() {
                    self.search.clear();
                    self.focus_search_frames = 2;
                } else {
                    self.hide_overlay(&ctx);
                }
            }
        } else if self.visible {
            let esc_in_modal = self.editor.as_ref().is_some_and(|m| !m.recording_key)
                && ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if esc_in_modal {
                self.editor = None;
            }
        }

        if !self.visible && !self.smoke_test && self.screenshot.is_none() {
            return;
        }

        let filtered_owned: Vec<ShortcutItem> =
            self.filtered_items().into_iter().cloned().collect();

        egui::Frame::new()
            .fill(BG_MAIN)
            .stroke(Stroke::new(1.0, BG_KEYCAP))
            .corner_radius(CornerRadius::same(10))
            .show(ui, |ui| {
                self.render_header(ui, &ctx);

                egui::Frame::new()
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        self.render_search_and_chips(ui, filtered_owned.len());
                        ui.add_space(8.0);
                        let list_h = (ui.available_height() - 34.0).max(160.0);
                        self.render_list(ui, &ctx, &filtered_owned, list_h);
                    });

                let footer_text = format!(
                    "{}: Open / Close   ·   Type to filter   ·   Esc: Clear / Close   ·   Synced with PowerToys Shortcut Guide",
                    self.hotkeys_footer_label
                );
                egui::Frame::new()
                    .fill(BG_HEADER)
                    .inner_margin(Margin::symmetric(14, 7))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(footer_text).size(11.0).color(FG_MUTED));
                        });
                    });
            });

        self.render_editor_modal(&ctx);

        if let Some(opts) = &self.screenshot {
            self.screenshot_frames += 1;
            if self.screenshot_frames == 3 && !self.screenshot_requested {
                self.screenshot_requested = true;
                let out_path = opts.out_path.clone();
                let failed_flag = Arc::clone(&opts.failed_flag);
                let tx = self.cmd_tx.clone();
                let ctx_cb = ctx.clone();
                ctx.request_screenshot(move |image| {
                    match save_png_from_color_image(&out_path, &image) {
                        Ok(()) => {
                            println!(
                                "SCREENSHOT_OK path={} width={} height={}",
                                out_path.display(),
                                image.width(),
                                image.height()
                            );
                        }
                        Err(err) => {
                            eprintln!("SCREENSHOT_ERR {err}");
                            failed_flag.store(true, Ordering::Release);
                        }
                    }
                    let _ = tx.send(UiCommand::Quit);
                    ctx_cb.request_repaint();
                });
            } else if self.screenshot_frames >= 60 {
                eprintln!("SCREENSHOT_ERR timeout waiting for egui screenshot callback");
                opts.failed_flag.store(true, Ordering::Release);
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            ctx.request_repaint();
        }

        if self.smoke_test {
            ctx.request_repaint();
        }
    }
}
