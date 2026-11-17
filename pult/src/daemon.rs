//! `pultd` (`pult daemon`): the host of the sessions. Every session is a shell (or command) in a PTY. The daemon
//! tracks the screen, including scrollback, in a vt100 emulator, so a newly attached client gets the full state.
//! Clients (terminal tabs) come and go while the session keeps running. Several clients can attach to one session.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use std::{fs, thread};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde_json::{Value, json};

use crate::LockExt;
use crate::proto::{self, DATA, EXIT, RESIZE};

const SCROLLBACK: usize = 10_000;

/// At most this many output chunks (≤ 64 KB each, ~4 MB) may wait in a client's queue. A client that falls
/// further behind (slow network, sleeping phone) loses the backlog and gets a fresh snapshot once it reads
/// again, so a slow client never holds back the session or the other clients.
const CLIENT_QUEUE: usize = 64;

/// A request line longer than this is a broken or hostile client.
const MAX_REQUEST: u64 = 1 << 20;

struct Client {
    id: u64,
    tx: SyncSender<Vec<u8>>,
    st: Arc<ClientState>,
}

#[derive(Default)]
struct ClientState {
    /// fell behind: gets no output until the writer has sent a fresh snapshot
    lagged: AtomicBool,
    /// the session ended: the writer sends EXIT before closing
    exited: AtomicBool,
    /// the client's window size (cols, rows)
    size: Mutex<(u16, u16)>,
}

/// Output to every client, without waiting.
fn broadcast(clients: &mut Vec<Client>, data: &[u8]) {
    clients.retain(|c| {
        if c.st.lagged.load(Ordering::SeqCst) {
            return true;
        }
        match c.tx.try_send(data.to_vec()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                c.st.lagged.store(true, Ordering::SeqCst);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    });
}

/// Of the emulator's events we keep the terminal title (OSC 0/2): Claude sets it to the topic of the
/// conversation, which makes the session picker much more useful.
#[derive(Default)]
struct Cb {
    title: String,
}

impl vt100::Callbacks for Cb {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = String::from_utf8_lossy(title).into_owned();
    }
}

struct Inner {
    parser: vt100::Parser<Cb>,
    clients: Vec<Client>,
    /// the client whose size is in effect: whoever typed last (or attached last)
    active: u64,
    cwd: String,
}

struct Session {
    id: u32,
    name: Option<String>,
    created: u64,
    pid: Option<u32>,
    /// a plain shell (not `pult new -- command`): not kept when idle and unattached
    shell: bool,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    inner: Mutex<Inner>,
}

static SESSIONS: LazyLock<Mutex<BTreeMap<u32, Arc<Session>>>> = LazyLock::new(Default::default);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

fn sessions() -> Vec<Arc<Session>> {
    SESSIONS.lock_ok().values().cloned().collect()
}

