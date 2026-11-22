# Starts the Rust API in this window, waits until it is healthy,
# then opens the Vite UI in a NEW window. Ctrl+C stops the backend.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo was not found. Install Rust, then reopen the terminal."
}
$npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
if (-not $npm) {
    throw "npm was not found. Install Node.js, then reopen the terminal."
}
if (-not (Test-Path "web\node_modules")) {
    Write-Host "Installing frontend packages..."
    & $npm.Source install --prefix web
}

Write-Host ""
Write-Host "API  http://127.0.0.1:8787"
Write-Host "UI   http://127.0.0.1:5173"
Write-Host "Starting backend first; the UI opens in a new window once the API is healthy."
Write-Host ""

$backend = Start-Process -FilePath "cargo" `
    -ArgumentList @("run", "--bin", "rust-job-agent") `
    -WorkingDirectory $PSScriptRoot `
    -PassThru -NoNewWindow

function Stop-Backend {
    if ($backend -and -not $backend.HasExited) {
        & taskkill.exe /PID $backend.Id /T /F 2>$null | Out-Null
    }
}

try {
    $deadline = (Get-Date).AddSeconds(180)
    $healthy = $false
    while ((Get-Date) -lt $deadline) {
        if ($backend.HasExited) {
            throw "Backend exited before becoming healthy. See the error above."
        }
        try {
            $response = Invoke-WebRequest -Uri "http://127.0.0.1:8787/api/health" -TimeoutSec 3 -UseBasicParsing
            if ($response.StatusCode -eq 200) { $healthy = $true; break }
        } catch {
            Start-Sleep -Seconds 1
        }
    }
    if (-not $healthy) {
        throw "Backend never became healthy at http://127.0.0.1:8787/api/health."
    }
    Write-Host "Backend is up. Opening UI in a new window..."
    $webDir = Join-Path $PSScriptRoot "web"
    Start-Process -FilePath $npm.Source `
        -ArgumentList @("run", "dev", "--", "--host", "127.0.0.1", "--port", "5173", "--strictPort") `
        -WorkingDirectory $webDir
    Write-Host "UI   http://127.0.0.1:5173 (new window; close it separately)"
    Write-Host "The UI proxies /api to this backend. Ctrl+C stops the backend."
    while (-not $backend.HasExited) {
        Start-Sleep -Milliseconds 400
    }
} finally {
    Stop-Backend
}
