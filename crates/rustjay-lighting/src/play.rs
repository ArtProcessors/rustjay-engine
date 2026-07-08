//! Recorded-show playback and priority compositing.
//!
//! [`ShowPlayer`] advances through a `.dmxrec` event log (see [`crate::rec`])
//! and maintains the channel state at the playhead as a [`MaskedFrame`] — a
//! [`DmxFrame`] plus per-channel ownership, so a recording only claims the
//! channels it actually touches. [`composite`] merges several masked layers
//! sACN-style: highest priority wins per channel, with a per-layer weight for
//! fade in/out (owned channels lerp against whatever is underneath, which
//! fades pan/tilt as movement rather than a dip through zero).

use std::collections::BTreeMap;

use crate::dmx::{DmxFrame, Universe, DMX_UNIVERSE_SIZE};
use crate::rec::{rec_duration_ms, RecEvent};

/// One universe of values plus a bitmask of which channels are owned.
#[derive(Debug, Clone)]
pub struct MaskedUniverse {
    values: Universe,
    mask: [u64; 8],
}

impl Default for MaskedUniverse {
    fn default() -> Self {
        Self { values: [0; DMX_UNIVERSE_SIZE], mask: [0; 8] }
    }
}

impl MaskedUniverse {
    #[inline]
    fn set(&mut self, ch: usize, value: u8) {
        self.values[ch] = value;
        self.mask[ch / 64] |= 1 << (ch % 64);
    }

    #[inline]
    pub fn owned(&self, ch: usize) -> bool {
        self.mask[ch / 64] & (1 << (ch % 64)) != 0
    }

    pub fn values(&self) -> &Universe {
        &self.values
    }
}

/// A sparse set of universes with per-channel ownership — the currency of
/// [`composite`]. Only owned channels take part in the merge.
#[derive(Debug, Clone, Default)]
pub struct MaskedFrame {
    universes: BTreeMap<u16, MaskedUniverse>,
}

impl MaskedFrame {
    /// Set one channel (0-based) and mark it owned.
    pub fn set(&mut self, universe: u16, channel: u16, value: u8) {
        let ch = channel as usize;
        if ch < DMX_UNIVERSE_SIZE {
            self.universes.entry(universe).or_default().set(ch, value);
        }
    }

    /// Write a contiguous span starting at a 1-based DMX address, clamped at
    /// the end of the universe (no wrap — matches fixture patching).
    pub fn write_span(&mut self, universe: u16, address: u16, bytes: &[u8]) {
        let start = address.max(1) as usize - 1;
        let u = self.universes.entry(universe).or_default();
        for (i, &b) in bytes.iter().enumerate() {
            if start + i >= DMX_UNIVERSE_SIZE {
                break;
            }
            u.set(start + i, b);
        }
    }

    /// Masked twin of [`crate::pack_fixtures`]: fixture-major `data` packed
    /// from `start_universe`/`start_channel` (1-based), never splitting a
    /// fixture across a universe boundary.
    pub fn pack_fixtures(
        &mut self,
        footprint: usize,
        data: &[u8],
        start_universe: u16,
        start_channel: u16,
    ) {
        if footprint == 0 || footprint > DMX_UNIVERSE_SIZE {
            return;
        }
        let mut universe = start_universe;
        let mut ch = start_channel.max(1) as usize - 1;
        for fixture in data.chunks(footprint) {
            if ch + footprint > DMX_UNIVERSE_SIZE {
                universe = universe.wrapping_add(1);
                ch = 0;
            }
            let u = self.universes.entry(universe).or_default();
            for (i, &b) in fixture.iter().enumerate() {
                if ch + i < DMX_UNIVERSE_SIZE {
                    u.set(ch + i, b);
                }
            }
            ch += footprint;
        }
    }

    pub fn get(&self, universe: u16) -> Option<&MaskedUniverse> {
        self.universes.get(&universe)
    }

    pub fn iter(&self) -> impl Iterator<Item = (u16, &MaskedUniverse)> {
        self.universes.iter().map(|(u, m)| (*u, m))
    }

    pub fn is_empty(&self) -> bool {
        self.universes.is_empty()
    }

    /// Zero all values but keep ownership — loop-wrap reset for a player whose
    /// channel set must not flicker in and out of the merge.
    fn zero_values(&mut self) {
        for u in self.universes.values_mut() {
            u.values = [0; DMX_UNIVERSE_SIZE];
        }
    }
}

/// Merge masked layers into a wire-ready [`DmxFrame`].
///
/// Layers are `(priority, weight, frame)`. Painting runs lowest priority
/// first; equal priorities keep the caller's order (later = on top), which
/// stands in for per-channel LTP.
// ponytail: source-order tie-break, not per-channel last-change timestamps —
// upgrade to per-channel LTP if two same-priority sources ever fight over one
// channel in practice.
pub fn composite(layers: &[(u8, f32, &MaskedFrame)]) -> DmxFrame {
    let mut order: Vec<usize> = (0..layers.len()).collect();
    order.sort_by_key(|&i| layers[i].0); // stable: caller order breaks ties

    let mut out = DmxFrame::new();
    for i in order {
        let (_, weight, frame) = layers[i];
        let w = weight.clamp(0.0, 1.0);
        if w <= 0.0 {
            continue;
        }
        for (universe, masked) in frame.iter() {
            let acc = out.universe_mut(universe);
            for (ch, slot) in acc.iter_mut().enumerate() {
                if masked.owned(ch) {
                    let under = *slot as f32;
                    *slot = (under + (masked.values[ch] as f32 - under) * w).round() as u8;
                }
            }
        }
    }
    out
}

