fn view_fog_factor(position:vec3<f32>, projected_depth:f32) -> f32 {
    if uniforms.fog.end <= uniforms.fog.start || uniforms.fog.mode==FOG_DISABLED { return 0.0; }
    if uniforms.fog.mode==FOG_RADIAL {
        return clamp((distance(position,uniforms.camera_position.xyz)-uniforms.fog.start)
            /(uniforms.fog.end-uniforms.fog.start),0.0,uniforms.fog.max_density);
    }
    if uniforms.fog.mode==FOG_HEIGHT {
        let point_distance=dot(vec4<f32>(position,1.0),uniforms.medium_plane);
        let eye_distance=dot(vec4<f32>(uniforms.camera_position.xyz,1.0),uniforms.medium_plane);
        let fraction=clamp(-point_distance/max(eye_distance-point_distance,0.00001),0.0,1.0);
        return clamp(fraction*projected_depth/(uniforms.fog.end-uniforms.fog.start),0.0,uniforms.fog.max_density);
    }
    return clamp((projected_depth-uniforms.fog.start)
        /(uniforms.fog.end-uniforms.fog.start),0.0,uniforms.fog.max_density);
}

fn view_fog_color_factor(position:vec3<f32>,projected_depth:f32) -> f32 {
    let factor = view_fog_factor(position,projected_depth);
    // Source range-fog opacity is squared; height-fog depth remains linear.
    let height=uniforms.fog.mode==FOG_HEIGHT;
    let opacity = select(factor*factor, factor, height);
    return opacity*select(1.0,uniforms.fog_color.a,height || uniforms.fog.mode==FOG_IMMERSION);
}
