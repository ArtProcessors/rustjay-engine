# CuePool agent guide

The repository-root `AGENTS.md` still applies. This file adds guidance for the
standalone CuePool workspace under `examples/cuepool`.

## Workspace boundary

- Run Cargo commands from `examples/cuepool`, or pass
  `--manifest-path examples/cuepool/Cargo.toml` from the repository root. Do
  not invoke the repository-root workspace and expect it to include CuePool.
- Keep CuePool dependencies in this workspace's `Cargo.toml`.
- The intentional dependency across the nested-workspace boundary is
  `rustjay-lighting`; do not introduce another engine-crate dependency without
  checking that the crate is safe to use across that boundary.

## Crate ownership

- `cuepool`: executable, event loop, and application orchestration.
- `cuepool-core`: cue domain models, project serialization, and migrations.
- `cuepool-audio`: decoding, real-time playback, and DSP.
- `cuepool-video`: video decoding and presentation pipeline.
- `cuepool-gui`: egui/wgpu interface components.
- `cuepool-protocols`: OSC, MSC, MIDI, Art-Net, and sACN integrations.
- `cuepool-harness`: shared test and diagnostic support.

Put behavior in the narrowest owning crate. Keep the executable focused on
wiring and orchestration, and avoid coupling domain models to GUI or device
backends.

## Runtime constraints

- Do not block, allocate, log, or perform file/network I/O on real-time audio
  callbacks.
- Keep decoding and device I/O off the UI thread.
- Preserve project-file compatibility. When persisted structures change, add
  an explicit migration or a backward-compatible serde default and test it.
- Treat media paths, OSC/MIDI input, and project files as untrusted input; fail
  with context rather than panicking.

## Validation

Use the smallest check that covers the change, then widen it when practical:

```sh
cargo fmt --all -- --check
cargo test -p <affected-crate>
cargo check -p <affected-crate>
```

For workspace-wide or release-facing changes, also run:

```sh
cargo test --workspace
cargo check --workspace
```

Hardware-dependent audio, video, MIDI, and lighting behavior needs a short
manual verification note when automated coverage cannot exercise it.

On Windows, building CuePool from source requires FFmpeg development headers
and import libraries discoverable through vcpkg or `pkg-config`. Runtime DLLs
alone are insufficient, and a vcpkg checkout must also have the FFmpeg port
installed for the target triplet.
