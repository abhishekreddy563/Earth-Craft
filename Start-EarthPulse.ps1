param([switch]$BackendOnly)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$python = Join-Path $PSScriptRoot '.venv/Scripts/python.exe'
$earthCraftRoot = Join-Path $PSScriptRoot 'EarthCraft'
if (-not (Test-Path (Join-Path $earthCraftRoot 'Cargo.toml'))) {
    $earthCraftRoot = $PSScriptRoot
}
$desktop = Join-Path $earthCraftRoot 'target/debug/earthcraft.exe'
if (-not (Test-Path $python)) { throw 'Create .venv and install requirements.txt first.' }
if (!$BackendOnly -and !(Test-Path $desktop)) { throw 'Build the desktop app with Build-EarthPulse.ps1 first.' }
if (!$env:EARTH_PULSE_TOKEN) {
    $env:EARTH_PULSE_TOKEN = (& $python -c 'import secrets; print(secrets.token_urlsafe(32))')
}
if ($null -eq $env:OLLAMA_CPU_ONLY) { $env:OLLAMA_CPU_ONLY = '1' }
if ($BackendOnly) {
    & $python -m uvicorn backend.server:app --host 127.0.0.1 --port 8765 --no-access-log
    exit $LASTEXITCODE
}
$service = Start-Process -FilePath $python -ArgumentList '-m','uvicorn','backend.server:app','--host','127.0.0.1','--port','8765','--no-access-log' -WorkingDirectory $PSScriptRoot -WindowStyle Hidden -PassThru
try {
    $ready = $false
    foreach ($attempt in 1..75) {
        if ($service.HasExited) { throw 'Backend exited. Run Start-EarthPulse.ps1 -BackendOnly for details.' }
        try {
            $null = Invoke-RestMethod 'http://127.0.0.1:8765/tools' -Headers @{Authorization="Bearer $env:EARTH_PULSE_TOKEN"} -TimeoutSec 2
            $ready = $true
            break
        } catch { Start-Sleep -Milliseconds 400 }
    }
    if (!$ready) { throw 'Local backend failed to become ready.' }
    # Only the backend receives provider secrets. Scope these changes to the desktop child.
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $desktop
    $start.WorkingDirectory = $earthCraftRoot
    $start.UseShellExecute = $false
    foreach ($name in 'NVIDIA_API_KEY','OLLAMA_API_KEY','GOOGLE_APPLICATION_CREDENTIALS') {
        $start.EnvironmentVariables.Remove($name)
    }
    # GCC-built Windows targets may load the matching MinGW runtime DLLs.
    # Restrict that runtime path change to the desktop process.
    $mingwBin = Join-Path ([IO.Path]::GetPathRoot($PSScriptRoot)) 'EarthPulseBuildTools/bin'
    if (Test-Path $mingwBin) {
        $start.EnvironmentVariables['PATH'] = "$mingwBin;$env:PATH"
    }
    $window = [Diagnostics.Process]::Start($start)
    $window.WaitForExit()
} finally {
    if (!$service.HasExited) {
        # The virtual-environment Python launcher owns a second Python process.
        # Stop that entire owned tree so uvicorn releases its listening socket.
        if ([Diagnostics.Process].GetMethod('Kill', [type[]]@([bool]))) {
            $service.Kill($true)
        } else {
            & (Join-Path $env:SystemRoot 'System32/taskkill.exe') /PID $service.Id /T /F | Out-Null
            if ($LASTEXITCODE -ne 0 -and !$service.HasExited) {
                Write-Warning "Could not stop the backend process tree for PID $($service.Id)."
            }
        }
        $null = $service.WaitForExit(5000)
    }
}