pub fn run() {
    let path = proto::socket_path();
    if UnixStream::connect(&path).is_ok() {
        eprintln!("pultd is already running ({})", path.display());
        return;
    }
    let _ = fs::remove_file(&path);
    // the socket is 0600 from the moment it exists (umask around bind), not only after a later chmod
    let old_mask = unsafe { libc::umask(0o177) };
    let bound = UnixListener::bind(&path);
    unsafe { libc::umask(old_mask) };
    let listener = match bound {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    eprintln!("pultd listening on {}", path.display());

    thread::spawn(cwd_watcher);
    // only a live session with something running in it counts: a stale state file must not keep the machine awake
    thread::spawn(|| {
        crate::awake::keeper(|id| SESSIONS.lock_ok().get(&id).cloned().is_some_and(|s| busy(&s)));
    });
    for conn in listener.incoming().flatten() {
        thread::spawn(move || {
            if let Err(e) = handle(conn) {
                eprintln!("connection error: {e}");
            }
        });
    }
}

fn handle(stream: UnixStream) -> std::io::Result<()> {
    let mut rd = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    (&mut rd).take(MAX_REQUEST).read_line(&mut line)?;
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let mut out = stream.try_clone()?;
    let reply = |out: &mut UnixStream, v: Value| out.write_all(format!("{v}\n").as_bytes());

    match req["cmd"].as_str().unwrap_or("") {
        "ls" => reply(&mut out, list()),
        "kill" => {
            let s = find(&req["id"]);
            if let Some(s) = &s {
                let _ = s.killer.lock_ok().kill();
            }
            reply(&mut out, json!({ "ok": s.is_some() }))
        }
        "new" => match spawn_session(&req) {
            Ok(s) => attach(s, rd, out, &req),
            Err(e) => reply(&mut out, json!({ "ok": false, "error": e })),
        },
        "attach" => match find(&req["id"]) {
            Some(s) => attach(s, rd, out, &req),
            None => reply(&mut out, json!({ "ok": false, "error": "no such session" })),
        },
        other => reply(
            &mut out,
            json!({ "ok": false, "error": format!("unknown command: {other}") }),
        ),
    }
}

/// A session by number or name (for a name: the most recent one).
fn find(id: &Value) -> Option<Arc<Session>> {
    let map = SESSIONS.lock_ok();
    match id {
        Value::Number(n) => map.get(&u32::try_from(n.as_u64()?).ok()?).cloned(),
        Value::String(s) => match s.parse::<u32>() {
            Ok(n) => map.get(&n).cloned(),
            Err(_) => map
                .values()
                .rev()
                .find(|x| x.name.as_deref() == Some(s.as_str()))
                .cloned(),
        },
        _ => None,
    }
}

fn size_of(req: &Value) -> (u16, u16) {
    let g = |k: &str, d: u16| {
        req[k]
            .as_u64()
            .filter(|&v| v > 0 && v < 10_000)
            .map(|v| v as u16)
            .unwrap_or(d)
    };
    (g("cols", 80), g("rows", 24))
}

fn spawn_session(req: &Value) -> Result<Arc<Session>, String> {
    let (cols, rows) = size_of(req);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let argv: Vec<String> = req["argv"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut cmd = if argv.is_empty() {
        CommandBuilder::new_default_prog()
    } else {
        CommandBuilder::from_argv(argv.iter().map(Into::into).collect())
    };
    // The session gets the client's environment (WAYLAND_DISPLAY, DBUS...), so a Claude in it can reach the
    // clipboard (image paste).
    if let Some(env) = req["env"].as_object() {
        cmd.env_clear();
        for (k, v) in env {
            if let Some(v) = v.as_str() {
                cmd.env(k, v);
            }
        }
    }
    // a WezTerm pane id is meaningless after a reattach
    cmd.env_remove("WEZTERM_PANE");
    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    cmd.env("PULT_SESSION", id.to_string());
    let cwd = req["cwd"]
        .as_str()
        .filter(|d| fs::metadata(d).map(|m| m.is_dir()).unwrap_or(false))
        .map(String::from)
        .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    cmd.cwd(&cwd);

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    // `--run claude`: typed into the shell, which starts it with its own PATH and stays after it exits
    if let Some(run) = req["run"].as_str().filter(|r| !r.is_empty()) {
        let _ = writer.write_all(format!("{run}\r").as_bytes());
    }

    let s = Arc::new(Session {
        id,
        name: req["name"].as_str().filter(|n| !n.is_empty()).map(String::from),
        created: crate::now(),
        shell: argv.is_empty(),
        pid: child.process_id(),
        killer: Mutex::new(child.clone_killer()),
        master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        inner: Mutex::new(Inner {
            parser: vt100::Parser::new_with_callbacks(
                rows.try_into().unwrap_or(vt100::Parser::DEFAULT_ROWS),
                cols.try_into().unwrap_or(vt100::Parser::DEFAULT_COLS),
                SCROLLBACK,
                Cb::default(),
            ),
            clients: Vec::new(),
            active: 0,
            cwd,
        }),
    });
    SESSIONS.lock_ok().insert(id, s.clone());

    // PTY output → emulator + every attached client
    let rs = s.clone();
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut inner = rs.inner.lock_ok();
            inner.parser.process(&buf[..n]);
            broadcast(&mut inner.clients, &buf[..n]);
        }
    });
    // when the child exits: clients get EXIT and the session is gone
    thread::spawn(move || {
        let _ = child.wait();
        // let the remaining output through
        thread::sleep(Duration::from_millis(200));
        if let Some(s) = SESSIONS.lock_ok().remove(&id) {
            for c in s.inner.lock_ok().clients.drain(..) {
                c.st.exited.store(true, Ordering::SeqCst);
            }
        }
    });
    Ok(s)
}

fn resize(s: &Session, inner: &mut Inner, cols: u16, rows: u16) {
    if cols == 0 || rows == 0 {
        return;
    }
    let (r, c) = inner.parser.screen().size();
    if (r.get(), c.get()) == (rows, cols) {
        return;
    }
    let _ = s.master.lock_ok().resize(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    });
    if let (Ok(r), Ok(c)) = (rows.try_into(), cols.try_into()) {
        inner.parser.set_size(r, c);
    }
}

