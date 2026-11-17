//! Multiple machines: sessions on the hosts listed in `~/.config/pult/hosts`, reached over SSH.
//!
//! ```text
//! # name     ssh target (an alias from ~/.ssh/config works too)
//! devbox     devbox.example.com
//! self       laptop          # optional: the name of this machine in listings (default: its hostname)
//! ```
//! `pult` must be installed on every host as `~/.cargo/bin/pult`. SSH connections are kept open
//! (ControlMaster, 10 minutes), so later queries are fast. A host that does not answer within ~2 s is "unreachable".

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const REMOTE_PULT: &str = "~/.cargo/bin/pult";

pub struct Host {
    pub name: String,
    pub target: String,
}

pub struct Config {
    pub self_name: String,
    pub hosts: Vec<Host>,
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
        .join("pult")
}

pub fn config() -> Config {
    let text = std::fs::read_to_string(config_dir().join("hosts")).unwrap_or_default();
    parse_config(&text, crate::daemon::hostname())
}

fn parse_config(text: &str, own_hostname: String) -> Config {
    let mut cfg = Config {
        self_name: own_hostname,
        hosts: Vec::new(),
    };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut it = line.split_whitespace();
        let (Some(name), Some(target)) = (it.next(), it.next()) else {
            continue;
        };
        if name == "self" {
            cfg.self_name = target.to_string();
        } else if target.starts_with('-') {
            // would be parsed by ssh as an option
            eprintln!("pult: ignoring host {name}: the ssh target must not start with '-'");
        } else {
            cfg.hosts.push(Host {
                name: name.to_string(),
                target: target.to_string(),
            });
        }
    }
    cfg
}

fn unknown_host(name: &str) -> String {
    format!("unknown host: {name} (see ~/.config/pult/hosts)")
}

/// `devbox:3` → (Some(devbox), "3"); `3` or `<this machine>:3` → (None, "3") = local.
pub fn split_target<'a>(cfg: &'a Config, spec: &'a str) -> Result<(Option<&'a Host>, &'a str), String> {
    let Some((name, id)) = spec.split_once(':') else {
        return Ok((None, spec));
    };
    find_host(cfg, name).map(|h| (h, id))
}

/// By host name (or ssh target); `None` if it is this machine. An error for an unknown name.
pub fn find_host<'a>(cfg: &'a Config, name: &str) -> Result<Option<&'a Host>, String> {
    if name == cfg.self_name || name == crate::daemon::hostname() {
        return Ok(None);
    }
    cfg.hosts
        .iter()
        .find(|h| h.name == name || h.target == name)
        .map(Some)
        .ok_or_else(|| unknown_host(name))
}

pub fn ssh(host: &Host, tty: bool) -> Command {
    let ctl = crate::proto::socket_path()
        .parent()
        .map(|p| p.join("pult-ssh-%C"))
        .unwrap_or_default();
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=3",
        "-o",
        "ControlMaster=auto",
        "-o",
        "ControlPersist=10m",
    ]);
    cmd.arg("-o").arg(format!("ControlPath={}", ctl.display()));
    if tty {
        cmd.arg("-t");
    }
    cmd.arg("--").arg(&host.target);
    cmd
}

/// One argument, quoted for the remote shell.
pub fn quote(a: &str) -> String {
    if !a.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,~".contains(c))
    {
        return a.to_string();
    }
    format!("'{}'", a.replace('\'', "'\\''"))
}

/// Run `pult <args>` on the remote host with this terminal attached (attach / new). Does not return.
pub fn exec_remote(host: &Host, args: &[String]) -> ! {
    // sshd does not accept these by default; without them a remote Claude draws with fewer colours and does not
    // know what terminal it runs in
    let env = ["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"]
        .iter()
        .filter_map(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| format!("{k}={}", quote(&v)))
        });
    let remote = env
        .chain(std::iter::once(REMOTE_PULT.to_string()))
        .chain(args.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    let err = ssh(host, true).arg(remote).exec();
    eprintln!("pult: ssh {}: {err}", host.target);
    std::process::exit(1);
}

/// `pult <args>` on the remote host, with its output and a timeout. The caller quotes the arguments.
pub fn run_remote(host: &Host, args: &[&str], timeout: Duration) -> Result<String, String> {
    let remote = std::iter::once(REMOTE_PULT)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    run_remote_sh(host, &remote, None, timeout)
}

