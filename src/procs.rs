// Running processes (Toolhelp snapshot, full paths with PROCESS_QUERY_LIMITED_INFORMATION), what their .exe is
// called (version info), and which sockets each one owns (GetExtendedTcpTable / GetExtendedUdpTable). All
// read-only; without admin some paths of other users' and system processes can't be read.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::*;
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Threading::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// "brave.exe"
    pub name: String,
    /// Full path, when Windows lets us read it.
    pub path: Option<String>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

/// Full path of a process's .exe.
pub fn path_of(pid: u32) -> Option<String> {
    if pid == 0 || pid == 4 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Every running process. Paths are read for all of them (about 1-3 ms for 300 processes on a normal PC; an
/// estimate, not measured here yet).
pub fn list() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let pid = e.th32ProcessID;
            out.push(Proc { pid, ppid: e.th32ParentProcessID, name: from_wide(&e.szExeFile), path: path_of(pid) });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// The .exe's FileDescription ("Brave Browser"), or None.
pub fn describe(path: &str) -> Option<String> {
    unsafe {
        let p = wide(path);
        let size = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, size, data.as_mut_ptr() as *mut _).ok()?;
        // The first language/code page pair, then FileDescription in it.
        let mut tr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let q = wide(r"\VarFileInfo\Translation");
        let (lang, cp) = if VerQueryValueW(data.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut tr, &mut len).as_bool() && len >= 4 {
            let pair = std::slice::from_raw_parts(tr as *const u16, 2);
            (pair[0], pair[1])
        } else {
            (0x0409, 0x04B0)
        };
        let q = wide(&format!(r"\StringFileInfo\{:04x}{:04x}\FileDescription", lang, cp));
        let mut v: *mut core::ffi::c_void = std::ptr::null_mut();
        if !VerQueryValueW(data.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut v, &mut len).as_bool() || len == 0 {
            return None;
        }
        let s = from_wide(std::slice::from_raw_parts(v as *const u16, len as usize));
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    }
}

/// "brave.exe" -> "brave"; the UI's fallback name.
pub fn file_stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file).to_string()
}

/// A name for the UI: the description unless it's empty or generic, else the file name without .exe.
pub fn display_name(path: &str) -> String {
    match describe(path) {
        Some(d) if d.len() <= 40 && !d.eq_ignore_ascii_case("application") => d,
        _ => file_stem(path),
    }
}

/// Is this a Windows component (under %SystemRoot%)? The "Add an app" list hides those.
pub fn is_windows_path(path: &str) -> bool {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let p = path.to_lowercase();
    let r = root.to_lowercase();
    p.starts_with(&(r + "\\"))
}

/// Windows' own programs and Microsoft Defender: never pinned, even if a rule names them.
pub fn is_protected(path: &str) -> bool {
    let p = crate::config::norm(path);
    let under = |var: &str, sub: &str| {
        std::env::var(var).is_ok_and(|v| p.starts_with(&format!("{}\\{sub}\\", crate::config::norm(&v))))
    };
    is_windows_path(path) || under("ProgramFiles", "windows defender") || under("ProgramData", r"microsoft\windows defender")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sock {
    pub proto: Proto,
    pub local: SocketAddr,
    /// TCP only.
    pub remote: Option<SocketAddr>,
    /// MIB_TCP_STATE for TCP (2 = listen, 5 = established), 0 for UDP.
    pub state: u32,
    pub pid: u32,
}

const TCP_LISTEN: u32 = 2;
const TCP_TIME_WAIT: u32 = 11;

fn port(p: u32) -> u16 {
    u16::from_be(p as u16)
}

fn table(get: impl Fn(Option<*mut core::ffi::c_void>, *mut u32) -> u32) -> Option<Vec<u64>> {
    let mut size = 0u32;
    let r = get(None, &mut size);
    if r != ERROR_INSUFFICIENT_BUFFER.0 && r != NO_ERROR.0 {
        return None;
    }
    for _ in 0..4 {
        size += 4096; // the table can grow between the two calls
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        match get(Some(buf.as_mut_ptr() as *mut _), &mut size) {
            r if r == NO_ERROR.0 => return Some(buf),
            r if r == ERROR_INSUFFICIENT_BUFFER.0 => continue,
            _ => return None,
        }
    }
    None
}

unsafe fn rows<T: Copy>(buf: &[u64]) -> &[T] {
    let n = *(buf.as_ptr() as *const u32) as usize;
    // Rows start right after dwNumEntries, aligned for T (all these rows are 4-byte aligned).
    std::slice::from_raw_parts((buf.as_ptr() as *const u8).add(4) as *const T, n)
}

/// Every TCP and UDP socket on the machine with its owning PID (IPv4 and IPv6).
pub fn sockets() -> Vec<Sock> {
    let mut out = Vec::new();
    unsafe {
        if let Some(b) = table(|p, s| GetExtendedTcpTable(p, s, false, AF_INET.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)) {
            for r in rows::<MIB_TCPROW_OWNER_PID>(&b) {
                out.push(Sock {
                    proto: Proto::Tcp,
                    local: SocketAddr::new(IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwLocalAddr))), port(r.dwLocalPort)),
                    remote: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwRemoteAddr))), port(r.dwRemotePort))),
                    state: r.dwState,
                    pid: r.dwOwningPid,
                });
            }
        }
        if let Some(b) = table(|p, s| GetExtendedTcpTable(p, s, false, AF_INET6.0 as u32, TCP_TABLE_OWNER_PID_ALL, 0)) {
            for r in rows::<MIB_TCP6ROW_OWNER_PID>(&b) {
                out.push(Sock {
                    proto: Proto::Tcp,
                    local: SocketAddr::new(IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)), port(r.dwLocalPort)),
                    remote: Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(r.ucRemoteAddr)), port(r.dwRemotePort))),
                    state: r.dwState,
                    pid: r.dwOwningPid,
                });
            }
        }
        if let Some(b) = table(|p, s| GetExtendedUdpTable(p, s, false, AF_INET.0 as u32, UDP_TABLE_OWNER_PID, 0)) {
            for r in rows::<MIB_UDPROW_OWNER_PID>(&b) {
                out.push(Sock {
                    proto: Proto::Udp,
                    local: SocketAddr::new(IpAddr::V4(Ipv4Addr::from(u32::from_be(r.dwLocalAddr))), port(r.dwLocalPort)),
                    remote: None,
                    state: 0,
                    pid: r.dwOwningPid,
                });
            }
        }
        if let Some(b) = table(|p, s| GetExtendedUdpTable(p, s, false, AF_INET6.0 as u32, UDP_TABLE_OWNER_PID, 0)) {
            for r in rows::<MIB_UDP6ROW_OWNER_PID>(&b) {
                out.push(Sock {
                    proto: Proto::Udp,
                    local: SocketAddr::new(IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr)), port(r.dwLocalPort)),
                    remote: None,
                    state: 0,
                    pid: r.dwOwningPid,
                });
            }
        }
    }
    out
}

