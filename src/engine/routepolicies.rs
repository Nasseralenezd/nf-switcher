// Windows' global "Route Policies" TCP/IP option (netsh interface ipv4|ipv6 set global routepolicies=...). Connection
// policies are accepted but ignored while it is off (the default): measured with --selftest on build 26200,
// 2026-10-18. NF Switcher only changes it with store=active, which Windows forgets at the next boot, and only while
// it has policies in place. Before changing a family (IPv4, IPv6) it writes what it found to routepolicies.json in
// its admin-only data folder; when done, or at the next start after a crash, it turns off only the families that
// were off, and deletes the file. Set NFSW_ROUTEPOLICIES=leave to never touch it (tests only).
use super::wfp::RoutePolicies;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Family::V4 => "ipv4",
            Family::V6 => "ipv6",
        }
    }
}

/// "Route Policies : enabled" from `netsh interface <family> show global`. None when the line isn't there (a
/// Windows in another language, or an old build).
pub fn parse(output: &str) -> Option<bool> {
    let line = output.lines().find(|l| l.trim_start().to_ascii_lowercase().starts_with("route policies"))?;
    let v = line.split(':').nth(1)?.trim().to_ascii_lowercase();
    match v.as_str() {
        "enabled" => Some(true),
        "disabled" => Some(false),
        _ => None,
    }
}

/// netsh.exe from System32 by full path: an elevated app must not pick up a netsh.exe planted next to itself.
fn netsh_exe() -> PathBuf {
    let mut buf = [0u16; 260];
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    let dir = if n > 0 && n < buf.len() { String::from_utf16_lossy(&buf[..n]) } else { r"C:\Windows\System32".into() };
    PathBuf::from(dir).join("netsh.exe")
}

fn netsh(args: &[&str]) -> Result<String, String> {
    let out = Command::new(netsh_exe())
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("netsh: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() { Ok(text) } else { Err(format!("netsh {}: {}", args.join(" "), text.trim())) }
}

fn show(f: Family) -> Result<String, String> {
    netsh(&["interface", f.name(), "show", "global"])
}

fn set(f: Family, on: bool) -> Result<(), String> {
    let v = if on { "routepolicies=enabled" } else { "routepolicies=disabled" };
    netsh(&["interface", f.name(), "set", "global", v, "store=active"]).map(|_| ())
}

/// Both families on? None when it can't be read (only used for reports).
pub fn read() -> Option<bool> {
    let v4 = parse(&show(Family::V4).ok()?)?;
    let v6 = parse(&show(Family::V6).ok()?)?;
    Some(v4 && v6)
}

/// What each family was before NF Switcher turned it on: "off", "on" or "unknown"; missing = not touched.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
struct Journal {
    v4: Option<String>,
    v6: Option<String>,
}

impl Journal {
    fn get(&self, f: Family) -> Option<&str> {
        match f {
            Family::V4 => self.v4.as_deref(),
            Family::V6 => self.v6.as_deref(),
        }
    }
    fn put(&mut self, f: Family, v: &str) {
        let slot = match f {
            Family::V4 => &mut self.v4,
            Family::V6 => &mut self.v6,
        };
        *slot = Some(v.to_string());
    }
}

pub struct Netsh {
    path: PathBuf,
    /// What to put back (this run's journal, or one a crashed run left behind).
    journal: Journal,
    /// This run has made sure the option is on.
    on: bool,
    leave: bool,
}

impl Netsh {
    pub fn new() -> Netsh {
        let path = crate::config::data_dir().join("routepolicies.json");
        let journal = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let leave = std::env::var("NFSW_ROUTEPOLICIES").is_ok_and(|v| v == "leave");
        Netsh { path, journal, on: false, leave }
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string(&self.journal).unwrap_or_default();
        std::fs::write(&self.path, text).map_err(|e| format!("saving {}: {e}", self.path.display()))
    }
}

impl RoutePolicies for Netsh {
    fn need(&mut self) -> Result<(), String> {
        if self.leave || self.on {
            return Ok(());
        }
        // Always look at the real state: a reboot turns it off whatever an old journal says.
        for f in [Family::V4, Family::V6] {
            let before = show(f)?;
            let state = parse(&before);
            if state == Some(true) {
                continue;
            }
            // Write down what we found before changing anything, and never overwrite an earlier record (after a
            // crash it holds the real original).
            if self.journal.get(f).is_none() {
                self.journal.put(f, if state == Some(false) { "off" } else { "unknown" });
                self.save()?;
            }
            set(f, true)?;
            if state.is_none() && self.journal.get(f) == Some("unknown") {
                // Not English: if turning it on changed the output, it was off.
                let after = show(f)?;
                self.journal.put(f, if after != before { "off" } else { "on" });
                self.save()?;
            }
        }
        self.on = true;
        Ok(())
    }

    fn release(&mut self) {
        let j = std::mem::take(&mut self.journal);
        for f in [Family::V4, Family::V6] {
            if j.get(f) == Some("off") {
                let _ = set(f, false);
            }
        }
        if j != Journal::default() {
            let _ = std::fs::remove_file(&self.path);
        }
        self.on = false;
    }
}

impl Drop for Netsh {
    // Also on a panic: put the option back rather than leave it on until the next boot.
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netsh_output() {
        let out = "\nGeneral Global Parameters\n---------------------------------------------\n\
                   Default Hop Limit                   : 128 hops\nRoute Policies                      : disabled\n";
        assert_eq!(parse(out), Some(false));
        assert_eq!(parse("Route Policies : enabled"), Some(true));
        assert_eq!(parse("Richtlinien : aktiviert"), None);
    }

    #[test]
    fn netsh_comes_from_system32() {
        let p = netsh_exe();
        assert!(p.to_string_lossy().to_lowercase().ends_with(r"system32\netsh.exe"), "{}", p.display());
        assert!(p.is_absolute());
    }

    #[test]
    fn journal_keeps_families_apart() {
        let mut j = Journal::default();
        j.put(Family::V6, "off");
        assert_eq!(j.get(Family::V4), None);
        assert_eq!(j.get(Family::V6), Some("off"));
        let back: Journal = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
        assert_eq!(back, j);
    }

    #[test]
    fn reads_this_pc_read_only() {
        // `show global` only reads. This PC had it disabled on 2026-10-18.
        let _ = read();
    }
}
