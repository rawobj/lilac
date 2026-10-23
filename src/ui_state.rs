//! Transient interface state: panels, toasts, menus and dialogs.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::*;

use crate::model::Track;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPanel {
    Hidden,
    NowPlaying,
    Queue,
    Devices,
    Friends,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

pub struct Toast {
    pub id: u64,
    pub text: SharedString,
    pub kind: ToastKind,
}

pub type MenuAction = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct MenuItem {
    pub icon: &'static str,
    pub label: SharedString,
    pub action: MenuAction,
    pub danger: bool,
    pub separator: bool,
}

impl MenuItem {
    pub fn new(
        icon: &'static str,
        label: impl Into<SharedString>,
        action: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            icon,
            label: label.into(),
            action: Rc::new(action),
            danger: false,
            separator: false,
        }
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Draw a divider above this item.
    pub fn separated(mut self) -> Self {
        self.separator = true;
        self
    }
}

pub struct Menu {
    pub position: Point<Pixels>,
    pub items: Vec<MenuItem>,
    /// Bumped whenever the menu is replaced, so stale element state is not reused.
    pub generation: u64,
}

#[derive(Clone, PartialEq)]
pub enum Dialog {
    CreatePlaylist {
        seed: Option<Box<Track>>,
    },
    EditPlaylist {
        id: SharedString,
    },
    DeletePlaylist {
        id: SharedString,
        name: SharedString,
    },
    Shortcuts,
    About,
}

pub struct UiState {
    pub right_panel: RightPanel,
    pub sidebar_collapsed: bool,
    pub toasts: Vec<Toast>,
    pub menu: Option<Menu>,
    pub dialog: Option<Dialog>,
    pub immersive: bool,
    next_id: u64,
}

impl UiState {
    pub fn new(sidebar_collapsed: bool, show_now_playing: bool) -> Self {
        Self {
            right_panel: if show_now_playing {
                RightPanel::NowPlaying
            } else {
                RightPanel::Hidden
            },
            sidebar_collapsed,
            toasts: Vec::new(),
            menu: None,
            dialog: None,
            immersive: false,
            next_id: 0,
        }
    }

    pub fn toggle_panel(&mut self, panel: RightPanel, cx: &mut Context<Self>) {
        self.right_panel = if self.right_panel == panel {
            RightPanel::Hidden
        } else {
            panel
        };
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    pub fn toggle_immersive(&mut self, cx: &mut Context<Self>) {
        self.immersive = !self.immersive;
        cx.notify();
    }

    pub fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.push_toast(text.into(), ToastKind::Info, cx);
    }

    pub fn error(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.push_toast(text.into(), ToastKind::Error, cx);
    }

    fn push_toast(&mut self, text: SharedString, kind: ToastKind, cx: &mut Context<Self>) {
        self.next_id += 1;
        let id = self.next_id;
        self.toasts.push(Toast { id, text, kind });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        cx.notify();

        let lifetime = if kind == ToastKind::Error { 5 } else { 3 };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(lifetime))
                .await;
            this.update(cx, |this, cx| {
                this.toasts.retain(|t| t.id != id);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn open_menu(
        &mut self,
        position: Point<Pixels>,
        items: Vec<MenuItem>,
        cx: &mut Context<Self>,
    ) {
        self.next_id += 1;
        self.menu = Some(Menu {
            position,
            items,
            generation: self.next_id,
        });
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    pub fn open_dialog(&mut self, dialog: Dialog, cx: &mut Context<Self>) {
        self.menu = None;
        self.dialog = Some(dialog);
        cx.notify();
    }

    pub fn close_dialog(&mut self, cx: &mut Context<Self>) {
        if self.dialog.take().is_some() {
            cx.notify();
        }
    }
}
