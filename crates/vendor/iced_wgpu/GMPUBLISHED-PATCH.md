Opt into BC texture compression when the adapter has it (`src/lib.rs`,
`src/window/compositor.rs`):

```diff
-required_features: wgpu::Features::empty(),
+required_features: wgpu::Features::TEXTURE_COMPRESSION_BC
+    .intersection(adapter.features()),
```


