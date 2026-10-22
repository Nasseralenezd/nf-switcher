// The flyout (concept C, picked 2026-10-18): main page (connection tiles, the apps with a Wi-Fi switch each,
// Add an app, Revert all, Settings), the "Add an app" page and the Settings page. One function lays out and, when
// given a painter, draws; the window lays out first to size itself.
use super::paint::{Align, Font, Fonts, Painter, Rect};
use super::theme::Theme;
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;

pub const WIDTH: f32 = 380.0;
const PAD: f32 = 16.0;
const HEADER: f32 = 48.0;
const TILES: f32 = 92.0;
const SECTION: f32 = 28.0;
const ROW: f32 = 52.0;
const ADD: f32 = 48.0;
const FOOTER: f32 = 52.0;
const SEARCH: f32 = 46.0;
const RADIO: f32 = 44.0;
/// The Add page's "Select all" row and its "Add N apps to Wi-Fi" bar.
const SELECT_ALL: f32 = 36.0;
const ADD_BAR: f32 = 50.0;

/// A check box: ticked, partly (some of a list), or not.
#[derive(Clone, Copy, PartialEq)]
enum Check {
    Off,
    Part,
    On,
}

pub mod glyph {
    pub const APP: u16 = 0xE8AB;
    pub const ETHERNET: u16 = 0xE839;
    pub const WIFI: u16 = 0xE701;
    pub const ADD: u16 = 0xE710;
    pub const BACK: u16 = 0xE72B;
    pub const SETTINGS: u16 = 0xE713;
    pub const UNDO: u16 = 0xE7A7;
    pub const SEARCH: u16 = 0xE721;
    pub const WARNING: u16 = 0xE7BA;
    pub const INFO: u16 = 0xE946;
    pub const FOLDER: u16 = 0xE8B7;
    pub const CHECK: u16 = 0xE73E;
    pub const DASH: u16 = 0xE738;
    pub const CLOSE: u16 = 0xE711;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Main,
    Add,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hit {
    Toggle(usize),
    AppRow(usize),
    /// The × at the start of an app's row: takes it off the list.
    Remove(usize),
    AddApp,
    Revert,
    Gear,
    Back,
    Search,
    Pick(usize),
    PickExe,
    /// "Select all" on the Add page, and its "Add N apps to Wi-Fi" button.
    SelectAll,
    AddSelected,
    WhenDown(bool),
    AdapterAuto,
    Adapter(usize),
    LanKeep,
    Reconnect,
    CheckUpdates,
    UpdateDownload,
    Autostart,
    BannerButton,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Banner {
    pub tone: Tone,
    pub text: String,
    pub button: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Tile {
    pub title: String,
    pub sub: String,
    /// Accent fill: this connection carries the pinned apps right now.
    pub on: bool,
    /// Grey glyph and text: not connected.
    pub off: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct AppItem {
    pub name: String,
    pub path: String,
    pub wifi: bool,
    pub status: String,
    pub warn: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PickItem {
    pub name: String,
    pub path: String,
    pub added: bool,
    /// Ticked for "Add N apps to Wi-Fi".
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct AdapterItem {
    pub name: String,
    pub sub: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub page: Page,
    pub status: String,
    pub eth: Tile,
    pub wifi: Tile,
    pub banner: Option<Banner>,
    pub apps: Vec<AppItem>,
    pub picks: Vec<PickItem>,
    /// The Add page's ticked programs (paths); kept while the search changes, cleared when the page opens.
    pub selected: std::collections::BTreeSet<String>,
    pub running: usize,
    pub search: String,
    pub search_focus: bool,
    pub caret: bool,
    pub footer: String,
    pub block_when_down: bool,
    pub adapter_auto: bool,
    pub adapters: Vec<AdapterItem>,
    pub lan_keep: bool,
    pub reconnect: bool,
    pub check_updates: bool,
    pub autostart: bool,
    /// A newer version on GitHub ("0.2.0"): the gear gets a warning dot, Settings a "New update" card.
    pub update: Option<String>,
    pub update_note: String,
    /// The card's second line, its button ("Update now", "Download", none while it runs) and progress bar.
    pub update_sub: String,
    pub update_button: Option<String>,
    pub update_progress: Option<f32>,
    pub about: Vec<String>,
    pub hover: Option<Hit>,
    pub pressed: Option<Hit>,
    pub scroll: f32,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            page: Page::Main,
            status: String::new(),
            eth: Tile::default(),
            wifi: Tile::default(),
            banner: None,
            apps: Vec::new(),
            picks: Vec::new(),
            selected: Default::default(),
            running: 0,
            search: String::new(),
            search_focus: false,
            caret: false,
            footer: String::new(),
            block_when_down: false,
            adapter_auto: true,
            adapters: Vec::new(),
            lan_keep: true,
            reconnect: true,
            check_updates: true,
            autostart: true,
            update: None,
            update_note: String::new(),
            update_sub: String::new(),
            update_button: None,
            update_progress: None,
            about: Vec::new(),
            hover: None,
            pressed: None,
            scroll: 0.0,
        }
    }
}

/// What a layout pass found: the flyout's height, the clickable areas and the scrollable list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub height: f32,
    pub hits: Vec<(Rect, Hit)>,
    /// The scrolling area and how tall its content is.
    pub list: Option<(Rect, f32)>,
}

impl Frame {
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }
    pub fn max_scroll(&self) -> f32 {
        self.list.map(|(r, content)| (content - r.h).max(0.0)).unwrap_or(0.0)
    }
}

/// App icons, by path (None draws the first letter on a tile).
pub trait Icons {
    fn icon(&self, path: &str) -> Option<ID2D1Bitmap>;
}

pub struct NoIcons;
impl Icons for NoIcons {
    fn icon(&self, _: &str) -> Option<ID2D1Bitmap> {
        None
    }
}

struct Ctx<'a> {
    p: Option<&'a Painter<'a>>,
    th: &'a Theme,
    m: &'a Model,
    fonts: &'a Fonts,
    icons: &'a dyn Icons,
    hits: Vec<(Rect, Hit)>,
}

impl Ctx<'_> {
    fn hit(&mut self, r: Rect, h: Hit) {
        self.hits.push((r, h));
    }
    fn hovered(&self, h: Hit) -> bool {
        self.m.hover == Some(h)
    }
    fn pressed(&self, h: Hit) -> bool {
        self.m.pressed == Some(h) && self.m.hover == Some(h)
    }

