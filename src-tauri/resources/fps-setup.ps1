# Legends Island: однократная настройка FPS (запускается с правами администратора).
param([string]$src)
$ErrorActionPreference = 'Stop'
$base = 'C:\ProgramData\LegendsIsland'; $bin = Join-Path $base 'bin'; $st = Join-Path $base 'state'
try {
  New-Item -ItemType Directory -Force -Path $bin, $st | Out-Null
  Get-ScheduledTask -TaskName 'LegendsIslandFPS' -ErrorAction SilentlyContinue | Stop-ScheduledTask -ErrorAction SilentlyContinue
  Get-Process PresentMon* -ErrorAction SilentlyContinue | Stop-Process -Force
  Copy-Item (Join-Path $src 'PresentMon.exe'), (Join-Path $src 'fps-loop.ps1') $bin -Force
  # bin: менять может только администратор и система; state: пользователи могут включать/выключать
  icacls $bin /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
  icacls $st /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)M' | Out-Null
  $a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $bin 'fps-loop.ps1') + '"')
  $t = New-ScheduledTaskTrigger -AtStartup
  $p = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
  $s = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
  Register-ScheduledTask -TaskName 'LegendsIslandFPS' -Action $a -Trigger $t -Principal $p -Settings $s -Force | Out-Null
  Start-ScheduledTask -TaskName 'LegendsIslandFPS'
  Set-Content (Join-Path $st 'setup.ok') 'ok'
} catch {
  New-Item -ItemType Directory -Force -Path $st | Out-Null
  Set-Content (Join-Path $st 'setup.err') $_.Exception.Message
}
