//! Окно PLASTO на egui.
//!
//! Повторяет прежний интерфейс: карточка-кнопка с фотографией состояния и
//! панель настроек поверх неё. Вызовы бэкенда идут через `api::call` на
//! рабочих потоках, ответы и события возвращаются по каналам — окно не
//! подвисает на проверках и загрузках, а без событий не перерисовывается.

mod card;
mod conflict;
mod diag;
mod first;
mod glass;
mod health;
mod icons;
mod kit;
mod pages;
mod prefs;
mod gpu;
mod home;
mod scene;
mod services;
pub mod shell;
mod stab;
mod sysload;
mod theme;
mod updates;
mod video;

use std::{
    collections::{BTreeSet, VecDeque},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{json, Map, Value};

use crate::{
    api,
    host::{Event, Host},
    platform::{self, Sound, TrayCmd},
};
use stab::Stab;
use theme::{Status, Tone};

/* ---------- состояния карточки ----------------------------------------- */

pub struct StateDef {
    pub title: &'static str,
    pub sub: &'static str,
    pub sound: Option<Sound>,
}

/// Строка о том, что открывается без обхода.
fn baseline_status(score: u64) -> Status {
    if score >= 90 {
        Status::new(Tone::Ok, format!("Без обхода уже доступно **{score}%** — возможно, блокировок нет"))
    } else {
        Status::new(Tone::Plain, format!("Без обхода доступно **{score}%** целей"))
    }
}

pub fn state_def(state: &str) -> StateDef {
    match state {
        // Процесс запущен ещё не значит «работает»: пока пинг не прошёл,
        // обещать больше, чем знаем, не будем. После пинга — см. home_ui.
        "on" => StateDef { title: "Процесс запущен", sub: "Проверяю, открываются ли сайты", sound: Some(Sound::Start) },
        "timeout" => StateDef { title: "Не всё открывается", sub: "Процесс жив, но часть сайтов не отвечает — смените стратегию", sound: None },
        // Не состояние движка: подбор проверяет стратегии рядом с обходом (а
        // если не выйдет — выключает его до конца), карточка показывает подбор.
        "testing" => StateDef { title: "Подбор стратегии", sub: "Проверяю стратегии в стороне, обход работает как работал", sound: None },
        "error" => StateDef { title: "Ошибка", sub: "Стратегия не запустилась. Подробности в логах", sound: Some(Sound::Error) },
        "critical" => StateDef { title: "Критично", sub: "Нет прав администратора или компоненты не установлены", sound: Some(Sound::Error) },
        _ => StateDef { title: "Выключено", sub: "Нажмите, чтобы включить обход", sound: Some(Sound::Stop) },
    }
}

/* ---------- ответы рабочих потоков ------------------------------------- */

/// Что делать с ответом бэкенда.
#[derive(Clone, Debug)]
enum Op {
    Ignore,
    Bootstrap,
    Toggle,
    /// Главная кнопка: zapret и hosts ИИ-сервисов.
    Power,
    Ping,
    /// Остановка чужого zapret.
    ConflictWinws,
    /// Telegram-прокси после остановки помехи или смены порта.
    TgwsResolve,
    Preview,
    ApplyStrategy,
    ZapretCheck,
    AppCheck,
    AppDownload,
    TgwsCheck,
    TgwsUpdate,
    TgwsStatus,
    EngineRestart,
    ZapretInstall { auto: bool },
    PickDir,
    /// Смена папки компонентов: уже скачанное переезжает туда.
    Components,
    Service,
    GameFilter,
    Check,
    EngineMode(String),
    TestRun,
    /// Сохранённый отчёт прошлого подбора.
    TestLast,
    Speed,
    StabQuery,
    StabLive,
    StabStatus,
    StabExport,
    StabClear,
    Diag,
    TgwsToggle,
    TgwsLink,
    AiList,
    AiApply,
    AiSource,
    AiDropStale,
    AiRestore,
    AiProbe { via_proxy: bool },
    DnsList,
    DnsApply { auto: bool },
    DnsRestore,
    DnsCheck,
    DnsWatch,
    DnsRotate,
    DnsAdd,
    DnsRemove,
    Autostart,
}

/// Разделы нового окна.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Home,
    Services,
    Diag,
    Settings,
}

/// Механизм обхода на странице «Сервисы».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mech {
    Zapret,
    Telegram,
    Ai,
    Dns,
}

/// Вкладки «Диагностики».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiagTab {
    Speed,
    Stability,
    System,
    Tests,
    Log,
}

/// Что окно должно сделать с собой. Очередью, а не командами egui: показать
/// окно надо уметь и из трея, когда кадры не рисуются.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinCmd {
    Show,
    Hide,
    /// Свернуть на панель задач, как обычную программу; трей — только «закрыть».
    Minimize,
    Quit,
}

pub struct Preset {
    pub file: String,
    pub label: String,
    pub group: String,
}

pub struct Boot {
    pub title: String,
    pub sub: String,
    pub frac: f32,
}

#[derive(Default)]
pub struct Tests {
    pub running: bool,
    pub stage: String,
    pub frac: f32,
    pub baseline: Option<Status>,
    pub rows: Vec<Value>,
    pub note: Option<(String, bool)>,
    pub stop_perfect: bool,
    /// Состояние движка, которое вернуть карточке после подбора: события
    /// во время подбора копятся здесь, а не мелькают на главной.
    pub resume: Option<(String, Option<String>)>,
    pub live: Live,
}

/// Что подбор проверяет прямо сейчас: стратегию (или замер без обхода),
/// её номер в очереди и ход по сайтам.
#[derive(Default)]
pub struct Live {
    /// Пусто — замер без обхода.
    pub label: String,
    pub index: usize,
    pub count: usize,
    pub targets: usize,
    /// По сайтам, в порядке тестера: имя, номер текущей попытки (0 — ещё
    /// не начат) и итог. Сайты проверяются разом и отвечают вразнобой.
    pub hosts: Vec<String>,
    pub tries: Vec<u64>,
    pub marks: Vec<Option<bool>>,
}

impl Live {
    /// Доля всего подбора: пройденные стратегии плюс доля сайтов текущей.
    fn frac(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        let inner = if self.targets > 0 { self.done() as f32 / self.targets as f32 } else { 0.0 };
        ((self.index as f32 + inner) / self.count as f32).min(1.0)
    }

    pub fn done(&self) -> usize {
        self.marks.iter().flatten().count()
    }
}

#[derive(Default)]
pub struct Speed {
    pub running: bool,
    pub frac: f32,
    pub stage: String,
    /// Строки по серверам: приходят по ходу замера, в конце — из итога.
    pub rows: Vec<Value>,
    /// Живая скорость, этап («reach», «down», «up») и сервер — для шкалы.
    pub live: f64,
    pub phase: String,
    pub server: String,
    /// Скорость текущего сервера по ходу замера — для живого графика.
    pub live_series: Vec<f64>,
    pub result: Option<Value>,
    pub note: Option<(String, bool)>,
}

#[derive(Default)]
pub struct Diag {
    pub busy: Option<String>,
    pub verdict: Option<Status>,
    pub rows: Vec<Value>,
    pub failed: bool,
    /// Первая проверка уже запускалась сама, при открытии вкладки.
    pub auto: bool,
}

#[derive(Default)]
pub struct Ai {
    pub services: Vec<Value>,
    pub bypass: BTreeSet<String>,
    pub proxy: BTreeSet<String>,
    /// В hosts стоит блок PLASTO с прокси или блоклистом.
    pub hosts_live: bool,
    pub blocklist: bool,
    pub extra: String,
    pub probe_proxy: String,
    /// Свой SNI-прокси вместо адресов каталога; пусто — каталог.
    pub entry: String,
    pub loaded: bool,
    pub apply_state: Option<Status>,
    pub hosts_state: Option<Status>,
    pub stale: u64,
    pub stale_state: Option<Status>,
    pub busy_apply: bool,
    pub busy_source: bool,
    pub busy_stale: bool,
    pub all_on: bool,
    pub probe_busy: bool,
    pub probe_frac: f32,
    pub verdict: Option<Status>,
    pub rows: Vec<Value>,
    pub rows_note: Option<String>,
}

