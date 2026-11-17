//! Wire protocol between the `pult` client and the `pultd` daemon (local unix socket).
//!
//! A connection starts with one JSON line as the request (`{"cmd": "ls" | "kill" | "new" | "attach", ...}`) and
//! one JSON line as the reply. After `new` / `attach` it switches to binary frames: `[kind u8][len u32 BE][data]`.

use std::io::{self, Read, Write};
use std::path::PathBuf;

/// client → daemon: keystrokes; daemon → client: terminal output
pub const DATA: u8 = 0;
/// client → daemon: new size (`cols u16, rows u16`, big endian)
pub const RESIZE: u8 = 1;
/// daemon → client: the session ended
pub const EXIT: u8 = 2;

const MAX_FRAME: usize = 16 << 20;

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok((head[0], payload))
}

pub fn resize_payload(cols: u16, rows: u16) -> [u8; 4] {
    let (c, r) = (cols.to_be_bytes(), rows.to_be_bytes());
    [c[0], c[1], r[0], r[1]]
}

pub fn parse_resize(p: &[u8]) -> Option<(u16, u16)> {
    (p.len() == 4).then(|| (u16::from_be_bytes([p[0], p[1]]), u16::from_be_bytes([p[2], p[3]])))
}

/// True if the input consists only of automatic terminal replies (focus reports, device attributes, cursor
/// position, mode and keyboard queries, OSC colour answers, XTGETTCAP/XTVERSION), not of anything typed.
///
/// With several clients attached, every terminal sends these. If they counted as typing, two clients would
/// keep resizing the session to their own size, and the app would re-query after every resize, forever.
pub fn is_terminal_reply(p: &[u8]) -> bool {
    let mut i = 0;
    while i < p.len() {
        if p[i] != 0x1b || i + 1 >= p.len() {
            return false;
        }
        match p[i + 1] {
            b'[' => {
                let start = i + 2;
                let mut j = start;
                while j < p.len() && (0x20..=0x3f).contains(&p[j]) {
                    j += 1;
                }
                if j >= p.len() {
                    return false;
                }
                let params = &p[start..j];
                let ok = match p[j] {
                    b'I' | b'O' => params.is_empty(),
                    b'c' => params.first().is_some_and(|b| *b == b'?' || *b == b'>'),
                    b'R' => {
                        let n = params.strip_prefix(b"?").unwrap_or(params);
                        n.contains(&b';') && digits(n)
                    }
                    // XTWINOPS reports (cell size, pixel size, text area size, window state)
                    b't' => !params.is_empty() && digits(params),
                    // DSR (`CSI 0 n`, `CSI ? … n`)
                    b'n' => {
                        let n = params.strip_prefix(b"?").unwrap_or(params);
                        !n.is_empty() && digits(n)
                    }
                    b'y' => params.last() == Some(&b'$'),
                    b'u' => params.first() == Some(&b'?'),
                    _ => false,
                };
                if !ok {
                    return false;
                }
                i = j + 1;
            }
            // OSC / DCS: up to BEL or ST (ESC \)
            b']' | b'P' => {
                let mut j = i + 2;
                loop {
                    if j >= p.len() {
                        return false;
                    }
                    if p[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if p[j] == 0x1b && p.get(j + 1) == Some(&b'\\') {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j;
            }
            _ => return false,
        }
    }
    !p.is_empty()
}

fn digits(p: &[u8]) -> bool {
    p.iter().all(|b| b.is_ascii_digit() || *b == b';')
}

/// `$XDG_RUNTIME_DIR/pult.sock` (Linux), otherwise `$TMPDIR/pult-<uid>.sock` (macOS).
pub fn socket_path() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("pult.sock");
    }
    let tmp = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/tmp".into());
    tmp.join(format!("pult-{}.sock", unsafe { libc::getuid() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, DATA, b"hello").unwrap();
        write_frame(&mut buf, RESIZE, &resize_payload(120, 40)).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap(), (DATA, b"hello".to_vec()));
        let (k, p) = read_frame(&mut r).unwrap();
        assert_eq!((k, parse_resize(&p)), (RESIZE, Some((120, 40))));
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut head = vec![DATA];
        head.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
        assert!(read_frame(&mut &head[..]).is_err());
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, DATA, b"hello").unwrap();
        buf.truncate(buf.len() - 1);
        assert!(read_frame(&mut &buf[..]).is_err());
    }

    #[test]
    fn resize_payload_roundtrip() {
        assert_eq!(parse_resize(&resize_payload(1, 65535)), Some((1, 65535)));
        assert_eq!(parse_resize(&[0, 1, 2]), None);
    }

    #[test]
    fn terminal_replies() {
        for r in [
            &b"\x1b[I"[..],
            b"\x1b[O",
            b"\x1b[?62;22c",
            b"\x1b[>1;277;0c",
            b"\x1b[12;40R",
            b"\x1b[?2026;2$y",
            b"\x1b[?0u",
            b"\x1b[6;41;21t\x1b[?65;4;6;18;22;52c",
            b"\x1b[8;48;161t",
            b"\x1b[0n",
            b"\x1b[?12;40R",
            b"\x1b[4;2$y",
            b"\x1b]11;rgb:2d2d/2a2a/2e2e\x1b\\",
            b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[?62c",
            b"\x1bP>|WezTerm 2026\x1b\\",
        ] {
            assert!(is_terminal_reply(r), "{r:?}");
        }
    }

    #[test]
    fn typed_input_is_not_a_terminal_reply() {
        for k in [
            &b"a"[..],
            b"\r",
            b"\x1b",
            b"\x1b[A",
            b"\x1b[1;5C",
            b"\x1b[200~text\x1b[201~",
            b"\x1b[I\r",
            b"",
            b"\x1b[<0;10;5M",
            b"\x1b[t",
            b"\x1b[15~",
            b"\x1b]0;unterminated",
        ] {
            assert!(!is_terminal_reply(k), "{k:?}");
        }
    }
}