    /// A subtle hover/press background, the way Windows 11 list items do it.
    fn hover_bg(&self, r: Rect, h: Hit) {
        if let Some(p) = self.p {
            if self.pressed(h) {
                p.fill(r, 5.0, self.th.press);
            } else if self.hovered(h) {
                p.fill(r, 5.0, self.th.hover);
            }
        }
    }

    fn header(&mut self, y: f32, title: &str, right: &str, back: bool) {
        let th = *self.th;
        if back {
            let b = Rect::new(8.0, y + 8.0, 32.0, 32.0);
            self.hover_bg(b, Hit::Back);
            if let Some(p) = self.p {
                p.glyph(glyph::BACK, b, 14.0, th.text);
            }
            self.hit(b, Hit::Back);
        }
        if let Some(p) = self.p {
            let tx = if back {
                44.0
            } else {
                p.glyph(glyph::APP, Rect::new(PAD, y + 14.0, 20.0, 20.0), 16.0, th.accent);
                46.0
            };
            p.text(title, Rect::new(tx, y + 12.0, 220.0, 24.0), Font::Title, th.text, Align::Left);
            p.text(right, Rect::new(WIDTH - PAD - 150.0, y + 12.0, 150.0, 24.0), Font::Caption, th.text3, Align::Right);
        }
    }

    fn tile(&self, r: Rect, g: u16, t: &Tile) {
        let Some(p) = self.p else { return };
        let th = self.th;
        let (fg, sub) = if t.on {
            p.fill(r, 6.0, th.accent);
            (th.on_accent, th.on_accent)
        } else {
            p.fill(r, 6.0, th.tile);
            p.stroke(r, 6.0, th.tile_line, 1.0);
            if t.off { (th.text3, th.text3) } else { (th.text, th.text2) }
        };
        p.glyph(g, Rect::new(r.x + 12.0, r.y + 10.0, 20.0, 20.0), 17.0, fg);
        p.text(&t.title, Rect::new(r.x + 12.0, r.y + 36.0, r.w - 24.0, 20.0), Font::BodyStrong, fg, Align::Left);
        p.text(&t.sub, Rect::new(r.x + 12.0, r.y + 55.0, r.w - 24.0, 18.0), Font::Mono, sub, Align::Left);
    }

