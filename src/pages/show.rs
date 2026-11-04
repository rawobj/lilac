//! Podcast page: show details and its episodes.

use gpui_kit::*;

use super::scroll_page;
use crate::app;
use crate::library::SavedEntity;
use crate::menus::{self, TrackMenuContext};
use crate::model::*;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::theme;
use crate::ui::{self, Placeholder, icons};

pub struct ShowPage {
    id: SharedString,
    show: Option<Show>,
    episodes: Vec<Track>,
    total: u32,
    loading_more: bool,
    error: Option<SharedString>,
    scroll: ScrollHandle,
}

impl ShowPage {
    pub fn new(id: SharedString, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&app::player(cx), |_, _, event: &PlayerEvent, cx| {
            // Rows show what plays and whether it is paused, not where in the track.
            if !matches!(event, PlayerEvent::Seeked) {
                cx.notify()
            }
        })
        .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();

        let api = app::api(cx);
        let fetch_id = id.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move {
                let show = api.show(&fetch_id).await?;
                let page = api.show_episodes(&show, 0).await?;
                Ok((show, page))
            })
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((show, page)) => {
                        this.show = Some(show);
                        this.total = page.total;
                        this.episodes = page.items;
                    }
                    Err(err) => this.error = Some(format!("{err}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        Self {
            id,
            show: None,
            episodes: Vec::new(),
            total: 0,
            loading_more: false,
            error: None,
            scroll: ScrollHandle::new(),
        }
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(show) = self.show.clone() else {
            return;
        };
        if self.loading_more {
            return;
        }
        self.loading_more = true;
        cx.notify();
        let api = app::api(cx);
        let offset = self.episodes.len() as u32;
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.show_episodes(&show, offset).await }).await;
            this.update(cx, |this, cx| {
                this.loading_more = false;
                if let Ok(page) = result {
                    this.episodes.extend(page.items);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn play(&self, index: usize, cx: &mut App) {
        let name = self
            .show
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let (id, episodes) = (self.id.clone(), self.episodes.clone());
        app::player(cx).update(cx, |player, cx| {
            player.play_context(Some(ContextUri::Show(id)), name, episodes, index, cx)
        });
    }

    fn episode_row(
        &self,
        ix: usize,
        episode: &Track,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (is_current, playing) = {
            let player = app::player(cx);
            let p = player.read(cx);
            let current = p.is_current(&episode.id);
            (current, current && p.playing)
        };
        let meta = match &episode.release_date {
            Some(date) => format!("{date} · {}", format_total(episode.duration_ms as u64)),
            None => format_total(episode.duration_ms as u64),
        };
        let menu_episode = episode.clone();

        div()
            .id(("episode", ix))
            .mx_3()
            .p_3()
            .flex()
            .gap_4()
            .rounded(px(theme::RADIUS_CONTROL))
            .hover(|s| s.bg(theme::hover_fill()))
            .child(ui::cover(
                episode.cover().for_size(96),
                96.0,
                8.0,
                Placeholder::Podcast,
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
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if is_current {
                                theme::accent()
                            } else {
                                theme::text()
                            })
                            .child(episode.name.clone()),
                    )
                    .children(episode.description.clone().map(|d| {
                        div()
                            .line_clamp(2)
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(d)
                    }))
                    .child(
                        div()
                            .mt_1()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(ui::play_button("play", 32.0, playing).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if is_current {
                                        app::player(cx).update(cx, |p, cx| p.toggle(cx));
                                    } else {
                                        this.play(ix, cx);
                                    }
                                },
                            )))
                            .child(div().text_xs().text_color(theme::text_muted()).child(meta)),
                    ),
            )
            .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                menus::track_menu(
                    &menu_episode,
                    TrackMenuContext::default(),
                    event.position,
                    cx,
                )
            })
    }
}

impl Render for ShowPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.error {
            return ui::empty_state(
                icons::CIRCLE_ALERT,
                "Couldn't load this show",
                error.clone(),
            );
        }
        let Some(show) = self.show.clone() else {
            return ui::loading_state();
        };
        let following = app::library(cx).read(cx).is_saved(&show.id);
        let follow_show = show.clone();

        let hero = div()
            .relative()
            .h(px(288.0))
            .child(
                div()
                    .absolute()
                    .size_full()
                    .bg(theme::hero_gradient(theme::seed_of(&self.id))),
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
                        show.images.for_size(208),
                        208.0,
                        theme::RADIUS_CARD,
                        Placeholder::Podcast,
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
                            .child(div().text_xs().child("Podcast"))
                            .child(
                                div()
                                    .line_clamp(2)
                                    .text_size(px(40.0))
                                    .line_height(px(48.0))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .child(show.name.clone()),
                            )
                            .children(
                                show.publisher
                                    .clone()
                                    .map(|p| div().font_weight(FontWeight::SEMIBOLD).child(p)),
                            ),
                    ),
            );

        let mut body = div()
            .pb_8()
            .flex()
            .flex_col()
            .gap_4()
            .child(hero)
            .child(
                div().px_6().flex().items_center().gap_3().child(
                    ui::pill_button(
                        "follow",
                        if following { "Following" } else { "Follow" },
                        false,
                    )
                    .on_click(move |_, _, cx| {
                        let item = SavedEntity::Show(follow_show.clone());
                        app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx))
                    }),
                ),
            )
            .children(show.description.clone().filter(|d| !d.is_empty()).map(|d| {
                div()
                    .px_6()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(ui::section_title("About"))
                    .child(
                        div()
                            .max_w(px(720.0))
                            .line_clamp(4)
                            .text_color(theme::text_muted())
                            .child(d),
                    )
            }))
            .child(div().px_6().child(ui::section_title("All episodes")));

        let episodes = self.episodes.clone();
        for (ix, episode) in episodes.iter().enumerate() {
            body = body.child(self.episode_row(ix, episode, window, cx));
        }
        if (self.episodes.len() as u32) < self.total {
            body = body.child(div().px_6().child(if self.loading_more {
                ui::spinner(20.0).into_any_element()
            } else {
                ui::pill_button("more", "Load more episodes", false)
                    .on_click(cx.listener(|this, _, _, cx| this.load_more(cx)))
                    .into_any_element()
            }));
        }

        scroll_page("show", &self.scroll, body)
    }
}
