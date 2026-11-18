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
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("..").join("..").join("shaders");
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
}
