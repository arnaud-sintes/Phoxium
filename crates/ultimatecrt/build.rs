//! Compiles the GLSL shaders of the workspace `shaders/` folder to SPIR-V with glslc.

use std::{env, path::PathBuf, process::Command};

const SHADERS: [&str; 2] = ["fullscreen.vert", "passthrough.frag"];

fn glslc() -> PathBuf {
    if let Ok(sdk) = env::var("VULKAN_SDK") {
        let p = PathBuf::from(sdk).join("Bin").join("glslc.exe");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("glslc")
}

fn main() {
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("..")
        .join("..")
        .join("shaders");
    for name in SHADERS {
        let src = dir.join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        let dst = out.join(format!("{name}.spv"));
        let status = Command::new(glslc())
            .args(["-O", "--target-env=vulkan1.3", "-o"])
            .arg(&dst)
            .arg(&src)
            .status()
            .unwrap_or_else(|e| panic!("cannot run glslc ({e}); install the Vulkan SDK and set VULKAN_SDK"));
        assert!(status.success(), "glslc failed on {}", src.display());
    }
    generate_params(&out);
}

/// Generates `params.rs` and `params.glsl` in `OUT_DIR` from the parameter table `params/*.toml` (paramgen),
/// then compiles a shader that includes `params.glsl` with glslc. Test-only files (`params/_*.toml`) are
/// included in non-release builds only, which also set `cfg(ucrt_test_params)`.
fn generate_params(out: &std::path::Path) {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("..")
        .join("..")
        .join("params");
    println!("cargo:rerun-if-changed={}", dir.display());
    println!("cargo::rustc-check-cfg=cfg(ucrt_test_params)");
    let include_test = env::var("PROFILE").map_or(true, |p| p != "release");
    if include_test {
        println!("cargo:rustc-cfg=ucrt_test_params");
    }
    let table = match paramgen::load_dir(&dir, include_test) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: parameter table: {e}");
            std::process::exit(1);
        }
    };
    std::fs::write(out.join("params.rs"), paramgen::rust_source(&table)).unwrap();
    std::fs::write(out.join("params.glsl"), paramgen::glsl_source(&table)).unwrap();
    let check = out.join("params_check.comp");
    std::fs::write(
        &check,
        "#version 450\n#extension GL_GOOGLE_include_directive : require\n#include \"params.glsl\"\n\
         layout(local_size_x = 1) in;\nvoid main() {}\n",
    )
    .unwrap();
    let status = Command::new(glslc())
        .args(["--target-env=vulkan1.3", "-o"])
        .arg(out.join("params_check.spv"))
        .arg(&check)
        .status()
        .unwrap_or_else(|e| panic!("cannot run glslc ({e}); install the Vulkan SDK and set VULKAN_SDK"));
    assert!(status.success(), "glslc failed on the generated params.glsl");
}
