"""Unit and integration tests for Shortcut Hub and PowerToys YAML integration."""

from __future__ import annotations

import json
import shutil
import tempfile
import unittest
from pathlib import Path

import discovery
import powertoys_yaml
import store

_SAMPLE_MASTER_SHELL = """\
PackageName: +WindowsNT.Shell
WindowFilter: "*"
BackgroundProcess: true
Shortcuts:
  - SectionName: Desktop Shortcuts
    Properties:
      - Name: Close active window
        Shortcut:
        - Win: false
          Ctrl: false
          Shift: false
          Alt: true
          Keys:
            - F4
      - Name: Cycle through open Windows
        Shortcut:
        - Win: false
          Ctrl: false
          Shift: false
          Alt: true
          Keys:
            - Esc
  - SectionName: Virtual desktop
    Properties:
      - Name: Create new virtual desktop
        Shortcut:
        - Win: true
          Ctrl: true
          Shift: false
          Alt: false
          Keys:
            - D
"""


class ShortcutHubTests(unittest.TestCase):
    """Tests covering YAML lossless filtering, auto-discovery, and store CRUD/sync."""

    def setUp(self) -> None:
        self.src_dir = Path(__file__).resolve().parent
        self.tmp = Path(tempfile.mkdtemp(prefix="shortcut_hub_test_"))
        self.project_dir = self.tmp / "shortcut-hub"
        self.project_dir.mkdir(parents=True)

        self.master_yml = self.tmp / "master" / "+WindowsNT.Shell.en-US.yml"
        self.master_yml.parent.mkdir(parents=True)
        self.master_yml.write_text(_SAMPLE_MASTER_SHELL, encoding="utf-8")

        self.user_pt_dir = self.tmp / "user_pt"
        self.user_pt_dir.mkdir(parents=True)
        shutil.copy2(self.master_yml, self.user_pt_dir / "+WindowsNT.Shell.en-US.yml")
        (self.user_pt_dir / "Microsoft.PowerToys.en-US.yml").write_text(
            """\
PackageName: Microsoft.PowerToys
Name: PowerToys
BackgroundProcess: True
WindowFilter: "powertoys.exe"
Shortcuts:
  - SectionName: General
    Properties:
      - Name: Peek
        Shortcut:
        - Win: False
          Ctrl: True
          Alt: False
          Shift: False
          Keys:
            - 32
""",
            encoding="utf-8",
        )

        self.watch_dir = self.tmp / "automations"
        self.watch_dir.mkdir(parents=True)
        (self.watch_dir / "quick-notes").mkdir()
        (self.watch_dir / "quick-notes" / "quick_notes.ahk").write_text(
            "#Requires AutoHotkey v2.0\n", encoding="utf-8"
        )
        (self.watch_dir / "quick-notes" / "config.json").write_text(
            json.dumps(
                {
                    "hotkeys": {
                        "quick_capture": "^!j",
                        "quick_capture_alt": "#F8",
                        "direct_destinations_enabled": True,
                        "direct_shortcuts": {"inbox": "^!+i"},
                    },
                    "destinations": [{"id": "inbox", "label": "Inbox"}],
                }
            ),
            encoding="utf-8",
        )

        self.startup_dir = self.tmp / "startup"
        self.startup_dir.mkdir(parents=True)
        (self.startup_dir / "PowerToggle.ahk").write_text(
            "; Win + F9: Set Screen & Standby to NEVER\n#F9::\n    return\n",
            encoding="utf-8",
        )
        (self.startup_dir / "WindowSwitcher.ahk").write_text(
            "; Middle mouse button opens Task View\nMButton::\n    return\n",
            encoding="utf-8",
        )

        self.appdata_dir = self.tmp / "appdata"
        (self.appdata_dir / "copyq").mkdir(parents=True)
        (self.appdata_dir / "copyq" / "copyq-commands.ini").write_text(
            "[Commands]\n1\\Name=Paste Plain\n1\\Shortcut=ctrl+shift+l\n1\\IsGlobalShortcut=true\n",
            encoding="utf-8",
        )
        (self.appdata_dir / "Code" / "User").mkdir(parents=True)
        (self.appdata_dir / "Code" / "User" / "keybindings.json").write_text(
            '// comment\n[{"key": "ctrl+alt+k", "command": "custom.cmd"}, {"key": "ctrl+a", "command": "-unbind.this"}]\n',
            encoding="utf-8",
        )

        cfg = {
            "hotkeys": {"toggle_overlay": "^!k", "toggle_overlay_alt": "#F7"},
            "powertoys": {
                "master_shell_yml": str(self.master_yml),
                "user_shortcuts_dir": str(self.user_pt_dir),
                "index_generator_exe": str(self.tmp / "missing_indexer.exe"),
                "custom_manifest_name": "+Custom.MyShortcuts.en-US.yml",
                "sync_enabled": True,
            },
            "discovery": {
                "scan_startup_dir": True,
                "extra_watch_dirs": [str(self.watch_dir)],
                "scan_powertoys_manifest": True,
                "scan_copyq": True,
                "scan_vscode_keybindings": True,
                "ignore_dir_names": ["backup_original"],
            },
        }
        (self.project_dir / "config.json").write_text(
            json.dumps(cfg, indent=2), encoding="utf-8"
        )

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_01_lossless_windows_yaml_parse_hide_and_restore(self) -> None:
        parsed = powertoys_yaml.parse_manifest(self.master_yml)
        self.assertEqual(len(parsed["items"]), 3)
        self.assertEqual(parsed["items"][0]["keys"], "Alt+F4")
        self.assertEqual(parsed["items"][2]["keys"], "Win+Ctrl+D")

        hidden_id = parsed["items"][1]["id"]
        out_yml = self.user_pt_dir / "+WindowsNT.Shell.en-US.yml"
        active_cnt, hidden_cnt = powertoys_yaml.write_filtered_shell_manifest(
            self.master_yml, out_yml, {hidden_id}
        )
        self.assertEqual(active_cnt, 2)
        self.assertEqual(hidden_cnt, 1)
        reparsed = powertoys_yaml.parse_manifest(out_yml)
        self.assertEqual(len(reparsed["items"]), 2)
        self.assertNotIn(hidden_id, [x["id"] for x in reparsed["items"]])

        # Restore back to full lossless master
        active2, hidden2 = powertoys_yaml.write_filtered_shell_manifest(
            self.master_yml, out_yml, set()
        )
        self.assertEqual(active2, 3)
        self.assertEqual(hidden2, 0)
        self.assertEqual(
            out_yml.read_text(encoding="utf-8"),
            self.master_yml.read_text(encoding="utf-8"),
        )

    def test_02_discovery_ahk_config_copyq_vscode_and_powertoys(self) -> None:
        cfg = store.load_config(self.project_dir)
        items = discovery.discover_all(
            config=cfg,
            project_dir=self.project_dir,
            startup_dir=self.startup_dir,
            appdata_dir=self.appdata_dir,
            localappdata_dir=self.tmp,
        )
        keys = {x["keys"]: x for x in items}
        self.assertIn("Ctrl+Alt+J", keys)
        self.assertIn("Win+F8", keys)
        self.assertIn("Ctrl+Alt+Shift+I", keys)
        self.assertIn("Win+F9", keys)
        self.assertEqual(keys["Win+F9"]["name"], "Set Screen & Standby to NEVER")
        self.assertIn("Middle Click", keys)
        self.assertIn("Ctrl+Shift+L", keys)
        self.assertIn("Ctrl+Alt+K", keys)
        self.assertIn("Ctrl+Space", keys)
        # Unbind '-unbind.this' must be ignored
        self.assertFalse(any("unbind.this" in x["name"] for x in items))

    def test_03_annotated_ahk_shortcut_comment(self) -> None:
        ahk_path = self.watch_dir / "custom.ahk"
        ahk_path.write_text(
            "; @shortcut [Research] Ctrl+Alt+R = Open ArXiv Search | Launches ArXiv query\n",
            encoding="utf-8",
        )
        items = discovery.parse_ahk_file(ahk_path)
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["category"], "Research")
        self.assertEqual(items[0]["keys"], "Ctrl+Alt+R")
        self.assertEqual(items[0]["name"], "Open ArXiv Search")
        self.assertEqual(items[0]["description"], "Launches ArXiv query")

    def test_04_manual_crud_hide_restore_and_sync_pipeline(self) -> None:
        # Add manual shortcut
        added = store.add_manual_shortcut(
            self.project_dir,
            category="My Workflows",
            keys="Ctrl+Alt+N",
            name="Quick Scratchpad",
            description="Opens scratchpad note",
        )
        self.assertTrue(added["id"].startswith("manual::"))

        # Edit manual shortcut
        updated = store.update_manual_shortcut(
            self.project_dir,
            added["id"],
            category="My Workflows",
            keys="Ctrl+Alt+M",
            name="Quick Scratchpad v2",
            description="Updated",
        )
        self.assertEqual(updated["keys"], "Ctrl+Alt+M")

        # Hide one Windows shortcut
        cat = store.get_catalog(
            self.project_dir,
            rescan=True,
            startup_dir=self.startup_dir,
            appdata_dir=self.appdata_dir,
            localappdata_dir=self.tmp,
        )
        win_item = [x for x in cat["active"] if x["source"] == "windows"][0]
        self.assertTrue(store.hide_shortcut(self.project_dir, win_item["id"]))

        # Sync to PowerToys manifests
        sync_res = store.sync_to_powertoys(
            self.project_dir,
            rescan=False,
            run_indexer=False,
            startup_dir=self.startup_dir,
            appdata_dir=self.appdata_dir,
            localappdata_dir=self.tmp,
        )
        self.assertEqual(sync_res["active_windows"], 2)
        self.assertEqual(sync_res["hidden_windows"], 1)
        self.assertGreater(sync_res["custom_count"], 0)
        self.assertTrue((self.project_dir / "state" / "backup" / "+WindowsNT.Shell.en-US.orig.yml").is_file())

        custom_yml_text = (self.user_pt_dir / "+Custom.MyShortcuts.en-US.yml").read_text(encoding="utf-8")
        self.assertIn("Quick Scratchpad v2", custom_yml_text)
        self.assertIn("My Workflows", custom_yml_text)
        self.assertIn("Quick Capture Overlay", custom_yml_text)

        # Restore all Windows shortcuts & delete manual item
        restored_cnt = store.restore_all_windows(self.project_dir)
        self.assertEqual(restored_cnt, 1)
        self.assertTrue(store.delete_manual_shortcut(self.project_dir, added["id"]))
        sync_res2 = store.sync_to_powertoys(
            self.project_dir,
            rescan=False,
            run_indexer=False,
            startup_dir=self.startup_dir,
            appdata_dir=self.appdata_dir,
            localappdata_dir=self.tmp,
        )
        self.assertEqual(sync_res2["active_windows"], 3)
        self.assertEqual(sync_res2["hidden_windows"], 0)

    def test_05_config_local_override_and_category_hints(self) -> None:
        (self.project_dir / "config.local.json").write_text(
            json.dumps(
                {
                    "discovery": {
                        "category_hints": {"quick-notes": "Notes App"},
                    }
                }
            ),
            encoding="utf-8",
        )
        cfg = store.load_config(self.project_dir)
        items = discovery.discover_all(
            config=cfg,
            project_dir=self.project_dir,
            startup_dir=self.startup_dir,
            appdata_dir=self.appdata_dir,
            localappdata_dir=self.tmp,
        )
        notes_items = [x for x in items if x["category"] == "Notes App"]
        self.assertGreaterEqual(len(notes_items), 3)

    def test_06_builtin_windows_fallback_when_powertoys_absent(self) -> None:
        standalone_dir = self.tmp / "standalone"
        standalone_dir.mkdir()
        (standalone_dir / "config.json").write_text(
            json.dumps(
                {
                    "powertoys": {
                        "master_shell_yml": str(self.tmp / "missing_master.yml"),
                        "user_shortcuts_dir": str(self.tmp / "missing_user_pt"),
                        "index_generator_exe": str(self.tmp / "missing_indexer.exe"),
                        "sync_enabled": True,
                    },
                    "discovery": {
                        "scan_startup_dir": False,
                        "extra_watch_dirs": [],
                        "scan_powertoys_manifest": False,
                        "scan_copyq": False,
                        "scan_vscode_keybindings": False,
                    },
                }
            ),
            encoding="utf-8",
        )
        sync_res = store.sync_to_powertoys(standalone_dir, rescan=True, run_indexer=True)
        self.assertTrue(sync_res["ok"])
        self.assertFalse(sync_res.get("powertoys_installed", True))
        cat = store.get_catalog(standalone_dir, rescan=False)
        self.assertGreaterEqual(cat["stats"]["windows"], 25)
        self.assertTrue(any(x["keys"] == "Win+V" for x in cat["active"]))


if __name__ == "__main__":
    unittest.main()
