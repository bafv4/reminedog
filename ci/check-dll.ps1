# ビルドした reminedog.dll を確かめる：x64 で、Agent_OnLoad を export し、どの Windows にもある
# DLL だけを import し（CRT は静的にリンクする：Java 8 には vcruntime140.dll がない。
# .cargo/config.toml を参照。WebView2 のローダーもリンクする）、バージョン情報のリソースに版を
# 持つ（-Version を付ければその版）。CI とリリースのワークフローが使う。
param(
    [Parameter(Mandatory)] [string] $Dll,
    [string] $Version
)
$ErrorActionPreference = 'Stop'

# DLL が import してよいもの：Windows 自身の DLL。新しい import は、その DLL がどの Windows 10・11
# にもあることを確かめてここに足すまで、確認に落ちる（ない DLL があるとエージェントが、つまり
# ゲームが起動しない）。
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
# コアのシステムの API セット（api-ms-win-core-*）は、どの Windows 10・11 にもある。
$unexpected = $imports | Where-Object { $_ -notin $allowed -and $_ -notmatch '^api-ms-win-core-' }
if ($unexpected) { throw "$Dll imports DLLs not on every PC: $($unexpected -join ', ')" }

$info = (Get-Item $Dll).VersionInfo
"Version resource: FileVersion '$($info.FileVersion)', ProductVersion '$($info.ProductVersion)'"
if (-not $info.FileVersion) { throw "$Dll has no version in its version resource" }
if ($Version -and $info.FileVersion -ne $Version) {
    throw "$Dll has the version '$($info.FileVersion)', not '$Version'"
}
