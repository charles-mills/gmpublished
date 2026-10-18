Request supported BC texture compression and primitive clip distances (`src/lib.rs`,
`src/window/compositor.rs`):

```diff
-required_features: wgpu::Features::empty(),
+required_features: (wgpu::Features::TEXTURE_COMPRESSION_BC
+    | wgpu::Features::CLIP_DISTANCES).intersection(adapter.features()),
```

Custom primitive pipelines receive the rendering adapter's downlevel capabilities
through `Pipeline::new_with_capabilities`. Existing pipelines retain their `new`
implementation by default. The renderer uses this contract to share compatible
linear/sRGB image views only on adapters supporting `VIEW_FORMATS`.

Viewer shaders select primitive clipping from the enabled device feature. Devices
without clip distances retain fragment clipping.

Both device initializers request three bind groups for independent viewer,
material and lightmap bindings. This remains within WGPU default limits.
