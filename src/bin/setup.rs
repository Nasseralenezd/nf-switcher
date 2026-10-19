#![windows_subsystem = "windows"]
// NF Switcher Setup: installs, updates and uninstalls NF Switcher (concept B: one small window in the flyout's look).
// The app is built into this exe (NFSW_APP_EXE at build time, see tools/build-setup.ps1).
//
//   NF-Switcher-Setup.exe                 the options page; Install asks for admin (UAC) and carries on elevated
//   uninstall.exe --uninstall [--quiet]   the uninstall page (Settings › Apps runs this); --quiet: no window
//   ... --update                          NF Switcher's "Update now" (as admin): updates with the current choices,
//                                         no options page, then starts the new NF Switcher and closes
//   ... --sandbox DIR                     everything under DIR, no registry, no task, no Program Files (tests)
use nf_switcher::installer::{self, Layout, Options};
use nf_switcher::ui::paint::{base, Fonts, Painter};
use nf_switcher::ui::setup_view::{render, Frame, Hit, Model, Page, WIDTH};
use nf_switcher::ui::theme::Theme;
use nf_switcher::{autostart, elevate};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static APP: &[u8] = include_bytes!(env!("NFSW_APP_EXE"));

const WM_STEP: u32 = WM_APP + 1;
const WM_FINISHED: u32 = WM_APP + 2;
const CLASS: PCWSTR = w!("NFSwitcherSetup");

/// The worker's error message, read when WM_FINISHED arrives (wparam 0 = ok, 1 = failed).
static ERROR: Mutex<String> = Mutex::new(String::new());

struct Args {
    uninstall: bool,
    quiet: bool,
    /// Relaunched elevated: start the work at once.
    run: bool,
    /// Started by NF Switcher's "Update now".
    update: bool,
    dir: Option<PathBuf>,
    autostart: bool,
    desktop: bool,
    remove_settings: bool,
    user: Option<String>,
    pos: Option<(i32, i32)>,
    sandbox: Option<PathBuf>,
    /// Sandbox tests only: close by itself when done and write the result next to the sandbox.
    auto: bool,
}

fn parse_args() -> Args {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let flag = |k: &str| a.iter().any(|x| x == k);
    Args {
        uninstall: flag("--uninstall") || flag("--uninstall-now"),
        quiet: flag("--quiet"),
        run: flag("--install") || flag("--uninstall-now"),
        update: flag("--update") && !flag("--uninstall") && !flag("--uninstall-now"),
        dir: val("--dir").map(PathBuf::from),
        autostart: val("--autostart").map(|v| v == "1").unwrap_or(true),
        desktop: val("--desktop").map(|v| v == "1").unwrap_or(false),
        remove_settings: val("--remove-settings").map(|v| v == "1").unwrap_or(false),
        user: val("--user"),
        pos: val("--pos").and_then(|p| {
            let (x, y) = p.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        }),
        sandbox: val("--sandbox").map(PathBuf::from),
        auto: flag("--auto") && flag("--sandbox"),
    }
}

struct Setup {
    hwnd: HWND,
    m: Model,
    frame: Frame,
    theme: Theme,
    factory: ID2D1Factory,
    fonts: Fonts,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: u32,
    layout: Layout,
    user: String,
    sandbox: Option<PathBuf>,
    tracking: bool,
    installed_exe: Option<PathBuf>,
    auto: bool,
    /// "Update now": start the new NF Switcher and close when done.
    update: bool,
}