/// Is this socket a network connection the UI counts? TCP that isn't listening or closing and doesn't go to
/// loopback; UDP bound to a specific, non-loopback address (one on 0.0.0.0 could leave by either adapter).
pub fn is_connection(s: &Sock) -> bool {
    match s.proto {
        Proto::Tcp => {
            s.state != TCP_LISTEN
                && s.state != TCP_TIME_WAIT
                && !s.remote.is_some_and(|r| r.ip().is_loopback() || r.ip().is_unspecified())
        }
        Proto::Udp => !s.local.ip().is_unspecified() && !s.local.ip().is_loopback(),
    }
}

/// Closes one IPv4 TCP connection (SetTcpEntry, DELETE_TCB; needs admin). The app sees a reset and, being pinned,
/// reconnects over Wi-Fi. Windows has no such call for IPv6.
pub fn close_tcp(s: &Sock) -> bool {
    let (std::net::SocketAddr::V4(l), Some(std::net::SocketAddr::V4(r))) = (s.local, s.remote) else { return false };
    if s.proto != Proto::Tcp {
        return false;
    }
    let row = MIB_TCPROW_LH {
        Anonymous: MIB_TCPROW_LH_0 { State: MIB_TCP_STATE_DELETE_TCB },
        dwLocalAddr: u32::from(*l.ip()).to_be(),
        dwLocalPort: l.port().to_be() as u32,
        dwRemoteAddr: u32::from(*r.ip()).to_be(),
        dwRemotePort: r.port().to_be() as u32,
    };
    unsafe { SetTcpEntry(&row) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(file_stem(r"C:\x\brave.exe"), "brave");
        assert_eq!(file_stem("Discord.EXE"), "Discord");
        assert!(is_windows_path(&format!(r"{}\System32\svchost.exe", std::env::var("SystemRoot").unwrap())));
        assert!(!is_windows_path(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"));
        assert!(!is_windows_path(r"C:\WindowsApps2\x.exe"));
        let pd = std::env::var("ProgramData").unwrap();
        assert!(is_protected(&format!(r"{pd}\Microsoft\Windows Defender\Platform\4.18\MsMpEng.exe")));
        assert!(is_protected(&format!(r"{}\System32\svchost.exe", std::env::var("SystemRoot").unwrap())));
        assert!(!is_protected(r"C:\Users\R\AppData\Local\BraveSoftware\Brave-Browser\Application\brave.exe"));
    }

    #[test]
    fn connections_skip_listeners_loopback_and_unbound_udp() {
        let a = |s: &str| s.parse::<SocketAddr>().unwrap();
        let tcp = |l: &str, r: &str, state| Sock { proto: Proto::Tcp, local: a(l), remote: Some(a(r)), state, pid: 7 };
        let udp = |l: &str| Sock { proto: Proto::Udp, local: a(l), remote: None, state: 0, pid: 7 };
        assert!(is_connection(&tcp("192.168.1.20:50000", "1.2.3.4:443", 5)));
        assert!(!is_connection(&tcp("0.0.0.0:80", "0.0.0.0:0", 2)));
        assert!(!is_connection(&tcp("127.0.0.1:5000", "127.0.0.1:5001", 5)));
        assert!(is_connection(&udp("192.168.0.34:60000")));
        assert!(!is_connection(&udp("0.0.0.0:5353")));
        assert!(!is_connection(&udp("127.0.0.1:9")));
    }

    #[test]
    fn reads_this_pc_read_only() {
        let me = std::process::id();
        let list = list();
        let mine = list.iter().find(|p| p.pid == me).expect("own process listed");
        assert!(mine.path.as_deref().unwrap_or("").to_lowercase().ends_with(".exe"));
        let _ = sockets();
    }
}
