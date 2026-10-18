struct Params {
    matrix: mat4x4<f32>,
    positions: array<vec4<f32>,8>,
    colors: array<vec4<f32>,2>,
    world_rect:vec4<f32>,
    sky_rect:vec4<f32>,
    query:vec4<f32>,
    flags:vec4<u32>,
}
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(1) var<storage,read_write> state:array<f32,4>;
@group(0) @binding(2) var depth:texture_depth_multisampled_2d;
var<workgroup> visible:array<u32,64>;
var<workgroup> possible:array<u32,64>;

fn query_visibility(sky:bool,lane:u32) {
    let rect=select(params.world_rect,params.sky_rect,sky);
    let dims=textureDimensions(depth);
    let low=vec2<i32>(max(floor(rect.xy),vec2<f32>(0.0)));
    let high=vec2<i32>(min(ceil(rect.zw),vec2<f32>(dims)));
    let extent=max(high-low,vec2<i32>(0));
    let count=u32(extent.x*extent.y);
    let threshold=select(params.query.x,params.query.y,sky);
    var passing=0u;
    var total=0u;
    for(var index=lane;index<count;index+=64u) {
        let xy=low+vec2<i32>(i32(index)%max(extent.x,1),i32(index)/max(extent.x,1));
        let center=vec2<f32>(xy)+vec2<f32>(0.5);
        if all(center>=rect.xy) && all(center<rect.zw) {
            for(var sample=0u;sample<textureNumSamples(depth);sample++) {
                passing+=u32(textureLoad(depth,xy,i32(sample))<=threshold);
                total++;
            }
        }
    }
    visible[lane]=passing;
    possible[lane]=total;
    workgroupBarrier();
    for(var stride=32u;stride>0u;stride/=2u) {
        if lane<stride {
            visible[lane]+=visible[lane+stride];
            possible[lane]+=possible[lane+stride];
        }
        workgroupBarrier();
    }
    if lane==0u {
        let index=select(1u,0u,sky);
        var requested=0.0;
        if possible[0]>0u {requested=f32(visible[0])/f32(possible[0]);}
        if requested>=0.95 {requested=1.0;}
        let full_extent=max(rect.zw-rect.xy,vec2<f32>(0.0));
        let clipped_extent=max(min(rect.zw,vec2<f32>(dims))-max(rect.xy,vec2<f32>(0.0)),vec2<f32>(0.0));
        let area=full_extent.x*full_extent.y;
        var clip_fraction=0.0;
        if area>0.0 {clip_fraction=clamp(clipped_extent.x*clipped_extent.y/area,0.0,1.0);}
        var brightness=clamp(requested,state[index]-params.query.z,state[index]+params.query.z);
        if params.query.w>0.5 {brightness=requested;}
        state[index]=brightness;
        state[index+2u]=brightness*clip_fraction;
        if !sky && params.flags.z==0u {state[2]=1.0;}
    }
}

@compute @workgroup_size(64)
fn query_world(@builtin(local_invocation_index) lane:u32) {query_visibility(false,lane);}
@compute @workgroup_size(64)
fn query_sky(@builtin(local_invocation_index) lane:u32) {query_visibility(true,lane);}
