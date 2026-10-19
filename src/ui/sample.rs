// Sample flyout states for --shots (the look is checked offscreen, never in a visible window). Real paths are
// used where they exist on this PC so the icons can be checked too.
use super::view::*;

fn tiles(m: &mut Model) {
    m.eth = Tile { title: "Ethernet · default".into(), sub: "192.168.1.20".into(), on: false, off: false };
    m.wifi = Tile { title: "Wi-Fi · Home-5G".into(), sub: "192.168.0.34".into(), on: true, off: false };
}

pub fn main_page() -> Model {
    let mut m = Model { status: "3 on Wi-Fi".into(), footer: "Admin".into(), update: Some("0.2.0".into()), ..Default::default() };
    tiles(&mut m);
    m.apps = vec![
        AppItem {
            name: "Brave".into(),
            path: r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe".into(),
            wifi: true,
            status: "Running · 38 on Wi-Fi".into(),
            warn: false,
        },
        AppItem {
            name: "Spotify".into(),
            path: r"C:\Users\You\AppData\Roaming\Spotify\Spotify.exe".into(),
            wifi: true,
            status: "Running · 6 on Wi-Fi · 2 still on Ethernet".into(),
            warn: false,
        },
        AppItem {
            name: "qBittorrent".into(),
            path: r"C:\Program Files\qBittorrent\qbittorrent.exe".into(),
            wifi: true,
            status: "Waits until it starts".into(),
            warn: false,
        },
        AppItem {
            name: "Discord".into(),
            path: r"C:\Users\You\AppData\Local\Discord\app-1.0.9213\Discord.exe".into(),
            wifi: false,
            status: "Running · default route".into(),
            warn: false,
        },
    ];
    m.hover = Some(Hit::AppRow(1));
    m
}

/// The README's picture: the main page without hover or update dot. The apps' real icons are used where they're
/// installed on the PC that renders it (paths from the environment, so no user name ends up in the code).
pub fn hero() -> Model {
    let mut m = main_page();
    m.hover = None;
    m.update = None;
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let discord = std::fs::read_dir(format!(r"{}\Discord", env("LOCALAPPDATA")))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("Discord.exe"))
        .find(|p| p.exists());
    let real = [
        Some(format!(r"{}\BraveSoftware\Brave-Browser\Application\brave.exe", env("LOCALAPPDATA")).into()),
        Some(format!(r"{}\Spotify\Spotify.exe", env("APPDATA")).into()),
        None,
        discord,
    ];
    for (a, p) in m.apps.iter_mut().zip(real) {
        if let Some(p) = p.filter(|p: &std::path::PathBuf| p.exists()) {
            a.path = p.display().to_string();
        }
    }
    m
}

pub fn wifi_down() -> Model {
    let mut m = main_page();
    m.status = "Wi-Fi down".into();
    m.wifi = Tile { title: "Wi-Fi".into(), sub: "Not connected".into(), on: false, off: true };
    m.banner = Some(Banner {
        tone: Tone::Warn,
        text: "Wi-Fi isn't connected. Pinned apps use the default route until it's back.".into(),
        button: None,
    });
    for a in m.apps.iter_mut().filter(|a| a.wifi) {
        a.status = "Wi-Fi is down · using the default route".into();
        a.warn = true;
    }
    m.hover = None;
    m
}

pub fn needs_admin() -> Model {
    let mut m = Model { status: "Needs admin".into(), footer: "Not admin".into(), ..Default::default() };
    tiles(&mut m);
    m.wifi.on = false;
    m.banner = Some(Banner {
        tone: Tone::Warn,
        text: "Pinning apps to Wi-Fi needs admin rights. Windows will ask (UAC).".into(),
        button: Some("Restart as admin".into()),
    });
    m.hover = Some(Hit::BannerButton);
    m
}

/// Windows 11 before 24H2: the reason and the way out, and no UAC prompt (so not admin).
pub fn too_old() -> Model {
    let mut m = Model { status: "Not supported".into(), footer: "Not admin".into(), ..Default::default() };
    tiles(&mut m);
    m.wifi.on = false;
    let this = crate::engine::ThisWindows { name: "Windows 11 23H2".into(), windows10: false, updatable: true };
    m.banner = Some(crate::app::too_old_banner(Some(&this)));
    m.apps = vec![AppItem {
        name: "Brave".into(),
        path: r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe".into(),
        wifi: true,
        status: "Needs Windows 11 24H2 or later".into(),
        warn: true,
    }];
    m.hover = Some(Hit::BannerButton);
    m
}

