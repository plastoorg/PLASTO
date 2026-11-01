//! Иконка exe и манифест (requireAdministrator: WinDivert грузит драйвер в ядро),
//! версия встроенного Telegram-прокси и встроенная сборка zapret.
fn main() {
    println!("cargo:rerun-if-changed=assets/plasto.rc");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/tray.ico");
    // Версия встроенного Telegram-прокси — из Cargo.lock: с ней сравнивается
    // последний релиз tg-ws-proxy-rs.
    println!("cargo:rerun-if-changed=Cargo.lock");
    let lock = std::fs::read_to_string("Cargo.lock").unwrap_or_default();
    let version = lock
        .split("[[package]]")
        .find(|p| p.contains("name = \"tg-ws-proxy-rs\""))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")))
        .map(|v| v.trim_end_matches('"'))
        .unwrap_or("0.0.0");
    println!("cargo:rustc-env=TGWS_BUILTIN_VERSION={version}");
    bundle_zapret();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Только для бинаря: тестовый exe с requireAdministrator не запустился
        // бы из обычной консоли.
        embed_resource::compile_for("assets/plasto.rc", ["plasto"], embed_resource::NONE)
            .manifest_required()
            .expect("не удалось вшить ресурсы");
    }
}

/// Встроенная сборка zapret (`assets/zapret`, её кладёт ci/refresh-zapret.py):
/// в архив внутри exe, версия — из `assets/zapret.json`. Порядок файлов и
/// время в архиве постоянные: одна и та же сборка даёт тот же архив.
fn bundle_zapret() {
    use std::io::Write;
    println!("cargo:rerun-if-changed=assets/zapret");
    println!("cargo:rerun-if-changed=assets/zapret.json");
    let meta = std::fs::read_to_string("assets/zapret.json").expect("нет assets/zapret.json: запустите ci/refresh-zapret.py --apply");
    let tag = meta.split("\"tag\"").nth(1).and_then(|r| r.split('"').nth(1)).expect("в assets/zapret.json нет tag");
    println!("cargo:rustc-env=ZAPRET_BUNDLED_TAG={tag}");

    let root = std::path::Path::new("assets/zapret");
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("assets/zapret").flatten() {
            let path = entry.path();
            if path.is_dir() {
                println!("cargo:rerun-if-changed={}", path.display());
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("zapret.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&out).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9));
    for path in files {
        let name = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        zip.start_file(name, options).unwrap();
        zip.write_all(&std::fs::read(&path).unwrap()).unwrap();
    }
    zip.finish().unwrap();
}
