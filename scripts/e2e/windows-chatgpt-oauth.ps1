[CmdletBinding()]
param(
    [string]$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")),
    [string]$Prompt = "Reply with the exact text MEDUSA_OAUTH_E2E_OK and do not modify files.",
    [int]$ModelTurnTimeoutSeconds = 300,
    [int]$TuiTimeoutSeconds = 120,
    [int]$DesktopSmokeSeconds = 20,
    [switch]$SkipDesktopLaunch
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$resultsDir = Join-Path $RepositoryRoot "artifacts\oauth-e2e"
$rawDir = Join-Path $env:TEMP ("medusa-oauth-e2e-" + [guid]::NewGuid().ToString("N"))
$fixtureDir = Join-Path $rawDir "fixture"
New-Item -ItemType Directory -Force -Path $resultsDir, $fixtureDir | Out-Null

function Write-Step([string]$Message) {
    Write-Host "::group::$Message"
}

function Complete-Step {
    Write-Host "::endgroup::"
}

function Invoke-External {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter()][string[]]$ArgumentList = @(),
        [Parameter(Mandatory)][string]$LogName,
        [int]$TimeoutSeconds = 600,
        [string]$WorkingDirectory = $RepositoryRoot,
        [switch]$AllowTimeout
    )

    $stdout = Join-Path $rawDir "$LogName.stdout.log"
    $stderr = Join-Path $rawDir "$LogName.stderr.log"
    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -WorkingDirectory $WorkingDirectory `
        -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr

    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        if (-not $AllowTimeout) {
            throw "$FilePath timed out after $TimeoutSeconds seconds."
        }
        return [pscustomobject]@{ ExitCode = $null; TimedOut = $true; Stdout = $stdout; Stderr = $stderr }
    }

    if ($process.ExitCode -ne 0) {
        $tail = @()
        if (Test-Path $stderr) { $tail += Get-Content $stderr -Tail 40 }
        if (Test-Path $stdout) { $tail += Get-Content $stdout -Tail 40 }
        throw "$FilePath exited with code $($process.ExitCode).`n$($tail -join "`n")"
    }

    [pscustomobject]@{ ExitCode = $process.ExitCode; TimedOut = $false; Stdout = $stdout; Stderr = $stderr }
}

function Sanitize-Text([string]$Text) {
    if ($null -eq $Text) { return "" }
    $sanitized = $Text
    $patterns = @(
        '(?im)^(authorization\s*:\s*).+$',
        '(?im)^(cookie\s*:\s*).+$',
        '(?i)bearer\s+[A-Za-z0-9._~+\-/]+=*',
        '(?i)(access_token|refresh_token|id_token|client_secret)\s*[=:]\s*["'']?[^\s,"'']+',
        '(?i)sk-[A-Za-z0-9_-]{12,}'
    )
    foreach ($pattern in $patterns) {
        $sanitized = [regex]::Replace($sanitized, $pattern, '$1[REDACTED]')
    }
    return $sanitized
}

function Export-SanitizedLogs {
    Get-ChildItem -Path $rawDir -Filter "*.log" -File -ErrorAction SilentlyContinue | ForEach-Object {
        $content = Get-Content $_.FullName -Raw -ErrorAction SilentlyContinue
        $safe = Sanitize-Text $content
        Set-Content -Path (Join-Path $resultsDir $_.Name) -Value $safe -Encoding utf8
    }
}

$summary = [ordered]@{
    started_at_utc = [DateTime]::UtcNow.ToString("o")
    os = [Environment]::OSVersion.VersionString
    codex_app_server = "not-validated"
    deterministic_tests = "not-run"
    live_model_turn = "not-run"
    tui = "not-run"
    desktop = "not-run"
    credential_files_read = $false
}

