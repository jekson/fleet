//! Alert list with dedupe and toast cooldown.

use serde::Serialize;
use std::collections::HashMap;

const MAX_ALERTS: usize = 300;

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub key: String,
    /// `critical` | `warning` | `info`
    pub severity: String,
    /// `agent` | `do` | `app`
    pub source: String,
    pub title: String,
    pub detail: String,
    pub first_ms: u64,
    pub last_ms: u64,
    pub active: bool,
    pub resolved_ms: Option<u64>,
    pub acked: bool,
}

#[derive(Default)]
pub struct Engine {
    pub list: Vec<Alert>,
    last_notified: HashMap<String, u64>,
}

/// What the caller should do after `raise`.
pub struct Raised {
    pub notify: bool,
    pub changed: bool,
}

impl Engine {
    /// Raises (or refreshes) `key`. `notify` is true for a new activation
    /// outside the cooldown window.
    #[allow(clippy::too_many_arguments)]
    pub fn raise(
        &mut self,
        key: &str,
        severity: &str,
        source: &str,
        title: String,
        detail: String,
        now: u64,
        cooldown_ms: u64,
    ) -> Raised {
        if let Some(a) = self.list.iter_mut().find(|a| a.key == key && a.active) {
            let changed = a.title != title || a.severity != severity;
            a.last_ms = now;
            a.title = title;
            a.detail = detail;
            a.severity = severity.into();
            return Raised {
                notify: false,
                changed,
            };
        }
        self.list.insert(
            0,
            Alert {
                key: key.into(),
                severity: severity.into(),
                source: source.into(),
                title,
                detail,
                first_ms: now,
                last_ms: now,
                active: true,
                resolved_ms: None,
                acked: false,
            },
        );
        self.list.truncate(MAX_ALERTS);
        let last = self.last_notified.get(key).copied().unwrap_or(0);
        let notify = now.saturating_sub(last) >= cooldown_ms;
        if notify {
            self.last_notified.insert(key.into(), now);
        }
        Raised {
            notify,
            changed: true,
        }
    }

    /// Clears `key`; true if something was active.
    pub fn resolve(&mut self, key: &str, now: u64) -> bool {
        let mut any = false;
        for a in self.list.iter_mut().filter(|a| a.key == key && a.active) {
            a.active = false;
            a.resolved_ms = Some(now);
            any = true;
        }
        any
    }

    /// Clears every active alert whose key starts with `prefix` and is not in `keep`.
    pub fn resolve_prefix_except(&mut self, prefix: &str, keep: &[String], now: u64) -> bool {
        let mut any = false;
        for a in self
            .list
            .iter_mut()
            .filter(|a| a.active && a.key.starts_with(prefix) && !keep.contains(&a.key))
        {
            a.active = false;
            a.resolved_ms = Some(now);
            any = true;
        }
        any
    }

    pub fn active(&self) -> impl Iterator<Item = &Alert> {
        self.list.iter().filter(|a| a.active)
    }

    pub fn ack_all(&mut self) {
        for a in &mut self.list {
            a.acked = true;
        }
    }

    pub fn clear_resolved(&mut self) {
        self.list.retain(|a| a.active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_and_cools_down() {
        let mut e = Engine::default();
        let cd = 1000;
        assert!(e.raise("k", "warning", "app", "t".into(), "d".into(), 10_000, cd).notify);
        // Still active: no new toast, no new row.
        assert!(!e.raise("k", "warning", "app", "t".into(), "d".into(), 10_500, cd).notify);
        assert_eq!(e.list.len(), 1);
        assert!(e.resolve("k", 10_600));
        // Re-fires within cooldown: new row, no toast.
        assert!(!e.raise("k", "warning", "app", "t".into(), "d".into(), 10_700, cd).notify);
        assert!(e.resolve("k", 10_800));
        assert!(e.raise("k", "warning", "app", "t".into(), "d".into(), 12_000, cd).notify);
        assert_eq!(e.active().count(), 1);
    }
}
