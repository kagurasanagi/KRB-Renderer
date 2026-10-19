#pragma once
// カメラのレンズ。
//  - pinhole：ピンホール（被写界深度なし）
//  - thin：薄レンズ。絞りの形（羽根の枚数・回転・丸み）、ピント、軸上色収差・倍率色収差（分光モード）、
//          自然な周辺減光（cos⁴θ）、口径食（前にもう 1 つの円形の開口を置き、2 つの重なりで光が通るかを決める）
//  - realistic：実際のレンズ設計データ（球面の並び）で光線を追跡する。最終出力向け。BDPT / VCM ではカメラへ直接
//          つなぐ戦略（t = 1）を使わない
#include <glm/glm.hpp>

#include <filesystem>
#include <string>
#include <vector>

namespace krbr::scene {

enum class LensModel { Pinhole = 0, Thin = 1, Realistic = 2 };

struct LensParams {
    LensModel model = LensModel::Pinhole;
    float focalLengthMm = 50.0f;
    float fNumber = 2.8f;
    float focusDistanceMm = 0.0f;  // 0 なら注視点までの距離
    int blades = 0;                // 絞りの羽根の枚数（0 か 3 未満で円）
    float bladeRotationDeg = 0.0f;
    float bladeRoundness = 0.0f;   // 0：多角形、1：円
    bool naturalVignetting = false;   // cos⁴θ
    float opticalVignetting = 0.0f;   // 口径食の強さ（画面の隅で、2 つの開口が絞りの直径のこの割合だけずれる）
    float abbeNumber = 0.0f;          // 軸上色収差：レンズのアッベ数（0 で無効。分光モードのみ）
    float lateralCA = 0.0f;           // 倍率色収差：F 線と C 線の像の大きさの差（割合。分光モードのみ）
    std::string lensFile;             // realistic：レンズデータ（空なら内蔵の double Gauss 50mm f/2）
};

// 実レンズの面（物体側から順に）。pbrt と同じ書式：曲率半径, 次の面までの距離, 屈折率（次の面までの媒質）, 有効径
struct LensSurface {
    float radius = 0.0f;     // 0 なら絞り（平面）
    float thickness = 0.0f;
    float ior = 1.0f;
    float aperture = 0.0f;   // 直径（mm）
};

class LensSystem {
public:
    static LensSystem doubleGauss50();
    static bool load(const std::filesystem::path& p, LensSystem& out, std::string& err);

    std::vector<LensSurface> surfaces;
    std::string name;

    // 物体側の頂点を z = 0、フィルム側を負とした各面の頂点の z（最後の面の後ろにフィルムがある）
    std::vector<float> vertexZ() const;
    // 物体側の距離 focusMm（前の頂点から）にピントを合わせるときの、最後の面からフィルムまでの距離
    float filmDistanceFor(float focusMm) const;
    float effectiveFocalLength() const;
    // フィルムの中央での明るさ（cos⁴θ · 後ろの開口の面積 / 距離² の、光が通る割合を含めた平均）。画像の明るさをピンホールにそろえる
    float centerWeight(float filmDistance) const;

    // 光線の追跡（レンズの座標：x 右、y 上、z が物体側）。物体側から入れる場合は fromScene = true。通らなければ false
    bool trace(glm::dvec3& o, glm::dvec3& d, bool fromScene, float filmDistance) const;
};

}  // namespace krbr::scene
