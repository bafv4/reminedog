# Builds the Windows agent (reminedog.dll and its .pdb) in release and checks it with
# ci/check-dll.ps1, optionally with the installer's jar, into dist\windows.
# Needs Rust and Visual Studio Build Tools (C++ desktop development); the installer needs a JDK
# 17 or later. Runs in Windows PowerShell 5.1 and PowerShell 7.
#
# Usage: scripts\windows\build.ps1 [-Check] [-Installer] [-Out DIR] [-InstallDir DIR]
#   -Check       first fmt (check only), clippy (warnings are errors) and the tests, as the CI
#   -Installer   also build the installer (its tests and reminedog-installer.jar)
#   -Out         where the files go (default: dist\windows in the repository)
#   -InstallDir  also copy them there, e.g. C:\reminedog for -agentpath:C:\reminedog\reminedog.dll
#                (fails while a game has the DLL loaded)
# REMINEDOG_VERSION, when set, is the version in the DLL and the jar (as the release workflow
# sets it); the library keeps its plain name, the jar is reminedog-installer-<version>.jar.
param(
    [switch] $Check,
    [switch] $Installer,
    [string] $Out,
    [string] $InstallDir
)
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Out) { $Out = Join-Path $repo 'dist\windows' }

# Runs a native command and stops when it fails.
function Invoke-Step([string] $Name, [scriptblock] $Command) {
    Write-Host "== $Name"
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Name failed (exit $LASTEXITCODE)" }
}

Push-Location $repo
try {
    if ($Check) {
        Invoke-Step 'fmt' { cargo fmt --all --check }
        Invoke-Step 'clippy' { cargo clippy --locked --workspace --all-targets -- -D warnings }
        Invoke-Step 'test' { cargo test --locked --workspace }
    }

    Invoke-Step 'build reminedog.dll' { cargo build --locked --release -p reminedog-hook-win }
    $dll = Join-Path $repo 'target\release\reminedog.dll'
    $pdb = Join-Path $repo 'target\release\reminedog.pdb'
    Write-Host '== check the DLL (x64, exports, imports, version resource)'
    $checkArgs = @{ Dll = $dll }
    if ($env:REMINEDOG_VERSION) { $checkArgs.Version = $env:REMINEDOG_VERSION }
    & (Join-Path $repo 'ci\check-dll.ps1') @checkArgs | Out-Null

    $files = @($dll, $pdb)
    if ($Installer) {
        $gradleArgs = @('build', '--no-daemon')
        if ($env:REMINEDOG_VERSION) { $gradleArgs += "-PreminedogVersion=$env:REMINEDOG_VERSION" }
        Push-Location (Join-Path $repo 'installer')
        try {
            Invoke-Step 'build the installer' { .\gradlew.bat @gradleArgs }
        } finally {
            Pop-Location
        }
        $jarName = if ($env:REMINEDOG_VERSION) { "reminedog-installer-$env:REMINEDOG_VERSION.jar" } else { 'reminedog-installer.jar' }
        $files += Join-Path $repo "installer\build\libs\$jarName"
    }

    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    Copy-Item $files $Out
    if ($InstallDir) {
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        try {
            Copy-Item $files $InstallDir
        } catch {
            throw "cannot copy into $InstallDir (is a game running with the DLL loaded? close it and run again): $_"
        }
    }

    Write-Host '== done'
    foreach ($dir in @($Out) + @($InstallDir | Where-Object { $_ })) {
        foreach ($file in $files) {
            $copy = Get-Item (Join-Path $dir (Split-Path $file -Leaf))
            '{0,10:N0} bytes  {1}' -f $copy.Length, $copy.FullName
        }
    }
} finally {
    Pop-Location
}
