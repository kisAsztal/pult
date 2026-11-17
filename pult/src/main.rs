//! `pult` CLI.
//!
//! - `pult hook`: called by Claude Code hooks. Turns the event into a status, publishes it to the terminal as an
//!   OSC 1337 `SetUserVar` (the terminal colours the tab from it) and writes a state file per Claude session.
//! - `pult daemon` (pultd): persistent terminal sessions (`daemon.rs`). `pult new / attach / ls / kill` is the
//!   client side (`client.rs`).

mod awake;
mod client;
mod daemon;
mod hosts;
mod proto;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Lock that keeps working after another thread panicked while holding it: one failed connection
/// must not take the whole daemon down.
pub(crate) trait LockExt<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_ok(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag_value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    match args.first().map(String::as_str) {
        Some("hook") => hook(),
        Some("daemon") => daemon::run(),
        Some("new") => {
            let (mut name, mut cwd, mut host, mut run, mut argv) = (None, None, None, None, Vec::new());
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--name" | "-n" => name = it.next().cloned(),
                    "--cwd" => cwd = it.next().cloned(),
                    "--host" | "-H" => host = it.next().cloned(),
                    "--run" | "-r" => run = it.next().cloned(),
                    "--" => argv = it.by_ref().cloned().collect(),
                    n if !n.starts_with('-') && name.is_none() => name = Some(n.to_string()),
                    _ => usage(),
                }
            }
            client::new(host, name, cwd, run, argv)
        }
        Some("attach" | "a") if args.len() == 2 => client::attach(&args[1]),
        Some("kill") if args.len() == 2 => client::kill(&args[1]),
        Some("ls") => client::ls(
            args.iter().any(|a| a == "--json"),
            args.iter().any(|a| a == "--all" || a == "-a"),
        ),
        Some("dirs") => client::dirs(flag_value("--host")),
        Some("paste-image") => client::paste_image(flag_value("--host")),
        Some("claude") => claude_ls(),
        Some("set") if (2..=4).contains(&args.len()) => {
            let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
            emit(&args[1], now(), arg(2), arg(3))
        }
        Some("--version" | "-V" | "version") => println!("pult {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h" | "help") => print!("{USAGE}"),
        _ => usage(),
    }
}

const USAGE: &str = "usage:
  pult new [name] [--host HOST] [--cwd DIR] [--run CMD] [-- command...]
                         new persistent session (a shell, or the command), here or on another host;
                         --run types CMD into the shell (the shell stays after it exits);
                         inside a pult session it opens a new WezTerm tab instead
  pult attach <id|name|host:id>   attach to a running session
  pult ls [--all] [--json]        sessions; --all: every host in ~/.config/pult/hosts
  pult kill <id|name|host:id>     stop a session
  pult dirs [--host HOST]         project directories (for the session picker)
  pult paste-image --host HOST    upload the clipboard image to HOST and print its remote path
  pult daemon                     run pultd (`pult new` starts it on demand)
  pult claude                     Claude sessions on this machine (hook state files)
  pult hook                       Claude Code hook (stdin: hook JSON)
  pult set <status> [ask] [kind]  set the status by hand (testing): working|waiting|done|error|idle
  pult --version | --help
";

fn usage() -> ! {
    eprint!("{USAGE}");
    std::process::exit(2);
}

/// Hook event → status. `None`: the event does not change the status; `Some("")`: the session ended.
fn status_for(event: &str, v: &Value) -> Option<&'static str> {
    Some(match event {
        "SessionStart" => "idle",
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied"
        | "PreCompact" | "SubagentStart" => "working",
        "PermissionRequest" | "Elicitation" => "waiting",
        "Notification" => match v["notification_type"].as_str().unwrap_or("") {
            "permission_prompt" | "elicitation_dialog" => "waiting",
            "idle_prompt" => "done",
            _ => return None,
        },
        "Stop" => "done",
        "StopFailure" => "error",
        "SessionEnd" => "",
        _ => return None,
    })
}

