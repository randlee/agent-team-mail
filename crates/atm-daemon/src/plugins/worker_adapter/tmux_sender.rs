//! Shared tmux delivery implementation with retries, verification and rate limiting.

use crate::plugin::PluginError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};
use uuid::Uuid;

const MAX_ATTEMPTS: u32 = 3;
const BASE_BACKOFF_MS: u64 = 100;
const VERIFY_DELAY_MS: u64 = 75;
const MIN_SEND_INTERVAL_MS: u64 = 200;
const TEXT_TO_ENTER_DELAY_MS: u64 = 500;
const CAPTURE_LINES: &str = "-200";

static LAST_SEND_BY_PANE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

fn last_send_map() -> &'static Mutex<HashMap<String, Instant>> {
    LAST_SEND_BY_PANE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Delivery mechanism for injecting text into a tmux pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMethod {
    /// `tmux send-keys -l "<text>"`
    SendKeys,
    /// `tmux set-buffer` + `tmux paste-buffer`
    PasteBuffer,
}

impl DeliveryMethod {
    /// Read method override from `ATM_TMUX_DELIVERY_METHOD`.
    ///
    /// Supported values:
    /// - `send-keys`
    /// - `paste-buffer`
    pub fn from_env() -> Option<Self> {
        let value = std::env::var("ATM_TMUX_DELIVERY_METHOD").ok()?;
        match value.trim().to_ascii_lowercase().as_str() {
            "send-keys" => Some(Self::SendKeys),
            "paste-buffer" => Some(Self::PasteBuffer),
            other => {
                warn!(
                    "Unknown ATM_TMUX_DELIVERY_METHOD='{}', falling back to default",
                    other
                );
                None
            }
        }
    }
}

#[async_trait]
pub trait TmuxSender: Send + Sync {
    /// Send text followed by Enter with reliability checks.
    async fn send_text_and_enter(
        &self,
        pane_id: &str,
        text: &str,
        method: DeliveryMethod,
        context: &str,
    ) -> Result<(), PluginError>;

    /// Send Enter only with pane validation + retries.
    async fn send_enter(&self, pane_id: &str, context: &str) -> Result<(), PluginError>;
}

/// Default tmux sender used by worker adapter code paths.
#[derive(Debug, Clone, Default)]
pub struct DefaultTmuxSender;

impl DefaultTmuxSender {
    fn runtime_error(msg: String) -> PluginError {
        PluginError::Runtime {
            message: msg,
            source: None,
        }
    }