#[derive(Default)]
pub struct Dns {
    pub presets: Vec<Value>,
    pub selected: String,
    pub state: Option<Status>,
    pub has_backup: bool,
    pub custom_name: String,
    pub custom_text: String,
    pub custom_msg: Option<String>,
    pub busy_apply: bool,
    /// Что делается, пока `busy_apply`: состояние и подпись для строки механизма.
    pub doing: Option<(&'static str, &'static str)>,
    pub busy_add: bool,
    /// Выставленный сейчас пресет (а `selected` — тот, что выбран в списке).
    pub active: Option<String>,
    /// Итог проверки по id пресета: `{"state", "ms", "plain", "doh", "spoofs"}`.
    pub health: Map<String, Value>,
    pub checking: bool,
    pub checked_at: Option<Instant>,
    /// Когда проверить выставленный пресет; None — присмотр выключен.
    pub due: Option<Instant>,
    /// Неудач подряд у выставленного пресета.
    pub fails: u32,
    /// Когда автопереключение сработало в последний раз.
    pub switched_at: Option<Instant>,
}

pub struct App {
    host: Host,
    events: mpsc::Receiver<Event>,
    replies_tx: mpsc::Sender<(Op, Value)>,
    replies: mpsc::Receiver<(Op, Value)>,
    tray: Arc<Mutex<Vec<TrayCmd>>>,
    hidden: bool,
    /// Окно свёрнуто на панель задач (не в трей): о нём сообщает оболочка.
    minimized: bool,
    window_cmds: Vec<WinCmd>,
    quitting: bool,
    shut_down: bool,
    pending_connect: Option<Instant>,

    /* карточка */
    cfg: Map<String, Value>,
    strategies: Vec<Preset>,
    state: String,
    reason: Option<String>,
    shown_state: Option<String>,
    user_action_until: Option<Instant>,
    toggle_busy: Option<Instant>,
    toggle_waiting: bool,
    flash: Option<(String, Instant)>,
    svc: Option<Value>,
    boot: Option<Boot>,
    close_dialog: Option<bool>,
    check_text: String,
    scene: scene::Scene,
    /// Когда обход включился: для «времени работы» на главной.
    on_since: Option<Instant>,
    page: Page,
    /// Первый запуск: компонентов нет, справа — шаги установки.
    first: Option<first::First>,
    mech: Mech,
    diag_tab: DiagTab,
    /// Журнал: только предупреждения и ошибки.
    log_errors: bool,
    /// Кадр рисует видеокарта: сцену собирает шейдер, иначе — CPU.
    gpu: bool,

    /* настройки */
    sel_strategy: String,
    preview: Option<(String, bool)>,
    game_filter: String,
    engine_mode: String,
    close_action: String,
    autostart: bool,
    auto_connect: bool,
    on_top: bool,
    sound: bool,
    auto_download: bool,
    behavior_note: Option<String>,
    ver_badge: (String, bool),
    upd: updates::Updates,
    /// Запуск сразу после самообновления: вернуть включённое в любом случае.
    after_update: bool,
    dir: String,
    dir_bad: bool,
    dir_hint: (String, bool),
    /// Куда скачиваются компоненты; по умолчанию папка профиля в %AppData%.
    components_dir: String,
    /// Почему не удалось сменить папку компонентов.
    components_note: Option<String>,
    engine_hint: (String, bool),
    check_busy: bool,
    probes: Vec<(String, bool, String)>,
    probes_note: Option<String>,

    /// Идёт удаление старой службы или драйвера WinDivert.
    svc_busy: bool,

    logs: VecDeque<Value>,
    copied: Option<(Instant, bool)>,

    tests: Tests,
    ping: health::Ping,
    /// Окно «уже работает другой zapret» или «порт занят».
    conflict: Option<conflict::Conflict>,
    speed: Speed,
    stab: Stab,
    diag: Diag,
    tgws: Value,
    tgws_status: Status,
    tgws_busy: bool,
    ai: Ai,
    dns: Dns,

    elevated: bool,
    version: String,
}

const DIR_HINT: &str = "Внутри должны быть `bin/winws.exe` и файлы `general*.bat`.";
const ENGINE_HINT: &str = "zapret2 — отдельный продукт, а не обновление: другой бинарь и другой формат стратегий. Сборка Flowseal пока на zapret1.";

impl App {
    pub fn new(
        ctx: &egui::Context,
        host: Host,
        events: mpsc::Receiver<Event>,
        start_hidden: bool,
        after_update: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        theme::install_fonts(ctx);
        kit::install_style(ctx);

        let waker = Arc::clone(&wake);
        host.set_waker(move || waker());

        let tray = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::clone(&tray);
        platform::spawn_tray(move |cmd| {
            if let Ok(mut q) = queue.lock() {
                q.push(cmd);
            }
            wake();
        });

        let (replies_tx, replies) = mpsc::channel();
        let app = App {
            host,
            events,
            replies_tx,
            replies,
            tray,
            hidden: start_hidden,
            minimized: false,
            window_cmds: Vec::new(),
            quitting: false,
            shut_down: false,
            pending_connect: None,
            cfg: Map::new(),
            strategies: Vec::new(),
            state: "off".into(),
            reason: None,
            shown_state: None,
            user_action_until: None,
            toggle_busy: None,
            toggle_waiting: false,
            flash: None,
            svc: None,
            boot: None,
            close_dialog: None,
            check_text: "—".into(),
            scene: scene::Scene::default(),
            on_since: None,
            page: Page::Home,
            first: None,
            mech: Mech::Zapret,
            diag_tab: DiagTab::Speed,
            log_errors: false,
            gpu: false,
            sel_strategy: String::new(),
            preview: None,
            game_filter: "off".into(),
            engine_mode: String::new(),
            close_action: "ask".into(),
            autostart: false,
            auto_connect: false,
            on_top: false,
            sound: true,
            auto_download: true,
            behavior_note: None,
            ver_badge: ("не установлено".into(), false),
            upd: updates::Updates::default(),
            after_update,
            dir: String::new(),
            dir_bad: false,
            dir_hint: (DIR_HINT.into(), false),
            components_dir: String::new(),
            components_note: None,
            engine_hint: (ENGINE_HINT.into(), false),
            check_busy: false,
            probes: Vec::new(),
            probes_note: None,
            svc_busy: false,
            logs: VecDeque::new(),
            copied: None,
            tests: Tests { note: Some(("Тест ещё не запускался.".into(), false)), stop_perfect: true, ..Default::default() },
            ping: health::Ping::default(),
            conflict: None,
            speed: Speed::default(),
            stab: Stab::new(),
            diag: Diag::default(),
            tgws: Value::Null,
            tgws_status: Status::new(Tone::Plain, "выключен"),
            tgws_busy: false,
            ai: Ai { rows_note: Some("Проверка ещё не запускалась.".into()), ..Default::default() },
            dns: Dns::default(),
            elevated: false,
            version: String::new(),
        };
        app.call(Op::Bootstrap, "app:bootstrap", Value::Null);
        app.call(Op::TestLast, "test:last", Value::Null);
        app
    }

    /* ---------- связь с бэкендом ---------------------------------------- */

    fn call(&self, op: Op, name: &'static str, payload: Value) {
        let host = self.host.clone();
        let tx = self.replies_tx.clone();
        std::thread::spawn(move || {
            let value = api::call(&host, name, payload);
            let _ = tx.send((op, value));
            host.wake();
        });
    }

    fn set_config(&mut self, patch: Value) {
        if let Some(p) = patch.as_object() {
            for (k, v) in p {
                self.cfg.insert(k.clone(), v.clone());
            }
        }
        self.call(Op::Ignore, "config:set", patch);
    }

    fn cfg_str(&self, key: &str) -> Option<&str> {
        self.cfg.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
    }