fn attach(s: Arc<Session>, mut rd: BufReader<UnixStream>, mut out: UnixStream, req: &Value) -> std::io::Result<()> {
    let (cols, rows) = size_of(req);
    out.write_all(format!("{}\n", json!({ "ok": true, "id": s.id })).as_bytes())?;

    let (tx, rx) = sync_channel(CLIENT_QUEUE);
    let cid = NEXT_CLIENT.fetch_add(1, Ordering::SeqCst);
    let st = Arc::new(ClientState {
        size: Mutex::new((cols, rows)),
        ..Default::default()
    });
    {
        // The new client's size takes effect. Snapshot and subscription happen under one lock, so no output
        // is lost or duplicated.
        let mut inner = s.inner.lock_ok();
        resize(&s, &mut inner, cols, rows);
        inner.active = cid;
        let snap = snapshot(&s, &mut inner);
        let _ = tx.try_send(snap);
        inner.clients.push(Client {
            id: cid,
            tx,
            st: st.clone(),
        });
    }

    let ws = s.clone();
    let wst = st.clone();
    let writer = thread::spawn(move || {
        loop {
            // Blocking wait. When lagging, the queue is full anyway, so the flag is checked after the next chunk.
            match rx.recv() {
                Ok(d) => {
                    if proto::write_frame(&mut out, DATA, &d).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    if wst.exited.load(Ordering::SeqCst) {
                        let _ = proto::write_frame(&mut out, EXIT, &[]);
                    }
                    break;
                }
            }
            // lagging: a fresh snapshot instead of the stale queued output
            if wst.lagged.load(Ordering::SeqCst) {
                while rx.try_recv().is_ok() {}
                let snap = {
                    let mut inner = ws.inner.lock_ok();
                    let snap = snapshot(&ws, &mut inner);
                    wst.lagged.store(false, Ordering::SeqCst);
                    snap
                };
                if proto::write_frame(&mut out, DATA, &snap).is_err() {
                    break;
                }
            }
        }
        let _ = out.shutdown(std::net::Shutdown::Both);
    });

    while let Ok((kind, payload)) = proto::read_frame(&mut rd) {
        match kind {
            DATA => {
                // Whoever types owns the size (switching laptop ↔ phone resizes). Automatic terminal replies are not
                // typing: those of an inactive client are dropped, or two clients would resize the session back and
                // forth and the app would answer every query twice.
                if proto::is_terminal_reply(&payload) {
                    if s.inner.lock_ok().active != cid {
                        continue;
                    }
                } else {
                    let mut inner = s.inner.lock_ok();
                    if inner.active != cid {
                        inner.active = cid;
                        let (c, r) = *st.size.lock_ok();
                        resize(&s, &mut inner, c, r);
                    }
                }
                let mut w = s.writer.lock_ok();
                let _ = w.write_all(&payload);
                let _ = w.flush();
            }
            RESIZE => {
                if let Some((c, r)) = proto::parse_resize(&payload) {
                    *st.size.lock_ok() = (c, r);
                    let mut inner = s.inner.lock_ok();
                    if inner.active == cid {
                        resize(&s, &mut inner, c, r);
                    }
                }
            }
            _ => {}
        }
    }
    let left = {
        let mut inner = s.inner.lock_ok();
        inner.clients.retain(|c| c.id != cid);
        // if it owned the size, the most recently attached remaining client takes over
        if inner.active == cid
            && let Some((id, (c, r))) = inner.clients.last().map(|c| (c.id, *c.st.size.lock_ok()))
        {
            inner.active = id;
            resize(&s, &mut inner, c, r);
        }
        inner.clients.len()
    };
    // The last client left and nothing runs in the shell: nothing to keep, the session ends. (Whatever does run
    // in it, like a Claude or a build, stays and can be attached again.)
    if left == 0 && !busy(&s) {
        let _ = s.killer.lock_ok().kill();
    }
    let _ = writer.join();
    Ok(())
}

