//! `config.local.toml`: DigitalOcean access and alert thresholds.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DoConfig {
    pub token: String,
    pub droplet_id: u64,
    pub database_id: String,
    pub poll_seconds: u64,
}

impl Default for DoConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            droplet_id: 0,
            database_id: String::new(),
            poll_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertConfig {
    pub cpu_pct: f64,
    pub cpu_for_minutes: u64,
    pub mem_pct: f64,
    pub disk_pct: f64,
    pub db_cpu_pct: f64,
    pub db_disk_pct: f64,
    /// Agent offline this long before alerting.
    pub unreachable_minutes: u64,
    /// Minimum time between two toasts for the same alert.
    pub cooldown_minutes: u64,
    pub notifications: bool,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            cpu_pct: 90.0,
            cpu_for_minutes: 5,
            mem_pct: 90.0,
            disk_pct: 85.0,
            db_cpu_pct: 90.0,
            db_disk_pct: 85.0,
            unreachable_minutes: 2,
            cooldown_minutes: 30,
            notifications: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub digitalocean: DoConfig,
    pub alerts: AlertConfig,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
    }
}
