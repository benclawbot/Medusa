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

function Assert-OAuthGateway {
    try {
        $response = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:10531/v1/models" -TimeoutSec 5
        if ($response.StatusCode -lt 200 -or $response.StatusCode -ge 500) {
            throw "Unexpected gateway status $($response.StatusCode)."
        }
        Write-Host "openai-oauth gateway is reachable at 127.0.0.1:10531."
    }
    catch {
        throw "The openai-oauth gateway is not reachable. Complete ChatGPT OAuth login first, then run: npx --yes openai-oauth@latest --detach"
    }
}

if ($env:OS -ne "Windows_NT") {
    throw "This bootstrap is only supported on Windows."
}

Assert-InteractiveUser
Require-Command git
Require-Command node
Require-Command npm
Require-Command rustup
Assert-Node22

rustup toolchain install 1.88.0 --profile minimal | Out-Host
Assert-OAuthGateway

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
