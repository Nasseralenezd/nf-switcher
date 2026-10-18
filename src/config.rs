// What the user picked, kept in %ProgramData%\NF Switcher\config.json, a folder only admins can change (secure.rs):
// the elevated app turns this file into WFP rules. NFSW_DATA_DIR overrides it for the test modes only. Saving writes
// a temp file and renames it, so a crash mid-save never leaves half a file.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    #[default]
    Default,
    Wifi,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppRule {
    /// Full path of the .exe, as Windows reports it.
    pub path: String,
    /// What the UI shows (the file description, or the file name).
    pub name: String,
    pub route: Route,
}

/// What pinned apps do while Wi-Fi is down.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WifiDown {
    /// Their new connections use the default route until Wi-Fi is back.
    #[default]
    Fallback,
    /// Their new connections are blocked until Wi-Fi is back.
    Block,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub when_wifi_down: WifiDown,
    /// The Wi-Fi adapter to use (its GUID, AdapterName); None picks the first connected one.
    pub wifi_adapter: Option<String>,
    /// Pinned apps still reach devices on the Ethernet network (router page, NAS, printer) over Ethernet.
    pub lan_keep: bool,
    /// While apps are pinned, connect Wi-Fi again when Windows drops it (it does that to an idle Wi-Fi while
    /// Ethernet is plugged in: seen on this PC on 2026-10-18).
    pub reconnect_wifi: bool,
    /// Ask GitHub for a newer release about twice a day.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { when_wifi_down: WifiDown::default(), wifi_adapter: None, lan_keep: true, reconnect_wifi: true, check_updates: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub apps: Vec<AppRule>,
    pub settings: Settings,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: 1, apps: Vec::new(), settings: Settings::default() }
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("NFSW_DATA_DIR") {
        return PathBuf::from(d);
    }
    crate::secure::machine_dir()
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

impl Config {
    /// A missing or unreadable file gives the defaults; a broken file is kept as config.json.bad.
    pub fn load_from(path: &Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Config::default(),
            Err(_) => {
                // Unreadable (not UTF-8, locked): keep it aside rather than overwrite the user's list on next save.
                let _ = std::fs::rename(path, path.with_extension("json.bad"));
                return Config::default();
            }
        };
        match serde_json::from_str::<Config>(&text) {
            Ok(mut c) => {
                c.apps.retain(|a| !a.path.trim().is_empty());
                c
            }
            Err(_) => {
                let _ = std::fs::rename(path, path.with_extension("json.bad"));
                Config::default()
            }
        }
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    pub fn load() -> Config {
        Config::load_from(&config_path())
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&config_path())
    }

    pub fn find(&self, path: &str) -> Option<usize> {
        let key = norm(path);
        self.apps.iter().position(|a| norm(&a.path) == key)
    }

    /// Adds the app or changes its route. Returns its index.
    pub fn set_route(&mut self, path: &str, name: &str, route: Route) -> usize {
        match self.find(path) {
            Some(i) => {
                self.apps[i].route = route;
                i
            }
            None => {
                self.apps.push(AppRule { path: path.to_string(), name: name.to_string(), route });
                self.apps.len() - 1
            }
        }
    }

    /// "Revert all": every app back on the default route. The list stays, so it's one click to undo.
    pub fn revert_all(&mut self) {
        for a in &mut self.apps {
            a.route = Route::Default;
        }
    }

    pub fn pinned(&self) -> Vec<String> {
        self.apps.iter().filter(|a| a.route == Route::Wifi).map(|a| a.path.clone()).collect()
    }
}

/// Lower case, backslashes, no \\?\ prefix: the form paths are compared in.
pub fn norm(path: &str) -> String {
    let p = path.trim().replace('/', "\\");
    let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
    p.to_lowercase()
}

/// Does the process at `proc_path` belong to the rule for `rule_path`? The same file, or the same file in a
/// sibling version folder (Discord\app-1.0.9213\Discord.exe after an update to app-1.0.9214), so updates
/// don't silently drop a rule. Both paths must already be normalised with norm().
pub fn same_app(rule_path: &str, proc_path: &str) -> bool {
    if rule_path == proc_path {
        return true;
    }
    let (Some((rdir, rfile)), Some((pdir, pfile))) = (rule_path.rsplit_once('\\'), proc_path.rsplit_once('\\'))
    else {
        return false;
    };
    if rfile != pfile {
        return false;
    }
    let (Some((rroot, rver)), Some((proot, pver))) = (rdir.rsplit_once('\\'), pdir.rsplit_once('\\')) else {
        return false;
    };
    let versioned = |s: &str| s.contains('.') && s.bytes().any(|b| b.is_ascii_digit());
    rroot == proot && versioned(rver) && versioned(pver)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nfsw-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = temp("missing");
        assert_eq!(Config::load_from(&d.join("config.json")), Config::default());
    }

    #[test]
    fn round_trip() {
        let d = temp("roundtrip");
        let p = d.join("config.json");
        let mut c = Config::default();
        c.set_route(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe", "Brave", Route::Wifi);
        c.settings.when_wifi_down = WifiDown::Block;
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p), c);
        assert!(!p.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn broken_file_is_kept_aside() {
        let d = temp("broken");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
        assert!(p.with_extension("json.bad").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn set_route_is_case_insensitive_and_revert_keeps_the_list() {
        let mut c = Config::default();
        c.set_route(r"C:\Apps\Brave.exe", "Brave", Route::Wifi);
        c.set_route(r"c:/apps/brave.exe", "Brave", Route::Wifi);
        assert_eq!(c.apps.len(), 1);
        assert_eq!(c.pinned().len(), 1);
        c.revert_all();
        assert_eq!(c.apps.len(), 1);
        assert!(c.pinned().is_empty());
        assert_eq!(c.find(r"C:\APPS\brave.exe"), Some(0));
    }

    #[test]
    fn unknown_fields_and_old_files_still_load() {
        let d = temp("old");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.json");
        std::fs::write(&p, r#"{"apps":[{"path":"C:\\a.exe","name":"a","route":"wifi"}],"extra":1}"#).unwrap();
        let c = Config::load_from(&p);
        assert_eq!(c.apps[0].route, Route::Wifi);
        assert_eq!(c.settings, Settings::default());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn matching_follows_version_folders_only() {
        let rule = norm(r"C:\Users\R\AppData\Local\Discord\app-1.0.9213\Discord.exe");
        assert!(same_app(&rule, &norm(r"C:\Users\R\AppData\Local\Discord\app-1.0.9214\Discord.exe")));
        assert!(!same_app(&rule, &norm(r"C:\Users\R\AppData\Local\Other\app-1.0.9214\Discord.exe")));
        let brave = norm(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe");
        assert!(same_app(&brave, &norm(r"\\?\C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe")));
        assert!(!same_app(&brave, &norm(r"C:\Program Files\BraveSoftware\Brave-Browser-Beta\Application\brave.exe")));
        assert!(!same_app(&brave, &norm(r"C:\Program Files\BraveSoftware\Brave-Browser\Other\brave.exe")));
    }
}
