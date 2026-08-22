# Local patch: merge duplicate EGL `SURFACE_TYPE` attributes

**One file differs from the published crate: `src/gles/egl.rs`.**
Everything else is pristine `wgpu-hal 30.0.0`.

## What and why

`choose_config` builds an EGL attribute list by concatenating the attribute
arrays of each tier:

```rust
for &(_, tier_attr) in tiers[..=tier_max].iter() {
    attributes.extend_from_slice(tier_attr);   // upstream
}
```

Two of those tiers ("off-screen" and "presentation") each contain a
`SURFACE_TYPE` key. From `tier_max >= 1` the concatenated list therefore holds
`SURFACE_TYPE` twice. A repeated key in an EGL attribute list is undefined
behaviour, and Mesa's response is to return **no configs at all** — so surface
creation fails outright on vc4 / Raspberry Pi.

The patch splits `SURFACE_TYPE` out, ORs the bits from every tier into a single
entry, and appends the remaining attributes unchanged.

## Status upstream

**Still present in 30.0.0** — verified by diffing the pristine crate. The
concatenation loop is byte-identical to 29.0.3, and the tier table still carries
two `SURFACE_TYPE` keys. Re-check on every wgpu bump; drop this vendor entry the
moment upstream dedupes the list.

## Reapplying after a version bump

1. Copy the new `wgpu-hal-<version>` from `~/.cargo/registry/src/*/` over this
   directory, then `chmod -R u+w` and delete `.cargo-ok`.
2. Reapply the block above in `choose_config` (`src/gles/egl.rs`).
3. Keep `[patch.crates-io] wgpu-hal = { path = "vendor/wgpu-hal" }` in the
   workspace `Cargo.toml`.

Origin: `bf9ff81 feat(pi2): cross-compile and run sputnik on Raspberry Pi 2`.
