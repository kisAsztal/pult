//! Client side: `pult new`, `pult attach`, `pult ls`, `pult kill`. `new` / `attach` put the terminal in raw mode,
//! forward keystrokes to the daemon and its output back. Closing the tab only detaches.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::LockExt;
use crate::proto::{self, DATA, EXIT, RESIZE};

/// Connect to the daemon; if `autostart` and it is not running, start it (`pult daemon`, in its own session).
pub fn connect(autostart: bool) -> std::io::Result<UnixStream> {
    let path = proto::socket_path();
    // in the /tmp fallback location another user could pre-create a socket: only connect to our own
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::symlink_metadata(&path)
            && m.uid() != unsafe { libc::getuid() }
        {
            return Err(std::io::Error::other(format!("socket is not ours: {}", path.display())));
        }
    }
    match UnixStream::connect(&path) {
        Ok(s) => Ok(s),
        Err(e) if autostart => {
            start_daemon()?;
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(50));
                if let Ok(s) = UnixStream::connect(&path) {
                    return Ok(s);
                }
            }
            Err(e)
        }
        Err(e) => Err(e),
    }
}

fn start_daemon() -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    let log_dir = crate::state_dir().parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let _ = crate::private_dir(&log_dir);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log_dir.join("pultd.log"))?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

fn request(req: Value) -> std::io::Result<Value> {
    let mut s = connect(false)?;
    s.write_all(format!("{req}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    Ok(serde_json::from_str(&line).unwrap_or(Value::Null))
}

/// `/home/<user>/x` or `/Users/<user>/x` → `~/x` (for a remote path, where our own HOME says nothing).
fn tilde(cwd: &str, home: Option<&str>) -> String {
    match home {
        Some(h) if !h.is_empty() && (cwd == h || cwd.starts_with(&format!("{h}/"))) => format!("~{}", &cwd[h.len()..]),
        Some(_) => cwd.to_string(),
        None => match cwd.strip_prefix("/home/").or_else(|| cwd.strip_prefix("/Users/")) {
            Some(rest) => rest
                .find('/')
                .map(|i| format!("~{}", &rest[i..]))
                .unwrap_or_else(|| "~".into()),
            None => cwd.to_string(),
        },
    }
}

fn print_sessions(sessions: &[Value], host: Option<&str>) {
    let t = crate::now();
    let home = std::env::var("HOME").unwrap_or_default();
    for s in sessions {
        let st = s["status"].as_str().unwrap_or("");
        let age = if st.is_empty() {
            String::new()
        } else {
            crate::fmt_age(t.saturating_sub(s["since"].as_u64().unwrap_or(t)))
        };
        let clients = s["clients"].as_u64().unwrap_or(0);
        // the session's own home (sent by its daemon) is the reliable one; guess only for older daemons
        let session_home = s["home"].as_str().filter(|h| !h.is_empty());
        let cwd = tilde(
            s["cwd"].as_str().unwrap_or("?"),
            session_home.or(host.is_none().then_some(home.as_str())),
        );
        let cmd = s["cmd"].as_str();
        // the shell's own title says nothing new, nor does a title that is just the command
        let title = s["title"]
            .as_str()
            .filter(|t| !t.is_empty() && cmd.is_some() && Some(*t) != cmd)
            .map(|t| format!("  “{t}”"))
            .unwrap_or_default();
        let id = match host {
            Some(h) => format!("{h}:{}", s["id"]),
            None => s["id"].to_string(),
        };
        println!(
            "{:>10}  {} {:<8} {:>5}  {:<3} {}{:<24} {}{}",
            id,
            crate::icon(st),
            if st.is_empty() { "-" } else { st },
            age,
            if clients > 0 {
                format!("{clients}×")
            } else {
                "·".into()
            },
            s["name"].as_str().map(|n| format!("[{n}] ")).unwrap_or_default(),
            cwd,
            cmd.unwrap_or("shell"),
            title,
        );
    }
}

/// `pult ls [--all] [--json]`: local sessions; `--all`: every host in `~/.config/pult/hosts`.
pub fn ls(as_json: bool, all: bool) {
    let res = request(json!({ "cmd": "ls" }));
    let local = match &res {
        Ok(v) => Value::Array(v["sessions"].as_array().cloned().unwrap_or_default()),
        // the daemon is not running: no local sessions (not an error)
        Err(_) => json!([]),
    };
    if !all {
        if as_json {
            println!("{local}");
        } else if res.is_err() {
            println!("pultd is not running");
        } else if let Some(list) = local.as_array().filter(|a| !a.is_empty()) {
            print_sessions(list, None);
        } else {
            println!("no sessions");
        }
        return;
    }
    let hosts = crate::hosts::ls_all(local);
    if as_json {
        println!("{hosts}");
        return;
    }
    for h in hosts.as_array().into_iter().flatten() {
        let name = h["name"].as_str().unwrap_or("?");
        let sessions = h["sessions"].as_array().cloned().unwrap_or_default();
        if h["ok"].as_bool() != Some(true) {
            println!("── {name}: unreachable ({})", h["error"].as_str().unwrap_or("?"));
            continue;
        }
        println!("── {name}{}", if sessions.is_empty() { ": no sessions" } else { "" });
        let local = h["local"].as_bool() == Some(true);
        print_sessions(&sessions, (!local).then_some(name));
    }
}

/// `pult kill <id|name|host:id>`
pub fn kill(spec: &str) {
    let cfg = crate::hosts::config();
    let (host, id) = crate::hosts::split_target(&cfg, spec).unwrap_or_else(|e| die(&e));
    if let Some(h) = host {
        if let Err(e) = crate::hosts::run_remote(h, &["kill", &crate::hosts::quote(id)], Duration::from_secs(8)) {
            die(&format!("{}: {e}", h.name));
        }
        return;
    }
    match request(json!({ "cmd": "kill", "id": id })) {
        Ok(v) if v["ok"].as_bool() == Some(true) => {}
        Ok(_) => die(&format!("no such session: {id}")),
        Err(e) => die(&format!("pultd is not running: {e}")),
    }
}

fn die(msg: &str) -> ! {
    eprintln!("pult: {msg}");
    std::process::exit(1);
}

/// The remote host named by `--host`, or `None` for this machine.
fn remote_host<'a>(cfg: &'a crate::hosts::Config, host: Option<&str>) -> Option<&'a crate::hosts::Host> {
    host.and_then(|h| crate::hosts::find_host(cfg, h).unwrap_or_else(|e| die(&e)))
}

