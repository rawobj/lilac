//! Small stateless building blocks shared by the views.

pub mod card;
pub mod icons;
pub mod slider;
pub mod track_row;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::images::ImageCache;
use crate::theme;

/// Attach a plain text tooltip.
pub trait WithTooltip: Sized {
    fn tip(self, text: &'static str) -> Self;
}

impl WithTooltip for Stateful<Div> {
    fn tip(self, text: &'static str) -> Self {
        self.tooltip(move |window, cx| Tooltip::new(text).build(window, cx))
    }
}

/// Round, borderless icon button.
pub fn icon_button(id: impl Into<ElementId>, path: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(32.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .text_color(theme::text_muted())
        .hover(|s| s.bg(theme::hover_fill()).text_color(theme::text()))
        .active(|s| s.bg(theme::active_fill()))
        .child(
            svg()
                .path(path)
                .size(px(18.0))
                .text_color(theme::text_muted()),
        )
}

/// Icon button that lights up in the accent color when `on`.
pub fn toggle_icon_button(id: impl Into<ElementId>, path: &'static str, on: bool) -> Stateful<Div> {
    let color = if on {
        theme::accent()
    } else {
        theme::text_muted()
    };
    div()
        .id(id)
        .flex_none()
        .size(px(32.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover_fill()))
        .active(|s| s.bg(theme::active_fill()))
        .child(svg().path(path).size(px(18.0)).text_color(color))
}

/// The filled accent play button used on headers and cards.
pub fn play_button(id: impl Into<ElementId>, size: f32, playing: bool) -> Stateful<Div> {
    let glyph = if playing { icons::PAUSE } else { icons::PLAY };
    div()
        .id(id)
        .flex_none()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(linear_gradient(
            160.0,
            linear_color_stop(theme::accent_soft(), 0.0),
            linear_color_stop(theme::accent_strong(), 1.0),
        ))
        .shadow(vec![BoxShadow {
            color: theme::accent_alpha(0.45),
            offset: point(px(0.0), px(6.0)),
            blur_radius: px(18.0),
            spread_radius: px(-4.0),
            inset: false,
        }])
        .hover(|s| s.opacity(0.92))
        .active(|s| s.opacity(0.8))
        .when(!playing, |button| {
            // Get the device ready while the button is still held down.
            button.on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| {
                let player = crate::app::player(cx);
                player.update(cx, |p, _| p.prepare_to_play());
            })
        })
        .child(
            svg()
                .path(glyph)
                .size(px(size * 0.42))
                .text_color(theme::backdrop_opaque()),
        )
}

/// Rounded text button. `primary` fills it with the accent.
pub fn pill_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex_none()
        .h(px(32.0))
        .px_4()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded_full()
        .cursor_pointer()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .child(label.into());
    if primary {
        base.bg(theme::accent())
            .text_color(theme::backdrop_opaque())
            .hover(|s| s.bg(theme::accent_soft()))
            .active(|s| s.opacity(0.85))
    } else {
        base.border_1()
            .border_color(theme::glass_border())
            .bg(theme::glass_fill())
            .text_color(theme::text())
            .hover(|s| s.bg(theme::hover_fill()))
            .active(|s| s.bg(theme::active_fill()))
    }
}

/// Filter chip as used above the library and search results.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex_none()
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(label.into());
    if selected {
        base.bg(theme::accent())
            .text_color(theme::backdrop_opaque())
    } else {
        base.bg(theme::glass_fill_strong())
            .text_color(theme::text())
            .hover(|s| s.bg(theme::active_fill()))
    }
}

/// What to draw while artwork is missing or still loading.
#[derive(Clone, Copy)]
pub enum Placeholder {
    Music,
    Artist,
    Playlist,
    Podcast,
    User,
}

impl Placeholder {
    fn icon(self) -> &'static str {
        match self {
            Placeholder::Music => icons::MUSIC,
            Placeholder::Artist => icons::USER,
            Placeholder::Playlist => icons::LIST_MUSIC,
            Placeholder::Podcast => icons::PODCAST,
            Placeholder::User => icons::USER,
        }
    }
}

/// Square artwork with a tinted placeholder.
pub fn cover(
    url: Option<SharedString>,
    size: f32,
    radius: f32,
    placeholder: Placeholder,
    window: &Window,
    cx: &mut App,
) -> Div {
    let edge = (size * window.scale_factor()).ceil() as u32;
    let image = url.as_ref().and_then(|url| ImageCache::get(url, edge, cx));
    let seed = url.as_ref().map(|u| theme::seed_of(u)).unwrap_or(7);

    let frame = div()
        .flex_none()
        .size(px(size))
        .rounded(px(radius))
        .overflow_hidden()
        .bg(theme::placeholder_tint(seed).opacity(0.35));

    match image {
        Some(image) => frame.child(
            img(ImageSource::Render(image))
                .size(px(size))
                .rounded(px(radius))
                .object_fit(ObjectFit::Cover),
        ),
        None => frame.flex().items_center().justify_center().child(
            svg()
                .path(placeholder.icon())
                .size(px((size * 0.4).clamp(12.0, 64.0)))
                .text_color(theme::white(0.35)),
        ),
    }
}

/// Small "E" badge for explicit tracks.
pub fn explicit_badge() -> Div {
    div()
        .flex_none()
        .size(px(15.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .bg(theme::white(0.22))
        .text_color(theme::backdrop_opaque())
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .child("E")
}

pub fn section_title(text: impl Into<SharedString>) -> Div {
    div()
        .text_xl()
        .font_weight(FontWeight::BOLD)
        .text_color(theme::text())
        .child(text.into())
}

/// Centered message for empty and error states.
pub fn empty_state(
    icon_path: &'static str,
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .py_16()
        .child(
            svg()
                .path(icon_path)
                .size(px(40.0))
                .text_color(theme::text_subtle()),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.into()),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(detail.into()),
        )
}

/// Indeterminate spinner.
pub fn spinner(size: f32) -> impl IntoElement {
    svg()
        .path(icons::LOADER)
        .size(px(size))
        .text_color(theme::accent())
        .with_animation(
            "spin",
            Animation::new(std::time::Duration::from_millis(900)).repeat(),
            |this, delta| this.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

pub fn loading_state() -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .py_16()
        .child(spinner(28.0))
}

/// A vertical scrollbar over the right edge of a scrolling area, always
/// visible so it is clear there is more. Put it in a `relative` parent next
/// to the scrolling element.
pub fn vertical_scrollbar<H>(handle: &H) -> Div
where
    H: gpui_kit::component::scroll::ScrollbarHandle + Clone,
{
    use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(px(12.0))
        // Faint, and only while the pointer is over the scrolling section
        // (whose parent carries the "scrollable" group).
        .opacity(0.55)
        .invisible()
        .group_hover("scrollable", |s| s.visible())
        .child(Scrollbar::vertical(handle).mode(ScrollbarMode::Always))
}

/// The horizontal counterpart of [`vertical_scrollbar`], along the bottom.
pub fn horizontal_scrollbar<H>(handle: &H) -> Div
where
    H: gpui_kit::component::scroll::ScrollbarHandle + Clone,
{
    use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(px(10.0))
        .opacity(0.55)
        .invisible()
        .group_hover("scrollable", |s| s.visible())
        .child(Scrollbar::horizontal(handle).mode(ScrollbarMode::Always))
}
