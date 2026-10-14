# Local vformats 0.1.0 patch

The published detail-prop decoder assumes 48/44-byte records. Valve's version-4 `DetailObjectLump_t` is 52 bytes, including `m_flScale` at offset 48. Reading consecutive SDK records at the wrong stride corrupts subsequent positions/type/leaf data and loses sprite scale.

This patch recognizes the 52-byte layout, retains the existing legacy layouts, and exposes scale (one for legacy records). Renderer scene data carries the decoded orientation and scale separately from composition. The regression uses multiple records with distinct origins, orientations and scales to catch stride errors.

Reference: https://github.com/ValveSoftware/source-sdk-2013/blob/master/src/public/gamebspfile.h

The remaining package source is the published 0.1.0 code. Keep this patch local until an upstream release contains the correction; both original licenses are included.
