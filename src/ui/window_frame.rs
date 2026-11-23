//! Moving, resizing and the window buttons, for when Lilac draws its own frame.
//!
//! Windows never hands a borderless window a "start moving" call: the system
//! asks which part of the window is under the pointer, so drag regions and
//! buttons are marked with `window_control_area`, and anything clickable
//! inside a drag region must `occlude()` or the system takes its clicks for
//! the title bar. Linux compositors that leave decorations to the client
//! (KDE or GNOME when no server decorations are given, most of X11) need the
//! move and the resize started by hand.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme;
use crate::ui::icons;

const BUTTON: f32 = 28.0;
const BUTTON_GAP: f32 = 4.0;
const BUTTONS_WIDTH: f32 = BUTTON * 3.0 + BUTTON_GAP * 2.0;
/// Where the top bar centers them (its `px_3` padding on the right).
const CORNER_TOP: f32 = (theme::TOP_BAR_HEIGHT - BUTTON) / 2.0;
const CORNER_RIGHT: f32 = 12.0;
/// Width of the grab band along each edge.
const EDGE: f32 = 6.0;
/// Corners grab a little further in so they are easy to hit.
const CORNER: f32 = 10.0;

/// Whether the window has no system-drawn buttons, so Lilac must draw them.
/// macOS keeps its traffic lights; Windows never gets a title bar.
pub fn needs_buttons(window: &Window) -> bool {
    cfg!(windows) || matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Makes an element move the window when dragged and maximize it on a
/// double click.
pub trait DragsWindow: InteractiveElement + Sized {
    fn drags_window(self) -> Self {
        let area = self.window_control_area(WindowControlArea::Drag);
        // Windows does all of it from the hit test, and better: a double click
        // also restores, and dragging a maximized window restores it.
        // `zoom_window` there only ever maximizes, so a double click handled
        // here would undo the system's restore.
        if cfg!(windows) {
            return area;
        }
        area.on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count == 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
    }
}

impl<E: InteractiveElement> DragsWindow for E {}

/// Minimize, maximize and close, or nothing when the system draws its own.
pub fn buttons(window: &Window) -> Option<Div> {
    if !needs_buttons(window) {
        return None;
    }
    // Wayland compositors say which of these they support (on Windows and X11
    // every one is).
    let supported = window.window_controls();
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(BUTTON_GAP))
            .when(supported.minimize, |row| {
                row.child(control(
                    "minimize",
                    icons::MINUS,
                    false,
                    WindowControlArea::Min,
                    |window| window.minimize_window(),
                ))
            })
            .when(supported.maximize, |row| {
                let icon = if window.is_maximized() {
                    icons::WINDOW_RESTORE
                } else {
                    icons::SQUARE
                };
                row.child(control(
                    "maximize",
                    icon,
                    false,
                    WindowControlArea::Max,
                    |window| window.zoom_window(),
                ))
            })
            .child(control(
                "close",
                icons::X,
                true,
                WindowControlArea::Close,
                |window| window.remove_window(),
            )),
    )
}

fn control(
    id: &'static str,
    icon: &'static str,
    danger: bool,
    area: WindowControlArea,
    action: fn(&mut Window),
) -> Stateful<Div> {
    let control = button(id, icon, danger).window_control_area(area);
    // Windows presses these itself, from the hit test, unless the press is
    // taken here first. It also restores a maximized window, where
    // `zoom_window` only ever maximizes.
    if cfg!(windows) {
        return control;
    }
    control
        // A press here is a click, not the start of a window drag.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, _| action(window))
}

/// The buttons on their own in the top-right corner, where the top bar would
/// put them.
pub fn corner_buttons(window: &Window) -> Option<Div> {
    buttons(window).map(|row| row.absolute().top(px(CORNER_TOP)).right(px(CORNER_RIGHT)))
}

/// How far in from the right edge something else in the top-right corner
/// must sit to clear the buttons (zero when there are none).
pub fn corner_reserve(window: &Window) -> f32 {
    if needs_buttons(window) {
        CORNER_RIGHT + BUTTONS_WIDTH
    } else {
        0.0
    }
}

fn button(id: &'static str, icon: &'static str, danger: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .occlude()
        .hover(move |s| {
            s.bg(if danger {
                theme::danger().opacity(0.8)
            } else {
                theme::hover_fill()
            })
        })
        .child(
            svg()
                .path(icon)
                .size(px(14.0))
                .text_color(theme::text_muted()),
        )
}

/// Grab bands along the free edges of a client-decorated Linux window.
/// Windows resizes from its own invisible frame and the rest have server
/// borders, so elsewhere this is empty; so is a maximized or fullscreen
/// window, and every side the compositor has tiled.
pub fn resize_edges(window: &Window) -> Vec<Div> {
    let Decorations::Client { tiling } = window.window_decorations() else {
        return Vec::new();
    };
    if cfg!(windows) || window.is_maximized() || window.is_fullscreen() {
        return Vec::new();
    }
    let (top, bottom, left, right) = (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right);
    let edge = |resize: ResizeEdge, cursor: CursorStyle| {
        div().absolute().occlude().cursor(cursor).on_mouse_down(
            MouseButton::Left,
            move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(resize);
            },
        )
    };
    let mut bands = Vec::with_capacity(8);
    if top {
        bands.push(
            edge(ResizeEdge::Top, CursorStyle::ResizeUpDown)
                .top_0()
                .left_0()
                .right_0()
                .h(px(EDGE)),
        );
    }
    if bottom {
        bands.push(
            edge(ResizeEdge::Bottom, CursorStyle::ResizeUpDown)
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(EDGE)),
        );
    }
    if left {
        bands.push(
            edge(ResizeEdge::Left, CursorStyle::ResizeLeftRight)
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(EDGE)),
        );
    }
    if right {
        bands.push(
            edge(ResizeEdge::Right, CursorStyle::ResizeLeftRight)
                .right_0()
                .top_0()
                .bottom_0()
                .w(px(EDGE)),
        );
    }
    // Corners go last so they sit above the edges they overlap.
    let corner = |resize, cursor| edge(resize, cursor).size(px(CORNER));
    if top && left {
        bands.push(
            corner(ResizeEdge::TopLeft, CursorStyle::ResizeUpLeftDownRight)
                .top_0()
                .left_0(),
        );
    }
    if top && right {
        bands.push(
            corner(ResizeEdge::TopRight, CursorStyle::ResizeUpRightDownLeft)
                .top_0()
                .right_0(),
        );
    }
    if bottom && left {
        bands.push(
            corner(ResizeEdge::BottomLeft, CursorStyle::ResizeUpRightDownLeft)
                .bottom_0()
                .left_0(),
        );
    }
    if bottom && right {
        bands.push(
            corner(ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight)
                .bottom_0()
                .right_0(),
        );
    }
    bands
}
