# 配布用の一式（Rust 版の krbr）を dist\KRB-Renderer-<版>-win64 にまとめ、zip にする。
# フォルダごとどこへ置いても動く（ポータブル版）。インストーラや KiCad の PCM パッケージもこれを元に作る。
#   powershell -ExecutionPolicy Bypass -File scripts\make_dist.ps1            # ビルドしてからまとめる
#   powershell -ExecutionPolicy Bypass -File scripts\make_dist.ps1 -NoBuild   # ビルド済みのものをまとめる
# 中身：krbr.exe、shaders\、dll\（step：STEP 変換器と OpenCASCADE、oidn：ノイズ除去、vcruntime：Visual C++ ランタイム）、
#       使い方、ライセンス（KiCad のプラグインは krbr.exe に埋め込んであり、ビューアの「設定 → KiCad プラグイン」で入れる）
# 直下には DLL を置かない。krbr.exe は Visual C++ ランタイムを静的に組み込んでいて（rust\.cargo\config.toml）、
# 起動の直後に dll\vcruntime を DLL の探し先に足す（STEP 変換器・OIDN の DLL が使う）
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
$stepOut = Join-Path $repo "build\release\step"  # STEP 変換器 DLL（scripts\build.cmd、CMake）
$oidnBin = Join-Path $repo "third_party\oidn\bin"  # scripts\get_oidn.ps1 が置く公式のバイナリ
$rustExe = Join-Path $repo "rust\target\release\krbr.exe"
$launcherExe = Join-Path $repo "rust\target\release\KRB-Renderer.exe"  # コンソールを出さずにビューアを開く起動役
$rustShaders = Join-Path $repo "rust\target\release\shaders"  # cargo build（krbr-render の build.rs）が glslc で作る

# Visual Studio（Visual C++ ランタイムの再頒布用ファイル）
$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -property installationPath 2>$null
if (-not $vs) { $vs = (Get-ChildItem "$env:ProgramFiles\Microsoft Visual Studio\*\*" -Directory | Where-Object { Test-Path "$($_.FullName)\VC\Redist\MSVC" } | Select-Object -First 1).FullName }
if (-not $vs) { throw "Visual Studio が見つかりません（Visual C++ ランタイムを入れるのに使う）" }

if (-not $NoBuild) {
    Write-Host "== STEP 変換器 DLL のビルド（CMake）"
    & cmd /c (Join-Path $PSScriptRoot "build.cmd")
    if ($LASTEXITCODE -ne 0) { throw "scripts\build.cmd に失敗" }
    Write-Host "== Rust 版のビルド（シェーダも作る）"
    Push-Location (Join-Path $repo "rust")
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build に失敗" }
    Pop-Location
}
foreach ($p in @($rustExe, "$rustShaders\pathtrace.comp.spv", "$stepOut\krbr_step.dll", "$oidnBin\OpenImageDenoise.dll")) {
    if (-not (Test-Path $p)) { throw "見つかりません: $p（-NoBuild を外してビルドしてください。OIDN は scripts\get_oidn.ps1）" }
}

