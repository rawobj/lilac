//! Context menus for tracks, albums, artists and playlists.

use gpui_kit::*;

use crate::app;
use crate::library::SavedEntity;
use crate::model::*;
use crate::nav::Route;
use crate::ui::icons;
use crate::ui_state::{Dialog, MenuItem};

fn copy_link(kind: &'static str, id: &SharedString, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(format!(
        "https://open.spotify.com/{kind}/{id}"
    )));
    app::toast("Link copied to clipboard", cx);
}

fn show(position: Point<Pixels>, items: Vec<MenuItem>, cx: &mut App) {
    app::ui(cx).update(cx, |ui, cx| ui.open_menu(position, items, cx));
}

/// Where a track row lives, which decides a few of its menu entries.
#[derive(Clone, Default)]
pub struct TrackMenuContext {
    /// Set when the row belongs to a playlist the user can edit.
    pub editable_playlist: Option<SharedString>,
    /// Set for rows of the manual queue.
    pub queue_index: Option<usize>,
}

pub fn track_menu(track: &Track, context: TrackMenuContext, position: Point<Pixels>, cx: &mut App) {
    let mut items = Vec::new();
    let liked = app::library(cx).read(cx).is_liked(&track.id);

    if let Some(index) = context.queue_index {
        items.push(MenuItem::new(
            icons::TRASH,
            "Remove from queue",
            move |_, cx| {
                app::player(cx).update(cx, |player, cx| player.remove_queued(index, cx));
            },
        ));
    } else {
        let t = track.clone();
        items.push(MenuItem::new(
            icons::LIST_PLUS,
            "Add to queue",
            move |_, cx| {
                let track = t.clone();
                app::player(cx).update(cx, |player, cx| player.enqueue(track, cx));
            },
        ));
        let t = track.clone();
        items.push(MenuItem::new(icons::LIST_END, "Play next", move |_, cx| {
            let track = t.clone();
            app::player(cx).update(cx, |player, cx| player.play_next(track, cx));
        }));
    }

    if !track.is_local && !track.is_episode() {
        let t = track.clone();
        items.push(
            MenuItem::new(
                if liked {
                    icons::HEART_FILL
                } else {
                    icons::HEART
                },
                if liked {
                    "Remove from Liked Songs"
                } else {
                    "Save to Liked Songs"
                },
                move |_, cx| {
                    app::library(cx).update(cx, |library, cx| library.toggle_like(&t, cx));
                },
            )
            .separated(),
        );
        let t = track.clone();
        items.push(MenuItem::new(
            icons::PLUS,
            "Add to playlist…",
            move |_, cx| {
                add_to_playlist_menu(t.clone(), position, cx);
            },
        ));
    }

    if let Some(playlist_id) = context.editable_playlist {
        let t = track.clone();
        items.push(MenuItem::new(
            icons::TRASH,
            "Remove from this playlist",
            move |_, cx| {
                let (id, track) = (playlist_id.clone(), t.clone());
                app::library(cx).update(cx, |library, cx| {
                    library.remove_from_playlist(id, track, cx)
                });
            },
        ));
    }

    let mut first_nav = true;
    for artist in track.artists.iter().take(3).filter(|a| !a.id.is_empty()) {
        let id = artist.id.clone();
        let label = if track.artists.len() > 1 {
            format!("Go to {}", artist.name)
        } else {
            "Go to artist".to_string()
        };
        let mut item = MenuItem::new(icons::USER, label, move |_, cx| {
            app::navigate(Route::Artist(id.clone()), cx);
        });
        if first_nav {
            item = item.separated();
            first_nav = false;
        }
        items.push(item);
    }
    if let Some(album) = track.album.as_ref().filter(|a| !a.id.is_empty()) {
        let id = album.id.clone();
        let mut item = MenuItem::new(icons::DISC, "Go to album", move |_, cx| {
            app::navigate(Route::Album(id.clone()), cx);
        });
        if first_nav {
            item = item.separated();
        }
        items.push(item);
    }
    if let Some(show) = track.show.as_ref().filter(|s| !s.id.is_empty()) {
        let id = show.id.clone();
        items.push(
            MenuItem::new(icons::PODCAST, "Go to show", move |_, cx| {
                app::navigate(Route::Show(id.clone()), cx);
            })
            .separated(),
        );
    }

    if !track.is_local {
        let id = track.id.clone();
        let kind = if track.is_episode() {
            "episode"
        } else {
            "track"
        };
        items.push(
            MenuItem::new(icons::LINK, "Copy link", move |_, cx| {
                copy_link(kind, &id, cx)
            })
            .separated(),
        );
    }

    show(position, items, cx);
}

