Opt into BC texture compression when the adapter has it (`src/lib.rs`,
`src/window/compositor.rs`):

```diff
-required_features: wgpu::Features::empty(),
+required_features: wgpu::Features::TEXTURE_COMPRESSION_BC
+    .intersection(adapter.features()),
```

Request 4 bind groups instead of upstream's 2 (`src/lib.rs`,
`src/window/compositor.rs`). The file-preview water shader binds group 2
(screen-space refraction), and the viewer3d pipelines share iced's device, so
shaders must fit within the limits requested here. 4 is the wgpu default and
downlevel baseline, so this does not reduce adapter compatibility. Guarded by
`viewer_shaders_fit_within_requested_device_limits` in
`crates/app/src/features/file_preview/viewer3d/pipeline.rs`.

```diff
-max_bind_groups: 2,
+max_bind_groups: 4,
```
