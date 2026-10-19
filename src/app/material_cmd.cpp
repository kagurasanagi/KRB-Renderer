#include "app/material_cmd.hpp"

#include <cstdio>
#include <set>

#include "app/config.hpp"
#include "app/prepare.hpp"
#include "core/hash.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "kicad/model_resolver.hpp"
#include "model/mesh_cache.hpp"

namespace krbr::app {

namespace fs = std::filesystem;

namespace {

struct ModelRef {
    std::string spec;  // 展開前のパス
    std::string hash;
    fs::path file;
};

// --model の指定に一致する、基板の中のモデルを探す（展開前のパス、ファイル名、実ファイルのパス）
std::vector<ModelRef> findModels(const kicad::Board& board, const kicad::ModelResolver& resolver,
                                 const std::string& query) {
    std::vector<ModelRef> out;
    std::set<std::string> seen;
    const fs::path qp = platform::u8path(query);
    std::error_code ec;
    for (const auto& fp : board.footprints) {
        for (const auto& m : fp.models) {
            const auto r = resolver.resolve(m.path);
            const std::string fname = platform::pathToUtf8(platform::u8path(m.path).filename());
            bool match = m.path == query || fname == query;
            if (!match && r.found) {
                match = platform::pathToUtf8(r.path.filename()) == query ||
                        (fs::exists(qp, ec) && fs::equivalent(qp, r.path, ec));
            }
            if (!match || seen.count(m.path)) continue;
            seen.insert(m.path);
            ModelRef ref{m.path, {}, r.found ? r.path : fs::path{}};
            if (r.found)
                if (auto bytes = platform::readBinaryFile(r.path)) ref.hash = toHex(xxh64(bytes->data(), bytes->size()));
            out.push_back(std::move(ref));
        }
    }
    return out;
}

}  // namespace

bool setMaterial(const MaterialSetOptions& o, std::string& err, std::string* written) {
    if (o.ref.empty() == o.model.empty()) {
        err = "specify exactly one of --ref or --model";
        return false;
    }
    if (o.global && o.model.empty()) {
        err = "--global can only be used with --model (per-reference overrides belong to the project)";
        return false;
    }
    auto board = loadBoardOrReport(o.pcb);
    if (!board) {
        err = "cannot load board";
        return false;
    }
    MaterialSetup ms = loadMaterialSetup(board->file);
    if (!ms.library.find(o.material)) {
        err = "material '" + o.material + "' is not in the material library (see: krbr material list)";
        return false;
    }
    auto assign = [&](scene::PartAssignment& a) {
        if (o.part.empty() || o.part == "*") {
            a.material = o.material;
            if (o.part == "*") a.parts.erase("*");
        } else {
            a.parts[o.part] = o.material;
        }
    };

    if (!o.ref.empty()) {
        bool found = false;
        for (const auto& fp : board->footprints) found = found || fp.reference == o.ref;
        if (!found) log::warn("reference {} is not on this board (written anyway)", o.ref);
        assign(ms.project.refs[o.ref]);
        if (!ms.project.save(ms.projectFile, err)) return false;
        if (written) *written = platform::pathToUtf8(ms.projectFile);
        return true;
    }

    const ProjectConfig cfg = loadProjectConfig(board->file);
    kicad::ModelResolver resolver(board->projectDir, cfg.pathVars, cfg.modelSearchDirs);
    const auto models = findModels(*board, resolver, o.model);
    if (models.empty()) {
        err = "no 3D model on this board matches '" + o.model + "'";
        return false;
    }
    if (models.size() > 1) {
        err = "'" + o.model + "' matches several models; use the full path as written in the board:";
        for (const auto& m : models) err += "\n  " + m.spec;
        return false;
    }
    const ModelRef& m = models.front();
    if (o.global) {
        assign(ms.global.upsert(m.spec, m.hash).assign);
        if (!ms.global.save(ms.globalFile, err)) return false;
        if (written) *written = platform::pathToUtf8(ms.globalFile);
    } else {
        assign(ms.project.upsert(m.spec, m.hash).assign);
        if (!ms.project.save(ms.projectFile, err)) return false;
        if (written) *written = platform::pathToUtf8(ms.projectFile);
    }
    return true;
}

std::vector<scene::MaterialAssignment> computeAssignments(const std::string& pcb, std::string& err) {
    std::vector<scene::MaterialAssignment> out;
    auto board = loadBoardOrReport(pcb);
    if (!board) {
        err = "cannot load board";
        return out;
    }
    const ProjectConfig cfg = loadProjectConfig(board->file);
    const MaterialSetup ms = loadMaterialSetup(board->file);
    const scene::MaterialResolver materials(ms.library, ms.project, ms.global);
    kicad::ModelResolver resolver(board->projectDir, cfg.pathVars, cfg.modelSearchDirs);
    model::ModelLibrary library(platform::cacheDir(), model::MeshParams{}, true);
    for (const auto& fp : board->footprints) {
        for (const auto& m : fp.models) {
            if (m.hidden) continue;
            const auto r = resolver.resolve(m.path);
            if (!r.found) continue;
            const auto lr = library.get(r.path);
            if (!lr.mesh) continue;
            const std::string modelFile = platform::pathToUtf8(r.path.filename());
            for (const auto& part : lr.mesh->parts) {
                const scene::ResolvedMaterial res =
                    materials.resolve({fp.reference, m.path, lr.fileHash, modelFile, &part});
                scene::MaterialAssignment a;
                a.reference = fp.reference;
                a.footprint = fp.libId;
                a.modelSpec = m.path;
                a.modelFile = modelFile;
                a.modelHash = lr.fileHash;
                for (const auto& k : part.keys()) a.partKeys += (a.partKeys.empty() ? "" : " ") + k;
                a.material = res.name;
                a.source = res.source;
                out.push_back(std::move(a));
            }
        }
    }
    return out;
}

int cmdMaterialSet(const MaterialSetOptions& o) {
    std::string err, written;
    if (!setMaterial(o, err, &written)) {
        log::error("{}", err);
        return 1;
    }
    log::info("wrote {}", written);
    return 0;
}

int cmdMaterialShow(const std::string& pcb, const std::string& refFilter) {
    std::string err;
    auto rows = computeAssignments(pcb, err);
    if (!err.empty()) {
        log::error("{}", err);
        return 1;
    }
    size_t wRef = 3, wFile = 5, wPart = 4, wMat = 8;
    for (const auto& a : rows) {
        if (!refFilter.empty() && a.reference != refFilter) continue;
        wRef = std::max(wRef, a.reference.size());
        wFile = std::max(wFile, a.modelFile.size());
        wPart = std::max(wPart, a.partKeys.size());
        wMat = std::max(wMat, a.material.size());
    }
    std::printf("%-*s  %-*s  %-*s  %-*s  %s\n", int(wRef), "ref", int(wFile), "model", int(wPart), "part",
                int(wMat), "material", "source");
    for (const auto& a : rows) {
        if (!refFilter.empty() && a.reference != refFilter) continue;
        std::printf("%-*s  %-*s  %-*s  %-*s  %s\n", int(wRef), a.reference.c_str(), int(wFile), a.modelFile.c_str(),
                    int(wPart), a.partKeys.c_str(), int(wMat), a.material.c_str(), a.source.c_str());
    }
    return 0;
}

int cmdMaterialList() {
    const auto lib = scene::MaterialLibrary::load(platform::configDir());
    for (const auto& n : lib.names()) std::printf("%-20s %s\n", n.c_str(), lib.sourceOf(n).c_str());
    return 0;
}

}  // namespace krbr::app
