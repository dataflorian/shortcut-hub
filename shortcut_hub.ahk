#Requires AutoHotkey v2.0
#SingleInstance Force
#UseHook True
ListLines False
KeyHistory 0
A_MenuMaskKey := "vkE8"

; ==============================================================================
; Shortcut Hub - Global Overlay & PowerToys Sync Trigger (AutoHotkey v2)
; ==============================================================================

global ProjectDir := A_ScriptDir
global ConfigPath := ProjectDir . "\config.json"
global StateDir := ProjectDir . "\state"
global DebugLogPath := StateDir . "\hub_debug.log"
global OverlayScript := ProjectDir . "\overlay_ui.py"
global StoreScript := ProjectDir . "\store.py"
global PythonWExe := ResolvePythonWExe()

global AppConfig := LoadConfigFile()
global ToggleKey := AppConfig["hotkeys"].Get("toggle_overlay", "^!k")
global ToggleDisplay := AppConfig["hotkeys"].Get("toggle_overlay_display", "Ctrl+Alt+K")
global ToggleAltKey := AppConfig["hotkeys"].Get("toggle_overlay_alt", "#F7")
global ToggleAltDisplay := AppConfig["hotkeys"].Get("toggle_overlay_alt_display", "Win+F7")

global ToastGui := ""
global ToastTimerFn := ""

EnsureDir(StateDir)
HotkeysLabel := ToggleDisplay . (ToggleAltDisplay != "" ? " / " . ToggleAltDisplay : "")
A_IconTip := "Shortcut Hub (" . HotkeysLabel . ")"
SetupTrayMenu()
RegisterConfiguredHotkeys()

; ==============================================================================
; Setup & Helpers
; ==============================================================================

ResolvePythonWExe() {
    localAppData := EnvGet("LOCALAPPDATA")
    roots := []
    if (localAppData != "")
        roots.Push(localAppData . "\Programs\Python")
    roots.Push("C:")

    for root in roots {
        if !DirExist(root)
            continue
        Loop Files root . "\Python*", "D" {
            for exeName in ["pythonw.exe", "python.exe"] {
                cand := A_LoopFileFullPath . "\" . exeName
                if FileExist(cand)
                    return cand
            }
        }
    }
    return "pythonw.exe"
}

EnsureDir(dirPath) {
    if !DirExist(dirPath)
        DirCreate(dirPath)
}

LoadConfigFile() {
    if !FileExist(ConfigPath) {
        MsgBox("Missing config.json at:`n" . ConfigPath, "Shortcut Hub", "Icon!")
        ExitApp()
    }
    raw := FileRead(ConfigPath, "UTF-8")
    return JsonParse(raw)
}

EnsureHookPrefix(hkStr) {
    trimmed := Trim(hkStr)
    if (trimmed = "")
        return ""
    first := SubStr(trimmed, 1, 1)
    if (first = "$" || first = "~" || first = "*")
        return trimmed
    return "$" . trimmed
}

LogDebug(msg) {
    line := FormatTime(, "yyyy-MM-dd HH:mm:ss") . " | " . msg . "`n"
    try FileAppend(line, DebugLogPath, "UTF-8")
    try FileAppend(line, "*")
}

SetupTrayMenu() {
    tray := A_TrayMenu
    tray.Delete()
    trayLabel := ToggleDisplay . (ToggleAltDisplay != "" ? " / " . ToggleAltDisplay : "")
    defaultTitle := "Toggle Shortcut Hub (" . trayLabel . ")"
    tray.Add(defaultTitle, (*) => ToggleOverlay("TrayMenu"))
    tray.Default := defaultTitle
    tray.Add("Auto-Scan && Sync to PowerToys Now", (*) => SyncNow())
    tray.Add("Open PowerToys Shortcut Guide", (*) => Send("{LWin down}{LShift down}{vkBF}{LShift up}{LWin up}"))
    tray.Add()
    tray.Add("Open Project Folder", (*) => Run('explorer.exe "' . ProjectDir . '"'))
    tray.Add("Reload Script", (*) => Reload())
    tray.Add("Exit", (*) => ExitApp())
}

