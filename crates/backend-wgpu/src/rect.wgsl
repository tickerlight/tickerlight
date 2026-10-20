struct Globals {
    size: vec2<f32>,
    // Uniform buffers are 16-byte multiples (WebGL2 insists on it).
    padding: vec2<f32>,
}

@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    @location(0) rect: vec4<f32>, // x, y, w, h in pixels
    @location(1) color: vec4<f32>,
    @location(2) radius: f32,
    @location(3) border: f32,
    @location(4) to: vec4<f32>,
    @location(5) feather: f32,
    @location(6) direction: u32, // 0 down, 1 right
}

struct Out {
    @builtin(position) position: vec4<f32>,
    // Premultiplied: a gradient to a transparent color fades without darkening.
    @location(0) color: vec4<f32>,
    // Position inside the rectangle, measured from its center, and its half size.
    @location(1) local: vec2<f32>,
    @location(2) half_size: vec2<f32>,
    @location(3) radius: f32,
    @location(4) border: f32,
    @location(5) feather: f32,
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
    // The gradient runs along one side of the quad, so it is interpolated between the vertices.
    let t = select(corner.y, corner.x, inst.direction == 1u);
    let start = vec4(inst.color.rgb * inst.color.a, inst.color.a);
    let end = vec4(inst.to.rgb * inst.to.a, inst.to.a);
    var out: Out;
    out.position = vec4(ndc, 0.0, 1.0);
    out.color = mix(start, end, t);
    out.half_size = inst.rect.zw * 0.5;
    out.local = (corner - vec2(0.5)) * inst.rect.zw;
    out.radius = inst.radius;
    out.border = inst.border;
    out.feather = inst.feather;
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    if in.radius <= 0.0 && in.border <= 0.0 && in.feather <= 0.0 {
        return in.color;
    }
    // Distance to the edge of a rounded box: negative inside. Without a feather straight edges stay crisp
    // because fragments only exist inside the quad, so only the corners get a soft edge.
    let q = abs(in.local) - (in.half_size - vec2(in.radius));
    let d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - in.radius;
    var coverage = clamp(0.5 - d, 0.0, 1.0);
    if in.feather > 0.0 {
        let c = clamp(-d / in.feather, 0.0, 1.0);
        coverage = c * c * (3.0 - 2.0 * c);
    }
    if in.border > 0.0 {
        // An outline: also cut away what is deeper inside than its width.
        coverage = coverage * clamp(0.5 + d + in.border, 0.0, 1.0);
    }
    return in.color * coverage;
}
