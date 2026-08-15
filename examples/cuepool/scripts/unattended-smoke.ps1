[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$Executable,

    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path -LiteralPath $_ -PathType Leaf })]
    [string]$Project,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9-]{0,63}$')]
    [string]$Profile,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1024, 65535)]
    [int]$ApiPort,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-fA-F]{7,40}$')]
    [string]$ExpectedCommit,

    [Parameter(Mandatory = $true)]
    [string]$Artifact,

    [string]$CueQid = '1',

    [ValidateRange(5, 300)]
    [int]$StartupTimeoutSeconds = 60
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Executable = (Resolve-Path -LiteralPath $Executable).Path
$Project = (Resolve-Path -LiteralPath $Project).Path
$Artifact = [IO.Path]::GetFullPath($Artifact)
$apiBase = "http://127.0.0.1:$ApiPort"
$startedAt = [DateTime]::UtcNow
$process = $null
$failure = $null
$result = 'failed'
$exitCode = $null
$healthStart = $null
$healthIdle = $null
$statusPlaying = $null
$statusFinal = $null
$history = $null
$logs = $null
$commands = @()

$random = [Security.Cryptography.RandomNumberGenerator]::Create()
$tokenBytes = New-Object byte[] 32
$random.GetBytes($tokenBytes)
$random.Dispose()
$token = [Convert]::ToBase64String($tokenBytes)

function Invoke-CuePoolGet {
    param([Parameter(Mandatory = $true)][string]$Path)
    Invoke-RestMethod -UseBasicParsing -TimeoutSec 10 -Uri "$apiBase$Path"
}

