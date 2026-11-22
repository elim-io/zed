//! Scrolling as a Mac scrolls (elim-io fork, Ensemble native-feel 1/5).
//!
//! - One gesture, one area, one axis: a trackpad gesture and its momentum go to the scroll area chosen at the
//!   gesture's first moving event (the innermost under the pointer that can move on the gesture's main axis), whatever
//!   comes under the pointer later; a wheel's notch goes to the innermost area that can move its way.
//! - A wheel glides: a line delta moves a target and the offset eases to it; the next notch moves the target.
//! - Elastic edges: a gesture past an edge stretches the content (paint only: the offset stays inside the content),
//!   and it springs back once the fingers lift. Momentum that reaches an edge is spent on one bounce at its speed
//!   there, and the rest of it is dropped (as AppKit's scroll views do): fed into the band, the system's tail of
//!   momentum events kept pushing against the spring, and a fast flick shook at the edge.
//! - A programme's own move (an offset set by anyone else) cancels a glide or a spring.
//!
//! The router lives on the window; a scroll area (a `div` with overflow scroll, `uniform_list`, `list`) offers itself
//! in the capture phase of a scroll event, takes the delta the router gives it in the bubble phase, and steps its
//! motion at prepaint, asking for the next frame only while something moves.

// shortcut: one module for the router, the glide, the band and the spring (they share the motion state); split when it grows.
use crate::{IsZero, Pixels, Point, ScrollPhase, Size, point, px};
use collections::FxHashMap;
use std::time::{Duration, Instant};

/// A scroll area's identity across frames: the address of its shared offset (a `div`'s or a handle's
/// `Rc<RefCell<Point>>`, a list's state).
pub(crate) type AreaKey = usize;

/// How long a wheel's notch glides to its target.
pub(crate) const GLIDE: Duration = Duration::from_millis(150);
/// The rubber band's constant (AppKit's: the stretch is `(1 - 1 / (x * C / d + 1)) * d`).
const RUBBER: f32 = 0.55;
/// The spring back's angular frequency (critically damped): a 100 px stretch is under half a point in 350 ms.
const OMEGA: f32 = 20.;
/// The fastest bounce, in points a second: a critically damped spring from rest at `v` peaks at `v / (OMEGA * e)`,
/// so the bounce is never drawn past about 55 points.
const BOUNCE_MAX: f32 = 3000.;

/// The axis a gesture moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
    /// A canvas (`allow_concurrent_scroll`) takes both.
    Both,
}

/// What an area under the pointer offers for one event.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    pub key: AreaKey,
    /// The axes with overflow scroll.
    pub scrolls: Point<bool>,
    /// How far the content can move (zero: it fits).
    pub max: Point<Pixels>,
    /// The offset now (zero at the start, `-max` at the end), or the glide's target while one runs.
    pub offset: Point<Pixels>,
    /// Both axes at once (a canvas).
    pub both: bool,
    /// A wheel on the other axis scrolls an area that scrolls one axis only (`!restrict_scroll_to_axis`).
    pub map_wheel: bool,
}

/// Where an event goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    pub key: AreaKey,
    pub axis: Axis,
}

impl Candidate {
    /// The delta this area takes from an event: a wheel on the other axis of a one-axis area as today; a gesture never.
    fn delta(&self, delta: Point<Pixels>, phased: bool) -> Point<Pixels> {
        let map = self.map_wheel && !phased;
        let x = if !self.scrolls.x {
            Pixels::ZERO
        } else if !delta.x.is_zero() {
            delta.x
        } else if map && !self.scrolls.y {
            delta.y
        } else {
            Pixels::ZERO
        };
        let y = if !self.scrolls.y {
            Pixels::ZERO
        } else if !delta.y.is_zero() {
            delta.y
        } else if map && !self.scrolls.x {
            delta.x
        } else {
            Pixels::ZERO
        };
        point(x, y)
    }

    /// The area can move this delta's way on at least one axis.
    fn can_move(&self, d: Point<Pixels>) -> bool {
        // Pixels order by `total_cmp` (-0 < 0): compare a hundredth of a point inside each end.
        let one = |d: Pixels, o: Pixels, max: Pixels| {
            (d > Pixels::ZERO && o < px(-0.01)) || (d < Pixels::ZERO && o > px(0.01) - max)
        };
        one(d.x, self.offset.x, self.max.x) || one(d.y, self.offset.y, self.max.y)
    }
}

/// Keeps the larger axis of a delta (or both for a canvas).
pub(crate) fn on_axis(d: Point<Pixels>, axis: Axis) -> Point<Pixels> {
    match axis {
        Axis::X => point(d.x, Pixels::ZERO),
        Axis::Y => point(Pixels::ZERO, d.y),
        Axis::Both => d,
    }
}

