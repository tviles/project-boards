use crate::config::Config;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct PollConfig {
    pub focused: Duration,
    pub background: Duration,
    pub idle_after: Duration,
    pub full_refresh: Duration,
}

impl PollConfig {
    pub fn from_config(c: &Config) -> Self {
        Self {
            focused: Duration::from_secs(c.poll_interval_secs),
            background: Duration::from_secs(c.background_poll_interval_secs),
            idle_after: Duration::from_secs(c.idle_after_secs),
            full_refresh: Duration::from_secs(c.full_refresh_mins * 60),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollKind {
    Incremental,
    Full,
}

pub struct Scheduler {
    cfg: PollConfig,
    focus_events: bool,
    focused: bool,
    last_input: Instant,
    last_poll: Instant,
    last_full: Instant,
    low_budget: bool,
    paused_until: Option<Instant>,
    in_flight: bool,
}

impl Scheduler {
    /// `now` is when the initial load started; the first poll is one interval later.
    pub fn new(cfg: PollConfig, now: Instant) -> Self {
        Self {
            cfg,
            focus_events: false,
            focused: true,
            last_input: now,
            last_poll: now,
            last_full: now,
            low_budget: false,
            paused_until: None,
            in_flight: false,
        }
    }

    pub fn focus_events_seen(&self) -> bool {
        self.focus_events
    }

    /// A poll has started and not finished.
    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// Focused (when the terminal reports focus), or used recently (when it does not).
    pub fn active(&self, now: Instant) -> bool {
        if self.focus_events {
            self.focused
        } else {
            now.saturating_duration_since(self.last_input) < self.cfg.idle_after
        }
    }

    pub fn interval(&self, now: Instant) -> Duration {
        let base = if self.active(now) {
            self.cfg.focused
        } else {
            self.cfg.background
        };
        if self.low_budget { base * 4 } else { base }
    }

    fn due_kind(&self, now: Instant) -> PollKind {
        if now.saturating_duration_since(self.last_full) >= self.cfg.full_refresh {
            PollKind::Full
        } else {
            PollKind::Incremental
        }
    }

    fn blocked(&self, now: Instant) -> bool {
        self.in_flight || self.paused_until.is_some_and(|until| now < until)
    }

    /// Records a focus change. Returns `Some` when regaining focus should refresh now and
    /// nothing is in flight or paused; the caller must then call `started()`.
    pub fn on_focus(&mut self, focused: bool, now: Instant) -> Option<PollKind> {
        let was = self.focus_events && self.focused;
        self.focus_events = true;
        self.focused = focused;
        if focused {
            self.last_input = now;
        }
        (focused && !was && !self.blocked(now)).then(|| self.due_kind(now))
    }

    /// Records user input. Returns `Some` for the first key after idling (no focus events)
    /// when not in flight or paused; the caller must then call `started()`.
    pub fn on_input(&mut self, now: Instant) -> Option<PollKind> {
        let was_active = self.active(now);
        self.last_input = now;
        (!self.focus_events && !was_active && !self.blocked(now)).then(|| self.due_kind(now))
    }

    /// Returns `Some` when a poll is due; the caller must then call `started()`.
    pub fn tick(&mut self, now: Instant) -> Option<PollKind> {
        if self.blocked(now) {
            return None;
        }
        if self.active(now)
            && now.saturating_duration_since(self.last_full) >= self.cfg.full_refresh
        {
            return Some(PollKind::Full);
        }
        (now.saturating_duration_since(self.last_poll) >= self.interval(now))
            .then_some(PollKind::Incremental)
    }

    pub fn started(&mut self, kind: PollKind, now: Instant) {
        self.in_flight = true;
        self.last_poll = now;
        if kind == PollKind::Full {
            self.last_full = now;
        }
    }

    pub fn finished(&mut self) {
        self.in_flight = false;
    }

    /// Stretches the interval 4x while the rate-limit budget is low. Never returns a poll.
    pub fn set_low_budget(&mut self, low: bool) {
        self.low_budget = low;
    }

    /// Suppresses all polls until `until`; never shortens an active pause.
    pub fn pause_until(&mut self, until: Instant) {
        self.paused_until = Some(self.paused_until.map_or(until, |p| p.max(until)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PollConfig {
        PollConfig {
            focused: Duration::from_secs(30),
            background: Duration::from_secs(300),
            idle_after: Duration::from_secs(300),
            full_refresh: Duration::from_secs(600),
        }
    }
    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn polls_every_30s_while_active() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        assert_eq!(sch.tick(t0 + s(29)), None);
        assert_eq!(sch.tick(t0 + s(30)), Some(PollKind::Incremental));
    }

    #[test]
    fn in_flight_polls_are_not_stacked() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        sch.started(PollKind::Incremental, t0 + s(30));
        assert!(sch.in_flight());
        assert_eq!(sch.tick(t0 + s(90)), None);
        sch.finished();
        assert!(!sch.in_flight());
        assert_eq!(sch.tick(t0 + s(90)), Some(PollKind::Incremental));
    }

    #[test]
    fn idle_fallback_slows_down_and_wakes_on_input() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        let idle = t0 + s(301);
        assert!(!sch.active(idle));
        sch.started(PollKind::Incremental, idle);
        sch.finished();
        assert_eq!(
            sch.tick(idle + s(60)),
            None,
            "background interval is 5 minutes"
        );
        assert_eq!(
            sch.on_input(idle + s(60)),
            Some(PollKind::Incremental),
            "first key after idling refreshes"
        );
        assert_eq!(sch.on_input(idle + s(61)), None);
    }

    #[test]
    fn focus_events_take_over_from_idle_detection() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        assert_eq!(sch.on_focus(false, t0 + s(1)), None);
        assert!(sch.focus_events_seen());
        assert!(
            !sch.active(t0 + s(2)),
            "unfocused even though input was recent"
        );
        assert_eq!(
            sch.on_input(t0 + s(3)),
            None,
            "input does not wake an unfocused pane"
        );
        assert_eq!(sch.on_focus(true, t0 + s(4)), Some(PollKind::Incremental));
    }

    #[test]
    fn full_refresh_every_10_minutes_only_while_active() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        sch.on_focus(true, t0);
        for i in 1..20u64 {
            sch.on_input(t0 + s(i * 30));
            if let Some(kind) = sch.tick(t0 + s(i * 30)) {
                sch.started(kind, t0 + s(i * 30));
                sch.finished();
            }
        }
        sch.started(PollKind::Incremental, t0 + s(600));
        sch.finished();
        assert_eq!(sch.tick(t0 + s(631)), Some(PollKind::Full));
        sch.on_focus(false, t0 + s(632));
        sch.started(PollKind::Incremental, t0 + s(632));
        sch.finished();
        assert_ne!(sch.tick(t0 + s(1300)), Some(PollKind::Full));
    }

    #[test]
    fn pause_blocks_immediate_refreshes_but_keeps_bookkeeping() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        sch.on_focus(true, t0);
        sch.pause_until(t0 + s(100));
        assert_eq!(sch.on_focus(false, t0 + s(10)), None);
        assert_eq!(sch.on_focus(true, t0 + s(20)), None);
        assert!(sch.active(t0 + s(21)));

        let mut sch = Scheduler::new(cfg(), t0);
        let idle = t0 + s(301);
        sch.pause_until(idle + s(100));
        assert_eq!(sch.on_input(idle + s(10)), None);
        assert!(sch.active(idle + s(11)), "input bookkeeping still updated");
        assert_eq!(sch.tick(idle + s(101)), Some(PollKind::Incremental));
    }

    #[test]
    fn pause_never_shortens() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        sch.pause_until(t0 + s(100));
        sch.pause_until(t0 + s(50));
        assert_eq!(sch.tick(t0 + s(60)), None);
        assert_eq!(sch.tick(t0 + s(101)), Some(PollKind::Incremental));
    }

    #[test]
    fn low_budget_and_pauses_stretch_polling() {
        let t0 = Instant::now();
        let mut sch = Scheduler::new(cfg(), t0);
        sch.set_low_budget(true);
        assert_eq!(sch.interval(t0), s(120));
        sch.set_low_budget(false);
        sch.pause_until(t0 + s(100));
        assert_eq!(sch.tick(t0 + s(60)), None);
        assert_eq!(sch.tick(t0 + s(101)), Some(PollKind::Incremental));
    }
}