    fn banner(&mut self, y: f32, b: &Banner) -> f32 {
        let th = *self.th;
        let w = WIDTH - 2.0 * PAD;
        let text_w = w - 44.0;
        let text_h = paragraph_height(self.fonts, &b.text, Font::Caption, text_w);
        let btn_h = if b.button.is_some() { 36.0 } else { 0.0 };
        let h = (text_h + 20.0).max(40.0) + btn_h;
        let r = Rect::new(PAD, y, w, h);
        if let Some(p) = self.p {
            let (bg, fg, g) = match b.tone {
                Tone::Info => (th.tile, th.accent, glyph::INFO),
                Tone::Warn => (th.warn_bg, th.warn, glyph::WARNING),
                Tone::Error => (th.warn_bg, th.err, glyph::WARNING),
            };
            p.fill(r, 6.0, bg);
            p.stroke(r, 6.0, th.tile_line, 1.0);
            p.glyph(g, Rect::new(r.x + 10.0, r.y + 10.0, 20.0, 20.0), 15.0, fg);
            paragraph(p, &b.text, Rect::new(r.x + 36.0, r.y + 10.0, text_w, text_h), Font::Caption, th.text);
        }
        if let Some(label) = &b.button {
            let bw = self.fonts.measure(label, Font::Body) + 28.0;
            let br = Rect::new(r.x + 36.0, r.bottom() - 38.0, bw, 30.0);
            self.button(br, label, None, Hit::BannerButton, true);
        }
        h + 10.0
    }

    fn button(&mut self, r: Rect, label: &str, g: Option<u16>, h: Hit, accent: bool) {
        let th = *self.th;
        if let Some(p) = self.p {
            let bg = match (accent, self.hovered(h)) {
                (true, false) => th.accent,
                (true, true) => th.accent_hover,
                (false, false) => th.button,
                (false, true) => th.button_hover,
            };
            p.fill(r, 5.0, bg);
            let fg = if accent { th.on_accent } else { th.text };
            match g {
                Some(g) => {
                    let tw = p.measure(label, Font::Body);
                    let total = 14.0 + 7.0 + tw;
                    let x = r.x + (r.w - total) / 2.0;
                    p.glyph(g, Rect::new(x, r.y, 14.0, r.h), 12.0, fg);
                    p.text(label, Rect::new(x + 21.0, r.y, tw + 2.0, r.h), Font::Body, fg, Align::Left);
                }
                None => p.text(label, r, Font::Body, fg, Align::Center),
            }
        }
        self.hit(r, h);
    }

    /// A small × button; red when hovered, since it removes something.
    fn remove_x(&mut self, r: Rect, h: Hit) {
        let th = *self.th;
        self.hit(r, h);
        if let Some(p) = self.p {
            let hot = self.hovered(h);
            if self.pressed(h) {
                p.fill(r, 4.0, th.press);
            } else if hot {
                p.fill(r, 4.0, th.hover);
            }
            p.glyph(glyph::CLOSE, r, 10.0, if hot { th.err } else { th.text3 });
        }
    }

    /// A Windows-style check box: accent with a check mark (or a dash for "some"), else an outline.
    fn check(&mut self, r: Rect, state: Check, h: Hit) {
        let th = *self.th;
        if let Some(p) = self.p {
            if state == Check::Off {
                p.stroke(r, 4.0, if self.hovered(h) { th.text } else { th.text2 }, 1.0);
            } else {
                p.fill(r, 4.0, if self.hovered(h) { th.accent_hover } else { th.accent });
                let g = if state == Check::On { glyph::CHECK } else { glyph::DASH };
                p.glyph(g, r, 12.0, th.on_accent);
            }
        }
    }

    fn toggle(&mut self, r: Rect, on: bool, h: Hit) {
        let th = *self.th;
        if let Some(p) = self.p {
            let big = self.hovered(h);
            if on {
                p.fill(r, r.h / 2.0, if big { th.accent_hover } else { th.accent });
                let k = if big { 7.0 } else { 6.0 };
                p.circle(r.right() - 10.0, r.y + r.h / 2.0, k, th.on_accent);
            } else {
                p.stroke(r, r.h / 2.0, th.text2, 1.0);
                let k = if big { 6.0 } else { 5.0 };
                p.circle(r.x + 10.0, r.y + r.h / 2.0, k, th.text2);
            }
        }
        self.hit(r.inset(-4.0, -8.0), h);
    }

    fn icon(&self, r: Rect, path: &str, name: &str) {
        let Some(p) = self.p else { return };
        p.fill(r, 6.0, self.th.icon_tile);
        if let Some(b) = self.icons.icon(path) {
            p.bitmap(&b, r.inset(5.0, 5.0));
        } else {
            let letter: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            p.text(&letter, r, Font::BodyStrong, self.th.text2, Align::Center);
        }
    }

