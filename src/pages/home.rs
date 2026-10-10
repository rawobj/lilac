//! Home: quick picks and shelves built from the user's listening history.

use gpui_kit::*;

use super::{album_card, artist_card, playlist_card, scroll_page, show_card};
use crate::actions;
use crate::app;
use crate::model::*;
use crate::nav::Route;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::theme;
use crate::ui::card::shelf;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder, icons};

pub struct HomePage {
    recent: Vec<Track>,
    top_artists: Vec<Artist>,
    top_tracks: Vec<Track>,
    new_releases: Vec<Album>,
    loaded: bool,
    scroll: ScrollHandle,
}

impl HomePage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();
        cx.subscribe(&app::player(cx), |_, _, _: &PlayerEvent, cx| cx.notify())
            .detach();

        let mut this = Self {
            recent: Vec::new(),
            top_artists: Vec::new(),
            top_tracks: Vec::new(),
            new_releases: Vec::new(),
            loaded: false,
            scroll: ScrollHandle::new(),
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        macro_rules! fetch {
            ($field:ident, $call:expr) => {{
                let api = app::api(cx);
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(async move { $call(api).await }).await;
                    this.update(cx, |this, cx| {
                        // A shelf that fails to load is simply left out.
                        if let Ok(items) = result {
                            this.$field = items;
                        }
                        this.loaded = true;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }};
        }
        fetch!(recent, |api: crate::spotify::api::Api| async move {
            api.recently_played().await
        });
        fetch!(top_artists, |api: crate::spotify::api::Api| async move {
            api.top_artists("short_term", 12).await
        });
        fetch!(top_tracks, |api: crate::spotify::api::Api| async move {
            api.top_tracks("short_term", 8).await
        });
        fetch!(new_releases, |api: crate::spotify::api::Api| async move {
            api.new_releases().await
        });
    }

    /// The grid of quick links at the top, like the official home screen.
    fn quick_picks(&self, window: &Window, cx: &mut App) -> Div {
        enum Pick {
            Liked,
            Playlist(Playlist),
            Album(Album),
        }
        let mut picks = vec![Pick::Liked];
        {
            let library = app::library(cx);
            let library = library.read(cx);
            picks.extend(
                library
                    .playlists
                    .iter()
                    .take(5)
                    .cloned()
                    .map(Pick::Playlist),
            );
            picks.extend(library.albums.iter().take(2).cloned().map(Pick::Album));
        }
        picks.truncate(8);

        let columns = if crate::views::layout(window, cx).main >= 860.0 {
            4
        } else {
            2
        };
        let mut grid = div().px_6().grid().grid_cols(columns).gap_2();
        for (ix, pick) in picks.into_iter().enumerate() {
            let (title, art, route): (SharedString, AnyElement, Route) = match &pick {
                Pick::Liked => (
                    "Liked Songs".into(),
                    div()
                        .flex_none()
                        .size(px(56.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(linear_gradient(
                            135.0,
                            linear_color_stop(theme::accent_strong(), 0.0),
                            linear_color_stop(theme::pink(), 1.0),
                        ))
                        .child(
                            svg()
                                .path(icons::HEART_FILL)
                                .size(px(22.0))
                                .text_color(theme::white(0.95)),
                        )
                        .into_any_element(),
                    Route::Liked,
                ),
                Pick::Playlist(p) => (
                    p.name.clone(),
                    ui::cover(
                        p.images.for_size(56),
                        56.0,
                        0.0,
                        Placeholder::Playlist,
                        window,
                        cx,
                    )
                    .into_any_element(),
                    Route::Playlist(p.id.clone()),
                ),
                Pick::Album(a) => (
                    a.name.clone(),
                    ui::cover(
                        a.images.for_size(56),
                        56.0,
                        0.0,
                        Placeholder::Music,
                        window,
                        cx,
                    )
                    .into_any_element(),
                    Route::Album(a.id.clone()),
                ),
            };
            grid = grid.child(
                div()
                    .id(("pick", ix))
                    .group("pick")
                    .h(px(56.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .pr_2()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .overflow_hidden()
                    .cursor_pointer()
                    .bg(theme::glass_fill_strong())
                    .hover(|s| s.bg(theme::active_fill()))
                    .child(art)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .opacity(0.0)
                            .group_hover("pick", |s| s.opacity(1.0))
                            .child(ui::play_button("play", 34.0, false).on_click(
                                move |_, _, cx| {
                                    cx.stop_propagation();
                                    match &pick {
                                        Pick::Liked => actions::play_liked(cx),
                                        Pick::Playlist(p) => {
                                            actions::play_playlist(p.id.clone(), p.name.clone(), cx)
                                        }
                                        Pick::Album(a) => actions::play_album(a.id.clone(), cx),
                                    }
                                },
                            )),
                    )
                    .on_click(move |_, _, cx| app::navigate(route.clone(), cx)),
            );
        }
        grid
    }
}

fn greeting() -> &'static str {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let offset = local_utc_offset_secs();
    let hour = ((secs as i64 + offset).rem_euclid(86_400)) / 3600;
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        _ => "Good evening",
    }
}

/// Offset of local time from UTC in seconds.
///
/// Asking `date` once is cheaper than shipping a timezone database for a greeting.
fn local_utc_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| parse_utc_offset(s.trim()))
            .unwrap_or(0)
    })
}

