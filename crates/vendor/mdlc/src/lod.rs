//! 多 LOD 与 fixup 表：顶点池的排序、分段与重映射。
//!
//! ```text
//! lodA = Q_log2(lodFlagsA)          // 最高置位 bit
//! 主键：lodB - lodA                 // 降序（LOD N-1 在前，LOD 0 在后）
//! 次键：vertexOffset 升序            // mesh 序
//! 末键：meshVertID   升序            // mesh 内顶点号
//! ```
//! ```text
//! numLODVertexes[n] = Σ_m Σ_{k >= n} |block_m[k]|
//! ```

use std::collections::HashMap;

use crate::model::Vertex;
use crate::vvd::VvdTangent;

/// 一个顶点的 LOD 归属位掩码（bit n = 被 LOD n 使用）。
pub type LodFlags = u32;

/// 一条 fixup 记录（`vertexFileFixup_t`，**12 字节**）。
///
/// ```text
/// +0x00  int32  lod              该块的 LOD 号（用于跳过被裁掉的高细节 LOD）
/// +0x04  int32  sourceVertexID   **绝对**下标，相对顶点/切线块的起点
/// +0x08  int32  numVertexes      块长度
/// ```
///
/// 实测确认（`studio.h` 1863 行 + 53/53 个真实模型的表内容）。
/// 注意 `sourceVertexID` 是**相对顶点块起点**的下标，不是字节偏移 ——
/// 这一点与 `mstudiomodel_t.vertexindex`（字节偏移）不同，容易混。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixup {
    pub lod: i32,
    pub source_vertex_id: i32,
    pub num_vertexes: i32,
}

/// `vertexFileFixup_t` 的字节大小。
pub const FIXUP_SIZE: usize = 12;

/// 一个 mesh 跨全部 LOD 的统一数据。
///
/// 「统一」指顶点池是各 LOD 顶点池的**并集**（按属性精确去重），
/// 与 `UnifyLODs.cpp` 的 `CVertexDictionary` 语义一致。
#[derive(Debug, Clone, PartialEq)]
pub struct MeshLods {
    /// 统一顶点池。顺序 = 各 LOD 依次追加时首次出现的顺序
    /// （LOD 0 的顶点在前，然后是新出现的 LOD 1 顶点，依此类推）。
    pub vertices: Vec<Vertex>,
    /// 与 `vertices` 等长：每个顶点被哪些 LOD 使用。
    pub lod_flags: Vec<LodFlags>,
    /// 每个 LOD 的三角形，下标指向 `vertices`。
    /// `triangles[n]` 是 LOD n 的三角形列表。
    pub triangles: Vec<Vec<[u32; 3]>>,
    /// 每个 LOD 的顶点数（= 该 LOD 实际引用的统一顶点数）。
    pub lod_vertex_counts: Vec<usize>,
    /// 每个 LOD 的顶点下标映射：**该 LOD 的局部下标 → 统一池下标**。
    ///
    /// 单 LOD 时它就是 `0..n`。多 LOD 时用于把「该 LOD 自己的顶点号」
    /// 翻译回统一池 —— 注意 `MeshLods::triangles[n]` 里的下标**已经**
    /// 是统一池下标，不需要再过这张表。
    pub lod_vertex_index: Vec<Vec<u32>>,
}

impl MeshLods {
    /// 从「单个 LOD 的顶点池 + 三角形」构造（单 LOD 快捷路径）。
    pub fn single(vertices: Vec<Vertex>, triangles: Vec<[u32; 3]>) -> Self {
        let n = vertices.len();
        let index: Vec<u32> = (0..n as u32).collect();
        Self {
            lod_flags: vec![1; n],
            vertices,
            triangles: vec![triangles],
            lod_vertex_counts: vec![n],
            lod_vertex_index: vec![index],
        }
    }

    /// LOD 数。
    pub fn num_lods(&self) -> usize {
        self.triangles.len()
    }
}

/// 顶点去重键（按位比较，与 `compile.rs` 的语义一致）。
///
/// 用位模式而非 `==`：`-0.0 == 0.0` 但在文件里是不同的字节，
/// 而且 `NaN` 用 `==` 永不相等会让去重失效。
#[derive(PartialEq, Eq, Hash)]
struct VertexKey {
    pos: [u32; 3],
    normal: [u32; 3],
    uv: [u32; 2],
    bones: Vec<(u32, u32)>,
}

fn fbits(v: f32) -> u32 {
    if v == 0.0 {
        0.0f32.to_bits()
    } else {
        v.to_bits()
    }
}

fn vertex_key(v: &Vertex) -> VertexKey {
    let mut bones: Vec<(u32, u32)> = v.bones.iter().map(|p| (fbits(p[0]), fbits(p[1]))).collect();
    bones.sort_unstable();
    VertexKey {
        pos: [fbits(v.pos[0]), fbits(v.pos[1]), fbits(v.pos[2])],
        normal: [fbits(v.normal[0]), fbits(v.normal[1]), fbits(v.normal[2])],
        uv: [fbits(v.uv[0]), fbits(v.uv[1])],
        bones,
    }
}

/// 把多个 LOD 的独立顶点池合并成一个统一池。
///
/// # 语义（对应 `UnifyLODs.cpp`）
///
/// - 顶点按**属性精确匹配**去重：位置、法线、UV、骨骼绑定全同才算同一个顶点。
/// - `lodFlags` 取并集（`m_bLoD |= vertex.m_bLoD`），且**由三角形推导**：
///   只有被该 LOD 的某个三角形真正引用到的顶点才算「被该 LOD 使用」。
/// - 输入顺序即 LOD 顺序：`lods[0]` 是 LOD 0（最精细）。
///
/// # 为什么 lodFlags 必须由三角形推导，而不是「顶点出现在该 LOD 的列表里」
///
/// 一个 LOD 的顶点列表里可能有**没有任何三角形引用**的顶点（导出器的残留）。
/// 若按「出现在列表里」就打标记，这些顶点会被算进 `numLODVertexes`，
/// 于是声明的顶点数大于实际被引用的顶点数 —— 引擎按 `numLODVertexes[n]`
/// 截断顶点块时会读到多余数据，而 fixup 表又覆盖不到它们，
/// 结果是**顶点错位**（不报错）。
///
/// # 为什么要去重
///
/// 不去重的话，同一个顶点在 VVD 里会出现多次（每个 LOD 一份），
/// `numLODVertexes` 与实际存储量对不上，fixup 表也无从表达
/// 「LOD 1 复用 LOD 0 的顶点」这件事。
pub fn unify_lods(lods: &[(Vec<Vertex>, Vec<[u32; 3]>)]) -> MeshLods {
    let mut vertices: Vec<Vertex> = Vec::new();
    let mut lod_flags: Vec<LodFlags> = Vec::new();
    let mut table: HashMap<VertexKey, u32> = HashMap::new();
    let mut triangles: Vec<Vec<[u32; 3]>> = Vec::with_capacity(lods.len());
    let mut lod_vertex_index: Vec<Vec<u32>> = Vec::with_capacity(lods.len());
    let mut lod_vertex_counts: Vec<usize> = Vec::with_capacity(lods.len());

    for (n, (verts, tris)) in lods.iter().enumerate() {
        // 第一遍：把本 LOD 的顶点并入统一池，建立「局部下标 → 池下标」映射。
        let mut remap: Vec<u32> = Vec::with_capacity(verts.len());
        let mut index: Vec<u32> = Vec::with_capacity(verts.len());
        for v in verts {
            let key = vertex_key(v);
            let id = match table.get(&key) {
                Some(&i) => i, // 已存在 → 复用，**不**在这里打 lodFlags
                None => {
                    let i = vertices.len() as u32;
                    vertices.push(v.clone());
                    lod_flags.push(0);
                    table.insert(key, i);
                    i
                }
            };
            remap.push(id);
            index.push(id);
        }
        // 第二遍：把三角形下标从「本 LOD 局部」改写到「统一池」，
        // 并据此置 lodFlags —— 只有被三角形引用到的顶点才算本 LOD 使用。
        let bit: LodFlags = 1u32 << n;
        let mut remapped: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
        for t in tris {
            let mut out = [0u32; 3];
            for (k, &u) in t.iter().enumerate() {
                let Some(&pool) = remap.get(u as usize) else {
                    // 越界下标属上游 bug：跳过这个三角形而不是 panic。
                    out = [u32::MAX; 3];
                    break;
                };
                out[k] = pool;
                lod_flags[pool as usize] |= bit;
            }
            if out[0] != u32::MAX {
                remapped.push(out);
            }
        }
        let used = lod_flags.iter().filter(|f| **f & bit != 0).count();
        triangles.push(remapped);
        lod_vertex_counts.push(used);
        lod_vertex_index.push(index);
    }

    // 孤立顶点（哪个 LOD 都没引用）强制归到最低细节 LOD。
    // 对应 `write.cpp` 2592 行 —— 不加这一步，它们的 lodFlags 是 0，
    // 排序时 `Q_log2(0)` 未定义，会被排到错误的位置。
    let lowest = 1u32 << (lods.len().max(1) - 1);
    for f in lod_flags.iter_mut() {
        if *f == 0 {
            *f = lowest;
        }
    }

    MeshLods {
        vertices,
        lod_flags,
        triangles,
        lod_vertex_counts,
        lod_vertex_index,
    }
}

/// 一个 mesh 在排序后顶点池里的分段结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshBlocks {
    /// `blocks[n]` = 该 mesh 中「最高 LOD 位恰好 == n」的块的起点（排序池下标）。
    /// 长度 == LOD 数；长度为 0 的块用 `None` 表示。
    pub blocks: Vec<Option<Block>>,
    /// 该 mesh 的 `finalMeshVertID` 映射：排序池下标 → mesh 内编号。
    ///
    /// 这就是 VTX 的 `origMeshVertID`。
    pub final_mesh_vert_id: HashMap<usize, u32>,
    /// 该 mesh 跨 LOD 的顶点总数（= `mstudiomesh_t.numvertices`）。
    pub total_vertexes: usize,
}

/// 排序池里的一段连续区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// 区间起点（排序池下标）。
    pub start: usize,
    /// 区间长度。
    pub len: usize,
}

