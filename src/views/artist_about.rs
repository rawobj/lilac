//! "About the artist", in full: the popup an artist page's About card opens.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::images::ImageCache;
use crate::model::format_count;
use crate::panels::{concert_row, link_pills, paragraphs};
use crate::{app, theme, ui};

pub fn render(id: &SharedString, cx: &mut App) -> AnyElement {
    let details = app::artists(cx)
        .read(cx)
        .get(id)
        .cloned()
        .unwrap_or_default();
    let Some(artist) = details.profile.clone() else {
        return ui::loading_state().into_any_element();
    };
    let overview = details.overview.clone().unwrap_or_default();
    let bio = overview
        .biography
        .clone()
        .or_else(|| details.extras.as_ref().and_then(|e| e.biography.clone()));
    let picture = overview
        .gallery
        .first()
        .cloned()
        .or(overview.header_image.clone())
        .map(SharedString::from)
        .or_else(|| artist.images.for_size(640));
    let blurred = picture
        .as_ref()
        .and_then(|url| ImageCache::backdrop(url, cx));
    let image = picture.and_then(|url| ImageCache::get(&url, 1100, cx));

    let stat = |value: String, label: &'static str| {
        div()
            .flex()
            .flex_col()
            .child(div().text_xl().font_weight(FontWeight::BOLD).child(value))
            .child(div().text_xs().text_color(theme::text_muted()).child(label))
    };
    let mut stats = div().flex().flex_wrap().gap_8();
    if let Some(n) = overview.monthly_listeners {
        stats = stats.child(stat(format_count(n), "Monthly listeners"));
    }
    if let Some(n) = overview.followers.or(artist
        .followers
        .as_ref()
        .map(|f| f.total)
        .filter(|n| *n > 0))
    {
        stats = stats.child(stat(format_count(n), "Followers"));
    }
    if let Some(rank) = overview.world_rank {
        stats = stats.child(stat(format!("#{rank}"), "In the world"));
    }

    let section = |title: &'static str| div().pt_2().font_weight(FontWeight::BOLD).child(title);

    div()
        .flex()
        .flex_col()
        .gap_4()
        .children(image.map(|image| {
            div()
                .relative()
                .h(px(320.0))
                .rounded(px(theme::RADIUS_CARD))
                .overflow_hidden()
                .children(blurred.map(|b| {
                    img(ImageSource::Render(b.image))
                        .absolute()
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                }))
                .child(
                    img(ImageSource::Render(image))
                        .absolute()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
        }))
        .child(
            div()
                .text_2xl()
                .font_weight(FontWeight::EXTRA_BOLD)
                .child(artist.name.clone()),
        )
        .child(stats)
        .when(!artist.genres.is_empty(), |d| {
            d.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(artist.genres.iter().map(|g| {
                        div()
                            .px_3()
                            .py_1()
                            .rounded_full()
                            .bg(theme::white(0.08))
                            .text_xs()
                            .child(g.clone())
                    })),
            )
        })
        .children(bio.map(|bio| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .text_color(theme::text_muted())
                .children(paragraphs(&bio).into_iter().map(|p| div().child(p)))
        }))
        .when(!overview.top_cities.is_empty(), |d| {
            d.child(section("Where people listen")).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(overview.top_cities.iter().take(5).map(
                        |(city, country, listeners)| {
                            div()
                                .flex()
                                .justify_between()
                                .gap_4()
                                .text_sm()
                                .child(div().truncate().child(if country.is_empty() {
                                    city.clone()
                                } else {
                                    format!("{city}, {country}")
                                }))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(theme::text_muted())
                                        .child(format!("{} listeners", format_count(*listeners))),
                                )
                        },
                    )),
            )
        })
        .when(!overview.concerts.is_empty(), |d| {
            d.child(section("On tour")).child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(overview.concerts.iter().take(8).map(concert_row)),
            )
        })
        .when(!overview.links.is_empty(), |d| {
            d.child(section("Links"))
                .child(link_pills("about-dialog", &overview.links))
        })
        .children(details.overview_error.clone().map(|why| {
            let id = id.clone();
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_subtle())
                        .child(format!(
                            "Listener numbers, links and tour dates are unavailable: {why}"
                        )),
                )
                .child(ui::pill_button("about-retry", "Try again", false).on_click(
                    move |_, _, cx| app::artists(cx).update(cx, |s, cx| s.retry_overview(&id, cx)),
                ))
        }))
        .into_any_element()
}
