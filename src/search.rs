//! Search-as-you-type state shared by the top bar field and the results page.

use std::time::Duration;

use gpui_kit::*;

use crate::app;
use crate::model::SearchResults;
use crate::runtime;

const DEBOUNCE: Duration = Duration::from_millis(220);
const TYPES: &str = "track,artist,album,playlist,show,episode";

#[derive(Default)]
pub struct Search {
    pub query: SharedString,
    pub results: Option<SearchResults>,
    pub loading: bool,
    pub error: Option<SharedString>,
    generation: u64,
    pending: Option<Task<()>>,
}

impl Search {
    pub fn set_query(&mut self, query: SharedString, cx: &mut Context<Self>) {
        let trimmed: SharedString = query.trim().to_string().into();
        if trimmed == self.query {
            return;
        }
        self.query = trimmed.clone();
        self.generation += 1;
        self.error = None;

        if trimmed.is_empty() {
            self.pending = None;
            self.results = None;
            self.loading = false;
            cx.notify();
            return;
        }

        self.loading = true;
        cx.notify();

        let generation = self.generation;
        let api = app::api(cx);
        // Replacing the task cancels the previous keystroke's request.
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let result = runtime::run(async move { api.search(&trimmed, TYPES, 10).await }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(results) => this.results = Some(results),
                    Err(err) => this.error = Some(format!("{err}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }
}
