# Removes the Maestro service and program (run as administrator). Your data in C:\ProgramData\maestro stays,
# unless you pass -RemoveData.
param([switch]$RemoveData)
$ErrorActionPreference = "Stop"
$dir = Join-Path $env:ProgramFiles "Maestro"
$exe = Join-Path $dir "maestro.exe"
if (Test-Path $exe) { & $exe service uninstall }
Get-NetFirewallRule -DisplayName "Maestro (music server)" -ErrorAction SilentlyContinue | Remove-NetFirewallRule
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
if ($RemoveData) { Remove-Item -Recurse -Force (Join-Path $env:ProgramData "maestro") }
Write-Host "Maestro removed."
