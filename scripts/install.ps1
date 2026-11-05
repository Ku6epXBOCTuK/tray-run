$ErrorActionPreference = 'Stop'

$source = Join-Path $PSScriptRoot '..\target\release\tray-run.exe'
$dest = Join-Path (Join-Path $HOME 'bin') 'tray-run.exe'

$running = @(Get-CimInstance Win32_Process -Filter "Name = 'tray-run.exe'")
$arguments = ''
if ($running.Count -gt 0) {
    $arguments = ($running[0].CommandLine -replace '^\s*("[^"]*"|\S+)', '').Trim()
    Write-Host "stopping tray-run (pid $($running[0].ProcessId))"
    Get-Process tray-run -ErrorAction SilentlyContinue | Stop-Process -Force

    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Process tray-run -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
}

for ($attempt = 1; $attempt -le 20; $attempt++) {
    try {
        Copy-Item -Path $source -Destination $dest -Force
        break
    }
    catch {
        if ($attempt -eq 20) { throw }
        Start-Sleep -Milliseconds 250
    }
}
Write-Host "installed $dest"

if ($running.Count -gt 0) {
    if ($arguments) {
        Start-Process -FilePath $dest -ArgumentList $arguments
    }
    else {
        Start-Process -FilePath $dest
    }
    Write-Host 'restarted tray-run'
}
