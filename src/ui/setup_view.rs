// The installer's window (concept B, picked 2026-10-18): one small card in the flyout's style. Pages: the options,
// the work in progress, done, failed, and the same for uninstalling. Laid out and drawn like the flyout (view.rs).
use super::paint::{Align, Font, Fonts, Painter, Rect};
use super::theme::{hex, Theme};
use super::view::paragraph;

pub const WIDTH: f32 = 380.0;
const PAD: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Options,
    Working,
    Done,
    Failed,
    Uninstall,
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Change,
    Autostart,
    Desktop,
    Install,
    Open,
    Close,
    Uninstall,
    RemoveSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub page: Page,
    pub version: String,
    /// The installed version, when this is an update.
    pub installed: Option<String>,
    pub dir: String,
    pub autostart: bool,
    pub desktop: bool,
    pub remove_settings: bool,
    pub admin: bool,
    pub uninstalling: bool,
    pub steps: Vec<String>,
    pub step: usize,
    pub error: String,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            page: Page::Options,
            version: env!("CARGO_PKG_VERSION").into(),
            installed: None,
            dir: r"C:\Program Files\NF Switcher".into(),
            autostart: true,
            desktop: false,
            remove_settings: false,
            admin: false,
            uninstalling: false,
            steps: Vec::new(),
            step: 0,
            error: String::new(),
            hover: None,
            pressed: None,
        }
    }
}

pub struct Frame {
    pub height: f32,
    pub hits: Vec<(Rect, Hit)>,
}

impl Frame {
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }
}

struct Ctx<'a> {
    p: Option<&'a Painter<'a>>,
    th: Theme,
    m: &'a Model,
    hits: Vec<(Rect, Hit)>,
}

impl Ctx<'_> {
    fn hot(&self, h: Hit) -> bool {
        self.m.hover == Some(h)
    }

    fn hero(&self, glyph: u16, tile: super::theme::Rgba, title: &str, sub: &str) {
        let Some(p) = self.p else { return };
        let t = Rect::new(PAD, 22.0, 48.0, 48.0);
        p.fill(t, 11.0, tile);
        p.glyph(glyph, t, 24.0, hex(0xffffff));
        p.text(title, Rect::new(82.0, 22.0, WIDTH - 82.0 - 52.0, 28.0), Font::Heading, self.th.text, Align::Left);
        p.text(sub, Rect::new(82.0, 48.0, WIDTH - 82.0 - PAD, 20.0), Font::Caption, self.th.text3, Align::Left);
    }

    fn close_x(&mut self) {
        let r = Rect::new(WIDTH - 12.0 - 32.0, 10.0, 32.0, 32.0);
        if let Some(p) = self.p {
            if self.hot(Hit::Close) {
                p.fill(r, 5.0, self.th.hover);
            }
            p.glyph(0xE8BB, r, 10.0, self.th.text2);
        }
        self.hits.push((r, Hit::Close));
    }

    fn button(&mut self, r: Rect, label: &str, h: Hit, accent: bool) {
        let th = self.th;
        if let Some(p) = self.p {
            let bg = match (accent, self.hot(h)) {
                (true, false) => th.accent,
                (true, true) => th.accent_hover,
                (false, false) => th.button,
                (false, true) => th.button_hover,
            };
            p.fill(r, 5.0, bg);
            let font = if accent { Font::BodyStrong } else { Font::Body };
            p.text(label, r, font, if accent { th.on_accent } else { th.text }, Align::Center);
        }
        self.hits.push((r, h));
    }

    fn toggle_row(&mut self, y: f32, title: &str, sub: &str, on: bool, h: Hit) -> f32 {
        let th = self.th;
        let row = Rect::new(8.0, y, WIDTH - 16.0, 48.0);
        if let Some(p) = self.p {
            if self.hot(h) {
                p.fill(row, 5.0, th.hover);
            }
            p.text(title, Rect::new(PAD, y + 5.0, WIDTH - 2.0 * PAD - 56.0, 20.0), Font::Body, th.text, Align::Left);
            p.text(sub, Rect::new(PAD, y + 24.0, WIDTH - 2.0 * PAD - 56.0, 18.0), Font::Caption, th.text3, Align::Left);
            let t = Rect::new(WIDTH - PAD - 40.0, y + 14.0, 40.0, 20.0);
            if on {
                p.fill(t, 10.0, th.accent);
                p.circle(t.right() - 10.0, t.y + 10.0, 6.0, th.on_accent);
            } else {
                p.stroke(t, 10.0, th.text2, 1.0);
                p.circle(t.x + 10.0, t.y + 10.0, 5.0, th.text2);
            }
        }
        self.hits.push((row, h));
        y + 48.0
    }

    fn steps(&self, y: f32, done_all: bool) -> f32 {
        let th = self.th;
        let mut y = y;
        for (i, s) in self.m.steps.iter().enumerate() {
            if let Some(p) = self.p {
                let (g, c, tc) = if done_all || i < self.m.step {
                    (0xE73E, th.ok, th.text2)
                } else if i == self.m.step {
                    (0xE76C, th.accent, th.text)
                } else {
                    (0xE73E, super::theme::Rgba(0.0, 0.0, 0.0, 0.0), th.text3)
                };
                p.glyph(g, Rect::new(PAD, y, 16.0, 24.0), 11.0, c);
                p.text(s, Rect::new(PAD + 22.0, y, WIDTH - 2.0 * PAD - 22.0, 24.0), Font::Body, tc, Align::Left);
            }
            y += 24.0;
        }
        y
    }

    fn progress(&self, y: f32, frac: f32) {
        let Some(p) = self.p else { return };
        let r = Rect::new(PAD, y, WIDTH - 2.0 * PAD, 4.0);
        p.fill(r, 2.0, if self.th.dark { hex(0x444444) } else { hex(0xdddddd) });
        p.fill(Rect::new(r.x, r.y, r.w * frac.clamp(0.0, 1.0), r.h), 2.0, self.th.accent);
    }
}

