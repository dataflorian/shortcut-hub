"""Claude-style dark global shortcut reference overlay & customizer in Tkinter."""

from __future__ import annotations

import argparse
import ctypes
import sys
import threading
import tkinter as tk
from tkinter import messagebox, ttk
from typing import Any

import store

_BG_BORDER = "#45475A"
_BG_MAIN = "#1E1E2E"
_BG_HEADER = "#181825"
_BG_CARD = "#2A2B3C"
_BG_CARD_ALT = "#313244"
_BG_KEYCAP = "#45475A"
_BG_CHIP = "#313244"
_BG_CHIP_ACTIVE = "#89B4FA"
_FG_TEXT = "#CDD6F4"
_FG_MUTED = "#A6ADC8"
_FG_ACCENT = "#89B4FA"
_FG_GREEN = "#A6E3A1"
_FG_RED = "#F38BA8"
_FG_YELLOW = "#F9E2AF"
_FG_DARK = "#11111B"

_SOURCE_COLORS: dict[str, tuple[str, str]] = {
    "manual": ("Manual", _FG_GREEN),
    "ahk": ("AHK", _FG_ACCENT),
    "ahk-config": ("Auto", _FG_ACCENT),
    "powertoys": ("PowerToys", _FG_YELLOW),
    "copyq": ("CopyQ", "#CBA6F7"),
    "vscode": ("IDE", "#94E2D5"),
    "windows": ("Windows", _FG_MUTED),
}


def _enable_dpi_awareness() -> None:
    if sys.platform == "win32":
        try:
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except Exception:
            try:
                ctypes.windll.user32.SetProcessDPIAware()
            except Exception:
                pass


