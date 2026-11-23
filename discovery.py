"""Automatic shortcut discovery across AHK scripts, Startup, PowerToys, CopyQ, and IDEs."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

try:
    import winreg
except ImportError:  # pragma: no cover - non-Windows
    winreg = None  # type: ignore[assignment]

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
    cleaned = raw.strip().replace("<", "").replace(">", "")
    cleaned = re.sub(r"CommandOrControl|CmdOrCtrl", "Ctrl", cleaned, flags=re.IGNORECASE)
    tokens = [t.strip() for t in re.split(r"[+\s]+", cleaned) if t.strip()]
    out: list[str] = []
    for t in tokens:
        low = t.lower()
        if low in ("ctrl", "control", "ctrl_l", "ctrl_r"):
            out.append("Ctrl")
        elif low in ("alt", "alt_l", "alt_r"):
            out.append("Alt")
        elif low in ("shift", "shift_l", "shift_r"):
            out.append("Shift")
        elif low in ("win", "meta", "super", "cmd", "command"):
            out.append("Win")
        elif low == "oem_102":
            out.append("<")
        elif len(t) == 1:
            out.append(t.upper())
        elif re.match(r"^f\d{1,2}$", low):
            out.append(t.upper())
        else:
            out.append(t.capitalize())
    ordered_mods = [m for m in ("Win", "Ctrl", "Alt", "Shift") if m in out]
    rest = [k for k in out if k not in ("Win", "Ctrl", "Alt", "Shift")]
    return "+".join(ordered_mods + rest)


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


_SYSTEM_DIR_MARKERS = (
    "\\windows\\",
    "\\program files\\",
    "\\program files (x86)\\",
    "\\windowsapps\\",
    "\\appdata\\local\\programs\\",
    "\\appdata\\local\\microsoft\\",
    "\\appdata\\local\\google\\",
    "\\appdata\\local\\jetbrains\\",
    "\\appdata\\roaming\\uv\\",
    "\\appdata\\roaming\\npm\\",
    "\\chocolatey\\",
)

_SCRIPT_HOST_NAMES = {
    "wscript.exe",
    "cscript.exe",
    "powershell.exe",
    "pwsh.exe",
    "cmd.exe",
    "python.exe",
    "pythonw.exe",
    "autohotkey.exe",
    "autohotkey64.exe",
    "autohotkeyu64.exe",
    "autohotkey32.exe",
}

_WIN32_VK_NAMES: dict[str, str] = {
    "VK_SPACE": "Space",
    "VK_RETURN": "Enter",
    "VK_ENTER": "Enter",
    "VK_ESCAPE": "Esc",
    "VK_ESC": "Esc",
    "VK_TAB": "Tab",
    "VK_BACK": "Backspace",
    "VK_DELETE": "Delete",
    "VK_INSERT": "Insert",
    "VK_HOME": "Home",
    "VK_END": "End",
    "VK_PRIOR": "Page Up",
    "VK_NEXT": "Page Down",
    "VK_LEFT": "Left",
    "VK_RIGHT": "Right",
    "VK_UP": "Up",
    "VK_DOWN": "Down",
    "VK_SNAPSHOT": "PrtScr",
    "VK_PAUSE": "Pause",
    "VK_CAPITAL": "CapsLock",
    "VK_OEM_PLUS": "+",
    "VK_OEM_MINUS": "-",
    "VK_OEM_COMMA": ",",
    "VK_OEM_PERIOD": ".",
    "VK_OEM_2": "/",
    "VK_OEM_3": "`",
}
for _c in range(ord("A"), ord("Z") + 1):
    _WIN32_VK_NAMES[f"VK_{chr(_c)}"] = chr(_c)
for _d in range(ord("0"), ord("9") + 1):
    _WIN32_VK_NAMES[f"VK_{chr(_d)}"] = chr(_d)
for _f in range(1, 25):
    _WIN32_VK_NAMES[f"VK_F{_f}"] = f"F{_f}"

_CMD_PATH_RE = re.compile(
    r'"([^"]+\.(?:exe|ahk|py|ps1|vbs|bat|cmd))"'
    r'|([A-Za-z]:\\[^\s"]+\.(?:exe|ahk|py|ps1|vbs|bat|cmd))',
    re.IGNORECASE,
)
_REGISTER_HOTKEY_RE = re.compile(r"RegisterHotKey\s*\(([^;]{1,500}?)\)", re.DOTALL)
_CONST_INT_RE = re.compile(
    r"\b([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(0x[0-9A-Fa-f]+|\d+)\b"
)
_PY_HOTKEY_RE = re.compile(
    r"(?:keyboard\.(?:add_hotkey|register_hotkey)|globalShortcut\.register)\s*\(\s*['\"]([^'\"]+)['\"]",
    re.IGNORECASE,
)
_PYNPUT_ENTRY_RE = re.compile(r"['\"](<[A-Za-z0-9_+<>]+>|[^'\"]*\+[^'\"]+)['\"]\s*:")
_GENERIC_ANNOTATION_RE = re.compile(
    r"^\s*(?:;|//|#)\s*@shortcut\s+(?:\[([^\]]+)\]\s+)?([^|=]+?)\s*=\s*([^|]+?)(?:\s*\|\s*(.+))?$",
    re.IGNORECASE,
)
_README_HOTKEY_RE = re.compile(
    r"(?:Keyboard\s*Shortcut|Global\s*Hotkey|Shortcut)\s*[:\-]\s*(?:Press\s*)?[`*]*((?:(?:Ctrl|Control|Alt|Shift|Win)\s*\+\s*)+[A-Za-z0-9_]+)[`*]*",
    re.IGNORECASE,
)
_SOURCE_EXTS = {".cs", ".py", ".rs", ".js", ".ts", ".ps1", ".cpp", ".c", ".h"}


def _is_custom_app_path(path: Path) -> bool:
    low = str(path).lower()
    if path.name.lower() in _SCRIPT_HOST_NAMES:
        return False
    if "shortcut-hub" in low:
        return False
    return not any(marker in low for marker in _SYSTEM_DIR_MARKERS)


def _extract_paths_from_command(cmd_line: str) -> list[Path]:
    found: list[Path] = []
    for m in _CMD_PATH_RE.finditer(cmd_line):
        raw = (m.group(1) or m.group(2) or "").strip()
        if not raw:
            continue
        p = Path(os.path.expandvars(raw))
        if p.is_file():
            found.append(p.resolve())
    return found


def scan_registry_run() -> list[tuple[str, str]]:
    """Read HKCU Run and RunOnce startup command entries on Windows."""
    if winreg is None:
        return []
    entries: list[tuple[str, str]] = []
    subkeys = (
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        r"Software\Microsoft\Windows\CurrentVersion\RunOnce",
    )
    for subkey in subkeys:
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, subkey) as key:
                idx = 0
                while True:
                    try:
                        name, val, _ = winreg.EnumValue(key, idx)
                        idx += 1
                        if isinstance(val, str) and val.strip():
                            entries.append((str(name), val))
                    except OSError:
                        break
        except OSError:
            continue
    return entries


def get_running_user_exes() -> list[Path]:
    """Return unique custom user executable paths currently running on Windows."""
    if sys.platform != "win32":
        return []
    ps_cmd = "Get-Process | Where-Object { $_.Path } | Select-Object -ExpandProperty Path -Unique"
    try:
        proc = subprocess.run(
            ["powershell", "-NoProfile", "-Command", ps_cmd],
            capture_output=True,
            text=True,
            creationflags=subprocess.CREATE_NO_WINDOW,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    if proc.returncode != 0:
        return []
    out: list[Path] = []
    for line in proc.stdout.splitlines():
        raw = line.strip()
        if not raw:
            continue
        p = Path(raw)
        if p.is_file() and _is_custom_app_path(p):
            out.append(p.resolve())
    return out


def _resolve_lnk_records(dirs: list[Path]) -> list[dict[str, str]]:
    """Inspect .lnk files in the given directories for TargetPath, Arguments, and Hotkey."""
    if sys.platform != "win32":
        return []
    existing = [str(d).replace("'", "''") for d in dirs if d.is_dir()]
    if not existing:
        return []
    dirs_literal = ",".join(f"'{d}'" for d in existing)
    ps_cmd = (
        "$w = New-Object -ComObject WScript.Shell; "
        f"foreach ($d in @({dirs_literal})) {{ "
        "Get-ChildItem -LiteralPath $d -Filter '*.lnk' -Recurse -ErrorAction SilentlyContinue | ForEach-Object { "
        "try { $s = $w.CreateShortcut($_.FullName); "
        "Write-Output ($_.BaseName + '|' + $s.TargetPath + '|' + $s.Arguments + '|' + $s.Hotkey) } catch {} } }"
    )
    try:
        proc = subprocess.run(
            ["powershell", "-NoProfile", "-Command", ps_cmd],
            capture_output=True,
            text=True,
            creationflags=subprocess.CREATE_NO_WINDOW,
            timeout=15,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    if proc.returncode != 0:
        return []
    records: list[dict[str, str]] = []
    for row in proc.stdout.splitlines():
        parts = row.split("|", 3)
        if len(parts) != 4:
            continue
        records.append(
            {
                "name": parts[0].strip(),
                "target": parts[1].strip(),
                "args": parts[2].strip(),
                "hotkey": parts[3].strip(),
            }
        )
    return records


def scan_lnk_hotkeys(dirs: list[Path]) -> list[dict[str, Any]]:
    """Extract Windows Shortcut (.lnk) files that have an assigned Hotkey property."""
    items: list[dict[str, Any]] = []
    for rec in _resolve_lnk_records(dirs):
        raw_hk = rec.get("hotkey", "")
        if not raw_hk:
            continue
        combo = _format_generic_shortcut(raw_hk)
        if not combo:
            continue
        name = rec.get("name") or "Windows Shortcut"
        target = rec.get("target") or ""
        items.append(
            _make_item(
                "lnk",
                "Windows Shortcuts (.lnk)",
                combo,
                name,
                f"Shortcut target: {Path(target).name}" if target else "Windows .lnk hotkey",
            )
        )
    return items


def _resolve_vk_token(vk_expr: str, constants: dict[str, int]) -> str:
    expr = re.sub(r"\([^)]+\)", "", vk_expr).strip()
    token = expr.split(".")[-1].strip()
    up = token.upper()
    if up in _WIN32_VK_NAMES:
        return _WIN32_VK_NAMES[up]
    if re.match(r"^'[A-Za-z0-9]'$", token):
        return token[1].upper()
    if re.match(r"^[A-Za-z]$", token):
        return token.upper()
    if re.match(r"^F(?:[1-9]|1\d|2[0-4])$", up):
        return up
    val: int | None = None
    if token in constants:
        val = constants[token]
    elif up.startswith("0X"):
        try:
            val = int(up, 16)
        except ValueError:
            val = None
    elif token.isdigit():
        val = int(token)
    if val is not None:
        if 0x41 <= val <= 0x5A or 0x30 <= val <= 0x39:
            return chr(val)
        if 0x70 <= val <= 0x87:
            return f"F{val - 0x6F}"
        if val == 0x20:
            return "Space"
        if val == 0x0D:
            return "Enter"
        if val == 0x1B:
            return "Esc"
    return ""


def _resolve_mod_tokens(mods_expr: str, constants: dict[str, int]) -> list[str]:
    mods: list[str] = []
    for part in re.split(r"[|+,]", mods_expr):
        tok = part.strip()
        if not tok:
            continue
        low = tok.split(".")[-1].strip().lower()
        if any(k in low for k in ("mod_win", "win")):
            if "Win" not in mods:
                mods.append("Win")
        elif any(k in low for k in ("mod_control", "mod_ctrl", "control", "ctrl")):
            if "Ctrl" not in mods:
                mods.append("Ctrl")
        elif any(k in low for k in ("mod_alt", "alt")):
            if "Alt" not in mods:
                mods.append("Alt")
        elif any(k in low for k in ("mod_shift", "shift")):
            if "Shift" not in mods:
                mods.append("Shift")
        else:
            val: int | None = constants.get(tok)
            if val is None and low.startswith("0x"):
                try:
                    val = int(low, 16)
                except ValueError:
                    val = None
            elif val is None and tok.isdigit():
                val = int(tok)
            if val is not None:
                if val & 0x0008 and "Win" not in mods:
                    mods.append("Win")
                if val & 0x0002 and "Ctrl" not in mods:
                    mods.append("Ctrl")
                if val & 0x0001 and "Alt" not in mods:
                    mods.append("Alt")
                if val & 0x0004 and "Shift" not in mods:
                    mods.append("Shift")
    return [m for m in ("Win", "Ctrl", "Alt", "Shift") if m in mods]


def _parse_win32_register_hotkey(
    text: str, path: Path, default_cat: str, label_hint: str = ""
) -> list[dict[str, Any]]:
    constants: dict[str, int] = {}
    for m in _CONST_INT_RE.finditer(text):
        raw_val = m.group(2)
        try:
            constants[m.group(1)] = int(raw_val, 16) if raw_val.lower().startswith("0x") else int(raw_val)
        except ValueError:
            continue

    items: list[dict[str, Any]] = []
    app_label = label_hint or default_cat
    for m in _REGISTER_HOTKEY_RE.finditer(text):
        args = [a.strip() for a in m.group(1).split(",")]
        if len(args) != 4:
            continue
        mods = _resolve_mod_tokens(args[2], constants)
        vk = _resolve_vk_token(args[3], constants)
        if not vk or not mods:
            continue
        combo = "+".join(mods + [vk])
        items.append(
            _make_item(
                "native-hotkey",
                default_cat,
                combo,
                f"{app_label} ({combo})" if app_label else combo,
                f"RegisterHotKey in {path.name}",
            )
        )
    return items


def _parse_python_or_js_hotkeys(
    text: str, path: Path, default_cat: str, label_hint: str = ""
) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    app_label = label_hint or default_cat
    for m in _PY_HOTKEY_RE.finditer(text):
        combo = _format_generic_shortcut(m.group(1))
        if "+" in combo:
            items.append(
                _make_item(
                    "native-hotkey",
                    default_cat,
                    combo,
                    f"{app_label} ({combo})",
                    f"Registered in {path.name}",
                )
            )
    if "GlobalHotKeys" in text:
        for block in re.finditer(r"GlobalHotKeys\s*\(\s*\{([^}]+)\}", text, re.DOTALL):
            for entry in _PYNPUT_ENTRY_RE.finditer(block.group(1)):
                combo = _format_generic_shortcut(entry.group(1))
                if "+" in combo:
                    items.append(
                        _make_item(
                            "native-hotkey",
                            default_cat,
                            combo,
                            f"{app_label} ({combo})",
                            f"GlobalHotKeys in {path.name}",
                        )
                    )
    return items


def _parse_generic_annotations(text: str, default_cat: str) -> list[dict[str, Any]]:
    items: list[dict[str, Any]] = []
    for line in text.splitlines():
        m = _GENERIC_ANNOTATION_RE.match(line)
        if not m:
            continue
        ann_cat = (m.group(1) or default_cat).strip()
        raw_k = m.group(2).strip()
        name = m.group(3).strip()
        desc = (m.group(4) or "").strip()
        combo = format_ahk_hotkey(raw_k) or _format_generic_shortcut(raw_k)
        if combo:
            items.append(_make_item("native-hotkey", ann_cat, combo, name, desc))
    return items


def _parse_generic_json_hotkeys(cfg_path: Path, default_cat: str) -> list[dict[str, Any]]:
    try:
        cfg = json.loads(cfg_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return []
    if not isinstance(cfg, dict):
        return []
    items: list[dict[str, Any]] = []
    for key in ("hotkey", "shortcut", "global_hotkey", "toggle_hotkey"):
        val = cfg.get(key)
        if isinstance(val, str) and val.strip():
            combo = format_ahk_hotkey(val) or _format_generic_shortcut(val)
            if "+" in combo:
                items.append(
                    _make_item(
                        "app-config",
                        default_cat,
                        combo,
                        f"{default_cat} ({key.replace('_', ' ').title()})",
                        f"Config: {cfg_path.name}",
                    )
                )
    return items


def scan_custom_app_dir(
    app_dir: Path,
    category_hints: dict[str, str] | None = None,
    ignore_dir_names: list[str] | None = None,
    label_hint: str = "",
) -> list[dict[str, Any]]:
    """Scan a custom utility directory for native RegisterHotKey, Python/JS hotkeys, and configs."""
    if not app_dir.is_dir():
        return []
    ignore_set = {name.lower() for name in (ignore_dir_names or [])}
    default_cat = label_hint or _infer_category(app_dir / app_dir.name, category_hints)
    items: list[dict[str, Any]] = []
    readme_candidates: list[Path] = []
    base_depth = len(app_dir.parts)

    for cur_root, subdirs, files in os.walk(app_dir):
        cur_path = Path(cur_root)
        if len(cur_path.parts) - base_depth >= 3:
            subdirs[:] = []
        else:
            subdirs[:] = [d for d in subdirs if d.lower() not in ignore_set]
        for fname in sorted(files):
            low = fname.lower()
            fpath = cur_path / fname
            if "test" in low:
                continue
            if low == "config.json":
                items.extend(_parse_companion_config(fpath, default_cat))
                items.extend(_parse_generic_json_hotkeys(fpath, default_cat))
                continue
            if low == "readme.md":
                readme_candidates.append(fpath)
                continue
            if fpath.suffix.lower() not in _SOURCE_EXTS:
                continue
            try:
                if fpath.stat().st_size > 512_000:
                    continue
                text = fpath.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            items.extend(_parse_generic_annotations(text, default_cat))
            items.extend(_parse_win32_register_hotkey(text, fpath, default_cat, label_hint))
            if fpath.suffix.lower() in (".py", ".js", ".ts"):
                items.extend(_parse_python_or_js_hotkeys(text, fpath, default_cat, label_hint))

    if not items:
        for rpath in readme_candidates:
            try:
                rtext = rpath.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            for m in _README_HOTKEY_RE.finditer(rtext):
                combo = _format_generic_shortcut(m.group(1))
                if combo and combo.lower() != "ctrl+alt+del":
                    items.append(
                        _make_item(
                            "native-hotkey",
                            default_cat,
                            combo,
                            f"{label_hint or default_cat} ({combo})",
                            f"Documented in {rpath.name}",
                        )
                    )
    return items


def discover_all(
    config: dict[str, Any],
    project_dir: Path,
    startup_dir: Path | None = None,
    appdata_dir: Path | None = None,
    localappdata_dir: Path | None = None,
    registry_entries: list[tuple[str, str]] | None = None,
    running_exes: list[Path] | None = None,
    lnk_scan_dirs: list[Path] | None = None,
) -> list[dict[str, Any]]:
    """Run all enabled shortcut auto-discovery scanners and deduplicate cleanly."""
    disc_cfg = config.get("discovery") or {}
    pt_cfg = config.get("powertoys") or {}
    is_live_env = startup_dir is None

    resolved_appdata = appdata_dir or Path(os.environ.get("APPDATA", ""))
    resolved_localappdata = localappdata_dir or Path(os.environ.get("LOCALAPPDATA", ""))
    resolved_startup = startup_dir or (
        resolved_appdata / "Microsoft" / "Windows" / "Start Menu" / "Programs" / "Startup"
    )
    ignore_dirs = list(disc_cfg.get("ignore_dir_names") or [])
    category_hints = disc_cfg.get("category_hints")
    if not isinstance(category_hints, dict):
        category_hints = None

    ahk_paths: list[Path] = []
    custom_app_dirs: dict[Path, str] = {}

    if disc_cfg.get("scan_startup_dir", True):
        ahk_paths.extend(resolve_startup_ahk_targets(resolved_startup))
        for rec in _resolve_lnk_records([resolved_startup]):
            for p in _extract_paths_from_command(f'"{rec["target"]}" {rec["args"]}'):
                if p.suffix.lower() == ".ahk":
                    ahk_paths.append(p)
                elif _is_custom_app_path(p):
                    custom_app_dirs.setdefault(p.parent.resolve(), rec["name"])

    if disc_cfg.get("scan_registry_run", True):
        reg_list = registry_entries if registry_entries is not None else (scan_registry_run() if is_live_env else [])
        for reg_name, cmd_line in reg_list:
            for p in _extract_paths_from_command(cmd_line):
                if p.suffix.lower() == ".ahk":
                    ahk_paths.append(p)
                elif _is_custom_app_path(p):
                    custom_app_dirs.setdefault(p.parent.resolve(), reg_name)

    if disc_cfg.get("scan_running_processes", True):
        exe_list = running_exes if running_exes is not None else (get_running_user_exes() if is_live_env else [])
        for exe_path in exe_list:
            if _is_custom_app_path(exe_path):
                custom_app_dirs.setdefault(exe_path.parent.resolve(), exe_path.stem)

    watch_dirs = list(disc_cfg.get("extra_watch_dirs") or [])
    if str(project_dir) not in watch_dirs:
        watch_dirs.append(str(project_dir))
    ahk_paths.extend(scan_watch_dirs(watch_dirs, ignore_dirs))

    for raw_dir in watch_dirs:
        wroot = Path(os.path.expandvars(raw_dir))
        if wroot.is_dir():
            for child in sorted(wroot.iterdir()):
                if child.is_dir() and child.name.lower() not in {d.lower() for d in ignore_dirs}:
                    if "shortcut-hub" not in child.name.lower():
                        custom_app_dirs.setdefault(child.resolve(), "")

    seen_files: set[Path] = set()
    raw_items: list[dict[str, Any]] = []
    for p in ahk_paths:
        rp = p.resolve()
        if rp in seen_files:
            continue
        seen_files.add(rp)
        raw_items.extend(parse_ahk_file(rp, category_hints=category_hints))

    for app_dir, label_hint in custom_app_dirs.items():
        if "shortcut-hub" in str(app_dir).lower():
            continue
        raw_items.extend(
            scan_custom_app_dir(
                app_dir,
                category_hints=category_hints,
                ignore_dir_names=ignore_dirs,
                label_hint=label_hint,
            )
        )

    if disc_cfg.get("scan_lnk_hotkeys", True):
        if lnk_scan_dirs is not None:
            raw_items.extend(scan_lnk_hotkeys(lnk_scan_dirs))
        elif is_live_env and sys.platform == "win32":
            user_prof = Path(os.environ.get("USERPROFILE", ""))
            raw_items.extend(
                scan_lnk_hotkeys(
                    [
                        user_prof / "Desktop",
                        resolved_startup,
                        resolved_appdata / "Microsoft" / "Windows" / "Start Menu" / "Programs",
                    ]
                )
            )

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