/// 整个 model 的 LOD 布局：排序后的顶点池 + fixup 表 + 各 mesh 的分段。
#[derive(Debug, Clone, PartialEq)]
pub struct LodLayout {
    /// 排序后的顶点池顺序：每项是 `(mesh 序号, mesh 内顶点号)`。
    pub order: Vec<(usize, u32)>,
    /// 排序后的顶点（与 `order` 等长）。
    pub vertices: Vec<Vertex>,
    /// 排序后的 LOD 掩码（与 `order` 等长）。
    pub lod_flags: Vec<LodFlags>,
    /// `numLODVertexes[0..num_lods]`。
    pub num_lod_vertexes: Vec<i32>,
    /// fixup 表（`num_lods == 1` 时为空）。
    pub fixups: Vec<Fixup>,
    /// 每个 mesh 的分段结果（顺序 = 传入顺序）。
    pub meshes: Vec<MeshBlocks>,
    /// LOD 数。
    pub num_lods: usize,
    /// 反查表：`(mesh 序号, 统一池顶点号) → 排序池下标`。
    ///
    /// 写 VTX 时要用它把「某 LOD 的三角形引用的统一顶点」翻译成
    /// `finalMeshVertID`（= `origMeshVertID`）。
    pub pos_of: HashMap<(usize, u32), usize>,
}

impl LodLayout {
    /// 求「某 mesh 的某个统一顶点」在 VTX 里的 `origMeshVertID`。
    ///
    /// 多 LOD 时这是 `finalMeshVertID`（把 LOD 排序的块按从粗到细拼回
    /// mesh 顺序后的编号）；单 LOD 时就是该顶点在 mesh 内的原下标。
    /// 返回 `None` 表示该顶点不属于这个 mesh（上游 bug）。
    pub fn mesh_local_id(&self, mesh: usize, unified: u32) -> Option<u32> {
        let pos = *self.pos_of.get(&(mesh, unified))?;
        self.meshes.get(mesh)?.final_mesh_vert_id.get(&pos).copied()
    }
}

/// `Q_log2`：最高置位 bit 的下标（`v == 0` 返回 -1）。
///
/// `write.cpp` 2383 行用它做排序主键。这里显式处理 0，
/// 避免 Rust 的 `leading_zeros` 在 0 上给出 32。
#[inline]
fn q_log2(v: u32) -> i32 {
    if v == 0 {
        -1
    } else {
        31 - v.leading_zeros() as i32
    }
}

/// 计算整个 model 的 LOD 布局（排序 + fixup + finalMeshVertID）。
///
/// `meshes` 的顺序必须与 MDL 的 mesh 顺序一致（bodypart → model → mesh）。
///
/// # 排序键（`_CompareUsedVertexes`）
///
/// ```text
/// 主键：Q_log2(lodFlags) 降序
/// 次键：mesh 序号         升序
/// 末键：mesh 内顶点号      升序
/// ```
///
/// # 单 LOD 的恒等性
///
/// `num_lods == 1` 时所有掩码都是 1，主键全相同，排序退化为
/// 「按 mesh 序、mesh 内顶点号升序」—— 即自然顺序。
/// `numLODVertexes[0]` = 总顶点数，`[1..8]` 用最后一个有效值填充
/// （`write.cpp` 2836 行的 ripple），`fixups` 为空。
pub fn build_lod_layout(meshes: &[MeshLods]) -> LodLayout {
    let num_lods = meshes
        .iter()
        .map(|m| m.num_lods())
        .max()
        .unwrap_or(1)
        .max(1);

    // ---- 1. 收集全部顶点并排序 ----
    let mut items: Vec<(usize, u32, LodFlags)> = Vec::new();
    for (mi, m) in meshes.iter().enumerate() {
        for (vi, &f) in m.lod_flags.iter().enumerate() {
            items.push((mi, vi as u32, f));
        }
    }
    // 稳定排序 + 显式的三级键，与 `_CompareUsedVertexes` 一致。
    items.sort_by(|a, b| {
        q_log2(b.2)
            .cmp(&q_log2(a.2)) // 最高位降序
            .then(a.0.cmp(&b.0)) // mesh 序升序
            .then(a.1.cmp(&b.1)) // mesh 内顶点号升序
    });

    let order: Vec<(usize, u32)> = items.iter().map(|&(m, v, _)| (m, v)).collect();
    let lod_flags: Vec<LodFlags> = items.iter().map(|&(_, _, f)| f).collect();
    let vertices: Vec<Vertex> = order
        .iter()
        .map(|&(m, v)| meshes[m].vertices[v as usize].clone())
        .collect();

    // ---- 2. 每个 mesh 的分段（FindVertexOffsets）----
    // 对每个 mesh、每个 LOD n：取「属于该 mesh 且最高位恰好 == n」的连续区间。
    //
    // 顺带建 `pos_of`（反查表），写 VTX 时要用。
    let mut pos_of: HashMap<(usize, u32), usize> = HashMap::with_capacity(order.len());
    for (k, &key) in order.iter().enumerate() {
        pos_of.insert(key, k);
    }
    let mut mesh_blocks: Vec<MeshBlocks> = Vec::with_capacity(meshes.len());
    for mi in 0..meshes.len() {
        let n_lods = num_lods;
        let mut blocks: Vec<Option<Block>> = vec![None; n_lods];
        // 从粗到细扫描（`FindVertexOffsets` 是 `for i = numLods-1; i >= 0; i--`）。
        for n in (0..n_lods).rev() {
            // 找该 mesh 中第一个「最高位 == n」的顶点。
            let mut found = None;
            for (j, &(m2, _, f)) in items.iter().enumerate() {
                if m2 != mi || q_log2(f) != n as i32 {
                    continue;
                }
                // 从 j 往后数，直到 mesh 变了或最高位不再是 n。
                let mut k = j;
                while k < items.len() {
                    let (m3, _, f3) = items[k];
                    if m3 != mi || q_log2(f3) != n as i32 {
                        break;
                    }
                    k += 1;
                }
                found = Some(Block {
                    start: j,
                    len: k - j,
                });
                break;
            }
            blocks[n] = found;
        }
        // finalMeshVertID：按 n 从粗到细，把各 block 的顶点依次编号。
        let mut final_ids: HashMap<usize, u32> = HashMap::new();
        let mut next = 0u32;
        for n in (0..n_lods).rev() {
            if let Some(b) = blocks[n] {
                for k in 0..b.len {
                    final_ids.insert(b.start + k, next);
                    next += 1;
                }
            }
        }
        mesh_blocks.push(MeshBlocks {
            blocks,
            final_mesh_vert_id: final_ids,
            total_vertexes: next as usize,
        });
    }

    // ---- 3. numLODVertexes（write.cpp 2820-2840）----
    // numLODVertexes[n] = Σ_m Σ_{k>=n} |block_m[k]|
    let mut num_lod_vertexes = vec![0i32; num_lods];
    for (n, slot) in num_lod_vertexes.iter_mut().enumerate() {
        let mut total = 0usize;
        for mb in &mesh_blocks {
            for (k, b) in mb.blocks.iter().enumerate() {
                if k >= n
                    && let Some(b) = b
                {
                    total += b.len;
                }
            }
        }
        *slot = total as i32;
    }

    // ---- 4. fixup 表（write.cpp 2842-2884）----
    // numLODs == 1 时显式不做 fixup（2777 行）。
    let mut fixups: Vec<Fixup> = Vec::new();
    if num_lods > 1 {
        for mb in &mesh_blocks {
            for n in (0..num_lods).rev() {
                if let Some(b) = mb.blocks[n]
                    && b.len > 0
                {
                    fixups.push(Fixup {
                        lod: n as i32,
                        source_vertex_id: b.start as i32,
                        num_vertexes: b.len as i32,
                    });
                }
            }
        }
        // `write.cpp` 2777：只有 1 个 mesh、或只有 1 条 fixup、或只有 1 个 LOD
        // 时不需要重定位表（数据本来就是连续的）。
        if meshes.len() == 1 || fixups.len() == 1 {
            fixups.clear();
        }
    }

    LodLayout {
        order,
        vertices,
        lod_flags,
        num_lod_vertexes,
        fixups,
        meshes: mesh_blocks,
        num_lods,
        pos_of,
    }
}

/// 计算**排序后**顶点池的切线。
///
/// # 为什么要在统一池上算，而不是每个 LOD 各算各的
///
/// `studiomdl` 的顺序是：先 `UnifyLODs` 把各 LOD 的**面**合并进同一个
/// `pSrc->face` 数组（`CopyFaces` 对每个 LOD 都调一次），再
/// `CalcTangentSpaces()` 遍历 `g_model[modelID]->source`。
/// 也就是说**切线是在合并后的面上累加的** —— 一个被 LOD 0 和 LOD 1 共用的
/// 顶点，它的切线同时包含两个 LOD 的三角形贡献。
///
/// 按 LOD 分别累加会给出不同的结果（法线贴图在 LOD 切换时会出现接缝），
/// 所以这里严格按「LOD 0 的三角形在前、依次追加」的顺序累加。
///
/// 返回与 `layout.vertices` 等长的切线表。
pub fn tangents_for_layout(meshes: &[MeshLods], layout: &LodLayout) -> Vec<VvdTangent> {
    // 统一池下标 → 累加器下标。排序池里第 k 项对应 (mesh, vert)。
    let mut acc_s = vec![[0.0f32; 3]; layout.vertices.len()];
    let mut acc_t = vec![[0.0f32; 3]; layout.vertices.len()];
    // 直接用布局里已经建好的反查表。
    let pos_of = &layout.pos_of;

    // 按 LOD 升序、LOD 内按三角形顺序累加（与 CopyFaces + CalcModelTangentSpaces 一致）。
    let num_lods = layout.num_lods;
    for n in 0..num_lods {
        for (mi, m) in meshes.iter().enumerate() {
            let Some(tris) = m.triangles.get(n) else {
                continue;
            };
            for tri in tris {
                // `MeshLods::triangles[n]` 里的下标**已经是统一池下标**
                // （`unify_lods` 在构造时就改写过了），所以直接用，
                // 不能再过一次 `lod_vertex_index` —— 那会把统一池下标
                // 当成局部下标二次翻译，导致越界或错位。
                let Some(&p0) = pos_of.get(&(mi, tri[0])) else {
                    continue;
                };
                let Some(&p1) = pos_of.get(&(mi, tri[1])) else {
                    continue;
                };
                let Some(&p2) = pos_of.get(&(mi, tri[2])) else {
                    continue;
                };
                let (v0, v1, v2) = (
                    &layout.vertices[p0],
                    &layout.vertices[p1],
                    &layout.vertices[p2],
                );
                let (s, t) = crate::tangent::triangle_tangent_space(
                    v0.pos, v1.pos, v2.pos, v0.uv, v1.uv, v2.uv,
                );
                for &p in &[p0, p1, p2] {
                    for k in 0..3 {
                        acc_s[p][k] += s[k];
                        acc_t[p][k] += t[k];
                    }
                }
            }
        }
    }

    layout
        .vertices
        .iter()
        .enumerate()
        .map(|(i, v)| crate::tangent::orthonormalize(acc_s[i], acc_t[i], v.normal))
        .collect()
}

