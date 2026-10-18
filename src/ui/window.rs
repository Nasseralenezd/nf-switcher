// The tray icon and the flyout window (concept C): a borderless, rounded popup above the clock that hides when it
// loses focus, like Windows' own quick settings. Direct2D draws it; its render target is released while hidden,
// and the working set trimmed, so an idle NF Switcher costs little memory.
//
// Message handling borrows the state from a thread-local; anything that runs a modal loop (menus, the file dialog,
// the UAC relaunch) runs after that borrow is released, so re-entrant messages still find the state.
use super::icons::IconCache;
use super::paint::{base, Fonts, Painter};
use super::theme::Theme;
use super::view::{render, Frame, Hit, Model, Page, WIDTH};
use crate::app::App;
use crate::elevate;
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::Controls::{LoadIconMetric, LIM_SMALL, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
const WM_NETCHANGE: u32 = WM_APP + 2;
const WM_UPDATE: u32 = WM_APP + 3;
const T_NET: usize = 1;
const T_ACTIVITY: usize = 2;
const T_CARET: usize = 3;
const T_RESYNC: usize = 4;
const T_QUIT: usize = 5;
const T_UITEST: usize = 6;
const T_FOCUS: usize = 7;
const T_VERSIONS: usize = 8;
const T_TRAY: usize = 9;
const T_UPDATE: usize = 10;
/// First update check 20 s after start, then every 12 hours.
const UPDATE_FIRST_MS: u32 = 20_000;
const UPDATE_EVERY_MS: u32 = 12 * 60 * 60 * 1000;
const TRAY_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const MAX_H: f32 = 640.0;
const MARGIN: f32 = 12.0;

pub struct Options {
    /// The window class: the real app's, or a separate one for test modes so a second start never wakes them.
    pub class: PCWSTR,
    /// Open the flyout once at start (not when started with --tray).
    pub show: bool,
    /// Add the tray icon (off only for the hidden --smoke-idle measurement).
    pub tray: bool,
    /// Quit by itself after this long (--smoke-idle).
    pub quit_after: Option<Duration>,
    /// --uitest: click through the real flyout with posted messages, capture it, write a report here, quit.
    pub uitest: Option<PathBuf>,
}

struct UiTest {
    dir: PathBuf,
    step: u32,
    log: Vec<serde_json::Value>,
}

/// What a message handler asks for once the state is no longer borrowed.
enum Effect {
    None,
    TrayMenu(POINT),
    RowMenu(usize, POINT),
    PickExe,
    RestartAdmin,
    /// Opening a link goes through Explorer (COM, which pumps messages), so outside the borrow too.
    OpenUrl(String),
    /// DestroyWindow re-enters the window procedure, so it must run outside the borrow too.
    Quit,
}

struct Flyout {
    hwnd: HWND,
    app: App,
    m: Model,
    frame: Frame,
    theme: Theme,
    factory: ID2D1Factory,
    fonts: Fonts,
    icons: IconCache,
    rt: Option<ID2D1HwndRenderTarget>,
    dpi: u32,
    size: (i32, i32),
    visible: bool,
    hidden_at: Option<Instant>,
    tracking: bool,
    /// A menu or dialog of ours is open: losing focus to it must not hide the flyout.
    modal: bool,
    show_msg: u32,
    taskbar_created: u32,
    _watcher: Option<crate::net::Watcher>,
    test: Option<UiTest>,
    /// The tray icon for the taskbar's theme, loaded once per theme (Shell_NotifyIcon copies it; a fresh one per
    /// update would leak). `icon_owned`: ours to destroy (not the stock fallback). `icon_dark`: the white one.
    icon: HICON,
    icon_owned: bool,
    icon_dark: bool,
    /// The icon is missing from the tray (adding failed): retried on a timer.
    tray_missing: u32,
    /// The foreground window right after showing: if Windows didn't let the flyout take focus, another window
    /// becoming the foreground is how we learn the user clicked away.
    fg_at_show: HWND,
}

thread_local! {
    static FLY: RefCell<Option<Flyout>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Flyout) -> R) -> Option<R> {
    FLY.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn lo(v: isize) -> i32 {
    (v & 0xffff) as i16 as i32
}
fn hi(v: isize) -> i32 {
    ((v >> 16) & 0xffff) as i16 as i32
}

/// The tray icon, no background like Windows' own: white on a dark taskbar, near black on a light one (res/app.rc).
/// Returns it and whether it's ours to destroy.
fn tray_icon(dark: bool) -> (HICON, bool) {
    unsafe {
        let hinst = GetModuleHandleW(None).ok().map(|h| HINSTANCE(h.0));
        let id: usize = if dark { 2 } else { 3 };
        match LoadIconMetric(hinst, PCWSTR(id as _), LIM_SMALL).or_else(|_| LoadIconMetric(hinst, PCWSTR(1 as _), LIM_SMALL)) {
            Ok(i) => (i, true),
            Err(_) => (LoadIconW(None, IDI_APPLICATION).unwrap_or_default(), false),
        }
    }
}

/// Is the taskbar dark, so the tray glyph should be white? Contrast themes leave SystemUsesLightTheme as it was, so
/// with one on, the window colour decides instead ("Desert" is near white, "Aquatic" black).
fn taskbar_dark(theme: &Theme) -> bool {
    unsafe {
        let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
        let on = SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
            .is_ok()
            && hc.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0;
        if on {
            let c = GetSysColor(COLOR_WINDOW);
            let (r, g, b) = (c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF);
            return 299 * r + 587 * g + 114 * b < 128_000;
        }
    }
    theme.dark
}

fn wide_fixed<const N: usize>(s: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (i, c) in s.encode_utf16().take(N - 1).enumerate() {
        out[i] = c;
    }
    out
}

impl Flyout {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn tray(&mut self, add: bool) {
        unsafe {
            let icon = self.icon;
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
                uCallbackMessage: WM_TRAY,
                hIcon: icon,
                szTip: wide_fixed(&format!(
                    "NF Switcher · {}{}",
                    self.app.status_text(),
                    if self.app.update.is_some() { " · update available" } else { "" }
                )),
                ..Default::default()
            };
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            if add {
                // At logon Explorer may be too busy to answer: keep trying for a minute rather than run iconless.
                let ok = Shell_NotifyIconW(NIM_ADD, &nid).as_bool() || Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool();
                if ok {
                    let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
                    self.tray_missing = 0;
                    let _ = KillTimer(Some(self.hwnd), T_TRAY);
                } else if self.tray_missing < 30 {
                    self.tray_missing += 1;
                    SetTimer(Some(self.hwnd), T_TRAY, 2000, None);
                }
            } else {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
        }
    }

    /// Reads Windows' theme again; when the taskbar went light or dark, the tray icon follows. Called on
    /// WM_SETTINGCHANGE, and also when the flyout opens and once a minute, in case UIPI kept that broadcast from
    /// the elevated window.
    fn follow_theme(&mut self) {
        self.theme = Theme::current();
        let dark = taskbar_dark(&self.theme);
        if dark != self.icon_dark {
            let (old, owned) = (self.icon, self.icon_owned);
            (self.icon, self.icon_owned) = tray_icon(dark);
            self.icon_dark = dark;
            self.tray(false);
            // Explorer keeps its own copy, so the old one can go.
            if owned {
                unsafe {
                    let _ = DestroyIcon(old);
                }
            }
        }
    }

    fn tray_remove(&self) {
        let nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            ..Default::default()
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    /// Fills the model from the app and works out the flyout's height (no drawing).
    fn refresh(&mut self) {
        self.app.fill(&mut self.m);
        self.layout();
    }

    fn layout(&mut self) {
        let max_h = self.max_height();
        self.icons_unbound_if_hidden();
        self.frame = render(None, &self.theme, &self.m, &self.fonts, &super::view::NoIcons, max_h);
        let ms = self.frame.max_scroll();
        if self.m.scroll > ms {
            self.m.scroll = ms;
            self.frame = render(None, &self.theme, &self.m, &self.fonts, &super::view::NoIcons, max_h);
        }
        if self.visible {
            self.place();
        }
        self.invalidate();
    }

    fn icons_unbound_if_hidden(&self) {
        if self.rt.is_none() {
            self.icons.unbind();
        }
    }

    /// The monitor the tray icon is on, its work area and DPI.
    fn monitor(&self) -> (RECT, RECT, u32) {
        unsafe {
            let id = NOTIFYICONIDENTIFIER {
                cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_ID,
                ..Default::default()
            };
            let pt = match Shell_NotifyIconGetRect(&id) {
                Ok(r) => POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 },
                Err(_) => {
                    let mut p = POINT::default();
                    let _ = GetCursorPos(&mut p);
                    p
                }
            };
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            (mi.rcMonitor, mi.rcWork, dx)
        }
    }

    fn max_height(&self) -> f32 {
        let (_, work, dpi) = self.monitor();
        let s = dpi as f32 / 96.0;
        (((work.bottom - work.top) as f32 / s) - 2.0 * MARGIN).min(MAX_H)
    }

    /// Puts the flyout next to the tray, on whichever edge the taskbar is.
    fn place(&mut self) {
        let (mon, work, dpi) = self.monitor();
        if dpi != self.dpi {
            self.dpi = dpi;
            if let Some(rt) = &self.rt {
                unsafe { rt.SetDpi(dpi as f32, dpi as f32) };
            }
        }
        let s = self.scale();
        let w = (WIDTH * s).round() as i32;
        let h = (self.frame.height * s).round() as i32;
        let m = (MARGIN * s).round() as i32;
        let x = if work.left > mon.left { work.left + m } else { work.right - w - m };
        let y = if work.top > mon.top { work.top + m } else { work.bottom - h - m };
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
        if (w, h) != self.size {
            self.size = (w, h);
            if let Some(rt) = &self.rt {
                unsafe {
                    let _ = rt.Resize(&D2D_SIZE_U { width: w as u32, height: h as u32 });
                }
            }
        }
    }

    fn show(&mut self) {
        self.app.refresh_activity();
        self.m.page = Page::Main;
        self.m.scroll = 0.0;
        self.m.hover = None;
        self.m.pressed = None;
        self.follow_theme();
        self.visible = true;
        self.refresh();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(self.hwnd));
            self.fg_at_show = GetForegroundWindow();
            SetTimer(Some(self.hwnd), T_ACTIVITY, 2000, None);
            SetTimer(Some(self.hwnd), T_FOCUS, 300, None);
        }
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.hidden_at = Some(Instant::now());
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = KillTimer(Some(self.hwnd), T_ACTIVITY);
            let _ = KillTimer(Some(self.hwnd), T_CARET);
            let _ = KillTimer(Some(self.hwnd), T_FOCUS);
        }
        // Let go of the GPU target and icons while hidden, and give the memory back.
        self.rt = None;
        self.icons.unbind();
        self.app.procs.clear();
        self.app.socks.clear();
        unsafe {
            let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
    }

    /// Working set and private bytes of this process, in MB.
    fn memory() -> (f64, f64) {
        use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
        let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        unsafe {
            let _ = K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb);
        }
        let mb = |b: usize| (b as f64 / 1048576.0 * 10.0).round() / 10.0;
        (mb(c.WorkingSetSize), mb(c.PrivateUsage))
    }

    /// The window as Windows composes it (PrintWindow with PW_RENDERFULLCONTENT), saved as PNG.
    fn capture(&self, name: &str) -> String {
        use windows::Win32::Graphics::Imaging::WICBitmapIgnoreAlpha;
        use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
        let Some(t) = &self.test else { return String::new() };
        let out = t.dir.join(format!("{name}.png"));
        unsafe {
            let mut r = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut r);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let hdc = GetDC(Some(self.hwnd));
            let mem = CreateCompatibleDC(Some(hdc));
            let bmp = CreateCompatibleBitmap(hdc, w, h);
            let old = SelectObject(mem, bmp.into());
            let printed = PrintWindow(self.hwnd, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
            SelectObject(mem, old);
            let saved = IconCache::wic().and_then(|wic| {
                let b = wic.CreateBitmapFromHBITMAP(bmp, HPALETTE::default(), WICBitmapIgnoreAlpha)?;
                super::shot::encode_png(&wic, &b, w as u32, h as u32, &out)
            });
            let _ = DeleteObject(bmp.into());
            let _ = DeleteDC(mem);
            ReleaseDC(Some(self.hwnd), hdc);
            format!("{name}: {w}x{h} printed={printed} saved={}", saved.is_ok())
        }
    }

    /// Posts a real click (move, down, up) at the centre of a hit area, as the mouse would.
    fn post_click(&self, h: Hit) -> bool {
        let Some((r, _)) = self.frame.hits.iter().find(|(_, x)| *x == h) else { return false };
        let s = self.scale();
        let (x, y) = (((r.x + r.w / 2.0) * s) as isize, ((r.y + r.h / 2.0) * s) as isize);
        let lp = LPARAM((y << 16) | (x & 0xffff));
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_MOUSEMOVE, WPARAM(0), lp);
            let _ = PostMessageW(Some(self.hwnd), WM_LBUTTONDOWN, WPARAM(1), lp);
            let _ = PostMessageW(Some(self.hwnd), WM_LBUTTONUP, WPARAM(0), lp);
        }
        true
    }

    fn ui_step(&mut self) -> Effect {
        let step = self.test.as_ref().map(|t| t.step).unwrap_or(99);
        let page = format!("{:?}", self.m.page);
        let rec: serde_json::Value = match step {
            0 => {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(self.hwnd, &mut r);
                }
                let (ws, private) = Flyout::memory();
                serde_json::json!({"step": "shown", "page": page, "visible": self.visible, "dpi": self.dpi,
                    "window": [r.left, r.top, r.right - r.left, r.bottom - r.top], "capture": self.capture("1-main"),
                    "apps": self.m.apps.len(), "status": self.m.status, "working_set_mb": ws, "private_mb": private})
            }
            1 => serde_json::json!({"step": "click Add an app", "posted": self.post_click(Hit::AddApp)}),
            2 => serde_json::json!({"step": "add page", "page": page, "picks": self.m.picks.len(), "capture": self.capture("2-add")}),
            3 => {
                for c in "brave".chars() {
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_CHAR, WPARAM(c as usize), LPARAM(0));
                    }
                }
                serde_json::json!({"step": "type brave"})
            }
            4 => serde_json::json!({"step": "search", "search": self.m.search,
                "picks": self.m.picks.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), "capture": self.capture("3-add-search")}),
            5 => {
                unsafe {
                    let _ = PostMessageW(Some(self.hwnd), WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
                    let _ = PostMessageW(Some(self.hwnd), WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
                }
                serde_json::json!({"step": "escape twice"})
            }
            6 => serde_json::json!({"step": "click settings", "page_before": page, "posted": self.post_click(Hit::Gear)}),
            7 => serde_json::json!({"step": "settings page", "page": page, "adapters": self.m.adapters.len(), "capture": self.capture("4-settings")}),
            8 => serde_json::json!({"step": "click back", "posted": self.post_click(Hit::Back)}),
            9 => {
                let (ws, private) = Flyout::memory();
                serde_json::json!({"step": "main again", "page": page, "capture": self.capture("5-main-again"),
                    "working_set_mb": ws, "private_mb": private})
            }
            10 => {
                self.modal = false;
                self.hide();
                serde_json::json!({"step": "hidden", "visible": self.visible})
            }
            _ => {
                let (ws, private) = Flyout::memory();
                let mut t = self.test.take().expect("uitest");
                t.log.push(serde_json::json!({"step": "after hide", "working_set_mb": ws, "private_mb": private}));
                let _ = std::fs::write(t.dir.join("report.json"), serde_json::to_string_pretty(&t.log).unwrap_or_default());
                return Effect::Quit;
            }
        };
        if let Some(t) = self.test.as_mut() {
            t.log.push(rec);
            t.step += 1;
        }
        Effect::None
    }

    /// Starts a background check; its result arrives as WM_UPDATE.
    fn check_updates(&self) {
        if !self.app.cfg.settings.check_updates || self.test.is_some() {
            return;
        }
        let hwnd = self.hwnd.0 as usize;
        crate::update::check_async(move || unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_UPDATE, WPARAM(0), LPARAM(0));
        });
    }

    fn toggle_from_tray(&mut self) {
        if self.visible {
            self.hide();
        } else if self.hidden_at.is_some_and(|t| t.elapsed() < Duration::from_millis(300)) {
            // This is the same click that took focus away from the flyout and hid it.
        } else {
            self.show();
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &mut ps);
            if self.visible {
                if self.rt.is_none() {
                    let props = D2D1_RENDER_TARGET_PROPERTIES {
                        // Software: a hardware target loads the GPU driver into the process (measured 2026-10-18:
                        // 62 MB private, kept after hiding). A 380-DIP flyout draws fine on the CPU.
                        r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                        dpiX: self.dpi as f32,
                        dpiY: self.dpi as f32,
                        ..Default::default()
                    };
                    let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                        hwnd: self.hwnd,
                        pixelSize: D2D_SIZE_U { width: self.size.0.max(1) as u32, height: self.size.1.max(1) as u32 },
                        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                    };
                    self.rt = self.factory.CreateHwndRenderTarget(&props, &hprops).ok();
                }
                if let Some(rt) = self.rt.clone() {
                    let b = base(&rt);
                    self.icons.bind(&b, self.dpi as f32);
                    if let Ok(p) = Painter::new(&b, &self.fonts, &self.factory) {
                        b.BeginDraw();
                        self.frame = render(Some(&p), &self.theme, &self.m, &self.fonts, &self.icons, self.max_height());
                        if b.EndDraw(None, None).is_err() {
                            // D2DERR_RECREATE_TARGET (device lost): make a new one next time.
                            self.rt = None;
                            self.icons.unbind();
                            self.invalidate();
                        }
                    }
                }
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let s = self.scale();
        (lo(lp.0) as f32 / s, hi(lp.0) as f32 / s)
    }

    fn screen_point(&self, lp: LPARAM) -> POINT {
        let mut p = POINT { x: lo(lp.0), y: hi(lp.0) };
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut p);
        }
        p
    }

    fn set_page(&mut self, p: Page) {
        self.m.page = p;
        self.m.scroll = 0.0;
        self.m.hover = None;
        self.m.search_focus = p == Page::Add;
        unsafe {
            if p == Page::Add {
                self.m.search.clear();
                self.m.caret = true;
                SetTimer(Some(self.hwnd), T_CARET, 530, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), T_CARET);
            }
        }
        self.refresh();
    }

    fn click(&mut self, h: Hit) -> Effect {
        let changes = !matches!(h, Hit::AddApp | Hit::Gear | Hit::Back | Hit::Search | Hit::BannerButton | Hit::UpdateDownload);
        if changes && self.app.read_only() {
            // The banner already says why (and offers "Restart as admin" when that's it).
            return Effect::None;
        }
        match h {
            Hit::Toggle(i) | Hit::AppRow(i) => {
                self.app.toggle(i);
                self.app.refresh_activity();
            }
            Hit::AddApp => self.set_page(Page::Add),
            Hit::Revert => self.app.revert_all(),
            Hit::Gear => self.set_page(Page::Settings),
            Hit::Back => self.set_page(Page::Main),
            Hit::Search => self.m.search_focus = true,
            Hit::Pick(i) => {
                if let Some(p) = self.m.picks.get(i).map(|p| p.path.clone()) {
                    self.app.add(&p);
                    self.set_page(Page::Main);
                }
            }
            Hit::PickExe => return Effect::PickExe,
            Hit::WhenDown(b) => self.app.set_block(b),
            Hit::AdapterAuto => self.app.set_adapter(None),
            Hit::LanKeep => {
                let on = !self.m.lan_keep;
                self.app.set_lan_keep(on);
            }
            Hit::Reconnect => {
                let on = !self.m.reconnect;
                self.app.set_reconnect(on);
            }
            Hit::Autostart => {
                let on = !self.m.autostart;
                self.app.set_autostart(on);
            }
            Hit::CheckUpdates => {
                let on = !self.m.check_updates;
                self.app.set_check_updates(on);
                if on {
                    self.check_updates();
                }
            }
            Hit::UpdateDownload => {
                if let Some(u) = &self.app.update {
                    return Effect::OpenUrl(u.url.clone());
                }
            }
            Hit::Adapter(i) => {
                if let Some((guid, _)) = self.app.adapter_choices().get(i).cloned() {
                    self.app.set_adapter(Some(guid));
                }
            }
            Hit::BannerButton => {
                match self.m.banner.as_ref().and_then(|b| b.button.as_deref()) {
                    Some("Restart as admin") => return Effect::RestartAdmin,
                    Some("Connect Wi-Fi") => self.app.connect_wifi(false),
                    Some("Open Windows Update") => return Effect::OpenUrl(crate::update::WINDOWS_UPDATE.into()),
                    _ => self.app.retry(),
                }
            }
        }
        self.refresh();
        self.tray(false);
        Effect::None
    }

    fn key(&mut self, vk: u16) -> bool {
        match VIRTUAL_KEY(vk) {
            VK_ESCAPE => {
                if self.m.page == Page::Add && !self.m.search.is_empty() {
                    self.m.search.clear();
                    self.refresh();
                } else if self.m.page != Page::Main {
                    self.set_page(Page::Main);
                } else {
                    self.hide();
                }
            }
            VK_BACK if self.m.page == Page::Add => {
                self.m.search.pop();
                self.m.scroll = 0.0;
                self.refresh();
            }
            VK_RETURN if self.m.page == Page::Add => {
                if let Some(i) = self.m.picks.iter().position(|p| !p.added) {
                    let _ = self.click(Hit::Pick(i));
                }
            }
            _ => return false,
        }
        true
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> (Option<LRESULT>, Effect) {
        let done = (Some(LRESULT(0)), Effect::None);
        match msg {
            WM_TRAY => {
                let event = (lp.0 & 0xffff) as u32;
                match event {
                    NIN_SELECT | NIN_KEYSELECT => self.toggle_from_tray(),
                    WM_CONTEXTMENU => {
                        let pt = POINT { x: lo(wp.0 as isize), y: hi(wp.0 as isize) };
                        return (Some(LRESULT(0)), Effect::TrayMenu(pt));
                    }
                    _ => {}
                }
                done
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xffff) as u32 == WA_INACTIVE && !self.modal {
                    self.hide();
                }
                done
            }
            WM_PAINT => {
                self.paint();
                done
            }
            WM_ERASEBKGND => (Some(LRESULT(1)), Effect::None),
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
                let (x, y) = self.dip(lp);
                let h = self.frame.hit(x, y);
                if h != self.m.hover {
                    self.m.hover = h;
                    self.invalidate();
                }
                done
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.m.hover.take().is_some() {
                    self.invalidate();
                }
                done
            }
            WM_LBUTTONDOWN => {
                let (x, y) = self.dip(lp);
                self.m.pressed = self.frame.hit(x, y);
                unsafe { SetCapture(self.hwnd) };
                self.invalidate();
                done
            }
            WM_LBUTTONUP => {
                unsafe {
                    let _ = ReleaseCapture();
                }
                let (x, y) = self.dip(lp);
                let h = self.frame.hit(x, y);
                let pressed = self.m.pressed.take();
                self.invalidate();
                if let Some(h) = h.filter(|h| Some(*h) == pressed) {
                    let e = self.click(h);
                    return (Some(LRESULT(0)), e);
                }
                done
            }
            WM_RBUTTONUP => {
                let (x, y) = self.dip(lp);
                match self.frame.hit(x, y) {
                    Some(Hit::AppRow(i)) | Some(Hit::Toggle(i)) => (Some(LRESULT(0)), Effect::RowMenu(i, self.screen_point(lp))),
                    _ => done,
                }
            }
            WM_MOUSEWHEEL => {
                let delta = hi(wp.0 as isize) as f32;
                let next = (self.m.scroll - delta / 120.0 * 52.0).clamp(0.0, self.frame.max_scroll());
                if next != self.m.scroll {
                    self.m.scroll = next;
                    self.m.hover = None;
                    self.layout();
                }
                done
            }
            WM_KEYDOWN => {
                if self.key(wp.0 as u16) { done } else { (None, Effect::None) }
            }
            WM_CHAR => {
                let c = char::from_u32(wp.0 as u32).unwrap_or('\0');
                if self.m.page == Page::Add && !c.is_control() {
                    self.m.search.push(c);
                    self.m.scroll = 0.0;
                    self.m.search_focus = true;
                    self.app.fill_picks(&mut self.m);
                    self.layout();
                }
                done
            }
            WM_TIMER => {
                match wp.0 {
                    T_NET => {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), T_NET);
                        }
                        self.app.refresh_net();
                        if self.visible {
                            self.refresh();
                        }
                        self.tray(false);
                    }
                    // Not on the Add page: re-sorting that list under the mouse could pin the neighbouring app.
                    T_ACTIVITY if self.visible && self.m.page != Page::Add => {
                        self.app.refresh_activity();
                        self.refresh();
                    }
                    T_UPDATE => {
                        unsafe { SetTimer(Some(self.hwnd), T_UPDATE, UPDATE_EVERY_MS, None) };
                        self.check_updates();
                    }
                    T_TRAY => {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), T_TRAY);
                        }
                        self.tray(true);
                    }
                    T_FOCUS if self.visible && !self.modal => {
                        let fg = unsafe { GetForegroundWindow() };
                        if fg != self.hwnd && fg != self.fg_at_show && !fg.is_invalid() {
                            self.hide();
                        }
                    }
                    T_CARET => {
                        self.m.caret = !self.m.caret;
                        self.invalidate();
                    }
                    T_QUIT => return (Some(LRESULT(0)), Effect::Quit),
                    T_UITEST => return (Some(LRESULT(0)), self.ui_step()),
                    T_RESYNC => {
                        // A missed adapter change, or a new version folder: read everything again now and then.
                        self.app.refresh_net();
                        // Windows drops an idle Wi-Fi while Ethernet is plugged in; pinned apps need it.
                        self.app.connect_wifi(true);
                        self.follow_theme();
                    }
                    T_VERSIONS => {
                        // An app in a version folder (Discord) may update and restart from a new folder any time.
                        if self.app.has_versioned_rules() {
                            self.app.sync(false);
                        }
                    }
                    _ => {}
                }
                done
            }
            WM_UPDATE => {
                if let Some(r) = crate::update::take_result() {
                    self.app.update_result(r);
                    self.refresh();
                    self.tray(false);
                }
                done
            }
            WM_NETCHANGE => {
                // Windows sends bursts of changes; wait until it settles.
                unsafe { SetTimer(Some(self.hwnd), T_NET, 400, None) };
                done
            }
            // Light/dark mode, and a contrast theme switched on or off.
            WM_SETTINGCHANGE | WM_SYSCOLORCHANGE | WM_THEMECHANGED => {
                self.follow_theme();
                self.invalidate();
                (None, Effect::None)
            }
            WM_DPICHANGED => {
                self.dpi = hi(wp.0 as isize) as u32;
                if let Some(rt) = &self.rt {
                    unsafe { rt.SetDpi(self.dpi as f32, self.dpi as f32) };
                }
                if self.visible {
                    self.place();
                }
                done
            }
            WM_QUERYENDSESSION => (Some(LRESULT(1)), Effect::None),
            WM_SYSCOMMAND if (wp.0 & 0xFFF0) as u32 == SC_CLOSE => {
                // Alt+F4 closes the flyout, not NF Switcher (that would drop every pin). Quit is in the tray menu.
                self.hide();
                done
            }
            WM_ENDSESSION => {
                if wp.0 != 0 {
                    // Log-off or shutdown: take everything out of Windows now, there's no later.
                    self.app.shutdown();
                    self.tray_remove();
                    for t in [T_NET, T_ACTIVITY, T_RESYNC, T_VERSIONS, T_FOCUS, T_CARET, T_TRAY, T_UPDATE] {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), t);
                        }
                    }
                    // Restart Manager (an installer) closing just this app: actually go.
                    if lp.0 as u32 & ENDSESSION_CLOSEAPP != 0 {
                        return (Some(LRESULT(0)), Effect::Quit);
                    }
                }
                done
            }
            WM_DESTROY => {
                self.app.shutdown();
                self.tray_remove();
                unsafe { PostQuitMessage(0) };
                done
            }
            m if m == self.show_msg => {
                if !self.visible {
                    self.show();
                }
                done
            }
            m if m == self.taskbar_created && m != 0 => {
                // Explorer restarted: the tray icon is gone with it.
                self.tray(true);
                done
            }
            _ => (None, Effect::None),
        }
    }
}

