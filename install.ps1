param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Medusa\bin"
)

$ErrorActionPreference = 'Stop'
$repo = 'benclawbot/Medusa'
$asset = 'medusa-cli-windows.zip'
$url = "https://github.com/$repo/releases/latest/download/$asset"
$tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("medusa-install-" + [Guid]::NewGuid().ToString('N'))
$archive = Join-Path $tempDir $asset

New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
try {
    Write-Host 'Downloading Medusa...'
    $client = [System.Net.Http.HttpClient]::new()
    try {
        $response = $client.GetAsync($url, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode()
        $total = $response.Content.Headers.ContentLength
        $input = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output = [System.IO.File]::Create($archive)
        try {
            $buffer = New-Object byte[] (1024 * 128)
            $readTotal = 0L
            while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                $output.Write($buffer, 0, $read)
                $readTotal += $read
                if ($total -and $total -gt 0) {
                    $percent = [Math]::Min(100, [int](($readTotal * 100) / $total))
                    Write-Progress -Activity 'Installing Medusa' -Status "$percent%" -PercentComplete $percent
                }
            }
        }
        finally {
            $output.Dispose()
            $input.Dispose()
            Write-Progress -Activity 'Installing Medusa' -Completed
        }
    }
    finally {
        $client.Dispose()
    }

    Expand-Archive -LiteralPath $archive -DestinationPath $tempDir -Force
    $binary = Get-ChildItem -Path $tempDir -Filter 'medusa.exe' -File -Recurse | Select-Object -First 1
    if (-not $binary) {
        throw 'The release archive did not contain medusa.exe.'
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $target = Join-Path $InstallDir 'medusa.exe'
    Copy-Item -LiteralPath $binary.FullName -Destination $target -Force

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $pathEntries = @($userPath -split ';' | Where-Object { $_ })
    if ($pathEntries -notcontains $InstallDir) {
        $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) { $InstallDir } else { "$InstallDir;$userPath" }
        [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
    }
    if (($env:Path -split ';') -notcontains $InstallDir) {
        $env:Path = "$InstallDir;$env:Path"
    }

    $version = & $target --version 2>$null
    Write-Host "Installed $version"
    Write-Host 'Launching Medusa...'
    Write-Host ''
    Start-Process -FilePath $target -NoNewWindow -Wait
}
finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
