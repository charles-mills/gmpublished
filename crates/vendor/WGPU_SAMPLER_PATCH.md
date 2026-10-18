# Native anisotropic sampler capabilities

Pinned crates: wgpu-types 27.0.1, wgpu-core 27.0.3, wgpu-hal 27.0.4. Each directory retains its MIT and Apache 2.0 licenses.

`INDEPENDENT_ANISOTROPIC_MIP_FILTER` permits nearest mip filtering with linear minification/magnification and anisotropy above 1. Devices must enable the advertised feature. Without it, standard sampler validation is unchanged. `NATIVE_ANISOTROPIC_POINT_FILTERING` independently permits nearest minification/magnification with anisotropy above 1. Nearest mip filtering still requires the mip capability. An anisotropic sampler is classified as filtering even when all three filters are nearest, preserving unfilterable-texture and nonfiltering-binding validation.

Vulkan advertises the feature only with hardware sampler anisotropy. OpenGL uses its existing extension/maximum-anisotropy capability. HAL sampler construction remains unchanged. Metal, DX12 and WebGPU do not advertise this capability.

These capabilities preserve native sampler descriptor expressibility. They do not promise identical anisotropic sampling across drivers or APIs. Vulkan and GL may choose different anisotropic sampling footprints and mip levels.