/// The settings folder couldn't be made admin-only (here a planted file another program holds open).
pub fn insecure() -> Model {
    let mut m = Model { status: "Error".into(), footer: "Admin".into(), ..Default::default() };
    tiles(&mut m);
    m.wifi.on = false;
    m.banner = Some(crate::app::insecure_banner(
        r"C:\ProgramData\NF Switcher\config.json: can't secure or remove it: The process cannot access the file because it is being used by another process. (os error 32)",
    ));
    m.apps = vec![AppItem {
        name: "Brave".into(),
        path: r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe".into(),
        wifi: true,
        status: "Couldn't switch it (see the message above)".into(),
        warn: true,
    }];
    m.hover = None;
    m
}

pub fn add_page() -> Model {
    let mut m = Model { page: Page::Add, running: 38, search: "ste".into(), search_focus: true, caret: true, ..Default::default() };
    m.picks = vec![
        PickItem { name: "Steam".into(), path: r"C:\Program Files (x86)\Steam\steam.exe".into(), added: false, selected: true },
        PickItem {
            name: "Steam Client WebHelper".into(),
            path: r"C:\Program Files (x86)\Steam\bin\cef\cef.win64\steamwebhelper.exe".into(),
            added: false,
            selected: false,
        },
        PickItem {
            name: "SteelSeries GG".into(),
            path: r"C:\Program Files\SteelSeries\GG\SteelSeriesGG.exe".into(),
            added: true,
            selected: false,
        },
    ];
    // One of the two shown is ticked (Select all shows a dash), plus one the search hides now.
    m.selected = [r"C:\Program Files (x86)\Steam\steam.exe", r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"]
        .into_iter()
        .map(String::from)
        .collect();
    m.hover = Some(Hit::Pick(1));
    m
}

/// "Select all" with nothing ticked yet.
pub fn add_page_none() -> Model {
    let mut m = add_page();
    m.search.clear();
    m.search_focus = false;
    m.selected.clear();
    for p in &mut m.picks {
        p.selected = false;
    }
    m.hover = Some(Hit::SelectAll);
    m
}

pub fn settings_page() -> Model {
    let mut m = Model { page: Page::Settings, ..Default::default() };
    m.block_when_down = false;
    m.adapter_auto = true;
    m.adapters = vec![
        AdapterItem { name: "Wi-Fi".into(), sub: "Realtek 8922AE WiFi 7 · Home-5G".into(), selected: false },
        AdapterItem { name: "Ethernet 2".into(), sub: "USB tethering · 192.168.42.7".into(), selected: false },
    ];
    m.about = vec![
        format!("NF Switcher {}", env!("CARGO_PKG_VERSION")),
        "Pinned apps are sent over Wi-Fi with Windows' own connection policies (WFP). Nothing is changed for good: \
         closing NF Switcher removes them, and so does Windows if NF Switcher stops."
            .into(),
    ];
    m.hover = Some(Hit::WhenDown(true));
    m.update = Some("0.2.0".into());
    m.update_note = "Checked at 21:30: version 0.2.0 is out".into();
    m.update_sub = "NF Switcher 0.2.0 is out".into();
    m.update_button = Some("Update now".into());
    m
}

/// "Update now" while it downloads.
pub fn settings_updating() -> Model {
    let mut m = settings_page();
    m.update_sub = "Downloading… 45%".into();
    m.update_button = None;
    m.update_progress = Some(0.45);
    m.hover = None;
    m
}

pub fn all() -> Vec<(&'static str, Model)> {
    vec![
        ("main", main_page()),
        ("hero", hero()),
        ("wifi-down", wifi_down()),
        ("needs-admin", needs_admin()),
        ("too-old", too_old()),
        ("insecure", insecure()),
        ("add", add_page()),
        ("add-none", add_page_none()),
        ("settings", settings_page()),
        ("settings-updating", settings_updating()),
    ]
}
