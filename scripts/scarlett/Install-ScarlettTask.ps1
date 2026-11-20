<#
.SYNOPSIS
  One-time setup: registers the scheduled task ggoled uses to change the
  Scarlett's LED timeout without an admin prompt.

.DESCRIPTION
  Creates a Task Scheduler task (no triggers) that runs
  Reset-ScarlettLEDs.ps1 -FromFile with highest privileges, as you.
  ggoled starts it with: schtasks /run /tn "ggoled Scarlett LEDs"

  Run from an elevated (Administrator) PowerShell, from this folder.
  Re-run it if you move this folder. Remove with:
    Unregister-ScheduledTask -TaskName "ggoled Scarlett LEDs"
#>
#Requires -RunAsAdministrator
param([string]$TaskName = "ggoled Scarlett LEDs")

$script = Join-Path $PSScriptRoot "Reset-ScarlettLEDs.ps1"
if (-not (Test-Path $script)) { throw "Reset-ScarlettLEDs.ps1 not found next to this script." }

# conhost --headless keeps PowerShell from flashing a window
$argList = "--headless powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$script`" -FromFile"
$action = New-ScheduledTaskAction -Execute "conhost.exe" -Argument $argList -WorkingDirectory $PSScriptRoot
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit (New-TimeSpan -Minutes 2) -MultipleInstances IgnoreNew

Register-ScheduledTask -TaskName $TaskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
Write-Host "Registered task '$TaskName'."
Write-Host "Test it (no admin needed):  schtasks /run /tn `"$TaskName`""
Write-Host "Log: $env:APPDATA\ggoled_scarlett.log"
