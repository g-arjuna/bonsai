param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
)

$ErrorActionPreference = "Stop"

function Resolve-BonsaiPython {
    $candidates = @(
        (Join-Path $RepoRoot ".venv\Scripts\python.exe"),
        "C:\Users\arjun\AppData\Local\Programs\Python\Python313\python.exe",
        "python3",
        "python"
    )

    foreach ($candidate in $candidates) {
        if ($candidate -match "\\|/") {
            if (Test-Path $candidate) {
                return $candidate
            }
            continue
        }

        $cmd = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($cmd) {
            return $cmd.Source
        }
    }

    throw "No usable Python interpreter found. Install Python or create .venv first."
}

$python = Resolve-BonsaiPython

Push-Location $RepoRoot
try {
    & $python -m grpc_tools.protoc `
        -I proto `
        --python_out=python\generated `
        --grpc_python_out=python\generated `
        proto\bonsai_service.proto
    if ($LASTEXITCODE -ne 0) {
        throw "grpc_tools.protoc failed with exit code $LASTEXITCODE"
    }
    Write-Host "Regenerated Python gRPC stubs using $python"
}
finally {
    Pop-Location
}
