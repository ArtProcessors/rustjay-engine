//! Fixture "looks" — normalized target state for cue-based control.
//!
//! Where [`color_pipeline`](crate::color_pipeline) maps a *sampled pixel* to
//! channel bytes (pixel mapping), [`render_look`] maps a *user-authored*
//! [`FixtureLook`] (dimmer/colour/pan/tilt/…) to channel bytes. Values are sent
//! as set — no gamma, no white extraction; a console does what you tell it.

use serde::{Deserialize, Serialize};

use crate::color::{ChannelRole, FixtureProfile};

/// Normalized target state for one fixture in a lighting cue.
///
/// All `f32` fields are 0.0–1.0. Pan/tilt are fractions of the fixture's full
/// travel (0.5 = centred). `gobo` is a raw DMX byte because slot ranges are
/// fixture-specific.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FixtureLook {
    #[serde(default)]
    pub dimmer: f32,
    /// RGB, 0.0–1.0 each.
    #[serde(default)]
    pub color: [f32; 3],
    /// Explicit white channel (not derived from RGB).
    #[serde(default)]
    pub white: f32,
    #[serde(default = "half")]
    pub pan: f32,
    #[serde(default = "half")]
    pub tilt: f32,
    #[serde(default)]
    pub zoom: f32,
    /// Strobe rate; 0 = open / no strobe on most fixtures.
    #[serde(default)]
    pub strobe: f32,
    /// Raw gobo byte.
    #[serde(default)]
    pub gobo: u8,
}

fn half() -> f32 {
    0.5
}

impl Default for FixtureLook {
    fn default() -> Self {
        Self {
            dimmer: 0.0,
            color: [0.0; 3],
            white: 0.0,
            pan: 0.5,
            tilt: 0.5,
            zoom: 0.0,
            strobe: 0.0,
            gobo: 0,
        }
    }
}

impl FixtureLook {
    /// Linear interpolation, `t` clamped to 0.0–1.0. Continuous parameters
    /// lerp; discrete/rate parameters (strobe, gobo) snap at the end — a
    /// half-lerped strobe rate or gobo byte is visual garbage mid-fade.
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let l = |a: f32, b: f32| a + (b - a) * t;
        Self {
            dimmer: l(self.dimmer, other.dimmer),
            color: [
                l(self.color[0], other.color[0]),
                l(self.color[1], other.color[1]),
                l(self.color[2], other.color[2]),
            ],
            white: l(self.white, other.white),
            pan: l(self.pan, other.pan),
            tilt: l(self.tilt, other.tilt),
            zoom: l(self.zoom, other.zoom),
            strobe: if t >= 1.0 { other.strobe } else { self.strobe },
            gobo: if t >= 1.0 { other.gobo } else { self.gobo },
        }
    }
}

