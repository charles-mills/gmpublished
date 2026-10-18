//! VVD（vertex data，`IDSV` v4）读写。
//!
//! ```text
//!  0x00  char[4]   id                "IDSV"
//!  0x04  int32     version           4
//!  0x08  int32     checksum          与 MDL 的 checksum 配对（见下）
//!  0x0C  int32     numLODs
//!  0x10  int32[8]  numLODVertexes    [0] 是**实际存储的顶点总数**（含切线）
//!  0x30  int32     numFixups
//!  0x34  int32     fixupTableStart
//!  0x38  int32     vertexDataStart
//!  0x3C  int32     tangentDataStart
//! ```
//! ```text
//!  +0x00  float[3]  weight        骨骼权重（最多 3 根）
//!  +0x0C  u8[3]     bone          **全局**骨骼下标
//!  +0x0F  u8        boneCount     有效骨骼数 1..3
//!  +0x10  float[3]  position
//!  +0x1C  float[3]  normal
//!  +0x28  float[2]  texCoord      已归一化到 0..1
//! ```
//! ```text
//! fixupTableStart  = ALIGN4(64)                      = 64
//! vertexDataStart  = ALIGN16(fixupTableStart + numFixups * 12)
//! tangentDataStart = ALIGN16(vertexDataStart + numLODVertexes[0] * 48)
//! 文件长度          = tangentDataStart + numLODVertexes[0] * 16   （尾部无填充）
//! ```

/// VVD 头部的字节大小。
pub const HEADER_SIZE: usize = 64;
/// `mstudiovertex_t` 的字节大小。
pub const VERTEX_SIZE: usize = 48;
/// `SourceVector4D` 的字节大小。
pub const TANGENT_SIZE: usize = 16;
/// `vertexFileFixup_t` 的字节大小。
pub const FIXUP_SIZE: usize = 12;
/// 文件魔数（磁盘字节序）。
pub const ID: &[u8; 4] = b"IDSV";
/// 支持的文件版本。
pub const VERSION: i32 = 4;
/// `numLODVertexes` 的槽位数（`MAX_NUM_LODS`）。
pub const MAX_NUM_LODS: usize = 8;

/// 向上对齐到 `a` 的倍数（`a` 必须是 2 的幂）。
#[inline]
pub fn align_up(v: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    (v + a - 1) & !(a - 1)
}

/// 读写 VVD 时的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VvdError {
    /// 文件不足 4 字节，读不出魔数。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The file is only %{len} bytes; cannot read the 4-byte magic",
            len = len
        )
    )]
    TooShortForId { len: usize },
    /// 魔数不是 `IDSV`（`IDCV` 是压缩变体，本实现不支持）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The magic should be IDSV but is %{found}",
            found = format!(
                "{:?}",
                .found.iter().map(|b| if b.is_ascii_graphic() { *b as char } else { '?' }).collect::<String>()
            )
        )
    )]
    BadId { found: [u8; 4] },
    /// 版本不受支持。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The version should be %{expected} but is %{found}",
            expected = VERSION,
            found = found
        )
    )]
    BadVersion { found: i32 },
    /// 顶点 stride 与版本不符（v54..59 是 64 字节）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The vertex stride of MDL version %{mdl_version} is 64 bytes; this implementation only supports 48",
            mdl_version = mdl_version
        )
    )]
    BadVertexStride { mdl_version: i32 },
    /// 头部字段自相矛盾。
    #[error(
        "{}",
        crate::tr_fmt!("Header fields are inconsistent: %{detail}", detail = detail)
    )]
    Inconsistent { detail: String },
    /// 文件长度不足以容纳声明的数据。
    #[error(
        "{}",
        crate::tr_fmt!(
            "%{what} needs %{need} bytes but the file only has %{have}",
            what = crate::tr(what),
            need = need,
            have = have
        )
    )]
    Truncated {
        need: usize,
        have: usize,
        what: &'static str,
    },
}

