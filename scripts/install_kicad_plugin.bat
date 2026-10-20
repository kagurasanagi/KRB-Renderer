@echo off
rem （このファイルは Shift-JIS・CRLF。cmd が日本語を正しく読めるように）
rem KiCad の PCB エディタのツールバーに「KRB Render でプレビュー」のボタンを足す（KiCad 9 以降のすべての版）。
rem ダブルクリックで実行できる。引数はそのまま krbr kicad-plugin install に渡す（例 --dir <...\scripting\plugins>）
setlocal
rem 配布用の一式では同じフォルダの krbr.exe、リポジトリでは rust のビルド結果を使う
set "KRBR=%~dp0krbr.exe"
if not exist "%KRBR%" set "KRBR=%~dp0..\rust\target\release\krbr.exe"
if not exist "%KRBR%" (
    echo krbr.exe が見つかりません: %KRBR%
    echo 先に rust フォルダで cargo build --release を実行してください。
    goto :end
)
"%KRBR%" kicad-plugin install %*
if errorlevel 1 (
    echo インストールに失敗しました。
) else (
    echo.
    echo インストールしました。KiCad を開き直すか、PCB エディタの「ツール ^> 外部プラグイン ^> プラグインを再読み込み」を実行してください。
)
:end
endlocal
rem エクスプローラーから実行したときに、結果を読めるよう止める（引数があるとき＝コマンドラインからは止めない）
if "%~1"=="" pause
