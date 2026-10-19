#pragma once
// レンダラに渡すシーン（メッシュ・材質・配置・基板の層テクスチャ）
#include <string>
#include <vector>

#include "geom/board_mesh.hpp"
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
    glm::mat4 xform{1.0f};  // オブジェクト -> ワールド
};

struct BoardMaterialIds {
    uint32_t top = 0, bottom = 0, edge = 0, plated = 0;  // 基板メッシュの面
    uint32_t mask = 0, copper = 0, finish = 0, fr4 = 0, silk = 0;  // 基板面の中で使い分ける材質
};

struct MissingModel {
    std::string reference, footprint, value;
    std::string spec;      // ファイルに書かれた指定
    std::string resolved;  // 展開・解決を試みたパス
    std::string reason;
};

struct Scene {
    std::vector<SceneMesh> meshes;
    std::vector<Material> materials;
    std::vector<Instance> instances;
    geom::BoardGeometry board;
    raster::LayerTextures layers;
    BoardMaterialIds boardMats;
    std::vector<MissingModel> missing;
    int modelInstances = 0;
    int placeholders = 0;
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
};

struct SceneContext {
    const kicad::Board& board;
    const kicad::ModelResolver& resolver;
    model::ModelLibrary& library;
    const ModelMap& modelMap;
    const SceneOptions& options;
};

Scene buildScene(const SceneContext& ctx);

}  // namespace krbr::scene
