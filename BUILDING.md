# Сборка PLASTO

Всё приложение — один Rust-бинарь: интерфейс на [egui](https://github.com/emilk/egui)
с выводом через DirectX 12 (wgpu), без Electron и WebView. Если видеокарты нет
(удалённый рабочий стол, виртуальная машина) или задано `PLASTO_RENDERER=cpu`,
кадр растеризуется на процессоре. В трее устройство видеокарты отдаётся целиком.

## Локально

Нужны [Rust](https://rustup.rs) (stable, MSVC) и Windows SDK: `rc.exe` вшивает
иконку и манифест в exe.

Двойной щелчок по **`build.cmd`** — тесты и release-сборка. Или вручную:

```bat
cargo test
cargo build --release      :: target\release\plasto.exe
```

Тесты запускаются без прав администратора: манифест с `requireAdministrator`
вшивается только в `plasto.exe`, а логика живёт в библиотеке крейта.

Отладочная сборка открывает нужный раздел сразу: `--PLASTO_PAGE=services|diag|settings|first`
и `--PLASTO_TAB=speed|stability|system|tests|log` (аргументы, а не переменные
окружения: они не переходят через запрос UAC).

## Релиз

Сборку и публикацию делает GitHub Actions (`.github/workflows/build.yml`): тесты
на каждый push и PR, а по тегу `v*` exe попадает в **Releases** как
`PLASTO-<версия>.exe` вместе с `SHA256SUMS-<версия>.txt`.

```bat
:: поднять version в Cargo.toml и закоммитить
git tag v1.0.0
git push origin main v1.0.0
```

Версия в теге должна совпадать с `version` в `Cargo.toml`. Тег с дефисом
(`v1.0.0-beta-1`) публикуется как pre-release.

### Встроенный zapret и автосборки

Сборка Flowseal zapret-discord-youtube лежит в `assets/zapret` (без `service.bat`,
`utils/` и `ipset-all.txt.backup` — PLASTO их не запускает), её версия — в
`assets/zapret.json`; build.rs пакует её в exe. Обновить вручную:

```bat
python ciefresh-zapret.py --apply
```

Раз в сутки (и вручную: **Actions → build → Run workflow**, галка «Проверить новый
zapret») CI делает то же сам: вышла новая сборка — поднимает версию PLASTO
(`1.0.0-beta-2` → `beta-3`), гоняет тесты, коммитит в `main` с тегом и публикует
pre-release «автосборка, не проверено». Его видит только канал обновлений «Бета».
Тест `bundled::встроенная_сборка_цела_и_все_стратегии_собираются` не пустит в
релиз сборку, чьи стратегии PLASTO не разбирает.

Раннер — свой `[self-hosted, Linux, ARM64]` (Raspberry Pi 4): тесты логики идут
нативно под Linux, а `plasto.exe` собирается кросс-компиляцией скриптом
`ci/build-windows.sh` через `cargo-xwin`. На раннере нужны `rustup` и
`python3` и `sudo apt install clang lld llvm libssl-dev pkg-config`, около 1 ГБ свободной
памяти. Тесты интерфейса идут только в Windows — их запускает `build.cmd`.

## Живые проверки

Помечены `#[ignore]`, ходят в сеть и в систему:

```bat
cargo test --release --lib -- --ignored --nocapture живой_замер      :: замер скорости целиком
cargo test --lib -- --ignored --nocapture живая_система              :: Win32-вызовы мониторинга
cargo test --lib -- --ignored --nocapture живой_сборщик              :: 80 с сбора стабильности
```

Список серверов 2ip вшит снимком (`src/speed-2ip.txt`): их страница закрыта
проверкой на ботов, снимок обновляется вручную.
