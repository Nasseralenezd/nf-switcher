// What NF Switcher wants Windows to do, worked out from the saved settings and the adapters' state (plan()), and the
// backend that does it (Engine). The plan is pure and unit-tested; wfp.rs is the real backend.
use crate::config::{norm, same_app, Config, WifiDown};
use crate::net::{Iface, Kind};
use std::net::Ipv4Addr;

pub mod routepolicies;
pub mod wfp;

/// The real engine: WFP connection policies plus the routepolicies option. Needs admin.
pub fn open_real() -> Result<Box<dyn Engine>, EngineError> {
    let b = wfp::real::RealWfp::open()?;
    Ok(Box::new(wfp::WfpEngine::new(b, routepolicies::Netsh::new())))
}

/// Can this Windows pin apps at all (Windows 11 24H2 and later)? Needs no admin, so a too-old Windows is told
/// apart before asking for UAC.
pub fn supported() -> bool {
    wfp::real::policy_functions().is_some()
}

/// This Windows as people know it, for the "needs Windows 11 24H2" message.
#[derive(Clone, Debug, PartialEq)]
pub struct ThisWindows {
    /// "Windows 11 23H2", "Windows 10 LTSC 21H2", "Windows Server 2022 Standard".
    pub name: String,
    /// Windows 10: the way forward is an upgrade to Windows 11, not an update.
    pub windows10: bool,
    /// Can Windows Update bring it to a newer Windows 11? Not on Server, and not on LTSC, which never gets
    /// feature updates.
    pub updatable: bool,
}

pub fn this_windows() -> Option<ThisWindows> {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let read = |v: &str| crate::installer::reg_read(KEY, v).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let build: u32 = read("CurrentBuild")?.parse().ok()?;
    if read("InstallationType").is_some_and(|t| t.starts_with("Server")) {
        let name = read("ProductName").unwrap_or_else(|| "Windows Server".into());
        return Some(ThisWindows { name, windows10: false, updatable: false });
    }
    // EnterpriseS, EnterpriseSN, IoTEnterpriseS...: the Long-Term Servicing editions.
    let ltsc = read("EditionID").is_some_and(|e| e.contains("EnterpriseS"));
    // ProductName still says "Windows 10" on Windows 11, so the build tells them apart.
    let base = if build >= 22000 { "Windows 11" } else { "Windows 10" };
    let base = if ltsc { format!("{base} LTSC") } else { base.to_string() };
    let name = match read("DisplayVersion") {
        Some(v) => format!("{base} {v}"),
        None => format!("{base} (build {build})"),
    };
    Some(ThisWindows { name, windows10: build < 22000, updatable: !ltsc })
}

/// An IPv4 network, like 192.168.0.0/24.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Net4 {
    pub addr: Ipv4Addr,
    pub len: u8,
}

impl Net4 {
    pub fn mask(len: u8) -> u32 {
        if len == 0 { 0 } else { u32::MAX << (32 - len.min(32) as u32) }
    }
    pub fn of(a: Ipv4Addr, len: u8) -> Net4 {
        Net4 { addr: Ipv4Addr::from(u32::from(a) & Net4::mask(len)), len }
    }
    pub fn contains(&self, a: Ipv4Addr) -> bool {
        u32::from(a) & Net4::mask(self.len) == u32::from(self.addr)
    }
    /// Do the two networks share addresses (one contains the other)?
    pub fn overlaps(&self, o: &Net4) -> bool {
        let m = Net4::mask(self.len.min(o.len));
        u32::from(self.addr) & m == u32::from(o.addr) & m
    }
}

/// The connection pinned apps are sent over.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub luid: u64,
    pub index: u32,
    pub name: String,
    pub ipv4: Vec<Ipv4Addr>,
    /// It has a global IPv6 address and an IPv6 gateway.
    pub ipv6: bool,
}

