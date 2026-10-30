//! シェーダ（<repo>/shaders の GLSL）を glslc で SPIR-V にして OUT_DIR/shaders に置き、それをライブラリに埋め込む表
//! （OUT_DIR/embedded_shaders.rs）を作る。埋め込むので、exe の横にシェーダのフォルダが無くても（DLL として読み込まれても）動く。
//! 積分器（レイを飛ばすもの）は、ray query 版（.comp.spv）に加えて ray tracing pipeline 版（.rgen.spv）も作る。
//! ray_query を持たない GPU（GTX 10 系など）では後者と rt_*.spv を使う。
//! glslc は 環境変数 GLSLC、%VULKAN_SDK%\Bin、PATH の順に探す（Vulkan SDK に付属。配布の zip を使うだけなら不要）
//! Rust と共有する定数（バインディング番号・格子の大きさなど）は src/shader_consts.rs が正本で、-D で渡す

use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
mod shader_consts {
    include!("src/shader_consts.rs");
}

/// そのまま SPIR-V にするもの（ファイル名の拡張子でステージが決まる）
const SOURCES: &[&str] = &["pathtrace.comp", "tonemap.comp", "glow.comp", "denoise_prep.comp", "denoise_blend.comp", "vcm_light.comp", "vcm_camera.comp", "rt_hit.rchit", "rt_miss.rmiss", "rt_shadow.rmiss", "solder_blur.comp", "solder_util.comp", "solder_phi.comp", "solder_apply.comp"];
/// ray tracing pipeline 版（.rgen.spv）も作るもの
const RAY_SOURCES: &[&str] = &["pathtrace.comp", "vcm_light.comp", "vcm_camera.comp"];

fn find_glslc() -> Result<PathBuf, String> {
    let exe = if cfg!(windows) { "glslc.exe" } else { "glslc" };
    let mut tried = Vec::new();
    if let Some(p) = std::env::var_os("GLSLC") {
        return Ok(PathBuf::from(p));
    }
    tried.push("GLSLC (not set)".to_string());
    if let Some(sdk) = std::env::var_os("VULKAN_SDK") {
        for bin in ["Bin", "bin"] {
            let p = Path::new(&sdk).join(bin).join(exe);
            if p.exists() {
                return Ok(p);
            }
            tried.push(p.display().to_string());
        }
    } else {
        tried.push("VULKAN_SDK (not set)".to_string());
    }
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(p) = std::env::split_paths(&path).map(|d| d.join(exe)).find(|p| p.exists()) {
            return Ok(p);
        }
    }
    tried.push("PATH".to_string());
    Err(format!("glslc not found (tried: {}). Install the Vulkan SDK, or set GLSLC to the path of glslc", tried.join(", ")))
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // canonicalize は Windows で \\?\ 付きのパスになり、glslc が #include を解決できないので使わない
    let src_dir = manifest.ancestors().nth(3).expect("repository root").join("shaders");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dst = out_dir.join("shaders");
    std::fs::create_dir_all(&dst).expect("create shader output directory");

    println!("cargo:rerun-if-env-changed=GLSLC");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    println!("cargo:rerun-if-changed=src/shader_consts.rs");
    for e in std::fs::read_dir(&src_dir).expect("read shaders directory").flatten() {
        println!("cargo:rerun-if-changed={}", e.path().display());
    }

    let glslc = find_glslc().unwrap_or_else(|e| panic!("{e}"));
    let defines: Vec<String> = shader_consts::glsl_defines().into_iter().map(|(k, v)| format!("-D{k}={v}")).collect();
    let mut jobs: Vec<(String, Vec<String>)> = Vec::new();
    for s in SOURCES {
        jobs.push((format!("{s}.spv"), vec![s.to_string()]));
    }
    for s in RAY_SOURCES {
        let base = s.trim_end_matches(".comp");
        jobs.push((format!("{base}.rgen.spv"), vec!["-fshader-stage=rgen".into(), "-DKRBR_RT_PIPELINE=1".into(), s.to_string()]));
    }
    // 大きなシェーダ（VCM）は 1 つに数十秒かかるので並列に
    let errors: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|(out, args)| {
                let (glslc, src_dir, out_path, defines) = (&glslc, &src_dir, dst.join(out), &defines);
                scope.spawn(move || {
                    let r = Command::new(glslc)
                        .args(["--target-env=vulkan1.3", "-O", "-I"])
                        .arg(src_dir)
                        .args(defines)
                        .arg("-o")
                        .arg(&out_path)
                        .args(args.iter().map(|a| if a.starts_with('-') { a.into() } else { src_dir.join(a).into_os_string() }))
                        .output();
                    match r {
                        Ok(o) if o.status.success() => None,
                        Ok(o) => Some(format!("glslc {out}:\n{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
                        Err(e) => Some(format!("cannot run {}: {e}", glslc.display())),
                    }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
    });
    if !errors.is_empty() {
        panic!("shader compilation failed:\n{}", errors.join("\n"));
    }

    // 埋め込みの表：(ファイル名, SPIR-V)
    let mut table = String::from("pub static EMBEDDED_SHADERS: &[(&str, &[u8])] = &[\n");
    for (out, _) in &jobs {
        table += &format!("    ({out:?}, include_bytes!({:?})),\n", dst.join(out).display().to_string());
    }
    table += "];\n";
    std::fs::write(out_dir.join("embedded_shaders.rs"), table).expect("write embedded_shaders.rs");
}
