# Installs Maestro as a Windows service. Run in PowerShell as administrator, from the unzipped release folder:
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Music "D:\Music"
# Without questions (e.g. scripted): -Admin NAME with the password in the MAESTRO_PASSWORD environment variable,
# or -SkipAdmin when you already have an admin user.
# Puts maestro.exe (and ffmpeg.exe, if it's in this folder) into C:\Program Files\Maestro, writes
# C:\ProgramData\maestro\config.toml (kept if it exists), installs and starts the service, and opens port 4533
# for the local network only if you pass -OpenFirewall.
param(
  [Parameter(Mandatory = $true)][string]$Music,
  [string]$Listen = "0.0.0.0:4533",
  [string]$Admin,
  [switch]$SkipAdmin,
  [switch]$OpenFirewall
)
$ErrorActionPreference = "Stop"
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw "Run this in PowerShell as administrator."
}
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dir = Join-Path $env:ProgramFiles "Maestro"
$data = Join-Path $env:ProgramData "maestro"
New-Item -ItemType Directory -Force -Path $dir, $data | Out-Null
$svc = Get-Service -Name Maestro -ErrorAction SilentlyContinue
if ($svc) { & (Join-Path $dir "maestro.exe") service stop 2>$null; Start-Sleep 2 }
Copy-Item (Join-Path $here "maestro.exe") $dir -Force
if (Test-Path (Join-Path $here "ffmpeg.exe")) { Copy-Item (Join-Path $here "ffmpeg.exe") $dir -Force }
$cfg = Join-Path $data "config.toml"
if (-not (Test-Path $cfg)) {
  $musicToml = $Music.Replace('\', '\\')
  $dataToml = $data.Replace('\', '\\')
  $ffmpegToml = (Join-Path $dir "ffmpeg.exe").Replace('\', '\\')
  @"
data_dir = "$dataToml"
listen = "$Listen"
ffmpeg = "$ffmpegToml"

[[libraries]]
name = "Music"
path = "$musicToml"
scan_threads = 4
"@ | Set-Content -Encoding UTF8 $cfg
  Write-Host "Wrote $cfg (edit it for slskd, Lidarr, ListenBrainz…; see config.example.toml)"
}
$exe = Join-Path $dir "maestro.exe"
if (-not $svc) { & $exe --config $cfg service install }
if ($Admin -and $env:MAESTRO_PASSWORD) {
  & $exe --config $cfg user add $Admin --admin
  if ($LASTEXITCODE -ne 0) { throw "couldn't add the admin user" }
} elseif (-not $SkipAdmin) {
  $name = if ($Admin) { $Admin } else { Read-Host "Name for the first admin user (Enter to skip if you already have one)" }
  if ($name) {
    $pw = Read-Host "Password for $name" -AsSecureString
    $env:MAESTRO_PASSWORD = [Runtime.InteropServices.Marshal]::PtrToStringAuto([Runtime.InteropServices.Marshal]::SecureStringToBSTR($pw))
    & $exe --config $cfg user add $name --admin
    Remove-Item Env:\MAESTRO_PASSWORD
  }
}
& $exe service start
if ($LASTEXITCODE -ne 0) { throw "the service didn't start; see $data\maestro.log" }
if ($OpenFirewall) {
  New-NetFirewallRule -DisplayName "Maestro (music server)" -Direction Inbound -Protocol TCP -LocalPort ($Listen.Split(':')[-1]) -Profile Private -Action Allow | Out-Null
  Write-Host "Opened the port for private networks."
}
Write-Host "Maestro is running: http://localhost:$($Listen.Split(':')[-1])/admin"