/// `pult dirs [--host HOST]`: project directories for a new session (feeds the picker).
pub fn dirs(host: Option<String>) {
    let cfg = crate::hosts::config();
    match remote_host(&cfg, host.as_deref()) {
        None => crate::hosts::dirs().iter().for_each(|d| println!("{d}")),
        Some(r) => match crate::hosts::run_remote(r, &["dirs"], Duration::from_secs(8)) {
            Ok(out) => print!("{out}"),
            Err(e) => die(&format!("{}: {e}", r.name)),
        },
    }
}

/// `pult new [name] [--host HOST] [--cwd DIR] [--run CMD] [-- command...]`
pub fn new(
    host: Option<String>,
    name: Option<String>,
    cwd: Option<String>,
    run_cmd: Option<String>,
    argv: Vec<String>,
) {
    let cfg = crate::hosts::config();
    let remote = remote_host(&cfg, host.as_deref());
    // a local directory means nothing on a remote host: there, the given one or the remote home
    let cwd = if remote.is_some() {
        cwd
    } else {
        cwd.or_else(|| std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned()))
    };
    if std::env::var_os("PULT_SESSION").is_some() {
        // sessions do not nest: open a new WezTerm tab with the new session instead
        let mut sub = vec!["new"];
        if let Some(h) = &host {
            sub.extend(["--host", h.as_str()]);
        }
        if let Some(r) = &run_cmd {
            sub.extend(["--run", r.as_str()]);
        }
        open_in_new_tab(&sub, name.as_deref(), cwd.as_deref(), &argv);
        return;
    }
    if let Some(r) = remote {
        let mut args = vec!["new".to_string()];
        if let Some(n) = name {
            args.extend(["--name".into(), n]);
        }
        if let Some(d) = cwd {
            args.extend(["--cwd".into(), d]);
        }
        if let Some(r) = run_cmd {
            args.extend(["--run".into(), r]);
        }
        if !argv.is_empty() {
            args.push("--".into());
            args.extend(argv);
        }
        crate::hosts::exec_remote(r, &args);
    }
    let env: serde_json::Map<String, Value> = std::env::vars()
        .filter(|(k, _)| !skip_env(k))
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    let req = json!({ "cmd": "new", "name": name, "cwd": cwd, "run": run_cmd, "argv": argv, "env": env });
    match connect(true) {
        Ok(s) => run(s, req),
        Err(e) => {
            // stay usable without the daemon: a plain shell
            eprintln!("pult: pultd does not start ({e}), starting a plain shell");
            fallback_shell();
        }
    }
}

