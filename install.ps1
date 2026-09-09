param(
    [string]$InstallDir,
    [ValidateSet('auto', 'release', 'main')]
    [string]$Channel = 'auto',
    [switch]$NoLaunch,
    [switch]$SelfTest
)

$ErrorActionPreference = 'Stop'
$repo = 'benclawbot/Medusa'
$defaultInstallDir = "$env:LOCALAPPDATA\Medusa\bin"
$legacyLockName = '.medusa-update.lock'
$bootstrapLockName = '.medusa-bootstrap.lock'
$channelMarkerName = '.medusa-install-channel'

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

function Read-KeyValueFile([string]$Path) {
    $values = @{}
    if (-not (Test-Path -LiteralPath $Path)) {
        return $values
    }
    foreach ($line in Get-Content -LiteralPath $Path -ErrorAction SilentlyContinue) {
        $parts = $line -split '=', 2
        if ($parts.Count -eq 2 -and -not [string]::IsNullOrWhiteSpace($parts[0])) {
            $values[$parts[0].Trim()] = $parts[1].Trim()
        }
    }
    return $values
}

function Test-ProcessIdentity([System.Diagnostics.Process]$Process, [string]$Identity) {
    if ([string]::IsNullOrWhiteSpace($Identity)) {
        return $false
    }
    $parts = $Identity -split '\|'
    if ($parts.Count -lt 2 -or $parts[0] -ne 'windows_filetime_100ns_v1') {
        return $false
    }
    $expected = 0L
    if (-not [long]::TryParse($parts[1], [ref]$expected)) {
        return $false
    }
    try {
        return $Process.StartTime.ToUniversalTime().ToFileTimeUtc() -eq $expected
    }
    catch {
        return $false
    }
}

function Stop-VerifiedLegacyHelper([string]$LockPath) {
    if (-not (Test-Path -LiteralPath $LockPath)) {
        return
    }

    $lock = Read-KeyValueFile $LockPath
    $helperPid = 0
    if (-not $lock.ContainsKey('helper_pid') -or
        -not [int]::TryParse([string]$lock['helper_pid'], [ref]$helperPid) -or
        $helperPid -le 0) {
        return
    }

    $helper = Get-Process -Id $helperPid -ErrorAction SilentlyContinue
    if ($null -eq $helper) {
        Remove-Item -LiteralPath $LockPath -Force -ErrorAction SilentlyContinue
        return
    }

    $identity = if ($lock.ContainsKey('helper_identity')) { [string]$lock['helper_identity'] } else { '' }
    if (-not (Test-ProcessIdentity $helper $identity)) {
        throw "A process is using legacy updater PID $helperPid, but its identity does not match $LockPath. Refusing to terminate an unrelated process."
    }

    Write-Host "Stopping legacy Medusa update helper $helperPid ..."
    Stop-Process -Id $helperPid -Force -ErrorAction SilentlyContinue
    try {
        Wait-Process -Id $helperPid -Timeout 5 -ErrorAction SilentlyContinue
    }
    catch {
        # The helper may already be gone.
    }
    Remove-Item -LiteralPath $LockPath -Force -ErrorAction SilentlyContinue
}

function Take-Over-LegacyUpdate([string]$Directory, [string]$Target) {
    $lockPath = Join-Path $Directory $legacyLockName
    Stop-VerifiedLegacyHelper $lockPath
    Stop-TargetProcesses $Target

    if (-not (Test-Path -LiteralPath $lockPath)) {
        return
    }

    # Older helpers may not have a verifiable helper identity. Once every process
    # executing the exact Medusa target is stopped, give such a helper a short
    # opportunity to finish before deciding whether its lock is stale.
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        if (-not (Test-Path -LiteralPath $lockPath)) {
            return
        }
        Start-Sleep -Milliseconds 100
    }

    $lock = Read-KeyValueFile $lockPath
    $ownerPids = @()
    foreach ($key in @('helper_pid', 'parent_pid')) {
        if ($lock.ContainsKey($key)) {
            $pidValue = 0
            if ([int]::TryParse([string]$lock[$key], [ref]$pidValue) -and $pidValue -gt 0) {
                $ownerPids += $pidValue
            }
        }
    }
    $liveOwners = @($ownerPids | Where-Object { $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue) })
    if ($liveOwners.Count -gt 0) {
        throw "A legacy Medusa update helper still owns $lockPath (process IDs: $($liveOwners -join ', '))."
    }

    # File existence is not ownership. A crashed helper or power loss can leave
    # this artifact behind indefinitely, so reclaim it when every recorded owner
    # is gone.
    Remove-Item -LiteralPath $lockPath -Force -ErrorAction SilentlyContinue
}

function Acquire-BootstrapLock([string]$Directory) {
    New-Item -ItemType Directory -Force -Path $Directory | Out-Null
    $path = Join-Path $Directory $bootstrapLockName
    try {
        return [System.IO.File]::Open(
            $path,
            [System.IO.FileMode]::OpenOrCreate,
            [System.IO.FileAccess]::ReadWrite,
            [System.IO.FileShare]::None
        )
    }
    catch {
        throw "Another Medusa installer is already replacing this installation: $path"
    }
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
        (Join-Path $directory $legacyLockName),
        ([System.IO.Path]::ChangeExtension($Target, 'update-new.exe')),
        ([System.IO.Path]::ChangeExtension($Target, 'previous.exe')),
        ([System.IO.Path]::ChangeExtension($Target, 'update.ps1'))
    )
    foreach ($artifact in $artifacts) {
        Remove-Item -LiteralPath $artifact -Force -ErrorAction SilentlyContinue
    }
}

