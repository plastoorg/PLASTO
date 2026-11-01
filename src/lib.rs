//! PLASTO: бэкенд (запуск winws, тесты стратегий, hosts, DNS, служба) и
//! интерфейс на egui. Бинарь — тонкая обёртка в main.rs: манифест с
//! requireAdministrator вшивается только в него, тесты библиотеки запускаются
//! без повышения прав.

pub mod ai;
pub mod api;
pub mod bundled;
pub mod conflict;
pub mod diagnostics;
pub mod dns;
pub mod doh;
pub mod engines;
pub mod host;
pub mod hosts;
pub mod isolate;
pub mod ping;
pub mod platform;
pub mod probe;
pub mod selfupdate;
pub mod runtime;
pub mod service;
pub mod speedtest;
pub mod stability;
pub mod strategy;
pub mod tester;
pub mod tgws;
#[cfg(windows)]
pub mod ui;
pub mod updater;
