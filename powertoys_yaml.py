"""Lossless pure-stdlib PowerToys Shortcut Guide YAML parser, writer, and indexer."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

# Win32 virtual-key code map for numeric keys used in PowerToys YAML manifests
_VK_TO_KEY: dict[int, str] = {
    8: "Backspace",
    9: "Tab",
    13: "Enter",
    19: "Pause",
    20: "CapsLock",
    27: "Esc",
    32: "Space",
    33: "Page Up",
    34: "Page Down",
    35: "End",
    36: "Home",
    37: "Left",
    38: "Up",
    39: "Right",
    40: "Down",
    44: "PrtScr",
    45: "Insert",
    46: "Delete",
    91: "Win",
    92: "RWin",
    186: ";",
    187: "+",
    188: ",",
    189: "-",
    190: ".",
    191: "/",
    192: "`",
    219: "[",
    220: "\\",
    221: "]",
    222: "'",
    226: "<",
}
for _code in range(48, 58):
    _VK_TO_KEY[_code] = chr(_code)
for _code in range(65, 91):
    _VK_TO_KEY[_code] = chr(_code)
for _idx, _code in enumerate(range(112, 136), start=1):
    _VK_TO_KEY[_code] = f"F{ _idx }"

_SPECIAL_KEY_TO_PT: dict[str, str] = {
    "space": "<Space>",
    "enter": "<Enter>",
    "return": "<Enter>",
    "tab": "<Tab>",
    "esc": "<Esc>",
    "escape": "<Esc>",
    "backspace": "<Backspace>",
    "delete": "<Delete>",
    "del": "<Delete>",
    "insert": "<Insert>",
    "ins": "<Insert>",
    "home": "<Home>",
    "end": "<End>",
    "pageup": "<Page Up>",
    "page up": "<Page Up>",
    "pgup": "<Page Up>",
    "pagedown": "<Page Down>",
    "page down": "<Page Down>",
    "pgdn": "<Page Down>",
    "up": "<Up>",
    "down": "<Down>",
    "left": "<Left>",
    "right": "<Right>",
    "prtscr": "<PrtScr>",
    "printscreen": "<PrtScr>",
    "mbutton": "<MButton>",
    "middle click": "<MButton>",
    "xbutton1": "<XButton1>",
    "xbutton2": "<XButton2>",
}

_MOD_ALIASES: dict[str, str] = {
    "win": "Win",
    "windows": "Win",
    "super": "Win",
    "meta": "Win",
    "ctrl": "Ctrl",
    "control": "Ctrl",
    "alt": "Alt",
    "shift": "Shift",
}


def atomic_write_text(path: Path, text: str) -> None:
    """Atomically write UTF-8 text to *path* using a temporary file rename."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    tmp_path.write_text(text, encoding="utf-8")
    os.replace(tmp_path, path)


def atomic_write_json(path: Path, data: Any) -> None:
    """Atomically write UTF-8 JSON to *path*."""
    atomic_write_text(
        path, json.dumps(data, indent=2, ensure_ascii=False) + "\n"
    )


def _unquote_yaml(val: str) -> str:
    s = val.strip()
    if len(s) >= 2 and s[0] == s[-1] and s[0] in ('"', "'"):
        inner = s[1:-1]
        if s[0] == '"':
            inner = inner.replace('\\"', '"').replace("\\\\", "\\")
        else:
            inner = inner.replace("''", "'")
        return inner
    return s


def quote_yaml(val: str) -> str:
    """Quote a YAML scalar safely if it contains special characters."""
    s = str(val)
    if not s:
        return '""'
    needs_quotes = (
        any(ch in s for ch in (":", "#", "{", "}", "[", "]", "&", "*", "?", "|", ">", "!", "%", "@", "`", '"', "'"))
        or s.startswith(("-", " ", "<", ">"))
        or s.endswith(" ")
        or s.lower() in ("true", "false", "null", "yes", "no")
    )
    if not needs_quotes:
        return s
    escaped = s.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def _slug(text: str) -> str:
    cleaned = re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_")
    return cleaned or "item"


def _clean_section_title(raw_section: str) -> str:
    text = _unquote_yaml(raw_section)
    text = re.sub(r"^<[^>]+>\s*", "", text).strip()
    return text or "General"


def _format_key_token(raw_key: str) -> str:
    tok = _unquote_yaml(raw_key)
    if len(tok) >= 2 and tok.isdigit():
        num = int(tok)
        return _VK_TO_KEY.get(num, f"VK_{num}")
    if tok.startswith("<") and tok.endswith(">") and len(tok) > 2:
        return tok[1:-1]
    return tok


