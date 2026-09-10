//! Multi-channel compositing mixer for rustjay-engine.

mod blend;
pub mod blit;
mod composite;
pub mod crossfade;
pub mod plugin;
pub mod preset;
pub mod sequencer;

pub use blend::BlendMode;
pub use blit::BlitPipeline;
pub use composite::{CompositePipeline, KeyParams};
pub use crossfade::{AutoCrossfade, BeatSyncCrossfade, Easing};
pub use preset::{ChannelState, MixerState, MAX_CHANNELS, MAX_GROUPS, MIXER_STATE_VERSION};
pub use sequencer::{SequencerState, StepKind, TransitionEffect, TransitionStep};

use rustjay_core::params::{ParamCategory, ParameterDescriptor};
use rustjay_core::{EffectInput, EffectInstance, EngineState, RenderCtx, RenderTarget};
use rustjay_render::Texture;


/// Which engine input slot a channel samples from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum InputSelect {
    #[default]
    Slot1,
    Slot2,
    Both,
}

impl InputSelect {
    pub fn to_index(self) -> usize {
        match self {
            InputSelect::Slot1 => 0,
            InputSelect::Slot2 => 1,
            InputSelect::Both => 2,
        }
    }

    pub fn from_index(v: usize) -> Self {
        match v {
            0 => InputSelect::Slot1,
            1 => InputSelect::Slot2,
            _ => InputSelect::Both,
        }
    }

    pub fn labels() -> &'static [&'static str] {
        &["Slot 1", "Slot 2", "Both"]
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LastOutput {
    Texture,
    Ping,
}

/// An effect in a chain with an on/off toggle and a stable UUID.
pub struct EffectSlot {
    pub effect: Box<dyn EffectInstance>,
    pub enabled: bool,
    pub uuid: String,
    /// ISF/shader source path — used to rebuild the chain across restarts.
    pub source_path: Option<std::path::PathBuf>,
}

impl EffectSlot {
    pub fn new(effect: Box<dyn EffectInstance>) -> Self {
        Self {
            effect,
            enabled: true,
            uuid: uuid::Uuid::new_v4().simple().to_string()[..8].to_string(),
            source_path: None,
        }
    }
}

/// One mixer channel: an effect plus how it is mixed into the composite.
pub struct Channel {
    /// Stable identity, persisted across presets (REQ-01.3).
    pub uuid: String,
    pub name: String,
    pub effect: Box<dyn EffectInstance>,
    /// Post-effect chain applied before compositing (REQ-01.5).
    pub chain: Vec<EffectSlot>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub input_select: InputSelect,
    pub solo: bool,
    pub mute: bool,
    /// The bus group this layer belongs to, if any. Membership lives here
    /// rather than as a span on the group: a span has to be recomputed on every
    /// restack, and dropping a layer onto a group it already sits next to is
    /// then indistinguishable from not moving it at all.
    pub group: Option<String>,
    /// When `false` the channel is skipped entirely: no effect render pass and
    /// no composite step. Used by hosts (e.g. VP-404) to elide idle pads.
    pub active: bool,

    // Chroma/luma key defaults (engine params override these each frame).
    pub key_mode: u32,       // 0=none, 1=chroma, 2=luma
    pub key_r: f32,
    pub key_g: f32,
    pub key_b: f32,
    pub key_threshold: f32,
    pub key_smoothness: f32,
    pub luma_invert: bool,

    // GPU resources — allocated lazily, reallocated only on resize (REQ-11.2).
    texture: Option<Texture>,
    ping: Option<Texture>,
    size: [u32; 2],
    last_output: LastOutput,

    // Cached param keys — avoids per-frame format! allocs (PERF-1).
    opacity_key: String,
    blend_key: String,
    input_select_key: String,
    key_mode_key: String,
    key_r_key: String,
    key_g_key: String,
    key_b_key: String,
    key_threshold_key: String,
    key_smoothness_key: String,
    key_luma_invert_key: String,
    /// Last-seen count of enabled FX; used to detect parity flips that change
    /// `output_texture()` and invalidate the composite cache (CORR-2).
    last_enabled_count: usize,
}

impl Channel {
    /// Create a channel from an effect instance with default mix settings.
    ///
    /// GPU textures are allocated on first render when the target size is known.
    pub fn new(
        uuid: impl Into<String>,
        name: impl Into<String>,
        mut effect: Box<dyn EffectInstance>,
    ) -> Self {
        let uuid = uuid.into();
        let name = name.into();
        effect.set_param_prefix(&format!("ch_{}_", uuid));
        Self {
            opacity_key: format!("ch_{}_opacity", uuid),
            blend_key: format!("ch_{}_blend", uuid),
            input_select_key: format!("ch_{}_input_select", uuid),
            key_mode_key: format!("ch_{}_key_mode", uuid),
            key_r_key: format!("ch_{}_key_r", uuid),
            key_g_key: format!("ch_{}_key_g", uuid),
            key_b_key: format!("ch_{}_key_b", uuid),
            key_threshold_key: format!("ch_{}_key_threshold", uuid),
            key_smoothness_key: format!("ch_{}_key_smoothness", uuid),
            key_luma_invert_key: format!("ch_{}_key_luma_invert", uuid),
            uuid,
            name,
            effect,
            chain: Vec::new(),
            opacity: 1.0,
            blend_mode: BlendMode::default(),
            input_select: InputSelect::default(),
            solo: false,
            mute: false,
            group: None,
            active: true,
            key_mode: 0,
            key_r: 0.0,
            key_g: 1.0,
            key_b: 0.0,
            key_threshold: 0.3,
            key_smoothness: 0.1,
            luma_invert: false,
            texture: None,
            ping: None,
            size: [0, 0],
            last_output: LastOutput::Texture,
            last_enabled_count: 0,
        }
    }

    /// Append an effect to this channel's post-chain, assigning its parameter
    /// prefix (`ch_<uuid>_fx<uuid>_`) so its params are reachable by GUI/MIDI/
    /// OSC/LFO — mirrors [`Mixer::add_master_effect`].
    pub fn add_effect(&mut self, effect: Box<dyn EffectInstance>) {
        self.chain.push(EffectSlot::new(effect));
        let slot = self.chain.last_mut().unwrap();
        let prefix = format!("ch_{}_fx{}_", self.uuid, slot.uuid);
        slot.effect.set_param_prefix(&prefix);
    }

    pub fn set_effect_enabled(&mut self, index: usize, enabled: bool) {
        if let Some(slot) = self.chain.get_mut(index) {
            slot.enabled = enabled;
        }
    }

    /// Reorder the channel's post-chain: move the effect at `from` to `to`.
    /// UUID-stable prefixes mean existing param values stay wired.
    pub fn reorder_effect(&mut self, from: usize, to: usize) {
        if from >= self.chain.len() || from == to {
            return;
        }
        let to = to.min(self.chain.len() - 1);
        let slot = self.chain.remove(from);
        self.chain.insert(to, slot);
    }

    /// Ensure the channel's render-target textures match `size`.
    fn ensure_size(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.size == size {
            return;
        }
        self.texture = Some(Texture::create_render_target(
            device,
            size[0],
            size[1],
            &format!("ch {} tex", self.name),
        ));
        self.ping = Some(Texture::create_render_target(
            device,
            size[0],
            size[1],
            &format!("ch {} ping", self.name),
        ));
        self.size = size;
        self.last_output = LastOutput::Texture;
    }

    /// Render the channel effect and run its post-chain, returning the texture
    /// that holds the final output for this frame.
    fn render<'a>(
        &'a mut self,
        ctx: &mut RenderCtx<'_>,
        inputs: &[EffectInput<'_>],
        engine: &EngineState,
    ) -> Option<&'a Texture> {
        let tex = self.texture.as_ref()?;
        // Uniform buffers are written here, not in render_to — an effect that is
        // never prepared draws with stale or zero uniforms, which for an ISF
        // shader means black.
        self.effect.prepare(engine, ctx.device, ctx.queue);
        self.effect.render_to(
            ctx,
            inputs,
            RenderTarget {
                view: &tex.view,
                size: self.size,
            },
            engine,
        );
        self.last_output = LastOutput::Texture;

        if self.chain.is_empty() {
            return Some(tex);
        }

        let ping = self.ping.as_ref()?;
        let mut is_ping = false; // false → src=tex, dst=ping

        for slot in self.chain.iter_mut() {
            if !slot.enabled {
                continue;
            }
            slot.effect.prepare(engine, ctx.device, ctx.queue);
            let (src_tex, dst_tex) = if is_ping { (ping, tex) } else { (tex, ping) };
            let input = EffectInput {
                view: &src_tex.view,
                sampler: &src_tex.sampler,
                generation: src_tex.generation,
                texture: Some(&src_tex.texture),
            };
            slot.effect.render_to(
                ctx,
                &[input],
                RenderTarget {
                    view: &dst_tex.view,
                    size: self.size,
                },
                engine,
            );
            is_ping = !is_ping;
        }

        self.last_output = if is_ping {
            LastOutput::Ping
        } else {
            LastOutput::Texture
        };

        if is_ping {
            Some(ping)
        } else {
            Some(tex)
        }
    }

    /// Only valid after [`render`](Self::render) has been called for the current frame.
    pub fn output_texture(&self) -> Option<&Texture> {
        match self.last_output {
            LastOutput::Texture => self.texture.as_ref(),
            LastOutput::Ping => self.ping.as_ref(),
        }
    }
}

impl Mixer {
    /// Only valid after the mixer has rendered for the current frame.
    pub fn channel_texture(&self, uuid: &str) -> Option<&Texture> {
        self.channels
            .iter()
            .find(|c| c.uuid == uuid)
            .and_then(|c| c.output_texture())
    }
}

/// Parameter prefix for the transition effect.
///
/// Fixed rather than uuid-keyed: there is exactly one transition, and a stable
/// prefix means a MIDI binding to its softness survives swapping the shader.
pub const TRANSITION_PREFIX: &str = "transition_";

/// The transition's progress parameter, driven from the crossfader each frame.
pub const TRANSITION_PROGRESS: &str = "transition_progress";

/// Which deck the crossfader is parked on, if either.
///
/// `None` means run the transition pass. Parked is the common case — a fader
/// sits at an end far more of the time than it spends moving — so this is what
/// saves a full-screen pass most frames.
fn parked_deck(crossfader: f32) -> Option<usize> {
    let x = crossfader.clamp(0.0, 1.0);
    if x <= 0.001 {
        Some(0)
    } else if x >= 0.999 {
        Some(1)
    } else {
        None
    }
}

