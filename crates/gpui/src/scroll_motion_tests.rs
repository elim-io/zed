use super::*;
use crate::size;

fn cand(key: AreaKey, y_max: f32, y_off: f32) -> Candidate {
    Candidate {
        key,
        scrolls: point(false, true),
        max: point(px(0.), px(y_max)),
        offset: point(px(0.), px(y_off)),
        both: false,
        map_wheel: true,
    }
}

fn offer(r: &mut ScrollRouter, cs: &[Candidate]) {
    r.begin_event();
    for c in cs {
        r.offer(*c);
    }
}

#[test]
fn a_gesture_stays_with_the_area_it_began_in() {
    let mut r = ScrollRouter::default();
    let page = cand(1, 1000., -100.);
    let list = cand(2, 400., 0.);
    offer(&mut r, &[page, list]);
    assert_eq!(r.take(2, ScrollPhase::Began, point(px(0.), px(0.))), None);
    offer(&mut r, &[page, list]);
    assert!(
        r.take(2, ScrollPhase::Changed, point(px(0.), px(-5.)))
            .is_some()
    );
    assert_eq!(
        r.latched(),
        Some(Route {
            key: 2,
            axis: Axis::Y
        })
    );
    // The page under the pointer gets nothing; the list keeps the events when the pointer left it.
    offer(&mut r, &[page]);
    assert_eq!(
        r.take(1, ScrollPhase::Changed, point(px(0.), px(-5.))),
        None
    );
    assert!(
        r.take(2, ScrollPhase::Changed, point(px(0.), px(-5.)))
            .is_some()
    );
    // Its momentum too, after the pointer moved.
    offer(&mut r, &[page]);
    assert_eq!(
        r.take(1, ScrollPhase::Momentum, point(px(0.), px(-3.))),
        None
    );
    assert!(
        r.take(2, ScrollPhase::Momentum, point(px(0.), px(-3.)))
            .is_some()
    );
    offer(&mut r, &[page]);
    r.take(2, ScrollPhase::MomentumEnded, Point::default());
    assert_eq!(r.latched(), None);
}

#[test]
fn a_list_at_its_end_keeps_the_gesture() {
    let mut r = ScrollRouter::default();
    let page = cand(1, 1000., -100.);
    let list = cand(2, 400., -400.);
    offer(&mut r, &[page, list]);
    // Pulling further past the list's end: the list stretches, the page does not move.
    let d = r.take(2, ScrollPhase::Changed, point(px(0.), px(-8.)));
    assert_eq!(d, Some(point(px(0.), px(-8.))));
    assert_eq!(
        r.take(1, ScrollPhase::Changed, point(px(0.), px(-8.))),
        None
    );
}

#[test]
fn an_area_that_fits_leaves_the_gesture_to_the_one_that_moves() {
    let mut r = ScrollRouter::default();
    let page = cand(1, 1000., 0.);
    let fits = cand(2, 0., 0.);
    offer(&mut r, &[page, fits]);
    assert!(
        r.take(1, ScrollPhase::Changed, point(px(0.), px(-4.)))
            .is_some()
    );
    assert_eq!(r.latched().map(|l| l.key), Some(1));
}

#[test]
fn a_gesture_keeps_its_main_axis() {
    let mut r = ScrollRouter::default();
    let both = Candidate {
        scrolls: point(true, true),
        max: point(px(300.), px(300.)),
        ..cand(1, 0., 0.)
    };
    offer(&mut r, &[both]);
    let d = r
        .take(1, ScrollPhase::Changed, point(px(-2.), px(-9.)))
        .unwrap();
    assert_eq!(d, point(px(0.), px(-9.)));
    offer(&mut r, &[both]);
    // Later events lean sideways: the gesture stays vertical.
    let d = r
        .take(1, ScrollPhase::Changed, point(px(-9.), px(-2.)))
        .unwrap();
    assert_eq!(d, point(px(0.), px(-2.)));
    // A canvas takes both.
    let mut r = ScrollRouter::default();
    let canvas = Candidate { both: true, ..both };
    offer(&mut r, &[canvas]);
    let d = r
        .take(1, ScrollPhase::Changed, point(px(-2.), px(-9.)))
        .unwrap();
    assert_eq!(d, point(px(-2.), px(-9.)));
}

#[test]
fn a_wheel_moves_the_innermost_area_that_can_move_its_way_and_only_one() {
    let mut r = ScrollRouter::default();
    let page = cand(1, 1000., -100.);
    let list = cand(2, 400., -400.);
    offer(&mut r, &[page, list]);
    // Down past the list's end: the page takes it.
    assert_eq!(r.take(2, ScrollPhase::None, point(px(0.), px(-20.))), None);
    assert!(
        r.take(1, ScrollPhase::None, point(px(0.), px(-20.)))
            .is_some()
    );
    // Up: the list can move, the page gets nothing.
    offer(&mut r, &[page, list]);
    assert!(
        r.take(2, ScrollPhase::None, point(px(0.), px(20.)))
            .is_some()
    );
    assert_eq!(r.take(1, ScrollPhase::None, point(px(0.), px(20.))), None);
}

