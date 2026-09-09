//! Per-layer thumbnails.
//!
//! Each layer owns a small render target that its output is blitted into once
//! per frame, and egui draws that. The alternative — the engine's own preview
//! path — registers a full-resolution texture and copies into it, which is
//! affordable for two or three previews and not for one per layer.
//!
//! The texture is ours, so the registered id never goes stale when a channel's
//! output ping-pongs between buffers: only the blit's *source* changes.

use std::collections::HashMap;

use rustjay_mixer::Mixer;
use rustjay_mixer::blit::BlitPipeline;

/// Small enough to cost nothing, large enough to tell two clips apart.
const THUMB_W: u32 = 160;
const THUMB_H: u32 = 90;

/// Refresh previews every Nth frame rather than every frame.
///
/// Each preview is its own render pass and allocates a bind group, measured at
/// roughly 30us per layer — about 0.5ms, or 3% of a 60fps frame, across a full
/// sixteen.
///
/// This runs on the *output* render hook, which is paced by `target_fps` (60 by
/// default). The GUI that displays these draws on its own slower cadence —
/// `UI_RENDER_INTERVAL`, 33ms, so 30fps — which meant every second blit was
/// produced for a frame nobody ever drew. A divisor of three lands at 20fps:
/// under the GUI's rate, so no blit is wasted, and still live enough to read a
/// layer at a glance.
const REFRESH_EVERY: u64 = 3;

/// How long previews keep updating after the last one was drawn.
///
/// A grace window rather than an immediate stop, so previews are already warm
/// when a panel comes back, and a tab switch cannot strobe them.
const IDLE_GRACE_FRAMES: u64 = 30;

pub const ASPECT: f32 = THUMB_W as f32 / THUMB_H as f32;

struct Entry {
    #[allow(dead_code)] // Keeps the texture alive for the registered view.
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    key: u64,
    registered: bool,
}

#[derive(Default)]
pub struct Thumbnails {
    pipeline: Option<BlitPipeline>,
    entries: HashMap<String, Entry>,
    next_key: u64,
    /// Frames seen, for the refresh divisor and the idle window.
    frame: u64,
    /// The frame a preview was last actually drawn on.
    last_wanted: u64,
    /// Resolved egui ids by layer uuid, refreshed by [`Self::sync`].
    pub ids: HashMap<String, egui::TextureId>,
}

impl Thumbnails {
    /// Note that something drew a preview this frame.
    ///
    /// Called from wherever [`Self::ids`] is read. Without it the previews
    /// refresh at full rate forever, including while nothing is on screen to
    /// show them — and they are drawn in the layer list, so "is the Library
    /// panel open" would be the wrong question to ask.
    pub fn mark_wanted(&mut self) {
        self.last_wanted = self.frame;
    }

    /// Blit every layer's current output into its thumbnail. Call after the
    /// mixer has rendered, or the outputs are a frame stale.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        vertex_buffer: &wgpu::Buffer,
        mixer: &Mixer,
    ) {
        self.frame = self.frame.wrapping_add(1);
        // Nothing has drawn a preview lately, so there is nothing to keep
        // current. The textures hold their last contents either way.
        if self.frame.saturating_sub(self.last_wanted) > IDLE_GRACE_FRAMES {
            return;
        }
        if !self.frame.is_multiple_of(REFRESH_EVERY) {
            return;
        }

        let pipeline = self
            .pipeline
            .get_or_insert_with(|| BlitPipeline::new(device, wgpu::TextureFormat::Bgra8Unorm));

        for channel in &mixer.channels {
            let Some(source) = channel.output_texture() else {
                continue;
            };
            if !self.entries.contains_key(&channel.uuid) {
                self.next_key += 1;
                let key = self.next_key;
                let entry = {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("layer thumbnail"),
                        size: wgpu::Extent3d {
                            width: THUMB_W,
                            height: THUMB_H,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Bgra8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    });
                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    Entry {
                        texture,
                        view,
                        key,
                        registered: false,
                    }
                };
                self.entries.insert(channel.uuid.clone(), entry);
            }
            let entry = &self.entries[&channel.uuid];
            // A blit samples, so this downscales; a plain texture copy would
            // take the top-left 160x90 corner instead.
            pipeline.blit(device, encoder, &source.view, &entry.view, vertex_buffer);
        }

        // Deleted layers must not keep a texture (or a registered id) alive.
        self.entries
            .retain(|uuid, _| mixer.channels.iter().any(|c| &c.uuid == uuid));
    }

    /// Register anything new and refresh [`Self::ids`]. Only the shell is
    /// handed the host, so it does this once a frame on everyone's behalf.
    pub fn sync(&mut self, host: &mut rustjay_gui::EguiControlGui) {
        for entry in self.entries.values_mut() {
            if !entry.registered {
                entry.registered = true;
                host.pending_textures.push((entry.key, entry.view.clone()));
            }
        }
        self.ids.clear();
        for (uuid, entry) in &self.entries {
            // Absent until the host settles the request at the end of the
            // frame the layer was created in, so a new layer shows its
            // placeholder for one frame.
            if let Some(id) = host.registered_textures.get(&entry.key) {
                self.ids.insert(uuid.clone(), *id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The blit itself needs a GPU, so this exercises the decision around it:
    /// does `update` intend to do work this frame?
    impl Thumbnails {
        fn would_refresh(&mut self) -> bool {
            self.frame = self.frame.wrapping_add(1);
            if self.frame.saturating_sub(self.last_wanted) > IDLE_GRACE_FRAMES {
                return false;
            }
            self.frame.is_multiple_of(REFRESH_EVERY)
        }
    }

    #[test]
    fn previews_refresh_at_a_fraction_of_the_frame_rate() {
        let mut t = Thumbnails::default();
        t.mark_wanted();
        let refreshed = (0..30).filter(|_| t.would_refresh()).count();
        // Every third frame, not every frame.
        assert_eq!(refreshed, 10, "expected 1 in {REFRESH_EVERY} frames");
    }

    #[test]
    fn previews_stop_when_nothing_is_drawing_them() {
        let mut t = Thumbnails::default();
        t.mark_wanted();
        // Inside the grace window, work still happens.
        for _ in 0..IDLE_GRACE_FRAMES {
            t.would_refresh();
        }
        assert!(
            t.frame.saturating_sub(t.last_wanted) <= IDLE_GRACE_FRAMES,
            "still within grace"
        );
        // Well past it, with nobody marking, every frame is skipped.
        for _ in 0..60 {
            t.would_refresh();
        }
        let refreshed = (0..30).filter(|_| t.would_refresh()).count();
        assert_eq!(refreshed, 0, "idle previews must stop costing anything");
    }

    #[test]
    fn drawing_a_preview_again_wakes_them_back_up() {
        let mut t = Thumbnails::default();
        for _ in 0..200 {
            t.would_refresh();
        }
        assert_eq!((0..30).filter(|_| t.would_refresh()).count(), 0, "idle");
        // A panel comes back: the next marked frame resumes refreshing, so a
        // preview is never left showing a stale frame once it is visible again.
        t.mark_wanted();
        assert_eq!(
            (0..30).filter(|_| t.would_refresh()).count(),
            10,
            "previews must resume once something draws them"
        );
    }
}
