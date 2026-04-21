param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [switch]$SkipWsl
)

$ErrorActionPreference = "Continue"

function Test-CommandLine {
    param(
        [string]$Name,
        [scriptblock]$Command
    )

    Write-Host "`n== $Name =="
    try {
        & $Command
        if ($LASTEXITCODE -ne $null -and $LASTEXITCODE -ne 0) {
            Write-Host "FAILED exit=$LASTEXITCODE"
        }
    }
    catch {
        Write-Host "FAILED $($_.Exception.Message)"
    }
}

Push-Location $RepoRoot
try {
    Test-CommandLine "Windows Python" {
        & "C:\Users\arjun\AppData\Local\Programs\Python\Python313\python.exe" --version
        & "C:\Users\arjun\AppData\Local\Programs\Python\Python313\python.exe" -m grpc_tools.protoc --version
    }

    Test-CommandLine "ripgrep real binary" {
        & "C:\ProgramData\chocolatey\lib\ripgrep\tools\ripgrep-14.1.0-x86_64-pc-windows-msvc\rg.exe" --version
    }

    Test-CommandLine "cargo" {
        cargo --version
    }

    Test-CommandLine "Bonsai process" {
        $proc = Get-Process -Name bonsai -ErrorAction SilentlyContinue
        if ($proc) {
            $proc | Select-Object Id, ProcessName, StartTime
        }
        else {
            Write-Host "Not running"
        }
    }

    Test-CommandLine "Bonsai readiness" {
        try {
            $ready = Invoke-WebRequest -Uri "http://127.0.0.1:3000/api/readiness" -UseBasicParsing -TimeoutSec 3
            Write-Host "HTTP readiness status=$($ready.StatusCode)"
        }
        catch {
            Write-Host "HTTP readiness unavailable"
        }
    }

    if (-not $SkipWsl) {
        Test-CommandLine "WSL Python and lab tools" {
            wsl.exe bash -lc "cd /mnt/c/Users/arjun/Desktop/bonsai && python3 --version && .venv/bin/python --version && command -v clab || true"
        }
    }
}
finally {
    Pop-Location
}