    fn radio(&mut self, r: Rect, on: bool, title: &str, sub: &str, h: Hit) {
        let th = *self.th;
        self.hover_bg(r, h);
        if let Some(p) = self.p {
            let cx = r.x + 18.0;
            let cy = r.y + r.h / 2.0;
            if on {
                p.circle(cx, cy, 9.0, th.accent);
                p.circle(cx, cy, 4.0, th.on_accent);
            } else {
                p.ring(cx, cy, 9.0, th.text2, 1.0);
            }
            if sub.is_empty() {
                p.text(title, Rect::new(r.x + 38.0, r.y, r.w - 46.0, r.h), Font::Body, th.text, Align::Left);
            } else {
                p.text(title, Rect::new(r.x + 38.0, r.y + 4.0, r.w - 46.0, 20.0), Font::Body, th.text, Align::Left);
                p.text(sub, Rect::new(r.x + 38.0, r.y + 22.0, r.w - 46.0, 18.0), Font::Caption, th.text3, Align::Left);
            }
        }
        self.hit(r, h);
    }

    fn section(&self, y: f32, left: &str, right: &str) {
        let Some(p) = self.p else { return };
        p.text(left, Rect::new(PAD, y, 200.0, SECTION), Font::Caption, self.th.text3, Align::Left);
        p.text(right, Rect::new(WIDTH - PAD - 100.0, y, 100.0, SECTION), Font::Caption, self.th.text3, Align::Right);
    }

    fn footer(&mut self, y: f32) {
        let th = *self.th;
        if let Some(p) = self.p {
            p.fill(Rect::new(0.0, y, WIDTH, FOOTER), 0.0, th.footer);
            p.line(0.0, y, WIDTH, y, th.line, 1.0);
        }
        let rw = self.fonts.measure("Revert all", Font::Body) + 49.0;
        self.button(Rect::new(PAD, y + 11.0, rw, 30.0), "Revert all", Some(glyph::UNDO), Hit::Revert, false);
        if let Some(p) = self.p {
            p.text(&self.m.footer, Rect::new(PAD + rw + 12.0, y, 170.0, FOOTER), Font::Caption, th.text2, Align::Left);
        }
        let g = Rect::new(WIDTH - PAD - 32.0 + 4.0, y + 10.0, 32.0, 32.0);
        self.hover_bg(g, Hit::Gear);
        if let Some(p) = self.p {
            p.glyph(glyph::SETTINGS, g, 15.0, th.text);
            if self.m.update.is_some() {
                // A new version is out: a warning dot on the gear, like Windows Settings does.
                p.circle(g.right() - 7.0, g.y + 7.0, 5.0, th.footer);
                p.circle(g.right() - 7.0, g.y + 7.0, 4.0, th.warn);
            }
        }
        self.hit(g, Hit::Gear);
    }

    fn list_begin(&self, r: Rect) {
        if let Some(p) = self.p {
            p.clip(r);
        }
    }

    fn list_end(&self) {
        if let Some(p) = self.p {
            p.unclip();
        }
    }
}

/// Lays out the current page; draws it when a painter is given. `max_h` is the tallest the flyout may be.
pub fn render(p: Option<&Painter>, th: &Theme, m: &Model, fonts: &Fonts, icons: &dyn Icons, max_h: f32) -> Frame {
    let mut c = Ctx { p, th, m, fonts, icons, hits: Vec::new() };
    if let Some(p) = c.p {
        p.clear(super::theme::Rgba(0.0, 0.0, 0.0, 0.0));
        p.fill(Rect::new(0.0, 0.0, WIDTH, 4000.0), 0.0, th.surface);
    }
    match m.page {
        Page::Main => main_page(&mut c, max_h),
        Page::Add => add_page(&mut c, max_h),
        Page::Settings => settings_page(&mut c, max_h),
    }
}

fn clip_list(hits: &mut [(Rect, Hit)], from: usize, area: Rect) {
    // Rows scrolled out of view must not be clickable.
    for (r, _) in hits[from..].iter_mut() {
        let top = r.y.max(area.y);
        let bottom = r.bottom().min(area.bottom());
        r.h = (bottom - top).max(0.0);
        r.y = top;
    }
}