/// 把一个 IR 顶点转成 VVD 顶点（权重排序 + 截断到 3 根骨骼）。
///
/// 权重按从大到小排序 —— 引擎的硬件蒙皮路径假定槽位 0 是主骨骼。
pub fn to_vvd_vertex(v: &Vertex) -> crate::vvd::VvdVertex {
    let mut weight = [0.0f32; 3];
    let mut bone = [0u8; 3];
    let mut pairs: Vec<(u8, f32)> = v.bones.iter().map(|p| (p[0] as u8, p[1])).collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (i, (b, w)) in pairs.iter().take(3).enumerate() {
        bone[i] = *b;
        weight[i] = *w;
    }
    crate::vvd::VvdVertex {
        weight,
        bone,
        bone_count: pairs.len().min(3) as u8,
        position: v.pos,
        normal: v.normal,
        tex_coord: v.uv,
    }
}

/// 单 LOD 的 VVD：顶点按 mesh 顺序摊平，**切线按真实三角形计算**。
///
/// # 顶点顺序与旧实现完全一致
///
/// 顶点块就是「bodypart → model → mesh → mesh 内顶点」的自然顺序
/// （与 [`crate::mdl_writer::flatten_vertices`] 同一套编号），
/// 所以除切线块以外的字节与「切线用占位值」的旧实现**逐字节相同**。
///
/// 切线按**每个 mesh 单独**计算（`studiomdl` 的 `vertToTriMap` 也是 per-mesh 的），
/// 不是全局一起算 —— 跨 mesh 累加会把不相关的面混进切线空间。
pub fn build_single_lod_vvd(
    compiled: &crate::model::CompiledModelDesc,
    checksum: i32,
) -> Result<crate::vvd::Vvd, crate::vvd::VvdError> {
    let mut vertices = Vec::new();
    let mut tangents = Vec::new();
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                // 该 mesh 的切线：用它的三角形与顶点算。
                let t = crate::tangent::tangents_for_mesh(&mesh.vertices, &mesh.triangles);
                for (v, tan) in mesh.vertices.iter().zip(t) {
                    vertices.push(to_vvd_vertex(v));
                    tangents.push(tan);
                }
            }
        }
    }
    crate::vvd::Vvd::from_vertices(checksum, vertices, Some(tangents))
}

/// 多 LOD 的 VVD + 布局：顶点池跨 LOD 去重、按 LOD 归属排序，并生成 fixup 表。
///
/// 返回 `(vvd, layout)`。`layout` 里的 `final_mesh_vert_id` 是 VTX 重映射
/// 与 MDL 字段填充的依据。
///
/// # 顶点池是**全局**的，不是 per-model
///
/// 实测 53 个真实 fixup 模型：`fixup.sourceVertexID` 是**相对整个顶点块**
/// 的绝对下标（`studio.h` 1867 行注释原文：
/// "absolute index from start of vertex/tangent blocks"），
/// 所以排序与分段必须在**全部 bodypart/model/mesh 之上**统一做，
/// 不能按 model 分开 —— 分开会让 `sourceVertexID` 与全局下标错位。
pub fn build_multi_lod_vvd(
    compiled: &crate::model::CompiledModelDesc,
    checksum: i32,
) -> Result<(crate::vvd::Vvd, LodLayout), crate::vvd::VvdError> {
    // 收集全部 mesh 的 LOD 数据（全局顺序 == MDL 的 mesh 顺序）。
    let mut all: Vec<MeshLods> = Vec::new();
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            let Some(lods) = &m.lods else {
                // 有的 model 没有 LOD 数据 → 用单 LOD 退化形态，保持 mesh 数一致。
                for mesh in &m.meshes {
                    all.push(MeshLods::single(
                        mesh.vertices.clone(),
                        mesh.triangles.clone(),
                    ));
                }
                continue;
            };
            all.extend(lods.meshes.iter().cloned());
        }
    }

    let layout = build_lod_layout(&all);
    let tangents = tangents_for_layout(&all, &layout);
    let vertices: Vec<crate::vvd::VvdVertex> = layout.vertices.iter().map(to_vvd_vertex).collect();

    let mut vvd = crate::vvd::Vvd::from_vertices_lods(
        checksum,
        vertices,
        Some(tangents),
        layout.num_lods as i32,
    )?;
    vvd.set_lod_counts(&layout.num_lod_vertexes);
    vvd.fixups = layout.fixups.clone();
    // 头部偏移必须**跟着 fixup 表一起重算** —— 有 fixup 时顶点块被推到
    // `ALIGN16(64 + numFixups*12)`，不再是 64。不重算会让
    // `check_invariants` 报「vertexDataStart 应为 112，实际为 64」，
    // 也会让引擎把 fixup 表当成顶点读。
    vvd.recompute_offsets();
    Ok((vvd, layout))
}

/// 按 `compiled` 里有没有 LOD 数据，自动分派单 LOD / 多 LOD 两条路径。
///
/// 判据 = 「**任一** model 带 LOD 且 `num_lods > 1`」（见
/// [`crate::model::ModelLods::is_multi`]）。多 LOD 时丢弃 [`LodLayout`]
/// —— 只想要 VVD 字节的调用者不需要它；需要 `layout` 的
/// （MDL 字段填充 / VTX 重映射）直接调 [`build_multi_lod_vvd`]。
pub fn build_vvd(
    compiled: &crate::model::CompiledModelDesc,
    checksum: i32,
) -> Result<crate::vvd::Vvd, crate::vvd::VvdError> {
    let multi = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .any(|m| m.lods.as_ref().is_some_and(|l| l.is_multi()));
    if multi {
        let (vvd, _layout) = build_multi_lod_vvd(compiled, checksum)?;
        Ok(vvd)
    } else {
        build_single_lod_vvd(compiled, checksum)
    }
}

/// 一个 model 在**多 LOD** 下写 MDL 所需的字段。
///
/// # 为什么 MDL 也要跟着变
///
/// 多 LOD 时 VVD 顶点块是「跨 LOD 去重并按 LOD 排序」的池，
/// `mstudiomodel_t.vertexindex` / `mstudiomesh_t.vertexoffset` 指向的是
/// **fixup 还原后**的 mesh 顺序，所以：
///
/// - `mesh.numvertices` = 该 mesh **跨全部 LOD 去重后**的顶点总数
///   （= Σ|block|），不是 LOD 0 的顶点数；
/// - `mesh.vertexoffset` 按这个总数累加；
/// - `model.numvertices` = 各 mesh 之和；
/// - `model.vertexindex` = fixup 后各 model 顶点总数的累计字节偏移。
///
/// 实测依据：53/53 个真实 fixup 模型的
/// `MDL.mesh.numvertices == Σ block 长度`（`probe_vvd_lod_model.js` 的 R6），
/// 且 `Σ MDL.model.numvertices == VVD.numLODVertexes[0]`（3302/3302）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVertexLayout {
    /// `mstudiomodel_t.numvertices`。
    pub model_num_vertices: usize,
    /// `mstudiomodel_t.vertexindex`（相对 VVD 顶点块的**字节**偏移）。
    pub model_vertex_index: usize,
    /// 每个 mesh 的 `(numvertices, vertexoffset)`，顺序 == MDL 的 mesh 顺序。
    /// `vertexoffset` 是**相对该 model** 的顶点下标。
    pub meshes: Vec<(usize, usize)>,
    /// 每个 mesh 的 `numLODVertexes[0..num_lods]`（**累计值**）。
    ///
    /// `[n]` = 该 mesh 中「最高 LOD 位 >= n」的顶点数 = `Σ_{k>=n} |block_m[k]|`，
    /// 单调不增。写在 `mstudiomesh_t` 偏移 **0x34**
    /// （即 `mstudio_meshvertexdata_t.numLODVertexes[8]`），
    /// 运行时按它裁剪该 mesh 在 LOD n 的顶点读取范围。
    ///
    /// 实测依据：**448/448** 个多 LOD 模型的 mesh 满足这条
    /// （`probe_mesh_numlodvertexes.js`），且
    /// `Σ_mesh mesh.numLODVertexes[n] == VVD.numLODVertexes[n]`。
    pub mesh_lod_counts: Vec<Vec<i32>>,
}

/// 按 [`LodLayout`] 算出各 model 在 MDL 里该填的顶点字段。
///
/// `mesh_counts` 是各 model 的 mesh 数（顺序与 MDL 的 bodypart/model 一致）。
///
/// Vertex offsets address the mesh-ordered array produced by VVD fixups,
/// rather than the physical LOD-sorted vertex array stored in the file.
pub fn model_vertex_layout(layout: &LodLayout, mesh_counts: &[usize]) -> Vec<ModelVertexLayout> {
    let num_lods = layout.num_lods;
    let mut out = Vec::with_capacity(mesh_counts.len());
    let mut mesh_cursor = 0usize;
    let mut vertex_cursor = 0usize;
    for &n in mesh_counts {
        let mut meshes = Vec::with_capacity(n);
        let mut lod_counts = Vec::with_capacity(n);
        let mut model_total = 0usize;
        for k in 0..n {
            let mb = layout.meshes.get(mesh_cursor + k);
            let total = mb.map_or(0, |mb| mb.total_vertexes);
            meshes.push((total, model_total));
            model_total += total;
            // 每个 LOD 的**累计**顶点数：Σ_{j>=n} |block_m[j]|。
            let mut row = Vec::with_capacity(num_lods);
            for n2 in 0..num_lods {
                let s: usize = mb.map_or(0, |mb| {
                    mb.blocks
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j >= n2)
                        .filter_map(|(_, b)| *b)
                        .map(|b| b.len)
                        .sum()
                });
                row.push(s as i32);
            }
            lod_counts.push(row);
        }
        out.push(ModelVertexLayout {
            model_num_vertices: model_total,
            model_vertex_index: vertex_cursor * crate::vvd::VERTEX_SIZE,
            meshes,
            mesh_lod_counts: lod_counts,
        });
        mesh_cursor += n;
        vertex_cursor += model_total;
    }
    out
}

