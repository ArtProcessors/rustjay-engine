# KOVVBOJ pre-release review

Reviewed 2026-09-15 against `main` @ `8ca6ecd` (worktree branch
`worktree-agent-aff90f8bcf0973487`). Review only; nothing in the app was changed.

## How this was verified

- **Code**: every finding below cites `file:line` in `crates/kovvboj/src`
  unless another crate is named. Line numbers are as of `8ca6ecd`.
- **Lint**: the three CI commands pass with zero warnings — `cargo clippy -p
  kovvboj --all-targets -- -D warnings`, the same with `--features projection`,
  and `cargo clippy --workspace --all-targets -- -D warnings`. The only output is
  the future-incompat note for the `block v0.1.6` dependency.
- **Tests**: `cargo test -p kovvboj --features projection` — see the note at the
  end of Part A.
- **GUI**: the prebuilt all-features release binary
  (`target/release/kovvboj`, 14 Sep, no UI-relevant commits since) was run from
  an isolated scratch workspace holding only copies of `folders.json`,
  `keymap.json` and `ui.json`, so it opened the default set. Driven with a
  CGEvent helper and System Events at a 1200×800pt control window and in real
  fullscreen (1728×1117pt). Screenshots are in the session scratchpad
  (`shots/01_mix_1200.png` … `26_fullscreen.png`); they are referenced as
  `[shot NN]` and are not committed. The default stage created no `recordings/`
  directory during the run.
- **Environment gap**: this Mac's Xcode 27 licence has not been re-accepted,
  so `cc` from `/Applications/Xcode.app` refuses to link. Everything above was
  built with `DEVELOPER_DIR=/Library/Developer/CommandLineTools`. Worth running
  `sudo xcodebuild -license` before the next release build.
- **Not verified**: laser DACs, sACN/Art-Net hardware, Windows and Linux
  packaging, and first launch of the *bundled* `.app` on a fresh Mac. Findings
  about the bundle are read off `release-apps.yml` and the code, and are marked
  **unverified** where the behaviour was inferred rather than observed.

Severity: **blocker** (would embarrass the release or lose user data),
**should-fix** (visible to a first-time user or a live-show hazard),
**polish**.

---

## Part A — project review

### Blockers

**A1. The shipped bundle has no shaders, assets or resource directory
(blocker, unverified on a fresh machine).**
`shaders_dir()` and `assets_dir()` are `env!("CARGO_MANIFEST_DIR")` joined at
compile time (`lib.rs:10-12`, `lib.rs:188-190`), and every saved path is
relativized against the same directory (`scene/mod.rs:624-626`
`topology_base()`). The release workflow packages only the binary plus
frameworks/dylibs (`.github/workflows/release-apps.yml:200-240` macOS,
`242-252` Linux, `254-275` Windows). On a user's machine that directory is the
CI runner's path, so: the library lists nothing but the user's own folders; the
default set's two `ColorCycle.fs` layers fail to build (`lib.rs:1868-1880`);
`transition_dissolve.fs` fails and the fallback is the same missing file, so
the app toasts "Transition dissolve could not be loaded" on every launch and
the crossfader is inert (`lib.rs:99-125`, `render_transition` returns `None`
without a transition); `install_shader` cannot copy into the library
(`sources/registry.rs:432-459`); and any scene saved from `cargo run` stores
`shaders/foo.fs` relative to a path that does not exist on the other machine.
*Fix*: resolve one runtime resources root — `Contents/Resources/` in a bundle,
next to the executable otherwise, `$KOVVBOJ_RESOURCES` override, falling back
to `CARGO_MANIFEST_DIR` for `cargo run` — and use it for `shaders_dir`,
`assets_dir` and `topology_base`; copy `crates/kovvboj/shaders` and `assets`
into the package in the three packaging steps. Size M.

**A2. The workspace and recordings are CWD-relative (blocker, unverified for
a Finder launch).**
`default_workspace()` is `./.kovvboj` (`persistence/mod.rs:517-524`); one-click
recordings go to `./recordings` (`ui/mod.rs:16-26`, `lib.rs:3808-3811`,
`lib.rs:3945-3947`). Nothing calls `set_current_dir`. A macOS app launched from
Finder starts with CWD `/`, so every save hits `/.kovvboj` and fails; the
failure is a `log::warn!` (`lib.rs:936-953`), never a toast, so the set is
silently never persisted and the 30-second auto-save fails silently thirty
times a show. *Fix*: default the workspace to
`dirs::data_dir()/rustjay/kovvboj/<set>` (or `~/Documents/KOVVBOJ`) when CWD
holds no `.kovvboj` and is not writable; recordings to `~/Movies/KOVVBOJ`;
surface a failed save as an error toast once. Size S–M.

**A3. A layer whose file is missing is dropped from the scene and then saved
away (blocker).**
A clip on an unmounted drive, a moved image or a shader that no longer parses
makes `instantiate_source` fail; `apply_topology` logs and `continue`s
(`lib.rs:2025-2036`), and the layer is neither in `mixer.channels` nor in
`layer_sources`. An FX slot whose shader fails is dropped the same way
(`lib.rs:2286-2292`, `build_fx_slot` → `None`). Thirty seconds later the
auto-save (`lib.rs:2890-2905`) writes `scene.json` from the live mixer
(`Topology::from_mixer`), so the layer and its modulation prefix are gone from
disk permanently. If *every* layer fails, `scene_snapshot_if_ready`
(`lib.rs:829-835`) sees an empty channel list and saves an empty scene. The
user gets one log line. *Fix*: keep the descriptor — build a placeholder
(`SolidColorSource`) with the original `SourceEntry` kept in `layer_sources`
and a "missing: <path>" marker on the row, and toast it; the inspector's
"File…" / source picker then repairs it in place via the existing
`PendingSourceSwap`. Size M.

