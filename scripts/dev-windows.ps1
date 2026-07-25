$ErrorActionPreference = "Continue"
$root = (Get-Location).ProviderPath
$port = 1940

Write-Host "Starting backend (cargo run)..."
$b = Start-Process -FilePath "cargo" `
  -ArgumentList @("run", "--bin", "marionette") `
  -WorkingDirectory $root `
  -NoNewWindow `
  -PassThru

$ready = $false
$deadline = (Get-Date).AddMinutes(5)
Write-Host "Waiting for backend on 127.0.0.1:$port ..."

while ((Get-Date) -lt $deadline) {
  if ($b.HasExited) {
    Write-Host "Backend process exited early (exit $($b.ExitCode))." -ForegroundColor Red
    exit 1
  }
  try {
    $c = New-Object System.Net.Sockets.TcpClient
    $iar = $c.BeginConnect("127.0.0.1", $port, $null, $null)
    if ($iar.AsyncWaitHandle.WaitOne(400, $false) -and $c.Connected) {
      $c.EndConnect($iar)
      $c.Close()
      $ready = $true
      break
    }
    try { $c.Close() } catch {}
  } catch {}
  Start-Sleep -Milliseconds 400
}

if (-not $ready) {
  Write-Host "Timed out waiting for backend on :$port" -ForegroundColor Red
  if (-not $b.HasExited) {
    Stop-Process -Id $b.Id -Force -ErrorAction SilentlyContinue
  }
  exit 1
}

Write-Host "Backend ready. Starting Vite on :1941 ..."
try {
  Set-Location (Join-Path $root "web")
  & npm.cmd run dev
} finally {
  if ($b -and -not $b.HasExited) {
    Write-Host "Stopping backend (pid $($b.Id))..."
    Stop-Process -Id $b.Id -Force -ErrorAction SilentlyContinue
  }
}