fn main_axis(d: Point<Pixels>) -> Axis {
    if d.x.abs() > d.y.abs() {
        Axis::X
    } else {
        Axis::Y
    }
}

/// The router: the areas under the pointer for the event being dispatched, the gesture's latch, every area's motion.
#[derive(Default)]
pub(crate) struct ScrollRouter {
    candidates: Vec<Candidate>,
    decided: Option<Option<Route>>,
    latch: Option<Route>,
    motions: FxHashMap<AreaKey, Motion>,
    /// The stretch of the area last stepped at prepaint, for the element that lays its children out.
    pub(crate) overscroll: Point<Pixels>,
}

impl ScrollRouter {
    /// A scroll event starts its dispatch: no area has offered itself yet.
    pub(crate) fn begin_event(&mut self) {
        self.candidates.clear();
        self.decided = None;
    }

    /// An area under the pointer offers itself (the capture phase: outermost first).
    pub(crate) fn offer(&mut self, mut c: Candidate) {
        if let Some(g) = self.motions.get(&c.key).and_then(|m| m.glide) {
            c.offset = g.to;
        }
        self.candidates.push(c);
    }

    /// The gesture's area, latched or not (a programme or a test reads it).
    #[cfg(test)]
    pub(crate) fn latched(&self) -> Option<Route> {
        self.latch
    }

    /// The area latched to the gesture now, which takes its events wherever the pointer is.
    pub(crate) fn is_latched(&self, key: AreaKey) -> bool {
        self.latch.is_some_and(|l| l.key == key)
    }

    /// The delta `key` takes from this event, decided once per event (the bubble phase: innermost first).
    pub(crate) fn take(
        &mut self,
        key: AreaKey,
        phase: ScrollPhase,
        delta: Point<Pixels>,
    ) -> Option<Point<Pixels>> {
        let route = *self
            .decided
            .get_or_insert_with(|| decide(&mut self.latch, &self.candidates, phase, delta));
        let route = route.filter(|r| r.key == key)?;
        let c = self
            .candidates
            .iter()
            .find(|c| c.key == key)
            .copied()
            .unwrap_or(Candidate {
                key,
                scrolls: point(true, true),
                max: Point::default(),
                offset: Point::default(),
                both: route.axis == Axis::Both,
                map_wheel: false,
            });
        Some(on_axis(c.delta(delta, phase.is_phased()), route.axis))
    }