/// A shell command on the remote host (optionally with stdin data), with its output and a timeout.
fn run_remote_sh(host: &Host, remote: &str, input: Option<Vec<u8>>, timeout: Duration) -> Result<String, String> {
    let mut child = ssh(host, false)
        .arg(remote)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&data);
        });
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let _ = child.stdout.take().map(|mut o| o.read_to_string(&mut out));
                if status.success() {
                    return Ok(out);
                }
                let mut err = String::new();
                let _ = child.stderr.take().map(|mut e| e.read_to_string(&mut err));
                let err = err.lines().last().unwrap_or("").trim().to_string();
                return Err(if err.is_empty() {
                    format!("failed ({status})")
                } else {
                    err
                });
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("no response".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Upload a file into `~/.cache/pult/paste` on the remote host and return its remote path.
/// Uploads older than 7 days are deleted on the way. `name` must be a plain file name made by the caller.
pub fn upload(host: &Host, data: Vec<u8>, name: &str) -> Result<String, String> {
    let remote = format!(
        "umask 077 && d=\"$HOME/.cache/pult/paste\" && mkdir -p \"$d\" && cat > \"$d/{name}\" && echo \"$d/{name}\" && \
         {{ find \"$d\" -type f -mtime +7 -delete 2>/dev/null; true; }}"
    );
    run_remote_sh(host, &remote, Some(data), Duration::from_secs(20)).map(|s| s.trim().to_string())
}

/// Sessions on every host: the local ones immediately, the remote ones in parallel with a ~2 s timeout.
pub fn ls_all(local: Value) -> Value {
    let cfg = config();
    let handles: Vec<_> = cfg
        .hosts
        .into_iter()
        .map(|h| {
            std::thread::spawn(move || {
                let res = run_remote(&h, &["ls", "--json"], Duration::from_millis(2200));
                match res.and_then(|o| serde_json::from_str::<Value>(o.trim()).map_err(|e| e.to_string())) {
                    Ok(v) => json!({ "name": h.name, "local": false, "ok": true, "sessions": v }),
                    Err(e) => json!({ "name": h.name, "local": false, "ok": false, "error": e, "sessions": [] }),
                }
            })
        })
        .collect();
    let sessions = if local.is_array() { local.clone() } else { json!([]) };
    let mut out = vec![json!({ "name": cfg.self_name, "local": true, "ok": local.is_array(), "sessions": sessions })];
    out.extend(handles.into_iter().filter_map(|h| h.join().ok()));
    Value::Array(out)
}

/// Directories offered for a new session: `~`, then the sub-directories of the project roots listed in
/// `~/.config/pult/projects` (one directory per line, `~` allowed). Without the file: `~/projects`, `~/src`, `~/code`.
pub fn dirs() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = match std::fs::read_to_string(config_dir().join("projects")) {
        Ok(text) => parse_roots(&text, &home),
        Err(_) => ["projects", "src", "code"]
            .iter()
            .map(|d| format!("{home}/{d}"))
            .collect(),
    };
    let mut out = vec![home];
    for root in roots {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        let mut sub: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .map(|n| format!("{root}/{n}"))
            .collect();
        sub.sort();
        out.push(root);
        out.extend(sub);
    }
    out
}

fn parse_roots(text: &str, home: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| match l.strip_prefix('~') {
            Some(rest) => format!("{home}{rest}"),
            None => l.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        parse_config(
            "# comment\nbox  box.example.com   # trailing\nself laptop\nbad -oProxyCommand=x\n\nlonely\n",
            "fallback".into(),
        )
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("attach"), "attach");
        assert_eq!(quote("/home/x/projects/app"), "/home/x/projects/app");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("$(reboot)"), "'$(reboot)'");
        assert_eq!(quote("a;b"), "'a;b'");
    }

    #[test]
    fn config_parsing() {
        let c = cfg();
        assert_eq!(c.self_name, "laptop");
        assert_eq!(c.hosts.len(), 1);
        assert_eq!(
            (c.hosts[0].name.as_str(), c.hosts[0].target.as_str()),
            ("box", "box.example.com")
        );
        assert_eq!(parse_config("", "h".into()).self_name, "h");
    }

    #[test]
    fn targets() {
        let c = cfg();
        assert!(matches!(split_target(&c, "3"), Ok((None, "3"))));
        assert!(matches!(split_target(&c, "laptop:3"), Ok((None, "3"))));
        assert!(matches!(split_target(&c, "box:app"), Ok((Some(_), "app"))));
        assert!(matches!(split_target(&c, "box.example.com:1"), Ok((Some(_), "1"))));
        assert!(split_target(&c, "mac:1").is_err());
    }

    #[test]
    fn project_roots() {
        let roots = parse_roots("~/work\n# x\n  /srv/code  # c\n\n", "/home/u");
        assert_eq!(roots, ["/home/u/work", "/srv/code"]);
    }
}
