//! Persistence — `.kovvboj/` workspace layout.
//!
//! - `scene.json`  — channels, decks, effects, modulation, crossfader, sequences
//! - `stage.json`  — surface layout, outputs, warp calibration
//! - `midi.json`   — MIDI controller mappings
//! - `keymap.json` — keyboard shortcut bindings
//! - `presets/`    — saved deck/channel presets
//!
//! See VARDA_PORT.md Phase 11.

use crate::scene::Scene;
use std::path::{Path, PathBuf};

/// Export/import a whole set as one file.
#[cfg(feature = "mixer")]
pub mod bundle;

/// Workspace loader/saver.
#[derive(Clone)]
pub struct Workspace {
    pub dir: PathBuf,
}

impl Default for Workspace {
    fn default() -> Self {
        default_workspace()
    }
}

impl Workspace {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
        }
    }

    pub fn scene_path(&self) -> PathBuf {
        self.dir.join("scene.json")
    }

    pub fn stage_path(&self) -> PathBuf {
        self.dir.join("stage.json")
    }

    pub fn keymap_path(&self) -> PathBuf {
        self.dir.join("keymap.json")
    }

    pub fn ui_path(&self) -> PathBuf {
        self.dir.join("ui.json")
    }

    /// Load UI preferences, falling back to defaults when absent or unreadable —
    /// a corrupt prefs file must not stop the app opening.
    pub fn load_ui(&self) -> UiPrefs {
        std::fs::read_to_string(self.ui_path())
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    pub fn save_ui(&self, prefs: &UiPrefs) -> anyhow::Result<()> {
        self.ensure_dir()?;
        std::fs::write(self.ui_path(), serde_json::to_string_pretty(prefs)?)?;
        Ok(())
    }

    pub fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    /// Where state that belongs to the rig rather than to one show lives.
    ///
    /// A workspace is a set: its scene, its stage, its keymap. Saved layers and
    /// favourites are not that — they are building blocks you assemble sets
    /// from, and having them vanish when you open a new set makes them useless.
    /// Same call [`recent_path`] makes for the recent-sets list.
    ///
    /// Falls back to the workspace when there is no home directory, which beats
    /// losing them outright.
    fn global_root(&self) -> PathBuf {
        dirs::data_dir()
            .map(|d| d.join("rustjay"))
            .unwrap_or_else(|| self.dir.clone())
    }

    /// Where saved layers live, one JSON file each so they can be copied
    /// between rigs by hand. Global: a layer you built is a building block, not
    /// a property of the set you happened to build it in.
    pub fn layers_dir(&self) -> PathBuf {
        let global = self.global_root().join("layers");
        // Carry a workspace's layers over the first time this build runs.
        let legacy = self.dir.join("layers");
        if legacy != global && legacy.is_dir() {
            migrate_dir(&legacy, &global);
        }
        global
    }

    /// Starred library entries. Global, for the same reason as [`Self::layers_dir`]:
    /// a star says "I like this shader", not "I like it during this show".
    pub fn favourites_path(&self) -> PathBuf {
        let global = self.global_root().join("favourites.json");
        let legacy = self.dir.join("favourites.json");
        if legacy != global && legacy.is_file() && !global.exists() {
            migrate_file(&legacy, &global);
        }
        global
    }

    /// Ids of library entries the user starred. A missing or unreadable file
    /// just means none.
    pub fn load_favourites(&self) -> std::collections::HashSet<String> {
        std::fs::read_to_string(self.favourites_path())
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    pub fn save_favourites(
        &self,
        favourites: &std::collections::HashSet<String>,
    ) -> anyhow::Result<()> {
        // The favourites file is global now, so it is that parent which has to
        // exist — `ensure_dir` only makes the workspace.
        let path = self.favourites_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Sorted, so the file does not churn between runs for no reason.
        let mut ids: Vec<&String> = favourites.iter().collect();
        ids.sort();
        std::fs::write(path, serde_json::to_string_pretty(&ids)?)?;
        Ok(())
    }

    pub fn folders_path(&self) -> PathBuf {
        self.dir.join("folders.json")
    }

    /// Extra folders the library scans, on top of the bundled shaders and
    /// assets dirs.
    ///
    /// Still per-workspace, unlike favourites and saved layers: a set can carry
    /// the clips for that gig. Worth revisiting — a shader library is usually a
    /// property of the rig, and per-workspace folders mean a new set starts
    /// with an empty library.
    pub fn load_folders(&self) -> Vec<PathBuf> {
        std::fs::read_to_string(self.folders_path())
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    pub fn save_folders(&self, folders: &[PathBuf]) -> anyhow::Result<()> {
        self.ensure_dir()?;
        std::fs::write(self.folders_path(), serde_json::to_string_pretty(folders)?)?;
        Ok(())
    }
}

#[cfg(feature = "mixer")]
impl Workspace {
    /// Write a saved layer, returning the file it landed in. The name is
    /// slugified so a layer called "Cam / Blur" cannot escape the directory.
    pub fn save_layer(&self, layer: &crate::scene::SavedLayer) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(self.layers_dir())?;
        let slug: String = layer
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let slug = slug.trim_matches('_').to_string();
        let slug = if slug.is_empty() {
            "layer".to_string()
        } else {
            slug
        };
        let path = self.layers_dir().join(format!("{slug}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(layer)?)?;
        Ok(path)
    }

    /// Write a saved master chain. Same slug rules as a saved layer.
    pub fn save_chain(&self, chain: &crate::scene::SavedChain) -> anyhow::Result<PathBuf> {
        let dir = self.dir.join("chains");
        std::fs::create_dir_all(&dir)?;
        let slug: String = chain
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let slug = slug.trim_matches('_').to_string();
        let slug = if slug.is_empty() {
            "chain".to_string()
        } else {
            slug
        };
        let path = dir.join(format!("{slug}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(chain)?)?;
        Ok(path)
    }

    /// Write a saved group.
    pub fn save_group(&self, group: &crate::scene::SavedGroup) -> anyhow::Result<PathBuf> {
        let dir = self.dir.join("groups");
        std::fs::create_dir_all(&dir)?;
        let slug: String = group
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let slug = slug.trim_matches('_').to_string();
        let slug = if slug.is_empty() {
            "group".to_string()
        } else {
            slug
        };
        let path = dir.join(format!("{slug}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(group)?)?;
        Ok(path)
    }

    /// Every saved group on disk, name-sorted.
    pub fn load_groups(&self) -> Vec<crate::scene::SavedGroup> {
        let mut out: Vec<crate::scene::SavedGroup> = std::fs::read_dir(self.dir.join("groups"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .filter_map(|e| {
                let text = std::fs::read_to_string(e.path()).ok()?;
                match serde_json::from_str(&text) {
                    Ok(g) => Some(g),
                    Err(err) => {
                        log::warn!("[Groups] skipping {}: {err}", e.path().display());
                        None
                    }
                }
            })
            .collect();
        out.sort_by_key(|g| g.name.to_lowercase());
        out
    }

    /// Remove a saved group by name.
    pub fn delete_group(&self, name: &str) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(self.dir.join("groups"))?.flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if let Ok(g) = serde_json::from_str::<crate::scene::SavedGroup>(&text)
                && g.name == name
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// Every saved master chain on disk, name-sorted.
    pub fn load_chains(&self) -> Vec<crate::scene::SavedChain> {
        let mut out: Vec<crate::scene::SavedChain> = std::fs::read_dir(self.dir.join("chains"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .filter_map(|e| {
                let text = std::fs::read_to_string(e.path()).ok()?;
                match serde_json::from_str(&text) {
                    Ok(chain) => Some(chain),
                    Err(err) => {
                        log::warn!("[Chains] skipping {}: {err}", e.path().display());
                        None
                    }
                }
            })
            .collect();
        out.sort_by_key(|c| c.name.to_lowercase());
        out
    }

    /// Remove a saved chain by name.
    pub fn delete_chain(&self, name: &str) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(self.dir.join("chains"))?.flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if let Ok(saved) = serde_json::from_str::<crate::scene::SavedChain>(&text)
                && saved.name == name
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// Every saved layer on disk, name-sorted. Unreadable files are skipped
    /// rather than failing the whole listing.
    pub fn load_layers(&self) -> Vec<crate::scene::SavedLayer> {
        let mut out: Vec<crate::scene::SavedLayer> = std::fs::read_dir(self.layers_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .filter_map(|e| {
                let text = std::fs::read_to_string(e.path()).ok()?;
                match serde_json::from_str(&text) {
                    Ok(layer) => Some(layer),
                    Err(err) => {
                        log::warn!("[Layers] skipping {}: {err}", e.path().display());
                        None
                    }
                }
            })
            .collect();
        out.sort_by_key(|l| l.name.to_lowercase());
        out
    }

    /// Remove a saved layer by name, matching how it was written.
    pub fn delete_layer(&self, name: &str) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(self.layers_dir())?.flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            if let Ok(saved) = serde_json::from_str::<crate::scene::SavedLayer>(&text)
                && saved.name == name
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn save_scene(&self, scene: &Scene) -> anyhow::Result<()> {
        self.ensure_dir()?;
        let path = self.scene_path();
        let json = serde_json::to_string_pretty(scene)?;
        std::fs::write(&path, json)?;
        log::info!("[Workspace] scene saved to {}", path.display());
        Ok(())
    }

    pub fn load_scene(&self) -> anyhow::Result<Scene> {
        let path = self.scene_path();
        let json = std::fs::read_to_string(&path)?;
        let scene: Scene = serde_json::from_str(&json)?;
        log::info!("[Workspace] scene loaded from {}", path.display());
        Ok(scene)
    }

    #[cfg(feature = "projection")]
    pub fn save_stage(&self, stage: &crate::stage::KovvbojStage) -> anyhow::Result<()> {
        self.ensure_dir()?;
        let path = self.stage_path();
        let json = serde_json::to_string_pretty(stage)?;
        std::fs::write(&path, json)?;
        log::info!("[Workspace] stage saved to {}", path.display());
        Ok(())
    }

    #[cfg(feature = "projection")]
    pub fn load_stage(&self) -> anyhow::Result<crate::stage::KovvbojStage> {
        let path = self.stage_path();
        let json = std::fs::read_to_string(&path)?;
        let mut stage: crate::stage::KovvbojStage = serde_json::from_str(&json)?;
        stage.migrate_legacy_segments();
        stage.ensure_builtin_fixture_profiles();
        log::info!("[Workspace] stage loaded from {}", path.display());
        Ok(stage)
    }

    pub fn save_keymap(&self, keymap: &crate::keymap::Keymap) -> anyhow::Result<()> {
        self.ensure_dir()?;
        let path = self.keymap_path();
        let json = serde_json::to_string_pretty(keymap)?;
        std::fs::write(&path, json)?;
        log::info!("[Workspace] keymap saved to {}", path.display());
        Ok(())
    }

    pub fn load_keymap(&self) -> anyhow::Result<crate::keymap::Keymap> {
        let path = self.keymap_path();
        let json = std::fs::read_to_string(&path)?;
        let keymap: crate::keymap::Keymap = serde_json::from_str(&json)?;
        log::info!("[Workspace] keymap loaded from {}", path.display());
        Ok(keymap)
    }

    pub fn exists(&self) -> bool {
        self.scene_path().exists()
    }
}

/// Default workspace path: `./.kovvboj/` relative to CWD.
///
/// Falls back to a pre-rename `./.varda/` when it exists and `./.kovvboj/` does
/// not, so workspaces saved before the KOVVBOJ rename keep loading. Saving from
/// a legacy workspace keeps writing to `.varda/` — it is never migrated behind
/// the user's back.
/// UI preferences that outlive a session but are not part of the scene.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UiPrefs {
    /// Palette preset id — see `rustjay_gui::egui_theme::Palette::PRESETS`.
    pub palette: String,
    /// Width of the library panel.
    #[serde(default = "default_library_width")]
    pub library_width: f32,
    /// Width of the inspector panel.
    #[serde(default = "default_inspector_width")]
    pub inspector_width: f32,
    /// Built-in tabs left open as windows, by `GuiTab` name.
    ///
    /// Stored by name rather than index so adding a tab upstream cannot silently
    /// reopen the wrong window.
    #[serde(default)]
    pub open_windows: Vec<String>,
    /// Whether the Outputs window is showing.
    #[serde(default)]
    pub outputs_open: bool,
    /// Whether the Sequencer window is showing.
    #[serde(default)]
    pub sequencer_open: bool,
    /// Whether the inspector panel is showing.
    #[serde(default = "default_true")]
    pub inspector_open: bool,
    /// Whether the library panel is showing.
    #[serde(default = "default_true")]
    pub library_open: bool,
}

fn default_library_width() -> f32 {
    // Wide enough for a name plus both deck buttons and the scroll bar. At 200
    // the second button fell off the edge and could not be clicked. A saved
    // width from an older prefs file still wins — drag the edge if it is tight.
    240.0
}

fn default_inspector_width() -> f32 {
    300.0
}

fn default_true() -> bool {
    true
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            palette: "kovvboj".to_string(),
            library_width: default_library_width(),
            inspector_width: default_inspector_width(),
            open_windows: Vec::new(),
            outputs_open: false,
            sequencer_open: false,
            inspector_open: true,
            library_open: true,
        }
    }
}

/// Copy a directory's files into `to`, skipping any that already exist.
///
/// Migration only fills gaps: whatever is already global was written more
/// recently than whatever a workspace is carrying.
fn migrate_dir(from: &Path, to: &Path) {
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    let mut moved = 0usize;
    for e in entries.flatten() {
        let Some(name) = e.path().file_name().map(|n| n.to_owned()) else {
            continue;
        };
        let dst = to.join(&name);
        if dst.exists() {
            continue;
        }
        if std::fs::copy(e.path(), &dst).is_ok() {
            moved += 1;
        }
    }
    if moved > 0 {
        log::info!(
            "[Workspace] carried {moved} file(s) over from {}",
            from.display()
        );
    }
}

/// Copy one file if the destination has none.
fn migrate_file(from: &Path, to: &Path) {
    if let Some(parent) = to.parent()
        && std::fs::create_dir_all(parent).is_ok()
        && std::fs::copy(from, to).is_ok()
    {
        log::info!("[Workspace] carried {} over", from.display());
    }
}

pub fn default_workspace() -> Workspace {
    // ponytail: read-only compatibility shim. Delete once no `.varda/` remains
    // in the wild; a real migration would have to move presets/ too.
    if !Path::new(".kovvboj").exists() && Path::new(".varda").exists() {
        return Workspace::new(".varda");
    }
    Workspace::new(".kovvboj")
}

/// Recently opened workspaces, newest first.
///
/// Global rather than per-workspace — a list that vanished when you switched
/// sets would be useless. Lives beside the engine's own config, not in any
/// `.kovvboj/`.
fn recent_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("rustjay")
            .join("kovvboj-recent.json"),
    )
}

pub fn load_recent() -> Vec<PathBuf> {
    recent_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|j| serde_json::from_str::<Vec<PathBuf>>(&j).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

/// Move `dir` to the front of the list, capped at eight.
fn promote(list: &mut Vec<PathBuf>, dir: &Path) {
    list.retain(|p| p != dir);
    list.insert(0, dir.to_path_buf());
    list.truncate(8);
}

pub fn push_recent(dir: &Path) {
    let Some(path) = recent_path() else { return };
    let mut list = load_recent();
    promote(&mut list, dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(path, json);
    }
}

pub fn clear_recent() {
    if let Some(path) = recent_path() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_promotes_and_caps() {
        let mut list = Vec::new();
        for i in 0..10 {
            promote(&mut list, Path::new(&format!("/sets/{i}")));
        }
        assert_eq!(list.len(), 8);
        assert_eq!(list[0], PathBuf::from("/sets/9"));

        // Re-opening an old set moves it up rather than duplicating it.
        promote(&mut list, Path::new("/sets/5"));
        assert_eq!(list[0], PathBuf::from("/sets/5"));
        assert_eq!(list.iter().filter(|p| p.ends_with("5")).count(), 1);
    }
}

#[cfg(test)]
mod global_state_tests {
    use super::*;

    /// The bug: opening a new set lost every saved layer and every star,
    /// because both were stored in the workspace. They are building blocks you
    /// assemble sets from, so they belong to the rig.
    #[test]
    fn layers_and_favourites_are_not_stored_per_workspace() {
        // Only meaningful where a home directory exists — which is every real
        // machine. Without one the fallback is the workspace, deliberately.
        let Some(_) = dirs::data_dir() else { return };
        let ws = std::env::temp_dir().join(format!("kv-global-{}", std::process::id()));
        let w = Workspace::new(&ws);
        assert!(
            !w.favourites_path().starts_with(&ws),
            "favourites must not live in the workspace: {}",
            w.favourites_path().display()
        );
        assert!(
            !w.layers_dir().starts_with(&ws),
            "saved layers must not live in the workspace: {}",
            w.layers_dir().display()
        );
        // The scene is the show, and stays with it.
        assert!(
            w.scene_path().starts_with(&ws),
            "the scene is per-workspace"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_workspace_file_is_carried_over_but_never_clobbers() {
        let base = std::env::temp_dir().join(format!("kv-mig-{}", std::process::id()));
        let from = base.join("from");
        let to = base.join("to");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("a.json"), "old").unwrap();
        std::fs::write(from.join("b.json"), "carried").unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("a.json"), "mine").unwrap();

        migrate_dir(&from, &to);
        assert_eq!(
            std::fs::read_to_string(to.join("a.json")).unwrap(),
            "mine",
            "an existing global file wins"
        );
        assert_eq!(
            std::fs::read_to_string(to.join("b.json")).unwrap(),
            "carried",
            "a missing one is carried over"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
