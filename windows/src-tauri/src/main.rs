//! Fleet for Windows: a monitor for one server (agent over SSH) plus
//! DigitalOcean droplet / managed DB, with threshold alerts as toasts.
//!
//! Read-only by design: the only change this app makes on a server is
//! `install_agent` in Agent-only security mode.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod config;
mod keys;
mod pushkey;

use config::Config;
use fleet_core_ffi::{
    AgentEventRow, CoreListener, ExistingAgentAction, FleetCore, FleetError, HostKeyPrompt,
    InstallArtifact, InstallListener, InstallProgress, JournalPageRow, JournalQueryArgs,
    JournalSink, LogPriority, MetricSeriesRow, MetricsSampleRow, MetricsSink, SecurityModeArg,
    ServerMetricsRow, SessionKind, StateChange, StreamHandle, StreamStatus,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// 1 h of 10 s fleet telemetry.
const HISTORY: usize = 360;
const ALERT_TICK: Duration = Duration::from_secs(15);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// When this process started: "unreachable for N min" counts from here at most.
fn app_start_ms() -> u64 {
    static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *START.get_or_init(now_ms)
}

fn err(e: FleetError) -> String {
    match e {
        FleetError::SshKeyRefused => "SSH key refused: add this app's SSH public key to \
            /root/.ssh/authorized_keys (or use the one-time password)"
            .into(),
        e => e.to_string(),
    }
}

// ---------- paths ----------

struct Paths {
    /// `windows/`
    root: PathBuf,
    data: PathBuf,
    config: PathBuf,
}

/// Where keys, cache and config live:
/// 1. `FLEET_WIN_DIR` (`<dir>/data`, `<dir>/config.local.toml`);
/// 2. the source tree (`windows/data`), only for an exe run from `target/`;
/// 3. `%LOCALAPPDATA%\Fleet` (everything in that one folder, so it can be
///    copied whole to another PC).
fn paths() -> Paths {
    let split = |root: PathBuf| Paths {
        data: root.join("data"),
        config: root.join("config.local.toml"),
        root,
    };
    if let Some(d) = std::env::var_os("FLEET_WIN_DIR") {
        return split(PathBuf::from(d));
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf);
    let from_target = std::env::current_exe().ok().is_some_and(|exe| {
        src.as_ref()
            .and_then(|s| s.parent())
            .is_some_and(|repo| exe.starts_with(repo.join("target")))
    });
    if let Some(src) = src.filter(|s| from_target && s.exists()) {
        return split(src);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Fleet");
    Paths {
        data: base.clone(),
        config: base.join("config.local.toml"),
        root: base,
    }
}

/// Newest `target/linux/x86_64/fleet-agent_*_amd64.deb`, else that dir.
fn default_deb(root: &Path) -> String {
    let dir = root
        .parent()
        .unwrap_or(root)
        .join("target")
        .join("linux")
        .join("x86_64");
    let newest = std::fs::read_dir(&dir).ok().and_then(|rd| {
        rd.filter_map(Result::ok)
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("fleet-agent_") && n.ends_with("_amd64.deb")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
            .map(|e| e.path())
    });
    newest.unwrap_or(dir).to_string_lossy().into_owned()
}

// ---------- shared state ----------

#[derive(Default)]
struct Live {
    state: String,
    failure: Option<String>,
    state_since_ms: u64,
    last_metrics_ms: u64,
    cpu: Option<f32>,
    mem: Option<f32>,
    disk: Option<f32>,
    cpu_high_since: Option<u64>,
    history: VecDeque<(u64, Option<f32>, Option<f32>, Option<f32>)>,
}

struct Shared {
    app: tauri::AppHandle,
    core: Option<Arc<FleetCore>>,
    paths: Paths,
    phase: Mutex<String>,
    error: Mutex<Option<String>>,
    config: Mutex<Config>,
    config_error: Option<String>,
    live: Mutex<HashMap<String, Live>>,
    alerts: Mutex<alerts::Engine>,
    do_snapshot: Mutex<Option<serde_json::Value>>,
    do_fetching: tokio::sync::Mutex<()>,
    detail_stream: Mutex<Option<Arc<StreamHandle>>>,
    journal_stream: Mutex<Option<Arc<StreamHandle>>>,
}

impl Shared {
    fn core(&self) -> Result<Arc<FleetCore>, String> {
        self.core
            .clone()
            .ok_or_else(|| lock(&self.error).clone().unwrap_or("core not open".into()))
    }

    fn emit<T: Serialize + Clone>(&self, name: &str, payload: T) {
        let _ = self.app.emit(name, payload);
    }

    fn toast(&self, title: &str, body: &str) -> Result<(), String> {
        if !lock(&self.config).alerts.notifications {
            return Ok(());
        }
        self.app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| e.to_string())
    }

    fn cooldown_ms(&self) -> u64 {
        lock(&self.config).alerts.cooldown_minutes * 60_000
    }

    /// Raises a batch of alerts under `prefix` and clears the rest of it.
    fn sync_alerts(&self, prefix: &str, want: Vec<(String, &str, &str, String, String)>) {
        let now = now_ms();
        let cd = self.cooldown_ms();
        let mut toasts = Vec::new();
        let mut changed = false;
        {
            let mut a = lock(&self.alerts);
            let keys: Vec<String> = want.iter().map(|w| w.0.clone()).collect();
            for (key, sev, src, title, detail) in want {
                let r = a.raise(&key, sev, src, title.clone(), detail.clone(), now, cd);
                changed |= r.changed;
                if r.notify {
                    toasts.push((title, detail));
                }
            }
            changed |= a.resolve_prefix_except(prefix, &keys, now);
        }
        for (t, d) in toasts {
            let _ = self.toast(&t, &d);
        }
        if changed {
            self.alerts_changed();
        }
    }

    fn alerts_changed(&self) {
        let (n, crit) = {
            let a = lock(&self.alerts);
            let n = a.active().count();
            let crit = a.active().any(|x| x.severity == "critical");
            (n, crit)
        };
        if let Some(tray) = self.app.tray_by_id("main") {
            let tip = if n == 0 {
                "Fleet: всё в порядке".to_string()
            } else {
                format!("Fleet: активных алертов {n}")
            };
            let _ = tray.set_tooltip(Some(tip));
            let color = if n == 0 {
                [0x3f, 0xb7, 0x72]
            } else if crit {
                [0xef, 0x5a, 0x4a]
            } else {
                [0xe8, 0x83, 0x2a]
            };
            let _ = tray.set_icon(Some(dot_icon(color)));
        }
        self.emit("alerts", ());
    }
}

/// A 32×32 status dot for the tray.
fn dot_icon(rgb: [u8; 3]) -> tauri::image::Image<'static> {
    const N: u32 = 32;
    let mut px = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let a = (13.0 - d).clamp(0.0, 1.0);
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (a * 255.0) as u8]);
        }
    }
    tauri::image::Image::new_owned(px, N, N)
}