    #[cfg(unix)]
    fn validate_pane_exists(&self, pane_id: &str) -> Result<(), PluginError> {
        let output = Command::new("tmux")
            .arg("display-message")
            .arg("-p")
            .arg("-t")
            .arg(pane_id)
            .arg("#{pane_id}")
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("tmux pane validation failed for {pane_id}: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Self::runtime_error(format!(
                "tmux pane '{pane_id}' is unavailable: {stderr}"
            )));
        }

        Ok(())
    }

    #[cfg(not(unix))]
    fn validate_pane_exists(&self, _pane_id: &str) -> Result<(), PluginError> {
        Ok(())
    }

    async fn enforce_min_send_interval(&self, pane_id: &str) {
        let delay = {
            let guard = last_send_map().lock();
            if let Ok(map) = guard {
                map.get(pane_id)
                    .and_then(|last| {
                        let elapsed = last.elapsed();
                        if elapsed < Duration::from_millis(MIN_SEND_INTERVAL_MS) {
                            Some(Duration::from_millis(MIN_SEND_INTERVAL_MS) - elapsed)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default()
            } else {
                Duration::ZERO
            }
        };

        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        if let Ok(mut map) = last_send_map().lock() {
            map.insert(pane_id.to_string(), Instant::now());
        }
    }

    fn backoff_with_jitter(attempt: u32) -> Duration {
        let base = BASE_BACKOFF_MS * (1 << (attempt.saturating_sub(1)));
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        // 75%..=125%
        let pct = 75 + (nanos % 51) as u64;
        Duration::from_millis(base * pct / 100)
    }

    fn capture_verify_enabled() -> bool {
        std::env::var("ATM_TMUX_VERIFY_CAPTURE")
            .ok()
            .is_some_and(|v| {
                let lower = v.trim().to_ascii_lowercase();
                matches!(lower.as_str(), "1" | "true" | "yes" | "on")
            })
    }

    #[cfg(unix)]
    fn send_payload(&self, pane_id: &str, payload: &str, method: DeliveryMethod) -> Result<(), PluginError> {
        match method {
            DeliveryMethod::SendKeys => {
                let output = Command::new("tmux")
                    .arg("send-keys")
                    .arg("-t")
                    .arg(pane_id)
                    .arg("-l")
                    .arg(payload)
                    .output()
                    .map_err(|e| PluginError::Runtime {
                        message: format!("tmux send-keys failed for pane {pane_id}: {e}"),
                        source: Some(Box::new(e)),
                    })?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(Self::runtime_error(format!(
                        "tmux send-keys literal failed for pane {pane_id}: {stderr}"
                    )));
                }
            }
            DeliveryMethod::PasteBuffer => {
                let buffer = format!("atm-delivery-{}", Uuid::new_v4());
                let set_output = Command::new("tmux")
                    .arg("set-buffer")
                    .arg("-b")
                    .arg(&buffer)
                    .arg("--")
                    .arg(payload)
                    .output()
                    .map_err(|e| PluginError::Runtime {
                        message: format!("tmux set-buffer failed for pane {pane_id}: {e}"),
                        source: Some(Box::new(e)),
                    })?;
                if !set_output.status.success() {
                    let stderr = String::from_utf8_lossy(&set_output.stderr);
                    return Err(Self::runtime_error(format!(
                        "tmux set-buffer failed for pane {pane_id}: {stderr}"
                    )));
                }

                let paste_output = Command::new("tmux")
                    .arg("paste-buffer")
                    .arg("-d")
                    .arg("-b")
                    .arg(&buffer)
                    .arg("-t")
                    .arg(pane_id)
                    .output()
                    .map_err(|e| PluginError::Runtime {
                        message: format!("tmux paste-buffer failed for pane {pane_id}: {e}"),
                        source: Some(Box::new(e)),
                    })?;
                if !paste_output.status.success() {
                    let stderr = String::from_utf8_lossy(&paste_output.stderr);
                    return Err(Self::runtime_error(format!(
                        "tmux paste-buffer failed for pane {pane_id}: {stderr}"
                    )));
                }
            }
        }

        Ok(())
    }

    #[cfg(not(unix))]
    fn send_payload(&self, _pane_id: &str, _payload: &str, _method: DeliveryMethod) -> Result<(), PluginError> {
        Ok(())
    }

    #[cfg(unix)]
    fn send_enter_once(&self, pane_id: &str) -> Result<(), PluginError> {
        let output = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(pane_id)
            .arg("Enter")
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("tmux send Enter failed for pane {pane_id}: {e}"),
                source: Some(Box::new(e)),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Self::runtime_error(format!(
                "tmux send Enter failed for pane {pane_id}: {stderr}"
            )));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn send_enter_once(&self, _pane_id: &str) -> Result<(), PluginError> {
        Ok(())
    }

    #[cfg(unix)]
    fn verify_text_visible(&self, pane_id: &str, text: &str) -> Result<(), PluginError> {
        let output = Command::new("tmux")
            .arg("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(pane_id)
            .arg("-S")
            .arg(CAPTURE_LINES)
            .output()
            .map_err(|e| PluginError::Runtime {
                message: format!("tmux capture-pane failed for pane {pane_id}: {e}"),
                source: Some(Box::new(e)),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Self::runtime_error(format!(
                "tmux capture-pane failed for pane {pane_id}: {stderr}"
            )));
        }

        let captured = String::from_utf8_lossy(&output.stdout);
        if !captured.contains(text) {
            return Err(Self::runtime_error(format!(
                "delivery verification failed for pane {pane_id}: text not found in capture"
            )));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn verify_text_visible(&self, _pane_id: &str, _text: &str) -> Result<(), PluginError> {
        Ok(())
    }
}

#[async_trait]
impl TmuxSender for DefaultTmuxSender {
    async fn send_text_and_enter(
        &self,
        pane_id: &str,
        text: &str,
        method: DeliveryMethod,
        context: &str,
    ) -> Result<(), PluginError> {
        let mut last_err: Option<PluginError> = None;

        for attempt in 1..=MAX_ATTEMPTS {
            if let Err(e) = self.validate_pane_exists(pane_id) {
                last_err = Some(e);
            } else {
                self.enforce_min_send_interval(pane_id).await;
                let result = (|| -> Result<(), PluginError> {
                    self.send_payload(pane_id, text, method)?;
                    Ok(())
                })();
                if let Err(e) = result {
                    last_err = Some(e);
                } else {
                    tokio::time::sleep(Duration::from_millis(TEXT_TO_ENTER_DELAY_MS)).await;
                    match self.send_enter_once(pane_id) {
                        Err(e) => last_err = Some(e),
                        Ok(()) => {
                            if Self::capture_verify_enabled() {
                                tokio::time::sleep(Duration::from_millis(VERIFY_DELAY_MS)).await;
                                match self.verify_text_visible(pane_id, text) {
                                    Ok(()) => return Ok(()),
                                    Err(e) => last_err = Some(e),
                                }
                            } else {
                                return Ok(());
                            }
                        }
                    }
                }
            }

            if attempt < MAX_ATTEMPTS {
                let delay = Self::backoff_with_jitter(attempt);
                debug!(
                    "tmux delivery retry for pane {} [{}], attempt {}/{} after {:?}",
                    pane_id, context, attempt + 1, MAX_ATTEMPTS, delay
                );
                tokio::time::sleep(delay).await;
            }
        }

        let detail = last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown delivery failure".to_string());
        Err(Self::runtime_error(format!(
            "tmux delivery failed for pane {} [{}] after {} attempts: {}",
            pane_id, context, MAX_ATTEMPTS, detail
        )))
    }

    async fn send_enter(&self, pane_id: &str, context: &str) -> Result<(), PluginError> {
        let mut last_err: Option<PluginError> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            if let Err(e) = self.validate_pane_exists(pane_id) {
                last_err = Some(e);
            } else {
                self.enforce_min_send_interval(pane_id).await;
                match self.send_enter_once(pane_id) {
                    Ok(()) => return Ok(()),
                    Err(e) => last_err = Some(e),
                }
            }

            if attempt < MAX_ATTEMPTS {
                tokio::time::sleep(Self::backoff_with_jitter(attempt)).await;
            }
        }

        let detail = last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown enter failure".to_string());
        Err(Self::runtime_error(format!(
            "tmux Enter send failed for pane {} [{}] after {} attempts: {}",
            pane_id, context, MAX_ATTEMPTS, detail
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn test_delivery_method_from_env_parse() {
        unsafe {
            std::env::set_var("ATM_TMUX_DELIVERY_METHOD", "send-keys");
        }
        assert_eq!(DeliveryMethod::from_env(), Some(DeliveryMethod::SendKeys));
        unsafe {
            std::env::set_var("ATM_TMUX_DELIVERY_METHOD", "paste-buffer");
        }
        assert_eq!(DeliveryMethod::from_env(), Some(DeliveryMethod::PasteBuffer));
        unsafe {
            std::env::remove_var("ATM_TMUX_DELIVERY_METHOD");
        }
        assert_eq!(DeliveryMethod::from_env(), None);
    }

    #[test]
    fn test_backoff_with_jitter_stays_in_expected_range() {
        let d1 = DefaultTmuxSender::backoff_with_jitter(1).as_millis() as u64;
        let d2 = DefaultTmuxSender::backoff_with_jitter(2).as_millis() as u64;
        let d3 = DefaultTmuxSender::backoff_with_jitter(3).as_millis() as u64;

        assert!((75..=125).contains(&d1));
        assert!((150..=250).contains(&d2));
        assert!((300..=500).contains(&d3));
    }
}