fn main_page(c: &mut Ctx, max_h: f32) -> Frame {
    let m = c.m;
    let th = *c.th;
    c.header(0.0, "NF Switcher", &m.status, false);
    let mut y = HEADER;
    let tw = (WIDTH - 2.0 * PAD - 8.0) / 2.0;
    c.tile(Rect::new(PAD, y + 4.0, tw, 78.0), glyph::ETHERNET, &m.eth);
    c.tile(Rect::new(PAD + tw + 8.0, y + 4.0, tw, 78.0), glyph::WIFI, &m.wifi);
    y += TILES;
    if let Some(b) = &m.banner {
        y += c.banner(y, b);
    }
    c.section(y, "Apps", if m.apps.is_empty() { "" } else { "Wi-Fi" });
    y += SECTION;

    let fixed = y + ADD + FOOTER;
    let content = if m.apps.is_empty() { 64.0 } else { m.apps.len() as f32 * ROW + 4.0 };
    let list_h = content.min((max_h - fixed).max(ROW + 4.0));
    let area = Rect::new(0.0, y, WIDTH, list_h);
    let first_hit = c.hits.len();
    c.list_begin(area);
    if m.apps.is_empty() {
        if let Some(p) = c.p {
            paragraph(
                p,
                "No apps yet. Add one to send it over Wi-Fi; everything else keeps Windows' default route.",
                Rect::new(PAD, y + 8.0, WIDTH - 2.0 * PAD, 48.0),
                Font::Caption,
                th.text2,
            );
        }
    }
    let mut ry = y + 2.0 - m.scroll;
    for (i, a) in m.apps.iter().enumerate() {
        if ry + ROW >= y && ry <= y + list_h {
            let row = Rect::new(8.0, ry, WIDTH - 16.0, ROW);
            c.hover_bg(row, Hit::AppRow(i));
            c.hit(row, Hit::AppRow(i));
            // Added after the row, so a click on it removes the app instead of switching it.
            c.remove_x(Rect::new(10.0, ry + 14.0, 24.0, 24.0), Hit::Remove(i));
            c.icon(Rect::new(PAD + 22.0, ry + 11.0, 30.0, 30.0), &a.path, &a.name);
            if let Some(p) = c.p {
                let tx = PAD + 64.0;
                let tw = WIDTH - tx - PAD - 52.0;
                p.text(&a.name, Rect::new(tx, ry + 7.0, tw, 20.0), Font::Body, th.text, Align::Left);
                let sc = if a.warn { th.warn } else { th.text3 };
                p.text(&a.status, Rect::new(tx, ry + 27.0, tw, 18.0), Font::Caption, sc, Align::Left);
            }
            c.toggle(Rect::new(WIDTH - PAD - 40.0, ry + 16.0, 40.0, 20.0), a.wifi, Hit::Toggle(i));
        }
        ry += ROW;
    }
    c.list_end();
    clip_list(&mut c.hits, first_hit, area);
    y += list_h;

    let add = Rect::new(PAD, y + 4.0, WIDTH - 2.0 * PAD, 34.0);
    if let Some(p) = c.p {
        if c.hovered(Hit::AddApp) {
            p.fill(add, 5.0, th.hover);
        }
        p.dashed(add, 5.0, th.dashed);
        let label = "Add an app";
        let lw = p.measure(label, Font::Body);
        let x = add.x + (add.w - lw - 21.0) / 2.0;
        p.glyph(glyph::ADD, Rect::new(x, add.y, 14.0, add.h), 12.0, th.text2);
        p.text(label, Rect::new(x + 21.0, add.y, lw + 2.0, add.h), Font::Body, th.text2, Align::Left);
    }
    c.hit(add, Hit::AddApp);
    y += ADD;
    c.footer(y);
    y += FOOTER;
    Frame { height: y, hits: std::mem::take(&mut c.hits), list: Some((area, content)) }
}

