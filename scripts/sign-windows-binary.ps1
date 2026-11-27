<#
.SYNOPSIS
Signs and verifies Windows PE binaries (e.g. lucidd.exe) with Azure Artifact Signing.

.DESCRIPTION
Shared by GitHub Actions (.github/workflows/release.yml) and local Windows maintainer runs.
Contains zero hardcoded credentials, tenants, endpoints, accounts, or certificate profiles.

Required environment variables (unless -VerifyOnly is set):
  LUCID_AZURE_SIGN_ENDPOINT   Regional Artifact Signing endpoint (https://<region>.codesigning.azure.net/)
  LUCID_AZURE_SIGN_ACCOUNT    Azure Artifact Signing account name
  LUCID_AZURE_SIGN_PROFILE    Certificate profile name (Public Trust)

Authentication uses DefaultAzureCredential restricted to:
  - EnvironmentCredential     (AZURE_TENANT_ID, AZURE_CLIENT_ID, AZURE_CLIENT_SECRET)
  - WorkloadIdentityCredential / AzureCliCredential (GitHub OIDC azure/login@v2 or local `az login`)

NuGet signing tools are downloaded into a temporary/cache directory outside the repository and
verified against pinned SHA-256 checksums before execution.

.EXAMPLE
  $env:LUCID_AZURE_SIGN_ENDPOINT = 'https://<region>.codesigning.azure.net/'
  $env:LUCID_AZURE_SIGN_ACCOUNT  = '<signing-account>'
  $env:LUCID_AZURE_SIGN_PROFILE  = '<cert-profile>'
  az login
  pwsh -NoProfile -File scripts\sign-windows-binary.ps1 -Path target\x86_64-pc-windows-msvc\release\lucidd.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string[]]$Path,

    [string]$ToolsDir = '',

    [switch]$VerifyOnly,

    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Add-Type -AssemblyName System.IO.Compression.FileSystem

$TimestampUrl = 'http://timestamp.acs.microsoft.com'

# Pinned NuGet packages checked by SHA-256 of their .nupkg before extraction.
$SdkBuildTools = @{
    Id      = 'Microsoft.Windows.SDK.BuildTools'
    Version = '10.0.28000.2705'
    Sha256  = '8BFDFB6CA2633F531CF80B5FA22512BA61A394D7988F0970DB83BAADC67929ED'
}
$SigningClient = @{
    Id      = 'Microsoft.ArtifactSigning.Client'
    Version = '1.0.128'
    Sha256  = '74BD7D27E6CE1051409C38D9B46BC8DF0400ECD643D51FFBF2AC00869061E40B'
}

if (-not $ToolsDir) {
    $baseTemp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
    $ToolsDir = Join-Path $baseTemp 'lucid-signing-tools'
}

function Write-Step([string]$Message) {
    Write-Host ''
    Write-Host "==> $Message"
}

function Get-FileSha256([string]$FilePath) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $FilePath).Hash.ToUpperInvariant()
}

function Invoke-Native([string]$Exe, [string[]]$Arguments) {
    & $Exe @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) {
        throw "$Exe failed (exit code $LASTEXITCODE)"
    }
}

function Invoke-Captured([string]$Exe, [string[]]$Arguments) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        return (& $Exe @Arguments 2>&1 | ForEach-Object { "$_" }) -join "`n"
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Write-Utf8NoBom([string]$FilePath, [string]$Text) {
    New-Item -ItemType Directory -Force (Split-Path -Parent $FilePath) | Out-Null
    $normalized = $Text.Replace("`r`n", "`n")
    if (-not $normalized.EndsWith("`n")) { $normalized += "`n" }
    [System.IO.File]::WriteAllText($FilePath, $normalized, (New-Object System.Text.UTF8Encoding $false))
}