#[test]
fn a_notch_glides_to_its_target_and_the_sum_is_the_jump() {
    let mut r = ScrollRouter::default();
    let t0 = Instant::now();
    let max = point(px(0.), px(1000.));
    let sz = size(px(300.), px(400.));
    let mut o = Point::default();
    for i in 0..5 {
        let at = t0 + Duration::from_millis(40 * i);
        r.step(9, &mut o, max, at);
        r.apply(
            9,
            &mut o,
            max,
            sz,
            point(px(0.), px(-40.)),
            true,
            ScrollPhase::None,
            at,
        );
    }
    // Mid-glide the offset is between; at the end it is the five jumps.
    let (_, moving) = r.step(9, &mut o, max, t0 + Duration::from_millis(200));
    assert!(moving && o.y > px(-200.) && o.y < px(0.));
    let (_, moving) = r.step(9, &mut o, max, t0 + Duration::from_millis(400));
    assert!(!moving);
    assert_eq!(o.y, px(-200.));
    // A glide stops at the content's end.
    r.apply(
        9,
        &mut o,
        max,
        sz,
        point(px(0.), px(-5000.)),
        true,
        ScrollPhase::None,
        t0,
    );
    r.step(9, &mut o, max, t0 + Duration::from_secs(1));
    assert_eq!(o.y, px(-1000.));
}

#[test]
fn a_programmes_move_cancels_a_glide() {
    let mut r = ScrollRouter::default();
    let t0 = Instant::now();
    let max = point(px(0.), px(1000.));
    let mut o = Point::default();
    r.apply(
        3,
        &mut o,
        max,
        size(px(1.), px(400.)),
        point(px(0.), px(-120.)),
        true,
        ScrollPhase::None,
        t0,
    );
    r.step(3, &mut o, max, t0 + Duration::from_millis(30));
    o.y = px(-700.); // set_offset, scroll_to_item
    let (_, moving) = r.step(3, &mut o, max, t0 + Duration::from_millis(60));
    assert!(!moving);
    assert_eq!(o.y, px(-700.));
}

#[test]
fn the_band_is_apples_curve_and_the_spring_rests_in_half_a_second() {
    let d = 400.;
    assert!((rubber(100., d) - (1. - 1. / (100. * 0.55 / d + 1.)) * d).abs() < 1e-3);
    assert!(rubber(10_000., d) < d);
    // Small steps draw the same curve as one long drag.
    let (mut o, mut s) = (Pixels::ZERO, Pixels::ZERO);
    for _ in 0..100 {
        elastic(&mut o, &mut s, px(1.), px(500.), px(d));
    }
    let s: f32 = s.into();
    assert!(
        (s - rubber(100., d)).abs() < 1.5,
        "{s} against {}",
        rubber(100., d)
    );
    assert_eq!(o, Pixels::ZERO);
    // The spring: at rest within 500 ms from a large stretch, stepped at 120 Hz.
    let (mut x, mut v, mut t) = (150f32, 0f32, 0f32);
    while x != 0. {
        (x, v) = spring(x, v, 1. / 120.);
        t += 1. / 120.;
        assert!(x >= 0., "no overshoot");
    }
    assert!(t <= 0.5, "rested after {t} s");
}

#[test]
fn a_stretch_is_paint_only_and_springs_back_after_the_fingers_lift() {
    let mut r = ScrollRouter::default();
    let t0 = Instant::now();
    let max = point(px(0.), px(1000.));
    let sz = size(px(300.), px(400.));
    let mut o = Point::default();
    for i in 0..20 {
        let at = t0 + Duration::from_millis(8 * i);
        r.apply(
            5,
            &mut o,
            max,
            sz,
            point(px(0.), px(8.)),
            false,
            ScrollPhase::Changed,
            at,
        );
    }
    // The offset stays inside the content; the stretch is drawn.
    assert_eq!(o, Point::default());
    let pulled = r.stretch(5).y;
    assert!(pulled > px(40.) && pulled < px(400.), "{pulled:?}");
    // Held: no frames asked.
    let (_, moving) = r.step(5, &mut o, max, t0 + Duration::from_millis(170));
    assert!(!moving);
    let lift = t0 + Duration::from_millis(180);
    r.apply(
        5,
        &mut o,
        max,
        sz,
        Point::default(),
        false,
        ScrollPhase::Ended,
        lift,
    );
    let mut t = lift;
    let mut frames = 0;
    loop {
        t += Duration::from_micros(8333);
        frames += 1;
        let (s, moving) = r.step(5, &mut o, max, t);
        assert!(s.y >= Pixels::ZERO && o == Point::default());
        if !moving {
            break;
        }
    }
    assert!(
        t - lift <= Duration::from_millis(500) && frames >= 20,
        "{frames} frames"
    );
    // A content that fits never stretches.
    let mut o = Point::default();
    r.apply(
        6,
        &mut o,
        Point::default(),
        sz,
        point(px(0.), px(30.)),
        false,
        ScrollPhase::Changed,
        t0,
    );
    assert_eq!(r.stretch(6), Point::default());
}

#[test]
fn a_wheel_on_an_area_that_fits_moves_nothing() {
    let mut r = ScrollRouter::default();
    let c = Candidate {
        map_wheel: false,
        offset: point(px(0.), px(-0.)),
        ..cand(1, 0., 0.)
    };
    offer(&mut r, &[c]);
    assert_eq!(r.take(1, ScrollPhase::None, point(px(0.), px(100.))), None);
}
