# SPDX-License-Identifier: Apache-2.0

[CmdletBinding()]
param(
    [string]$Repo = $(if ($env:PHASE_RELEASE_REPO) { $env:PHASE_RELEASE_REPO } else { "msitarzewski/phase" }),
    [string]$Tag = $(if ($env:PHASE_RELEASE_TAG) { $env:PHASE_RELEASE_TAG } else { "latest" }),
    [string]$FromDir = "",
    [string]$InstallDir = $(if ($env:LOCALAPPDATA) { Join-Path $env:LOCALAPPDATA "LUCID\bin" } else { throw "LOCALAPPDATA is not set" })
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Target = "x86_64-pc-windows-msvc"

function Invoke-LucidDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$Destination
    )

    Invoke-WebRequest `
        -Uri $Uri `
        -OutFile $Destination `
        -Headers @{
            Accept = "application/vnd.github+json, application/octet-stream"
            "User-Agent" = "LUCID-Installer"
        }
}

function Resolve-LucidTag {
    param([Parameter(Mandatory = $true)][string]$Repository)

    Write-Host "Resolving the latest published LUCID release..."
    $releases = @(
        Invoke-RestMethod `
            -Uri "https://api.github.com/repos/$Repository/releases?per_page=1" `
            -Headers @{
                Accept = "application/vnd.github+json"
                "User-Agent" = "LUCID-Installer"
            }
    )
    $release = $releases | Where-Object { -not $_.draft } | Select-Object -First 1
    if ($null -eq $release -or [string]::IsNullOrWhiteSpace([string]$release.tag_name)) {
        throw "GitHub did not return a latest release tag for $Repository"
    }
    return [string]$release.tag_name
}

function Get-ExpectedHash {
    param(
        [Parameter(Mandatory = $true)][string]$SumsFile,
        [Parameter(Mandatory = $true)][string]$FileName
    )

    $checksumMatches = @()
    foreach ($line in Get-Content -LiteralPath $SumsFile) {
        if ($line -match "^(?<hash>[0-9a-fA-F]{64})\s+\*?(?<file>.+?)\s*$") {
            if ($Matches.file -eq $FileName) {
                $checksumMatches += $Matches.hash.ToLowerInvariant()
            }
        }
    }
    if ($checksumMatches.Count -ne 1) {
        throw "Expected exactly one SHA-256 entry for $FileName in SHA256SUMS.txt; found $($checksumMatches.Count)"
    }
    return $checksumMatches[0]
}

function Assert-SafeArchivePaths {
    param([Parameter(Mandatory = $true)][string]$Archive)

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace("\", "/")
            if (
                $name.StartsWith("/") -or
                $name.StartsWith("../") -or
                $name -match "(^|/)\.\.(/|$)" -or
                $name -match "^[A-Za-z]:/"
            ) {
                throw "Archive contains an unsafe path: $($entry.FullName)"
            }
        }
    }
    finally {
        $zip.Dispose()
    }
}

if ($Repo -notmatch "^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$") {
    throw "Repo must be in OWNER/REPO form"
}
if ([string]::IsNullOrWhiteSpace($Tag)) {
    throw "Tag cannot be empty"
}
if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [System.Runtime.InteropServices.OSPlatform]::Windows
    )) {
    throw "This installer supports Windows only"
}
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne
        [System.Runtime.InteropServices.Architecture]::X64) {
    throw "This installer supports Windows x86_64 only"
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("lucid-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

try {
    $sumsFile = Join-Path $tempRoot "SHA256SUMS.txt"
    if ($FromDir) {
        Copy-Item -LiteralPath (Join-Path $FromDir "SHA256SUMS.txt") -Destination $sumsFile
    }
    else {
        if ($Tag -eq "latest") {
            $Tag = Resolve-LucidTag -Repository $Repo
        }
        if ($Tag -notmatch "^[A-Za-z0-9][A-Za-z0-9._-]*$") {
            throw "Release tag contains unsupported characters: $Tag"
        }
        $baseUrl = "https://github.com/$Repo/releases/download/$Tag"
        Invoke-LucidDownload -Uri "$baseUrl/SHA256SUMS.txt" -Destination $sumsFile
    }

    $archiveName = @(
        Get-Content -LiteralPath $sumsFile |
            ForEach-Object {
                if ($_ -match "^[0-9a-fA-F]{64}\s+\*?(?<file>.+?)\s*$") {
                    $Matches.file
                }
            } |
            Where-Object { $_ -match ("^lucidd-.+-" + [regex]::Escape($Target) + "\.zip$") }
    )
    if ($archiveName.Count -ne 1) {
        throw "Expected exactly one $Target archive in SHA256SUMS.txt; found $($archiveName.Count)"
    }
    $archiveName = $archiveName[0]
    if (
        [System.IO.Path]::GetFileName($archiveName) -ne $archiveName -or
        $archiveName.Contains("/") -or
        $archiveName.Contains("\")
    ) {
        throw "Archive name in SHA256SUMS.txt is not a plain file name"
    }

    $archivePath = Join-Path $tempRoot $archiveName
    if ($FromDir) {
        Copy-Item -LiteralPath (Join-Path $FromDir $archiveName) -Destination $archivePath
    }
    else {
        Write-Host "Downloading $archiveName from $Repo release $Tag..."
        Invoke-LucidDownload -Uri "$baseUrl/$archiveName" -Destination $archivePath
    }

    $expectedHash = Get-ExpectedHash -SumsFile $sumsFile -FileName $archiveName
    $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $expectedHash) {
        throw "SHA-256 mismatch for $archiveName"
    }
    Write-Host "Verified SHA-256 ($archiveName): $actualHash"

    Assert-SafeArchivePaths -Archive $archivePath
    $stage = Join-Path $tempRoot "unpack"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $stage
    $executables = @(Get-ChildItem -LiteralPath $stage -Recurse -File -Filter "lucidd.exe")
    if ($executables.Count -ne 1) {
        throw "Expected exactly one lucidd.exe in the verified archive; found $($executables.Count)"
    }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $destination = Join-Path $InstallDir "lucidd.exe"
    Copy-Item -LiteralPath $executables[0].FullName -Destination $destination -Force
    & $destination --help | Out-Null

    $processPathEntries = @($env:Path -split ";")
    if ($processPathEntries -notcontains $InstallDir) {
        $env:Path = $InstallDir + ";" + $env:Path
    }
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $userPathEntries = @()
    if (-not [string]::IsNullOrWhiteSpace($userPath)) {
        $userPathEntries = @($userPath -split ";")
    }
    if ($userPathEntries -notcontains $InstallDir) {
        $newUserPath = @($userPathEntries + $InstallDir) |
            Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
            Select-Object -Unique
        [Environment]::SetEnvironmentVariable(
            "Path",
            ($newUserPath -join ";"),
            "User"
        )
    }

    Write-Host "Installed: $destination"
    Write-Host "Open a new terminal, then run: lucidd --help"
}
finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
