mod assets;
mod images;
mod model;
mod nav;
mod paths;
mod runtime;
mod settings;
mod spotify;
mod theme;

use gpui_kit::*;

struct Shell;

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(theme::backdrop())
            .text_color(theme::text())
            .font_family(theme::FONT_UI)
            .flex()
            .items_center()
            .justify_center()
            .child(
                theme::glass()
                    .px_8()
                    .py_6()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        svg()
                            .path("icons/lilac.svg")
                            .size_6()
                            .text_color(theme::accent()),
                    )
                    .child(div().text_xl().child("lilac")),
            )
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);

            let bounds = Bounds::centered(None, size(px(1320.0), px(840.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("lilac".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                window_min_size: Some(size(px(960.0), px(600.0))),
                app_id: Some("lilac".into()),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| Shell))
                .expect("failed to open window");
            cx.activate(true);
        });
}
