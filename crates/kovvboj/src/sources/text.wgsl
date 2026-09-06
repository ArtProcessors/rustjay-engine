// Text: one instanced quad per glyph, sampled out of a font atlas.
//
// The whole layout is in em units — the atlas decides what an em is — and the
// vertex stage maps it to the screen. Per-glyph animation happens here too,
// keyed off each glyph's ordinal, so it costs nothing per frame on the CPU and
// the quads only get rebuilt when the words change.

const TAU: f32 = 6.2831853;

struct U {
    color: vec4<f32>,
    resolution: vec2<f32>,
    center: vec2<f32>,
    // Size of the text block, in em units.
    block: vec2<f32>,
    // Block height as a fraction of the target's height.
    scale: f32,
    angle: f32,
    // Animation clock, in cycles.
    time: f32,
    stagger: f32,
    wave: f32,
    spin: f32,
    explode: f32,
    // 1.0 when the atlas carries its own colour rather than coverage.
    colour_atlas: f32,
    // Whole-block rotation in 3D: turn about its vertical axis, tilt about its
    // horizontal one. Radians, with the animated part already folded in.
    turn: f32,
    tilt: f32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(
    @location(0) corner: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) index: f32,
) -> VertexOut {
    var out: VertexOut;
    out.uv = mix(uv.xy, uv.zw, corner);

    let em = rect.xy + corner * rect.zw;
    var glyph = rect.xy + rect.zw * 0.5;
    var local = em - glyph;
    let block_centre = u.block * 0.5;

    // Each glyph runs the same animation, later than the one before it.
    let phase = (u.time + index * u.stagger) * TAU;

    glyph.y = glyph.y + sin(phase) * u.wave;

    // Pushed out from the middle of the block, so a word bursts apart.
    let out_dir = normalize(glyph - block_centre + vec2<f32>(1e-5, 1e-5));
    glyph = glyph + out_dir * u.explode * (0.5 + 0.5 * sin(phase));

    // Spin about the glyph's own vertical axis. The perspective divide on the
    // rotated depth is what makes it read as a letter turning rather than a
    // letter being squashed.
    let a = phase * u.spin;
    let x = local.x * cos(a);
    let z = local.x * sin(a);
    local = vec2<f32>(x, local.y) / (1.0 + z * 0.6);

    // Em units, centred on the block. Deliberately *not* divided by the
    // block's height: sizing by the block made Size behave like a zoom that
    // fought the line count and the line height — two lines, or looser
    // leading, and the glyphs shrank to keep the block the same height. Size
    // is the size of an em, and leading is leading.
    let b = (glyph + local) - block_centre;

    // Turn and tilt the whole string in 3D. The perspective divide is what
    // makes it a card turning rather than a card being squashed, and z is
    // measured against the block's half-width so a long string foreshortens
    // like a short one instead of flying through the camera.
    var v = vec3<f32>(b, 0.0);
    let ct = cos(u.tilt);
    let st = sin(u.tilt);
    v = vec3<f32>(v.x, v.y * ct - v.z * st, v.y * st + v.z * ct);
    let cy = cos(u.turn);
    let sy = sin(u.turn);
    v = vec3<f32>(v.x * cy + v.z * sy, v.y, v.z * cy - v.x * sy);
    let half_w = max(u.block.x * 0.5, 0.5);
    let persp = 1.0 / max(1.0 + v.z / half_w * 0.5, 0.15);

    // Square screen space, where the flat rotation cannot shear.
    var p = v.xy * persp * u.scale;
    let ca = cos(u.angle);
    let sa = sin(u.angle);
    p = vec2<f32>(p.x * ca - p.y * sa, p.x * sa + p.y * ca);

    let aspect = u.resolution.x / max(u.resolution.y, 1.0);
    let screen = u.center + vec2<f32>(p.x / aspect, p.y);
    out.position = vec4<f32>(screen.x * 2.0 - 1.0, 1.0 - screen.y * 2.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas, atlas_sampler, in.uv);
    if (u.colour_atlas > 0.5) {
        return vec4<f32>(texel.rgb * u.color.rgb, texel.a * u.color.a);
    }
    // A rasterised font is coverage only, in red; the colour is the tint.
    return vec4<f32>(u.color.rgb, u.color.a * texel.r);
}
