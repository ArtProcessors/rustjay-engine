//! NDI source — receives frames from an NDI stream and uploads to GPU texture.
//!
//! Wraps `rustjay_io::NdiReceiver` as an `EffectInstance` for use in a deck.

use rustjay_core::{EffectInput, EffectInstance, EngineState, RenderCtx, RenderTarget};
use rustjay_mixer::BlitPipeline;

/// Renders live NDI frames to the target.
pub struct NdiSource {
    receiver: rustjay_io::NdiReceiver,
    source_name: String,
    started: bool,
    pipeline: BlitPipeline,
    texture: Option<wgpu::Texture>,
    view: Option<wgpu::TextureView>,
    width: u32,
    height: u32,
    /// Layout of the frames arriving now. A sender can change it mid-stream
    /// (alpha appearing flips 4:2:2 to BGRA), which resizes the texture.
    layout: rustjay_io::NdiPixelLayout,
    /// Staging buffers whose copy was encoded last frame, so it has been
    /// submitted by now and they can be mapped again.
    pending_remap: Vec<wgpu::Buffer>,
}

impl NdiSource {
    pub fn new(device: &wgpu::Device, source_name: impl Into<String>) -> Self {
        let source_name = source_name.into();
        let receiver = rustjay_io::NdiReceiver::new(&source_name);
        let pipeline = BlitPipeline::new(device, rustjay_core::working_format());
        Self {
            receiver,
            source_name,
            started: false,
            pipeline,
            texture: None,
            view: None,
            width: 1920,
            height: 1080,
            layout: rustjay_io::NdiPixelLayout::Bgra,
            pending_remap: Vec::new(),
        }
    }

    /// Texels across, for the frame layout in use. Packed 4:2:2 rides in a
    /// half-width RGBA texture: one texel carries two pixels.
    fn texel_width(&self) -> u32 {
        match self.layout {
            rustjay_io::NdiPixelLayout::Uyvy => self.width.div_ceil(2),
            rustjay_io::NdiPixelLayout::Bgra => self.width,
        }
    }

    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        layout: rustjay_io::NdiPixelLayout,
    ) {
        if self.texture.is_none()
            || self.width != width
            || self.height != height
            || self.layout != layout
        {
            self.width = width;
            self.height = height;
            self.layout = layout;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("NdiSource Texture"),
                size: wgpu::Extent3d {
                    width: self.texel_width(),
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // BGRA is colour and is stored as such. Packed 4:2:2 is not
                // colour at all — it is four raw bytes per texel (U Y0 V Y1)
                // that the shader decodes, so it needs a format that hands
                // them back in memory order. Through Bgra8Unorm the sampler
                // would swap bytes 0 and 2, i.e. Cb with Cr, i.e. red with
                // blue.
                format: match self.layout {
                    rustjay_io::NdiPixelLayout::Uyvy => wgpu::TextureFormat::Rgba8Unorm,
                    rustjay_io::NdiPixelLayout::Bgra => wgpu::TextureFormat::Bgra8Unorm,
                },
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.view = Some(texture.create_view(&wgpu::TextureViewDescriptor::default()));
            self.texture = Some(texture);
        }
    }
}

impl EffectInstance for NdiSource {
    fn prepare(&mut self, _engine: &EngineState, device: &wgpu::Device, _queue: &wgpu::Queue) {
        if !self.started {
            if let Err(e) = self.receiver.start() {
                log::warn!("[NdiSource] Failed to start receiver: {}", e);
            } else {
                self.started = true;
                log::info!("[NdiSource] Started receiver for '{}'", self.source_name);
            }
        }

        // Last frame's copies have been submitted, so those buffers can be
        // mapped again. Their callbacks — and the re-map itself — only run
        // while the device is polled, which the engine does not do.
        for buffer in self.pending_remap.drain(..) {
            self.receiver.remap_staged(buffer);
        }
        device.poll(wgpu::PollType::Poll).ok();

        // Offer buffers for the size now arriving: the receive thread writes
        // frames straight into them, so nothing copies them on this thread.
        let (width, height) = self.receiver.resolution();
        self.receiver
            .provide_staging(device, width, height, self.layout);
    }

    fn render_to(
        &mut self,
        ctx: &mut RenderCtx<'_>,
        _inputs: &[EffectInput<'_>],
        target: RenderTarget<'_>,
        _engine: &EngineState,
    ) {
        if !self.started {
            return;
        }

        if let Some(frame) = self.receiver.get_latest_frame() {
            self.ensure_texture(ctx.device, frame.width, frame.height, frame.layout);
            let texel_width = self.texel_width();
            let size = wgpu::Extent3d {
                width: texel_width,
                height: frame.height,
                depth_or_array_layers: 1,
            };
            match (self.texture.as_ref(), frame.staged.as_ref()) {
                // Already in GPU-visible memory: the GPU does the rest.
                (Some(texture), Some(staged)) => ctx.encoder.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &staged.buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(staged.bytes_per_row),
                            rows_per_image: Some(frame.height),
                        },
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    size,
                ),
                (Some(texture), None) => ctx.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &frame.data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(texel_width * 4),
                        rows_per_image: Some(frame.height),
                    },
                    size,
                ),
                (None, _) => {}
            }
            match frame.staged {
                // Re-mapped next frame, once this copy has been submitted.
                Some(staged) => self.pending_remap.push(staged.buffer),
                // write_texture made its own copy; the Vec goes back for reuse.
                None => self.receiver.recycle(frame.data),
            }
        }

        if let Some(ref view) = self.view {
            let blit = match self.layout {
                rustjay_io::NdiPixelLayout::Uyvy => BlitPipeline::blit_uyvy,
                rustjay_io::NdiPixelLayout::Bgra => BlitPipeline::blit,
            };
            blit(
                &self.pipeline,
                ctx.device,
                ctx.encoder,
                view,
                target.view,
                ctx.vertex_buffer,
            );
        }
    }
}
