# Shortcut Hub Roadmap & Future Architecture Ideas

## Shipped in `v0.5.0`

- **Full-Screen Multi-Column Overview & Auto-Focused Search**: Opens across the active monitor work area with responsive multi-column grids (`2–4` columns) and immediately focuses the search bar when toggled open.
- **Quick Reference & Notes Sidebar (`/skills`, Snippets, Scratchpad)**: Dedicated right-hand reference panel for AI prompt `/skills`, reusable templates, and CLI snippets with 1-click **`Copy`**, **`Pin`**, and an auto-saved multiline **`Scratchpad`**.
- **Shortcut Learning Highlights (`Pin` / `Pinned`)**: Pin individual shortcuts in gold to stand out while internalizing them, with a dedicated `Pinned (N)` filter chip.
- **Restore Undo (`Undo Restore`) & Header Click Fix**: Fixed header button interaction capture and added one-click revert for `Restore Windows`.

## Shipped in `v0.4.0`

- **In-UI Overlay Hotkey Settings (`Hotkeys`)**: Reassign both the Primary and Alternate global overlay toggle shortcuts directly inside the UI using live **`Record`**, **`Clear`**, **`Reset Defaults`**, and **`Save & Apply`**, persisted cleanly to gitignored `config.local.json`.
- **Zero-Restart Live Hot-Reload**: Uses a Win32 `WM_USER + 1` thread message to unregister old `RegisterHotKey` / `WH_KEYBOARD_LL` bindings and register new ones dynamically—including auto-pausing active global bindings during key recording so previously bound combinations can be captured cleanly.
- **Actionable Custom Hotkeys**: Manual entries added via **`+ Add Shortcut`** or **`Edit`** support optional **`Open App / URL`** (`ShellExecuteW`) and **`Run Command`** (hidden PowerShell CLI) bindings labeled with a `Live` badge. Hiding or deleting an item automatically unregisters its global hotkey.

---

## Future Ideas & Technical Blueprints

The capabilities below were intentionally deferred to keep Shortcut Hub lightweight, deterministic, and conflict-free, along with concrete implementation notes for anyone looking to tackle them next.

### 1. Per-App / Context-Aware Hotkey Scopes

**Goal**: Trigger a custom hotkey or filter the overlay automatically depending on which application is currently in the foreground (e.g., browser vs. IDE vs. terminal).

**Implementation Blueprint**:
- Add an optional `app_scope: Option<String>` (process executable name such as `code.exe` or window class filter) to `HotkeyBinding` and `ShortcutItem`.
- Because Win32 `RegisterHotKey` cannot conditionally pass keys back to the active application, route scoped bindings exclusively through `WH_KEYBOARD_LL`.
- Inside `ll_keyboard_proc`, resolve `GetForegroundWindow()` → `GetWindowThreadProcessId()` → `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` → `QueryFullProcessImageNameW`. Cache `(HWND, exe_name)` with a short invalidation check on foreground changes so hook latency stays well under Windows' `LowLevelHooksTimeout` (~300 ms).
- Return `CallNextHookEx(...)` untouched whenever the foreground process does not match `app_scope`.

### 2. Key-to-Key Chord Remapping & Multi-Step Macros

**Goal**: Remap hardware keys (e.g., `CapsLock` → `Ctrl`) or trigger multi-step keystroke sequences and text snippets from a single hotkey.

**Implementation Blueprint**:
- Introduce an action variant `SendKeySequence(Vec<KeyStep>)` backed by Win32 `SendInput`.
- Before injecting the target chord, synthesize explicit `KEYEVENTF_KEYUP` events for any physical trigger modifiers currently held down so they do not bleed into the remapped output.
- Tag all synthetic `KEYBDINPUT` events with a non-zero `dwExtraInfo` sentinel (e.g., `0x5348_5542`) instead of ignoring `LLKHF_INJECTED` globally, preventing infinite feedback loops while remaining compatible with virtual keyboards and Accessibility tools.
- Consider opt-in UIPI elevation notices since standard-privilege hooks cannot send synthetic input into Administrator windows.

### 3. Two-Way External App Config Write-Back

**Goal**: Modify or remove shortcuts discovered from VS Code, Cursor, PowerToys, or AutoHotkey directly inside Shortcut Hub and write those edits back to the original config files cleanly.

**Implementation Blueprint**:
- **IDEs (VS Code / Cursor / Antigravity)**: Replace regex JSONC stripping with a span-preserving CST/token parser (or surgical line-level patcher) so editing `keybindings.json` preserves user comments, ordering, and trailing commas.
- **PowerToys**: Parse and update specific module blocks in `%LOCALAPPDATA%\Microsoft\PowerToys\*\settings.json` with atomic write + backup snapshots before touching user settings.
- **AutoHotkey**: Only auto-patch structured `; @shortcut` metadata lines or Companion `config.json` files; keep arbitrary `.ahk` control-flow scripts read-only to avoid breaking multi-line hotkey routines or `#HotIf` blocks.

### 4. Sharable Community Shortcut Packs (Import / Export)

**Goal**: Export curated categories or download portable shortcut bundles for common workflows (Vim navigation, Tiling Window Managers, Video Editing, Research) with conflict pre-checks.

**Implementation Blueprint**:
- Serialize selected categories from `state/user_shortcuts.json` into a portable `.shortcuthub.json` pack (excluding machine-local file paths and hidden IDs unless explicitly opted in).
- Add an **Import Pack** modal that runs `parse_display_hotkey` + dry-run `RegisterHotKey` conflict detection and lets users resolve duplicate bindings before merging.
