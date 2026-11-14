//! Full-window "now playing" mode.

use gpui_kit::*;

use crate::app;
use crate::model::format_duration;
use crate::theme;
use crate::ui::{self, Placeholder, WithTooltip, icons};

pub fn render(window: &Window, cx: &mut App) -> Div {
    let player = app::player(cx);
    let (track, playing, position, duration, progress) = {
        let p = player.read(cx);
        (
            p.current.clone(),
            p.playing,
            p.position_ms(),
            p.duration_ms(),
            p.progress(),
        )
    };
    // Keep the progress bar moving while this view is up.
    if playing {
        window.request_animation_frame();
    }

    let exit = ui::icon_button("exit", icons::MINIMIZE)
        .size(px(40.0))
        .tip("Exit immersive mode")
        .on_click(|_, _, cx| app::ui(cx).update(cx, |ui, cx| ui.toggle_immersive(cx)));

    let Some(track) = track else {
        return div()
            .size_full()
            .child(div().absolute().top_4().right_4().child(exit))
            .child(ui::empty_state(
                icons::MUSIC,
                "Nothing playing",
                "Pick something to listen to.",
            ));
    };
    let seed = theme::seed_of(&track.id);
    let edge = (f32::from(window.viewport_size().height) * 0.46).clamp(220.0, 520.0);

    div()
        .relative()
        .size_full()
        .child(div().absolute().size_full().bg(theme::hero_gradient(seed)))
        .child(
            theme::glow(theme::placeholder_tint(seed).opacity(0.5), 360.0)
                .top(px(120.0))
                .left(px(160.0)),
        )
        .child(div().absolute().top_4().right_4().child(exit))
        .child(
            div()
                .relative()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_6()
                .child(
                    div()
                        .rounded(px(18.0))
                        .shadow(vec![BoxShadow {
                            color: theme::black(0.55),
                            offset: point(px(0.0), px(30.0)),
                            blur_radius: px(70.0),
                            spread_radius: px(-10.0),
                            inset: false,
                        }])
                        .child(ui::cover(
                            track.cover().for_size(640),
                            edge,
                            18.0,
                            Placeholder::Music,
                            window,
                            cx,
                        )),
                )
                .child(
                    div()
                        .max_w(px(720.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .truncate()
                                .text_3xl()
                                .font_weight(FontWeight::BOLD)
                                .child(track.name.clone()),
                        )
                        .child(
                            ui::artist_line_links(
                                "immersive",
                                &track,
                                theme::text_muted(),
                                theme::text(),
                            )
                            .text_lg(),
                        ),
                )
                .child(
                    div()
                        .w(px(520.0))
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format_duration(position))
                        .child(
                            div()
                                .flex_1()
                                .h(px(4.0))
                                .rounded_full()
                                .bg(theme::white(0.18))
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(progress))
                                        .rounded_full()
                                        .bg(theme::text()),
                                ),
                        )
                        .child(format_duration(duration)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_6()
                        .child(
                            ui::icon_button("previous", icons::SKIP_BACK)
                                .size(px(44.0))
                                .on_click(|_, _, cx| {
                                    app::player(cx).update(cx, |p, cx| p.previous(cx))
                                }),
                        )
                        .child(
                            ui::play_button("play", 64.0, playing).on_click(|_, _, cx| {
                                app::player(cx).update(cx, |p, cx| p.toggle(cx))
                            }),
                        )
                        .child(
                            ui::icon_button("next", icons::SKIP_FORWARD)
                                .size(px(44.0))
                                .on_click(|_, _, cx| {
                                    app::player(cx).update(cx, |p, cx| p.next(cx))
                                }),
                        ),
                ),
        )
}
