# Checks a built reminedog.dll: it exports Agent_OnLoad, links the CRT statically (Java 8 ships
# no vcruntime140.dll; see .cargo/config.toml) and, with -Version, carries that version in its
# version resource. Used by the CI and release workflows.
param(
    [Parameter(Mandatory)] [string] $Dll,
    [string] $Version
)
$ErrorActionPreference = 'Stop'

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$dumpbin = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
if (-not $dumpbin) { throw 'dumpbin.exe not found' }

$exports = & $dumpbin /nologo /exports $Dll
if ($LASTEXITCODE -ne 0) { throw "dumpbin /exports failed ($LASTEXITCODE)" }
$exports
if (-not ($exports -match '\sAgent_OnLoad(\s|$)')) { throw "$Dll does not export Agent_OnLoad" }

$deps = & $dumpbin /nologo /dependents $Dll
if ($LASTEXITCODE -ne 0) { throw "dumpbin /dependents failed ($LASTEXITCODE)" }
$deps
$crt = $deps -match '^\s*(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)'
if ($crt) { throw "$Dll imports the dynamic CRT: $(($crt | ForEach-Object Trim) -join ', ')" }

$info = (Get-Item $Dll).VersionInfo
"Version resource: FileVersion '$($info.FileVersion)', ProductVersion '$($info.ProductVersion)'"
if ($Version -and $info.FileVersion -ne $Version) {
    throw "$Dll has the version '$($info.FileVersion)', not '$Version'"
}
