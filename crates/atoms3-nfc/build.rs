use std::path::PathBuf;

fn main() {
    embuild::espidf::sysenv::output();
    emit_build_timestamp();
    check_nfc_shim_freshness();
}

fn emit_build_timestamp() {
    println!(
        "cargo:rustc-env=FIRMWARE_BUILD_TIME={}",
        chrono::Local::now().to_rfc3339()
    );
}

/// Guards against the ESP-IDF/CMake incremental build silently relinking a
/// stale compiled object for a component source file edited after the last
/// successful build. Observed in practice (2026-07-20, issue #96): a
/// `nfc_sample_test.cpp.obj` left over from an earlier session got reused
/// unchanged by `cargo build --release`, so the flashed firmware silently
/// lacked hours of newer diagnostic-logging edits.
fn check_nfc_shim_freshness() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.join("../..");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = <target_dir>/<triple>/<profile>/build/<pkg>-<hash>/out
    let Some(target_dir) = out_dir.ancestors().nth(5) else {
        return;
    };

    for name in ["nfc_shim.cpp", "nfc_sample_test.cpp"] {
        let src = workspace_root.join("components/nfc_shim").join(name);
        println!("cargo:rerun-if-changed={}", src.display());

        let Ok(src_mtime) = std::fs::metadata(&src).and_then(|m| m.modified()) else {
            continue;
        };

        let pattern = format!(
            "{}/**/esp-idf-sys-*/out/build/esp-idf/nfc_shim/CMakeFiles/__idf_nfc_shim.dir/{name}.obj",
            target_dir.display()
        );
        let Ok(paths) = glob::glob(&pattern) else {
            continue;
        };
        for obj in paths.flatten() {
            let Ok(obj_mtime) = std::fs::metadata(&obj).and_then(|m| m.modified()) else {
                continue;
            };
            if obj_mtime < src_mtime {
                panic!(
                    "stale ESP-IDF build cache: {} was compiled BEFORE the newer source {} \
                     was saved (CMake/ninja silently skipped recompiling it).\n  \
                     obj mtime:    {:?}\n  source mtime: {:?}\n\
                     Fix: delete the esp-idf-sys build dir for this profile under {} and rebuild.",
                    obj.display(),
                    src.display(),
                    obj_mtime,
                    src_mtime,
                    target_dir.display(),
                );
            }
        }
    }
}
