// The controller: the saved settings, the adapters, what's running, and the engine. Every change goes
// config -> save -> plan -> engine, and the flyout's model is filled from here.
use crate::config::{norm, same_app, Config, Route, WifiDown};
use crate::engine::{self, Applied, Engine, EngineError, Plan};
use crate::net::{self, Iface, Kind};
use crate::procs::{self, Proc, Sock};
#[cfg(test)]
use crate::procs::Proto;
use crate::ui::view::{AdapterItem, AppItem, Banner, Model, PickItem, Tile, Tone};
use std::collections::HashMap;
use std::io::Write;

pub struct App {
    pub cfg: Config,
    pub admin: bool,
    pub ifaces: Vec<Iface>,
    /// Windows' best interface to the internet; not one of `ifaces` when a VPN or other virtual adapter holds it.
    pub default_index: Option<u32>,
    engine: Box<dyn Engine>,
    /// Running as admin with a usable Windows, so a WFP session that died or failed to open can be opened again.
    can_reopen: bool,
    /// Log-off or shutdown cleaned up: nothing is applied again, even if timers still fire.
    stopped: bool,
    /// The Wi-Fi profile last seen connected, and when NF Switcher last tried to reconnect it.
    last_profile: Option<String>,
    last_reconnect: Option<std::time::Instant>,
    /// A newer release on GitHub, and what the last check said (for Settings).
    pub update: Option<crate::update::Release>,
    pub update_note: String,
    /// "Start with Windows": the sign-in task exists (read once at start, then kept up to date here).
    pub autostart: bool,
    pub error: Option<EngineError>,
    pub applied: Applied,
    last_plan: Option<Plan>,
    /// Refreshed while the flyout is open.
    pub procs: Vec<Proc>,
    pub socks: Vec<Sock>,
    names: HashMap<String, String>,
    own_exe: String,
}

/// The local time as HH:MM (for "checked at").
pub fn clock() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}", t.wHour, t.wMinute)
}

/// What a too-old Windows is told: why, and the way out where Windows Update has one (not on LTSC or Server).
pub fn too_old_banner(this: Option<&engine::ThisWindows>) -> Banner {
    let has = match this {
        Some(w) => format!("This PC has {}, which doesn't have", w.name),
        None => "This Windows doesn't have".into(),
    };
    // Windows Update only offers it to PCs that meet Windows 11's requirements (not to bypassed installs).
    let update = match this {
        Some(w) if !w.updatable => None,
        Some(w) if w.windows10 => Some(" Windows 11 is a free upgrade in Windows Update if this PC meets its requirements."),
        _ => Some(" The update is free in Windows Update if this PC meets Windows 11's requirements."),
    };
    Banner {
        tone: Tone::Warn,
        text: format!(
            "NF Switcher needs Windows 11 24H2 or later. {has} the per-app connection policies it uses to pin apps.{}",
            update.unwrap_or_default()
        ),
        button: update.map(|_| "Open Windows Update".to_string()),
    }
}

pub fn log(line: &str) {
    // Only into a folder that already exists (the elevated start secures it); never created from here.
    let path = crate::config::data_dir().join("nf-switcher.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{secs} {line}");
    }
}

impl App {
    pub fn new(admin: bool, engine: Result<Box<dyn Engine>, EngineError>) -> App {
        let can_reopen = admin && !matches!(engine, Err(EngineError::NeedsAdmin) | Err(EngineError::Unsupported(_)));
        let (engine, error): (Box<dyn Engine>, _) = match engine {
            Ok(e) => (e, None),
            Err(e) => (Box::new(engine::Unavailable(e.clone())), Some(e)),
        };
        let mut app = App {
            cfg: Config::load(),
            admin,
            ifaces: Vec::new(),
            default_index: None,
            engine,
            can_reopen,
            stopped: false,
            last_profile: None,
            last_reconnect: None,
            update: None,
            update_note: String::new(),
            autostart: crate::autostart::exists(),
            error,
            applied: Applied::default(),
            last_plan: None,
            procs: Vec::new(),
            socks: Vec::new(),
            names: HashMap::new(),
            own_exe: std::env::current_exe().map(|p| norm(&p.to_string_lossy())).unwrap_or_default(),
        };
        app.refresh_net();
        app
    }