RegisterConfiguredHotkeys() {
    HotIf()
    failed := []

    primaryHook := EnsureHookPrefix(ToggleKey)
    if (primaryHook != "") {
        try {
            Hotkey(primaryHook, (*) => ToggleOverlay(ToggleDisplay))
            LogDebug("Registered primary hotkey " . ToggleDisplay . " (" . primaryHook . ")")
        } catch as err {
            msg := ToggleDisplay . " (" . primaryHook . "): " . err.Message
            failed.Push(msg)
            LogDebug("FAILED primary hotkey " . msg)
        }
    }

    altHook := EnsureHookPrefix(ToggleAltKey)
    if (altHook != "") {
        try {
            Hotkey(altHook, (*) => ToggleOverlay(ToggleAltDisplay))
            LogDebug("Registered alternate hotkey " . ToggleAltDisplay . " (" . altHook . ")")
        } catch as err {
            msg := ToggleAltDisplay . " (" . altHook . "): " . err.Message
            failed.Push(msg)
            LogDebug("FAILED alternate hotkey " . msg)
        }
    }

    readyKeys := ToggleDisplay . (ToggleAltDisplay != "" ? " or " . ToggleAltDisplay : "")
    LogDebug("Ready. Press " . readyKeys . " to toggle Shortcut Hub.")
    if (failed.Length > 0) {
        ShowToast("Shortcut Hub Hotkey Warning", failed[1], "error", 5000)
    } else {
        ShowToast("Shortcut Hub Ready", "Press " . readyKeys . " to open overlay", "success", 3500)
    }
}

; ==============================================================================
; Toggle Overlay & Background Sync
; ==============================================================================

ToggleOverlay(source) {
    LogDebug("Trigger [" . source . "] -> ToggleOverlay")
    try {
        hwnd := WinExist("Shortcut Hub ahk_class TkTopLevel")
        if (hwnd) {
            WinClose("ahk_id " . hwnd)
            LogDebug("Closed existing Shortcut Hub overlay (hwnd=" . hwnd . ")")
            return
        }
        cmd := '"' . PythonWExe . '" "' . OverlayScript . '"'
        Run(cmd, ProjectDir, "Hide")
        LogDebug("Launched Shortcut Hub overlay")
    } catch as err {
        LogDebug("ERROR in ToggleOverlay: " . err.Message)
        ShowToast("Shortcut Hub Error", err.Message, "error", 4500)
    }
}

SyncNow() {
    LogDebug("Manual PowerToys sync triggered from tray")
    ShowToast("Shortcut Hub", "Scanning & syncing to PowerToys...", "info", 2200)
    try {
        cmd := '"' . PythonWExe . '" "' . StoreScript . '" sync'
        RunWait(cmd, ProjectDir, "Hide")
        ShowToast("Shortcut Hub Synced", "PowerToys Shortcut Guide updated.", "success", 3000)
    } catch as err {
        ShowToast("Sync Error", err.Message, "error", 4500)
    }
}

; ==============================================================================
; Non-Activating DPI-Aware Toast Notification
; ==============================================================================

ShowToast(title, body := "", kind := "info", durationMs := 2800) {
    global ToastGui, ToastTimerFn

    if IsObject(ToastGui) {
        try ToastGui.Destroy()
    }
    if (ToastTimerFn != "") {
        SetTimer(ToastTimerFn, 0)
    }

    accentColor := "89B4FA"
    if (kind = "success")
        accentColor := "A6E3A1"
    else if (kind = "error")
        accentColor := "F38BA8"
    else if (kind = "warn")
        accentColor := "F9E2AF"

    ToastGui := Gui("+AlwaysOnTop -Caption +ToolWindow +Border +E0x08000000")
    ToastGui.BackColor := "1E1E2E"
    ToastGui.MarginX := 14
    ToastGui.MarginY := 10

    ToastGui.SetFont("s9.5 Bold c" . accentColor, "Segoe UI")
    ToastGui.AddText("x14 y10 w310", title)

    if (Trim(body) != "") {
        ToastGui.SetFont("s9 Norm cCDD6F4", "Segoe UI")
        ToastGui.AddText("x14 y30 w310", body)
        winH := 62
    } else {
        winH := 40
    }

    winW := 340
    scale := A_ScreenDPI / 96.0
    physW := Round(winW * scale)
    physH := Round(winH * scale)
    margin := Round(20 * scale)
    MonitorGetWorkArea(1, &wL, &wT, &wR, &wB)
    tx := Max(wL, wR - physW - margin)
    ty := Max(wT, wB - physH - margin)

    ToastGui.Show("NoActivate x" . tx . " y" . ty . " w" . winW . " h" . winH)
    ToastTimerFn := HideToast
    SetTimer(HideToast, -durationMs)
}

