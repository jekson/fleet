//! Puts the app's SSH public key into the server's authorized_keys with the
//! user's own SSH setup (Windows OpenSSH: any key type, ssh-agent, the
//! user's known_hosts). Also reads the server's host key fingerprints over
//! that already-verified channel, so the app can pin its own probe without
//! asking.

use serde::Serialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize, Clone)]
pub struct PushResult {
    /// `SHA256:…` of every /etc/ssh/ssh_host_*_key.pub.
    pub host_fingerprints: Vec<String>,
}

fn ssh_exe() -> PathBuf {
    let sys = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join(r"System32\OpenSSH\ssh.exe");
    if sys.exists() { sys } else { PathBuf::from("ssh") }
}

/// The public key line goes inside single quotes on the remote shell.
fn valid_line(l: &str) -> bool {
    !l.is_empty()
        && l.len() < 1024
        && l.chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=@._ -".contains(c))
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && !h.starts_with('-')
        && h.len() <= 253
        && h.chars().all(|c| c.is_ascii_alphanumeric() || ".:-[]".contains(c))
}

fn valid_user(u: &str) -> bool {
    !u.is_empty()
        && !u.starts_with('-')
        && u.len() <= 32
        && u.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

fn hint(stderr: &str) -> String {
    let s = stderr.trim();
    let h = if s.contains("Host key verification failed")
        || s.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || s.contains("No ED25519 host key is known")
        || s.contains("host key for")
    {
        "\nПодсказка: хост неизвестен или его ключ изменился — подключитесь один раз из терминала \
         (ssh -p PORT user@host), чтобы обновить known_hosts."
    } else if s.contains("Permission denied") || s.contains("passphrase") {
        "\nПодсказка: ключ не принят или защищён паролем (BatchMode не спрашивает пароль) — \
         загрузите ключ в ssh-agent (ssh-add) или используйте ручную команду."
    } else if s.contains("timed out") || s.contains("Connection refused") {
        "\nПодсказка: сервер недоступен по этому адресу/порту."
    } else {
        ""
    };
    format!("ssh: {s}{h}")
}

pub fn push(
    host: &str,
    port: u16,
    user: &str,
    key_path: Option<&str>,
    line: &str,
) -> Result<PushResult, String> {
    if !valid_line(line) {
        return Err("app public key has unexpected characters".into());
    }
    if !valid_host(host) || !valid_user(user) {
        return Err("invalid host or user".into());
    }
    let remote = format!(
        "umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; \
         grep -qxF '{line}' ~/.ssh/authorized_keys || echo '{line}' >> ~/.ssh/authorized_keys; \
         echo FLEET-KEY-OK; for f in /etc/ssh/ssh_host_*_key.pub; do ssh-keygen -lf \"$f\"; done"
    );
    let mut cmd = Command::new(ssh_exe());
    cmd.arg("-p").arg(port.to_string());
    if let Some(k) = key_path.map(str::trim).filter(|k| !k.is_empty()) {
        cmd.arg("-i").arg(k);
    }
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(format!("{user}@{host}"))
        .arg(remote)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| format!("ssh.exe: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("ssh: timed out after 30 s".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("ssh.exe: {e}")),
        }
    }
    let out = child.wait_with_output().map_err(|e| format!("ssh.exe: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !stdout.contains("FLEET-KEY-OK") {
        return Err(hint(&String::from_utf8_lossy(&out.stderr)));
    }
    let host_fingerprints = stdout
        .split_whitespace()
        .filter(|w| w.starts_with("SHA256:"))
        .map(str::to_string)
        .collect();
    Ok(PushResult { host_fingerprints })
}
