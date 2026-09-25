//! Shaders that GL drivers (and so VDMX, glslsandbox, the Shadertoy converter)
//! accept, which glslang rejects unless the transpiler smooths them over.

fn transpile(src: &str) -> rustjay_isf::Transpiled {
    let isf = rustjay_isf::header::parse(src).expect("header");
    rustjay_isf::generate_wgsl(&isf, src).expect("transpiles")
}

// Converted multi-buffer Shadertoys paste each buffer's defines into one file.
#[test]
fn a_define_redefined_with_a_new_value_compiles() {
    transpile(
        "/*{ \"INPUTS\": [] }*/\n#define amount 0.2\n#define amount 0.8\n\
         void main() { gl_FragColor = vec4(amount); }",
    );
}

// `_<name>_imgSize` works for IMPORTED images and pass targets, not just INPUTS,
// and is read off the texture rather than a uniform nothing uploads.
#[test]
fn img_size_aux_values_cover_every_texture() {
    let t = transpile(
        "/*{ \"INPUTS\": [ { \"NAME\": \"inputImage\", \"TYPE\": \"image\" } ],\n\
         \"IMPORTED\": { \"noise\": { \"PATH\": \"noise.png\" } },\n\
         \"PASSES\": [ { \"TARGET\": \"bufA\" }, {} ] }*/\n\
         void main() { gl_FragColor = vec4(_inputImage_imgSize + _noise_imgSize, _bufA_imgRect.zw); }",
    );
    assert!(
        !t.manifest.input_fields.iter().any(|f| f.name.contains("_imgSize")),
        "aux values must not be dead uniform fields"
    );
}

// glslsandbox declares its own samplers; each becomes a texture the host binds.
#[test]
fn a_bare_uniform_sampler2d_becomes_a_bound_texture() {
    let t = transpile(
        "/*{ \"INPUTS\": [] }*/\nuniform sampler2D tex0;\n\
         void main() { gl_FragColor = texture2D(tex0, vec2(0.5)); }",
    );
    assert!(t.manifest.textures.iter().any(|b| b.name == "tex0"));
}