fn add_page(c: &mut Ctx, max_h: f32) -> Frame {
    let m = c.m;
    let th = *c.th;
    c.header(0.0, "Add an app", &format!("{} running", m.running), true);
    let mut y = HEADER;
    let field = Rect::new(PAD, y + 4.0, WIDTH - 2.0 * PAD, 34.0);
    if let Some(p) = c.p {
        p.fill(field, 5.0, th.field);
        p.stroke(field, 5.0, th.tile_line, 1.0);
        if m.search_focus {
            p.fill(Rect::new(field.x + 1.0, field.bottom() - 2.0, field.w - 2.0, 2.0), 1.0, th.accent);
        }
        p.glyph(glyph::SEARCH, Rect::new(field.x + 8.0, field.y, 18.0, field.h), 12.0, th.text3);
        let tx = field.x + 32.0;
        if m.search.is_empty() {
            p.text("Search by name or path", Rect::new(tx, field.y, field.w - 40.0, field.h), Font::Body, th.text3, Align::Left);
        } else {
            p.text(&m.search, Rect::new(tx, field.y, field.w - 40.0, field.h), Font::Body, th.text, Align::Left);
        }
        if m.search_focus && m.caret {
            let cx = tx + p.measure(&m.search, Font::Body) + 1.0;
            p.line(cx, field.y + 9.0, cx, field.bottom() - 9.0, th.text, 1.0);
        }
    }
    c.hit(field, Hit::Search);
    y += SEARCH;

    // "Select all": the programs the search shows that aren't added yet.
    let open = m.picks.iter().filter(|p| !p.added).count();
    let ticked = m.picks.iter().filter(|p| !p.added && p.selected).count();
    if open > 0 {
        let row = Rect::new(8.0, y, WIDTH - 16.0, SELECT_ALL);
        c.hover_bg(row, Hit::SelectAll);
        c.hit(row, Hit::SelectAll);
        if let Some(p) = c.p {
            p.text("Select all", Rect::new(PAD, y, 200.0, SELECT_ALL), Font::Body, th.text, Align::Left);
        }
        let state = match ticked {
            0 => Check::Off,
            n if n == open => Check::On,
            _ => Check::Part,
        };
        c.check(Rect::new(WIDTH - PAD - 28.0, y + 8.0, 20.0, 20.0), state, Hit::SelectAll);
        y += SELECT_ALL;
    }

    let rows = m.picks.len() + 1;
    let content = rows as f32 * ROW + 8.0;
    let list_h = content.min((max_h - y - 6.0 - ADD_BAR).max(ROW * 3.0));
    let area = Rect::new(0.0, y, WIDTH, list_h);
    let first_hit = c.hits.len();
    c.list_begin(area);
    let mut ry = y - m.scroll;
    // Row 0 picks an .exe from disk (an app that isn't running yet); the running programs follow.
    for row_i in 0..rows {
        if ry + ROW >= y && ry <= y + list_h {
            let row = Rect::new(8.0, ry, WIDTH - 16.0, ROW);
            if row_i > 0 {
                let i = row_i - 1;
                let it = &m.picks[i];
                let h = Hit::Pick(i);
                if !it.added {
                    c.hover_bg(row, h);
                    c.hit(row, h);
                }
                c.icon(Rect::new(PAD, ry + 11.0, 30.0, 30.0), &it.path, &it.name);
                let tx = PAD + 42.0;
                let tw = WIDTH - tx - PAD - 70.0;
                if let Some(p) = c.p {
                    p.text(&it.name, Rect::new(tx, ry + 7.0, tw, 20.0), Font::Body, th.text, Align::Left);
                    p.text(&it.path, Rect::new(tx, ry + 27.0, tw, 18.0), Font::Mono, th.text3, Align::Left);
                    if it.added {
                        let b = Rect::new(WIDTH - PAD - 60.0, ry + 12.0, 60.0, 28.0);
                        p.text("Added", b, Font::Caption, th.text3, Align::Center);
                    }
                }
                if !it.added {
                    let state = if it.selected { Check::On } else { Check::Off };
                    c.check(Rect::new(WIDTH - PAD - 28.0, ry + 16.0, 20.0, 20.0), state, h);
                }
            } else {
                c.hover_bg(row, Hit::PickExe);
                c.hit(row, Hit::PickExe);
                if let Some(p) = c.p {
                    let t = Rect::new(PAD, ry + 11.0, 30.0, 30.0);
                    p.fill(t, 6.0, th.icon_tile);
                    p.glyph(glyph::FOLDER, t, 14.0, th.text2);
                    let tx = PAD + 42.0;
                    p.text("Pick an .exe that isn't running…", Rect::new(tx, ry + 7.0, 280.0, 20.0), Font::Body, th.text, Align::Left);
                    p.text("It switches when it starts", Rect::new(tx, ry + 27.0, 280.0, 18.0), Font::Caption, th.text3, Align::Left);
                }
            }
        }
        ry += ROW;
    }
    c.list_end();
    clip_list(&mut c.hits, first_hit, area);
    y += list_h + 6.0;

    // The ticked programs (also ones the search hides now) go on Wi-Fi together.
    let n = m.selected.len();
    let b = Rect::new(PAD, y + 4.0, WIDTH - 2.0 * PAD, 36.0);
    if n > 0 {
        let label = if n == 1 { "Add 1 app to Wi-Fi".to_string() } else { format!("Add {n} apps to Wi-Fi") };
        c.button(b, &label, None, Hit::AddSelected, true);
    } else if let Some(p) = c.p {
        p.fill(b, 5.0, th.button);
        p.text("Tick the apps to add", b, Font::Body, th.text3, Align::Center);
    }
    y += ADD_BAR;
    Frame { height: y, hits: std::mem::take(&mut c.hits), list: Some((area, content)) }
}