    /// Adapters changed (or first start): read them again and re-apply.
    pub fn refresh_net(&mut self) {
        self.ifaces = net::snapshot();
        self.default_index = net::default_route_index();
        if let Some(p) = self.target().filter(|t| t.up()).and_then(|t| t.profile.clone()) {
            self.last_profile = Some(p);
        }
        self.sync(false);
    }

    /// Wi-Fi is the pin target, it's down, and an app is pinned: it's worth connecting it again. Not when nothing
    /// can be pinned (no admin, too-old Windows): then Wi-Fi is left alone.
    fn wifi_wanted(&self) -> bool {
        matches!(self.error, None | Some(EngineError::Failed(_)))
            && !self.cfg.pinned().is_empty()
            && self.target().is_some_and(|t| t.kind == Kind::Wifi && !t.up())
    }

    /// "Connect Wi-Fi", and the automatic attempt (at most once a minute, so it never fights Windows or the user).
    pub fn connect_wifi(&mut self, automatic: bool) {
        if self.stopped || !self.wifi_wanted() {
            return;
        }
        if automatic {
            let recent = self.last_reconnect.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(55));
            if !self.cfg.settings.reconnect_wifi || recent {
                return;
            }
        }
        self.last_reconnect = Some(std::time::Instant::now());
        let Some(t) = self.target() else { return };
        match net::wifi_connect(&t.guid, self.last_profile.as_deref()) {
            Ok(p) => log(&format!("connecting Wi-Fi to {p}{}", if automatic { " (Windows had dropped it)" } else { "" })),
            Err(e) => log(&format!("connecting Wi-Fi: {e}")),
        }
    }

    /// What the background update check found.
    pub fn update_result(&mut self, r: Result<Option<crate::update::Release>, String>) {
        let now = clock();
        match r {
            Ok(Some(rel)) if rel.version > crate::update::current() => {
                log(&format!("update available: {}", rel.tag));
                self.update_note = format!("Checked at {now}: version {} is out", rel.version_text());
                self.update = Some(rel);
            }
            Ok(_) => {
                self.update = None;
                self.update_note = format!("Checked at {now}: you have the latest version");
            }
            Err(e) => self.update_note = format!("Couldn't check at {now}: {e}"),
        }
    }

    pub fn set_check_updates(&mut self, on: bool) {
        self.cfg.settings.check_updates = on;
        if !on {
            self.update = None;
            self.update_note.clear();
        }
        self.changed();
    }

    /// Creates or removes the sign-in task that starts NF Switcher in the tray (needs admin).
    pub fn set_autostart(&mut self, on: bool) {
        let r = match std::env::current_exe() {
            Ok(exe) if on => crate::autostart::create(&exe, &crate::autostart::current_user(), &crate::config::data_dir()),
            Ok(_) => crate::autostart::delete(),
            Err(e) => Err(e.to_string()),
        };
        match r {
            Ok(()) => self.autostart = on,
            Err(e) => log(&format!("start with Windows: {e}")),
        }
    }

    pub fn set_reconnect(&mut self, on: bool) {
        self.cfg.settings.reconnect_wifi = on;
        self.changed();
    }

    /// Works out the plan and hands it to the engine when it changed (or `force`).
    pub fn sync(&mut self, force: bool) {
        if self.stopped {
            return;
        }
        let plan = engine::plan(&self.cfg, &self.ifaces, &engine::disk_siblings);
        if !force && self.last_plan.as_ref() == Some(&plan) {
            return;
        }
        let mut result = self.engine.apply(&plan);
        if let (Err(EngineError::Failed(e)), true) = (&result, self.can_reopen) {
            // Most likely the WFP session died (the filtering service restarted): start a new one and try once more.
            // The old engine goes first, so its cleanup (routepolicies back off) can't undo the new one's.
            log(&format!("apply failed: {e}; opening a new WFP session"));
            self.engine = Box::new(engine::Unavailable(EngineError::Failed(e.clone())));
            match engine::open_real() {
                Ok(fresh) => {
                    self.engine = fresh;
                    result = self.engine.apply(&plan);
                }
                Err(err) => result = Err(err),
            }
        }
        match result {
            Ok(a) => {
                if a != self.applied || force {
                    log(&format!("applied: {} policies, {} filters, skipped {:?}", a.policies, a.filters, a.skipped));
                }
                self.applied = a;
                // A permanent reason (no admin, old Windows) stays; a failure clears once applying works.
                if matches!(self.error, Some(EngineError::Failed(_))) {
                    self.error = None;
                }
            }
            Err(e) => {
                log(&format!("apply failed: {e}"));
                let retry = matches!(e, EngineError::Failed(_));
                self.error = Some(e);
                if retry {
                    // Try again at the next adapter change or the minute timer, not only on "Try again".
                    self.last_plan = None;
                    return;
                }
            }
        }
        self.last_plan = Some(plan);
    }

    fn changed(&mut self) {
        // Without admin nothing is saved: the settings folder is admin-only, and a copy without admin must not
        // create it as an ordinary user (the tray's "Revert all" gets here too).
        if !self.read_only() {
            if let Err(e) = self.cfg.save() {
                log(&format!("saving settings failed: {e}"));
            }
        }
        self.sync(false);
    }

    pub fn toggle(&mut self, i: usize) {
        if let Some(a) = self.cfg.apps.get_mut(i) {
            a.route = if a.route == Route::Wifi { Route::Default } else { Route::Wifi };
            self.changed();
        }
    }

    pub fn add(&mut self, path: &str) {
        let name = self.name(path);
        self.cfg.set_route(path, &name, Route::Wifi);
        self.changed();
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.cfg.apps.len() {
            self.cfg.apps.remove(i);
            self.changed();
        }
    }

    /// Every app back on the default route; the list stays so it's one click to undo.
    pub fn revert_all(&mut self) {
        self.cfg.revert_all();
        self.changed();
        // Belt and braces: whatever the plan says, nothing of ours stays in Windows.
        match self.engine.clear() {
            Ok(()) if matches!(self.error, Some(EngineError::Failed(_))) => self.error = None,
            Ok(()) => {}
            Err(e) => log(&format!("revert: clear failed: {e}")),
        }
        self.last_plan = Some(Plan::default());
    }

    pub fn set_block(&mut self, block: bool) {
        self.cfg.settings.when_wifi_down = if block { WifiDown::Block } else { WifiDown::Fallback };
        self.changed();
    }

    pub fn set_lan_keep(&mut self, on: bool) {
        self.cfg.settings.lan_keep = on;
        self.changed();
    }

    pub fn set_adapter(&mut self, guid: Option<String>) {
        self.cfg.settings.wifi_adapter = guid;
        self.changed();
    }

    /// Is this app pinned, routed right now, and running with connections still on the default route?
    pub fn can_move(&self, i: usize) -> bool {
        let Some(a) = self.cfg.apps.get(i) else { return false };
        if a.route != Route::Wifi || self.error.is_some() || !self.target().is_some_and(|t| t.up()) {
            return false;
        }
        let pids = self.pids_of(&a.path);
        !pids.is_empty() && self.split(&pids).1 > 0
    }

    /// Closes the app's IPv4 TCP connections that aren't on the pin target, so it reconnects over it.
    /// Returns (closed, couldn't close).
    pub fn move_connections(&mut self, i: usize) -> (u32, u32) {
        let Some(a) = self.cfg.apps.get(i) else { return (0, 0) };
        let pids = self.pids_of(&a.path);
        let mine: Vec<std::net::IpAddr> =
            self.target().map(|t| t.ipv4.iter().map(|a| (*a).into()).collect()).unwrap_or_default();
        let (mut ok, mut failed) = (0, 0);
        for s in self.socks.iter().filter(|s| pids.contains(&s.pid) && procs::is_connection(s) && s.state == 5) {
            if s.local.is_ipv4() && !mine.contains(&s.local.ip()) && !self.to_lan(s) {
                if procs::close_tcp(s) { ok += 1 } else { failed += 1 }
            }
        }
        log(&format!("moved connections of {}: {ok} closed, {failed} failed", a.path));
        self.refresh_activity();
        (ok, failed)
    }

    /// Without admin nothing can be applied, so nothing is changed or saved either.
    pub fn read_only(&self) -> bool {
        !self.admin
    }

    pub fn retry(&mut self) {
        self.sync(true);
    }

    /// Removes everything from Windows: on Quit, at log-off and shutdown.
    pub fn shutdown(&mut self) {
        self.stopped = true;
        if let Err(e) = self.engine.clear() {
            log(&format!("shutdown: clear failed: {e}"));
        }
        self.last_plan = None;
    }

    /// Does a pinned app live in a version folder (Discord\app-1.0.9213)? Then a new version can appear any time.
    pub fn has_versioned_rules(&self) -> bool {
        self.cfg.pinned().iter().any(|p| !engine::disk_siblings(p).is_empty())
    }

    /// A connection to the Ethernet LAN that LAN-keep keeps on Ethernet on purpose.
    fn to_lan(&self, s: &Sock) -> bool {
        if !self.cfg.settings.lan_keep {
            return false;
        }
        let Some(lan) = engine::lan_of(&self.ifaces, self.target()) else { return false };
        match s.remote.map(|r| r.ip()) {
            Some(std::net::IpAddr::V4(a)) => lan.nets.iter().any(|n| n.contains(a)),
            _ => false,
        }
    }

    pub fn refresh_activity(&mut self) {
        self.procs = procs::list();
        self.socks = procs::sockets();
    }

    pub fn name(&mut self, path: &str) -> String {
        if let Some(n) = self.names.get(path) {
            return n.clone();
        }
        let n = procs::display_name(path);
        self.names.insert(path.to_string(), n.clone());
        n
    }

    fn target(&self) -> Option<&Iface> {
        engine::pick_target(&self.ifaces, self.cfg.settings.wifi_adapter.as_deref())
    }

    fn target_label(&self) -> String {
        match self.target() {
            Some(t) if t.kind == Kind::Wifi => "Wi-Fi".into(),
            Some(t) => t.alias.clone(),
            None => "Wi-Fi".into(),
        }
    }

    /// PIDs running each rule's program.
    fn pids_of(&self, rule_path: &str) -> Vec<u32> {
        let key = norm(rule_path);
        self.procs.iter().filter(|p| p.path.as_deref().is_some_and(|pp| same_app(&key, &norm(pp)))).map(|p| p.pid).collect()
    }

    /// (on the pin target, elsewhere) connections of these PIDs, told apart by their local address.
    fn split(&self, pids: &[u32]) -> (u32, u32) {
        let mine: Vec<std::net::IpAddr> = self
            .target()
            .map(|t| t.ipv4.iter().map(|a| (*a).into()).chain(t.ipv6.iter().map(|a| (*a).into())).collect())
            .unwrap_or_default();
        let (mut on, mut off) = (0, 0);
        for s in self.socks.iter().filter(|s| pids.contains(&s.pid) && procs::is_connection(s)) {
            if mine.contains(&s.local.ip()) {
                on += 1
            } else if !self.to_lan(s) {
                off += 1
            }
        }
        (on, off)
    }

    fn app_status(&self, rule_path: &str, wifi: bool) -> (String, bool) {
        let pids = self.pids_of(rule_path);
        let running = !pids.is_empty();
        let label = self.target_label();
        if !wifi {
            return (if running { "Running · default route" } else { "Not running · default route" }.into(), false);
        }
        if procs::is_protected(rule_path) {
            return ("Windows' own programs can't be pinned".into(), true);
        }
        match &self.error {
            Some(EngineError::NeedsAdmin) => return ("Needs admin to switch".into(), true),
            Some(EngineError::Unsupported(_)) => return ("Needs Windows 11 24H2 or later".into(), true),
            Some(EngineError::Failed(_)) => return ("Couldn't switch it (see the message above)".into(), true),
            None => {}
        }
        if self.applied.skipped.iter().any(|(p, _)| norm(p) == norm(rule_path)) {
            return ("Can't find this program".into(), true);
        }
        if !self.target().is_some_and(|t| t.up()) {
            return match self.cfg.settings.when_wifi_down {
                WifiDown::Block => (format!("{label} is down · blocked until it's back"), true),
                WifiDown::Fallback => (format!("{label} is down · using the default route"), true),
            };
        }
        if !running {
            return ("Waits until it starts".into(), false);
        }
        let (on, off) = self.split(&pids);
        let s = match (on, off) {
            (0, 0) => "Running · no connections yet".to_string(),
            (on, 0) => format!("Running · {on} on {label}"),
            (0, off) => format!("Running · {off} still on the default route, restart it to move them"),
            (on, off) => format!("Running · {on} on {label} · {off} still on the default route"),
        };
        (s, false)
    }

    pub fn status_text(&self) -> String {
        let pinned = self.cfg.pinned().len();
        if let (Some(u), None) = (&self.update, &self.error) {
            if pinned == 0 {
                return format!("Update {}", u.version_text());
            }
        }
        match &self.error {
            Some(EngineError::NeedsAdmin) => "Needs admin".into(),
            Some(EngineError::Unsupported(_)) => "Not supported".into(),
            Some(EngineError::Failed(_)) => "Error".into(),
            None if pinned == 0 => "Ready".into(),
            None if !self.target().is_some_and(|t| t.up()) => format!("{} down", self.target_label()),
            None => format!("{pinned} on {}", self.target_label()),
        }
    }

    fn banner(&self) -> Option<Banner> {
        let pinned = !self.cfg.pinned().is_empty();
        let label = self.target_label();
        match &self.error {
            Some(EngineError::NeedsAdmin) => {
                return Some(Banner {
                    tone: Tone::Warn,
                    text: "Pinning apps to Wi-Fi needs admin rights. Windows will ask (UAC).".into(),
                    button: Some("Restart as admin".into()),
                })
            }
            Some(EngineError::Unsupported(_)) => return Some(too_old_banner(engine::this_windows().as_ref())),
            Some(EngineError::Failed(why)) => {
                return Some(Banner { tone: Tone::Error, text: format!("Windows refused the change: {why}"), button: Some("Try again".into()) })
            }
            None => {}
        }
        let vpn = self.default_index.is_some_and(|d| d != 0 && !self.ifaces.iter().any(|i| i.index == d));
        if pinned && vpn {
            return Some(Banner {
                tone: Tone::Info,
                text: format!("Another connection (a VPN?) is Windows' default route. Pinned apps go around it, over {label}."),
                button: None,
            });
        }
        let Some(t) = self.target() else {
            let text = if pinned && self.cfg.settings.when_wifi_down == WifiDown::Block {
                "No Wi-Fi adapter found. Pinned apps are blocked until it's back."
            } else {
                "No Wi-Fi adapter found. Apps keep the default route."
            };
            return Some(Banner { tone: if pinned { Tone::Warn } else { Tone::Info }, text: text.into(), button: None });
        };
        if pinned && !t.up() {
            let what = match self.cfg.settings.when_wifi_down {
                WifiDown::Block => "are blocked until it's back",
                WifiDown::Fallback => "use the default route until it's back",
            };
            let button = (t.kind == Kind::Wifi).then(|| "Connect Wi-Fi".to_string());
            return Some(Banner { tone: Tone::Warn, text: format!("{label} isn't connected. Pinned apps {what}."), button });
        }
        if pinned && t.default_route {
            return Some(Banner {
                tone: Tone::Info,
                text: format!("{label} is Windows' default route right now, so every app already uses it."),
                button: None,
            });
        }
        None
    }

    fn ip_text(i: &Iface) -> String {
        if !i.up() {
            return "Not connected".into();
        }
        i.ipv4.iter().find(|a| !a.is_link_local()).map(|a| a.to_string()).or_else(|| i.ipv6.first().map(|a| a.to_string())).unwrap_or_default()
    }

    /// Fills the flyout's data; the page, search, scroll and hover stay as they are.
    pub fn fill(&mut self, m: &mut Model) {
        m.status = self.status_text();
        m.footer = if self.admin { "Admin".into() } else { "Not admin".into() };
        m.banner = self.banner();
        m.eth = match net::pick_ethernet(&self.ifaces) {
            Some(e) => Tile {
                title: if e.default_route { "Ethernet · default".into() } else { "Ethernet".into() },
                sub: App::ip_text(e),
                on: false,
                off: !e.up(),
            },
            None => Tile { title: "Ethernet".into(), sub: "No adapter".into(), on: false, off: true },
        };
        let any_pinned = !self.cfg.pinned().is_empty();
        m.wifi = match self.target() {
            Some(t) => Tile {
                title: match (&t.ssid, t.kind) {
                    (Some(s), Kind::Wifi) => format!("Wi-Fi · {s}"),
                    (None, Kind::Wifi) => "Wi-Fi".into(),
                    _ => t.alias.clone(),
                },
                sub: App::ip_text(t),
                on: t.up() && any_pinned && self.error.is_none(),
                off: !t.up(),
            },
            None => Tile { title: "Wi-Fi".into(), sub: "No adapter".into(), on: false, off: true },
        };
        let rules: Vec<(String, String, bool)> =
            self.cfg.apps.iter().map(|a| (a.name.clone(), a.path.clone(), a.route == Route::Wifi)).collect();
        m.apps = rules
            .into_iter()
            .map(|(name, path, wifi)| {
                let (status, warn) = self.app_status(&path, wifi);
                AppItem { name, path, wifi, status, warn }
            })
            .collect();
        self.fill_picks(m);
        m.block_when_down = self.cfg.settings.when_wifi_down == WifiDown::Block;
        m.adapter_auto = self.cfg.settings.wifi_adapter.is_none();
        m.lan_keep = self.cfg.settings.lan_keep;
        m.reconnect = self.cfg.settings.reconnect_wifi;
        m.check_updates = self.cfg.settings.check_updates;
        m.autostart = self.autostart;
        m.update = self.update.as_ref().map(|r| r.version_text());
        m.update_note = self.update_note.clone();
        m.adapters = self.adapter_choices().into_iter().map(|(_, it)| it).collect();
        m.about = vec![
            format!("NF Switcher {}", env!("CARGO_PKG_VERSION")),
            "Pinned apps are sent over Wi-Fi with Windows' own connection policies (WFP). Only new connections move; \
             Windows' DNS lookups keep the default route. Closing NF Switcher removes everything, and so does Windows \
             if NF Switcher stops."
                .into(),
            "Windows ignores these policies unless its Route Policies option is on, so NF Switcher turns it on while an \
             app is pinned (until the next restart at most) and puts it back afterwards."
                .into(),
            format!("Settings: {}", crate::config::config_path().display()),
        ];
    }

    /// Adapters that can carry pinned apps: connected ones other than the default route, plus the current pick.
    /// (One Wi-Fi card can show up as several adapters, the extra ones never connected; they're left out.)
    pub fn adapter_choices(&self) -> Vec<(String, AdapterItem)> {
        let picked = self.cfg.settings.wifi_adapter.as_deref();
        self.ifaces
            .iter()
            .filter(|i| (i.up() && !i.default_route) || picked.is_some_and(|p| i.guid.eq_ignore_ascii_case(p)))
            .map(|i| {
                let detail = match (&i.ssid, i.up()) {
                    (Some(s), true) => format!("{} · {s}", i.desc),
                    (None, true) => format!("{} · {}", i.desc, App::ip_text(i)),
                    (_, false) => format!("{} · not connected", i.desc),
                };
                let selected = picked.is_some_and(|p| i.guid.eq_ignore_ascii_case(p));
                (i.guid.clone(), AdapterItem { name: i.alias.clone(), sub: detail, selected })
            })
            .collect()
    }

    /// The "Add an app" list: running programs outside Windows' own folder, one row per .exe, filtered by `m.search`.
    pub fn fill_picks(&mut self, m: &mut Model) {
        let mut seen = std::collections::HashSet::new();
        let mut rows: Vec<(String, String)> = Vec::new();
        let paths: Vec<String> = self.procs.iter().filter_map(|p| p.path.clone()).collect();
        for path in paths {
            let key = norm(&path);
            if key == self.own_exe || procs::is_windows_path(&path) || !seen.insert(key) {
                continue;
            }
            let name = self.name(&path);
            rows.push((name, path));
        }
        m.running = rows.len();
        let q = m.search.trim().to_lowercase();
        rows.retain(|(n, p)| q.is_empty() || n.to_lowercase().contains(&q) || p.to_lowercase().contains(&q));
        rows.sort_by_key(|(n, _)| n.to_lowercase());
        m.picks = rows
            .into_iter()
            .map(|(name, path)| {
                let added = self.cfg.find(&path).is_some();
                PickItem { name, path, added }
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::iface;

    struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<Plan>>>);
    impl Engine for Recorder {
        fn apply(&mut self, plan: &Plan) -> Result<Applied, EngineError> {
            self.0.borrow_mut().push(plan.clone());
            Ok(Applied { policies: plan.apps.len(), ..Default::default() })
        }
        fn clear(&mut self) -> Result<(), EngineError> {
            self.0.borrow_mut().push(Plan::default());
            Ok(())
        }
    }

    fn app_with(ifaces: Vec<Iface>) -> (App, std::rc::Rc<std::cell::RefCell<Vec<Plan>>>) {
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut a = App {
            cfg: Config::default(),
            admin: true,
            ifaces,
            default_index: None,
            engine: Box::new(Recorder(log.clone())),
            can_reopen: false,
            stopped: false,
            last_profile: None,
            last_reconnect: None,
            update: None,
            update_note: String::new(),
            autostart: false,
            error: None,
            applied: Applied::default(),
            last_plan: None,
            procs: vec![],
            socks: vec![],
            names: HashMap::new(),
            own_exe: String::new(),
        };
        a.names.insert(r"C:\B\brave.exe".into(), "Brave".into());
        (a, log)
    }

    #[test]
    fn statuses_and_banners_follow_the_state() {
        let dir = std::env::temp_dir().join(format!("nfsw-app-{}", std::process::id()));
        std::env::set_var("NFSW_DATA_DIR", &dir);
        let (mut a, log) = app_with(vec![
            iface(Kind::Ethernet, "{E}", 1, true, true, true),
            iface(Kind::Wifi, "{W}", 2, true, true, false),
        ]);
        a.add(r"C:\B\brave.exe");
        assert_eq!(log.borrow().last().unwrap().apps.len(), 1);
        let mut m = Model::default();
        a.fill(&mut m);
        assert_eq!(m.status, "1 on Wi-Fi");
        assert_eq!(m.apps[0].status, "Waits until it starts");
        assert!(m.wifi.on && !m.eth.on);
        assert_eq!(m.eth.title, "Ethernet · default");
        assert!(m.banner.is_none());

        // The app runs with one connection on Wi-Fi and one from before the pin.
        a.procs = vec![Proc { pid: 9, ppid: 1, name: "brave.exe".into(), path: Some(r"C:\B\brave.exe".into()) }];
        let s = |l: &str, r: &str| Sock { proto: Proto::Tcp, local: l.parse().unwrap(), remote: Some(r.parse().unwrap()), state: 5, pid: 9 };
        a.socks = vec![s("192.168.2.10:50000", "1.2.3.4:443"), s("192.168.1.10:50001", "1.2.3.4:443")];
        a.fill(&mut m);
        assert_eq!(m.apps[0].status, "Running · 1 on Wi-Fi · 1 still on the default route");

        // Wi-Fi drops.
        a.ifaces[1].status = crate::net::Status::Disconnected;
        a.sync(false);
        assert!(log.borrow().last().unwrap().apps.is_empty());
        a.fill(&mut m);
        assert_eq!(m.status, "Wi-Fi down");
        assert!(m.apps[0].warn);
        assert_eq!(m.banner.as_ref().unwrap().tone, Tone::Warn);

        // Revert all: nothing pinned, engine cleared.
        a.revert_all();
        assert!(log.borrow().last().unwrap().is_empty());
        a.fill(&mut m);
        assert_eq!(m.status, "Ready");
        assert!(!m.apps[0].wifi);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_vpn_holding_the_default_route_is_pointed_out() {
        let (mut a, _) = app_with(vec![iface(Kind::Ethernet, "{E}", 1, true, false, false), iface(Kind::Wifi, "{W}", 2, true, false, false)]);
        a.cfg.set_route(r"C:Brave.exe", "Brave", Route::Wifi);
        a.default_index = Some(77);
        let mut m = Model::default();
        a.fill(&mut m);
        assert!(m.banner.as_ref().unwrap().text.contains("VPN"));
        a.default_index = Some(1);
        a.fill(&mut m);
        assert!(m.banner.is_none());
    }

    #[test]
    fn needs_admin_shows_the_restart_button() {
        let (mut a, _) = app_with(vec![iface(Kind::Wifi, "{W}", 2, true, true, false)]);
        a.error = Some(EngineError::NeedsAdmin);
        a.admin = false;
        let mut m = Model::default();
        a.fill(&mut m);
        assert_eq!(m.status, "Needs admin");
        assert_eq!(m.banner.unwrap().button.as_deref(), Some("Restart as admin"));
        assert_eq!(m.footer, "Not admin");
    }

    #[test]
    fn too_old_windows_says_why_and_offers_windows_update() {
        let (mut a, _) = app_with(vec![iface(Kind::Wifi, "{W}", 2, true, true, false)]);
        a.cfg.set_route(r"C:\B\brave.exe", "Brave", Route::Wifi);
        a.error = Some(EngineError::too_old());
        let mut m = Model::default();
        a.fill(&mut m);
        assert_eq!(m.status, "Not supported");
        let b = m.banner.unwrap();
        assert!(b.text.starts_with("NF Switcher needs Windows 11 24H2 or later.") && b.text.contains("free"));
        assert_eq!(b.button.as_deref(), Some("Open Windows Update"));
        assert_eq!(m.apps[0].status, "Needs Windows 11 24H2 or later");

        let win = |name: &str, windows10, updatable| engine::ThisWindows { name: name.into(), windows10, updatable };
        let b = too_old_banner(Some(&win("Windows 11 23H2", false, true)));
        assert!(b.text.contains("This PC has Windows 11 23H2, which doesn't have"));
        assert!(b.text.ends_with("free in Windows Update if this PC meets Windows 11's requirements."));
        assert_eq!(b.button.as_deref(), Some("Open Windows Update"));
        // Windows 10 gets Windows 11 as an upgrade.
        let b = too_old_banner(Some(&win("Windows 10 22H2", true, true)));
        assert!(b.text.contains("free upgrade") && b.button.is_some());
        // LTSC and Server: Windows Update has nothing to offer, so no promise and no button.
        for w in [win("Windows 10 LTSC 21H2", true, false), win("Windows Server 2022 Standard", false, false)] {
            let b = too_old_banner(Some(&w));
            assert!(b.text.ends_with("it uses to pin apps.") && !b.text.contains("free"), "{}", b.text);
            assert!(b.button.is_none());
        }
        let b = too_old_banner(None);
        assert!(b.text.contains("This Windows doesn't have") && b.button.is_some());
    }

    #[test]
    fn wifi_is_left_alone_when_nothing_can_be_pinned() {
        let (mut a, _) = app_with(vec![iface(Kind::Wifi, "{W}", 2, false, true, false)]);
        a.cfg.set_route(r"C:\B\brave.exe", "Brave", Route::Wifi);
        assert!(a.wifi_wanted());
        for e in [EngineError::too_old(), EngineError::NeedsAdmin] {
            a.error = Some(e);
            assert!(!a.wifi_wanted());
        }
    }
}
