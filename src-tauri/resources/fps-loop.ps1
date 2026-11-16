# Legends Island: помощник FPS. Работает от SYSTEM (задача LegendsIslandFPS), окна не показывает.
# Пока существует state\fps.on — запускает PresentMon и раз в секунду пишет state\fps.json: {"pid":["имя.exe",fps], ...}
$ErrorActionPreference = 'SilentlyContinue'
$base = 'C:\ProgramData\LegendsIsland'
$pm = Join-Path $base 'bin\PresentMon.exe'
$st = Join-Path $base 'state'
while ($true) {
  if (Test-Path (Join-Path $st 'fps.on')) {
    $script:cnt = @{}; $script:names = @{}; $script:hdr = $null
    $script:sw = [Diagnostics.Stopwatch]::StartNew()
    & $pm --output_stdout --no_console_stats --stop_existing_session --session_name LegendsIslandFPS --timed 30 --terminate_after_timed 2>$null | ForEach-Object {
      if (-not $script:hdr) {
        $script:hdr = $_ -split ','
        $script:ia = [array]::IndexOf($script:hdr, 'Application')
        $script:ip = [array]::IndexOf($script:hdr, 'ProcessID')
        return
      }
      $c = $_ -split ','
      if ($c.Count -le $script:ip) { return }
      $p = $c[$script:ip]
      $script:cnt[$p] = 1 + [int]$script:cnt[$p]
      $script:names[$p] = $c[$script:ia]
      if ($script:sw.ElapsedMilliseconds -ge 1000) {
        $s = $script:sw.ElapsedMilliseconds / 1000
        $parts = foreach ($k in $script:cnt.Keys) { '"{0}":["{1}",{2}]' -f $k, ($script:names[$k] -replace '[\\"]', ''), [math]::Round($script:cnt[$k] / $s) }
        Set-Content -Path (Join-Path $st 'fps.json') -Value ('{' + ($parts -join ',') + '}') -Encoding ASCII
        $script:cnt = @{}; $script:sw.Restart()
        if (-not (Test-Path (Join-Path $st 'fps.on'))) { Get-Process PresentMon* | Stop-Process -Force }
      }
    }
    Remove-Item (Join-Path $st 'fps.json')
  }
  Start-Sleep -Seconds 2
}
