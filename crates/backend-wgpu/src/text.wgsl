struct Globals {
    size: vec2<f32>,
    // Uniform buffers are 16-byte multiples (WebGL2 insists on it).
    padding: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct Instance {
    @location(0) rect: vec4<f32>, // x, y, w, h in pixels
    @location(1) uv: vec4<f32>,   // u0, v0, u1, v1
    @location(2) color: vec4<f32>,
}

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32, inst: Instance) -> Out {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let corner = corners[index];
    let pixel = inst.rect.xy + corner * inst.rect.zw;
    let ndc = vec2(pixel.x / globals.size.x * 2.0 - 1.0, 1.0 - pixel.y / globals.size.y * 2.0);
    var out: Out;
    out.position = vec4(ndc, 0.0, 1.0);
    out.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    out.color = inst.color;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let coverage = textureSample(atlas, atlas_sampler, in.uv).r;
    return vec4(in.color.rgb, in.color.a * coverage);
}
