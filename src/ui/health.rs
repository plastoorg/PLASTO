//! Пинг до сервисов на главной и автоподбор стратегии: если zapret работает,
//! а YouTube и Discord не отвечают, в режиме «Автоматически» PLASTO сам
//! прогоняет стратегии и включает лучшую. Здесь же присмотр за DNS: если
//! выставленный пресет перестал отвечать или подменять, и стоит галка
//! автопереключения, — переход на рабочий того же рода.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::{capitalize, kit::Tone, s, u, App, Mech, Op};

/// Как часто перепроверять, пока что-то включено.
const EVERY: Duration = Duration::from_secs(10 * 60);
/// Первая неудача может быть случайной: перепроверка через полминуты.
const RETRY: Duration = Duration::from_secs(30);
/// Пока сломано — проверять чаще, чтобы заметить, когда снова заработает.
const BROKEN_EVERY: Duration = Duration::from_secs(2 * 60);
/// Автоподбор не чаще этого: стратегия, что то работает, то нет, не должна
/// гонять тесты по кругу.
const AUTO_PAUSE: Duration = Duration::from_secs(30 * 60);
/// Ниже этого процента стратегию не включаем: лучше сказать, что не нашлось.
const GOOD_ENOUGH: u64 = 50;
/// Как часто проверять выставленный DNS.
const DNS_EVERY: Duration = Duration::from_secs(5 * 60);
/// Первая неудача может быть случайной: перепроверка через полминуты.
const DNS_RETRY: Duration = Duration::from_secs(30);
/// После переключения столько не переключаемся снова, чтобы не метаться.
const DNS_PAUSE: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub struct Ping {
    pub busy: bool,
    /// Когда пришёл последний ответ.
    pub at: Option<Instant>,
    /// Когда проверить в следующий раз.
    pub due: Option<Instant>,
    /// Ответ по механизмам: `{"zapret": {"ms": 142, "sites": [...]}, ...}`.
    pub mechs: Map<String, Value>,
    /// Сколько проверок подряд zapret не открывал сайты.
    pub fails: u32,
    /// Когда последний раз запускался автоподбор.
    pub auto_at: Option<Instant>,
    /// Идущий тест стратегий запущен автоподбором.
    pub auto_running: bool,
}

impl Mech {
    pub fn key(self) -> &'static str {
        match self {
            Mech::Zapret => "zapret",
            Mech::Telegram => "telegram",
            Mech::Ai => "ai",
            Mech::Dns => "dns",
        }
    }
}

/// «только что», «5 мин назад», «2 ч назад».
pub fn ago(at: Instant) -> String {
    let secs = at.elapsed().as_secs();
    match secs {
        0..=59 => "только что".into(),
        60..=3599 => format!("{} мин назад", secs / 60),
        _ => format!("{} ч назад", secs / 3600),
    }
}

impl App {
    /// Проверить через `delay`, если раньше ничего не назначено.
    pub(super) fn ping_soon(&mut self, delay: Duration) {
        let at = Instant::now() + delay;
        if self.ping.due.is_none_or(|due| due > at) {
            self.ping.due = Some(at);
        }
    }

    /// Пинг включённых механизмов. Выключенные не меряются и не показываются.
    pub(super) fn run_ping(&mut self) {
        self.ping.due = None;
        let on: Vec<&str> = [Mech::Zapret, Mech::Telegram, Mech::Ai, Mech::Dns].into_iter().filter(|m| self.mech_info(*m).on).map(Mech::key).collect();
        if on.is_empty() || self.ping.busy {
            return;
        }
        self.ping.busy = true;
        self.call(Op::Ping, "ping:run", json!({ "mechs": on }));
    }

    /// Каждый кадр: не пора ли проверить.
    pub(super) fn ping_tick(&mut self, ctx: &egui::Context) {
        if let Some(due) = self.ping.due {
            let now = Instant::now();
            if now >= due {
                self.run_ping();
            } else {
                ctx.request_repaint_after(due - now);
            }
        }
    }