    /// Какой tg-ws-proxy выбран в настройках — для подписей.
    fn tgws_name(&self) -> &'static str {
        match crate::tgws::Flavor::parse(self.cfg_str("tgwsFlavor")) {
            crate::tgws::Flavor::Builtin => "tg-ws-proxy-rs (valnesfjord), встроенный",
            crate::tgws::Flavor::Original => "Flowseal tg-ws-proxy",
            crate::tgws::Flavor::Rust => "tg-ws-proxy-rs (valnesfjord), скачанный",
        }
    }

    /* ---------- карточка ------------------------------------------------ */

    fn mark_user_action(&mut self) {
        // Звук — только на то, что человек сделал сам; автоматические
        // переходы в фоне звенеть не должны.
        self.user_action_until = Some(Instant::now() + Duration::from_secs(8));
    }

    fn render_state(&mut self, state: &str, reason: Option<String>) {
        let state = if ["on", "off", "timeout", "error", "critical", "testing"].contains(&state) { state } else { "off" };
        // «Нет ответа» — тот же запущенный процесс: время работы не сбрасываем.
        let running = |s: &str| s == "on" || s == "timeout";
        if !running(state) {
            self.on_since = None;
            self.ping.mechs.remove("zapret");
            self.ping.fails = 0;
        } else if !running(&self.state) || self.on_since.is_none() {
            self.on_since = Some(Instant::now());
            // Дать winws пару секунд и проверить, открываются ли сайты.
            self.ping_soon(Duration::from_secs(4));
        }
        self.state = state.to_owned();
        self.reason = reason.filter(|r| !r.is_empty());
        if self.shown_state.as_deref() != Some(state) {
            self.shown_state = Some(state.to_owned());
            let by_user = self.user_action_until.is_some_and(|t| Instant::now() < t);
            if let (Some(sound), true, true) = (state_def(state).sound, by_user, self.sound) {
                platform::play(sound);
            }
            // Упал сам (перезапуск не помог), а не по кнопке и не в тесте стратегий.
            if !by_user && !self.tests.running && matches!(state, "error" | "critical") {
                let text = self.reason.clone().unwrap_or_else(|| state_def(state).sub.to_owned());
                self.tray_alert(&format!("Обход остановился: {text}"));
            }
            self.user_action_until = None;
        }
        self.flash = None;
        platform::set_tray_tip(&format!("PLASTO — {}", state_def(state).title));
    }

    fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now() + Duration::from_millis(3200)));
    }

    /// Главная кнопка: выключает zapret и прокси ИИ-сервисов, Telegram и
    /// DNS оставляет; включает то, что настроено.
    fn power(&mut self) {
        // Во время подбора главная кнопка — «Остановить подбор».
        if self.tests.running {
            self.call(Op::Ignore, "test:cancel", Value::Null);
            return;
        }
        if !self.can_toggle() {
            return;
        }
        self.mark_user_action();
        self.toggle_waiting = true;
        let running = self.state == "on" || self.state == "timeout";
        self.call(Op::Power, if running { "power:off" } else { "power:on" }, Value::Null);
    }

    /// Только zapret: тумблер в «Сервисах».
    fn toggle(&mut self) {
        if !self.can_toggle() {
            return;
        }
        self.mark_user_action();
        self.toggle_waiting = true;
        self.call(Op::Toggle, "engine:toggle", Value::Null);
    }

    /// Подбор закончился: карточке — то состояние, в котором он оставил движок.
    fn tests_over(&mut self) {
        if let Some((state, reason)) = self.tests.resume.take() {
            self.render_state(&state, reason);
        }
    }

    /// Можно ли сейчас щёлкать обходом; если нет компонентов — первый запуск.
    fn can_toggle(&mut self) -> bool {
        if self.tests.running {
            self.flash("Идёт подбор стратегии — остановите его, чтобы управлять обходом");
            return false;
        }
        if self.toggle_busy.is_some_and(|t| Instant::now() < t) || self.toggle_waiting {
            return false;
        }
        if self.cfg_str("zapretDir").is_none() {
            // Компонентов нет: показываем первый запуск, он предложит скачать.
            if self.first.is_none() {
                self.begin_first(first::First { waiting: true, ..Default::default() });
            }
            self.go(Page::Home);
            return false;
        }
        true
    }

    /* ---------- стратегии ----------------------------------------------- */

    fn paint_strategies(&mut self, list: &Value, selected: Option<String>) {
        self.strategies = list
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| Preset { file: s(&p["file"]).into(), label: s(&p["label"]).into(), group: s(&p["group"]).into() })
            .collect();
        if self.strategies.is_empty() {
            self.sel_strategy.clear();
            self.preview = None;
            return;
        }
        let valid = selected.as_ref().is_some_and(|f| self.strategies.iter().any(|p| &p.file == f));
        self.sel_strategy = if valid { selected.unwrap_or_default() } else { self.strategies[0].file.clone() };
        if !valid {
            let file = self.sel_strategy.clone();
            self.set_config(json!({ "strategy": file }));
        }
        self.request_preview();
    }

    fn label_of(&self, file: &str) -> String {
        self.strategies
            .iter()
            .find(|p| p.file == file)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| file.trim_end_matches(".bat").trim_end_matches(".BAT").to_owned())
    }

    fn save_strategy(&mut self, file: String, restart: bool) {
        if file.is_empty() {
            return;
        }
        self.sel_strategy = file.clone();
        self.cfg.insert("strategy".into(), json!(file));
        if restart {
            self.mark_user_action();
            self.call(Op::ApplyStrategy, "engine:applyStrategy", json!(file));
        } else {
            self.call(Op::Ignore, "config:set", json!({ "strategy": file }));
        }
        self.request_preview();
    }

    fn request_preview(&self) {
        if !self.sel_strategy.is_empty() {
            self.call(Op::Preview, "engine:preview", json!(self.sel_strategy));
        }
    }

    /* ---------- компоненты --------------------------------------------- */

    fn paint_dir(&mut self, dir: Option<&str>, status: &Value) {
        let dir = dir.unwrap_or("");
        self.dir = dir.to_owned();
        let bad = !dir.is_empty() && status["ok"] == false;
        self.dir_bad = bad;
        self.dir_hint = if bad { (format!("Не подходит: {}", s(&status["reason"])), true) } else { (DIR_HINT.into(), false) };
    }

    fn paint_version(&mut self, tag: Option<&str>, ok: bool) {
        self.ver_badge = match tag.filter(|t| !t.is_empty()) {
            Some(t) => (format!("zapret {t}"), true),
            None if ok => ("своя папка".into(), true),
            None => ("не установлено".into(), false),
        };
    }

    fn installed(&mut self, r: &Value) {
        self.boot = None;
        if let Some(dir) = r["dir"].as_str() {
            self.cfg.insert("zapretDir".into(), json!(dir));
        }
        self.cfg.insert("zapretTag".into(), r["tag"].clone());
        self.paint_version(r["tag"].as_str(), true);
        let dir = r["dir"].as_str().map(str::to_owned);
        self.paint_dir(dir.as_deref(), &r["dirStatus"]);
        let selected = self.cfg_str("strategy").map(str::to_owned);
        self.paint_strategies(&r["strategies"], selected);
    }

    /* ---------- окно ---------------------------------------------------- */

    pub fn take_window_cmds(&mut self) -> Vec<WinCmd> {
        std::mem::take(&mut self.window_cmds)
    }

    /// Оболочка сообщает, кто рисует кадр: от этого зависит, как собирается сцена.
    pub fn set_gpu(&mut self, on: bool) {
        self.gpu = on;
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    fn hide(&mut self) {
        if self.hidden {
            return;
        }
        self.hidden = true;
        self.close_dialog = None;
        // Пока окно в трее, картинки в памяти не держим.
        self.scene.release();
        self.window_cmds.push(WinCmd::Hide);
    }

    pub(super) fn minimize(&mut self) {
        self.window_cmds.push(WinCmd::Minimize);
    }

    pub fn set_minimized(&mut self, minimized: bool) {
        self.minimized = minimized;
    }

    fn show(&mut self) {
        // Пинг на главной не должен быть старым: давно не меряли — перемерить.
        if self.ping.at.is_none_or(|at| at.elapsed() > Duration::from_secs(120)) {
            self.ping_soon(Duration::ZERO);
        }
        self.hidden = false;
        self.window_cmds.push(WinCmd::Show);
    }

    /// Кнопка «закрыть», Alt+F4 и закрытие из панели задач.
    pub fn close_pressed(&mut self) {
        match self.close_action.as_str() {
            "tray" => self.hide(),
            "quit" => self.quit(),
            _ => {
                if self.hidden {
                    self.show();
                }
                self.close_dialog = Some(false);
            }
        }
    }

    fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.shutdown();
        self.window_cmds.push(WinCmd::Quit);
    }

    /// Процессы winws и tg-ws не переживают приложение.
    pub fn shutdown(&mut self) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        crate::tester::cancel();
        crate::stability::stop();
        crate::runtime::shutdown(&self.host);
        crate::tgws::shutdown(&self.host);
        platform::remove_tray();
    }

    /* ---------- события бэкенда ----------------------------------------- */

    fn on_event(&mut self, name: &str, p: Value) {
        match name {
            "plasto:engine:state" => {
                let reason = p["reason"].as_str().map(str::to_owned);
                if self.tests.running {
                    self.tests.resume = Some((s(&p["state"]).to_owned(), reason));
                } else {
                    self.render_state(s(&p["state"]), reason);
                }
            }
            "plasto:engine:log" => self.push_log(p),
            "plasto:engine:health" => self.paint_health(&p),
            "plasto:zapret:progress" => {
                let boot = self.boot.get_or_insert(Boot { title: "Загружаю компоненты".into(), sub: String::new(), frac: 0.0 });
                match s(&p["stage"]) {
                    "download" if p["detail"].is_object() => {
                        let d = &p["detail"];
                        let (received, size) = (u(&d["received"]), u(&d["size"]));
                        boot.title = "Загружаю компоненты".into();
                        boot.sub = format!("{} · {} из {}", s(&d["name"]), fmt_bytes(received), fmt_bytes(size));
                        if size > 0 {
                            boot.frac = (received as f32 / size as f32).min(1.0);
                        }
                    }
                    "unblock" => boot.sub = "снимаю блокировку файлов".into(),
                    "extract" => {
                        boot.title = "Распаковываю".into();
                        boot.sub = "ещё немного".into();
                        boot.frac = 1.0;
                    }
                    "done" => self.boot = None,
                    _ => {}
                }
            }
            "plasto:zapret:installed" => self.installed(&p),
            "plasto:app:progress" => {
                let size = u(&p["size"]).max(1);
                self.upd.app_frac = (u(&p["received"]) as f32 / size as f32).min(1.0);
            }
            "plasto:test:stage" => {
                let live = &mut self.tests.live;
                if matches!(s(&p["stage"]), "baseline" | "preset") {
                    live.label = s(&p["preset"]["label"]).to_owned();
                    live.targets = 0;
                    live.hosts.clear();
                    live.tries.clear();
                    live.marks.clear();
                }
                self.tests.stage = match s(&p["stage"]) {
                    "baseline" => "Проверяю, что доступно без обхода".into(),
                    "preset" if p["preset"].is_object() => format!("{} · {} из {}", s(&p["preset"]["label"]), live.index + 1, live.count.max(1)),
                    "preset" => "Проверяю пресет".into(),
                    "early-exit" => "Найден пресет со 100% — останавливаюсь".into(),
                    other => other.into(),
                };
            }
            "plasto:test:progress" => {
                let live = &mut self.tests.live;
                if p["phase"] == "queue" {
                    live.index = u(&p["index"]) as usize;
                    live.count = u(&p["total"]).max(1) as usize;
                } else {
                    live.targets = u(&p["total"]) as usize;
                    let n = live.targets;
                    live.hosts.resize(n, String::new());
                    live.tries.resize(n, 0);
                    live.marks.resize(n, None);
                    if let Some(i) = p["index"].as_u64().map(|i| i as usize).filter(|&i| i < n) {
                        live.hosts[i] = s(&p["host"]).to_owned();
                        if let Some(attempt) = p["try"].as_u64() {
                            live.tries[i] = attempt;
                        }
                        if let Some(ok) = p["ok"].as_bool() {
                            live.marks[i] = Some(ok);
                        }
                    }
                }
                self.tests.frac = live.frac();
            }
            "plasto:test:result" => {
                if self.tests.note.is_some() {
                    self.tests.note = None;
                    self.tests.rows.clear();
                }
                self.tests.rows.push(p);
            }
            "plasto:test:baseline" => self.tests.baseline = Some(baseline_status(u(&p["score"]))),
            "plasto:test:done" => {
                self.tests.running = false;
                self.tests_over();
                self.paint_report(&p);
            }
            // Обрыв: в трей, если окно спрятано, и сразу перечитать вкладку.
            "plasto:stab:incident" => {
                if let Some((text, _)) = stab::event_text(&p) {
                    self.tray_alert(&text);
                }
                self.stab.due = Some(Instant::now());
            }
            "plasto:stab:event" => self.stab.due = Some(Instant::now()),
            "plasto:stab:stopped" => {
                self.stab.running = false;
                self.stab.live = None;
                self.stab.due = Some(Instant::now());
            }
            "plasto:stab:error" => self.stab.note = p["error"].as_str().map(str::to_owned),
            "plasto:speed:progress" => {
                self.speed.frac = p["frac"].as_f64().unwrap_or(0.0) as f32;
                let (phase, server) = (s(&p["phase"]).to_owned(), s(&p["server"]).to_owned());
                if phase != self.speed.phase || server != self.speed.server {
                    self.speed.live_series.clear();
                }
                self.speed.live = p["mbps"].as_f64().unwrap_or(0.0);
                if p["mbps"].is_number() {
                    self.speed.live_series.push(self.speed.live);
                }
                self.speed.phase = phase;
                self.speed.server = server;
                let live = p["mbps"].as_f64().map(|v| format!(" · {}", fmt_mbps(v))).unwrap_or_default();
                self.speed.stage = match s(&p["phase"]) {
                    "reach" => "Ищу ближайшие серверы".into(),
                    "down" => format!("Загрузка: {}{live}", s(&p["server"])),
                    _ => format!("Отдача: {}{live}", s(&p["server"])),
                };
            }
            "plasto:speed:rows" => {
                if let Some(rows) = p.as_array() {
                    self.speed.rows = rows.clone();
                }
            }
            "plasto:diag:progress" => {
                if p["last"].is_object() {
                    self.diag.rows.push(p["last"].clone());
                    self.diag.busy = Some(format!("Проверяю… {}/{}", u(&p["done"]), u(&p["total"])));
                }
            }
            "plasto:ai:progress" => {
                if p["last"].is_object() {
                    self.ai.probe_frac = u(&p["done"]) as f32 / u(&p["total"]).max(1) as f32;
                    self.ai.rows.push(p["last"].clone());
                    self.ai.rows_note = None;
                }
            }
            "plasto:tgws:state" => self.paint_tgws(p),
            "plasto:tgws:progress" if p["stage"] == "download" && p["detail"].is_object() => {
                let d = &p["detail"];
                self.tgws_status = Status::new(Tone::Plain, format!("Загружаю tg-ws-proxy · {} из {}", fmt_bytes(u(&d["received"])), fmt_bytes(u(&d["size"]))));
            }
            _ => {}
        }
    }

    fn push_log(&mut self, entry: Value) {
        if entry.is_object() {
            self.logs.push_back(entry);
            while self.logs.len() > 500 {
                self.logs.pop_front();
            }
        }
    }

    fn paint_health(&mut self, h: &Value) {
        if !h.is_object() {
            return;
        }
        self.probes_note = None;
        self.probes = h["results"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| {
                let ok = r["ok"] == true;
                let text = if ok { format!("{} · {}мс", r["status"], u(&r["ms"])) } else { r["error"].as_str().unwrap_or("нет ответа").to_owned() };
                (s(&r["name"]).to_owned(), ok, text)
            })
            .collect();
        self.check_text = format!("{}/{} · {}", u(&h["okCount"]), u(&h["total"]), platform::local_time(u(&h["ts"])));
    }

    /// Служба из старой версии: пока она стоит, включать обход нельзя.
    fn paint_service(&mut self, st: &Value) {
        if st.is_object() && !st["error"].is_string() {
            self.svc = Some(st.clone());
        }
    }

    fn paint_tgws(&mut self, st: Value) {
        if !st.is_object() {
            return;
        }
        let running = st["running"] == true || st["external"] == true;
        if running && self.tgws["running"] != true {
            self.ping_soon(Duration::from_secs(3));
        } else if !running {
            self.ping.mechs.remove("telegram");
        }
        self.tgws_status = if st["running"] == true || st["external"] == true {
            if st["external"] == true {
                Status::new(Tone::Plain, format!("Порт **{}** уже занят — похоже, tg-ws запущен отдельно", st["port"]))
            } else {
                let wait = if st["secret"].is_string() { "" } else { " · жду секрет…" };
                let what = if st["flavor"] == "builtin" { "Работает" } else { "Процесс запущен" };
                Status::new(Tone::Ok, format!("**{what}** · {}:{}{wait}", s(&st["host"]), st["port"]))
            }
        } else if st["installed"]["builtin"] == true {
            Status::new(Tone::Plain, format!("Выключен · встроенный {}", s(&st["installed"]["tag"])))
        } else if st["installed"].is_object() {
            Status::new(Tone::Plain, format!("Выключен · установлен {}", s(&st["installed"]["tag"])))
        } else {
            Status::new(Tone::Plain, "Выключен · бинарь будет скачан при первом запуске")
        };
        self.tgws = st;
    }

    fn paint_report(&mut self, report: &Value) {
        self.tests.running = false;
        let rows = report["results"].as_array().cloned().unwrap_or_default();
        self.tests.rows = rows;
        self.tests.note = if self.tests.rows.is_empty() {
            Some(("Ничего не проверено.".into(), false))
        } else if report["cancelled"] == true {
            Some(("Проверка прервана — показаны частичные результаты.".into(), false))
        } else if report["stoppedOnPerfect"] == true {
            let (done, total) = (self.tests.rows.len(), u(&report["presetsTotal"]));
            Some((format!("Проверено {done} из {total}: нашлась стратегия со 100%, остальные не проверялись. Чтобы проверить все, снимите галку «Остановиться на первой со 100%»."), false))
        } else {
            None
        };
    }

    /// Отчёт прошлого подбора из файла: те же строки, но с датой и оговоркой.
    fn paint_saved(&mut self, report: &Value) {
        self.paint_report(report);
        if report["baseline"]["score"].is_u64() {
            self.tests.baseline = Some(baseline_status(u(&report["baseline"]["score"])));
        }
        let at = u(&report["finishedAt"]);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let age = Duration::from_millis(now.saturating_sub(at)).as_secs();
        let ago = match age {
            0..=3599 => format!("{} мин назад", (age / 60).max(1)),
            3600..=86_399 => format!("{} ч назад", age / 3600),
            _ => format!("{} дн назад", age / 86_400),
        };
        let partial = if report["cancelled"] == true {
            ", прерван".to_owned()
        } else if report["stoppedOnPerfect"] == true {
            format!(", проверено {} из {} — остановлен на первой со 100%", self.tests.rows.len(), u(&report["presetsTotal"]))
        } else {
            String::new()
        };
        let stamp = if at > 0 { format!("{} ({ago}{partial})", platform::local_stamp(at)) } else { "давно".into() };
        self.tests.note = Some((format!("Результаты подбора от {stamp}. С тех пор блокировки могли измениться — для свежих данных проверьте заново."), false));
    }

    fn paint_diag(&mut self, report: &Value) {
        self.diag.busy = None;
        let Some(results) = report["results"].as_array() else {
            self.diag.failed = true;
            self.diag.rows.clear();
            return;
        };
        self.diag.failed = false;
        let tone = match s(&report["verdict"]) {
            "ok" => Tone::Ok,
            "fail" => Tone::Bad,
            _ => Tone::Plain,
        };
        self.diag.verdict = Some(Status::new(tone, s(&report["summary"])));
        // Проблемы наверх, чтобы не искать глазами.
        let order = |l: &str| match l {
            "fail" => 0,
            "warn" => 1,
            _ => 2,
        };
        let mut rows = results.clone();
        rows.sort_by_key(|r| order(s(&r["level"])));
        self.diag.rows = rows;
    }

    /* ---------- ответы -------------------------------------------------- */

    fn on_reply(&mut self, ctx: &egui::Context, op: Op, v: Value) {
        match op {
            Op::Ignore => {}
            Op::Bootstrap => self.on_bootstrap(ctx, v),
            Op::Toggle | Op::Power => {
                self.toggle_waiting = false;
                self.toggle_busy = Some(Instant::now() + Duration::from_millis(700));
                let retry = if matches!(op, Op::Power) { conflict::Retry::Power } else { conflict::Retry::Toggle };
                // Чужой zapret — окно с выбором, а не ошибка.
                if !self.take_conflict(&v, retry) {
                    if let Some(e) = err(&v) {
                        self.flash(capitalize(&e));
                    } else if let Some(e) = v["aiError"].as_str() {
                        self.flash(format!("Прокси ИИ-сервисов: {e}"));
                    }
                }
                if matches!(op, Op::Power) {
                    // hosts поменялся: главной нужно знать, работают ли ИИ-сервисы.
                    self.call(Op::AiList, "ai:list", Value::Null);
                }
            }
            Op::Preview => {
                self.preview = if v.is_null() {
                    None
                } else if let Some(e) = err(&v) {
                    Some((format!("Ошибка разбора: {e}"), true))
                } else {
                    Some((s(&v["command"]).to_owned(), false))
                };
            }
            Op::ApplyStrategy => {
                if let Some(e) = err(&v) {
                    self.flash(capitalize(&e));
                }
            }
            Op::ZapretInstall { auto } => {
                self.boot = None;
                self.upd.busy = None;
                if v["ok"] == true {
                    self.installed(&v);
                    if let Some(first) = self.first.as_mut() {
                        first.done = true;
                    } else if !auto {
                        self.zapret_updated();
                    }
                } else {
                    let e = err(&v).unwrap_or_else(|| "неизвестная ошибка".into());
                    self.upd.error = Some(("zapret", e.clone()));
                    if let Some(first) = self.first.as_mut() {
                        first.failed = Some(e);
                    }
                }
            }
            Op::AppCheck | Op::ZapretCheck | Op::TgwsCheck | Op::AppDownload | Op::TgwsUpdate | Op::EngineRestart => self.on_update_reply(&op, v),
            Op::TgwsStatus => self.paint_tgws(v),
            Op::PickDir => {
                if v.is_null() {
                    return;
                }
                let dir = v["dir"].as_str().map(str::to_owned);
                self.paint_dir(dir.as_deref(), &v["status"]);
                if v["status"]["ok"] == true {
                    if let Some(d) = dir {
                        self.cfg.insert("zapretDir".into(), json!(d));
                    }
                    self.paint_version(None, true);
                    let selected = self.cfg_str("strategy").map(str::to_owned);
                    self.paint_strategies(&v["strategies"], selected);
                    if let Some(first) = self.first.as_mut() {
                        first.done = true;
                        first.failed = None;
                    }
                } else if let Some(e) = err(&v) {
                    self.dir_hint = (e, true);
                }
            }
            Op::Components => {
                if v.is_null() {
                    return;
                }
                if let Some(e) = err(&v) {
                    self.components_note = Some(capitalize(&e));
                    return;
                }
                self.components_dir = s(&v["dir"]).to_owned();
                self.components_note = None;
                if let Some(d) = v["zapretDir"].as_str() {
                    self.cfg.insert("zapretDir".into(), json!(d));
                    self.dir = d.to_owned();
                }
            }
            Op::Service => {
                self.svc_busy = false;
                if let Some(e) = err(&v) {
                    self.flash(capitalize(&e));
                }
                self.paint_service(&v);
                // Строки «Системы» после удаления устарели.
                self.run_diag();
            }
            // Команда разбирается заново: фильтр меняет порты в аргументах.
            Op::GameFilter => self.request_preview(),
            Op::Check => {
                self.check_busy = false;
                if v.is_null() {
                    self.probes.clear();
                    self.probes_note = Some("обход не запущен".into());
                }
            }
            Op::EngineMode(mode) => {
                if v["ok"] == true {
                    self.engine_mode = mode.clone();
                    self.cfg.insert("engineMode".into(), if mode.is_empty() { Value::Null } else { json!(mode) });
                    let selected = self.cfg_str("strategy").map(str::to_owned);
                    let presets = v["presets"].clone();
                    let count = presets.as_array().map_or(0, Vec::len);
                    self.paint_strategies(&presets, selected);
                    self.engine_hint = (format!("Движок: {}. Пресетов найдено: {count}.", s(&v["engine"])), false);
                } else {
                    self.engine_hint = (s(&v["reason"]).to_owned(), true);
                }
            }
            Op::TestRun => {
                self.tests.running = false;
                self.tests_over();
                if self.take_conflict(&v, conflict::Retry::Tests) {
                    self.ping.auto_running = false;
                    self.tests.note = Some(("Подбор не начался: работает другой zapret.".into(), true));
                    return;
                }
                if v.is_null() || v["error"].is_string() {
                    self.tests.rows.clear();
                    self.tests.note = Some((err(&v).unwrap_or_else(|| "тест не выполнен".into()), true));
                } else {
                    self.paint_report(&v);
                }
                if self.ping.auto_running {
                    self.on_auto_pick(&v);
                }
            }
            Op::TestLast => {
                // Пока грузилось, мог начаться новый подбор — его не перетираем.
                if !self.tests.running && self.tests.rows.is_empty() && v["results"].is_array() {
                    self.paint_saved(&v);
                }
            }
            Op::Ping => self.on_ping(v),
            Op::ConflictWinws => self.on_conflict_reply(&op, v),
            Op::TgwsResolve => {
                self.tgws_busy = false;
                self.on_conflict_reply(&op, v);
            }
            Op::StabQuery => self.on_stab_query(v),
            Op::StabLive => self.on_stab_live(v),
            Op::StabStatus => self.stab.running = v["running"] == true,
            Op::StabExport => self.on_stab_export(v),
            Op::StabClear => self.stab.due = Some(Instant::now()),
            Op::Speed => {
                self.speed.running = false;
                if let Some(e) = err(&v).or_else(|| v.is_null().then(|| "замер не выполнен".into())) {
                    self.speed.note = Some((e, true));
                } else {
                    self.speed.note = (v["cancelled"] == true).then(|| ("Замер прерван — показано, что успели измерить.".into(), false));
                    self.speed.rows = v["servers"].as_array().cloned().unwrap_or_default();
                    self.speed.result = Some(v);
                }
            }
            Op::Diag => self.paint_diag(&v),
            Op::TgwsToggle => {
                self.tgws_busy = false;
                if !self.take_conflict(&v, conflict::Retry::Toggle) {
                    if let Some(e) = err(&v) {
                        self.tgws_status = Status::new(Tone::Bad, format!("Ошибка: {e}"));
                    } else {
                        self.paint_tgws(v);
                    }
                }
            }
            Op::TgwsLink => {
                if !v.is_string() {
                    self.tgws_status = Status::new(Tone::Bad, "Секрет ещё не получен — подождите пару секунд после запуска");
                }
            }
            Op::AiList => self.on_ai_list(v),
            Op::AiApply => self.on_ai_apply(v),
            Op::AiSource => {
                self.ai.busy_source = false;
                self.ai.hosts_state = Some(match err(&v) {
                    Some(e) => Status::new(Tone::Bad, format!("Каталог не скачался: {e}")),
                    None => Status::new(
                        Tone::Ok,
                        format!(
                            "Каталог обновлён: **{}** имён через прокси, **{}** в блоклисте{}",
                            u(&v["unblock"]),
                            u(&v["blocklist"]),
                            v["updated"].as_str().map(|d| format!(" · {d}")).unwrap_or_default()
                        ),
                    ),
                });
                self.call(Op::AiList, "ai:list", Value::Null);
            }
            Op::AiDropStale => {
                self.ai.busy_stale = false;
                self.ai.stale_state = Some(match err(&v) {
                    Some(e) => Status::new(Tone::Bad, format!("Не убралось: {e}")),
                    None => {
                        let n = u(&v["removed"]);
                        Status::new(Tone::Ok, format!("Убрано **{n}** устаревших {}", plural(n, "строка", "строки", "строк")))
                    }
                });
                self.call(Op::AiList, "ai:list", Value::Null);
            }
            Op::AiRestore => {
                self.ai.hosts_state = Some(match err(&v) {
                    Some(e) => Status::new(Tone::Bad, format!("Не восстановился: {e}")),
                    None => Status::new(Tone::Ok, "hosts возвращён из резервной копии"),
                });
                self.call(Op::AiList, "ai:list", Value::Null);
            }
            Op::AiProbe { via_proxy } => self.on_ai_probe(v, via_proxy),
            Op::DnsList => self.on_dns_list(v),
            Op::DnsCheck => self.on_dns_check(v),
            Op::DnsWatch => self.on_dns_watch(v),
            Op::DnsRotate => self.on_dns_rotate(v),
            Op::DnsApply { auto } => {
                self.dns.busy_apply = false;
                if auto {
                    if let Some(e) = err(&v) {
                        self.dns_log("error", format!("Автопереключение DNS не удалось: {e}"));
                        self.flash(format!("DNS не переключился: {e}"));
                    }
                }
                // Тумблер переключается сразу: dns:list опрашивает адаптеры
                // несколько секунд и лишь уточнит состояние.
                if err(&v).is_none() {
                    self.dns.has_backup = true;
                }
                self.dns.state = Some(match err(&v) {
                    Some(e) => Status::new(if v["reverted"] == true { Tone::Warn } else { Tone::Bad }, e),
                    None => {
                        let mut t = format!("Выставлено **{}** на {}", join(&v["servers"]), join(&v["adapters"]));
                        if u(&v["doh"]) > 0 {
                            t += " · DoH включён";
                        }
                        if v["unblocks"] != true {
                            t += " · гео этот резолвер **не снимает**";
                        }
                        Status::new(Tone::Ok, t)
                    }
                });
                self.call(Op::DnsList, "dns:list", Value::Null);
            }
            Op::DnsRestore => {
                self.dns.busy_apply = false;
                if err(&v).is_none() {
                    self.dns.has_backup = false;
                }
                self.dns.state = Some(match err(&v) {
                    Some(e) => Status::new(Tone::Bad, format!("Не вернулось: {e}")),
                    None => Status::new(Tone::Ok, format!("Прежние настройки возвращены: {}", join(&v["adapters"]))),
                });
                self.call(Op::DnsList, "dns:list", Value::Null);
            }
            Op::DnsAdd => {
                self.dns.busy_add = false;
                match err(&v) {
                    Some(e) => self.dns.custom_msg = Some(e),
                    None => {
                        let p = &v["preset"];
                        let mut msg = format!("Добавлено: {}", s(&p["name"]));
                        if p["encryptedOnly"] == true {
                            msg += " · только шифрованный (DoH)";
                        }
                        if !p["doh"].is_string() {
                            msg += " · без шифрования";
                        }
                        self.dns.custom_msg = Some(msg);
                        self.dns.custom_text.clear();
                        self.dns.custom_name.clear();
                        self.dns.selected = s(&p["id"]).to_owned();
                        self.call(Op::DnsList, "dns:list", Value::Null);
                    }
                }
            }
            Op::DnsRemove => {
                self.dns.custom_msg = Some(err(&v).unwrap_or_else(|| "Пресет удалён.".into()));
                self.call(Op::DnsList, "dns:list", Value::Null);
            }
            Op::Autostart => {
                self.behavior_note = err(&v).map(|e| format!("Автозапуск не настроен: {e}"));
            }
        }
    }

    fn on_bootstrap(&mut self, ctx: &egui::Context, b: Value) {
        self.cfg = b["config"].as_object().cloned().unwrap_or_default();
        self.components_dir = s(&b["componentsDir"]).to_owned();
        let dir = self.cfg_str("zapretDir").map(str::to_owned);
        let dir_ok = b["dirStatus"]["ok"] == true;
        self.paint_dir(dir.as_deref(), &b["dirStatus"]);
        let tag = b["installed"]["tag"].as_str().or_else(|| self.cfg_str("zapretTag")).map(str::to_owned);
        self.paint_version(tag.as_deref(), dir_ok);
        let selected = self.cfg_str("strategy").map(str::to_owned);
        self.paint_strategies(&b["strategies"], selected);

        let flag = |cfg: &Map<String, Value>, k: &str, default: bool| cfg.get(k).and_then(Value::as_bool).unwrap_or(default);
        self.autostart = flag(&self.cfg, "autostart", false);
        self.auto_connect = flag(&self.cfg, "autoConnect", false);
        self.on_top = flag(&self.cfg, "alwaysOnTop", false);
        self.sound = flag(&self.cfg, "sound", true);
        self.auto_download = flag(&self.cfg, "autoDownload", true);
        // Мониторинг стабильности переживает перезапуск: был включён — продолжаем.
        if flag(&self.cfg, "stabilityMonitor", false) {
            self.stab.running = true;
            self.call(Op::StabStatus, "stab:start", Value::Null);
        }
        self.game_filter = norm_game_filter(self.cfg.get("gameFilter"));
        self.close_action = self.cfg_str("closeAction").unwrap_or("ask").to_owned();
        self.engine_mode = self.cfg_str("engineMode").unwrap_or("").to_owned();
        if let Some(engine) = b["engineDetected"]["engine"].as_str() {
            self.engine_hint = (format!("Определён {engine} — {}.", s(&b["engineDetected"]["reason"])), false);
        }
        self.version = s(&b["version"]).to_owned();
        self.elevated = b["elevated"] == true;
        if b["service"].is_object() {
            self.paint_service(&b["service"]);
        }
        if b["tgws"].is_object() {
            self.paint_tgws(b["tgws"].clone());
        }
        for entry in b["logs"].as_array().cloned().unwrap_or_default() {
            self.push_log(entry);
        }
        self.render_state(s(&b["snapshot"]["state"]), None);
        // Обход мог работать до открытия окна: процесс знает, сколько.
        if let Some(ms) = b["snapshot"]["uptime"].as_u64().filter(|ms| *ms > 0) {
            self.on_since = Instant::now().checked_sub(Duration::from_millis(ms)).or(self.on_since);
        }
        if b["snapshot"]["lastCheck"].is_object() {
            let h = b["snapshot"]["lastCheck"].clone();
            self.paint_health(&h);
        }

        if self.autostart {
            // Путь к exe мог измениться с прошлого запуска — перепишем задачу.
            self.spawn_autostart(true);
        }
        // Списки ИИ и DNS нужны главной: по ним видно, включены ли эти механизмы.
        self.call(Op::AiList, "ai:list", Value::Null);
        self.call(Op::DnsList, "dns:list", Value::Null);
        if !dir_ok && !self.auto_download {
            self.begin_first(first::First { waiting: true, ..Default::default() });
        }
        if !dir_ok && self.auto_download {
            self.begin_first(first::First::default());
            self.boot = Some(Boot { title: "Загружаю компоненты".into(), sub: "последний релиз zapret".into(), frac: 0.0 });
            self.call(Op::ZapretInstall { auto: true }, "zapret:install", Value::Null);
        }
        // «При запуске включать то, что было включено».
        let resume = |key: &str| b["config"]["resume"][key] == true;
        let restore = self.auto_connect || self.after_update;
        if restore && dir_ok && resume("zapret") {
            self.pending_connect = Some(Instant::now() + Duration::from_millis(1200));
        }
        if restore && resume("telegram") && b["tgws"]["running"] != true {
            self.tgws_busy = true;
            self.call(Op::TgwsToggle, "tgws:toggle", Value::Null);
        }
        // «Проверять обновления при запуске»: только проверка, без загрузки.
        if dir_ok && self.first.is_none() && flag(&self.cfg, "checkUpdates", true) {
            self.check_updates();
        }
        #[cfg(debug_assertions)]
        self.debug_ping();
        #[cfg(debug_assertions)]
        self.debug_conflict();
        ctx.request_repaint();
    }

    fn spawn_autostart(&self, on: bool) {
        let tx = self.replies_tx.clone();
        let host = self.host.clone();
        std::thread::spawn(move || {
            let v = match platform::set_autostart(on) {
                Ok(()) => Value::Null,
                Err(e) => json!({ "error": e }),
            };
            let _ = tx.send((Op::Autostart, v));
            host.wake();
        });
    }

    fn on_ai_list(&mut self, st: Value) {
        if let Some(e) = err(&st) {
            self.ai.apply_state = Some(Status::new(Tone::Bad, format!("Раздел ИИ недоступен: {e}")));
            return;
        }
        self.ai.services = st["services"].as_array().cloned().unwrap_or_default();
        let set = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect::<BTreeSet<_>>();
        self.ai.bypass = set(&st["bypass"]);
        self.ai.proxy = set(&st["proxy"]);
        self.ai.hosts_live = u(&st["hostsApplied"]) + u(&st["hostsBlocked"]) > 0;
        if !self.ai.loaded {
            self.ai.extra = s(&st["extra"]).to_owned();
            self.ai.probe_proxy = s(&st["probeProxy"]).to_owned();
            self.ai.entry = s(&st["entry"]).to_owned();
            self.ai.blocklist = st["blocklist"] == true;
            self.ai.loaded = true;
        }
        if st["dirOk"] != true {
            self.ai.apply_state = Some(Status::new(Tone::Bad, "Компоненты zapret не установлены — писать список некуда"));
        } else if u(&st["applied"]) > 0 || u(&st["hostsApplied"]) > 0 {
            self.ai.apply_state =
                Some(Status::new(Tone::Plain, format!("В обходе **{}** доменов · в hosts **{}** имён", u(&st["applied"]), u(&st["hostsApplied"]))));
        } else if self.ai.apply_state.as_ref().is_some_and(|s| s.tone == Tone::Bad && s.text.starts_with("Компоненты")) {
            self.ai.apply_state = None;
        }
        let mut bits = vec![match st["sourceUpdated"].as_str() {
            Some(d) => format!("каталог от {d}"),
            None => "каталог ещё не скачан".into(),
        }];
        if u(&st["hostsBlocked"]) > 0 {
            bits.push(format!("**{}** имён в блоклисте", u(&st["hostsBlocked"])));
        }
        self.ai.hosts_state = Some(Status::new(Tone::Plain, bits.join(" · ")));
        self.paint_ai_stale(u(&st["hostsStale"]));
    }

    fn paint_ai_stale(&mut self, count: u64) {
        self.ai.stale = count;
        if count > 0 {
            self.ai.stale_state = Some(Status::new(
                Tone::Warn,
                format!("Найдена устаревшая копия каталога: **{count}** {} по именам", plural(count, "конфликт", "конфликта", "конфликтов")),
            ));
        }
    }

    fn on_ai_apply(&mut self, res: Value) {
        self.ai.busy_apply = false;
        if let Some(e) = err(&res) {
            self.ai.apply_state = Some(Status::new(Tone::Bad, format!("Не записалось: {e}")));
            return;
        }
        let h = &res["hosts"];
        let mut parts = vec![format!("обход: **{}** доменов", u(&res["list"]["added"]))];
        if h.is_object() {
            parts.push(format!("hosts: **{}** имён", u(&h["proxies"])));
            if h["aclRepaired"] == true {
                parts.push("права на hosts починены".into());
            }
            if u(&h["shadowed"]) > 0 {
                parts.push(format!("перекрыто **{}** устаревших дублей ниже в файле", u(&h["shadowed"])));
            }
            // Мёртвый прокси в hosts хуже пустого hosts: файл читается раньше
            // резолвера. Такие адреса не пишутся — и об этом надо сказать.
            if let Some(swaps) = h["swaps"].as_array().filter(|a| !a.is_empty()) {
                let n: u64 = swaps.iter().map(|s| s["hosts"].as_array().map_or(0, |a| a.len() as u64)).sum();
                let to: Vec<&str> = swaps.iter().filter_map(|s| s["to"].as_str()).collect();
                parts.push(format!(
                    "прокси каталога не отвечал — **{n}** {} на живой **{}**",
                    plural(n, "имя переведено", "имени переведены", "имён переведены"),
                    to.join(", ")
                ));
            }
            if let Some(ip) = h["own"].as_str() {
                let cc = h["ownCountry"].as_str().map(|c| format!(", {c}")).unwrap_or_default();
                parts.push(format!("через свой прокси **{ip}**{cc}"));
            }
            // Российский прокси провайдера обходит, но сервис видит Россию.
            if let Some(dom) = h["domestic"].as_array().filter(|a| !a.is_empty()) {
                let n = dom.len() as u64;
                parts.push(format!("**{n}** {} на российском прокси — блок по стране он не снимет", plural(n, "имя", "имени", "имён")));
            }
            if let Some(skipped) = h["skipped"].as_array().filter(|a| !a.is_empty()) {
                let n = skipped.len() as u64;
                parts.push(format!("**{n}** {} без живого прокси — оставлены на DNS", plural(n, "имя", "имени", "имён")));
            }
        }
        if let Some(e) = res["hostsError"].as_str() {
            parts.push(format!("hosts не записался: {e}"));
        }
        if let Some(e) = res["restartError"].as_str() {
            parts.push(format!("обход не перезапустился: {e}"));
        }
        if let Some(e) = res["settingsError"].as_str() {
            parts.push(format!("настройки не сохранены: {e}"));
        }
        if res["restarted"] == true {
            parts.push("обход перезапущен".into());
        } else if res["list"]["changed"] == true {
            parts.push("применится при следующем запуске обхода".into());
        }
        // «Записано» и «работает» — разные вещи: исход проверки отдельно.
        let mut bad = res["hostsError"].is_string();
        let verify = &h["verify"];
        if verify["active"] == true {
            parts.push("hosts **действует**".into());
        } else if verify["reason"] == "ignored" {
            bad = true;
            parts.push(if h["aclRepaired"] == true {
                "права починены, но служба DNS держит старое состояние — **перезагрузите компьютер**, иначе hosts не заработает".into()
            } else {
                "Windows не читает hosts — проверьте антивирус и «оптимизаторы», которые защищают этот файл".into()
            });
        }
        if h["ruOnly"] == true {
            parts.push("все живые прокси **российские**: блок провайдера снимут, блок по стране — нет. Для гео нужен зарубежный IP (VPN)".into());
        }
        self.ai.apply_state = Some(Status::new(if bad { Tone::Bad } else { Tone::Ok }, parts.join(" · ")));
        if h["stale"].is_object() {
            self.paint_ai_stale(u(&h["stale"]["count"]));
        }
        // Каталог мог скачаться только что — серые галки оживут.
        self.call(Op::AiList, "ai:list", Value::Null);
    }

    fn on_ai_probe(&mut self, report: Value, via_proxy: bool) {
        self.ai.probe_busy = false;
        self.ai.probe_frac = 0.0;
        if report.is_null() || report["error"].is_string() {
            self.ai.verdict = Some(Status::new(Tone::Bad, format!("Проверка не выполнена: {}", err(&report).unwrap_or_else(|| "нет ответа".into()))));
            return;
        }
        let sm = &report["summary"];
        let total = u(&sm["total"]);
        let good = u(&sm["ok"]) + u(&sm["reachable"]);
        let tone = if good == total { Tone::Ok } else if good > 0 { Tone::Plain } else { Tone::Bad };
        let prefix = if via_proxy { "**Через прокси:** " } else { "" };
        self.ai.verdict = Some(Status::new(tone, format!("{prefix}{}", ai_verdict_text(sm))));
        // Сломанное наверх, рабочее вниз; непримененный hosts — в самое начало.
        let order = |st: &str| match st {
            "hosts-inactive" | "proxy-dead" => -1,
            "blocked" | "dns" => 0,
            "geo" | "partial" => 1,
            "proxy" => 2,
            "reachable" => 4,
            "ok" => 5,
            _ => 3,
        };
        let mut rows = report["results"].as_array().cloned().unwrap_or_default();
        rows.sort_by_key(|r| order(s(&r["state"])));
        self.ai.rows = rows;
        self.ai.rows_note = None;
    }

    fn on_dns_list(&mut self, st: Value) {
        self.dns.presets = st["presets"].as_array().cloned().unwrap_or_default();
        let active = st["active"].as_str().map(str::to_owned);
        if active != self.dns.active {
            self.dns.active = active;
            self.dns.fails = 0;
            // Сразу после старта или переключения — дать сети прийти в себя.
            self.dns_watch_in(Duration::from_secs(30));
        }
        let keep = self.dns.presets.iter().any(|p| p["id"] == self.dns.selected.as_str());
        if let Some(active) = st["active"].as_str() {
            self.dns.selected = active.to_owned();
        } else if !keep {
            self.dns.selected = self.dns.presets.first().map(|p| s(&p["id"]).to_owned()).unwrap_or_default();
        }
        self.dns.has_backup = st["hasBackup"] == true;
        let adapters = st["adapters"].as_array().cloned().unwrap_or_default();
        self.dns.state = if let Some(e) = err(&st) {
            Some(Status::new(Tone::Bad, format!("Настройки сети не читаются: {e}")))
        } else if let Some(active) = st["active"].as_str() {
            let name = self.dns.presets.iter().find(|p| p["id"] == active).map_or(active, |p| s(&p["name"]));
            Some(Status::new(Tone::Ok, format!("Сейчас: **{name}**")))
        } else if !adapters.is_empty() {
            let text = adapters
                .iter()
                .map(|a| {
                    let servers = if a["dhcp"] == true { "от роутера".to_owned() } else { Some(join(&a["servers"])).filter(|j| !j.is_empty()).unwrap_or_else(|| "—".into()) };
                    format!("{}: {servers}", s(&a["alias"]))
                })
                .collect::<Vec<_>>()
                .join(" · ");
            Some(Status::new(Tone::Plain, text))
        } else {
            None
        };
    }

    /* ---------- такт логики (и в трее тоже) ----------------------------- */

    pub fn tick(&mut self, ctx: &egui::Context) {
        let cmds: Vec<TrayCmd> = self.tray.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for cmd in cmds {
            match cmd {
                // Свёрнутое окно значок в трее разворачивает, а не прячет.
                TrayCmd::Toggle if self.hidden || self.minimized => self.show(),
                TrayCmd::Toggle => self.hide(),
                TrayCmd::Show => self.show(),
                TrayCmd::Quit => self.quit(),
            }
        }
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(&ev.name, ev.payload);
        }
        while let Ok((op, v)) = self.replies.try_recv() {
            self.on_reply(ctx, op, v);
        }
        self.ping_tick(ctx);
        self.dns_tick(ctx);
        self.stab_tick(ctx);
        if let Some(at) = self.pending_connect {
            let now = Instant::now();
            if now >= at {
                self.pending_connect = None;
                if self.state != "on" && self.state != "timeout" {
                    self.toggle_waiting = true;
                    self.call(Op::Power, "power:on", Value::Null);
                }
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
    }
}

