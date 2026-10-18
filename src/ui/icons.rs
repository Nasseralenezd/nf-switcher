// Each app's own icon, read from its .exe (SHDefExtractIconW), turned into a premultiplied WIC bitmap once, and into
// a Direct2D bitmap per render target. A missing icon is remembered too, so a bad .exe isn't re-read every frame.
use super::view::Icons;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use windows::core::{Interface, HSTRING};
use windows::Win32::Graphics::Direct2D::{ID2D1Bitmap, ID2D1RenderTarget};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

pub struct IconCache {
    wic: IWICImagingFactory,
    /// Icon size in pixels (the 20 DIP icon at the current DPI).
    size: Cell<u32>,
    sources: RefCell<HashMap<String, Option<IWICBitmapSource>>>,
    bitmaps: RefCell<HashMap<String, Option<ID2D1Bitmap>>>,
    rt: RefCell<Option<ID2D1RenderTarget>>,
}

impl IconCache {
    pub fn new(wic: IWICImagingFactory) -> IconCache {
        IconCache {
            wic,
            size: Cell::new(20),
            sources: RefCell::new(HashMap::new()),
            bitmaps: RefCell::new(HashMap::new()),
            rt: RefCell::new(None),
        }
    }

    pub fn wic() -> windows::core::Result<IWICImagingFactory> {
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
    }

    /// Draws into `rt` from now on; Direct2D bitmaps of an older target are dropped.
    pub fn bind(&self, rt: &ID2D1RenderTarget, dpi: f32) {
        let size = ((20.0 * dpi / 96.0).round() as u32).max(16);
        let same = self.rt.borrow().as_ref().is_some_and(|r| r.as_raw() == rt.as_raw());
        if !same || size != self.size.get() {
            self.bitmaps.borrow_mut().clear();
            *self.rt.borrow_mut() = Some(rt.clone());
        }
        if size != self.size.get() {
            self.sources.borrow_mut().clear();
            self.size.set(size);
        }
    }

    pub fn unbind(&self) {
        self.bitmaps.borrow_mut().clear();
        *self.rt.borrow_mut() = None;
    }

    fn source(&self, path: &str) -> Option<IWICBitmapSource> {
        if let Some(s) = self.sources.borrow().get(path) {
            return s.clone();
        }
        let s = self.load(path);
        self.sources.borrow_mut().insert(path.to_string(), s.clone());
        s
    }

    fn load(&self, path: &str) -> Option<IWICBitmapSource> {
        unsafe {
            let mut large = HICON::default();
            let r = SHDefExtractIconW(&HSTRING::from(path), 0, 0, Some(&mut large), None, self.size.get());
            if r.is_err() || large.is_invalid() {
                return None;
            }
            let bmp = self.wic.CreateBitmapFromHICON(large);
            let _ = DestroyIcon(large);
            let bmp = bmp.ok()?;
            let conv = self.wic.CreateFormatConverter().ok()?;
            conv.Initialize(&bmp, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
                .ok()?;
            // Copy into a plain bitmap so the HICON-backed source isn't re-read on every draw.
            let copy = self.wic.CreateBitmapFromSource(&conv, WICBitmapCacheOnLoad).ok()?;
            copy.cast().ok()
        }
    }
}

impl Icons for IconCache {
    fn icon(&self, path: &str) -> Option<ID2D1Bitmap> {
        if path.is_empty() {
            return None;
        }
        if let Some(b) = self.bitmaps.borrow().get(path) {
            return b.clone();
        }
        let rt = self.rt.borrow().clone()?;
        let b = self.source(path).and_then(|s| unsafe { rt.CreateBitmapFromWicBitmap(&s, None).ok() });
        self.bitmaps.borrow_mut().insert(path.to_string(), b.clone());
        b
    }
}
