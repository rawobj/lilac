//! Colors, metrics and the glass surface helpers shared by every view.

use gpui_kit::*;

/// Build an `Hsla` from 8-bit RGB and a float alpha.
pub const fn rgba8(hex: u32, alpha: f32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: alpha,
    }
}

pub fn white(alpha: f32) -> Hsla {
    rgba8(0xffffff, alpha).into()
}

pub fn black(alpha: f32) -> Hsla {
    rgba8(0x000000, alpha).into()
}

/// Window backdrop. Kept translucent so the compositor blur shows through.
pub fn backdrop() -> Hsla {
    rgba8(0x0b0714, 0.78).into()
}

pub fn backdrop_opaque() -> Hsla {
    rgba8(0x0b0714, 1.0).into()
}

pub fn accent() -> Hsla {
    rgba8(0xa78bfa, 1.0).into()
}

pub fn accent_strong() -> Hsla {
    rgba8(0x8b5cf6, 1.0).into()
}

pub fn accent_soft() -> Hsla {
    rgba8(0xc4b5fd, 1.0).into()
}

pub fn accent_alpha(alpha: f32) -> Hsla {
    rgba8(0xa78bfa, alpha).into()
}

pub fn pink() -> Hsla {
    rgba8(0xf0abfc, 1.0).into()
}

pub fn text() -> Hsla {
    rgba8(0xf3eefc, 1.0).into()
}

pub fn text_muted() -> Hsla {
    rgba8(0xb1a8cc, 1.0).into()
}

pub fn text_subtle() -> Hsla {
    rgba8(0x7d7499, 1.0).into()
}

pub fn danger() -> Hsla {
    rgba8(0xfb7185, 1.0).into()
}

/// Fill of a resting glass surface.
pub fn glass_fill() -> Hsla {
    rgba8(0xd9ccff, 0.055).into()
}

pub fn glass_fill_strong() -> Hsla {
    rgba8(0xd9ccff, 0.10).into()
}

pub fn glass_border() -> Hsla {
    rgba8(0xe6dcff, 0.10).into()
}

pub fn hover_fill() -> Hsla {
    rgba8(0xe6dcff, 0.085).into()
}

pub fn active_fill() -> Hsla {
    rgba8(0xe6dcff, 0.14).into()
}

/// Opaque-ish surface for popups that sit above other content.
pub fn popup_fill() -> Hsla {
    rgba8(0x1c1530, 0.97).into()
}

pub const RADIUS_PANEL: f32 = 16.0;
pub const RADIUS_CARD: f32 = 12.0;
pub const RADIUS_CONTROL: f32 = 8.0;
pub const GAP: f32 = 8.0;

pub const SIDEBAR_WIDTH: f32 = 300.0;
pub const SIDEBAR_COLLAPSED: f32 = 76.0;
pub const RIGHT_PANEL_WIDTH: f32 = 340.0;
pub const PLAYER_BAR_HEIGHT: f32 = 84.0;
pub const TOP_BAR_HEIGHT: f32 = 56.0;

pub const FONT_UI: &str = "Inter";

/// A frosted panel: translucent fill, hairline border, soft inner highlight.
pub fn glass() -> Div {
    div()
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgba8(0xe6dcff, 0.075), 0.0),
            linear_color_stop(rgba8(0xd0c0ff, 0.035), 1.0),
        ))
        .border_1()
        .border_color(glass_border())
        .rounded(px(RADIUS_PANEL))
        .shadow(vec![BoxShadow {
            color: black(0.28),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(24.0),
            spread_radius: px(-6.0),
            inset: false,
        }])
}

/// Floating surface used by menus, popovers and toasts.
pub fn popup() -> Div {
    div()
        .bg(popup_fill())
        .border_1()
        .border_color(rgba8(0xe6dcff, 0.14))
        .rounded(px(RADIUS_CARD))
        .shadow(vec![
            BoxShadow {
                color: black(0.5),
                offset: point(px(0.0), px(16.0)),
                blur_radius: px(40.0),
                spread_radius: px(-8.0),
                inset: false,
            },
            BoxShadow {
                color: accent_alpha(0.10),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(24.0),
                spread_radius: px(0.0),
                inset: false,
            },
        ])
}

/// A soft glow blob. Stacked behind the panels to give the glass something to refract.
pub fn glow(color: Hsla, size: f32) -> Div {
    div()
        .absolute()
        .size(px(size))
        .rounded_full()
        .shadow(vec![BoxShadow {
            color,
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(size * 1.4),
            spread_radius: px(size * 0.35),
            inset: false,
        }])
}

/// Gradient used for hero headers, derived from a seed so every page gets its own tint.
pub fn hero_gradient(seed: u64) -> Background {
    let hue = 0.66 + ((seed % 1000) as f32 / 1000.0) * 0.22;
    let top = hsla(hue.fract(), 0.62, 0.42, 0.55);
    let bottom = hsla(hue.fract(), 0.5, 0.2, 0.0);
    linear_gradient(
        180.0,
        linear_color_stop(top, 0.0),
        linear_color_stop(bottom, 1.0),
    )
}

/// Stable tint for placeholder artwork.
pub fn placeholder_tint(seed: u64) -> Hsla {
    let hue = 0.68 + ((seed % 997) as f32 / 997.0) * 0.2;
    hsla(hue.fract(), 0.45, 0.32, 1.0)
}

pub fn seed_of(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