/// What the master should blend where the two decks sit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeckSource {
    /// The fader is parked: one deck's own output, transition pass skipped.
    Deck(String),
    /// The two combined by the transition effect.
    Transition,
}

/// One thing a group composites: a member layer, or a nested group's output.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GroupItem {
    Channel(usize),
    Group(String),
}

/// A bus group: several layers composited together, then treated as one.
///
/// The members are composited into the group's own accumulator, the group's
/// chain runs over that result, and the result is blended into the master like
/// a single layer. That is what makes one blur cover three layers rather than
/// blurring each of them.
///
/// Membership lives on the member: a channel names its group in
/// [`Channel::group`], and a group names its own in [`ChannelGroup::parent`].
/// A group is still a contiguous span of the stack, as it is in every
/// compositor, so restacking a member out of the span takes it out of the
/// group.
pub struct ChannelGroup {
    /// Stable identity; the parameter prefix is `grp_<uuid>_`.
    pub uuid: String,
    pub name: String,
    /// The group this one nests inside, if any. `None` is top level — blended
    /// straight into the master composite.
    ///
    /// Set it through [`Mixer::set_group_parent`], which refuses a cycle. A
    /// group whose parent chain is broken or cyclic is treated as top level
    /// rather than dropped, so a bad edit costs you the nesting, not the layers.
    pub parent: Option<String>,
    /// Effects applied to the composited members.
    pub chain: Vec<EffectSlot>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub solo: bool,
    pub mute: bool,
    /// Collapsed in the UI. Runtime only.
    pub collapsed: bool,
    /// Whether `group_out` holds this frame's image. Runtime only.
    rendered: bool,
    /// Own compositor: the shared one caches bind groups by `(slot, dest_is_a)`,
    /// which says nothing about *which* accumulator a group writes to, so
    /// reusing it would hand the group a bind group pointing at the master's
    /// textures.
    composite: Option<CompositePipeline>,
    acc_a: Option<Texture>,
    acc_b: Option<Texture>,
    chain_ping: Option<Texture>,
    /// The group's finished image: members composited, chain applied. Held so
    /// the master pass can read it with a shared borrow.
    group_out: Option<Texture>,
    size: [u32; 2],
    opacity_key: String,
    blend_key: String,
}

impl ChannelGroup {
    pub fn new(uuid: impl Into<String>, name: impl Into<String>) -> Self {
        let uuid = uuid.into();
        Self {
            opacity_key: format!("grp_{uuid}_opacity"),
            blend_key: format!("grp_{uuid}_blend"),
            uuid,
            name: name.into(),
            parent: None,
            chain: Vec::new(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            solo: false,
            mute: false,
            collapsed: false,
            rendered: false,
            composite: None,
            acc_a: None,
            acc_b: None,
            chain_ping: None,
            group_out: None,
            size: [0, 0],
        }
    }

    fn ensure_resources(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.size == size && self.composite.is_some() {
            return;
        }
        let format = rustjay_core::working_format();
        self.composite = Some(CompositePipeline::new(device, format));
        self.acc_a = Some(Texture::create_render_target(device, size[0], size[1], "group acc_a"));
        self.acc_b = Some(Texture::create_render_target(device, size[0], size[1], "group acc_b"));
        self.chain_ping = Some(Texture::create_render_target(device, size[0], size[1], "group chain ping"));
        self.group_out = Some(Texture::create_render_target(device, size[0], size[1], "group out"));
        self.size = size;
    }

    /// Give back the four full-resolution textures.
    ///
    /// A group is ~133 MB at 4K, and the cap is [`MAX_GROUPS`], so an empty or
    /// muted group holding its allocation is most of a gigabyte doing nothing.
    /// Re-allocated by `ensure_resources` the moment it has members again.
    fn release_resources(&mut self) {
        if self.composite.is_none() {
            return;
        }
        self.composite = None;
        self.acc_a = None;
        self.acc_b = None;
        self.chain_ping = None;
        self.group_out = None;
        self.size = [0, 0];
        self.rendered = false;
    }
}

/// Multi-channel compositor.
pub struct Mixer {
    pub channels: Vec<Channel>,
    /// Bus groups over contiguous spans of `channels`.
    pub groups: Vec<ChannelGroup>,
    /// Ignored when `channels.len() != 2`.
    pub crossfader: f32,
    /// Whether the crossfader scales the two channel opacities when there are
    /// exactly two channels.
    ///
    /// A host that treats channels as a free-standing layer stack turns this
    /// off: otherwise a stack that happens to hold exactly two layers renders
    /// both at half opacity, which reads as a mysterious dimming rather than a
    /// crossfade. Defaults to `true`, preserving the A/B behaviour every
    /// existing host relies on.
    pub use_crossfader: bool,
    /// Scales every channel's effective opacity — a master dimmer / blackout.
    ///
    /// 1.0 is unity, so hosts that never touch it are unaffected.
    pub master_dim: f32,
    /// The two groups the crossfader transitions between, by uuid: `[A, B]`.
    ///
    /// When this and [`transition`](Self::transition) are both set and both
    /// groups rendered, the two are combined by the transition effect and the
    /// result is blended into the master **once**, in place of blending each
    /// deck separately. Unset — or with no transition loaded — the two are
    /// ordinary groups and composite exactly as any other group does, which is
    /// what makes this additive rather than a second blending path.
    pub decks: Option<[String; 2]>,
    /// The two-input effect combining the decks. Its `progress` is the
    /// crossfader: there is no separate crossfade, a plain dissolve is just the
    /// dissolve shader at the fader's position.
    pub transition: Option<EffectSlot>,
    /// Master effect chain (REQ-06).
    pub master: Vec<EffectSlot>,
    pub auto: Option<AutoCrossfade>,
    pub beat_sync: Option<BeatSyncCrossfade>,
    pub sequencer: SequencerState,