/// 一个顶点（`mstudiovertex_t`）。
///
/// 权重与骨骼下标是**并排**的两个数组，不是交错的 `(bone, weight)` 对。
/// 用 `f32` 而非 `f16`/归一化整数保存，`from_le_bytes` / `to_le_bytes`
/// 往返不丢位（NaN 的载荷位也保留）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VvdVertex {
    /// 骨骼权重；下标 `boneCount..3` 的部分无意义（studiomdl 常写 0）。
    pub weight: [f32; 3],
    /// **全局**骨骼下标（不是 strip 内的槽位号）。
    pub bone: [u8; 3],
    /// 有效骨骼数，1..=3。
    pub bone_count: u8,
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// 已归一化到 0..1。
    pub tex_coord: [f32; 2],
}

impl VvdVertex {
    fn read(buf: &[u8], off: usize) -> Self {
        let f = |o: usize| f32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        Self {
            weight: [f(off), f(off + 4), f(off + 8)],
            bone: [buf[off + 12], buf[off + 13], buf[off + 14]],
            bone_count: buf[off + 15],
            position: [f(off + 16), f(off + 20), f(off + 24)],
            normal: [f(off + 28), f(off + 32), f(off + 36)],
            tex_coord: [f(off + 40), f(off + 44)],
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        for w in self.weight {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&self.bone);
        out.push(self.bone_count);
        for v in self.position {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.normal {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.tex_coord {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
}

/// 一条切线（`SourceVector4D`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VvdTangent {
    pub xyz: [f32; 3],
    /// 手性符号，±1。
    pub w: f32,
}

/// VVD 头部（`vertexFileHeader_t`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VvdHeader {
    /// 与同模型 `.mdl` 的 `checksum` 配对。**不是内容哈希**，见模块文档。
    pub checksum: i32,
    pub num_lods: i32,
    /// 各 LOD 的顶点数；`[0]` 是实际存储的顶点总数。
    pub num_lod_vertexes: [i32; MAX_NUM_LODS],
    pub num_fixups: i32,
    pub fixup_table_start: i32,
    pub vertex_data_start: i32,
    pub tangent_data_start: i32,
}

impl VvdHeader {
    /// 实际存储的顶点数（顶点块与切线块的共同长度）。
    pub fn vertex_count(&self) -> usize {
        self.num_lod_vertexes[0].max(0) as usize
    }
}

/// 一份完整的 VVD。
///
/// 支持两种形态：
/// - **无 fixup**（`numFixups == 0`）：顶点块按 mesh 顺序，三偏移重合在 64；
/// - **有 fixup**（`numFixups > 0`）：顶点块按 LOD 排序，需要 [`Self::fixups`]
///   才能还原 mesh 顺序。实测 53/3302 个真实模型是这一形态，
///   且**全部是多 LOD 模型**。
#[derive(Debug, Clone, PartialEq)]
pub struct Vvd {
    pub header: VvdHeader,
    /// 按**文件顺序**排列的顶点（有 fixup 时是 LOD 排序，不是 mesh 顺序）。
    pub vertices: Vec<VvdVertex>,
    pub tangents: Vec<VvdTangent>,
    /// fixup 表（`numFixups == 0` 时为空）。
    ///
    /// 语义见 [`crate::lod::Fixup`]。解析时**原样读出**，不做重排 ——
    /// 「往返逐字节相同」是布局判据，重排会让它失去意义。
    pub fixups: Vec<crate::lod::Fixup>,
}

impl Vvd {
    /// 从字节解析。
    ///
    /// 只按头部声明的偏移读取，不做 fixup 重排。任何越界都报错而非 panic。
    pub fn parse(buf: &[u8]) -> Result<Self, VvdError> {
        if buf.len() < 4 {
            return Err(VvdError::TooShortForId { len: buf.len() });
        }
        let id: [u8; 4] = buf[0..4].try_into().unwrap();
        if &id != ID {
            return Err(VvdError::BadId { found: id });
        }
        if buf.len() < HEADER_SIZE {
            return Err(VvdError::Truncated {
                need: HEADER_SIZE,
                have: buf.len(),
                what: "VVD header",
            });
        }
        let i32_at = |o: usize| i32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);

        let version = i32_at(4);
        if version != VERSION {
            return Err(VvdError::BadVersion { found: version });
        }
        let mut num_lod_vertexes = [0i32; MAX_NUM_LODS];
        for (i, slot) in num_lod_vertexes.iter_mut().enumerate() {
            *slot = i32_at(0x10 + i * 4);
        }
        let header = VvdHeader {
            checksum: i32_at(0x08),
            num_lods: i32_at(0x0C),
            num_lod_vertexes,
            num_fixups: i32_at(0x30),
            fixup_table_start: i32_at(0x34),
            vertex_data_start: i32_at(0x38),
            tangent_data_start: i32_at(0x3C),
        };

        let count = header.vertex_count();
        let vstart = header.vertex_data_start.max(0) as usize;
        let tstart = header.tangent_data_start.max(0) as usize;

        // fixup 表（可能为空）。越界一律报错而不是 panic。
        let fixups = if header.num_fixups > 0 {
            let fstart = header.fixup_table_start.max(0) as usize;
            let need = fstart + header.num_fixups as usize * FIXUP_SIZE;
            if need > buf.len() {
                return Err(VvdError::Truncated {
                    need,
                    have: buf.len(),
                    what: "fixup table",
                });
            }
            (0..header.num_fixups as usize)
                .map(|i| {
                    let o = fstart + i * FIXUP_SIZE;
                    crate::lod::Fixup {
                        lod: i32_at(o),
                        source_vertex_id: i32_at(o + 4),
                        num_vertexes: i32_at(o + 8),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        let vbytes = count * VERTEX_SIZE;
        if vstart + vbytes > buf.len() {
            return Err(VvdError::Truncated {
                need: vstart + vbytes,
                have: buf.len(),
                what: "vertex block",
            });
        }
        let tbytes = count * TANGENT_SIZE;
        if tstart + tbytes > buf.len() {
            return Err(VvdError::Truncated {
                need: tstart + tbytes,
                have: buf.len(),
                what: "tangent block",
            });
        }

        let vertices = (0..count)
            .map(|i| VvdVertex::read(buf, vstart + i * VERTEX_SIZE))
            .collect();
        let tangents = (0..count)
            .map(|i| {
                let o = tstart + i * TANGENT_SIZE;
                let f = |k: usize| {
                    f32::from_le_bytes([buf[o + k], buf[o + k + 1], buf[o + k + 2], buf[o + k + 3]])
                };
                VvdTangent {
                    xyz: [f(0), f(4), f(8)],
                    w: f(12),
                }
            })
            .collect();

        Ok(Self {
            header,
            vertices,
            tangents,
            fixups,
        })
    }

    /// 按头部声明的偏移序列化（支持 fixup 表与多 LOD）。
    ///
    /// 偏移一律**重算**，不沿用读入时的值 —— 这正是「往返必须逐字节相同」
    /// 能证明布局理解正确的原因：若原文件的偏移与重算结果不一致，
    /// 往返测试会失败而不是悄悄照抄。
    ///
    /// # 偏移的算法（`write.cpp` 2799-2815 行）
    ///
    /// ```text
    /// fixupTableStart  = ALIGN4(64)                     = 64
    /// vertexDataStart  = ALIGN16(fixupTableStart + numFixups * 12)
    /// tangentDataStart = ALIGN16(vertexDataStart + numLODVertexes[0] * 48)
    /// ```
    ///
    /// 无 fixup 时 `numFixups * 12 == 0`，三个偏移退化成
    /// `64 / 64 / 64 + n*48`，与旧实现的输出**逐字节相同**。
    pub fn to_bytes(&self) -> Result<Vec<u8>, VvdError> {
        let count = self.vertices.len();
        if self.tangents.len() != count {
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "there are %{count} vertices but %{tangents} tangents",
                    count = count,
                    tangents = self.tangents.len()
                ),
            });
        }
        if self.fixups.len() != self.header.num_fixups.max(0) as usize {
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "numFixups declares %{declared} entries but there are actually %{actual}",
                    declared = self.header.num_fixups,
                    actual = self.fixups.len()
                ),
            });
        }
        // 顶点块的长度由 numLODVertexes[0] 决定，必须与实际顶点数一致 ——
        // 否则写出的文件长度与头部声明对不上，引擎会读到别的块里去。
        if self.header.vertex_count() != count {
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "numLODVertexes[0] declares %{declared} vertices but there are actually %{actual}",
                    declared = self.header.vertex_count(),
                    actual = count
                ),
            });
        }

        // ---- 骨骼下标：**格式**上限（有符号 char，0..=127）----
        //
        // ⚠️ 查的是**被顶点引用的下标**，不是骨骼总数。实测
        // （`docs/_probe/diag_bone_use.js`）：
        //   · mdlc 的 `linnea-export` 有 **134 根**骨骼，但最大引用下标只 **118**
        //   · 官方 122 根产物最大引用下标 **119**
        // 两者都没越界 —— 没被引用的骨骼只存在于 `mstudiobone_t` 数组里。
        //
        // 所以「骨骼总数 ≤ 128」是**错的**判据（那样会误拒上面两个真实模型），
        // 真正装不下的是**下标 ≥ 128**：`char` 会把 200 读成 −56。
        //
        // 只查前 `bone_count` 个槽位 —— 后面的填充槽位无意义
        // （studiomdl 常写 0），不参与语义。
        for (i, v) in self.vertices.iter().enumerate() {
            // `bone[]` 是定长 3 元素数组（`MAX_NUM_BONES_PER_VERT`）。
            let n = (v.bone_count as usize).min(3);
            for k in 0..n {
                let b = v.bone[k];
                if b > crate::mdl_writer::MAX_BONE_INDEX_IN_VERTEX as u8 {
                    return Err(VvdError::Inconsistent {
                        detail: crate::tr_fmt!(
                            "vertex %{vertex} references the bone index %{bone}, over the format limit %{max} -- \
                             `mstudioboneweight_t.bone[]` in VVD is a **signed char**, \
                             so an index >=128 is read as negative by the engine",
                            vertex = i,
                            bone = b,
                            max = crate::mdl_writer::MAX_BONE_INDEX_IN_VERTEX
                        ),
                    });
                }
            }
        }

        let fixup_table_start = HEADER_SIZE; // ALIGN4(64) == 64
        let vertex_data_start = align_up(fixup_table_start + self.fixups.len() * FIXUP_SIZE, 16);
        let tangent_data_start = align_up(vertex_data_start + count * VERTEX_SIZE, 16);
        // 尾部**不**对齐（实测文件长度恰好是切线块结束）。
        let total = tangent_data_start + count * TANGENT_SIZE;
        debug_assert_eq!(
            (fixup_table_start, vertex_data_start, tangent_data_start),
            (
                self.header.fixup_table_start.max(0) as usize,
                self.header.vertex_data_start.max(0) as usize,
                self.header.tangent_data_start.max(0) as usize,
            ),
            "头部偏移与重算结果不一致 —— 调用方应先调 recompute_offsets()"
        );

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(ID);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&self.header.checksum.to_le_bytes());
        out.extend_from_slice(&self.header.num_lods.to_le_bytes());
        for slot in self.header.num_lod_vertexes {
            out.extend_from_slice(&slot.to_le_bytes());
        }
        out.extend_from_slice(&(self.fixups.len() as i32).to_le_bytes());
        out.extend_from_slice(&(fixup_table_start as i32).to_le_bytes());
        out.extend_from_slice(&(vertex_data_start as i32).to_le_bytes());
        out.extend_from_slice(&(tangent_data_start as i32).to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_SIZE);

        // fixup 表（紧跟头部；numFixups == 0 时长度为 0）。
        for f in &self.fixups {
            out.extend_from_slice(&f.lod.to_le_bytes());
            out.extend_from_slice(&f.source_vertex_id.to_le_bytes());
            out.extend_from_slice(&f.num_vertexes.to_le_bytes());
        }
        // ALIGN16 的填充（studiomdl 用 memset 0 初始化整个缓冲区，所以是 0）。
        out.resize(vertex_data_start, 0);

        for v in &self.vertices {
            v.write(&mut out);
        }
        // 顶点块到切线块之间的填充。
        out.resize(tangent_data_start, 0);
        for t in &self.tangents {
            for c in t.xyz {
                out.extend_from_slice(&c.to_le_bytes());
            }
            out.extend_from_slice(&t.w.to_le_bytes());
        }
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }

    /// 用头部声明推导出的长度（用于自检，不依赖实际写入的字节数）。
    pub fn declared_len(&self) -> usize {
        let count = self.header.vertex_count();
        let vstart = align_up(
            HEADER_SIZE + self.header.num_fixups.max(0) as usize * FIXUP_SIZE,
            16,
        );
        let tstart = align_up(vstart + count * VERTEX_SIZE, 16);
        tstart + count * TANGENT_SIZE
    }

    /// 从「一组顶点」构造一份最简 VVD（无 fixup、单 LOD）。
    ///
    /// `tangents` 为 `None` 时由法线派生一组**占位**切线 ——
    /// 这条路径**只应用于没有三角形信息的场合**。有三角形时请用
    /// [`crate::tangent::tangents_for_mesh`] 算真实切线（占位切线会让
    /// 法线贴图发黑/光照方向错，且不会报任何错）。
    pub fn from_vertices(
        checksum: i32,
        vertices: Vec<VvdVertex>,
        tangents: Option<Vec<VvdTangent>>,
    ) -> Result<Self, VvdError> {
        Self::from_vertices_lods(checksum, vertices, tangents, 1)
    }

    /// 从「一组顶点」构造一份 VVD，显式指定 LOD 数与各 LOD 顶点数。
    ///
    /// `num_lod_vertexes` 的长度必须等于 `num_lods`；其余槽位用最后一个
    /// 有效值填充（studiomdl 的「ripple」行为，`write.cpp` 2836 行）。
    ///
    /// # 关于 `numLODVertexes` 的语义（实测确认）
    ///
    /// 它**不是**「每个 LOD 各自的顶点数」，而是**累计数**：
    /// `numLODVertexes[n]` = 渲染 LOD n 所需的顶点数
    /// = 所有「细节不低于 n」的块的长度之和。所以它随 n 增大而**单调不增**，
    /// 且 `[0]` 就是存储的顶点总数。见 [`crate::lod::build_lod_layout`]。
    pub fn from_vertices_lods(
        checksum: i32,
        vertices: Vec<VvdVertex>,
        tangents: Option<Vec<VvdTangent>>,
        num_lods: i32,
    ) -> Result<Self, VvdError> {
        let count = vertices.len();
        let tangents = match tangents {
            Some(t) if t.len() == count => t,
            Some(t) => {
                return Err(VvdError::Inconsistent {
                    detail: crate::tr_fmt!(
                        "there are %{count} vertices but %{tangents} tangents",
                        count = count,
                        tangents = t.len()
                    ),
                });
            }
            None => vertices
                .iter()
                .map(|v| {
                    // 由法线派生一个正交的占位切线（w = 1）。
                    crate::tangent::fallback_tangent_from_normal(v.normal)
                })
                .collect(),
        };
        let mut num_lod_vertexes = [0i32; MAX_NUM_LODS];
        // **全部 8 个槽位都要填顶点数**，不是只填 [0]。
        // 实测 studiomdl 产物：只有 1 个 LOD 时 `numLODVertexes` 是
        // `[8, 8, 8, 8, 8, 8, 8, 8]`，其余槽位留 0 会让引擎的 LOD 切换
        // 读到「该 LOD 有 0 个顶点」。
        //
        // 多 LOD 时槽位 `[0..num_lods]` 由调用方通过 [`Self::set_lod_counts`]
        // 填真实的累计值，这里先统一填 count（单 LOD 的正确值）。
        num_lod_vertexes.fill(count as i32);
        Ok(Self {
            header: VvdHeader {
                checksum,
                num_lods: num_lods.max(1),
                num_lod_vertexes,
                num_fixups: 0,
                fixup_table_start: HEADER_SIZE as i32,
                vertex_data_start: HEADER_SIZE as i32,
                tangent_data_start: (HEADER_SIZE + count * VERTEX_SIZE) as i32,
            },
            vertices,
            tangents,
            fixups: Vec::new(),
        })
    }

    /// 覆盖 `numLODVertexes` 的前 `counts.len()` 个槽位，并按 studiomdl
    /// 的规则把最后一个有效值**ripple** 到剩余槽位。
    ///
    /// `counts[n]` 必须是累计值（单调不增），见
    /// [`crate::lod::build_lod_layout`]。
    pub fn set_lod_counts(&mut self, counts: &[i32]) {
        if counts.is_empty() {
            return;
        }
        let n = counts.len().min(MAX_NUM_LODS);
        for (i, c) in counts.iter().take(n).enumerate() {
            self.header.num_lod_vertexes[i] = *c;
        }
        let last = self.header.num_lod_vertexes[n - 1];
        for slot in self.header.num_lod_vertexes.iter_mut().skip(n) {
            *slot = last;
        }
        self.header.num_lods = n as i32;
    }

    /// 按 `write.cpp` 2799-2815 行的公式**重算**三个偏移并写回头部。
    ///
    /// ```text
    /// fixupTableStart  = ALIGN4(64)                     = 64
    /// vertexDataStart  = ALIGN16(fixupTableStart + numFixups * 12)
    /// tangentDataStart = ALIGN16(vertexDataStart + numLODVertexes[0] * 48)
    /// ```
    ///
    /// 这是**唯一**的偏移公式来源：[`Self::to_bytes`] 与
    /// [`check_invariants`] 都用它，`crate::lod::build_multi_lod_vvd` 在
    /// 填完 fixup 表后也必须调它 —— 否则头部声明的偏移与实际写出的
    /// 位置对不上，自检会（正确地）报错。
    pub fn recompute_offsets(&mut self) {
        let count = self.header.vertex_count();
        let fixup_table_start = HEADER_SIZE; // ALIGN4(64) == 64
        let vertex_data_start = align_up(fixup_table_start + self.fixups.len() * FIXUP_SIZE, 16);
        let tangent_data_start = align_up(vertex_data_start + count * VERTEX_SIZE, 16);
        self.header.num_fixups = self.fixups.len() as i32;
        self.header.fixup_table_start = fixup_table_start as i32;
        self.header.vertex_data_start = vertex_data_start as i32;
        self.header.tangent_data_start = tangent_data_start as i32;
    }
}

