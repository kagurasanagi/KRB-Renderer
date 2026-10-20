# 配布用の一式（Rust 版の krbr）を dist\KRB-Renderer-<版>-win64 にまとめ、zip にする。
# フォルダごとどこへ置いても動く（ポータブル版）。インストーラや KiCad の PCM パッケージもこれを元に作る。
#   powershell -ExecutionPolicy Bypass -File scripts\make_dist.ps1            # ビルドしてからまとめる
#   powershell -ExecutionPolicy Bypass -File scripts\make_dist.ps1 -NoBuild   # ビルド済みのものをまとめる
# 中身：krbr.exe、shaders\、step\（STEP 変換器と OpenCASCADE）、OIDN（ノイズ除去）、Visual C++ ランタイム、
#       KiCad のプラグイン（kicad_plugin\）とそのインストール・削除のバッチ、使い方、ライセンス
param(
    [switch]$NoBuild,
    [string]$OutDir = ""
)
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $repo "dist" }
$version = (Select-String -Path (Join-Path $repo "rust\Cargo.toml") -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "KRB-Renderer-$version-win64"
$dst = Join-Path $OutDir $name
$cppOut = Join-Path $repo "build\release"
$rustExe = Join-Path $repo "rust\target\release\krbr.exe"

# Visual Studio（cmake、Visual C++ ランタイムの再頒布用ファイル）
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -property installationPath 2>$null
if (-not $vs) { $vs = (Get-ChildItem "$env:ProgramFiles\Microsoft Visual Studio\*\*" -Directory | Where-Object { Test-Path "$($_.FullName)\VC\Redist\MSVC" } | Select-Object -First 1).FullName }
if (-not $vs) { throw "Visual Studio が見つかりません（Visual C++ ランタイムを入れるのに使う）" }

if (-not $NoBuild) {
    Write-Host "== C++ 版のビルド（シェーダ、STEP 変換器 DLL）"
    Import-Module "$vs\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
    Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments "-arch=x64" | Out-Null
    Push-Location $repo
    cmake --build --preset release
    if ($LASTEXITCODE -ne 0) { throw "cmake --build に失敗" }
    Pop-Location
    Write-Host "== Rust 版のビルド"
    Push-Location (Join-Path $repo "rust")
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build に失敗" }
    Pop-Location
}
foreach ($p in @($rustExe, "$cppOut\shaders\pathtrace.comp.spv", "$cppOut\step\krbr_step.dll")) {
    if (-not (Test-Path $p)) { throw "見つかりません: $p（-NoBuild を外してビルドしてください）" }
}

Write-Host "== $dst にまとめる"
if (Test-Path $dst) { Remove-Item -Recurse -Force $dst }
New-Item -ItemType Directory -Force "$dst\shaders", "$dst\step", "$dst\kicad_plugin\krbr_preview", "$dst\licenses" | Out-Null
Copy-Item $rustExe $dst
Copy-Item "$cppOut\shaders\*.spv" "$dst\shaders"
Copy-Item "$cppOut\step\krbr_step.dll", "$cppOut\step\TK*.dll" "$dst\step"
# ノイズ除去（OIDN）。HIP・SYCL のデバイスは使わないので入れない
Copy-Item "$cppOut\OpenImageDenoise.dll", "$cppOut\OpenImageDenoise_core.dll", "$cppOut\OpenImageDenoise_device_cpu.dll", "$cppOut\OpenImageDenoise_device_cuda.dll", "$cppOut\tbb12.dll" $dst
# Visual C++ ランタイム（アプリの横に置いてよい再頒布用ファイル。krbr.exe・STEP 変換器・OpenCASCADE が使う）
$crt = Get-ChildItem "$vs\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" -Directory | Sort-Object FullName | Select-Object -Last 1
foreach ($f in @("msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll", "vcruntime140.dll", "vcruntime140_1.dll", "concrt140.dll")) {
    Copy-Item (Join-Path $crt.FullName $f) $dst
}
# KiCad のプラグインと、そのインストール・削除（バッチは同じフォルダの krbr.exe を使う）
Copy-Item "$repo\kicad_plugin\krbr_preview\*" "$dst\kicad_plugin\krbr_preview" -Exclude "krbr_path.txt", "__pycache__"
Copy-Item "$repo\scripts\install_kicad_plugin.bat", "$repo\scripts\uninstall_kicad_plugin.bat" $dst
# ライセンス
Copy-Item "$repo\third_party\oidn\doc\LICENSE.txt" "$dst\licenses\OpenImageDenoise-LICENSE.txt"
Copy-Item "$repo\third_party\oidn\doc\third-party-programs-oneTBB.txt" "$dst\licenses\oneTBB-third-party-programs.txt"
$occt = Get-ChildItem "$repo\build\vcpkg_installed\*\share\opencascade\copyright" | Select-Object -First 1
if ($occt) { Copy-Item $occt.FullName "$dst\licenses\OpenCASCADE-LICENSE.txt" }
if (Test-Path "$repo\LICENSE") { Copy-Item "$repo\LICENSE" "$dst\licenses\KRB-Renderer-LICENSE.txt" }
# 使い方（メモ帳で読めるよう UTF-8 の BOM 付き）
$readme = @"
KRB-Renderer $version（Rust 版）  KiCad の基板の物理ベースのレンダラー

■ 使い方
・krbr.exe のアイコンに .kicad_pcb をドラッグ＆ドロップすると、ビューアで開きます。
・コマンドラインでは  krbr.exe <基板.kicad_pcb>  （ビューア）、 krbr.exe render <基板.kicad_pcb>  （画像の書き出し）。
  krbr.exe --help で一覧を出します。
・ビューアは基板のファイルの保存を監視していて、KiCad で保存するたびに描き直します。
・GPU の弱い PC では、ビューアのメニューの下の「一時停止」（P キー）でレンダリングを止めると操作が軽くなります。

■ KiCad のプラグイン（PCB エディタのツールバーに「KRB Render でプレビュー」のボタン）
・install_kicad_plugin.bat をダブルクリックすると、KiCad 9 以降に入ります。KiCad を開き直すと出ます。
・削除は uninstall_kicad_plugin.bat です。
・このフォルダを移動したら、install_kicad_plugin.bat をもう一度実行してください（krbr.exe の場所を書き直します）。

■ 動かすのに必要なもの
・Windows 10 / 11（64 bit）、Vulkan 1.3 とレイトレーシングに対応した GPU（NVIDIA GTX 10 系以降など）。
・3D モデルは KiCad のインストール先から読みます（KiCad 10 なら C:\Program Files\KiCad\10.0）。
・線フォントのシルクの文字には kicad-cli（KiCad に付属）を使います。

■ 同梱物のライセンス（licenses フォルダ）
・Intel Open Image Denoise（Apache-2.0）、oneTBB（Apache-2.0）、Open CASCADE Technology（LGPL-2.1、例外条項付き）。
・Microsoft Visual C++ ランタイム（再頒布用ファイル）。
"@
[System.IO.File]::WriteAllText("$dst\README.txt", $readme, (New-Object System.Text.UTF8Encoding $true))

Write-Host "== zip にする"
$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $dst -DestinationPath $zip
$size = (Get-ChildItem $dst -Recurse | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("完成: {0}（{1:N0} MB）、{2}（{3:N0} MB）" -f $dst, $size, $zip, ((Get-Item $zip).Length / 1MB))
