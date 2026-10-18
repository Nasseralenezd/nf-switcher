// Concept C's colours (the tray flyout, picked 2026-10-18). Flyouts next to the clock follow Windows' *system*
// theme (taskbar colour), not the app theme, and the acrylic backdrop only when transparency effects are on.
use windows::core::w;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

pub const fn hex(v: u32) -> Rgba {
    Rgba(((v >> 16) & 255) as f32 / 255.0, ((v >> 8) & 255) as f32 / 255.0, (v & 255) as f32 / 255.0, 1.0)
}

pub const fn hexa(v: u32, a: f32) -> Rgba {
    let c = hex(v);
    Rgba(c.0, c.1, c.2, a)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// Draw on the system acrylic (DWM) instead of an opaque fill.
    pub acrylic: bool,
    /// The flyout's own fill: under acrylic only a light tint, else opaque.
    pub surface: Rgba,
    pub footer: Rgba,
    pub tile: Rgba,
    pub tile_line: Rgba,
    pub hover: Rgba,
    pub press: Rgba,
    pub icon_tile: Rgba,
    pub line: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub text3: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub on_accent: Rgba,
    pub field: Rgba,
    pub button: Rgba,
    pub button_hover: Rgba,
    pub dashed: Rgba,
    pub warn: Rgba,
    pub warn_bg: Rgba,
    pub ok: Rgba,
    pub err: Rgba,
}

impl Theme {
    pub fn dark(acrylic: bool) -> Theme {
        Theme {
            dark: true,
            acrylic,
            surface: if acrylic { hexa(0x202020, 0.55) } else { hex(0x2c2c2c) },
            footer: if acrylic { hexa(0x000000, 0.28) } else { hex(0x202020) },
            tile: hexa(0xffffff, 0.06),
            tile_line: hexa(0xffffff, 0.09),
            hover: hexa(0xffffff, 0.06),
            press: hexa(0xffffff, 0.04),
            icon_tile: hexa(0xffffff, 0.08),
            line: hexa(0xffffff, 0.08),
            text: hex(0xffffff),
            text2: hex(0xc5c5c5),
            text3: hex(0x9d9d9d),
            accent: hex(0x60cdff),
            accent_hover: hex(0x5abce9),
            on_accent: hex(0x000000),
            field: hexa(0x000000, 0.3),
            button: hexa(0xffffff, 0.08),
            button_hover: hexa(0xffffff, 0.12),
            dashed: hexa(0xffffff, 0.28),
            warn: hex(0xfce100),
            warn_bg: hexa(0xfce100, 0.10),
            ok: hex(0x6ccb5f),
            err: hex(0xff99a4),
        }
    }

    pub fn light(acrylic: bool) -> Theme {
        Theme {
            dark: false,
            acrylic,
            surface: if acrylic { hexa(0xf3f3f3, 0.6) } else { hex(0xf3f3f3) },
            footer: if acrylic { hexa(0xffffff, 0.45) } else { hex(0xeeeeee) },
            tile: hexa(0xffffff, 0.7),
            tile_line: hexa(0x000000, 0.06),
            hover: hexa(0x000000, 0.04),
            press: hexa(0x000000, 0.025),
            icon_tile: hexa(0x000000, 0.05),
            line: hexa(0x000000, 0.08),
            text: hex(0x1a1a1a),
            text2: hex(0x5d5d5d),
            text3: hex(0x8a8a8a),
            accent: hex(0x0067c0),
            accent_hover: hex(0x1975c5),
            on_accent: hex(0xffffff),
            field: hexa(0xffffff, 0.8),
            button: hexa(0x000000, 0.06),
            button_hover: hexa(0x000000, 0.09),
            dashed: hexa(0x000000, 0.25),
            warn: hex(0x9d5d00),
            warn_bg: hexa(0xfff4ce, 0.9),
            ok: hex(0x0f7b0f),
            err: hex(0xc42b1c),
        }
    }

    /// Windows' current system theme and transparency setting.
    /// Windows' system theme. Always the opaque palette: the window has no DWM backdrop (concept C is near-opaque
    /// anyway), and a translucent fill would be drawn over black.
    pub fn current() -> Theme {
        let light = reg_dword("SystemUsesLightTheme").unwrap_or(0) == 1;
        if light { Theme::light(false) } else { Theme::dark(false) }
    }
}

fn reg_dword(name: &str) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            windows::core::PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(v)
}