Write-Host "== $dst にまとめる"
if (Test-Path $dst) { Remove-Item -Recurse -Force $dst }
New-Item -ItemType Directory -Force "$dst\shaders", "$dst\dll\step", "$dst\dll\oidn", "$dst\dll\vcruntime", "$dst\licenses" | Out-Null
Copy-Item $rustExe $dst
Copy-Item $launcherExe $dst
Copy-Item "$rustShaders\*.spv" "$dst\shaders"
Copy-Item "$stepOut\krbr_step.dll", "$stepOut\TK*.dll" "$dst\dll\step"
# ノイズ除去（OIDN）。HIP・SYCL のデバイスは使わないので入れない
Copy-Item "$oidnBin\OpenImageDenoise.dll", "$oidnBin\OpenImageDenoise_core.dll", "$oidnBin\OpenImageDenoise_device_cpu.dll", "$oidnBin\OpenImageDenoise_device_cuda.dll", "$oidnBin\tbb12.dll" "$dst\dll\oidn"
# Visual C++ ランタイム（アプリと一緒に置いてよい再頒布用ファイル。STEP 変換器・OpenCASCADE・OIDN が使う）
$crt = Get-ChildItem "$vs\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT" -Directory | Sort-Object FullName | Select-Object -Last 1
foreach ($f in @("msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll", "vcruntime140.dll", "vcruntime140_1.dll", "concrt140.dll")) {
    Copy-Item (Join-Path $crt.FullName $f) "$dst\dll\vcruntime"
}
$rootDlls = Get-ChildItem "$dst\*.dll"
if ($rootDlls) { throw "直下に DLL があります: $($rootDlls.Name -join ', ')" }
# krbr bench の同梱の基板と、その 3D モデル（基板の設定ファイル .krbr は入れない）
New-Item -ItemType Directory -Force "$dst\bench" | Out-Null
Copy-Item "$repo\bench\krbr_bench.kicad_pcb", "$repo\bench\LICENSE.md" "$dst\bench"
Copy-Item "$repo\bench\3dmodels" "$dst\bench" -Recurse
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
・KRB-Renderer.exe をダブルクリックするとビューアが開きます（前に開いた基板を開く）。KRB-Renderer.exe のアイコンや
  ビューアのウィンドウに .kicad_pcb をドラッグ＆ドロップしても開けます。最近使った基板は「ファイル → 最近使った基板」。
  （krbr.exe に落としても開けますが、黒いコンソールの窓が一瞬出ます）
・コマンドラインでは  krbr.exe <基板.kicad_pcb>  （ビューア）、 krbr.exe render <基板.kicad_pcb>  （画像の書き出し）。
  krbr.exe --help で一覧を出します。
・ビューアは基板のファイルの保存を監視していて、KiCad で保存するたびに描き直します。
・GPU の弱い PC では、ビューアのメニューの下の「一時停止」（P キー）でレンダリングを止めると操作が軽くなります。

■ KiCad のプラグイン（PCB エディタのツールバーに「KRB Render でプレビュー」のボタン）
・ビューアのメニューの「設定 → KiCad プラグイン...」で、KiCad の版（9.0、10.0 など）ごとに登録・解除します。
  登録した後、KiCad を開き直すと出ます。
・このフォルダを移動したら、krbr.exe を一度起動してください（登録済みの版の krbr.exe の場所を書き直します）。

■ フォルダの中身（フォルダごと移動して使えます。中のファイルは動かさないでください）
・KRB-Renderer.exe  ビューアを開く（コンソールを出さない起動役。中で krbr.exe を動かす）
・krbr.exe          本体（コマンドライン）
・shaders\          シェーダ
・dll\step\         STEP 変換器（3D モデルの読み込み）と OpenCASCADE
・dll\oidn\         ノイズ除去（Intel Open Image Denoise）
・dll\vcruntime\    Visual C++ ランタイム（インストール不要）
・bench\           krbr.exe bench で速さを測る基板（3D モデルも同梱。KiCad が無くても動く）
・licenses\         同梱物のライセンス

■ 動かすのに必要なもの
・Windows 10 / 11（64 bit）、Vulkan 1.3 とレイトレーシングに対応した GPU（NVIDIA GTX 10 系以降など）。
・3D モデルは KiCad のインストール先から読みます（KiCad 10 なら C:\Program Files\KiCad\10.0）。
・線フォントのシルクの文字には kicad-cli（KiCad に付属）を使います。

■ 同梱物のライセンス（licenses フォルダ）
・Intel Open Image Denoise（Apache-2.0）、oneTBB（Apache-2.0）、Open CASCADE Technology（LGPL-2.1、例外条項付き）。
・Microsoft Visual C++ ランタイム（再頒布用ファイル）。
・bench\3dmodels の STEP：KiCad の標準ライブラリ（CC-BY-SA 4.0、例外条項付き。bench\LICENSE.md）。
"@
[System.IO.File]::WriteAllText("$dst\README.txt", $readme, (New-Object System.Text.UTF8Encoding $true))

Write-Host "== zip にする"
$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $dst -DestinationPath $zip
$size = (Get-ChildItem $dst -Recurse | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("完成: {0}（{1:N0} MB）、{2}（{3:N0} MB）" -f $dst, $size, $zip, ((Get-Item $zip).Length / 1MB))
