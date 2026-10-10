//! The signed-in user's profile.

use gpui_kit::*;

use super::{artist_card, playlist_card, scroll_page};
use crate::app;
use crate::model::*;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::theme;
use crate::ui::card::shelf;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder};

pub struct ProfilePage {
    top_artists: Vec<Artist>,
    top_tracks: Vec<Track>,
    scroll: ScrollHandle,
}

impl ProfilePage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.subscribe(&app::player(cx), |_, _, _: &PlayerEvent, cx| cx.notify())
            .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();

        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let (a, b) = (api.clone(), api.clone());
            let artists = runtime::spawn(async move { a.top_artists("medium_term", 10).await });
            let tracks = runtime::spawn(async move { b.top_tracks("medium_term", 10).await });
            let (artists, tracks) = (artists.await, tracks.await);
            this.update(cx, |this, cx| {
                if let Ok(Ok(artists)) = artists {
                    this.top_artists = artists;
                }
                if let Ok(Ok(tracks)) = tracks {
                    this.top_tracks = tracks;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        Self {
            top_artists: Vec::new(),
            top_tracks: Vec::new(),
            scroll: ScrollHandle::new(),
        }
    }
}

impl Render for ProfilePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(user) = app::session(cx).read(cx).user.clone() else {
            return ui::loading_state();
        };
        let (own_playlists, following) = {
            let library = app::library(cx);
            let library = library.read(cx);
            let own: Vec<Playlist> = library
                .playlists
                .iter()
                .filter(|p| p.owner.id == user.id)
                .cloned()
                .collect();
            (own, library.artists.len())
        };

        let hero = div()
            .relative()
            .h(px(280.0))
            .child(
                div()
                    .absolute()
                    .size_full()
                    .bg(theme::hero_gradient(theme::seed_of(&user.id))),
            )
            .child(
                div()
                    .relative()
                    .size_full()
                    .px_6()
                    .pb_6()
                    .flex()
                    .items_end()
                    .gap_6()
                    .child(ui::cover(
                        user.images.for_size(184),
                        184.0,
                        92.0,
                        Placeholder::User,
                        window,
                        cx,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_xs().child("Profile"))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(52.0))
                                    .line_height(px(60.0))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .child(user.name()),
                            )
                            .child(div().text_color(theme::text_muted()).child(format!(
                                "{} playlists · {} following",
                                own_playlists.len(),
                                following
                            ))),
                    ),
            );

        let mut body = div().pb_8().flex().flex_col().gap_6().child(hero);

        if !self.top_artists.is_empty() {
            let cards = self
                .top_artists
                .iter()
                .enumerate()
                .map(|(ix, a)| artist_card(ix, a, window, cx))
                .collect();
            body = body.child(shelf("artists", "Top artists", cards));
        }
        if !self.top_tracks.is_empty() {
            let mut rows = div().flex().flex_col();
            for (ix, track) in self.top_tracks.iter().enumerate() {
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
                                Some(ContextUri::Tracks("profile".into())),
                                "Top tracks",
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
                    .child(div().px_6().child(ui::section_title("Top tracks")))
                    .child(rows),
            );
        }
        if !own_playlists.is_empty() {
            let cards = own_playlists
                .iter()
                .enumerate()
                .map(|(ix, p)| playlist_card(ix, p, window, cx))
                .collect();
            body = body.child(shelf("playlists", "Playlists", cards));
        }

        scroll_page("profile", &self.scroll, body)
    }
}
