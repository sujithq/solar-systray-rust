#requires -Version 7.0
param(
    [ValidateSet('Verify', 'Host', 'Child')][string]$Mode = 'Verify',
    [string]$ResultPath
)
$ErrorActionPreference = 'Stop'
if ($Mode -eq 'Child') {
    Start-Sleep -Seconds 3
    @{
        token_received = $env:SOLAR_LAUNCHER_SYNTHETIC_TOKEN -eq 'synthetic-only'
        config_received = $env:SOLAR_SYSTRAY_HA_CONFIG -eq 'synthetic-config'
    } | ConvertTo-Json | Set-Content -LiteralPath "$ResultPath.pending"
    Move-Item -LiteralPath "$ResultPath.pending" -Destination $ResultPath
    exit
}
. (Join-Path $PSScriptRoot 'Start-HaTray.ps1') -ConfigPath 'synthetic-config'
if ($Mode -eq 'Host') {
    $env:SOLAR_LAUNCHER_SYNTHETIC_TOKEN = 'parent-sentinel'
    $env:SOLAR_SYSTRAY_HA_CONFIG = 'parent-config'
    $credential = ConvertTo-SecureString 'synthetic-only' -AsPlainText -Force
    try {
        $process = Start-IsolatedHaChild -Executable (Join-Path $PSHOME 'pwsh.exe') `
            -Arguments @('-NoProfile', '-File', $PSCommandPath, '-Mode', 'Child', '-ResultPath', $ResultPath) `
            -Configuration 'synthetic-config' -TokenReference 'SOLAR_LAUNCHER_SYNTHETIC_TOKEN' -Credential $credential
        $process.Dispose()
        if ($env:SOLAR_LAUNCHER_SYNTHETIC_TOKEN -ne 'parent-sentinel' -or
            $env:SOLAR_SYSTRAY_HA_CONFIG -ne 'parent-config') {
            throw 'Child environment changed launching host environment.'
        }
    } finally { $credential.Dispose() }
    exit
}
foreach ($file in @('Start-HaTray.ps1', 'Test-HaTrayLauncher.ps1')) {
    $tokens = $null
    $errors = $null
    $null = [Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot $file), [ref]$tokens, [ref]$errors)
    if ($errors.Count -ne 0) { throw "Syntax errors in $file" }
}
$testDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\launcher-validation'
$null = New-Item -ItemType Directory -Path $testDirectory -Force
$result = Join-Path $testDirectory ("solar-launcher-test-" + [Guid]::NewGuid().ToString('N') + '.json')
$config = "$result.config"
try {
    '{"solar":{"energySource":{"type":"home-assistant"},"homeAssistant":{"atomicGrid":"sensor.synthetic","tokenEnv":"SOLAR_LAUNCHER_SYNTHETIC_TOKEN"}}}' |
        Set-Content -LiteralPath $config
    $ConfigPath = $config
    $script:duplicate = $true
    $script:buildCalled = $false
    function Get-Process { if ($script:duplicate) { return [pscustomobject]@{ Id = 0 } } }
    function git { $global:LASTEXITCODE = 0; return 'synthetic-branch' }
    function cargo { $script:buildCalled = $true; $global:LASTEXITCODE = 1 }
    function Read-Host { throw 'FAIL: credential prompt must not be reached by failing preflight.' }
    function Assert-PreflightFailure([string]$Expected) {
        try { Start-HaTray; throw 'FAIL: expected rejection.' }
        catch { if ($_.Exception.Message -notlike $Expected) { throw } }
    }
    Assert-PreflightFailure 'Solar Systray is already running.*'
    if ($script:buildCalled) { throw 'Duplicate preflight reached build.' }
    $script:duplicate = $false
    $ExpectedBranch = 'different-branch'
    Assert-PreflightFailure 'Checkout branch does not match*'
    if ($script:buildCalled) { throw 'Wrong branch reached build.' }
    $ExpectedBranch = ''
    Assert-PreflightFailure 'Build failed;*'
    if (-not $script:buildCalled) { throw 'Build-failure case never exercised.' }
    $ConfigPath = "$config.missing"
    Assert-PreflightFailure 'Supply an existing absolute*'
    $ConfigPath = $config
    '{"solar":{"energySource":{"type":"mock"}}}' | Set-Content -LiteralPath $config
    Assert-PreflightFailure 'Config must explicitly select*'
    Remove-Item Function:\Get-Process, Function:\git, Function:\cargo, Function:\Read-Host
    $credential = ConvertTo-SecureString 'synthetic-only' -AsPlainText -Force
    try {
        try {
            $null = Start-IsolatedHaChild -Executable "$config.missing.exe" -Configuration 'synthetic-config' `
                -TokenReference 'SOLAR_LAUNCHER_SYNTHETIC_TOKEN' -Credential $credential
            throw 'FAIL: nonexistent executable should fail.'
        } catch {
            if ($_.Exception.Message -ne 'Unable to start the native tray; no credentials or configuration were saved.') { throw }
        }
        if (Test-Path Env:\SOLAR_LAUNCHER_SYNTHETIC_TOKEN) { throw 'Failed spawn leaked credential into parent.' }
    } finally { $credential.Dispose() }
    $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'))
    $start.UseShellExecute = $false
    foreach ($arg in @('-NoProfile', '-File', $PSCommandPath, '-Mode', 'Host', '-ResultPath', $result)) {
        $start.ArgumentList.Add($arg)
    }
    $hostProcess = [Diagnostics.Process]::Start($start)
    try {
        if (-not $hostProcess.WaitForExit(10000) -or $hostProcess.ExitCode -ne 0) { throw 'Synthetic launcher host failed.' }
        if (Test-Path -LiteralPath $result) { throw 'Synthetic child did not outlive its launching host.' }
        for ($i = 0; $i -lt 100 -and -not (Test-Path -LiteralPath $result); $i++) {
            Start-Sleep -Milliseconds 100
        }
        if (-not (Test-Path -LiteralPath $result)) { throw 'Independent synthetic child did not finish.' }
        $data = Get-Content -LiteralPath $result -Raw | ConvertFrom-Json
        if (-not $data.token_received -or -not $data.config_received) { throw 'Child environment isolation failed.' }
    } finally { $hostProcess.Dispose() }
    Write-Output 'Syntax, failing preflight, child-only environment and independent-lifetime checks passed. No tray or HA access.'
} finally {
    if (Test-Path -LiteralPath $result) { Remove-Item -LiteralPath $result -Force }
    if (Test-Path -LiteralPath $config) { Remove-Item -LiteralPath $config -Force }
}
