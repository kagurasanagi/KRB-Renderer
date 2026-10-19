#include "scene/material_io.hpp"

namespace krbr::scene {

namespace {

ParamDesc F(const char* k, float Material::* p) { return {k, ParamType::Float, p, nullptr, nullptr}; }
ParamDesc C(const char* k, glm::vec3 Material::* p) { return {k, ParamType::Color, nullptr, p, nullptr}; }
ParamDesc B(const char* k, bool Material::* p) { return {k, ParamType::Bool, nullptr, nullptr, p}; }

const char* bumpName(BumpPattern p) {
    switch (p) {
        case BumpPattern::Noise: return "noise";
        case BumpPattern::Weave: return "weave";
        default: return "none";
    }
}

toml::array colorArray(glm::vec3 c) { return toml::array{double(c.r), double(c.g), double(c.b)}; }

}  // namespace

const std::vector<ParamDesc>& openPbrParams() {
    static const std::vector<ParamDesc> params = {
        F("base_weight", &Material::base_weight),
        C("base_color", &Material::base_color),
        F("base_diffuse_roughness", &Material::base_diffuse_roughness),
        F("base_metalness", &Material::base_metalness),
        F("specular_weight", &Material::specular_weight),
        C("specular_color", &Material::specular_color),
        F("specular_roughness", &Material::specular_roughness),
        F("specular_roughness_anisotropy", &Material::specular_roughness_anisotropy),
        F("specular_ior", &Material::specular_ior),
        F("transmission_weight", &Material::transmission_weight),
        C("transmission_color", &Material::transmission_color),
        F("transmission_depth", &Material::transmission_depth),
        C("transmission_scatter", &Material::transmission_scatter),
        F("transmission_scatter_anisotropy", &Material::transmission_scatter_anisotropy),
        F("transmission_dispersion_scale", &Material::transmission_dispersion_scale),
        F("transmission_dispersion_abbe_number", &Material::transmission_dispersion_abbe_number),
        F("subsurface_weight", &Material::subsurface_weight),
        C("subsurface_color", &Material::subsurface_color),
        F("subsurface_radius", &Material::subsurface_radius),
        C("subsurface_radius_scale", &Material::subsurface_radius_scale),
        F("subsurface_scatter_anisotropy", &Material::subsurface_scatter_anisotropy),
        F("fuzz_weight", &Material::fuzz_weight),
        C("fuzz_color", &Material::fuzz_color),
        F("fuzz_roughness", &Material::fuzz_roughness),
        F("coat_weight", &Material::coat_weight),
        C("coat_color", &Material::coat_color),
        F("coat_roughness", &Material::coat_roughness),
        F("coat_roughness_anisotropy", &Material::coat_roughness_anisotropy),
        F("coat_ior", &Material::coat_ior),
        F("coat_darkening", &Material::coat_darkening),
        F("thin_film_weight", &Material::thin_film_weight),
        F("thin_film_thickness", &Material::thin_film_thickness),
        F("thin_film_ior", &Material::thin_film_ior),
        F("emission_luminance", &Material::emission_luminance),
        C("emission_color", &Material::emission_color),
        F("geometry_opacity", &Material::geometry_opacity),
        B("geometry_thin_walled", &Material::geometry_thin_walled),
    };
    return params;
}

void readMaterial(const toml::table& t, Material& m, std::vector<std::string>* unknown) {
    const auto& params = openPbrParams();
    for (const auto& [key, node] : t) {
        const std::string k(key.str());
        if (k == "krbr") continue;
        const ParamDesc* d = nullptr;
        for (const auto& p : params)
            if (k == p.key) d = &p;
        if (!d) {
            if (unknown) unknown->push_back(k);
            continue;
        }
        switch (d->type) {
            case ParamType::Float:
                if (auto v = node.value<double>()) m.*(d->f) = static_cast<float>(*v);
                break;
            case ParamType::Color:
                if (const toml::array* a = node.as_array(); a && a->size() == 3)
                    for (int i = 0; i < 3; ++i) (m.*(d->c))[i] = static_cast<float>((*a)[i].value_or(0.0));
                else if (auto v = node.value<double>())  // 単一の値なら灰色
                    m.*(d->c) = glm::vec3(static_cast<float>(*v));
                break;
            case ParamType::Bool:
                if (auto v = node.value<bool>()) m.*(d->b) = *v;
                break;
        }
    }
    if (const toml::table* k = t["krbr"].as_table()) {
        if (auto v = (*k)["bump_pattern"].value<std::string>())
            m.bump_pattern = *v == "noise" ? BumpPattern::Noise : *v == "weave" ? BumpPattern::Weave : BumpPattern::None;
        if (auto v = (*k)["bump_strength"].value<double>()) m.bump_strength = static_cast<float>(*v);
        if (auto v = (*k)["bump_scale"].value<double>()) m.bump_scale = static_cast<float>(*v);
        if (auto v = (*k)["roughness_variation"].value<double>()) m.roughness_variation = static_cast<float>(*v);
        if (auto v = (*k)["base_color_from_model"].value<bool>()) m.base_color_from_model = *v;
        const toml::array* sb = (*k)["sellmeier_b"].as_array();
        const toml::array* sc = (*k)["sellmeier_c"].as_array();
        if (sb && sc && sb->size() == 3 && sc->size() == 3) {
            Sellmeier s;
            for (int i = 0; i < 3; ++i) {
                s.B[i] = static_cast<float>((*sb)[i].value_or(0.0));
                s.C[i] = static_cast<float>((*sc)[i].value_or(0.0));
            }
            m.sellmeier = s;
        }
    }
}

toml::table writeMaterial(const Material& m) {
    const Material def;
    toml::table t;
    for (const auto& p : openPbrParams()) {
        switch (p.type) {
            case ParamType::Float:
                if (m.*(p.f) != def.*(p.f)) t.insert(p.key, double(m.*(p.f)));
                break;
            case ParamType::Color:
                if (m.*(p.c) != def.*(p.c)) {
                    if (p.c == &Material::base_color && m.base_color_from_model) break;
                    t.insert(p.key, colorArray(m.*(p.c)));
                }
                break;
            case ParamType::Bool:
                if (m.*(p.b) != def.*(p.b)) t.insert(p.key, m.*(p.b));
                break;
        }
    }
    toml::table k;
    if (m.base_color_from_model) k.insert("base_color_from_model", true);
    if (m.bump_pattern != BumpPattern::None) {
        k.insert("bump_pattern", bumpName(m.bump_pattern));
        k.insert("bump_strength", double(m.bump_strength));
        k.insert("bump_scale", double(m.bump_scale));
    }
    if (m.roughness_variation != 0.0f) k.insert("roughness_variation", double(m.roughness_variation));
    if (m.sellmeier) {
        k.insert("sellmeier_b", toml::array{double(m.sellmeier->B[0]), double(m.sellmeier->B[1]), double(m.sellmeier->B[2])});
        k.insert("sellmeier_c", toml::array{double(m.sellmeier->C[0]), double(m.sellmeier->C[1]), double(m.sellmeier->C[2])});
    }
    if (!k.empty()) t.insert("krbr", k);
    return t;
}

}  // namespace krbr::scene
