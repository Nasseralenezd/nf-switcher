// Drawing helpers over any Direct2D render target (the flyout's window or an offscreen bitmap for --shots).
// Everything is in DIPs: the render target's DPI does the scaling.
use super::theme::Rgba;
use std::cell::RefCell;
use std::collections::HashMap;
use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows_numerics::Vector2;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Font {
    /// Flyout title: Segoe UI Variable Display, semibold.
    Title,
    /// The installer's big title.
    Heading,
    /// List names.
    Body,
    BodyStrong,
    /// Second lines, section labels.
    Caption,
    /// IP addresses and paths.
    Mono,
    /// Segoe Fluent Icons at a size in tenths of a DIP.
    Glyph(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// DirectWrite objects that outlive a frame: the factory and the text formats.
pub struct Fonts {
    pub dw: IDWriteFactory,
    formats: RefCell<HashMap<(Font, Align), IDWriteTextFormat>>,
}

impl Fonts {
    pub fn new() -> windows::core::Result<Fonts> {
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Fonts { dw, formats: RefCell::new(HashMap::new()) })
    }

    fn spec(f: Font) -> (PCWSTR, f32, DWRITE_FONT_WEIGHT) {
        match f {
            Font::Title => (w!("Segoe UI Variable Display"), 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Font::Heading => (w!("Segoe UI Variable Display"), 18.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Font::Body => (w!("Segoe UI Variable Text"), 13.5, DWRITE_FONT_WEIGHT_NORMAL),
            Font::BodyStrong => (w!("Segoe UI Variable Text"), 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Font::Caption => (w!("Segoe UI Variable Text"), 11.5, DWRITE_FONT_WEIGHT_NORMAL),
            Font::Mono => (w!("Cascadia Mono"), 11.0, DWRITE_FONT_WEIGHT_NORMAL),
            Font::Glyph(t) => (w!("Segoe Fluent Icons"), t as f32 / 10.0, DWRITE_FONT_WEIGHT_NORMAL),
        }
    }

    pub fn format(&self, f: Font, a: Align) -> windows::core::Result<IDWriteTextFormat> {
        if let Some(t) = self.formats.borrow().get(&(f, a)) {
            return Ok(t.clone());
        }
        let (family, size, weight) = Fonts::spec(f);
        let t = unsafe {
            let t = self.dw.CreateTextFormat(
                family,
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            t.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            t.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            t.SetTextAlignment(match a {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            let sign = self.dw.CreateEllipsisTrimmingSign(&t)?;
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            t.SetTrimming(&trim, &sign)?;
            t
        };
        self.formats.borrow_mut().insert((f, a), t.clone());
        Ok(t)
    }

    /// Width of a text in DIPs.
    pub fn measure(&self, s: &str, f: Font) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let Ok(fmt) = self.format(f, Align::Left) else { return 0.0 };
        unsafe {
            let Ok(layout) = self.dw.CreateTextLayout(&wide, &fmt, 10000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
        }
    }
}

fn color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 }
}

/// One frame's drawing.
pub struct Painter<'a> {
    pub rt: &'a ID2D1RenderTarget,
    pub fonts: &'a Fonts,
    brush: ID2D1SolidColorBrush,
    dash: Option<ID2D1StrokeStyle>,
}

impl<'a> Painter<'a> {
    pub fn new(rt: &'a ID2D1RenderTarget, fonts: &'a Fonts, factory: &ID2D1Factory) -> windows::core::Result<Painter<'a>> {
        let brush = unsafe { rt.CreateSolidColorBrush(&color(Rgba(0.0, 0.0, 0.0, 1.0)), None)? };
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_FLAT,
            endCap: D2D1_CAP_STYLE_FLAT,
            dashCap: D2D1_CAP_STYLE_FLAT,
            lineJoin: D2D1_LINE_JOIN_MITER,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_CUSTOM,
            dashOffset: 0.0,
        };
        let dash = unsafe { factory.CreateStrokeStyle(&props, Some(&[4.0, 3.0])).ok() };
        Ok(Painter { rt, fonts, brush, dash })
    }

    fn set(&self, c: Rgba) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(c)) };
        &self.brush
    }

    pub fn clear(&self, c: Rgba) {
        unsafe { self.rt.Clear(Some(&color(c))) };
    }

    pub fn fill(&self, r: Rect, radius: f32, c: Rgba) {
        unsafe {
            if radius <= 0.0 {
                self.rt.FillRectangle(&r.d2d(), self.set(c));
            } else {
                let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
                self.rt.FillRoundedRectangle(&rr, self.set(c));
            }
        }
    }

    pub fn stroke(&self, r: Rect, radius: f32, c: Rgba, width: f32) {
        // Half a stroke inside, so the line sits on the rect's edge pixels.
        let r = r.inset(width / 2.0, width / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.set(c), width, None) };
    }

    pub fn dashed(&self, r: Rect, radius: f32, c: Rgba) {
        let r = r.inset(0.5, 0.5);
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.set(c), 1.0, self.dash.as_ref()) };
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, c: Rgba, width: f32) {
        unsafe { self.rt.DrawLine(Vector2 { X: x1, Y: y1 }, Vector2 { X: x2, Y: y2 }, self.set(c), width, None) };
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: Rgba) {
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, self.set(c)) };
    }

    pub fn ring(&self, cx: f32, cy: f32, r: f32, c: Rgba, width: f32) {
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r - width / 2.0, radiusY: r - width / 2.0 };
        unsafe { self.rt.DrawEllipse(&e, self.set(c), width, None) };
    }

    pub fn text(&self, s: &str, r: Rect, f: Font, c: Rgba, a: Align) {
        let Ok(fmt) = self.fonts.format(f, a) else { return };
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt.DrawText(
                &wide,
                &fmt,
                &r.d2d(),
                self.set(c),
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    /// A Segoe Fluent Icons glyph centred in `r`.
    pub fn glyph(&self, code: u16, r: Rect, size: f32, c: Rgba) {
        let s = String::from_utf16_lossy(&[code]);
        self.text(&s, r, Font::Glyph((size * 10.0) as u16), c, Align::Center);
    }

    pub fn bitmap(&self, b: &ID2D1Bitmap, r: Rect) {
        unsafe {
            self.rt.DrawBitmap(b, Some(&r.d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
        }
    }

    pub fn clip(&self, r: Rect) {
        unsafe { self.rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn unclip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
    }

    pub fn measure(&self, s: &str, f: Font) -> f32 {
        self.fonts.measure(s, f)
    }
}

/// The render target as the base interface, for either kind of target.
pub fn base(rt: &impl Interface) -> ID2D1RenderTarget {
    rt.cast().expect("ID2D1RenderTarget")
}

#[allow(dead_code)]
pub fn hstring(s: &str) -> HSTRING {
    HSTRING::from(s)
}