function Get-PinnedNuGetPackage([hashtable]$Package) {
    $folder = Join-Path $ToolsDir "$($Package.Id).$($Package.Version)"
    $marker = Join-Path $folder '.verified'
    if (Test-Path -LiteralPath $marker) {
        $recorded = (Get-Content -LiteralPath $marker -Raw).Trim()
        if ($recorded -eq $Package.Sha256) {
            return $folder
        }
    }

    New-Item -ItemType Directory -Force -Path $ToolsDir | Out-Null
    $id = $Package.Id.ToLowerInvariant()
    $nupkg = Join-Path $ToolsDir "$id.$($Package.Version).nupkg"
    $url = "https://api.nuget.org/v3-flatcontainer/$id/$($Package.Version)/$id.$($Package.Version).nupkg"
    Write-Host "Downloading $($Package.Id) $($Package.Version)..."
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $nupkg

    $actual = Get-FileSha256 $nupkg
    if ($actual -ne $Package.Sha256) {
        Remove-Item -LiteralPath $nupkg -Force
        throw "$($Package.Id) $($Package.Version) SHA-256 mismatch: got $actual, expected $($Package.Sha256)"
    }

    if (Test-Path -LiteralPath $folder) {
        Remove-Item -LiteralPath $folder -Recurse -Force
    }
    [System.IO.Compression.ZipFile]::ExtractToDirectory($nupkg, $folder)
    Remove-Item -LiteralPath $nupkg -Force
    Set-Content -LiteralPath $marker -Value $actual -NoNewline
    return $folder
}

function Test-X64DotNet8OrLater {
    $roots = @()
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
        $roots += Join-Path $env:ProgramFiles 'dotnet\x64'
        $registered = Get-ItemProperty 'HKLM:\SOFTWARE\dotnet\Setup\InstalledVersions\x64' -Name InstallLocation -ErrorAction SilentlyContinue
        if ($registered -and $registered.InstallLocation) {
            $roots += $registered.InstallLocation
        }
    } else {
        $roots += Join-Path $env:ProgramFiles 'dotnet'
    }

    foreach ($root in ($roots | Select-Object -Unique)) {
        $shared = Join-Path $root 'shared\Microsoft.NETCore.App'
        if (Test-Path -LiteralPath $shared) {
            foreach ($verDir in Get-ChildItem -LiteralPath $shared -Directory) {
                $major = 0
                if ([int]::TryParse($verDir.Name.Split('.')[0], [ref]$major) -and $major -ge 8) {
                    return $true
                }
            }
        }
    }
    return $false
}

function Assert-SignedBinary([string]$SignToolPath, [string]$BinaryPath) {
    $sig = Get-AuthenticodeSignature -LiteralPath $BinaryPath
    if ($null -eq $sig -or $null -eq $sig.SignerCertificate) {
        throw "Authenticode signature missing on $BinaryPath"
    }
    if ($sig.Status -ne 'Valid') {
        throw "Authenticode signature invalid on ${BinaryPath}: $($sig.Status) ($($sig.StatusMessage))"
    }
    if ($null -eq $sig.TimeStamperCertificate) {
        throw "RFC 3161 timestamp certificate missing on $BinaryPath (required for short-lived Artifact Signing certificates)"
    }

    $verifyOut = Invoke-Captured $SignToolPath @('verify', '/pa', '/v', $BinaryPath)
    if ($LASTEXITCODE -ne 0) {
        Write-Host $verifyOut
        throw "signtool verify /pa failed for $BinaryPath"
    }
    if ($verifyOut -notmatch 'timestamp') {
        Write-Host $verifyOut
        throw "signtool verify output did not confirm a timestamp on $BinaryPath"
    }

    Write-Host "Verified Authenticode + timestamp: $BinaryPath"
    Write-Host "  Thumbprint: $($sig.SignerCertificate.Thumbprint)"
}

# ── Validate target files ─────────────────────────────────────────────────────

$resolvedPaths = @()
foreach ($item in $Path) {
    if (-not (Test-Path -LiteralPath $item -PathType Leaf)) {
        throw "Target binary not found: $item"
    }
    $resolvedPaths += (Resolve-Path -LiteralPath $item).ProviderPath
}