/// 交叉校验：头部字段之间、以及与实际数据长度之间必须自洽。
/// 这些等式在实测的官方模型上全部成立（见模块文档第 1 条），
/// 所以任何一条不成立都说明解析或写入有 bug —— 让它显式失败，
/// 而不是产出一个「看起来能读、进游戏才发现错」的文件。
///
/// # 两条形态（实测 3302 个真实模型）
///
/// - **无 fixup**（3249 个）：三偏移重合在 64，`numLODVertexes[0] == 顶点数`；
/// - **有 fixup**（53 个，全部是多 LOD）：偏移按 `ALIGN4/ALIGN16` 递推，
///   且 fixup 表必须精确铺满 `[0, numLODVertexes[0])`。
///
/// 多 LOD 的 `numLODVertexes` 是**累计值**，必须单调不增，
/// 且尾部槽位等于最后一个有效值（studiomdl 的 ripple）。
pub fn check_invariants(vvd: &Vvd, file_len: usize) -> Result<(), VvdError> {
    let count = vvd.header.vertex_count();
    let h = &vvd.header;
    let num_lods = h.num_lods.max(1) as usize;

    if !(1..=MAX_NUM_LODS).contains(&(h.num_lods.max(1) as usize)) {
        return Err(VvdError::Inconsistent {
            detail: crate::tr_fmt!(
                "numLODs should be in 1..=%{max}, but is %{got}",
                max = MAX_NUM_LODS,
                got = h.num_lods
            ),
        });
    }
    if h.num_fixups < 0 {
        return Err(VvdError::Inconsistent {
            detail: crate::tr_fmt!("numFixups must not be negative: %{got}", got = h.num_fixups),
        });
    }
    if vvd.fixups.len() != h.num_fixups as usize {
        return Err(VvdError::Inconsistent {
            detail: crate::tr_fmt!(
                "numFixups declares %{declared} entries but the table has %{actual}",
                declared = h.num_fixups,
                actual = vvd.fixups.len()
            ),
        });
    }

    // ---- 偏移链（write.cpp 2799-2815）----
    let expect_fixup = align_up(HEADER_SIZE, 4);
    let expect_vertex = align_up(expect_fixup + h.num_fixups as usize * FIXUP_SIZE, 16);
    let expect_tangent = align_up(expect_vertex + count * VERTEX_SIZE, 16);
    for (name, got, want) in [
        ("fixupTableStart", h.fixup_table_start, expect_fixup),
        ("vertexDataStart", h.vertex_data_start, expect_vertex),
        ("tangentDataStart", h.tangent_data_start, expect_tangent),
    ] {
        if got != want as i32 {
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "%{name} should be %{want}, but is %{got}",
                    name = name,
                    want = want,
                    got = got
                ),
            });
        }
    }
    // 文件长度恰好等于切线块结束（尾部不对齐）。
    let expect_len = h.tangent_data_start.max(0) as usize + count * TANGENT_SIZE;
    if file_len != expect_len {
        return Err(VvdError::Inconsistent {
            detail: crate::tr_fmt!(
                "the file length should be %{expect}, but is %{got}",
                expect = expect_len,
                got = file_len
            ),
        });
    }
    if vvd.vertices.len() != count || vvd.tangents.len() != count {
        return Err(VvdError::Inconsistent {
            detail: crate::tr_fmt!(
                "there should be %{count} vertices/tangents, but there are %{vertices}/%{tangents}",
                count = count,
                vertices = vvd.vertices.len(),
                tangents = vvd.tangents.len()
            ),
        });
    }

    // ---- numLODVertexes：单调不增 + ripple ----
    for i in 1..num_lods {
        let (a, b) = (h.num_lod_vertexes[i - 1], h.num_lod_vertexes[i]);
        if b > a {
            let prev = i - 1;
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "numLODVertexes must be monotonically non-increasing, but [%{prev}]=%{a} < [%{i}]=%{b}",
                    prev = prev,
                    a = a,
                    i = i,
                    b = b
                ),
            });
        }
    }
    let last = h.num_lod_vertexes[num_lods - 1];
    for i in num_lods..MAX_NUM_LODS {
        if h.num_lod_vertexes[i] != last {
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "numLODVertexes[%{i}] should ripple to %{last} (the last valid LOD), but is %{got}",
                    i = i,
                    last = last,
                    got = h.num_lod_vertexes[i]
                ),
            });
        }
    }
    if h.num_lod_vertexes[0] != count as i32 {
        return Err(VvdError::Inconsistent {
            detail: crate::tr_fmt!(
                "numLODVertexes[0] should be the vertex total %{count}, but is %{got}",
                count = count,
                got = h.num_lod_vertexes[0]
            ),
        });
    }

    // ---- fixup 表：精确铺满 [0, count)，且 LOD 合法 ----
    if !vvd.fixups.is_empty() {
        let mut covered = vec![false; count];
        for (i, f) in vvd.fixups.iter().enumerate() {
            if f.lod < 0 || f.lod as usize >= num_lods {
                return Err(VvdError::Inconsistent {
                    detail: crate::tr_fmt!(
                        "fixup[%{i}].lod = %{lod} is out of range (numLODs = %{num_lods})",
                        i = i,
                        lod = f.lod,
                        num_lods = num_lods
                    ),
                });
            }
            if f.num_vertexes < 0 || f.source_vertex_id < 0 {
                return Err(VvdError::Inconsistent {
                    detail: crate::tr_fmt!(
                        "fixup[%{i}] has negative values: src=%{src} n=%{n}",
                        i = i,
                        src = f.source_vertex_id,
                        n = f.num_vertexes
                    ),
                });
            }
            let (s, n) = (f.source_vertex_id as usize, f.num_vertexes as usize);
            // 该 LOD 的顶点必须落在它自己的前缀里。
            if s + n > h.num_lod_vertexes[f.lod as usize].max(0) as usize {
                return Err(VvdError::Inconsistent {
                    detail: crate::tr_fmt!(
                        "the fixup[%{i}] range [%{start},%{end}) goes past the %{count} vertices of LOD %{lod}",
                        i = i,
                        start = s,
                        end = s + n,
                        count = h.num_lod_vertexes[f.lod as usize],
                        lod = f.lod
                    ),
                });
            }
            for (k, slot) in covered.iter_mut().enumerate().skip(s).take(n) {
                if k >= count {
                    return Err(VvdError::Inconsistent {
                        detail: crate::tr_fmt!(
                            "the fixup[%{i}] range is out of range: %{k} >= %{count}",
                            i = i,
                            k = k,
                            count = count
                        ),
                    });
                }
                if *slot {
                    return Err(VvdError::Inconsistent {
                        detail: crate::tr_fmt!("overlapping fixup ranges @%{k}", k = k),
                    });
                }
                *slot = true;
            }
        }
        if let Some(gap) = covered.iter().position(|c| !c) {
            return Err(VvdError::Inconsistent {
                detail: crate::tr_fmt!(
                    "the fixup table does not cover the vertex %{gap} (there is a hole)",
                    gap = gap
                ),
            });
        }
    }
    Ok(())
}
