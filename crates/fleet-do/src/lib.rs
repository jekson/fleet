//! DigitalOcean API client. Independent of the monitored server: works over
//! the DO control plane only.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const API: &str = "https://api.digitalocean.com";

#[derive(Debug, thiserror::Error)]
pub enum DoError {
    #[error("http: {0}")]
    Http(String),
    #[error("status {0}: {1}")]
    Status(u16, String),
    #[error("parse: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DropletInfo {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub vcpus: u32,
    pub memory_mb: u64,
    pub disk_gb: u64,
    pub region: String,
    pub public_ipv4: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostMetrics {
    pub cpu_pct: Option<f64>,
    pub mem_pct: Option<f64>,
    pub disk_used_pct: Option<f64>,
    pub load1: Option<f64>,
    pub net_in_mbps: Option<f64>,
    pub net_out_mbps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbInfo {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub version: String,
    pub status: String,
    pub size: String,
    pub num_nodes: u32,
    pub region: String,
    pub host: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbMetrics {
    pub cpu_pct: Option<f64>,
    pub mem_pct: Option<f64>,
    pub disk_pct: Option<f64>,
    pub connections: Option<f64>,
    pub load1: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoSnapshot {
    pub fetched_at_unix: i64,
    pub droplet: Option<DropletInfo>,
    pub droplet_metrics: Option<HostMetrics>,
    pub db: Option<DbInfo>,
    pub db_metrics: Option<DbMetrics>,
    pub errors: Vec<String>,
}

pub struct DoClient {
    http: reqwest::Client,
    token: String,
}

type Series = Vec<(Value, Vec<(f64, f64)>)>;

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

fn droplet_from(d: &Value) -> DropletInfo {
    let ip = d["networks"]["v4"].as_array().and_then(|a| {
        a.iter()
            .find(|n| n["type"] == "public")
            .and_then(|n| n["ip_address"].as_str().map(String::from))
    });
    DropletInfo {
        id: d["id"].as_u64().unwrap_or(0),
        name: s(d, "name"),
        status: s(d, "status"),
        vcpus: d["vcpus"].as_u64().unwrap_or(0) as u32,
        memory_mb: d["memory"].as_u64().unwrap_or(0),
        disk_gb: d["disk"].as_u64().unwrap_or(0),
        region: d["region"]["slug"].as_str().unwrap_or_default().to_string(),
        public_ipv4: ip,
    }
}

fn db_from(d: &Value) -> DbInfo {
    DbInfo {
        id: s(d, "id"),
        name: s(d, "name"),
        engine: s(d, "engine"),
        version: s(d, "version"),
        status: s(d, "status"),
        size: s(d, "size"),
        num_nodes: d["num_nodes"].as_u64().unwrap_or(0) as u32,
        region: s(d, "region"),
        host: d["connection"]["host"].as_str().map(String::from),
    }
}

/// Prometheus matrix result -> list of (labels, [(ts, value)]).
fn matrix(v: &Value) -> Series {
    v["data"]["result"]
        .as_array()
        .map(|r| {
            r.iter()
                .map(|e| {
                    let pts = e["values"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| {
                                    let val = match &p[1] {
                                        Value::String(x) => x.parse::<f64>().ok()?,
                                        o => o.as_f64()?,
                                    };
                                    Some((p[0].as_f64()?, val))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (e["metric"].clone(), pts)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn last(m: &Series, pick: impl Fn(&Value) -> bool) -> Option<f64> {
    m.iter().find(|(l, p)| pick(l) && !p.is_empty()).and_then(|(_, p)| p.last().map(|x| x.1))
}

/// CPU% from cumulative per-mode seconds: two latest points, idle vs total.
fn cpu_pct(m: &Series) -> Option<f64> {
    let (mut idle, mut total) = (0.0, 0.0);
    for (l, p) in m {
        if p.len() < 2 {
            continue;
        }
        let d = p[p.len() - 1].1 - p[p.len() - 2].1;
        total += d;
        if l["mode"] == "idle" {
            idle += d;
        }
    }
    (total > 0.0).then(|| (100.0 * (1.0 - idle / total)).clamp(0.0, 100.0))
}

/// Parse Prometheus text exposition: (name, labels-string, value).
pub fn parse_prom(text: &str) -> Vec<(String, String, f64)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (head, val) = l.trim().rsplit_once(' ')?;
            let val: f64 = val.parse().ok()?;
            let (name, labels) = match head.split_once('{') {
                Some((n, rest)) => (n, rest.trim_end_matches('}')),
                None => (head, ""),
            };
            Some((name.to_string(), labels.to_string(), val))
        })
        .collect()
}

fn db_metrics_from(p: &[(String, String, f64)]) -> DbMetrics {
    let get = |n: &str| p.iter().find(|x| x.0 == n).map(|x| x.2);
    let sum = |names: &[&str]| {
        let v: Vec<f64> = p.iter().filter(|x| names.contains(&x.0.as_str())).map(|x| x.2).collect();
        (!v.is_empty()).then(|| v.iter().sum())
    };
    // Telegraf-style names (best guess for the DO managed DB exporter).
    let cpu = p
        .iter()
        .find(|x| x.0 == "cpu_usage_idle" && x.1.contains("cpu-total"))
        .or_else(|| p.iter().find(|x| x.0 == "cpu_usage_idle"))
        .map(|x| 100.0 - x.2);
    DbMetrics {
        cpu_pct: cpu,
        mem_pct: get("mem_used_percent"),
        disk_pct: get("disk_used_percent"),
        connections: sum(&[
            "postgresql_numbackends",
            "mysql_threads_connected",
            "redis_connected_clients",
            "mongodb_connections_current",
        ]),
        load1: get("system_load1"),
    }
}

impl DoClient {
    pub fn new(token: String) -> Result<Self, DoError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| DoError::Http(e.to_string()))?;
        Ok(Self { http, token })
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Vec<u8>, DoError> {
        let r = rb.send().await.map_err(|e| DoError::Http(e.to_string()))?;
        let st = r.status();
        let b = r.bytes().await.map_err(|e| DoError::Http(e.to_string()))?;
        if !st.is_success() {
            let body = String::from_utf8_lossy(&b).chars().take(200).collect();
            return Err(DoError::Status(st.as_u16(), body));
        }
        Ok(b.to_vec())
    }

    async fn get(&self, path: &str) -> Result<Value, DoError> {
        let b = self.send(self.http.get(format!("{API}{path}")).bearer_auth(&self.token)).await?;
        serde_json::from_slice(&b).map_err(|e| DoError::Parse(e.to_string()))
    }

    pub async fn list_droplets(&self) -> Result<Vec<DropletInfo>, DoError> {
        let v = self.get("/v2/droplets?per_page=200").await?;
        Ok(v["droplets"].as_array().map(|a| a.iter().map(droplet_from).collect()).unwrap_or_default())
    }

    pub async fn list_databases(&self) -> Result<Vec<DbInfo>, DoError> {
        let v = self.get("/v2/databases").await?;
        Ok(v["databases"].as_array().map(|a| a.iter().map(db_from).collect()).unwrap_or_default())
    }

    async fn droplet_metric(&self, name: &str, id: u64, start: i64, end: i64, extra: &str) -> Series {
        let p = format!("/v2/monitoring/metrics/droplet/{name}?host_id={id}&start={start}&end={end}{extra}");
        self.get(&p).await.map(|v| matrix(&v)).unwrap_or_default()
    }

    async fn droplet_metrics(&self, id: u64) -> HostMetrics {
        let end = now();
        let start = end - 300;
        let root = |l: &Value| l["mountpoint"].as_str().is_none_or(|m| m == "/");
        let (cpu, mt, ma, fs, ff, ld, bi, bo) = tokio::join!(
            self.droplet_metric("cpu", id, start, end, ""),
            self.droplet_metric("memory_total", id, start, end, ""),
            self.droplet_metric("memory_available", id, start, end, ""),
            self.droplet_metric("filesystem_size", id, start, end, ""),
            self.droplet_metric("filesystem_free", id, start, end, ""),
            self.droplet_metric("load_1", id, start, end, ""),
            self.droplet_metric("bandwidth", id, start, end, "&interface=public&direction=inbound"),
            self.droplet_metric("bandwidth", id, start, end, "&interface=public&direction=outbound"),
        );
        let ratio = |num: Option<f64>, den: Option<f64>| match (num, den) {
            (Some(n), Some(d)) if d > 0.0 => Some(n / d),
            _ => None,
        };
        HostMetrics {
            cpu_pct: cpu_pct(&cpu),
            mem_pct: ratio(last(&ma, |_| true), last(&mt, |_| true)).map(|r| 100.0 * (1.0 - r)),
            disk_used_pct: ratio(last(&ff, root), last(&fs, root)).map(|r| 100.0 * (1.0 - r)),
            load1: last(&ld, |_| true),
            net_in_mbps: last(&bi, |_| true),
            net_out_mbps: last(&bo, |_| true),
        }
    }

    async fn db_metrics(&self, cluster: &Value) -> Result<DbMetrics, DoError> {
        let ep = cluster["metrics_endpoints"].as_array().and_then(|a| a.first());
        let host = ep
            .and_then(|e| e["host"].as_str())
            .or_else(|| cluster["connection"]["host"].as_str())
            .ok_or_else(|| DoError::Parse("no db host".into()))?;
        let port = ep.and_then(|e| e["port"].as_u64()).unwrap_or(9273);
        let cred = self.get("/v2/databases/metrics/credentials").await?;
        let cr = &cred["credentials"];
        let (u, p) = (s(cr, "basic_auth_username"), s(cr, "basic_auth_password"));
        let url = format!("https://{host}:{port}/metrics");
        let b = self.send(self.http.get(url).basic_auth(u, Some(p))).await?;
        Ok(db_metrics_from(&parse_prom(&String::from_utf8_lossy(&b))))
    }

    /// Never fails as a whole; per-part errors land in `errors`.
    pub async fn snapshot(&self, droplet_id: Option<u64>, database_id: Option<&str>) -> DoSnapshot {
        let mut snap = DoSnapshot { fetched_at_unix: now(), ..Default::default() };
        let d = async {
            let id = droplet_id?;
            let path = format!("/v2/droplets/{id}");
            Some(tokio::join!(self.get(&path), self.droplet_metrics(id)))
        };
        let b = async {
            let id = database_id?;
            let info = self.get(&format!("/v2/databases/{id}")).await;
            let m = match &info {
                Ok(v) => Some(self.db_metrics(&v["database"]).await),
                Err(_) => None,
            };
            Some((info, m))
        };
        let (d, b) = tokio::join!(d, b);
        if let Some((info, m)) = d {
            match info {
                Ok(v) => snap.droplet = Some(droplet_from(&v["droplet"])),
                Err(e) => snap.errors.push(format!("droplet: {e}")),
            }
            snap.droplet_metrics = Some(m);
        }
        if let Some((info, m)) = b {
            match info {
                Ok(v) => snap.db = Some(db_from(&v["database"])),
                Err(e) => snap.errors.push(format!("database: {e}")),
            }
            match m {
                Some(Ok(m)) => snap.db_metrics = Some(m),
                Some(Err(e)) => snap.errors.push(format!("db metrics: {e}")),
                None => {}
            }
        }
        snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prom_parse() {
        let t = "# HELP x\ncpu_usage_idle{cpu=\"cpu-total\"} 75.5\nmem_used_percent 40\n";
        let m = db_metrics_from(&parse_prom(t));
        assert_eq!(m.cpu_pct, Some(24.5));
        assert_eq!(m.mem_pct, Some(40.0));
    }
}
