//! The guided Soloist setup: one dialog that walks someone who has never
//! heard of Soloist from "nothing plays" to music playing, doing every step
//! it can by itself.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::soloist::{self, Gate, Install, Process, Soloist, Step, build};
use crate::{app, theme, ui, ui::icons};

const STEPS: [(Step, &str); 3] = [
    (Step::Download, "Download Soloist"),
    (Step::Key, "Add your key"),
    (Step::Pair, "Connect your account"),
];

/// Title, body and main button of the setup dialog.
pub fn render(
    key: &Entity<InputState>,
    cx: &mut App,
) -> (&'static str, AnyElement, Option<AnyElement>) {
    let entity = app::soloist(cx);
    let step = entity.read(cx).step();
    let (content, confirm) = match step {
        Step::Download => download(entity.read(cx)),
        Step::Key => key_step(key),
        Step::Pair => pair(entity.read(cx)),
        Step::Done => done(cx),
    };
    let body = div()
        .flex()
        .flex_col()
        .gap_4()
        .when(step != Step::Done, |d| {
            d.child(muted(
                "Lilac plays music through Soloist, Spotify's own player for apps \
                 like Lilac. Setting it up takes a couple of minutes, once.",
            ))
        })
        .when(step != Step::Done, |d| d.child(progress(step)))
        .child(content)
        .into_any_element();
    let title = if step == Step::Done {
        "You're all set"
    } else {
        "Set up Soloist"
    };
    (title, body, confirm)
}

/// The three steps, ticked off as they are done.
fn progress(current: Step) -> Div {
    let mut list = div().flex().flex_col().gap_1();
    for (ix, (step, label)) in STEPS.iter().enumerate() {
        let done = (*step as u8) < (current as u8);
        let here = *step == current;
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(if here {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                .when(here, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(
                    div()
                        .size(px(20.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_xs()
                        .map(|d| {
                            if done {
                                d.bg(theme::accent()).child(
                                    svg()
                                        .path(icons::CHECK)
                                        .size(px(12.0))
                                        .text_color(theme::backdrop_opaque()),
                                )
                            } else if here {
                                d.border_1()
                                    .border_color(theme::accent())
                                    .text_color(theme::accent())
                                    .child((ix + 1).to_string())
                            } else {
                                d.border_1()
                                    .border_color(theme::glass_border())
                                    .child((ix + 1).to_string())
                            }
                        }),
                )
                .child(*label),
        );
    }
    list
}

fn muted(text: impl Into<SharedString>) -> Div {
    div().text_color(theme::text_muted()).child(text.into())
}

/// Numbered instructions, one per line.
fn numbered(lines: &[String]) -> Div {
    let mut list = div().flex().flex_col().gap_2();
    for (ix, line) in lines.iter().enumerate() {
        list = list.child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(16.0))
                        .text_color(theme::accent())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("{}.", ix + 1)),
                )
                .child(div().flex_1().min_w_0().child(line.clone())),
        );
    }
    list
}

fn status(text: impl Into<SharedString>, busy: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .when(busy, |d| d.child(ui::spinner(14.0)))
        // Bare text in a flex row keeps its one-line width; a shrinkable
        // box lets it wrap to the dialog.
        .child(div().flex_1().min_w_0().child(text.into()))
}

fn notice(text: impl Into<SharedString>) -> Div {
    div()
        .p_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(theme::glass_fill_strong())
        .border_1()
        .border_color(theme::glass_border())
        .flex()
        .flex_col()
        .gap_3()
        .child(text.into())
}

fn download(soloist: &Soloist) -> (AnyElement, Option<AnyElement>) {
    let expired = soloist.expired();
    let (line, busy, failed): (SharedString, bool, bool) = match &soloist.install {
        Install::Downloading(got, Some(total)) if *total > 0 => (
            format!("Downloading… {}%", got * 100 / total).into(),
            true,
            false,
        ),
        Install::Downloading(got, _) => (
            format!("Downloading… {:.1} MB", *got as f64 / 1e6).into(),
            true,
            false,
        ),
        Install::Unpacking => ("Installing…".into(), true, false),
        Install::Failed(why) => (format!("The download failed: {why}").into(), false, true),
        Install::Idle if expired => (
            "Your copy of Soloist has expired. Spotify's builds run for 90 days; \
             a fresh one takes a few seconds."
                .into(),
            false,
            false,
        ),
        Install::Idle => (
            "Lilac downloads it straight from Spotify (about 14 MB) and keeps it \
             up to date for you."
                .into(),
            false,
            false,
        ),
    };
    let body = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(status(line, busy))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("By downloading it you accept Spotify's Soloist terms."),
        )
        .into_any_element();
    let label = if failed { "Try again" } else { "Download" };
    let confirm = (!busy).then(|| {
        ui::pill_button("confirm", label, true)
            .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.install(cx)))
            .into_any_element()
    });
    (body, confirm)
}

fn key_step(key: &Entity<InputState>) -> (AnyElement, Option<AnyElement>) {
    let paste_into = key.clone();
    let save_from = key.clone();
    let body = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(muted(
            "Spotify gives everyone who uses Soloist a free personal key. \
             It needs a Spotify Premium account.",
        ))
        .child(numbered(&[
            "Click Get my key. Your browser opens Spotify's developer site; log in \
             with your Spotify account if it asks."
                .into(),
            "Accept the terms and copy the key it shows (it starts with spak_).".into(),
            "Come back to Lilac: it picks the key up from the clipboard by itself \
             (or click Paste)."
                .into(),
        ]))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    ui::pill_button("soloist-get-key", "Get my key", false)
                        .on_click(|_, _, cx| cx.open_url(build::KEY_PAGE)),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(32.0))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .bg(theme::glass_fill_strong())
                        .border_1()
                        .border_color(theme::glass_border())
                        .child(Input::new(key).appearance(false)),
                )
                .child(
                    ui::pill_button("soloist-paste-key", "Paste", false).on_click(
                        move |_, window, cx| {
                            let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
                                app::toast_error("The clipboard is empty; copy the key first", cx);
                                return;
                            };
                            let text = text.trim().to_string();
                            paste_into
                                .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
                            save_key(text, cx);
                        },
                    ),
                ),
        )
        .into_any_element();
    let confirm = ui::pill_button("confirm", "Continue", true)
        .on_click(move |_, _, cx| {
            let text = save_from.read(cx).value().trim().to_string();
            save_key(text, cx);
        })
        .into_any_element();
    (body, Some(confirm))
}