/// Variables that are not passed on to the new session. If the terminal was started from inside a Claude, its
/// own variables (`CLAUDECODE`, `CLAUDE_CODE_*`) would make a Claude in the session think it is a child session,
/// and the parent's messaging token would leak into it.
fn skip_env(k: &str) -> bool {
    k == "PULT_SESSION"
        || k == "CLAUDECODE"
        || k == "CLAUDE_PID"
        || k == "CLAUDE_EFFORT"
        || k.starts_with("CLAUDE_CODE_")
}

/// `pult paste-image --host HOST`: upload the clipboard image to HOST and print its remote path (WezTerm pastes
/// that path for the remote Claude). Exit code 3: no image on the clipboard, or the host is this machine.
pub fn paste_image(host: Option<String>) {
    let cfg = crate::hosts::config();
    let Some(h) = host else { die("--host is required") };
    let Some(remote) = crate::hosts::find_host(&cfg, &h).unwrap_or_else(|e| die(&e)) else {
        std::process::exit(3)
    };
    let Some((data, ext)) = clipboard_image() else {
        std::process::exit(3)
    };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    match crate::hosts::upload(remote, data, &format!("image-{ms}.{ext}")) {
        Ok(path) if !path.is_empty() => print!("{path}"),
        Ok(_) => die(&format!("{}: the upload returned no path", remote.name)),
        Err(e) => die(&format!("{}: {e}", remote.name)),
    }
}

/// An image from the clipboard (`wl-paste` on Wayland if present, otherwise `xclip`): (bytes, extension).
fn clipboard_image() -> Option<(Vec<u8>, &'static str)> {
    use std::process::{Command, Stdio};
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && Command::new("wl-paste")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
    let get = |mime: &str| -> Option<Vec<u8>> {
        let out = if wayland {
            Command::new("wl-paste")
                .args(["--no-newline", "--type", mime])
                .stderr(Stdio::null())
                .output()
        } else {
            Command::new("xclip")
                .args(["-selection", "clipboard", "-t", mime, "-o"])
                .stderr(Stdio::null())
                .output()
        }
        .ok()?;
        (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
    };
    let types = if wayland {
        Command::new("wl-paste")
            .arg("--list-types")
            .stderr(Stdio::null())
            .output()
            .ok()?
            .stdout
    } else {
        get("TARGETS")?
    };
    let types = String::from_utf8_lossy(&types);
    let (mime, ext) = [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/webp", "webp"),
        ("image/gif", "gif"),
    ]
    .into_iter()
    .find(|(m, _)| types.lines().any(|l| l.trim() == *m))?;
    Some((get(mime)?, ext))
}

/// `pult attach <id|name|host:id>`
pub fn attach(spec: &str) {
    if std::env::var_os("PULT_SESSION").is_some() {
        open_in_new_tab(&["attach", spec], None, None, &[]);
        return;
    }
    let cfg = crate::hosts::config();
    let (host, id) = crate::hosts::split_target(&cfg, spec).unwrap_or_else(|e| die(&e));
    if let Some(h) = host {
        crate::hosts::exec_remote(h, &["attach".into(), id.to_string()]);
    }
    match connect(false) {
        Ok(s) => run(s, json!({ "cmd": "attach", "id": id })),
        Err(e) => die(&format!("pultd is not running: {e}")),
    }
}

fn open_in_new_tab(sub: &[&str], name: Option<&str>, cwd: Option<&str>, argv: &[String]) {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "pult".into());
    let mut cmd = std::process::Command::new("wezterm");
    // `wezterm cli spawn` passes the environment on: without PULT_SESSION, or the new tab would open another tab
    cmd.env_remove("PULT_SESSION");
    cmd.args(["cli", "spawn"]);
    if let Some(d) = cwd {
        cmd.args(["--cwd", d]);
    }
    cmd.arg("--").arg(&exe).args(sub);
    if let Some(n) = name {
        cmd.args(["--name", n]);
    }
    if !argv.is_empty() {
        cmd.arg("--").args(argv);
    }
    match cmd.stdout(std::process::Stdio::null()).status() {
        Ok(st) if st.success() => {}
        _ => die("sessions do not nest, and a new WezTerm tab could not be opened either"),
    }
}