// ---------- core listener ----------

struct Listener(Arc<Shared>);

impl CoreListener for Listener {
    fn on_state(&self, c: StateChange) {
        {
            let mut live = lock(&self.0.live);
            let l = live.entry(c.server_id.clone()).or_default();
            let st = format!("{:?}", c.state);
            if l.state != st {
                l.state = st;
                l.state_since_ms = now_ms();
            }
            l.failure = c.failure;
        }
        self.0.emit("servers", ());
    }

    fn on_host_key(&self, p: HostKeyPrompt) {
        self.0.emit("host-key", HostKeyDto::from(p));
    }

    fn on_event(&self, e: AgentEventRow) {
        let Some(a) = e.alert else { return };
        let key = format!("agent:{}:{}:{}", e.server_id, a.rule_id, a.subject);
        if a.cleared {
            if lock(&self.0.alerts).resolve(&key, now_ms()) {
                self.0.alerts_changed();
            }
            return;
        }
        let sev = match a.severity {
            Some(fleet_core_ffi::AlertSeverity::Critical) => "critical",
            Some(fleet_core_ffi::AlertSeverity::Warning) => "warning",
            _ => "info",
        };
        let title = format!("Агент: {}", a.rule_id);
        let r = lock(&self.0.alerts).raise(
            &key,
            sev,
            "agent",
            title.clone(),
            a.subject.clone(),
            now_ms(),
            self.0.cooldown_ms(),
        );
        if r.notify {
            let _ = self.0.toast(&title, &a.subject);
        }
        if r.changed {
            self.0.alerts_changed();
        }
    }

    fn on_resync(&self) {
        self.0.emit("servers", ());
    }

    fn on_metrics(&self, row: ServerMetricsRow) {
        let threshold = lock(&self.0.config).alerts.cpu_pct;
        let now = now_ms();
        {
            let mut live = lock(&self.0.live);
            let l = live.entry(row.server_id.clone()).or_default();
            l.last_metrics_ms = now;
            l.cpu = row.cpu_percent;
            l.mem = row.mem_percent;
            l.disk = row.disk_percent;
            match row.cpu_percent {
                Some(c) if c as f64 > threshold => {
                    l.cpu_high_since.get_or_insert(now);
                }
                _ => l.cpu_high_since = None,
            }
            l.history.push_back((
                row.time_ms,
                row.cpu_percent,
                row.mem_percent,
                row.disk_percent,
            ));
            while l.history.len() > HISTORY {
                l.history.pop_front();
            }
        }
        self.0.emit(
            "metrics",
            MetricsDto {
                id: row.server_id,
                cpu: row.cpu_percent,
                mem: row.mem_percent,
                disk: row.disk_percent,
                time_ms: row.time_ms,
            },
        );
    }
}

// ---------- DTOs ----------

#[derive(Serialize, Clone)]
struct MetricsDto {
    id: String,
    cpu: Option<f32>,
    mem: Option<f32>,
    disk: Option<f32>,
    time_ms: u64,
}