fn menu(owner: HWND, pt: POINT, items: &[(usize, &str)]) -> usize {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return 0 };
        for (id, text) in items {
            if *id == 0 {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
            } else {
                let _ = AppendMenuW(m, MF_STRING, *id, &HSTRING::from(*text));
            }
        }
        // Needed so the menu closes when clicking elsewhere (documented TrackPopupMenu quirk).
        let _ = SetForegroundWindow(owner);
        let r = TrackPopupMenuEx(m, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0, pt.x, pt.y, owner, None);
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(m);
        r.0 as usize
    }
}

fn pick_exe(owner: HWND) -> Option<String> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let spec = [COMDLG_FILTERSPEC { pszName: w!("Programs"), pszSpec: w!("*.exe") }];
        let _ = dlg.SetFileTypes(&spec);
        let _ = dlg.SetTitle(w!("Pick a program to send over Wi-Fi"));
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn run_effect(hwnd: HWND, e: Effect) {
    match e {
        Effect::None => {}
        Effect::TrayMenu(pt) => {
            with(|f| f.modal = true);
            let cmd = menu(hwnd, pt, &[(1, "Open NF Switcher"), (2, "Revert all"), (0, ""), (3, "Quit (puts every app back)")]);
            with(|f| {
                f.modal = false;
                match cmd {
                    1 => f.show(),
                    // Without admin nothing was applied, and nothing is changed (as in the flyout).
                    2 if !f.app.read_only() => {
                        f.app.revert_all();
                        f.refresh();
                        f.tray(false);
                    }
                    _ => {}
                }
            });
            if cmd == 3 {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
        Effect::RowMenu(i, pt) => {
            if with(|f| f.app.read_only()).unwrap_or(true) {
                return;
            }
            let can_move = with(|f| {
                f.modal = true;
                f.app.can_move(i)
            })
            .unwrap_or(false);
            let mut items = Vec::new();
            if can_move {
                items.push((2, "Move its open connections now (they reconnect)"));
            }
            items.push((1, "Remove from the list"));
            let cmd = menu(hwnd, pt, &items);
            with(|f| {
                f.modal = false;
                match cmd {
                    1 => f.app.remove(i),
                    2 => {
                        f.app.move_connections(i);
                    }
                    _ => return,
                }
                f.refresh();
                f.tray(false);
            });
        }
        Effect::PickExe => {
            with(|f| f.modal = true);
            let path = pick_exe(hwnd);
            with(|f| {
                f.modal = false;
                if let Some(p) = path {
                    f.app.add(&p);
                    f.set_page(Page::Main);
                    f.tray(false);
                }
                unsafe {
                    let _ = SetForegroundWindow(f.hwnd);
                }
            });
        }
        Effect::OpenUrl(url) => {
            if let Err(e) = crate::update::open_unelevated(&url) {
                crate::app::log(&format!("opening {url}: {e}"));
            }
        }
        Effect::Quit => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        Effect::RestartAdmin => {
            with(|f| f.modal = true);
            let r = elevate::relaunch_as_admin(&["--elevated".into(), "--show".into()]);
            let started = r == elevate::Relaunch::Started;
            with(|f| f.modal = false);
            if started {
                // The admin copy takes over: leave now so it gets the single-instance lock.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match with(|f| f.handle(msg, wp, lp)) {
        Some((r, effect)) => {
            run_effect(hwnd, effect);
            r.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
        }
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Creates the (hidden) flyout and the tray icon, then runs the message loop until Quit.
pub fn run(app: App, opts: Options) -> windows::core::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: opts.class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            opts.class,
            w!("NF Switcher"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let show_msg = elevate::show_message();
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        // Elevated: let Explorer's tray callbacks, its restart broadcast and a second start's "show" through UIPI.
        for m in [WM_TRAY, show_msg, taskbar_created] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }
        let hwnd_val = hwnd.0 as usize;
        let watcher = crate::net::Watcher::start(move || {
            let _ = PostMessageW(Some(HWND(hwnd_val as *mut _)), WM_NETCHANGE, WPARAM(0), LPARAM(0));
        });
        // Anything that changed between the first read of the adapters and the watcher starting.
        let _ = PostMessageW(Some(hwnd), WM_NETCHANGE, WPARAM(0), LPARAM(0));
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let theme = Theme::current();
        let icon_dark = taskbar_dark(&theme);
        let (icon, icon_owned) = tray_icon(icon_dark);
        let fly = Flyout {
            hwnd,
            app,
            m: Model::default(),
            frame: Frame::default(),
            theme,
            factory,
            fonts: Fonts::new()?,
            icons: IconCache::new(IconCache::wic()?),
            rt: None,
            dpi: 96,
            size: (0, 0),
            visible: false,
            hidden_at: None,
            tracking: false,
            modal: false,
            show_msg,
            taskbar_created,
            _watcher: Some(watcher),
            test: opts.uitest.clone().map(|dir| UiTest { dir, step: 0, log: Vec::new() }),
            fg_at_show: HWND::default(),
            icon,
            icon_owned,
            icon_dark,
            tray_missing: 0,
        };
        FLY.with(|c| *c.borrow_mut() = Some(fly));
        with(|f| {
            f.refresh();
            if opts.tray {
                f.tray(true);
            }
            SetTimer(Some(f.hwnd), T_RESYNC, 60_000, None);
            SetTimer(Some(f.hwnd), T_VERSIONS, 5_000, None);
            SetTimer(Some(f.hwnd), T_UPDATE, UPDATE_FIRST_MS, None);
            if let Some(d) = opts.quit_after {
                SetTimer(Some(f.hwnd), T_QUIT, d.as_millis() as u32, None);
            }
            if f.test.is_some() {
                // Clicks are posted, so focus doesn't matter; don't let a lost focus hide the flyout mid-test.
                f.modal = true;
                SetTimer(Some(f.hwnd), T_UITEST, 900, None);
            }
            if opts.show {
                f.show();
            }
        });
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        FLY.with(|c| c.borrow_mut().take());
    }
    Ok(())
}
