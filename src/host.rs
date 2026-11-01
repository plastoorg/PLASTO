//! То, что раньше давал `tauri::AppHandle`: события в интерфейс, общее
//! состояние движков и папки профиля.
//!
//! Модули бэкенда зовут `app.emit(...)`, `app.state::<T>()` и
//! `app.path().app_data_dir()` так же, как при Tauri, поэтому их логика
//! переехала без изменений. Интерфейс забирает события из канала и
//! перерисовывается только когда они приходят.

use std::{
    io,
    path::PathBuf,
    sync::{mpsc, Arc, OnceLock},
};

use serde::Serialize;
use serde_json::Value;

use crate::{runtime::EngineState, tgws};

/// Папка профиля та же, что была у Tauri-сборки (`identifier` из
/// tauri.conf.json), чтобы настройки и скачанные компоненты не потерялись.
const IDENTIFIER: &str = "app.plasto.desktop";

pub struct Event {
    pub name: String,
    pub payload: Value,
}

#[derive(Clone)]
pub struct Host(Arc<Inner>);

struct Inner {
    events: mpsc::Sender<Event>,
    wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
    engine: EngineState,
    tgws: tgws::State,
}

impl Host {
    pub fn new() -> (Self, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let inner = Inner { events: tx, wake: OnceLock::new(), engine: Default::default(), tgws: Default::default() };
        (Host(Arc::new(inner)), rx)
    }

    /// Чем будить интерфейс после события. Ставится один раз, когда окно создано.
    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.0.wake.set(Box::new(wake));
    }

    pub fn wake(&self) {
        if let Some(wake) = self.0.wake.get() {
            wake();
        }
    }

    pub fn emit<S: Serialize>(&self, event: &str, payload: S) -> Result<(), String> {
        let payload = serde_json::to_value(payload).map_err(|e| e.to_string())?;
        self.0.events.send(Event { name: event.to_owned(), payload }).map_err(|e| e.to_string())?;
        self.wake();
        Ok(())
    }

    pub fn state<T: Managed>(&self) -> &T {
        T::pick(self)
    }

    pub fn path(&self) -> Paths {
        Paths
    }
}

pub trait Managed {
    fn pick(host: &Host) -> &Self;
}

impl Managed for EngineState {
    fn pick(host: &Host) -> &Self {
        &host.0.engine
    }
}

impl Managed for tgws::State {
    fn pick(host: &Host) -> &Self {
        &host.0.tgws
    }
}

pub struct Paths;

impl Paths {
    pub fn app_config_dir(&self) -> io::Result<PathBuf> {
        profile_dir()
    }

    pub fn app_data_dir(&self) -> io::Result<PathBuf> {
        profile_dir()
    }
}

fn profile_dir() -> io::Result<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(p).join(IDENTIFIER))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "не найдена папка APPDATA"))
}