/// The Ethernet side's own networks: pinned apps keep reaching them over Ethernet (router page, NAS, printer).
#[derive(Clone, Debug, PartialEq)]
pub struct Lan {
    pub luid: u64,
    pub nets: Vec<Net4>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    /// The pinned programs (version folders expanded). Empty: nothing at all is applied.
    pub apps: Vec<String>,
    /// Where their new connections go. None in "block" mode when there's no Wi-Fi adapter at all.
    pub target: Option<Target>,
    /// Route their IPv4 (off when the target has no IPv4 address: forcing IPv4 onto it would break IPv4).
    pub v4: bool,
    /// Route their IPv6 too.
    pub v6: bool,
    /// Block their internet IPv6 so they fall back to IPv4 (the target has no IPv6, Ethernet might).
    pub block_v6: bool,
    /// "Block until Wi-Fi is back": they may only use the target, loopback and the Ethernet LAN.
    pub guard: bool,
    pub lan: Option<Lan>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }
}

/// The adapter for pinned apps: the one picked in Settings if present, else the first connected Wi-Fi, else the
/// first Wi-Fi. Any adapter can be picked in Settings (a phone's USB tethering, or a second NIC in a test VM).
pub fn pick_target<'a>(ifaces: &'a [Iface], preferred: Option<&str>) -> Option<&'a Iface> {
    if let Some(p) = preferred {
        if let Some(i) = ifaces.iter().find(|i| i.guid.eq_ignore_ascii_case(p)) {
            return Some(i);
        }
    }
    crate::net::pick_wifi(ifaces, None)
}

pub fn target_of(i: &Iface) -> Target {
    Target {
        luid: i.luid,
        index: i.index,
        name: if i.kind == Kind::Wifi { "Wi-Fi".into() } else { i.alias.clone() },
        ipv4: i.ipv4.iter().copied().filter(|a| !a.is_link_local()).collect(),
        ipv6: !i.ipv6.is_empty() && i.gw6.is_some(),
    }
}

fn nets_of(i: &Iface) -> Vec<Net4> {
    i.nets
        .iter()
        .filter(|(a, len)| !a.is_link_local() && !a.is_loopback() && *len > 0 && *len < 32)
        .map(|(a, len)| Net4::of(*a, *len))
        .collect()
}

/// The LAN to keep: the up adapter holding the default route (else the connected Ethernet), when it isn't the
/// target, minus networks the target is on too (the same router behind both: the target reaches them anyway).
pub fn lan_of(ifaces: &[Iface], target: Option<&Iface>) -> Option<Lan> {
    let e = ifaces
        .iter()
        .find(|i| i.default_route && i.up())
        .or_else(|| crate::net::pick_ethernet(ifaces).filter(|i| i.up()))?;
    if target.is_some_and(|t| t.luid == e.luid) {
        return None;
    }
    let theirs = target.map(nets_of).unwrap_or_default();
    let nets: Vec<Net4> = nets_of(e).into_iter().filter(|n| !theirs.iter().any(|t| t.overlaps(n))).collect();
    (!nets.is_empty()).then_some(Lan { luid: e.luid, nets })
}

/// Every .exe a rule covers: its own path, plus the same file in sibling version folders that exist now
/// (Discord\app-1.0.9213 and app-1.0.9214 both, while an update is half done). `siblings` lists the files with
/// the same name one folder level up; it's injected so the plan stays testable.
pub fn expand(path: &str, siblings: &dyn Fn(&str) -> Vec<String>) -> Vec<String> {
    let found = siblings(path);
    let key = norm(path);
    // After an update deleted the rule's own version folder, only the versions that exist count.
    let gone = !found.is_empty() && !found.iter().any(|s| norm(s) == key);
    let mut out = if gone { Vec::new() } else { vec![path.to_string()] };
    for s in found {
        if norm(&s) != key && same_app(&key, &norm(&s)) && !out.iter().any(|o| norm(o) == norm(&s)) {
            out.push(s);
        }
    }
    out
}

