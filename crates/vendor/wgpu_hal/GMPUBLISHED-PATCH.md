# Vulkan loader lifecycle

Pinned upstream: wgpu-hal 27.0.4. The sampler capability changes are described in
[`../WGPU_SAMPLER_PATCH.md`](../WGPU_SAMPLER_PATCH.md).

Vulkan instance initialization, adapter discovery, instance destruction, and loader
library unloading share a process-wide reentrant mutex. NVIDIA's ICD negotiation
can publish its initialization flag before its dispatch pointers are populated;
the Vulkan loader can enter this negotiation concurrently through extension
enumeration and instance creation. Synchronizing those entry paths prevents a
caller from jumping through an uninitialized driver function pointer. Adapter
discovery and teardown also participate because they can unload driver libraries.

The owned entry wrapper keeps library destruction inside the same synchronization
boundary, including failed initialization and unwinding. Custom instance ownership
callbacks run before unloading the entry. Creation callbacks run outside the gate
so they can wait for work on other threads; reentrancy permits ownership callbacks
to initialize or release another Vulkan instance on the same thread. Device
creation, command submission, and rendering retain their existing concurrency.