/// Render a [`FixtureLook`] to channel bytes in `profile` order.
///
/// Pan/tilt are quantised to 16 bits; a coarse role emits the high byte and a
/// fine role the low byte, so a profile with only `Pan` degrades to 8-bit
/// naturally. Amber/UV use the same warm-white/blue approximations as
/// [`color_pipeline`](crate::color_pipeline).
pub fn render_look(profile: &FixtureProfile, look: &FixtureLook) -> Vec<u8> {
    let to_byte = |v: f32| (v * 255.0).clamp(0.0, 255.0) as u8;
    let to_u16 = |v: f32| (v * 65535.0).clamp(0.0, 65535.0) as u16;
    let pan16 = to_u16(look.pan);
    let tilt16 = to_u16(look.tilt);
    let [r, g, b] = look.color;

    profile
        .channels
        .iter()
        .map(|role| match role {
            ChannelRole::Red => to_byte(r),
            ChannelRole::Green => to_byte(g),
            ChannelRole::Blue => to_byte(b),
            ChannelRole::White => to_byte(look.white),
            ChannelRole::Amber => to_byte((r + g) * 0.5),
            ChannelRole::Uv => to_byte(b * 0.8),
            ChannelRole::Dimmer => to_byte(look.dimmer),
            ChannelRole::Pan => (pan16 >> 8) as u8,
            ChannelRole::PanFine => (pan16 & 0xff) as u8,
            ChannelRole::Tilt => (tilt16 >> 8) as u8,
            ChannelRole::TiltFine => (tilt16 & 0xff) as u8,
            ChannelRole::Zoom => to_byte(look.zoom),
            ChannelRole::Strobe => to_byte(look.strobe),
            ChannelRole::Gobo => look.gobo,
            ChannelRole::Static(v) => *v,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::builtin_profiles;

    fn mh() -> FixtureProfile {
        builtin_profiles()
            .into_iter()
            .find(|p| p.id == "moving_head_16bit")
            .expect("moving_head_16bit builtin must exist")
    }

    #[test]
    fn render_moving_head_golden_vector() {
        // Pan 0.5 → 32767 → coarse 127, fine 255. Tilt 0.25 → 16383 → 63/255.
        let look = FixtureLook {
            dimmer: 1.0,
            color: [1.0, 0.5, 0.0],
            pan: 0.5,
            tilt: 0.25,
            strobe: 0.0,
            ..Default::default()
        };
        let bytes = render_look(&mh(), &look);
        // [Pan, PanFine, Tilt, TiltFine, Dimmer, R, G, B, Strobe]
        assert_eq!(bytes, vec![127, 255, 63, 255, 255, 255, 127, 0, 0]);
    }

    #[test]
    fn pan_16bit_roundtrips_endpoints() {
        let p = mh();
        let low = render_look(&p, &FixtureLook { pan: 0.0, ..Default::default() });
        let high = render_look(&p, &FixtureLook { pan: 1.0, ..Default::default() });
        assert_eq!((low[0], low[1]), (0, 0));
        assert_eq!((high[0], high[1]), (255, 255));
    }

    #[test]
    fn coarse_only_profile_is_8bit() {
        let p = FixtureProfile {
            id: "mh8".into(),
            name: "8-bit head".into(),
            channels: vec![ChannelRole::Pan, ChannelRole::Tilt, ChannelRole::Dimmer],
        };
        let bytes = render_look(&p, &FixtureLook { pan: 0.5, tilt: 1.0, dimmer: 0.5, ..Default::default() });
        assert_eq!(bytes, vec![127, 255, 127]);
    }

    #[test]
    fn lerp_snaps_strobe_and_gobo_at_end() {
        let a = FixtureLook { strobe: 0.0, gobo: 10, dimmer: 0.0, ..Default::default() };
        let b = FixtureLook { strobe: 1.0, gobo: 20, dimmer: 1.0, ..Default::default() };
        let mid = a.lerp(&b, 0.5);
        assert_eq!(mid.strobe, 0.0, "strobe holds until fade completes");
        assert_eq!(mid.gobo, 10, "gobo holds until fade completes");
        assert!((mid.dimmer - 0.5).abs() < 1e-6, "dimmer lerps");
        let end = a.lerp(&b, 1.0);
        assert_eq!(end.strobe, 1.0);
        assert_eq!(end.gobo, 20);
    }

    #[test]
    fn static_and_white_pass_through() {
        let p = FixtureProfile {
            id: "x".into(),
            name: "x".into(),
            channels: vec![ChannelRole::Static(42), ChannelRole::White],
        };
        let bytes = render_look(&p, &FixtureLook { white: 1.0, ..Default::default() });
        assert_eq!(bytes, vec![42, 255]);
    }

    #[test]
    fn look_serde_defaults_center_pan_tilt() {
        // An empty JSON object must produce the documented defaults —
        // forward-compat for cue files that omit fields.
        let look: FixtureLook = serde_json::from_str("{}").unwrap();
        assert_eq!(look, FixtureLook::default());
        assert_eq!(look.pan, 0.5);
        assert_eq!(look.tilt, 0.5);
    }
}