/// Real sibling lookup: `<root>\*\<file>` for a rule at `<root>\<versioned folder>\<file>`.
pub fn disk_siblings(path: &str) -> Vec<String> {
    let versioned = |s: &str| s.contains('.') && s.bytes().any(|b| b.is_ascii_digit());
    let p = std::path::Path::new(path);
    let (Some(file), Some(dir)) = (p.file_name(), p.parent()) else { return Vec::new() };
    // Only apps that live in a version folder (Discord\app-1.0.9213) have siblings worth looking for.
    if !dir.file_name().is_some_and(|n| versioned(&n.to_string_lossy())) {
        return Vec::new();
    }
    let Some(root) = dir.parent() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| versioned(&e.file_name().to_string_lossy()))
        .map(|e| e.path().join(file))
        .filter(|f| f.is_file())
        .map(|f| f.to_string_lossy().into_owned())
        .collect()
}

pub fn plan(cfg: &Config, ifaces: &[Iface], siblings: &dyn Fn(&str) -> Vec<String>) -> Plan {
    let mut apps: Vec<String> = Vec::new();
    // Windows' own programs and Defender are never pinned, whatever the settings file says.
    for p in cfg.pinned().into_iter().filter(|p| !crate::procs::is_protected(p)) {
        for e in expand(&p, siblings) {
            if !apps.iter().any(|a| norm(a) == norm(&e)) {
                apps.push(e);
            }
        }
    }
    if apps.is_empty() {
        return Plan::default();
    }
    let strict = cfg.settings.when_wifi_down == WifiDown::Block;
    let target = pick_target(ifaces, cfg.settings.wifi_adapter.as_deref());
    let lan = if cfg.settings.lan_keep { lan_of(ifaces, target) } else { None };
    match target {
        Some(t) if t.up() => {
            let t = target_of(t);
            Plan {
                v4: !t.ipv4.is_empty() || strict,
                v6: t.ipv6 || strict,
                block_v6: !t.ipv6 && !strict,
                guard: strict,
                apps,
                target: Some(t),
                lan,
            }
        }
        // Block mode, Wi-Fi down: the same policies and filters as when it's up (the guard blocks what can't go
        // over Wi-Fi), so a flapping Wi-Fi changes nothing in Windows.
        Some(t) if strict => Plan { apps, target: Some(target_of(t)), v4: true, v6: true, block_v6: false, guard: true, lan },
        None if strict => Plan { apps, target: None, v4: false, v6: false, block_v6: false, guard: true, lan },
        // Fallback mode, Wi-Fi down or missing: pinned apps use the default route like everything else.
        _ => Plan::default(),
    }
}

/// What the backend reports after applying a plan.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Applied {
    pub policies: usize,
    pub filters: usize,
    /// Programs that couldn't be covered (for example a deleted .exe), with the reason.
    pub skipped: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineError {
    /// Not running as admin.
    NeedsAdmin,
    /// This Windows has no connection policies (before Windows 11 24H2).
    Unsupported(String),
    Failed(String),
}

impl EngineError {
    /// This Windows has no connection policies.
    pub fn too_old() -> EngineError {
        EngineError::Unsupported("Windows 11 24H2 or later is needed".into())
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            EngineError::NeedsAdmin => write!(f, "needs admin"),
            EngineError::Unsupported(s) => write!(f, "not supported: {s}"),
            EngineError::Failed(s) => write!(f, "{s}"),
        }
    }
}

pub trait Engine {
    /// Makes Windows match the plan: adds what's missing, removes what's no longer wanted.
    fn apply(&mut self, plan: &Plan) -> Result<Applied, EngineError>;
    /// Removes everything NF Switcher added. Called on Revert all, on exit and at log-off.
    fn clear(&mut self) -> Result<(), EngineError>;
}

/// The backend when routing can't run (no admin, old Windows): it applies nothing and says why.
pub struct Unavailable(pub EngineError);

