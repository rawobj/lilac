//! "Your Library": playlists, albums, artists and podcasts in one filterable list.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions;
use crate::app;
use crate::menus;
use crate::model::*;
use crate::nav::Route;
use crate::player::PlayerEvent;
use crate::theme;
use crate::ui::card::shift_scrolls_sideways;
use crate::ui::{self, Placeholder, WithTooltip, icons};
use crate::ui_state::Dialog;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

const FILTERS: [(Filter, &str); 4] = [
    (Filter::Playlists, "Playlists"),
    (Filter::Albums, "Albums"),
    (Filter::Artists, "Artists"),
    (Filter::Podcasts, "Podcasts"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Recents,
    Alphabetical,
    Creator,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Recents => "Recents",
            Sort::Alphabetical => "Alphabetical",
            Sort::Creator => "Creator",
        }
    }

    fn next(self) -> Self {
        match self {
            Sort::Recents => Sort::Alphabetical,
            Sort::Alphabetical => Sort::Creator,
            Sort::Creator => Sort::Recents,
        }
    }
}

/// A flattened library entry, ready to draw.
#[derive(Clone)]
enum Entry {
    Liked { count: u32 },
    Playlist(Playlist),
    Album(Album),
    Artist(Artist),
    Show(Show),
}

impl Entry {
    fn title(&self) -> SharedString {
        match self {
            Entry::Liked { .. } => "Liked Songs".into(),
            Entry::Playlist(p) => p.name.clone(),
            Entry::Album(a) => a.name.clone(),
            Entry::Artist(a) => a.name.clone(),
            Entry::Show(s) => s.name.clone(),
        }
    }

    fn creator(&self) -> SharedString {
        match self {
            Entry::Liked { .. } | Entry::Artist(_) => SharedString::default(),
            Entry::Playlist(p) => p.owner.name(),
            Entry::Album(a) => a.artist_line(),
            Entry::Show(s) => s.publisher.clone().unwrap_or_default(),
        }
    }

    fn subtitle(&self) -> SharedString {
        match self {
            Entry::Liked { count } => format!("Playlist · {count} songs").into(),
            Entry::Playlist(p) => format!("Playlist · {}", p.owner.name()).into(),
            Entry::Album(a) => format!("{} · {}", a.type_label(), a.artist_line()).into(),
            Entry::Artist(_) => "Artist".into(),
            Entry::Show(s) => match &s.publisher {
                Some(publisher) => format!("Podcast · {publisher}").into(),
                None => "Podcast".into(),
            },
        }
    }

    fn route(&self) -> Route {
        match self {
            Entry::Liked { .. } => Route::Liked,
            Entry::Playlist(p) => Route::Playlist(p.id.clone()),
            Entry::Album(a) => Route::Album(a.id.clone()),
            Entry::Artist(a) => Route::Artist(a.id.clone()),
            Entry::Show(s) => Route::Show(s.id.clone()),
        }
    }

    fn context(&self) -> ContextUri {
        match self {
            Entry::Liked { .. } => ContextUri::Liked,
            Entry::Playlist(p) => ContextUri::Playlist(p.id.clone()),
            Entry::Album(a) => ContextUri::Album(a.id.clone()),
            Entry::Artist(a) => ContextUri::Artist(a.id.clone()),
            Entry::Show(s) => ContextUri::Show(s.id.clone()),
        }
    }

    fn image(&self) -> Option<SharedString> {
        match self {
            Entry::Liked { .. } => None,
            Entry::Playlist(p) => p.images.for_size(48),
            Entry::Album(a) => a.images.for_size(48),
            Entry::Artist(a) => a.images.for_size(48),
            Entry::Show(s) => s.images.for_size(48),
        }
    }
}

pub struct Sidebar {
    filter: Option<Filter>,
    sort: Sort,
    searching: bool,
    query: SharedString,
    search: Entity<InputState>,
    entries: Vec<Entry>,
    scroll: UniformListScrollHandle,
    chips_scroll: ScrollHandle,
}