function Get-NativeStagedCandidate([string]$Target) {
    $candidate = [System.IO.Path]::ChangeExtension($Target, 'update-new.exe')
    if (-not (Test-Path -LiteralPath $candidate)) {
        return $null
    }
    $version = (& $candidate --version 2>$null | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($version)) {
        throw 'The native updater staging file failed its version probe.'
    }
    if ($version -notmatch '·\s*main\s+[0-9a-fA-F]{12}\b') {
        throw "The native updater staging file does not identify a rolling main build: $version"
    }
    return [pscustomobject]@{
        Path = $candidate
        Version = $version
    }
}

function Read-ChannelMarker([string]$Directory) {
    $path = Join-Path $Directory $channelMarkerName
    if (-not (Test-Path -LiteralPath $path)) {
        return $null
    }
    $value = (Get-Content -LiteralPath $path -Raw -ErrorAction SilentlyContinue).Trim().ToLowerInvariant()
    if ($value -in @('release', 'main')) {
        return $value
    }
    return $null
}

function Write-ChannelMarker([string]$Directory, [string]$Value) {
    Set-Content -LiteralPath (Join-Path $Directory $channelMarkerName) -Value $Value -Encoding Ascii
}

function Invoke-BootstrapSelfTest {
    $root = Join-Path ([System.IO.Path]::GetTempPath()) ("medusa-bootstrap-selftest-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $target = Join-Path $root 'medusa.exe'
    $staged = Join-Path $root 'medusa.bootstrap-new.exe'
    $process = $null
    $lock = $null
    try {
        $lock = Acquire-BootstrapLock $root
        $secondLockRejected = $false
        try {
            $other = Acquire-BootstrapLock $root
            $other.Dispose()
        }
        catch {
            $secondLockRejected = $true
        }
        if (-not $secondLockRejected) {
            throw 'self-test bootstrap lock did not serialize concurrent installers'
        }

        $ping = Join-Path $env:SystemRoot 'System32\PING.EXE'
        if (-not (Test-Path -LiteralPath $ping)) {
            throw "Windows ping executable is missing at $ping"
        }
        Copy-Item -LiteralPath $ping -Destination $target -Force
        Copy-Item -LiteralPath $env:ComSpec -Destination $staged -Force
        $replacementHash = (Get-FileHash -LiteralPath $staged -Algorithm SHA256).Hash
        $process = Start-Process -FilePath $target -ArgumentList @('127.0.0.1', '-t') -PassThru
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

        $legacyLock = Join-Path $root $legacyLockName
        Set-Content -LiteralPath $legacyLock -Value @(
            'schema=2',
            'parent_pid=999999',
            'helper_pid=999998'
        ) -Encoding Ascii
        Take-Over-LegacyUpdate $root $target
        if (Test-Path -LiteralPath $legacyLock) {
            throw 'self-test did not reclaim a stale legacy updater lock'
        }

        Write-Host 'Windows installer bootstrap self-test passed.'
    }
    finally {
        if ($null -ne $process) {
            Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        }
        if ($null -ne $lock) {
            $lock.Dispose()
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

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
$nativeCandidate = Get-NativeStagedCandidate $target
if ($Channel -eq 'auto') {
    $remembered = Read-ChannelMarker $InstallDir
    if ($null -ne $remembered) {
        $Channel = $remembered
    }
    elseif ($null -ne $nativeCandidate) {
        # A native staged candidate is explicit evidence that this installation
        # already requested the rolling-main updater. Do not infer channel from
        # the generic --version label, which stable builds also contain.
        $Channel = 'main'
    }
    else {
        $Channel = 'release'
    }
}

$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("medusa-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
$bootstrapLock = $null

try {
    $bootstrapLock = Acquire-BootstrapLock $InstallDir
    Take-Over-LegacyUpdate $InstallDir $target

    if ($Channel -eq 'main') {
        $nativeCandidate = Get-NativeStagedCandidate $target
        if ($null -eq $nativeCandidate) {
            throw 'No native-verified rolling-main candidate is staged. Run `medusa update` once, then rerun this installer with MEDUSA_INSTALL_CHANNEL=main if the legacy updater cannot complete the swap.'
        }
        $binaryPath = $nativeCandidate.Path
        $candidateVersion = $nativeCandidate.Version
    }
    else {
        $asset = 'medusa-cli-windows.zip'
        $archive = Join-Path $tempDir $asset
        Download-File "https://github.com/$repo/releases/latest/download/$asset" $archive 'Downloading Medusa'
        $extractDir = Join-Path $tempDir 'extract'
        Expand-Archive -LiteralPath $archive -DestinationPath $extractDir -Force
        $binaries = @(Get-ChildItem -Path $extractDir -Filter 'medusa.exe' -File -Recurse)
        if ($binaries.Count -ne 1) {
            throw "The archive must contain exactly one medusa.exe; found $($binaries.Count)."
        }
        $binaryPath = $binaries[0].FullName
        $candidateVersion = (& $binaryPath --version 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($candidateVersion)) {
            throw 'The downloaded Medusa executable failed its version probe.'
        }
    }

    $staged = Join-Path $InstallDir 'medusa.bootstrap-new.exe'
    Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath $binaryPath -Destination $staged -Force
    $candidateHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
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
    Write-ChannelMarker $InstallDir $Channel
    Add-InstallDirToPath $InstallDir

    $version = (& $target --version 2>$null | Out-String).Trim()
    if ($Channel -eq 'main') {
        Write-Host "Recovered $version"
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
    if ($null -ne $bootstrapLock) {
        $bootstrapLock.Dispose()
    }
    Remove-Item -LiteralPath (Join-Path $InstallDir $bootstrapLockName) -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