class ShortcutHubApp:
    """Interactive dark-mode overlay and customizer for all system & custom shortcuts."""

    def __init__(self, project_dir: store.Path, smoke_test: bool = False) -> None:
        self.project_dir = project_dir
        self.smoke_test = smoke_test
        self.root = tk.Tk()
        self.root.title("Shortcut Hub")
        self.root.configure(bg=_BG_BORDER)
        self.root.overrideredirect(True)
        self.root.attributes("-topmost", True)

        self.scale = max(1.0, self.root.winfo_fpixels("1i") / 96.0)
        screen_w = self.root.winfo_screenwidth()
        screen_h = self.root.winfo_screenheight()
        self.win_w = min(screen_w - self._px(40), self._px(960))
        self.win_h = min(screen_h - self._px(60), self._px(650))
        pos_x = max(0, (screen_w - self.win_w) // 2)
        pos_y = max(0, round((screen_h - self.win_h) * 0.35))
        self.root.geometry(f"{self.win_w}x{self.win_h}+{pos_x}+{pos_y}")

        self.catalog = store.get_catalog(self.project_dir, rescan=False)
        self.active_filter = "All"
        self.search_var = tk.StringVar()
        self._search_after_id: str | None = None
        self._drag_start = (0, 0)
        self._chip_buttons: dict[str, tk.Button] = {}
        self._sync_busy = False

        self._build_ui()
        self._bind_events()
        self._refresh_chips()
        self._render_list()

        if not self.smoke_test:
            self.root.after(25, self._focus_search)

    def _px(self, logical: int) -> int:
        return max(1, round(logical * self.scale))

    def _build_ui(self) -> None:
        pad = self._px(14)
        outer = tk.Frame(self.root, bg=_BG_MAIN)
        outer.pack(fill="both", expand=True, padx=self._px(1), pady=self._px(1))

        # --- Header / Drag Bar ---
        header = tk.Frame(outer, bg=_BG_HEADER, height=self._px(50))
        header.pack(fill="x", side="top")
        header.pack_propagate(False)
        header.bind("<ButtonPress-1>", self._on_drag_start)
        header.bind("<B1-Motion>", self._on_drag_move)

        title_lbl = tk.Label(
            header,
            text="Shortcut Hub",
            font=("Segoe UI", 13, "bold"),
            fg=_FG_ACCENT,
            bg=_BG_HEADER,
        )
        title_lbl.pack(side="left", padx=(pad, self._px(10)))
        title_lbl.bind("<ButtonPress-1>", self._on_drag_start)
        title_lbl.bind("<B1-Motion>", self._on_drag_move)

        self.status_lbl = tk.Label(
            header,
            text="",
            font=("Segoe UI", 9),
            fg=_FG_MUTED,
            bg=_BG_HEADER,
        )
        self.status_lbl.pack(side="left", padx=self._px(4))
        self.status_lbl.bind("<ButtonPress-1>", self._on_drag_start)
        self.status_lbl.bind("<B1-Motion>", self._on_drag_move)

        btn_style: dict[str, Any] = {
            "font": ("Segoe UI", 9, "bold"),
            "bd": 0,
            "relief": "flat",
            "cursor": "hand2",
            "padx": self._px(10),
            "pady": self._px(4),
        }

        tk.Button(
            header,
            text="✕",
            command=self.root.destroy,
            bg=_BG_HEADER,
            fg=_FG_MUTED,
            activebackground=_BG_CARD_ALT,
            activeforeground=_FG_RED,
            **{**btn_style, "padx": self._px(8)},
        ).pack(side="right", padx=(self._px(4), self._px(10)))

        self.restore_win_btn = tk.Button(
            header,
            text="Restore Windows Defaults",
            command=self._on_restore_all_windows,
            bg=_BG_CARD_ALT,
            fg=_FG_GREEN,
            activebackground=_BG_KEYCAP,
            activeforeground=_FG_GREEN,
            **btn_style,
        )
        self.restore_win_btn.pack(side="right", padx=self._px(4))

        tk.Button(
            header,
            text="↻ Auto-Scan",
            command=self._on_rescan,
            bg=_BG_CARD_ALT,
            fg=_FG_TEXT,
            activebackground=_BG_KEYCAP,
            activeforeground=_FG_ACCENT,
            **btn_style,
        ).pack(side="right", padx=self._px(4))

        tk.Button(
            header,
            text="+ Add Shortcut",
            command=lambda: self._open_editor_modal(None),
            bg=_FG_ACCENT,
            fg=_FG_DARK,
            activebackground="#B4BEFE",
            activeforeground=_FG_DARK,
            **btn_style,
        ).pack(side="right", padx=self._px(4))

        # --- Search Row ---
        search_bar = tk.Frame(outer, bg=_BG_MAIN)
        search_bar.pack(fill="x", padx=pad, pady=(self._px(10), self._px(6)))

        search_box = tk.Frame(search_bar, bg=_BG_CARD_ALT, highlightbackground=_BG_KEYCAP, highlightthickness=1)
        search_box.pack(fill="x")

        tk.Label(
            search_box,
            text="Search:",
            font=("Segoe UI", 10, "bold"),
            fg=_FG_ACCENT,
            bg=_BG_CARD_ALT,
        ).pack(side="left", padx=(self._px(10), self._px(6)))

        self.search_entry = tk.Entry(
            search_box,
            textvariable=self.search_var,
            font=("Segoe UI", 11),
            bg=_BG_CARD_ALT,
            fg=_FG_TEXT,
            insertbackground=_FG_ACCENT,
            bd=0,
            relief="flat",
        )
        self.search_entry.pack(side="left", fill="x", expand=True, ipady=self._px(6), padx=(0, self._px(8)))

        self.match_lbl = tk.Label(
            search_box,
            text="",
            font=("Segoe UI", 9),
            fg=_FG_MUTED,
            bg=_BG_CARD_ALT,
        )
        self.match_lbl.pack(side="right", padx=self._px(10))

        # --- Category Chips Bar ---
        self.chips_frame = tk.Frame(outer, bg=_BG_MAIN)
        self.chips_frame.pack(fill="x", padx=pad, pady=(0, self._px(8)))

        # --- Scrollable Shortcut List ---
        body = tk.Frame(outer, bg=_BG_MAIN)
        body.pack(fill="both", expand=True, padx=pad, pady=(0, self._px(8)))

        self.canvas = tk.Canvas(
            body, bg=_BG_MAIN, highlightthickness=0, bd=0
        )
        scrollbar = ttk.Scrollbar(body, orient="vertical", command=self.canvas.yview)
        self.list_frame = tk.Frame(self.canvas, bg=_BG_MAIN)

        self.list_frame.bind(
            "<Configure>",
            lambda _e: self.canvas.configure(scrollregion=self.canvas.bbox("all")),
        )
        self.canvas_window = self.canvas.create_window(
            (0, 0), window=self.list_frame, anchor="nw"
        )
        self.canvas.bind(
            "<Configure>",
            lambda e: self.canvas.itemconfigure(self.canvas_window, width=e.width),
        )
        self.canvas.configure(yscrollcommand=scrollbar.set)

        self.canvas.pack(side="left", fill="both", expand=True)
        scrollbar.pack(side="right", fill="y")

        # --- Footer Hint ---
        footer = tk.Frame(outer, bg=_BG_HEADER, height=self._px(30))
        footer.pack(fill="x", side="bottom")
        footer.pack_propagate(False)
        tk.Label(
            footer,
            text="Ctrl+Alt+K / Win+F7: Toggle   |   Type to filter   |   Esc: Clear / Close   |   Auto-synced with PowerToys Shortcut Guide",
            font=("Segoe UI", 9),
            fg=_FG_MUTED,
            bg=_BG_HEADER,
        ).pack(side="left", padx=pad)

    def _bind_events(self) -> None:
        self.search_var.trace_add("write", self._on_search_changed)
        self.root.bind("<Escape>", self._on_escape)
        self.root.bind_all("<MouseWheel>", self._on_mousewheel)

    def _focus_search(self) -> None:
        try:
            self.root.focus_force()
            self.search_entry.focus_set()
        except tk.TclError:
            pass

    def _on_drag_start(self, event: tk.Event) -> None:
        self._drag_start = (event.x_root - self.root.winfo_x(), event.y_root - self.root.winfo_y())

    def _on_drag_move(self, event: tk.Event) -> None:
        nx = event.x_root - self._drag_start[0]
        ny = event.y_root - self._drag_start[1]
        self.root.geometry(f"+{nx}+{ny}")

    def _on_mousewheel(self, event: tk.Event) -> None:
        try:
            if event.widget.winfo_toplevel() == self.root:
                self.canvas.yview_scroll(int(-1 * (event.delta / 120)), "units")
        except tk.TclError:
            pass

    def _on_escape(self, _event: tk.Event | None = None) -> None:
        if self.search_var.get().strip():
            self.search_var.set("")
            self.search_entry.focus_set()
        else:
            self.root.destroy()

    def _on_search_changed(self, *_args: Any) -> None:
        if self._search_after_id is not None:
            self.root.after_cancel(self._search_after_id)
        self._search_after_id = self.root.after(75, self._render_list)

    def _update_status_text(self, custom_msg: str = "") -> None:
        stats = self.catalog["stats"]
        if custom_msg:
            self.status_lbl.configure(text=custom_msg, fg=_FG_GREEN)
        else:
            self.status_lbl.configure(
                text=f"{stats['active_total']} active  ·  {stats['hidden_total']} hidden",
                fg=_FG_MUTED,
            )

    def _refresh_chips(self) -> None:
        for child in self.chips_frame.winfo_children():
            child.destroy()
        self._chip_buttons.clear()

        stats = self.catalog["stats"]
        chips: list[tuple[str, str]] = [
            ("All", "All"),
            ("Custom & Auto", "Custom & Auto"),
        ]
        for cat in self.catalog["custom_categories"]:
            chips.append((cat, cat))
        if stats["powertoys"] > 0:
            chips.append(("PowerToys", "PowerToys"))
        chips.append(("Windows", "Windows"))
        chips.append(("__hidden__", f"Hidden / Restore ({stats['hidden_total']})"))

        row_frame = tk.Frame(self.chips_frame, bg=_BG_MAIN)
        row_frame.pack(fill="x", anchor="w")
        cur_width = 0
        max_width = self.win_w - self._px(40)

        for key, label in chips:
            est_w = self._px(len(label) * 8 + 26)
            if cur_width + est_w > max_width and cur_width > 0:
                row_frame = tk.Frame(self.chips_frame, bg=_BG_MAIN)
                row_frame.pack(fill="x", anchor="w", pady=(self._px(4), 0))
                cur_width = 0

            is_sel = self.active_filter == key
            btn = tk.Button(
                row_frame,
                text=label,
                command=lambda k=key: self._select_filter(k),
                font=("Segoe UI", 9, "bold" if is_sel else "normal"),
                bg=_BG_CHIP_ACTIVE if is_sel else _BG_CHIP,
                fg=_FG_DARK if is_sel else _FG_TEXT,
                activebackground=_BG_CHIP_ACTIVE,
                activeforeground=_FG_DARK,
                bd=0,
                relief="flat",
                cursor="hand2",
                padx=self._px(10),
                pady=self._px(3),
            )
            btn.pack(side="left", padx=(0, self._px(6)))
            self._chip_buttons[key] = btn
            cur_width += est_w

        self._update_status_text()

    def _select_filter(self, filter_key: str) -> None:
        self.active_filter = filter_key
        self._refresh_chips()
        self._render_list()

    def _filtered_items(self) -> list[dict[str, Any]]:
        is_hidden_tab = self.active_filter == "__hidden__"
        pool = self.catalog["hidden"] if is_hidden_tab else self.catalog["active"]

        if not is_hidden_tab:
            if self.active_filter == "Custom & Auto":
                pool = [x for x in pool if x.get("source") not in ("windows", "powertoys")]
            elif self.active_filter == "Windows":
                pool = [x for x in pool if x.get("source") == "windows"]
            elif self.active_filter != "All":
                pool = [x for x in pool if x.get("category") == self.active_filter]

        q = self.search_var.get().strip().lower()
        if q:
            tokens = q.split()
            matched: list[dict[str, Any]] = []
            for item in pool:
                hay = " ".join(
                    [
                        str(item.get("keys", "")),
                        str(item.get("name", "")),
                        str(item.get("description", "")),
                        str(item.get("category", "")),
                    ]
                ).lower()
                if all(tok in hay for tok in tokens):
                    matched.append(item)
            pool = matched
        return pool

    def _render_list(self) -> None:
        self._search_after_id = None
        for child in self.list_frame.winfo_children():
            child.destroy()

        items = self._filtered_items()
        self.match_lbl.configure(text=f"{len(items)} shown")

        if not items:
            empty_msg = (
                "No hidden shortcuts. Click 'Hide' on any item to remove it from your overview."
                if self.active_filter == "__hidden__"
                else "No shortcuts match your filter."
            )
            tk.Label(
                self.list_frame,
                text=empty_msg,
                font=("Segoe UI", 10),
                fg=_FG_MUTED,
                bg=_BG_MAIN,
            ).pack(pady=self._px(40))
            self.canvas.yview_moveto(0)
            return

        grouped: dict[str, list[dict[str, Any]]] = {}
        for item in items:
            grouped.setdefault(str(item.get("category") or "General"), []).append(item)

        is_hidden_tab = self.active_filter == "__hidden__"
        for cat_name, cat_items in grouped.items():
            hdr = tk.Label(
                self.list_frame,
                text=f"{cat_name.upper()}  ({len(cat_items)})",
                font=("Segoe UI", 9, "bold"),
                fg=_FG_ACCENT,
                bg=_BG_MAIN,
                anchor="w",
            )
            hdr.pack(fill="x", pady=(self._px(10), self._px(4)))

            for item in cat_items:
                self._render_row(item, is_hidden_tab)

        self.canvas.yview_moveto(0)

    def _render_row(self, item: dict[str, Any], is_hidden_tab: bool) -> None:
        row = tk.Frame(self.list_frame, bg=_BG_CARD)
        row.pack(fill="x", pady=self._px(2))

        # Action buttons on right
        act_frame = tk.Frame(row, bg=_BG_CARD)
        act_frame.pack(side="right", padx=self._px(8), pady=self._px(6))

        src_label, src_color = _SOURCE_COLORS.get(
            str(item.get("source", "")), ("Auto", _FG_ACCENT)
        )
        tk.Label(
            act_frame,
            text=src_label,
            font=("Segoe UI", 8, "bold"),
            fg=src_color,
            bg=_BG_CARD,
        ).pack(side="left", padx=(0, self._px(8)))

        small_btn: dict[str, Any] = {
            "font": ("Segoe UI", 8, "bold"),
            "bd": 0,
            "relief": "flat",
            "cursor": "hand2",
            "padx": self._px(7),
            "pady": self._px(2),
        }

        if is_hidden_tab:
            tk.Button(
                act_frame,
                text="Restore",
                command=lambda iid=item["id"]: self._on_restore(iid),
                bg=_FG_GREEN,
                fg=_FG_DARK,
                **small_btn,
            ).pack(side="left")
        else:
            if item.get("source") == "manual":
                tk.Button(
                    act_frame,
                    text="Edit",
                    command=lambda it=item: self._open_editor_modal(it),
                    bg=_BG_CARD_ALT,
                    fg=_FG_TEXT,
                    **small_btn,
                ).pack(side="left", padx=(0, self._px(4)))
                tk.Button(
                    act_frame,
                    text="Del",
                    command=lambda iid=item["id"]: self._on_delete_manual(iid),
                    bg=_BG_CARD_ALT,
                    fg=_FG_RED,
                    **small_btn,
                ).pack(side="left", padx=(0, self._px(4)))

            tk.Button(
                act_frame,
                text="Hide",
                command=lambda iid=item["id"]: self._on_hide(iid),
                bg=_BG_CARD_ALT,
                fg=_FG_MUTED,
                **small_btn,
            ).pack(side="left")

        # Keycap badge column on left
        keys_box = tk.Frame(row, bg=_BG_CARD, width=self._px(210), height=self._px(28))
        keys_box.pack(side="left", padx=(self._px(10), self._px(8)), pady=self._px(6))
        keys_box.pack_propagate(False)

        keys_inner = tk.Frame(keys_box, bg=_BG_CARD)
        keys_inner.pack(side="left", fill="y")

        first_variant = str(item.get("keys", "")).split(" / ")[0]
        for k_idx, token in enumerate(t.strip() for t in first_variant.split("+") if t.strip()):
            if k_idx > 0:
                tk.Label(
                    keys_inner,
                    text="+",
                    font=("Segoe UI", 8),
                    fg=_FG_MUTED,
                    bg=_BG_CARD,
                ).pack(side="left", padx=self._px(2))
            tk.Label(
                keys_inner,
                text=token,
                font=("Consolas", 9, "bold"),
                fg=_FG_TEXT,
                bg=_BG_KEYCAP,
                padx=self._px(6),
                pady=self._px(2),
            ).pack(side="left")

        # Title + optional description in middle
        text_box = tk.Frame(row, bg=_BG_CARD)
        text_box.pack(side="left", fill="both", expand=True, pady=self._px(5))

        tk.Label(
            text_box,
            text=str(item.get("name", "")),
            font=("Segoe UI", 10, "bold"),
            fg=_FG_TEXT,
            bg=_BG_CARD,
            anchor="w",
        ).pack(fill="x")

        desc = str(item.get("description") or "").strip()
        if desc:
            tk.Label(
                text_box,
                text=desc,
                font=("Segoe UI", 9),
                fg=_FG_MUTED,
                bg=_BG_CARD,
                anchor="w",
            ).pack(fill="x")

    def _reload_and_sync_async(self, rescan: bool = False, msg: str = "Synced with PowerToys") -> None:
        self.catalog = store.get_catalog(self.project_dir, rescan=False)
        self._refresh_chips()
        self._render_list()
        self._update_status_text("Syncing to PowerToys...")

        def _worker() -> None:
            res = store.sync_to_powertoys(self.project_dir, rescan=rescan)
            self.root.after(0, lambda: self._on_sync_done(res, msg))

        threading.Thread(target=_worker, daemon=True).start()

    def _on_sync_done(self, res: dict[str, Any], msg: str) -> None:
        self.catalog = store.get_catalog(self.project_dir, rescan=False)
        self._refresh_chips()
        self._render_list()
        if res.get("ok"):
            self._update_status_text(msg)
        else:
            self._update_status_text(f"Sync warning: {res.get('index_output', 'check logs')}")
        self.root.after(3000, lambda: self._update_status_text())

    def _on_hide(self, item_id: str) -> None:
        store.hide_shortcut(self.project_dir, item_id)
        self._reload_and_sync_async(rescan=False, msg="Shortcut hidden & PowerToys updated")

    def _on_restore(self, item_id: str) -> None:
        store.restore_shortcut(self.project_dir, item_id)
        self._reload_and_sync_async(rescan=False, msg="Shortcut restored & PowerToys updated")

    def _on_restore_all_windows(self) -> None:
        count = store.restore_all_windows(self.project_dir)
        self._reload_and_sync_async(
            rescan=False,
            msg=f"Restored {count} Windows shortcut(s)",
        )

    def _on_delete_manual(self, item_id: str) -> None:
        store.delete_manual_shortcut(self.project_dir, item_id)
        self._reload_and_sync_async(rescan=False, msg="Custom shortcut deleted")

    def _on_rescan(self) -> None:
        self._update_status_text("Scanning system shortcuts...")

        def _worker() -> None:
            res = store.sync_to_powertoys(self.project_dir, rescan=True)
            self.root.after(0, lambda: self._on_sync_done(res, "Auto-scan complete"))

        threading.Thread(target=_worker, daemon=True).start()

    def _open_editor_modal(self, existing: dict[str, Any] | None) -> None:
        modal = tk.Toplevel(self.root)
        modal.title("Edit Shortcut" if existing else "Add Custom Shortcut")
        modal.configure(bg=_BG_MAIN)
        modal.attributes("-topmost", True)
        modal.transient(self.root)
        modal.grab_set()

        mw, mh = self._px(460), self._px(310)
        mx = self.root.winfo_x() + (self.win_w - mw) // 2
        my = self.root.winfo_y() + (self.win_h - mh) // 2
        modal.geometry(f"{mw}x{mh}+{mx}+{my}")

        pad = self._px(16)
        tk.Label(
            modal,
            text="Edit Custom Shortcut" if existing else "Add Custom Shortcut",
            font=("Segoe UI", 11, "bold"),
            fg=_FG_ACCENT,
            bg=_BG_MAIN,
        ).pack(anchor="w", padx=pad, pady=(pad, self._px(10)))

        form = tk.Frame(modal, bg=_BG_MAIN)
        form.pack(fill="both", expand=True, padx=pad)

        cats = list(dict.fromkeys(["Custom"] + self.catalog["custom_categories"]))
        cat_var = tk.StringVar(value=str(existing.get("category", "Custom")) if existing else "Custom")
        keys_var = tk.StringVar(value=str(existing.get("keys", "")) if existing else "")
        name_var = tk.StringVar(value=str(existing.get("name", "")) if existing else "")
        desc_var = tk.StringVar(value=str(existing.get("description", "")) if existing else "")

        tk.Label(form, text="Category (pick or type new):", font=("Segoe UI", 9), fg=_FG_MUTED, bg=_BG_MAIN).pack(anchor="w")
        cat_combo = ttk.Combobox(form, textvariable=cat_var, values=cats)
        cat_combo.pack(fill="x", pady=(self._px(2), self._px(8)))

        tk.Label(form, text="Shortcut Keys (e.g. Ctrl+Alt+N):", font=("Segoe UI", 9), fg=_FG_MUTED, bg=_BG_MAIN).pack(anchor="w")
        key_row = tk.Frame(form, bg=_BG_MAIN)
        key_row.pack(fill="x", pady=(self._px(2), self._px(8)))
        keys_entry = tk.Entry(key_row, textvariable=keys_var, font=("Consolas", 10), bg=_BG_CARD_ALT, fg=_FG_TEXT, insertbackground=_FG_ACCENT, bd=0)
        keys_entry.pack(side="left", fill="x", expand=True, ipady=self._px(5), padx=(0, self._px(6)))

        recording = {"active": False}

        def _toggle_record() -> None:
            recording["active"] = not recording["active"]
            rec_btn.configure(
                text="Press Keys..." if recording["active"] else "Record",
                bg=_FG_YELLOW if recording["active"] else _BG_KEYCAP,
                fg=_FG_DARK if recording["active"] else _FG_TEXT,
            )
            if recording["active"]:
                keys_entry.focus_set()

        def _on_key_record(event: tk.Event) -> str | None:
            if not recording["active"]:
                return None
            if event.keysym in ("Control_L", "Control_R", "Alt_L", "Alt_R", "Shift_L", "Shift_R", "Super_L", "Super_R"):
                return "break"
            mods: list[str] = []
            if event.state & 0x4:
                mods.append("Ctrl")
            if event.state & 0x20000 or event.state & 0x8:
                mods.append("Alt")
            if event.state & 0x1:
                mods.append("Shift")
            key_name = event.keysym.upper() if len(event.keysym) == 1 else event.keysym
            keys_var.set("+".join(mods + [key_name]))
            recording["active"] = False
            rec_btn.configure(text="Record", bg=_BG_KEYCAP, fg=_FG_TEXT)
            return "break"

        keys_entry.bind("<KeyPress>", _on_key_record)
        rec_btn = tk.Button(key_row, text="Record", command=_toggle_record, font=("Segoe UI", 9, "bold"), bg=_BG_KEYCAP, fg=_FG_TEXT, bd=0, padx=self._px(10), pady=self._px(4), cursor="hand2")
        rec_btn.pack(side="right")

        tk.Label(form, text="Action Name:", font=("Segoe UI", 9), fg=_FG_MUTED, bg=_BG_MAIN).pack(anchor="w")
        name_entry = tk.Entry(form, textvariable=name_var, font=("Segoe UI", 10), bg=_BG_CARD_ALT, fg=_FG_TEXT, insertbackground=_FG_ACCENT, bd=0)
        name_entry.pack(fill="x", ipady=self._px(5), pady=(self._px(2), self._px(8)))

        tk.Label(form, text="Description (optional):", font=("Segoe UI", 9), fg=_FG_MUTED, bg=_BG_MAIN).pack(anchor="w")
        desc_entry = tk.Entry(form, textvariable=desc_var, font=("Segoe UI", 10), bg=_BG_CARD_ALT, fg=_FG_TEXT, insertbackground=_FG_ACCENT, bd=0)
        desc_entry.pack(fill="x", ipady=self._px(5), pady=(self._px(2), self._px(10)))

        def _save() -> None:
            try:
                if existing:
                    store.update_manual_shortcut(
                        self.project_dir,
                        existing["id"],
                        cat_var.get(),
                        keys_var.get(),
                        name_var.get(),
                        desc_var.get(),
                    )
                else:
                    store.add_manual_shortcut(
                        self.project_dir,
                        cat_var.get(),
                        keys_var.get(),
                        name_var.get(),
                        desc_var.get(),
                    )
            except ValueError as exc:
                messagebox.showerror("Invalid Shortcut", str(exc), parent=modal)
                return
            modal.destroy()
            self._reload_and_sync_async(
                rescan=False,
                msg="Saved & synced to PowerToys",
            )

        btn_row = tk.Frame(modal, bg=_BG_MAIN)
        btn_row.pack(fill="x", padx=pad, pady=(0, pad))
        tk.Button(btn_row, text="Cancel", command=modal.destroy, font=("Segoe UI", 9), bg=_BG_CARD_ALT, fg=_FG_TEXT, bd=0, padx=self._px(12), pady=self._px(5), cursor="hand2").pack(side="right", padx=(self._px(6), 0))
        tk.Button(btn_row, text="Save & Sync", command=_save, font=("Segoe UI", 9, "bold"), bg=_FG_ACCENT, fg=_FG_DARK, bd=0, padx=self._px(14), pady=self._px(5), cursor="hand2").pack(side="right")

        keys_entry.focus_set()

    def run(self) -> int:
        """Start Tkinter event loop (or verify render and exit in smoke-test mode)."""
        if self.smoke_test:
            self.root.update_idletasks()
            self.root.update()
            stats = self.catalog["stats"]
            print(
                f"SMOKE_TEST_OK active={stats['active_total']} hidden={stats['hidden_total']} "
                f"win={self.win_w}x{self.win_h} scale={self.scale:.2f}"
            )
            self.root.destroy()
            return 0 if stats["active_total"] > 0 else 1

        self.root.mainloop()
        return 0


def main(argv: list[str] | None = None) -> int:
    """CLI entrypoint."""
    parser = argparse.ArgumentParser(description="Shortcut Hub Overlay & Customizer")
    parser.add_argument(
        "--project-dir",
        default=str(store._default_project_dir()),
        help="Path to shortcut-hub directory.",
    )
    parser.add_argument(
        "--smoke-test",
        action="store_true",
        help="Render once, verify loaded catalog > 0, print diagnostics, and exit.",
    )
    args = parser.parse_args(argv)
    _enable_dpi_awareness()
    app = ShortcutHubApp(
        project_dir=store.Path(args.project_dir).resolve(),
        smoke_test=args.smoke_test,
    )
    return app.run()


if __name__ == "__main__":
    sys.exit(main())
