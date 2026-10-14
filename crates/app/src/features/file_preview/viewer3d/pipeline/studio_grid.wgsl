struct StudioGrid {
    view_proj: mat4x4<f32>,
    origin_spacing: vec4<f32>,
    extent: vec4<f32>,
}
@group(0) @binding(0) var<uniform> grid: StudioGrid;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world: vec3<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0)
    );
    let world = vec3<f32>(grid.origin_spacing.xy + corners[index] * grid.extent.x, grid.origin_spacing.z);
    var out: VertexOutput;
    out.position = grid.view_proj * vec4<f32>(world, 1.0);
    out.world = world;
    return out;
}

fn lines(position: vec2<f32>, spacing: f32) -> f32 {
    let cells = position / spacing;
    let width = max(fwidth(cells), vec2<f32>(0.0001));
    let distance = abs(fract(cells - 0.5) - 0.5) / width;
    let line = 1.0 - min(min(distance.x, distance.y), 1.0);
    return line * (1.0 - smoothstep(0.3, 1.5, max(width.x, width.y)));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let minor = lines(input.world.xy, grid.origin_spacing.w);
    let major = lines(input.world.xy, grid.origin_spacing.w * 4.0);
    let distance = length(input.world.xy - grid.origin_spacing.xy);
    let fade = 1.0 - smoothstep(grid.extent.y * 0.2, grid.extent.y, distance);
    let background = vec3<f32>(0.035, 0.044, 0.056);
    let ground = vec3<f32>(0.055, 0.066, 0.080);
    let color = ground + vec3<f32>(minor * 0.014 + major * 0.019);
    return vec4<f32>(mix(background, color, fade), 1.0);
}
