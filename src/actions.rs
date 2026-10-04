//! One-shot operations triggered from cards, menus and shortcuts.

use gpui_kit::*;

use crate::app;
use crate::model::*;
use crate::runtime;

/// Fetch an album and start playing it.
pub fn play_album(id: SharedString, cx: &mut App) {
    let api = app::api(cx);
    cx.spawn(async move |cx| {
        let result = runtime::run(async move {
            let album = api.album(&id).await?;
            let tracks = api.album_tracks(&album).await?;
            Ok((album, tracks))
        })
        .await;
        cx.update(|cx| match result {
            Ok((album, tracks)) => app::player(cx).update(cx, |player, cx| {
                player.play_context(
                    Some(ContextUri::Album(album.id.clone())),
                    album.name.clone(),
                    tracks,
                    0,
                    cx,
                )
            }),
            Err(err) => app::toast_error(format!("Could not play album: {err}"), cx),
        });
    })
    .detach();
}

/// Fetch a playlist (first 100 items) and start playing it.
pub fn play_playlist(id: SharedString, name: SharedString, cx: &mut App) {
    let api = app::api(cx);
    let fetch_id = id.clone();
    cx.spawn(async move |cx| {
        let result = runtime::run(async move { api.playlist_items(&fetch_id, 0).await }).await;
        cx.update(|cx| match result {
            Ok(page) => {
                let tracks: Vec<Track> = page.items.into_iter().filter_map(|i| i.item).collect();
                app::player(cx).update(cx, |player, cx| {
                    player.play_context(Some(ContextUri::Playlist(id)), name, tracks, 0, cx)
                });
            }
            Err(err) => app::toast_error(format!("Could not play playlist: {err}"), cx),
        });
    })
    .detach();
}

/// Play an artist's popular tracks.
pub fn play_artist(id: SharedString, name: SharedString, cx: &mut App) {
    let api = app::api(cx);
    let (fetch_id, fetch_name) = (id.clone(), name.clone());
    cx.spawn(async move |cx| {
        let result =
            runtime::run(async move { api.artist_top_tracks(&fetch_id, &fetch_name).await }).await;
        cx.update(|cx| match result {
            Ok(tracks) => app::player(cx).update(cx, |player, cx| {
                player.play_context(Some(ContextUri::Artist(id)), name, tracks, 0, cx)
            }),
            Err(err) => app::toast_error(format!("Could not play artist: {err}"), cx),
        });
    })
    .detach();
}

/// Play the user's liked songs from the top.
pub fn play_liked(cx: &mut App) {
    let tracks = app::library(cx).read(cx).liked.clone();
    app::player(cx).update(cx, |player, cx| {
        player.play_context(Some(ContextUri::Liked), "Liked Songs", tracks, 0, cx)
    });
}
