//! `.kicad_pcb` の読み込み。座標はすべて基板座標（mm、x 右・y 下）に変換済み。

pub mod board;
pub mod curves;
pub mod parser;
pub mod resolver;
pub mod settings;
pub mod sexpr;
pub mod stroke_silk;

pub use board::*;
pub use parser::load_board;
pub use resolver::{ModelResolver, ResolvedModel};
pub use stroke_silk::stroke_silk_only_board;
