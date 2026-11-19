//! Home: quick picks and shelves built from the user's listening history.

use gpui_kit::*;

use super::{album_card, artist_card, playlist_card, scroll_page, show_card};
use crate::actions;
use crate::app;
use crate::disk_cache;
use crate::history;
use crate::model::*;
use crate::nav::Route;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::spotify::streaming;
use crate::theme;
use crate::ui::card::shelf;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder, icons};

/// Home's shelves are asked of Spotify again only after this.
const HOME_FRESH: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const HOME_KEY: &str = "home";

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
        cx.subscribe(&app::player(cx), |_, _, event: &PlayerEvent, cx| {
            // Rows show what plays and whether it is paused, not where in the track.
            if !matches!(event, PlayerEvent::Seeked) {
                cx.notify()
            }
        })
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

    /// Show the shelves as last seen at once, and ask Spotify for new ones
    /// only when those are older than [`HOME_FRESH`]: every shelf is a Web
    /// API request, and Home opens often.
    fn load(&mut self, cx: &mut Context<Self>) {
        let demo = app::api(cx).is_demo();
        cx.spawn(async move |this, cx| {
            let kept = if demo {
                None
            } else {
                cx.background_executor()
                    .spawn(async move { disk_cache::HOME.get(HOME_KEY) })
                    .await
            };
            let fresh = kept.as_ref().is_some_and(|hit| hit.age < HOME_FRESH);
            this.update(cx, |this, cx| {
                if let Some(hit) = kept {
                    this.apply_kept(&hit.value);
                    cx.notify();
                }
                // Lilac's own history is always current and costs nothing.
                let local = if demo {
                    Vec::new()
                } else {
                    history::recent(20)
                };
                if !local.is_empty() {
                    this.recent = local;
                    this.loaded = true;
                    cx.notify();
                }
                if !fresh {
                    this.fetch(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_kept(&mut self, value: &serde_json::Value) {
        fn list<T: serde::de::DeserializeOwned>(value: &serde_json::Value, key: &str) -> Vec<T> {
            serde_json::from_value(value.get(key).cloned().unwrap_or_default()).unwrap_or_default()
        }
        self.recent = list(value, "recent");
        self.top_artists = list(value, "top_artists");
        self.top_tracks = list(value, "top_tracks");
        self.new_releases = list(value, "new_releases");
        self.loaded = true;
    }

    /// Every shelf asks Spotify's Web API first and keeps what it had when
    /// that fails, with Lilac's own data standing in where nothing was
    /// kept: its play history (recently played, most played songs and
    /// artists) and the followed artists' releases through the streaming
    /// session. No shelf is left empty for want of the Web API.
    fn fetch(&mut self, cx: &mut Context<Self>) {
        let api = app::api(cx);
        let demo = api.is_demo();
        let followed: Vec<String> = app::library(cx)
            .read(cx)
            .artists
            .iter()
            .map(|a| a.id.to_string())
            .collect();

        // Recently played: Lilac's own history is on screen already (see
        // `load`); Spotify's, which includes other devices, replaces it when
        // it can be had.
        let recent_api = api.clone();
        self.fetch_shelf(
            cx,
            async move { recent_api.recently_played().await },
            |this, items| {
                if !items.is_empty() {
                    this.recent = items;
                }
            },
        );

        // New releases: Spotify's; when the Web API fails, the followed
        // artists' releases through the streaming session.
        let releases_api = api.clone();
        self.fetch_shelf(
            cx,
            async move {
                match releases_api.new_releases().await {
                    Ok(albums) => Ok(albums),
                    Err(err) if demo || followed.is_empty() => Err(err),
                    Err(_) => {
                        let mut albums = streaming::latest_releases(&followed).await?;
                        albums.sort_by(|a, b| b.release_date.cmp(&a.release_date));
                        albums.truncate(20);
                        Ok(albums)
                    }
                }
            },
            |this, items| this.new_releases = items,
        );

        // Listening stats: the Web API, else Lilac's own history.
        let stats = api.clone();
        cx.spawn(async move |this, cx| {
            let (artists, tracks) = runtime::run(async move {
                Ok(tokio::join!(
                    stats.top_artists("short_term", 12),
                    stats.top_tracks("short_term", 8)
                ))
            })
            .await
            .unwrap_or_else(|err| (Err(anyhow::anyhow!("{err}")), Err(anyhow::anyhow!("{err}"))));
            let artists_failed = artists.is_err();
            this.update(cx, |this, cx| {
                if let Ok(items) = artists {
                    this.top_artists = items;
                }
                match tracks {
                    Ok(items) => this.top_tracks = items,
                    Err(_) if this.top_tracks.is_empty() && !demo => {
                        let local = history::top_tracks(28, 8);
                        this.top_tracks = if local.is_empty() {
                            app::library(cx)
                                .read(cx)
                                .liked
                                .iter()
                                .take(8)
                                .cloned()
                                .collect()
                        } else {
                            local
                        };
                    }
                    Err(_) => {}
                }
                this.loaded = true;
                this.persist(cx);
                cx.notify();
                if artists_failed && this.top_artists.is_empty() && !demo {
                    this.local_top_artists(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Top artists from Lilac's play history (pictures through the
    /// streaming session), else the followed artists.
    fn local_top_artists(&mut self, cx: &mut Context<Self>) {
        let ranked = history::top_artists(28, 12);
        if ranked.is_empty() {
            self.top_artists = app::library(cx)
                .read(cx)
                .artists
                .iter()
                .take(12)
                .cloned()
                .collect();
            cx.notify();
            return;
        }
        let uris: Vec<String> = ranked
            .iter()
            .map(|a| format!("spotify:artist:{}", a.id))
            .collect();
        cx.spawn(async move |this, cx| {
            let found = runtime::run(async move { streaming::artists_by_uri(&uris).await }).await;
            this.update(cx, |this, cx| {
                this.top_artists = match found {
                    Ok(artists) if !artists.is_empty() => artists,
                    _ => ranked
                        .into_iter()
                        .map(|a| Artist {
                            id: a.id.clone(),
                            uri: format!("spotify:artist:{}", a.id).into(),
                            name: a.name,
                            ..Default::default()
                        })
                        .collect(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Load one shelf; one that fails keeps what was kept (or is left out).
    fn fetch_shelf<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        call: impl std::future::Future<Output = anyhow::Result<Vec<T>>> + Send + 'static,
        apply: impl FnOnce(&mut Self, Vec<T>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = runtime::run(call).await;
            this.update(cx, |this, cx| {
                if let Ok(items) = result {
                    apply(this, items);
                    this.persist(cx);
                }
                this.loaded = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn persist(&self, cx: &mut Context<Self>) {
        if app::api(cx).is_demo() {
            return;
        }
        let value = serde_json::json!({
            "recent": self.recent.iter().map(Track::to_api_json).collect::<Vec<_>>(),
            "top_artists": self.top_artists.iter().map(Artist::to_api_json).collect::<Vec<_>>(),
            "top_tracks": self.top_tracks.iter().map(Track::to_api_json).collect::<Vec<_>>(),
            "new_releases": self.new_releases.iter().map(Album::to_api_json).collect::<Vec<_>>(),
        });
        cx.background_executor()
            .spawn(async move { disk_cache::HOME.put(HOME_KEY, &value) })
            .detach();
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