    pub(super) fn on_ping(&mut self, v: Value) {
        self.ping.busy = false;
        self.ping.at = Some(Instant::now());
        self.ping.mechs = v["mechs"].as_object().cloned().unwrap_or_default();
        self.ping_soon(EVERY);
        let zapret = &v["mechs"]["zapret"];
        // Подбор может выключить zapret до конца проверки: его замеры не в счёт.
        if self.tests.running || !zapret["sites"].as_array().is_some_and(|s| !s.is_empty()) {
            return;
        }
        if zapret["ok"] == true {
            self.ping.fails = 0;
            return;
        }
        self.ping.fails += 1;
        if self.ping.fails < 2 {
            self.ping_soon(RETRY);
            return;
        }
        self.ping_soon(BROKEN_EVERY);
        if self.ping.fails == 2 {
            let down: Vec<String> = zapret["sites"].as_array().into_iter().flatten().filter(|site| site["ms"].is_null()).map(|site| s(&site["host"]).to_owned()).collect();
            self.tray_alert(&format!("Не открывается: {}", down.join(", ")));
        }
        self.maybe_auto_pick();
    }

    /// Окно в трее — сказать уведомлением; открыто — и так видно на карточке.
    pub(super) fn tray_alert(&self, text: &str) {
        if self.hidden {
            crate::platform::notify("PLASTO", text);
        }
    }

    /// Что показать в строке механизма вместо «включено»: пинг или «нет ответа».
    pub(super) fn ping_text(&self, mech: Mech) -> Option<(String, Tone, String)> {
        // Мерить было нечем (чужой прокси без секрета) — пинга не показываем.
        let r = self.ping.mechs.get(mech.key()).filter(|r| r["sites"].as_array().is_some_and(|s| !s.is_empty()))?;
        let sites: Vec<String> = r["sites"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|site| match site["ms"].as_u64() {
                Some(ms) => format!("{}: {ms} мс", s(&site["host"])),
                None => format!("{}: {}", s(&site["host"]), s(&site["error"])),
            })
            .collect();
        let tip = sites.join("\n");
        Some(match r["ms"].as_u64() {
            // Часть сайтов молчит: время есть, но важнее — что не открывается.
            Some(_) if r["ok"] == false => {
                let down: Vec<&str> = r["down"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                (format!("нет {}", down.join(", ")), Tone::Warn, tip)
            }
            Some(ms) => (format!("{ms} мс"), Tone::Ok, tip),
            None => ("нет ответа".into(), Tone::Warn, tip),
        })
    }

    /// zapret работает, но сайты молчат: в режиме «Автоматически» — подбор.
    fn maybe_auto_pick(&mut self) {
        let auto = self.cfg_str("strategyMode").unwrap_or("auto") == "auto";
        if !auto || self.tests.running || self.speed.running || self.ping.auto_at.is_some_and(|at| at.elapsed() < AUTO_PAUSE) {
            return;
        }
        self.ping.auto_at = Some(Instant::now());
        self.ping.auto_running = true;
        self.flash("Сайты не открываются, подбираю другую стратегию");
        self.start_tests(None);
    }

    /// Тест, запущенный автоподбором, закончился: включить лучшую стратегию.
    pub(super) fn on_auto_pick(&mut self, report: &Value) {
        self.ping.auto_running = false;
        self.ping.fails = 0;
        if let Some(e) = report["error"].as_str() {
            self.tray_alert(&format!("Подбор стратегии не удался: {e}"));
            self.flash(format!("Подбор не удался: {e}"));
            return;
        }
        let best = report["results"].as_array().and_then(|rows| rows.iter().filter(|r| r["ok"] != false).max_by_key(|r| u(&r["score"])));
        match best {
            Some(row) if u(&row["score"]) >= GOOD_ENOUGH => {
                let file = s(&row["preset"]).to_owned();
                let label = s(&row["label"]).to_owned();
                let current = self.cfg_str("strategy").unwrap_or("").to_owned();
                if file != current {
                    self.save_strategy(file, true);
                }
                let text = capitalize(&format!("включена стратегия {label}: открывается {}%", u(&row["score"])));
                self.tray_alert(&text);
                self.flash(text);
                self.ping_soon(Duration::from_secs(5));
            }
            _ => {
                self.tray_alert("Подходящей стратегии не нашлось");
                self.flash("Подходящей стратегии не нашлось. Попробуйте подобрать вручную в «Диагностике»");
            }
        }
    }
}

impl App {
    pub(super) fn dns_auto(&self) -> bool {
        self.cfg.get("dnsAutoSwitch").and_then(Value::as_bool).unwrap_or(false)
    }

    /// Проверить все пресеты: по кнопке и при первом открытии раздела.
    pub(super) fn dns_check_all(&mut self) {
        if self.dns.checking {
            return;
        }
        self.dns.checking = true;
        self.call(Op::DnsCheck, "dns:check", json!({}));
    }

    /// Следующая проверка выставленного DNS — если есть что проверять.
    pub(super) fn dns_watch_in(&mut self, delay: Duration) {
        self.dns.due = (self.dns_auto() && self.dns.active.is_some()).then(|| Instant::now() + delay);
    }

