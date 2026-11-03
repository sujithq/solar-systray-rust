#requires -Version 7.0
param(
    [Parameter(Mandatory)][string]$ConfigPath,
    [string]$ExpectedBranch
)

function Start-IsolatedHaChild {
    param(
        [string]$Executable,
        [string[]]$Arguments = @(),
        [string]$Configuration,
        [string]$TokenReference,
        [Security.SecureString]$Credential
    )
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $pointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Credential)
    try {
        $token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($pointer)
        if ([string]::IsNullOrEmpty($token) -or $token.Length -gt 8192 -or $token -match '[\x00-\x1f\x7f]') {
            throw 'Invalid existing token; not launched.'
        }
        $start.Environment[$TokenReference] = $token
        $start.Environment['SOLAR_SYSTRAY_HA_CONFIG'] = $Configuration
        try {
            $child = [Diagnostics.Process]::Start($start)
        } catch {
            throw 'Unable to start the native tray; no credentials or configuration were saved.'
        }
        return $child
    } finally {
        $start.Environment.Remove($TokenReference) | Out-Null
        $start.Environment.Remove('SOLAR_SYSTRAY_HA_CONFIG') | Out-Null
        $token = $null
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($pointer)
    }
}

function Start-HaTray {
    $ErrorActionPreference = 'Stop'
    if (-not $IsWindows) { throw 'The native tray launcher requires Windows and PowerShell 7.' }
    if (-not [IO.Path]::IsPathFullyQualified($ConfigPath) -or
        -not (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
        throw 'Supply an existing absolute isolated HA config path; no file will be created.'
    }
    if ((Get-Item -LiteralPath $ConfigPath).Length -gt 1048576) { throw 'Config exceeds the launcher size limit.' }
    try { $settings = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json }
    catch { throw 'Cannot parse isolated config; no credential requested.' }
    $solar = if ($null -ne $settings.solar) { $settings.solar } else { $settings }
    if ($solar.energySource.type -ne 'home-assistant' -or
        [string]::IsNullOrWhiteSpace($solar.homeAssistant.atomicGrid)) {
        throw 'Config must explicitly select Home Assistant and atomicGrid; no fallback is permitted.'
    }
    $reference = $solar.homeAssistant.tokenEnv
    if ([string]::IsNullOrEmpty($reference)) { $reference = 'SOLAR_HA_TOKEN' }
    if ($reference -cnotmatch '^[A-Za-z_][A-Za-z0-9_]{0,127}$' -or
        $reference -eq 'SOLAR_SYSTRAY_HA_CONFIG') {
        throw 'Invalid token environment reference; no credential requested.'
    }
    $repo = Split-Path -Parent $PSScriptRoot
    $guard = [Threading.Mutex]::new($false, 'Local\SolarSystray.Launcher')
    $owned = $false
    try {
        try { $owned = $guard.WaitOne(0) }
        catch [Threading.AbandonedMutexException] { $owned = $true }
        if (-not $owned) { throw 'Another launcher is active; no build or credential prompt started.' }
        if (Get-Process -Name 'solar-systray' -ErrorAction SilentlyContinue) {
            throw 'Solar Systray is already running. Use its tray Quit command first; nothing was stopped.'
        }
        Push-Location $repo
        try {
            $branch = & git branch --show-current
            if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($branch)) {
                throw 'Expected a checked-out repository branch; no credential requested.'
            }
            if ($ExpectedBranch -and $branch -ne $ExpectedBranch) {
                throw 'Checkout branch does not match the requested feature branch; no credential requested.'
            }
            Write-Host "Building current checkout branch: $branch"
            & cargo build --release --quiet -p solar-tray
            if ($LASTEXITCODE -ne 0) { throw 'Build failed; existing binary will not be launched.' }
            $executable = Join-Path $repo 'target\release\solar-systray.exe'
            if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Native binary missing after build.' }
            $revision = & git rev-parse --short HEAD
            if ($LASTEXITCODE -ne 0) { throw 'Cannot identify checkout revision; no credential requested.' }
            Write-Host "Built checkout revision: $revision (includes any local edits)"
            Write-Host "Executable SHA256: $((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash)"
            if (Get-Process -Name 'solar-systray' -ErrorAction SilentlyContinue) {
                throw 'A tray instance started during the build. Quit it yourself; nothing was stopped.'
            }
            $credential = Read-Host 'Existing HA token (hidden; child process only, never saved)' -AsSecureString
            try {
                $child = Start-IsolatedHaChild -Executable $executable -Configuration $ConfigPath `
                    -TokenReference $reference -Credential $credential
            } finally { $credential.Dispose() }
            try {
                if ($child.WaitForExit(2000)) {
                    throw 'Tray exited during startup. Check its local error dialog or duplicate-instance warning; no running tray was stopped.'
                }
                Write-Host "Tray process started (PID $($child.Id)); HA connection/display still requires tray observation."
                Write-Host 'No automatic timeout. Use the tray Quit command to stop it.'
                Write-Host 'This launcher now returns. Closing this ordinary terminal does not stop the independent tray process.'
                Write-Host 'Signing out, shutting down, or a terminal/job manager explicitly terminating descendants can still stop it.'
            } finally { $child.Dispose() }
        } finally { Pop-Location }
    } finally {
        if ($owned) { $guard.ReleaseMutex() }
        $guard.Dispose()
    }
}

if ($MyInvocation.InvocationName -ne '.') { Start-HaTray }
