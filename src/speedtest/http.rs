//! Провайдеры по обычному HTTPS: файл с зеркала, LibreSpeed, Cloudflare.

use std::{
    io::Read,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

/// Тело одного POST: дальше поток шлёт новый запрос.
const UP_CHUNK: u64 = 16 << 20;

fn agent() -> Result<ureq::Agent, String> {
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    Ok(ureq::AgentBuilder::new()
        .tls_connector(Arc::new(tls))
        .timeout_connect(Duration::from_secs(5))
        // Короткий: замедленный сервер держит соединение и молчит, а поток
        // должен заметить «стоп» вовремя.
        .timeout_read(Duration::from_secs(3))
        .timeout_write(Duration::from_secs(3))
        .user_agent("PLASTO")
        .build())
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("сервер ответил {code}"),
        e => e.to_string(),
    }
}

/// Качает `url` по кругу, пока не скажут остановиться.
pub fn pull(url: &str, bytes: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    let agent = agent()?;
    let mut buf = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::Acquire) {
        let mut body = agent.get(url).call().map_err(describe)?.into_reader();
        loop {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            match body.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => bytes.fetch_add(n as u64, Ordering::Relaxed),
                Err(_) if stop.load(Ordering::Acquire) => return Ok(()),
                Err(e) => return Err(e.to_string()),
            };
        }
    }
    Ok(())
}

/// Тело POST из нулей: считает отданное и обрывается по сигналу. Длина не
/// объявляется (chunked), поэтому оборвать можно в любой момент.
pub struct Zeros<'a> {
    pub left: u64,
    pub bytes: &'a AtomicU64,
    pub stop: &'a AtomicBool,
}

impl Read for Zeros<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 || self.stop.load(Ordering::Acquire) {
            return Ok(0);
        }
        let n = buf.len().min(64 * 1024).min(self.left as usize);
        buf[..n].fill(0);
        self.left -= n as u64;
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

pub fn push(url: &str, bytes: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    let agent = agent()?;
    while !stop.load(Ordering::Acquire) {
        let body = Zeros { left: UP_CHUNK, bytes, stop };
        match agent.post(url).set("Content-Type", "application/octet-stream").send(body) {
            Ok(_) => {}
            // Оборванное по «стопу» тело сервер вправе не принять.
            Err(_) if stop.load(Ordering::Acquire) => break,
            Err(e) => return Err(describe(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn нули_обрываются_по_сигналу_и_по_лимиту() {
        let (bytes, stop) = (AtomicU64::new(0), AtomicBool::new(false));
        let mut z = Zeros { left: 100_000, bytes: &bytes, stop: &stop };
        let mut sink = Vec::new();
        z.read_to_end(&mut sink).unwrap();
        assert_eq!(sink.len(), 100_000);
        assert_eq!(bytes.load(Ordering::Relaxed), 100_000);
        stop.store(true, Ordering::Release);
        let mut z = Zeros { left: 100_000, bytes: &bytes, stop: &stop };
        assert_eq!(z.read(&mut [0u8; 16]).unwrap(), 0);
    }
}
