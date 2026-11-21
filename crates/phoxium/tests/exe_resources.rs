//! The built phoxium.exe embeds assets/icon/icon.ico (every size, PNG frames from 64 px) and its
//! VERSIONINFO (product, description, version, copyright). Reads the executable's PE resource directory; opens
//! no window and needs no GPU.

#[test]
fn executable_embeds_icon_and_version() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_phoxium")).unwrap();
    let ico = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icon/icon.ico")).unwrap();
    let sizes = appres::pe::check_executable(
        &exe,
        &ico,
        &[
            "phoxium.exe",
            "Phoxium, physically based CRT shader",
            appres::PRODUCT_NAME,
            appres::COPYRIGHT,
            env!("CARGO_PKG_VERSION"),
        ],
    )
    .unwrap();
    assert_eq!(sizes, [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]);
    for f in appres::ico::parse(&ico).unwrap() {
        assert_eq!(f.is_png(), f.size >= 64, "{} px frame format", f.size);
    }
}