impl Engine for Unavailable {
    fn apply(&mut self, plan: &Plan) -> Result<Applied, EngineError> {
        if plan.is_empty() { Ok(Applied::default()) } else { Err(self.0.clone()) }
    }
    fn clear(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::config::Route;
    use crate::net::Status;

    pub fn iface(kind: Kind, guid: &str, luid: u64, up: bool, v6: bool, default_route: bool) -> Iface {
        let a = Ipv4Addr::new(192, 168, luid as u8, 10);
        Iface {
            index: luid as u32,
            index6: luid as u32,
            luid,
            guid: guid.into(),
            alias: if kind == Kind::Wifi { "Wi-Fi".into() } else { "Ethernet".into() },
            desc: String::new(),
            kind,
            status: if up { Status::Connected } else { Status::Disconnected },
            ipv4: vec![a],
            nets: vec![(a, 24)],
            ipv6: if v6 { vec!["2001:db8::10".parse().unwrap()] } else { vec![] },
            gw4: Some(Ipv4Addr::new(192, 168, luid as u8, 1)),
            gw6: if v6 { Some("fe80::1".parse().unwrap()) } else { None },
            metric4: 25,
            speed: 0,
            ssid: None,
            profile: None,
            default_route,
        }
    }

    fn cfg(block: bool) -> Config {
        let mut c = Config::default();
        c.set_route(r"C:\B\brave.exe", "Brave", Route::Wifi);
        c.set_route(r"C:\D\discord.exe", "Discord", Route::Default);
        if block {
            c.settings.when_wifi_down = WifiDown::Block;
        }
        c
    }

    fn none(_: &str) -> Vec<String> {
        Vec::new()
    }

    fn eth_wifi(wifi_up: bool, wifi_v6: bool) -> Vec<Iface> {
        vec![iface(Kind::Ethernet, "{E}", 1, true, true, true), iface(Kind::Wifi, "{W}", 2, wifi_up, wifi_v6, false)]
    }

    #[test]
    fn connection_policies_come_with_windows_11_24h2() {
        // Reads this PC only: 24H2/25H2/26H2 (builds 26100-26300) have them, 21H2-23H2 (22000-22631) don't.
        let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build: u32 = crate::installer::reg_read(key, "CurrentBuild").unwrap().trim().parse().unwrap();
        if (26100..28000).contains(&build) {
            assert!(supported());
        }
        if build <= 22631 {
            assert!(!supported());
        }
        let w = this_windows().unwrap();
        assert!(w.name.starts_with("Windows "));
        if !w.name.starts_with("Windows Server") {
            assert_eq!(w.windows10, build < 22000);
        }
    }

    #[test]
    fn routes_pinned_apps_over_connected_wifi_and_keeps_the_ethernet_lan() {
        let p = plan(&cfg(false), &eth_wifi(true, false), &none);
        assert_eq!(p.apps, vec![r"C:\B\brave.exe".to_string()]);
        assert_eq!(p.target.as_ref().unwrap().luid, 2);
        assert!(!p.guard);
        // Wi-Fi has no IPv6 here, Ethernet does: block the pinned app's internet IPv6 so it can't leak.
        assert!(p.block_v6 && !p.v6);
        assert_eq!(p.lan, Some(Lan { luid: 1, nets: vec![Net4 { addr: Ipv4Addr::new(192, 168, 1, 0), len: 24 }] }));
        let mut c = cfg(false);
        c.settings.lan_keep = false;
        assert_eq!(plan(&c, &eth_wifi(true, false), &none).lan, None);
    }

    #[test]
    fn same_network_on_both_sides_needs_no_lan_keep() {
        let mut ifs = eth_wifi(true, false);
        ifs[1].nets = vec![(Ipv4Addr::new(192, 168, 1, 77), 24)];
        assert_eq!(plan(&cfg(false), &ifs, &none).lan, None);
    }

    #[test]
    fn wifi_with_ipv6_routes_ipv6_too() {
        let p = plan(&cfg(false), &eth_wifi(true, true), &none);
        assert!(p.v6 && !p.block_v6);
    }

    #[test]
    fn wifi_down_falls_back_or_keeps_blocking_without_change() {
        assert!(plan(&cfg(false), &eth_wifi(false, false), &none).is_empty());
        let up = plan(&cfg(true), &eth_wifi(true, false), &none);
        let down = plan(&cfg(true), &eth_wifi(false, false), &none);
        assert!(up.guard && down.guard);
        // What reaches Windows (wfp::desired) is the same either way, so a Wi-Fi flap changes nothing.
        assert_eq!(wfp::desired(&up), wfp::desired(&down));
        // No Wi-Fi adapter at all: only the guard.
        let p = plan(&cfg(true), &eth_wifi(true, false)[..1], &none);
        assert!(p.guard && p.target.is_none());
    }

    #[test]
    fn nothing_pinned_means_nothing_applied() {
        let mut c = cfg(true);
        c.revert_all();
        assert!(plan(&c, &eth_wifi(true, false), &none).is_empty());
    }

    #[test]
    fn settings_can_pick_another_adapter() {
        let mut c = cfg(false);
        c.settings.wifi_adapter = Some("{e2}".into());
        let ifs = vec![
            iface(Kind::Ethernet, "{E}", 1, true, false, true),
            iface(Kind::Ethernet, "{E2}", 3, true, false, false),
            iface(Kind::Wifi, "{W}", 2, true, false, false),
        ];
        assert_eq!(plan(&c, &ifs, &none).target.unwrap().luid, 3);
    }

    #[test]
    fn version_folders_are_covered() {
        let mut c = Config::default();
        c.set_route(r"C:\U\Discord\app-1.0.1\Discord.exe", "Discord", Route::Wifi);
        let sib = |_: &str| {
            vec![
                r"C:\U\Discord\app-1.0.1\Discord.exe".to_string(),
                r"C:\U\Discord\app-1.0.2\Discord.exe".to_string(),
                r"C:\U\Discord\packages\Discord.exe".to_string(),
            ]
        };
        let p = plan(&c, &eth_wifi(true, true), &sib);
        assert_eq!(p.apps.len(), 2, "{:?}", p.apps);
        // The rule's own folder was removed by an update: only the new version is pinned.
        let after = |_: &str| vec![r"C:\U\Discord\app-1.0.2\Discord.exe".to_string()];
        let p = plan(&c, &eth_wifi(true, true), &after);
        assert_eq!(p.apps, vec![r"C:\U\Discord\app-1.0.2\Discord.exe".to_string()]);
    }

    #[test]
    fn windows_programs_are_never_pinned() {
        let mut c = Config::default();
        c.set_route(&format!(r"{}\System32\svchost.exe", std::env::var("SystemRoot").unwrap()), "svchost", Route::Wifi);
        assert!(plan(&c, &eth_wifi(true, true), &none).is_empty());
    }

    #[test]
    fn no_ipv4_policy_when_wifi_has_no_ipv4() {
        let mut ifs = eth_wifi(true, true);
        ifs[1].ipv4 = vec![];
        ifs[1].nets = vec![];
        let p = plan(&cfg(false), &ifs, &none);
        assert!(!p.v4 && p.v6);
        assert!(plan(&cfg(true), &ifs, &none).v4, "block mode keeps its items fixed");
    }

    #[test]
    fn networks() {
        let n = Net4::of(Ipv4Addr::new(192, 168, 0, 113), 24);
        assert_eq!(n.addr, Ipv4Addr::new(192, 168, 0, 0));
        assert!(n.overlaps(&Net4::of(Ipv4Addr::new(192, 168, 0, 9), 16)));
        assert!(!n.overlaps(&Net4::of(Ipv4Addr::new(192, 168, 8, 99), 24)));
        assert_eq!(Net4::mask(0), 0);
        assert_eq!(Net4::mask(32), u32::MAX);
    }

    #[test]
    fn unavailable_backend_only_complains_when_there_is_work() {
        let mut e = Unavailable(EngineError::NeedsAdmin);
        assert!(e.apply(&Plan::default()).is_ok());
        assert_eq!(e.apply(&plan(&cfg(false), &eth_wifi(true, true), &none)), Err(EngineError::NeedsAdmin));
    }
}
