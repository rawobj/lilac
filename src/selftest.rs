//! The test report: a few checks of what Lilac needs from Spotify, run on
//! the tester's machine and account, saved with the recent log for them
//! to send back. Anonymous (see `diagnostics::anonymise`).

use std::fmt::Write as _;
use std::time::Instant;

use gpui_kit::*;

use crate::app;
use crate::runtime;
use crate::spotify::streaming;

/// Well-known tracks the checks look up (any catalogue would have them).
const SAMPLE_TRACKS: [&str; 3] = [
    "spotify:track:4cOdK2wGLETKBW3PvgPWqT",
    "spotify:track:0VjIjW4GlUZAMYd2vXMi3b",
    "spotify:track:7qiZfU4dY1lWllzX7mPBI3",
];

/// What Lilac knows locally, gathered on the UI thread before the checks.
struct Local {
    account: String,
    engine: String,
    soloist: String,
    library: String,
}

/// Run the checks and save the report; says where when done.
pub fn run(cx: &mut App) {
    app::toast("Running checks for the test report…", cx);
    let local = gather(cx);
    let api = app::api(cx);
    cx.spawn(async move |cx| {
        let checks = runtime::run(async move { Ok(checks(api).await) })
            .await
            .unwrap_or_default();
        let mut body = String::new();
        let _ = writeln!(body, "Account type: {}", local.account);
        let _ = writeln!(body, "Playback: {}", local.engine);
        let _ = writeln!(body, "Soloist: {}", local.soloist);
        let _ = writeln!(body, "Library: {}", local.library);
        let _ = writeln!(body, "\nChecks:");
        body.push_str(&checks);
        let text = crate::diagnostics::test_report(&body);
        let id: String = text
            .lines()
            .next()
            .and_then(|l| l.rsplit(' ').next())
            .unwrap_or("report")
            .to_string();
        let saved =
            crate::diagnostics::save_for_sending(&format!("lilac-test-report-{id}.txt"), &text);
        cx.update(|cx| match saved {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    let _ = open::that_detached(dir);
                }
                app::toast(
                    format!(
                        "Test report saved as {} in your Downloads folder. Send that file back.",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    cx,
                );
            }
            None => app::toast_error("Could not save the test report", cx),
        });
    })
    .detach();
}

fn gather(cx: &App) -> Local {
    let session = app::session(cx).read(cx);
    let account = match session.user.as_ref().and_then(|u| u.product.clone()) {
        Some(product) => product.to_string(),
        None => "unknown".into(),
    };
    let engine = if session.is_free() {
        "previews (Spotify Free)".to_string()
    } else {
        "Soloist".to_string()
    };
    let soloist = app::soloist(cx).read(cx);
    let soloist = match soloist.installed() {
        None => "not installed".into(),
        Some(build) => format!(
            "{} installed{}{}",
            build.version,
            if soloist.has_key() {
                ", key set"
            } else {
                ", no key"
            },
            if soloist.logged_in { ", logged in" } else { "" },
        ),
    };
    let library = app::library(cx).read(cx);
    let library = format!(
        "{} playlists, {} liked songs, {} albums, {} artists, {} podcasts",
        library.playlists.len(),
        library.liked_total,
        library.albums.len(),
        library.artists.len(),
        library.shows.len()
    );
    Local {
        account,
        engine,
        soloist,
        library,
    }
}

/// One line per check: ✓ or ✗, what, how long, and the answer or error.
async fn checks(api: crate::spotify::api::Api) -> String {
    let mut out = String::new();
    let mut line = |ok: bool, what: &str, started: Instant, detail: String| {
        let _ = writeln!(
            out,
            "  {} {what} ({} ms){}",
            if ok { "OK  " } else { "FAIL" },
            started.elapsed().as_millis(),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    };

    let t = Instant::now();
    match api.me().await {
        Ok(user) => line(
            true,
            "Web API (profile)",
            t,
            format!("product {}", user.product.unwrap_or_default()),
        ),
        Err(err) => line(false, "Web API (profile)", t, format!("{err:#}")),
    }

    let t = Instant::now();
    match streaming::connected().await {
        Ok(()) => line(true, "Background connection", t, String::new()),
        Err(err) => line(false, "Background connection", t, format!("{err:#}")),
    }

    let t = Instant::now();
    match streaming::profile().await {
        Ok(user) => line(
            true,
            "Account details (background)",
            t,
            format!("product {}", user.product.unwrap_or_default()),
        ),
        Err(err) => line(false, "Account details (background)", t, format!("{err:#}")),
    }

    let t = Instant::now();
    match streaming::collection("collection").await {
        Ok((items, _)) => line(
            true,
            "Library service",
            t,
            format!("{} saved items", items.len()),
        ),
        Err(err) => line(false, "Library service", t, format!("{err:#}")),
    }

    let t = Instant::now();
    match streaming::rootlist().await {
        Ok(list) => line(
            true,
            "Playlist list",
            t,
            format!("{} playlists", list.len()),
        ),
        Err(err) => line(false, "Playlist list", t, format!("{err:#}")),
    }

    let t = Instant::now();
    match streaming::track(SAMPLE_TRACKS[0]).await {
        Ok(track) => line(
            true,
            "Song details",
            t,
            format!("playable: {}", track.playable()),
        ),
        Err(err) => line(false, "Song details", t, format!("{err:#}")),
    }

    let id = SAMPLE_TRACKS[0].rsplit(':').next().unwrap_or_default();
    let t = Instant::now();
    match streaming::lyrics(id).await {
        Ok(_) => line(true, "Lyrics", t, String::new()),
        Err(err) => line(false, "Lyrics", t, format!("{err:#}")),
    }

    for uri in SAMPLE_TRACKS {
        let t = Instant::now();
        match streaming::preview_probe(uri).await {
            Ok((0, _)) => line(false, "Preview clip", t, "the song has none".into()),
            Ok((files, bytes)) => line(
                true,
                "Preview clip",
                t,
                format!("{files} file(s), first is {} KB", bytes / 1024),
            ),
            Err(err) => line(false, "Preview clip", t, format!("{err:#}")),
        }
    }
    out
}
