[CmdletBinding()]
# Windows PowerShell 5.1 requires this UTF-8 script to be saved with a BOM.
param(
    [string]$Version,
    [string]$PackagePath,
    [string]$InstallDir,
    [ValidateSet("Auto", "Core", "Portable")]
    [string]$Variant = "Auto",
    [switch]$NoModifyPath,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$AppSlug = "bulibuli"
$Repo = if ($env:BULIBULI_REPO) { $env:BULIBULI_REPO } else { "Wong0728/bulibuli" }
$ManifestName = "bulibuli.package.json"
$Architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$Architecture = switch ($Architecture.ToUpperInvariant()) {
    "AMD64" { "x86_64"; break }
    "X86_64" { "x86_64"; break }
    "ARM64" { "arm64"; break }
    default { throw "不支持的 Windows CPU 架构：$Architecture" }
}
$defaultInstallBase = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { Join-Path $env:USERPROFILE ".local\share" }
$InstallDir = if ($InstallDir) {
    [IO.Path]::GetFullPath($InstallDir)
} else {
    Join-Path $defaultInstallBase $AppSlug
}
$script:TempRoot = Join-Path ([IO.Path]::GetTempPath()) ("bulibuli-install-" + [guid]::NewGuid().ToString("N"))

function Write-Info([string]$Message) { Write-Host "[bulibuli] $Message" -ForegroundColor Green }
function Write-Warn([string]$Message) { Write-Host "[warn] $Message" -ForegroundColor Yellow }

function Normalize-Version([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value) -or $Value -eq "latest") { return $null }
    if ($Value.StartsWith("v")) { return $Value }
    return "v$Value"
}

function Resolve-Version {
    $explicit = Normalize-Version $Version
    if ($explicit) { return $explicit }
    $uri = "https://github.com/$Repo/releases/latest/download/latest.json"
    try {
        $latest = Invoke-RestMethod -Uri $uri -Headers @{ Accept = "application/json" }
        $resolved = Normalize-Version ([string]$latest.version)
        if ($resolved) { return $resolved }
    } catch { }
    try {
        $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases?per_page=20" -Headers @{ Accept = "application/vnd.github+json" }
        $release = @($releases | Where-Object { -not $_.draft } | Select-Object -First 1)
        $resolved = Normalize-Version ([string]$release.tag_name)
        if ($resolved) { return $resolved }
        throw "发布列表缺少可用版本"
    } catch {
        throw "无法读取 Release 发布清单；请使用 -Version vX.Y.Z 固定版本重试。$($_.Exception.Message)"
    }
}

function Get-PackageRoot([string]$Path) {
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    if (Test-Path -LiteralPath $resolved -PathType Leaf) {
        if ([IO.Path]::GetExtension($resolved) -ne ".zip") { throw "PackagePath 必须是目录或 zip 归档：$resolved" }
        New-Item -ItemType Directory -Path $script:TempRoot -Force | Out-Null
        $unpack = Join-Path $script:TempRoot "package"
        Expand-Archive -LiteralPath $resolved -DestinationPath $unpack -Force
        $resolved = $unpack
    }
    $candidates = @((Get-Item -LiteralPath $resolved))
    $candidates += @(Get-ChildItem -LiteralPath $resolved -Directory -Force -ErrorAction SilentlyContinue)
    foreach ($candidate in $candidates) {
        $manifest = Join-Path $candidate.FullName $ManifestName
        if (Test-Path -LiteralPath $manifest -PathType Leaf) { return $candidate.FullName }
    }
    throw "目录或归档中没有 $ManifestName：$Path"
}

function Assert-Sha256([string]$File, [string]$Manifest) {
    $expected = ((Get-Content -LiteralPath $Manifest -Raw).Trim() -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expected -notmatch "^[0-9a-f]{64}$" -or $expected -ne $actual) {
        throw "SHA-256 校验失败：$File"
    }
}

