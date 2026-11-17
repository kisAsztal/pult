//! Keep the machine awake while a local session has a Claude that is `working`: no sleep on idle and none on
//! lid close. It lets go when Claude asks a question, is done or failed, and the normal power rules apply again.
//! Linux uses `systemd-inhibit`, macOS `caffeinate`. The inhibitor child waits on the daemon's pid, so it
//! disappears together with the daemon, however the daemon exits.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};
use std::{fs, thread};

use serde_json::Value;

/// A "working" Claude that sends no hook for this long is considered stuck (killed, crashed).
const STALE: Duration = Duration::from_secs(30 * 60);

pub(crate) fn keeper(alive: impl Fn(u32) -> bool) {
    let mut held: Option<Child> = None;
    loop {
        let want = working_sessions().into_iter().any(&alive);
        if let Some(c) = held.as_mut() {
            // the inhibitor exited on its own: try again on the next round
            if !matches!(c.try_wait(), Ok(None)) {
                held = None;
            }
        }
        match (want, held.is_some()) {
            (true, false) => {
                held = inhibit();
                if held.is_some() {
                    eprintln!("keep-awake: on (a Claude is working)");
                }
            }
            (false, true) => {
                if let Some(mut c) = held.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                eprintln!("keep-awake: off");
            }
            _ => {}
        }
        thread::sleep(Duration::from_secs(5));
    }
}

/// Pult sessions whose latest Claude state is a fresh `working`.
fn working_sessions() -> Vec<u32> {
    let now = SystemTime::now();
    let Ok(dir) = fs::read_dir(crate::state_dir()) else {
        return vec![];
    };
    let mut latest: HashMap<u32, (u64, bool)> = HashMap::new();
    for e in dir.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age < STALE);
        let Some(v) = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let Some(id) = v["pult"].as_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let since = v["since"].as_u64().unwrap_or(0);
        let working = fresh && v["status"].as_str() == Some("working");
        // several Claudes can run in one session over time: the latest one counts
        if latest.get(&id).is_none_or(|(s, _)| since >= *s) {
            latest.insert(id, (since, working));
        }
    }
    latest.into_iter().filter(|(_, (_, w))| *w).map(|(id, _)| id).collect()
}

#[cfg(target_os = "linux")]
fn inhibit() -> Option<Child> {
    let pid = std::process::id().to_string();
    Command::new("systemd-inhibit")
        .args([
            "--what=sleep:idle:handle-lid-switch",
            "--who=pult",
            "--why=Claude is working",
            "--mode=block",
        ])
        .args(["tail", "--pid", &pid, "-f", "/dev/null"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| eprintln!("keep-awake: cannot run systemd-inhibit: {e}"))
        .ok()
}

#[cfg(target_os = "macos")]
fn inhibit() -> Option<Child> {
    Command::new("caffeinate")
        .args(["-i", "-s", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| eprintln!("keep-awake: cannot run caffeinate: {e}"))
        .ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn inhibit() -> Option<Child> {
    None
}
