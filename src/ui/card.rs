//! Artwork cards and the horizontal shelves they sit in.

use gpui_kit::*;

use super::{Placeholder, cover, play_button};
use crate::theme;

pub const CARD_WIDTH: f32 = 176.0;
const COVER: f32 = 152.0;

pub struct Card {
    pub title: SharedString,
    pub subtitle: SharedString,
    pub image: Option<SharedString>,
    pub placeholder: Placeholder,
    /// Round artwork, used for artists and profiles.
    pub round: bool,
}

/// A clickable card. `on_play` adds the hover play button.
pub fn card(
    id: impl Into<ElementId>,
    card: Card,
    on_open: impl Fn(&mut Window, &mut App) + 'static,
    on_play: Option<Box<dyn Fn(&mut Window, &mut App)>>,
    window: &Window,
    cx: &mut App,
) -> Stateful<Div> {
    let radius = if card.round {
        COVER / 2.0
    } else {
        theme::RADIUS_CONTROL
    };
    let art = div()
        .relative()
        .size(px(COVER))
        .child(
            cover(card.image, COVER, radius, card.placeholder, window, cx).shadow(vec![
                BoxShadow {
                    color: theme::black(0.35),
                    offset: point(px(0.0), px(8.0)),
                    blur_radius: px(18.0),
                    spread_radius: px(-6.0),
                    inset: false,
                },
            ]),
        )
        .children(on_play.map(|on_play| {
            div()
                .absolute()
                .right(px(8.0))
                .bottom(px(8.0))
                .opacity(0.0)
                .group_hover("card", |s| s.opacity(1.0))
                .child(
                    play_button("play", 42.0, false).on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        on_play(window, cx);
                    }),
                )
        }));

    div()
        .id(id)
        .group("card")
        .flex_none()
        .w(px(CARD_WIDTH))
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap_2()
        .rounded(px(theme::RADIUS_CARD))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover_fill()))
        .child(art)
        .child(
            div()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(card.title),
        )
        .child(
            div()
                .h(px(32.0))
                .overflow_hidden()
                .line_clamp(2)
                .text_xs()
                .text_color(theme::text_muted())
                .child(card.subtitle),
        )
        .on_click(move |_, window, cx| on_open(window, cx))
}

/// A titled row of cards that scrolls sideways.
pub fn shelf(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    cards: Vec<AnyElement>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .px_6()
                .text_xl()
                .font_weight(FontWeight::BOLD)
                .text_color(theme::text())
                .child(title.into()),
        )
        .child(
            div()
                .id(id)
                .px_3()
                .flex()
                .overflow_x_scroll()
                .children(cards),
        )
}
