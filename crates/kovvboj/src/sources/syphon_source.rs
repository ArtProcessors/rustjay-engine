//! Syphon source — receives frames from a Syphon server (macOS).
//!
//! Wraps `rustjay_io::SyphonInputReceiver` as an `EffectInstance` for use in a deck.

use rustjay_core::{EffectInput, EffectInstance, EngineState, RenderCtx, RenderTarget};
use rustjay_mixer::BlitPipeline;

/// Renders live Syphon frames to the target.
pub struct SyphonSource {
    receiver: rustjay_io::SyphonInputReceiver,
    server_name: String,
    server_uuid: String,
    initialized: bool,
    /// When to try connecting again. Retrying every frame costs ~4% of a core
    /// and floods the log at 60 lines a second while the publisher is down.
    next_attempt: std::time::Instant,
    pipeline: BlitPipeline,
}

/// How long to wait between connection attempts while the server is absent.
const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

impl SyphonSource {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        server_name: impl Into<String>,
        server_uuid: impl Into<String>,
    ) -> Self {
        let server_name = server_name.into();
        let server_uuid = server_uuid.into();
        let mut receiver = rustjay_io::SyphonInputReceiver::new();
        receiver.initialize(device, queue);
        let pipeline = BlitPipeline::new(device, rustjay_core::working_format());
        Self {
            receiver,
            server_name,
            server_uuid,
            initialized: false,
            next_attempt: std::time::Instant::now(),
            pipeline,
        }
    }
}

impl EffectInstance for SyphonSource {
    fn prepare(&mut self, _engine: &EngineState, device: &wgpu::Device, queue: &wgpu::Queue) {
        if !self.initialized && std::time::Instant::now() >= self.next_attempt {
            match self
                .receiver
                .connect_by_uuid(&self.server_uuid, &self.server_name)
            { Err(e) => {
                self.next_attempt = std::time::Instant::now() + RETRY_INTERVAL;
                log::warn!("[SyphonSource] Failed to connect: {}", e);
            } _ => {
                self.initialized = true;
                log::info!(
                    "[SyphonSource] Connected to '{}'",
                    self.server_name
                );
            }}
        }
        self.receiver.try_receive_texture(device, queue);
    }

    fn render_to(
        &mut self,
        ctx: &mut RenderCtx<'_>,
        _inputs: &[EffectInput<'_>],
        target: RenderTarget<'_>,
        _engine: &EngineState,
    ) {
        if !self.initialized {
            return;
        }

        if let Some(tex) = self.receiver.output_texture() {
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            // Opaque: a Syphon publisher that writes only RGB leaves alpha at
            // the shared surface's initial zero, which composites to nothing.
            self.pipeline.blit_opaque(
                ctx.device,
                ctx.encoder,
                &view,
                target.view,
                ctx.vertex_buffer,
            );
        }
    }
}
