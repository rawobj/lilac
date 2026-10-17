//! What's New: latest releases and episodes from what the user follows.

use gpui_kit::*;

use super::scroll_page;
use crate::actions;
use crate::app;
use crate::feed::FeedItem;
use crate::model::{ContextUri, format_duration};
use crate::nav::Route;
use crate::theme;
use crate::ui::{self, Placeholder, WithTooltip, icons};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Music,
    Podcasts,
}

pub struct WhatsNewPage {
    filter: Filter,
    scroll: ScrollHandle,
}

impl WhatsNewPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let feed = app::feed(cx);
        cx.observe(&feed, |_, feed, cx| {
            Self::mark_seen(&feed, cx);
            cx.notify();
        })
        .detach();
        feed.update(cx, |feed, cx| feed.ensure_loaded(cx));
        Self::mark_seen(&feed, cx);
        Self {
            filter: Filter::All,
            scroll: ScrollHandle::new(),
        }
    }

    /// Opening the page clears the bell's dot.
    fn mark_seen(feed: &Entity<crate::feed::Feed>, cx: &mut App) {
        let Some(newest) = feed.read(cx).newest_date().map(str::to_string) else {
            return;
        };
        if app::settings(cx).read(cx).whats_new_seen < newest {
            app::update_settings(cx, |s| s.whats_new_seen = newest);
        }
    }

    fn row(ix: usize, item: &FeedItem, window: &Window, cx: &mut App) -> AnyElement {
        let (title, subtitle, kind, images, placeholder) = match item {
            FeedItem::Release(album) => (
                album.name.clone(),
                album.artist_line(),
                SharedString::from(album.type_label()),
                album.images.clone(),
                Placeholder::Music,
            ),
            FeedItem::Episode(episode) => (
                episode.name.clone(),
                episode
                    .show
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
                SharedString::from(format!(
                    "Episode · {}",
                    format_duration(episode.duration_ms)
                )),
                episode.cover().clone(),
                Placeholder::Podcast,
            ),
        };
        let date = item.date().to_string();
        let open = item.clone();
        let play = item.clone();
        div()
            .id(("whats-new", ix))
            .group("row")
            .mx_4()
            .p_3()
            .flex()
            .items_center()
            .gap_4()
            .rounded_lg()
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover_fill()))
            .on_click(move |_, _, cx| match &open {
                FeedItem::Release(album) => app::navigate(Route::Album(album.id.clone()), cx),
                FeedItem::Episode(episode) => {
                    if let Some(show) = &episode.show {
                        app::navigate(Route::Show(show.id.clone()), cx)
                    }
                }
            })
            .child(ui::cover(
                images.for_size(96),
                96.0,
                8.0,
                placeholder,
                window,
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::accent_soft())
                            .child(pretty_date(&date)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(subtitle),
                    )
                    .child(div().text_xs().text_color(theme::text_subtle()).child(kind)),
            )
            .child(div().invisible().group_hover("row", |s| s.visible()).child(
                ui::play_button(("whats-new-play", ix), 40.0, false).on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    match &play {
                        FeedItem::Release(album) => actions::play_album(album.id.clone(), cx),
                        FeedItem::Episode(episode) => {
                            let episode = episode.clone();
                            app::player(cx).update(cx, |player, cx| {
                                player.play_context(
                                    Some(ContextUri::Tracks("whats-new".into())),
                                    episode.name.clone(),
                                    vec![episode],
                                    0,
                                    cx,
                                )
                            });
                        }
                    }
                }),
            ))
            .into_any_element()
    }
}

impl Render for WhatsNewPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let feed = app::feed(cx);
        let (items, loaded) = {
            let feed = feed.read(cx);
            (feed.items.clone(), feed.loaded)
        };

        let mut chips = div().px_6().flex().gap_2();
        for (filter, label) in [
            (Filter::All, "All"),
            (Filter::Music, "Music"),
            (Filter::Podcasts, "Podcasts & Shows"),
        ] {
            chips = chips.child(ui::chip(label, label, self.filter == filter).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.filter = filter;
                    cx.notify();
                }),
            ));
        }

        let header = div()
            .px_6()
            .pt_8()
            .flex()
            .items_end()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(40.0))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .child("What's New"),
                    )
                    .child(div().text_sm().text_color(theme::text_muted()).child(
                        "The latest releases from artists, podcasts, and shows you follow.",
                    )),
            )
            .child(
                ui::icon_button("whats-new-refresh", icons::REFRESH)
                    .tip("Refresh")
                    .on_click(|_, _, cx| app::feed(cx).update(cx, |feed, cx| feed.refresh(cx))),
            );

        let mut body = div()
            .pb_8()
            .flex()
            .flex_col()
            .gap_4()
            .child(header)
            .child(chips);

        let filter = self.filter;
        let shown: Vec<&FeedItem> = items
            .iter()
            .filter(|item| match filter {
                Filter::All => true,
                Filter::Music => !item.is_episode(),
                Filter::Podcasts => item.is_episode(),
            })
            .collect();

        if !loaded {
            body = body.child(ui::loading_state());
        } else if shown.is_empty() {
            body = body.child(ui::empty_state(
                icons::BELL,
                "Nothing new right now",
                "Follow artists and shows to see their latest releases here.",
            ));
        } else {
            let mut list = div().flex().flex_col();
            for (ix, item) in shown.into_iter().enumerate() {
                list = list.child(Self::row(ix, item, window, cx));
            }
            body = body.child(list);
        }

        scroll_page("whats-new", &self.scroll, body)
    }
}

/// "12 Oct 2026" from `2026-10-12`; partial dates pass through as given.
fn pretty_date(date: &str) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut parts = date.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(y), Some(m), Some(d)) => match (m.parse::<usize>(), d.parse::<u32>()) {
            (Ok(m @ 1..=12), Ok(d)) => format!("{d} {} {y}", MONTHS[m - 1]),
            _ => date.to_string(),
        },
        _ => date.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::pretty_date;

    #[test]
    fn dates() {
        assert_eq!(pretty_date("2026-10-02"), "2 Oct 2026");
        assert_eq!(pretty_date("2026"), "2026");
        assert_eq!(pretty_date("2026-13-01"), "2026-13-01");
    }
}