**A4. An old or corrupt `scene.json` is overwritten with the default set
(blocker).**
`init()` falls back to `build_default_graph` when `scene.json` fails to parse
or its topology is version 0 or empty (`lib.rs:4310-4342`, `usable_topology`
`lib.rs:1469-1472`). The first auto-save then replaces the file. The only
warning about a stale topology is the runtime-load toast "Your file is
untouched" (`lib.rs:1485-1489`), which is true for thirty seconds. The
`.varda/` fallback (`persistence/mod.rs:517-524`) loads fine, but a `.varda/`
scene from before the layer model is exactly the version-0 case. *Fix*: when a
load fails or is stale, move the file to `scene.json.unreadable-<ts>` (or set a
`scene_read_only` flag that blocks auto-save until an explicit ⌘S), and say so.
Size S.

### Should-fix

**A5. Two recorders reconcile the same outputs with different settings.**
`OutputsTab::draw` (`ui/mod.rs:7761-7837`) and `prepare()`
(`lib.rs:3799-3819`, `3938-3955`) both start and stop per-output recordings.
The tab uses the codec picker and `auto_record_path` (`ui/mod.rs:272-294`);
`prepare` hard-codes H.264 and a different filename. Whichever runs first in a
frame wins, so the codec the user chose applies only if the Outputs window
happens to draw before `prepare`. *Fix*: delete the tab's block; make codec a
field on `KovvbojProjector`/`KovvbojHeadlessConfig` that `prepare` reads.
Size S.

**A6. First launch opens the camera and microphone before the user does
anything.**
The default graph contains a `Camera` layer on device 0 (`lib.rs:1833-1843`),
so a fresh Mac shows the camera TCC prompt at startup, and the engine's audio
analysis opens the microphone. With no camera the layer is black and the only
signal is a log warning (`sources/camera_source.rs:263-268`). The
`Info.plist` template does carry both usage strings
(`.github/packaging/Info.plist.tmpl`), so this prompts rather than crashes.
*Fix*: a camera-free default set (solid + two generators); the camera prompt
then appears when a camera layer is added. Size S.

**A7. The engine's global config leaks parameter values between sets.**
`~/.config/rustjay/KOVVBOJ.json` stores every custom parameter base
(`crates/rustjay-engine/src/config.rs:420-424`) and reapplies them at startup
(`config.rs:352-355`) before the scene's own restore runs. Values the scene
does not carry — the default set, a new set — inherit the previous set's, and
the startup log prints hundreds of "Config parameter 'ch_…' not found in
current descriptors" lines (observed in the GUI run's `app.log`). *Fix*:
kovvboj opts out of engine `custom_params` persistence (the scene owns
params); an engine-side `EffectPlugin::persists_params() -> bool` or clearing
`custom_params` in `on_engine_ready`. Size S.

**A8. Device discovery blocks the render thread for half a second.**
`rescan_library()` calls `refresh_builtins()`, which calls
`list_ndi_sources(500)` (`sources/registry.rs:305`) and Syphon discovery
(`registry.rs:316-329`). It runs on the render thread at startup
(`lib.rs:2499`), on every file-watcher Create/Remove (`lib.rs:2795-2802`), and
from the ⟳ buttons (`ui/mod.rs:1965-1967`, `2968-2975`). Dropping a shader into
the folder mid-show stalls the output ≥0.5 s. *Fix*: discover devices on a
thread with a pending-result slot (the pattern `pending_library_folder` already
uses), and do not re-enumerate devices on a *file* event. Size M.

**A9. Edge blend and dome are global; per-projector overrides are dead
data.**
One `EdgeBlendSync` and one `DomeSync` feed every projector
(`lib.rs:1687-1691`, `main.rs:84-98` clone the same `d`/`e` into each chain).
A two-projector blend needs mirrored edges, so the Edge Blend section in
Outputs cannot do the one job it exists for; it is presented as if it could.
`KovvbojProjector.use_global_warp/dome/edge_blend`, `warp_mode`,
`dome_enabled`, `edge_blend_config` (`stage/mod.rs:706-723`) are serialized
into every `stage.json` and never read (PARITY.md gaps table says so). *Fix
for release*: label the section "applies to every projector" and delete the
six dead fields (they are `serde(default)`, so old files still load). Later:
per-projector syncs. Size S now, M later.

**A10. The Outputs window overflows the screen (should-fix).** `[shot 06]`
The projector row (`ui/mod.rs:7178-7380`) puts a `text_edit_singleline`
(`ui/mod.rs:7180`) in a `horizontal` inside an auto-sizing `egui::Window`, so
the name field sizes from the screen width, the window grows to the full
1200pt, and even then "Fullscreen" is clipped and 🗑 is off-window. Detail in
Part B-L1.

**A11. In deck mode a layer's name collapses to "…" at 1200pt.** `[shot 01]`
Detail in Part B-L2.

**A12. `View → Input` and `View → Output` are engine windows that duplicate
and undermine the app's own.** `[shots 07, 08]`
`VIEW_TABS` (`shell.rs:44-55`) opens the engine's Input and Output tabs
unconditionally. `OutputsTab::replaces()` (`ui/mod.rs:7147-7149`) is dead code
under the shell: `run_with_projection_egui_shell` passes `Vec::new()` as the
custom tabs (`crates/rustjay-engine/src/lib.rs:212-221`), so nothing ever
consults it, and both windows can be open at once. The engine Output window
starts NDI/Syphon senders from the *hidden* main output under the engine's
"RustJay" name (`crates/rustjay-gui/src/egui_tabs/output.rs:33-93`) — a second
sender path beside the Outputs window's per-projector "kovvboj — <name>"
senders, and its "Fullscreen Output" toggles the hidden window. The engine
Input window offers the same cameras as the Library's DEVICES and starts a
second capture session on the same device (engine `InputManager` vs kovvboj's
per-device `CAMERA_SESSIONS`, `sources/camera_source.rs:54-70`), which the
LED-map code already warns clashes (`ui/ledmap_tab.rs:86-88`). *Fix*: remove
`GuiTab::Input` and `GuiTab::Output` from `VIEW_TABS`; delete the dead
`replaces()`. Size S.

**A13. First launch opens a screen-sized projector window over the control
window.** `[GUI run]`
`with_default_surface()` adds an enabled 1920×1080 windowed projector
(`stage/mod.rs:541-552`, defaults `726-756`), which `main.rs:65-101` opens at
startup. On a laptop it covers the control window and the cursor is hidden in
it (`projection.rs:702-704`). *Fix*: first projector defaults to a modest
window (e.g. 960×540) or to `enabled: false` until a monitor is chosen; or
open it below the control window. Size S.

