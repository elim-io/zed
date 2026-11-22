//! When a window's display link runs (Ensemble ADR-0173). The link wakes the main thread every vsync, 120 times a second
//! on a ProMotion display, even with nothing to draw, so it runs only for frames that come close together (typing, a
//! scroll, an animation) and [`IDLE_PAUSE`] after them. A lone frame (a caret's blink, a clock's second) is drawn at
//! once by one frame request and starts no link. No platform call is made here: [`FramePace`] only decides, and every
//! `now` is a time since the window opened.
//!
//! Its tests need no macOS: `rustc --test --edition 2024 crates/gpui_macos/src/frame_pace.rs`.

use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    time::Duration,
};

/// How long after frames that came close together the link keeps running with nothing to draw: the pauses in typing,
/// in a scroll, between an animation's frames.
const IDLE_PAUSE: Duration = Duration::from_millis(500);

/// Two frames closer than this belong together; a frame further from the one before it is a lone one.
const TOGETHER: Duration = Duration::from_millis(200);

/// How long an invalidated window may go without a frame it was promised (from a link that is not paused, or from a
/// lone frame's request) before the waker starts the link again. A link stopped for idle and started again right away
/// was seen to stay stopped (no frames until an occlusion change made a new link), so the waker does not trust the
/// paused flag alone, and no link is stopped this soon after a frame.
const STALL: Duration = Duration::from_millis(100);

/// What the waker does when the window gets work for a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wake {
    /// A frame is on its way.
    Nothing,
    /// Request one frame; the link stays stopped.
    Draw,
    /// Request one frame and start the link.
    Run,
    /// The link is not stopped and gives no frames: replace it, request one frame.
    Replace,
}

/// A window's pace, in milliseconds since it opened.
pub(crate) struct FramePace {
    /// The link is stopped (not dropped).
    paused: AtomicBool,
    /// When a lone frame was requested; 0 once it came.
    lone: AtomicU64,
    /// When the window last gave a frame, drawn or not.
    seen: AtomicU64,
    /// When it last drew.
    drawn: AtomicU64,
    /// The link runs until then.
    until: AtomicU64,
}

fn ms(t: Duration) -> u64 {
    t.as_millis() as u64
}

