//! Text source — a string laid out from a font atlas and drawn as one quad per
//! glyph.
//!
//! The quads are rebuilt only when the words or the layout change; size,
//! position, colour and every animation knob are uniforms, so they cost
//! nothing per frame and modulate smoothly. Per-glyph animation is keyed off
//! each glyph's ordinal in the vertex shader — that is what the atlas buys
//! over drawing the whole string into one texture.
//!
//! The atlas is either a rasterised TTF/OTF or an image someone drew; see
//! [`crate::sources::text_atlas`].

use rustjay_core::{
    EffectInput, EffectInstance, EngineState, ParamCategory, ParameterDescriptor, RenderCtx,
    RenderTarget,
};
use std::path::{Path, PathBuf};
use wgpu::util::DeviceExt;

use super::text_atlas::{Atlas, Layout, layout};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    color: [f32; 4],
    // Not `target`: a reserved word in WGSL, and the shader will not parse
    // with a field of that name.
    resolution: [f32; 2],
    center: [f32; 2],
    block: [f32; 2],
    scale: f32,
    angle: f32,
    time: f32,
    stagger: f32,
    wave: f32,
    spin: f32,
    explode: f32,
    colour_atlas: f32,
    turn: f32,
    tilt: f32,
}

/// Per-glyph instance data, matching the vertex layout in `text.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    index: f32,
    _pad: [f32; 3],
}

/// Draws a string from a font atlas, one quad per glyph.
pub struct TextSource {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: Option<wgpu::BindGroup>,
    uniform_buffer: wgpu::Buffer,
    /// The unit quad every glyph instances.
    corners: wgpu::Buffer,
    instances: Option<wgpu::Buffer>,
    glyph_count: u32,
    sampler: wgpu::Sampler,

    atlas: Option<Atlas>,
    font_path: Option<PathBuf>,
    /// The layout the current quads were built from; a difference rebuilds.
    layout: Layout,
    /// What the next rebuild should use — set by the UI, OSC or a parameter.
    pending: Layout,
    block: [f32; 2],
    dirty: bool,
    /// Set when the atlas itself changed and has to be uploaded again — a
    /// tracking tweak re-lays the quads but must not re-push the texture.
    atlas_dirty: bool,

    /// Animation clock, in cycles. Integrated so tempo changes do not jump it.
    time: f32,
    last_tick: Option<std::time::Instant>,

    param_prefix: String,
}

