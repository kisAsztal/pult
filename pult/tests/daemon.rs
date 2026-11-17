//! End-to-end tests: a real `pult daemon` on a private socket, spoken to over the wire protocol.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const DATA: u8 = 0;
const EXIT: u8 = 2;

struct Daemon {
    child: Child,
    dir: PathBuf,
}

impl Daemon {
    fn start(name: &str) -> Daemon {
        let dir = std::env::temp_dir().join(format!("pult-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_pult"))
            .arg("daemon")
            .env("XDG_RUNTIME_DIR", &dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("HOME", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let d = Daemon { child, dir };
        let deadline = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(d.socket()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("pult.sock")
    }

    /// One request, one JSON reply (for ls / kill).
    fn request(&self, req: Value) -> Value {
        let mut s = UnixStream::connect(self.socket()).unwrap();
        s.write_all(format!("{req}\n").as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// `new` / `attach`: returns the stream after the JSON reply, now speaking frames.
    fn open(&self, req: Value) -> (UnixStream, Value) {
        let mut s = UnixStream::connect(self.socket()).unwrap();
        s.write_all(format!("{req}\n").as_bytes()).unwrap();
        // read the reply line byte by byte: a BufReader would swallow the first frames
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while s.read(&mut b).unwrap() == 1 && b[0] != b'\n' {
            line.push(b[0]);
        }
        let reply = serde_json::from_slice(&line).unwrap();
        (s, reply)
    }

    fn sessions(&self) -> Vec<Value> {
        self.request(json!({ "cmd": "ls" }))["sessions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn read_frame(s: &mut UnixStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    s.read_exact(&mut head)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    let mut payload = vec![0u8; len];
    s.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

fn write_frame(s: &mut UnixStream, kind: u8, payload: &[u8]) {
    let mut buf = vec![kind];
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    s.write_all(&buf).unwrap();
}

/// Read frames until `needle` shows up in the output (or the deadline passes).
fn read_until(s: &mut UnixStream, needle: &str) -> String {
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut out = String::new();
    while Instant::now() < deadline {
        match read_frame(s) {
            Ok((DATA, d)) => {
                out.push_str(&String::from_utf8_lossy(&d));
                if out.contains(needle) {
                    return out;
                }
            }
            Ok((EXIT, _)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    panic!("never saw {needle:?}; got {out:?}");
}

fn new_req(argv: &[&str]) -> Value {
    json!({ "cmd": "new", "argv": argv, "cols": 80, "rows": 24, "cwd": "/", "env": { "PATH": std::env::var("PATH").unwrap() } })
}

#[test]
fn socket_is_private() {
    let d = Daemon::start("perm");
    let mode = std::fs::metadata(d.socket()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn empty_daemon_lists_nothing_and_rejects_unknown_commands() {
    let d = Daemon::start("empty");
    assert!(d.sessions().is_empty());
    let r = d.request(json!({ "cmd": "nonsense" }));
    assert_eq!(r["ok"], false);
    let r = d.request(json!({ "cmd": "attach", "id": 42 }));
    assert_eq!(r["ok"], false);
    let r = d.request(json!({ "cmd": "kill", "id": "nope" }));
    assert_eq!(r["ok"], false);
}

#[test]
fn garbage_request_does_not_kill_the_daemon() {
    let d = Daemon::start("garbage");
    let mut s = UnixStream::connect(d.socket()).unwrap();
    s.write_all(b"this is not json\n").unwrap();
    drop(s);
    let mut s = UnixStream::connect(d.socket()).unwrap();
    s.write_all(&[0xff, 0xfe, b'\n']).unwrap();
    drop(s);
    assert!(d.sessions().is_empty());
}

#[test]
fn session_output_reaches_the_client_and_the_session_ends() {
    let d = Daemon::start("run");
    let (mut s, reply) = d.open(new_req(&["sh", "-c", "echo pult-says-hi; sleep 1"]));
    assert_eq!(reply["ok"], true);
    read_until(&mut s, "pult-says-hi");
    // the command ends, the client gets EXIT, the session disappears
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    loop {
        match read_frame(&mut s) {
            Ok((EXIT, _)) => break,
            Ok(_) => {}
            Err(e) => panic!("no EXIT frame: {e}"),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while !d.sessions().is_empty() {
        assert!(Instant::now() < deadline, "session did not disappear");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn keystrokes_reach_the_session_and_a_second_client_gets_a_snapshot() {
    let d = Daemon::start("attach");
    let (mut a, reply) = d.open(new_req(&["cat"]));
    let id = reply["id"].as_u64().unwrap();
    write_frame(&mut a, DATA, b"first-line\n");
    read_until(&mut a, "first-line");

    assert_eq!(d.sessions().len(), 1);
    assert_eq!(d.sessions()[0]["clients"], 1);

    // the second client attaches later and still sees what happened before
    let (mut b, reply) = d.open(json!({ "cmd": "attach", "id": id, "cols": 100, "rows": 30 }));
    assert_eq!(reply["ok"], true);
    let snap = read_until(&mut b, "first-line");
    assert!(
        snap.contains("\x1b]1337;SetUserVar=pult_session="),
        "snapshot carries the pult user vars"
    );

    // both see new output
    write_frame(&mut b, DATA, b"second-line\n");
    read_until(&mut a, "second-line");
    read_until(&mut b, "second-line");
    assert_eq!(d.sessions()[0]["clients"], 2);

    assert_eq!(d.request(json!({ "cmd": "kill", "id": id }))["ok"], true);
}

#[test]
fn command_sessions_survive_detaching_and_kill_ends_them() {
    let d = Daemon::start("detach");
    let (s, reply) = d.open(new_req(&["sleep", "30"]));
    let id = reply["id"].as_u64().unwrap();
    drop(s);
    std::thread::sleep(Duration::from_millis(300));
    let list = d.sessions();
    assert_eq!(list.len(), 1, "a command session is kept without clients");
    assert_eq!(list[0]["clients"], 0);
    assert_eq!(list[0]["busy"], true);

    assert_eq!(d.request(json!({ "cmd": "kill", "id": id }))["ok"], true);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !d.sessions().is_empty() {
        assert!(Instant::now() < deadline, "killed session did not disappear");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn sessions_can_be_found_by_name() {
    let d = Daemon::start("name");
    let mut req = new_req(&["sleep", "30"]);
    req["name"] = json!("worker");
    let (_s, reply) = d.open(req);
    assert_eq!(reply["ok"], true);
    assert_eq!(d.sessions()[0]["name"], "worker");
    assert_eq!(d.request(json!({ "cmd": "kill", "id": "worker" }))["ok"], true);
}

#[test]
fn an_idle_shell_session_ends_when_the_last_client_leaves() {
    let d = Daemon::start("idle-shell");
    let (mut s, reply) = d.open(json!({
        "cmd": "new", "cols": 80, "rows": 24, "cwd": "/",
        "env": { "PATH": std::env::var("PATH").unwrap(), "SHELL": "/bin/sh" },
    }));
    assert_eq!(reply["ok"], true);
    // wait for the shell to come up, then detach with nothing running in it
    s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let _ = read_frame(&mut s);
    drop(s);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !d.sessions().is_empty() {
        assert!(Instant::now() < deadline, "idle shell session was kept");
        std::thread::sleep(Duration::from_millis(100));
    }
}
