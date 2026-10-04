//! Keyboard shortcuts.

use gpui_kit::*;

use crate::app;
use crate::nav::Route;
use crate::ui_state::RightPanel;

actions!(
    lilac,
    [
        TogglePlay,
        NextTrack,
        PreviousTrack,
        SeekForward,
        SeekBackward,
        VolumeUp,
        VolumeDown,
        ToggleMute,
        ToggleShuffle,
        CycleRepeat,
        LikeCurrent,
        FocusSearch,
        GoBack,
        GoForward,
        GoHome,
        ToggleQueue,
        ToggleLyrics,
        ToggleNowPlaying,
        ToggleSidebar,
        ToggleImmersive,
        OpenSettings,
        Dismiss,
        Quit,
    ]
);

/// Context in which single-key shortcuts apply: anywhere except a text field.
const IDLE: Option<&str> = Some("Lilac && !Input");
const ANYWHERE: Option<&str> = Some("Lilac");

/// Shortcuts as shown in the help dialog.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Space", "Play / pause"),
    ("Ctrl →", "Next track"),
    ("Ctrl ←", "Previous track"),
    ("Shift →", "Seek forward 5 s"),
    ("Shift ←", "Seek backward 5 s"),
    ("Ctrl ↑", "Volume up"),
    ("Ctrl ↓", "Volume down"),
    ("M", "Mute"),
    ("Ctrl S", "Shuffle"),
    ("Ctrl R", "Repeat"),
    ("Alt Shift B", "Like current song"),
    ("Ctrl K", "Search"),
    ("Alt ←", "Back"),
    ("Alt →", "Forward"),
    ("Alt Shift H", "Home"),
    ("Ctrl Q", "Queue"),
    ("Ctrl L", "Lyrics"),
    ("Ctrl B", "Toggle library"),
    ("Ctrl Shift N", "Now playing view"),
    ("F11", "Immersive mode"),
    ("Ctrl ,", "Settings"),
    ("Esc", "Close menu or dialog"),
];

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("space", TogglePlay, IDLE),
        KeyBinding::new("ctrl-right", NextTrack, IDLE),
        KeyBinding::new("ctrl-left", PreviousTrack, IDLE),
        KeyBinding::new("shift-right", SeekForward, IDLE),
        KeyBinding::new("shift-left", SeekBackward, IDLE),
        KeyBinding::new("ctrl-up", VolumeUp, ANYWHERE),
        KeyBinding::new("ctrl-down", VolumeDown, ANYWHERE),
        KeyBinding::new("m", ToggleMute, IDLE),
        KeyBinding::new("ctrl-s", ToggleShuffle, ANYWHERE),
        KeyBinding::new("ctrl-r", CycleRepeat, ANYWHERE),
        KeyBinding::new("alt-shift-b", LikeCurrent, ANYWHERE),
        KeyBinding::new("ctrl-k", FocusSearch, ANYWHERE),
        KeyBinding::new("alt-left", GoBack, ANYWHERE),
        KeyBinding::new("alt-right", GoForward, ANYWHERE),
        KeyBinding::new("alt-shift-h", GoHome, ANYWHERE),
        KeyBinding::new("ctrl-q", ToggleQueue, ANYWHERE),
        KeyBinding::new("ctrl-l", ToggleLyrics, ANYWHERE),
        KeyBinding::new("ctrl-b", ToggleSidebar, ANYWHERE),
        KeyBinding::new("ctrl-shift-n", ToggleNowPlaying, ANYWHERE),
        KeyBinding::new("f11", ToggleImmersive, ANYWHERE),
        KeyBinding::new("ctrl-,", OpenSettings, ANYWHERE),
        KeyBinding::new("escape", Dismiss, ANYWHERE),
        KeyBinding::new("ctrl-shift-q", Quit, ANYWHERE),
    ]);

    // Shortcuts that only touch shared state are handled globally. The ones
    // that need a view (focus search, dismiss) are handled by the root view.
    cx.on_action(|_: &TogglePlay, cx| app::player(cx).update(cx, |p, cx| p.toggle(cx)));
    cx.on_action(|_: &NextTrack, cx| app::player(cx).update(cx, |p, cx| p.next(cx)));
    cx.on_action(|_: &PreviousTrack, cx| app::player(cx).update(cx, |p, cx| p.previous(cx)));
    cx.on_action(|_: &SeekForward, cx| app::player(cx).update(cx, |p, cx| p.seek_by(5000, cx)));
    cx.on_action(|_: &SeekBackward, cx| app::player(cx).update(cx, |p, cx| p.seek_by(-5000, cx)));
    cx.on_action(|_: &VolumeUp, cx| {
        app::player(cx).update(cx, |p, cx| {
            p.set_volume(p.volume + 0.05, cx);
            p.persist_volume(cx);
        })
    });
    cx.on_action(|_: &VolumeDown, cx| {
        app::player(cx).update(cx, |p, cx| {
            p.set_volume(p.volume - 0.05, cx);
            p.persist_volume(cx);
        })
    });
    cx.on_action(|_: &ToggleMute, cx| app::player(cx).update(cx, |p, cx| p.toggle_mute(cx)));
    cx.on_action(|_: &ToggleShuffle, cx| app::player(cx).update(cx, |p, cx| p.toggle_shuffle(cx)));
    cx.on_action(|_: &CycleRepeat, cx| app::player(cx).update(cx, |p, cx| p.cycle_repeat(cx)));
    cx.on_action(|_: &LikeCurrent, cx| {
        if let Some(track) = app::player(cx).read(cx).current.clone() {
            app::library(cx).update(cx, |library, cx| library.toggle_like(&track, cx));
        }
    });
    cx.on_action(|_: &GoBack, cx| app::nav(cx).update(cx, |nav, cx| nav.back(cx)));
    cx.on_action(|_: &GoForward, cx| app::nav(cx).update(cx, |nav, cx| nav.forward(cx)));
    cx.on_action(|_: &GoHome, cx| app::navigate(Route::Home, cx));
    cx.on_action(|_: &ToggleQueue, cx| {
        app::ui(cx).update(cx, |ui, cx| ui.toggle_panel(RightPanel::Queue, cx))
    });
    cx.on_action(|_: &ToggleNowPlaying, cx| {
        app::ui(cx).update(cx, |ui, cx| ui.toggle_panel(RightPanel::NowPlaying, cx))
    });
    cx.on_action(|_: &ToggleLyrics, cx| {
        app::nav(cx).update(cx, |nav, cx| nav.toggle(Route::Lyrics, cx))
    });
    cx.on_action(|_: &ToggleSidebar, cx| app::ui(cx).update(cx, |ui, cx| ui.toggle_sidebar(cx)));
    cx.on_action(|_: &ToggleImmersive, cx| {
        app::ui(cx).update(cx, |ui, cx| ui.toggle_immersive(cx))
    });
    cx.on_action(|_: &OpenSettings, cx| {
        app::nav(cx).update(cx, |nav, cx| nav.toggle(Route::Settings, cx))
    });
    cx.on_action(|_: &Quit, cx| cx.quit());
}