function Invoke-CuePoolCommand {
    param(
        [Parameter(Mandatory = $true)][hashtable]$Command,
        [Parameter(Mandatory = $true)][string]$OperationId,
        [switch]$AllowRejected
    )

    $headers = @{
        Authorization = "Bearer $token"
        'Idempotency-Key' = $OperationId
    }
    $status = Invoke-RestMethod `
        -UseBasicParsing `
        -TimeoutSec 15 `
        -Method Post `
        -Uri "$apiBase/v1/commands" `
        -Headers $headers `
        -ContentType 'application/json' `
        -Body ($Command | ConvertTo-Json -Compress)

    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ($status.state -eq 'pending') {
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "Command '$OperationId' did not complete within 15 seconds"
        }
        Start-Sleep -Milliseconds 75
        $status = Invoke-CuePoolGet "/v1/commands/$($status.id)"
    }
    if ($status.state -eq 'rejected' -and -not $AllowRejected) {
        throw "Command '$OperationId' was rejected: $($status.message)"
    }
    $script:commands += $status
    $status
}

function Wait-CuePoolHealth {
    param(
        [Parameter(Mandatory = $true)][scriptblock]$Condition,
        [Parameter(Mandatory = $true)][string]$FailureMessage,
        [int]$TimeoutSeconds = 15
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        if ($script:process -and $script:process.HasExited) {
            throw "CuePool exited before it became ready (exit code $($script:process.ExitCode))"
        }
        try {
            $health = Invoke-CuePoolGet '/v1/health'
            if (& $Condition $health) {
                return $health
            }
        }
        catch {
            if ([DateTime]::UtcNow -ge $deadline) {
                throw
            }
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)

    throw $FailureMessage
}

function Get-FileEvidence {
    param([Parameter(Mandatory = $true)][string]$Path)
    $exists = Test-Path -LiteralPath $Path -PathType Leaf
    $sha256 = $null
    if ($exists) {
        $sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    [ordered]@{
        path = $Path
        exists = $exists
        sha256 = $sha256
    }
}

try {
    if (Get-NetTCPConnection -State Listen -LocalPort $ApiPort -ErrorAction SilentlyContinue) {
        throw "Loopback port $ApiPort is already in use"
    }

    $startInfo = New-Object Diagnostics.ProcessStartInfo
    $startInfo.FileName = $Executable
    $startInfo.Arguments = "--project `"$Project`""
    $startInfo.WorkingDirectory = Split-Path -Parent $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.EnvironmentVariables['CUEPOOL_AUTOMATION_PROFILE'] = $Profile
    $startInfo.EnvironmentVariables['CUEPOOL_API_BIND'] = "127.0.0.1:$ApiPort"
    $startInfo.EnvironmentVariables['CUEPOOL_API_CONTROL_TOKEN'] = $token

    $process = New-Object Diagnostics.Process
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw 'CuePool process did not start'
    }

    $healthStart = Wait-CuePoolHealth `
        -TimeoutSeconds $StartupTimeoutSeconds `
        -FailureMessage 'CuePool did not become ready' `
        -Condition { param($health) $health.ready -eq $true }

    if ($healthStart.profile -ne $Profile) {
        throw "Expected profile '$Profile', got '$($healthStart.profile)'"
    }
    if ($healthStart.commit -ne $ExpectedCommit) {
        throw "Expected build '$ExpectedCommit', got '$($healthStart.commit)'"
    }
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals($healthStart.project_path, $Project)) {
        throw "Expected project '$Project', got '$($healthStart.project_path)'"
    }
    if ([int]$healthStart.pid -ne $process.Id) {
        throw "Health PID $($healthStart.pid) does not match launched PID $($process.Id)"
    }

    Invoke-CuePoolCommand @{ command = 'select_cue'; qid = $CueQid } 'smoke-select' | Out-Null
    Invoke-CuePoolCommand @{ command = 'go' } 'smoke-go' | Out-Null
    Wait-CuePoolHealth `
        -FailureMessage 'CuePool did not report an active cue after GO' `
        -Condition { param($health) [int]$health.active_cues -gt 0 } | Out-Null

    Start-Sleep -Seconds 2
    $statusPlaying = Invoke-CuePoolGet '/v1/status'
    $rejectedShutdown = Invoke-CuePoolCommand `
        @{ command = 'shutdown' } `
        'smoke-shutdown-active' `
        -AllowRejected
    if ($rejectedShutdown.state -ne 'rejected') {
        throw 'Shutdown was not rejected while a cue was active'
    }

    Invoke-CuePoolCommand @{ command = 'pause' } 'smoke-pause' | Out-Null
    Invoke-CuePoolCommand @{ command = 'resume' } 'smoke-resume' | Out-Null
    Invoke-CuePoolCommand @{ command = 'stop' } 'smoke-stop' | Out-Null
    $healthIdle = Wait-CuePoolHealth `
        -FailureMessage 'CuePool did not become idle after Stop' `
        -Condition { param($health) [int]$health.active_cues -eq 0 }
    if ($healthIdle.dirty) {
        throw 'The smoke project became dirty'
    }

    Start-Sleep -Seconds 1
    $statusFinal = Invoke-CuePoolGet '/v1/status'
    $history = Invoke-CuePoolGet '/v1/status/history?seconds=10'
    $logs = Invoke-CuePoolGet '/v1/logs?after=0&limit=250'

    $shutdown = Invoke-CuePoolCommand @{ command = 'shutdown' } 'smoke-shutdown-idle'
    if ($shutdown.state -ne 'applied') {
        throw "Idle shutdown returned '$($shutdown.state)'"
    }
    if (-not $process.WaitForExit(10000)) {
        throw 'CuePool acknowledged shutdown but did not exit within 10 seconds'
    }
    $exitCode = $process.ExitCode
    if ($exitCode -ne 0) {
        throw "CuePool exited with code $exitCode"
    }
    $result = 'passed'
}
catch {
    $failure = $_.Exception.Message
}
finally {
    if ($process -and -not $process.HasExited) {
        try {
            if (-not $healthIdle) { $healthIdle = Invoke-CuePoolGet '/v1/health' }
            if (-not $statusFinal) { $statusFinal = Invoke-CuePoolGet '/v1/status' }
            if (-not $logs) { $logs = Invoke-CuePoolGet '/v1/logs?after=0&limit=250' }
        }
        catch {}
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        $process.WaitForExit(5000) | Out-Null
    }
    if ($process -and $process.HasExited -and $null -eq $exitCode) {
        $exitCode = $process.ExitCode
    }

    $profileRoot = Join-Path ([Environment]::GetFolderPath('ApplicationData')) "CuePool\automation\$Profile"
    $pidValue = $null
    if ($process) {
        $pidValue = $process.Id
    }
    $artifactData = [ordered]@{
        schema_version = 1
        result = $result
        error = $failure
        started_at = $startedAt.ToString('o')
        completed_at = [DateTime]::UtcNow.ToString('o')
        profile = $Profile
        api = $apiBase
        executable = (Get-FileEvidence $Executable)
        expected_commit = $ExpectedCommit
        project = (Get-FileEvidence $Project)
        pid = $pidValue
        exit_code = $exitCode
        health_start = $healthStart
        health_idle = $healthIdle
        status_playing = $statusPlaying
        status_final = $statusFinal
        status_history = $history
        logs = $logs
        commands = $commands
        settings = (Get-FileEvidence (Join-Path $profileRoot 'settings.json'))
        persistent_log = (Get-FileEvidence (Join-Path $profileRoot 'cuepool.log'))
    }
    $parent = Split-Path -Parent $Artifact
    if ($parent) {
        [IO.Directory]::CreateDirectory($parent) | Out-Null
    }
    $json = $artifactData | ConvertTo-Json -Depth 20 -Compress
    [IO.File]::WriteAllText($Artifact, $json, (New-Object Text.UTF8Encoding($false)))
    $token = $null
}

if ($failure) {
    throw "$failure. Smoke artifact: $Artifact"
}

Write-Output "CuePool unattended smoke passed: $Artifact"
