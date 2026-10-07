# Install lyra-node on Windows: let lyra work on this machine.
#   irm __LYRA_URL__/install.ps1 | iex
#   & ([scriptblock]::Create((irm __LYRA_URL__/install.ps1))) -Name office-pc
# Run in PowerShell as administrator. Downloads lyra-node.exe, checks it
# against the server's checksum, installs it as the "lyra node" service in
# C:\Program Files\lyra (settings in C:\ProgramData\lyra), and asks lyra to
# pair: approve the request in the lyra app (it shows a code to compare).
param([string]$Name = $env:COMPUTERNAME.ToLower())
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Url = '__LYRA_URL__'
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host 'lyra-node: run this in PowerShell as administrator (right-click > Run as administrator).' -ForegroundColor Red
    return
}
if (-not [Environment]::Is64BitOperatingSystem) {
    Write-Host 'lyra-node: only 64-bit Windows is supported.' -ForegroundColor Red
    return
}
$tmp = Join-Path $env:TEMP 'lyra-node-setup.exe'
Write-Host "downloading lyra-node from $Url"
Invoke-WebRequest "$Url/download/lyra-node.exe" -OutFile $tmp -UseBasicParsing
$want = ((Invoke-WebRequest "$Url/download/lyra-node.exe.sha256" -UseBasicParsing).Content -split '\s+')[0].Trim()
$got = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
if ($want -ne $got) {
    Remove-Item $tmp -Force
    Write-Host "lyra-node: the download doesn't match the server's checksum; not installing." -ForegroundColor Red
    return
}
& $tmp install --url $Url --name $Name
$code = $LASTEXITCODE
Remove-Item $tmp -Force -ErrorAction SilentlyContinue
if ($code -eq 0) {
    Write-Host "lyra-node is running as the 'lyra node' service. Manage it from the lyra app (Machines)." -ForegroundColor Green
}
