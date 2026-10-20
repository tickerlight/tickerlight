struct Globals {
    size: vec2<f32>,
    // Uniform buffers are 16-byte multiples (WebGL2 insists on it).
    padding: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    @location(0) points: vec4<f32>, // x0, y0, x1, y1 in pixels
    @location(1) width: f32,
    @location(2) color: vec4<f32>,
}

struct Out {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Distance from the line's center across it, in pixels.
    @location(1) across: f32,
    @location(2) half_width: f32,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32, inst: Instance) -> Out {
    // (position along the segment 0..1, side -1..1)
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, -1.0), vec2(1.0, -1.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    let corner = corners[index];
    let a = inst.points.xy;
    let b = inst.points.zw;
    var dir = vec2(1.0, 0.0);
    let len = length(b - a);
    if len > 1e-6 {
        dir = (b - a) / len;
    }
    let normal = vec2(-dir.y, dir.x);
    let half_width = inst.width * 0.5;
    // One extra pixel across for the anti-aliased edge, half a width past both ends so that
    // consecutive segments of a polyline overlap at the joints.
    let across = corner.y * (half_width + 1.0);
    let pixel = mix(a, b, corner.x) + dir * (corner.x * 2.0 - 1.0) * half_width + normal * across;
    let ndc = vec2(pixel.x / globals.size.x * 2.0 - 1.0, 1.0 - pixel.y / globals.size.y * 2.0);
    var out: Out;
    out.position = vec4(ndc, 0.0, 1.0);
    out.color = inst.color;
    out.across = across;
    out.half_width = half_width;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let coverage = clamp(in.half_width + 0.5 - abs(in.across), 0.0, 1.0);
    return vec4(in.color.rgb, in.color.a * coverage);
}