#[derive(Serialize, Clone)]
struct HostKeyDto {
    server_id: String,
    algorithm: String,
    fingerprint: String,
    target_unpinned: bool,
    jumps: Vec<String>,
}

impl From<HostKeyPrompt> for HostKeyDto {
    fn from(p: HostKeyPrompt) -> Self {
        Self {
            server_id: p.server_id,
            algorithm: p.algorithm,
            fingerprint: p.fingerprint,
            target_unpinned: p.target_unpinned,
            jumps: p.jumps.into_iter().map(|j| j.fingerprint).collect(),
        }
    }
}

#[derive(Serialize, Clone)]
struct ServerDto {
    id: String,
    name: String,
    host: String,
    port: u16,
    user: String,
    state: String,
    failure: Option<String>,
    state_since_ms: u64,
    host_key_pinned: bool,
    agent_pinned: bool,
    cpu: Option<f32>,
    mem: Option<f32>,
    disk: Option<f32>,
    last_metrics_ms: u64,
}

#[derive(Serialize, Clone)]
struct StatusDto {
    phase: String,
    error: Option<String>,
    config_error: Option<String>,
    fleet_name: Option<String>,
    ssh_public_key: Option<String>,
    data_dir: String,
    config_path: String,
    default_deb: String,
    active_alerts: usize,
}

#[derive(Serialize, Clone)]
struct HealthDto {
    agent_version: String,
    proto_version: u16,
    uptime_s: u64,
    gate_rss_bytes: u64,
    exec_rss_bytes: u64,
    roster_version: u64,
    policy_version: u64,
}

impl From<fleet_core_ffi::AgentHealthRow> for HealthDto {
    fn from(h: fleet_core_ffi::AgentHealthRow) -> Self {
        Self {
            agent_version: h.agent_version,
            proto_version: h.proto_version,
            uptime_s: h.uptime_s,
            gate_rss_bytes: h.gate_rss_bytes,
            exec_rss_bytes: h.exec_rss_bytes,
            roster_version: h.roster_version,
            policy_version: h.policy_version,
        }
    }
}

#[derive(Serialize, Clone)]
struct SystemInfoDto {
    hostname: String,
    os_id: String,
    os_version: String,
    kernel: String,
    arch: String,
    cpu_count: u32,
    mem_total_bytes: u64,
    uptime_s: u64,
}

#[derive(Serialize, Clone)]
struct ContainerDto {
    id: String,
    name: String,
    image: String,
    state: String,
    status: String,
    created_ms: u64,
    ports: Vec<String>,
    compose_project: Option<String>,
}

#[derive(Serialize, Clone)]
struct JournalEntryDto {
    time_us: u64,
    priority: u8,
    unit: Option<String>,
    identifier: Option<String>,
    pid: Option<u32>,
    message: String,
}

#[derive(Serialize, Clone)]
struct JournalDto {
    entries: Vec<JournalEntryDto>,
    cursor: Option<String>,
}

impl From<JournalPageRow> for JournalDto {
    fn from(j: JournalPageRow) -> Self {
        Self {
            entries: j
                .entries
                .into_iter()
                .map(|e| JournalEntryDto {
                    time_us: e.time_us,
                    priority: e.priority,
                    unit: e.unit,
                    identifier: e.identifier,
                    pid: e.pid,
                    message: e.message,
                })
                .collect(),
            cursor: j.cursor,
        }
    }
}

fn stream_status(s: StreamStatus) -> String {
    match s {
        StreamStatus::Live => "live".into(),
        StreamStatus::Reconnecting => "reconnecting".into(),
        StreamStatus::Ended { error: None } => "ended".into(),
        StreamStatus::Ended { error: Some(e) } => format!("ended: {e}"),
    }
}

// ---------- sinks ----------

struct DetailSink(Arc<Shared>);

#[derive(Serialize, Clone)]
struct SeriesDto {
    id: u16,
    name: String,
    unit: String,
}

#[derive(Serialize, Clone)]
struct SampleDto {
    time_ms: u64,
    values: Vec<(u16, f32)>,
}

impl MetricsSink for DetailSink {
    fn on_catalog(&self, series: Vec<MetricSeriesRow>) {
        let v: Vec<SeriesDto> = series
            .into_iter()
            .map(|s| SeriesDto {
                id: s.id,
                name: s.name,
                unit: format!("{:?}", s.unit),
            })
            .collect();
        self.0.emit("detail-catalog", v);
    }
    fn on_sample(&self, s: MetricsSampleRow) {
        self.0.emit(
            "detail-sample",
            SampleDto {
                time_ms: s.time_ms,
                values: s.values.into_iter().map(|v| (v.id, v.value)).collect(),
            },
        );
    }
    fn on_status(&self, st: StreamStatus) {
        self.0.emit("detail-status", stream_status(st));
    }
}

struct LogSink(Arc<Shared>);

impl JournalSink for LogSink {
    fn on_entries(&self, page: JournalPageRow) {
        self.0.emit("journal-entries", JournalDto::from(page));
    }
    fn on_status(&self, st: StreamStatus) {
        self.0.emit("journal-status", stream_status(st));
    }
}