fn hook() {
    // A hook must never get in Claude's way: swallow every error, always exit 0, never print to stdout.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let Ok(v) = serde_json::from_str::<Value>(&input) else {
        return;
    };
    let event = v["hook_event_name"].as_str().unwrap_or("");
    let status = status_for(event, &v);
    debug_log(&v, status);
    let Some(status) = status else { return };

    let session = v["session_id"].as_str().unwrap_or("unknown").to_string();
    let file = state_dir().join(format!("{}.json", sanitize(&session)));
    let prev: Value = fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);

    // `since` restarts only when the status changes (otherwise every tool call would reset it)
    let since = if prev["status"].as_str() == Some(status) {
        prev["since"].as_u64().unwrap_or_else(now)
    } else {
        now()
    };

    // What Claude waits for, and what kind of request it is. Only a one-line, untruncated permission request can
    // be approved from a notification. The generic Notification(permission_prompt) arrives after the
    // PermissionRequest: keep the precise request we already have.
    let (ask, kind) = match ask_summary(event, &v) {
        Some(_) if event == "Notification" && prev["status"].as_str() == Some("waiting") && prev["ask"].is_string() => {
            (
                prev["ask"].as_str().map(String::from),
                prev["ask_kind"].as_str().unwrap_or("").to_string(),
            )
        }
        // the idle_prompt after "done" ("Claude is waiting for your input") is not a request
        Some(_) if event == "Notification" && status != "waiting" => (None, String::new()),
        Some((a, k)) => (Some(a), k.to_string()),
        None => (None, String::new()),
    };

    if status.is_empty() {
        let _ = fs::remove_file(&file);
    } else {
        let state = json!({
            "status": status,
            "since": since,
            "event": event,
            "cwd": v["cwd"],
            "pane": std::env::var("WEZTERM_PANE").ok(),
            "pult": std::env::var("PULT_SESSION").ok(),
            "session_id": session,
            "ask": ask,
            "ask_kind": kind,
        });
        let _ = private_dir(&state_dir());
        let _ = write_private(&file, state.to_string().as_bytes());
    }
    emit(status, since, ask.as_deref().unwrap_or(""), &kind);
}

/// Directory readable by the user only (0700); the state files hold command text.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

pub(crate) fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)
}

/// What Claude asks permission for, in short (`Bash: git push`, `Edit: src/main.rs`), and the kind of request:
/// `permission` (one line, fully visible: can be approved from a notification), `permission-long` (multi-line
/// or truncated: can only be opened), `question`, `elicitation`, `notification`.
fn ask_summary(event: &str, v: &Value) -> Option<(String, &'static str)> {
    let (s, mut kind) = match event {
        "PermissionRequest" => {
            let tool = v["tool_name"].as_str().unwrap_or("?");
            let input = &v["tool_input"];
            if tool == "AskUserQuestion" {
                let q = format!("Question: {}", input["questions"][0]["question"].as_str().unwrap_or(""));
                return Some((one_line(&q).0, "question"));
            }
            let detail = [
                "command",
                "file_path",
                "notebook_path",
                "url",
                "pattern",
                "query",
                "description",
                "prompt",
            ]
            .iter()
            .find_map(|k| input[*k].as_str())
            .unwrap_or("");
            (
                if detail.is_empty() {
                    tool.to_string()
                } else {
                    format!("{tool}: {detail}")
                },
                "permission",
            )
        }
        "Elicitation" => (v["message"].as_str()?.to_string(), "elicitation"),
        "Notification" => (v["message"].as_str()?.to_string(), "notification"),
        _ => return None,
    };
    let (out, cut) = one_line(&s);
    if cut && kind == "permission" {
        kind = "permission-long";
    }
    Some((out, kind))
}

/// The first non-empty line, at most 140 characters, without control characters; `true` if anything was cut off.
fn one_line(s: &str) -> (String, bool) {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.chars().filter(|c| !c.is_control()).take(140).collect();
    let cut = line.chars().count() > 140 || s.trim().lines().count() > 1;
    if cut {
        out.push('…');
    }
    (out, cut)
}

/// OSC 1337 SetUserVar to the tab's terminal. Claude runs hooks without a controlling terminal (`/dev/tty`
/// cannot be opened), so the tab's pts device is looked up among the parent processes.
fn emit(status: &str, since: u64, ask: &str, kind: &str) {
    let Some(mut tty) = find_tty() else { return };
    let out = user_var("pult_status", status)
        + &user_var("pult_since", &since.to_string())
        + &user_var("pult_ask", ask)
        + &user_var("pult_ask_kind", kind);
    let _ = tty.write_all(out.as_bytes());
}

pub(crate) fn user_var(k: &str, v: &str) -> String {
    format!("\x1b]1337;SetUserVar={}={}\x07", k, base64(v.as_bytes()))
}

fn find_tty() -> Option<fs::File> {
    let open = |p: &str| fs::OpenOptions::new().write(true).open(p).ok();
    open("/dev/tty").or_else(|| tty_path_from_ancestors().and_then(|p| open(&p)))
}

/// Linux: `/proc/<pid>/fd/{0,1,2}` of the ancestors, the first terminal device.
#[cfg(target_os = "linux")]
fn tty_path_from_ancestors() -> Option<String> {
    let mut pid = std::process::id();
    for _ in 0..16 {
        for fd in 0..3 {
            if let Ok(t) = fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
                let t = t.to_string_lossy().to_string();
                if t.starts_with("/dev/pts/") || (t.starts_with("/dev/tty") && t != "/dev/tty") {
                    return Some(t);
                }
            }
        }
        // /proc/<pid>/stat is "pid (comm) state ppid ...", and comm may contain spaces
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.get(stat.rfind(')')? + 2..)?;
        pid = rest.split_whitespace().nth(1)?.parse().ok()?;
        if pid <= 1 {
            return None;
        }
    }
    None
}