function Read-Package([string]$Path) {
    $root = Get-PackageRoot $Path
    $manifestPath = Join-Path $root $ManifestName
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($manifest.schema_version -ne 1) { throw "不支持的 package manifest schema：$($manifest.schema_version)" }
    if ($manifest.platform -ne "windows" -or $manifest.architecture -ne $Architecture) {
        throw "包平台或架构不匹配：$($manifest.platform)/$($manifest.architecture)，目标 windows/$Architecture"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $root "bulibuli.exe") -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $root "static\index.html") -PathType Leaf)) {
        throw "包缺少 bulibuli.exe 或 static/index.html"
    }
    $rootPrefix = ([IO.Path]::GetFullPath($root)).TrimEnd("\") + "\"
    foreach ($entry in @($manifest.files)) {
        $relative = ([string]$entry.path).Replace("/", "\")
        if ([IO.Path]::IsPathRooted($relative)) { throw "manifest 含绝对路径：$relative" }
        $file = [IO.Path]::GetFullPath((Join-Path $root $relative))
        if (-not $file.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "manifest 路径越界：$relative"
        }
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "包缺少文件：$relative" }
        $actual = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne ([string]$entry.sha256).ToLowerInvariant()) { throw "包内文件校验失败：$relative" }
    }
    return [pscustomobject]@{ Root = $root; Manifest = $manifest }
}

function Assert-Package([object]$Package, [string]$ExpectedVersion, [string]$ExpectedVariant = "") {
    $actualVersion = Normalize-Version ([string]$Package.Manifest.app_version)
    if ($actualVersion -ne $ExpectedVersion) { throw "包版本不匹配：$actualVersion，目标 $ExpectedVersion" }
    if ($ExpectedVariant -and $Package.Manifest.variant -ne $ExpectedVariant.ToLowerInvariant()) {
        throw "包类型不匹配：$($Package.Manifest.variant)，目标 $($ExpectedVariant.ToLowerInvariant())"
    }
}

function Resolve-Tool([string]$Name, [string[]]$Variables) {
    foreach ($variable in $Variables) {
        $value = [Environment]::GetEnvironmentVariable($variable)
        if ([string]::IsNullOrWhiteSpace($value)) { continue }
        if (Test-Path -LiteralPath $value -PathType Leaf) { return (Resolve-Path -LiteralPath $value).Path }
        $candidate = Join-Path $value $Name
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { return (Resolve-Path -LiteralPath $candidate).Path }
    }
    $command = Get-Command ($Name -replace "\.exe$", "") -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    return $null
}

function Test-Executable([string]$Path, [string]$Argument) {
    if (-not $Path) { return $false }
    try {
        & $Path $Argument *> $null
        return $LASTEXITCODE -eq 0
    } catch { return $false }
}

function Get-SystemRuntime {
    $aria2 = Resolve-Tool "aria2c.exe" @("ARIA2C_PATH")
    $ffmpeg = Resolve-Tool "ffmpeg.exe" @("FFMPEG_PATH", "FFMPEG", "FF_PATH", "FFMPEG_HOME", "FFMPEG_DIR")
    if ((Test-Executable $aria2 "-v") -and (Test-Executable $ffmpeg "-version")) {
        return [pscustomobject]@{ Aria2 = $aria2; Ffmpeg = $ffmpeg }
    }
    return $null
}

function Test-PackageRuntime([object]$Package) {
    $resources = Join-Path $Package.Root "resources"
    $aria2 = Join-Path $resources "aria2c.exe"
    $ffmpeg = Join-Path $resources "ffmpeg.exe"
    if ((Test-Path -LiteralPath $aria2 -PathType Leaf) -and (Test-Path -LiteralPath $ffmpeg -PathType Leaf) -and
        (Test-Executable $aria2 "-v") -and (Test-Executable $ffmpeg "-version")) {
        return $true
    }
    return $false
}

