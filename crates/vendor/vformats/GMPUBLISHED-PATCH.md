# Local vformats 0.1.0 patch

The published detail-prop decoder assumes 48/44-byte records. Valve's version-4 `DetailObjectLump_t` is 52 bytes, including `m_flScale` at offset 48. Reading consecutive SDK records at the wrong stride corrupts subsequent positions/type/leaf data and loses sprite scale.

This patch recognizes the 52-byte layout, retains the existing legacy layouts, and exposes scale (one for legacy records). Renderer scene data carries the decoded orientation and scale separately from composition. The regression uses multiple records with distinct origins, orientations and scales to catch stride errors.

Reference: https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/gamebspfile.h

The remaining package source is the published 0.1.0 code. Keep this patch local until an upstream release contains the correction; both original licenses are included.


The renderer also needs the SDK's 12-byte `dleafwaterdata_t` records (lump 36). The added typed accessor preserves surface/minimum height and the signed surface texinfo ID, using the existing bounded/decompressed record reader. A two-record regression covers stride and negative material IDs. The domain layer retains leaf-to-volume association; material resolution and fog remain outside this wire-format crate.

The BSP adapter also exposes typed HDR faces (lump 58), sharing the normal face decoder so HDR light offsets are selected with the corresponding lighting lump. Regression coverage uses different LDR/HDR offsets and style lists.

Compiled static-prop lighting is exposed through a bounded, borrowed VHV v2 reader in `mdl`. It preserves BGRA stream bytes and checks counts, offsets, overlap, version and vertex format. Model assembly retains the MDL checksum, all-LOD hardware strip-group layout and each rendered vertex's original hardware lighting identity. Vertices from different strip groups or different hardware pool slots remain distinct even when they reference the same VVD vertex. VHV/model validation rejects incompatible companions, checksums, layouts and correspondence instead of attaching stale lighting to best-effort geometry. Regressions cover multiple model choices/LODs, unused and duplicated hardware vertices, mismatched companions and truncated/malformed VHV files.

Format references: https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/materialsystem/hardwareverts.h and https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/utils/vrad/vradstaticprops.cpp

BSP cubemap samples (lump 42) have a bounded typed accessor preserving original integer positions, record order and authored size. Texture filenames derive from those exact integers, so scene projection and floating-point transforms must not rewrite them. Partial records and excessive counts are rejected; three padding bytes do not affect decoded state. Dedicated regressions cover signed/extreme coordinates, ordering, padding, truncation and count limits. Spatial probe selection remains a renderer/native-engine contract rather than a wire-format heuristic.

Reference: https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/bspfile.h

BSP surface-frame data now includes compiled vertex normals/indices (lumps 30/31), displacement edge/corner neighbor tables and allowed-vertex bits. Tests cover exact record offsets, signed normal components and unsigned index identity. Model assembly preserves authored VVD tangents and handedness through the same bone blend as positions/normals, then reconstructs the binormal. Bone contributions retain their linear vector lengths and authored weight semantics; missing tangents are counted explicitly. These data/assembly changes are separate from native renderer acceptance of displacement smoothing and brush frame generation.

Frame references: https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/builddisp.cpp and https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/materialsystem/stdshaders/common_vs_fxc.h

Model assembly retains authored tangent W separately from its reconstructed binormal, including degenerate frames, because PS2B/PS3 model shaders reconstruct that binormal after interpolation. The studio header's authored illumination position is preserved in parsed and assembled model data without substituting calculated bounds or choosing a probe-selection policy. Regressions cover signed header coordinates and handedness that cannot be recovered from a zero binormal.

The compiled world-light reader preserves both LDR/HDR 88-byte `dworldlight_t` records, including source order, emitter type, linear intensity, cone/distance parameters, styles, flags and ownership metadata. It bounds raw/decompressed bytes and record count, rejects partial/unknown-layout/nonfinite payloads with typed diagnostics, and retains unknown emitter IDs without selecting or reinterpreting them. Wire layout is documented in Valve's [bspfile.h](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/bspfile.h); `tests/bsp_world_lights.rs` exercises all fields/types, malformed boundaries and every float slot.

Model assembly exposes an explicit vertex-compression option, defaulting to the original uncompressed path. Its Source UBYTE4 implementation follows `PackNormal_UBYTE4` and the paired shader decompression before bone blending, including negative tangent-W packing. Assembled data separately retains original VVD normals/tangents, so hardware-path experiments cannot erase asset identity. Axis, octant, handedness, invalid-input and pre-skinning ordering regressions cover this option. Native renderer acceptance belongs to the app's recorded controls rather than this format API.

Modern v48/v49 MDLs preserve the optional `studiohdr2_t::linearboneindex` table as nine typed arrays alongside full bone records. Bounds, counts, relative offsets and header overlap are validated before reading. Runtime bone resolution follows Source's linear-table precedence for positions, quaternions, animation scales and pose-to-bone matrices; animation parsing and geometry assembly use that same resolution. The raw full table remains accessible for diagnostics. Tests deliberately disagree between duplicate tables, verify transformed assembly and reject malformed optional data without panicking. Older accepted MDL versions retain their original path.

References: Valve [studio.h](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/studio.h), [bone_setup.cpp](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/bone_setup.cpp), [mathlib.h](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/mathlib/mathlib.h) and [common_vs_fxc.h](https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/materialsystem/stdshaders/common_vs_fxc.h).
