#pragma once
// レンダラに渡すシーン（メッシュ・材質・配置・基板の層テクスチャ）
#include <string>
#include <vector>

#include "geom/board_mesh.hpp"
#include "geom/relief.hpp"
#include "kicad/board.hpp"
#include "kicad/model_resolver.hpp"
#include "model/mesh_cache.hpp"
#include "raster/layer_art.hpp"
#include "scene/material.hpp"
#include "scene/model_map.hpp"

namespace krbr::scene {

struct SceneMesh {
    std::string name;
    geom::Mesh mesh;  // triMaterial は Scene::materials の番号
};

struct Instance {
    uint32_t mesh = 0;
    glm::mat4 xform{1.0f};      // オブジェクト -> ワールド
    uint32_t materialBase = 0;  // 材質の番号 = materialBase + 三角形の triMaterial（基板などは 0）
};

// 部品の部位ごとの材質の割り当て（krbr material show 用）
struct MaterialAssignment {
    std::string reference, footprint;
    std::string modelSpec, modelFile, modelHash;
    std::string partKeys;  // 部位のキー（空白区切り）
    std::string material;  // 材質ライブラリの名前
    std::string source;    // どの段階で決まったか
};

struct BoardMaterialIds {
    uint32_t top = 0, bottom = 0, edge = 0, plated = 0;  // 基板メッシュの面
    uint32_t mask = 0, copper = 0, finish = 0, fr4 = 0, silk = 0;  // 基板面の中で使い分ける材質
    uint32_t maskOnFr4 = 0, maskOnCopper = 0;                      // 凹凸を形状にした場合のレジスト面
};

struct MissingModel {
    std::string reference, footprint, value;
    std::string spec;      // ファイルに書かれた指定
    std::string resolved;  // 展開・解決を試みたパス
    std::string reason;
};

// 凹凸メッシュのタイルのキャッシュ（保存のたびに、変わった範囲のタイルだけを作り直す）
struct ReliefTileCache {
    double tileSize = 8.0;  // mm
    kicad::Box2 bounds;
    glm::dvec2 center{0.0};
    double coreHalf = 0.0;
    int nx = 0, ny = 0;
    std::vector<geom::Mesh> tiles[2];  // 表・裏。三角形の材質は Scene::materials の番号
    std::vector<char> valid[2];
    int lastBuilt = 0;  // 直前の構築で作り直したタイルの数

    bool matches(const geom::BoardGeometry& g) const;
    void reset(const geom::BoardGeometry& g);
    void invalidateAll();
    void invalidate(const std::vector<kicad::Box2>& dirty);
    kicad::Box2 tileRect(int index) const;
    int count() const { return nx * ny; }
};

struct Scene {
    std::vector<SceneMesh> meshes;
    std::vector<Material> materials;
    std::vector<Instance> instances;
    geom::BoardGeometry board;
    raster::LayerTextures layers;
    BoardMaterialIds boardMats;
    std::vector<MissingModel> missing;
    std::vector<MaterialAssignment> assignments;
    int modelInstances = 0;
    int placeholders = 0;
    int pendingModels = 0;  // 裏で分割中の 3D モデル（箱で仮表示中）
    geom::Bounds3 bounds;
    std::vector<glm::vec3> framingPoints;  // カメラの自動調整に使う点
};

struct SceneOptions {
    std::string finish = "auto";  // auto / ENIG / HASL / OSP
    glm::vec3 maskColor{0.05f, 0.42f, 0.16f};   // sRGB
    bool maskColorSet = false;                  // false なら stackup の色を使う
    float maskRoughness = 0.18f;
    float maskScatter = 0.3f;                   // レジストの顔料自身が散乱する割合
    glm::vec3 silkColor{0.93f, 0.93f, 0.90f};   // sRGB
    glm::vec3 fr4Color{0.62f, 0.58f, 0.42f};    // sRGB
    double placeholderHeight = 1.0;             // mm
    glm::vec3 placeholderColor{0.30f, 0.36f, 0.50f};  // sRGB
    bool includeDnp = true;
    raster::LayerArtOptions layerArt;
    bool relief = true;                    // 銅・レジスト・シルクの厚みを形状として作る
    bool layerTextures = false;            // 層テクスチャを作る（relief = false のとき、または確認用の出力）
    bool reliefCopperFromStackup = true;   // 銅の厚みは stackup の値を使う
    geom::ReliefParams reliefParams;
    float bumpIntensity = 0.6f;            // 表面の凹凸（バンプ）の強さの倍率。0 で無効（GPU へ送るときに掛ける）
};

struct SceneContext {
    const kicad::Board& board;
    const kicad::ModelResolver& resolver;
    model::ModelLibrary& library;
    const MaterialResolver& materials;
    const SceneOptions& options;
    ReliefTileCache* reliefCache = nullptr;  // 保存の監視中は、前回のタイルを使い回す
    bool asyncModels = false;                // 未分割の STEP は裏で分割し、それまでは箱で仮表示する
};

Scene buildScene(const SceneContext& ctx);

}  // namespace krbr::scene
