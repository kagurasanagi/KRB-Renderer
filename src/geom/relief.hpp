#pragma once
// 基板表面の凹凸（銅箔・レジスト・シルクの厚み）を実際の形状として作る。
// 領域ごとに上面と側壁を作り、側壁は基材の面まで下ろす（隣の高い領域の中に入る部分は見えない）。
#include "geom/board_mesh.hpp"

namespace krbr::geom {

struct ReliefParams {
    double copper = 0.035;           // 銅箔（mm）
    double maskOnCopper = 0.012;     // 銅の上のレジスト
    double maskOnSubstrate = 0.025;  // 基材の上のレジスト（銅の間を埋めるので厚い）
    double silk = 0.015;             // シルク
};

// 凹凸メッシュの三角形ごとの材質区分
enum ReliefMat : uint32_t {
    ReliefFr4 = 0,         // むき出しの基材
    ReliefFinish = 1,      // むき出しの銅（表面処理）
    ReliefMaskOnFr4 = 2,   // 基材の上のレジスト
    ReliefMaskOnCopper = 3,  // 銅の上のレジスト
    ReliefSilk = 4,
};

struct ReliefInput {
    Paths copper, maskOpen, silk;  // 面ごとの多角形（layer_art::collectSidePaths）
    Paths tentedHoles;             // レジストでふさがれたビアの穴（面を張る）
    const Paths* base = nullptr;   // 面を張る範囲を前もって求めてある場合（タイル単位で作るとき）
};

// 面を張る範囲：穴を開けた基板＋レジストでふさがれたビアの穴
Paths reliefBase(const BoardGeometry& geo, const Paths& tentedHoles);

// side: 0=表（+Y 向き）, 1=裏（-Y 向き）。基材の面は ±coreHalf
// clip を渡すと、その矩形の中だけを作る（タイル単位の作り直し用。入力も同じ矩形で切り抜いておくこと）
void buildRelief(const BoardGeometry& geo, int side, const ReliefInput& in, const ReliefParams& p, Mesh& out,
                 const kicad::Box2* clip = nullptr);

// 銅上のレジストの上面が板厚の半分（t/2）に来るように、基材の半分の厚みを決める
inline double reliefCoreHalf(double thickness, const ReliefParams& p) {
    return thickness / 2 - p.copper - p.maskOnCopper;
}

}  // namespace krbr::geom
