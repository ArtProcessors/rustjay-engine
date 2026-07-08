//! [`PunchRecorder`] — per-channel punch-in recording over an existing take.
//!
//! Semantics (see `DMX_RECORDER.md`): during a pass, a channel *punches in*
//! the moment its incoming value first differs from what the existing take
//! plays at that moment (implicit 0 for channels the take never touches),
//! and stays live until the pass ends. On [`PunchRecorder::finish`] the
//! channel's base events inside `[punch_time, pass_end)` are replaced by the
//! pass; base events after the pass end resume (audio-style punch-out at the
//! pass boundary). Untouched channels keep their base events verbatim.
//!
//! Pass events are streamed to a sidecar `.dmxrec` as they happen (crash
//! loses seconds, not the take); the merged take is composed at finish.
//!
//! The recorder is clock-free: callers stamp inputs with `t_ms` from their
//! own pass clock, which keeps every code path unit-testable.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::dmx::{Universe, DMX_UNIVERSE_SIZE};
use crate::play::{MaskedFrame, ShowPlayer};
use crate::rec::{RecEvent, RecWriter};

pub struct PunchRecorder {
    /// The existing take (empty for a fresh recording).
    base: Vec<RecEvent>,
    /// Base take advanced to the pass clock — supplies expected values.
    base_player: ShowPlayer,
    /// Events captured this pass, in time order.
    new_events: Vec<RecEvent>,
    /// (universe, channel) → punch-in time.
    punched: BTreeMap<(u16, u16), u32>,
    /// Last recorded value per punched channel (change dedup).
    last: BTreeMap<(u16, u16), u8>,
    /// Streaming crash-safety copy of the pass.
    writer: Option<RecWriter>,
    /// Clock of the latest input seen.
    pos_ms: u32,
}

impl PunchRecorder {
    /// Start a pass over `base` (empty vec = fresh recording). `pass_path`
    /// receives the raw pass events as they happen; pass `None` to keep the
    /// pass in memory only (tests).
    pub fn start(base: Vec<RecEvent>, pass_path: Option<&Path>) -> io::Result<Self> {
        let writer = pass_path.map(RecWriter::create).transpose()?;
        Ok(Self {
            base_player: ShowPlayer::new(base.clone()),
            base,
            new_events: Vec::new(),
            punched: BTreeMap::new(),
            last: BTreeMap::new(),
            writer,
            pos_ms: 0,
        })
    }

    /// Feed one channel of input at pass time `t_ms`.
    pub fn input(&mut self, t_ms: u32, universe: u16, channel: u16, value: u8) {
        if channel as usize >= DMX_UNIVERSE_SIZE {
            return;
        }
        self.pos_ms = self.pos_ms.max(t_ms);
        let key = (universe, channel);
        if let Some(&prev) = self.last.get(&key) {
            // Already punched: record changes only.
            if prev != value {
                self.record(t_ms, universe, channel, value);
            }
            return;
        }
        // Not punched: compare against what the base take plays right now
        // (0 for channels the take never touches).
        self.base_player.seek(t_ms);
        let expected = self
            .base_player
            .frame()
            .get(universe)
            .map_or(0, |u| u.values()[channel as usize]);
        if value != expected {
            self.punched.insert(key, t_ms);
            self.record(t_ms, universe, channel, value);
        }
    }

    /// Feed a whole received universe at pass time `t_ms`.
    pub fn input_universe(&mut self, t_ms: u32, universe: u16, data: &Universe) {
        for (ch, &value) in data.iter().enumerate() {
            self.input(t_ms, universe, ch as u16, value);
        }
    }

    fn record(&mut self, t_ms: u32, universe: u16, channel: u16, value: u8) {
        let e = RecEvent { t_ms, universe, channel, value };
        self.new_events.push(e);
        self.last.insert((universe, channel), value);
        if let Some(w) = &mut self.writer {
            // Pass file is best-effort crash safety; the take lives in memory.
            let _ = w.write(e);
        }
    }

    /// Merged monitor state at pass time `t_ms`: the base take playing,
    /// with punched channels overridden by their live values.
    pub fn monitor_frame(&mut self, t_ms: u32) -> MaskedFrame {
        self.base_player.seek(t_ms);
        let mut frame = self.base_player.frame().clone();
        for (&(universe, channel), &value) in &self.last {
            frame.set(universe, channel, value);
        }
        frame
    }

    /// Channels punched in so far.
    pub fn punched_count(&self) -> usize {
        self.punched.len()
    }

    /// Events captured this pass.
    pub fn event_count(&self) -> usize {
        self.new_events.len()
    }

    /// End the pass and compose the merged take: per punched channel, base
    /// events inside `[punch_time, pass_end)` give way to the pass; the base
    /// resumes after the pass end. Result is time-ordered.
    pub fn finish(self) -> io::Result<Vec<RecEvent>> {
        if let Some(w) = self.writer {
            w.finish()?;
        }
        let pass_end = self.pos_ms;
        let mut merged: Vec<RecEvent> = self
            .base
            .into_iter()
            .filter(|e| match self.punched.get(&(e.universe, e.channel)) {
                Some(&t_in) => e.t_ms < t_in || e.t_ms > pass_end,
                None => true,
            })
            .collect();
        merged.extend(self.new_events);
        merged.sort_by_key(|e| e.t_ms);
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t_ms: u32, universe: u16, channel: u16, value: u8) -> RecEvent {
        RecEvent { t_ms, universe, channel, value }
    }

