# KiCad が scripting/plugins/krbr_preview を読み込んだときに、ツールバーのボタンを登録する
from .krbr_preview_action import KrbrPreview

KrbrPreview().register()
