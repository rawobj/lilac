//! Artwork cards and the horizontal shelves they sit in.

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
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

/// Handler for a card's hover play button.
pub type PlayHandler = Box<dyn Fn(&mut Window, &mut App)>;

/// A clickable card. `on_play` adds the hover play button.
pub fn card(
    id: impl Into<ElementId>,
    card: Card,
    on_open: impl Fn(&mut Window, &mut App) + 'static,
    on_play: Option<PlayHandler>,
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
) -> Shelf {
    Shelf {
        id: id.into(),
        title: title.into(),
        cards,
    }
}

#[derive(IntoElement)]
pub struct Shelf {
    id: ElementId,
    title: SharedString,
    cards: Vec<AnyElement>,
}

impl RenderOnce for Shelf {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scroll = window
            .use_keyed_state(self.id.clone(), cx, |_, _| ScrollHandle::new())
            .read(cx)
            .clone();
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
                    .child(self.title),
            )
            .child(
                div()
                    .id(self.id)
                    .group("shelf")
                    .relative()
                    .child(
                        div()
                            .id("row")
                            .px_3()
                            .flex()
                            .overflow_x_scroll()
                            // Vertical wheel and swipes belong to the page, sideways ones to the shelf.
                            .restrict_scroll_to_axis()
                            .track_scroll(&scroll)
                            .on_scroll_wheel(shift_scrolls_sideways(scroll.clone()))
                            // Room for the scrollbar under the cards and
                            // their hover background (padding on the
                            // scrolling row itself is not laid out).
                            .child(div().flex().pb(px(14.0)).children(self.cards)),
                    )
                    // A thin bar while the pointer is over the shelf: shows there is
                    // more to the side and can be dragged with a mouse.
                    .child(
                        div()
                            .absolute()
                            .opacity(0.55)
                            .invisible()
                            .group_hover("shelf", |s| s.visible())
                            .left(px(theme::GAP * 3.0))
                            .right(px(theme::GAP * 3.0))
                            .bottom_0()
                            .h(px(10.0))
                            .child(
                                Scrollbar::horizontal(&scroll)
                                    .id("bar")
                                    .mode(ScrollbarMode::Always),
                            ),
                    ),
            )
    }
}

/// Shift + wheel scrolls a horizontal row, as in browsers. Wayland does not
/// translate it for us, and a mouse wheel has no other way to reach the row.
pub fn shift_scrolls_sideways(
    scroll: ScrollHandle,
) -> impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        if !event.modifiers.shift {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        if delta.y.is_zero() || !delta.x.is_zero() {
            return;
        }
        let mut offset = scroll.offset();
        let max = scroll.max_offset().x;
        offset.x = (offset.x + delta.y).clamp(-max, px(0.0));
        scroll.set_offset(offset);
        cx.stop_propagation();
        window.refresh();
    }
}