/// Escape sequences that rebuild the session state in an empty terminal: scrollback, visible screen, cursor,
/// modes, plus the pult user vars (session, Claude status) and the directory (OSC 7).
fn snapshot(s: &Session, inner: &mut Inner) -> Vec<u8> {
    let mut o = String::from("\x1b[m\x1b[H\x1b[2J\x1b[3J");
    let scr = inner.parser.screen_mut();
    let cols = scr.size().1.get();
    if scr.alternate_screen() {
        o.push_str("\x1b[?1049h");
        o.push_str(&scr.state_formatted());
    } else {
        scr.set_scrollback(usize::MAX);
        let n = scr.scrollback();
        for off in (1..=n).rev() {
            scr.set_scrollback(off);
            if let Some(row) = scr.rows_formatted(0, cols).next() {
                o.push_str("\x1b[m");
                o.push_str(&row);
                o.push_str("\r\n");
            }
        }
        scr.set_scrollback(0);
        let rows: Vec<String> = scr.rows_formatted(0, cols).collect();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                o.push_str("\r\n");
            }
            o.push_str("\x1b[m");
            o.push_str(row);
        }
        o.push_str("\x1b[m");
        o.push_str(&scr.attributes_formatted());
        o.push_str(&scr.cursor_state_formatted());
        o.push_str(&scr.input_mode_formatted());
    }
    let (status, since) = claude_status(s.id).unwrap_or_default();
    o.push_str(&crate::user_var("pult_session", &s.id.to_string()));
    o.push_str(&crate::user_var("pult_host", &hostname()));
    o.push_str(&crate::user_var("pult_status", &status));
    o.push_str(&crate::user_var("pult_since", &since.to_string()));
    o.push_str(&osc7(&inner.cwd));
    let title = &inner.parser.callbacks().title;
    if !title.is_empty() {
        let title: String = title.chars().filter(|c| !c.is_control()).collect();
        o.push_str(&format!("\x1b]2;{title}\x07"));
    }
    o.into_bytes()
}

/// OSC 7 (directory) for the client. The path is percent-encoded, so a directory name containing control
/// characters (ESC, BEL) cannot smuggle its own escape sequences (such as fake user vars) into the terminal.
fn osc7(cwd: &str) -> String {
    let mut path = String::with_capacity(cwd.len());
    for b in cwd.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            path.push(b as char);
        } else {
            path.push_str(&format!("%{b:02X}"));
        }
    }
    let host: String = hostname()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-._".contains(*c))
        .collect();
    format!("\x1b]7;file://{host}{path}\x1b\\")
}

pub(crate) fn hostname() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
    if !ok {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// The latest state of the Claude in each session, from the hook state files (their `"pult": "<id>"` field),
/// read once: pult session id → contents of its newest state file.
fn claude_states() -> HashMap<u32, Value> {
    let mut out: HashMap<u32, Value> = HashMap::new();
    let Ok(dir) = fs::read_dir(crate::state_dir()) else {
        return out;
    };
    for v in dir
        .flatten()
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
    {
        let Some(id) = v["pult"].as_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if out.get(&id).is_none_or(|o| v["since"].as_u64() >= o["since"].as_u64()) {
            out.insert(id, v);
        }
    }
    out
}

fn claude_status(id: u32) -> Option<(String, u64)> {
    let v = claude_states().remove(&id)?;
    Some((
        v["status"].as_str().unwrap_or("").to_string(),
        v["since"].as_u64().unwrap_or(0),
    ))
}

fn foreground_pid(s: &Session) -> Option<u32> {
    s.master.lock_ok().process_group_leader().map(|p| p as u32)
}

fn list() -> Value {
    let host = hostname();
    let states = claude_states();
    let rows: Vec<Value> = sessions()
        .iter()
        .map(|s| {
            let fg = foreground(s);
            let fresh_cwd = foreground_pid(s).or(s.pid).and_then(process_cwd);
            let mut inner = s.inner.lock_ok();
            if inner.clients.is_empty()
                && let Some(cwd) = fresh_cwd
            {
                inner.cwd = cwd;
            }
            let st = states.get(&s.id);
            let field = |k: &str| st.and_then(|v| v[k].as_str()).unwrap_or("").to_string();
            json!({
                "id": s.id,
                "name": s.name,
                "cwd": inner.cwd,
                "clients": inner.clients.len(),
                "created": s.created,
                "pid": s.pid,
                "busy": busy(s),
                "host": host,
                "cmd": fg,
                "title": inner.parser.callbacks().title,
                "status": field("status"),
                "since": st.and_then(|v| v["since"].as_u64()).unwrap_or(0),
                // approving from a notification checks against these that this very request is still pending
                "ask": field("ask"),
                "ask_kind": field("ask_kind"),
            })
        })
        .collect();
    json!({ "ok": true, "sessions": rows })
}

/// Every second: the directory of each attached session's foreground process. On change, clients are told with
/// OSC 7 (the terminal derives the tab title and the directory of new tabs from it).
fn cwd_watcher() {
    loop {
        thread::sleep(Duration::from_secs(1));
        for s in sessions() {
            // only attached sessions; for the others `list()` looks on demand
            if s.inner.lock_ok().clients.is_empty() {
                continue;
            }
            let Some(cwd) = foreground_pid(&s).or(s.pid).and_then(process_cwd) else {
                continue;
            };
            let mut inner = s.inner.lock_ok();
            if inner.cwd != cwd {
                let msg = osc7(&cwd).into_bytes();
                inner.cwd = cwd;
                broadcast(&mut inner.clients, &msg);
            }
        }
    }
}

/// The foreground program in short (`claude`, `npm run dev`); `None` if it is just the shell.
fn foreground(s: &Session) -> Option<String> {
    let fg = foreground_pid(s)?;
    if s.shell && Some(fg) == s.pid {
        return None;
    }
    let argv = process_argv(fg)?;
    let mut parts: Vec<String> = Vec::new();
    for (i, a) in argv.iter().enumerate() {
        // for `node script.js` and the like, the script name is the telling part
        let a = if i == 0 || (i == 1 && a.contains('/')) {
            a.rsplit('/').next().unwrap_or(a).to_string()
        } else {
            a.clone()
        };
        if i == 1
            && parts.first().is_some_and(|p| {
                ["node", "python", "python3", "bash", "sh", "zsh", "bun", "deno"].contains(&p.as_str())
            })
        {
            parts.clear();
        }
        parts.push(a);
        if parts.len() >= 3 {
            break;
        }
    }
    let mut out = parts.join(" ");
    if out.chars().count() > 32 {
        out = out.chars().take(31).collect::<String>() + "…";
    }
    Some(out)
}

#[cfg(target_os = "linux")]
fn process_argv(pid: u32) -> Option<Vec<String>> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let v: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    (!v.is_empty()).then_some(v)
}

