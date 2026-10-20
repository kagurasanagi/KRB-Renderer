# KiCad の PCB エディタのツールバーに「KRB Render でプレビュー」を出す Action Plugin（KiCad 9 / 10）。
# 押すと、開いている基板のファイルを krbr（Rust 版のビューア）で開く。ビューアは保存を監視しているので、
# KiCad で保存するたびに描き直される。
# インストール：krbr kicad-plugin install（このフォルダと krbr.exe の場所を KiCad のユーザーフォルダへ書く）
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


def _message(text, title="KRB Render"):
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


def launch(board_file, exe=None):
    """基板のファイルをビューアで開く。戻り値は (開いたか, 伝えること)"""
    if not board_file:
        return False, "基板がまだ保存されていません。いったん保存してから押してください。"
    exe = exe or find_krbr()
    if not exe:
        return False, "krbr.exe が見つかりません。krbr kicad-plugin install でインストールし直すか、環境変数 KRBR_EXE に場所を設定してください。"
    key = os.path.normcase(os.path.abspath(board_file))
    p = _viewers.get(key)
    if p is not None and p.poll() is None:
        return False, "この基板のビューアはもう開いています。KiCad で保存すると、そちらが描き直されます。"
    log = open(os.path.join(tempfile.gettempdir(), "krbr_preview.log"), "w", encoding="utf-8", errors="replace")
    flags = 0
    if os.name == "nt":
        flags = 0x08000000 | 0x00000200  # CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP（コンソールを出さない）
    # 作業フォルダを基板のフォルダにする（ビューアで S を押したとき、画像は <基板のフォルダ>\out\ に入る）
    _viewers[key] = subprocess.Popen([exe, board_file], cwd=os.path.dirname(board_file) or None, stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, creationflags=flags, close_fds=True)
    return True, ""


class KrbrPreview(pcbnew.ActionPlugin):
    def defaults(self):
        self.name = "KRB Render でプレビュー"
        self.category = "Render"
        self.description = "開いている基板を KRB Render（物理ベースのレンダラー）で表示します。保存するたびに描き直します。"
        self.show_toolbar_button = True
        self.icon_file_name = os.path.join(_HERE, "krbr_preview.png")
        self.dark_icon_file_name = self.icon_file_name

    def Run(self):
        board = pcbnew.GetBoard()
        ok, msg = launch(board.GetFileName() if board else "")
        if not ok:
            _message(msg)