    #[test]
    fn fresh_recording_captures_changes_only() {
        let mut r = PunchRecorder::start(Vec::new(), None).unwrap();
        // Keep-alive resends of the same universe must not duplicate events.
        let mut u = [0u8; DMX_UNIVERSE_SIZE];
        u[0] = 10;
        r.input_universe(0, 1, &u);
        r.input_universe(100, 1, &u); // identical resend
        u[0] = 20;
        u[5] = 7;
        r.input_universe(200, 1, &u);

        let take = r.finish().unwrap();
        assert_eq!(
            take,
            vec![ev(0, 1, 0, 10), ev(200, 1, 0, 20), ev(200, 1, 5, 7)],
            "zeros never punch a fresh take; changes dedup"
        );
    }

    #[test]
    fn matching_input_never_punches() {
        // Base: ch0 = 50 for the whole take.
        let base = vec![ev(0, 1, 0, 50), ev(5000, 1, 0, 50)];
        let mut r = PunchRecorder::start(base.clone(), None).unwrap();
        let mut u = [0u8; DMX_UNIVERSE_SIZE];
        u[0] = 50; // console replays the take exactly
        r.input_universe(0, 1, &u);
        r.input_universe(1000, 1, &u);
        assert_eq!(r.punched_count(), 0);
        assert_eq!(r.finish().unwrap(), base, "take unchanged");
    }

    #[test]
    fn deviation_punches_in_and_stays_live() {
        // Base: ch0 ramps 100 → 200 at t=1000, back to 0 at t=4000.
        let base = vec![ev(0, 1, 0, 100), ev(1000, 1, 0, 200), ev(4000, 1, 0, 0)];
        let mut r = PunchRecorder::start(base, None).unwrap();

        let mut u = [0u8; DMX_UNIVERSE_SIZE];
        u[0] = 100;
        r.input_universe(0, 1, &u); // matches → no punch
        u[0] = 150;
        r.input_universe(500, 1, &u); // deviates → punch at 500
        r.input_universe(1500, 1, &u); // still 150 (held) → stays live, no event
        u[0] = 160;
        r.input_universe(2000, 1, &u);

        assert_eq!(r.punched_count(), 1);
        let take = r.finish().unwrap();
        // Base t=0 kept (before punch); base t=1000 dropped (inside pass);
        // base t=4000 kept (after pass end 2000) — old take resumes.
        assert_eq!(
            take,
            vec![ev(0, 1, 0, 100), ev(500, 1, 0, 150), ev(2000, 1, 0, 160), ev(4000, 1, 0, 0)]
        );
    }

    #[test]
    fn untouched_channels_keep_base_events() {
        let base = vec![ev(0, 1, 0, 10), ev(0, 2, 3, 99), ev(3000, 2, 3, 55)];
        let mut r = PunchRecorder::start(base, None).unwrap();
        // Only universe 1 receives input, and it deviates immediately.
        let mut u = [0u8; DMX_UNIVERSE_SIZE];
        u[0] = 20;
        r.input_universe(100, 1, &u);
        let take = r.finish().unwrap();
        assert!(take.contains(&ev(0, 2, 3, 99)));
        assert!(take.contains(&ev(3000, 2, 3, 55)), "unpunched channel keeps its future");
        assert!(take.contains(&ev(100, 1, 0, 20)));
        assert!(take.contains(&ev(0, 1, 0, 10)), "pre-punch base events are kept");
    }

    #[test]
    fn monitor_merges_base_playback_and_punches() {
        let base = vec![ev(0, 1, 0, 100), ev(1000, 1, 1, 60)];
        let mut r = PunchRecorder::start(base, None).unwrap();
        let mut u = [0u8; DMX_UNIVERSE_SIZE];
        u[0] = 100;
        u[2] = 40; // ch2 not in base → punches
        r.input_universe(0, 1, &u);

        let m = r.monitor_frame(1500);
        let uni = m.get(1).unwrap();
        assert_eq!(uni.values()[0], 100, "base value (unpunched)");
        assert_eq!(uni.values()[1], 60, "base playback advances with the clock");
        assert_eq!(uni.values()[2], 40, "punched live value overrides");
    }

    #[test]
    fn pass_file_streams_and_survives() {
        let path = std::env::temp_dir()
            .join(format!("rustjay-punch-{}.dmxrec", std::process::id()));
        let mut r = PunchRecorder::start(Vec::new(), Some(&path)).unwrap();
        let mut u = [0u8; DMX_UNIVERSE_SIZE];
        u[0] = 42;
        r.input_universe(10, 1, &u);
        let take = r.finish().unwrap();
        let pass = crate::rec::read_rec(&path).unwrap();
        assert_eq!(pass, take, "fresh take == streamed pass file");
        std::fs::remove_file(&path).ok();
    }
}
