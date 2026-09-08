param(
    [string]$InstallDir,
    [ValidateSet('auto', 'release', 'main')]
    [string]$Channel = 'auto',
    [switch]$NoLaunch,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$repo = 'benclawbot/Medusa'
$mainManifestSchema = 'medusa-main-artifact-v1'
$signatureSchema = 'medusa-release-signature-v1'
$signatureKeyId = 'medusa-release-2026-08-primary'
$signatureAlgorithm = 'Ed25519'
$defaultInstallDir = "$env:LOCALAPPDATA\Medusa\bin"
$legacyLockName = '.medusa-update.lock'

function Normalize-Path([string]$Path) {
    return [System.IO.Path]::GetFullPath($Path)
}

function Get-ExistingMedusa {
    $command = Get-Command medusa -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $command) {
        return $null
    }
    if (-not [string]::IsNullOrWhiteSpace($command.Source)) {
        return Normalize-Path $command.Source
    }
    if (-not [string]::IsNullOrWhiteSpace($command.Path)) {
        return Normalize-Path $command.Path
    }
    return $null
}

function Get-TargetProcesses([string]$Target) {
    $normalizedTarget = Normalize-Path $Target
    $matches = @()
    foreach ($process in Get-Process -ErrorAction SilentlyContinue) {
        try {
            $processPath = $process.Path
            if (-not [string]::IsNullOrWhiteSpace($processPath) -and
                [string]::Equals(
                    (Normalize-Path $processPath),
                    $normalizedTarget,
                    [System.StringComparison]::OrdinalIgnoreCase
                )) {
                $matches += $process
            }
        }
        catch {
            # Protected processes can reject Path inspection. They cannot be this user-owned target.
        }
    }
    return @($matches)
}

function Stop-TargetProcesses([string]$Target) {
    $processes = @(Get-TargetProcesses $Target)
    if ($processes.Count -eq 0) {
        return
    }

    Write-Host "Stopping Medusa processes using $Target ..."
    foreach ($process in $processes) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }

    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if ((Get-TargetProcesses $Target).Count -eq 0) {
            return
        }
        Start-Sleep -Milliseconds 100
    }

    $remaining = @(Get-TargetProcesses $Target | ForEach-Object { $_.Id })
    throw "Could not stop all processes using $Target. Remaining process IDs: $($remaining -join ', ')"
}

function Wait-For-LegacyUpdateHelper([string]$Directory) {
    $lock = Join-Path $Directory $legacyLockName
    if (-not (Test-Path -LiteralPath $lock)) {
        return
    }

    Write-Host 'Waiting for the previous Medusa update helper to finish...'
    for ($attempt = 0; $attempt -lt 650; $attempt++) {
        if (-not (Test-Path -LiteralPath $lock)) {
            Start-Sleep -Milliseconds 250
            return
        }
        Start-Sleep -Milliseconds 100
    }
    throw "A Medusa update helper still owns $lock after 65 seconds. Wait for it to finish and rerun the installer."
}

function Invoke-SafeReplacement(
    [string]$Target,
    [string]$Staged,
    [scriptblock]$Validator
) {
    $backup = [System.IO.Path]::ChangeExtension($Target, 'bootstrap-previous.exe')
    if (Test-Path -LiteralPath $backup) {
        if (-not (Test-Path -LiteralPath $Target)) {
            Move-Item -LiteralPath $backup -Destination $Target -Force
        }
        else {
            Remove-Item -LiteralPath $backup -Force
        }
    }

    Stop-TargetProcesses $Target
    $hadTarget = Test-Path -LiteralPath $Target
    try {
        if ($hadTarget) {
            Move-Item -LiteralPath $Target -Destination $backup -Force
        }
        Move-Item -LiteralPath $Staged -Destination $Target -Force
        & $Validator $Target
        if ($hadTarget -and (Test-Path -LiteralPath $backup)) {
            Remove-Item -LiteralPath $backup -Force
        }
    }
    catch {
        Remove-Item -LiteralPath $Target -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $backup) {
            Move-Item -LiteralPath $backup -Destination $Target -Force
        }
        throw
    }
}