def _extract_property_meta(
    raw_lines: list[str],
    section_display: str,
    id_prefix: str,
    source: str,
    seen_ids: dict[str, int],
) -> dict[str, Any]:
    name = ""
    description = ""
    recommended = False
    combos: list[str] = []

    cur_mods: dict[str, bool] = {"Win": False, "Ctrl": False, "Alt": False, "Shift": False}
    cur_keys: list[str] = []
    in_shortcut = False
    in_keys = False

    def _flush_combo() -> None:
        nonlocal cur_mods, cur_keys
        parts = [m for m in ("Win", "Ctrl", "Alt", "Shift") if cur_mods.get(m)]
        parts.extend(cur_keys)
        if parts:
            combo = "+".join(parts)
            if combo not in combos:
                combos.append(combo)
        cur_mods = {"Win": False, "Ctrl": False, "Alt": False, "Shift": False}
        cur_keys = []

    for line in raw_lines:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue

        m_name = re.match(r"^-\s*Name:\s*(.+)$", stripped)
        if m_name:
            name = _unquote_yaml(m_name.group(1))
            in_shortcut = False
            in_keys = False
            continue

        m_desc = re.match(r"^Description:\s*(.+)$", stripped)
        if m_desc:
            description = _unquote_yaml(m_desc.group(1))
            in_shortcut = False
            in_keys = False
            continue

        m_rec = re.match(r"^Recommended:\s*(.+)$", stripped)
        if m_rec:
            recommended = _unquote_yaml(m_rec.group(1)).lower() == "true"
            in_shortcut = False
            in_keys = False
            continue

        if re.match(r"^Shortcut:\s*$", stripped):
            in_shortcut = True
            in_keys = False
            continue

        if in_shortcut:
            m_sc_start = re.match(r"^-\s*(Win|Ctrl|Shift|Alt):\s*(true|false)\s*$", stripped, re.IGNORECASE)
            if m_sc_start:
                _flush_combo()
                in_keys = False
                mod_key = m_sc_start.group(1).capitalize()
                cur_mods[mod_key] = m_sc_start.group(2).lower() == "true"
                continue

            m_mod = re.match(r"^(Win|Ctrl|Shift|Alt):\s*(true|false)\s*$", stripped, re.IGNORECASE)
            if m_mod:
                in_keys = False
                mod_key = m_mod.group(1).capitalize()
                cur_mods[mod_key] = m_mod.group(2).lower() == "true"
                continue

            if re.match(r"^Keys:\s*$", stripped):
                in_keys = True
                continue

            if in_keys:
                m_key = re.match(r"^-\s*(.+)$", stripped)
                if m_key:
                    cur_keys.append(_format_key_token(m_key.group(1)))
                    continue

    _flush_combo()
    keys_str = " / ".join(combos)
    base_id = f"{id_prefix}::{_slug(section_display)}::{_slug(name)}::{_slug(keys_str)}"
    count = seen_ids.get(base_id, 0) + 1
    seen_ids[base_id] = count
    item_id = base_id if count == 1 else f"{base_id}_{count}"

    return {
        "id": item_id,
        "category": section_display,
        "name": name,
        "description": description,
        "keys": keys_str,
        "recommended": recommended,
        "source": source,
        "raw_lines": list(raw_lines),
    }


def parse_manifest(
    path: Path, id_prefix: str = "win", source: str = "windows"
) -> dict[str, Any]:
    """Parse a PowerToys Shortcut Guide YAML manifest preserving exact raw lines."""
    if not path.is_file():
        return {
            "header_lines": [],
            "shortcuts_line": "Shortcuts:",
            "sections": [],
            "items": [],
        }

    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    header_lines: list[str] = []
    shortcuts_line = "Shortcuts:"
    sections: list[dict[str, Any]] = []
    items: list[dict[str, Any]] = []
    seen_ids: dict[str, int] = {}

    in_shortcuts = False
    cur_section: dict[str, Any] | None = None
    cur_prop_lines: list[str] | None = None

    def _flush_prop() -> None:
        nonlocal cur_prop_lines
        if cur_section is not None and cur_prop_lines:
            meta = _extract_property_meta(
                cur_prop_lines,
                cur_section["section_name"],
                id_prefix=id_prefix,
                source=source,
                seen_ids=seen_ids,
            )
            cur_section["properties"].append(meta)
            items.append(meta)
        cur_prop_lines = None

    for line in lines:
        if not in_shortcuts:
            if re.match(r"^Shortcuts:\s*$", line):
                in_shortcuts = True
                shortcuts_line = line
            else:
                header_lines.append(line)
            continue

        m_sec = re.match(r"^\s*-\s*SectionName:\s*(.+)$", line)
        if m_sec:
            _flush_prop()
            sec_display = _clean_section_title(m_sec.group(1))
            cur_section = {
                "raw_section_lines": [line],
                "section_name": sec_display,
                "properties": [],
            }
            sections.append(cur_section)
            continue

        if cur_section is not None and not cur_section["properties"] and cur_prop_lines is None:
            if re.match(r"^\s*Properties:\s*$", line):
                cur_section["raw_section_lines"].append(line)
                continue

        if re.match(r"^\s{4,8}-\s*Name:\s*.+$", line):
            _flush_prop()
            cur_prop_lines = [line]
            continue

        if cur_prop_lines is not None:
            cur_prop_lines.append(line)
        elif cur_section is not None:
            cur_section["raw_section_lines"].append(line)

    _flush_prop()
    return {
        "header_lines": header_lines,
        "shortcuts_line": shortcuts_line,
        "sections": sections,
        "items": items,
    }