// ===========================================================================
// `$lod` 的骨骼重映射（`bonetreecollapse` / `replacebone`）
// ===========================================================================

/// 把 `bonetreecollapse X` 展开成 `replacebone 子 -> X`（对 X 的**全部后代**）。
///
/// # 语义（`UnifyLODs.cpp:1547-1595`）
///
/// ```cpp
/// static void ReplaceBonesRecursive( int globalBoneID, bool replaceThis, ... )
/// {
///     if( replaceThis ) { ...push( g_bonetable[globalBoneID].name -> replacementName ); }
///     for( i ) if( g_bonetable[i].parent == globalBoneID )
///         ReplaceBonesRecursive( i, true, ... );      // ← 子节点一律 true
/// }
/// // 入口传 false —— **自己不被替换**，只有后代被替换
/// ReplaceBonesRecursive( i, false, boneReplacements, g_bonetable[i].name );
/// ```
///
/// ⚠️ **所以 `bonetreecollapse` 作用在叶子上是 no-op。**
/// 受控实验（`lodcbc`，`tip` 是叶子）：`numLODVertexes=[9,9]`，
/// 与「空 `$lod {}` 块」完全相同 —— 一个顶点都没动。
/// 而作用在有子节点的骨骼上才会改写（`lodcbm`：`[12,9]`，stdout
/// `Lod 1: vertexes: 12 (3 new)`）。
///
/// 返回 `(src, dst)` 对，`dst` 是**祖先**下标。
pub fn expand_bone_tree_collapses(roots: &[usize], parents: &[i32]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for &root in roots {
        // 深度优先，把 root 的每个后代都记成「后代 -> root」。
        let mut stack: Vec<usize> = (0..parents.len())
            .filter(|&i| parents[i] == root as i32)
            .collect();
        while let Some(child) = stack.pop() {
            out.push((child, root));
            for (i, &p) in parents.iter().enumerate() {
                if p == child as i32 {
                    stack.push(i);
                }
            }
        }
    }
    out
}

