// В релизе — без консольного окна.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("PLASTO работает только в Windows: WinDivert — драйвер ядра Windows.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    use plasto::{api, host, platform, ui};
    use serde_json::Value;

    // После обновления: дождаться, пока прежняя версия выйдет.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pid) = args.iter().position(|a| a == "--after").and_then(|i| args.get(i + 1)).and_then(|p| p.parse().ok()) {
        platform::wait_exit(pid, std::time::Duration::from_secs(15));
    }
    // Вторая копия не запускается: она будит первую и выходит.
    if !platform::claim_single_instance() {
        return;
    }
    plasto::selfupdate::cleanup();
    let autostart = std::env::args().any(|a| a == "--autostart");
    let (host, events) = host::Host::new();
    let cfg = api::read_cfg(&host);
    let num = |k: &str| cfg.get(k).and_then(Value::as_f64).map(|v| v as f32);
    let opts = ui::shell::Options {
        size: [num("w").unwrap_or(ui::BASE_W).clamp(ui::BASE_W, ui::MAX_W), num("h").unwrap_or(ui::BASE_H).clamp(ui::BASE_H, ui::MAX_H)],
        pos: num("x").zip(num("y")).map(|(x, y)| [x, y]),
        on_top: cfg.get("alwaysOnTop") == Some(&Value::Bool(true)),
        autostart,
        after_update: args.iter().any(|a| a == "--after"),
        // Запуск из автозагрузки — сразу в трей, если не просили иначе.
        start_hidden: autostart && cfg.get("startMinimized") != Some(&Value::Bool(false)),
    };
    if let Err(e) = ui::shell::run(host, events, opts) {
        eprintln!("[plasto] окно не создано: {e}");
    }
    // Рабочие потоки (тест, загрузка) не держат процесс после закрытия окна.
    std::process::exit(0);
}