    /// Moves an area by an event's delta: a line delta glides, a precise one moves at once, a phased one stretches
    /// past an edge. Returns whether the offset or the stretch changed (the view is notified then).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply(
        &mut self,
        key: AreaKey,
        offset: &mut Point<Pixels>,
        max: Point<Pixels>,
        size: Size<Pixels>,
        delta: Point<Pixels>,
        lines: bool,
        phase: ScrollPhase,
        now: Instant,
    ) -> bool {
        let before = *offset;
        if lines {
            let m = self.motions.entry(key).or_default();
            m.stretch = Point::default();
            m.velocity = Point::default();
            let from = m.glide.map_or(*offset, |g| g.to);
            let to = clamp(from + delta, max);
            if to != *offset {
                m.glide = Some(Glide {
                    from: *offset,
                    to,
                    start: now,
                });
            }
            m.written = Some(*offset);
            m.tick = now;
            return m.glide.is_some();
        }
        if !phase.is_phased() {
            *offset = clamp(*offset + delta, max);
            if let Some(m) = self.motions.get_mut(&key) {
                m.glide = None;
                m.written = Some(*offset);
            }
            return *offset != before;
        }
        let m = self.motions.entry(key).or_default();
        m.glide = None;
        if matches!(phase, ScrollPhase::MayBegin | ScrollPhase::Began) {
            m.bounced = false;
        }
        let since = m
            .event_at
            .map_or(1. / 60., |t| now.saturating_duration_since(t).as_secs_f32());
        m.event_at = Some(now);
        if phase.is_momentum() {
            // Momentum after the fingers left the band, or after its bounce: dropped.
            if m.bounced || m.stretch != Point::default() {
                m.bounced = true;
                return false;
            }
            let dt = since.clamp(1. / 240., 1. / 30.);
            let mut hit = false;
            for (o, v, d, max) in [
                (&mut offset.x, &mut m.velocity.x, delta.x, max.x),
                (&mut offset.y, &mut m.velocity.y, delta.y, max.y),
            ] {
                if max <= Pixels::ZERO {
                    continue;
                }
                let n = *o + d;
                if n > Pixels::ZERO || n < -max {
                    *o = n.clamp(-max, Pixels::ZERO);
                    *v = px((f32::from(d) / dt).clamp(-BOUNCE_MAX, BOUNCE_MAX));
                    hit = true;
                } else {
                    *o = n;
                }
            }
            m.bounced = hit;
            m.held = false;
            m.written = Some(*offset);
            m.tick = now;
            return *offset != before || hit;
        }
        let was = m.stretch;
        for (o, s, d, max, len) in [
            (&mut offset.x, &mut m.stretch.x, delta.x, max.x, size.width),
            (&mut offset.y, &mut m.stretch.y, delta.y, max.y, size.height),
        ] {
            if max > Pixels::ZERO {
                elastic(o, s, d, max, len);
            } else {
                *s = Pixels::ZERO;
            }
        }
        m.held = matches!(
            phase,
            ScrollPhase::MayBegin | ScrollPhase::Began | ScrollPhase::Changed
        );
        if !m.held && m.stretch != was {
            m.velocity = Point::default();
        }
        m.written = Some(*offset);
        m.tick = now;
        *offset != before || m.stretch != was || (!m.held && m.stretch != Point::default())
    }

    /// Steps an area's motion at prepaint: the glide's place now, the spring's pull, a programme's move noticed.
    /// Returns the stretch to draw and whether to ask for the next frame.
    pub(crate) fn step(
        &mut self,
        key: AreaKey,
        offset: &mut Point<Pixels>,
        max: Point<Pixels>,
        now: Instant,
    ) -> (Point<Pixels>, bool) {
        let Some(m) = self.motions.get_mut(&key) else {
            return (Point::default(), false);
        };
        if m.written.is_some_and(|w| w != *offset) {
            // A programme moved the area: its move wins.
            m.glide = None;
            m.stretch = Point::default();
            m.velocity = Point::default();
        }
        if let Some(g) = m.glide {
            let u = now.saturating_duration_since(g.start).as_secs_f32() / GLIDE.as_secs_f32();
            if u >= 1. {
                *offset = g.to;
                m.glide = None;
            } else {
                *offset = g.from + (g.to - g.from) * ease_out(u);
            }
        }
        *offset = clamp(*offset, max);
        if max.x.is_zero() {
            m.stretch.x = Pixels::ZERO;
        }
        if max.y.is_zero() {
            m.stretch.y = Pixels::ZERO;
        }
        if !m.held {
            let dt = now.saturating_duration_since(m.tick).as_secs_f32();
            let (sx, vx) = spring(m.stretch.x.into(), m.velocity.x.into(), dt);
            let (sy, vy) = spring(m.stretch.y.into(), m.velocity.y.into(), dt);
            m.stretch = point(px(sx), px(sy));
            m.velocity = point(px(vx), px(vy));
        }
        m.tick = now;
        m.written = Some(*offset);
        let stretch = m.stretch;
        let springing = stretch != Point::default() || m.velocity != Point::default();
        let moving = m.glide.is_some() || (!m.held && springing);
        // a bounce's motion stays until the next gesture: it remembers that the momentum is spent
        if !moving && !m.held && !m.bounced {
            self.motions.remove(&key);
        }
        (stretch, moving)
    }

    /// The stretch an area shows now (zero when it rests).
    #[cfg(test)]
    pub(crate) fn stretch(&self, key: AreaKey) -> Point<Pixels> {
        self.motions
            .get(&key)
            .map_or(Point::default(), |m| m.stretch)
    }
}

fn decide(
    latch: &mut Option<Route>,
    candidates: &[Candidate],
    phase: ScrollPhase,
    delta: Point<Pixels>,
) -> Option<Route> {
    match phase {
        ScrollPhase::MayBegin | ScrollPhase::Began | ScrollPhase::None => *latch = None,
        _ => {}
    }
    if phase.is_phased() {
        if let Some(l) = *latch {
            if matches!(phase, ScrollPhase::MomentumEnded) {
                *latch = None;
            }
            return Some(l);
        }
        if delta.x.is_zero() && delta.y.is_zero() {
            return None;
        }
        let axis = main_axis(delta);
        let on = |c: &Candidate| match axis {
            Axis::X => c.scrolls.x,
            _ => c.scrolls.y,
        };
        let moves = |c: &Candidate| match axis {
            Axis::X => c.max.x > Pixels::ZERO,
            _ => c.max.y > Pixels::ZERO,
        };
        let pick = candidates
            .iter()
            .rev()
            .find(|c| on(c) && moves(c))
            .or_else(|| candidates.iter().rev().find(|c| on(c)))?;
        let route = Route {
            key: pick.key,
            axis: if pick.both { Axis::Both } else { axis },
        };
        if !matches!(
            phase,
            ScrollPhase::MomentumEnded | ScrollPhase::Ended | ScrollPhase::Cancelled
        ) {
            *latch = Some(route);
        }
        return Some(route);
    }
    candidates.iter().rev().find_map(|c| {
        let d = c.delta(delta, false);
        let axis = if c.both { Axis::Both } else { main_axis(d) };
        let d = on_axis(d, axis);
        c.can_move(d).then_some(Route { key: c.key, axis })
    })
}