impl FramePace {
    pub(crate) fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            lone: AtomicU64::new(0),
            seen: AtomicU64::new(0),
            drawn: AtomicU64::new(0),
            until: AtomicU64::new(0),
        }
    }

    /// The window got work for a frame.
    pub(crate) fn wake(&self, now: Duration) -> Wake {
        let now = ms(now);
        let lone = self.lone.load(Relaxed);
        if !self.paused.load(Relaxed) {
            if now > self.seen.load(Relaxed) + ms(STALL) {
                Wake::Replace
            } else {
                Wake::Nothing
            }
        } else if lone != 0 {
            if now > lone + ms(STALL) {
                Wake::Run
            } else {
                Wake::Nothing
            }
        } else if now >= self.drawn.load(Relaxed) + ms(TOGETHER) {
            self.lone.store(now.max(1), Relaxed);
            Wake::Draw
        } else {
            Wake::Run
        }
    }

    /// The link runs from now (a new one, or the stopped one started).
    pub(crate) fn run(&self, now: Duration) {
        let now = ms(now);
        self.paused.store(false, Relaxed);
        self.lone.store(0, Relaxed);
        self.seen.store(now, Relaxed);
        self.drawn.store(now, Relaxed);
        self.until.store(now + ms(IDLE_PAUSE), Relaxed);
    }

    /// The link is dropped: nothing is paused.
    pub(crate) fn dropped(&self) {
        self.paused.store(false, Relaxed);
        self.lone.store(0, Relaxed);
    }

    /// A frame starts. Work that comes while a lone one is made (an animation's next frame) is no longer lone.
    pub(crate) fn begin(&self, now: Duration) {
        if self.lone.swap(0, Relaxed) != 0 {
            self.drawn.store(ms(now), Relaxed);
        }
    }

    /// A frame ended, `idle` when it drew nothing and nothing waits for the next one. True: stop the link.
    pub(crate) fn end(&self, now: Duration, idle: bool) -> bool {
        let now = ms(now);
        let paused = self.paused.load(Relaxed);
        let drawn = self.drawn.load(Relaxed);
        self.seen.store(now, Relaxed);
        if !idle {
            if !paused && now < drawn + ms(TOGETHER) {
                self.until.store(now + ms(IDLE_PAUSE), Relaxed);
            }
            self.drawn.store(now, Relaxed);
            return false;
        }
        let stop = !paused && now >= self.until.load(Relaxed) && now >= drawn + ms(STALL);
        if stop {
            self.paused.store(true, Relaxed);
        }
        stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window on a 120 Hz display: its link, the frames asked for, and what it was woken for.
    struct Window {
        pace: FramePace,
        running: bool,
        /// A frame was requested: it comes at the next millisecond.
        requested: bool,
        dirty: bool,
        /// The next frames an animation asks for, one in each frame.
        animating: u32,
        steps: u32,
        frames: Vec<u64>,
        starts: u32,
        now: u64,
    }

    impl Window {
        fn new() -> Self {
            let w = Self {
                pace: FramePace::new(),
                running: true,
                requested: false,
                dirty: false,
                animating: 0,
                steps: 0,
                frames: Vec::new(),
                starts: 0,
                now: 0,
            };
            w.pace.run(Duration::ZERO);
            w
        }

        fn at(&self) -> Duration {
            Duration::from_millis(self.now)
        }

        fn wake(&mut self) {
            match self.pace.wake(self.at()) {
                Wake::Nothing => {}
                Wake::Draw => self.requested = true,
                Wake::Run | Wake::Replace => {
                    self.pace.run(self.at());
                    self.running = true;
                    self.requested = true;
                    self.starts += 1;
                }
            }
        }

        fn notify(&mut self) {
            self.dirty = true;
            self.wake();
        }

        fn animate(&mut self, frames: u32) {
            self.animating = frames;
            self.notify();
        }

        fn step(&mut self) {
            self.requested = false;
            self.steps += 1;
            self.pace.begin(self.at());
            let drew = std::mem::take(&mut self.dirty);
            if drew {
                self.frames.push(self.now);
                if self.animating > 0 {
                    self.animating -= 1;
                    // the next frame's callback is queued in the draw: it redraws
                    self.wake();
                    self.dirty = true;
                }
            }
            if self.pace.end(self.at(), !drew && !self.dirty) {
                self.running = false;
            }
        }

        /// Runs `ms` milliseconds: a vsync about every 8 (125 a second), or a requested frame.
        fn run(&mut self, ms: u64) {
            for _ in 0..ms {
                self.now += 1;
                if self.requested || (self.running && self.now % 8 == 0) {
                    self.step();
                }
            }
        }

        fn rest(&mut self) {
            self.run(2000);
            assert!(!self.running, "the link stops at rest");
            self.steps = 0;
            self.frames.clear();
            self.starts = 0;
        }
    }

    #[test]
    fn a_blink_costs_its_own_frames() {
        let mut w = Window::new();
        w.rest();
        for period in [500, 515, 1000, 250] {
            w.rest();
            for _ in 0..10 {
                w.notify();
                w.run(period);
            }
            assert_eq!((w.frames.len(), w.steps, w.starts), (10, 10, 0), "{period}");
        }
    }

    #[test]
    fn a_blink_shorter_than_the_pause_lets_the_link_stop_after_a_burst() {
        let mut w = Window::new();
        w.rest();
        for _ in 0..5 {
            w.notify();
            w.run(90);
        }
        assert!(w.running, "typing runs the link");
        for _ in 0..10 {
            w.notify();
            w.run(490);
        }
        assert!(!w.running);
        // the pause after the burst, then a step a blink
        assert!(w.steps < 5 * 12 + 63 + 13 + 10, "{}", w.steps);
    }

    #[test]
    fn a_lone_frame_comes_at_once_and_the_second_starts_the_link() {
        let mut w = Window::new();
        w.rest();
        let t = w.now;
        w.notify();
        w.notify();
        w.run(3);
        assert_eq!((w.frames.clone(), w.starts), (vec![t + 1], 0));
        w.notify();
        w.run(3);
        assert_eq!((w.frames.clone(), w.starts), (vec![t + 1, t + 4], 1));
        assert!(w.running);
    }

    #[test]
    fn an_animation_from_rest_runs_the_link_to_its_end() {
        let mut w = Window::new();
        w.rest();
        w.animate(60);
        w.run(1000);
        assert_eq!((w.frames.len(), w.starts), (61, 1));
        let gap = w.frames.windows(2).map(|p| p[1] - p[0]).max();
        assert!(gap <= Some(9), "{gap:?}");
    }

    #[test]
    fn typing_keeps_the_link_through_a_pause() {
        let mut w = Window::new();
        w.rest();
        for pause in [90, 150, 190, 120, 480, 90] {
            w.notify();
            w.run(pause);
        }
        assert_eq!(w.starts, 1);
        assert!(w.running);
    }

    #[test]
    fn the_link_is_not_stopped_right_after_a_frame() {
        let mut w = Window::new();
        w.run(495);
        w.notify();
        w.run(60);
        assert!(w.running, "the frame at 496 is 60 ms old");
        w.run(60);
        assert!(!w.running);
    }

    #[test]
    fn a_link_that_gives_no_frames_is_replaced_and_a_lost_frame_asked_again() {
        let p = FramePace::new();
        let t = Duration::from_millis;
        p.run(t(0));
        assert_eq!(p.wake(t(100)), Wake::Nothing);
        assert_eq!(p.wake(t(101)), Wake::Replace);
        p.run(t(101));
        assert!(p.end(t(700), true));
        assert_eq!(p.wake(t(1000)), Wake::Draw);
        assert_eq!(p.wake(t(1100)), Wake::Nothing);
        assert_eq!(p.wake(t(1101)), Wake::Run);
        // an occluded window has no link: nothing is paused
        p.dropped();
        assert_eq!(p.wake(t(1102)), Wake::Replace);
    }
}
