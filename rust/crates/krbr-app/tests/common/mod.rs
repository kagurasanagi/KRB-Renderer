//! krbr-app のテストの共通部分
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// テストで使う基板：環境変数 KRBR_TEST_PCB、無ければ <repo>/kicad/mini_dsp_radio.kicad_pcb（どちらも読むだけで、書き換えるテストはコピーを使う）
pub fn test_board() -> PathBuf {
    match std::env::var_os("KRBR_TEST_PCB") {
        Some(p) => PathBuf::from(p),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../kicad/mini_dsp_radio.kicad_pcb"),
    }
}

/// 全体設定のフォルダをテスト用にし、基板の設定ファイル <名前>.krbr もそこへ置く（テストの基板のフォルダに書かない）
pub fn use_config_dir(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("settings.toml"), "board_settings = \"config_dir\"\n").unwrap();
    std::env::set_var("KRBR_CONFIG_DIR", dir);
}

/// KRBR_TEST_PCB で別の基板を指定したか（既定の基板の座標に合わせた設定を使わない）
pub fn custom_board() -> bool {
    std::env::var_os("KRBR_TEST_PCB").is_some()
}