/// Second-level menu listing the playlists a track can be added to.
fn add_to_playlist_menu(track: Track, position: Point<Pixels>, cx: &mut App) {
    let user_id = app::session(cx)
        .read(cx)
        .user
        .as_ref()
        .map(|u| u.id.clone())
        .unwrap_or_default();
    let seed = track.clone();
    let mut items = vec![MenuItem::new(icons::PLUS, "New playlist", move |_, cx| {
        let dialog = Dialog::CreatePlaylist {
            seed: Some(Box::new(seed.clone())),
        };
        app::ui(cx).update(cx, |ui, cx| ui.open_dialog(dialog, cx));
    })];

    let playlists: Vec<(SharedString, SharedString)> = app::library(cx)
        .read(cx)
        .editable_playlists(&user_id)
        .take(40)
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();

    for (ix, (id, name)) in playlists.into_iter().enumerate() {
        let track = track.clone();
        let mut item = MenuItem::new(icons::LIST_MUSIC, name, move |_, cx| {
            let (id, track) = (id.clone(), track.clone());
            app::library(cx).update(cx, |library, cx| {
                library.add_to_playlist(id, vec![track], cx)
            });
        });
        if ix == 0 {
            item = item.separated();
        }
        items.push(item);
    }
    show(position, items, cx);
}

pub fn album_menu(album: &Album, position: Point<Pixels>, cx: &mut App) {
    let saved = app::library(cx).read(cx).is_saved(&album.id);
    let mut items = Vec::new();

    let a = album.clone();
    items.push(MenuItem::new(
        if saved {
            icons::CIRCLE_CHECK
        } else {
            icons::CIRCLE_PLUS
        },
        if saved {
            "Remove from Your Library"
        } else {
            "Add to Your Library"
        },
        move |_, cx| {
            let item = SavedEntity::Album(a.clone());
            app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx));
        },
    ));
    if let Some(artist) = album.artists.first().filter(|a| !a.id.is_empty()) {
        let id = artist.id.clone();
        items.push(
            MenuItem::new(icons::USER, "Go to artist", move |_, cx| {
                app::navigate(Route::Artist(id.clone()), cx);
            })
            .separated(),
        );
    }
    let id = album.id.clone();
    items.push(
        MenuItem::new(icons::LINK, "Copy link", move |_, cx| {
            copy_link("album", &id, cx)
        })
        .separated(),
    );
    show(position, items, cx);
}

pub fn artist_menu(artist: &Artist, position: Point<Pixels>, cx: &mut App) {
    let following = app::library(cx).read(cx).is_saved(&artist.id);
    let a = artist.clone();
    let id = artist.id.clone();
    let items = vec![
        MenuItem::new(
            if following {
                icons::CHECK
            } else {
                icons::USER_PLUS
            },
            if following { "Unfollow" } else { "Follow" },
            move |_, cx| {
                let item = SavedEntity::Artist(a.clone());
                app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx));
            },
        ),
        MenuItem::new(icons::LINK, "Copy link", move |_, cx| {
            copy_link("artist", &id, cx)
        })
        .separated(),
    ];
    show(position, items, cx);
}

pub fn playlist_menu(playlist: &Playlist, position: Point<Pixels>, cx: &mut App) {
    let user_id = app::session(cx)
        .read(cx)
        .user
        .as_ref()
        .map(|u| u.id.clone())
        .unwrap_or_default();
    let owned = playlist.owner.id == user_id;
    let saved = app::library(cx).read(cx).is_saved(&playlist.id);
    let mut items = Vec::new();

    if owned {
        let id = playlist.id.clone();
        items.push(MenuItem::new(
            icons::PENCIL,
            "Edit details",
            move |_, cx| {
                let dialog = Dialog::EditPlaylist { id: id.clone() };
                app::ui(cx).update(cx, |ui, cx| ui.open_dialog(dialog, cx));
            },
        ));
        let (id, name) = (playlist.id.clone(), playlist.name.clone());
        items.push(
            MenuItem::new(icons::TRASH, "Delete", move |_, cx| {
                let dialog = Dialog::DeletePlaylist {
                    id: id.clone(),
                    name: name.clone(),
                };
                app::ui(cx).update(cx, |ui, cx| ui.open_dialog(dialog, cx));
            })
            .danger(),
        );
    } else {
        let p = playlist.clone();
        items.push(MenuItem::new(
            if saved {
                icons::CIRCLE_CHECK
            } else {
                icons::CIRCLE_PLUS
            },
            if saved {
                "Remove from Your Library"
            } else {
                "Add to Your Library"
            },
            move |_, cx| {
                let item = SavedEntity::Playlist(p.clone());
                app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx));
            },
        ));
    }

    let id = playlist.id.clone();
    items.push(
        MenuItem::new(icons::LINK, "Copy link", move |_, cx| {
            copy_link("playlist", &id, cx)
        })
        .separated(),
    );
    show(position, items, cx);
}

pub fn show_menu(show_item: &Show, position: Point<Pixels>, cx: &mut App) {
    let saved = app::library(cx).read(cx).is_saved(&show_item.id);
    let s = show_item.clone();
    let id = show_item.id.clone();
    let items = vec![
        MenuItem::new(
            if saved { icons::CHECK } else { icons::PLUS },
            if saved { "Unfollow" } else { "Follow" },
            move |_, cx| {
                let item = SavedEntity::Show(s.clone());
                app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx));
            },
        ),
        MenuItem::new(icons::LINK, "Copy link", move |_, cx| {
            copy_link("show", &id, cx)
        })
        .separated(),
    ];
    show(position, items, cx);
}
