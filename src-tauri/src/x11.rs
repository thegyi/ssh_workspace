//! X11 forwarding emulated with SSH remote port forwarding.
//!
//! libssh2 cannot accept inbound "x11" channels, so we do what `ssh -Y`
//! does by hand: a dedicated second SSH session asks the server to listen
//! on 127.0.0.1:<port>; every inbound connection is proxied to the local
//! X server. The X11 setup packet is rewritten to carry the local
//! MIT-MAGIC-COOKIE-1, so the remote side needs no xauth setup at all.

use crate::hosts::Host;
use crate::ssh::{establish, SshCommand};
use ssh2::Channel;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

#[derive(Clone)]
enum XTarget {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(String, u16),
}

/// Entry point: runs on its own thread for the lifetime of the shell
/// session (until `alive` flips false or the connection dies).
pub fn start(
    app: AppHandle,
    data_event: String,
    host: Host,
    secret: Option<String>,
    jump: Option<(Host, Option<String>)>,
    known_hosts: PathBuf,
    tx: Sender<SshCommand>,
    alive: Arc<AtomicBool>,
) {
    let note = |msg: String| {
        let _ = app.emit(
            &data_event,
            format!("\r\n\x1b[33m[x11] {msg}\x1b[0m\r\n").into_bytes(),
        );
    };
    let run = |note: &dyn Fn(String)| -> Result<(), String> {
        let (target, num) = local_display()?;
        let cookie = xauthority_cookie(&num);
        let (sess, _) = establish(
            &host,
            secret,
            jump.as_ref().map(|(h, s)| (h, s.clone())),
            Some(&app),
            &known_hosts,
        )?;
        // Still in blocking mode here: the listen request needs a server
        // round-trip, which would fail with WouldBlock if sent nonblocking.
        let (mut listener, port) = sess
            .channel_forward_listen(0, Some("127.0.0.1"), Some(8))
            .map_err(|e| format!("remote forwarding refused by server: {e}"))?;
        sess.set_blocking(false);
        if port <= 6000 {
            return Err(format!("server bound unexpected port {port}"));
        }
        // X display N maps to TCP port 6000+N.
        let remote_display = format!("127.0.0.1:{}.0", port - 6000);
        let _ = tx.send(SshCommand::Data(
            format!("export DISPLAY={remote_display}\n").into_bytes(),
        ));
        note(format!(
            "forwarding remote 127.0.0.1:{port} to local :{num} (DISPLAY={remote_display})"
        ));
        let mut last_ka = Instant::now();
        while alive.load(Ordering::Relaxed) {
            if last_ka.elapsed() >= Duration::from_secs(15) {
                last_ka = Instant::now();
                if let Err(e) = sess.keepalive_send().map_err(std::io::Error::from) {
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        break;
                    }
                }
            }
            match listener.accept() {
                Ok(chan) => {
                    let (t, c, a) = (target.clone(), cookie.clone(), alive.clone());
                    thread::spawn(move || proxy(chan, t, c, a));
                }
                Err(e) => {
                    let io: std::io::Error = e.into();
                    if io.kind() == std::io::ErrorKind::WouldBlock {
                        thread::sleep(Duration::from_millis(10));
                    } else {
                        return Err(format!("forward listener failed: {io}"));
                    }
                }
            }
        }
        Ok(())
    };
    if let Err(e) = run(&note) {
        note(format!("forwarding unavailable: {e}"));
    }
}

enum XStream {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for XStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            XStream::Unix(s) => s.read(buf),
            XStream::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for XStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            XStream::Unix(s) => s.write(buf),
            XStream::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl XStream {
    fn connect(target: &XTarget) -> std::io::Result<Self> {
        match target {
            #[cfg(unix)]
            XTarget::Unix(p) => UnixStream::connect(p).map(XStream::Unix),
            XTarget::Tcp(h, p) => TcpStream::connect((h.as_str(), *p)).map(XStream::Tcp),
        }
    }
    fn set_nonblocking(&self, on: bool) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            XStream::Unix(s) => s.set_nonblocking(on),
            XStream::Tcp(s) => s.set_nonblocking(on),
        }
    }
}