/// macOS and others: `ps` on the ancestors until one has a terminal.
#[cfg(not(target_os = "linux"))]
fn tty_path_from_ancestors() -> Option<String> {
    let mut pid = std::process::id();
    for _ in 0..16 {
        let out = std::process::Command::new("ps")
            .args(["-o", "tty=,ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let mut it = s.split_whitespace();
        let tty = it.next()?;
        let ppid: u32 = it.next()?.parse().ok()?;
        if tty != "??" && tty != "?" {
            return Some(if tty.starts_with("/dev/") {
                tty.to_string()
            } else {
                format!("/dev/{tty}")
            });
        }
        if ppid <= 1 {
            return None;
        }
        pid = ppid;
    }
    None
}

/// If `~/.local/state/pult/debug` exists, every hook event is logged to `hook.log`.
fn debug_log(v: &Value, status: Option<&str>) {
    let dir = state_dir().parent().map(Path::to_path_buf).unwrap_or_default();
    if !dir.join("debug").exists() {
        return;
    }
    let tty = tty_path_from_ancestors().unwrap_or_else(|| "-".into());
    let line = format!(
        "{} {} {} type={} -> {:?} tty={}\n",
        now(),
        v["session_id"].as_str().unwrap_or("?").get(..8).unwrap_or("?"),
        v["hook_event_name"].as_str().unwrap_or("?"),
        v["notification_type"].as_str().unwrap_or("-"),
        status,
        tty
    );
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join("hook.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn claude_ls() {
    let mut rows: Vec<Value> = fs::read_dir(state_dir())
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| fs::read_to_string(e.path()).ok())
                .filter_map(|s| serde_json::from_str(&s).ok())
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by_key(|r| r["since"].as_u64().unwrap_or(0));
    if rows.is_empty() {
        println!("no sessions");
    }
    let t = now();
    for r in rows {
        let st = r["status"].as_str().unwrap_or("?");
        let age = t.saturating_sub(r["since"].as_u64().unwrap_or(t));
        println!(
            "{} {:<8} {:>5}  {}",
            icon(st),
            st,
            fmt_age(age),
            r["cwd"].as_str().unwrap_or("?")
        );
    }
}

pub(crate) fn icon(st: &str) -> &'static str {
    match st {
        "working" => "🔵",
        "waiting" => "🟡",
        "done" => "🟢",
        "error" => "🔴",
        _ => "⚪",
    }
}

pub(crate) fn fmt_age(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

pub(crate) fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("pult/sessions")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"waiting"), "d2FpdGluZw==");
    }

    #[test]
    fn mapping() {
        let n = |t: &str| json!({ "notification_type": t });
        assert_eq!(status_for("PermissionRequest", &Value::Null), Some("waiting"));
        assert_eq!(status_for("Notification", &n("permission_prompt")), Some("waiting"));
        assert_eq!(status_for("Notification", &n("idle_prompt")), Some("done"));
        assert_eq!(status_for("Notification", &n("auth_success")), None);
        assert_eq!(status_for("PreToolUse", &Value::Null), Some("working"));
        assert_eq!(status_for("StopFailure", &Value::Null), Some("error"));
        assert_eq!(status_for("SessionEnd", &Value::Null), Some(""));
        assert_eq!(status_for("SomethingNew", &Value::Null), None);
    }

    #[test]
    fn one_line_truncates() {
        assert_eq!(one_line("git push"), ("git push".into(), false));
        assert_eq!(one_line("\n  first \nsecond"), ("first…".into(), true));
        let (s, cut) = one_line(&"x".repeat(200));
        assert!(cut);
        assert_eq!(s.chars().count(), 141);
        assert_eq!(one_line("a\x1b[31mb").0, "a[31mb");
    }

    #[test]
    fn ask_kinds() {
        let bash = json!({ "tool_name": "Bash", "tool_input": { "command": "git push" } });
        assert_eq!(
            ask_summary("PermissionRequest", &bash),
            Some(("Bash: git push".into(), "permission"))
        );
        let multi = json!({ "tool_name": "Bash", "tool_input": { "command": "a\nb" } });
        assert_eq!(
            ask_summary("PermissionRequest", &multi),
            Some(("Bash: a…".into(), "permission-long"))
        );
        let q = json!({ "tool_name": "AskUserQuestion", "tool_input": { "questions": [{ "question": "Which?" }] } });
        assert_eq!(
            ask_summary("PermissionRequest", &q),
            Some(("Question: Which?".into(), "question"))
        );
        assert_eq!(ask_summary("Stop", &Value::Null), None);
    }

    #[test]
    fn age_format() {
        assert_eq!(fmt_age(5), "5s");
        assert_eq!(fmt_age(125), "2m");
        assert_eq!(fmt_age(7200), "2h");
        assert_eq!(fmt_age(3 * 86400), "3d");
    }

    #[test]
    fn session_ids_are_sanitized() {
        assert_eq!(sanitize("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize("a1-b_2"), "a1-b_2");
    }
}