function Write-FileChecksum([string]$File) {
    $hash = (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "$File.sha256" -Value "$hash  $([IO.Path]::GetFileName($File))" -Encoding ascii
}

function Write-PackageManifest([string]$Root, [string]$Variant) {
    $files = @()
    foreach ($file in Get-ChildItem -LiteralPath $Root -File -Recurse -Force) {
        if ($file.Name -eq $ManifestName) { continue }
        $relative = $file.FullName.Substring($Root.Length).TrimStart("\").Replace("\", "/")
        $files += [ordered]@{
            path = $relative
            size = $file.Length
            sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $runtime = Join-Path $Root "resources"
    $manifest = [ordered]@{
        schema_version = 1
        app_version = (Get-Content -LiteralPath (Join-Path $Root $ManifestName) -Raw | ConvertFrom-Json).app_version
        platform = "windows"
        architecture = $Architecture
        variant = $Variant.ToLowerInvariant()
        files = $files
        runtime = [ordered]@{
            aria2c = Test-Path -LiteralPath (Join-Path $runtime "aria2c.exe") -PathType Leaf
            ffmpeg = Test-Path -LiteralPath (Join-Path $runtime "ffmpeg.exe") -PathType Leaf
            ffprobe = Test-Path -LiteralPath (Join-Path $runtime "ffprobe.exe") -PathType Leaf
        }
    }
    $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $Root $ManifestName) -Encoding utf8
}

function Download-Package([string]$TargetVersion, [string]$TargetVariant) {
    New-Item -ItemType Directory -Path $script:TempRoot -Force | Out-Null
    $name = "$AppSlug-windows-$Architecture-$($TargetVariant.ToLowerInvariant())-$TargetVersion.zip"
    $archive = Join-Path $script:TempRoot $name
    $checksum = "$archive.sha256"
    $base = "https://github.com/$Repo/releases/download/$TargetVersion"
    Write-Info "下载 $name"
    Invoke-WebRequest -Uri "$base/$name" -OutFile $archive
    Invoke-WebRequest -Uri "$base/$name.sha256" -OutFile $checksum
    Assert-Sha256 $archive $checksum
    $unpack = Join-Path $script:TempRoot ("unpacked-" + $TargetVariant.ToLowerInvariant())
    Expand-Archive -LiteralPath $archive -DestinationPath $unpack -Force
    $package = Read-Package $unpack
    Assert-Package $package $TargetVersion $TargetVariant
    return $package
}

function Copy-LocalRuntime([object]$Source, [object]$Target) {
    $sourceResources = Join-Path $Source.Root "resources"
    $targetResources = Join-Path $Target.Root "resources"
    New-Item -ItemType Directory -Path $targetResources -Force | Out-Null
    foreach ($name in @("aria2c.exe", "ffmpeg.exe")) {
        $sourceFile = Join-Path $sourceResources $name
        if (-not (Test-Path -LiteralPath $sourceFile -PathType Leaf)) { throw "本地运行时缺少 $name" }
        Copy-Item -LiteralPath $sourceFile -Destination (Join-Path $targetResources $name) -Force
        Write-FileChecksum (Join-Path $targetResources $name)
    }
    Write-PackageManifest $Target.Root "Portable"
    $updated = Read-Package $Target.Root
    Assert-Package $updated (Normalize-Version ([string]$Target.Manifest.app_version)) "Portable"
    return $updated
}

function Install-Package([object]$Package) {
    $source = [IO.Path]::GetFullPath($Package.Root).TrimEnd("\")
    $destination = [IO.Path]::GetFullPath($InstallDir).TrimEnd("\")
    if ($source -eq $destination) { return }
    if ((Test-Path -LiteralPath $destination) -and (Get-ChildItem -LiteralPath $destination -Force | Select-Object -First 1) -and -not $Force) {
        throw "安装目录已存在；升级或覆盖请显式指定 -Force：$destination"
    }
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $source -Force) {
        if ($item.Name -eq "data") { continue }
        Copy-Item -LiteralPath $item.FullName -Destination (Join-Path $destination $item.Name) -Recurse -Force
    }
    New-Item -ItemType Directory -Path (Join-Path $destination "data") -Force | Out-Null
}

function Add-UserPath([string]$Path) {
    $current = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @($current -split ";" | Where-Object { $_ })
    if ($entries | Where-Object { $_.TrimEnd("\") -ieq $Path.TrimEnd("\") }) { return $false }
    [Environment]::SetEnvironmentVariable("Path", (($entries + $Path) -join ";"), "User")
    return $true
}

try {
    $explicitPackage = $null
    if ($PackagePath) {
        if (-not (Test-Path -LiteralPath $PackagePath)) { throw "PackagePath 不存在：$PackagePath" }
        if ([IO.Path]::GetExtension((Resolve-Path -LiteralPath $PackagePath).Path) -eq ".zip") {
            $packagePathResolved = (Resolve-Path -LiteralPath $PackagePath).Path
            $checksum = "$packagePathResolved.sha256"
            if (-not (Test-Path -LiteralPath $checksum)) { throw "本地归档缺少同名 .sha256：$checksum" }
            Assert-Sha256 $packagePathResolved $checksum
        }
        $explicitPackage = Read-Package $PackagePath
    }

    $targetVersion = if ($explicitPackage -and -not (Normalize-Version $Version)) {
        Normalize-Version ([string]$explicitPackage.Manifest.app_version)
    } else { Resolve-Version }
    $localCandidates = @($explicitPackage)
    foreach ($candidatePath in @($InstallDir, $PSScriptRoot)) {
        if (-not $candidatePath -or -not (Test-Path -LiteralPath $candidatePath -PathType Container)) { continue }
        try { $localCandidates += Read-Package $candidatePath } catch { }
    }

    $selected = $null
    $runtimeSource = "系统 PATH/环境变量"
    foreach ($candidate in $localCandidates) {
        if (-not $candidate) { continue }
        if ($Variant -ne "Auto" -and $candidate.Manifest.variant -ne $Variant.ToLowerInvariant()) { continue }
        try { Assert-Package $candidate $targetVersion } catch { continue }
        if ($candidate -eq $explicitPackage -and $candidate.Manifest.variant -eq "core" -and (Get-SystemRuntime)) {
            $selected = $candidate
            $runtimeSource = "系统 PATH/环境变量"
            break
        }
        if ($candidate.Manifest.variant -eq "portable" -and (Test-PackageRuntime $candidate)) {
            $selected = $candidate
            $runtimeSource = "本地完整包"
            break
        }
        if (-not $runtimeSource -or $runtimeSource -eq "系统 PATH/环境变量") {
            if (Test-PackageRuntime $candidate) {
                $runtimeSource = "本地运行时"
            }
        }
    }

    if (-not $selected) {
        $systemRuntime = Get-SystemRuntime
        $localRuntime = $localCandidates | Where-Object { $_ -and (Test-PackageRuntime $_) } | Select-Object -First 1
        $canUseRuntime = $localRuntime -or $systemRuntime
        $requestedVariant = if ($Variant -eq "Auto") {
            if ($canUseRuntime) { "Core" } else { "Portable" }
        } else { $Variant }
        if ($requestedVariant -eq "Core" -and $localRuntime -and $localRuntime.Manifest.variant -eq "core" -and
            (Normalize-Version ([string]$localRuntime.Manifest.app_version)) -eq $targetVersion) {
            $selected = $localRuntime
            $runtimeSource = "本地运行时"
        } else {
            try {
                $selected = Download-Package $targetVersion $requestedVariant
            } catch {
                if ($requestedVariant -ne "Core" -or $Variant -eq "Core") { throw }
                Write-Warn "该 Release 没有可用的 Windows core，回退完整 portable 包"
                $selected = Download-Package $targetVersion "Portable"
                $requestedVariant = "Portable"
            }
            if ($requestedVariant -eq "Core" -and $localRuntime) {
                $selected = Copy-LocalRuntime $localRuntime $selected
                $runtimeSource = "本地运行时"
            } elseif ($requestedVariant -eq "Core") {
                $runtimeSource = "系统 PATH/环境变量"
            } else {
                $runtimeSource = "portable 包内置"
            }
        }
    }

    Install-Package $selected
    if (-not $NoModifyPath) {
        if (Add-UserPath $InstallDir) {
            Write-Info "已加入用户级 PATH；请重新打开 PowerShell 或 CMD。"
        }
    }
    $installedManifest = Join-Path $InstallDir $ManifestName
    $installed = if (Test-Path -LiteralPath $installedManifest) { Get-Content -LiteralPath $installedManifest -Raw | ConvertFrom-Json } else { $selected.Manifest }
    Write-Info "安装完成：$InstallDir"
    Write-Info "版本：$($installed.app_version)；包类型：$($installed.variant)；运行时来源：$runtimeSource"
    Write-Info "可运行：bulibuli --version / bulibuli.exe --version"
} finally {
    if (Test-Path -LiteralPath $script:TempRoot) { Remove-Item -LiteralPath $script:TempRoot -Recurse -Force -ErrorAction SilentlyContinue }
}