fn fallback_shell() -> ! {
    use std::os::unix::process::CommandExt;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let err = std::process::Command::new(&shell).exec();
    die(&format!("{shell}: {err}"))
}

fn run(stream: UnixStream, mut req: Value) {
    let (cols, rows) = term_size();
    req["cols"] = cols.into();
    req["rows"] = rows.into();

    let Ok(read_half) = stream.try_clone() else {
        die("cannot duplicate the socket")
    };
    let mut rd = BufReader::new(read_half);
    let w = Arc::new(Mutex::new(stream));
    if w.lock_ok().write_all(format!("{req}\n").as_bytes()).is_err() {
        die("the daemon closed the connection");
    }
    let mut line = String::new();
    let _ = rd.read_line(&mut line);
    let resp: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    if resp["ok"].as_bool() != Some(true) {
        die(resp["error"].as_str().unwrap_or("error"));
    }

    let raw = RawMode::enable();

    // keystrokes → daemon
    let wi = w.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut stdin = std::io::stdin().lock();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if proto::write_frame(&mut *wi.lock_ok(), DATA, &buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
    });
    // window size changes → daemon
    let wr = w.clone();
    std::thread::spawn(move || {
        let Ok(mut signals) = signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH]) else {
            return;
        };
        for _ in signals.forever() {
            let (c, r) = term_size();
            if proto::write_frame(&mut *wr.lock_ok(), RESIZE, &proto::resize_payload(c, r)).is_err() {
                break;
            }
        }
    });

    // daemon output → terminal
    let mut stdout = std::io::stdout().lock();
    while let Ok((kind, payload)) = proto::read_frame(&mut rd) {
        match kind {
            DATA => {
                if stdout.write_all(&payload).and_then(|_| stdout.flush()).is_err() {
                    break;
                }
            }
            EXIT => break,
            _ => {}
        }
    }
    drop(raw);
    std::process::exit(0);
}

fn term_size() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24)
    }
}

/// Raw terminal mode (every key goes to the session unchanged); restored on drop.
struct RawMode(Option<libc::termios>);

impl RawMode {
    fn enable() -> Self {
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 0 {
                return RawMode(None);
            }
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return RawMode(None);
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
            RawMode(Some(orig))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_filter() {
        assert!(skip_env("CLAUDE_CODE_CHILD_SESSION"));
        assert!(skip_env("CLAUDECODE"));
        assert!(!skip_env("CLAUDE_CONFIG_DIR"));
        assert!(!skip_env("WAYLAND_DISPLAY"));
    }

    #[test]
    fn tilde_paths() {
        assert_eq!(tilde("/home/me/src/app", Some("/home/me")), "~/src/app");
        assert_eq!(tilde("/home/me", Some("/home/me")), "~");
        assert_eq!(tilde("/srv/users/me/app", Some("/srv/users/me")), "~/app");
        assert_eq!(tilde("/home/meow/x", Some("/home/me")), "/home/meow/x");
        assert_eq!(tilde("/home/other/src/app", None), "~/src/app");
        assert_eq!(tilde("/Users/other", None), "~");
        assert_eq!(tilde("/srv/app", None), "/srv/app");
    }
}
