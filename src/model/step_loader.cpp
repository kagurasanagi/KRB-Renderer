// STEP 読み込み（OpenCASCADE / XCAF）。面ごとの色も取り出す
#include <algorithm>
#include <chrono>
#include <cmath>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "model/model_mesh.hpp"

#ifdef KRBR_HAVE_OCCT
#include <BRepMesh_IncrementalMesh.hxx>
#include <IFSelect_ReturnStatus.hxx>
#include <Interface_Static.hxx>
#include <Message.hxx>
#include <Message_Messenger.hxx>
#include <Poly_Triangle.hxx>
#include <Quantity_ColorRGBA.hxx>
#include <RWMesh_FaceIterator.hxx>
#include <TCollection_AsciiString.hxx>
#include <TDataStd_Name.hxx>
#include <STEPCAFControl_Reader.hxx>
#include <NCollection_Sequence.hxx>
#include <TDF_Label.hxx>
#include <TDocStd_Document.hxx>
#include <TopoDS_Shape.hxx>
#include <XCAFApp_Application.hxx>
#include <XCAFDoc_DocumentTool.hxx>
#include <XCAFDoc_ShapeTool.hxx>
#include <XCAFPrs_DocumentExplorer.hxx>
#include <mutex>
#endif

namespace krbr::model {

namespace {

std::string lowerExt(const std::filesystem::path& p) {
    std::string e = platform::pathToUtf8(p.extension());
    std::transform(e.begin(), e.end(), e.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return e;
}

class StepLoader final : public IModelLoader {
public:
    const char* name() const override { return "occt-step"; }
    bool canLoad(const std::filesystem::path& p) const override {
        const std::string e = lowerExt(p);
        return e == ".step" || e == ".stp";
    }
    LoadResult load(const std::filesystem::path& p, const MeshParams& params) const override;
};

#ifdef KRBR_HAVE_OCCT

// 部位の表（同じ名前・同じ色は1つの番号にまとめる）
struct PartTable {
    std::vector<ModelPart> parts;
    uint32_t index(const ModelPart& p) {
        for (size_t i = 0; i < parts.size(); ++i) {
            const ModelPart& q = parts[i];
            if (q.name == p.name && q.hasColor == p.hasColor && glm::all(glm::lessThan(glm::abs(q.srgb - p.srgb), glm::vec3(1e-4f))))
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

LoadResult StepLoader::load(const std::filesystem::path& p, const MeshParams& params) const {
    LoadResult res;
    const auto t0 = std::chrono::steady_clock::now();
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
        const std::string u8 = platform::pathToUtf8(p);
        if (reader.ReadFile(u8.c_str()) != IFSelect_RetDone) {
            res.error = "STEP read failed";
            app->Close(doc);
            return res;
        }
        if (!reader.Transfer(doc)) {
            res.error = "STEP transfer failed";
            app->Close(doc);
            return res;
        }
        Handle(XCAFDoc_ShapeTool) shapes = XCAFDoc_DocumentTool::ShapeTool(doc->Main());
        NCollection_Sequence<TDF_Label> roots;
        shapes->GetFreeShapes(roots);
        for (int i = 1; i <= roots.Length(); ++i) {
            TopoDS_Shape s = XCAFDoc_ShapeTool::GetShape(roots.Value(i));
            BRepMesh_IncrementalMesh mesher(s, params.linearDeflection, false, params.angularDeflection, true);
            (void)mesher;
        }

        auto out = std::make_shared<ModelMesh>();
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
                    c.GetRGB().Values(r, g, b, Quantity_TOC_sRGB);  // OCCT 内部はリニア。STEP に書かれた値に戻す
                    part.srgb = {static_cast<float>(r), static_cast<float>(g), static_cast<float>(b)};
                    part.alpha = c.Alpha();
                    part.hasColor = true;
                }
                const uint32_t ci = table.index(part);
                const uint32_t base = static_cast<uint32_t>(out->mesh.positions.size());
                const int lo = fit.NodeLower(), hi = fit.NodeUpper();
                for (int n = lo; n <= hi; ++n) {
                    const gp_Pnt pt = fit.NodeTransformed(n);
                    const gp_Dir nd = fit.NormalTransformed(n);
                    out->mesh.addVertex({static_cast<float>(pt.X()), static_cast<float>(pt.Y()), static_cast<float>(pt.Z())},
                                        {static_cast<float>(nd.X()), static_cast<float>(nd.Y()), static_cast<float>(nd.Z())});
                }
                for (int e = fit.ElemLower(); e <= fit.ElemUpper(); ++e) {
                    int a, b, c;
                    fit.TriangleOriented(e).Get(a, b, c);
                    out->mesh.addTriangle(base + static_cast<uint32_t>(a - lo), base + static_cast<uint32_t>(b - lo),
                                          base + static_cast<uint32_t>(c - lo), ci);
                }
            }
        }
        out->parts = std::move(table.parts);
        app->Close(doc);
        if (out->mesh.triangleCount() == 0) {
            res.error = "tessellation produced no triangles";
            return res;
        }
        res.mesh = std::move(out);
    } catch (const Standard_Failure& e) {
        res.error = std::string("OpenCASCADE exception: ") + e.what();
    } catch (const std::exception& e) {
        res.error = e.what();
    }
    res.seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    return res;
}

#else

LoadResult StepLoader::load(const std::filesystem::path&, const MeshParams&) const {
    LoadResult res;
    res.error = "built without OpenCASCADE";
    return res;
}

#endif

}  // namespace

std::unique_ptr<IModelLoader> createStepLoader() { return std::make_unique<StepLoader>(); }

}  // namespace krbr::model
