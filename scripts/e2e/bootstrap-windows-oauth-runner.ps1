[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$RunnerToken,

    [string]$RepositoryUrl = "https://github.com/benclawbot/Medusa",
    [string]$RunnerDirectory = "$env:USERPROFILE\actions-runner-medusa",
    [string]$RunnerName = "$env:COMPUTERNAME-medusa-oauth",
    [string]$RunnerVersion = "2.326.0",
    [switch]$Start
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Require-Command {
    param([Parameter(Mandatory = $true)][string]$Name)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found in PATH."
    }
}

function Assert-InteractiveUser {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    if ($identity -match "SYSTEM$") {
        throw "Run this script as the interactive Windows user that owns the ChatGPT OAuth session, not LocalSystem."
    }
    Write-Host "Configuring runner for interactive user $identity"
}

function Assert-Node22 {
    $version = (& node --version).Trim()
    $major = [int]$version.TrimStart('v').Split('.')[0]
    if ($major -ne 22) {
        throw "Node.js 22 is required; found $version."
    }
}

function Assert-CodexCli {
    $version = (& codex --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($version)) {
        throw "The Codex CLI could not be started. Install it and ensure codex is on PATH."
    }
    Write-Host "Found $version; Medusa will use codex app-server --stdio for ChatGPT OAuth."
}

if ($env:OS -ne "Windows_NT") {
    throw "This bootstrap is only supported on Windows."
}

Assert-InteractiveUser
Require-Command git
Require-Command node
Require-Command npm
Require-Command codex
Require-Command rustup
Assert-Node22
Assert-CodexCli

rustup toolchain install 1.88.0 --profile minimal | Out-Host

New-Item -ItemType Directory -Path $RunnerDirectory -Force | Out-Null
$archive = Join-Path $env:TEMP "actions-runner-win-x64-$RunnerVersion.zip"
$downloadUrl = "https://github.com/actions/runner/releases/download/v$RunnerVersion/actions-runner-win-x64-$RunnerVersion.zip"

if (-not (Test-Path $archive)) {
    Write-Host "Downloading GitHub Actions runner $RunnerVersion..."
    Invoke-WebRequest -UseBasicParsing -Uri $downloadUrl -OutFile $archive
}

if (-not (Test-Path (Join-Path $RunnerDirectory "config.cmd"))) {
    Expand-Archive -Path $archive -DestinationPath $RunnerDirectory -Force
}

Push-Location $RunnerDirectory
try {
    if (Test-Path ".runner") {
        Write-Host "Runner is already configured in $RunnerDirectory."
    }
    else {
        & .\config.cmd `
            --unattended `
            --url $RepositoryUrl `
            --token $RunnerToken `
            --name $RunnerName `
            --labels "medusa-oauth" `
            --work "_work" `
            --replace
        if ($LASTEXITCODE -ne 0) {
            throw "GitHub Actions runner configuration failed with exit code $LASTEXITCODE."
        }
    }

    Write-Host "Runner configured with labels: self-hosted, Windows, X64, medusa-oauth"
    Write-Host "Keep it running in this interactive desktop session so the native Windows app can launch."

    if ($Start) {
        & .\run.cmd
        exit $LASTEXITCODE
    }

    Write-Host "Start it with: cd '$RunnerDirectory'; .\run.cmd"
}
finally {
    Pop-Location
}