function Download-File([string]$Url, [string]$Destination, [string]$Activity) {
    $client = [System.Net.Http.HttpClient]::new()
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('medusa-windows-installer')
    try {
        $response = $client.GetAsync(
            $Url,
            [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
        ).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode()
        $total = $response.Content.Headers.ContentLength
        $input = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output = [System.IO.File]::Create($Destination)
        try {
            $buffer = New-Object byte[] (1024 * 128)
            $readTotal = 0L
            while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $output.Write($buffer, 0, $read)
                $readTotal += $read
                if ($total -and $total -gt 0) {
                    $percent = [Math]::Min(100, [int](($readTotal * 100) / $total))
                    Write-Progress -Activity $Activity -Status "$percent%" -PercentComplete $percent
                }
            }
        }
        finally {
            $output.Dispose()
            $input.Dispose()
            Write-Progress -Activity $Activity -Completed
        }
    }
    finally {
        $client.Dispose()
    }
}

function Get-WindowsArchitecture {
    $architecture = $null
    try {
        $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    catch {
        $architecture = $env:PROCESSOR_ARCHITECTURE
    }

    switch ($architecture.ToUpperInvariant()) {
        'X64' { return 'x86_64' }
        'AMD64' { return 'x86_64' }
        'ARM64' { return 'aarch64' }
        default { throw "Unsupported Windows architecture for Medusa bootstrap: $architecture" }
    }
}

function Get-MainRevision {
    $headers = @{ 'User-Agent' = 'medusa-windows-installer' }
    $commit = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/commits/main" -Headers $headers
    $revision = [string]$commit.sha
    if ($revision -notmatch '^[0-9a-f]{40}$') {
        throw "GitHub returned an invalid main revision: $revision"
    }
    return $revision
}

function Download-MainArchive(
    [string]$TempDir,
    [string]$Architecture
) {
    $revision = Get-MainRevision
    $asset = "medusa-main-windows-$Architecture.zip"
    $tag = "main-$revision"
    $baseUrl = "https://github.com/$repo/releases/download/$tag"
    $archive = Join-Path $TempDir $asset
    $manifestPath = "$archive.json"
    $signaturePath = "$manifestPath.sig.json"

    Write-Host "Resolving Medusa main $($revision.Substring(0, 12))..."
    $manifestReady = $false
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        try {
            Download-File "$baseUrl/$asset.json" $manifestPath 'Waiting for rolling main manifest'
            $manifestReady = $true
            break
        }
        catch {
            Remove-Item -LiteralPath $manifestPath -Force -ErrorAction SilentlyContinue
            if ($attempt -eq 119) {
                throw "The rolling main artifact for $($revision.Substring(0, 12)) was not published within 10 minutes."
            }
            Start-Sleep -Seconds 5
        }
    }
    if (-not $manifestReady) {
        throw 'Rolling main manifest was not published.'
    }

    Download-File "$baseUrl/$asset.json.sig.json" $signaturePath 'Downloading manifest signature'
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $signature = Get-Content -LiteralPath $signaturePath -Raw | ConvertFrom-Json

    if ($manifest.schema -ne $mainManifestSchema) {
        throw "Unexpected rolling main manifest schema: $($manifest.schema)"
    }
    if ($manifest.revision -ne $revision) {
        throw "Rolling main manifest revision mismatch: expected $revision, got $($manifest.revision)"
    }
    if ($manifest.name -ne $asset) {
        throw "Rolling main manifest asset mismatch: expected $asset, got $($manifest.name)"
    }
    if ([long]$manifest.bytes -le 0 -or [string]$manifest.sha256 -notmatch '^[0-9a-f]{64}$') {
        throw 'Rolling main manifest contains invalid size or SHA-256 metadata.'
    }

    $manifestDigest = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($signature.schema -ne $signatureSchema -or
        $signature.key_id -ne $signatureKeyId -or
        $signature.algorithm -ne $signatureAlgorithm -or
        [string]$signature.manifest_sha256 -ne $manifestDigest -or
        [string]$signature.signature -notmatch '^[0-9a-fA-F]{128}$') {
        throw 'Rolling main signature metadata is invalid.'
    }

    Download-File "$baseUrl/$asset" $archive 'Downloading Medusa rolling main'
    $archiveInfo = Get-Item -LiteralPath $archive
    if ($archiveInfo.Length -ne [long]$manifest.bytes) {
        throw "Rolling main archive size mismatch: expected $($manifest.bytes), got $($archiveInfo.Length)"
    }
    $archiveDigest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($archiveDigest -ne [string]$manifest.sha256) {
        throw 'Rolling main archive SHA-256 does not match its manifest.'
    }

    return [pscustomobject]@{
        Archive = $archive
        Revision = $revision
    }
}

