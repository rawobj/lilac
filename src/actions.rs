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
            let (album, tracks) = api.album_with_tracks(&id).await?;
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
/// Start a playlist from its first page, then queue the rest as it loads.
pub fn play_playlist(id: SharedString, name: SharedString, cx: &mut App) {
    let api = app::api(cx);
    cx.spawn(async move |cx| {
        let context = ContextUri::Playlist(id.clone());
        let mut offset = 0;
        let mut queued = 0;
        loop {
            let (page_api, page_id) = (api.clone(), id.clone());
            let page =
                match runtime::run(async move { page_api.playlist_items(&page_id, offset).await })
                    .await
                {
                    Ok(page) => page,
                    Err(err) if offset == 0 => {
                        cx.update(|cx| {
                            app::toast_error(format!("Could not play playlist: {err}"), cx)
                        });
                        return;
                    }
                    Err(_) => return,
                };
            offset += page.items.len() as u32;
            let more = page.next.is_some() && !page.items.is_empty();
            let tracks: Vec<Track> = page.items.into_iter().filter_map(|i| i.item).collect();
            let count = tracks.len();
            let still_playing = cx.update(|cx| {
                app::player(cx).update(cx, |player, cx| {
                    if queued == 0 {
                        player.play_context(Some(context.clone()), name.clone(), tracks, 0, cx);
                        true
                    } else {
                        player.extend_context(&context, queued, tracks, cx);
                        // Stop paging once the user has moved on to something else.
                        player.is_context(&context)
                    }
                })
            });
            queued += count;
            if !more || !still_playing {
                return;
            }
        }
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