    pub(super) fn dns_tick(&mut self, ctx: &egui::Context) {
        let Some(due) = self.dns.due else { return };
        let now = Instant::now();
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }
        self.dns.due = None;
        if let (Some(active), true, false) = (self.dns.active.clone(), self.dns_auto(), self.dns.busy_apply) {
            self.call(Op::DnsWatch, "dns:check", json!({ "ids": [active] }));
        }
    }

    pub(super) fn dns_name(&self, id: &str) -> String {
        self.dns.presets.iter().find(|p| p["id"] == id).map_or(id, |p| s(&p["name"])).to_owned()
    }

    fn dns_merge(&mut self, v: &Value) {
        if let Some(results) = v["results"].as_object() {
            for (id, h) in results {
                self.dns.health.insert(id.clone(), h.clone());
            }
            self.dns.checked_at = Some(Instant::now());
        }
    }

    pub(super) fn on_dns_check(&mut self, v: Value) {
        self.dns.checking = false;
        self.dns_merge(&v);
        // И при ошибке: иначе раздел запросит проверку снова на следующем кадре.
        self.dns.checked_at = Some(Instant::now());
    }

    /// Итог фоновой проверки выставленного DNS.
    pub(super) fn on_dns_watch(&mut self, v: Value) {
        self.dns_merge(&v);
        let Some(active) = self.dns.active.clone() else { return };
        // Нет сети вообще — DNS тут ни при чём.
        let bad = v["online"] == true && matches!(s(&v["results"][&active]["state"]), "down" | "noSpoof");
        if !bad {
            self.dns.fails = 0;
            self.dns_watch_in(DNS_EVERY);
            return;
        }
        self.dns.fails += 1;
        if self.dns.fails < 2 {
            self.dns_watch_in(DNS_RETRY);
        } else if self.dns.switched_at.is_some_and(|t| t.elapsed() < DNS_PAUSE) {
            self.dns_watch_in(DNS_EVERY);
        } else {
            self.dns.checking = true;
            self.call(Op::DnsRotate, "dns:check", json!({ "current": active }));
        }
    }

    /// Проверены все пресеты: переключаемся на лучший рабочий, если он есть.
    pub(super) fn on_dns_rotate(&mut self, v: Value) {
        self.dns.checking = false;
        self.dns_merge(&v);
        let Some(active) = self.dns.active.clone() else { return };
        let from = self.dns_name(&active);
        self.dns.fails = 0;
        match v["next"].as_str() {
            Some(next) if self.dns_auto() => {
                let to = self.dns_name(next);
                self.dns.switched_at = Some(Instant::now());
                self.dns.selected = next.to_owned();
                self.dns.busy_apply = true;
                self.dns.doing = Some(("переключаю…", "текущий не отвечает, ставлю рабочий"));
                self.dns_log("warn", format!("DNS {from} не работает, переключаю на {to}"));
                self.flash(format!("DNS {from} не работает, переключаю на {to}"));
                self.call(Op::DnsApply { auto: true }, "dns:apply", json!(next));
            }
            _ => {
                self.dns_log("warn", format!("DNS {from} не работает, рабочей замены нет"));
                self.flash(format!("DNS {from} не работает, рабочей замены нет"));
                self.dns_watch_in(DNS_EVERY);
            }
        }
    }

    pub(super) fn dns_log(&mut self, level: &str, message: String) {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        self.push_log(json!({ "ts": ts, "level": level, "message": message }));
    }
}

/// Только в отладочной сборке (`PLASTO_PING=1`): обход «включён», пинг
/// подставлен — чтобы посмотреть главную без прав администратора.
#[cfg(debug_assertions)]
impl App {
    pub(super) fn debug_ping(&mut self) {
        if std::env::var("PLASTO_PING").is_err() {
            return;
        }
        self.render_state("on", None);
        self.tgws = json!({ "running": true });
        let site = |host: &str, ms: Option<u64>| match ms {
            Some(ms) => json!({ "host": host, "ms": ms }),
            None => json!({ "host": host, "error": "таймаут" }),
        };
        self.ping.mechs = json!({
            "zapret": { "ms": 142, "sites": [site("www.youtube.com", Some(131)), site("discord.com", Some(153))] },
            "telegram": { "ms": null, "sites": [site("kws2.web.telegram.org", None), site("web.telegram.org", None)] },
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        self.ping.at = Instant::now().checked_sub(Duration::from_secs(150));
        self.ping.due = None;
    }
}
