//! Details about artists, shared by the Now Playing panel and artist pages:
//! the Web API's profile (picture, followers, genres) and what the
//! streaming session adds (top tracks, appearances, similar artists,
//! biography). Fetched once per artist and kept for a while.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde_json::json;

use gpui_kit::*;

use crate::app;
use crate::disk_cache;
use crate::model::Artist;
use crate::runtime;
use crate::spotify::streaming::{self, ArtistExtras, ArtistOverview, Credits};

/// Artists kept; one page or panel shows one at a time.
const KEPT: usize = 24;
/// Opening an artist shows what is kept and asks Spotify again behind it,
/// unless it was asked this recently (going back and forth, or the Now
/// Playing panel on every song by them, costs nothing).
const RECHECK: Duration = Duration::from_secs(10 * 60);

/// An artist as kept on disk, with its age.
fn from_disk(id: &str) -> Option<(ArtistDetails, Duration)> {
    let hit = disk_cache::ARTISTS.get(id)?;
    let value = hit.value;
    if value["lang"].as_str()? != streaming::language() {
        return None;
    }
    let details = ArtistDetails {
        profile: Some(serde_json::from_value(value["profile"].clone()).ok()?),
        extras: ArtistExtras::from_json(&value["extras"]),
        overview: serde_json::from_value(value["overview"].clone()).ok(),
        overview_error: None,
        overview_done: true,
        profile_done: true,
        extras_done: true,
    };
    Some((details, hit.age))
}

#[derive(Default, Clone)]
pub struct ArtistDetails {
    pub profile: Option<Artist>,
    pub extras: Option<ArtistExtras>,
    /// Listeners, links, tour dates: from Spotify's web player; may fail.
    pub overview: Option<ArtistOverview>,
    pub overview_error: Option<SharedString>,
    pub overview_done: bool,
    /// Both lookups have answered (whatever they said).
    pub profile_done: bool,
    pub extras_done: bool,
}

#[derive(Default)]
pub struct Artists {
    entries: HashMap<SharedString, ArtistDetails>,
    order: VecDeque<SharedString>,
    credits: HashMap<SharedString, Option<Credits>>,
    /// When each artist was last asked about in this run.
    checked: HashMap<SharedString, std::time::Instant>,
}

impl Artists {
    pub fn get(&self, id: &str) -> Option<&ArtistDetails> {
        self.entries.get(id)
    }

    pub fn credits(&self, track_id: &str) -> Option<&Credits> {
        self.credits.get(track_id).and_then(Option::as_ref)
    }

