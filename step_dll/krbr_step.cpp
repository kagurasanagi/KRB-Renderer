// STEP 変換器 DLL：OpenCASCADE（XCAF）で STEP を読み、面ごとの色と部位名つきで三角形に分割して .krbm に書く。
// OpenCASCADE に触るのはこのファイルだけ
#include "krbr_step.h"

#include <BRepMesh_IncrementalMesh.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <Message.hxx>
#include <Message_Messenger.hxx>
#include <NCollection_Sequence.hxx>
#include <Poly_Triangle.hxx>
#include <Quantity_ColorRGBA.hxx>
#include <RWMesh_FaceIterator.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <TCollection_AsciiString.hxx>
#include <TDF_Label.hxx>
#include <TDataStd_Name.hxx>
#include <TDocStd_Document.hxx>
#include <TopoDS_Shape.hxx>
#include <XCAFApp_Application.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <XCAFPrs_DocumentExplorer.hxx>

#include <cstring>
#include <filesystem>
#include <mutex>
#include <string>
#include <string_view>

#include "mesh_file.hpp"

namespace {

using namespace krbr::model;

// 部位の表（同じ名前・同じ色は 1 つの番号にまとめる）
struct PartTable {
    std::vector<ModelPart> parts;
    uint32_t index(const ModelPart& p) {
        for (size_t i = 0; i < parts.size(); ++i) {
            const ModelPart& q = parts[i];
            if (q.name == p.name && q.hasColor == p.hasColor &&
                glm::all(glm::lessThan(glm::abs(q.srgb - p.srgb), glm::vec3(1e-4f))))
                return static_cast<uint32_t>(i);
        }
        parts.push_back(p);
        return static_cast<uint32_t>(parts.size() - 1);
    }
};

std::string labelName(const TDF_Label& label) {
    Handle(TDataStd_Name) n;
    if (label.IsNull() || !label.FindAttribute(TDataStd_Name::GetID(), n)) return {};
    return TCollection_AsciiString(n->Get()).ToCString();  // UTF-8
}

std::once_flag g_occtInit;

// メモリを確保しないで書く（例外の後始末の中からも呼ぶので）
void setError(char* err, size_t len, std::string_view msg) noexcept {
    if (!err || len == 0) return;
    const size_t n = std::min(msg.size(), len - 1);
    std::memcpy(err, msg.data(), n);
    err[n] = '\0';
}

std::filesystem::path u8path(const char* s) { return std::filesystem::path(reinterpret_cast<const char8_t*>(s)); }

// STEP を読んで分割し、ModelMesh にする。失敗なら error を埋めて false
bool tessellate(const std::filesystem::path& p, double linear, double angular, ModelMesh& out, std::string& error) {
    std::call_once(g_occtInit, [] {
        // OCCT の標準出力へのメッセージを止める
        Message::DefaultMessenger()->RemovePrinters(STANDARD_TYPE(Message_Printer));
    });
    try {
        Handle(XCAFApp_Application) app = XCAFApp_Application::GetApplication();
        Handle(TDocStd_Document) doc;
        app->NewDocument("BinXCAF", doc);
        XCAFDoc_DocumentTool::SetLengthUnit(doc, 1.0, UnitsMethods_LengthUnit_Millimeter);

        STEPCAFControl_Reader reader;
        reader.SetColorMode(true);
        reader.SetNameMode(true);  // 部位の名前（XDE）を材質の割り当てに使う
        reader.SetLayerMode(false);
        reader.SetPropsMode(false);
        const std::string u8 = reinterpret_cast<const char*>(p.u8string().c_str());
        if (reader.ReadFile(u8.c_str()) != IFSelect_RetDone) {
            error = "STEP read failed";
            app->Close(doc);
            return false;
        }
        if (!reader.Transfer(doc)) {
            error = "STEP transfer failed";
            app->Close(doc);
            return false;
        }
        Handle(XCAFDoc_ShapeTool) shapes = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
        NCollection_Sequence<TDF_Label> roots;
        shapes->GetFreeShapes(roots);
        for (int i = 1; i <= roots.Length(); ++i) {
            TopoDS_Shape s = XCAFDoc_ShapeTool::GetShape(roots.Value(i));
            BRepMesh_IncrementalMesh mesher(s, linear, false, angular, true);
            (void)mesher;
        }

        PartTable table;
        // ルート（製品全体）の名前は部位の区別にならないので、部位名からは除く
        std::string rootName;
        if (roots.Length() == 1) rootName = labelName(roots.Value(1));
        for (XCAFPrs_DocumentExplorer ex(doc, XCAFPrs_DocumentExplorerFlags_OnlyLeafNodes); ex.More(); ex.Next()) {
            const XCAFPrs_DocumentNode& node = ex.Current();
            std::string nodeName = labelName(node.RefLabel);
            if (nodeName.empty()) nodeName = labelName(node.Label);
            if (nodeName == rootName) nodeName.clear();
            for (RWMesh_FaceIterator fit(node.RefLabel, node.Location, true, node.Style); fit.More(); fit.Next()) {
                if (fit.IsEmptyMesh()) continue;
                ModelPart part;
                part.name = nodeName;
                if (fit.HasFaceColor()) {
                    const Quantity_ColorRGBA& c = fit.FaceColor();
                    double r, g, b;
                    // STEP の色値は表示用（sRGB）。OCCT はリニアに変換して持っているので、sRGB の値で受け取る
                    c.GetRGB().Values(r, g, b, Quantity_TOC_sRGB);
                    part.srgb = {static_cast<float>(r), static_cast<float>(g), static_cast<float>(b)};
                    part.alpha = c.Alpha();
                    part.hasColor = true;
                }
                const uint32_t ci = table.index(part);
                const uint32_t base = static_cast<uint32_t>(out.mesh.positions.size());
                const int lo = fit.NodeLower(), hi = fit.NodeUpper();
                for (int n = lo; n <= hi; ++n) {
                    const gp_Pnt pt = fit.NodeTransformed(n);
                    const gp_Dir nd = fit.NormalTransformed(n);
                    out.mesh.addVertex({static_cast<float>(pt.X()), static_cast<float>(pt.Y()), static_cast<float>(pt.Z())},
                                       {static_cast<float>(nd.X()), static_cast<float>(nd.Y()), static_cast<float>(nd.Z())});
                }
                for (int e = fit.ElemLower(); e <= fit.ElemUpper(); ++e) {
                    int a, b, c;
                    fit.TriangleOriented(e).Get(a, b, c);
                    out.mesh.addTriangle(base + static_cast<uint32_t>(a - lo), base + static_cast<uint32_t>(b - lo),
                                         base + static_cast<uint32_t>(c - lo), ci);
                }
            }
        }
        out.parts = std::move(table.parts);
        app->Close(doc);
        if (out.mesh.triangleCount() == 0) {
            error = "tessellation produced no triangles";
            return false;
        }
        return true;
    } catch (const Standard_Failure& e) {
        error = std::string("OpenCASCADE exception: ") + e.what();
    } catch (const std::exception& e) {
        error = e.what();
    }
    return false;
}

}  // namespace

