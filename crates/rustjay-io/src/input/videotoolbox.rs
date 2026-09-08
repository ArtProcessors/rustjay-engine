//! Zero-copy import of VideoToolbox frames (macOS).
//!
//! A hardware-decoded frame is already in GPU-visible memory: VideoToolbox
//! hands back a `CVPixelBuffer` backed by an `IOSurface`, which Metal can wrap
//! as a texture without any copy. The alternative — `av_hwframe_transfer_data`
//! to system memory, swscale to RGBA, `write_texture` back to the GPU — moves
//! a 4K frame across the bus three times to end up where it started.
//!
//! What arrives is biplanar NV12: a full-resolution luma plane and a
//! half-resolution interleaved chroma plane. Both are imported as their own
//! wgpu texture and combined in a shader.

use std::ffi::c_void;

// Four CoreVideo calls, declared here rather than taking a binding crate for
// them. CoreVideo is already linked — ffmpeg's VideoToolbox support needs it.
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferRetain(buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferRelease(buffer: *mut c_void);
    fn CVPixelBufferGetIOSurface(buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetPlaneCount(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetWidthOfPlane(buffer: *mut c_void, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(buffer: *mut c_void, plane: usize) -> usize;
}

/// A decoded frame still living in GPU memory.
///
/// Holds a retained `CVPixelBuffer` so the decoder's frame pool cannot recycle
/// the surface out from under a texture that is still being sampled.
pub struct HardwareFrame {
    /// Retained `CVPixelBufferRef`.
    buffer: *mut c_void,
    pub width: u32,
    pub height: u32,
}

// SAFETY: CVPixelBuffer is an immutable, internally reference-counted CoreVideo
// object. It is decoded on the worker thread and imported on the render thread,
// which is the whole point of carrying it rather than its pixels.
unsafe impl Send for HardwareFrame {}

impl HardwareFrame {
    /// Retain the pixel buffer behind a hardware `AVFrame`.
    ///
    /// Returns `None` for a software frame — `data[3]` is where the VideoToolbox
    /// hwaccel stashes its `CVPixelBufferRef`, and it is null for anything else.
    pub fn from_av_frame(frame: &ffmpeg_next::util::frame::Video) -> Option<Self> {
        // SAFETY: `as_ptr` yields a live AVFrame for the borrow; data[3] is
        // either null or a CVPixelBufferRef owned by the frame, which we retain.
        unsafe {
            let raw = frame.as_ptr();
            let buffer = (*raw).data[3] as *mut c_void;
            if buffer.is_null() || CVPixelBufferGetPlaneCount(buffer) < 2 {
                return None;
            }
            Some(Self {
                buffer: CVPixelBufferRetain(buffer),
                width: frame.width(),
                height: frame.height(),
            })
        }
    }

    /// Wrap the luma and chroma planes as wgpu textures, sharing the surface's
    /// memory rather than copying it.
    ///
    /// Must be called on the thread that owns `device`. Returns `None` if this
    /// is not a Metal device or the surface cannot be wrapped, so the caller can
    /// fall back to the pixel path.
    pub fn import_planes(&self, device: &wgpu::Device) -> Option<(wgpu::Texture, wgpu::Texture)> {
        use objc2_metal::{
            MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTextureDescriptor, MTLTextureUsage,
        };

        // SAFETY: the buffer is retained for our lifetime, so its surface is too.
        let surface = unsafe { CVPixelBufferGetIOSurface(self.buffer) };
        if surface.is_null() {
            return None;
        }
        // SAFETY: a non-null IOSurfaceRef from a live CVPixelBuffer.
        let surface: &objc2_io_surface::IOSurfaceRef = unsafe { &*surface.cast() };

        // SAFETY: as_hal only requires the device to outlive the borrow.
        let hal_device = unsafe { device.as_hal::<wgpu_hal::api::Metal>() }?;
        let metal_device = hal_device.raw_device();

        let plane = |index: usize, format: MTLPixelFormat, wgpu_format: wgpu::TextureFormat| {
            // SAFETY: plane indices are within the count checked at construction.
            let (w, h) = unsafe {
                (
                    CVPixelBufferGetWidthOfPlane(self.buffer, index),
                    CVPixelBufferGetHeightOfPlane(self.buffer, index),
                )
            };

            let descriptor = MTLTextureDescriptor::new();
            descriptor.setPixelFormat(format);
            // SAFETY: plain setters on a descriptor we just made and solely own.
            unsafe {
                descriptor.setWidth(w);
                descriptor.setHeight(h);
            }
            descriptor.setUsage(MTLTextureUsage::ShaderRead);
            // The surface is shared with the decoder, so it cannot be private.
            descriptor.setStorageMode(MTLStorageMode::Shared);

            // SAFETY: descriptor and surface are both live; `index` is a valid
            // plane. Metal retains the surface for the texture's lifetime.
            let raw =
                metal_device.newTextureWithDescriptor_iosurface_plane(&descriptor, surface, index)?;

            let size = wgpu::Extent3d {
                width: w as u32,
                height: h as u32,
                depth_or_array_layers: 1,
            };
            // SAFETY: `raw` is a fresh MTLTexture matching the descriptor above,
            // and `wgpu_format` is the wgpu spelling of `format`. No drop
            // callback: dropping the wgpu texture releases the MTLTexture, which
            // releases its reference to the surface.
            let hal_texture = unsafe {
                wgpu_hal::metal::Device::texture_from_raw(
                    raw,
                    wgpu_format,
                    objc2_metal::MTLTextureType::Type2D,
                    1,
                    1,
                    wgpu_hal::CopyExtent {
                        width: size.width,
                        height: size.height,
                        depth: 1,
                    },
                    None,
                )
            };
            // SAFETY: the hal texture was just built for this device.
            let texture = unsafe {
                device.create_texture_from_hal::<wgpu_hal::api::Metal>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("VideoToolbox plane"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu_format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    // The surface already holds decoded pixels; nothing is
                    // going to write to it through wgpu.
                    wgpu::wgt::TextureUses::RESOURCE,
                )
            };
            Some(texture)
        };

        let luma = plane(0, MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm)?;
        let chroma = plane(1, MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm)?;
        Some((luma, chroma))
    }
}

impl Clone for HardwareFrame {
    fn clone(&self) -> Self {
        // SAFETY: retaining a buffer we already hold a reference to.
        Self {
            buffer: unsafe { CVPixelBufferRetain(self.buffer) },
            width: self.width,
            height: self.height,
        }
    }
}

impl std::fmt::Debug for HardwareFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl Drop for HardwareFrame {
    fn drop(&mut self) {
        // SAFETY: retained in `from_av_frame`, released exactly once here.
        unsafe { CVPixelBufferRelease(self.buffer) };
    }
}
