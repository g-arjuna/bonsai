# set-wsl-route.ps1
# Adds (or refreshes) a persistent route from Windows to the bonsai
# ContainerLab management subnet (172.100.100.0/24) via WSL2.
#
# Run this once after each WSL2 restart (WSL2 assigns a new internal
# IP on every restart, which invalidates any previous route entry).
# Run as Administrator.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\set-wsl-route.ps1

$mgmtSubnet  = "172.100.100.0"
$mgmtMask    = "255.255.255.0"

# Resolve current WSL2 internal IP from Windows side
$wslIp = (wsl hostname -I 2>$null).Trim().Split()[0]

if (-not $wslIp) {
    Write-Error "Could not resolve WSL2 IP. Is WSL2 running?"
    exit 1
}

Write-Host "WSL2 IP: $wslIp"

# Remove stale entry if present (ignore errors if not found)
route delete $mgmtSubnet 2>$null | Out-Null

# Add persistent route (-p survives Windows reboots)
$result = route -p add $mgmtSubnet mask $mgmtMask $wslIp
Write-Host "Route result: $result"
Write-Host ""
Write-Host "Bonsai lab management network ($mgmtSubnet/24) is now reachable from Windows."
Write-Host "gNMI targets:"
Write-Host "  srl1  172.100.100.11:57400"
Write-Host "  srl2  172.100.100.12:57400"
Write-Host "  srl3  172.100.100.13:57400"