struct InstallSink(Arc<Shared>);

#[derive(Serialize, Clone)]
struct ProgressDto {
    step: String,
    done: u64,
    total: u64,
}

#[derive(Serialize, Clone)]
struct ArtifactDto {
    name: String,
    blake3: String,
    arch: String,
}

impl InstallListener for InstallSink {
    fn on_progress(&self, p: InstallProgress) {
        self.0.emit(
            "install-progress",
            ProgressDto {
                step: format!("{:?}", p.step),
                done: p.done_bytes,
                total: p.total_bytes,
            },
        );
    }
    fn on_artifact(&self, a: InstallArtifact) {
        self.0.emit(
            "install-artifact",
            ArtifactDto {
                name: a.name,
                blake3: a.blake3,
                arch: a.arch,
            },
        );
    }
}

// ---------- commands ----------

type S<'a> = tauri::State<'a, Arc<Shared>>;

#[tauri::command]
fn status(s: S<'_>) -> StatusDto {
    let core = s.core.clone();
    StatusDto {
        phase: lock(&s.phase).clone(),
        error: lock(&s.error).clone(),
        config_error: s.config_error.clone(),
        fleet_name: core.as_ref().and_then(|c| c.fleet_name()),
        ssh_public_key: core.as_ref().and_then(|c| c.ssh_public_key().ok()),
        data_dir: s.paths.data.to_string_lossy().into_owned(),
        config_path: s.paths.config.to_string_lossy().into_owned(),
        default_deb: default_deb(&s.paths.root),
        active_alerts: lock(&s.alerts).active().count(),
    }
}