    /// Start fetching `id` unless it is known or on its way. A copy kept on
    /// disk shows at once; Spotify is asked again only when it is older
    /// than [`RECHECK`], and what it answers replaces the copy.
    pub fn ensure(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        if id.is_empty() {
            return;
        }
        if let Some(entry) = self.entries.get(id) {
            let settled = entry.profile_done && entry.extras_done && entry.overview_done;
            let stale = self
                .checked
                .get(id)
                .is_none_or(|at| at.elapsed() >= RECHECK);
            if settled && stale {
                self.fetch(id.clone(), cx);
            }
            return;
        }
        self.entries.insert(id.clone(), ArtistDetails::default());
        self.order.push_back(id.clone());
        while self.order.len() > KEPT {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }

        let demo = app::session(cx).read(cx).is_demo();
        let key = id.clone();
        cx.spawn(async move |this, cx| {
            let lookup = key.to_string();
            let kept = if demo {
                None
            } else {
                cx.background_executor()
                    .spawn(async move { from_disk(&lookup) })
                    .await
            };
            this.update(cx, |this, cx| {
                let fresh = kept.as_ref().is_some_and(|(_, age)| *age < RECHECK);
                if let Some((_, age)) = &kept
                    && let Some(at) = std::time::Instant::now().checked_sub(*age)
                {
                    this.checked.insert(key.clone(), at);
                }
                if let Some((kept, _)) = kept
                    && let Some(entry) = this.entries.get_mut(&key)
                {
                    *entry = kept;
                    cx.notify();
                }
                if !fresh {
                    this.fetch(key, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn fetch(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.checked.insert(id.clone(), std::time::Instant::now());
        if self.checked.len() > KEPT * 4 {
            self.checked.retain(|_, at| at.elapsed() < RECHECK);
        }
        let api = app::api(cx);
        let fetch = id.to_string();
        let key = id.clone();
        cx.spawn(async move |this, cx| {
            // The session first; the Web API (rate limited per app) only
            // when it fails.
            let profile = runtime::run(async move {
                match streaming::artist_profile(&fetch).await {
                    Ok(profile) => Ok(profile),
                    Err(_) => api.artist(&fetch).await,
                }
            })
            .await
            .ok();
            this.update(cx, |this, cx| {
                if let Some(entry) = this.entries.get_mut(&key) {
                    // A failed check keeps the copy already shown.
                    if profile.is_some() {
                        entry.profile = profile;
                    }
                    entry.profile_done = true;
                    this.persist(&key, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();

        self.fetch_overview(id.clone(), cx);

        let fetch = id.to_string();
        let key = id.clone();
        cx.spawn(async move |this, cx| {
            let extras = runtime::run(async move { streaming::artist(&fetch).await })
                .await
                .ok();
            this.update(cx, |this, cx| {
                if let Some(entry) = this.entries.get_mut(&key) {
                    if extras.is_some() {
                        entry.extras = extras;
                    }
                    entry.extras_done = true;
                    this.persist(&key, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keep an artist on disk once every lookup has answered.
    fn persist(&self, id: &SharedString, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(id) else {
            return;
        };
        // Without a profile the page cannot show; that is not worth keeping.
        if !(entry.profile_done && entry.extras_done && entry.overview_done)
            || entry.profile.is_none()
            || app::session(cx).read(cx).is_demo()
        {
            return;
        }
        let value = json!({
            "lang": streaming::language(),
            "profile": entry.profile.as_ref().map(Artist::to_api_json),
            "extras": entry.extras.as_ref().map(ArtistExtras::to_json),
            "overview": entry.overview.as_ref().and_then(|o| serde_json::to_value(o).ok()),
        });
        let key = id.to_string();
        cx.background_executor()
            .spawn(async move { disk_cache::ARTISTS.put(&key, &value) })
            .detach();
    }

    /// Ask again for an overview that failed.
    pub fn retry_overview(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.overview_done = false;
            entry.overview_error = None;
            cx.notify();
            self.fetch_overview(id.clone(), cx);
        }
    }

    fn fetch_overview(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let fetch = id.to_string();
        let login = app::session(cx).read(cx).streaming.clone();
        cx.spawn(async move |this, cx| {
            let result =
                runtime::run(async move { streaming::artist_overview(&fetch, &login).await }).await;
            this.update(cx, |this, cx| {
                if let Some(entry) = this.entries.get_mut(&id) {
                    match result {
                        Ok(overview) => {
                            entry.overview = Some(overview);
                            entry.overview_error = None;
                        }
                        // A kept overview stays up; the error shows only
                        // when there is nothing else.
                        Err(err) if entry.overview.is_none() => {
                            entry.overview_error = Some(format!("{err:#}").into())
                        }
                        Err(_) => {}
                    }
                    entry.overview_done = true;
                    this.persist(&id, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Start fetching the credits of a track (best effort).
    pub fn ensure_credits(&mut self, track_id: &SharedString, cx: &mut Context<Self>) {
        if track_id.is_empty() || self.credits.contains_key(track_id) {
            return;
        }
        if self.credits.len() > KEPT {
            self.credits.clear();
        }
        self.credits.insert(track_id.clone(), None);
        let (fetch, key) = (track_id.to_string(), track_id.clone());
        cx.spawn(async move |this, cx| {
            let credits = runtime::run(async move { streaming::credits(&fetch).await })
                .await
                .ok();
            this.update(cx, |this, cx| {
                this.credits.insert(key, credits);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