fn settings_page(c: &mut Ctx, max_h: f32) -> Frame {
    let m = c.m;
    let th = *c.th;
    c.header(0.0, "Settings", "", true);
    let top = HEADER;
    // The whole page scrolls when it doesn't fit.
    let area_h = (max_h - top).max(200.0);
    let area = Rect::new(0.0, top, WIDTH, area_h);
    let first_hit = c.hits.len();
    c.list_begin(area);
    let mut y = top - m.scroll;
    let toggle_row = |c: &mut Ctx, y: f32, title: &str, sub: &str, on: bool, h: Hit| {
        let row = Rect::new(8.0, y, WIDTH - 16.0, RADIO);
        c.hover_bg(row, h);
        c.hit(row, h);
        if let Some(p) = c.p {
            p.text(title, Rect::new(PAD, y + 4.0, WIDTH - 16.0 - 70.0, 20.0), Font::Body, c.th.text, Align::Left);
            p.text(sub, Rect::new(PAD, y + 22.0, WIDTH - 16.0 - 70.0, 18.0), Font::Caption, c.th.text3, Align::Left);
        }
        c.toggle(Rect::new(WIDTH - PAD - 40.0, y + 12.0, 40.0, 20.0), on, h);
    };
    if let Some(v) = &m.update {
        // The "New update" card, first on the page: "Update now" (or "Download"), then its progress.
        let r = Rect::new(PAD, y + 4.0, WIDTH - 2.0 * PAD, 62.0);
        let bw = m.update_button.as_ref().map(|l| c.fonts.measure(l, Font::Body) + 28.0).unwrap_or(0.0);
        if let Some(p) = c.p {
            p.fill(r, 6.0, th.warn_bg);
            p.stroke(r, 6.0, th.tile_line, 1.0);
            p.glyph(glyph::WARNING, Rect::new(r.x + 10.0, r.y + 10.0, 20.0, 20.0), 15.0, th.warn);
            p.text("New update", Rect::new(r.x + 38.0, r.y + 8.0, 180.0, 22.0), Font::BodyStrong, th.text, Align::Left);
            let sub = if m.update_sub.is_empty() { format!("NF Switcher {v} is out") } else { m.update_sub.clone() };
            let sub_w = r.w - 38.0 - if bw > 0.0 { bw + 24.0 } else { 12.0 };
            p.text(&sub, Rect::new(r.x + 38.0, r.y + 30.0, sub_w, 20.0), Font::Caption, th.text2, Align::Left);
            if let Some(f) = m.update_progress {
                let bar = Rect::new(r.x + 38.0, r.bottom() - 9.0, r.w - 50.0, 3.0);
                p.fill(bar, 1.5, th.tile_line);
                p.fill(Rect::new(bar.x, bar.y, bar.w * f.clamp(0.0, 1.0), bar.h), 1.5, th.accent);
            }
        }
        if let Some(label) = &m.update_button {
            c.button(Rect::new(r.right() - 12.0 - bw, r.y + 15.0, bw, 32.0), label, None, Hit::UpdateDownload, true);
        }
        y += 74.0;
    }
    c.section(y, "When Wi-Fi is down", "");
    y += SECTION;
    let w = WIDTH - 16.0;
    c.radio(Rect::new(8.0, y, w, RADIO), !m.block_when_down, "Use the default route", "Pinned apps keep working over Ethernet", Hit::WhenDown(false));
    y += RADIO;
    c.radio(Rect::new(8.0, y, w, RADIO), m.block_when_down, "Block until Wi-Fi is back", "Pinned apps never use Ethernet", Hit::WhenDown(true));
    y += RADIO + 8.0;
    c.section(y, "Connection for pinned apps", "");
    y += SECTION;
    c.radio(Rect::new(8.0, y, w, RADIO), m.adapter_auto, "Automatic", "The connected Wi-Fi adapter", Hit::AdapterAuto);
    y += RADIO;
    for (i, a) in m.adapters.iter().enumerate() {
        c.radio(Rect::new(8.0, y, w, RADIO), a.selected, &a.name, &a.sub, Hit::Adapter(i));
        y += RADIO;
    }
    y += 8.0;
    c.section(y, "Local network", "");
    y += SECTION;
    let row = Rect::new(8.0, y, w, RADIO);
    c.hover_bg(row, Hit::LanKeep);
    c.hit(row, Hit::LanKeep);
    if let Some(p) = c.p {
        p.text("Keep the Ethernet network on Ethernet", Rect::new(PAD, y + 4.0, w - 70.0, 20.0), Font::Body, th.text, Align::Left);
        p.text("Pinned apps still reach the router, NAS, printer", Rect::new(PAD, y + 22.0, w - 70.0, 18.0), Font::Caption, th.text3, Align::Left);
    }
    c.toggle(Rect::new(WIDTH - PAD - 40.0, y + 12.0, 40.0, 20.0), m.lan_keep, Hit::LanKeep);
    y += RADIO;
    let row = Rect::new(8.0, y, w, RADIO);
    c.hover_bg(row, Hit::Reconnect);
    c.hit(row, Hit::Reconnect);
    if let Some(p) = c.p {
        p.text("Reconnect Wi-Fi when Windows drops it", Rect::new(PAD, y + 4.0, w - 70.0, 20.0), Font::Body, th.text, Align::Left);
        p.text("While apps are pinned, at most once a minute", Rect::new(PAD, y + 22.0, w - 70.0, 18.0), Font::Caption, th.text3, Align::Left);
    }
    c.toggle(Rect::new(WIDTH - PAD - 40.0, y + 12.0, 40.0, 20.0), m.reconnect, Hit::Reconnect);
    y += RADIO + 8.0;
    c.section(y, "Startup", "");
    y += SECTION;
    toggle_row(c, y, "Start with Windows", "In the tray at sign-in, without a UAC prompt", m.autostart, Hit::Autostart);
    y += RADIO + 8.0;
    c.section(y, "Updates", "");
    y += SECTION;
    let row = Rect::new(8.0, y, w, RADIO);
    c.hover_bg(row, Hit::CheckUpdates);
    c.hit(row, Hit::CheckUpdates);
    if let Some(p) = c.p {
        let sub = if m.update_note.is_empty() { "Asks GitHub once when NF Switcher starts" } else { m.update_note.as_str() };
        p.text("Check for updates", Rect::new(PAD, y + 4.0, w - 70.0, 20.0), Font::Body, th.text, Align::Left);
        p.text(sub, Rect::new(PAD, y + 22.0, w - 70.0, 18.0), Font::Caption, th.text3, Align::Left);
    }
    c.toggle(Rect::new(WIDTH - PAD - 40.0, y + 12.0, 40.0, 20.0), m.check_updates, Hit::CheckUpdates);
    y += RADIO + 8.0;
    c.section(y, "About", "");
    y += SECTION;
    for line in &m.about {
        let h = paragraph_height(c.fonts, line, Font::Caption, WIDTH - 2.0 * PAD);
        if let Some(p) = c.p {
            paragraph(p, line, Rect::new(PAD, y, WIDTH - 2.0 * PAD, h), Font::Caption, th.text2);
        }
        y += h + 6.0;
    }
    y += 10.0;
    c.list_end();
    let content = y + m.scroll - top;
    clip_list(&mut c.hits, first_hit, area);
    let shown = content.min(area_h);
    let area = Rect::new(0.0, top, WIDTH, shown);
    Frame { height: top + shown, hits: std::mem::take(&mut c.hits), list: Some((area, content)) }
}

