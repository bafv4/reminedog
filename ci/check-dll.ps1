# Checks a built reminedog.dll: it is x64, exports Agent_OnLoad, imports only DLLs every
# Windows has (the CRT is linked statically: Java 8 ships no vcruntime140.dll, see
# .cargo/config.toml; WebView2's loader is linked in too) and carries a version in its version
# resource (with -Version, that version). Used by the CI and release workflows.
param(
    [Parameter(Mandatory)] [string] $Dll,
    [string] $Version
)
$ErrorActionPreference = 'Stop'

# What the DLL may import: Windows' own DLLs. A new import fails the check until it is added
# here, after making sure the DLL is on every Windows 10 and 11 (a missing one keeps the agent,
# and so the game, from starting).
$allowed = @(
    'advapi32.dll', 'bcryptprimitives.dll', 'combase.dll', 'coremessaging.dll', 'd3d11.dll',
    'gdi32.dll', 'kernel32.dll', 'ntdll.dll', 'ole32.dll', 'oleaut32.dll', 'user32.dll'
)

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$dumpbin = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
if (-not $dumpbin) { throw 'dumpbin.exe not found' }

$headers = & $dumpbin /nologo /headers $Dll
if ($LASTEXITCODE -ne 0) { throw "dumpbin /headers failed ($LASTEXITCODE)" }
if (-not ($headers -match '^\s*8664 machine \(x64\)')) { throw "$Dll is not an x64 DLL" }

$exports = & $dumpbin /nologo /exports $Dll
if ($LASTEXITCODE -ne 0) { throw "dumpbin /exports failed ($LASTEXITCODE)" }
$exports
if (-not ($exports -match '\sAgent_OnLoad(\s|$)')) { throw "$Dll does not export Agent_OnLoad" }

$deps = & $dumpbin /nologo /dependents $Dll
if ($LASTEXITCODE -ne 0) { throw "dumpbin /dependents failed ($LASTEXITCODE)" }
$deps
$imports = $deps | Where-Object { $_ -match '^\s+\S+\.dll\s*$' } | ForEach-Object { $_.Trim().ToLowerInvariant() } |
    Sort-Object -Unique
$crt = $imports | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)' }
if ($crt) { throw "$Dll imports the dynamic CRT: $($crt -join ', ')" }
# API sets of the core system (api-ms-win-core-*) are in every Windows 10 and 11.
$unexpected = $imports | Where-Object { $_ -notin $allowed -and $_ -notmatch '^api-ms-win-core-' }
if ($unexpected) { throw "$Dll imports DLLs not on every PC: $($unexpected -join ', ')" }

$info = (Get-Item $Dll).VersionInfo
"Version resource: FileVersion '$($info.FileVersion)', ProductVersion '$($info.ProductVersion)'"
if (-not $info.FileVersion) { throw "$Dll has no version in its version resource" }
if ($Version -and $info.FileVersion -ne $Version) {
    throw "$Dll has the version '$($info.FileVersion)', not '$Version'"
}