#[derive(Clone, Copy, Debug)]
struct Glide {
    from: Point<Pixels>,
    to: Point<Pixels>,
    start: Instant,
}

#[derive(Debug)]
struct Motion {
    glide: Option<Glide>,
    /// Drawn past the edge: positive past the start (the content pulled down or right), negative past the end.
    stretch: Point<Pixels>,
    velocity: Point<Pixels>,
    /// Fingers on the trackpad: no spring yet.
    held: bool,
    /// The gesture's momentum reached an edge and bounced: the rest of it is dropped.
    bounced: bool,
    /// When the last phased event came (a momentum's speed at the edge).
    event_at: Option<Instant>,
    /// The offset this motion last wrote; anything else is a programme's move.
    written: Option<Point<Pixels>>,
    tick: Instant,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            glide: None,
            stretch: Point::default(),
            velocity: Point::default(),
            held: false,
            bounced: false,
            event_at: None,
            written: None,
            tick: Instant::now(),
        }
    }
}

fn clamp(o: Point<Pixels>, max: Point<Pixels>) -> Point<Pixels> {
    point(
        o.x.clamp(-max.x, Pixels::ZERO),
        o.y.clamp(-max.y, Pixels::ZERO),
    )
}

/// The ease-out of a glide (quadratic): fast at the notch, resting at the target. A cubic's last third moves less
/// than a device pixel (no frame changes there, measured: 35 to 43 ms with no change inside each 150 ms glide).
pub(crate) fn ease_out(u: f32) -> f32 {
    let v = 1. - u.clamp(0., 1.);
    1. - v * v
}

/// AppKit's rubber band: how far content dragged `x` past an edge is drawn, in an area `d` long.
pub(crate) fn rubber(x: f32, d: f32) -> f32 {
    if d <= 0. {
        return 0.;
    }
    (1. - 1. / (x.abs() * RUBBER / d + 1.)) * d * x.signum()
}

/// The band's give at a stretch `s`: the derivative of [`rubber`] there, so that dragging in small steps draws the
/// same curve as one long drag, and dragging back retraces it.
fn give(s: f32, d: f32) -> f32 {
    let r = (1. - s.abs() / d).max(0.001);
    RUBBER * r * r
}

/// One axis of a phased delta: inside the content it moves the offset; past an edge it stretches by the band.
fn elastic(o: &mut Pixels, s: &mut Pixels, d: Pixels, max: Pixels, len: Pixels) {
    let len: f32 = len.into();
    let mut d: f32 = d.into();
    let mut st: f32 = (*s).into();
    if st != 0. {
        let k = give(st, len);
        if d.signum() == st.signum() {
            st += d * k;
        } else if (d * k).abs() <= st.abs() {
            st += d * k;
            d = 0.;
        } else {
            // Back through the edge: the rest of the drag moves the content again.
            d += st / k;
            st = 0.;
        }
        if st != 0. {
            *s = px(st.clamp(-len * 0.999, len * 0.999));
            return;
        }
    }
    let n = *o + px(d);
    if n > Pixels::ZERO {
        *o = Pixels::ZERO;
        st = rubber(n.into(), len);
    } else if n < -max {
        *o = -max;
        st = rubber((n + max).into(), len);
    } else {
        *o = n;
    }
    *s = px(st);
}

/// A critically damped spring toward zero from `x` at velocity `v`, `dt` seconds later; at rest under half a point.
pub(crate) fn spring(x: f32, v: f32, dt: f32) -> (f32, f32) {
    if x == 0. && v == 0. {
        return (0., 0.);
    }
    let e = (-OMEGA * dt).exp();
    let b = v + OMEGA * x;
    let nx = (x + b * dt) * e;
    let nv = (v - OMEGA * b * dt) * e;
    if nx.abs() < 0.5 && nv.abs() < 20. {
        (0., 0.)
    } else {
        (nx, nv)
    }
}

#[cfg(test)]
#[path = "scroll_motion_tests.rs"]
mod tests;