impl App {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        fit_scale(&ctx);
        if let Some((_, until)) = &self.flash {
            let now = Instant::now();
            if now >= *until {
                self.flash = None;
            } else {
                ctx.request_repaint_after(*until - now);
            }
        }
        self.window_ui(ui);
        self.close_dialog_ui(ui);
        self.conflict_ui(ui);
        card::resize_grips(ui);
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.close_dialog.is_some() {
                self.close_dialog = None;
            } else if self.conflict.as_ref().is_some_and(|c| !c.busy) {
                self.conflict = None;
            } else if self.page != Page::Home {
                self.go(Page::Home);
            }
        }
    }

}

/// Интерфейс масштабируется по меньшей стороне окна: тянем окно — растут и
/// шрифты, и кнопки. Шаг 0.05 и потолок 1.6, чтобы текст не плыл.
fn fit_scale(ctx: &egui::Context) {
    let zoom = ctx.zoom_factor();
    let size = ctx.content_rect().size() * zoom;
    let raw = (size.x / BASE_W).min(size.y / BASE_H);
    let step = ((raw * 20.0).round() / 20.0).clamp(1.0, 1.6);
    if (step - zoom).abs() > 0.001 {
        ctx.set_zoom_factor(step);
    }
}

pub const BASE_W: f32 = 1100.0;
pub const BASE_H: f32 = 608.0;
/// Больше окно не растягивается: интерфейс к этому размеру уже в 1,6 раза крупнее.
pub const MAX_W: f32 = 1760.0;
pub const MAX_H: f32 = 973.0;