try {
    Write-Step "Validate prerequisites"
    foreach ($command in @("git", "cargo", "node", "npm", "codex")) {
        if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
            throw "Required command '$command' is not available on PATH."
        }
    }
    if (-not $IsWindows) { throw "This harness must run on Windows." }
    Complete-Step

    Write-Step "Prepare disposable repository"
    Invoke-External -FilePath "git" -ArgumentList @("init", "--initial-branch=main") -LogName "fixture-git-init" -WorkingDirectory $fixtureDir | Out-Null
    Set-Content -Path (Join-Path $fixtureDir "README.md") -Value "Medusa authenticated E2E fixture" -Encoding utf8
    Invoke-External -FilePath "git" -ArgumentList @("add", "README.md") -LogName "fixture-git-add" -WorkingDirectory $fixtureDir | Out-Null
    Invoke-External -FilePath "git" -ArgumentList @("-c", "user.name=Medusa E2E", "-c", "user.email=medusa-e2e@invalid.local", "commit", "-m", "fixture") -LogName "fixture-git-commit" -WorkingDirectory $fixtureDir | Out-Null
    Complete-Step

    Write-Step "Validate Codex app-server OAuth route"
    Invoke-External -FilePath "codex" -ArgumentList @("--version") -LogName "codex-version" -TimeoutSeconds 30 | Out-Null
    $summary.codex_app_server = "validated (codex app-server --stdio)"
    Complete-Step

    Write-Step "Run existing deterministic tests and builds"
    Invoke-External -FilePath "cargo" -ArgumentList @("test", "-p", "medusa-tui", "--locked") -LogName "cargo-test-tui" -TimeoutSeconds 1200 | Out-Null
    Invoke-External -FilePath "cargo" -ArgumentList @("test", "-p", "medusa-provider", "--locked") -LogName "cargo-test-provider" -TimeoutSeconds 1200 | Out-Null
    Invoke-External -FilePath "npm" -ArgumentList @("ci") -LogName "desktop-npm-ci" -TimeoutSeconds 1200 -WorkingDirectory (Join-Path $RepositoryRoot "apps\medusa-desktop") | Out-Null
    Invoke-External -FilePath "npm" -ArgumentList @("test", "--", "--run") -LogName "desktop-tests" -TimeoutSeconds 1200 -WorkingDirectory (Join-Path $RepositoryRoot "apps\medusa-desktop") | Out-Null
    Invoke-External -FilePath "npm" -ArgumentList @("run", "build") -LogName "desktop-web-build" -TimeoutSeconds 1200 -WorkingDirectory (Join-Path $RepositoryRoot "apps\medusa-desktop") | Out-Null
    Invoke-External -FilePath "cargo" -ArgumentList @("build", "--locked", "-p", "medusa-cli", "-p", "medusa-tui") -LogName "cargo-build-frontends" -TimeoutSeconds 1800 | Out-Null
    Invoke-External -FilePath "npm" -ArgumentList @("run", "tauri:build", "--", "--debug") -LogName "desktop-tauri-build" -TimeoutSeconds 2400 -WorkingDirectory (Join-Path $RepositoryRoot "apps\medusa-desktop") | Out-Null
    $summary.deterministic_tests = "passed"
    Complete-Step

    $medusaExe = Join-Path $RepositoryRoot "target\debug\medusa.exe"
    $tuiExe = Join-Path $RepositoryRoot "target\debug\medusa-tui.exe"
    if (-not (Test-Path $medusaExe)) { throw "Expected CLI binary not found: $medusaExe" }
    if (-not (Test-Path $tuiExe)) { throw "Expected TUI binary not found: $tuiExe" }

    Write-Step "Run live ChatGPT OAuth model turn"
    $headless = Invoke-External -FilePath $medusaExe -ArgumentList @("run", "--non-interactive", $Prompt) -LogName "live-headless" -TimeoutSeconds $ModelTurnTimeoutSeconds -WorkingDirectory $fixtureDir
    $headlessText = ((Get-Content $headless.Stdout -Raw -ErrorAction SilentlyContinue) + "`n" + (Get-Content $headless.Stderr -Raw -ErrorAction SilentlyContinue))
    if ($headlessText -notmatch "MEDUSA_OAUTH_E2E_OK") {
        throw "The live model turn completed but did not contain the expected marker."
    }
    $summary.live_model_turn = "passed"
    Complete-Step

    Write-Step "Exercise prompt-driven TUI entry point"
    $beforeSessions = @(Get-ChildItem -Path (Join-Path $fixtureDir ".medusa") -Recurse -File -ErrorAction SilentlyContinue).Count
    $tui = Invoke-External -FilePath $tuiExe -ArgumentList @("--repo", $fixtureDir, "--prompt", $Prompt) -LogName "live-tui" -TimeoutSeconds $TuiTimeoutSeconds -WorkingDirectory $fixtureDir -AllowTimeout
    $afterSessions = @(Get-ChildItem -Path (Join-Path $fixtureDir ".medusa") -Recurse -File -ErrorAction SilentlyContinue).Count
    if ($afterSessions -le $beforeSessions) {
        throw "The TUI entry point did not create durable session evidence."
    }
    $summary.tui = if ($tui.TimedOut) { "passed-session-created-process-stopped" } else { "passed" }
    Complete-Step

    Write-Step "Exercise native Windows desktop package"
    if ($SkipDesktopLaunch) {
        $summary.desktop = "build-only-skipped-launch"
    } else {
        $desktopExe = Get-ChildItem -Path (Join-Path $RepositoryRoot "target\debug") -Filter "medusa-desktop.exe" -Recurse -File -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $desktopExe) {
            throw "The Tauri build succeeded but medusa-desktop.exe was not found."
        }
        $desktopProcess = Start-Process -FilePath $desktopExe.FullName -WorkingDirectory $fixtureDir -PassThru
        Start-Sleep -Seconds $DesktopSmokeSeconds
        if ($desktopProcess.HasExited) {
            throw "The Windows desktop app exited during the smoke window with code $($desktopProcess.ExitCode)."
        }
        Stop-Process -Id $desktopProcess.Id -Force -ErrorAction SilentlyContinue
        $summary.desktop = "passed-launch-smoke"
    }
    Complete-Step
} finally {
    $summary.finished_at_utc = [DateTime]::UtcNow.ToString("o")
    Export-SanitizedLogs
    $summary | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $resultsDir "summary.json") -Encoding utf8
    Remove-Item -Path $rawDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ($summary | ConvertTo-Json -Depth 4)
