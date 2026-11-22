# Installs Conductor as a Windows service. Run in PowerShell as administrator, from the unzipped release folder:
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Music "D:\Music"
# Without questions (e.g. scripted): -Admin NAME with the password in the CONDUCTOR_PASSWORD environment variable,
# or -SkipAdmin when you already have an admin user.
# Puts conductor.exe (and ffmpeg.exe, if it's in this folder) into C:\Program Files\Conductor, writes
# C:\ProgramData\conductor\config.toml (kept if it exists), installs and starts the service, and opens port 4533
# for the local network only if you pass -OpenFirewall. The data folder is readable only by SYSTEM (the service) and
# administrators: it holds secrets.toml, secret.key and conductor.log (with the one-time /setup link), so run
# `conductor setup` and `conductor user …` in an administrator window. An older install from when Conductor was
# called Maestro is moved over: its service removed, its data folder (C:\ProgramData\maestro) and settings kept
# under the new name.
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
$dir = Join-Path $env:ProgramFiles "Conductor"
$data = Join-Path $env:ProgramData "conductor"
# Conductor was called Maestro: move an older install over
$oldName = "Maes" + "tro"
if (Get-Service -Name $oldName -ErrorAction SilentlyContinue) {
  Stop-Service $oldName -ErrorAction SilentlyContinue
  sc.exe delete $oldName | Out-Null
  Write-Host "Removed the old $oldName service."
}
$oldData = Join-Path $env:ProgramData $oldName.ToLower()
if ((Test-Path $oldData) -and -not (Test-Path $data)) {
  Move-Item $oldData $data
  $oldCfg = Join-Path $data "config.toml"
  if (Test-Path $oldCfg) {
    $t = (Get-Content $oldCfg -Raw).Replace("ProgramData\\$($oldName.ToLower())", 'ProgramData\\conductor').Replace("Program Files\\$oldName", 'Program Files\\Conductor')
    Set-Content -Encoding UTF8 -NoNewline -Path $oldCfg -Value $t
  }
  Write-Host "Moved your data from $oldData to $data (the database is renamed when Conductor starts)."
}
$oldRule = Get-NetFirewallRule -DisplayName "$oldName (music server)" -ErrorAction SilentlyContinue
if ($oldRule) { $oldRule | Remove-NetFirewallRule; $OpenFirewall = $true }
$oldDir = Join-Path $env:ProgramFiles $oldName
if (Test-Path $oldDir) { Remove-Item -Recurse -Force $oldDir -ErrorAction SilentlyContinue }
New-Item -ItemType Directory -Force -Path $dir, $data | Out-Null
# Only SYSTEM and administrators (well-known SIDs, so any Windows language works); what's in it inherits that
icacls $data /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" | Out-Null
if ($LASTEXITCODE -ne 0) { throw "couldn't make $data private" }
if (Get-ChildItem -Force $data) { icacls "$data\*" /reset /T /C /Q | Out-Null }
$svc = Get-Service -Name Conductor -ErrorAction SilentlyContinue
if ($svc) { & (Join-Path $dir "conductor.exe") service stop 2>$null; Start-Sleep 2 }
Copy-Item (Join-Path $here "conductor.exe") $dir -Force
if (Test-Path (Join-Path $here "ffmpeg.exe")) { Copy-Item (Join-Path $here "ffmpeg.exe") $dir -Force }
$cfg = Join-Path $data "config.toml"
if (-not (Test-Path $cfg)) {
  $musicToml = $Music.Replace('\', '\\')
  $dataToml = $data.Replace('\', '\\')
  # only an ffmpeg.exe that's there: else the one on PATH is used
  $ffmpegExe = Join-Path $dir "ffmpeg.exe"
  $ffmpegLine = if (Test-Path $ffmpegExe) { "ffmpeg = ""$($ffmpegExe.Replace('\', '\\'))""" } else { "" }
  @"
data_dir = "$dataToml"
listen = "$Listen"
$ffmpegLine

[[libraries]]
name = "Music"
path = "$musicToml"
scan_threads = 4
"@ | Set-Content -Encoding UTF8 $cfg
  Write-Host "Wrote $cfg (edit it for slskd, Lidarr, ListenBrainz…; see config.example.toml)"
}
$exe = Join-Path $dir "conductor.exe"
if (-not $svc) {
  & $exe --config $cfg service install
  if ($LASTEXITCODE -ne 0) { throw "couldn't install the service" }
}
try {
  # an admin that's there already (an upgrade) is left as it is
  if ($Admin -and $env:CONDUCTOR_PASSWORD) {
    & $exe --config $cfg user add $Admin --admin --if-missing
    if ($LASTEXITCODE -ne 0) { Write-Warning "couldn't add the admin user $Admin" }
  } elseif (-not $SkipAdmin) {
    $name = if ($Admin) { $Admin } else { Read-Host "Name for the first admin user (Enter to skip if you already have one)" }
    if ($name) {
      $pw = Read-Host "Password for $name" -AsSecureString
      $env:CONDUCTOR_PASSWORD = [Runtime.InteropServices.Marshal]::PtrToStringAuto([Runtime.InteropServices.Marshal]::SecureStringToBSTR($pw))
      & $exe --config $cfg user add $name --admin --if-missing
      if ($LASTEXITCODE -ne 0) { Write-Warning "couldn't add the admin user $name" }
      Remove-Item Env:\CONDUCTOR_PASSWORD
    }
  }
} finally {
  # started again whatever happened above (an upgrade stopped it)
  & $exe service start
}
if ($LASTEXITCODE -ne 0) { throw "the service didn't start; see $data\conductor.log" }
if ($OpenFirewall) {
  New-NetFirewallRule -DisplayName "Conductor (music server)" -Direction Inbound -Protocol TCP -LocalPort ($Listen.Split(':')[-1]) -Profile Private -Action Allow | Out-Null
  Write-Host "Opened the port for private networks."
}
Write-Host "Conductor is running: http://localhost:$($Listen.Split(':')[-1])/admin"
