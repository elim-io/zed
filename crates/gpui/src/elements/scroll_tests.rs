//! Scrolling as a Mac scrolls, through real elements (elim-io fork, native-feel 1/5): the latch over nested areas,
//! the glide of a wheel's notch, the stretch past an edge kept out of the offset.

use crate::{
    self as gpui, AppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollDelta, ScrollHandle, ScrollPhase, ScrollWheelEvent,
    StatefulInteractiveElement as _, Styled as _, TestAppContext, UniformListScrollHandle, Window,
    div, point, px, size, uniform_list,
};
use std::time::Duration;

struct Nested {
    page: ScrollHandle,
    list: ScrollHandle,
}

impl Render for Nested {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // A page 200 tall over 1,000 of content; inside it at the top, a list 100 tall over 500.
        div()
            .id("page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.page)
            .child(
                div()
                    .id("list")
                    .h(px(100.))
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.list)
                    .child(div().h(px(500.)).w_full()),
            )
            .child(div().h(px(900.)).w_full())
    }
}

fn gesture(phase: ScrollPhase, dy: f32, at: f32) -> ScrollWheelEvent {
    ScrollWheelEvent {
        position: point(px(50.), px(at)),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        scroll_phase: phase,
        ..Default::default()
    }
}

#[gpui::test]
fn a_gesture_begun_on_the_list_never_moves_the_page(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let (page, list) = (ScrollHandle::new(), ScrollHandle::new());
    let view = cx.update(|_, cx| {
        cx.new(|_| Nested {
            page: page.clone(),
            list: list.clone(),
        })
    });
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.draw(point(px(0.), px(0.)), size(px(100.), px(200.)), |_, _| {
            view.clone().into_any_element()
        });
    };
    draw(cx);
    cx.simulate_event(gesture(ScrollPhase::Began, 0., 50.));
    for _ in 0..3 {
        cx.simulate_event(gesture(ScrollPhase::Changed, -10., 50.));
        draw(cx);
    }
    assert_eq!(list.offset().y, px(-30.));
    assert_eq!(page.offset().y, px(0.));
    // The pointer leaves the list for the page: the gesture and its momentum stay with the list.
    cx.simulate_event(gesture(ScrollPhase::Changed, -10., 150.));
    cx.simulate_event(gesture(ScrollPhase::Ended, 0., 150.));
    cx.simulate_event(gesture(ScrollPhase::MomentumBegan, -10., 150.));
    cx.simulate_event(gesture(ScrollPhase::Momentum, -10., 150.));
    cx.simulate_event(gesture(ScrollPhase::MomentumEnded, 0., 150.));
    draw(cx);
    assert_eq!(list.offset().y, px(-60.));
    assert_eq!(page.offset().y, px(0.));

    // The list at its end: a gesture pulling further stretches the list, the page stays.
    list.set_offset(point(px(0.), px(-400.)));
    draw(cx);
    cx.simulate_event(gesture(ScrollPhase::Began, 0., 50.));
    for _ in 0..10 {
        cx.simulate_event(gesture(ScrollPhase::Changed, -10., 50.));
        draw(cx);
    }
    assert_eq!(page.offset().y, px(0.));
    assert_eq!(
        list.offset().y,
        px(-400.),
        "the offset stays inside the content"
    );
    assert!(
        list.overscroll().y < px(-10.),
        "stretched: {:?}",
        list.overscroll()
    );
    // The fingers lift: the list springs back and rests within 600 ms.
    cx.simulate_event(gesture(ScrollPhase::Ended, 0., 50.));
    std::thread::sleep(Duration::from_millis(600));
    draw(cx);
    assert_eq!(list.overscroll().y, px(0.));

    // A wheel's notch on the list at its end moves the page (one area an event), and glides there.
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(50.), px(50.)),
        delta: ScrollDelta::Lines(point(0., -3.)),
        ..Default::default()
    });
    draw(cx);
    assert_eq!(list.offset().y, px(-400.));
    let mid = page.offset().y;
    std::thread::sleep(Duration::from_millis(200));
    draw(cx);
    let end = page.offset().y;
    assert!(end < px(0.) && mid > end, "glided from {mid:?} to {end:?}");
}

#[gpui::test]
fn a_stretched_uniform_list_lays_out_from_the_clamped_offset(cx: &mut TestAppContext) {
    struct Rows(UniformListScrollHandle);
    impl Render for Rows {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            uniform_list("rows", 2000, |range, _, _| {
                range
                    .map(|_| div().h(px(20.)).w_full().into_any_element())
                    .collect()
            })
            .track_scroll(&self.0)
            .size_full()
        }
    }
    let cx = cx.add_empty_window();
    let handle = UniformListScrollHandle::new();
    let view = cx.update(|_, cx| cx.new(|_| Rows(handle.clone())));
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.draw(point(px(0.), px(0.)), size(px(100.), px(200.)), |_, _| {
            view.clone().into_any_element()
        });
    };
    draw(cx);
    cx.simulate_event(gesture(ScrollPhase::Began, 0., 50.));
    for _ in 0..20 {
        cx.simulate_event(gesture(ScrollPhase::Changed, 12., 50.));
        draw(cx);
    }
    let base = handle.0.borrow().base_handle.clone();
    assert_eq!(base.offset().y, px(0.));
    assert!(base.overscroll().y > px(20.), "{:?}", base.overscroll());
}

#[gpui::test]
fn a_page_key_glides_by_the_handle(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let (page, list) = (ScrollHandle::new(), ScrollHandle::new());
    let view = cx.update(|_, cx| {
        cx.new(|_| Nested {
            page: page.clone(),
            list: list.clone(),
        })
    });
    let draw = |cx: &mut gpui::VisualTestContext| {
        cx.draw(point(px(0.), px(0.)), size(px(100.), px(200.)), |_, _| {
            view.clone().into_any_element()
        });
    };
    draw(cx);
    cx.update(|window, _| page.glide_by(point(px(0.), px(-160.)), window));
    draw(cx);
    assert!(page.offset().y > px(-160.));
    std::thread::sleep(Duration::from_millis(200));
    draw(cx);
    assert_eq!(page.offset().y, px(-160.));
    // A glide past the end rests at the end.
    cx.update(|window, _| page.glide_by(point(px(0.), px(-5000.)), window));
    std::thread::sleep(Duration::from_millis(200));
    draw(cx);
    draw(cx);
    assert_eq!(page.offset().y, px(-800.));
}
