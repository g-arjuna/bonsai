param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
)

$ErrorActionPreference = "Stop"

$pidFile = Join-Path $RepoRoot "runtime\bonsai-windows.pid"

if (Test-Path $pidFile) {
    $pidText = (Get-Content -Path $pidFile -Raw).Trim()
    if ($pidText) {
        $proc = Get-Process -Id ([int]$pidText) -ErrorAction SilentlyContinue
        if ($proc -and $proc.ProcessName -eq "bonsai") {
            Stop-Process -Id $proc.Id -Force
            Write-Host "Stopped Bonsai PID=$($proc.Id)"
            Remove-Item -Path $pidFile -Force
            return
        }
    }
}

$processes = Get-Process -Name bonsai -ErrorAction SilentlyContinue
if (-not $processes) {
    Write-Host "No Bonsai process found."
    return
}

$processes | ForEach-Object {
    Stop-Process -Id $_.Id -Force
    Write-Host "Stopped Bonsai PID=$($_.Id)"
}

if (Test-Path $pidFile) {
    Remove-Item -Path $pidFile -Force
}
