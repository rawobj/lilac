//! A horizontal slider used for seeking and volume.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::*;

use crate::theme;

/// Payload carried while a slider thumb is being dragged.
#[derive(Clone)]
struct SliderDrag(ElementId);

/// Invisible drag preview; the slider itself is the feedback.
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn ratio(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    if bounds.size.width <= px(0.0) {
        return 0.0;
    }
    ((position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0)
}

/// `on_change` receives the new value in `0.0..=1.0` and whether it came from
/// a drag (`true`) or a click (`false`).
pub fn slider(
    id: impl Into<ElementId>,
    value: f32,
    on_change: impl Fn(f32, bool, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id: ElementId = id.into();
    let value = value.clamp(0.0, 1.0);
    let on_change = Rc::new(on_change);
    let bounds = Rc::new(Cell::new(Bounds::default()));

    div()
        .id(id.clone())
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
                |_, _, _, _| {},
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
                        .bg(theme::text())
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
                .invisible()
                .group_hover("slider", |s| s.visible()),
        )
        .on_mouse_down(MouseButton::Left, {
            let on_change = on_change.clone();
            let bounds = bounds.clone();
            move |event, window, cx| {
                on_change(ratio(bounds.get(), event.position), false, window, cx);
            }
        })
        .on_drag(SliderDrag(id.clone()), |_, _, _, cx| cx.new(|_| DragGhost))
        .on_drag_move::<SliderDrag>(move |event, window, cx| {
            if event.drag(cx).0 == id {
                on_change(ratio(event.bounds, event.event.position), true, window, cx);
            }
        })
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
