//! A horizontal slider used for seeking and volume.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme;

/// What a slider gesture reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SliderChange {
    /// The thumb is held at this value; the gesture is not over yet.
    Preview(f32),
    /// The button was released here.
    Commit(f32),
}

fn ratio(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    if bounds.size.width <= px(0.0) {
        return 0.0;
    }
    ((position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0)
}

/// `on_change` gets values in `0.0..=1.0`: previews while the button is held
/// (including the press itself), then one commit on release.
pub fn slider(
    id: impl Into<ElementId>,
    value: f32,
    on_change: impl Fn(SliderChange, &mut Window, &mut App) + 'static,
) -> Slider {
    Slider {
        id: id.into(),
        value: value.clamp(0.0, 1.0),
        on_change: Rc::new(on_change),
    }
}

type OnChange = Rc<dyn Fn(SliderChange, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Slider {
    id: ElementId,
    value: f32,
    on_change: OnChange,
}

impl RenderOnce for Slider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Held across frames so the gesture survives re-renders; the pointer
        // is tracked window-wide, so leaving the slider mid-drag is fine.
        let held = window.use_keyed_state(self.id.clone(), cx, |_, _| false);
        let dragging = *held.read(cx);
        let value = self.value;
        let on_change = self.on_change;
        let bounds = Rc::new(Cell::new(Bounds::default()));

        div()
            .id(self.id)
            .group("slider")
            .relative()
            .w_full()
            .h(px(16.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                canvas(
                    {
                        let bounds = bounds.clone();
                        move |b, _, _| bounds.set(b)
                    },
                    {
                        let held = held.clone();
                        let on_change = on_change.clone();
                        move |b, _, window, _| {
                            if !dragging {
                                return;
                            }
                            let on_move = on_change.clone();
                            window.on_mouse_event(
                                move |event: &MouseMoveEvent, phase, window, cx| {
                                    if phase == DispatchPhase::Capture && event.dragging() {
                                        on_move(
                                            SliderChange::Preview(ratio(b, event.position)),
                                            window,
                                            cx,
                                        );
                                    }
                                },
                            );
                            let held = held.clone();
                            let on_up = on_change.clone();
                            window.on_mouse_event(
                                move |event: &MouseUpEvent, phase, window, cx| {
                                    if phase == DispatchPhase::Capture
                                        && event.button == MouseButton::Left
                                    {
                                        held.update(cx, |held, cx| {
                                            *held = false;
                                            cx.notify();
                                        });
                                        on_up(
                                            SliderChange::Commit(ratio(b, event.position)),
                                            window,
                                            cx,
                                        );
                                    }
                                },
                            );
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .w_full()
                    .h(px(4.0))
                    .rounded_full()
                    .bg(theme::white(0.16))
                    .child(
                        div()
                            .h_full()
                            .w(relative(value))
                            .rounded_full()
                            .bg(if dragging {
                                theme::accent()
                            } else {
                                theme::text()
                            })
                            .group_hover("slider", |s| s.bg(theme::accent())),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left(relative(value))
                    .ml(px(-6.0))
                    .size(px(12.0))
                    .rounded_full()
                    .bg(theme::text())
                    .shadow(vec![BoxShadow {
                        color: theme::black(0.4),
                        offset: point(px(0.0), px(1.0)),
                        blur_radius: px(4.0),
                        spread_radius: px(0.0),
                        inset: false,
                    }])
                    .when(!dragging, |thumb| {
                        thumb.invisible().group_hover("slider", |s| s.visible())
                    }),
            )
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                held.update(cx, |held, cx| {
                    *held = true;
                    cx.notify();
                });
                on_change(
                    SliderChange::Preview(ratio(bounds.get(), event.position)),
                    window,
                    cx,
                );
                cx.stop_propagation();
            })
    }
}

#[cfg(test)]
mod tests {
    use super::ratio;
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn ratio_is_clamped() {
        let bounds = Bounds::new(point(px(100.0), px(0.0)), size(px(200.0), px(16.0)));
        assert_eq!(ratio(bounds, point(px(50.0), px(0.0))), 0.0);
        assert_eq!(ratio(bounds, point(px(200.0), px(0.0))), 0.5);
        assert_eq!(ratio(bounds, point(px(900.0), px(0.0))), 1.0);
        assert_eq!(ratio(Bounds::default(), point(px(5.0), px(0.0))), 0.0);
    }
}
