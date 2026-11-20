"""Shortcut Hub store: manual shortcuts, hide/restore state, and PowerToys sync."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import sys
import uuid
from pathlib import Path
from typing import Any

from discovery import discover_all
from powertoys_yaml import (
    atomic_write_json,
    parse_manifest,
    run_powertoys_indexer,
    write_custom_manifest,
    write_filtered_shell_manifest,
)


_BUILTIN_WINDOWS_SHORTCUTS: tuple[tuple[str, str, str, str], ...] = (
    ("Desktop & Window Management", "Alt+Tab", "Switch open windows", "Cycle forward through active apps"),
    ("Desktop & Window Management", "Alt+F4", "Close active window", "Quit the focused app or open shutdown dialog on desktop"),
    ("Desktop & Window Management", "Win+Left", "Snap window left", "Tile the active window to the left half of the screen"),
    ("Desktop & Window Management", "Win+Right", "Snap window right", "Tile the active window to the right half of the screen"),
    ("Desktop & Window Management", "Win+Up", "Maximize window", "Expand the active window to fill the screen"),
    ("Desktop & Window Management", "Win+Down", "Restore or minimize window", "Step window down from maximized to restored or minimized"),
    ("Desktop & Window Management", "Win+Z", "Open Snap Layouts", "Choose a multi-pane snap grid for the current monitor"),
    ("Desktop & Window Management", "Win+D", "Show or hide desktop", "Toggle all open windows minimized"),
    ("Desktop & Window Management", "Win+M", "Minimize all windows", "Minimize every open desktop window"),
    ("Desktop & Window Management", "Win+Shift+Left", "Move window to left monitor", "Send the active window across displays"),
    ("Desktop & Window Management", "Win+Shift+Right", "Move window to right monitor", "Send the active window across displays"),
    ("Desktop & Window Management", "Win+Home", "Minimize inactive windows", "Hide all desktop windows except the focused one"),
    ("Virtual Desktops", "Win+Tab", "Open Task View", "View all open windows and virtual desktops"),
    ("Virtual Desktops", "Win+Ctrl+D", "Create virtual desktop", "Add a clean new virtual desktop workspace"),
    ("Virtual Desktops", "Win+Ctrl+Left", "Switch virtual desktop left", "Move to the previous virtual desktop"),
    ("Virtual Desktops", "Win+Ctrl+Right", "Switch virtual desktop right", "Move to the next virtual desktop"),
    ("Virtual Desktops", "Win+Ctrl+F4", "Close current virtual desktop", "Close the active virtual desktop and move its windows"),
    ("System & Capture", "Win+V", "Open Clipboard History", "Browse and paste recent text and image clips"),
    ("System & Capture", "Win+Shift+S", "Capture screen snip", "Take a rectangular, freeform, window, or full screenshot"),
    ("System & Capture", "Win+L", "Lock PC", "Immediately lock the Windows session"),
    ("System & Capture", "Win+I", "Open Settings", "Launch Windows Settings"),
    ("System & Capture", "Win+X", "Open Quick Link menu", "Power-user menu for Terminal, Device Manager, and Disk Management"),
    ("System & Capture", "Win+A", "Open Quick Settings", "Toggle Wi-Fi, Bluetooth, brightness, and audio"),
    ("System & Capture", "Win+N", "Open Notification Center", "View notifications and calendar flyout"),
    ("System & Capture", "Win+H", "Start Voice Typing", "Launch Windows speech-to-text dictation"),
    ("System & Capture", "Win+.", "Open Emoji & Symbols", "Insert emoji, Kaomoji, and Unicode symbols"),
    ("System & Capture", "Ctrl+Shift+Esc", "Open Task Manager", "Inspect running processes and performance"),
    ("Taskbar & File Explorer", "Win+E", "Open File Explorer", "Launch a new File Explorer window"),
    ("Taskbar & File Explorer", "Win+R", "Open Run dialog", "Execute a command, path, or control panel applet"),
    ("Taskbar & File Explorer", "Win+S", "Open Windows Search", "Search apps, files, and settings"),
    ("Taskbar & File Explorer", "Win+T", "Cycle taskbar apps", "Move keyboard focus across pinned and running taskbar apps"),
    ("Taskbar & File Explorer", "Win+1", "Launch taskbar app 1-9", "Open or switch to apps pinned in slots 1 through 9"),
)


def _slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_") or "item"


def _deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    merged = dict(base)
    for k, v in override.items():
        if isinstance(merged.get(k), dict) and isinstance(v, dict):
            merged[k] = _deep_merge(merged[k], v)
        else:
            merged[k] = v
    return merged


def _default_project_dir() -> Path:
    return Path(__file__).resolve().parent


def load_config(project_dir: Path) -> dict[str, Any]:
    """Load config.json from *project_dir*, deep-merged with optional config.local.json."""
    cfg: dict[str, Any] = {}
    base_path = project_dir / "config.json"
    if base_path.is_file():
        data = json.loads(base_path.read_text(encoding="utf-8-sig"))
        if isinstance(data, dict):
            cfg = data
    local_path = project_dir / "config.local.json"
    if local_path.is_file():
        local_data = json.loads(local_path.read_text(encoding="utf-8-sig"))
        if isinstance(local_data, dict):
            cfg = _deep_merge(cfg, local_data)
    return cfg


def resolve_powertoys_paths(
    project_dir: Path, config: dict[str, Any]
) -> dict[str, Path]:
    """Resolve PowerToys Shortcut Guide manifest and indexer paths."""
    pt_cfg = config.get("powertoys") or {}
    user_dir = Path(
        os.path.expandvars(
            str(
                pt_cfg.get("user_shortcuts_dir")
                or "%LOCALAPPDATA%\\Microsoft\\WinGet\\KeyboardShortcuts"
            )
        )
    )
    master_shell = Path(
        os.path.expandvars(
            str(
                pt_cfg.get("master_shell_yml")
                or "C:\\Program Files\\PowerToys\\WinUI3Apps\\Assets\\ShortcutGuide\\Manifests\\+WindowsNT.Shell.en-US.yml"
            )
        )
    )
    indexer = Path(
        os.path.expandvars(
            str(
                pt_cfg.get("index_generator_exe")
                or "C:\\Program Files\\PowerToys\\WinUI3Apps\\PowerToys.ShortcutGuide.IndexYmlGenerator.exe"
            )
        )
    )
    custom_name = str(
        pt_cfg.get("custom_manifest_name") or "+Custom.MyShortcuts.en-US.yml"
    )
    return {
        "master_shell_yml": master_shell,
        "user_shortcuts_dir": user_dir,
        "user_shell_yml": user_dir / "+WindowsNT.Shell.en-US.yml",
        "custom_yml": user_dir / custom_name,
        "index_generator_exe": indexer,
        "backup_shell_yml": project_dir
        / "state"
        / "backup"
        / "+WindowsNT.Shell.en-US.orig.yml",
    }


def _state_path(project_dir: Path) -> Path:
    return project_dir / "state" / "user_shortcuts.json"


def load_state(project_dir: Path) -> dict[str, Any]:
    """Load persisted manual shortcuts, hidden IDs, and discovered cache."""
    path = _state_path(project_dir)
    default = {
        "manual_shortcuts": [],
        "hidden_ids": [],
        "discovered_cache": [],
        "last_scan_utc": "",
    }
    if not path.is_file():
        return default
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(data, dict):
            default.update(data)
            return default
    except (json.JSONDecodeError, OSError):
        pass
    return default


def save_state(project_dir: Path, state: dict[str, Any]) -> None:
    """Persist state atomically to state/user_shortcuts.json."""
    atomic_write_json(_state_path(project_dir), state)


def scan_and_save(
    project_dir: Path,
    startup_dir: Path | None = None,
    appdata_dir: Path | None = None,
    localappdata_dir: Path | None = None,
) -> list[dict[str, Any]]:
    """Run auto-discovery and update state/user_shortcuts.json."""
    config = load_config(project_dir)
    discovered = discover_all(
        config=config,
        project_dir=project_dir,
        startup_dir=startup_dir,
        appdata_dir=appdata_dir,
        localappdata_dir=localappdata_dir,
    )
    state = load_state(project_dir)
    state["discovered_cache"] = discovered
    state["last_scan_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    save_state(project_dir, state)
    return discovered


def _builtin_windows_items() -> list[dict[str, Any]]:
    return [
        {
            "id": f"win::{_slug(cat)}::{_slug(name)}::{_slug(keys)}",
            "category": cat,
            "keys": keys,
            "name": name,
            "description": desc,
            "source": "windows",
        }
        for cat, keys, name, desc in _BUILTIN_WINDOWS_SHORTCUTS
    ]


def _load_windows_items(paths: dict[str, Path]) -> list[dict[str, Any]]:
    source_path = paths["master_shell_yml"]
    if not source_path.is_file():
        source_path = paths["backup_shell_yml"]
    if not source_path.is_file():
        source_path = paths["user_shell_yml"]
    if not source_path.is_file():
        return _builtin_windows_items()

    parsed = parse_manifest(source_path, id_prefix="win", source="windows")
    items = [
        {
            "id": item["id"],
            "category": item["category"],
            "keys": item["keys"],
            "name": item["name"],
            "description": item["description"],
            "source": "windows",
        }
        for item in parsed["items"]
        if item.get("keys")
    ]
    return items or _builtin_windows_items()


def get_catalog(
    project_dir: Path,
    rescan: bool = False,
    startup_dir: Path | None = None,
    appdata_dir: Path | None = None,
    localappdata_dir: Path | None = None,
) -> dict[str, Any]:
    """Merge manual, auto-discovered, and Windows shortcuts with hide/restore flags."""
    config = load_config(project_dir)
    paths = resolve_powertoys_paths(project_dir, config)
    state = load_state(project_dir)

    if rescan or not state.get("discovered_cache"):
        discovered = scan_and_save(
            project_dir=project_dir,
            startup_dir=startup_dir,
            appdata_dir=appdata_dir,
            localappdata_dir=localappdata_dir,
        )
        state = load_state(project_dir)
    else:
        discovered = list(state.get("discovered_cache") or [])

    manual = [dict(item, source="manual") for item in (state.get("manual_shortcuts") or [])]
    windows = _load_windows_items(paths)
    hidden_set = set(str(x) for x in (state.get("hidden_ids") or []))

    all_items: list[dict[str, Any]] = []
    for item in manual + discovered + windows:
        entry = dict(item)
        entry["hidden"] = entry["id"] in hidden_set
        all_items.append(entry)

    active = [x for x in all_items if not x["hidden"]]
    hidden = [x for x in all_items if x["hidden"]]

    custom_cats: list[str] = []
    win_cats: list[str] = []
    for item in all_items:
        cat = str(item.get("category") or "General")
        if item.get("source") == "windows":
            if cat not in win_cats:
                win_cats.append(cat)
        elif cat != "PowerToys":
            if cat not in custom_cats:
                custom_cats.append(cat)

    categories: list[str] = list(custom_cats)
    if any(x.get("source") == "powertoys" for x in all_items):
        categories.append("PowerToys")
    for wc in win_cats:
        if wc not in categories:
            categories.append(wc)

    stats = {
        "active_total": len(active),
        "hidden_total": len(hidden),
        "manual": sum(1 for x in active if x.get("source") == "manual"),
        "auto": sum(1 for x in active if x.get("source") not in ("manual", "windows", "powertoys")),
        "powertoys": sum(1 for x in active if x.get("source") == "powertoys"),
        "windows": sum(1 for x in active if x.get("source") == "windows"),
        "windows_hidden": sum(1 for x in hidden if x.get("source") == "windows"),
    }

    return {
        "active": active,
        "hidden": hidden,
        "all": all_items,
        "categories": categories,
        "custom_categories": custom_cats,
        "stats": stats,
        "last_scan_utc": state.get("last_scan_utc", ""),
    }


def add_manual_shortcut(
    project_dir: Path,
    category: str,
    keys: str,
    name: str,
    description: str = "",
) -> dict[str, Any]:
    """Add a user-defined manual shortcut and persist it."""
    cat = category.strip() or "Custom"
    key_str = keys.strip()
    title = name.strip()
    if not key_str or not title:
        raise ValueError("Both shortcut keys and name are required.")

    state = load_state(project_dir)
    item = {
        "id": f"manual::{uuid.uuid4().hex[:12]}",
        "category": cat,
        "keys": key_str,
        "name": title,
        "description": description.strip(),
        "source": "manual",
    }
    state.setdefault("manual_shortcuts", []).append(item)
    save_state(project_dir, state)
    return item


def update_manual_shortcut(
    project_dir: Path,
    item_id: str,
    category: str,
    keys: str,
    name: str,
    description: str = "",
) -> dict[str, Any]:
    """Edit an existing manual shortcut by ID."""
    state = load_state(project_dir)
    for item in state.get("manual_shortcuts") or []:
        if item.get("id") == item_id:
            item["category"] = category.strip() or "Custom"
            item["keys"] = keys.strip()
            item["name"] = name.strip()
            item["description"] = description.strip()
            save_state(project_dir, state)
            return dict(item)
    raise KeyError(f"Manual shortcut '{item_id}' not found.")


def delete_manual_shortcut(project_dir: Path, item_id: str) -> bool:
    """Delete a manual shortcut permanently."""
    state = load_state(project_dir)
    before = len(state.get("manual_shortcuts") or [])
    state["manual_shortcuts"] = [
        item for item in (state.get("manual_shortcuts") or []) if item.get("id") != item_id
    ]
    state["hidden_ids"] = [hid for hid in (state.get("hidden_ids") or []) if hid != item_id]
    save_state(project_dir, state)
    return len(state["manual_shortcuts"]) < before


def hide_shortcut(project_dir: Path, item_id: str) -> bool:
    """Hide any shortcut (Windows, auto-discovered, or manual) from active overviews."""
    state = load_state(project_dir)
    hidden = state.setdefault("hidden_ids", [])
    if item_id in hidden:
        return False
    hidden.append(item_id)
    save_state(project_dir, state)
    return True


def restore_shortcut(project_dir: Path, item_id: str) -> bool:
    """Restore a previously hidden shortcut."""
    state = load_state(project_dir)
    hidden = state.get("hidden_ids") or []
    if item_id not in hidden:
        return False
    state["hidden_ids"] = [hid for hid in hidden if hid != item_id]
    save_state(project_dir, state)
    return True


def restore_all_windows(project_dir: Path) -> int:
    """Restore every hidden native Windows shortcut."""
    state = load_state(project_dir)
    hidden = state.get("hidden_ids") or []
    kept = [hid for hid in hidden if not str(hid).startswith("win::")]
    restored_count = len(hidden) - len(kept)
    state["hidden_ids"] = kept
    save_state(project_dir, state)
    return restored_count


def sync_to_powertoys(
    project_dir: Path,
    rescan: bool = False,
    run_indexer: bool = True,
    startup_dir: Path | None = None,
    appdata_dir: Path | None = None,
    localappdata_dir: Path | None = None,
) -> dict[str, Any]:
    """Write filtered Windows YAML + custom YAML and rebuild PowerToys index.yml."""
    config = load_config(project_dir)
    pt_cfg = config.get("powertoys") or {}
    paths = resolve_powertoys_paths(project_dir, config)

    catalog = get_catalog(
        project_dir=project_dir,
        rescan=rescan,
        startup_dir=startup_dir,
        appdata_dir=appdata_dir,
        localappdata_dir=localappdata_dir,
    )

    if not pt_cfg.get("sync_enabled", True):
        return {"ok": True, "skipped": True, "stats": catalog["stats"]}

    # Back up original user shell YAML once before first overwrite
    if paths["user_shell_yml"].is_file() and not paths["backup_shell_yml"].is_file():
        paths["backup_shell_yml"].parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(paths["user_shell_yml"], paths["backup_shell_yml"])

    master_src = paths["master_shell_yml"]
    if not master_src.is_file():
        master_src = paths["backup_shell_yml"]
    if not master_src.is_file():
        master_src = paths["user_shell_yml"]

    powertoys_installed = master_src.is_file() or paths["index_generator_exe"].is_file()
    if not powertoys_installed:
        return {
            "ok": True,
            "skipped": True,
            "powertoys_installed": False,
            "active_windows": 0,
            "hidden_windows": 0,
            "custom_count": 0,
            "stats": catalog["stats"],
        }

    hidden_ids = {item["id"] for item in catalog["hidden"]}
    active_win, hidden_win = (0, 0)
    if master_src.is_file():
        active_win, hidden_win = write_filtered_shell_manifest(
            master_src, paths["user_shell_yml"], hidden_ids
        )

    grouped_custom: dict[str, list[dict[str, Any]]] = {}
    for item in catalog["active"]:
        if item.get("source") in ("windows", "powertoys"):
            continue
        cat = str(item.get("category") or "Custom")
        grouped_custom.setdefault(cat, []).append(item)

    custom_count = write_custom_manifest(paths["custom_yml"], grouped_custom)

    indexer_ok = True
    index_output = "skipped"
    if run_indexer:
        indexer_ok, index_output = run_powertoys_indexer(paths["index_generator_exe"])

    return {
        "ok": indexer_ok,
        "skipped": False,
        "active_windows": active_win,
        "hidden_windows": hidden_win,
        "custom_count": custom_count,
        "custom_yml": str(paths["custom_yml"]),
        "user_shell_yml": str(paths["user_shell_yml"]),
        "indexer_ok": indexer_ok,
        "index_output": index_output,
        "stats": catalog["stats"],
    }


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Shortcut Hub CLI & PowerToys Shortcut Guide sync tool."
    )
    parser.add_argument(
        "--project-dir",
        default=str(_default_project_dir()),
        help="Path to shortcut-hub directory.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("scan", help="Scan system for custom/AHK/app shortcuts.")

    p_sync = sub.add_parser("sync", help="Sync shortcuts and hidden items to PowerToys.")
    p_sync.add_argument("--no-rescan", action="store_true", help="Skip re-scanning disk.")
    p_sync.add_argument("--no-indexer", action="store_true", help="Skip IndexYmlGenerator.exe.")

    p_list = sub.add_parser("list", help="List active or hidden shortcuts.")
    p_list.add_argument("--category", default="", help="Filter by category substring.")
    p_list.add_argument("--hidden", action="store_true", help="Show hidden shortcuts.")

    p_add = sub.add_parser("add", help="Add a manual shortcut and sync.")
    p_add.add_argument("--category", required=True)
    p_add.add_argument("--keys", required=True)
    p_add.add_argument("--name", required=True)
    p_add.add_argument("--desc", default="")

    p_hide = sub.add_parser("hide", help="Hide a shortcut ID from overlays.")
    p_hide.add_argument("id")

    p_res = sub.add_parser("restore", help="Restore a hidden shortcut ID.")
    p_res.add_argument("id")

    sub.add_parser("restore-all-windows", help="Restore all hidden native Windows shortcuts.")
    return parser


def main(argv: list[str] | None = None) -> int:
    """CLI entrypoint."""
    parser = _build_parser()
    args = parser.parse_args(argv)
    project_dir = Path(args.project_dir).resolve()

    if args.command == "scan":
        items = scan_and_save(project_dir)
        print(json.dumps({"ok": True, "discovered": len(items), "items": items}, indent=2, ensure_ascii=False))
        return 0

    if args.command == "sync":
        res = sync_to_powertoys(
            project_dir=project_dir,
            rescan=not args.no_rescan,
            run_indexer=not args.no_indexer,
        )
        print(json.dumps(res, indent=2, ensure_ascii=False))
        return 0 if res.get("ok") else 1

    if args.command == "list":
        cat = get_catalog(project_dir)
        pool = cat["hidden"] if args.hidden else cat["active"]
        if args.category:
            needle = args.category.lower()
            pool = [x for x in pool if needle in str(x.get("category", "")).lower()]
        print(json.dumps({"count": len(pool), "items": pool}, indent=2, ensure_ascii=False))
        return 0

    if args.command == "add":
        item = add_manual_shortcut(project_dir, args.category, args.keys, args.name, args.desc)
        sync_res = sync_to_powertoys(project_dir, rescan=False)
        print(json.dumps({"ok": True, "added": item, "sync": sync_res}, indent=2, ensure_ascii=False))
        return 0

    if args.command == "hide":
        changed = hide_shortcut(project_dir, args.id)
        sync_res = sync_to_powertoys(project_dir, rescan=False)
        print(json.dumps({"ok": True, "changed": changed, "sync": sync_res}, indent=2, ensure_ascii=False))
        return 0

    if args.command == "restore":
        changed = restore_shortcut(project_dir, args.id)
        sync_res = sync_to_powertoys(project_dir, rescan=False)
        print(json.dumps({"ok": True, "changed": changed, "sync": sync_res}, indent=2, ensure_ascii=False))
        return 0

    if args.command == "restore-all-windows":
        count = restore_all_windows(project_dir)
        sync_res = sync_to_powertoys(project_dir, rescan=False)
        print(json.dumps({"ok": True, "restored": count, "sync": sync_res}, indent=2, ensure_ascii=False))
        return 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
