# Intel Open Image Denoise の公式ビルド済みバイナリを third_party/oidn に置く（git には含めない）
# ハッシュは GitHub のリリースに公開されている値（sha256）
$ErrorActionPreference = 'Stop'
$version = '2.5.1'
$sha256 = 'f11f91bc072a5e3a564515724cb72ab8fcfbc445c84a84c197ff9b16cc01396f'
$root = Split-Path -Parent $PSScriptRoot
$dl = Join-Path $root "build\downloads\oidn-$version"
$zip = Join-Path $dl "oidn-$version.x64.windows.zip"
New-Item -ItemType Directory -Force $dl | Out-Null
if (-not (Test-Path $zip)) {
    Invoke-WebRequest -Uri "https://github.com/RenderKit/oidn/releases/download/v$version/oidn-$version.x64.windows.zip" -OutFile $zip
}
$actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
if ($actual -ne $sha256) { throw "SHA256 mismatch: $actual" }
$extract = Join-Path $dl 'x'
if (Test-Path $extract) { Remove-Item -Recurse -Force $extract -Confirm:$false }
Expand-Archive -Path $zip -DestinationPath $extract
$dest = Join-Path $root 'third_party\oidn'
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest -Confirm:$false }
Move-Item (Join-Path $extract "oidn-$version.x64.windows") $dest
Write-Output "OIDN $version -> $dest"
