// Test helper, not shipped: shows which local address (so which adapter) Windows picked for a connection.
// Copied into two folders by --selftest, one pinned in NF Switcher and one not.
//   probe.exe [tcp host:port] [udp dns-server:53]   one TCP exchange and one DNS query
//   probe.exe loop N host:port                      a new TCP connection every 600 ms, N times
//   probe.exe hold N host:port                      one TCP connection, N HTTP keep-alive requests 700 ms apart
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

fn resolve(s: &str) -> Option<SocketAddr> {
    s.to_socket_addrs().ok()?.next()
}

/// A DNS query for example.com, type A.
fn dns_query() -> Vec<u8> {
    let mut q = vec![0x4e, 0x46, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["example", "com"] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    q
}

/// Sends a HEAD request and reads the response headers. Returns the status line.
fn head(s: &mut TcpStream, host: &str) -> std::io::Result<String> {
    let req = format!("HEAD / HTTP/1.1\r\nHost: {host}\r\nConnection: keep-alive\r\n\r\n");
    s.write_all(req.as_bytes())?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if s.read(&mut byte)? == 0 {
            return Err(std::io::Error::other("closed"));
        }
        buf.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&buf).lines().next().unwrap_or("").to_string())
}

fn local(s: &TcpStream) -> String {
    s.local_addr().map(|a| a.to_string()).unwrap_or_default()
}

fn once(tcp: &str, udp: &str) {
    match resolve(tcp) {
        None => println!("TCP can't resolve {tcp}"),
        Some(addr) => match TcpStream::connect_timeout(&addr, Duration::from_secs(6)) {
            Ok(mut s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(6)));
                let host = tcp.rsplit_once(':').map(|(h, _)| h).unwrap_or(tcp);
                let first = head(&mut s, host).unwrap_or_default();
                println!("TCP local={} remote={} answer={:?}", local(&s), addr, first);
            }
            Err(e) => println!("TCP remote={addr} error={e}"),
        },
    }
    match resolve(udp) {
        None => println!("UDP can't resolve {udp}"),
        Some(addr) => {
            let bind = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
            let r = UdpSocket::bind(bind).and_then(|u| {
                u.connect(addr)?;
                u.set_read_timeout(Some(Duration::from_secs(4)))?;
                u.send(&dns_query())?;
                let mut buf = [0u8; 512];
                let got = u.recv(&mut buf).map(|n| n > 0).unwrap_or(false);
                Ok((u.local_addr()?, got))
            });
            match r {
                Ok((local, got)) => println!("UDP local={local} remote={addr} answer={got}"),
                Err(e) => println!("UDP remote={addr} error={e}"),
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n = |i: usize| args.get(i).and_then(|s| s.parse::<u32>().ok()).unwrap_or(8);
    let target = |i: usize| args.get(i).cloned().unwrap_or_else(|| "1.1.1.1:80".into());
    match args.first().map(String::as_str) {
        Some("loop") => {
            let addr = resolve(&target(2)).expect("address");
            for i in 0..n(1) {
                match TcpStream::connect_timeout(&addr, Duration::from_secs(4)) {
                    Ok(s) => println!("LOOP {i} local={} ok", local(&s)),
                    Err(e) => println!("LOOP {i} error={e}"),
                }
                std::thread::sleep(Duration::from_millis(600));
            }
        }
        Some("hold") => {
            let t = target(2);
            let host = t.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_default();
            match TcpStream::connect_timeout(&resolve(&t).expect("address"), Duration::from_secs(6)) {
                Ok(mut s) => {
                    let _ = s.set_read_timeout(Some(Duration::from_secs(4)));
                    for i in 0..n(1) {
                        match head(&mut s, &host) {
                            Ok(status) => println!("HOLD {i} local={} ok {status:?}", local(&s)),
                            Err(e) => println!("HOLD {i} local={} error={e}", local(&s)),
                        }
                        std::thread::sleep(Duration::from_millis(700));
                    }
                }
                Err(e) => println!("HOLD connect error={e}"),
            }
        }
        _ => {
            let tcp = args.first().cloned().unwrap_or_else(|| "example.com:80".into());
            let udp = args.get(1).cloned().unwrap_or_else(|| "1.1.1.1:53".into());
            once(&tcp, &udp);
        }
    }
}
