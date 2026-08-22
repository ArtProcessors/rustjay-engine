//! Uninhabited stand-in for [`crate::renderer::ImGuiRenderer`].
//!
//! `imgui-wgpu` has no wgpu 30 release (0.28.0, the latest, requires wgpu 29),
//! so the Dear ImGui renderer cannot be built against the wgpu the engine now
//! uses. Only the *renderer backend* is affected — the `imgui` crate itself has
//! no wgpu dependency, so every `imgui::Ui` tab in the engine and the examples
//! still compiles normally.
//!
//! Rather than thread `#[cfg]` through the engine's egui/imgui branches — which
//! are `else if` arms and awkward to attribute — the type stays in place but
//! becomes impossible to construct. Every `Option<ImGuiRenderer>` is therefore
//! `None`, the imgui arms are dead code, and each method body is `match *self
//! {}`, which the compiler accepts precisely because the type is uninhabited.
//!
//! Delete this file and drop the `#[cfg]`s in `lib.rs` once `imgui-wgpu`
//! supports wgpu 30.

use anyhow::Result;
use std::sync::Arc;
use winit::window::Window;

pub enum ImGuiRenderer {}

impl ImGuiRenderer {
    pub async fn new(
        _instance: &wgpu::Instance,
        _adapter: &wgpu::Adapter,
        _device: Arc<wgpu::Device>,
        _queue: Arc<wgpu::Queue>,
        _window: Arc<Window>,
        _scale_factor: f64,
    ) -> Result<Self> {
        anyhow::bail!(
            "the Dear ImGui renderer is not compiled in: imgui-wgpu has no wgpu 30 \
             release. Build with --features imgui-renderer once it does, or use egui."
        )
    }

    pub fn handle_event(&mut self, _event: &winit::event::Event<()>) {
        match *self {}
    }

    pub fn set_display_size(&mut self, _width: f32, _height: f32) {
        match *self {}
    }

    pub fn set_scale_factor(&mut self, _scale_factor: f64) {
        match *self {}
    }

    pub fn scale_factor(&self) -> f64 {
        match *self {}
    }

    pub fn resize(&mut self, _width: u32, _height: u32) {
        match *self {}
    }

    pub fn create_preview_texture(&mut self, _width: u32, _height: u32) -> imgui::TextureId {
        match *self {}
    }

    pub fn get_preview_view(&self, _texture_id: imgui::TextureId) -> Option<&wgpu::TextureView> {
        match *self {}
    }

    pub fn update_preview_texture(
        &mut self,
        _texture_id: imgui::TextureId,
        _source_texture: &wgpu::Texture,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        match *self {}
    }

    pub fn render_frame<F>(&mut self, _build_ui: F) -> Result<()>
    where
        F: FnOnce(&mut imgui::Ui),
    {
        match *self {}
    }

    pub fn device(&self) -> &wgpu::Device {
        match *self {}
    }

    pub fn queue(&self) -> &wgpu::Queue {
        match *self {}
    }
}
