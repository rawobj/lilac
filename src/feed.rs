//! "What's New": recent releases from followed artists and new episodes of
//! followed shows.
//!
//! Spotify builds this feed server-side for its own clients only, so it is
//! assembled here: the followed artists and shows come from the library
//! Lilac already has, the artists' releases from the streaming session in
//! two batched lookups. Only new episodes of shows ask the Web API, one
//! request per show. Loading starts a little after login, not at startup.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::*;

use crate::app;
use crate::model::{Album, Track};
use crate::runtime;
use crate::session::{Session, SessionEvent};

/// How far back the feed reaches.
const WINDOW_DAYS: i64 = 28;
const MAX_ARTISTS: usize = 40;
const MAX_SHOWS: usize = 20;
const MAX_ITEMS: usize = 60;
/// Wait this long after login so the first screen is not competing for the network.
const START_DELAY: Duration = Duration::from_secs(15);
/// Requests in flight at once while building the feed.
const PARALLEL: usize = 4;

#[derive(Clone)]
pub enum FeedItem {
    Release(Album),
    Episode(Track),
}

impl FeedItem {
    /// `YYYY-MM-DD`, or a coarser prefix when the API only knows the year or month.
    pub fn date(&self) -> &str {
        match self {
            FeedItem::Release(album) => &album.release_date,
            FeedItem::Episode(episode) => episode.release_date.as_deref().unwrap_or(""),
        }
    }

    pub fn is_episode(&self) -> bool {
        matches!(self, FeedItem::Episode(_))
    }
}

pub struct Feed {
    pub items: Vec<FeedItem>,
    pub loaded: bool,
    task: Option<Task<()>>,
}

impl Feed {
    pub fn new(session: &Entity<Session>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(session, |this, _, event, cx| match event {
            SessionEvent::Ready => this.schedule(START_DELAY, cx),
            SessionEvent::LoggedOut => {
                this.items.clear();
                this.loaded = false;
                this.task = None;
                cx.notify();
            }
        })
        .detach();
        Self {
            items: Vec::new(),
            loaded: false,
            task: None,
        }
    }

    /// Items newer than the last time the user opened the page.
    pub fn unseen(&self, seen: &str) -> usize {
        self.items.iter().filter(|i| i.date() > seen).count()
    }

    pub fn newest_date(&self) -> Option<&str> {
        self.items.first().map(FeedItem::date)
    }

    /// Load now unless a load already ran or is running.
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if !self.loaded && self.task.is_none() {
            self.schedule(Duration::ZERO, cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.schedule(Duration::ZERO, cx);
    }

    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let api = app::api(cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let demo = api.is_demo();
            // What the library knows; the Web API only before it has loaded.
            let known = cx
                .update(|cx| {
                    let library = app::library(cx).read(cx);
                    library.loaded.then(|| {
                        let artists: Vec<String> =
                            library.artists.iter().map(|a| a.id.to_string()).collect();
                        (artists, library.shows.clone())
                    })
                })
                .filter(|_| !demo);
            let result = runtime::run(async move {
                let cutoff = (!demo).then(|| date_days_ago(WINDOW_DAYS));
                Ok(build(api, known, cutoff).await)
            })
            .await;
            this.update(cx, |this, cx| {
                this.task = None;
                this.loaded = true;
                if let Ok(items) = result {
                    this.items = items;
                }
                cx.notify();
            })
            .ok();
        }));
    }
}

async fn build(
    api: crate::spotify::api::Api,
    known: Option<(Vec<String>, Vec<crate::model::Show>)>,
    cutoff: Option<String>,
) -> Vec<FeedItem> {
    let (artists, shows) = match known {
        Some(known) => known,
        None => {
            let (artists, shows) = tokio::join!(api.followed_artists(), api.saved_shows());
            let artists = artists
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.id.to_string())
                .collect();
            (artists, shows.unwrap_or_default())
        }
    };
    let recent = |date: &str| cutoff.as_deref().is_none_or(|cutoff| date >= cutoff);
    let artists: Vec<String> = artists.into_iter().take(MAX_ARTISTS).collect();

    let mut items = Vec::new();
    // Releases from the streaming session; one Web API request per artist
    // only when that fails.
    let mut jobs: Vec<Job> = Vec::new();
    match crate::spotify::streaming::latest_releases(&artists).await {
        Ok(albums) if !api.is_demo() => items.extend(
            albums
                .into_iter()
                .map(FeedItem::Release)
                .filter(|item| recent(item.date())),
        ),
        _ => jobs.extend(artists.into_iter().map(|id| Job::Artist(id.into()))),
    }
    jobs.extend(shows.into_iter().take(MAX_SHOWS).map(Job::Show));
    let mut tasks = tokio::task::JoinSet::new();
    let mut jobs = jobs.into_iter();
    let spawn = |tasks: &mut tokio::task::JoinSet<Vec<FeedItem>>, job: Job| {
        let api = api.clone();
        tasks.spawn(async move {
            match job {
                Job::Artist(id) => api
                    .latest_releases(&id)
                    .await
                    .map(|albums| albums.into_iter().map(FeedItem::Release).collect())
                    .unwrap_or_default(),
                Job::Show(show) => api
                    .show_episodes(&show, 0)
                    .await
                    .map(|page| {
                        page.items
                            .into_iter()
                            .take(5)
                            .map(FeedItem::Episode)
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        });
    };
    for job in jobs.by_ref().take(PARALLEL) {
        spawn(&mut tasks, job);
    }
    while let Some(done) = tasks.join_next().await {
        if let Ok(found) = done {
            items.extend(found.into_iter().filter(|item| recent(item.date())));
        }
        if let Some(job) = jobs.next() {
            spawn(&mut tasks, job);
        }
    }

    // Newest first; an album that appears for several followed artists is listed once.
    items.sort_by(|a, b| b.date().cmp(a.date()));
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| match item {
        FeedItem::Release(album) => seen.insert(album.id.clone()),
        FeedItem::Episode(episode) => seen.insert(episode.id.clone()),
    });
    items.truncate(MAX_ITEMS);
    items
}

enum Job {
    Artist(SharedString),
    Show(crate::model::Show),
}

/// The UTC calendar date `days` ago, as `YYYY-MM-DD`.
fn date_days_ago(days: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(now / 86_400 - days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_726), (2026, 9, 30));
    }

    #[test]
    fn partial_dates_compare_sensibly() {
        // A release known only by year sorts before any full date in that year,
        // so it drops out of a window that starts mid-year.
        assert!("2026" < "2026-10-01");
        assert!("2026-10-02" >= "2026-10-01");
    }
}
