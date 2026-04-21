param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [int]$ReadinessTimeoutSeconds = 45
)

$ErrorActionPreference = "Stop"

$exe = Join-Path $RepoRoot "target\release\bonsai.exe"
$runtimeDir = Join-Path $RepoRoot "runtime"
$pidFile = Join-Path $runtimeDir "bonsai-windows.pid"
$stdoutLog = Join-Path $runtimeDir "bonsai-windows.out.log"
$stderrLog = Join-Path $runtimeDir "bonsai-windows.err.log"

if (-not (Test-Path $exe)) {
    throw "Missing $exe. Run 'cargo build --release' from Windows PowerShell first."
}

New-Item -ItemType Directory -Force -Path $runtimeDir | Out-Null

# Some shells expose both Path and PATH. Windows treats env var names as
# case-insensitive, but Start-Process can still trip over the duplicate while
# building its launch dictionary. Normalize before starting Bonsai.
$pathValue = [Environment]::GetEnvironmentVariable("Path", "Process")
if (-not $pathValue) {
    $pathValue = [Environment]::GetEnvironmentVariable("PATH", "Process")
}
if ($pathValue) {
    [Environment]::SetEnvironmentVariable("PATH", $null, "Process")
    [Environment]::SetEnvironmentVariable("Path", $pathValue, "Process")
}

try {
    $ready = Invoke-WebRequest -Uri "http://127.0.0.1:3000/api/readiness" -UseBasicParsing -TimeoutSec 3
    if ($ready.StatusCode -eq 200) {
        Write-Host "Bonsai already appears ready at http://127.0.0.1:3000/api/readiness"
        return
    }
}
catch {
    # Not running yet, continue with start.
}

$proc = Start-Process `
    -FilePath $exe `
    -WorkingDirectory $RepoRoot `
    -RedirectStandardOutput $stdoutLog `
    -RedirectStandardError $stderrLog `
    -PassThru

Set-Content -Path $pidFile -Value $proc.Id

for ($i = 0; $i -lt $ReadinessTimeoutSeconds; $i++) {
    Start-Sleep -Seconds 1
    if ($proc.HasExited) {
        throw "Bonsai exited early with code $($proc.ExitCode). See $stdoutLog and $stderrLog."
    }

    try {
        $ready = Invoke-WebRequest -Uri "http://127.0.0.1:3000/api/readiness" -UseBasicParsing -TimeoutSec 3
        if ($ready.StatusCode -eq 200) {
            Write-Host "Bonsai started. PID=$($proc.Id)"
            Write-Host "Readiness: http://127.0.0.1:3000/api/readiness"
            Write-Host "Logs: $stdoutLog ; $stderrLog"
            return
        }
    }
    catch {
        # Keep waiting until timeout.
    }
}

throw "Bonsai did not become ready within $ReadinessTimeoutSeconds seconds. PID=$($proc.Id). See $stdoutLog and $stderrLog."
