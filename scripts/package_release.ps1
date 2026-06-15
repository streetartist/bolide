param(
    [string]$Version = "0.12.1",
    [string]$LinuxTarget = "x86_64-unknown-linux-gnu",
    [string]$DistDir = "dist",
    [switch]$SkipMsi,
    [switch]$SkipWindows,
    [switch]$SkipLinux
)

$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$DistRoot = Join-Path $Root $DistDir

function Require-Command {
    param([string]$Name, [string]$Hint)
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "Required command '$Name' was not found. $Hint"
    }
}

function Invoke-Checked {
    param(
        [string]$Command,
        [string[]]$Arguments
    )
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $Command $($Arguments -join ' ')"
    }
}

function Remove-Dir {
    param([string]$Path)
    if (Test-Path $Path) {
        Remove-Item -LiteralPath $Path -Recurse -Force
    }
}

function Copy-Std {
    param([string]$Destination)
    Copy-Item -LiteralPath (Join-Path $Root "std") -Destination $Destination -Recurse -Force
}

function Write-Utf8NoBom {
    param([string]$Path, [string]$Value)
    $Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Value, $Utf8NoBom)
}

function Xml-Escape {
    param([string]$Text)
    return [System.Security.SecurityElement]::Escape($Text)
}

function New-StableId {
    param([string]$Prefix, [string]$Value)
    $Md5 = [System.Security.Cryptography.MD5]::Create()
    try {
        $Bytes = [System.Text.Encoding]::UTF8.GetBytes($Value.ToLowerInvariant())
        $Hash = $Md5.ComputeHash($Bytes)
        $Hex = -join ($Hash | ForEach-Object { $_.ToString("x2") })
        return ($Prefix + "_" + $Hex)
    } finally {
        $Md5.Dispose()
    }
}

function Resolve-Wix {
    $Global = Get-Command "wix" -ErrorAction SilentlyContinue
    if ($Global) {
        return $Global.Source
    }

    $ToolDir = Join-Path $Root ".tools/wix"
    $LocalExe = Join-Path $ToolDir "wix.exe"
    if (Test-Path $LocalExe) {
        return $LocalExe
    }

    Require-Command "dotnet" "Install .NET SDK 8 or newer."
    New-Item -ItemType Directory -Force -Path $ToolDir | Out-Null
    Invoke-Checked "dotnet" @("tool", "install", "--tool-path", $ToolDir, "wix")
    if (-not (Test-Path $LocalExe)) {
        throw "WiX tool install completed but wix.exe was not found at $LocalExe"
    }
    return $LocalExe
}

function New-ReleaseReadme {
    param([string]$Destination, [string]$Platform)
    $Text = @"
Bolide $Version ($Platform)

Contents:
- bolide executable
- bolide runtime static library
- std/ standard library wrappers

Quick check:
  bolide --version
  bolide run path/to/file.bl
  bolide compile path/to/file.bl -o app

AOT compilation needs a platform C linker and system development libraries.
See README.md in the source repository for details.
"@
    Set-Content -LiteralPath (Join-Path $Destination "README.txt") -Value $Text -Encoding UTF8
}

