# KiCad の PCB エディタのツールバーに「KRB Renderer でプレビュー」（英語では Preview in KRB Renderer）を出す Action Plugin（KiCad 9 / 10）。
# 押すと、開いている基板のファイルを krbr（Rust 版のビューア）で開く。ビューアは保存を監視しているので、
# KiCad で保存するたびに描き直される。
# インストール：ビューアの「設定 → KiCad プラグイン」か krbr kicad-plugin install（このフォルダと krbr.exe の場所を、KiCad の版ごとのユーザーフォルダへ書く）
import hashlib
import os
import subprocess
import tempfile

import pcbnew

try:
    import wx
except ImportError:  # KiCad の外（テスト）では wx が無いことがある
    wx = None

_HERE = os.path.dirname(os.path.abspath(__file__))
# 開いたビューア（基板のパス → プロセス）。同じ基板を二重に開かない
_viewers = {}
# 起動してからこの時間（ミリ秒）のうちに失敗で終わったら、ログの末尾を添えて知らせる。
# 起動の直後の失敗（Vulkan の初期化や基板の読み込みの失敗）を拾うための待ち時間
_STARTUP_CHECK_MS = 3000
# 知らせるときに添えるログの行数
_LOG_TAIL_LINES = 15


# 表示の文（KiCad の表示の言語に合わせる。訳の無い言語は英語）
_TEXTS = {
    "en": {
        "name": "Preview in KRB Renderer",
        "description": "Show the open board in KRB Renderer (a physically based renderer). It is redrawn every time you save.",
        "not_saved": "The board has not been saved yet. Save it once, then press the button again.",
        "no_exe": "krbr.exe was not found (was it moved or deleted?). Start krbr.exe once and it records its new location (see Settings > KiCad plugin), or set its location in the KRBR_EXE environment variable.",
        "already_open": "A viewer for this board is already open. It is redrawn whenever you save in KiCad.",
        "exited": "The viewer stopped right after starting (exit code {code}).\nLog: {log}",
    },
    "ja": {
        "name": "KRB Renderer でプレビュー",
        "description": "開いている基板を KRB Renderer（物理ベースのレンダラー）で表示します。保存するたびに描き直します。",
        "not_saved": "基板がまだ保存されていません。いったん保存してから押してください。",
        "no_exe": "krbr.exe が見つかりません（移動・削除した？）。krbr.exe を一度起動すると、新しい場所を書き直します（「設定 → KiCad プラグイン」）。または環境変数 KRBR_EXE に場所を設定してください。",
        "already_open": "この基板のビューアはもう開いています。KiCad で保存すると、そちらが描き直されます。",
        "exited": "ビューアが起動の直後に終了しました（終了コード {code}）。\nログ: {log}",
    },
}


def _language():
    """KiCad の表示の言語（"ja" など）。KiCad の中では wx のロケールが KiCad で選んだ言語になっている"""
    try:
        loc = wx.GetLocale() if wx is not None else None
        name = loc.GetCanonicalName() if loc is not None else ""
    except Exception:
        name = ""
    return name.split("_")[0].lower()


def _text(key):
    return _TEXTS.get(_language(), _TEXTS["en"])[key]


def _message(text, title="KRB Renderer"):
    if wx is not None and wx.GetApp() is not None:
        wx.MessageBox(text, title, wx.OK | wx.ICON_INFORMATION)
    else:
        print(f"{title}: {text}")


def find_krbr():
    """krbr.exe の場所：環境変数 KRBR_EXE、インストール時に書いた krbr_path.txt の順"""
    env = os.environ.get("KRBR_EXE", "")
    if env and os.path.isfile(env):
        return env
    try:
        with open(os.path.join(_HERE, "krbr_path.txt"), encoding="utf-8") as f:
            p = f.read().strip()
        if p and os.path.isfile(p):
            return p
    except OSError:
        pass
    return None


def _log_path(key):
    """ビューアのログのファイル。基板ごとに分ける（ほかの基板のビューアのログを切り詰めないように）"""
    digest = hashlib.sha1(key.encode("utf-8", "surrogatepass")).hexdigest()[:12]
    return os.path.join(tempfile.gettempdir(), f"krbr_preview_{digest}.log")


def _log_tail(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return "".join(f.readlines()[-_LOG_TAIL_LINES:]).strip()
    except OSError:
        return ""


def _report_if_failed(proc, log_file):
    """起動の直後にビューアが失敗で終わっていたら、ログの末尾を添えて知らせる（正常に閉じた場合は何もしない）"""
    code = proc.poll()
    if code is None or code == 0:
        return
    text = _text("exited").format(code=code, log=log_file)
    tail = _log_tail(log_file)
    _message(f"{text}\n\n{tail}" if tail else text)


def launch(board_file, exe=None):
    """基板のファイルをビューアで開く。戻り値は (開いたか, 伝えること)"""
    if not board_file:
        return False, _text("not_saved")
    exe = exe or find_krbr()
    if not exe:
        return False, _text("no_exe")
    key = os.path.normcase(os.path.abspath(board_file))
    p = _viewers.get(key)
    if p is not None and p.poll() is None:
        return False, _text("already_open")
    flags = 0
    if os.name == "nt":
        flags = 0x08000000 | 0x00000200  # CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP（コンソールを出さない）
    log_file = _log_path(key)
    # 子のプロセスにはハンドルの複製が渡るので、こちら側は起動したらすぐ閉じる
    with open(log_file, "w", encoding="utf-8", errors="replace") as log:
        # 作業フォルダを基板のフォルダにする（ビューアで S を押したときの画像は、--out を付けなければ基板のフォルダに入る）
        proc = subprocess.Popen([exe, board_file], cwd=os.path.dirname(board_file) or None, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, creationflags=flags, close_fds=True)
    _viewers[key] = proc
    if wx is not None and wx.GetApp() is not None:
        wx.CallLater(_STARTUP_CHECK_MS, _report_if_failed, proc, log_file)
    return True, ""


class KrbrPreview(pcbnew.ActionPlugin):
    def defaults(self):
        self.name = _text("name")
        self.category = "Render"
        self.description = _text("description")
        self.show_toolbar_button = True
        self.icon_file_name = os.path.join(_HERE, "krbr_preview.png")
        self.dark_icon_file_name = self.icon_file_name

    def Run(self):
        board = pcbnew.GetBoard()
        ok, msg = launch(board.GetFileName() if board else "")
        if not ok:
            _message(msg)
