#![windows_subsystem = "windows"]
// NF Switcher: pick, per app, whether it uses Wi-Fi or Windows' default route (Ethernet). See README.md, and
// engine/wfp.rs for how the routing works and its trade-offs.
//
//   nf-switcher.exe              asks for admin (UAC), then lives in the tray and opens the flyout once
//   nf-switcher.exe --tray       the same without opening the flyout (for starting with Windows)
//   nf-switcher.exe --no-elevate never asks for admin: read-only, routing says "Needs admin"
//   nf-switcher.exe --smoke      hidden check: no window, no tray, no admin, data in %TEMP%; prints one SMOKE line
//   nf-switcher.exe --shots DIR  renders the flyout's sample states to PNG, offscreen
//   nf-switcher.exe --selftest   (admin) pins a throwaway probe copy and checks the routing; report in %TEMP%
//   nf-switcher.exe --uitest [DIR] shows the real flyout ~12 s, clicks through it, captures it (no admin)
//   nf-switcher.exe --smoke-idle N  the real hidden window for N seconds, no tray icon, no admin (memory checks)

use nf_switcher::{app, config, elevate, engine, procs, secure, selftest, ui};

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn shots(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    for (name, m) in ui::sample::all() {
        for (tname, th) in [("dark", ui::theme::Theme::dark(false)), ("light", ui::theme::Theme::light(false))] {
            let out = dir.join(format!("{name}-{tname}.png"));
            match ui::shot::save_png(&m, &th, 2.0, 560.0, &out) {
                Ok(h) => println!("SHOT {} {}x{}", out.display(), ui::view::WIDTH, h),
                Err(e) => println!("SHOT-ERROR {} {e}", out.display()),
            }
        }
    }
    // The installer's states too (its window shares the flyout's look).
    for (name, m) in ui::setup_view::samples() {
        for (tname, th) in [("dark", ui::theme::Theme::dark(false)), ("light", ui::theme::Theme::light(false))] {
            let out = dir.join(format!("{name}-{tname}.png"));
            match ui::shot::save_setup_png(&m, &th, 2.0, &out) {
                Ok(h) => println!("SHOT {} {}x{}", out.display(), ui::setup_view::WIDTH, h),
                Err(e) => println!("SHOT-ERROR {} {e}", out.display()),
            }
        }
    }
}

