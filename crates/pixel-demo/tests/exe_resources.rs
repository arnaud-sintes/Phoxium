//! The built pixel-demo.exe embeds assets/icon/pixel-demo.ico (every size) and its VERSIONINFO. Reads the
//! executable's PE resource directory; opens no window.

#[test]
fn executable_embeds_icon_and_version() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_pixel-demo")).unwrap();
    let ico = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icon/pixel-demo.ico")).unwrap();
    let sizes = appres::pe::check_executable(
        &exe,
        &ico,
        &[
            "pixel-demo.exe",
            appres::PRODUCT_NAME,
            appres::COPYRIGHT,
            env!("CARGO_PKG_VERSION"),
        ],
    )
    .unwrap();
    assert_eq!(sizes, [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]);
}