/// Wrapped text (banners, About).
pub fn paragraph(p: &Painter, s: &str, r: Rect, f: Font, color: super::theme::Rgba) {
    use windows::Win32::Graphics::DirectWrite::*;
    let Ok(fmt) = p.fonts.format(f, Align::Left) else { return };
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let Ok(layout) = p.fonts.dw.CreateTextLayout(&wide, &fmt, r.w, r.h.max(1.0)) else { return };
        let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
        let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
        let brush = p.rt.CreateSolidColorBrush(
            &windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r: color.0, g: color.1, b: color.2, a: color.3 },
            None,
        );
        if let Ok(b) = brush {
            p.rt.DrawTextLayout(
                windows_numerics::Vector2 { X: r.x, Y: r.y },
                &layout,
                &b,
                windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
}

pub fn paragraph_height(fonts: &Fonts, s: &str, f: Font, w: f32) -> f32 {
    use windows::Win32::Graphics::DirectWrite::*;
    let Ok(fmt) = fonts.format(f, Align::Left) else { return 18.0 };
    let wide: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let Ok(layout) = fonts.dw.CreateTextLayout(&wide, &fmt, w, 1000.0) else { return 18.0 };
        let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
        let mut m = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut m).is_ok() { m.height.ceil() } else { 18.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_app_row_has_an_x_that_removes_it() {
        let fonts = Fonts::new().unwrap();
        let m = super::super::sample::main_page();
        let f = render(None, &Theme::dark(false), &m, &fonts, &NoIcons, 640.0);
        let xs: Vec<Rect> = f.hits.iter().filter(|(_, h)| matches!(h, Hit::Remove(_))).map(|(r, _)| *r).collect();
        assert_eq!(xs.len(), m.apps.len());
        let (x, y) = (xs[0].x + xs[0].w / 2.0, xs[0].y + xs[0].h / 2.0);
        assert_eq!(f.hit(x, y), Some(Hit::Remove(0)));
        // The rest of the row still switches the app; the switch itself too.
        assert_eq!(f.hit(200.0, y), Some(Hit::AppRow(0)));
        assert_eq!(f.hit(WIDTH - PAD - 20.0, y), Some(Hit::Toggle(0)));
    }
}