impl Sidebar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search in Your Library"));
        cx.subscribe(&search, |this: &mut Self, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_lowercase().into();
                this.rebuild(cx);
            }
        })
        .detach();

        cx.observe(&app::library(cx), |this: &mut Self, _, cx| this.rebuild(cx))
            .detach();
        cx.observe(&app::ui(cx), |_, _, cx| cx.notify()).detach();
        cx.observe(&app::nav(cx), |_, _, cx| cx.notify()).detach();
        cx.subscribe(&app::player(cx), |_, _, event: &PlayerEvent, cx| {
            // Rows show what plays and whether it is paused, not where in the track.
            if !matches!(event, PlayerEvent::Seeked) {
                cx.notify()
            }
        })
        .detach();

        let mut this = Self {
            filter: None,
            sort: Sort::Recents,
            searching: false,
            query: SharedString::default(),
            search,
            entries: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            chips_scroll: ScrollHandle::new(),
        };
        this.rebuild(cx);
        this
    }

    /// Recompute the visible entries from the library, filter, search and sort.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let library = app::library(cx);
        let library = library.read(cx);
        let wants = |f: Filter| self.filter.is_none() || self.filter == Some(f);
        let mut entries = Vec::new();

        if wants(Filter::Playlists) {
            entries.extend(library.playlists.iter().cloned().map(Entry::Playlist));
        }
        if wants(Filter::Albums) {
            entries.extend(library.albums.iter().cloned().map(Entry::Album));
        }
        if wants(Filter::Artists) {
            entries.extend(library.artists.iter().cloned().map(Entry::Artist));
        }
        if wants(Filter::Podcasts) {
            entries.extend(library.shows.iter().cloned().map(Entry::Show));
        }

        if !self.query.is_empty() {
            let query = self.query.as_ref();
            entries.retain(|e| {
                e.title().to_lowercase().contains(query)
                    || e.creator().to_lowercase().contains(query)
            });
        }
        match self.sort {
            // The API already returns each collection most-recent first.
            Sort::Recents => {}
            Sort::Alphabetical => entries.sort_by_cached_key(|e| e.title().to_lowercase()),
            Sort::Creator => entries.sort_by_cached_key(|e| e.creator().to_lowercase()),
        }

        if wants(Filter::Playlists)
            && (self.query.is_empty() || "liked songs".contains(self.query.as_ref()))
        {
            entries.insert(
                0,
                Entry::Liked {
                    count: library.liked_total,
                },
            );
        }

        self.entries = entries;
        cx.notify();
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = if self.filter == Some(filter) {
            None
        } else {
            Some(filter)
        };
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.rebuild(cx);
    }

    fn render_entry(
        &self,
        ix: usize,
        collapsed: bool,
        window: &Window,
        cx: &mut App,
    ) -> AnyElement {
        let entry = &self.entries[ix];
        let route = entry.route();
        let selected = *app::nav(cx).read(cx).current() == route;
        let playing = {
            let player = app::player(cx);
            let player = player.read(cx);
            player.playing && player.is_context(&entry.context())
        };

        let art = match entry {
            Entry::Liked { .. } => div()
                .flex_none()
                .size(px(44.0))
                .rounded(px(6.0))
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
                        .size(px(18.0))
                        .text_color(theme::white(0.95)),
                ),
            Entry::Artist(_) => {
                ui::cover(entry.image(), 44.0, 22.0, Placeholder::Artist, window, cx)
            }
            Entry::Show(_) => ui::cover(entry.image(), 44.0, 8.0, Placeholder::Podcast, window, cx),
            Entry::Album(_) => ui::cover(entry.image(), 44.0, 6.0, Placeholder::Music, window, cx),
            Entry::Playlist(_) => {
                ui::cover(entry.image(), 44.0, 6.0, Placeholder::Playlist, window, cx)
            }
        };

        let menu_entry = entry.clone();
        let open_route = route.clone();
        let play_entry = entry.clone();

        div()
            .id(("entry", ix))
            .h(px(60.0))
            .mx_2()
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(theme::RADIUS_CONTROL))
            .cursor_pointer()
            .when(collapsed, |d| d.justify_center())
            .when(selected, |d| d.bg(theme::active_fill()))
            .hover(|s| s.bg(theme::hover_fill()))
            .child(art)
            .when(!collapsed, |d| {
                d.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if playing {
                                    theme::accent()
                                } else {
                                    theme::text()
                                })
                                .child(entry.title()),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(entry.subtitle()),
                        ),
                )
                .when(playing, |d| {
                    d.child(
                        svg()
                            .path(icons::VOLUME_HIGH)
                            .flex_none()
                            .size(px(14.0))
                            .text_color(theme::accent()),
                    )
                })
            })
            .on_click(move |event, _, cx| {
                if event.click_count() >= 2 {
                    match &play_entry {
                        Entry::Liked { .. } => actions::play_liked(cx),
                        Entry::Playlist(p) => {
                            actions::play_playlist(p.id.clone(), p.name.clone(), cx)
                        }
                        Entry::Album(a) => actions::play_album(a.id.clone(), cx),
                        Entry::Artist(a) => actions::play_artist(a.id.clone(), a.name.clone(), cx),
                        Entry::Show(_) => {}
                    }
                } else {
                    app::navigate(open_route.clone(), cx);
                }
            })
            .on_mouse_down(MouseButton::Right, move |event, _, cx| match &menu_entry {
                Entry::Liked { .. } => {}
                Entry::Playlist(p) => menus::playlist_menu(p, event.position, cx),
                Entry::Album(a) => menus::album_menu(a, event.position, cx),
                Entry::Artist(a) => menus::artist_menu(a, event.position, cx),
                Entry::Show(s) => menus::show_menu(s, event.position, cx),
            })
            .into_any_element()
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Narrow windows collapse the sidebar even when the user left it expanded.
        let collapsed = super::layout(window, cx).sidebar <= theme::SIDEBAR_COLLAPSED;
        let loaded = app::library(cx).read(cx).loaded;
        let count = self.entries.len();

        let header = div()
            .flex_none()
            .h(px(52.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .when(collapsed, |d| d.justify_center())
            .child(
                div()
                    .id("toggle")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(36.0))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    .text_color(theme::text_muted())
                    .hover(|s| s.text_color(theme::text()).bg(theme::hover_fill()))
                    .child(
                        svg()
                            .path(icons::LIBRARY)
                            .size(px(20.0))
                            .text_color(theme::text_muted()),
                    )
                    .when(!collapsed, |d| {
                        d.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Your Library"),
                        )
                    })
                    .tip(if collapsed {
                        "Expand Your Library"
                    } else {
                        "Collapse Your Library"
                    })
                    .on_click(|_, _, cx| {
                        app::ui(cx).update(cx, |ui, cx| ui.toggle_sidebar(cx));
                        let collapsed = app::ui(cx).read(cx).sidebar_collapsed;
                        app::update_settings(cx, |s| s.sidebar_collapsed = collapsed);
                    }),
            )
            .when(!collapsed, |d| {
                d.child(div().flex_1()).child(
                    ui::icon_button("create", icons::PLUS)
                        .tip("Create playlist")
                        .on_click(|_, _, cx| {
                            app::ui(cx).update(cx, |ui, cx| {
                                ui.open_dialog(Dialog::CreatePlaylist { seed: None }, cx)
                            })
                        }),
                )
            });

        let chips = (!collapsed).then(|| {
            let mut row = div()
                .id("filters")
                .flex_none()
                .px_3()
                .pb_2()
                .flex()
                .gap_2()
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .track_scroll(&self.chips_scroll)
                .on_scroll_wheel(shift_scrolls_sideways(self.chips_scroll.clone()));
            for (filter, label) in FILTERS {
                row = row.child(
                    ui::chip(label, label, self.filter == Some(filter))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx))),
                );
            }
            row
        });

        let tools = (!collapsed).then(|| {
            div()
                .flex_none()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    ui::icon_button("search", icons::SEARCH)
                        .size(px(28.0))
                        .tip("Search in Your Library")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.searching = !this.searching;
                            if this.searching {
                                this.search.update(cx, |input, cx| input.focus(window, cx));
                            } else {
                                this.search
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            }
                            cx.notify();
                        })),
                )
                .child(if self.searching {
                    div()
                        .flex_1()
                        .h(px(30.0))
                        .px_1()
                        .flex()
                        .items_center()
                        .rounded(px(6.0))
                        .bg(theme::glass_fill_strong())
                        .child(Input::new(&self.search).appearance(false).cleanable(true))
                        .into_any_element()
                } else {
                    div().flex_1().into_any_element()
                })
                .when(!self.searching, |d| {
                    d.child(
                        div()
                            .id("sort")
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .h(px(28.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .hover(|s| s.text_color(theme::text()).bg(theme::hover_fill()))
                            .child(self.sort.label())
                            .child(
                                svg()
                                    .path(icons::SORT)
                                    .size(px(13.0))
                                    .text_color(theme::text_muted()),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sort = this.sort.next();
                                this.rebuild(cx);
                            })),
                    )
                })
        });

        let list = if count == 0 && !loaded {
            ui::loading_state().into_any_element()
        } else if count == 0 {
            div()
                .p_4()
                .text_xs()
                .text_color(theme::text_muted())
                .when(!collapsed, |d| d.child("Nothing here yet."))
                .into_any_element()
        } else {
            uniform_list(
                "library",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                    range
                        .map(|ix| {
                            div()
                                .w_full()
                                .child(this.render_entry(ix, collapsed, window, cx))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .children(chips)
            .children(tools)
            .child(div().flex_1().min_h_0().pb_2().child(list))
    }
}
