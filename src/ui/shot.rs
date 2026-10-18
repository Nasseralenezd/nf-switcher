// Draws a view into an offscreen bitmap and saves it as PNG (--shots, --smoke), so the UI can be checked without
// showing any window.
use super::icons::IconCache;
use super::paint::{base, Fonts, Painter};
use super::theme::Theme;
use super::view::{render, Model, WIDTH};
use std::path::Path;
use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::*;

/// Lays out (to get the height), then draws at `scale` and saves. Returns the height in DIPs.
pub fn save_with(
    width: f32,
    scale: f32,
    out: &Path,
    layout: impl Fn(&Fonts) -> f32,
    draw: impl Fn(&Painter, &Fonts, &IconCache),
) -> Result<f32> {
    unsafe {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let fonts = Fonts::new()?;
        let wic = IconCache::wic()?;
        let icons = IconCache::new(wic.clone());
        let height = layout(&fonts);
        let (wpx, hpx) = ((width * scale).round() as u32, (height * scale).round() as u32);
        let bitmap = wic.CreateBitmap(wpx, hpx, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0 * scale,
            dpiY: 96.0 * scale,
            ..Default::default()
        };
        let rt = factory.CreateWicBitmapRenderTarget(&bitmap, &props)?;
        let rt = base(&rt);
        icons.bind(&rt, 96.0 * scale);
        let p = Painter::new(&rt, &fonts, &factory)?;
        rt.BeginDraw();
        draw(&p, &fonts, &icons);
        rt.EndDraw(None, None)?;
        icons.unbind();
        encode_png(&wic, &bitmap, wpx, hpx, out)?;
        Ok(height)
    }
}

/// The flyout. Acrylic is a DWM effect, so shots use the opaque colours.
pub fn save_png(m: &Model, th: &Theme, scale: f32, max_h: f32, out: &Path) -> Result<f32> {
    save_with(
        WIDTH,
        scale,
        out,
        |fonts| render(None, th, m, fonts, &super::view::NoIcons, max_h).height,
        |p, fonts, icons| {
            render(Some(p), th, m, fonts, icons, max_h);
        },
    )
}

/// The installer's window.
pub fn save_setup_png(m: &super::setup_view::Model, th: &Theme, scale: f32, out: &Path) -> Result<f32> {
    use super::setup_view::{render as setup, WIDTH as SW};
    save_with(SW, scale, out, |fonts| setup(None, th, m, fonts).height, |p, fonts, _| {
        setup(Some(p), th, m, fonts);
    })
}

/// Saves a premultiplied 32-bit WIC bitmap as PNG.
pub fn encode_png(wic: &IWICImagingFactory, bitmap: &IWICBitmap, w: u32, h: u32, out: &Path) -> Result<()> {
    unsafe {
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(out.as_os_str()), GENERIC_WRITE.0)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.expect("frame");
        frame.Initialize(None)?;
        frame.SetSize(w, h)?;
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(bitmap, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()
    }
}