/// Playhead over a time-ordered `.dmxrec` event log.
///
/// The full channel set is claimed up front (value 0 until a channel's first
/// event), so ownership never grows mid-playback and a loop wrap can reset
/// values without the merge seeing channels appear or vanish.
pub struct ShowPlayer {
    events: Vec<RecEvent>,
    duration_ms: u32,
    cursor: usize,
    pos_ms: u32,
    frame: MaskedFrame,
}

impl ShowPlayer {
    pub fn new(events: Vec<RecEvent>) -> Self {
        let duration_ms = rec_duration_ms(&events);
        let mut frame = MaskedFrame::default();
        for e in &events {
            frame.set(e.universe, e.channel, 0);
        }
        Self { events, duration_ms, cursor: 0, pos_ms: 0, frame }
    }

    pub fn duration_ms(&self) -> u32 {
        self.duration_ms
    }

    /// Move the playhead to `t_ms`, applying events along the way. Seeking
    /// backwards (loop wrap, scrub) replays from zero state.
    pub fn seek(&mut self, t_ms: u32) {
        if t_ms < self.pos_ms {
            self.frame.zero_values();
            self.cursor = 0;
        }
        while let Some(e) = self.events.get(self.cursor) {
            if e.t_ms > t_ms {
                break;
            }
            self.frame.set(e.universe, e.channel, e.value);
            self.cursor += 1;
        }
        self.pos_ms = t_ms;
    }

    /// Channel state at the current playhead.
    pub fn frame(&self) -> &MaskedFrame {
        &self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t_ms: u32, universe: u16, channel: u16, value: u8) -> RecEvent {
        RecEvent { t_ms, universe, channel, value }
    }

    #[test]
    fn player_advances_and_wraps() {
        let mut p = ShowPlayer::new(vec![
            ev(0, 1, 0, 100),
            ev(1000, 1, 0, 200),
            ev(2000, 1, 1, 50),
        ]);
        assert_eq!(p.duration_ms(), 2000);

        p.seek(0);
        assert_eq!(p.frame().get(1).unwrap().values()[0], 100);
        // Channel 1 is owned before its first event, at value 0.
        assert!(p.frame().get(1).unwrap().owned(1));
        assert_eq!(p.frame().get(1).unwrap().values()[1], 0);

        p.seek(1500);
        assert_eq!(p.frame().get(1).unwrap().values()[0], 200);

        p.seek(2000);
        assert_eq!(p.frame().get(1).unwrap().values()[1], 50);

        // Loop wrap: back to t=100 → replay from zero state.
        p.seek(100);
        let u = p.frame().get(1).unwrap();
        assert_eq!(u.values()[0], 100);
        assert_eq!(u.values()[1], 0);
        assert!(u.owned(1), "ownership survives the wrap");
    }

    #[test]
    fn composite_priority_and_mask() {
        // Low layer owns ch0+ch1; high layer owns ch1 only.
        let mut low = MaskedFrame::default();
        low.set(1, 0, 10);
        low.set(1, 1, 20);
        let mut high = MaskedFrame::default();
        high.set(1, 1, 200);

        let out = composite(&[(100, 1.0, &low), (150, 1.0, &high)]);
        let u = out.get(1).unwrap();
        assert_eq!(u[0], 10, "unontested channel from low layer");
        assert_eq!(u[1], 200, "high priority wins where it has data");
        assert_eq!(u[2], 0, "unowned channel stays 0");

        // Order in the slice must not matter for different priorities.
        let out2 = composite(&[(150, 1.0, &high), (100, 1.0, &low)]);
        assert_eq!(out2.get(1).unwrap()[1], 200);
    }

    #[test]
    fn composite_equal_priority_later_wins() {
        let mut a = MaskedFrame::default();
        a.set(1, 0, 10);
        let mut b = MaskedFrame::default();
        b.set(1, 0, 90);
        let out = composite(&[(100, 1.0, &a), (100, 1.0, &b)]);
        assert_eq!(out.get(1).unwrap()[0], 90, "caller order breaks ties");
    }

    #[test]
    fn composite_weight_lerps_against_underlayer() {
        let mut under = MaskedFrame::default();
        under.set(1, 0, 100);
        let mut over = MaskedFrame::default();
        over.set(1, 0, 200);

        let half = composite(&[(100, 1.0, &under), (150, 0.5, &over)]);
        assert_eq!(half.get(1).unwrap()[0], 150, "50% fade = midpoint");

        let zero = composite(&[(100, 1.0, &under), (150, 0.0, &over)]);
        assert_eq!(zero.get(1).unwrap()[0], 100, "weight 0 releases to under-layer");

        // No under-layer: fade rises from 0.
        let solo = composite(&[(150, 0.5, &over)]);
        assert_eq!(solo.get(1).unwrap()[0], 100);
    }

    #[test]
    fn masked_pack_fixtures_wraps_universes() {
        let mut f = MaskedFrame::default();
        // 171 RGB fixtures from universe 1 ch 1 — #171 must land in universe 2
        // (same convention as patch::pack_fixtures).
        let mut data = Vec::new();
        for i in 0u16..171 {
            let v = (i % 256) as u8;
            data.extend_from_slice(&[v, v, v]);
        }
        f.pack_fixtures(3, &data, 1, 1);
        let u1 = f.get(1).unwrap();
        assert_eq!(u1.values()[507..510], [169, 169, 169]);
        assert!(!u1.owned(510), "unwritten tail channels stay unowned");
        assert_eq!(f.get(2).unwrap().values()[0..3], [170, 170, 170]);
    }
}
