@echo off
rem （このファイルは Shift-JIS・CRLF。cmd が日本語を正しく読めるように）
rem KiCad のプラグイン「KRB Render でプレビュー」を削除する（KiCad 9 以降のすべての版）。
rem ダブルクリックで実行できる。引数はそのまま krbr kicad-plugin uninstall に渡す（例 --dir <...\scripting\plugins>）
rem 削除する場所の決め方（KICAD_DOCUMENTS_HOME、KiCad 9.0 以上の版のフォルダ）は krbr.exe の中（kicad_plugin.rs）の 1 か所だけにある。
rem krbr.exe が無いときは、ここで同じ規則をまねて消すことはせず、手で消す場所を案内する
setlocal
rem 配布用の一式では同じフォルダの krbr.exe、リポジトリでは rust のビルド結果を使う
set "KRBR=%~dp0krbr.exe"
if not exist "%KRBR%" set "KRBR=%~dp0..\rust\target\release\krbr.exe"
if not exist "%KRBR%" (
    echo krbr.exe が見つからないので、自動では削除できません: %KRBR%
    echo 次のフォルダを手で削除してください。KiCad の版ごとにあります:
    if defined KICAD_DOCUMENTS_HOME (
        echo   %KICAD_DOCUMENTS_HOME%\^<版^>\scripting\plugins\krbr_preview
    ) else (
        echo   %USERPROFILE%\Documents\KiCad\^<版^>\scripting\plugins\krbr_preview
    )
    goto :end
)
"%KRBR%" kicad-plugin uninstall %*
if errorlevel 1 (
    echo 削除に失敗しました。
) else (
    echo.
    echo 終わりました。削除したフォルダは上に表示しています。何も表示されていなければ、プラグインは入っていませんでした。
    echo KiCad を開き直すと、ツールバーのボタンが消えます。
)
:end
endlocal
rem エクスプローラーから実行したときに、結果を読めるよう止める（引数があるとき＝コマンドラインからは止めない）
if "%~1"=="" pause