if (-not $VerifyOnly) {
    foreach ($varName in @('LUCID_AZURE_SIGN_ENDPOINT', 'LUCID_AZURE_SIGN_ACCOUNT', 'LUCID_AZURE_SIGN_PROFILE')) {
        $val = [Environment]::GetEnvironmentVariable($varName)
        if ([string]::IsNullOrWhiteSpace($val)) {
            throw "Missing required environment variable $varName for Azure Artifact Signing."
        }
    }
    $endpoint = $env:LUCID_AZURE_SIGN_ENDPOINT.Trim()
    if (-not $endpoint.StartsWith('https://')) {
        throw 'LUCID_AZURE_SIGN_ENDPOINT must start with https://'
    }
    if (-not (Test-X64DotNet8OrLater)) {
        throw 'Azure Artifact Signing dlib requires x64 .NET Runtime 8 or later (install Microsoft.DotNet.Runtime.8 --architecture x64).'
    }
}

# ── Resolve pinned tools ──────────────────────────────────────────────────────

Write-Step 'Resolving SHA-256-pinned signing tools'
$sdkRoot = Get-PinnedNuGetPackage $SdkBuildTools
$sdkBinDir = Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'bin') -Directory | Sort-Object Name -Descending | Select-Object -First 1
if ($null -eq $sdkBinDir) {
    throw "Could not locate SDK bin directory under $sdkRoot"
}
# Artifact Signing dlib is x64-only; x64 signtool runs natively on x64 and under emulation on ARM64.
$signTool = Join-Path $sdkBinDir.FullName 'x64\signtool.exe'
if (-not (Test-Path -LiteralPath $signTool -PathType Leaf)) {
    throw "signtool.exe not found at $signTool"
}

$dlib = ''
if (-not $VerifyOnly) {
    $clientRoot = Get-PinnedNuGetPackage $SigningClient
    $dlib = Join-Path $clientRoot 'bin\x64\Azure.CodeSigning.Dlib.dll'
    if (-not (Test-Path -LiteralPath $dlib -PathType Leaf)) {
        throw "Azure.CodeSigning.Dlib.dll not found at $dlib"
    }
}

if ($VerifyOnly) {
    Write-Step 'Verifying existing signatures'
    foreach ($bin in $resolvedPaths) {
        Assert-SignedBinary -SignToolPath $signTool -BinaryPath $bin
    }
    exit 0
}

if ($DryRun) {
    Write-Step 'Dry run complete'
    Write-Host "Tools verified and environment checks passed for $($resolvedPaths.Count) file(s); skipping SignTool invocation."
    exit 0
}

# ── Sign and verify ───────────────────────────────────────────────────────────

$workDir = Join-Path ([System.IO.Path]::GetTempPath()) ("lucid-sign-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $workDir | Out-Null

try {
    $metadataFile = Join-Path $workDir 'artifact-signing.json'
    $metadata = [ordered]@{
        Endpoint               = $env:LUCID_AZURE_SIGN_ENDPOINT.Trim()
        CodeSigningAccountName = $env:LUCID_AZURE_SIGN_ACCOUNT.Trim()
        CertificateProfileName = $env:LUCID_AZURE_SIGN_PROFILE.Trim()
        ExcludeCredentials     = @(
            'ManagedIdentityCredential',
            'SharedTokenCacheCredential',
            'VisualStudioCredential',
            'VisualStudioCodeCredential',
            'AzurePowerShellCredential',
            'AzureDeveloperCliCredential',
            'InteractiveBrowserCredential'
        )
    } | ConvertTo-Json -Depth 4
    Write-Utf8NoBom -FilePath $metadataFile -Text $metadata

    Write-Step "Signing $($resolvedPaths.Count) binary file(s) with Azure Artifact Signing"
    foreach ($bin in $resolvedPaths) {
        Write-Host "Signing $bin"
        Invoke-Native $signTool @(
            'sign', '/v',
            '/fd', 'SHA256',
            '/tr', $TimestampUrl,
            '/td', 'SHA256',
            '/dlib', $dlib,
            '/dmdf', $metadataFile,
            $bin
        )
        Assert-SignedBinary -SignToolPath $signTool -BinaryPath $bin
    }
} finally {
    if (Test-Path -LiteralPath $workDir) {
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
