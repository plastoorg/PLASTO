//! Хранилище мониторинга: SQLite в папке данных PLASTO.
//!
//! Три уровня подробности, чтобы фильтр «год» не читал тридцать миллионов
//! строк: секунды — двое суток, минуты — два месяца, часы — два года.
//! События (обрывы, сбросы адаптера, сон) хранятся всё время целиком.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use super::analyze::{Agg, Event, Rtt, Sample};

pub const RAW_KEEP: i64 = 2 * 86400;
pub const MINUTE_KEEP: i64 = 60 * 86400;
pub const HOUR_KEEP: i64 = 2 * 366 * 86400;

/// Уровни агрегатов в таблице `agg`.
pub const MINUTE: u8 = 1;
pub const HOUR: u8 = 2;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        // WAL: интерфейс читает, пока сборщик пишет, и никто никого не ждёт.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=3000;
             CREATE TABLE IF NOT EXISTS raw (
                 ts INTEGER PRIMARY KEY, gw REAL, isp REAL, inet REAL, udp REAL,
                 up INTEGER, wifi INTEGER, rssi INTEGER, quality INTEGER, rate INTEGER, link INTEGER,
                 err INTEGER NOT NULL DEFAULT 0, disc INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS agg (level INTEGER NOT NULL, ts INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY (level, ts));
             CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, kind TEXT NOT NULL, detail TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS events_ts ON events (ts);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Store { conn })
    }

    pub fn insert_raw(&mut self, samples: &[Sample]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut st = tx
                .prepare_cached("INSERT OR REPLACE INTO raw (ts, gw, isp, inet, udp, up, wifi, rssi, quality, rate, link, err, disc) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)")
                .map_err(|e| e.to_string())?;
            for s in samples {
                st.execute(params![
                    s.ts,
                    s.rtt[0].to_sql(),
                    s.rtt[1].to_sql(),
                    s.rtt[2].to_sql(),
                    s.rtt[3].to_sql(),
                    s.up,
                    s.wifi,
                    s.rssi,
                    s.quality,
                    s.rate,
                    s.link,
                    s.err,
                    s.disc
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn raw(&self, from: i64, to: i64) -> Result<Vec<Sample>, String> {
        let mut st = self
            .conn
            .prepare_cached("SELECT ts, gw, isp, inet, udp, up, wifi, rssi, quality, rate, link, err, disc FROM raw WHERE ts >= ?1 AND ts < ?2 ORDER BY ts")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![from, to], |r| {
                Ok(Sample {
                    ts: r.get(0)?,
                    rtt: [Rtt::from_sql(r.get(1)?), Rtt::from_sql(r.get(2)?), Rtt::from_sql(r.get(3)?), Rtt::from_sql(r.get(4)?)],
                    up: r.get(5)?,
                    wifi: r.get::<_, Option<bool>>(6)?.unwrap_or(false),
                    rssi: r.get(7)?,
                    quality: r.get(8)?,
                    rate: r.get(9)?,
                    link: r.get(10)?,
                    err: r.get(11)?,
                    disc: r.get(12)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    pub fn put_agg(&mut self, level: u8, a: &Agg) -> Result<(), String> {
        let data = serde_json::to_string(a).map_err(|e| e.to_string())?;
        self.conn.execute("INSERT OR REPLACE INTO agg (level, ts, data) VALUES (?1, ?2, ?3)", params![level, a.ts, data]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn aggs(&self, level: u8, from: i64, to: i64) -> Result<Vec<Agg>, String> {
        let mut st = self.conn.prepare_cached("SELECT data FROM agg WHERE level = ?1 AND ts >= ?2 AND ts < ?3 ORDER BY ts").map_err(|e| e.to_string())?;
        let rows = st.query_map(params![level, from, to], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for data in rows {
            // Битая строка не должна ронять весь график.
            if let Ok(a) = serde_json::from_str::<Agg>(&data.map_err(|e| e.to_string())?) {
                out.push(a);
            }
        }
        Ok(out)
    }

    pub fn add_event(&mut self, e: &Event) -> Result<(), String> {
        self.conn.execute("INSERT INTO events (ts, kind, detail) VALUES (?1, ?2, ?3)", params![e.ts, e.kind, e.detail.to_string()]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn events(&self, from: i64, to: i64) -> Result<Vec<Event>, String> {
        let mut st = self.conn.prepare_cached("SELECT ts, kind, detail FROM events WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, id").map_err(|e| e.to_string())?;
        let rows = st
            .query_map(params![from, to], |r| {
                let detail: String = r.get(2)?;
                Ok(Event { ts: r.get(0)?, kind: r.get(1)?, detail: serde_json::from_str(&detail).unwrap_or(Value::Null) })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }

    /// С какого момента вообще есть замеры — чтобы честно сказать
    /// «данные с такого-то числа», а не рисовать пустой год.
    pub fn first_ts(&self) -> Option<i64> {
        let q = |sql: &str| self.conn.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).ok().flatten();
        [q("SELECT MIN(ts) FROM raw"), q("SELECT MIN(ts) FROM agg")].into_iter().flatten().min()
    }

    pub fn meta(&self, key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn prune(&mut self, now: i64) -> Result<(), String> {
        let run = |sql: &str, p: i64| self.conn.execute(sql, [p]).map(|_| ()).map_err(|e| e.to_string());
        run("DELETE FROM raw WHERE ts < ?1", now - RAW_KEEP)?;
        run("DELETE FROM agg WHERE level = 1 AND ts < ?1", now - MINUTE_KEEP)?;
        run("DELETE FROM agg WHERE level = 2 AND ts < ?1", now - HOUR_KEEP)?;
        Ok(())
    }

    /// «Очистить историю»: замеры и события, но не отметки журнала событий —
    /// иначе старые сбросы адаптера загрузились бы снова.
    pub fn clear(&mut self) -> Result<(), String> {
        self.conn.execute_batch("DELETE FROM raw; DELETE FROM agg; DELETE FROM events;").map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("s.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn секунды_сохраняются_со_всеми_тремя_состояниями_ответа() {
        let (_d, mut st) = temp();
        let s = Sample { ts: 100, rtt: [Rtt::Ms(1.5), Rtt::Skip, Rtt::Lost, Rtt::Ms(20.0)], up: Some(true), wifi: true, rssi: Some(-61), quality: Some(80), rate: Some(866), link: None, err: 2, disc: 0 };
        st.insert_raw(&[s.clone(), Sample { ts: 101, ..Default::default() }]).unwrap();
        let back = st.raw(100, 101).unwrap();
        assert_eq!(back, vec![s]);
        assert_eq!(st.raw(0, 1000).unwrap().len(), 2);
        assert_eq!(st.first_ts(), Some(100));
    }

    #[test]
    fn агрегаты_события_и_чистка_по_возрасту() {
        let (_d, mut st) = temp();
        let now = 10 * 86400;
        let a = Agg { ts: now - 3600, secs: 60, err: 5, ..Default::default() };
        st.put_agg(MINUTE, &a).unwrap();
        st.put_agg(MINUTE, &Agg { ts: now - MINUTE_KEEP - 60, secs: 1, ..Default::default() }).unwrap();
        st.put_agg(HOUR, &Agg { ts: now - 7200, secs: 3600, ..Default::default() }).unwrap();
        st.insert_raw(&[Sample { ts: now - RAW_KEEP - 1, ..Default::default() }, Sample { ts: now - 5, ..Default::default() }]).unwrap();
        st.add_event(&Event::new(now - 10, "outage", json!({ "scope": "isp", "secs": 4 }))).unwrap();
        st.prune(now).unwrap();
        assert_eq!(st.aggs(MINUTE, 0, now).unwrap(), vec![a]);
        assert_eq!(st.aggs(HOUR, 0, now).unwrap().len(), 1);
        assert_eq!(st.raw(0, now).unwrap().len(), 1);
        let ev = st.events(0, now).unwrap();
        assert_eq!((ev[0].kind.as_str(), ev[0].detail["secs"].as_i64()), ("outage", Some(4)));
        st.set_meta("k", "v").unwrap();
        st.clear().unwrap();
        assert!(st.events(0, now).unwrap().is_empty());
        assert_eq!(st.meta("k").as_deref(), Some("v"));
    }
}
