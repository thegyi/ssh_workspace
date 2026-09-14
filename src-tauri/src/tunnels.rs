//! Port forwarding for a host: local (-L), remote (-R) and dynamic
//! SOCKS5 (-D) tunnels. Every forwarded connection gets its own SSH
//! session — the ssh2 crate ties `Channel` lifetimes to their `Session`,
//! so multiplexing channels on one session across threads is not
//! possible with the safe API.

use crate::hosts::{Host, Tunnel, TunnelKind};
use crate::ssh::{establish, splice_channel};
use ssh2::Channel;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

#[derive(Clone)]
struct Ctx {
    host: Host,
    secret: Option<String>,
    jump: Option<(Host, Option<String>)>,
    known_hosts: PathBuf,
    alive: Arc<AtomicBool>,
    app: AppHandle,
    note: Arc<dyn Fn(String) + Send + Sync>,
}

impl Ctx {
    fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
    fn say(&self, msg: impl Into<String>) {
        (self.note)(msg.into());
    }
    fn jump_ref(&self) -> Option<(&Host, Option<String>)> {
        self.jump
            .as_ref()
            .map(|(h, s)| (h, s.clone()))
    }
}

/// Spawn one thread per configured tunnel. All of them stop when the
/// owning shell session ends (`alive` flips false).
pub fn start_all(
    app: AppHandle,
    data_event: String,
    host: Host,
    secret: Option<String>,
    jump: Option<(Host, Option<String>)>,
    known_hosts: PathBuf,
    alive: Arc<AtomicBool>,
) {
    if host.tunnels.is_empty() {
        return;
    }
    let ctx = Ctx {
        host,
        secret,
        jump,
        known_hosts,
        alive,
        app: app.clone(),
        note: Arc::new(move |msg: String| {
            let _ = app.emit(
                &data_event,
                format!("\r\n\x1b[36m[tunnel] {msg}\x1b[0m\r\n").into_bytes(),
            );
        }),
    };
    for t in ctx.host.tunnels.clone() {
        let c = ctx.clone();
        thread::spawn(move || {
            let res = match t.kind {
                TunnelKind::Local => run_local(&c, &t),
                TunnelKind::Remote => run_remote(&c, &t),
                TunnelKind::Dynamic => run_dynamic(&c, &t),
            };
            match res {
                Ok(desc) => c.say(format!("{desc} closed")),
                Err(e) => c.say(format!("unavailable: {e}")),
            }
        });
    }
}

/// -L: local TCP listener; each connection opens a fresh SSH session and a
/// direct-tcpip channel to the target.
fn run_local(ctx: &Ctx, t: &Tunnel) -> Result<String, String> {
    let listener = TcpListener::bind((t.bind.as_str(), t.listen_port))
        .map_err(|e| format!("cannot listen on {}:{}: {e}", t.bind, t.listen_port))?;
    listener.set_nonblocking(true).ok();
    let desc = format!(
        "local {}:{} → {}:{}",
        t.bind, t.listen_port, t.target_host, t.target_port
    );
    ctx.say(format!("listening {desc}"));
    while ctx.alive() {
        match listener.accept() {
            Ok((sock, _)) => {
                let (c, t) = (ctx.clone(), t.clone());
                thread::spawn(move || serve(&c, sock, &t.target_host, t.target_port));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(format!("local listener failed: {e}")),
        }
    }
    Ok(desc)
}

/// -R: remote listener on the SSH server; each accepted channel is proxied
/// to the local target. Needs AllowTcpForwarding on the server.
fn run_remote(ctx: &Ctx, t: &Tunnel) -> Result<String, String> {
    let (sess, _) = establish(
        &ctx.host,
        ctx.secret.clone(),
        ctx.jump_ref(),
        Some(&ctx.app),
        &ctx.known_hosts,
    )?;
    // Still blocking here: the listen request needs a server round-trip.
    let (mut listener, bound) = sess
        .channel_forward_listen(t.listen_port, Some(&t.bind), None)
        .map_err(|e| format!("remote listen {}:{} refused: {e}", t.bind, t.listen_port))?;
    sess.set_blocking(false);
    let desc = format!(
        "remote {}:{} → {}:{}",
        t.bind, bound, t.target_host, t.target_port
    );
    ctx.say(format!("listening {desc}"));
    let mut last_ka = Instant::now();
    while ctx.alive() {
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
                let c = ctx.clone();
                let (th, tp) = (t.target_host.clone(), t.target_port);
                thread::spawn(move || proxy_to_local(chan, &c, &th, tp));
            }
            Err(e) => {
                let io: std::io::Error = e.into();
                if io.kind() == std::io::ErrorKind::WouldBlock {
                    thread::sleep(Duration::from_millis(10));
                } else {
                    return Err(format!("remote listener failed: {io}"));
                }
            }
        }
    }
    Ok(desc)
}

fn proxy_to_local(mut chan: Channel, ctx: &Ctx, host: &str, port: u16) {
    let mut sock = match TcpStream::connect((host, port)) {
        Ok(s) => s,
        Err(e) => {
            ctx.say(format!("remote tunnel connect to {host}:{port} failed: {e}"));
            return;
        }
    };
    sock.set_nonblocking(true).ok();
    splice_channel(&mut chan, &mut sock, &ctx.alive);
}

