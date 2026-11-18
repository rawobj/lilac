//! Pages shown in the main panel, one per route.

mod artist;
mod collection;
mod home;
pub mod immersive;
mod lyrics;
mod profile;
mod search;
mod settings;
mod show;
mod whats_new;

use gpui_kit::*;

use crate::actions;
use crate::app;
use crate::menus;
use crate::model::*;
use crate::nav::Route;
use crate::ui::Placeholder;
use crate::ui::card::{Card, card};

use collection::{CollectionPage, Source};

/// Create the view for `route`.
pub fn build(route: &Route, window: &mut Window, cx: &mut App) -> AnyView {
    match route.clone() {
        Route::Home => cx.new(home::HomePage::new).into(),
        Route::Search => cx.new(|cx| search::SearchPage::new(None, cx)).into(),
        Route::Genre(name) => cx.new(|cx| search::SearchPage::new(Some(name), cx)).into(),
        Route::Liked => cx.new(|cx| CollectionPage::new(Source::Liked, cx)).into(),
        Route::Playlist(id) => cx
            .new(|cx| CollectionPage::new(Source::Playlist(id), cx))
            .into(),
        Route::Album(id) => cx
            .new(|cx| CollectionPage::new(Source::Album(id), cx))
            .into(),
        Route::Artist(id) => cx.new(|cx| artist::ArtistPage::new(id, cx)).into(),
        Route::Show(id) => cx.new(|cx| show::ShowPage::new(id, cx)).into(),
        Route::Profile => cx.new(profile::ProfilePage::new).into(),
        Route::WhatsNew => cx.new(whats_new::WhatsNewPage::new).into(),
        Route::Lyrics => cx.new(lyrics::LyricsPage::new).into(),
        Route::Settings => cx.new(|cx| settings::SettingsPage::new(window, cx)).into(),
    }
}

/// A vertically scrolling page body with an overlay scrollbar.
pub fn scroll_page(id: &'static str, handle: &ScrollHandle, content: impl IntoElement) -> Div {
    div()
        .relative()
        .size_full()
        .child(
            div()
                .id(id)
                .size_full()
                .overflow_y_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(handle)
                // Room for the scrollbar, so it never covers content.
                .pr(px(12.0))
                .child(content),
        )
        .group("scrollable")
        .child(crate::ui::vertical_scrollbar(handle))
}

// Cards for each kind of catalog item, shared by home, search, artist and profile.

pub fn album_card(
    ix: usize,
    album: &Album,
    subtitle: Option<SharedString>,
    window: &Window,
    cx: &mut App,
) -> AnyElement {
    let (open_id, play_id, menu_album) = (album.id.clone(), album.id.clone(), album.clone());
    let subtitle = subtitle.unwrap_or_else(|| {
        let artists = album.artist_line();
        if album.year().is_empty() {
            artists
        } else {
            format!("{} · {artists}", album.year()).into()
        }
    });
    card(
        ("album", ix),
        Card {
            title: album.name.clone(),
            subtitle,
            image: album.images.for_size(160),
            placeholder: Placeholder::Music,
            round: false,
        },
        move |_, cx| app::navigate(Route::Album(open_id.clone()), cx),
        Some(Box::new(move |_, cx| {
            actions::play_album(play_id.clone(), cx)
        })),
        window,
        cx,
    )
    .on_mouse_down(MouseButton::Right, move |event, _, cx| {
        menus::album_menu(&menu_album, event.position, cx)
    })
    .into_any_element()
}

pub fn artist_card(ix: usize, artist: &Artist, window: &Window, cx: &mut App) -> AnyElement {
    let (open_id, play_id, name, menu_artist) = (
        artist.id.clone(),
        artist.id.clone(),
        artist.name.clone(),
        artist.clone(),
    );
    card(
        ("artist", ix),
        Card {
            title: artist.name.clone(),
            subtitle: "Artist".into(),
            image: artist.images.for_size(160),
            placeholder: Placeholder::Artist,
            round: true,
        },
        move |_, cx| app::navigate(Route::Artist(open_id.clone()), cx),
        Some(Box::new(move |_, cx| {
            actions::play_artist(play_id.clone(), name.clone(), cx)
        })),
        window,
        cx,
    )
    .on_mouse_down(MouseButton::Right, move |event, _, cx| {
        menus::artist_menu(&menu_artist, event.position, cx)
    })
    .into_any_element()
}

pub fn playlist_card(ix: usize, playlist: &Playlist, window: &Window, cx: &mut App) -> AnyElement {
    let (open_id, play_id, name, menu_playlist) = (
        playlist.id.clone(),
        playlist.id.clone(),
        playlist.name.clone(),
        playlist.clone(),
    );
    let subtitle = match playlist.description.as_ref().filter(|d| !d.is_empty()) {
        Some(description) => description.clone(),
        None if playlist.owner.name().is_empty() => "Playlist".into(),
        None => format!("By {}", playlist.owner.name()).into(),
    };
    card(
        ("playlist", ix),
        Card {
            title: playlist.name.clone(),
            subtitle,
            image: playlist.images.for_size(160),
            placeholder: Placeholder::Playlist,
            round: false,
        },
        move |_, cx| app::navigate(Route::Playlist(open_id.clone()), cx),
        Some(Box::new(move |_, cx| {
            actions::play_playlist(play_id.clone(), name.clone(), cx)
        })),
        window,
        cx,
    )
    .on_mouse_down(MouseButton::Right, move |event, _, cx| {
        menus::playlist_menu(&menu_playlist, event.position, cx)
    })
    .into_any_element()
}

pub fn show_card(ix: usize, show: &Show, window: &Window, cx: &mut App) -> AnyElement {
    let (open_id, menu_show) = (show.id.clone(), show.clone());
    card(
        ("show", ix),
        Card {
            title: show.name.clone(),
            subtitle: show.publisher.clone().unwrap_or_else(|| "Podcast".into()),
            image: show.images.for_size(160),
            placeholder: Placeholder::Podcast,
            round: false,
        },
        move |_, cx| app::navigate(Route::Show(open_id.clone()), cx),
        None,
        window,
        cx,
    )
    .on_mouse_down(MouseButton::Right, move |event, _, cx| {
        menus::show_menu(&menu_show, event.position, cx)
    })
    .into_any_element()
}