pub fn render(p: Option<&Painter>, th: &Theme, m: &Model, fonts: &Fonts) -> Frame {
    let mut c = Ctx { p, th: *th, m, hits: Vec::new() };
    if let Some(p) = c.p {
        p.clear(th.surface);
    }
    let brand = hex(0x0067c0);
    let version = format!("NF Switcher {}", m.version);
    let height = match m.page {
        Page::Options => {
            let (title, sub) = match &m.installed {
                Some(v) => ("Update NF Switcher".to_string(), format!("{v} → {} · your settings stay", m.version)),
                None => ("NF Switcher".to_string(), format!("Version {} · 0.8 MB · needs Windows 11 24H2", m.version)),
            };
            c.hero(0xE8AB, brand, &title, &sub);
            c.close_x();
            let path = Rect::new(PAD, 90.0, WIDTH - 2.0 * PAD, 38.0);
            if let Some(p) = c.p {
                p.fill(path, 6.0, th.tile);
                p.stroke(path, 6.0, th.tile_line, 1.0);
                p.text(&m.dir, Rect::new(path.x + 12.0, path.y, path.w - 90.0, path.h), Font::Mono, th.text2, Align::Left);
                let col = if c.hot(Hit::Change) { th.accent_hover } else { th.accent };
                p.text("Change", Rect::new(path.right() - 76.0, path.y, 64.0, path.h), Font::Body, col, Align::Right);
            }
            if m.installed.is_none() {
                c.hits.push((Rect::new(path.right() - 84.0, path.y, 84.0, path.h), Hit::Change));
            }
            let mut y = 138.0;
            y = c.toggle_row(y, "Start with Windows", "In the tray at sign-in, without a UAC prompt", m.autostart, Hit::Autostart);
            y = c.toggle_row(y, "Desktop shortcut", "The Start menu entry is always added", m.desktop, Hit::Desktop);
            y += 12.0;
            let label = if m.installed.is_some() { "Update" } else { "Install" };
            c.button(Rect::new(PAD, y, WIDTH - 2.0 * PAD, 38.0), label, Hit::Install, true);
            y += 46.0;
            if let Some(p) = c.p {
                let note = if m.admin { "Installs for everyone on this PC" } else { "Windows will ask for admin (UAC)" };
                p.text(note, Rect::new(PAD, y, WIDTH - 2.0 * PAD, 18.0), Font::Caption, th.text3, Align::Center);
            }
            y + 30.0
        }
        Page::Working => {
            let title = if m.uninstalling { "Removing…" } else { "Installing…" };
            c.hero(0xE8AB, brand, title, &version);
            let frac = (m.step as f32 + 0.5) / m.steps.len().max(1) as f32;
            c.progress(90.0, frac);
            c.steps(106.0, false) + 22.0
        }
        Page::Done => {
            c.hero(0xE73E, hex(0x0f7b0f), "NF Switcher is installed", "Find it in the tray next to the clock");
            c.close_x();
            c.progress(90.0, 1.0);
            let mut y = c.steps(106.0, true) + 14.0;
            c.button(Rect::new(PAD, y, WIDTH - 2.0 * PAD, 38.0), "Open NF Switcher", Hit::Open, true);
            y += 46.0;
            c.button(Rect::new(PAD, y, WIDTH - 2.0 * PAD, 32.0), "Close", Hit::Close, false);
            y + 52.0
        }
        Page::Failed => {
            let title = if m.uninstalling { "Couldn't remove it" } else { "Couldn't install" };
            c.hero(0xE7BA, hex(0xc42b1c), title, &version);
            c.close_x();
            let w = WIDTH - 2.0 * PAD;
            let h = super::view::paragraph_height(fonts, &m.error, Font::Caption, w);
            if let Some(p) = c.p {
                paragraph(p, &m.error, Rect::new(PAD, 92.0, w, h), Font::Caption, th.text2);
            }
            let y = 92.0 + h + 16.0;
            c.button(Rect::new(PAD, y, w, 34.0), "Close", Hit::Close, false);
            y + 54.0
        }
        Page::Uninstall => {
            c.hero(0xE8AB, brand, "Remove NF Switcher?", &version);
            c.close_x();
            let w = WIDTH - 2.0 * PAD;
            let text = "Every app goes back to Windows' default route, and \"Start with Windows\" is removed.";
            let h = super::view::paragraph_height(fonts, text, Font::Caption, w);
            if let Some(p) = c.p {
                paragraph(p, text, Rect::new(PAD, 90.0, w, h), Font::Caption, th.text2);
            }
            let mut y = 90.0 + h + 8.0;
            y = c.toggle_row(y, "Also remove its settings", "Your list of apps in ProgramData", m.remove_settings, Hit::RemoveSettings);
            y += 12.0;
            c.button(Rect::new(PAD, y, w, 38.0), "Uninstall", Hit::Uninstall, true);
            y += 46.0;
            if let Some(p) = c.p {
                let note = if m.admin { "Removes it for everyone on this PC" } else { "Windows will ask for admin (UAC)" };
                p.text(note, Rect::new(PAD, y, w, 18.0), Font::Caption, th.text3, Align::Center);
            }
            y + 30.0
        }
        Page::Removed => {
            c.hero(0xE73E, hex(0x0f7b0f), "NF Switcher is removed", "Every app uses Windows' default route again");
            c.close_x();
            c.progress(90.0, 1.0);
            let y = c.steps(106.0, true) + 14.0;
            c.button(Rect::new(PAD, y, WIDTH - 2.0 * PAD, 34.0), "Close", Hit::Close, false);
            y + 54.0
        }
    };
    Frame { height, hits: c.hits }
}

/// Sample states for --shots.
pub fn samples() -> Vec<(&'static str, Model)> {
    let steps: Vec<String> = crate::installer::INSTALL_STEPS.iter().map(|s| s.to_string()).collect();
    let un: Vec<String> = crate::installer::UNINSTALL_STEPS.iter().map(|s| s.to_string()).collect();
    vec![
        ("setup-options", Model { hover: Some(Hit::Install), ..Default::default() }),
        ("setup-update", Model { installed: Some("0.1.0".into()), version: "0.2.0".into(), admin: true, ..Default::default() }),
        ("setup-working", Model { page: Page::Working, steps: steps.clone(), step: 2, ..Default::default() }),
        ("setup-done", Model { page: Page::Done, steps: steps[..3].to_vec(), step: 3, hover: Some(Hit::Open), ..Default::default() }),
        (
            "setup-failed",
            Model {
                page: Page::Failed,
                error: "Replacing NF Switcher failed (is it still running?): Access is denied. (os error 5)".into(),
                ..Default::default()
            },
        ),
        ("setup-uninstall", Model { page: Page::Uninstall, ..Default::default() }),
        ("setup-removed", Model { page: Page::Removed, steps: un, step: 4, ..Default::default() }),
    ]
}