def write_filtered_shell_manifest(
    master_path: Path, output_path: Path, hidden_ids: set[str]
) -> tuple[int, int]:
    """Emit a lossless filtered +WindowsNT.Shell.en-US.yml omitting *hidden_ids*."""
    parsed = parse_manifest(master_path, id_prefix="win", source="windows")
    out_lines: list[str] = list(parsed["header_lines"])
    out_lines.append(parsed["shortcuts_line"])

    active_count = 0
    hidden_count = 0

    for sec in parsed["sections"]:
        kept_props = []
        for prop in sec["properties"]:
            if prop["id"] in hidden_ids:
                hidden_count += 1
            else:
                kept_props.append(prop)
                active_count += 1

        if not kept_props:
            continue
        out_lines.extend(sec["raw_section_lines"])
        for prop in kept_props:
            out_lines.extend(prop["raw_lines"])

    atomic_write_text(output_path, "\n".join(out_lines) + "\n")
    return active_count, hidden_count


def _parse_single_combo(combo_str: str) -> dict[str, Any] | None:
    raw = combo_str.strip()
    if not raw:
        return None

    mods = {"Win": False, "Ctrl": False, "Shift": False, "Alt": False}
    keys: list[str] = []

    for token in (part.strip() for part in raw.split("+") if part.strip()):
        low = token.lower()
        if low in _MOD_ALIASES:
            mods[_MOD_ALIASES[low]] = True
        elif low in _SPECIAL_KEY_TO_PT:
            keys.append(_SPECIAL_KEY_TO_PT[low])
        elif len(token) == 1:
            keys.append(token.upper())
        else:
            keys.append(token)

    if not keys:
        return None
    return {**mods, "Keys": keys}


def parse_shortcut_combo(combo_str: str) -> list[dict[str, Any]]:
    """Parse one or more (' / '-separated) shortcut strings into PowerToys Shortcut dicts."""
    variants: list[dict[str, Any]] = []
    for part in combo_str.split(" / "):
        parsed = _parse_single_combo(part)
        if parsed is not None:
            variants.append(parsed)
    return variants


def write_custom_manifest(
    output_path: Path, items_by_category: dict[str, list[dict[str, Any]]]
) -> int:
    """Write +Custom.MyShortcuts.en-US.yml for PowerToys Shortcut Guide."""
    lines: list[str] = [
        "PackageName: +Custom.MyShortcuts",
        "Name: My Custom Shortcuts",
        'WindowFilter: "*"',
        "BackgroundProcess: true",
        "Shortcuts:",
    ]

    emitted = 0
    for category, items in items_by_category.items():
        section_props: list[tuple[dict[str, Any], list[dict[str, Any]]]] = []
        for item in items:
            combos = parse_shortcut_combo(str(item.get("keys", "")))
            if not combos:
                continue
            section_props.append((item, combos))

        if not section_props:
            continue

        lines.append(f"  - SectionName: {quote_yaml(category)}")
        lines.append("    Properties:")
        for item, combos in section_props:
            emitted += 1
            lines.append(f"      - Name: {quote_yaml(str(item.get('name', 'Shortcut')))}")
            desc = str(item.get("description") or "").strip()
            if desc:
                lines.append(f"        Description: {quote_yaml(desc)}")
            lines.append("        Recommended: true")
            lines.append("        Shortcut:")
            for sc in combos:
                lines.append(f"          - Win: {'true' if sc['Win'] else 'false'}")
                lines.append(f"            Ctrl: {'true' if sc['Ctrl'] else 'false'}")
                lines.append(f"            Shift: {'true' if sc['Shift'] else 'false'}")
                lines.append(f"            Alt: {'true' if sc['Alt'] else 'false'}")
                lines.append("            Keys:")
                for k in sc["Keys"]:
                    lines.append(f"              - {quote_yaml(str(k))}")

    atomic_write_text(output_path, "\n".join(lines) + "\n")
    return emitted


def run_powertoys_indexer(indexer_exe_path: Path) -> tuple[bool, str]:
    """Run PowerToys ShortcutGuide IndexYmlGenerator.exe silently."""
    if not indexer_exe_path.is_file():
        return False, f"Indexer not found: {indexer_exe_path}"
    flags = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
    proc = subprocess.run(
        [str(indexer_exe_path)],
        capture_output=True,
        text=True,
        creationflags=flags,
        timeout=25,
    )
    output = ((proc.stdout or "") + "\n" + (proc.stderr or "")).strip()
    return proc.returncode == 0, output