HideToast() {
    global ToastGui
    if IsObject(ToastGui) {
        try ToastGui.Destroy()
        ToastGui := ""
    }
}

; ==============================================================================
; Pure AutoHotkey v2 JSON Parser
; ==============================================================================

JsonParse(str) {
    pos := 1
    len := StrLen(str)

    SkipWs() {
        while (pos <= len) {
            ch := SubStr(str, pos, 1)
            if (ch = " " || ch = "`t" || ch = "`r" || ch = "`n")
                pos += 1
            else
                break
        }
    }

    ParseValue() {
        SkipWs()
        if (pos > len)
            throw Error("Unexpected end of JSON")
        ch := SubStr(str, pos, 1)
        if (ch = "{")
            return ParseObject()
        if (ch = "[")
            return ParseArray()
        if (ch = '"')
            return ParseString()
        if (ch = "t" && SubStr(str, pos, 4) = "true") {
            pos += 4
            return true
        }
        if (ch = "f" && SubStr(str, pos, 5) = "false") {
            pos += 5
            return false
        }
        if (ch = "n" && SubStr(str, pos, 4) = "null") {
            pos += 4
            return ""
        }
        return ParseNumber()
    }

    ParseObject() {
        obj := Map()
        obj.CaseSense := "Off"
        pos += 1
        SkipWs()
        if (pos <= len && SubStr(str, pos, 1) = "}") {
            pos += 1
            return obj
        }
        while (pos <= len) {
            SkipWs()
            key := ParseString()
            SkipWs()
            if (SubStr(str, pos, 1) != ":")
                throw Error("Expected ':' at position " . pos)
            pos += 1
            val := ParseValue()
            obj[key] := val
            SkipWs()
            ch := SubStr(str, pos, 1)
            if (ch = "}") {
                pos += 1
                return obj
            }
            if (ch != ",")
                throw Error("Expected ',' or '}' at position " . pos)
            pos += 1
        }
        throw Error("Unterminated JSON object")
    }

    ParseArray() {
        arr := []
        pos += 1
        SkipWs()
        if (pos <= len && SubStr(str, pos, 1) = "]") {
            pos += 1
            return arr
        }
        while (pos <= len) {
            arr.Push(ParseValue())
            SkipWs()
            ch := SubStr(str, pos, 1)
            if (ch = "]") {
                pos += 1
                return arr
            }
            if (ch != ",")
                throw Error("Expected ',' or ']' at position " . pos)
            pos += 1
        }
        throw Error("Unterminated JSON array")
    }

    ParseString() {
        pos += 1
        out := ""
        while (pos <= len) {
            ch := SubStr(str, pos, 1)
            if (ch = '"') {
                pos += 1
                return out
            }
            if (ch = "\") {
                pos += 1
                esc := SubStr(str, pos, 1)
                if (esc = '"' || esc = "\" || esc = "/")
                    out .= esc
                else if (esc = "b")
                    out .= "`b"
                else if (esc = "f")
                    out .= "`f"
                else if (esc = "n")
                    out .= "`n"
                else if (esc = "r")
                    out .= "`r"
                else if (esc = "t")
                    out .= "`t"
                else if (esc = "u") {
                    hex := SubStr(str, pos + 1, 4)
                    out .= Chr(Integer("0x" . hex))
                    pos += 4
                } else {
                    out .= esc
                }
                pos += 1
            } else {
                out .= ch
                pos += 1
            }
        }
        throw Error("Unterminated JSON string")
    }

    ParseNumber() {
        start := pos
        if (SubStr(str, pos, 1) = "-")
            pos += 1
        while (pos <= len && InStr("0123456789.eE+-", SubStr(str, pos, 1)))
            pos += 1
        numStr := SubStr(str, start, pos - start)
        if (InStr(numStr, ".") || InStr(numStr, "e") || InStr(numStr, "E"))
            return Float(numStr)
        return Integer(numStr)
    }

    return ParseValue()
}
