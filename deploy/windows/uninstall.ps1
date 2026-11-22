# Removes the Conductor service and program (run as administrator). Your data in C:\ProgramData\conductor stays,
# unless you pass -RemoveData.
param([switch]$RemoveData)
$ErrorActionPreference = "Stop"
$dir = Join-Path $env:ProgramFiles "Conductor"
$exe = Join-Path $dir "conductor.exe"
if (Test-Path $exe) { & $exe service uninstall }
Get-NetFirewallRule -DisplayName "Conductor (music server)" -ErrorAction SilentlyContinue | Remove-NetFirewallRule
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
if ($RemoveData) { Remove-Item -Recurse -Force (Join-Path $env:ProgramData "conductor") }
Write-Host "Conductor removed."
