mod actions;
mod app;
mod assets;
mod disk_cache;
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
mod romanize;
mod runtime;
mod search;
mod session;
mod settings;
mod single_instance;
mod spotify;
#[cfg(target_os = "linux")]
mod system_volume;
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
    #[cfg(target_os = "linux")]
    name_audio_stream();
    #[cfg(target_os = "linux")]
    keep_discrete_gpu_asleep();
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

/// Our audio goes out through ALSA, which PipeWire otherwise lists as
/// "PipeWire ALSA [lilac]" in the desktop's volume mixer.
#[cfg(target_os = "linux")]
fn name_audio_stream() {
    if std::env::var_os("PIPEWIRE_ALSA").is_none() {
        // The process id lets `system_volume` recognise its own stream; the
        // ALSA plugin does not set it.
        let props = format!(
            r#"{{ application.name = "Lilac" node.description = "Lilac" media.name = "Lilac" media.role = "Music" application.process.id = "{}" }}"#,
            std::process::id()
        );
        // SAFETY: first thing in main, before any other thread exists.
        unsafe { std::env::set_var("PIPEWIRE_ALSA", props) };
    }
}

/// When the window opens, wgpu lists every GPU, and on a hybrid laptop that
/// loads the NVIDIA Vulkan driver and wakes the discrete GPU: over a second
/// before the first frame, and power drawn for nothing, since the GPU
/// driving the displays is the one that draws Lilac. Skip that driver when
/// another GPU drives the displays, unless the user chose drivers themselves.
#[cfg(target_os = "linux")]
fn keep_discrete_gpu_asleep() {
    const CHOSEN: [&str; 4] = [
        "VK_DRIVER_FILES",
        "VK_ICD_FILENAMES",
        "VK_LOADER_DRIVERS_SELECT",
        "VK_LOADER_DRIVERS_DISABLE",
    ];
    if CHOSEN.iter().any(|var| std::env::var_os(var).is_some()) {
        return;
    }
    let Ok(devices) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return;
    };
    let read = |dev: &std::path::Path, file: &str| {
        std::fs::read_to_string(dev.join(file))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let (mut nvidia, mut other_drives_displays) = (false, false);
    for dev in devices.flatten().map(|d| d.path()) {
        // PCI class 0x03: display controllers.
        if !read(&dev, "class").starts_with("0x03") {
            continue;
        }
        if read(&dev, "vendor") == "0x10de" {
            nvidia = true;
        } else if read(&dev, "boot_vga") == "1" {
            other_drives_displays = true;
        }
    }
    if nvidia && other_drives_displays {
        // SAFETY: called from main before any other thread exists.
        unsafe { std::env::set_var("VK_LOADER_DRIVERS_DISABLE", "*nvidia*") };
    }
}
