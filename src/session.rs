//! Login state and the authenticated API handle.

use std::sync::Arc;

use gpui_kit::*;

use crate::model::User;
use crate::runtime;
use crate::spotify::api::{Api, Auth, status_of};
use crate::spotify::oauth::{self, Credentials};

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
    pub api: Api,
    pub user: Option<User>,
    pub error: Option<SharedString>,
    login: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let credentials = Credentials::load();
        let has_credentials = credentials.is_some();
        let api = Api::new(Arc::new(Auth::new(credentials)));
        let mut this = Self {
            status: if has_credentials {
                Status::Starting
            } else {
                Status::LoggedOut
            },
            api,
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

    /// Start the browser login.
    pub fn login(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.status = Status::WaitingForBrowser;
        cx.notify();

        let auth = self.api.auth.clone();
        self.login = Some(cx.spawn(async move |this, cx| {
            let result = runtime::run(async move {
                let credentials = oauth::login().await?;
                credentials.save()?;
                auth.set(Some(credentials)).await;
                Ok(())
            })
            .await;

            this.update(cx, |this, cx| match result {
                Ok(()) => this.fetch_profile(cx),
                Err(err) => this.fail(err, cx),
            })
            .ok();
        }));
    }

    pub fn cancel_login(&mut self, cx: &mut Context<Self>) {
        self.login = None;
        oauth::cancel_login();
        self.status = Status::LoggedOut;
        cx.notify();
    }

    pub fn logout(&mut self, cx: &mut Context<Self>) {
        if self.is_demo() {
            // Leaving the demo must not touch a real stored login.
            self.api = Api::new(Arc::new(Auth::new(Credentials::load())));
        } else {
            Credentials::forget();
            let auth = self.api.auth.clone();
            runtime::spawn(async move { auth.set(None).await });
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

    fn fetch_profile(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.me().await }).await;
            this.update(cx, |this, cx| match result {
                Ok(user) => {
                    this.user = Some(user);
                    this.status = Status::LoggedIn;
                    this.error = None;
                    cx.emit(SessionEvent::Ready);
                    cx.notify();
                }
                Err(err) => {
                    // Only a rejected token means the login is gone. Being offline does not.
                    if matches!(status_of(&err), Some(400 | 401 | 403)) {
                        Credentials::forget();
                    }
                    this.fail(err, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
