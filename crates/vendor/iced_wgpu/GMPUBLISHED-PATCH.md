Opt into BC texture compression when the adapter has it (`src/lib.rs`,
`src/window/compositor.rs`):

```diff
-required_features: wgpu::Features::empty(),
+required_features: wgpu::Features::TEXTURE_COMPRESSION_BC
+    .intersection(adapter.features()),
```

Headless initialization also logs the selected adapter, render target format,
and enabled device features so fixed-camera map captures identify the graphics
hardware and rendering path used.