**A14. Docs disagree with the app.**
`guide/src/examples/kovvboj.md:16-21` says there is no crossfader and a master
dimmer replaces it; `:26-27` and `:89-93` describe MIX/STAGE/**MAP** modes and
"LED Map mode" (the app has MIX/STAGE/**CALIBRATE**); `:97-100` says learn is
armed "from the MIDI / Modulation windows" (it is the top-bar MIDI/MOD
buttons, `shell.rs:1105-1145`). Nothing mentions decks, TAKE, transitions,
workspaces' `File` menu items beyond a sentence, or the Library menu's scan.
`crates/kovvboj/PARITY.md:33-34` says the crossfader was removed and `:59`
claims a CPU/MEM top-bar readout that does not exist (`sysmon` writes
`perf.cpu_percent`, nothing draws it). `KOVVBOJ_UI.md` still lists "MAP mode".
*Fix*: rewrite the guide page's "What it is" and modes around decks; retire
PARITY.md or mark it historical. Size S.

**A15. No app icon in the package.** `crates/kovvboj/packaging/` holds only
`README.md`; the workflow's icon slots (`release-apps.yml:15-20`) are empty, so
the `.app` and the Windows shortcut ship with the generic icon. Size S.

**A16. An intentionally emptied set relaunches as the default set.**
`usable_topology` treats an empty layer list as stale (`lib.rs:1471`), so a
set the user cleared to nothing comes back with the four default layers, the
demo sequence and the demo FX. *Fix*: only `version < TOPOLOGY_VERSION` is
stale. Size S.

### Polish

**A17. "Content Mapping" is a no-op.** `[shot 02]` The Fill/Mapped combo
(`ui/mod.rs:6639-6661`) changes nothing in the render path; `uv_crop()`
returns the same for both (`stage/mod.rs:235-240`) and is not called; the only
effect is the dashed bounding box overlay (`ui/mod.rs:5636-5679`). Remove the
combo or the variant.

**A18. `SurfaceSource::Deck` is dead.** Never constructed; matched only to
warn "not yet implemented" (`stage/mod.rs:39-43`, `lib.rs:2348-2353`,
`lib.rs:1183-1185`). Delete the variant.

**A19. Every imported SVG/DXF surface gets uuid `"import0"`**
(`ui/mod.rs:4381`, `4405`), so a second import collides and lighting segments
that reference a surface by uuid (`LightingSegment.source_surface`) become
ambiguous. Use `scene::new_uuid()`.

**A20. `keymap.json` is written every 30 s and never read for dispatch.**
`keymap.rs:24-27` says so; the only read is a count (`lib.rs:2585-2599`).
Either dispatch from it or stop persisting it.

**A21. Demo content in every new set.** `build_default_graph` pre-loads a
four-step sequencer (`lib.rs:1904-1915`) and a "FX demo exercise"
BrightnessContrast on the second layer (`lib.rs:1917-1928`).

**A22. Hot-reload only watches the bundled shader folder**
(`lib.rs:2500`), not the user's library folders, while the guide advertises
hot-reload generally.

**A23. `→ HAP` conversion needs `ffmpeg`/`ffprobe` on `PATH`**
(`ui/mod.rs:704`, `rustjay_io::hap_encode::ffmpeg_to_hap`). A Finder-launched
bundle does not have `/opt/homebrew/bin` on `PATH`; the button will fail with
a toast. Fall back to the bundled `Contents/libs` ffmpeg or document it.

**A24. Lock-poison unwraps** — `sources/camera_source.rs:59,257,303`,
`ui/ledmap_tab.rs:115,122,131,156,188`, `lib.rs:1736-1786`, `stage/mod.rs:1368`
— only panic after a prior panic on the same lock; `ui/mod.rs:7485-7486` unwrap
`dome_sync`/`edge_blend_sync`, which are `Some` outside tests. No user-data or
I/O unwraps were found in non-test code; parsing paths all go through
`serde_json::from_str` with warnings, and `KovvbojStage` has `serde(default)`
on every field added since Phase 8 except `KovvbojSurface.warp`
(`stage/mod.rs:94-98`) and the three `use_global_*` bools, so a hand-edited or
non-projection-build `stage.json` falls back to the default stage rather than
crashing (`lib.rs:2574-2582`, `main.rs:32`).

**A25. The 145 GB recording is closed.** `KovvbojProjector.recording` and
`KovvbojHeadlessConfig.recording` are `#[serde(skip)]` (`stage/mod.rs:694-699`,
`869-874`); the user's `stage.json` carries no `recording` key; the GUI run
created no `recordings/` directory. Only the `output_type: Recording` routing
persists, disarmed.

**A26. Web remote defaults to "Trusted LAN Mode"** `[shot 16]` (engine
default): once started, any device on the LAN controls the show without a
token. Worth a one-line note in the release notes.

### Persistence compatibility (checked, fine)

- `.varda/` fallback works (`persistence/mod.rs:517-524`); saved layers,
  favourites and previs migrate to the global dir on first read
  (`persistence/mod.rs:90-109`, `previs.rs:181-188`).
- Older `scene.json`: `topology` absent → default graph (expected);
  `groups`/`parent`/`transition`/`audio_routing` absent → defaults, with tests
  (`scene/mod.rs:929-934`, `1175-1190`, `1213-1219`, `1158-1167`).
- Older `stage.json`: legacy single `segment` migrates
  (`stage/mod.rs:936-946`); missing `fixture_profiles`, `lighting_outputs`,
  `led_surface`, `laser_*` default.
- Old `mixer_state` applies by uuid and ignores unknown channels
  (`crates/rustjay-mixer/src/preset.rs:141-166`).

### Leads from the brief, resolved

| Lead | Verdict |
|---|---|
| Built-in `Output` window vs app `Outputs` | Both open at once; `replaces()` is dead under the shell. See A12. |
| Built-in `Input` vs Library sources | Redundant and opens a second camera session. See A12. |
| Projector config split Stage/Outputs | Real; the geometry half belongs in Stage, the "what outputs exist" half in Outputs. Edge blend is the piece on the wrong side. See Part B. |
| Lighting outputs in the Stage LIGHTING inspector | Right place; but LED lives in three places (CALIBRATE, LIGHTING list, LIGHTING pane). See B overlap matrix. |
| Deck pop-outs beside projector windows | Fine: `add_preview` keeps them off the projector index list (`projection.rs:632-651`); verified opening one leaves "Projector 1" at index 0 `[shot 24]`. |
| Recording in three places | Top-bar REC and the Outputs "Recording" section both record the *hidden main output* through the engine; per-output Recording is separate; the duplicate reconciler is A5; `stage.json` cannot auto-start (A25). |
| Settings mixes palette with engine settings | Yes, with two headings and an "Output Resolution" that governs the hidden window. See B. |

### Test run

`cargo test -p kovvboj --features projection` result: see the "Test result"
line appended at the end of this file.

---

## Part B — UI audit

### B.1 Inventory

Every surface, from the code and the screenshots.

| # | Surface | Where drawn | Controls |
|---|---|---|---|
| 1 | **Top bar** | `shell.rs:764-1238` | KOVVBOJ; File (workspace label, New/Open/Open Recent/Save ⌘S/Save As/Revert, Export/Import Set, Reveal, Quit); Edit (Undo/Redo, Settings); View (Input, Output, Color†, Motion†, Audio, Modulation, MIDI, OSC, Web, Presets · Outputs, Sequencer · Library, Preview); Library (Scan/Rescan/Cancel, Add folder); Help (About). Right: MIDI, MOD, ● REC, OSC pill, WEB pill, BPM (beat flash), FPS. |
| 2 | **Mode bar** | `shell.rs:1244-1284` | MIX / STAGE / CALIBRATE; ⛶ focus (MIX only). |
| 3 | **Library panel** (MIX) | `ui/mod.rs:2935-3905`, shell `468-522` | ◀ collapse, ⟳ rescan, search ✖; sections LAYERS, DECKS, GROUPS, CHAINS, DEVICES, MEDIA, GENERATORS, EFFECTS — each row ★ + `[A][B]` (or ➕ for effects/chains) + ✖ for saved items + name (effects drag); Add file…; Folders (list ✖, Add folder…); Add Stream URL (URL, name, Add). Width drag. |
| 4 | **Deck columns** (MIX) | `shell.rs:604-671`, `ui/mod.rs:2236-2930` | DECK A/B heading: name field + 💾 save deck. Layer row: ≡ drag, thumb+name (select, ⌘-pick, context menu Group N / Remove from group / Ungroup), S, M, opacity slider, blend combo, ✕; strip: source chip → inspector, FX chips (dot bypass, click select, drag), ✖ per chip, `+` file picker. Group row (`2098-2233`): ≡, ▶/▼, name, opacity, M, S, 💾, ✖ ungroup, group FX strip. "NOT ON A DECK" section. |
| 5 | **Crossfader strip** | `shell.rs:1442-1625` | Deck A thumbnail (click → pop-out), A–B slider, "transition" label → inspector, transition combo (+ Add transition…), TAKE seconds, TAKE, deck B thumbnail. |
| 6 | **MASTER panel** | `shell.rs:593-601`, `ui/mod.rs:1569-1704` | Dim slider; chain name + 💾 Save chain (+ "replaces"/"N saved"); master FX strip. Resizable, floor 200pt. |
| 7 | **Inspector** (right) | `shell.rs:525-558`, `ui/mod.rs:1715-2067` | Live master preview. Nothing selected: MASTER Dimmer + hint. Layer: rename field, Save to library, Source combo + ⟳ (devices), Opacity, Blend, kind label; Video adds File…/→ HAP, play/trigger/speed/tempo/loop/position/in/out; Text adds text, font …/combo, atlas summary, speed/tempo. Source chip: text controls, pacing (Speed, Sync, div, ⟲), every `ch_<uuid>_` param — including two **unlabeled** enum combos (input select "Slot 1", key mode "None") and Key R/G/B/threshold/smoothness/Luma Invert. FX chip: pacing + params. Group: rename + `grp_` params. Transition: its params. |
| 8 | **STAGE mode** | `ui/mod.rs:3910-4048`, `4234-5695` | Heading "Stage"; layer tabs SURFACES/LIGHTING/LASERS; left list (Surfaces: select/✖, + Add Rectangle, + Add Circle, Import path + Import — or LED Surface on LIGHTING: + Add, map file, Load, count, priority, Drive sACN, Remove); canvas bar: Canvas W×H, Live preview, Edit mode, Zoom + Fit; canvas: master preview, surfaces, UV-crop handles, corner-pin/mesh handles, edge-blend bands, LED dots + quad, laser quad + live path, lighting segment regions + handles; bottom pane (240pt): Geometry / Lighting / Lasers inspector. |
| 8a | Stage → Geometry pane | `ui/mod.rs:6548-7136` | Properties: Name, Source (Master/channels/Domemaster), Content Mapping, Position & Size px (or X/Y/Ø), "→ output: W×H px (source ×N)", Vertices (px), Extra Contours, Warp Mode, Corners (px on output / normalized), UV Crop + Reset, Dome (when Domemaster): Enabled, Resolution, FOV, Tilt, Az/El/Roll. |
| 8b | Stage → Lighting pane | `ui/mod.rs:5850-6545` | Lighting Outputs: per output enable, name, type sACN/Art-Net, 🗑, gamma, priority, fps, dest IP; Segments: enable, name, ✖, source surface, region u0 v0 u1 v1, grid, corner, serp, axis, profile, universe, ch, patch summary, sample mode, bright, gains R/G/B, dim, white mode + amount; + Add segment; Activity meters; + Add lighting; Patch overlaps; Fixture Profiles (built-ins listed; custom: name, add R/G/B/W/A/UV/D/S, ✖ last, Clear, static value, order); New from template + Add. |
| 8c | Stage → Lasers pane | `ui/mod.rs:5711-5801`, `ui/laser_tab.rs:430-686` | Scan region: + Place on canvas, Draw live path, Reset, Remove; Field correction TL/TR/BR/BL + Reset. Laser deck: material path/Browse/Load, deck tabs, ARM/DISARM, BLACKOUT, blocked reason, preview, Show blanked travel, Scanner pps/Hz/Retune, Path settling (4 sliders + Skip dark), DAC (laser-dac): scan, list, Connect/Disconnect, Calibrate with a camera. |
| 9 | **CALIBRATE mode** | `ui/ledmap_tab.rs:179-277` | LED count, universe, channel, colour order, on level, threshold, hold frames, camera index, sACN priority, output file, Start calibration / progress / Stop, status; Playback (sACN): Start/Stop LED output. |
| 10 | **Outputs window** | `ui/mod.rs:7142-7932`, shell `1350-1358` | Projectors: enable, name, size, monitor, surface, type (Display/NDI/Recording/Syphon; Spout/V4L2 by OS), ⏺ REC/⏹ STOP when Recording, rotate, Fullscreen, 🗑; + Add projector. Headless: same minus monitor/rotate/fullscreen; + Add headless. Edge Blend: Left/Right/Top/Bottom enable, width, γ. Recording: codec, path, Browse…, ⏺ Start / ⏹ Stop, ● REC. |
| 11 | **Sequencer window** | `ui/mod.rs:7934-8069` | Play/Pause/Stop/Loop; steps list ✖; + Crossfade/Hold (beats/timed); Quick Transitions Auto→A/B, Beat-Sync→A/B. |
| 12 | **Settings window** (Edit) | `shell.rs:1313-1346` + `rustjay-gui/src/egui_tabs/settings.rs` | Appearance: Palette. Engine: UI Scale, Hide main output window, Internal Resolution, Output Resolution (Display/NDI), Apply, keyboard shortcut list, Performance readouts, Target FPS, Present mode, Save All Settings. |
| 13 | **Engine windows** (View) | `shell.rs:1296-1311` | Input (Refresh, Input 1/2 status, webcam/NDI/Syphon pickers, Start Input 1/2); Output (Fullscreen Output, NDI name + Start, Syphon name + Start); Color/Motion (param categories); Audio; Modulation (LFO/ADSR/step/audio/trigger sources); MIDI (devices, learn, clear, per-category params); OSC (server, addresses); Web (server, LAN mode); Presets (Quick Slots 1–8). |
| 14 | **OS windows** | `main.rs:65-101`, `shell.rs:1678-1698`, engine | Projector N windows (cursor hidden; Shift+F/Esc); Deck A/B pop-outs; the engine's main output window, hidden by default (`lib.rs:2391-2393`). |

† Color/Motion only when such params exist.

### B.2 Overlap matrix

| Capability | Appears in | Should own it | Others |
|---|---|---|---|
| Master dimmer | MASTER panel "Dim" (`ui/mod.rs:1602`); inspector empty state "Dimmer" (`2073`) | MASTER panel | Inspector empty state keeps only the hint. |
| Layer opacity / blend | Layer row; inspector (Layer) | Row (performance) | Inspector keeps them as the precise editor — intentional, keep. |
| Layer keying (mode, colour, threshold…) | Inspector **Source** chip, as unlabeled params (`1483-1496`) | Inspector **Layer** (it is a mix property, `ch_<uuid>_key_*`) | Drop from the Source view; label the combo. |
| Engine input select ("Slot 1") | Inspector Source chip | Nowhere in kovvboj (layers are their own sources) | Hide `input_select` in the inspector (`DECK_CONTROL_KEYS`, `ui/mod.rs:967-979`). |
| Record to disk | Top-bar ● REC (engine main output, H.264, `shell.rs:1147-1172`); Outputs "Recording" section (engine main output, codec/path, `ui/mod.rs:7846-7930`); Outputs per-output type Recording + ⏺ (`7299-7316`, `7646-7659`) | Outputs per-output | Top-bar REC arms/disarms the first enabled projector's recording; delete the engine Recording section (the hidden window is not what anyone means by "record"). |
| NDI / Syphon / Spout send | Outputs per-output type; View → Output (hidden main output as "RustJay") | Outputs | Delete View → Output (A12). |
| Fullscreen | Outputs per-projector Fullscreen; View → Output "Fullscreen Output" + Shift+F (hidden window) | Outputs | Delete with View → Output. |
| Output resolution | Settings "Output Resolution (Display/NDI)" (hidden window); projector size in Outputs | Outputs | Hide the engine output-res block or caption it "hidden main output". |
| Cameras / NDI / Syphon as sources | Library DEVICES + inspector Source combo; View → Input | Library | Delete View → Input (A12). |
| Edge blend | Outputs (controls, `7711-7759`); Stage canvas (preview bands, `4594-4625`) | Stage SURFACES inspector, per projector | Move controls beside the preview; remove from Outputs. |
| Projector geometry | Outputs (size/monitor/surface/type/rotate); Stage Geometry (read-only "→ output px (source ×N)", corner-pin in output px) | Split is right: Outputs = which outputs exist; Stage = geometry | Nothing to move once edge blend goes. |
| LED strip | CALIBRATE (make map, "Playback: Start LED output" `ledmap_tab.rs:253-276`); Stage LIGHTING list (LED Surface: map file, Load, "Drive sACN output" `5808-5842`); Stage LIGHTING pane (Lighting Outputs, unrelated grid outputs) | Stage LIGHTING list | Delete CALIBRATE's Playback section (same `StartLed`, different priority/path fields); after calibration, pre-fill the LED surface's map path. |
| Lighting outputs + fixture profiles | Stage LIGHTING pane only (moved from Outputs) | Stage LIGHTING | Right place; the pane needs to be a column, not a 240pt strip (B-L6). |
| Crossfade / transition | Crossfader strip (transition, TAKE, seconds); Sequencer "Quick Transitions" Auto→A/B, Beat-Sync→A/B (fixed 1 s / 4 beats, ignore `take_seconds`, `8043-8067`) | Crossfader strip | Delete Quick Transitions; keep steps. |
| Whole-set snapshots | File → Save/Save As (workspace); Presets Quick Slots 1–8 (engine bank storing the whole `Scene`, `lib.rs:4276-4301`); Library saved decks/groups/chains/layers | File + Library | Question for the user: Quick Slots (⇧F1–F8) are a performance feature; if kept, rename "Snapshots" and say what a slot stores. |
| MIDI learn / LFO assign | Top-bar MIDI/MOD; MIDI window "MIDI Learn" text; Modulation window | Top bar | Windows keep device/config only. |
| Undo | Edit menu; ⌘Z | Both — fine. | |
| Live previews | Inspector master preview; Stage canvas (same texture); deck thumbs; pop-outs; projector windows | By design | None. |
| Palette + engine settings | One Settings window, two headings | Settings | Drop the inner "Application Settings" heading (engine) or put the palette combo *inside* the engine tab via a hook. |

### B.3 Proposed information architecture

Principle: three tiers — **MIX** (set), **STAGE** (venue), **Outputs** (what
exists) — and one place per capability. Deletions first.

Delete: `View → Input`, `View → Output`, Outputs "Edge Blend", Outputs
"Recording", Sequencer "Quick Transitions", CALIBRATE "Playback (sACN)",
Stage's `ui.heading("Stage")`, "Content Mapping", the inner headings in every
View window, the engine Output-resolution block from Settings (or caption it).

Merge: edge blend → Stage SURFACES inspector; LED playback → Stage LIGHTING
list; keying → inspector Layer view.

```
TOP BAR   KOVVBOJ  File Edit View Library Help          60 FPS 120 BPM  WEB OSC  ●REC  MOD MIDI
          View: Color Motion Audio Modulation MIDI OSC Web Presets | Outputs Sequencer | Library Preview
MODES     [MIX] [STAGE] [CALIBRATE]                                                              ⛶

STAGE (all three layers share this frame)
┌ SURFACES │ LIGHTING │ LASERS      Canvas 1920×1080  ☑ Live  ☐ Edit  zoom ──── Fit ┐
├───────────┬────────────────────────────────────────────┬───────────────────────────┤
│ LIST 180  │ CANVAS (fills)                             │ INSPECTOR  (right column, │
│           │                                            │  scrolls, same width as   │
│ SURFACES: │  master preview · surfaces · UV/warp       │  MIX's inspector)         │
│  Main   ✕ │  handles · blend bands · LED dots · laser  │                           │
│  + Rect   │                                            │ SURFACES: Name, Source,   │
│  + Circle │                                            │  Size px, Warp, Corners,  │
│  Import…  │                                            │  UV crop, EDGE BLEND      │
│           │                                            │  (per projector showing   │
│ LIGHTING: │                                            │  it), Dome if Domemaster, │
│  LED surf │                                            │  → outputs (read-only)    │
│  Outputs… │                                            │ LIGHTING: selected output │
│  + Add    │                                            │  transport, segments,     │
│           │                                            │  profiles                 │
│ LASERS:   │                                            │ LASERS: scan region,      │
│  Deck 1   │                                            │  field correction, deck   │
└───────────┴────────────────────────────────────────────┴───────────────────────────┘
The bottom 240pt strip becomes a right column: one inspector idiom for the whole app,
and the lighting patch editor stops living in a letterbox.

OUTPUTS window (View → Outputs), default 640pt, resizable
┌ Outputs ─────────────────────────────────────────────────────┐
│ PROJECTORS                                         + Add     │
│ ☑ Projector 1        1920 × 1080   monitor None   surface Main│
│    type [Display ▾]  rotate [0° ▾]  codec [H.264 ▾]  [Fullscreen] [🗑] │
│ HEADLESS                                           + Add     │
│ ☐ Headless 1         1920 × 1080   surface Main              │
│    type [NDI ▾]                                    [🗑]      │
│ RECORDINGS folder  ~/Movies/KOVVBOJ                  [Browse…] │
└──────────────────────────────────────────────────────────────┘
Two-line rows, name field at a fixed width. REC lives on the row whose type is Recording;
the top-bar ● REC mirrors the first enabled projector's row.

MIX (unchanged in structure)
 Library │ DECK A ─ DECK B │ Inspector      — inspector Layer view gains the key block;
         │ crossfader strip │                 Source view loses key + input select.
         │ MASTER           │
```

Commit-sized slices for Phase 2 (each presentational, each with a kittest
rect assertion where a width is involved): (1) `VIEW_TABS` minus
Input/Output + dead `replaces()`; (2) Outputs projector/headless rows to two
lines with fixed-width name (test: row right edge ≤ window right at 640pt);
(3) delete Outputs Recording + duplicate reconciler (A5, bug fix);
(4) edge blend → Stage inspector; (5) Stage inspector bottom strip → right
column; (6) LED playback out of CALIBRATE; (7) Sequencer Quick Transitions
out; (8) inspector: key block → Layer, hide input_select, label enums;
(9) layer row name/slider fix (test: name width ≥ N pt at a 290pt column);
(10) headings: remove doubles, remove "Stage"; (11) window default positions
cascade; (12) library heading hint truncates.

### B.4 Layout defects

**B-L1. Outputs window blows out to the screen width and still clips.**
`[shot 06]` `ui/mod.rs:7178-7380`. The name `text_edit_singleline`
(`:7180`) has no `desired_width`; inside an auto-sizing `Window`
(`shell.rs:1352-1356`, `default_width(520)`, no max) `available_width()` is the
screen's, so the row is wider than any window: at 1200pt the window is 1200pt
wide, "Fullscreen" is cut to "Fullscree" and 🗑 is unreachable. Same shape for
headless rows (`7556-7670`). Fix: `.desired_width(140.0)` on both name fields
and split each row into two `horizontal`s; add `Window::max_width`. Test: with
a 640pt window, `get_by_label("🗑")` rect inside the window rect.

**B-L2. Layer name collapses to "…" and the opacity slider reads as a
checkbox.** `[shot 01]` vs `[shot 21, 25]`. With the persisted panel widths
(library 200, inspector 400) a deck column is ~290pt; `controls_w`
(`ui/mod.rs:2436-2440`) reserves ~200pt, the 39pt thumbnail eats most of the
rest, and `truncate()` leaves "…" for every layer. At the default widths
(240/300) names longer than ~8 characters still ellipsize (unverified at
defaults). Separately, the opacity slider's track is invisible against the row
so at 1.0 only the handle shows and it looks like a checkbox — visible in
every MIX shot, including focus mode where it is 96pt wide. Fix: drop the
thumbnail below ~330pt columns or cap it at the row's spare width; give the
slider a visible track (`trailing_fill` or the theme's `inactive.bg_fill`).
Test: name label width ≥ 40pt at a 290pt column.

**B-L3. Library section hint is clipped.** `[shot 01]` "➕ new layer" renders
as "+ new lay" at 200pt: the hint label in the right-to-left heading
(`ui/mod.rs:3329-3339`) has no `truncate()` and the heading button before it
does not yield. Fix: `Label::new(..).truncate()` or drop the hint into a
hover on the heading.

**B-L4. Library names truncate hard at 200pt.** `[shot 01]` ★ + A + B + icon
consume ~70pt before the name (`ui/mod.rs:3105-3287`). The default is 240pt
(`persistence/mod.rs:445-450`) but a saved 200 wins. Fix: floor the persisted
width at the default, or collapse A/B into one split button.

**B-L5. Doubled handle labels on the stage canvas.** `[shot 02]` UV-crop
handles (`ui/mod.rs:4817-4924`, labelled TL/TR/BR/BL) and corner-pin handles
(`4928-4999`, same labels, same positions for an identity warp) draw on top of
each other: "BL BL", "BR BR". Fix: draw corner-pin labels only when the corner
differs from the crop corner, or offset them (the code already computes
`label_alignments` for this and then draws both).

**B-L6. Stage inspector is a 240pt letterbox with a centred 400pt column.**
`[shots 02, 03]` `inspector_h = min(240, 40%)` (`ui/mod.rs:3955`);
`draw_surface_properties` wraps everything in `vertical_centered` +
`set_max_width(400)` (`6606-6608`) so labels are centred over left-aligned
widgets and only Name/Source/Content Mapping fit before scrolling. The
lighting pane (`5850-6545`) puts a seven-row segment editor and the profile
library in the same strip. Fix: right column (B.3); left-aligned labels.

**B-L7. Every window repeats its title as a heading**, and Stage repeats the
mode name. `[shots 02, 06-17]` `ui.heading("Stage")` (`ui/mod.rs:3920`),
`ui.heading("Outputs")` (`7161`), `ui.heading("Sequencer")` (`7948`), and the
engine tabs' own headings under egui's window title. Fix: remove the
`heading` calls in kovvboj; for engine tabs, `draw_builtin_tab` could take a
`show_heading: bool` (engine change) or the shell titles the window with the
tab name and accepts the duplicate.

**B-L8. Settings window has two heading tiers.** `[shot 10]` "Appearance"
(app) then "Application Settings" (engine, `settings.rs:11`), and "Output
Resolution (Display/NDI)" configures the hidden main output. Fix: see B.2.

**B-L9. Modulation window rows are truncated.** `[shot 13]` "LFO lfo_ →
0.00" at the 420pt default (`shell.rs:1300-1310`). Engine tab; a 520pt default
for that one window is enough.

**B-L10. Every View window opens at the same top-left spot** and stacks
(`shell.rs:1296-1311`, no `default_pos`). `[shots 06-17]` Fix: cascade by index
or remember positions in `UiPrefs`.

**B-L11. MASTER panel is mostly empty at 1200×800.** `[shot 01]` The floor
is 200pt (`shell.rs:593-596`) for ~100pt of content (dim row, chain-name row,
strip). Fix: floor at 120pt with the strip scrolling, or drop the floor once
the crossfader strip's own panel absorbs the previews.

**B-L12. LASERS keeps the Surfaces list; LIGHTING swaps it.** `[shots 03,
04]` `ui/mod.rs:4303-4308`. Fix: a lasers list (decks) or an empty list with
the placement controls.

**B-L13. Two enum params in the inspector have no label.** `[shot 19]`
`draw_param`'s `ParamType::Enum` arm (`ui/mod.rs:1483-1496`) draws a bare
`ComboBox`. Fix: `ui.horizontal` with `short_param_name` like the Bool arm.

**B-L14. Focus mode and fullscreen are fine.** `[shots 21, 26]` Rows fit,
names show, the crossfader centres. Nothing to fix at 1728pt beyond B-L4.

---

## Part C — backend performance (review only)

Ranked by expected gain per unit of risk. Nothing here was measured; the
estimates say so. Items marked *closed* were checked against the current code
and need no work.

**C1. The web-API snapshot is built and serialized every frame, with no
client (should-do, est. 0.2–1 ms/frame CPU, unmeasured).**
`lib.rs:2877-2888` runs `build_kovvboj_snapshot` + `serde_json::to_value`
every `prepare()` when the `api` feature is on — which the release build is
(`--all-features`). `registry_to_library` (`lib.rs:4633-4670`) clones every
library entry's `id`, `name`, `path`, `kind` string each frame; the user's
library has ~360 shaders plus fonts and media, so that is >1000 `String`
allocations and a `serde_json::Value` tree per frame on the render thread,
whether or not the web server is running. *Fix*: build the library half once
per `library_generation` and the channel half only when `params_dirty` or a
param base changed (the `param_bases_cache` compare at `lib.rs:2956` already
detects that), or skip entirely while `!engine.web_enabled`. Risk low.
Size S. Measure: `--profile profiling`, `sample` on the render thread with
the default set, before/after; fps unchanged is the pass criterion.

**C2. Identity warp still costs a full-resolution pass per projector per
frame (should-do, est. 0.1–0.3 ms GPU at 1080p, ~1 ms at 4K, per projector,
unmeasured).**
`KovvbojWarpStage` has no `is_active` (`stage/mod.rs:1209-1264`), so the
engine's stage loop (`crates/rustjay-engine/src/app/projection.rs:215-263`)
runs it even for an identity corner-pin. Dome and edge-blend already opt out
(`stage/mod.rs:1471-1474`, `1520-1522`). *Fix*: `is_active()` = `!mode.is_identity()`
(the accessor exists, `stage/mod.rs:1706`), keeping `KovvbojSourceStage`
active so something still copies the input to the surface (the engine presents
an untouched surface when `active_count == 0`, `projection.rs:216-220`).
Risk low: the engine already feeds the previous stage's output straight
through. Size S.

**C3. Lighting samplers run for disabled outputs and rebuild views and bind
groups every frame (should-do, est. one render pass + readback per disabled
output per frame, plus per-frame `TextureView`/bind-group creation for
channel-sourced segments; unmeasured).**
`prepare()` adds/updates a pixel sampler for every lighting output before
checking `want` (`lib.rs:4063-4077`), and the projection subsystem renders
every sampler each frame (`projection.rs:946-948`) — the user's own stage has
one disabled sACN output, so this is live for them. For segments that source
a channel, `lib.rs:4083-4106` wraps a *fresh* `Arc::new(create_view(..))`
each frame; `set_tile_sources` compares `Arc::as_ptr` (`crates/rustjay-projection/src/sample_stage.rs:142-159`),
so the pointer never matches and the tile's bind group is rebuilt every frame.
`output_atlas_layout` (`lib.rs:1190-1205`) also allocates a layout per output
per frame only for `set_layout` to discard it as equal. *Fix*: skip
add/update/tile-sources when `!want` and remove the sampler; cache the channel
view keyed on `channel_texture().generation` exactly as `sync_surface_source`
does (`lib.rs:2321-2336`). Risk low. Size S.

**C4. A shader-folder event re-reads the whole library on the main thread
(should-do, est. tens of ms hitch per event with ~360 shaders, unmeasured).**
Any watcher event bumps `library_generation` (`lib.rs:2788-2790`). The next
UI frame clears `EffectsTab.previs_keys` and re-reads and re-hashes every
shader (`ui/mod.rs:3069-3100`), and the next time the transition picker opens
`transition_shaders` re-reads every shader not named `transition_*`
(`lib.rs:35-45`, via `transitions()` `785-795`). A hot-reload save while
performing therefore costs two full library reads. *Fix*: key the memos by
path+mtime and invalidate only the event's paths; move the transition-shape
check into the registry scan so it is done once. Risk low. Size S–M.

**C5. Device discovery on the render thread (A8).** 500 ms NDI timeout plus
Syphon discovery inside `rescan_library()`, called from `prepare()` on file
events. Gain: removes a ≥0.5 s output stall. Size M.

**C6. The parked-away deck renders at full cost (design question, est. up
to ~half the layer GPU cost when the fader is parked, unmeasured).**
Both decks render every frame by design (`KOVVBOJ_DECKS.md` "Budget";
`crates/rustjay-mixer/src/lib.rs:1523-1536`), and the parked case is the
common one. The measured GPU-bound set (five ISF layers at 18 fps) would gain
most from rendering the hidden deck at a reduced rate (every 2nd/3rd frame)
while its clocks keep advancing — ISF `TIMEDELTA`/`PHASE_TIME` accumulate per
render call, so the skipped frames' `dt` has to be folded into the next
render or generators slow down. Risk medium (preview stutter, clock drift if
`dt` is not carried). Size M. This re-opens a settled decision, so it is a
question for the user, not a recommendation.

**C7. `BlitPipeline::blit` creates a bind group per call (polish, est.
~30 µs per call, ~0.1–0.3 ms/frame).**
`crates/rustjay-mixer/src/blit.rs:359` and `:408`. Callers per frame: the
master final blit (`mixer lib.rs:1851-1858`), one per rendered group
(`1611-1618` — two decks minimum), the thumbnails every third frame
(`thumbs.rs:151`, already measured at ~30 µs each) and any deck pop-out. Cache
by (source view id, destination view id) as `CompositePipeline` does with its
allocation-id key. Risk low. Size S–M.

**C8. Small per-frame allocations in `Mixer::render_to` (polish).**
`raw_opacities` Vec (`mixer lib.rs:1486`), `active` Vec (`1642-1647`),
`group_anchor` HashMap (`1658`), `group_render_order` Vec<String>
(`882-890`) and `group_items` per group (`927-946`); kovvboj's side clones
`audio_routing` (`lib.rs:2954`) and `fixture_profiles` (`lib.rs:4054`) each
frame. With ≤16 layers and ≤8 groups this is microseconds; only worth a
`SmallVec`/scratch-buffer pass if a profile shows `RawVec::reserve`.

**Closed leads (verified, no work):**
- NDI receive per-frame allocation: the GPU staging ring is the primary path
  and the CPU fallback reuses a recycled `Vec` via `spare_rx` (`crates/rustjay-io/src/input/ndi.rs:325-330`,
  `strip_stride` `:530-551` clears and extends, no zeroing). Covered by the
  staging PRs.
- Zero-opacity and muted layers: skipped before their source renders
  (`mixer lib.rs:1490-1493`); groups release their four textures when
  inaudible (`1530-1536`).
- Hidden main output: the renderer does not acquire or present the hidden
  window's surface (`crates/rustjay-render/src/renderer.rs:667`), so the
  1440p "Output Resolution" costs nothing while hidden.
- Composite bind-group cache: keyed by generation plus the source
  allocation id (the `ae651c1` fix), and `apply_topology` bumps the
  generation unconditionally (`lib.rs:2177-2182`).
- Thumbnails, text layers, UI drawing: measured not hot previously; nothing
  in the current code contradicts that. The per-frame `bands` map in the
  library (`ui/mod.rs:3073-3100`, one hash lookup and a possible error-string
  clone per shader at the 30 fps UI rate) is the only new per-frame UI cost
  and is small.
- hap-wgpu prefetch redesign and Syphon reconnect spinning are engine/crate
  items outside kovvboj; unchanged, not re-proposed.

**Measurement plan (if any of C1–C4 is taken):** build with
`--profile profiling`; run the default set parked at A for 30 s windows of
`ps` on `pgrep -x kovvboj`, with `RUST_LOG=info,fps=debug` for the median
fps; A/B C1 by toggling the `api` feature, C2 by forcing `is_active` true,
C3 by removing the disabled lighting output from `stage.json`. For C6, the
five-ISF set from the 2026-09-08 profile is the benchmark, fps first.

---

Test result: `cargo test -p kovvboj --features projection` — 131 unit tests
pass; the `ui_kittest` suite has 8 passing and 2 failing, and the two failures
are exactly the known macOS-only snapshot mismatches
(`stage_preview_disabled_snapshot` / `stage_edge_blend_preview_snapshot`,
baselines rendered by CI on lavapipe). No new failures; no baselines were
touched.
