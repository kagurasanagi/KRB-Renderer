# krbr bench の同梱の基板

`krbr_bench.kicad_pcb` は `scripts/gen_bench_board.py` が作ったベンチマーク用の基板です（実在の回路ではありません）。
手で書き換えずに、スクリプトで作り直してください。基板を作り直すと、内容のハッシュが変わるので、それより前の `krbr bench` の記録とは比べられなくなります。

## 3D モデルとフットプリント

`3dmodels/` の STEP と、基板に埋め込んだフットプリントは、KiCad 10.0.6 に付属する標準のライブラリのものです。STEP は手を加えずに写しています。

- 出どころ：KiCad Libraries（<https://gitlab.com/kicad/libraries/kicad-packages3D>、<https://gitlab.com/kicad/libraries/kicad-footprints>）
- 作者：KiCad Libraries の寄稿者のみなさん
- ライセンス：Creative Commons Attribution-ShareAlike 4.0 International（CC-BY-SA 4.0）に、次の例外条項が付いたもの（<https://www.kicad.org/libraries/license/>）

> To the extent that the creation of electronic designs that use 'Licensed Material' can be considered to be 'Adapted Material', then the copyright holder waives article 3 of the license with respect to these designs and any generated files which use data provided as part of the 'Licensed Material'.

ライセンスの全文：<https://creativecommons.org/licenses/by-sa/4.0/legalcode>