extern "C" {

// C の関数から C++ の例外を外へ出すと std::terminate で呼んだプロセス（krbr.exe）ごと落ちるので、
// 本体全体を try で囲み、どんな例外もエラーの値（4）にして返す
KRBR_STEP_API int krbr_step_to_mesh_file(const char* step_file, double linear_deflection, double angular_deflection,
                                         const char* out_file, char* err, size_t err_len) {
    try {
        if (!step_file || !out_file) {
            setError(err, err_len, "null path");
            return 1;
        }
        ModelMesh mesh;
        std::string error;
        if (!tessellate(u8path(step_file), linear_deflection, angular_deflection, mesh, error)) {
            setError(err, err_len, error);
            return 2;
        }
        if (!writeMeshFile(u8path(out_file), mesh)) {
            setError(err, err_len, std::string("cannot write ") + out_file);
            return 3;
        }
        return 0;
    } catch (const std::exception& e) {
        setError(err, err_len, e.what());
    } catch (...) {
        setError(err, err_len, "unexpected exception");
    }
    return 4;
}

KRBR_STEP_API int krbr_step_api_version(void) { return KRBR_STEP_API_VERSION; }
KRBR_STEP_API int krbr_step_mesh_file_version(void) { return static_cast<int>(kMeshFileVersion); }

}  // extern "C"