impl TextSource {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        font: Option<&Path>,
        text: Option<&str>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Text Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Text Uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Two triangles in 0..1; every glyph is this quad, moved and sized by
        // its instance.
        let corners = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Text Quad"),
            contents: bytemuck::cast_slice(&[
                0.0_f32, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0,
            ]),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Text BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Text Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });

        // Max, not alpha blending: the mixer expects straight alpha, and
        // src-over would leave the target premultiplied. Taking the greater
        // coverage is also what overlapping glyphs want — adding them rings at
        // the joins.
        let overlap = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Text Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: 8,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            1 => Float32x4, 2 => Float32x4, 3 => Float32
                        ],
                    }),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState { color: overlap, alpha: overlap }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Text Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let font_path = font.map(Path::to_path_buf).or_else(default_font);
        Self {
            pipeline,
            bind_group_layout,
            bind_group: None,
            uniform_buffer,
            corners,
            instances: None,
            glyph_count: 0,
            sampler,
            atlas: None,
            font_path,
            layout: Layout::default(),
            pending: Layout {
                text: text.unwrap_or("TEXT").to_string(),
                ..Layout::default()
            },
            block: [1.0, 1.0],
            dirty: true,
            atlas_dirty: true,
            time: 0.0,
            last_tick: None,
            param_prefix: String::new(),
        }
    }

    /// The string being rendered.
    pub fn text(&self) -> &str {
        &self.pending.text
    }

    /// Replace the string. Rebuilt on the next `prepare`.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.pending.text = text.into();
    }

    /// One line about the atlas in use. Whether a drawn atlas found its
    /// sidecar is the first thing to check when the letters come out wrong,
    /// and there is nowhere else to see it.
    pub fn atlas_summary(&self) -> String {
        match &self.atlas {
            None => "no atlas — pick a font".to_string(),
            Some(a) if a.colour => format!(
                "drawn atlas · {} characters · {}",
                a.cells.len(),
                if a.sidecar {
                    "mapped by its .txt"
                } else {
                    "no .txt — assuming ASCII in 16 columns"
                }
            ),
            Some(a) => format!("{} glyphs rasterised", a.cells.len()),
        }
    }

    /// The font file or atlas image in use, if one was found.
    pub fn font_path(&self) -> Option<&Path> {
        self.font_path.as_deref()
    }

    /// Point at a different font file or atlas image. A file that will not
    /// load is ignored, so a bad pick cannot blank a layer mid-set.
    pub fn set_font(&mut self, path: &Path) {
        self.font_path = Some(path.to_path_buf());
        self.atlas = None;
        self.dirty = true;
    }

    /// Build the atlas for the current font, if it is not built already or no
    /// longer covers the text.
    fn ensure_atlas(&mut self) -> bool {
        if let Some(atlas) = &self.atlas
            && atlas.covers(&self.pending.text)
        {
            return true;
        }
        let Some(path) = self.font_path.clone() else {
            return false;
        };
        self.atlas_dirty = true;
        self.atlas = if is_font(&path) {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| ab_glyph::FontVec::try_from_vec(bytes).ok())
                .map(|font| Atlas::from_font(&font, &self.pending.text))
        } else {
            Atlas::from_image(&path)
        };
        if self.atlas.is_none() {
            log::warn!("[Text] could not load font {}", path.display());
        }
        self.atlas.is_some()
    }

    /// Upload the atlas and lay the string out into per-glyph instances.
    fn rebuild(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        // Mark the attempt done before making it: a font that will not load
        // must not be retried, and re-logged, on every frame.
        self.dirty = false;
        self.layout = self.pending.clone();
        if !self.ensure_atlas() {
            return;
        }
        if self.atlas_dirty || self.bind_group.is_none() {
            self.upload_atlas(device, queue);
        }
        let atlas = self.atlas.as_ref().expect("ensured above");
        let (quads, block) = layout(atlas, &self.pending);
        let instances: Vec<Instance> = quads
            .iter()
            .map(|q| Instance { rect: q.rect, uv: q.uv, index: q.index, _pad: [0.0; 3] })
            .collect();
        self.glyph_count = instances.len() as u32;
        self.instances = (!instances.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Text Glyphs"),
                contents: bytemuck::cast_slice(&instances),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.block = block;
    }

    /// Push the atlas to the GPU and rebind. Only when it actually changed.
    fn upload_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let atlas = self.atlas.as_ref().expect("caller ensures an atlas");
        let format = if atlas.colour {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::R8Unorm
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Text Atlas"),
            size: wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes_per_pixel = if atlas.colour { 4 } else { 1 };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_pixel * atlas.width),
                rows_per_image: Some(atlas.height),
            },
            wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Text BG"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniform_buffer,
                        offset: 0,
                        size: None,
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        }));

        self.atlas_dirty = false;
    }

    fn param(&self, engine: &EngineState, name: &str, default: f32) -> f32 {
        engine
            .get_param(&format!("{}{name}", self.param_prefix))
            .unwrap_or(default)
    }
}

