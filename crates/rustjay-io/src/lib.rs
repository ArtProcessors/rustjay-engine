// `objc::msg_send!` expands to `sel!`, so the macros have to be imported even
// though every call is path-qualified. Only the webcam backend uses them.
#[cfg(all(target_os = "macos", feature = "webcam"))]
#[macro_use]
extern crate objc;

pub(crate) mod input;
pub(crate) mod output;
pub(crate) mod texture_utils;

#[cfg(target_os = "linux")]
pub(crate) mod v4l2_devices;

#[cfg(feature = "hap-encode")]
pub mod hap_encode;

#[cfg(feature = "ffmpeg")]
pub use input::ffmpeg::{detect_hap_codec, FfmpegDecoder, LoopMode, StreamDecoder, VideoFrame};
#[cfg(feature = "webcam")]
pub use input::webcam::{WebcamCapture, WebcamFrame, list_cameras};
// Without the feature the same names still exist, so a caller compiles either
// way — `list_cameras` simply finds nothing.
#[cfg(not(feature = "webcam"))]
pub use input::{WebcamFrame, list_cameras};
pub use input::InputManager;
pub use input::SpoutSenderInfo;
pub use input::SyphonServerInfo;
#[cfg(feature = "ndi")]
pub use input::{NdiPixelLayout, NdiReceiver, list_ndi_sources, low_bandwidth, set_low_bandwidth};
#[cfg(target_os = "macos")]
pub use input::{SyphonInputReceiver, SyphonDiscovery};
#[cfg(target_os = "windows")]
pub use input::{SpoutDiscovery, SpoutInputReceiver};
pub use output::recorder::{list_audio_devices, Recorder, RecorderCodec};
pub use output::OutputManager;

/// An FFmpeg command-line tool (`"ffmpeg"`, `"ffprobe"`) to run: the copy
/// shipped next to the running executable when there is one, so a packaged app
/// records and converts with nothing installed; otherwise the bare name, found
/// on `PATH`.
pub fn ffmpeg_tool(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().ok();
    tool_beside(exe.as_deref().and_then(std::path::Path::parent), name)
}

fn tool_beside(dir: Option<&std::path::Path>, name: &str) -> std::path::PathBuf {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    dir.map(|d| d.join(&file))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| file.into())
}

#[cfg(test)]
mod ffmpeg_tool_tests {
    #[test]
    fn a_bundled_copy_wins_over_path() {
        let dir = std::env::temp_dir().join(format!("rustjay-ffmpeg-tool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = format!("ffmpeg{}", std::env::consts::EXE_SUFFIX);
        // Nothing beside the executable: fall back to the bare name on PATH.
        assert_eq!(super::tool_beside(Some(&dir), "ffmpeg"), std::path::PathBuf::from(&file));
        std::fs::write(dir.join(&file), b"").unwrap();
        assert_eq!(super::tool_beside(Some(&dir), "ffmpeg"), dir.join(&file));
        std::fs::remove_dir_all(&dir).ok();
    }
}
#[cfg(target_os = "linux")]
pub use v4l2_devices::{V4l2DeviceInfo, list_output_devices};