/// -D: local SOCKS5 server; each CONNECT opens a direct-tcpip channel to
/// the requested destination on a fresh SSH session.
fn run_dynamic(ctx: &Ctx, t: &Tunnel) -> Result<String, String> {
    let listener = TcpListener::bind((t.bind.as_str(), t.listen_port))
        .map_err(|e| format!("cannot listen on {}:{}: {e}", t.bind, t.listen_port))?;
    listener.set_nonblocking(true).ok();
    let desc = format!("socks5 {}:{}", t.bind, t.listen_port);
    ctx.say(format!("listening {desc}"));
    while ctx.alive() {
        match listener.accept() {
            Ok((mut sock, _)) => {
                let c = ctx.clone();
                thread::spawn(move || {
                    sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
                    match socks5_target(&mut sock) {
                        Ok((h, p)) => serve(&c, sock, &h, p),
                        Err(e) => c.say(format!("socks5 handshake failed: {e}")),
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(format!("socks5 listener failed: {e}")),
        }
    }
    Ok(desc)
}

/// Open a channel to `host:port` on a fresh SSH session and splice it to
/// the local socket until either side closes.
fn serve(ctx: &Ctx, mut sock: TcpStream, host: &str, port: u16) {
    let mut run = || -> Result<(), String> {
        sock.set_read_timeout(None).ok();
        sock.set_nonblocking(true).ok();
        let (sess, _) = establish(
            &ctx.host,
            ctx.secret.clone(),
            ctx.jump_ref(),
            Some(&ctx.app),
            &ctx.known_hosts,
        )?;
        let mut chan = sess
            .channel_direct_tcpip(host, port, None)
            .map_err(|e| format!("channel to {host}:{port} refused: {e}"))?;
        sess.set_blocking(false);
        splice_channel(&mut chan, &mut sock, &ctx.alive);
        Ok(())
    };
    if let Err(e) = run() {
        ctx.say(e);
    }
}

/// Minimal SOCKS5 server handshake (no-auth, CONNECT only).
/// Returns the requested destination.
fn socks5_target(sock: &mut TcpStream) -> std::io::Result<(String, u16)> {
    let mut h = [0u8; 2];
    sock.read_exact(&mut h)?;
    if h[0] != 5 {
        return Err(std::io::Error::other("not a SOCKS5 greeting"));
    }
    let mut m = vec![0u8; h[1] as usize];
    sock.read_exact(&mut m)?;
    if !m.contains(&0) {
        return Err(std::io::Error::other("client requires authentication"));
    }
    sock.write_all(&[5, 0])?; // no-auth
    let mut r = [0u8; 4];
    sock.read_exact(&mut r)?;
    if r[0] != 5 || r[1] != 1 {
        return Err(std::io::Error::other("only CONNECT is supported"));
    }
    let host = match r[3] {
        1 => {
            let mut a = [0u8; 4];
            sock.read_exact(&mut a)?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l)?;
            let mut d = vec![0u8; l[0] as usize];
            sock.read_exact(&mut d)?;
            String::from_utf8_lossy(&d).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            sock.read_exact(&mut a)?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => return Err(std::io::Error::other("bad address type")),
    };
    let mut p = [0u8; 2];
    sock.read_exact(&mut p)?;
    sock.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0])?; // success
    Ok((host, u16::from_be_bytes(p)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Drive socks5_target against a scripted client over loopback; returns
    /// the parsed (host, port) and everything the server wrote back.
    fn socks_roundtrip(req: &[u8]) -> std::io::Result<((String, u16), Vec<u8>)> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        client.write_all(req).unwrap();
        let res = socks5_target(&mut server);
        // Closing the server side lets the client read the full reply.
        drop(server);
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        res.map(|hp| (hp, reply))
    }

    #[test]
    fn socks5_ipv4_connect() {
        // greeting: v5, 1 method (no-auth); request: CONNECT 1.2.3.4:80
        let req = [5, 1, 0, 5, 1, 0, 1, 1, 2, 3, 4, 0, 80];
        let ((host, port), reply) = socks_roundtrip(&req).unwrap();
        assert_eq!(host, "1.2.3.4");
        assert_eq!(port, 80);
        // method pick [5,0] + success reply [5,0,0,1,0,0,0,0,0,0]
        assert_eq!(reply, vec![5, 0, 5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn socks5_domain_connect() {
        let mut req = vec![5, 1, 0, 5, 1, 0, 3, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        let ((host, port), _) = socks_roundtrip(&req).unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn socks5_rejects_auth_only_client() {
        // Client offers only method 2 (user/pass) — no no-auth method.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(&[5, 1, 2]).unwrap();
        assert!(socks5_target(&mut server).is_err());
    }

    #[test]
    fn socks5_rejects_wrong_version() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        client.write_all(&[4, 1, 0]).unwrap();
        assert!(socks5_target(&mut server).is_err());
    }
}