impl EffectInstance for TextSource {
    fn label(&self) -> &str {
        "text"
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn set_param_prefix(&mut self, prefix: &str) {
        self.param_prefix = prefix.to_string();
    }

    fn parameters(&self) -> Vec<ParameterDescriptor> {
        // Bare names — the mixer applies the channel prefix.
        let cat = ParamCategory::Custom("Text".to_string());
        let mut params = vec![
            ParameterDescriptor::float("text_size", "Size", cat.clone(), 0.0, 2.0, 0.33, 0.005),
            ParameterDescriptor::float("text_x", "X", cat.clone(), -1.0, 2.0, 0.5, 0.005),
            ParameterDescriptor::float("text_y", "Y", cat.clone(), -1.0, 2.0, 0.5, 0.005),
            ParameterDescriptor::float("text_rot", "Rotation", cat.clone(), -180.0, 180.0, 0.0, 1.0),
            // The whole string in 3D, as opposed to `text_spin`, which turns
            // each glyph on its own.
            ParameterDescriptor::float("text_turn", "Turn", cat.clone(), -180.0, 180.0, 0.0, 1.0),
            ParameterDescriptor::float("text_tilt", "Tilt", cat.clone(), -180.0, 180.0, 0.0, 1.0),
            ParameterDescriptor::float(
                "text_turn_rate",
                "Turn Rate",
                cat.clone(),
                -4.0,
                4.0,
                0.0,
                0.01,
            ),
            ParameterDescriptor::float("text_r", "Red", cat.clone(), 0.0, 1.0, 1.0, 0.01),
            ParameterDescriptor::float("text_g", "Green", cat.clone(), 0.0, 1.0, 1.0, 0.01),
            ParameterDescriptor::float("text_b", "Blue", cat.clone(), 0.0, 1.0, 1.0, 0.01),
            ParameterDescriptor::float("text_a", "Alpha", cat.clone(), 0.0, 1.0, 1.0, 0.01),
            // Per-glyph animation. All of it is uniform work, so an LFO on any
            // of these is free.
            ParameterDescriptor::float("text_wave", "Wave", cat.clone(), -1.0, 1.0, 0.0, 0.01),
            ParameterDescriptor::float("text_spin", "Spin", cat.clone(), -2.0, 2.0, 0.0, 0.01),
            ParameterDescriptor::float("text_explode", "Explode", cat.clone(), 0.0, 4.0, 0.0, 0.01),
            ParameterDescriptor::float("text_stagger", "Stagger", cat.clone(), -0.5, 0.5, 0.05, 0.005),
            // Layout: a change here rebuilds the quads, so these are not the
            // ones to hang an LFO on.
            ParameterDescriptor::float("text_track", "Tracking", cat.clone(), -0.3, 1.0, 0.0, 0.01),
            ParameterDescriptor::float("text_line", "Line Height", cat.clone(), 0.5, 3.0, 1.2, 0.01),
            ParameterDescriptor::enum_param(
                "text_align",
                "Align",
                cat.clone(),
                vec!["Left".into(), "Centre".into(), "Right".into()],
                1,
            ),
            ParameterDescriptor::float("speed", "Speed", cat, 0.0, 4.0, 1.0, 0.01),
        ];
        // The same tempo lock clips use: one animation cycle per division.
        params.extend(super::sync_parameters().into_iter().filter(|p| p.id != "mode"));
        params
    }

    fn prepare(&mut self, engine: &EngineState, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.pending.tracking = self.param(engine, "text_track", 0.0);
        self.pending.line_height = self.param(engine, "text_line", 1.2).max(0.1);
        self.pending.align = self.param(engine, "text_align", 1.0).round().clamp(0.0, 2.0) as usize;
        if self.dirty || self.pending != self.layout {
            self.rebuild(device, queue);
        }

        // One cycle per beat division when synced, one per second when not.
        let rate = super::clip_speed(
            self.param(engine, "speed", 1.0),
            self.param(engine, "sync", 0.0) >= 0.5,
            self.param(engine, "div", 4.0) as usize,
            engine.effective_bpm(),
            1.0,
        );
        let now = std::time::Instant::now();
        let dt = self.last_tick.map(|t| (now - t).as_secs_f32()).unwrap_or(0.0);
        self.last_tick = Some(now);
        self.time += dt * rate;
    }

    fn render_to(
        &mut self,
        ctx: &mut RenderCtx<'_>,
        _inputs: &[EffectInput<'_>],
        target: RenderTarget<'_>,
        engine: &EngineState,
    ) {
        let uniforms = Uniforms {
            color: [
                self.param(engine, "text_r", 1.0),
                self.param(engine, "text_g", 1.0),
                self.param(engine, "text_b", 1.0),
                self.param(engine, "text_a", 1.0),
            ],
            resolution: [target.size[0].max(1) as f32, target.size[1].max(1) as f32],
            center: [
                self.param(engine, "text_x", 0.5),
                self.param(engine, "text_y", 0.5),
            ],
            block: self.block,
            scale: self.param(engine, "text_size", 0.33).max(0.0),
            angle: self.param(engine, "text_rot", 0.0).to_radians(),
            time: self.time,
            stagger: self.param(engine, "text_stagger", 0.05),
            wave: self.param(engine, "text_wave", 0.0),
            spin: self.param(engine, "text_spin", 0.0),
            explode: self.param(engine, "text_explode", 0.0),
            colour_atlas: f32::from(u8::from(
                self.atlas.as_ref().is_some_and(|a| a.colour),
            )),
            // Turn Rate spins the string on the animation clock — whole
            // revolutions per cycle, so synced it lands on the beat. The angle
            // parameter is the offset it spins from.
            turn: self.param(engine, "text_turn", 0.0).to_radians()
                + self.time * self.param(engine, "text_turn_rate", 0.0) * std::f32::consts::TAU,
            tilt: self.param(engine, "text_tilt", 0.0).to_radians(),
        };
        ctx.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Text Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view,
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
        let (Some(bind_group), Some(instances)) = (&self.bind_group, &self.instances) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, self.corners.slice(..));
        pass.set_vertex_buffer(1, instances.slice(..));
        pass.draw(0..6, 0..self.glyph_count);
    }
}

/// Somewhere to start when no font has been picked. The OS font directories
/// are all we look at — a font shipped with the app would be a licence
/// question.
pub fn default_font() -> Option<PathBuf> {
    for dir in font_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut candidates: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_font(p))
            .collect();
        candidates.sort();
        if let Some(first) = candidates.into_iter().find(|p| {
            std::fs::read(p)
                .ok()
                .is_some_and(|b| ab_glyph::FontVec::try_from_vec(b).is_ok())
        }) {
            return Some(first);
        }
    }
    None
}

