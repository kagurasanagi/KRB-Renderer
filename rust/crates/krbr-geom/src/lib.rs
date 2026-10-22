//! 基板の形状：KiCad の図形を多角形に（Clipper2）、外形を押し出した板、銅・レジスト・シルクの厚みの凹凸、
//! 層の多角形の集め方、被覆率のラスタライズと距離場。

pub mod board_mesh;
pub mod raster;
pub mod relief;
pub mod shapes;
pub mod svg_reader;
pub mod tiles;

pub use board_mesh::{build_board_geometry, extrude_region, BoardBuildOptions, BoardGeometry, BoardMat};
pub use krbr_kicad::Box2;
pub use raster::{Grid, LayerArtOptions, Side, SidePaths};
pub use relief::{ReliefDimple, ReliefInput, ReliefMat, ReliefParams};
pub use clipper2::FillRule;
pub use shapes::{Path, Paths, Vec2};
pub use tiles::{ReliefTileCache, SdfPatch};
