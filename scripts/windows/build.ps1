# Windows のエージェント（reminedog.dll とその .pdb）をリリースでビルドして ci/check-dll.ps1 で
# 確かめ、必要ならインストーラーの jar と一緒に dist\windows に出す。
# Rust と Visual Studio Build Tools（C++ によるデスクトップ開発）が要る。インストーラーには JDK 17
# 以降が要る。Windows PowerShell 5.1 と PowerShell 7 で動く。
#
# 使い方：scripts\windows\build.ps1 [-Check] [-Installer] [-Out DIR] [-InstallDir DIR]
#   -Check       CI と同じく、先に fmt（確認だけ）、clippy（警告はエラー）、テスト
#   -Installer   インストーラーもビルドする（そのテストと reminedog-installer.jar）
#   -Out         ファイルの出力先（既定：リポジトリの dist\windows）
#   -InstallDir  そこにもコピーする。例：-agentpath:C:\reminedog\reminedog.dll なら C:\reminedog
#                （ゲームが DLL を読み込んでいる間は失敗する）
# REMINEDOG_VERSION を設定すれば、それが DLL と jar の版になる（リリースのワークフローと同じ）。
# ライブラリの名前はそのまま、jar は reminedog-installer-<version>.jar。
param(
    [switch] $Check,
    [switch] $Installer,
    [string] $Out,
    [string] $InstallDir
)
$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Out) { $Out = Join-Path $repo 'dist\windows' }

# ネイティブのコマンドを動かし、失敗したら止まる。
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
    # Cargo の出力のフォルダ（設定がどこに置いていても。CARGO_TARGET_DIR、build.target-dir）。
    $metadata = cargo metadata --format-version 1 --no-deps
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed (exit $LASTEXITCODE)" }
    $target = ($metadata | ConvertFrom-Json).target_directory
    $dll = Join-Path $target 'release\reminedog.dll'
    $pdb = Join-Path $target 'release\reminedog.pdb'
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