function Add-InstallDirToPath([string]$Directory) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $pathEntries = @($userPath -split ';' | Where-Object { $_ })
    if ($pathEntries -notcontains $Directory) {
        $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) {
            $Directory
        }
        else {
            "$Directory;$userPath"
        }
        [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
    }
    if (($env:Path -split ';') -notcontains $Directory) {
        $env:Path = "$Directory;$env:Path"
    }
}

function Clear-LegacyUpdateArtifacts([string]$Target) {
    $directory = Split-Path -Parent $Target
    $artifacts = @(
        (Join-Path $directory '.medusa-update-state'),
        (Join-Path $directory '.medusa-update-health'),
        ([System.IO.Path]::ChangeExtension($Target, 'update-new.exe')),
        ([System.IO.Path]::ChangeExtension($Target, 'previous.exe')),
        ([System.IO.Path]::ChangeExtension($Target, 'update.ps1'))
    )
    foreach ($artifact in $artifacts) {
        Remove-Item -LiteralPath $artifact -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-BootstrapSelfTest {
    $root = Join-Path ([System.IO.Path]::GetTempPath()) ("medusa-bootstrap-selftest-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $target = Join-Path $root 'medusa.exe'
    $staged = Join-Path $root 'medusa.bootstrap-new.exe'
    $process = $null
    try {
        $ping = Join-Path $env:SystemRoot 'System32\PING.EXE'
        if (-not (Test-Path -LiteralPath $ping)) {
            throw "Windows ping executable is missing at $ping"
        }
        Copy-Item -LiteralPath $ping -Destination $target -Force
        Copy-Item -LiteralPath $env:ComSpec -Destination $staged -Force
        $replacementHash = (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash
        $process = Start-Process -FilePath $target -ArgumentList @(
            '127.0.0.1',
            '-t'
        ) -PassThru
        Start-Sleep -Milliseconds 300

        Invoke-SafeReplacement $target $staged {
            param($Path)
            if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $replacementHash) {
                throw 'self-test replacement hash mismatch'
            }
        }
        $process.Refresh()
        if (-not $process.HasExited) {
            throw 'self-test target process was not stopped before replacement'
        }

        Copy-Item -LiteralPath $ping -Destination $target -Force
        Copy-Item -LiteralPath $env:ComSpec -Destination $staged -Force
        $originalHash = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
        $rollbackObserved = $false
        try {
            Invoke-SafeReplacement $target $staged {
                param($Path)
                throw 'intentional self-test validation failure'
            }
        }
        catch {
            $rollbackObserved = $true
        }
        if (-not $rollbackObserved) {
            throw 'self-test did not observe the intentional validation failure'
        }
        if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne $originalHash) {
            throw 'self-test rollback did not restore the original target'
        }
        if (Test-Path -LiteralPath ([System.IO.Path]::ChangeExtension($target, 'bootstrap-previous.exe'))) {
            throw 'self-test left a bootstrap backup behind'
        }

        Write-Host 'Windows installer bootstrap self-test passed.'
    }
    finally {
        if ($null -ne $process) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($SelfTest) {
    Invoke-BootstrapSelfTest
    return
}

if (-not [string]::IsNullOrWhiteSpace($env:MEDUSA_INSTALL_CHANNEL)) {
    if ($env:MEDUSA_INSTALL_CHANNEL -notin @('auto', 'release', 'main')) {
        throw 'MEDUSA_INSTALL_CHANNEL must be auto, release, or main.'
    }
    $Channel = $env:MEDUSA_INSTALL_CHANNEL
}

$existingTarget = Get-ExistingMedusa
$existingVersion = $null
if ($null -ne $existingTarget -and (Test-Path -LiteralPath $existingTarget)) {
    try {
        $existingVersion = (& $existingTarget --version 2>$null | Out-String).Trim()
    }
    catch {
        $existingVersion = $null
    }
}

if ($Channel -eq 'auto') {
    if ($existingVersion -and $existingVersion -match '\bmain\b') {
        $Channel = 'main'
    }
    else {
        $Channel = 'release'
    }
}

if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    if ($null -ne $existingTarget) {
        $target = $existingTarget
        $InstallDir = Split-Path -Parent $target
    }
    else {
        $InstallDir = $defaultInstallDir
        $target = Join-Path $InstallDir 'medusa.exe'
    }
}
else {
    $InstallDir = Normalize-Path $InstallDir
    $target = Join-Path $InstallDir 'medusa.exe'
}

$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("medusa-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null

try {
    $revision = $null
    if ($Channel -eq 'main') {
        $main = Download-MainArchive $tempDir (Get-WindowsArchitecture)
        $archive = $main.Archive
        $revision = $main.Revision
    }
    else {
        $asset = 'medusa-cli-windows.zip'
        $archive = Join-Path $tempDir $asset
        Download-File "https://github.com/$repo/releases/latest/download/$asset" $archive 'Downloading Medusa'
    }

    $extractDir = Join-Path $tempDir 'extract'
    Expand-Archive -LiteralPath $archive -DestinationPath $extractDir -Force
    $binaries = @(Get-ChildItem -Path $extractDir -Filter 'medusa.exe' -File -Recurse)
    if ($binaries.Count -ne 1) {
        throw "The archive must contain exactly one medusa.exe; found $($binaries.Count)."
    }
    $binary = $binaries[0]

    $candidateVersion = (& $binary.FullName --version 2>$null | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($candidateVersion)) {
        throw 'The downloaded Medusa executable failed its version probe.'
    }
    if ($Channel -eq 'main' -and
        $candidateVersion -notmatch [regex]::Escape($revision.Substring(0, 12))) {
        throw "The downloaded Medusa executable does not identify main $($revision.Substring(0, 12))."
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Wait-For-LegacyUpdateHelper $InstallDir

    $staged = Join-Path $InstallDir 'medusa.bootstrap-new.exe'
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $binary.FullName -Destination $staged -Force
    $candidateHash = (Get-FileHash -LiteralPath $binary.FullName -Algorithm SHA256).Hash
    if ((Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash -ne $candidateHash) {
        throw 'The staged Medusa executable changed while being copied.'
    }

    $expectedVersion = $candidateVersion
    Invoke-SafeReplacement $target $staged {
        param($Path)
        $installedVersion = (& $Path --version 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or $installedVersion -ne $expectedVersion) {
            throw 'The installed Medusa executable failed post-replacement validation.'
        }
    }

    Clear-LegacyUpdateArtifacts $target
    Add-InstallDirToPath $InstallDir

    $version = (& $target --version 2>$null | Out-String).Trim()
    if ($Channel -eq 'main') {
        Write-Host "Bootstrapped $version"
    }
    else {
        Write-Host "Installed $version"
    }

    if (-not $NoLaunch) {
        Write-Host 'Launching Medusa...'
        Write-Host ''
        try {
            while ([Console]::KeyAvailable) {
                [void][Console]::ReadKey($true)
            }
        }
        catch {
            # Some hosts do not expose a readable console input buffer; launch normally there.
        }
        Start-Process -FilePath $target -NoNewWindow -Wait
    }
}
finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