/// Parse `+HHMM` / `-HHMM`.
pub(crate) fn parse_utc_offset(s: &str) -> Option<i64> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

impl Render for HomePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div().pb_8().flex().flex_col().gap_6().child(
            div()
                .relative()
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(220.0))
                        .bg(theme::hero_gradient(133)),
                )
                .child(
                    div()
                        .relative()
                        .pt_6()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .px_6()
                                .text_3xl()
                                .font_weight(FontWeight::BOLD)
                                .child(greeting()),
                        )
                        .child(self.quick_picks(window, cx)),
                ),
        );

        if !self.top_tracks.is_empty() {
            let mut rows = div().flex().flex_col();
            for (ix, track) in self.top_tracks.iter().take(5).enumerate() {
                let tracks = self.top_tracks.clone();
                rows = rows.child(track_row(
                    ("top", ix),
                    track,
                    RowOptions {
                        number: Some(ix as u32 + 1),
                        show_cover: true,
                        show_album: true,
                        ..Default::default()
                    },
                    move |_, cx| {
                        let tracks = tracks.clone();
                        app::player(cx).update(cx, |player, cx| {
                            player.play_context(
                                Some(ContextUri::Tracks("top-tracks".into())),
                                "Your top songs",
                                tracks,
                                ix,
                                cx,
                            )
                        });
                    },
                    window,
                    cx,
                ));
            }
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().px_6().child(ui::section_title("On repeat")))
                    .child(rows),
            );
        }

        if !self.recent.is_empty() {
            // Recently played tracks are grouped into their albums for the shelf.
            let mut seen = std::collections::HashSet::new();
            let cards: Vec<AnyElement> = self
                .recent
                .iter()
                .filter_map(|t| t.album.as_ref())
                .filter(|a| !a.id.is_empty() && seen.insert(a.id.clone()))
                .take(12)
                .enumerate()
                .map(|(ix, a)| {
                    let album = Album {
                        id: a.id.clone(),
                        name: a.name.clone(),
                        images: a.images.clone(),
                        ..Default::default()
                    };
                    album_card(ix, &album, Some("Album".into()), window, cx)
                })
                .collect();
            body = body.child(shelf("recent", "Recently played", cards));
        }

        if !self.top_artists.is_empty() {
            let cards = self
                .top_artists
                .iter()
                .enumerate()
                .map(|(ix, a)| artist_card(ix, a, window, cx))
                .collect();
            body = body.child(shelf("top-artists", "Your top artists this month", cards));
        }

        let (playlists, shows) = {
            let library = app::library(cx);
            let library = library.read(cx);
            (
                library
                    .playlists
                    .iter()
                    .take(12)
                    .cloned()
                    .collect::<Vec<_>>(),
                library.shows.iter().take(12).cloned().collect::<Vec<_>>(),
            )
        };
        if !playlists.is_empty() {
            let cards = playlists
                .iter()
                .enumerate()
                .map(|(ix, p)| playlist_card(ix, p, window, cx))
                .collect();
            body = body.child(shelf("playlists", "Your playlists", cards));
        }
        if !self.new_releases.is_empty() {
            let cards = self
                .new_releases
                .iter()
                .enumerate()
                .map(|(ix, a)| album_card(ix, a, None, window, cx))
                .collect();
            body = body.child(shelf("new", "New releases", cards));
        }
        if !shows.is_empty() {
            let cards = shows
                .iter()
                .enumerate()
                .map(|(ix, s)| show_card(ix, s, window, cx))
                .collect();
            body = body.child(shelf("shows", "Your shows", cards));
        }

        if !self.loaded {
            body = body.child(ui::loading_state());
        }

        scroll_page("home", &self.scroll, body)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_utc_offset;

    #[test]
    fn utc_offsets() {
        assert_eq!(parse_utc_offset("+0545"), Some(5 * 3600 + 45 * 60));
        assert_eq!(parse_utc_offset("-0800"), Some(-8 * 3600));
        assert_eq!(parse_utc_offset("0800"), None);
        assert_eq!(parse_utc_offset("+08"), None);
    }
}