/// The hidden check: the real adapters, processes and settings file, the real flyout model and layout, PNGs of it,
/// and no routing (it refuses to run as admin, so it can't change anything).
fn smoke() -> i32 {
    if elevate::is_elevated() {
        println!("SMOKE {}", serde_json::json!({"ok": false, "error": "refusing to run --smoke as admin"}));
        return 2;
    }
    let dir = std::env::temp_dir().join("nf-switcher-smoke");
    std::env::set_var("NFSW_DATA_DIR", &dir);
    let _ = std::fs::create_dir_all(&dir);
    let t0 = std::time::Instant::now();
    let mut app = app::App::new(false, Err(engine::EngineError::NeedsAdmin));
    let t_start = t0.elapsed().as_millis();
    let t1 = std::time::Instant::now();
    app.refresh_activity();
    let t_activity = t1.elapsed().as_millis();
    let mut m = ui::view::Model::default();
    app.fill(&mut m);
    let shots_dir = dir.join("shots");
    let _ = std::fs::create_dir_all(&shots_dir);
    let th = ui::theme::Theme::current();
    let mut heights = serde_json::Map::new();
    let mut errors = Vec::new();
    for page in [ui::view::Page::Main, ui::view::Page::Add, ui::view::Page::Settings] {
        m.page = page;
        let name = format!("{page:?}").to_lowercase();
        match ui::shot::save_png(&m, &th, 1.5, 640.0, &shots_dir.join(format!("{name}.png"))) {
            Ok(h) => {
                heights.insert(name, h.into());
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    let adapters: Vec<_> = app
        .ifaces
        .iter()
        .map(|i| {
            serde_json::json!({
                "alias": i.alias, "kind": format!("{:?}", i.kind), "status": format!("{:?}", i.status),
                "ipv4": i.ipv4.iter().map(|a| a.to_string()).collect::<Vec<_>>(), "ipv6": i.ipv6.len(),
                "gateway": i.gw4.map(|g| g.to_string()), "default_route": i.default_route, "ssid": i.ssid.is_some(),
            })
        })
        .collect();
    let out = serde_json::json!({
        "ok": errors.is_empty(),
        "admin": app.admin,
        "status": m.status,
        "banner": m.banner.as_ref().map(|b| b.text.clone()),
        "eth_tile": format!("{} | {}", m.eth.title, m.eth.sub),
        "wifi_tile": format!("{} | {}", m.wifi.title, m.wifi.sub),
        "adapters": adapters,
        "processes": app.procs.len(),
        "sockets": app.socks.len(),
        "add_list": m.running,
        "apps_in_config": app.cfg.apps.len(),
        "heights": heights,
        "routepolicies_enabled": engine::routepolicies::read(),
        "ms": {"start": t_start, "activity": t_activity},
        "shots": shots_dir.display().to_string(),
        "errors": errors,
    });
    println!("SMOKE {out}");
    if errors.is_empty() { 0 } else { 1 }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    if let Some(i) = args.iter().position(|a| a == "--shots") {
        shots(std::path::Path::new(args.get(i + 1).map(String::as_str).unwrap_or(".")));
        return;
    }
    if has(&args, "--smoke") {
        std::process::exit(smoke());
    }
    if has(&args, "--check-update") {
        // The same request the background check makes, printed (read-only; for testing a release).
        let r = nf_switcher::update::latest();
        let current = nf_switcher::update::current();
        let out = match &r {
            Ok(Some(rel)) => serde_json::json!({"current": current, "latest": rel.tag, "url": rel.url, "newer": rel.version > current}),
            Ok(None) => serde_json::json!({"current": current, "latest": null}),
            Err(e) => serde_json::json!({"current": current, "error": e}),
        };
        println!("UPDATE {out}");
        std::process::exit(if r.is_ok() { 0 } else { 1 });
    }
    if let Some(i) = args.iter().position(|a| a == "--uitest") {
        // The real flyout, clicked through with posted messages and captured; no admin. Shows it for ~12 s.
        let dir = args
            .get(i + 1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("nf-switcher-uitest"));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("NFSW_DATA_DIR", &dir);
        // Up to three real programs from this PC in the list, the first pinned, so rows, icons and statuses are real.
        let mut cfg = config::Config::default();
        let mut seen = std::collections::HashSet::new();
        for p in procs::list().into_iter().filter_map(|p| p.path) {
            if procs::is_windows_path(&p) || seen.len() >= 3 || !seen.insert(config::norm(&p)) {
                continue;
            }
            let route = if cfg.apps.is_empty() { config::Route::Wifi } else { config::Route::Default };
            cfg.set_route(&p, &procs::display_name(&p), route);
        }
        let _ = cfg.save();
        let app = app::App::new(elevate::is_elevated(), Err(engine::EngineError::NeedsAdmin));
        let opts = ui::window::Options {
            class: windows::core::w!("NFSwitcherFlyoutTest"),
            show: true,
            tray: true,
            quit_after: Some(std::time::Duration::from_secs(40)),
            uitest: Some(dir),
        };
        let _ = ui::window::run(app, opts);
        return;
    }
    if has(&args, "--selftest") {
        std::process::exit(selftest::run());
    }
    if let Some(i) = args.iter().position(|a| a == "--selftest-child") {
        let pinned = args.get(i + 1).cloned().unwrap_or_default();
        let ready = args.get(i + 2).cloned().unwrap_or_default();
        std::process::exit(selftest::child(&pinned, &ready));
    }
    if let Some(i) = args.iter().position(|a| a == "--smoke-idle") {
        // For measuring memory: the real window and message loop, hidden, no tray icon, no admin, then quit.
        if elevate::is_elevated() {
            return;
        }
        std::env::set_var("NFSW_DATA_DIR", std::env::temp_dir().join("nf-switcher-smoke"));
        let secs = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(10u64);
        let app = app::App::new(false, Err(engine::EngineError::NeedsAdmin));
        let opts = ui::window::Options {
            class: windows::core::w!("NFSwitcherFlyoutTest"),
            show: false,
            tray: false,
            quit_after: Some(std::time::Duration::from_secs(secs)),
            uitest: None,
        };
        let _ = ui::window::run(app, opts);
        return;
    }

    // Test-only switches never steer a normal run (the elevated app must use its admin-only folder).
    std::env::remove_var("NFSW_DATA_DIR");
    std::env::remove_var("NFSW_ROUTEPOLICIES");
    // Started by "Restart as admin": the copy without admin is quitting right now; give it a moment to let go.
    if has(&args, "--elevated") {
        for _ in 0..50 {
            if !elevate::other_instance() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    // A second start just opens the running copy's flyout (before any UAC prompt).
    if elevate::other_instance() {
        elevate::wake_other();
        return;
    }
    if !elevate::is_elevated() && !has(&args, "--no-elevate") && !has(&args, "--elevated") {
        let mut again = args.clone();
        again.push("--elevated".into());
        match elevate::relaunch_as_admin(&again) {
            elevate::Relaunch::Started => return,
            // Declined or failed: carry on without admin; the flyout says what's missing.
            r => app::log(&format!("no admin: {r:?}")),
        }
    }
    let Some(_instance) = elevate::take_instance() else {
        elevate::wake_other();
        return;
    };
    let admin = elevate::is_elevated();
    let secured = if admin { secure::prepare(&secure::machine_dir()) } else { Ok(()) };
    let engine = match secured {
        Err(e) => Err(engine::EngineError::Failed(format!("can't secure NF Switcher's settings folder: {e}"))),
        Ok(()) if admin => engine::open_real(),
        Ok(()) => Err(engine::EngineError::NeedsAdmin),
    };
    if let Err(e) = &engine {
        app::log(&format!("engine: {e}"));
    }
    let app = app::App::new(admin, engine);
    let show = !has(&args, "--tray") || has(&args, "--show");
    if let Err(e) = ui::window::run(app, ui::window::Options { class: elevate::WINDOW_CLASS, show, tray: true, quit_after: None, uitest: None }) {
        app::log(&format!("window: {e}"));
    }
}