fn server_dtos(s: &Shared) -> Result<Vec<ServerDto>, String> {
    let rows = s.core()?.list_servers().map_err(err)?;
    let live = lock(&s.live);
    Ok(rows
        .into_iter()
        .map(|r| {
            let l = live.get(&r.id);
            ServerDto {
                state: l
                    .map(|l| l.state.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{:?}", r.state)),
                failure: l.and_then(|l| l.failure.clone()),
                state_since_ms: l.map_or(0, |l| l.state_since_ms),
                cpu: l.and_then(|l| l.cpu).or(r.cpu_percent),
                mem: l.and_then(|l| l.mem).or(r.mem_percent),
                disk: l.and_then(|l| l.disk).or(r.disk_percent),
                last_metrics_ms: l.map_or(0, |l| l.last_metrics_ms),
                id: r.id,
                name: r.name,
                host: r.host,
                port: r.port,
                user: r.user,
                host_key_pinned: r.host_key_pinned,
                agent_pinned: r.agent_pinned,
            }
        })
        .collect())
}

#[tauri::command]
fn servers(s: S<'_>) -> Result<Vec<ServerDto>, String> {
    server_dtos(&s)
}

#[tauri::command]
fn history(s: S<'_>, id: String) -> Vec<(u64, Option<f32>, Option<f32>, Option<f32>)> {
    lock(&s.live)
        .get(&id)
        .map(|l| l.history.iter().copied().collect())
        .unwrap_or_default()
}

#[tauri::command]
fn add_server(
    s: S<'_>,
    name: String,
    host: String,
    port: u16,
    user: String,
) -> Result<String, String> {
    let row = s
        .core()?
        .add_server(fleet_core_ffi::NewServer {
            name,
            host,
            port,
            user,
            group_id: None,
            tags: vec![],
            proxy_jump: None,
        })
        .map_err(err)?;
    s.emit("servers", ());
    Ok(row.id)
}

#[tauri::command]
fn remove_server(s: S<'_>, id: String) -> Result<(), String> {
    s.core()?.remove_server(id.clone()).map_err(err)?;
    lock(&s.live).remove(&id);
    s.emit("servers", ());
    Ok(())
}

#[tauri::command]
fn reconnect(s: S<'_>, id: String) -> Result<(), String> {
    s.core()?.reconnect(id).map_err(err)
}

/// `with_auth`: authenticate with the app's key (fails with "SSH key
/// refused" if it isn't in authorized_keys yet); otherwise host key only.
#[tauri::command]
async fn probe_host_key(s: S<'_>, id: String, with_auth: bool) -> Result<HostKeyDto, String> {
    let core = s.core()?;
    let p = if with_auth {
        core.probe_host_key(id).await
    } else {
        core.probe_host_key_only(id).await
    };
    p.map(Into::into).map_err(err)
}

#[tauri::command]
fn accept_host_key(
    s: S<'_>,
    id: String,
    fingerprint: String,
    jumps: Vec<String>,
) -> Result<(), String> {
    s.core()?
        .accept_host_key(id, fingerprint, jumps)
        .map_err(err)?;
    s.emit("servers", ());
    Ok(())
}

#[tauri::command]
fn reject_host_key(s: S<'_>, id: String) -> Result<(), String> {
    s.core()?.reject_host_key(id).map_err(err)
}

/// Always Agent-only (never Managed): Fleet does not touch firewall,
/// bans or authorized_keys on this server.
#[tauri::command]
async fn install_agent(
    s: S<'_>,
    id: String,
    artifact: String,
    password: Option<String>,
    add_key: bool,
    existing: String,
) -> Result<HealthDto, String> {
    let core = s.core()?;
    let shared: Arc<Shared> = s.inner().clone();
    let existing = match existing.as_str() {
        "adopt" => ExistingAgentAction::Adopt,
        "replace" => ExistingAgentAction::Replace,
        "reinstall" => ExistingAgentAction::Reinstall,
        _ => ExistingAgentAction::Ask,
    };
    let password = password.filter(|p| !p.is_empty()).map(String::into_bytes);
    let r = core
        .install_agent(
            id,
            None,
            Some(artifact),
            None,
            HashMap::new(),
            SecurityModeArg::AgentOnly,
            password,
            add_key,
            existing,
            Box::new(InstallSink(shared)),
        )
        .await;
    s.emit("servers", ());
    match r {
        Ok(h) => Ok(h.into()),
        Err(FleetError::ExistingAgent {
            same_fleet,
            exec_active,
            ..
        }) => Err(format!(
            "EXISTING_AGENT: an agent is already installed (running: {exec_active}, this \
             fleet: {same_fleet}). Choose Adopt (same fleet) or Replace."
        )),
        Err(e) => Err(err(e)),
    }
}

#[tauri::command]
fn open_data_dir(s: S<'_>) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(&s.paths.data)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Adds the app's SSH key to `user@host`'s authorized_keys using the
/// user's own OpenSSH setup; returns the server's host key fingerprints.
#[tauri::command]
async fn push_app_key(
    s: S<'_>,
    host: String,
    port: u16,
    user: String,
    key_path: Option<String>,
) -> Result<pushkey::PushResult, String> {
    let line = s.core()?.ssh_public_key().map_err(err)?;
    tauri::async_runtime::spawn_blocking(move || {
        pushkey::push(&host, port, &user, key_path.as_deref(), &line)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn pick_key(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut d = app.dialog().file();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        let dir = PathBuf::from(home).join(".ssh");
        if dir.exists() {
            d = d.set_directory(dir);
        }
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    d.pick_file(move |f| {
        let _ = tx.send(f.map(|f| f.to_string()));
    });
    rx.await.ok().flatten()
}

#[tauri::command]
async fn pick_deb(app: tauri::AppHandle, start: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let mut d = app
        .dialog()
        .file()
        .add_filter("Debian package / agent binary", &["deb"])
        .add_filter("All files", &["*"]);
    let p = Path::new(&start);
    let dir = if p.is_dir() { Some(p) } else { p.parent() };
    if let Some(dir) = dir.filter(|d| d.exists()) {
        d = d.set_directory(dir);
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    d.pick_file(move |f| {
        let _ = tx.send(f.map(|f| f.to_string()));
    });
    rx.await.ok().flatten()
}

#[tauri::command]
async fn system_info(s: S<'_>, id: String) -> Result<SystemInfoDto, String> {
    let v = s.core()?.system_info(id).await.map_err(err)?;
    Ok(SystemInfoDto {
        hostname: v.hostname,
        os_id: v.os_id,
        os_version: v.os_version,
        kernel: v.kernel,
        arch: v.arch,
        cpu_count: v.cpu_count,
        mem_total_bytes: v.mem_total_bytes,
        uptime_s: v.uptime_s,
    })
}

#[tauri::command]
async fn agent_health(s: S<'_>, id: String) -> Result<HealthDto, String> {
    Ok(s.core()?.agent_health(id).await.map_err(err)?.into())
}

#[tauri::command]
async fn docker_containers(s: S<'_>, id: String) -> Result<Vec<ContainerDto>, String> {
    let v = s.core()?.docker_containers(id, true).await.map_err(err)?;
    Ok(v.into_iter()
        .map(|c| ContainerDto {
            id: c.id,
            name: c.name,
            image: c.image,
            state: c.state,
            status: c.status,
            created_ms: c.created_ms,
            ports: c.ports,
            compose_project: c.compose_project,
        })
        .collect())
}

fn priority(p: Option<u8>) -> Option<LogPriority> {
    Some(match p? {
        0 => LogPriority::Emerg,
        1 => LogPriority::Alert,
        2 => LogPriority::Crit,
        3 => LogPriority::Err,
        4 => LogPriority::Warning,
        5 => LogPriority::Notice,
        6 => LogPriority::Info,
        _ => LogPriority::Debug,
    })
}

fn journal_args(units: Vec<String>, grep: Option<String>, prio: Option<u8>, limit: u32) -> JournalQueryArgs {
    JournalQueryArgs {
        units: units.into_iter().filter(|u| !u.trim().is_empty()).collect(),
        priority: priority(prio),
        since_ms: None,
        until_ms: None,
        grep: grep.filter(|g| !g.is_empty()),
        after_cursor: None,
        limit: limit.clamp(1, 10_000),
    }
}

#[tauri::command]
async fn journal(
    s: S<'_>,
    id: String,
    units: Vec<String>,
    grep: Option<String>,
    priority: Option<u8>,
    limit: u32,
) -> Result<JournalDto, String> {
    let q = journal_args(units, grep, priority, limit);
    Ok(s.core()?.journal_query(id, q).await.map_err(err)?.into())
}

#[tauri::command]
fn follow_journal(
    s: S<'_>,
    id: String,
    units: Vec<String>,
    grep: Option<String>,
    priority: Option<u8>,
) -> Result<(), String> {
    let q = journal_args(units, grep, priority, 500);
    let h = s
        .core()?
        .follow_journal(id, q, Box::new(LogSink(s.inner().clone())))
        .map_err(err)?;
    if let Some(old) = lock(&s.journal_stream).replace(h) {
        old.cancel();
    }
    Ok(())
}

#[tauri::command]
fn stop_journal(s: S<'_>) {
    if let Some(h) = lock(&s.journal_stream).take() {
        h.cancel();
    }
}

#[tauri::command]
fn follow_metrics(s: S<'_>, id: String) -> Result<(), String> {
    let h = s
        .core()?
        .subscribe_metrics(id, true, Box::new(DetailSink(s.inner().clone())))
        .map_err(err)?;
    if let Some(old) = lock(&s.detail_stream).replace(h) {
        old.cancel();
    }
    Ok(())
}

#[tauri::command]
fn stop_metrics(s: S<'_>) {
    if let Some(h) = lock(&s.detail_stream).take() {
        h.cancel();
    }
}

#[tauri::command]
fn alerts_list(s: S<'_>) -> Vec<alerts::Alert> {
    lock(&s.alerts).list.clone()
}

#[tauri::command]
fn alerts_ack(s: S<'_>) {
    lock(&s.alerts).ack_all();
    s.alerts_changed();
}

#[tauri::command]
fn alerts_clear(s: S<'_>) {
    lock(&s.alerts).clear_resolved();
    s.alerts_changed();
}

#[tauri::command]
fn alerts_test(s: S<'_>) -> Result<(), String> {
    s.toast("Fleet", "Тестовое уведомление: алерты работают")
}

#[tauri::command]
fn get_config(s: S<'_>) -> Config {
    lock(&s.config).clone()
}

#[tauri::command]
async fn save_config(s: S<'_>, config: Config) -> Result<(), String> {
    config.save(&s.paths.config)?;
    *lock(&s.config) = config;
    let sh = s.inner().clone();
    do_fetch(&sh).await;
    Ok(())
}

#[tauri::command]
fn do_snapshot(s: S<'_>) -> Option<serde_json::Value> {
    lock(&s.do_snapshot).clone()
}

#[tauri::command]
async fn do_refresh(s: S<'_>) -> Result<Option<serde_json::Value>, String> {
    let sh = s.inner().clone();
    do_fetch(&sh).await;
    Ok(lock(&s.do_snapshot).clone())
}

#[derive(Serialize)]
struct DoListDto {
    droplets: Vec<fleet_do::DropletInfo>,
    databases: Vec<fleet_do::DbInfo>,
}

/// Droplets and databases on the account, for picking ids in Settings.
#[tauri::command]
async fn do_list(token: String) -> Result<DoListDto, String> {
    let c = fleet_do::DoClient::new(token).map_err(|e| e.to_string())?;
    let droplets = c.list_droplets().await.map_err(|e| e.to_string())?;
    let databases = c.list_databases().await.map_err(|e| e.to_string())?;
    Ok(DoListDto {
        droplets,
        databases,
    })
}

// ---------- background loops ----------

async fn do_fetch(s: &Arc<Shared>) {
    let _g = s.do_fetching.lock().await;
    let cfg = lock(&s.config).clone();
    if cfg.digitalocean.token.trim().is_empty() {
        return;
    }
    let client = match fleet_do::DoClient::new(cfg.digitalocean.token.trim().to_string()) {
        Ok(c) => c,
        Err(e) => {
            let snap = serde_json::json!({
                "fetched_at_unix": now_ms() / 1000,
                "errors": [e.to_string()],
            });
            *lock(&s.do_snapshot) = Some(snap.clone());
            s.emit("do-snapshot", snap);
            return;
        }
    };
    let droplet = (cfg.digitalocean.droplet_id != 0).then_some(cfg.digitalocean.droplet_id);
    let db = cfg.digitalocean.database_id.trim();
    let db = (!db.is_empty()).then_some(db);
    let snap = client.snapshot(droplet, db).await;
    evaluate_do(s, &cfg, &snap);
    let v = serde_json::to_value(&snap).unwrap_or_default();
    *lock(&s.do_snapshot) = Some(v.clone());
    s.emit("do-snapshot", v);
}

fn agent_fresh(s: &Shared) -> bool {
    let now = now_ms();
    lock(&s.live)
        .values()
        .any(|l| l.state == "Ready" && now.saturating_sub(l.last_metrics_ms) < 120_000)
}

fn evaluate_do(s: &Shared, cfg: &Config, snap: &fleet_do::DoSnapshot) {
    let a = &cfg.alerts;
    let mut want: Vec<(String, &str, &str, String, String)> = Vec::new();
    if let Some(d) = &snap.droplet {
        if d.status != "active" {
            want.push((
                "do:droplet".into(),
                "critical",
                "do",
                format!("Droplet {}: {}", d.name, d.status),
                "DigitalOcean reports the droplet is not active".into(),
            ));
        }
    }
    if let Some(db) = &snap.db {
        if db.status != "online" {
            want.push((
                "do:db".into(),
                "critical",
                "do",
                format!("База {}: {}", db.name, db.status),
                format!("{} {} — статус не online", db.engine, db.version),
            ));
        }
    }
    if let Some(m) = &snap.db_metrics {
        if let Some(c) = m.cpu_pct.filter(|c| *c > a.db_cpu_pct) {
            want.push((
                "do:db-cpu".into(),
                "warning",
                "do",
                format!("CPU базы {c:.0}%"),
                format!("порог {:.0}%", a.db_cpu_pct),
            ));
        }
        if let Some(d) = m.disk_pct.filter(|d| *d > a.db_disk_pct) {
            want.push((
                "do:db-disk".into(),
                "critical",
                "do",
                format!("Диск базы {d:.0}%"),
                format!("порог {:.0}%", a.db_disk_pct),
            ));
        }
    }
    // Droplet metrics from DO only stand in when the agent is silent.
    if !agent_fresh(s)
        && let Some(m) = &snap.droplet_metrics
    {
        if let Some(c) = m.cpu_pct.filter(|c| *c > a.cpu_pct) {
            want.push((
                "do:cpu".into(),
                "warning",
                "do",
                format!("CPU дроплета {c:.0}% (DO)"),
                format!("порог {:.0}%", a.cpu_pct),
            ));
        }
        if let Some(v) = m.mem_pct.filter(|v| *v > a.mem_pct) {
            want.push((
                "do:mem".into(),
                "warning",
                "do",
                format!("RAM дроплета {v:.0}% (DO)"),
                format!("порог {:.0}%", a.mem_pct),
            ));
        }
        if let Some(v) = m.disk_used_pct.filter(|v| *v > a.disk_pct) {
            want.push((
                "do:disk".into(),
                "critical",
                "do",
                format!("Диск дроплета {v:.0}% (DO)"),
                format!("порог {:.0}%", a.disk_pct),
            ));
        }
    }
    if snap.droplet.is_none() && snap.db.is_none() && !snap.errors.is_empty() {
        want.push((
            "do:api".into(),
            "warning",
            "do",
            "DigitalOcean API недоступен".into(),
            snap.errors.join("; "),
        ));
    }
    s.sync_alerts("do:", want);
}

fn evaluate_agents(s: &Shared) {
    let Ok(servers) = server_dtos(s) else { return };
    let a = lock(&s.config).alerts.clone();
    let now = now_ms();
    let mut want: Vec<(String, &str, &str, String, String)> = Vec::new();
    let live = lock(&s.live);
    for srv in servers.iter().filter(|x| x.agent_pinned) {
        let l = live.get(&srv.id);
        let state = l.map(|l| l.state.as_str()).unwrap_or("");
        let since = l.map_or(0, |l| l.state_since_ms).max(app_start_ms());
        if state != "Ready" && now.saturating_sub(since) > a.unreachable_minutes * 60_000 {
            want.push((
                format!("srv:{}:down", srv.id),
                "critical",
                "agent",
                format!("{} недоступен", srv.name),
                l.and_then(|l| l.failure.clone())
                    .unwrap_or_else(|| format!("состояние: {}", if state.is_empty() { "нет связи" } else { state })),
            ));
            continue;
        }
        let Some(l) = l.filter(|l| now.saturating_sub(l.last_metrics_ms) < 120_000) else {
            continue;
        };
        if let Some(t0) = l.cpu_high_since
            && now.saturating_sub(t0) >= a.cpu_for_minutes * 60_000
        {
            want.push((
                format!("srv:{}:cpu", srv.id),
                "warning",
                "agent",
                format!("{}: CPU {:.0}%", srv.name, l.cpu.unwrap_or(0.0)),
                format!("выше {:.0}% дольше {} мин", a.cpu_pct, a.cpu_for_minutes),
            ));
        }
        if let Some(m) = l.mem.filter(|m| *m as f64 > a.mem_pct) {
            want.push((
                format!("srv:{}:mem", srv.id),
                "warning",
                "agent",
                format!("{}: RAM {m:.0}%", srv.name),
                format!("порог {:.0}%", a.mem_pct),
            ));
        }
        if let Some(d) = l.disk.filter(|d| *d as f64 > a.disk_pct) {
            want.push((
                format!("srv:{}:disk", srv.id),
                "critical",
                "agent",
                format!("{}: диск / {d:.0}%", srv.name),
                format!("порог {:.0}%", a.disk_pct),
            ));
        }
    }
    drop(live);
    s.sync_alerts("srv:", want);
}

fn device_name() -> String {
    let host: String = std::env::var("COMPUTERNAME")
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(40)
        .collect();
    if host.is_empty() {
        "Windows".into()
    } else {
        format!("Windows {host}")
    }
}

/// First launch: create the fleet (genesis roster, this PC its only
/// device), save the recovery words to `data/recovery.txt`. Then start the
/// connection manager with device sessions.
fn bootstrap(s: Arc<Shared>) {
    let set_phase = |p: &str| {
        *lock(&s.phase) = p.into();
        s.emit("status", ());
    };
    let core = match s.core() {
        Ok(c) => c,
        Err(_) => {
            set_phase("error");
            return;
        }
    };
    if !core.is_enrolled() {
        set_phase("enrolling");
        let r = (|| -> Result<(), String> {
            let e = core
                .clone()
                .create_fleet("Fleet".into(), device_name())
                .map_err(err)?;
            let words = e.recovery_words().map_err(err)?;
            let text = format!(
                "Fleet recovery code (24 words). Keep offline; needed to recover the fleet if \
                 the keys in data/keys are lost.\n\n{}\n",
                words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| format!("{:2}. {w}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            std::fs::write(s.paths.data.join("recovery.txt"), text)
                .map_err(|e| format!("recovery.txt: {e}"))?;
            let ch = e.challenge().map_err(err)?;
            let answers = ch
                .iter()
                .map(|&i| words.get(i as usize).cloned().unwrap_or_default())
                .collect();
            if !e.confirm_words(answers).map_err(err)? {
                return Err("recovery word check failed".into());
            }
            tauri::async_runtime::block_on(e.finish(String::new())).map_err(err)?;
            Ok(())
        })();
        if let Err(e) = r {
            *lock(&s.error) = Some(format!("enrollment: {e}"));
            set_phase("error");
            return;
        }
    }
    core.set_session_kind(SessionKind::Device);
    if let Err(e) = core.clone().start(Box::new(Listener(s.clone()))) {
        *lock(&s.error) = Some(format!("start: {}", err(e)));
        set_phase("error");
        return;
    }
    set_phase("ready");
    s.emit("servers", ());
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app_start_ms();
            let paths = paths();
            std::fs::create_dir_all(paths.data.join("keys"))?;
            let (config, config_error) = match Config::load(&paths.config) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(e)),
            };
            let keys_dir = paths.data.join("keys");
            let opened = keys::FileSigner::open(&keys_dir).and_then(|signer| {
                FleetCore::open(
                    paths.data.join("cache.sqlite").to_string_lossy().into_owned(),
                    Box::new(signer),
                    Box::new(keys::FileKeyStore::new(&keys_dir)),
                )
                .map_err(err)
            });
            let (core, error) = match opened {
                Ok(c) => (Some(c), None),
                Err(e) => (None, Some(e)),
            };
            let shared = Arc::new(Shared {
                app: app.handle().clone(),
                core,
                paths,
                phase: Mutex::new("starting".into()),
                error: Mutex::new(error),
                config: Mutex::new(config),
                config_error,
                live: Mutex::default(),
                alerts: Mutex::default(),
                do_snapshot: Mutex::new(None),
                do_fetching: tokio::sync::Mutex::new(()),
                detail_stream: Mutex::new(None),
                journal_stream: Mutex::new(None),
            });
            app.manage(shared.clone());

            // Tray.
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
            let show = MenuItem::with_id(app, "show", "Открыть Fleet", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(dot_icon([0x8d, 0x91, 0x99]))
                .tooltip("Fleet")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            let s = shared.clone();
            std::thread::Builder::new()
                .name("fleet-bootstrap".into())
                .spawn(move || {
                    bootstrap(s.clone());
                    s.alerts_changed();
                })?;

            let s = shared.clone();
            tauri::async_runtime::spawn(async move {
                // Give the first connection time before judging.
                tokio::time::sleep(Duration::from_secs(20)).await;
                loop {
                    evaluate_agents(&s);
                    tokio::time::sleep(ALERT_TICK).await;
                }
            });

            let s = shared;
            tauri::async_runtime::spawn(async move {
                loop {
                    do_fetch(&s).await;
                    let secs = lock(&s.config).digitalocean.poll_seconds.max(15);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close hides to the tray; alerts keep running. Quit from the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            servers,
            history,
            add_server,
            remove_server,
            reconnect,
            probe_host_key,
            accept_host_key,
            reject_host_key,
            install_agent,
            pick_deb,
            pick_key,
            open_data_dir,
            push_app_key,
            system_info,
            agent_health,
            docker_containers,
            journal,
            follow_journal,
            stop_journal,
            follow_metrics,
            stop_metrics,
            alerts_list,
            alerts_ack,
            alerts_clear,
            alerts_test,
            get_config,
            save_config,
            do_snapshot,
            do_refresh,
            do_list,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Fleet");
}
