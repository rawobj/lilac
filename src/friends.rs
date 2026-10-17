//! Friend activity, refreshed only while its panel is open.

use std::time::Duration;

use gpui_kit::*;

use crate::app;
use crate::runtime;
use crate::spotify::buddylist::{self, Friend};
use crate::ui_state::{RightPanel, UiState};

/// The official client refreshes about this often.
const INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq)]
pub enum Status {
    Loading,
    Ready,
    /// Spotify does not serve friend activity to this login.
    Unavailable,
    Failed(SharedString),
}

pub struct Friends {
    pub list: Vec<Friend>,
    pub status: Status,
    poll: Option<Task<()>>,
}

impl Friends {
    pub fn new(ui: &Entity<UiState>, cx: &mut Context<Self>) -> Self {
        cx.observe(ui, |this, ui, cx| {
            let open = ui.read(cx).right_panel == RightPanel::Friends;
            match (open, this.poll.is_some()) {
                (true, false) => this.start(cx),
                // No requests while nobody is looking.
                (false, true) => this.poll = None,
                _ => {}
            }
        })
        .detach();
        Self {
            list: Vec::new(),
            status: Status::Loading,
            poll: None,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let api = cx.update(|cx| app::api(cx));
                let result = runtime::run(async move { buddylist::fetch(&api).await }).await;
                let unavailable = this
                    .update(cx, |this, cx| {
                        match result {
                            Ok(list) => {
                                this.list = list;
                                this.status = Status::Ready;
                            }
                            Err(err) if err.downcast_ref::<buddylist::Unavailable>().is_some() => {
                                this.status = Status::Unavailable;
                            }
                            // Keep showing the last list through a transient failure.
                            Err(err) if this.list.is_empty() => {
                                this.status = Status::Failed(format!("{err:#}").into());
                            }
                            Err(_) => {}
                        }
                        cx.notify();
                        this.status == Status::Unavailable
                    })
                    .unwrap_or(true);
                if unavailable {
                    break;
                }
                cx.background_executor().timer(INTERVAL).await;
            }
        }));
    }
}
