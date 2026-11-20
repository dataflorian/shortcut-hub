"""Automatic shortcut discovery across AHK scripts, Startup, PowerToys, CopyQ, and IDEs."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

from powertoys_yaml import parse_manifest

_MOD_MAP: dict[str, str] = {
    "^": "Ctrl",
    "!": "Alt",
    "+": "Shift",
    "#": "Win",
}

_AHK_KEY_NAMES: dict[str, str] = {
    "space": "Space",
    "enter": "Enter",
    "return": "Enter",
    "tab": "Tab",
    "esc": "Esc",
    "escape": "Esc",
    "bs": "Backspace",
    "backspace": "Backspace",
    "del": "Delete",
    "delete": "Delete",
    "ins": "Insert",
    "insert": "Insert",
    "home": "Home",
    "end": "End",
    "pgup": "Page Up",
    "pgdn": "Page Down",
    "up": "Up",
    "down": "Down",
    "left": "Left",
    "right": "Right",
    "mbutton": "Middle Click",
    "lbutton": "Left Click",
    "rbutton": "Right Click",
    "xbutton1": "Mouse Back",
    "xbutton2": "Mouse Forward",
    "printscreen": "PrtScr",
}

_BARE_ALLOWED_RE = re.compile(
    r"^(?:F(?:[1-9]|1\d|2[0-4])|MButton|XButton1|XButton2|Pause|PrintScreen|ScrollLock)$",
    re.IGNORECASE,
)

_ANNOTATION_RE = re.compile(
    r"^\s*;\s*@shortcut\s+(?:\[([^\]]+)\]\s+)?([^|]+?)(?:\s*\|\s*(.+))?$",
    re.IGNORECASE,
)

_STATIC_HOTKEY_RE = re.compile(r"^\s*([#!^+$~*<>]*[A-Za-z0-9_]+)\s*::(.*)$")

_CATEGORY_HINTS: dict[str, str] = {
    "shortcut-hub": "Shortcut Hub",
}


def format_ahk_hotkey(raw_hk: str) -> str:
    """Convert an AHK hotkey expression like '^!j' or '#F9' into 'Ctrl+Alt+J'."""
    s = raw_hk.strip()
    if not s:
        return ""
    if re.search(r"\s+up$", s, re.IGNORECASE):
        return ""

    mods: list[str] = []
    idx = 0
    while idx < len(s):
        ch = s[idx]
        if ch in ("$", "~", "*", "<", ">"):
            idx += 1
            continue
        if ch in _MOD_MAP:
            label = _MOD_MAP[ch]
            if label not in mods:
                mods.append(label)
            idx += 1
            continue
        break

    key_part = s[idx:].strip()
    if not key_part:
        return ""

    if not mods and not _BARE_ALLOWED_RE.match(key_part):
        return ""

    low = key_part.lower()
    if low in _AHK_KEY_NAMES:
        key_label = _AHK_KEY_NAMES[low]
    elif len(key_part) == 1:
        key_label = key_part.upper()
    elif re.match(r"^f\d{1,2}$", low):
        key_label = key_part.upper()
    else:
        key_label = key_part

    ordered_mods = [m for m in ("Win", "Ctrl", "Alt", "Shift") if m in mods]
    return "+".join(ordered_mods + [key_label])


def _infer_category(
    path: Path, extra_hints: dict[str, str] | None = None
) -> str:
    parent_low = path.parent.name.lower()
    stem_low = path.stem.lower()
    merged_hints = dict(_CATEGORY_HINTS)
    if extra_hints:
        for k, v in extra_hints.items():
            if str(k).strip() and str(v).strip():
                merged_hints[str(k).strip().lower()] = str(v).strip()
    for hint, cat in merged_hints.items():
        if hint in parent_low or hint in stem_low:
            return cat
    clean = re.sub(r"[-_]+", " ", path.stem).strip()
    return clean.title() if clean else "Custom AHK"


def _clean_comment_label(comment: str, key_label: str) -> str:
    text = comment.lstrip(";").strip()
    if ":" in text:
        head, tail = text.split(":", 1)
        head_norm = re.sub(r"[^a-z0-9]", "", head.lower())
        key_norm = re.sub(r"[^a-z0-9]", "", key_label.lower())
        if head_norm and (head_norm in key_norm or key_norm in head_norm):
            return tail.strip()
    return text


def _make_item(
    source: str, category: str, keys: str, name: str, description: str = ""
) -> dict[str, Any]:
    slug_cat = re.sub(r"[^a-z0-9]+", "_", category.lower()).strip("_") or "custom"
    slug_keys = re.sub(r"[^a-z0-9]+", "_", keys.lower()).strip("_") or "key"
    slug_name = re.sub(r"[^a-z0-9]+", "_", name.lower()).strip("_") or "shortcut"
    return {
        "id": f"auto::{source}::{slug_cat}::{slug_keys}::{slug_name}",
        "category": category,
        "keys": keys,
        "name": name,
        "description": description,
        "source": source,
    }


def _parse_companion_config(cfg_path: Path, default_cat: str) -> list[dict[str, Any]]:
    try:
        cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return []

    items: list[dict[str, Any]] = []
    hk_cfg = cfg.get("hotkeys")
    if not isinstance(hk_cfg, dict):
        return items

    dest_labels = {
        str(d.get("id", "")): str(d.get("label", ""))
        for d in (cfg.get("destinations") or [])
        if isinstance(d, dict) and d.get("id")
    }

    if "quick_capture" in hk_cfg:
        combo = format_ahk_hotkey(str(hk_cfg["quick_capture"]))
        if combo:
            items.append(
                _make_item(
                    "ahk-config",
                    default_cat,
                    combo,
                    "Quick Capture Overlay",
                    "Type idea -> Enter -> Pick list",
                )
            )
    if hk_cfg.get("quick_capture_alt"):
        combo = format_ahk_hotkey(str(hk_cfg["quick_capture_alt"]))
        if combo:
            items.append(
                _make_item(
                    "ahk-config",
                    default_cat,
                    combo,
                    "Quick Capture Overlay (Backup)",
                    "Alternate trigger for Quick Capture",
                )
            )
    if hk_cfg.get("direct_destinations_enabled", True) and isinstance(
        hk_cfg.get("direct_shortcuts"), dict
    ):
        for dest_id, raw_hk in hk_cfg["direct_shortcuts"].items():
            combo = format_ahk_hotkey(str(raw_hk))
            if combo:
                label = dest_labels.get(dest_id, dest_id)
                items.append(
                    _make_item(
                        "ahk-config",
                        default_cat,
                        combo,
                        f"Direct Capture: {label}",
                        f"Send idea straight to {label}",
                    )
                )

    if "toggle_overlay" in hk_cfg:
        combo = format_ahk_hotkey(str(hk_cfg["toggle_overlay"]))
        if combo:
            items.append(
                _make_item(
                    "ahk-config",
                    default_cat,
                    combo,
                    "Toggle Shortcut Hub Overlay",
                    "Search, add, or manage shortcuts",
                )
            )
    if hk_cfg.get("toggle_overlay_alt"):
        combo = format_ahk_hotkey(str(hk_cfg["toggle_overlay_alt"]))
        if combo:
            items.append(
                _make_item(
                    "ahk-config",
                    default_cat,
                    combo,
                    "Toggle Shortcut Hub Overlay (Backup)",
                    "Alternate trigger for Shortcut Hub",
                )
            )

    return items


def parse_ahk_file(
    path: Path,
    category_override: str = "",
    category_hints: dict[str, str] | None = None,
) -> list[dict[str, Any]]:
    """Extract declared and static hotkeys from an AutoHotkey v1/v2 script."""
    if not path.is_file():
        return []

    default_cat = category_override or _infer_category(path, category_hints)
    items: list[dict[str, Any]] = []

    cfg_path = path.parent / "config.json"
    if cfg_path.is_file():
        items.extend(_parse_companion_config(cfg_path, default_cat))

    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    header_hint = ""
    in_block_comment = False

    for idx, line in enumerate(lines):
        stripped = line.strip()
        if "/*" in stripped:
            in_block_comment = True
        if "*/" in stripped:
            in_block_comment = False
            continue
        if in_block_comment:
            continue

        if idx < 25 and not header_hint:
            m_hdr = re.match(r"^\s*;\s*(?:Hotkey|Description)\s*:\s*(.+)$", line, re.IGNORECASE)
            if m_hdr:
                header_hint = m_hdr.group(1).strip()

        m_ann = _ANNOTATION_RE.match(line)
        if m_ann:
            ann_cat = (m_ann.group(1) or default_cat).strip()
            ann_name = m_ann.group(2).strip()
            ann_desc = (m_ann.group(3) or "").strip()
            # Expect the next non-comment line or hotkey line if format is "@shortcut [Cat] Key = Name | Desc"
            if "=" in ann_name:
                raw_k, real_name = ann_name.split("=", 1)
                combo = format_ahk_hotkey(raw_k.strip()) or raw_k.strip()
                items.append(_make_item("ahk", ann_cat, combo, real_name.strip(), ann_desc))
            continue

        if stripped.startswith(";"):
            continue

        m_hk = _STATIC_HOTKEY_RE.match(line)
        if not m_hk:
            continue

        raw_hk = m_hk.group(1)
        tail = m_hk.group(2).strip()
        combo = format_ahk_hotkey(raw_hk)
        if not combo:
            continue

        # Look immediately above for a descriptive comment
        comment_text = ""
        for back in range(idx - 1, max(-1, idx - 4), -1):
            prev = lines[back].strip()
            if not prev:
                break
            if prev.startswith(";"):
                cand = _clean_comment_label(prev, combo)
                if cand and not cand.startswith("===") and not cand.lower().startswith("hotkey"):
                    comment_text = cand
                    break
            else:
                break

        if not comment_text and ";" in tail:
            comment_text = tail.split(";", 1)[1].strip()
        if not comment_text:
            comment_text = header_hint or f"{path.stem} ({combo})"

        items.append(
            _make_item(
                "ahk",
                default_cat,
                combo,
                comment_text,
                f"Script: {path.name}",
            )
        )

    return items


def resolve_startup_ahk_targets(startup_dir: Path) -> list[Path]:
    """Return .ahk files directly in Startup plus .ahk scripts launched by .lnk files."""
    if not startup_dir.is_dir():
        return []

    found: list[Path] = []
    for ahk in sorted(startup_dir.glob("*.ahk")):
        found.append(ahk.resolve())

    lnk_files = sorted(startup_dir.glob("*.lnk"))
    if lnk_files and sys.platform == "win32":
        ps_cmd = (
            "$w = New-Object -ComObject WScript.Shell; "
            "Get-ChildItem -LiteralPath '"
            + str(startup_dir).replace("'", "''")
            + "' -Filter *.lnk | ForEach-Object { "
            "$s = $w.CreateShortcut($_.FullName); "
            "Write-Output ($s.TargetPath + '|' + $s.Arguments) }"
        )
        try:
            proc = subprocess.run(
                ["powershell", "-NoProfile", "-Command", ps_cmd],
                capture_output=True,
                text=True,
                creationflags=subprocess.CREATE_NO_WINDOW,
                timeout=10,
            )
            if proc.returncode == 0:
                for row in proc.stdout.splitlines():
                    if "|" not in row:
                        continue
                    target, args = row.split("|", 1)
                    if target.lower().endswith(".ahk"):
                        p = Path(target.strip('" '))
                        if p.is_file():
                            found.append(p.resolve())
                    for match in re.findall(r'"([^"]+\.ahk)"|([^\s"]+\.ahk)', args, re.IGNORECASE):
                        cand_str = match[0] or match[1]
                        cand = Path(cand_str)
                        if cand.is_file():
                            found.append(cand.resolve())
        except (OSError, subprocess.SubprocessError):
            pass

    deduped: list[Path] = []
    seen: set[Path] = set()
    for p in found:
        if p not in seen:
            seen.add(p)
            deduped.append(p)
    return deduped


def scan_watch_dirs(
    dirs: list[str], ignore_dir_names: list[str]
) -> list[Path]:
    """Recursively find active .ahk scripts inside configured watch folders."""
    ignore_set = {name.lower() for name in ignore_dir_names}
    found: list[Path] = []
    for raw_dir in dirs:
        root = Path(os.path.expandvars(raw_dir))
        if not root.is_dir():
            continue
        for cur_root, subdirs, files in os.walk(root):
            subdirs[:] = [d for d in subdirs if d.lower() not in ignore_set]
            for fname in sorted(files):
                low = fname.lower()
                if low.endswith(".ahk") and "test" not in low:
                    found.append((Path(cur_root) / fname).resolve())
    return found


def scan_powertoys_manifest(manifest_path: Path) -> list[dict[str, Any]]:
    """Read live enabled PowerToys shortcuts from Microsoft.PowerToys.en-US.yml."""
    parsed = parse_manifest(manifest_path, id_prefix="pt", source="powertoys")
    items: list[dict[str, Any]] = []
    for item in parsed["items"]:
        if not item.get("keys"):
            continue
        items.append(
            {
                "id": item["id"],
                "category": "PowerToys",
                "keys": item["keys"],
                "name": item["name"],
                "description": item["description"] or "PowerToys module",
                "source": "powertoys",
            }
        )
    return items


def _format_generic_shortcut(raw: str) -> str:
    tokens = [t.strip() for t in re.split(r"[+\s]+", raw.strip()) if t.strip()]
    out: list[str] = []
    for t in tokens:
        low = t.lower()
        if low in ("ctrl", "control"):
            out.append("Ctrl")
        elif low == "alt":
            out.append("Alt")
        elif low == "shift":
            out.append("Shift")
        elif low in ("win", "meta", "super"):
            out.append("Win")
        elif low == "oem_102":
            out.append("<")
        elif len(t) == 1:
            out.append(t.upper())
        else:
            out.append(t.capitalize())
    return "+".join(out)


def scan_copyq(copyq_dir: Path) -> list[dict[str, Any]]:
    """Scan CopyQ copyq-commands.ini for custom global/command shortcuts."""
    cmd_ini = copyq_dir / "copyq-commands.ini"
    if not cmd_ini.is_file():
        return []

    records: dict[str, dict[str, str]] = {}
    for line in cmd_ini.read_text(encoding="utf-8", errors="replace").splitlines():
        m = re.match(r"^(\d+)\\([A-Za-z0-9_]+)=(.*)$", line.strip())
        if not m:
            continue
        idx, key, val = m.group(1), m.group(2), m.group(3).strip()
        records.setdefault(idx, {})[key] = val

    items: list[dict[str, Any]] = []
    for idx in sorted(records, key=lambda x: int(x)):
        rec = records[idx]
        raw_sc = rec.get("Shortcut", "")
        if not raw_sc:
            continue
        combo = _format_generic_shortcut(raw_sc)
        if combo.lower() in ("enter", "return", "escape"):
            continue
        name = rec.get("Name") or f"CopyQ Command {idx}"
        is_global = rec.get("IsGlobalShortcut", "").lower() == "true"
        items.append(
            _make_item(
                "copyq",
                "CopyQ",
                combo,
                name,
                "Global CopyQ shortcut" if is_global else "CopyQ window shortcut",
            )
        )
    return items


def _strip_jsonc(raw: str) -> str:
    no_block = re.sub(r"/\*.*?\*/", "", raw, flags=re.DOTALL)
    no_line = re.sub(r"//.*?$", "", no_block, flags=re.MULTILINE)
    no_trailing = re.sub(r",\s*([}\]])", r"\1", no_line)
    return no_trailing


def scan_vscode_keybindings(paths: list[Path]) -> list[dict[str, Any]]:
    """Parse user keybindings.json files (VS Code / Antigravity)."""
    items: list[dict[str, Any]] = []
    for kb_path in paths:
        if not kb_path.is_file():
            continue
        try:
            data = json.loads(_strip_jsonc(kb_path.read_text(encoding="utf-8", errors="replace")))
        except json.JSONDecodeError:
            continue
        if not isinstance(data, list):
            continue
        editor_name = (
            "Antigravity"
            if "antigravity" in str(kb_path).lower()
            else ("Cursor" if "cursor" in str(kb_path).lower() else "VS Code")
        )
        for entry in data:
            if not isinstance(entry, dict):
                continue
            cmd = str(entry.get("command", "")).strip()
            key_raw = str(entry.get("key", "")).strip()
            if not cmd or not key_raw or cmd.startswith("-"):
                continue
            combo = _format_generic_shortcut(key_raw)
            items.append(
                _make_item(
                    "vscode",
                    f"{editor_name} / IDE",
                    combo,
                    cmd,
                    str(entry.get("when", "")),
                )
            )
    return items


def discover_all(
    config: dict[str, Any],
    project_dir: Path,
    startup_dir: Path | None = None,
    appdata_dir: Path | None = None,
    localappdata_dir: Path | None = None,
) -> list[dict[str, Any]]:
    """Run all enabled shortcut auto-discovery scanners and deduplicate cleanly."""
    disc_cfg = config.get("discovery") or {}
    pt_cfg = config.get("powertoys") or {}

    resolved_appdata = appdata_dir or Path(os.environ.get("APPDATA", ""))
    resolved_localappdata = localappdata_dir or Path(os.environ.get("LOCALAPPDATA", ""))
    resolved_startup = startup_dir or (
        resolved_appdata / "Microsoft" / "Windows" / "Start Menu" / "Programs" / "Startup"
    )

    ahk_paths: list[Path] = []
    if disc_cfg.get("scan_startup_dir", True):
        ahk_paths.extend(resolve_startup_ahk_targets(resolved_startup))

    watch_dirs = list(disc_cfg.get("extra_watch_dirs") or [])
    if str(project_dir) not in watch_dirs:
        watch_dirs.append(str(project_dir))
    ahk_paths.extend(
        scan_watch_dirs(
            watch_dirs,
            list(disc_cfg.get("ignore_dir_names") or []),
        )
    )

    category_hints = disc_cfg.get("category_hints")
    if not isinstance(category_hints, dict):
        category_hints = None

    seen_files: set[Path] = set()
    raw_items: list[dict[str, Any]] = []
    for p in ahk_paths:
        rp = p.resolve()
        if rp in seen_files:
            continue
        seen_files.add(rp)
        raw_items.extend(parse_ahk_file(rp, category_hints=category_hints))

    if disc_cfg.get("scan_powertoys_manifest", True):
        user_pt_dir = Path(
            os.path.expandvars(
                str(pt_cfg.get("user_shortcuts_dir") or (resolved_localappdata / "Microsoft" / "WinGet" / "KeyboardShortcuts"))
            )
        )
        pt_manifest = user_pt_dir / "Microsoft.PowerToys.en-US.yml"
        raw_items.extend(scan_powertoys_manifest(pt_manifest))

    if disc_cfg.get("scan_copyq", True) and str(resolved_appdata):
        raw_items.extend(scan_copyq(resolved_appdata / "copyq"))

    if disc_cfg.get("scan_vscode_keybindings", True) and str(resolved_appdata):
        raw_items.extend(
            scan_vscode_keybindings(
                [
                    resolved_appdata / "Antigravity" / "User" / "keybindings.json",
                    resolved_appdata / "Cursor" / "User" / "keybindings.json",
                    resolved_appdata / "Code" / "User" / "keybindings.json",
                ]
            )
        )

    deduped: list[dict[str, Any]] = []
    seen_pairs: set[tuple[str, str]] = set()
    for item in raw_items:
        pair = (
            str(item.get("keys", "")).strip().lower(),
            str(item.get("name", "")).strip().lower(),
        )
        if not pair[0] or pair in seen_pairs:
            continue
        seen_pairs.add(pair)
        deduped.append(item)
    return deduped