/// 把替换链折叠到末端（`FixupReplacedBonesForLOD`，`UnifyLODs.cpp:1610`）。
///
/// # 为什么必须做
///
/// ```cpp
/// do {
///     changed = false;
///     for i, j:
///         if( replacements[i].src == replacements[j].dst ) {
///             replacements[j].dst = replacements[i].dst;   // 接到链的末端
///             changed = true;
///         }
/// } while( changed );
/// ```
///
/// 即 `A→B`、`B→C` 折叠成 `A→C`、`B→C`。
/// `BuildBoneLODMapping` 是**单趟**查表（`boneMap[j] = k`），
/// 不折叠的话 `A→B` 会停在中间节点 `B`，而官方会一路走到 `C`。
pub fn fixup_replaced_bones(replacements: &mut [(usize, usize)]) {
    loop {
        let mut changed = false;
        for i in 0..replacements.len() {
            for j in 0..replacements.len() {
                if i == j {
                    continue;
                }
                if replacements[i].0 == replacements[j].1 {
                    replacements[j].1 = replacements[i].1;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// 构造某一档的骨骼映射表（`BuildBoneLODMapping`，`UnifyLODs.cpp:1260`）。
///
/// 先建**恒等映射**，再把每条替换 `src→dst` 写进去。
/// 找不到的骨骼**静默跳过**（官方只在 `g_verbose` 下打 warning）。
pub fn build_bone_lod_mapping(n_bones: usize, replacements: &[(usize, usize)]) -> Vec<usize> {
    let mut map: Vec<usize> = (0..n_bones).collect();
    for &(src, dst) in replacements {
        if src < n_bones && dst < n_bones {
            map[src] = dst;
        }
    }
    map
}

/// 把顶点权重按 `bone_map` 重定向（`RemapBoneWeights`，`UnifyLODs.cpp:779`）。
///
/// `bone_map` 为空 = 恒等映射。
pub fn remap_bone_weights(v: &mut Vertex, bone_map: &[usize]) {
    if bone_map.is_empty() {
        return;
    }
    for p in v.bones.iter_mut() {
        let b = p[0] as usize;
        if b < bone_map.len() {
            p[0] = bone_map[b] as f32;
        }
    }
}

/// 合并同骨骼的权重并按权重降序排序（`CollapseBoneWeights` + `SortBoneWeightByWeight`）。
///
/// # 顺序（`UnifyLODs.cpp:846-872` + `807-820`）
///
/// 1. **先按骨骼下标排序**（`SortBoneWeightByIndex`）—— 合并的前提；
/// 2. 相邻同骨骼的权重**相加**，`numbones` 递减（要**回退一步**，
///    因为可能有多根骨骼塌进同一根）；
/// 3. **再按权重降序排序**（`SortBoneWeightByWeight`，bubble sort，
///    `weight[k] >= weight[k+1]` 时 `continue` ⟹ 稳定且大者在前）。
///
/// 落盘时 VVD 的 `weight[]`/`bone[]` 就是这个顺序，所以**必须一致**。
pub fn collapse_and_sort_bone_weights(v: &mut Vertex) {
    // 1) 按骨骼下标排序（f32 里存的是整数下标，可直接比较）。
    v.bones
        .sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
    // 2) 合并同骨骼。
    let mut i = 0;
    while i + 1 < v.bones.len() {
        if v.bones[i][0] == v.bones[i + 1][0] {
            v.bones[i][1] += v.bones[i + 1][1];
            v.bones.remove(i + 1);
            // 回退一步：可能有多根骨骼塌进同一根。
            i = i.saturating_sub(1);
        } else {
            i += 1;
        }
    }
    // 3) 按权重降序（稳定：相等时保持下标序）。
    v.bones
        .sort_by(|a, b| b[1].partial_cmp(&a[1]).unwrap_or(std::cmp::Ordering::Equal));
}

#[derive(Debug, Clone, PartialEq)]
struct DictVert {
    v: Vertex,
    tangent: VvdTangent,
    lod_flags: LodFlags,
}

// Exact matches share a grid cell; unindexable coordinates use a linear scan.
struct PosIndex {
    buckets: HashMap<(i64, i64, i64), Vec<u32>>,
}

const CELL_SIZE: f64 = 0.2;
const CELL_LIMIT: f64 = 1.0e9;

#[inline]
fn cell_of(p: [f32; 3]) -> Option<(i64, i64, i64)> {
    let mut out = [0i64; 3];
    for k in 0..3 {
        let c = p[k];
        if !c.is_finite() {
            return None;
        }
        let v = c as f64;
        if v.abs() > CELL_LIMIT {
            return None;
        }
        out[k] = (v / CELL_SIZE).floor() as i64;
    }
    Some((out[0], out[1], out[2]))
}

impl PosIndex {
    fn new() -> Self {
        PosIndex {
            buckets: HashMap::new(),
        }
    }

    fn insert(&mut self, i: usize, p: [f32; 3]) {
        if let Some(cell) = cell_of(p) {
            self.buckets.entry(cell).or_default().push(i as u32);
        }
    }

    fn find_exact(
        &self,
        pool: &[DictVert],
        start: usize,
        end: usize,
        v: &Vertex,
        t: &VvdTangent,
    ) -> Option<usize> {
        let end = end.min(pool.len());
        if start >= end {
            return None;
        }
        let Some(c) = cell_of(v.pos) else {
            return pick_exact(pool, start..end, v, t);
        };
        let candidates = self
            .buckets
            .get(&c)
            .into_iter()
            .flatten()
            .copied()
            .map(|index| index as usize)
            .filter(|&index| (start..end).contains(&index));
        pick_exact(pool, candidates, v, t)
    }
}

/// 权重**集合**是否相同（`AreBoneWeightsEqual`，`UnifyLODs.cpp:971`）。
///
/// 与顺序无关：把 `b1` 的每根骨骼在 `b2` 里找同名项，要求**全部找到**
/// 且权重**逐位相等**。`numbones` 也必须相同。
fn bone_weights_equal(b1: &[[f32; 2]], b2: &[[f32; 2]]) -> bool {
    if b1.len() != b2.len() {
        return false;
    }
    let mut matched = 0;
    for p in b1 {
        for q in b2 {
            if q[0] == p[0] {
                if q[1] != p[1] {
                    return false;
                }
                matched += 1;
                break;
            }
        }
    }
    matched == b1.len()
}

/// `FindVertexInDictionaryExact`（`UnifyLODs.cpp:1016`）的判定体。
///
/// **逐位**比较位置 / 权重集合 / UV / 法线 / 切线 —— 没有 epsilon。
/// 顺序是 `for nVertID in start..end`，**第一个匹配者获胜**。
fn pick_exact(
    pool: &[DictVert],
    cands: impl Iterator<Item = usize>,
    v: &Vertex,
    t: &VvdTangent,
) -> Option<usize> {
    for i in cands {
        let c = &pool[i];
        if c.v.pos != v.pos {
            continue;
        }
        if !bone_weights_equal(&c.v.bones, &v.bones) {
            continue;
        }
        if c.v.uv != v.uv {
            continue;
        }
        if c.v.normal != v.normal {
            continue;
        }
        if c.tangent != *t {
            continue;
        }
        return Some(i);
    }
    None
}

fn find_exact_in_range_naive(
    pool: &[DictVert],
    start: usize,
    end: usize,
    v: &Vertex,
    t: &VvdTangent,
) -> Option<usize> {
    let end = end.min(pool.len());
    if start >= end {
        return None;
    }
    pick_exact(pool, start..end, v, t)
}

/// 一个 LOD 档的输入：源网格 + 该档的骨骼映射。
#[derive(Debug, Clone, PartialEq)]
pub struct LodSource {
    pub vertices: Vec<Vertex>,
    pub triangles: Vec<[u32; 3]>,
    /// `bone_map[i]` = 骨骼 `i` 在本档映射到的骨骼下标。**空 = 恒等**。
    pub bone_map: Vec<usize>,
}

impl LodSource {
    /// 恒等映射（等价于旧的 `unify_lods` 的一档）。
    pub fn new(vertices: Vec<Vertex>, triangles: Vec<[u32; 3]>) -> Self {
        Self {
            vertices,
            triangles,
            bone_map: Vec::new(),
        }
    }
}

/// Unify LOD vertices without changing supplied geometry or bone weights.
/// Explicit per-LOD bone replacements are applied before exact attribute matching.
pub fn unify_lods_remapped(lods: &[LodSource], bone_usage: &mut [LodFlags]) -> MeshLods {
    unify_lods_impl::<true>(lods, bone_usage)
}

#[cfg(test)]
pub(crate) fn unify_lods_remapped_naive(
    lods: &[LodSource],
    bone_usage: &mut [LodFlags],
) -> MeshLods {
    unify_lods_impl::<false>(lods, bone_usage)
}

/// `USE_INDEX = true` 走位置桶索引，`false` 走原来的线性扫描。
fn unify_lods_impl<const USE_INDEX: bool>(
    lods: &[LodSource],
    bone_usage: &mut [LodFlags],
) -> MeshLods {
    let num_lods = lods.len().max(1);
    let mut pool: Vec<DictVert> = Vec::new();
    let mut triangles: Vec<Vec<[u32; 3]>> = Vec::with_capacity(num_lods);
    let mut lod_vertex_index: Vec<Vec<u32>> = Vec::with_capacity(num_lods);

    // ---- LOD 0：CopyVerts（**不去重**，逐条追加）----
    let l0 = &lods[0];
    let l0_tan = crate::tangent::tangents_for_mesh(&l0.vertices, &l0.triangles);
    let root_start = pool.len();
    for (i, v) in l0.vertices.iter().enumerate() {
        let mut nv = v.clone();
        // `AddVertexFromSource` 会 `SortBoneWeightByIndex`。
        nv.bones
            .sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));
        pool.push(DictVert {
            v: nv,
            tangent: l0_tan.get(i).copied().unwrap_or(VvdTangent {
                xyz: [0.0; 3],
                w: -1.0,
            }),
            lod_flags: 1,
        });
    }
    let root_end = pool.len();

    // `MarkRootLODBones`：恒等重映射 + 折叠 + 按权重排序 + 标记 LOD0。
    for d in pool.iter_mut().take(root_end).skip(root_start) {
        let mut nv = d.v.clone();
        collapse_and_sort_bone_weights(&mut nv);
        for p in &nv.bones {
            let b = p[0] as usize;
            if b < bone_usage.len() {
                bone_usage[b] |= 1;
            }
        }
        d.v = nv;
    }

    // LOD 0 的三角形下标**已经是池下标**（CopyVerts 是恒等追加）。
    triangles.push(l0.triangles.clone());
    lod_vertex_index.push((root_start..root_end).map(|i| i as u32).collect());

    let mut index = PosIndex::new();
    if USE_INDEX && num_lods > 1 {
        for (i, d) in pool.iter().enumerate() {
            index.insert(i, d.v.pos);
        }
    }
    #[inline]
    fn lookup_exact<const USE_INDEX: bool>(
        index: &mut PosIndex,
        pool: &[DictVert],
        start: usize,
        end: usize,
        v: &Vertex,
        t: &VvdTangent,
    ) -> Option<usize> {
        if USE_INDEX {
            index.find_exact(pool, start, end, v, t)
        } else {
            find_exact_in_range_naive(pool, start, end, v, t)
        }
    }

    // ---- LOD 1..N：CreateLODVertsInDictionary ----
    for (n, src) in lods.iter().enumerate().take(num_lods).skip(1) {
        let prev_count = pool.len();
        let src_tan = crate::tangent::tangents_for_mesh(&src.vertices, &src.triangles);
        let zero_tan = VvdTangent {
            xyz: [0.0; 3],
            w: -1.0,
        };
        let mut remap_ids: Vec<u32> = Vec::with_capacity(src.vertices.len());

        for (ci, cand) in src.vertices.iter().enumerate() {
            let cand_t = src_tan.get(ci).copied().unwrap_or(zero_tan);

            let mut ideal = DictVert {
                v: cand.clone(),
                tangent: cand_t,
                lod_flags: 0,
            };

            let bit: LodFlags = 1u32 << n;
            hotpath::measure_block!("    unify: remap+collapse", {
                remap_bone_weights(&mut ideal.v, &src.bone_map);
                collapse_and_sort_bone_weights(&mut ideal.v);
                for p in &ideal.v.bones {
                    let b = p[0] as usize;
                    if b < bone_usage.len() {
                        bone_usage[b] |= bit;
                    }
                }
                ideal.lod_flags = bit;
            });

            let id = hotpath::measure_block!("    unify: exact match", {
                match lookup_exact::<USE_INDEX>(
                    &mut index,
                    &pool,
                    0,
                    prev_count,
                    &ideal.v,
                    &ideal.tangent,
                ) {
                    Some(k) => {
                        pool[k].lod_flags |= bit;
                        k
                    }
                    None => {
                        let k = pool.len();
                        if USE_INDEX {
                            index.insert(k, ideal.v.pos);
                        }
                        pool.push(ideal);
                        k
                    }
                }
            });
            remap_ids.push(id as u32);
        }

        // 三角形下标：源顶点号 → 池下标。
        let mut remapped: Vec<[u32; 3]> = Vec::with_capacity(src.triangles.len());
        for t in &src.triangles {
            let mut out = [u32::MAX; 3];
            let mut ok = true;
            for (k, &u) in t.iter().enumerate() {
                match remap_ids.get(u as usize) {
                    Some(&p) => out[k] = p,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                remapped.push(out);
            }
        }
        triangles.push(remapped);
        lod_vertex_index.push(remap_ids);
    }

    // 孤立顶点（哪个 LOD 都没引用）强制归到最低细节 LOD
    // （`write.cpp` 2592 行）。
    let lowest = 1u32 << (num_lods.max(1) - 1);
    for d in pool.iter_mut() {
        if d.lod_flags == 0 {
            d.lod_flags = lowest;
        }
    }

    let vertices: Vec<Vertex> = pool.iter().map(|d| d.v.clone()).collect();
    let lod_flags: Vec<LodFlags> = pool.iter().map(|d| d.lod_flags).collect();
    let lod_vertex_counts: Vec<usize> = (0..num_lods)
        .map(|n| {
            let bit = 1u32 << n;
            lod_flags.iter().filter(|f| **f & bit != 0).count()
        })
        .collect();

    MeshLods {
        vertices,
        lod_flags,
        triangles,
        lod_vertex_counts,
        lod_vertex_index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pos: [f32; 3], uv: [f32; 2]) -> Vertex {
        Vertex {
            pos,
            normal: [0.0, 0.0, 1.0],
            uv,
            bones: vec![[0.0, 1.0]],
        }
    }

    #[test]
    fn lower_lod_preserves_supplied_normals_uvs_and_weights() {
        let root = Vertex {
            pos: [1.0, 2.0, 3.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.2, 0.3],
            bones: vec![[0.0, 1.0]],
        };
        let lower = Vertex {
            normal: [0.0, 0.6, 0.8],
            uv: [0.21, 0.31],
            bones: vec![[1.0, 1.0]],
            ..root.clone()
        };
        let mut usage = vec![0; 2];
        let output = unify_lods_remapped(
            &[
                LodSource::new(vec![root.clone()], vec![[0, 0, 0]]),
                LodSource::new(vec![lower.clone()], vec![[0, 0, 0]]),
            ],
            &mut usage,
        );
        assert_eq!(output.vertices, vec![root.clone(), lower.clone()]);
        assert_eq!(output.triangles[1], vec![[1, 1, 1]]);
        assert_eq!(usage, vec![1, 2]);
        let mut source = LodSource::new(vec![lower.clone()], vec![[0, 0, 0]]);
        source.bone_map = vec![0, 0];
        let output = unify_lods_remapped(
            &[LodSource::new(vec![root], vec![[0, 0, 0]]), source],
            &mut [0; 2],
        );
        assert_eq!(output.vertices[1].bones, vec![[0.0, 1.0]]);
        assert_eq!(output.vertices[1].normal, lower.normal);
        assert_eq!(output.vertices[1].uv, lower.uv);
    }

    #[test]
    fn model_vertex_offsets_address_fixup_order_with_multiple_meshes_and_blank_choices() {
        let meshes: Vec<_> = (0..3)
            .map(|mesh| {
                let high = vec![v([mesh as f32 * 10.0, 0.0, 0.0], [0.0; 2])];
                let low = vec![v([mesh as f32 * 10.0 + 1.0, 0.0, 0.0], [0.0; 2])];
                unify_lods(&[(high, vec![[0; 3]]), (low, vec![[0; 3]])])
            })
            .collect();
        let layout = build_lod_layout(&meshes);
        assert_eq!(layout.order[0], (0, 1));
        assert_eq!(layout.order[1], (1, 1));
        let models = model_vertex_layout(&layout, &[2, 0, 1]);
        assert_eq!(models[0].model_num_vertices, 4);
        assert_eq!(models[0].model_vertex_index, 0);
        assert_eq!(models[0].meshes, [(2, 0), (2, 2)]);
        assert_eq!(models[1].model_num_vertices, 0);
        assert_eq!(models[1].model_vertex_index, 4 * crate::vvd::VERTEX_SIZE);
        assert_eq!(models[2].model_num_vertices, 2);
        assert_eq!(models[2].model_vertex_index, 4 * crate::vvd::VERTEX_SIZE);
        assert_eq!(models[2].meshes, [(2, 0)]);
    }

    /// 单 LOD 时排序必须是恒等变换 —— 这是「不破坏现有产物」的依据。
    #[test]
    fn single_lod_layout_is_identity() {
        // 两个 mesh，顶点数不同，检验跨 mesh 的连续编号。
        let m0 = MeshLods::single(
            vec![v([0.0; 3], [0.0, 0.0]), v([1.0, 0.0, 0.0], [1.0, 0.0])],
            vec![[0, 1, 0]],
        );
        let m1 = MeshLods::single(vec![v([2.0, 0.0, 0.0], [0.0, 0.0])], vec![]);
        let l = build_lod_layout(&[m0, m1]);
        assert_eq!(l.num_lods, 1);
        assert_eq!(l.num_lod_vertexes, vec![3], "单 LOD 的顶点数应为总数");
        assert!(l.fixups.is_empty(), "单 LOD 不应有 fixup");
        // 顺序必须是 (0,0),(0,1),(1,0)
        assert_eq!(l.order, vec![(0, 0), (0, 1), (1, 0)]);
        // 每个 mesh 的 finalMeshVertID 就是 mesh 内序号
        assert_eq!(l.meshes[0].final_mesh_vert_id[&0], 0);
        assert_eq!(l.meshes[0].final_mesh_vert_id[&1], 1);
        assert_eq!(l.meshes[1].final_mesh_vert_id[&2], 0);
        assert_eq!(l.meshes[0].total_vertexes, 2);
        assert_eq!(l.meshes[1].total_vertexes, 1);
    }

    /// 多 LOD：LOD 0 的顶点排在后面，LOD 1 独占的排在前面。
    ///
    /// 同时钉住 `write.cpp` 2777 行的规则：**单 mesh 时不做 fixup**。
    /// 实测依据：177 个「多 LOD 且单 mesh」的真实模型
    /// `numFixups` **全部为 0**；53 个「多 LOD 且多 mesh」的全部非 0
    /// （`docs/_probe/dbg_singlemesh_lod.js`）。
    #[test]
    fn higher_lod_vertices_sort_first() {
        // LOD 0 有 3 个顶点，LOD 1 只有前 2 个（共享）。
        let m = unify_lods(&[
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
            (
                vec![v([0.0; 3], [0.0, 0.0]), v([1.0, 0.0, 0.0], [1.0, 0.0])],
                vec![[0, 1, 0]],
            ),
        ]);
        // 顶点 0、1 被两个 LOD 共用 → 掩码 0b11，最高位 1
        // 顶点 2 只被 LOD 0 用 → 掩码 0b01，最高位 0
        assert_eq!(m.lod_flags, vec![0b11, 0b11, 0b01]);
        assert_eq!(m.num_lods(), 2);

        let l = build_lod_layout(&[m]);
        // 最高位 1 的两个顶点在前，最高位 0 的在后
        assert_eq!(l.order, vec![(0, 0), (0, 1), (0, 2)]);
        // numLODVertexes[0] = 全部 3 个；[1] = 最高位>=1 的 2 个
        assert_eq!(l.num_lod_vertexes, vec![3, 2]);
        // 单 mesh → 按 `write.cpp` 2777 行**不做** fixup（数据本来就连续）。
        assert!(
            l.fixups.is_empty(),
            "单 mesh 不应产生 fixup 表：{:?}",
            l.fixups
        );
        // 但分段信息仍然要在（它是 numLODVertexes 与 VTX 重映射的依据）。
        assert_eq!(l.meshes[0].blocks[1], Some(Block { start: 0, len: 2 }));
        assert_eq!(l.meshes[0].blocks[0], Some(Block { start: 2, len: 1 }));
    }

    /// **多 mesh** 时 fixup 表才出现，且按 mesh 分组、组内 LOD 从粗到细。
    /// 与实测的 53 个真实模型的表结构一致。
    #[test]
    fn multi_mesh_produces_fixups_grouped_per_mesh() {
        // 两个 mesh，各 3 个顶点、2 个 LOD（LOD 1 复用前 2 个）。
        let mk = |tag: f32| {
            let verts: Vec<Vertex> = (0..3)
                .map(|i| v([tag, i as f32, 0.0], [i as f32, 0.0]))
                .collect();
            let lod1: Vec<Vertex> = verts[..2].to_vec();
            unify_lods(&[(verts, vec![[0, 1, 2]]), (lod1, vec![[0, 1, 0]])])
        };
        let l = build_lod_layout(&[mk(0.0), mk(10.0)]);
        assert_eq!(l.num_lods, 2);
        assert_eq!(
            l.fixups.len(),
            4,
            "2 个 mesh × 2 个 LOD = 4 条：{:?}",
            l.fixups
        );

        // 按 mesh 分组：每 2 条一组，组内 lod 递减（1 然后 0）。
        for g in l.fixups.chunks(2) {
            assert_eq!(g[0].lod, 1, "每组第一条应是最粗的 LOD：{g:?}");
            assert_eq!(g[1].lod, 0);
            // 同一 mesh 的两条区间应拼成该 mesh 的完整顶点数 3。
            assert_eq!(
                (g[0].num_vertexes + g[1].num_vertexes) as usize,
                l.meshes[0].total_vertexes
            );
        }
        // 两个 mesh 的区间不能重叠（精确铺满池）。
        let mut covered = vec![false; l.order.len()];
        for f in &l.fixups {
            let s = f.source_vertex_id as usize;
            let n = f.num_vertexes as usize;
            for (k, slot) in covered.iter_mut().enumerate().skip(s).take(n) {
                assert!(!*slot, "区间重叠 @{k}");
                *slot = true;
            }
        }
        assert!(covered.iter().all(|c| *c), "fixup 应精确铺满顶点池");
    }

    /// 不变式：各 block 精确铺满 `[0, numLODVertexes[0])`，不重叠无空洞。
    /// （对应实测的 R1。）
    #[test]
    fn blocks_tile_the_pool_exactly() {
        let a = unify_lods(&[
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                    v([1.0, 1.0, 0.0], [1.0, 1.0]),
                ],
                vec![[0, 1, 2], [1, 3, 2]],
            ),
            (
                vec![v([0.0; 3], [0.0, 0.0]), v([1.0, 1.0, 0.0], [1.0, 1.0])],
                vec![[0, 1, 0]],
            ),
        ]);
        let b = unify_lods(&[
            (
                vec![v([5.0; 3], [0.0, 0.0]), v([6.0, 0.0, 0.0], [1.0, 0.0])],
                vec![[0, 1, 0]],
            ),
            (vec![v([5.0; 3], [0.0, 0.0])], vec![]),
        ]);
        let l = build_lod_layout(&[a, b]);
        let n0 = l.num_lod_vertexes[0] as usize;
        assert_eq!(n0, l.order.len(), "numLODVertexes[0] 应等于池大小");
        let mut owner = vec![None; n0];
        for (mi, mb) in l.meshes.iter().enumerate() {
            for b in mb.blocks.iter().flatten() {
                for k in 0..b.len {
                    assert!(owner[b.start + k].is_none(), "block 重叠 @{}", b.start + k);
                    owner[b.start + k] = Some(mi);
                }
            }
        }
        assert!(owner.iter().all(|o| o.is_some()), "有空洞：{owner:?}");
    }

    /// 不变式：`numLODVertexes[n] == Σ_{k>=n} 独占数`（对应实测的 R2）。
    #[test]
    fn num_lod_vertexes_is_cumulative() {
        let a = unify_lods(&[
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                    v([1.0, 1.0, 0.0], [1.0, 1.0]),
                    v([2.0, 0.0, 0.0], [2.0, 0.0]),
                ],
                vec![[0, 1, 2]],
            ),
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([2.0, 0.0, 0.0], [2.0, 0.0]),
                ],
                vec![[0, 1, 2]],
            ),
        ]);
        let l = build_lod_layout(&[a]);
        // 独占数：LOD1 = 掩码最高位 1 的顶点数，LOD0 = 最高位 0 的
        let exc1 = l.lod_flags.iter().filter(|f| q_log2(**f) == 1).count();
        let exc0 = l.lod_flags.iter().filter(|f| q_log2(**f) == 0).count();
        assert_eq!(l.num_lod_vertexes[0] as usize, exc1 + exc0);
        assert_eq!(l.num_lod_vertexes[1] as usize, exc1);
    }

    /// 不变式：排序后按池序扫描，最高 LOD 位非递增（对应实测的 R3）。
    #[test]
    fn sorted_by_highest_lod_bit_descending() {
        let a = unify_lods(&[
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
            (
                vec![v([0.0; 3], [0.0, 0.0]), v([0.0, 1.0, 0.0], [0.0, 1.0])],
                vec![[0, 1, 0]],
            ),
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                    v([3.0, 0.0, 0.0], [3.0, 0.0]),
                ],
                vec![[0, 1, 2]],
            ),
        ]);
        let l = build_lod_layout(&[a]);
        let hi: Vec<i32> = l.lod_flags.iter().map(|f| q_log2(*f)).collect();
        assert!(
            hi.windows(2).all(|w| w[0] >= w[1]),
            "最高位必须非递增：{hi:?}"
        );
    }

    /// 不变式：`MDL.mesh.numvertices == Σ block 长度`（对应实测的 R6）。
    #[test]
    fn mesh_total_is_sum_of_blocks() {
        let a = unify_lods(&[
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
            (vec![v([0.0; 3], [0.0, 0.0])], vec![]),
        ]);
        let l = build_lod_layout(&[a]);
        for mb in &l.meshes {
            let sum: usize = mb.blocks.iter().flatten().map(|b| b.len).sum();
            assert_eq!(sum, mb.total_vertexes);
            assert_eq!(mb.final_mesh_vert_id.len(), mb.total_vertexes);
        }
    }

    /// 孤立顶点（**没有任何三角形引用**）必须被强制归到最低细节 LOD，
    /// 否则掩码为 0，排序主键 `Q_log2(0)` 未定义。
    ///
    /// 「最低细节」= **最大**的 LOD 下标（`write.cpp` 2592 行：
    /// `lodFlags = 1 << (numLODs - 1)`）。两 LOD 时是 bit 1，不是 bit 0。
    ///
    /// 实测依据：230 个真实多 LOD 模型里有 **4 个确实存在孤立顶点**
    /// （`dead_male_legs_01` / `logpile2` / `airport_fuel_truck` /
    /// `light_spotlight01_lamp`，并集比 `MDL.mesh.numvertices` 少 1..14 个），
    /// 所以这条规则不是防御性代码，是真实路径
    /// （`docs/_probe/dbg_lod_orphans.js`）。
    #[test]
    fn orphan_vertex_gets_lowest_lod_flag() {
        let m = unify_lods(&[
            (
                vec![
                    v([0.0; 3], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                ],
                // 只引用 0、1，顶点 2 是孤立点
                vec![[0, 1, 0]],
            ),
            (vec![v([0.0; 3], [0.0, 0.0])], vec![]),
        ]);
        assert_eq!(
            m.lod_flags[2], 2,
            "孤立顶点应归到最低细节 LOD（两 LOD 时是 bit 1）"
        );
        let l = build_lod_layout(&[m]);
        assert_eq!(l.num_lod_vertexes[0], 3);
        // 孤立顶点仍必须被某个 block 覆盖，否则 fixup 铺不满池。
        let covered: usize = l.meshes[0].blocks.iter().flatten().map(|b| b.len).sum();
        assert_eq!(covered, 3, "孤立顶点也必须落在某个 block 里");
    }

    /// 统一池必须跨 LOD 去重：同一顶点在 LOD 0/1 都有时只存一份。
    ///
    /// `lodFlags` 按**三角形引用**推导（不是「出现在该 LOD 的顶点列表里」）——
    /// 所以这里 LOD 1 虽然列出了顶点 A，但它的三角形列表为空，
    /// A 的掩码只有 LOD 0 的那一位。理由见 `unify_lods` 的文档。
    #[test]
    fn unify_dedups_across_lods() {
        let m = unify_lods(&[
            (
                vec![
                    v([1.0, 2.0, 3.0], [0.5, 0.5]),
                    v([9.0, 9.0, 9.0], [1.0, 1.0]),
                ],
                vec![[0, 1, 0]],
            ),
            // LOD 1 复用 A，但没有任何三角形。
            (vec![v([1.0, 2.0, 3.0], [0.5, 0.5])], vec![]),
        ]);
        assert_eq!(m.vertices.len(), 2, "共用的顶点只能存一份");
        assert_eq!(
            m.lod_flags[0], 0b01,
            "A 只被 LOD 0 的三角形引用，掩码应只有 bit 0"
        );
        assert_eq!(m.lod_flags[1], 0b01);
    }

    /// 被两个 LOD 的三角形**都**引用时，掩码是并集。
    #[test]
    fn shared_vertex_used_by_two_lods_gets_union_mask() {
        let m = unify_lods(&[
            (
                vec![
                    v([0.0, 0.0, 0.0], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([1.0, 1.0, 0.0], [1.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
            (
                vec![
                    v([0.0, 0.0, 0.0], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([1.0, 1.0, 0.0], [1.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
        ]);
        assert_eq!(m.vertices.len(), 3, "两个 LOD 的顶点完全重合 → 只存一份");
        assert_eq!(
            m.lod_flags,
            vec![0b11, 0b11, 0b11],
            "掩码应是两个 LOD 的并集"
        );
    }

    /// 切线的 LOD 归属：顶点在哪个 LOD 就用哪个 LOD 的三角形累加。
    #[test]
    fn tangents_use_all_lods_triangles() {
        // LOD 0 一个三角形，LOD 1 复用同样三个顶点 + 另一个三角形。
        let a = unify_lods(&[
            (
                vec![
                    v([0.0, 0.0, 0.0], [0.0, 0.0]),
                    v([1.0, 0.0, 0.0], [1.0, 0.0]),
                    v([1.0, 1.0, 0.0], [1.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
            (
                vec![
                    v([0.0, 0.0, 0.0], [0.0, 0.0]),
                    v([1.0, 1.0, 0.0], [1.0, 1.0]),
                    v([0.0, 1.0, 0.0], [0.0, 1.0]),
                ],
                vec![[0, 1, 2]],
            ),
        ]);
        let l = build_lod_layout(std::slice::from_ref(&a));
        let t = tangents_for_layout(std::slice::from_ref(&a), &l);
        assert_eq!(t.len(), l.vertices.len());
        for tan in &t {
            // XY 平面、法线 +Z、UV 沿 +X/+Y → 切线应≈+X
            assert!(tan.xyz[0].abs() > 0.99, "切线应≈±X：{tan:?}");
        }
    }

    // -----------------------------------------------------------------------
    // `$lod` 骨骼坍缩：以下每个测试都对应一个**受控 oracle 实验**
    // （`docs\_probe\smdl\lod*.qc`，产物用 `dump_vvd_verts.js` 读回）。
    // 断言里的数字是官方 `studiomdl.exe` 的实测值，不是推导值。
    // -----------------------------------------------------------------------

    fn bv(pos: [f32; 3], uv: [f32; 2], bone: usize) -> Vertex {
        Vertex {
            pos,
            normal: [0.0, 0.0, 1.0],
            uv,
            bones: vec![[bone as f32, 1.0]],
        }
    }

    /// `bonetreecollapse X` ≡ 对 X 的**每个后代**做 `replacebone 后代 -> X`；
    /// **X 自己不被替换**（所以作用在叶子上是 no-op）。
    #[test]
    fn bone_tree_collapse_expands_to_descendants_only() {
        // root(0) <- mid(1) <- tip(2)
        let parents = [-1i32, 0, 1];
        // 作用在叶子 `tip` 上 ⟹ 没有后代 ⟹ 空（官方实测 `[9,9]` no-op）。
        assert_eq!(
            expand_bone_tree_collapses(&[2], &parents),
            Vec::<(usize, usize)>::new(),
            "叶子节点没有后代，必须是 no-op"
        );
        // 作用在 `mid` 上 ⟹ 只有 `tip` 被改写（官方实测 `[12,9]`）。
        assert_eq!(expand_bone_tree_collapses(&[1], &parents), vec![(2, 1)]);
        // 作用在 `root` 上 ⟹ `mid` 与 `tip` 都被改写（官方实测 `[15,9]`）。
        let mut got = expand_bone_tree_collapses(&[0], &parents);
        got.sort_unstable();
        assert_eq!(got, vec![(1, 0), (2, 0)]);
    }

    /// 替换链必须折叠到末端：`A→B`、`B→C` ⟹ `A→C`、`B→C`。
    ///
    /// 不折叠的话 `BuildBoneLODMapping` 的单趟查表会让 `A` 停在中间节点 `B`。
    #[test]
    fn replace_bone_chain_collapses_to_terminal() {
        let mut r = vec![(0usize, 1usize), (1, 2)];
        fixup_replaced_bones(&mut r);
        assert_eq!(r, vec![(0, 2), (1, 2)]);
        // 三条链
        let mut r3 = vec![(0usize, 1usize), (1, 2), (2, 3)];
        fixup_replaced_bones(&mut r3);
        assert_eq!(r3, vec![(0, 3), (1, 3), (2, 3)]);
    }

    #[test]
    fn empty_lod_block_preserves_distinct_bone_weights() {
        // 池顺序 = mid0,mid1,mid2, tip0,tip1,tip2
        let verts: Vec<Vertex> = (0..3)
            .map(|i| bv([i as f32, 0.0, 0.0], [i as f32, 0.0], 1))
            .chain((0..3).map(|i| bv([i as f32, 0.0, 0.0], [i as f32, 0.0], 2)))
            .collect();
        let tris = vec![[0u32, 1, 2], [3, 4, 5]];
        let mut usage = vec![0u32; 3];
        let m = unify_lods_remapped(
            &[
                LodSource::new(verts.clone(), tris.clone()),
                LodSource::new(verts, tris),
            ],
            &mut usage,
        );
        assert_eq!(m.vertices.len(), 6, "空块不应新增顶点");
        assert_eq!(m.lod_flags, vec![3; 6]);
        let layout = build_lod_layout(std::slice::from_ref(&m));
        assert_eq!(layout.num_lod_vertexes, vec![6, 6]);
        assert_eq!(m.vertices[layout.order[0].1 as usize].bones[0][0], 1.0);
        assert_eq!(usage, vec![0, 3, 3]);
    }

    /// `replacebone tip->mid`：理想顶点先取 `tip`，**重映射后**再精确查重，
    /// 于是命中 LOD0 的 `mid` 顶点 ⟹ 顺序反过来，且**不新增顶点**。
    ///
    /// 官方实测（`lodwdw`）：`numLODVertexes=[6,3]`，VVD 顺序 `mid*, tip*`。
    #[test]
    fn replace_bone_redirects_to_root_vertex() {
        let verts: Vec<Vertex> = (0..3)
            .map(|i| bv([i as f32, 0.0, 0.0], [i as f32, 0.0], 1))
            .chain((0..3).map(|i| bv([i as f32, 0.0, 0.0], [i as f32, 0.0], 2)))
            .collect();
        let tris = vec![[0u32, 1, 2], [3, 4, 5]];
        let mut usage = vec![0u32; 3];
        let m = unify_lods_remapped(
            &[
                LodSource::new(verts.clone(), tris.clone()),
                LodSource {
                    vertices: verts,
                    triangles: tris,
                    bone_map: vec![0, 1, 1], // tip(2) -> mid(1)
                },
            ],
            &mut usage,
        );
        assert_eq!(
            m.vertices.len(),
            6,
            "重映射后应与 LOD0 顶点精确重合，不新增"
        );
        assert_eq!(m.lod_flags, vec![3, 3, 3, 1, 1, 1], "LOD1 应标记在 mid 上");
        let l = build_lod_layout(std::slice::from_ref(&m));
        assert_eq!(l.num_lod_vertexes, vec![6, 3], "官方实测 [6,3]");
        let first_bone = m.vertices[l.order[0].1 as usize].bones[0][0];
        assert_eq!(first_bone, 1.0, "排在最前的应是 mid");
        // 骨骼使用位：`MarkBonesUsedByLod` 只标记**权重里出现**的骨骼，
        // **不走父链**（父链传播是之后的 `MarkParentBoneLODs` 单独一趟）。
        // 所以 root(0) 没有任何顶点绑定 ⟹ 0；
        // mid(1) 被 LOD0+LOD1 用 ⟹ 3；tip(2) 只被 LOD0 用 ⟹ 1。
        assert_eq!(
            usage,
            vec![0, 3, 1],
            "MarkBonesUsedByLod 在**重映射后**的权重上打标，且不传播到父骨骼"
        );
    }

    /// ⚠️ **切线参与精确查重** —— 这是 `loddtan` 实验的指纹。
    ///
    /// 两个顶点的位置/UV/法线/骨骼全同、**只有切线不同**时，
    /// 精确查重必须**认作不同顶点**（否则会少一个顶点）。
    /// 官方 `loddtan` 实测 `numLODVertexes=[9,5]`（8 + 1 个新增）。
    #[test]
    fn exact_match_distinguishes_tangents() {
        let a = bv([0.0, 0.0, 0.0], [0.0, 1.0], 1);
        let mut pool = vec![
            DictVert {
                v: a.clone(),
                tangent: VvdTangent {
                    xyz: [0.707, -0.707, 0.0],
                    w: 1.0,
                },
                lod_flags: 1,
            },
            DictVert {
                v: a.clone(),
                tangent: VvdTangent {
                    xyz: [1.0, 0.0, 0.0],
                    w: -1.0,
                },
                lod_flags: 1,
            },
        ];
        let t1 = VvdTangent {
            xyz: [1.0, 0.0, 0.0],
            w: -1.0,
        };
        assert_eq!(
            find_exact_in_range_naive(&pool, 0, 2, &a, &t1),
            Some(1),
            "切线相同的那一个才应命中"
        );
        let t2 = VvdTangent {
            xyz: [0.0, 1.0, 0.0],
            w: -1.0,
        };
        assert_eq!(
            find_exact_in_range_naive(&pool, 0, 2, &a, &t2),
            None,
            "切线不同的两个都不该命中 ⟹ 调用方会**新增**顶点"
        );
        // 反向验证：把切线也算进 key 之后，池里两项确实互不相等。
        assert_ne!(pool[0].tangent, pool[1].tangent);
        pool.clear();
    }

    /// 权重折叠：同骨骼相加、按权重降序。
    ///
    /// 对应 `CollapseBoneWeights` + `SortBoneWeightByWeight`。
    /// 落盘顺序就是这个顺序，所以必须一致。
    #[test]
    fn collapse_merges_same_bone_and_sorts_by_weight() {
        // 两根骨骼都映射到 5 ⟹ 权重相加 = 0.75。
        let mut v = Vertex {
            pos: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            bones: vec![[5.0, 0.25], [5.0, 0.5], [7.0, 0.25]],
        };
        collapse_and_sort_bone_weights(&mut v);
        assert_eq!(v.bones.len(), 2, "同骨骼应合并");
        assert_eq!(v.bones[0][0], 5.0);
        assert!((v.bones[0][1] - 0.75).abs() < 1e-6, "权重应相加");
        assert_eq!(v.bones[1][0], 7.0, "降序：大的在前");
    }

    /// `bone_map` 重定向 + 折叠的组合语义。
    #[test]
    fn remap_then_collapse_follows_official_order() {
        // 骨骼 1 与 2 都映射到 0 ⟹ 合并成一根、权重 1.0。
        let mut v = Vertex {
            pos: [0.0; 3],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
            bones: vec![[1.0, 0.5], [2.0, 0.5]],
        };
        remap_bone_weights(&mut v, &[0, 0, 0]);
        collapse_and_sort_bone_weights(&mut v);
        assert_eq!(v.bones.len(), 1);
        assert_eq!(v.bones[0][0], 0.0);
        assert!((v.bones[0][1] - 1.0).abs() < 1e-6);
    }

    /// `q_log2` 的边界：0 必须给出 -1 而不是 32。
    #[test]
    fn q_log2_handles_zero() {
        assert_eq!(q_log2(0), -1);
        assert_eq!(q_log2(1), 0);
        assert_eq!(q_log2(2), 1);
        assert_eq!(q_log2(0b101), 2);
        assert_eq!(q_log2(1 << 7), 7);
    }

    fn pool_with_index(verts: &[Vertex]) -> (Vec<DictVert>, PosIndex) {
        let pool: Vec<_> = verts
            .iter()
            .map(|v| DictVert {
                v: v.clone(),
                tangent: VvdTangent {
                    xyz: [1.0, 0.0, 0.0],
                    w: -1.0,
                },
                lod_flags: 1,
            })
            .collect();
        let mut index = PosIndex::new();
        for (i, vertex) in pool.iter().enumerate() {
            index.insert(i, vertex.v.pos);
        }
        (pool, index)
    }

    #[test]
    fn exact_index_preserves_first_match_and_rejects_changed_attributes() {
        let vertex = v([0.0; 3], [0.0; 2]);
        let (pool, index) = pool_with_index(&[vertex.clone(), vertex.clone()]);
        let tangent = pool[0].tangent;
        assert_eq!(index.find_exact(&pool, 0, 2, &vertex, &tangent), Some(0));
        assert_eq!(index.find_exact(&pool, 1, 2, &vertex, &tangent), Some(1));
        for attribute in 0..5 {
            let mut changed = vertex.clone();
            let mut changed_tangent = tangent;
            match attribute {
                0 => changed.pos[0] = 0.001,
                1 => changed.uv[0] = 0.001,
                2 => changed.normal[0] = 0.001,
                3 => changed.bones[0][1] = 0.5,
                _ => changed_tangent.w = 1.0,
            }
            assert_eq!(
                index.find_exact(&pool, 0, 2, &changed, &changed_tangent),
                None
            );
        }
        for (start, end) in [(0, 0), (1, 1), (2, 5)] {
            assert_eq!(index.find_exact(&pool, start, end, &vertex, &tangent), None);
        }
        assert_eq!(index.find_exact(&[], 0, 0, &vertex, &tangent), None);
    }

    #[test]
    fn exact_index_matches_linear_scan_for_boundaries_and_extreme_coordinates() {
        let mut vertices: Vec<_> = (0..200)
            .map(|i| v([i as f32 * 0.2 - 20.0, 0.0, 0.0], [0.0; 2]))
            .collect();
        vertices.extend([
            v([-0.0, 0.0, 0.0], [0.0; 2]),
            v([0.0, 0.0, 0.0], [0.0; 2]),
            v([1e30, 0.0, 0.0], [0.0; 2]),
            v([-1e30, 0.0, 0.0], [0.0; 2]),
            v([f32::INFINITY, 0.0, 0.0], [0.0; 2]),
            v([f32::NEG_INFINITY, 0.0, 0.0], [0.0; 2]),
            v([f32::NAN, 0.0, 0.0], [0.0; 2]),
        ]);
        let (pool, index) = pool_with_index(&vertices);
        for (i, vertex) in pool.iter().enumerate() {
            for (start, end) in [(0, pool.len()), (i, i + 1), (i + 1, pool.len())] {
                let actual = index.find_exact(&pool, start, end, &vertex.v, &vertex.tangent);
                let expected =
                    find_exact_in_range_naive(&pool, start, end, &vertex.v, &vertex.tangent);
                assert_eq!(actual, expected, "vertex {i}, range {start}..{end}");
                if start == i {
                    assert_eq!(actual, (!vertex.v.pos[0].is_nan()).then_some(i));
                }
            }
        }
    }

    #[test]
    fn exact_index_tracks_appended_vertices() {
        let (mut pool, mut index) = pool_with_index(&[]);
        for i in 0..300 {
            let vertex = v([i as f32, 0.0, 0.0], [0.0; 2]);
            let tangent = VvdTangent {
                xyz: [1.0, 0.0, 0.0],
                w: -1.0,
            };
            assert_eq!(
                index.find_exact(&pool, 0, pool.len(), &vertex, &tangent),
                None
            );
            index.insert(i, vertex.pos);
            pool.push(DictVert {
                v: vertex.clone(),
                tangent,
                lod_flags: 1,
            });
            assert_eq!(
                index.find_exact(&pool, 0, pool.len(), &vertex, &tangent),
                Some(i)
            );
        }
    }

    #[test]
    fn unify_lods_remapped_matches_naive_end_to_end() {
        // 造 3 档 LOD：LOD1/LOD2 是 LOD0 的稀疏子集 + 少量位移点。
        let mut l0: Vec<Vertex> = Vec::new();
        for i in 0..120 {
            l0.push(Vertex {
                pos: [
                    (i % 12) as f32 * 0.37,
                    (i / 12) as f32 * 0.41,
                    (i % 5) as f32 * 0.13,
                ],
                normal: [0.0, 0.0, 1.0],
                uv: [(i % 7) as f32 * 0.1, (i % 3) as f32 * 0.1],
                bones: vec![[(i % 3) as f32, 1.0]],
            });
        }
        let tris0: Vec<[u32; 3]> = (0..40).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect();

        let mut l1: Vec<Vertex> = l0.iter().step_by(2).cloned().collect();
        l1.push(Vertex {
            pos: [100.0, 100.0, 100.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.5, 0.5],
            bones: vec![[0.0, 1.0]],
        });
        let tris1: Vec<[u32; 3]> = (0..10).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect();

        // LOD2：更稀疏 + 带骨骼坍缩映射（1→0）
        let l2: Vec<Vertex> = l0.iter().step_by(4).cloned().collect();
        let tris2: Vec<[u32; 3]> = (0..5).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect();

        let srcs = vec![
            LodSource::new(l0.clone(), tris0.clone()),
            LodSource {
                vertices: l1,
                triangles: tris1,
                bone_map: Vec::new(),
            },
            LodSource {
                vertices: l2,
                triangles: tris2,
                bone_map: vec![0, 0, 1], // 骨骼 1 塌到 0
            },
        ];

        let mut usage_a = vec![0u32; 3];
        let mut usage_b = vec![0u32; 3];
        let got = unify_lods_remapped(&srcs, &mut usage_a);
        let want = unify_lods_remapped_naive(&srcs, &mut usage_b);

        assert_eq!(got.vertices[..srcs[0].vertices.len()], srcs[0].vertices);
        assert_eq!(
            got.triangles.iter().map(Vec::len).collect::<Vec<_>>(),
            [40, 10, 5]
        );
        assert!(got.lod_vertex_counts.iter().all(|c| *c > 0));

        assert_eq!(got.vertices, want.vertices, "顶点池不一致");
        assert_eq!(got.lod_flags, want.lod_flags, "lod_flags 不一致");
        assert_eq!(got.triangles, want.triangles, "三角形不一致");
        assert_eq!(got.lod_vertex_counts, want.lod_vertex_counts);
        assert_eq!(got.lod_vertex_index, want.lod_vertex_index);
        assert_eq!(usage_a, usage_b, "bone_lod_usage 不一致");
    }
}
