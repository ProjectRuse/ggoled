<#
.SYNOPSIS
  Sets the Scarlett 2i2 4th Gen front-panel LED sleep time from Windows by
  briefly handing the device to WSL and running scarlett_fp.py there.

.DESCRIPTION
  Audio through the Scarlett drops out for a few seconds while this runs.
  Keep scarlett_fp.py in the same folder as this script.
  Run from an elevated (Administrator) PowerShell.

.EXAMPLE
  .\Reset-ScarlettLEDs.ps1 -FromFile  # value written by ggoled (used by the scheduled task)
  .\Reset-ScarlettLEDs.ps1            # 5 seconds (default)
  .\Reset-ScarlettLEDs.ps1 -Seconds 30
  .\Reset-ScarlettLEDs.ps1 -Status    # just read the current values
#>
param(
    [ValidateRange(0, 86400)][int]$Seconds = 5,
    [switch]$Status,
    [switch]$FromFile,              # read seconds from %APPDATA%\ggoled_scarlett_led.txt
    [string]$Distro = ""            # blank = your default WSL distro
)

# Windows PowerShell 5.1 turns anything a native tool writes to stderr into an
# error, and usbipd writes routine "info:" lines there. So don't stop on
# errors; check exit codes explicitly instead.
$ErrorActionPreference = "Continue"
$HwId = "1235:8219"                 # Scarlett 2i2 4th Gen

function Fail($msg) { Write-Host "ERROR: $msg" -ForegroundColor Red; exit 1 }

if ($FromFile) {
    # Running hidden from the scheduled task: keep a log for troubleshooting
    $log = Join-Path $env:APPDATA "ggoled_scarlett.log"
    if ((Test-Path $log) -and (Get-Item $log).Length -gt 256KB) { Remove-Item $log }
    Start-Transcript -Path $log -Append | Out-Null
    $valueFile = Join-Path $env:APPDATA "ggoled_scarlett_led.txt"
    if (-not (Test-Path $valueFile)) { Fail "$valueFile not found (ggoled writes it)." }
    $raw = (Get-Content $valueFile -Raw).Trim()
    if ($raw -notmatch '^\d+$' -or [int]$raw -gt 86400) { Fail "Bad value in ${valueFile}: '$raw'" }
    $Seconds = [int]$raw
    Write-Host "$(Get-Date -Format s) Setting Scarlett LED timeout to $Seconds s"
}

# --- Preconditions -------------------------------------------------------
$isAdmin = ([Security.Principal.WindowsPrincipal] `
    [Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Fail "Run this from an Administrator PowerShell." }

if (-not (Get-Command usbipd -ErrorAction SilentlyContinue)) {
    Fail "usbipd isn't installed. Run: winget install usbipd  (then reopen PowerShell)"
}

$script = Join-Path $PSScriptRoot "scarlett_fp.py"
if (-not (Test-Path $script)) { Fail "scarlett_fp.py not found next to this script." }

$wslArgs = @()
if ($Distro) { $wslArgs = @("-d", $Distro) }

# Convert the Windows path to a WSL path (C:\x\y -> /mnt/c/x/y)
$winPath = $script -replace '\\', '/'
$wslPath = (& wsl @wslArgs -e wslpath -a "$winPath").Trim()
if (-not $wslPath) { Fail "Couldn't reach WSL. Check 'wsl -l -v'." }

# --- Find the Scarlett ---------------------------------------------------
$line = usbipd list | Select-String -SimpleMatch $HwId | Select-Object -First 1
if (-not $line) { Fail "Scarlett ($HwId) not found in 'usbipd list'." }
$busId = ($line.Line.Trim() -split '\s+')[0]
Write-Host "Scarlett found on bus $busId"

# Sharing ("bind") is a one-time, persistent setting. Windows can still use a
# shared device normally whenever it isn't attached to WSL.
if ($line.Line -match 'Not shared') {
    Write-Host "Sharing Scarlett with WSL (one-time)..."
    usbipd bind --busid $busId 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "usbipd bind failed." }
}

$attached = $false
try {
    Write-Host "Handing Scarlett to WSL (audio will drop briefly)..."
    usbipd attach --wsl --busid $busId 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail "usbipd attach failed. Try running it by hand to see the error: usbipd attach --wsl --busid $busId" }
    $attached = $true

    # Wait for the device to appear inside WSL
    $seen = $false
    for ($i = 0; $i -lt 20; $i++) {
        Start-Sleep -Milliseconds 500
        $found = & wsl @wslArgs -u root -- sh -c "grep -l 8219 /sys/bus/usb/devices/*/idProduct 2>/dev/null"
        if ($found) { $seen = $true; break }
    }
    if (-not $seen) { Fail "Scarlett didn't show up inside WSL." }

    if ($Status) {
        & wsl @wslArgs -u root -- python3 $wslPath status
    } else {
        & wsl @wslArgs -u root -- python3 $wslPath sleep $Seconds
        # ggoled may have asked for a different value while this ran (the task
        # ignores a second start), so apply again if the file changed meanwhile.
        for ($i = 0; $FromFile -and $i -lt 3; $i++) {
            $latest = (Get-Content $valueFile -Raw).Trim()
            if ($latest -notmatch '^\d+$' -or [int]$latest -eq $Seconds -or [int]$latest -gt 86400) { break }
            $Seconds = [int]$latest
            Write-Host "$(Get-Date -Format s) Value changed meanwhile; setting $Seconds s"
            & wsl @wslArgs -u root -- python3 $wslPath sleep $Seconds
        }
    }
    if ($LASTEXITCODE -ne 0) { Write-Host "scarlett_fp.py reported an error (see above)." -ForegroundColor Yellow }
}
finally {
    if ($attached) {
        Write-Host "Returning Scarlett to Windows..."
        usbipd detach --busid $busId 2>&1 | Out-Null
        Write-Host "Done. Audio should return in a few seconds."
    }
}
