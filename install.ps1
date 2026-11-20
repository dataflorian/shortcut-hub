param(
    [switch]$SkipLaunch,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"

$ProjectDir   = Split-Path -Parent $MyInvocation.MyCommand.Path
$NativeDir    = Join-Path $ProjectDir "native"
$NativeExe    = Join-Path $ProjectDir "shortcut-hub.exe"
$StoreScript  = Join-Path $ProjectDir "store.py"

$StartupDir   = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\Startup"
$ShortcutPath = Join-Path $StartupDir "ShortcutHub.lnk"

function Find-PythonExe {
    $cmd = Get-Command python.exe -ErrorAction SilentlyContinue
    if ($cmd -and (Test-Path $cmd.Source)) {
        return $cmd.Source
    }

    $pyLauncher = Get-Command py.exe -ErrorAction SilentlyContinue
    if ($pyLauncher) {
        try {
            $resolved = (& $pyLauncher.Source -3 -c "import sys; print(sys.executable)" 2>$null | Select-Object -First 1)
            if ($resolved -and (Test-Path $resolved.Trim())) {
                return $resolved.Trim()
            }
        } catch {
        }
    }

    $searchRoots = @("C:\")
    if ($env:LOCALAPPDATA) {
        $searchRoots = @((Join-Path $env:LOCALAPPDATA "Programs\Python"), "C:\")
    }
    foreach ($root in $searchRoots) {
        if (-not (Test-Path $root)) { continue }
        $dirs = Get-ChildItem -Path $root -Directory -Filter "Python*" -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending
        foreach ($d in $dirs) {
            $exe = Join-Path $d.FullName "python.exe"
            if (Test-Path $exe) {
                return $exe
            }
        }
    }

    return $null
}

function Stop-AllShortcutHub {
    Get-Process -Name "shortcut-hub" -ErrorAction SilentlyContinue |
        Stop-Process -Force -ErrorAction SilentlyContinue

    $ahkProcs = Get-CimInstance Win32_Process -Filter "Name='AutoHotkey64.exe'" -ErrorAction SilentlyContinue
    foreach ($proc in $ahkProcs) {
        if ($proc.CommandLine -and $proc.CommandLine -like "*shortcut_hub.ahk*") {
            Stop-Process -Id $proc.ProcessId -Force -ErrorAction SilentlyContinue
        }
    }
}

if ($Uninstall) {
    Stop-AllShortcutHub
    if (Test-Path $ShortcutPath) {
        Remove-Item $ShortcutPath -Force
        Write-Host "Removed Startup shortcut: $ShortcutPath"
    }
    Write-Host "Uninstall complete."
    exit 0
}

if (-not (Test-Path $NativeExe)) {
    $cargo = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if ($cargo -and (Test-Path (Join-Path $NativeDir "Cargo.toml"))) {
        Write-Host "Building native binary with cargo..."
        Push-Location $NativeDir
        try {
            & $cargo.Source build --release
            if ($LASTEXITCODE -ne 0) {
                throw "cargo build --release failed with exit code $LASTEXITCODE"
            }
        } finally {
            Pop-Location
        }
        $built = Join-Path $NativeDir "target\release\shortcut-hub.exe"
        if (-not (Test-Path $built)) {
            throw "Build completed but expected binary was not found at $built"
        }
        Copy-Item -Path $built -Destination $NativeExe -Force
    } else {
        throw "Missing shortcut-hub.exe at $NativeExe. Download a release binary from GitHub Releases or install Rust (https://rustup.rs) and run 'cargo build --release' in native/."
    }
}

Stop-AllShortcutHub

$PythonExe = Find-PythonExe
if ($PythonExe -and (Test-Path $StoreScript)) {
    Write-Host "Running shortcut discovery & PowerToys sync..."
    & $PythonExe $StoreScript sync
    if ($LASTEXITCODE -ne 0) {
        throw "store.py sync failed with exit code $LASTEXITCODE"
    }
} else {
    Write-Host "Python not detected; Shortcut Hub will run standalone with its built-in Windows catalog and manual editor."
}

$wsh = New-Object -ComObject WScript.Shell
$sc = $wsh.CreateShortcut($ShortcutPath)
$sc.TargetPath = $NativeExe
$sc.Arguments = "--hidden"
$sc.WorkingDirectory = $ProjectDir
$sc.Description = "Shortcut Hub Native Overlay (Ctrl+Alt+K / Win+F7)"
$sc.Save()
Write-Host "Created Startup shortcut: $ShortcutPath -> $NativeExe --hidden"

if (-not $SkipLaunch) {
    Start-Process -FilePath $NativeExe -WorkingDirectory $ProjectDir
    Write-Host "Launched shortcut-hub.exe on active desktop. Press Ctrl+Alt+K or Win+F7 to toggle."
}