/// Whether a path is a font file to rasterise, as opposed to an atlas image.
pub fn is_font(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_lowercase().as_str(), "ttf" | "otf" | "ttc"))
}

/// The platform's font directories.
pub fn font_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir();
    #[cfg(target_os = "macos")]
    let dirs = [
        Some(PathBuf::from("/System/Library/Fonts")),
        Some(PathBuf::from("/Library/Fonts")),
        home.map(|h| h.join("Library/Fonts")),
    ];
    #[cfg(target_os = "windows")]
    let dirs = [
        Some(PathBuf::from("C:\\Windows\\Fonts")),
        home.map(|h| h.join("AppData/Local/Microsoft/Windows/Fonts")),
        None,
    ];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let dirs = [
        Some(PathBuf::from("/usr/share/fonts")),
        Some(PathBuf::from("/usr/local/share/fonts")),
        home.map(|h| h.join(".local/share/fonts")),
    ];
    dirs.into_iter().flatten().filter(|d| d.is_dir()).collect()
}

#[cfg(test)]
mod tests {
    /// A shader that will not parse is a panic inside `create_shader_module`
    /// the first time someone adds a text layer — which is exactly how a field
    /// named `target`, a WGSL reserved word, got in.
    #[test]
    fn the_glyph_shader_parses() {
        wgpu::naga::front::wgsl::parse_str(include_str!("text.wgsl"))
            .expect("text.wgsl must be valid WGSL");
    }
}
