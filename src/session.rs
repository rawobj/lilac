//! Login state and the authenticated API handle.

use std::sync::Arc;

use gpui_kit::*;

use crate::model::User;
use crate::runtime;
use crate::spotify::api::{Api, Auth, status_of};
use crate::spotify::oauth::{self, Client, Credentials};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Stored credentials are being verified.
    Starting,
    LoggedOut,
    /// The browser is open and we are waiting for the redirect.
    WaitingForBrowser,
    LoggedIn,
}

pub enum SessionEvent {
    /// Fired once the user is known and the API is usable.
    Ready,
    LoggedOut,
}

pub struct Session {
    pub status: Status,
    /// Web API access through lilac's own client.
    pub api: Api,
    /// Tokens of the desktop client for the streaming session.
    pub streaming: Arc<Auth>,
    /// What the browser is being asked for during the two-step login.
    pub login_step: Option<SharedString>,
    pub user: Option<User>,
    pub error: Option<SharedString>,
    login: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (web, streaming) = auths();
        let has_credentials = web.has_credentials() && streaming.has_credentials();
        let api = Api::new(web);
        let mut this = Self {
            status: if has_credentials {
                Status::Starting
            } else {
                Status::LoggedOut
            },
            api,
            streaming,
            login_step: None,
            user: None,
            error: None,
            login: None,
        };
        if std::env::args().any(|arg| arg == "--demo") {
            this.status = Status::Starting;
            this.enter_demo(cx);
        } else if has_credentials {
            this.fetch_profile(cx);
        }
        this
    }

    pub fn is_demo(&self) -> bool {
        self.api.is_demo()
    }

    /// Browse a bundled sample library without an account. Nothing is played.
    pub fn enter_demo(&mut self, cx: &mut Context<Self>) {
        self.login = None;
        self.error = None;
        self.api = Api::demo();
        self.fetch_profile(cx);
    }

    /// Start the browser login: the desktop client for streaming, then
    /// lilac's own client for the Web API.
    pub fn login(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.status = Status::WaitingForBrowser;
        self.login_step = Some("Step 1 of 2: allow playback".into());
        cx.notify();

        let web = self.api.auth.clone();
        let streaming = self.streaming.clone();
        self.login = Some(cx.spawn(async move |this, cx| {
            let result = runtime::run(async move {
                let credentials = oauth::login(&streaming.client).await?;
                credentials.save(&streaming.client)?;
                streaming.set(Some(credentials)).await;
                Ok(())
            })
            .await;
            let next = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.login_step = Some("Step 2 of 2: allow access to your library".into());
                    cx.notify();
                    true
                }
                Err(err) => {
                    this.fail(err, cx);
                    false
                }
            });
            if !matches!(next, Ok(true)) {
                return;
            }

            let result = runtime::run(async move {
                let credentials = oauth::login(&web.client).await?;
                credentials.save(&web.client)?;
                web.set(Some(credentials)).await;
                Ok(())
            })
            .await;

            this.update(cx, |this, cx| {
                this.login_step = None;
                match result {
                    Ok(()) => this.fetch_profile(cx),
                    Err(err) => this.fail(err, cx),
                }
            })
            .ok();
        }));
    }

    pub fn cancel_login(&mut self, cx: &mut Context<Self>) {
        self.login = None;
        self.login_step = None;
        oauth::cancel_login();
        self.status = Status::LoggedOut;
        cx.notify();
    }

    pub fn logout(&mut self, cx: &mut Context<Self>) {
        if self.is_demo() {
            // Leaving the demo must not touch a real stored login.
            let (web, streaming) = auths();
            self.api = Api::new(web);
            self.streaming = streaming;
        } else {
            Credentials::forget_all();
            crate::playlist_cache::clear();
            crate::disk_cache::clear();
            let _ = std::fs::remove_file(profile_file());
            let (web, streaming) = (self.api.auth.clone(), self.streaming.clone());
            runtime::spawn(async move {
                web.set(None).await;
                streaming.set(None).await;
            });
        }
        self.user = None;
        self.login = None;
        self.status = Status::LoggedOut;
        cx.emit(SessionEvent::LoggedOut);
        cx.notify();
    }

    fn fail(&mut self, err: anyhow::Error, cx: &mut Context<Self>) {
        self.error = Some(format!("{err:#}").into());
        self.status = Status::LoggedOut;
        cx.notify();
    }

    /// With a profile kept from last time, Lilac starts at once and checks
    /// it behind the scenes; the first sign-in waits for it.
    fn fetch_profile(&mut self, cx: &mut Context<Self>) {
        if let Some(user) = load_profile().filter(|u| !u.id.is_empty()) {
            // Announced on the next tick: this runs from `new`, before the
            // player and library have subscribed.
            cx.spawn(async move |this, cx| {
                this.update(cx, |this, cx| {
                    this.user = Some(user);
                    this.status = Status::LoggedIn;
                    this.error = None;
                    cx.emit(SessionEvent::Ready);
                    cx.notify();
                    this.refresh_profile(cx);
                })
                .ok();
            })
            .detach();
            return;
        }
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.me().await }).await;
            this.update(cx, |this, cx| match result {
                Ok(user) => {
                    save_profile(&user);
                    this.user = Some(user);
                    this.status = Status::LoggedIn;
                    this.error = None;
                    cx.emit(SessionEvent::Ready);
                    cx.notify();
                }
                Err(err) => {
                    // Only a rejected token means the login is gone.
                    if matches!(status_of(&err), Some(400 | 401 | 403)) {
                        Credentials::forget_all();
                        this.fail(err, cx);
                        return;
                    }
                    // Offline or rate limited: stay signed in and say what
                    // is going on; the profile fills in once Spotify answers.
                    this.user = Some(Default::default());
                    this.status = Status::LoggedIn;
                    this.error = None;
                    cx.emit(SessionEvent::Ready);
                    crate::app::toast_error(format!("{err:#}"), cx);
                    cx.notify();
                    this.refresh_profile(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Ask Spotify for the profile again, behind what is shown.
    fn refresh_profile(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.me().await }).await;
            this.update(cx, |this, cx| match result {
                Ok(user) => {
                    if this.status != Status::LoggedIn || this.is_demo() {
                        return;
                    }
                    save_profile(&user);
                    this.user = Some(user);
                    cx.notify();
                }
                Err(err) if matches!(status_of(&err), Some(401)) => {
                    // The login was revoked while Lilac kept the old profile.
                    Credentials::forget_all();
                    let _ = std::fs::remove_file(profile_file());
                    this.user = None;
                    this.fail(err, cx);
                    cx.emit(SessionEvent::LoggedOut);
                }
                Err(_) => {}
            })
            .ok();
        })
        .detach();
    }
}

/// Auth for the Web API and for streaming, from whatever is stored.
fn auths() -> (Arc<Auth>, Arc<Auth>) {
    (
        Arc::new(Auth::load(Client::web())),
        Arc::new(Auth::load(Client::streaming())),
    )
}

fn profile_file() -> std::path::PathBuf {
    crate::paths::config_dir().join("profile.json")
}

/// The signed-in profile, kept for starts when Spotify cannot be asked.
fn save_profile(user: &crate::model::User) {
    let value = serde_json::json!({
        "id": user.id.as_ref(),
        "display_name": user.display_name.as_ref().map(|n| n.as_ref()),
        "images": user.images.to_api_json(),
        "product": user.product.as_ref().map(|p| p.as_ref()),
        "country": user.country.as_ref().map(|c| c.as_ref()),
    });
    if let Ok(raw) = serde_json::to_vec(&value) {
        let _ = crate::paths::write_private(&profile_file(), &raw);
    }
}

fn load_profile() -> Option<crate::model::User> {
    let raw = std::fs::read(profile_file()).ok()?;
    serde_json::from_slice(&raw).ok()
}
