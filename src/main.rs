mod actions;
mod app;
mod assets;
mod feed;
mod friends;
mod images;
mod keys;
mod library;
mod menus;
mod model;
#[cfg(target_os = "linux")]
mod mpris;
mod nav;
mod pages;
mod panels;
mod paths;
mod player;
mod runtime;
mod search;
mod session;
mod settings;
mod single_instance;
mod spotify;
mod theme;
mod ui;
mod ui_state;
mod views;

use gpui_kit::*;

fn main() {
    let Some(mut show_requests) = single_instance::claim() else {
        // The running Lilac brings its window to the front.
        return;
    };
    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            theme::init_fonts(cx);
            images::ImageCache::init(cx);
            app::AppState::init(cx);
            keys::init(cx);
            #[cfg(target_os = "linux")]
            mpris::init(cx);

            let bounds = Bounds::centered(None, size(px(1320.0), px(840.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Lilac".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                window_decorations: Some(WindowDecorations::Client),
                window_min_size: Some(size(px(960.0), px(600.0))),
                app_id: Some("lilac".into()),
                ..Default::default()
            };

            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| views::Lilac::new(window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);

            cx.spawn(async move |cx| {
                while show_requests.recv().await.is_some() {
                    cx.update(|cx| {
                        cx.activate(true);
                        for window in cx.windows() {
                            let _ = window.update(cx, |_, window, _| window.activate_window());
                        }
                    });
                }
            })
            .detach();
        });
}