enum Effect {
    None,
    PickFolder,
    Elevate(Vec<String>),
    Close,
}

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Setup) -> R) -> Option<R> {
    SETUP.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn lo(v: isize) -> i32 {
    (v & 0xffff) as i16 as i32
}
fn hi(v: isize) -> i32 {
    ((v >> 16) & 0xffff) as i16 as i32
}

impl Setup {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Lays out, resizes the window to fit (keeping its top-left), and repaints.
    fn refresh(&mut self) {
        self.frame = render(None, &self.theme, &self.m, &self.fonts);
        let s = self.scale();
        let (w, h) = ((WIDTH * s).round() as i32, (self.frame.height * s).round() as i32);
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(rt) = &self.rt {
                let _ = rt.Resize(&D2D_SIZE_U { width: w as u32, height: h as u32 });
            }
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &mut ps);
            if self.rt.is_none() {
                let mut rc = RECT::default();
                let _ = GetClientRect(self.hwnd, &mut rc);
                let props = D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: self.dpi as f32,
                    dpiY: self.dpi as f32,
                    ..Default::default()
                };
                let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: self.hwnd,
                    pixelSize: D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 },
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                };
                self.rt = self.factory.CreateHwndRenderTarget(&props, &hprops).ok();
            }
            if let Some(rt) = self.rt.clone() {
                let b = base(&rt);
                if let Ok(p) = Painter::new(&b, &self.fonts, &self.factory) {
                    b.BeginDraw();
                    self.frame = render(Some(&p), &self.theme, &self.m, &self.fonts);
                    if b.EndDraw(None, None).is_err() {
                        self.rt = None;
                    }
                }
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn position(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        (r.left, r.top)
    }

    /// Starts the install or uninstall on a worker thread; steps and the end come back as messages.
    fn start_work(&mut self) {
        let uninstalling = matches!(self.m.page, Page::Uninstall) || self.m.uninstalling;
        self.m.uninstalling = uninstalling;
        self.m.steps = if uninstalling { installer::UNINSTALL_STEPS } else { installer::INSTALL_STEPS }
            .iter()
            .map(|s| s.to_string())
            .collect();
        if !uninstalling {
            self.m.steps.pop(); // "Done" shows as its own page
        }
        self.m.step = 0;
        self.m.page = Page::Working;
        self.refresh();
        let hwnd = self.hwnd.0 as usize;
        let layout = clone_layout(&self.layout);
        let opts = Options { autostart: self.m.autostart, desktop: self.m.desktop, user: self.user.clone() };
        let remove = self.m.remove_settings;
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let post = |msg: u32, wp: usize| unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(wp), LPARAM(0));
            };
            let mut step = |i: usize| post(WM_STEP, i);
            let r = if uninstalling {
                installer::uninstall(&layout, remove, &mut step)
            } else {
                let me = std::env::current_exe().unwrap_or_default();
                installer::install(&layout, &opts, APP, &me, &mut step)
            };
            let failed = r.is_err();
            if let Err(e) = r {
                if let Ok(mut s) = ERROR.lock() {
                    *s = e;
                }
            }
            post(WM_FINISHED, failed as usize);
        });
    }

    /// The arguments for an elevated copy that carries on from here.
    fn elevated_args(&self) -> Vec<String> {
        let (x, y) = self.position();
        let mut a = if self.m.page == Page::Uninstall {
            vec!["--uninstall-now".into(), "--remove-settings".into(), (self.m.remove_settings as u8).to_string()]
        } else {
            vec![
                "--install".into(),
                "--dir".into(),
                self.layout.dir.to_string_lossy().into_owned(),
                "--autostart".into(),
                (self.m.autostart as u8).to_string(),
                "--desktop".into(),
                (self.m.desktop as u8).to_string(),
            ]
        };
        a.extend(["--user".into(), self.user.clone(), "--pos".into(), format!("{x},{y}")]);
        a
    }

    fn click(&mut self, h: Hit) -> Effect {
        let working = self.m.page == Page::Working;
        match h {
            Hit::Close if !working => return Effect::Close,
            Hit::Change => return Effect::PickFolder,
            Hit::Autostart => self.m.autostart = !self.m.autostart,
            Hit::Desktop => self.m.desktop = !self.m.desktop,
            Hit::RemoveSettings => self.m.remove_settings = !self.m.remove_settings,
            Hit::Install | Hit::Uninstall => {
                if self.m.admin || self.sandbox.is_some() {
                    self.start_work();
                } else {
                    return Effect::Elevate(self.elevated_args());
                }
            }
            Hit::Open => {
                if let Some(exe) = &self.installed_exe {
                    // This setup runs as admin, so NF Switcher starts as admin too: no second UAC prompt.
                    let _ = std::process::Command::new(exe).spawn();
                }
                return Effect::Close;
            }
            _ => {}
        }
        self.refresh();
        Effect::None
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> (Option<LRESULT>, Effect) {
        let done = (Some(LRESULT(0)), Effect::None);
        match msg {
            WM_PAINT => {
                self.paint();
                done
            }
            WM_ERASEBKGND => (Some(LRESULT(1)), Effect::None),
            WM_NCHITTEST => {
                // Drag the window by any spot that isn't a button.
                let mut pt = POINT { x: lo(lp.0), y: hi(lp.0) };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let s = self.scale();
                let over = self.frame.hit(pt.x as f32 / s, pt.y as f32 / s).is_some();
                (Some(LRESULT(if over { HTCLIENT } else { HTCAPTION } as isize)), Effect::None)
            }
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut t);
                    }
                    self.tracking = true;
                }
                let s = self.scale();
                let h = self.frame.hit(lo(lp.0) as f32 / s, hi(lp.0) as f32 / s);
                if h != self.m.hover {
                    self.m.hover = h;
                    unsafe {
                        let _ = InvalidateRect(Some(self.hwnd), None, false);
                    }
                }
                done
            }
            0x02A3 /* WM_MOUSELEAVE */ => {
                self.tracking = false;
                self.m.hover = None;
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
                done
            }
            WM_LBUTTONDOWN => {
                let s = self.scale();
                self.m.pressed = self.frame.hit(lo(lp.0) as f32 / s, hi(lp.0) as f32 / s);
                done
            }
            WM_LBUTTONUP => {
                let s = self.scale();
                let h = self.frame.hit(lo(lp.0) as f32 / s, hi(lp.0) as f32 / s);
                let pressed = self.m.pressed.take();
                if let Some(h) = h.filter(|h| Some(*h) == pressed) {
                    return (Some(LRESULT(0)), self.click(h));
                }
                done
            }
            WM_KEYDOWN => match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE if self.m.page != Page::Working => (Some(LRESULT(0)), Effect::Close),
                VK_RETURN => {
                    let h = match self.m.page {
                        Page::Options => Hit::Install,
                        Page::Uninstall => Hit::Uninstall,
                        Page::Done => Hit::Open,
                        Page::Working => return done,
                        _ => Hit::Close,
                    };
                    (Some(LRESULT(0)), self.click(h))
                }
                _ => (None, Effect::None),
            },
            WM_STEP => {
                self.m.step = wp.0;
                self.refresh();
                done
            }
            WM_FINISHED => {
                if wp.0 == 0 {
                    self.m.page = if self.m.uninstalling { Page::Removed } else { Page::Done };
                    self.m.step = self.m.steps.len();
                    self.installed_exe = Some(self.layout.exe());
                } else {
                    self.m.page = Page::Failed;
                    self.m.error = ERROR.lock().map(|s| s.clone()).unwrap_or_default();
                }
                self.refresh();
                if self.update && wp.0 == 0 {
                    // Back to where the user was: the new NF Switcher, as admin like this setup (no UAC prompt).
                    if self.sandbox.is_none() {
                        let _ = std::process::Command::new(self.layout.exe()).spawn();
                    }
                    unsafe { SetTimer(Some(self.hwnd), 1, 1500, None) };
                }
                if self.auto {
                    if let Some(root) = &self.sandbox {
                        let r = if wp.0 == 0 { "ok".to_string() } else { format!("failed: {}", self.m.error) };
                        let _ = std::fs::write(root.join("result.txt"), format!("{:?} {r}", self.m.page));
                    }
                    unsafe { SetTimer(Some(self.hwnd), 1, 1500, None) };
                }
                done
            }
            WM_TIMER if self.auto || self.update => (Some(LRESULT(0)), Effect::Close),
            WM_CLOSE if self.m.page == Page::Working => done, // not in the middle of it
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                done
            }
            WM_DPICHANGED => {
                self.dpi = hi(wp.0 as isize) as u32;
                if let Some(rt) = &self.rt {
                    unsafe { rt.SetDpi(self.dpi as f32, self.dpi as f32) };
                }
                self.refresh();
                done
            }
            _ => (None, Effect::None),
        }
    }
}