function New-WindowsInstaller {
    param([string]$PayloadDir, [string]$InstallerPath, [string]$Version)

    $Installer = @'
param(
    [string]$InstallDir,
    [switch]$Machine,
    [switch]$Update,
    [switch]$Uninstall,
    [switch]$NoAddToPath
)

$ErrorActionPreference = "Stop"

$SourceDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Payload = Join-Path $SourceDir "payload"

function Get-RegistryKey {
    param([string]$Scope)
    if ($Scope -eq "Machine") {
        return "HKLM:\Software\Bolide"
    }
    return "HKCU:\Software\Bolide"
}

function Read-InstallInfo {
    param([string]$Scope)
    $Key = Get-RegistryKey $Scope
    if (-not (Test-Path $Key)) {
        return $null
    }
    $Props = Get-ItemProperty -Path $Key -ErrorAction SilentlyContinue
    if (-not $Props) {
        return $null
    }
    return [PSCustomObject]@{
        InstallDir = $Props.InstallDir
        Version    = $Props.Version
    }
}

function Write-InstallInfo {
    param([string]$Scope, [string]$InstallDir, [string]$Version)
    $Key = Get-RegistryKey $Scope
    if (-not (Test-Path $Key)) {
        New-Item -Path $Key -Force | Out-Null
    }
    Set-ItemProperty -Path $Key -Name "InstallDir" -Value $InstallDir
    Set-ItemProperty -Path $Key -Name "Version" -Value $Version
}

function Remove-InstallInfo {
    param([string]$Scope)
    $Key = Get-RegistryKey $Scope
    if (Test-Path $Key) {
        Remove-Item -Path $Key -Recurse -Force
    }
}

function Get-InstallScope {
    if ($Machine) { return "Machine" }
    return "User"
}

function Resolve-InstallDir {
    param([string]$Requested, [string]$Scope)
    if ($Requested) {
        return $Requested
    }
    $Info = Read-InstallInfo $Scope
    if ($Info -and $Info.InstallDir -and (Test-Path $Info.InstallDir)) {
        return $Info.InstallDir
    }
    if ($Scope -eq "Machine") {
        return "$env:ProgramFiles\Bolide"
    }
    return "$env:LOCALAPPDATA\Programs\Bolide"
}

function Add-ToPath {
    param([string]$Dir, [string]$Scope)
    $CurrentPath = [Environment]::GetEnvironmentVariable("Path", $Scope)
    $Parts = @()
    if ($CurrentPath) {
        $Parts = $CurrentPath -split ";" | Where-Object { $_ -and $_.Trim() -ne "" }
    }

    $NormalizedDir = [System.IO.Path]::GetFullPath($Dir).TrimEnd("\")
    $AlreadyPresent = $false
    foreach ($Part in $Parts) {
        try {
            if ([System.IO.Path]::GetFullPath($Part).TrimEnd("\") -ieq $NormalizedDir) {
                $AlreadyPresent = $true
                break
            }
        } catch {
            if ($Part.TrimEnd("\") -ieq $Dir.TrimEnd("\")) {
                $AlreadyPresent = $true
                break
            }
        }
    }

    if (-not $AlreadyPresent) {
        $NewPath = if ($CurrentPath) { "$CurrentPath;$Dir" } else { $Dir }
        [Environment]::SetEnvironmentVariable("Path", $NewPath, $Scope)
    }
}

function Remove-FromPath {
    param([string]$Dir, [string]$Scope)
    $CurrentPath = [Environment]::GetEnvironmentVariable("Path", $Scope)
    if (-not $CurrentPath) {
        return
    }

    $NormalizedDir = [System.IO.Path]::GetFullPath($Dir).TrimEnd("\")
    $Parts = $CurrentPath -split ";" | Where-Object {
        $Part = $_
        if (-not $Part -or $Part.Trim() -eq "") {
            return $false
        }
        try {
            return [System.IO.Path]::GetFullPath($Part).TrimEnd("\") -ine $NormalizedDir
        } catch {
            return $Part.TrimEnd("\") -ine $Dir.TrimEnd("\")
        }
    }

    $NewPath = $Parts -join ";"
    [Environment]::SetEnvironmentVariable("Path", $NewPath, $Scope)
}

function Install-Payload {
    param([string]$Dir, [string]$Scope)
    if (-not (Test-Path $Payload)) {
        throw "Payload directory not found: $Payload"
    }

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null

    Remove-Item -LiteralPath (Join-Path $Dir "bolide.exe") -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Join-Path $Dir "bolide_runtime.lib") -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Join-Path $Dir "README.txt") -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath (Join-Path $Dir "std") -Recurse -Force -ErrorAction SilentlyContinue

    Copy-Item -Path (Join-Path $Payload "*") -Destination $Dir -Recurse -Force

    Write-InstallInfo $Scope $Dir $VERSION_PLACEHOLDER

    if (-not $NoAddToPath) {
        Add-ToPath $Dir $Scope
    }
}

function Uninstall-Existing {
    param([string]$Scope)
    $Info = Read-InstallInfo $Scope
    $Dir = if ($Info -and $Info.InstallDir) { $Info.InstallDir } else { $null }
    if (-not $Dir -and $InstallDir) {
        $Dir = $InstallDir
    }
    if (-not $Dir) {
        if ($Scope -eq "Machine") {
            $Dir = "$env:ProgramFiles\Bolide"
        } else {
            $Dir = "$env:LOCALAPPDATA\Programs\Bolide"
        }
    }

    if (Test-Path $Dir) {
        Remove-Item -LiteralPath $Dir -Recurse -Force
    }

    Remove-FromPath $Dir $Scope
    Remove-InstallInfo $Scope
}

$Scope = Get-InstallScope

if ($Uninstall) {
    Uninstall-Existing $Scope
    Write-Host "Bolide uninstalled."
    return
}

if ($Update) {
    $Info = Read-InstallInfo $Scope
    if (-not $Info -or -not $Info.InstallDir) {
        throw "No existing Bolide installation found for $Scope scope. Run without -Update to install."
    }
    $InstallDir = $Info.InstallDir
    Install-Payload $InstallDir $Scope
    Write-Host "Bolide updated to $VERSION_PLACEHOLDER in $InstallDir"
    Write-Host "Open a new terminal, then run: bolide --version"
    return
}

$InstallDir = Resolve-InstallDir $InstallDir $Scope
Install-Payload $InstallDir $Scope
Write-Host "Bolide $VERSION_PLACEHOLDER installed to $InstallDir"
Write-Host "Open a new terminal, then run: bolide --version"
'@

    $Installer = [regex]::Replace($Installer, [regex]::Escape('$VERSION_PLACEHOLDER'), $Version)
    Set-Content -LiteralPath $InstallerPath -Value $Installer -Encoding UTF8
}

function New-WindowsMsi {
    param([string]$PayloadDir, [string]$MsiPath)

    $Wix = Resolve-Wix
    $WixWork = Join-Path $DistRoot "wix"
    Remove-Dir $WixWork
    New-Item -ItemType Directory -Force -Path $WixWork | Out-Null

    $WxsPath = Join-Path $WixWork "bolide.wxs"
    $UpgradeCode = "7C5B0D5E-4A7C-4BA3-AF57-66B1CBBFAF04"

    $Dirs = @{}
    $DirEntries = New-Object System.Text.StringBuilder
    $ComponentRefs = New-Object System.Text.StringBuilder
    $ComponentsByDir = @{}

    function Register-Dir {
        param([hashtable]$Table, [string]$RelDir)
        $RelDir = $RelDir.Replace("\", "/").TrimEnd("/")
        if (-not $Table.ContainsKey($RelDir)) {
            $Table[$RelDir] = if ($RelDir -eq "") { "INSTALLFOLDER" } else { New-StableId "Dir" $RelDir }
        }
    }

    $Files = Get-ChildItem -LiteralPath $PayloadDir -Recurse -File | Sort-Object FullName
    foreach ($File in $Files) {
        $Rel = [System.IO.Path]::GetRelativePath($PayloadDir, $File.FullName).Replace("\", "/")
        $RelDir = ([System.IO.Path]::GetDirectoryName($Rel) -replace "\\", "/")
        if (-not $RelDir) { $RelDir = "" }
        Register-Dir $Dirs $RelDir
        $Accum = ""
        foreach ($Part in ($RelDir -split "/")) {
            if ($Part -eq "") { continue }
            $Accum = if ($Accum -eq "") { $Part } else { "$Accum/$Part" }
            Register-Dir $Dirs $Accum
        }
    }

    $SortedDirs = $Dirs.Keys | Where-Object { $_ -ne "" } | Sort-Object { ($_ -split "/").Count }, { $_ }
    foreach ($RelDir in $SortedDirs) {
        $Parent = ([System.IO.Path]::GetDirectoryName($RelDir) -replace "\\", "/")
        if (-not $Parent) { $Parent = "" }
        $DirId = $Dirs[$RelDir]
        $ParentId = $Dirs[$Parent]
        $Name = Split-Path $RelDir -Leaf
        [void]$DirEntries.AppendLine("    <DirectoryRef Id=`"$ParentId`">")
        [void]$DirEntries.AppendLine("      <Directory Id=`"$DirId`" Name=`"$(Xml-Escape $Name)`" />")
        [void]$DirEntries.AppendLine("    </DirectoryRef>")
    }

    foreach ($File in $Files) {
        $Rel = [System.IO.Path]::GetRelativePath($PayloadDir, $File.FullName).Replace("\", "/")
        $RelDir = ([System.IO.Path]::GetDirectoryName($Rel) -replace "\\", "/")
        if (-not $RelDir) { $RelDir = "" }
        $DirId = $Dirs[$RelDir]
        $ComponentId = New-StableId "Cmp" $Rel
        $FileId = New-StableId "File" $Rel
        if (-not $ComponentsByDir.ContainsKey($DirId)) {
            $ComponentsByDir[$DirId] = New-Object System.Text.StringBuilder
        }
        [void]$ComponentsByDir[$DirId].AppendLine("      <Component Id=`"$ComponentId`" Guid=`"*`">")
        [void]$ComponentsByDir[$DirId].AppendLine("        <File Id=`"$FileId`" Source=`"$(Xml-Escape $File.FullName)`" KeyPath=`"yes`" />")
        [void]$ComponentsByDir[$DirId].AppendLine("      </Component>")
        [void]$ComponentRefs.AppendLine("      <ComponentRef Id=`"$ComponentId`" />")
    }

    if (-not $ComponentsByDir.ContainsKey("INSTALLFOLDER")) {
        $ComponentsByDir["INSTALLFOLDER"] = New-Object System.Text.StringBuilder
    }
    [void]$ComponentsByDir["INSTALLFOLDER"].AppendLine("      <Component Id=`"Cmp_SystemPath`" Guid=`"*`">")
    [void]$ComponentsByDir["INSTALLFOLDER"].AppendLine("        <RegistryValue Root=`"HKLM`" Key=`"Software\Bolide`" Name=`"InstallDir`" Type=`"string`" Value=`"[INSTALLFOLDER]`" KeyPath=`"yes`" />")
    [void]$ComponentsByDir["INSTALLFOLDER"].AppendLine("        <Environment Id=`"Env_AddBolideToPath`" Name=`"PATH`" Value=`"[INSTALLFOLDER]`" Permanent=`"no`" Part=`"last`" Action=`"set`" System=`"yes`" />")
    [void]$ComponentsByDir["INSTALLFOLDER"].AppendLine("        <RemoveFolder Id=`"RemoveInstallFolder`" Directory=`"INSTALLFOLDER`" On=`"uninstall`" />")
    [void]$ComponentsByDir["INSTALLFOLDER"].AppendLine("      </Component>")
    [void]$ComponentRefs.AppendLine("      <ComponentRef Id=`"Cmp_SystemPath`" />")

    $ComponentXml = New-Object System.Text.StringBuilder
    foreach ($DirId in ($ComponentsByDir.Keys | Sort-Object)) {
        [void]$ComponentXml.AppendLine("    <DirectoryRef Id=`"$DirId`">")
        [void]$ComponentXml.Append($ComponentsByDir[$DirId].ToString())
        [void]$ComponentXml.AppendLine("    </DirectoryRef>")
    }

    $Wxs = @"
<Wix xmlns="http://wixtoolset.org/schemas/v4/wxs"
     xmlns:ui="http://wixtoolset.org/schemas/v4/wxs/ui">
  <Package Name="Bolide" Manufacturer="Bolide" Version="$Version" UpgradeCode="$UpgradeCode" Scope="perMachine">
    <MajorUpgrade DowngradeErrorMessage="A newer version of Bolide is already installed." />
    <MediaTemplate EmbedCab="yes" />

    <ui:WixUI Id="WixUI_InstallDir" InstallDirectory="INSTALLFOLDER" />

    <Feature Id="MainFeature" Title="Bolide" Level="1">
$($ComponentRefs.ToString().TrimEnd())
    </Feature>

    <StandardDirectory Id="ProgramFiles64Folder">
      <Directory Id="INSTALLFOLDER" Name="Bolide" />
    </StandardDirectory>

$($DirEntries.ToString().TrimEnd())

$($ComponentXml.ToString().TrimEnd())
  </Package>
</Wix>
"@

    Write-Utf8NoBom $WxsPath $Wxs
    if (Test-Path $MsiPath) {
        Remove-Item -LiteralPath $MsiPath -Force
    }
    Invoke-Checked $Wix @("eula", "accept", "wix7")
    Invoke-Checked $Wix @("extension", "add", "WixToolset.UI.wixext")
    Invoke-Checked $Wix @("--acceptEula", "yes", "build", $WxsPath, "-arch", "x64", "-ext", "WixToolset.UI.wixext", "-o", $MsiPath)
}

function New-LinuxInstaller {
    param([string]$PayloadDir, [string]$InstallerPath)

    $PayloadTar = Join-Path (Split-Path -Parent $InstallerPath) "payload.tar.gz"
    if (Test-Path $PayloadTar) {
        Remove-Item -LiteralPath $PayloadTar -Force
    }

    tar -czf $PayloadTar -C $PayloadDir .
    $TarBase64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($PayloadTar))
    Remove-Item -LiteralPath $PayloadTar -Force

    $Lines = New-Object System.Collections.Generic.List[string]
    for ($i = 0; $i -lt $TarBase64.Length; $i += 76) {
        $Len = [Math]::Min(76, $TarBase64.Length - $i)
        $Lines.Add($TarBase64.Substring($i, $Len))
    }
    $PayloadText = [string]::Join("`n", $Lines)

    $Script = @"
#!/usr/bin/env sh
set -eu

PREFIX="`${PREFIX:-/usr/local}"
INSTALL_DIR="`$PREFIX/lib/bolide"
BIN_DIR="`$PREFIX/bin"

if [ "`$(id -u)" -ne 0 ] && [ "`${PREFIX#/usr}" != "`$PREFIX" ]; then
  echo "Installing under `$PREFIX usually requires root. Re-run with sudo or set PREFIX." >&2
  exit 1
fi

TMP_DIR="`$(mktemp -d)"
cleanup() {
  rm -rf "`$TMP_DIR"
}
trap cleanup EXIT

sed '1,/^__BOLIDE_PAYLOAD__$/d' "`$0" | base64 -d > "`$TMP_DIR/payload.tar.gz"

mkdir -p "`$INSTALL_DIR" "`$BIN_DIR"
rm -f "`$INSTALL_DIR/bolide" "`$INSTALL_DIR/libbolide_runtime.a" "`$INSTALL_DIR/README.txt"
rm -rf "`$INSTALL_DIR/std"
tar -xzf "`$TMP_DIR/payload.tar.gz" -C "`$INSTALL_DIR"
chmod +x "`$INSTALL_DIR/bolide"
ln -sfn "`$INSTALL_DIR/bolide" "`$BIN_DIR/bolide"

echo "Bolide $Version installed to `$INSTALL_DIR"
echo "Run: bolide --version"
exit 0
__BOLIDE_PAYLOAD__
"@

    $Script = $Script.TrimEnd("`r", "`n") + "`n" + $PayloadText + "`n"
    Write-Utf8NoBom $InstallerPath $Script
}

Remove-Dir $DistRoot
New-Item -ItemType Directory -Force -Path $DistRoot | Out-Null

if (-not $SkipWindows) {
    Require-Command "cargo" "Install Rust from https://rustup.rs/."
    Write-Host "Building Windows release..."
    Push-Location $Root
    try {
        Invoke-Checked "cargo" @("build", "--release", "-p", "bolide-cli", "-p", "bolide-runtime")
    } finally {
        Pop-Location
    }

    $WinPayload = Join-Path $DistRoot "bolide-$Version-windows-x86_64"
    Remove-Dir $WinPayload
    New-Item -ItemType Directory -Force -Path $WinPayload | Out-Null
    Copy-Item -LiteralPath (Join-Path $Root "target/release/bolide.exe") -Destination $WinPayload -Force
    Copy-Item -LiteralPath (Join-Path $Root "target/release/bolide_runtime.lib") -Destination $WinPayload -Force
    Copy-Std $WinPayload
    New-ReleaseReadme $WinPayload "windows-x86_64"

    $ZipPath = Join-Path $DistRoot "bolide-$Version-windows-x86_64.zip"
    if (Test-Path $ZipPath) {
        Remove-Item -LiteralPath $ZipPath -Force
    }
    Compress-Archive -Path (Join-Path $WinPayload "*") -DestinationPath $ZipPath -Force

    $WinInstallerDir = Join-Path $DistRoot "bolide-$Version-windows-x86_64-installer"
    Remove-Dir $WinInstallerDir
    New-Item -ItemType Directory -Force -Path $WinInstallerDir | Out-Null
    $WinInstallerPayload = Join-Path $WinInstallerDir "payload"
    New-Item -ItemType Directory -Force -Path $WinInstallerPayload | Out-Null
    Copy-Item -Path (Join-Path $WinPayload "*") -Destination $WinInstallerPayload -Recurse -Force
    New-WindowsInstaller $WinInstallerPayload (Join-Path $WinInstallerDir "install.ps1") $Version

    $WinInstallerZip = Join-Path $DistRoot "bolide-$Version-windows-x86_64-installer.zip"
    if (Test-Path $WinInstallerZip) {
        Remove-Item -LiteralPath $WinInstallerZip -Force
    }
    Compress-Archive -Path (Join-Path $WinInstallerDir "*") -DestinationPath $WinInstallerZip -Force

    if (-not $SkipMsi) {
        $MsiPath = Join-Path $DistRoot "bolide-$Version-windows-x86_64.msi"
        New-WindowsMsi $WinPayload $MsiPath
    }
}

if (-not $SkipLinux) {
    Require-Command "cargo" "Install Rust from https://rustup.rs/."
    Require-Command "zig" "Install Zig and put it on PATH."
    Require-Command "cargo-zigbuild" "Run: cargo install cargo-zigbuild"
    Require-Command "tar" "Install bsdtar or GNU tar."

    Write-Host "Building Linux release with cargo zigbuild for $LinuxTarget..."
    Push-Location $Root
    try {
        Invoke-Checked "rustup" @("target", "add", $LinuxTarget)
        Invoke-Checked "cargo" @("zigbuild", "--release", "--target", $LinuxTarget, "-p", "bolide-cli", "-p", "bolide-runtime")
    } finally {
        Pop-Location
    }

    $LinuxPayload = Join-Path $DistRoot "bolide-$Version-linux-x86_64"
    Remove-Dir $LinuxPayload
    New-Item -ItemType Directory -Force -Path $LinuxPayload | Out-Null
    Copy-Item -LiteralPath (Join-Path $Root "target/$LinuxTarget/release/bolide") -Destination $LinuxPayload -Force
    Copy-Item -LiteralPath (Join-Path $Root "target/$LinuxTarget/release/libbolide_runtime.a") -Destination $LinuxPayload -Force
    Copy-Std $LinuxPayload
    New-ReleaseReadme $LinuxPayload "linux-x86_64"

    $LinuxInstaller = Join-Path $DistRoot "bolide-$Version-linux-x86_64-install.sh"
    New-LinuxInstaller $LinuxPayload $LinuxInstaller
}

Write-Host "Release artifacts written to $DistRoot"
