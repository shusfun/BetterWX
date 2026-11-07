use betterwx::progress::Progress;
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, String>;

pub struct Task {
    pub id: String,
    pub action: String,
    pub progress: Progress,
    pub cancellable: AtomicBool,
    state: Mutex<Inner>,
    started: Instant,
    log: PathBuf,
}

struct Inner {
    status: String,
    message: String,
    bytes: u64,
    total: u64,
    baseline: u64,
    result: Option<Value>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub operation_id: String,
    pub action: String,
    pub status: String,
    pub message: String,
    pub bytes: u64,
    pub total: u64,
    pub speed: f64,
    pub eta: Option<f64>,
    pub cancellable: bool,
    pub records: Vec<String>,
    pub result: Option<Value>,
}

impl Task {
    pub fn new(action: &str, logs: &std::path::Path) -> Result<Arc<Self>> {
        fs::create_dir_all(logs).map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let log = logs.join(format!("{id}.jsonl"));
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log)
            .map_err(|e| e.to_string())?;
        let task = Arc::new(Self {
            id,
            action: action.into(),
            progress: Progress::default(),
            cancellable: AtomicBool::new(true),
            started: Instant::now(),
            log,
            state: Mutex::new(Inner {
                status: "running".into(),
                message: action.into(),
                bytes: 0,
                total: 0,
                baseline: 0,
                result: None,
            }),
        });
        task.record("start", action);
        Ok(task)
    }

    pub fn record(&self, kind: &str, message: &str) {
        let value = serde_json::json!({"operationId": self.id, "action": self.action, "event": kind, "message": message, "at": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()});
        if let Ok(mut file) = OpenOptions::new().append(true).open(&self.log) {
            let _ = writeln!(file, "{value}");
        }
        self.progress.record(kind, message);
    }

    pub fn message(&self, message: &str) {
        self.state.lock().unwrap().message = message.into();
    }

    pub fn download(&self, bytes: u64, total: u64) {
        let mut state = self.state.lock().unwrap();
        state.bytes = bytes;
        state.total = total;
    }

    pub fn baseline(&self, bytes: u64) {
        self.state.lock().unwrap().baseline = bytes;
    }

    pub fn finish(&self, result: Result<Value>) {
        let (status, message, value) = match result {
            Ok(value) => ("success", "操作完成".to_string(), Some(value)),
            Err(error) if self.progress.check().is_err() => ("paused", error, None),
            Err(error) => ("error", error, None),
        };
        self.record(status, &message);
        let mut state = self.state.lock().unwrap();
        state.status = status.into();
        state.message = message;
        state.result = value;
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap();
        let speed = state.bytes.saturating_sub(state.baseline) as f64
            / self.started.elapsed().as_secs_f64().max(0.1);
        let mut records: Vec<_> = self
            .progress
            .records()
            .iter()
            .rev()
            .take(80)
            .map(|r| {
                format!(
                    "{}{} · {}",
                    r.group
                        .as_ref()
                        .map(|g| format!("{g} / "))
                        .unwrap_or_default(),
                    r.name,
                    r.detail
                )
            })
            .collect();
        records.reverse();
        let message = if state.status == "running" && state.total == 0 {
            self.progress
                .steps()
                .iter()
                .rev()
                .find(|s| !s.text().finished)
                .map(|s| format!("{} · {}", s.name, s.text().detail))
                .unwrap_or_else(|| state.message.clone())
        } else {
            state.message.clone()
        };
        Snapshot {
            operation_id: self.id.clone(),
            action: self.action.clone(),
            status: state.status.clone(),
            message,
            bytes: state.bytes,
            total: state.total,
            speed,
            eta: (speed > 0.0 && state.total > state.bytes)
                .then(|| (state.total - state.bytes) as f64 / speed),
            cancellable: self.cancellable.load(Ordering::Relaxed),
            records,
            result: state.result.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_is_correlated_in_result_and_log() {
        let dir = tempfile::tempdir().unwrap();
        let t = Task::new("check", dir.path()).unwrap();
        t.record("resource", "fixture.dll");
        t.finish(Err("rejected".into()));
        let s = t.snapshot();
        assert_eq!(s.status, "error");
        for line in fs::read_to_string(&t.log).unwrap().lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            assert_eq!(row["operationId"], s.operation_id);
        }
    }
}
