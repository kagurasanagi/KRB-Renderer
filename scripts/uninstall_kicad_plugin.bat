@echo off
rem （このファイルは Shift-JIS・CRLF。cmd が日本語を正しく読めるように）
rem KiCad のプラグイン「KRB Render でプレビュー」を削除する（KiCad 9 以降のすべての版）。
rem ダブルクリックで実行できる。引数はそのまま krbr kicad-plugin uninstall に渡す（例 --dir <...\scripting\plugins>）
setlocal
rem 配布用の一式では同じフォルダの krbr.exe、リポジトリでは rust のビルド結果を使う
set "KRBR=%~dp0krbr.exe"
if not exist "%KRBR%" set "KRBR=%~dp0..\rust\target\release\krbr.exe"
if exist "%KRBR%" (
    "%KRBR%" kicad-plugin uninstall %*
) else (
    rem krbr.exe が無くても消せるよう、既定の場所を直接消す
    for /d %%V in ("%USERPROFILE%\Documents\KiCad\*") do (
        if exist "%%V\scripting\plugins\krbr_preview" (
            rmdir /s /q "%%V\scripting\plugins\krbr_preview"
            echo removed %%V\scripting\plugins\krbr_preview
        )
    )
)
echo.
echo 削除しました。KiCad を開き直すと、ツールバーのボタンが消えます。
endlocal
if "%~1"=="" pause