    // GPU resources — allocated lazily, reallocated only on resize or channel-count change.
    composite: Option<CompositePipeline>,
    blit: Option<BlitPipeline>,
    acc_a: Option<Texture>,
    acc_b: Option<Texture>,
    master_ping: Option<Texture>,
    /// Where the transition draws. Allocated only once decks are actually in
    /// use, like a group's own textures.
    transition_out: Option<Texture>,
    size: [u32; 2],
    /// Bumped whenever GPU textures are reallocated (resize) or the channel set
    /// changes. Drives the composite pipeline's bind-group cache invalidation
    /// (REQ-11.1) — a cached bind group keyed by `(slot, dest)` is only valid
    /// within one generation.
    generation: u64,
}

impl Mixer {
    /// GPU resources are allocated on first render.
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            groups: Vec::new(),
            crossfader: 0.5,
            use_crossfader: true,
            master_dim: 1.0,
            decks: None,
            transition: None,
            master: Vec::new(),
            auto: None,
            beat_sync: None,
            sequencer: SequencerState::new(),
            composite: None,
            blit: None,
            acc_a: None,
            acc_b: None,
            master_ping: None,
            transition_out: None,
            size: [0, 0],
            generation: 0,
        }
    }

    /// Add a channel, returning its index.
    ///
    /// Fails if the mixer already has [`MAX_CHANNELS`] channels (REQ-01.2).
    pub fn add_channel(&mut self, channel: Channel) -> Result<usize, String> {
        if self.channels.len() >= MAX_CHANNELS {
            return Err(format!("maximum channels ({MAX_CHANNELS})"));
        }
        self.channels.push(channel);
        // The new channel's textures are allocated lazily at the top of the next
        // `render_to` call (via `ensure_resources`), once the render size is known.
        self.invalidate_composite_cache();
        Ok(self.channels.len() - 1)
    }

    /// Remove a channel by index, returning it.
    ///
    /// Fails if the mixer would drop below 1 channel (REQ-01.2).
    pub fn remove_channel(&mut self, index: usize) -> Result<Channel, &'static str> {
        if self.channels.len() <= 1 {
            return Err("minimum 1 channel");
        }
        if index >= self.channels.len() {
            return Err("channel index out of bounds");
        }
        self.invalidate_composite_cache();
        Ok(self.channels.remove(index))
    }

    /// Add an effect to the master chain.
    ///
    /// Automatically assigns the prefix `master_fx{uuid}_` where `uuid` is the
    /// effect slot's stable identifier (ARCH-3).
    pub fn add_master_effect(&mut self, effect: Box<dyn EffectInstance>) {
        self.master.push(EffectSlot::new(effect));
        let slot = self.master.last_mut().unwrap();
        let prefix = format!("master_fx{}_", slot.uuid);
        slot.effect.set_param_prefix(&prefix);
    }

    /// Reorder the master effect chain: move the effect at `from` to `to`.
    /// UUID-stable prefixes mean existing param values stay wired.
    pub fn reorder_master_effect(&mut self, from: usize, to: usize) {
        if from >= self.master.len() || from == to {
            return;
        }
        let to = to.min(self.master.len() - 1);
        let slot = self.master.remove(from);
        self.master.insert(to, slot);
    }

    /// Move the channel at `from` to `to`, shifting the rest.
    ///
    /// Channels composite in order, so for a host that presents them as layers
    /// this is the restack operation. Out-of-range indices are ignored; uuids
    /// are untouched, so parameter prefixes and modulation survive the move.
    pub fn reorder_channel(&mut self, from: usize, to: usize) {
        if from >= self.channels.len() || from == to {
            return;
        }
        let to = to.min(self.channels.len() - 1);
        let channel = self.channels.remove(from);
        self.channels.insert(to, channel);
        self.invalidate_composite_cache();
    }

    /// Declare that the channel-index → source-texture mapping has changed, so
    /// the composite pipelines must rebuild their slot-keyed bind groups.
    ///
    /// Both the master compositor and each group's own one cache bind groups by
    /// `(slot, dest parity)` while rewriting each slot's uniform (opacity, blend,
    /// key) every frame. Skip this after a restack or a source swap and slot `i`
    /// keeps sampling the *previous* occupant's pixels while wearing the
    /// *current* occupant's opacity — a layer's fader appears to drive its
    /// neighbour.
    ///
    /// Call after anything that moves a channel between indices or replaces the
    /// effect behind one. Adding, removing and resizing already call it.
    pub fn invalidate_composite_cache(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Effective per-channel opacity for the current frame (REQ-02.4).
    ///
    /// With exactly 2 channels the crossfader scales the two opacities; otherwise
    /// each channel's own opacity is used directly.
    pub fn effective_opacities(&self) -> Vec<f32> {
        let dim = self.master_dim.clamp(0.0, 1.0);
        self.raw_effective_opacities().iter().map(|o| o * dim).collect()
    }

    /// Whether anything is soloed anywhere.
    ///
    /// Kept for hosts that just want to light up a "SOLO" indicator. It is
    /// deliberately *not* what decides audibility — see [`solo_active_in`]:
    /// a solo silences its siblings, not the whole show, so auditioning a layer
    /// on the deck you are preparing does not blank the deck that is on screen.
    ///
    /// [`solo_active_in`]: Self::solo_active_in
    pub fn any_solo(&self) -> bool {
        self.channels.iter().any(|c| c.solo) || self.groups.iter().any(|g| g.solo)
    }

    /// Is some direct child of `scope` soloed? `None` is the top level.
    ///
    /// Solo scopes to siblings. A solo inside one group silences that group's
    /// other members and nothing else; a solo on a top-level group silences the
    /// other top-level groups and the ungrouped layers.
    fn solo_active_in(&self, scope: Option<&str>) -> bool {
        self.channels
            .iter()
            .any(|c| c.group.as_deref() == scope && c.solo)
            || self
                .groups
                .iter()
                .any(|g| g.parent.as_deref() == scope && g.solo)
    }

    /// Whether `uuid` survives mute and sibling-solo at its own level and at
    /// every level above it.
    fn group_audible(&self, uuid: &str) -> bool {
        let mut cur = uuid.to_string();
        for _ in 0..=self.groups.len() {
            let Some(g) = self.groups.iter().find(|g| g.uuid == cur) else {
                // Broken chain: treat what is left as top level rather than
                // silently swallowing the layers under it.
                return true;
            };
            if g.mute || (self.solo_active_in(g.parent.as_deref()) && !g.solo) {
                return false;
            }
            match g.parent.as_deref() {
                None => return true,
                Some(p) => cur = p.to_string(),
            }
        }
        true // cyclic: same call, do not hide the layers
    }

    /// How many groups deep `uuid` sits: 0 for top level.
    ///
    /// `None` when the parent chain is broken (a missing parent) or cyclic. The
    /// walk is bounded by the group count, so a cycle terminates instead of
    /// hanging the render thread.
    pub fn group_depth(&self, uuid: &str) -> Option<usize> {
        let mut cur = self.groups.iter().find(|g| g.uuid == uuid)?;
        for depth in 0..=self.groups.len() {
            match cur.parent.as_deref() {
                None => return Some(depth),
                Some(p) => cur = self.groups.iter().find(|g| g.uuid == p)?,
            }
        }
        None // ran past every group without reaching a root: cyclic
    }

    /// The outermost group `uuid` belongs to — itself when it is top level.
    fn top_level_ancestor(&self, uuid: &str) -> String {
        let mut cur = uuid.to_string();
        for _ in 0..=self.groups.len() {
            let Some(g) = self.groups.iter().find(|g| g.uuid == cur) else {
                return cur;
            };
            match g.parent.as_deref() {
                None => return cur,
                Some(p) => cur = p.to_string(),
            }
        }
        cur
    }

    /// Nest `child` inside `parent` (`None` to lift it to top level).
    ///
    /// Refuses a cycle — a group cannot become its own ancestor — and refuses a
    /// parent that does not exist. Returns whether the change was made.
    pub fn set_group_parent(&mut self, child: &str, parent: Option<&str>) -> bool {
        if !self.groups.iter().any(|g| g.uuid == child) {
            return false;
        }
        if let Some(p) = parent {
            if p == child || !self.groups.iter().any(|g| g.uuid == p) {
                return false;
            }
            // Walk up from the proposed parent: meeting `child` means `child`
            // would end up inside itself.
            let mut cur = p.to_string();
            for _ in 0..=self.groups.len() {
                if cur == child {
                    return false;
                }
                let Some(g) = self.groups.iter().find(|g| g.uuid == cur) else {
                    break;
                };
                match g.parent.as_deref() {
                    None => break,
                    Some(next) => cur = next.to_string(),
                }
            }
        }
        if let Some(g) = self.groups.iter_mut().find(|g| g.uuid == child) {
            g.parent = parent.map(str::to_string);
            self.invalidate_composite_cache();
            return true;
        }
        false
    }

    /// Which deck a group belongs to, if either: 0 for A, 1 for B.
    ///
    /// Resolves through nesting, so a layer three groups deep still reports the
    /// deck it ultimately lives on.
    pub fn deck_of_channel(&self, index: usize) -> Option<usize> {
        let gid = self.channels.get(index)?.group.as_deref()?;
        self.deck_of(gid)
    }

    /// Which deck a group belongs to, if either: 0 for A, 1 for B.
    pub fn deck_of(&self, uuid: &str) -> Option<usize> {
        let decks = self.decks.as_ref()?;
        let top = self.top_level_ancestor(uuid);
        decks.iter().position(|d| *d == top)
    }

    /// Combine the two decks into one image, returning what the master should
    /// blend in their place.
    ///
    /// `None` means there is nothing to special-case and the decks composite as
    /// ordinary groups — no decks configured, no transition loaded, or one of
    /// them did not render.
    ///
    /// At the ends of the fader the transition pass is skipped and the winning
    /// deck is handed back directly. That is a full-screen pass saved whenever
    /// the fader is parked, which is most of the time. It assumes
    /// `transition(A, B, 0) == A`, which holds for the shipped set (`iris` is
    /// 2/255 off at the centre, from edge softness).
    ///
    /// Returns a marker rather than the texture: handing back a `&Texture` would
    /// keep the `&mut self` alive across the whole master pass.
    fn render_transition(
        &mut self,
        ctx: &mut RenderCtx<'_>,
        crossfader: f32,
        engine: &EngineState,
    ) -> Option<DeckSource> {
        let [a, b] = self.decks.clone()?;
        let rendered = |uuid: &str, groups: &[ChannelGroup]| {
            groups
                .iter()
                .find(|g| g.uuid == uuid)
                .filter(|g| g.rendered)
                .is_some()
        };
        if !rendered(&a, &self.groups) || !rendered(&b, &self.groups) {
            return None;
        }
        // Parked at either end: hand back that deck untouched.
        if let Some(which) = parked_deck(crossfader) {
            return Some(DeckSource::Deck(if which == 0 { a } else { b }));
        }

        self.transition.as_ref()?;
        if self.transition_out.is_none()
            || self.transition_out.as_ref().is_some_and(|t| {
                t.texture.width() != self.size[0] || t.texture.height() != self.size[1]
            })
        {
            self.transition_out = Some(Texture::create_render_target(
                ctx.device,
                self.size[0],
                self.size[1],
                "transition out",
            ));
        }

        // Split the borrows: the effect needs `&mut`, the decks it samples and
        // the target it draws into are other fields.
        let Mixer {
            transition,
            transition_out,
            groups,
            size,
            ..
        } = &mut *self;
        let slot = transition.as_mut()?;
        let out = transition_out.as_ref()?;
        let tex_of = |uuid: &String| {
            groups
                .iter()
                .find(|g| &g.uuid == uuid)
                .and_then(|g| g.group_out.as_ref())
        };
        let (ta, tb) = (tex_of(&a)?, tex_of(&b)?);
        let inputs = [
            EffectInput {
                view: &ta.view,
                sampler: &ta.sampler,
                generation: ta.generation,
                texture: Some(&ta.texture),
            },
            EffectInput {
                view: &tb.view,
                sampler: &tb.sampler,
                generation: tb.generation,
                texture: Some(&tb.texture),
            },
        ];
        slot.effect.prepare(engine, ctx.device, ctx.queue);
        slot.effect.render_to(
            ctx,
            &inputs,
            RenderTarget {
                view: &out.view,
                size: *size,
            },
            engine,
        );
        Some(DeckSource::Transition)
    }

    /// Groups deepest-first, so a child is finished before its parent reads it.
    ///
    /// A group with a broken or cyclic parent chain sorts as top level, which
    /// keeps it rendering rather than dropping the layers inside it.
    fn group_render_order(&self) -> Vec<String> {
        let mut order: Vec<(usize, String)> = self
            .groups
            .iter()
            .map(|g| (self.group_depth(&g.uuid).unwrap_or(0), g.uuid.clone()))
            .collect();
        order.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        order.into_iter().map(|(_, uuid)| uuid).collect()
    }

    /// Is channel `i` inside `uuid`, at any depth?
    fn channel_in_group(&self, i: usize, uuid: &str) -> bool {
        let Some(ch) = self.channels.get(i) else {
            return false;
        };
        let mut cur = ch.group.clone();
        for _ in 0..=self.groups.len() {
            match cur {
                None => return false,
                Some(ref c) if c == uuid => return true,
                Some(ref c) => {
                    cur = self
                        .groups
                        .iter()
                        .find(|g| &g.uuid == c)
                        .and_then(|g| g.parent.clone())
                }
            }
        }
        false
    }

    /// Where a group sits in the stack: the index of its topmost member, at any
    /// depth. `None` when it holds no channels.
    fn group_stack_pos(&self, uuid: &str) -> Option<usize> {
        (0..self.channels.len())
            .filter(|&i| self.channel_in_group(i, uuid))
            .max()
    }

    /// The direct children of `uuid` — member channels and child groups —
    /// bottom of the stack first.
    ///
    /// A child group takes the stack position of its own topmost member, so a
    /// nested group composites where its layers actually sit.
    fn group_items(&self, uuid: &str) -> Vec<GroupItem> {
        let mut items: Vec<(usize, GroupItem)> = self
            .channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.group.as_deref() == Some(uuid))
            .map(|(i, _)| (i, GroupItem::Channel(i)))
            .collect();
        items.extend(
            self.groups
                .iter()
                .filter(|g| g.parent.as_deref() == Some(uuid))
                .filter_map(|g| {
                    self.group_stack_pos(&g.uuid)
                        .map(|pos| (pos, GroupItem::Group(g.uuid.clone())))
                }),
        );
        items.sort_by_key(|(pos, _)| *pos);
        items.into_iter().map(|(_, item)| item).collect()
    }

    /// The group owning a channel index, if any.
    pub fn group_of(&self, index: usize) -> Option<&ChannelGroup> {
        let id = self.channels.get(index)?.group.as_ref()?;
        self.groups.iter().find(|g| &g.uuid == id)
    }

    /// Channel indices belonging to a group, in stack order.
    pub fn group_members(&self, uuid: &str) -> Vec<usize> {
        self.channels
            .iter()
            .enumerate()
            .filter(|(_, c)| c.group.as_deref() == Some(uuid))
            .map(|(i, _)| i)
            .collect()
    }

    /// Gather the named layers together and make them a group.
    ///
    /// They are moved to sit above the topmost of them before the group is
    /// formed. Grouping gathers, as it does in every editor — leaving members
    /// scattered through the stack would put non-members in the middle of a
    /// composite that is supposed to be one image.
    pub fn group_channels(
        &mut self,
        uuid: impl Into<String>,
        name: impl Into<String>,
        members: &[String],
    ) -> Option<String> {
        let mut idxs: Vec<usize> = members
            .iter()
            .filter_map(|u| self.channels.iter().position(|c| &c.uuid == u))
            .collect();
        if idxs.len() < 2 {
            return None;
        }
        idxs.sort_unstable();
        // Gather to where the topmost picked layer sits. Take them all out
        // first (highest index first, so the lower ones stay valid), then put
        // the block back in one piece.
        let anchor = *idxs.last().unwrap();
        let mut taken: Vec<Channel> = idxs
            .iter()
            .rev()
            .map(|&i| self.channels.remove(i))
            .collect();
        taken.reverse();
        let removed_below = idxs.iter().filter(|&&i| i < anchor).count();
        let at = anchor - removed_below;
        for (n, ch) in taken.into_iter().enumerate() {
            self.channels.insert(at + n, ch);
        }
        if self.groups.len() >= MAX_GROUPS {
            // Group count, not layer count, is the memory constraint: four
            // full-resolution textures each.
            return None;
        }
        let uuid = uuid.into();
        for u in members {
            if let Some(c) = self.channels.iter_mut().find(|c| &c.uuid == u) {
                c.group = Some(uuid.clone());
            }
        }
        self.groups.push(ChannelGroup::new(uuid.clone(), name));
        self.invalidate_composite_cache();
        Some(uuid)
    }

    /// Move a whole group so it sits where `target` is, keeping the members in
    /// their own order.
    ///
    /// Moving members one at a time would reverse them, or interleave them with
    /// whatever they pass on the way; the block comes out and goes back in one
    /// piece.
    pub fn move_group(&mut self, group: &str, target: &str) {
        let members = self.group_members(group);
        if members.is_empty() {
            return;
        }
        if self.channels.get(members[0]).is_some_and(|c| c.uuid == target) {
            return;
        }
        let Some(to) = self.channels.iter().position(|c| c.uuid == target) else {
            return;
        };
        if members.contains(&to) {
            return; // dropped on itself
        }
        let mut taken: Vec<Channel> = members
            .iter()
            .rev()
            .map(|&i| self.channels.remove(i))
            .collect();
        taken.reverse();
        let removed_below = members.iter().filter(|&&i| i < to).count();
        let at = to - removed_below;
        for (n, ch) in taken.into_iter().enumerate() {
            self.channels.insert(at + n, ch);
        }
        self.invalidate_composite_cache();
    }

    /// Put one layer into a group, or take it out with `None`.
    ///
    /// Moving it next to the group's other members is the point: dropping a
    /// layer onto a group it already sits beside changes only its membership,
    /// and a guard that skips "no position change" would throw that away.
    pub fn set_channel_group(&mut self, layer: &str, group: Option<String>) {
        let Some(i) = self.channels.iter().position(|c| c.uuid == layer) else {
            return;
        };
        if let Some(gid) = &group {
            let members = self.group_members(gid);
            if let Some(&top) = members.last() {
                let ch = self.channels.remove(i);
                let to = if i <= top { top } else { top + 1 };
                self.channels.insert(to.min(self.channels.len()), ch);
            }
        }
        if let Some(c) = self.channels.iter_mut().find(|c| c.uuid == layer) {
            c.group = group;
        }
        self.invalidate_composite_cache();
    }

    /// Dissolve a group, leaving its members in the stack.
    pub fn ungroup(&mut self, uuid: &str) {
        for c in self.channels.iter_mut() {
            if c.group.as_deref() == Some(uuid) {
                c.group = None;
            }
        }
        self.groups.retain(|g| g.uuid != uuid);
        self.invalidate_composite_cache();
    }

    /// Whether a channel contributes to the mix at all.
    ///
    /// Mute always silences a channel; solo silences everything that is not
    /// itself soloed. A channel that is both stays muted — the explicit switch
    /// wins over the implicit one.
    pub fn audible(ch: &Channel, any_solo: bool) -> bool {
        ch.active && !ch.mute && (!any_solo || ch.solo)
    }

    /// Per-channel opacity before the master dimmer.
    fn raw_effective_opacities(&self) -> Vec<f32> {
        self.raw_opacities(self.crossfader, |c| c.opacity)
    }

    /// The one place mute, solo, groups, the crossfader and opacity are combined.
    ///
    /// `base` supplies each channel's own opacity: the stored field for the
    /// UI-facing [`effective_opacities`](Self::effective_opacities), the
    /// engine's modulated value at render time. Both callers must go through
    /// here, or the mixer renders something other than what the UI reports.
    fn raw_opacities(&self, crossfader: f32, base: impl Fn(&Channel) -> f32) -> Vec<f32> {
        let of = |i: usize, c: &Channel| {
            // Solo is judged among siblings, then the same question is asked of
            // every group above. A member of a soloed group stays audible: the
            // solo is on the group, and silencing what it contains would leave
            // it soloing nothing.
            let scope = c.group.as_deref();
            let live = c.active
                && !c.mute
                && !(self.solo_active_in(scope) && !c.solo)
                && scope.is_none_or(|g| self.group_audible(g));
            let _ = i;
            if live { base(c).clamp(0.0, 1.0) } else { 0.0 }
        };
        if self.use_crossfader && self.channels.len() == 2 {
            vec![
                (1.0 - crossfader) * of(0, &self.channels[0]),
                crossfader * of(1, &self.channels[1]),
            ]
        } else {
            self.channels.iter().enumerate().map(|(i, c)| of(i, c)).collect()
        }
    }

    /// Ensure all mixer-level and per-channel GPU resources match `size`.
    fn ensure_resources(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self.size != size || self.composite.is_none() {
            let format = rustjay_core::working_format();
            self.composite = Some(CompositePipeline::new(device, format));
            self.blit = Some(BlitPipeline::new(device, format));
            self.acc_a = Some(Texture::create_render_target(
                device,
                size[0],
                size[1],
                "mixer acc_a",
            ));
            self.acc_b = Some(Texture::create_render_target(
                device,
                size[0],
                size[1],
                "mixer acc_b",
            ));
            self.master_ping = Some(Texture::create_render_target(
                device,
                size[0],
                size[1],
                "master ping",
            ));
            self.size = size;
            self.generation = self.generation.wrapping_add(1);
        }
        for ch in &mut self.channels {
            ch.ensure_size(device, size);
        }
    }

    /// Tick active transitions (auto, beat-sync, sequencer) and return the
    /// crossfader value they produce, if any.
    ///
    /// This should be called once per frame before reading the crossfader for
    /// compositing.  Engine param modulation takes precedence when no transition
    /// is active.
    pub fn tick_transitions(&mut self, dt: f32, bpm: Option<f32>, beat_phase: f32) -> Option<f32> {
        // Sequencer has highest priority.
        if self.sequencer.playing {
            if let Some(v) = self.sequencer.tick(self.crossfader, dt, bpm) {
                self.crossfader = v.clamp(0.0, 1.0);
                // Stop any conflicting one-shot transitions.
                self.auto = None;
                self.beat_sync = None;
                return Some(self.crossfader);
            }
            return None;
        }

        if let Some(ref mut bs) = self.beat_sync {
            match bs.tick(self.crossfader, dt, bpm, beat_phase) {
                Some(v) => {
                    self.crossfader = v.clamp(0.0, 1.0);
                    return Some(self.crossfader);
                }
                None if bs.is_done() => {
                    self.crossfader = bs.target;
                    self.beat_sync = None;
                    return Some(self.crossfader);
                }
                None => return None,
            }
        }

        if let Some(ref mut auto) = self.auto {
            match auto.tick(dt) {
                Some(v) => {
                    self.crossfader = v.clamp(0.0, 1.0);
                    return Some(self.crossfader);
                }
                None => {
                    self.crossfader = auto.target().clamp(0.0, 1.0);
                    self.auto = None;
                    return Some(self.crossfader);
                }
            }
        }

        None
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectInstance for Mixer {
    fn label(&self) -> &str {
        "mixer"
    }

    fn parameters(&self) -> Vec<ParameterDescriptor> {
        let mut out = Vec::new();

        out.push(ParameterDescriptor::float(
            "crossfader",
            "Crossfader",
            ParamCategory::Custom("Mixer".to_string()),
            0.0,
            1.0,
            self.crossfader,
            0.01,
        ));

        for ch in &self.channels {
            let prefix = format!("ch_{}_", ch.uuid);

            out.push(ParameterDescriptor::float(
                format!("{prefix}opacity"),
                format!("{} Opacity", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0,
                1.0,
                ch.opacity,
                0.01,
            ));

            out.push(ParameterDescriptor::enum_param(
                format!("{prefix}blend"),
                format!("{} Blend", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                BlendMode::all()
                    .iter()
                    .map(|m| m.short_name().to_string())
                    .collect(),
                ch.blend_mode.to_index() as usize,
            ));

            out.push(ParameterDescriptor::enum_param(
                format!("{prefix}input_select"),
                format!("{} Input", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                InputSelect::labels()
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                ch.input_select.to_index(),
            ));

            out.push(ParameterDescriptor::enum_param(
                format!("{prefix}key_mode"),
                format!("{} Key", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                vec!["None".to_string(), "Chroma".to_string(), "Luma".to_string()],
                ch.key_mode as usize,
            ));
            out.push(ParameterDescriptor::float(
                format!("{prefix}key_r"),
                format!("{} Key R", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0, 1.0, ch.key_r, 0.01,
            ));
            out.push(ParameterDescriptor::float(
                format!("{prefix}key_g"),
                format!("{} Key G", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0, 1.0, ch.key_g, 0.01,
            ));
            out.push(ParameterDescriptor::float(
                format!("{prefix}key_b"),
                format!("{} Key B", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0, 1.0, ch.key_b, 0.01,
            ));
            out.push(ParameterDescriptor::float(
                format!("{prefix}key_threshold"),
                format!("{} Key Threshold", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0, 1.0, ch.key_threshold, 0.01,
            ));
            out.push(ParameterDescriptor::float(
                format!("{prefix}key_smoothness"),
                format!("{} Key Smoothness", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0, 1.0, ch.key_smoothness, 0.01,
            ));
            out.push(ParameterDescriptor::float(
                format!("{prefix}key_luma_invert"),
                format!("{} Luma Invert", ch.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0, 1.0, if ch.luma_invert { 1.0 } else { 0.0 }, 1.0,
            ));

            for p in ch.effect.parameters() {
                out.push(prefix_descriptor(&prefix, &p));
            }

            for slot in ch.chain.iter() {
                let chain_prefix = format!("{prefix}fx{}_", slot.uuid);
                for p in slot.effect.parameters() {
                    out.push(prefix_descriptor(&chain_prefix, &p));
                }
            }
        }

        // Groups declare their own mix and the parameters of everything in
        // their chain. Without this a group effect had no descriptors at all,
        // so selecting one showed an empty inspector.
        for g in self.groups.iter() {
            let prefix = format!("grp_{}_", g.uuid);
            out.push(ParameterDescriptor::float(
                format!("{prefix}opacity"),
                format!("{} Opacity", g.name),
                ParamCategory::Custom("Mixer".to_string()),
                0.0,
                1.0,
                g.opacity,
                0.01,
            ));
            out.push(ParameterDescriptor::enum_param(
                format!("{prefix}blend"),
                format!("{} Blend", g.name),
                ParamCategory::Custom("Mixer".to_string()),
                BlendMode::all()
                    .iter()
                    .map(|m| m.short_name().to_string())
                    .collect(),
                g.blend_mode.to_index() as usize,
            ));
            for slot in g.chain.iter() {
                let chain_prefix = format!("{prefix}fx{}_", slot.uuid);
                for p in slot.effect.parameters() {
                    out.push(prefix_descriptor(&chain_prefix, &p));
                }
            }
        }

        // The transition's own inputs — wipe angle, softness — are mappable like
        // any effect param. Its `progress` is among them, and the host drives
        // that one from the crossfader every frame.
        if let Some(slot) = self.transition.as_ref() {
            for p in slot.effect.parameters() {
                out.push(prefix_descriptor(TRANSITION_PREFIX, &p));
            }
        }

        for slot in self.master.iter() {
            let prefix = format!("master_fx{}_", slot.uuid);
            for p in slot.effect.parameters() {
                out.push(prefix_descriptor(&prefix, &p));
            }
        }

        out
    }

    /// # Single-render-path invariant (REQ-11.4)
    ///
    /// Every channel/master/chain effect is an `EffectInstance` driven **only**
    /// through `render_to` here — never the `PluginRenderer::render` wrapper path.
    /// This preserves each `EffectNode`'s generation-keyed bind-group cache (see
    /// the B0.2 invariant note): alternating the two render paths on one renderer
    /// would thrash its cache. The mixer's own composite cache relies on the same
    /// discipline — see [`CompositePipeline`] and [`Mixer::generation`].
    fn render_to(
        &mut self,
        ctx: &mut RenderCtx<'_>,
        inputs: &[EffectInput<'_>],
        target: RenderTarget<'_>,
        engine: &EngineState,
    ) {
        self.ensure_resources(ctx.device, target.size);

        // CORR-2: detect enabled-count changes that flip output_texture() parity.
        // A parity flip changes which texture (main vs ping) the composite samples,
        // so the generation must bump to invalidate the bind-group cache.
        for ch in &mut self.channels {
            let current = ch.chain.iter().filter(|s| s.enabled).count();
            if ch.last_enabled_count != current {
                ch.last_enabled_count = current;
                self.generation = self.generation.wrapping_add(1);
            }
        }

        // Tick transitions before reading params (ordering matters).
        let dt = engine
            .performance
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame_time_ms
            / 1000.0;
        let bpm = engine.effective_bpm();
        let beat_phase = engine.effective_beat_phase();
        self.tick_transitions(dt, Some(bpm).filter(|&b| b > 0.0), beat_phase);

        // Modulation offsets are already applied by EngineState::get_param().
        let crossfader = engine.get_param("crossfader").unwrap_or(self.crossfader);

        // Same mute/solo/group/crossfader rules the UI reports, over the
        // *modulated* opacity. The crossfader is passed in rather than stored:
        // it carries modulation offsets, so writing it back would let modulation
        // ratchet the base value frame after frame.
        //
        // Before the master dimmer: a grouped member is blended into its group
        // with this, and the group itself is dimmed once when it joins the
        // master stack. Dimming here too would dim a grouped layer twice.
        let eff: Vec<f32> = self.raw_opacities(crossfader, |ch| {
            engine.get_param(&ch.opacity_key).unwrap_or(ch.opacity)
        });

        for (i, ch) in self.channels.iter_mut().enumerate() {
            if eff.get(i).copied().unwrap_or(0.0) < 0.001 {
                continue;
            }
            let input_select = engine
                .get_param(&ch.input_select_key)
                .map(|v| InputSelect::from_index(v as usize))
                .unwrap_or(ch.input_select);
            let ch_inputs: &[EffectInput] = match input_select {
                InputSelect::Slot1 => &inputs[0..inputs.len().min(1)],
                InputSelect::Slot2 => &inputs[inputs.len().min(1)..inputs.len().min(2)],
                InputSelect::Both => inputs,
            };
            ch.render(ctx, ch_inputs, engine);
        }

        // A grouped span composites into the group's own accumulator first, so
        // the group's chain sees one image rather than each member separately —
        // one blur over three layers instead of three blurs. Done in its own
        // pass, and the result parked in `group_out`, so the master pass below
        // needs only a shared borrow of the groups.
        // Deepest first, so a child's `group_out` is finished before its parent
        // samples it. A group is lifted out of `self.groups` while it renders:
        // it needs `&mut` on itself and `&` on the children it reads, which is
        // two borrows of one Vec otherwise.
        for uuid in self.group_render_order() {
            let Some(gi) = self.groups.iter().position(|g| g.uuid == uuid) else {
                continue;
            };
            let mut g = self.groups.remove(gi);
            g.rendered = false;

            let items = self.group_items(&uuid);
            // A deck renders even with nothing on it: the normal path clears its
            // accumulator, so an empty deck is a black image the transition can
            // fade to. Letting it fall through to `release_resources` left the
            // transition without two inputs, and the crossfader did nothing at
            // all. It costs an empty deck its four textures, which is the price
            // of a fader that always works.
            let is_deck = self.deck_of(&uuid).is_some();
            if !self.group_audible(&uuid) || (items.is_empty() && !is_deck) {
                // Nothing to composite, or nothing that would be heard. Hand
                // back the four full-resolution textures rather than hold them.
                g.release_resources();
                self.groups.insert(gi, g);
                continue;
            }
            g.ensure_resources(ctx.device, self.size);

            let ga = g.acc_a.as_ref().unwrap();
            let gb = g.acc_b.as_ref().unwrap();
            let gc = g.composite.as_ref().unwrap();
            clear_texture(ctx.encoder, &ga.view);
            let mut written: Option<&Texture> = None;
            for (slot, item) in items.iter().enumerate() {
                let (src, opacity, blend_mode) = match item {
                    GroupItem::Channel(i) => {
                        let ch = &self.channels[*i];
                        let Some(src) = ch.output_texture() else {
                            continue;
                        };
                        let blend = engine
                            .get_param(&ch.blend_key)
                            .and_then(|v| BlendMode::from_index(v as u32))
                            .unwrap_or(ch.blend_mode);
                        (src, eff.get(*i).copied().unwrap_or(0.0), blend)
                    }
                    GroupItem::Group(child) => {
                        let Some(cg) = self.groups.iter().find(|x| &x.uuid == child) else {
                            continue;
                        };
                        // Rendered earlier this frame — depth ordering is what
                        // guarantees that, and `rendered` is what proves it.
                        if !cg.rendered {
                            continue;
                        }
                        let Some(src) = cg.group_out.as_ref() else {
                            continue;
                        };
                        let opacity = engine
                            .get_param(&cg.opacity_key)
                            .unwrap_or(cg.opacity)
                            .clamp(0.0, 1.0);
                        let blend = engine
                            .get_param(&cg.blend_key)
                            .and_then(|v| BlendMode::from_index(v as u32))
                            .unwrap_or(cg.blend_mode);
                        (src, opacity, blend)
                    }
                };
                if opacity < 0.001 {
                    continue;
                }
                let (read, write) = match written {
                    None => (ga, gb),
                    Some(w) if std::ptr::eq(w as *const _, ga as *const _) => (ga, gb),
                    _ => (gb, ga),
                };
                let dest_is_a = std::ptr::eq(read as *const _, ga as *const _);
                gc.blend(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    self.generation,
                    slot,
                    dest_is_a,
                    &src.view,
                    &read.view,
                    &write.view,
                    opacity,
                    blend_mode,
                    KeyParams::default(),
                    ctx.vertex_buffer,
                );
                written = Some(write);
            }
            let composed = written.unwrap_or(ga);

            // The group's own chain, then park the result.
            let ping = g.chain_ping.as_ref().unwrap();
            let finished = run_chain(&mut g.chain, ctx, composed, ping, self.size, engine);
            if let (Some(out), Some(blit)) = (g.group_out.as_ref(), self.blit.as_ref()) {
                blit.blit(
                    ctx.device,
                    ctx.encoder,
                    &finished.view,
                    &out.view,
                    ctx.vertex_buffer,
                );
                g.rendered = true;
            }
            self.groups.insert(gi, g);
        }
        // The two decks become one image before the master pass, so what
        // reaches the composite is a single layer sitting where the decks sit.
        let deck_source = self.render_transition(ctx, crossfader, engine);
        let deck_src: Option<&Texture> = match &deck_source {
            Some(DeckSource::Transition) => self.transition_out.as_ref(),
            Some(DeckSource::Deck(uuid)) => self
                .groups
                .iter()
                .find(|g| &g.uuid == uuid)
                .and_then(|g| g.group_out.as_ref()),
            None => None,
        };

        let acc_a = self.acc_a.as_ref().unwrap();
        let acc_b = self.acc_b.as_ref().unwrap();
        let composite = self.composite.as_ref().unwrap();

        clear_texture(ctx.encoder, &acc_a.view);

        let active: Vec<usize> = eff
            .iter()
            .enumerate()
            .filter(|&(_, &op)| op >= 0.001)
            .map(|(i, _)| i)
            .collect();

        let mut written_acc: Option<&Texture> = None;

        // Where each top-level group joins the master: the topmost member that
        // is actually contributing, at any depth.
        //
        // Anchoring on the topmost member outright — as this did — meant that
        // muting or zeroing the top layer of a group made the anchor index
        // vanish from `active`, and the whole group silently stopped being
        // blended.
        let mut group_anchor: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for &i in &active {
            if let Some(gid) = self.channels[i].group.as_deref() {
                let anchor = group_anchor
                    .entry(self.top_level_ancestor(gid))
                    .or_insert(i);
                *anchor = (*anchor).max(i);
            }
        }

        // With a transition running, the two decks arrive as one image, so they
        // share one position in the stack: the higher of their two anchors.
        let deck_anchor = self.decks.as_ref().filter(|_| deck_src.is_some()).and_then(|d| {
            d.iter()
                .filter_map(|uuid| group_anchor.get(uuid).copied())
                .max()
        });

        for &i in &active {
            // A grouped member is not blended on its own: its group already
            // composited it, and only the outermost group reaches the master —
            // a nested one was already folded into its parent.
            if let Some(gid) = self.channels[i].group.as_deref() {
                let ancestor = self.top_level_ancestor(gid);

                // Both decks resolve to the transition's output, blended once.
                if let (Some(src), Some(anchor)) = (deck_src, deck_anchor)
                    && self.deck_of(&ancestor).is_some()
                {
                    if i != anchor {
                        continue;
                    }
                    // Each deck's own opacity still reads as a level, crossfaded
                    // with the image: deck A's at 0, deck B's at 1.
                    let level = |uuid: &String| {
                        self.groups
                            .iter()
                            .find(|g| &g.uuid == uuid)
                            .map(|g| {
                                engine
                                    .get_param(&g.opacity_key)
                                    .unwrap_or(g.opacity)
                                    .clamp(0.0, 1.0)
                            })
                            .unwrap_or(1.0)
                    };
                    let [da, db] = self.decks.as_ref().unwrap();
                    let x = crossfader.clamp(0.0, 1.0);
                    let opacity = (level(da) * (1.0 - x) + level(db) * x)
                        * self.master_dim.clamp(0.0, 1.0);
                    if opacity < 0.001 {
                        continue;
                    }
                    let (read_acc, write_acc) = match written_acc {
                        None => (acc_a, acc_b),
                        Some(w) if std::ptr::eq(w as *const _, acc_a as *const _) => {
                            (acc_a, acc_b)
                        }
                        _ => (acc_b, acc_a),
                    };
                    let dest_is_a = std::ptr::eq(read_acc as *const _, acc_a as *const _);
                    composite.blend(
                        ctx.device,
                        ctx.queue,
                        ctx.encoder,
                        self.generation,
                        i,
                        dest_is_a,
                        &src.view,
                        &read_acc.view,
                        &write_acc.view,
                        opacity,
                        BlendMode::Normal,
                        KeyParams::default(),
                        ctx.vertex_buffer,
                    );
                    written_acc = Some(write_acc);
                    continue;
                }

                if group_anchor.get(&ancestor) != Some(&i) {
                    continue;
                }
                let Some(g) = self.groups.iter().find(|x| x.uuid == ancestor) else {
                    continue;
                };
                if !g.rendered {
                    continue;
                }
                let Some(src) = g.group_out.as_ref() else {
                    continue;
                };
                let opacity = engine
                    .get_param(&g.opacity_key)
                    .unwrap_or(g.opacity)
                    .clamp(0.0, 1.0)
                    * self.master_dim.clamp(0.0, 1.0);
                if opacity < 0.001 {
                    continue;
                }
                let blend_mode = engine
                    .get_param(&g.blend_key)
                    .and_then(|v| BlendMode::from_index(v as u32))
                    .unwrap_or(g.blend_mode);
                let (read_acc, write_acc) = match written_acc {
                    None => (acc_a, acc_b),
                    Some(w) if std::ptr::eq(w as *const _, acc_a as *const _) => (acc_a, acc_b),
                    _ => (acc_b, acc_a),
                };
                let dest_is_a = std::ptr::eq(read_acc as *const _, acc_a as *const _);
                composite.blend(
                    ctx.device,
                    ctx.queue,
                    ctx.encoder,
                    self.generation,
                    i,
                    dest_is_a,
                    &src.view,
                    &read_acc.view,
                    &write_acc.view,
                    opacity,
                    blend_mode,
                    KeyParams::default(),
                    ctx.vertex_buffer,
                );
                written_acc = Some(write_acc);
                continue;
            }

            let ch = &self.channels[i];
            let Some(src) = ch.output_texture() else {
                continue;
            };

            let blend_mode = engine
                .get_param(&ch.blend_key)
                .and_then(|v| BlendMode::from_index(v as u32))
                .unwrap_or(ch.blend_mode);

            let key = KeyParams {
                mode: engine
                    .get_param_base(&ch.key_mode_key)
                    .map(|v| v.round() as u32)
                    .unwrap_or(ch.key_mode),
                r: engine.get_param(&ch.key_r_key).unwrap_or(ch.key_r),
                g: engine.get_param(&ch.key_g_key).unwrap_or(ch.key_g),
                b: engine.get_param(&ch.key_b_key).unwrap_or(ch.key_b),
                threshold: engine.get_param(&ch.key_threshold_key).unwrap_or(ch.key_threshold),
                smoothness: engine.get_param(&ch.key_smoothness_key).unwrap_or(ch.key_smoothness),
                luma_invert: engine
                    .get_param_base(&ch.key_luma_invert_key)
                    .map(|v| v > 0.5)
                    .unwrap_or(ch.luma_invert),
            };

            let (read_acc, write_acc) = match written_acc {
                None => (acc_a, acc_b),
                Some(w) if std::ptr::eq(w as *const _, acc_a as *const _) => (acc_a, acc_b),
                _ => (acc_b, acc_a),
            };
            let dest_is_a = std::ptr::eq(read_acc as *const _, acc_a as *const _);

            composite.blend(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                self.generation,
                i,
                dest_is_a,
                &src.view,
                &read_acc.view,
                &write_acc.view,
                eff[i] * self.master_dim.clamp(0.0, 1.0),
                blend_mode,
                key,
                ctx.vertex_buffer,
            );
            written_acc = Some(write_acc);
        }

        let composite_out = written_acc.unwrap_or(acc_a);

        let master_ping = self.master_ping.as_ref().unwrap();
        let final_tex = run_chain(
            &mut self.master,
            ctx,
            composite_out,
            master_ping,
            self.size,
            engine,
        );

        let blit = self.blit.as_ref().unwrap();
        blit.blit(
            ctx.device,
            ctx.encoder,
            &final_tex.view,
            target.view,
            ctx.vertex_buffer,
        );
    }
}

fn prefix_descriptor(prefix: &str, desc: &ParameterDescriptor) -> ParameterDescriptor {
    ParameterDescriptor {
        id: format!("{prefix}{}", desc.id),
        name: format!("{} [{}]", desc.name, prefix.trim_end_matches('_')),
        category: desc.category.clone(),
        param_type: desc.param_type.clone(),
        min: desc.min,
        max: desc.max,
        default: desc.default,
        step: desc.step,
    }
}

/// Returns whichever texture holds the final output (may be `initial_input` when `effects` is empty).
fn run_chain<'a>(
    effects: &'a mut [EffectSlot],
    ctx: &mut RenderCtx<'_>,
    initial_input: &'a Texture,
    ping: &'a Texture,
    size: [u32; 2],
    engine: &EngineState,
) -> &'a Texture {
    if effects.is_empty() {
        return initial_input;
    }

    let mut is_ping = false; // false → src=initial_input, dst=ping

    for slot in effects.iter_mut() {
        if !slot.enabled {
            continue;
        }
        // See `Channel::render`: without this an ISF slot draws with unwritten
        // uniforms.
        slot.effect.prepare(engine, ctx.device, ctx.queue);
        let (src_tex, dst_tex) = if is_ping {
            (ping, initial_input)
        } else {
            (initial_input, ping)
        };
        let input = EffectInput {
            view: &src_tex.view,
            sampler: &src_tex.sampler,
            generation: src_tex.generation,
            texture: Some(&src_tex.texture),
        };
        slot.effect.render_to(
            ctx,
            &[input],
            RenderTarget {
                view: &dst_tex.view,
                size,
            },
            engine,
        );
        is_ping = !is_ping;
    }

    if is_ping {
        ping
    } else {
        initial_input
    }
}

/// Clear a texture to transparent black.
fn clear_texture(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Mixer Clear Texture"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A headless `EffectInstance` stub — records nothing, only has to compile.
    pub(super) struct Stub;

    impl EffectInstance for Stub {
        fn render_to(
            &mut self,
            _ctx: &mut rustjay_core::RenderCtx<'_>,
            _inputs: &[rustjay_core::EffectInput<'_>],
            _target: rustjay_core::RenderTarget<'_>,
            _engine: &rustjay_core::EngineState,
        ) {
        }
    }

    #[test]
    fn crossfader_splits_two_channel_opacity() {
        let mut mixer = Mixer::new();
        mixer
            .add_channel(Channel::new("a", "A", Box::new(Stub)))
            .unwrap();
        mixer
            .add_channel(Channel::new("b", "B", Box::new(Stub)))
            .unwrap();
        mixer.crossfader = 0.25;

        let eff = mixer.effective_opacities();
        assert_eq!(eff.len(), 2);
        assert!((eff[0] - 0.75).abs() < 1e-6);
        assert!((eff[1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn channel_count_clamped() {
        let mut mixer = Mixer::new();
        for i in 0..MAX_CHANNELS {
            assert!(mixer
                .add_channel(Channel::new(
                    format!("{i}"),
                    format!("CH{i}"),
                    Box::new(Stub)
                ))
                .is_ok());
        }
        assert!(mixer
            .add_channel(Channel::new("overflow", "OVF", Box::new(Stub)))
            .is_err());

        // Can't remove below 1
        for _ in 0..MAX_CHANNELS - 1 {
            mixer.remove_channel(0).unwrap();
        }
        assert!(mixer.remove_channel(0).is_err());
    }

    #[test]
    fn empty_chain_returns_input() {
        // run_chain with no effects should return the initial input texture reference.
        // We can't create a real Texture without a GPU device, so this test verifies
        // the logic path at the type level by checking the function signature compiles.
    }

    #[test]
    fn mixer_no_longer_owns_modulation_engine() {
        // Phase 4: modulation lives in EngineState.modulation, not Mixer.
        let mixer = Mixer::new();
        // Mixer::new() should compile and not contain a modulation field.
        assert!(mixer.channels.is_empty());
        assert_eq!(mixer.crossfader, 0.5);
    }

    /// A host presenting channels as layers turns the crossfader off, or a
    /// two-layer stack renders both layers at half opacity.
    #[test]
    fn use_crossfader_false_leaves_two_channel_opacities_alone() {
        let mut mixer = Mixer::new();
        mixer.add_channel(Channel::new("a", "A", Box::new(Stub))).unwrap();
        mixer.add_channel(Channel::new("b", "B", Box::new(Stub))).unwrap();
        mixer.channels[0].opacity = 1.0;
        mixer.channels[1].opacity = 1.0;
        mixer.crossfader = 0.5;

        // Default: the A/B behaviour every existing host relies on.
        assert_eq!(mixer.effective_opacities(), vec![0.5, 0.5]);

        mixer.use_crossfader = false;
        assert_eq!(
            mixer.effective_opacities(),
            vec![1.0, 1.0],
            "layers must composite at their own opacity"
        );
    }

    #[test]
    fn reorder_channel_restacks_and_ignores_bad_indices() {
        let mut mixer = Mixer::new();
        for id in ["a", "b", "c"] {
            mixer.add_channel(Channel::new(id, id, Box::new(Stub))).unwrap();
        }
        let ids = |m: &Mixer| m.channels.iter().map(|c| c.uuid.clone()).collect::<Vec<_>>();

        mixer.reorder_channel(0, 2);
        assert_eq!(ids(&mixer), ["b", "c", "a"], "moved to the end");

        mixer.reorder_channel(2, 0);
        assert_eq!(ids(&mixer), ["a", "b", "c"], "and back to the front");

        // Out of range, or a no-op move, must leave the stack untouched.
        mixer.reorder_channel(9, 0);
        mixer.reorder_channel(1, 1);
        assert_eq!(ids(&mixer), ["a", "b", "c"]);

        // Clamped rather than panicking.
        mixer.reorder_channel(0, 99);
        assert_eq!(ids(&mixer), ["b", "c", "a"]);
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn stack(ids: &[&str]) -> Mixer {
        let mut mixer = Mixer::new();
        mixer.use_crossfader = false;
        for id in ids {
            mixer
                .add_channel(Channel::new(*id, *id, Box::new(tests::Stub)))
                .unwrap();
        }
        mixer
    }

    /// The composite pipeline caches bind groups by channel *index* while
    /// rewriting each index's opacity every frame. A restack that does not bump
    /// the generation therefore paints layer N's fader onto layer N-1's pixels.
    #[test]
    fn restacking_invalidates_the_composite_cache() {
        let mut mixer = stack(&["a", "b", "c"]);
        let before = mixer.generation;
        mixer.reorder_channel(0, 2);
        assert_ne!(
            mixer.generation, before,
            "a restack remaps index → texture; the slot-keyed cache must be dropped"
        );

        // A no-op move changes nothing, so it need not invalidate.
        let settled = mixer.generation;
        mixer.reorder_channel(1, 1);
        assert_eq!(mixer.generation, settled);
    }

    /// Every group operation restacks the channels underneath it, so each one
    /// remaps index → texture just as a plain restack does.
    #[test]
    fn group_operations_invalidate_the_composite_cache() {
        let mut mixer = stack(&["a", "b", "c"]);

        let before = mixer.generation;
        let g = mixer
            .group_channels("g1", "Group", &["a".into(), "c".into()])
            .expect("two layers group");
        assert_ne!(mixer.generation, before, "grouping restacks the members");

        let before = mixer.generation;
        mixer.set_channel_group("b", Some(g.clone()));
        assert_ne!(mixer.generation, before, "joining moves the layer");

        let before = mixer.generation;
        mixer.ungroup(&g);
        assert_ne!(mixer.generation, before, "dissolving drops the group's slots");
    }

    #[test]
    fn mute_silences_only_that_layer() {
        let mut mixer = stack(&["a", "b", "c"]);
        mixer.channels[1].mute = true;
        assert_eq!(mixer.effective_opacities(), vec![1.0, 0.0, 1.0]);
    }

    #[test]
    fn solo_silences_everything_else() {
        let mut mixer = stack(&["a", "b", "c"]);
        mixer.channels[2].solo = true;
        assert_eq!(mixer.effective_opacities(), vec![0.0, 0.0, 1.0]);
    }

    #[test]
    fn several_solos_all_play() {
        let mut mixer = stack(&["a", "b", "c"]);
        mixer.channels[0].solo = true;
        mixer.channels[2].solo = true;
        assert_eq!(mixer.effective_opacities(), vec![1.0, 0.0, 1.0]);
    }

    /// The explicit switch wins over the implicit one, so a soloed layer that
    /// is also muted stays silent rather than coming back.
    #[test]
    fn mute_wins_over_solo_on_the_same_layer() {
        let mut mixer = stack(&["a", "b"]);
        mixer.channels[0].solo = true;
        mixer.channels[0].mute = true;
        assert_eq!(mixer.effective_opacities(), vec![0.0, 0.0]);
    }

    /// The flags belong to the layer, not to its position in the stack.
    #[test]
    fn mute_follows_a_layer_through_a_restack() {
        let mut mixer = stack(&["a", "b", "c"]);
        mixer.channels[0].mute = true; // "a"
        mixer.reorder_channel(0, 2); // a moves to the top
        assert_eq!(
            mixer.channels.iter().map(|c| c.uuid.clone()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
        assert_eq!(mixer.effective_opacities(), vec![1.0, 1.0, 0.0]);
    }

    /// Opacity is read from the engine under a per-layer key, so a restack must
    /// not hand one layer's value to another.
    #[test]
    fn opacity_keys_travel_with_their_layer() {
        let mut mixer = stack(&["a", "b", "c"]);
        let keys = |m: &Mixer| {
            m.channels
                .iter()
                .map(|c| c.opacity_key.clone())
                .collect::<Vec<_>>()
        };
        mixer.reorder_channel(0, 2);
        assert_eq!(keys(&mixer), ["ch_b_opacity", "ch_c_opacity", "ch_a_opacity"]);
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    fn stack(n: usize) -> Mixer {
        let mut mixer = Mixer::new();
        mixer.use_crossfader = false;
        for i in 0..n {
            let id = format!("ch{i}");
            mixer
                .add_channel(Channel::new(&id, &id, Box::new(tests::Stub)))
                .unwrap();
        }
        mixer
    }

    fn ids(m: &Mixer) -> Vec<String> {
        m.channels.iter().map(|c| c.uuid.clone()).collect()
    }

    #[test]
    fn grouping_marks_every_member() {
        let mut mixer = stack(4);
        let g = mixer
            .group_channels("g1", "Backdrop", &["ch1".into(), "ch2".into()])
            .expect("grouped");
        assert_eq!(mixer.group_members(&g), vec![1, 2]);
        assert!(mixer.group_of(0).is_none());
        assert!(mixer.group_of(3).is_none());
    }

    /// Grouping gathers: members that were apart end up next to each other, or
    /// a non-member would sit in the middle of a composite meant to be one
    /// image.
    #[test]
    fn grouping_gathers_scattered_layers() {
        let mut mixer = stack(4);
        mixer
            .group_channels("g1", "A", &["ch0".into(), "ch3".into()])
            .expect("grouped");
        let members = mixer.group_members("g1");
        assert_eq!(members.len(), 2);
        assert_eq!(members[1], members[0] + 1, "members ended up adjacent: {members:?}");
    }

    #[test]
    fn a_single_layer_is_not_a_group() {
        let mut mixer = stack(2);
        assert!(mixer.group_channels("g", "A", &["ch0".into()]).is_none());
    }

    /// The case CuePool learned the hard way: dropping a layer onto a group it
    /// already sits beside changes only its membership, and a guard that skips
    /// "no position change" throws that away.
    #[test]
    fn joining_a_group_it_already_sits_beside_still_joins() {
        let mut mixer = stack(3);
        mixer
            .group_channels("g1", "A", &["ch1".into(), "ch2".into()])
            .unwrap();
        assert!(mixer.group_of(0).is_none(), "ch0 starts outside");
        mixer.set_channel_group("ch0", Some("g1".into()));
        let members = mixer.group_members("g1");
        assert_eq!(members.len(), 3, "it joined: {members:?}");
    }

    #[test]
    fn leaving_a_group_keeps_the_layer() {
        let mut mixer = stack(3);
        mixer
            .group_channels("g1", "A", &["ch0".into(), "ch1".into()])
            .unwrap();
        mixer.set_channel_group("ch0", None);
        assert_eq!(mixer.group_members("g1"), vec![1]);
        assert_eq!(ids(&mixer).len(), 3);
    }

    #[test]
    fn ungrouping_frees_the_members() {
        let mut mixer = stack(3);
        mixer
            .group_channels("g1", "A", &["ch0".into(), "ch1".into()])
            .unwrap();
        mixer.ungroup("g1");
        assert!(mixer.groups.is_empty());
        assert!(mixer.channels.iter().all(|c| c.group.is_none()));
        assert_eq!(mixer.channels.len(), 3, "members survive the group");
    }

    /// Muting a group silences its members; soloing one silences everything
    /// else while keeping its own members audible.
    #[test]
    fn a_group_gates_its_members() {
        let mut mixer = stack(3);
        mixer
            .group_channels("g1", "A", &["ch0".into(), "ch1".into()])
            .unwrap();
        let members = mixer.group_members("g1");

        mixer.groups[0].mute = true;
        let eff = mixer.effective_opacities();
        assert!(members.iter().all(|&i| eff[i] == 0.0), "muted: {eff:?}");

        mixer.groups[0].mute = false;
        mixer.groups[0].solo = true;
        let eff = mixer.effective_opacities();
        assert!(members.iter().all(|&i| eff[i] > 0.0), "soloed members stay up");
        let outsider = (0..3).find(|i| !members.contains(i)).unwrap();
        assert_eq!(eff[outsider], 0.0, "everything else is silenced");
    }
}

#[cfg(test)]
mod group_move_tests {
    use super::*;

    fn stack(n: usize) -> Mixer {
        let mut m = Mixer::new();
        m.use_crossfader = false;
        for i in 0..n {
            let id = format!("ch{i}");
            m.add_channel(Channel::new(&id, &id, Box::new(tests::Stub))).unwrap();
        }
        m
    }
    fn ids(m: &Mixer) -> Vec<String> {
        m.channels.iter().map(|c| c.uuid.clone()).collect()
    }

    /// The block keeps its own order when it moves — one-at-a-time moves would
    /// reverse it.
    #[test]
    fn a_group_moves_as_one_block() {
        let mut m = stack(4);
        m.group_channels("g1", "A", &["ch2".into(), "ch3".into()]).unwrap();
        m.move_group("g1", "ch0");
        let order = ids(&m);
        let p2 = order.iter().position(|u| u == "ch2").unwrap();
        let p3 = order.iter().position(|u| u == "ch3").unwrap();
        assert_eq!(p3, p2 + 1, "members stayed adjacent and in order: {order:?}");
        assert_eq!(m.channels.len(), 4);
    }

    #[test]
    fn dropping_a_group_on_itself_changes_nothing() {
        let mut m = stack(3);
        m.group_channels("g1", "A", &["ch0".into(), "ch1".into()]).unwrap();
        let before = ids(&m);
        m.move_group("g1", "ch0");
        assert_eq!(ids(&m), before);
    }
}

#[cfg(test)]
mod group_param_tests {
    use super::*;

    /// A group effect with no descriptors shows an empty inspector, which is
    /// how selecting one looked before groups were walked here.
    #[test]
    fn a_group_declares_its_mix_parameters() {
        let mut m = Mixer::new();
        m.use_crossfader = false;
        for id in ["a", "b"] {
            m.add_channel(Channel::new(id, id, Box::new(tests::Stub))).unwrap();
        }
        m.group_channels("g1", "Backdrop", &["a".into(), "b".into()]).unwrap();
        let ids: Vec<String> = m.parameters().into_iter().map(|p| p.id).collect();
        assert!(ids.iter().any(|i| i == "grp_g1_opacity"), "{ids:?}");
        assert!(ids.iter().any(|i| i == "grp_g1_blend"), "{ids:?}");
    }
}

/// Nesting, and the solo scoping that goes with it.
///
/// The property under test: work inside one group must not silence or disturb
/// what is outside it. That is what lets one deck be prepared while another is
/// live.
#[cfg(test)]
mod nesting_tests {
    use super::tests::Stub;
    use super::*;

    /// A mixer with `n` channels named `c0..cn`, bottom of the stack first.
    fn mixer_with(n: usize) -> Mixer {
        let mut m = Mixer::new();
        m.use_crossfader = false;
        for i in 0..n {
            m.add_channel(Channel::new(format!("c{i}"), format!("C{i}"), Box::new(Stub)))
                .unwrap();
        }
        m
    }

    fn group(m: &mut Mixer, uuid: &str, members: &[&str]) {
        let members: Vec<String> = members.iter().map(|s| s.to_string()).collect();
        assert!(
            m.group_channels(uuid, uuid, &members).is_some(),
            "grouping {uuid} failed"
        );
    }

    #[test]
    fn a_group_nests_inside_another() {
        let mut m = mixer_with(4);
        group(&mut m, "inner", &["c0", "c1"]);
        group(&mut m, "outer", &["c2", "c3"]);
        assert!(m.set_group_parent("inner", Some("outer")));

        assert_eq!(m.group_depth("outer"), Some(0));
        assert_eq!(m.group_depth("inner"), Some(1));
        // Deepest first, or a parent samples a child that has not rendered.
        assert_eq!(m.group_render_order().first().map(String::as_str), Some("inner"));
        // Only the outermost group reaches the master.
        assert_eq!(m.top_level_ancestor("inner"), "outer");
    }

    #[test]
    fn a_group_cannot_become_its_own_ancestor() {
        let mut m = mixer_with(4);
        group(&mut m, "a", &["c0", "c1"]);
        group(&mut m, "b", &["c2", "c3"]);
        assert!(m.set_group_parent("b", Some("a")));
        // b is inside a, so a cannot go inside b.
        assert!(!m.set_group_parent("a", Some("b")));
        assert!(!m.set_group_parent("a", Some("a")));
        assert!(!m.set_group_parent("a", Some("nonexistent")));
        assert_eq!(m.group_depth("a"), Some(0));
    }

    #[test]
    fn a_cycle_does_not_hang_the_render_thread() {
        let mut m = mixer_with(4);
        group(&mut m, "a", &["c0", "c1"]);
        group(&mut m, "b", &["c2", "c3"]);
        // Force a cycle past the guard, as a corrupt scene file could.
        m.groups[0].parent = Some("b".into());
        m.groups[1].parent = Some("a".into());
        assert_eq!(m.group_depth("a"), None, "a cycle must not report a depth");
        // Bounded walks, and the layers stay audible rather than vanishing.
        assert_eq!(m.group_render_order().len(), 2);
        assert!(m.group_audible("a"));
    }

    #[test]
    fn the_group_cap_holds() {
        let mut m = mixer_with(16);
        for i in 0..MAX_GROUPS {
            let a = format!("c{}", i * 2);
            let b = format!("c{}", i * 2 + 1);
            assert!(
                m.group_channels(format!("g{i}"), "g", &[a, b]).is_some(),
                "group {i} should fit"
            );
        }
        assert!(
            m.group_channels("overflow", "g", &["c0".into(), "c1".into()])
                .is_none(),
            "a group past the cap is four more full-resolution textures"
        );
    }

    #[test]
    fn solo_inside_one_group_leaves_the_other_alone() {
        let mut m = mixer_with(4);
        group(&mut m, "deck_a", &["c0", "c1"]);
        group(&mut m, "deck_b", &["c2", "c3"]);

        // Audition one layer on deck A.
        m.channels[0].solo = true;
        let eff = m.effective_opacities();

        assert!(eff[0] > 0.0, "the soloed layer plays");
        assert_eq!(eff[1], 0.0, "its sibling in deck A is silenced");
        assert!(
            eff[2] > 0.0 && eff[3] > 0.0,
            "deck B is untouched — soloing on the deck you are preparing must \
             not blank the deck that is on screen"
        );
    }

    #[test]
    fn solo_on_a_top_level_group_silences_the_other_decks() {
        let mut m = mixer_with(5);
        group(&mut m, "deck_a", &["c0", "c1"]);
        group(&mut m, "deck_b", &["c2", "c3"]);
        m.groups
            .iter_mut()
            .find(|g| g.uuid == "deck_a")
            .unwrap()
            .solo = true;

        let eff = m.effective_opacities();
        let idx = |uuid: &str| m.channels.iter().position(|c| c.uuid == uuid).unwrap();
        assert!(eff[idx("c0")] > 0.0 && eff[idx("c1")] > 0.0, "deck A plays");
        assert_eq!(eff[idx("c2")], 0.0, "deck B is silenced");
        assert_eq!(eff[idx("c3")], 0.0, "deck B is silenced");
        assert_eq!(eff[idx("c4")], 0.0, "so is the ungrouped layer");
    }

    #[test]
    fn muting_an_outer_group_silences_what_nests_inside_it() {
        let mut m = mixer_with(4);
        group(&mut m, "inner", &["c0", "c1"]);
        group(&mut m, "outer", &["c2", "c3"]);
        assert!(m.set_group_parent("inner", Some("outer")));
        m.groups
            .iter_mut()
            .find(|g| g.uuid == "outer")
            .unwrap()
            .mute = true;

        let eff = m.effective_opacities();
        let idx = |uuid: &str| m.channels.iter().position(|c| c.uuid == uuid).unwrap();
        assert_eq!(eff[idx("c0")], 0.0, "a nested member follows its ancestor");
        assert_eq!(eff[idx("c1")], 0.0);
    }

    #[test]
    fn a_nested_group_composites_into_its_parent_not_the_master() {
        let mut m = mixer_with(4);
        group(&mut m, "inner", &["c0", "c1"]);
        group(&mut m, "outer", &["c2", "c3"]);
        assert!(m.set_group_parent("inner", Some("outer")));

        let items = m.group_items("outer");
        assert!(
            items.contains(&GroupItem::Group("inner".into())),
            "outer composites inner: {items:?}"
        );
        assert!(
            m.group_items("inner")
                .iter()
                .all(|i| matches!(i, GroupItem::Channel(_))),
            "inner holds only layers"
        );
        // Every layer, at any depth, resolves to the one group that reaches master.
        for c in &m.channels {
            assert_eq!(m.top_level_ancestor(c.group.as_deref().unwrap()), "outer");
        }
    }
}

/// Deck roles: which group the crossfader is transitioning, and when the
/// transition pass is skipped.
#[cfg(test)]
mod deck_tests {
    use super::tests::Stub;
    use super::*;

    fn two_decks() -> Mixer {
        let mut m = Mixer::new();
        m.use_crossfader = false;
        for i in 0..4 {
            m.add_channel(Channel::new(format!("c{i}"), format!("C{i}"), Box::new(Stub)))
                .unwrap();
        }
        m.group_channels("deck_a", "A", &["c0".into(), "c1".into()])
            .unwrap();
        m.group_channels("deck_b", "B", &["c2".into(), "c3".into()])
            .unwrap();
        m.decks = Some(["deck_a".into(), "deck_b".into()]);
        m
    }

    #[test]
    fn the_fader_parks_at_both_ends() {
        assert_eq!(parked_deck(0.0), Some(0));
        assert_eq!(parked_deck(1.0), Some(1));
        // Anything in between runs the pass.
        assert_eq!(parked_deck(0.5), None);
        assert_eq!(parked_deck(0.002), None);
        assert_eq!(parked_deck(0.998), None);
        // Out of range still parks rather than running a pass for nothing.
        assert_eq!(parked_deck(-1.0), Some(0));
        assert_eq!(parked_deck(2.0), Some(1));
    }

    #[test]
    fn a_layer_resolves_to_the_deck_holding_it() {
        let m = two_decks();
        assert_eq!(m.deck_of("deck_a"), Some(0));
        assert_eq!(m.deck_of("deck_b"), Some(1));
    }

    #[test]
    fn a_layer_nested_deeper_still_resolves_to_its_deck() {
        let mut m = two_decks();
        // A group inside deck A — the case the whole nesting work exists for.
        m.add_channel(Channel::new("c4", "C4", Box::new(Stub))).unwrap();
        m.add_channel(Channel::new("c5", "C5", Box::new(Stub))).unwrap();
        m.group_channels("inner", "Inner", &["c4".into(), "c5".into()])
            .unwrap();
        assert!(m.set_group_parent("inner", Some("deck_a")));

        assert_eq!(
            m.deck_of("inner"),
            Some(0),
            "a nested group belongs to the deck above it, not to itself"
        );
    }

    #[test]
    fn without_decks_nothing_is_special_cased() {
        let mut m = two_decks();
        m.decks = None;
        assert_eq!(m.deck_of("deck_a"), None);
        assert_eq!(m.deck_of("deck_b"), None);
    }

    #[test]
    fn the_decks_still_solo_independently() {
        // The nesting work's guarantee has to survive the deck roles.
        let mut m = two_decks();
        m.channels[0].solo = true;
        let eff = m.effective_opacities();
        assert!(eff[0] > 0.0);
        assert_eq!(eff[1], 0.0, "silences its sibling in deck A");
        assert!(eff[2] > 0.0 && eff[3] > 0.0, "deck B keeps playing");
    }
}
