param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Pattern,

    [Parameter(Position = 1, ValueFromRemainingArguments = $true)]
    [string[]]$SearchPath
)

$ErrorActionPreference = "Stop"

function Resolve-BonsaiRipgrep {
    $candidates = @(
        "C:\ProgramData\chocolatey\lib\ripgrep\tools\ripgrep-14.1.0-x86_64-pc-windows-msvc\rg.exe",
        "C:\Program Files\WindowsApps\OpenAI.Codex_26.415.1938.0_x64__2p2nqsd0c76g0\app\resources\rg.exe"
    )

    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            return $candidate
        }
    }

    $cmd = Get-Command rg -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    throw "ripgrep was not found. Install ripgrep or update scripts/search_repo.ps1."
}

$rg = Resolve-BonsaiRipgrep
if (-not $SearchPath -or $SearchPath.Count -eq 0) {
    $SearchPath = @(".")
}

& $rg -n --hidden --glob "!.git" --glob "!target" --glob "!.venv" --glob "!python/.pytest_cache" -- $Pattern @SearchPath