/// One forwarded X11 connection: rewrite the setup packet (inject the local
/// cookie), then splice bytes in both directions on a nonblocking session.
fn proxy(mut chan: Channel, target: XTarget, cookie: Option<Vec<u8>>, alive: Arc<AtomicBool>) {
    let mut stream = match XStream::connect(&target) {
        Ok(s) => s,
        Err(_) => return,
    };
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let mut hs: Vec<u8> = Vec::new();
    let mut hs_done = cookie.is_none(); // no cookie → pass through untouched
    let mut to_local: VecDeque<u8> = VecDeque::new();
    let mut to_remote: VecDeque<u8> = VecDeque::new();
    let mut cbuf = [0u8; 65536];
    let mut sbuf = [0u8; 65536];
    let mut s_eof = false;
    loop {
        if !alive.load(Ordering::Relaxed) {
            return;
        }
        let mut active = false;
        let c_eof = chan.eof();

        if !c_eof {
            match chan.read(&mut cbuf) {
                Ok(0) => {}
                Ok(n) => {
                    active = true;
                    if hs_done {
                        to_local.extend(&cbuf[..n]);
                    } else {
                        hs.extend(&cbuf[..n]);
                        if let Some(pkt) = rewrite_setup(&hs, cookie.as_deref().unwrap_or(&[])) {
                            to_local.extend(pkt);
                            hs_done = true;
                            hs.clear();
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        }
        if !s_eof {
            match stream.read(&mut sbuf) {
                Ok(0) => s_eof = true,
                Ok(n) => {
                    active = true;
                    to_remote.extend(&sbuf[..n]);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        }
        while !to_local.is_empty() {
            match stream.write(to_local.make_contiguous()) {
                Ok(0) => break,
                Ok(n) => {
                    to_local.drain(..n);
                    active = true;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
        while !to_remote.is_empty() {
            match chan.write(to_remote.make_contiguous()) {
                Ok(0) => break,
                Ok(n) => {
                    to_remote.drain(..n);
                    active = true;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
        if chan.eof() && s_eof && to_local.is_empty() && to_remote.is_empty() {
            return;
        }
        if !active {
            thread::sleep(Duration::from_millis(4));
        }
    }
}

/// Once the fixed 12-byte X11 setup header plus the client-supplied
/// auth name/data have arrived, replace them with MIT-MAGIC-COOKIE-1 and
/// the local cookie. Returns None until the full packet is buffered.
fn rewrite_setup(hs: &[u8], cookie: &[u8]) -> Option<Vec<u8>> {
    if hs.len() < 12 {
        return None;
    }
    let be = hs[0] == b'B';
    let rd = |o: usize| -> u16 {
        if be {
            u16::from_be_bytes([hs[o], hs[o + 1]])
        } else {
            u16::from_le_bytes([hs[o], hs[o + 1]])
        }
    };
    let name_len = rd(6) as usize;
    let data_len = rd(8) as usize;
    let total = 12 + pad4(name_len) + pad4(data_len);
    if hs.len() < total {
        return None;
    }
    const NAME: &[u8] = b"MIT-MAGIC-COOKIE-1";
    let mut out = Vec::with_capacity(12 + pad4(NAME.len()) + pad4(cookie.len()) + hs.len() - total);
    out.extend_from_slice(&hs[..6]); // byte order, pad, protocol version
    let wr = |out: &mut Vec<u8>, v: u16| {
        if be {
            out.extend_from_slice(&v.to_be_bytes());
        } else {
            out.extend_from_slice(&v.to_le_bytes());
        }
    };
    wr(&mut out, NAME.len() as u16);
    wr(&mut out, cookie.len() as u16);
    out.extend_from_slice(&hs[10..12]);
    out.extend_from_slice(NAME);
    out.resize(12 + pad4(NAME.len()), 0);
    out.extend_from_slice(cookie);
    out.resize(12 + pad4(NAME.len()) + pad4(cookie.len()), 0);
    out.extend_from_slice(&hs[total..]); // bytes already read past the setup packet
    Some(out)
}

fn pad4(n: usize) -> usize {
    n.div_ceil(4) * 4
}

/// Resolve the local X endpoint: prefer DISPLAY; on unix fall back to the
/// first /tmp/.X11-unix socket (covers Wayland/XWayland when DISPLAY did
/// not reach the app environment). On Windows there is no DISPLAY — use
/// the VcXsrv/Xming default 127.0.0.1:6000.
fn local_display() -> Result<(XTarget, String), String> {
    if let Ok(d) = std::env::var("DISPLAY") {
        if !d.trim().is_empty() {
            return parse_display(&d);
        }
    }
    #[cfg(unix)]
    {
        if let Ok(rd) = std::fs::read_dir("/tmp/.X11-unix") {
            for e in rd.flatten() {
                let fname = e.file_name();
                let Some(num) = fname.to_str().and_then(|n| n.strip_prefix('X')) else {
                    continue;
                };
                if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
                    return Ok((XTarget::Unix(e.path()), num.to_string()));
                }
            }
        }
        return Err("no local X display found (DISPLAY unset, /tmp/.X11-unix empty)".into());
    }
    #[cfg(not(unix))]
    Ok((XTarget::Tcp("127.0.0.1".into(), 6000), "0".into()))
}

fn parse_display(d: &str) -> Result<(XTarget, String), String> {
    let d = d.trim();
    let (host_part, rest) = d
        .split_once(':')
        .ok_or_else(|| format!("cannot parse DISPLAY {d:?}"))?;
    let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num.is_empty() {
        return Err(format!("cannot parse DISPLAY {d:?}"));
    }
    if host_part.is_empty() || host_part == "unix" || host_part == "localhost" {
        #[cfg(unix)]
        {
            return Ok((
                XTarget::Unix(PathBuf::from(format!("/tmp/.X11-unix/X{num}"))),
                num,
            ));
        }
        #[cfg(not(unix))]
        {
            let n: u16 = num
                .parse()
                .map_err(|_| format!("bad display number in {d:?}"))?;
            let port = 6000u16
                .checked_add(n)
                .ok_or_else(|| format!("display number too large in {d:?}"))?;
            return Ok((XTarget::Tcp("127.0.0.1".into(), port), num));
        }
    }
    let n: u16 = num
        .parse()
        .map_err(|_| format!("bad display number in {d:?}"))?;
    let port = 6000u16
        .checked_add(n)
        .ok_or_else(|| format!("display number too large in {d:?}"))?;
    Ok((XTarget::Tcp(host_part.to_string(), port), num))
}

/// Find the MIT-MAGIC-COOKIE-1 for the display in XAUTHORITY (~/.Xauthority
/// fallback). Records: u16be family, then four length-prefixed strings
/// (address, display number, auth name, auth data).
fn xauthority_cookie(display_num: &str) -> Option<Vec<u8>> {
    let path = std::env::var_os("XAUTHORITY")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".Xauthority")))?;
    let data = std::fs::read(path).ok()?;
    let mut p: &[u8] = &data;
    let mut wildcard = None;
    while let Some((family, _addr, num, name, cookie)) = read_record(&mut p) {
        if name != b"MIT-MAGIC-COOKIE-1" {
            continue;
        }
        if num == display_num.as_bytes() {
            return Some(cookie.to_vec());
        }
        if family == 0xffff && wildcard.is_none() {
            wildcard = Some(cookie.to_vec());
        }
    }
    wildcard
}

fn read_record<'a>(p: &mut &'a [u8]) -> Option<(u16, &'a [u8], &'a [u8], &'a [u8], &'a [u8])> {
    let family = take_u16(p)?;
    let addr = take_str(p)?;
    let num = take_str(p)?;
    let name = take_str(p)?;
    let cookie = take_str(p)?;
    Some((family, addr, num, name, cookie))
}

fn take_u16(p: &mut &[u8]) -> Option<u16> {
    let b = p.get(..2)?;
    *p = &p[2..];
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn take_str<'a>(p: &mut &'a [u8]) -> Option<&'a [u8]> {
    let n = take_u16(p)? as usize;
    let b = p.get(..n)?;
    *p = &p[n..];
    Some(b)
}