#[cfg(not(target_os = "linux"))]
fn process_argv(pid: u32) -> Option<Vec<String>> {
    let out = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(String::from)
        .collect();
    (!v.is_empty()).then_some(v)
}

/// Is anything running in the session? A command session: always. A shell: when something other than the shell
/// is in the foreground, or it has child processes (background jobs).
fn busy(s: &Session) -> bool {
    if !s.shell {
        return true;
    }
    let Some(pid) = s.pid else { return true };
    if foreground_pid(s).is_some_and(|fg| fg != pid) {
        return true;
    }
    has_children(pid)
}

#[cfg(target_os = "linux")]
fn has_children(pid: u32) -> bool {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    tasks.flatten().any(|t| {
        fs::read_to_string(t.path().join("children"))
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false)
    })
}

#[cfg(not(target_os = "linux"))]
fn has_children(pid: u32) -> bool {
    std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: u32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// macOS: `lsof` for the process's cwd (runs rarely, fine for a handful of sessions).
#[cfg(not(target_os = "linux"))]
fn process_cwd(pid: u32) -> Option<String> {
    let out = std::process::Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n').map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_escapes_control_characters() {
        let s = osc7("/tmp/a b\x1b]1337;SetUserVar=x=y\x07");
        assert!(s.starts_with("\x1b]7;file://"));
        assert!(s.ends_with("\x1b\\"));
        // only the sequence's own ESC and ST remain
        assert_eq!(s.matches('\x1b').count(), 2);
        assert!(!s.contains('\x07'));
        assert!(s.contains("a%20b%1B%5D1337%3BSetUserVar%3Dx%3Dy%07"));
    }

    #[test]
    fn request_sizes_are_bounded() {
        assert_eq!(size_of(&json!({})), (80, 24));
        assert_eq!(size_of(&json!({ "cols": 120, "rows": 40 })), (120, 40));
        assert_eq!(size_of(&json!({ "cols": 0, "rows": 99999 })), (80, 24));
    }

    #[test]
    fn broadcast_marks_slow_clients_and_drops_dead_ones() {
        let mk = |cap: usize| {
            let (tx, rx) = sync_channel(cap);
            (
                Client {
                    id: 0,
                    tx,
                    st: Arc::new(ClientState::default()),
                },
                rx,
            )
        };
        let (slow, _slow_rx) = mk(1);
        let (dead, dead_rx) = mk(1);
        drop(dead_rx);
        let slow_state = slow.st.clone();
        let mut clients = vec![slow, dead];
        broadcast(&mut clients, b"one");
        assert_eq!(clients.len(), 1);
        assert!(!slow_state.lagged.load(Ordering::SeqCst));
        broadcast(&mut clients, b"two");
        assert!(slow_state.lagged.load(Ordering::SeqCst));
        assert_eq!(clients.len(), 1);
    }
}