/* ---------- мелочи ------------------------------------------------------ */

pub fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

pub fn u(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0)
}

pub fn err(v: &Value) -> Option<String> {
    match v.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::String(e)) => Some(e.clone()),
        Some(other) => Some(other.to_string()),
    }
}

fn join(v: &Value) -> String {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
}

fn capitalize(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn fmt_bytes(n: u64) -> String {
    if n == 0 {
        return "0 МБ".into();
    }
    format!("{:.1} МБ", n as f64 / 1_048_576.0)
}

/// Мбит/с: до сотни с десятыми, дальше целыми.
pub fn fmt_mbps(v: f64) -> String {
    if v < 100.0 {
        format!("{v:.1} Мбит/с")
    } else {
        format!("{v:.0} Мбит/с")
    }
}

pub fn plural(n: u64, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let a = n % 100;
    let b = a % 10;
    if (11..20).contains(&a) {
        many
    } else if (2..5).contains(&b) {
        few
    } else if b == 1 {
        one
    } else {
        many
    }
}

/// Старые конфиги хранили Game Filter как true/false.
fn norm_game_filter(v: Option<&Value>) -> String {
    match v {
        Some(Value::Bool(true)) => "all".into(),
        Some(Value::String(s)) if ["off", "all", "tcp", "udp"].contains(&s.as_str()) => s.clone(),
        _ => "off".into(),
    }
}

fn ai_verdict_text(sm: &Value) -> String {
    let total = u(&sm["total"]);
    if total == 0 {
        return "проверка не запускалась".into();
    }
    if u(&sm["ok"]) == total {
        return "все сервисы отвечают".into();
    }
    let mut parts = Vec::new();
    for (key, text) in [
        ("ok", "работает"),
        ("reachable", "отвечает, но гео видно только после входа"),
        ("blocked", "режет сеть — поможет обход"),
        ("geo", "режет сам сервис — нужен зарубежный IP"),
        ("hostsInactive", "мимо прокси — нужна перезагрузка"),
        ("proxyDead", "через мёртвый SNI-прокси — снимите галку «прокси»"),
        ("failed", "без ответа"),
    ] {
        let n = u(&sm[key]);
        if n > 0 {
            parts.push(format!("{n} {text}"));
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plural_ru() {
        assert_eq!(plural(1, "строка", "строки", "строк"), "строка");
        assert_eq!(plural(3, "строка", "строки", "строк"), "строки");
        assert_eq!(plural(11, "строка", "строки", "строк"), "строк");
        assert_eq!(plural(21, "строка", "строки", "строк"), "строка");
        assert_eq!(plural(0, "строка", "строки", "строк"), "строк");
    }

    #[test]
    fn game_filter_legacy_values() {
        assert_eq!(norm_game_filter(Some(&json!(true))), "all");
        assert_eq!(norm_game_filter(Some(&json!(false))), "off");
        assert_eq!(norm_game_filter(Some(&json!("udp"))), "udp");
        assert_eq!(norm_game_filter(Some(&json!("bogus"))), "off");
        assert_eq!(norm_game_filter(None), "off");
    }

    #[test]
    fn error_field_is_read_only_when_present() {
        assert_eq!(err(&json!({ "error": "x" })), Some("x".into()));
        assert_eq!(err(&json!({ "error": null })), None);
        assert_eq!(err(&json!(null)), None);
    }
}