fn save_key(key: String, cx: &mut App) {
    if key.is_empty() {
        app::toast_error("Paste your key first", cx);
    } else if key.len() < 16 || key.contains(char::is_whitespace) {
        app::toast_error("That doesn't look like a Soloist key; copy it again", cx);
    } else {
        app::soloist(cx).update(cx, |s, cx| s.set_api_key(key, cx));
    }
}

fn pair(soloist: &Soloist) -> (AnyElement, Option<AnyElement>) {
    let mut body = div().flex().flex_col().gap_3().child(muted(
        "Last step: let Spotify connect Soloist to your account. You do this once, \
         from the Spotify app on your phone.",
    ));
    let mut confirm = None;

    match &soloist.gate {
        Gate::Blocked(_) => {
            body = body.child(notice(
                "Your computer's firewall keeps your phone from seeing Soloist. \
                 Lilac can let it through until you're connected, at most 15 minutes. \
                 Your computer will ask for your password.",
            ));
            confirm = Some(
                ui::pill_button("confirm", "Let my phone through", true)
                    .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.open_firewall(cx)))
                    .into_any_element(),
            );
        }
        Gate::Opening(_) => {
            body = body.child(status("Waiting for your password…", true));
        }
        Gate::Failed(kind, why) => {
            let manual = soloist
                .pairing_port()
                .map(|port| kind.manual_command(port))
                .unwrap_or_default();
            body = body.child(notice(format!("The firewall stayed closed: {why}.")).when(
                !manual.is_empty(),
                |d| {
                    d.child(muted("Or run this in a terminal:"))
                        .child(div().font_family("monospace").text_xs().child(manual))
                },
            ));
            confirm = Some(
                ui::pill_button("confirm", "Try again", true)
                    .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.open_firewall(cx)))
                    .into_any_element(),
            );
        }
        Gate::Unchecked | Gate::Clear | Gate::Open(_) => {}
    }

    let reachable = matches!(soloist.gate, Gate::Clear | Gate::Open(_));
    body = body.child(
        div().when(!reachable, |d| d.opacity(0.5)).child(numbered(&[
            "Connect your phone to the same Wi-Fi as this computer.".into(),
            "Open Spotify on it and tap the devices button (the speaker icon \
                 next to the song that's playing, or in Settings → Devices)."
                .into(),
            format!("Pick “{}”.", soloist::DEVICE_NAME),
        ])),
    );

    let line = match &soloist.process {
        Process::Failed(why) => {
            confirm = Some(
                ui::pill_button("confirm", "Start again", true)
                    .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.start(cx)))
                    .into_any_element(),
            );
            Some(status(format!("Soloist stopped: {why}"), false).text_color(theme::danger()))
        }
        Process::Stopped | Process::Starting => Some(status("Starting Soloist…", true)),
        Process::Running if reachable => Some(status("Waiting for your phone…", true)),
        Process::Running => None,
    };
    (body.children(line).into_any_element(), confirm)
}

fn done(cx: &mut App) -> (AnyElement, Option<AnyElement>) {
    let track = app::player(cx)
        .read(cx)
        .current
        .as_ref()
        .map(|t| t.name.clone());
    let body = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(muted(
            "Soloist is connected to your account. Lilac plays through it, and \
             downloads new Soloist builds before old ones run out.",
        ))
        .child(muted(
            "You can switch your phone back to whatever it was playing; it isn't needed any more.",
        ))
        .into_any_element();
    let close = |cx: &mut App| app::ui(cx).update(cx, |ui, cx| ui.close_dialog(cx));
    let confirm = match track {
        Some(name) => ui::pill_button("confirm", format!("Play “{name}”"), true)
            .on_click(move |_, _, cx| {
                close(cx);
                app::player(cx).update(cx, |p, cx| p.play_here_on_soloist(cx));
            })
            .into_any_element(),
        None => ui::pill_button("confirm", "Done", true)
            .on_click(move |_, _, cx| close(cx))
            .into_any_element(),
    };
    (body, Some(confirm))
}

/// Shown above the player bar while nothing can play here, so the app does
/// not look broken: what is wrong, and the one thing to do about it.
pub fn locked_banner() -> Div {
    div()
        .flex_none()
        .mx(px(theme::GAP))
        .mt(px(theme::GAP))
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_4()
        .rounded(px(theme::RADIUS_CONTROL))
        .bg(theme::glass_fill_strong())
        .border_1()
        .border_color(theme::accent())
        .child(
            svg()
                .path(icons::SPEAKER)
                .flex_none()
                .size(px(22.0))
                .text_color(theme::accent()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("One more step before Lilac can play music"),
                )
                .child(muted(
                    "Lilac plays through Soloist, Spotify's own player. Setting it up \
                     takes about two minutes, once.",
                )),
        )
        .child(
            ui::pill_button("locked-setup", "Set up Soloist", true)
                .on_click(|_, _, cx| Soloist::open_setup(cx)),
        )
}
