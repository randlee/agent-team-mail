use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc;
use uuid::Uuid;

const GH_OBSERVABILITY_LEDGER_FILE: &str = ".atm/daemon/gh-observability.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GhLedgerKind {
    Execution,
    Freshness,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhLedgerRecord {
    pub kind: GhLedgerKind,
    pub action: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_used_in_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_limit_per_hour: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_reset_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_call_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl GhLedgerRecord {
    pub fn new(kind: GhLedgerKind, action: impl Into<String>) -> Self {
        Self {
            kind,
            action: action.into(),
            at: Utc::now().to_rfc3339(),
            request_id: None,
            call_id: None,
            team: None,
            repo: None,
            runtime: None,
            caller: None,
            info_type: None,
            argv: None,
            branch: None,
            reference: None,
            lifecycle_state: None,
            in_flight: None,
            budget_used_in_window: None,
            budget_limit_per_hour: None,
            rate_limit_remaining: None,
            rate_limit_limit: None,
            rate_limit_reset_at: None,
            cache_age_secs: None,
            linked_call_ids: None,
            duration_ms: None,
            result: None,
            error: None,
            block_reason: None,
            degraded_reason: None,
        }
    }
}

enum WriterMessage {
    Append { path: PathBuf, line: String },
    Flush(mpsc::Sender<()>),
}

fn writer_sender() -> &'static mpsc::Sender<WriterMessage> {
    static SENDER: OnceLock<mpsc::Sender<WriterMessage>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<WriterMessage>();
        std::thread::Builder::new()
            .name("gh-observability-ledger".to_string())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    match message {
                        WriterMessage::Append { path, line } => {
                            let _ = append_line(&path, &line);
                        }
                        WriterMessage::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("spawn gh observability ledger writer");
        tx
    })
}

fn append_line(path: &Path, line: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("create gh ledger directory {}: {err}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| format!("open gh ledger {}: {err}", path.display()))?;
    writeln!(file, "{line}").map_err(|err| format!("append gh ledger {}: {err}", path.display()))
}

pub fn gh_observability_ledger_path(home: &Path) -> PathBuf {
    home.join(GH_OBSERVABILITY_LEDGER_FILE)
}

pub fn new_gh_request_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn new_gh_call_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn append_gh_observability_record(home: &Path, record: &GhLedgerRecord) -> Result<(), String> {
    let path = gh_observability_ledger_path(home);
    let line = serde_json::to_string(record)
        .map_err(|err| format!("serialize gh ledger record for {}: {err}", path.display()))?;
    writer_sender()
        .send(WriterMessage::Append { path, line })
        .map_err(|err| format!("send gh ledger record to writer: {err}"))
}

pub fn flush_gh_observability_records() -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    writer_sender()
        .send(WriterMessage::Flush(tx))
        .map_err(|err| format!("send gh ledger flush to writer: {err}"))?;
    rx.recv()
        .map_err(|err| format!("wait for gh ledger flush completion: {err}"))
}

pub fn read_gh_observability_records(home: &Path) -> Result<Vec<GhLedgerRecord>, String> {
    flush_gh_observability_records()?;
    let path = gh_observability_ledger_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|err| format!("read gh ledger {}: {err}", path.display()))?;
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<GhLedgerRecord>(line)
                .map_err(|err| format!("parse gh ledger line `{line}`: {err}"))
        })
        .collect()
}
