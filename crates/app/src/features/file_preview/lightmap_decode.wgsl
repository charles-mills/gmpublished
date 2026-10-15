// LDR lightmap bytes store half-strength gamma-encoded radiance.
fn lightmap_scale() -> f32 { return select(4.5947934,2.0,uniforms.lightmap_mode==LIGHTMAP_HDR_FALLBACK); }