fn clone_layout(l: &Layout) -> Layout {
    Layout {
        dir: l.dir.clone(),
        start_menu: l.start_menu.clone(),
        desktop: l.desktop.clone(),
        data: l.data.clone(),
        sandbox: l.sandbox.clone(),
    }
}

/// "Change": pick a folder; NF Switcher goes into an "NF Switcher" folder inside it.
fn pick_folder(owner: HWND) -> Option<PathBuf> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let _ = dlg.SetOptions(dlg.GetOptions().ok()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetTitle(w!("Where should NF Switcher go?"));
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        let mut path = PathBuf::from(s?);
        if !path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("NF Switcher")) {
            path.push("NF Switcher");
        }
        Some(path)
    }
}

fn run_effect(hwnd: HWND, e: Effect) {
    match e {
        Effect::None => {}
        Effect::Close => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        Effect::PickFolder => {
            if let Some(dir) = pick_folder(hwnd) {
                with(|s| {
                    s.layout = match &s.sandbox {
                        Some(_) => clone_layout(&s.layout),
                        None => Layout::real(dir.clone()),
                    };
                    s.m.dir = dir.to_string_lossy().into_owned();
                    s.refresh();
                });
            }
        }
        Effect::Elevate(args) => {
            if elevate::relaunch_as_admin(&args) == elevate::Relaunch::Started {
                // The admin copy carries on in the same spot; this one goes.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match with(|s| s.handle(msg, wp, lp)) {
        Some((r, e)) => {
            run_effect(hwnd, e);
            r.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
        }
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// --uninstall --quiet: no window (for scripts and winget). Asks for admin if needed.
fn quiet_uninstall(a: &Args) -> i32 {
    let layout = match &a.sandbox {
        Some(root) => Layout::sandbox(root),
        None => Layout::real(installer::installed().map(|(d, _)| d).unwrap_or_else(installer_dir_of_self)),
    };
    if a.sandbox.is_none() && !elevate::is_elevated() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        return match elevate::relaunch_as_admin(&args) {
            elevate::Relaunch::Started => 0,
            _ => 1,
        };
    }
    let code = match installer::uninstall(&layout, a.remove_settings, &mut |_| {}) {
        Ok(()) => 0,
        Err(_) => 1,
    };
    installer::remove_after_exit();
    code
}

/// When run as uninstall.exe from the install folder, that folder.
fn installer_dir_of_self() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(Layout::default_dir)
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let a = parse_args();
    if a.uninstall && a.quiet {
        std::process::exit(quiet_uninstall(&a));
    }
    // Where it goes: an update keeps the installed folder.
    let installed = installer::installed();
    let dir = a
        .dir
        .clone()
        .or_else(|| installed.as_ref().map(|(d, _)| d.clone()))
        .or_else(|| a.uninstall.then(installer_dir_of_self))
        .unwrap_or_else(Layout::default_dir);
    let layout = match &a.sandbox {
        Some(root) => Layout::sandbox(root),
        None => Layout::real(dir),
    };
    let admin = elevate::is_elevated();
    let mut m = Model {
        page: if a.uninstall { Page::Uninstall } else { Page::Options },
        installed: installed.as_ref().filter(|_| a.sandbox.is_none()).map(|(_, v)| v.clone()),
        dir: layout.dir.to_string_lossy().into_owned(),
        autostart: if a.run { a.autostart } else { installed.is_none() || autostart::exists() || a.sandbox.is_some() },
        desktop: a.desktop || (installed.is_some() && layout.desktop.exists()),
        remove_settings: a.remove_settings,
        admin,
        uninstalling: a.uninstall,
        ..Default::default()
    };
    if m.installed.is_none() {
        m.autostart = if a.run { a.autostart } else { true };
    }
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_APPWINDOW,
            CLASS,
            &HSTRING::from(if a.uninstall {
                "Uninstall NF Switcher"
            } else if a.update {
                "Updating NF Switcher"
            } else {
                "NF Switcher Setup"
            }),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            0,
            100,
            100,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let factory: ID2D1Factory = match D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) {
            Ok(f) => f,
            Err(_) => return,
        };
        let Ok(fonts) = Fonts::new() else { return };
        let user = a.user.clone().unwrap_or_else(autostart::current_user);
        let setup = Setup {
            hwnd,
            m,
            frame: Frame { height: 300.0, hits: Vec::new() },
            theme: Theme::current(),
            factory,
            fonts,
            rt: None,
            dpi,
            layout,
            user,
            sandbox: a.sandbox.clone(),
            tracking: false,
            installed_exe: None,
            auto: a.auto,
            update: false,
        };
        SETUP.with(|c| *c.borrow_mut() = Some(setup));
        with(|s| {
            s.refresh();
            // Centre on the screen, or where the copy without admin was.
            let (w, h) = ((WIDTH * s.scale()).round() as i32, (s.frame.height * s.scale()).round() as i32);
            let (x, y) = a.pos.unwrap_or_else(|| {
                let mut work = RECT::default();
                let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
                ((work.left + work.right - w) / 2, (work.top + work.bottom - h) / 2)
            });
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            // An update keeps the current folder, "Start with Windows" and desktop shortcut (read above). Without
            // admin it's the normal options page instead, whose Update button asks for it.
            if (a.run || a.update) && (admin || a.sandbox.is_some()) {
                s.update = a.update;
                s.start_work();
            }
        });
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        SETUP.with(|c| c.borrow_mut().take());
    }
    installer::remove_after_exit();
}
