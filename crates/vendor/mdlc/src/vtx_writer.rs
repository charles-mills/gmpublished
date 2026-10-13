//! VTX（`.dx90.vtx`，version 7）写出。
//!
//! ```text
//!   0  FileHeader            36   无魔数；checksum 在 0x10
//!  36  BodyPartHeader         8   {numModels, modelOffset}
//!  44  ModelHeader            8   {numLODs, lodOffset}
//!  52  ModelLODHeader        12   {numMeshes, meshOffset, switchPoint}
//!  64  MeshHeader             9   {numStripGroups, stripGroupOffset, flags}
//!  73  StripGroupHeader      25   {numVerts, vertOffset, numIndices, indexOffset,
//!                                  numStrips, stripOffset, flags}
//!  98  StripHeader           27   {indexCount, indexOffset, vertexCount, vertOffset,
//!                                  boneCount, flags, boneStateChangeCount,
//!                                  boneStateChangeOffset}
//! 125  Vertex[]           8×9    {boneWeightIndex[3], boneCount, origMeshVertID, boneId[3]}
//! 197  Index[]           36×2    uint16，tri-list
//! 269  BoneStateChange[]  1×8    {hardwareID, newBoneID}
//! 277  MaterialReplacementList 8 {numReplacements=0, replacementOffset}
//! ```

use std::collections::HashMap;

use crate::model::CompiledModelDesc;

/// `FileHeader_t` 的字节大小。
pub const HEADER_SIZE: usize = 36;
/// `BodyPartHeader_t` 的字节大小。
pub const BODY_PART_SIZE: usize = 8;
/// `ModelHeader_t` 的字节大小。
pub const MODEL_SIZE: usize = 8;
/// `ModelLODHeader_t` 的字节大小。
pub const MODEL_LOD_SIZE: usize = 12;
/// `MeshHeader_t` 的字节大小（**紧凑，无填充**）。
pub const MESH_SIZE: usize = 9;
/// `StripGroupHeader_t` 的字节大小（L4D2 实测，**无 v49+ 扩展**）。
pub const STRIP_GROUP_SIZE: usize = 25;
/// `StripHeader_t` 的字节大小（L4D2 实测，**无 v49+ 扩展**）。
pub const STRIP_SIZE: usize = 27;
/// `Vertex_t` 的字节大小。
pub const VERTEX_SIZE: usize = 9;
/// `BoneStateChangeHeader_t` 的字节大小。
pub const BONE_STATE_CHANGE_SIZE: usize = 8;
/// `MaterialReplacementListHeader_t` 的字节大小。
pub const MATERIAL_REPLACEMENT_LIST_SIZE: usize = 8;

/// 文件版本。
pub const VERSION: i32 = 7;

/// `STRIPGROUP_IS_FLEXED`：该 strip group 使用顶点动画（flex）。
///
/// ⚠️ **L4D2 的 DX9 产物不用这一位** —— 用的是
/// [`STRIPGROUP_IS_DELTA_FLEXED`]（`optimize.cpp:975` 的注释：
/// 「Going forward, DX9 models are delta flexed」）。
pub const STRIPGROUP_IS_FLEXED: u8 = 0x01;
/// `STRIPGROUP_IS_HWSKINNED`：硬件蒙皮。
pub const STRIPGROUP_IS_HWSKINNED: u8 = 0x02;
/// `STRIPGROUP_IS_DELTA_FLEXED`：该 strip group 的顶点动画是**差量**形式。
///
/// 有 VTA 载荷的 mesh 会写 `IS_HWSKINNED | IS_DELTA_FLEXED = 0x6`
/// （`optimize.cpp:972-976`）。语料 17/17 命中、无 flex 的 5568 个全不带
/// （`probe_vtx_flex_flags.js`）。
pub const STRIPGROUP_IS_DELTA_FLEXED: u8 = 0x04;
/// `STRIP_IS_TRILIST`：索引数组是独立三角形列表。
pub const STRIP_IS_TRILIST: u8 = 0x01;
/// `STRIP_IS_TRISTRIP`：索引数组是三角形带（L4D2 不用）。
pub const STRIP_IS_TRISTRIP: u8 = 0x02;

/// 写出错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VtxWriteError {
    /// mesh 数超过 `uint16` 能表达的范围（`origMeshVertID` 是 u16）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The mesh %{model} has %{count} vertices, over the VTX per-mesh limit of %{max}\n\
             `origMeshVertID` in VTX is `uint16`, which can express indices 0..=65535, \
             so **a mesh can have at most 65536 vertices**.\n\
             Note the granularity is **mesh (= one material)**, not the whole model.\n\
             \n\
             You should not normally see this: the compiler automatically splits \
             an oversized mesh into several meshes of the same material by triangles \
             (TOML's `[model] split_oversized_meshes`, **default true**).\n\
             Seeing it means that option was explicitly turned off; set it back to `true`.\n\
             You can also split by material (give several `$cdmaterials`/several \
             textures in the QC, or several `$bodygroup` submodels), \
             **no need to split the SMD file by hand**.",
            model = model,
            count = count,
            max = crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH
        )
    )]
    TooManyVertices { model: String, count: usize },
    /// 单个 strip 的索引数超过 `int32`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The triangle index count %{count} of %{model} overflows int32",
            model = model,
            count = count
        )
    )]
    TooManyIndices { model: String, count: usize },
    /// 单个 strip 的骨骼调色板超过有符号 `char` 能表达的范围。
    ///
    /// 见 [`MAX_STRIP_BONES`]：`Vertex_t.boneID[]` 是 `char`，
    /// 槽位下标 ≥128 会被引擎读成负数 ⟹ 顶点蒙皮到错误骨骼。
    #[error(
        "{}",
        crate::tr_fmt!(
            "A strip of %{model} uses %{count} bones, over the VTX per-strip limit of %{max}\n\
             `Vertex_t.boneID[]` in VTX is a **signed char** (`optimize.h:51`); \
             hardware slot indices >=128 are read as negative by the engine \
             ==> vertices are skinned to the wrong bones.\n\
             \n\
             This is a **format** limit, not a choice of this implementation, and you \
             should not normally see it: the writer splits a mesh whose palette is over \
             the limit into **several strips** following the official semantics \
             (each <= `maxBonesPerStrip` = 53 bones, see [`plan_strips`]).\n\
             Seeing it means the mesh has **out-of-range triangle indices** (the input \
             is already broken) -- in that case the writer falls back to \"the whole \
             group as one strip\", so all the bones of the group are squeezed into one \
             strip. Fix the out-of-range vertex indices in the SMD first.",
            model = model,
            count = count,
            max = MAX_STRIP_BONES
        )
    )]
    TooManyStripBones { model: String, count: usize },
    /// 内部不一致 —— 属本实现的 bug。
    #[error("{}", crate::tr_fmt!("Internal error (please report): %{err}", err = .0))]
    Internal(String),
}

/// 一个 mesh 的 VTX 侧统计（供自检与日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshVtxStat {
    pub vertex_count: usize,
    pub triangle_count: usize,
}

/// VTX 写出选项。
///
/// # 为什么**默认不做**顶点缓存优化
///
/// 加了它产物就与**真 studiomdl** 不再逐字节一致，而本项目的验收基准
/// 是真 studiomdl（见 `HANDBOOK.md` 教训 75）。所以默认 `false`，
/// 由 TOML `[model] optimize_vtx` 或命令行 `--optimize-vtx` 显式打开。
///
/// # 它做什么
///
/// 对**每个 strip group 单独**跑 `meshopt::optimize_vertex_cache`
/// （每个 strip group 是一次独立的 draw call，必须分开优化 ——
/// 这是 meshopt 文档明确要求的），重排索引以**减少顶点着色器调用次数**。
///
/// 三角形**集合**不变，只改顺序；顶点池与 `origMeshVertID` 也不变。
/// 所以这是个**纯性能优化**，不影响几何、UV、法线、骨骼绑定。
///
/// # 与 nekomdl 的关系
///
/// nekomdl 默认走 meshoptimizer、`-nvtristrip` 回退 NvTriStrip；
/// 真 studiomdl 的对应开关也是 `-nvtristrip`。**两者都会重排**
/// （实测：真 studiomdl 与 nekomdl 的 `origMeshVertID` 都是非平凡排列，
/// 21/21 组），所以打开本项是**接近官方行为**的，只是不会逐字节相同
/// —— meshopt 与 NvTriStrip 是两套不同算法，输出顺序必然不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VtxOptions {
    /// 是否对每个 strip group 做顶点缓存优化（`meshopt_optimizeVertexCache`）。
    ///
    /// 默认 `false` —— 保持与真 studiomdl 的逐字节一致。
    pub optimize_vertex_cache: bool,
}

/// 写出结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VtxWriteOutcome {
    pub bytes: Vec<u8>,
    /// 每个 mesh 的顶点/三角形数（顺序与 MDL 的 mesh 一致）。
    pub mesh_stats: Vec<MeshVtxStat>,
    pub checksum: i32,
}

/// 对一个 strip group 的三角形做**顶点缓存优化**，返回重排后的索引。
///
/// # 为什么必须**逐 strip group** 调用
///
/// `meshopt::optimize_vertex_cache` 的文档明确写了：
///
/// > If index buffer contains multiple ranges for multiple draw calls,
/// > this function needs to be called on each range individually.
///
/// VTX 里每个 strip group 是**一次独立的 draw call**（引擎按 strip group
/// 提交），所以逐组优化才对。若把它们拼成一个数组一起优化，meshopt 会
/// 为了跨组的缓存局部性而打乱组内顺序，反而**恶化**单次 draw 的命中率。
///
/// # 输入是「局部槽位」还是「mesh 顶点号」
///
/// 这里收发的都是 **strip group 局部槽位**（= 写进 VTX 索引数组的值）。
/// 缓存优化的对象正是这个空间 —— 引擎的 post-transform cache 索引的就是
/// 提交给它的槽位序号。`vertex_count` 也传局部顶点数。
///
/// # 三角形集合不变
///
/// meshopt 只是重排三角形顺序（并可能翻转绕序以改善连续性？——**不会**，
/// `optimizeVertexCache` 保持三角形顶点循环序）。这里额外断言
/// 「输入输出的排序后三角形多重集相同」，一旦不符就是 meshopt 行为变了，
/// 直接报 [`VtxWriteError::Internal`] 而不是静默写出坏数据。
fn optimize_group_indices(
    tris: &[[u16; 3]],
    vertex_count: usize,
) -> Result<Vec<[u16; 3]>, VtxWriteError> {
    if tris.is_empty() {
        return Ok(Vec::new());
    }
    // meshopt 要 u32 索引；VTX 是 u16，转换无损。
    let flat: Vec<u32> = tris
        .iter()
        .flat_map(|t| t.iter().map(|v| *v as u32))
        .collect();
    let optimized = meshopt::optimize_vertex_cache(&flat, vertex_count);

    if optimized.len() != flat.len() {
        return Err(VtxWriteError::Internal(crate::tr_fmt!(
            "meshopt returned %{got} indices but the input has %{want}",
            got = optimized.len(),
            want = flat.len()
        )));
    }

    // 用 `as_chunks::<3>()` 而不是 `chunks_exact(3)`：后者对定长块会触发
    // `clippy::chunks_exact_to_as_chunks`（CI 的 `-D warnings` 会当错误）。
    let out: Vec<[u16; 3]> = optimized
        .as_chunks::<3>()
        .0
        .iter()
        .map(|c| {
            [
                u16::try_from(c[0]).unwrap_or(u16::MAX),
                u16::try_from(c[1]).unwrap_or(u16::MAX),
                u16::try_from(c[2]).unwrap_or(u16::MAX),
            ]
        })
        .collect();

    // 自检：**三角形集合必须不变**。这是「只能重排、不能改拓扑」的硬约束。
    //
    // 用「排序后的三角形」做多重集比较 —— 这会**丢掉绕序**，所以它只能
    // 证明「几何没变」，不能证明「朝向没变」。后者由下面的循环单独保证：
    // `optimizeVertexCache` 保持每个三角形的顶点循环序（只整体轮转/不变），
    // 所以这里断言「每个三角形的顶点集合相同」之外，再断言
    // **原始三元组（未排序）也出现在输出里**。
    let sorted_key = |t: &[u16; 3]| {
        let mut s = *t;
        s.sort_unstable();
        s
    };
    let mut a: Vec<[u16; 3]> = tris.iter().map(sorted_key).collect();
    let mut b: Vec<[u16; 3]> = out.iter().map(sorted_key).collect();
    a.sort_unstable();
    b.sort_unstable();
    if a != b {
        return Err(VtxWriteError::Internal(crate::tr(
            "meshopt changed the triangle set (it should only reorder them) -- refusing to write",
        )));
    }
    // 绕序检查：**未排序**的三元组多重集也必须一致。
    let mut ra: Vec<[u16; 3]> = tris.to_vec();
    let mut rb: Vec<[u16; 3]> = out.clone();
    ra.sort_unstable();
    rb.sort_unstable();
    if ra != rb {
        return Err(VtxWriteError::Internal(crate::tr(
            "meshopt changed the triangle winding (backface culling would be inverted) -- refusing to write",
        )));
    }
    Ok(out)
}

/// 一个 strip 的骨骼调色板（`BoneStateChangeHeader_t[]`）与顶点的硬件下标。
///
/// 见 [`build_strip_palette`] 的语义说明。
#[derive(Debug, Clone, PartialEq, Eq)]
struct StripPalette {
    /// 硬件槽位 `i` → **全局**骨骼下标。写出时 `hardwareID` 直接写 `i`。
    bsc: Vec<i32>,
    /// 每个顶点（按传入顺序）三个槽位的**硬件**下标；未用槽位恒为 0。
    hw: Vec<[u8; 3]>,
    /// `StripHeader_t.numBones`。
    num_bones: u16,
}

/// 按官方语义构造 strip 的骨骼调色板。
///
/// # 官方语义（`optimize.h:33-52` + `optimize.cpp:2153-2183`）
///
/// ```c
/// struct BoneStateChangeHeader_t { int hardwareID; int newBoneID; };
/// struct Vertex_t {
///     unsigned char boneWeightIndex[MAX_NUM_BONES_PER_VERT];
///     unsigned char numBones;
///     unsigned short origMeshVertID;
///     char boneID[MAX_NUM_BONES_PER_VERT];
/// };
/// ```
///
/// - `hardwareID` 是**硬件槽位**，实测**恒等于条目下标** `i`。
/// - `newBoneID` 是**全局骨骼下标**，strip 内互不相同。
/// - `Vertex_t.boneID[k]` 是**硬件下标**（`optimize.cpp:2181`：
///   `vert->boneID[boneID] = globalToHardwareBoneIndex[globalBoneID]`），
///   要经 `boneStateChange[boneID[k]].newBoneID` 才是全局骨骼。
/// - 未用槽位（`k >= numBones`）写 **0**。
/// - `numBones` = **max(顶点骨骼数)**，不是唯一骨骼数
///   （`optimize.cpp:1484-1487` 的非 fixed-function 分支）。
///
/// # 语料实测（`docs/_probe/survey_vtx_bonestate.js`，3302 文件 / 6677 strip）
///
/// | 判据 | 结果 |
/// |---|---|
/// | `hardwareID == i` | 6677/6677 |
/// | `newBoneID ∈ [0,126]`、strip 内不重复 | 6677/6677 |
/// | `boneID[k] ∈ [0, numBoneStateChanges)` | 6677/6677 |
/// | 未用槽位为 0 | 6677/6677 |
/// | `numBoneStateChanges == 顶点用到的唯一全局骨骼数` | **6677/6677** |
/// | `numBones == max(顶点 numBones)` | **6677/6677** |
///
/// # ⚠️ 这个 bug 的症状
///
/// 早先这里把 `boneID[k]` 写成**全局**骨骼下标、且 `numBoneStateChanges` 恒写 1。
/// 于是引擎读 `boneStateChange[3]`（调色板只有 1 项）拿到**越界的垃圾骨骼**，
/// 顶点被蒙皮到错误的骨骼上 —— 表现为**顶点错乱 / 模型撕裂**。
///
/// `boneID` 是**有符号** `char`（`optimize.h:51`），所以硬件槽位也必须 ≤ 127；
/// 但槽位数受 `maxBonesPerStrip`（53）约束，正常远小于它。
///
/// # 顺序
///
/// 官方按「洪泛分配次序」排列 `newBoneID`（`optimize.cpp:814-885`），
/// 本实现按「顶点序 × 槽位序的首次出现次序」。**两者语义等价** ——
/// 引擎只用 `boneStateChange` 建双向映射（`optimize.cpp:2153-2163`），
/// 不依赖顺序；但**不逐字节相同**。
fn build_strip_palette(vertices: &[&crate::model::Vertex]) -> StripPalette {
    let mut bsc: Vec<i32> = Vec::new();
    let mut hw: Vec<[u8; 3]> = Vec::with_capacity(vertices.len());
    let mut num_bones = 0u16;

    for v in vertices {
        let n = v.bones.len().min(3);
        num_bones = num_bones.max(n as u16);
        // 未用槽位保持 0（官方口径）。
        let mut slot = [0u8; 3];
        for (k, b) in v.bones.iter().take(3).enumerate() {
            // SMD 里的骨骼下标是浮点，这里按官方口径取整。
            let gb = b[0] as i32;
            let pos = match bsc.iter().position(|&x| x == gb) {
                Some(p) => p,
                None => {
                    bsc.push(gb);
                    bsc.len() - 1
                }
            };
            slot[k] = pos as u8;
        }
        hw.push(slot);
    }

    StripPalette { bsc, hw, num_bones }
}

/// 一个 strip 的骨骼调色板上限。
///
/// `Vertex_t.boneID[]` 是**有符号 `char`**（`optimize.h:51`），
/// 所以硬件槽位必须 ≤ 127。官方另有 `MAX_NUM_BONES_PER_STRIP = 512`
/// （`optimize.h:20`）与 `maxBonesPerStrip = 53`（L4D2 实测值）两层约束，
/// 但它们都**大于** `char` 能表达的范围 —— 真正卡住的是这个 127。
///
/// ⚠️ 这只是**格式**兜底。真正必须守住的是 [`MAX_BONES_PER_STRIP`]（53）——
/// 调色板超标的 mesh 会被拆成多条 strip（见 `plan_strips`）。
pub const MAX_STRIP_BONES: usize = 127;

/// `FileHeader_t.maxBonesPerStrip` 的实测值，也是**拆分 strip 的阈值**。
///
/// 语料实测（`docs/_probe/vtx_bsclimit.js`，3302 个官方 `.dx90.vtx`）：
///
/// | 判据 | 结果 |
/// |---|---|
/// | 头部 `maxBonesPerStrip` 取值 | **53**（3302/3302，唯一取值） |
/// | `numBoneStateChanges > maxBonesPerStrip` 的 strip | **0**（最大超出 0） |
///
/// 所以 53 不是「随手一个上限」，而是官方**写进文件头、并被
/// `optimize.cpp:871 assert( newStrip.numBoneStateChanges <= maxBonesPerStrip )`
/// 保证覆盖全部 strip** 的硬上界。写出一条超过它的 strip 就骗过了引擎 ——
/// 调色板超标的 mesh 会被拆成多条 strip（见 `plan_strips`）。
pub const MAX_BONES_PER_STRIP: usize = 53;

/// 一条 strip 的写出计划（一个 strip group 内可能有**多条** strip）。
///
/// # 官方约定（语料实测，`docs/_probe/vtx_split_detail.js`）
///
/// 参考样本 `mods/models/!survivors/survivor_TeenAngst.dx90.vtx` 的
/// `bp0/model0/lod0/mesh5/sg0` 被拆成 **53 + 8** 两条：
///
/// ```text
/// group{verts=4065 idx=20178 numStrips=2}
///   strip[0] nv=3817@0     ni=19065@0     nBSC=53  index∈[0,3816]    hw∈[0,52]
///   strip[1] nv= 248@3817  ni= 1113@19065 nBSC= 8  index∈[3817,4064] hw∈[0,7]
/// ```
///
/// 由此钉死三条语义：
///
/// - 同一 strip group 的**顶点数组与索引数组只有一份**，被各 strip
///   **按连续子区间瓜分**：`strip[i].vertOffset` / `indexOffset` 是**组内**
///   起点，逐条首尾相接（`strip[0]` 从 0 起，
///   `strip[i+1].vertOffset == strip[i].vertOffset + strip[i].numVerts`）；
/// - **索引值是「组内绝对顶点下标」**，不是相对本 strip 的局部下标 ——
///   实测 `strip[1]` 的索引恰好落在 `[3817, 4064]`，正是它自己的顶点子区间；
/// - **每条 strip 有自己独立的 `boneStateChange` 数组**，`boneID[]` 写的是
///   **该 strip 内**的局部硬件槽位（`hw ≤ nBSC - 1`）。
///
/// 官方产物里两条 strip 的 `origMeshVertID` 区间是**重叠**的
/// （`strip[0] ∈ [0,8386]`、`strip[1] ∈ [602,6452]`）⟹ 同一个 mesh 顶点被
/// 两条 strip 各存一份是**官方行为**，不是异常。
struct StripPlan {
    /// 本 strip 的顶点在**组顶点数组**内的起点（组内局部下标）。
    v_off: usize,
    /// 本 strip 的索引在**组索引数组**内的起点（组内局部下标）。
    i_off: usize,
    /// 本 strip 的顶点：元素是「组顶点槽位」。
    ///
    /// 单 LOD 路径下槽位 == mesh 顶点下标；多 LOD 路径下槽位是
    /// `Item::verts` 的下标。写出时由调用方翻译成 `origMeshVertID`。
    verts: Vec<u32>,
    /// 本 strip 的三角形，元素是**组内绝对顶点下标**（已加 `v_off`）。
    tris: Vec<[u32; 3]>,
    /// 本 strip 自己的骨骼调色板。
    pal: StripPalette,
}

/// 把一个 strip group 拆成若干条 strip，使每条的骨骼调色板都不超过
/// `max_bones`（= [`MAX_BONES_PER_STRIP`]，53）。
///
/// # 为什么必须拆
///
/// `FileHeader_t.maxBonesPerStrip` 是**文件头声明的硬上界**。官方语料
/// 3302/3302 个 `.dx90.vtx` 零违规（`vtx_multistrip.js --validate`），
/// 而拆出来的每条 strip 都不超过它。引擎的硬件蒙皮路径按这个声明分配
/// 骨骼矩阵槽位 —— 写出 `numBoneStateChanges > 53` 的 strip
/// （以及 `boneID[]` 里 > 52 的槽位）就越过了声明。
///
/// 实测（`vtx_strip_dump.js` + `vtx_multistrip.js --validate`）：本工程
/// 的 `survivor_TeenAngst.dx90.vtx` 有 **1 条 strip 的 `nBSC = 55`、
/// `maxHw = 54`**，是全语料唯一的违规者；而游戏里能正常加载的旧产物
/// 在同一个 mesh 上拆成 53 + 8。症状是进图几秒后崩在
/// `shaderapidx9` 的顶点拷贝里。
///
/// # 拆分方式
///
/// 官方是「以三角形为种子洪泛 + 硬件矩阵 LRU 分配」，槽位耗尽就 flush
/// 一条（`optimize.cpp:814-886`）。本实现用**等价的贪心**：
///
/// - 按 mesh 的三角形**原序**遍历，维护「当前 strip 用到的全局骨骼集合」；
/// - 若加入下一个三角形会让集合超过 `max_bones`，先 flush 当前 strip；
/// - 单三角形的骨骼数 ≤ 3 顶点 × 3 骨 = 9 ⟹ 贪心**永远不会卡住**。
///
/// 每条 strip 的顶点列表 = 该 strip 的三角形引用到的顶点，按 mesh 顶点序
/// 去重排列（确定性）；组顶点数组 = 各 strip 顶点列表首尾相接。
///
/// # 不需要拆分时
///
/// 整组唯一骨骼数 ≤ `max_bones` 时返回**恰好一条** strip，且顶点保持
/// mesh 原序、`v_off`/`i_off` 都是 0、索引原样 —— 与拆分特性引入前的
/// 产物**逐字节相同**（`single_lod_path_is_unchanged` 守着这一点）。
///
/// 三角形里出现越界下标时（已经坏掉的输入）同样退回单 strip 原样，
/// 保持旧行为、不 panic。
fn plan_strips(
    vertices: &[&crate::model::Vertex],
    triangles: &[[u32; 3]],
    max_bones: usize,
) -> Vec<StripPlan> {
    let whole = build_strip_palette(vertices);
    let out_of_range = triangles
        .iter()
        .any(|t| t.iter().any(|&v| v as usize >= vertices.len()));
    if whole.bsc.len() <= max_bones || out_of_range {
        return vec![StripPlan {
            v_off: 0,
            i_off: 0,
            verts: (0..vertices.len() as u32).collect(),
            tris: triangles.to_vec(),
            pal: whole,
        }];
    }

    let mut strips: Vec<StripPlan> = Vec::new();
    let mut cur_bones: Vec<i32> = Vec::new();
    let mut cur_tris: Vec<[u32; 3]> = Vec::new();

    for tri in triangles {
        // 该三角形引入的、当前 strip 还没有的骨骼。
        let mut fresh: Vec<i32> = Vec::new();
        for &vi in tri {
            for b in vertices[vi as usize].bones.iter().take(3) {
                let gb = b[0] as i32;
                if !cur_bones.contains(&gb) && !fresh.contains(&gb) {
                    fresh.push(gb);
                }
            }
        }
        if !cur_tris.is_empty() && cur_bones.len() + fresh.len() > max_bones {
            strips.push(finish_strip(&mut cur_bones, &mut cur_tris, vertices));
        }
        cur_bones.extend(fresh);
        cur_tris.push(*tri);
    }
    if !cur_tris.is_empty() || strips.is_empty() {
        strips.push(finish_strip(&mut cur_bones, &mut cur_tris, vertices));
    }

    // 组内偏移：各 strip 的顶点/索引子区间首尾相接。
    let mut v_off = 0usize;
    let mut i_off = 0usize;
    for s in &mut strips {
        s.v_off = v_off;
        s.i_off = i_off;
        let base = v_off as u32;
        for t in &mut s.tris {
            t[0] += base;
            t[1] += base;
            t[2] += base;
        }
        v_off += s.verts.len();
        i_off += s.tris.len() * 3;
    }

    // 自检：拆完之后**每条** strip 都必须落在头部声明的
    // `maxBonesPerStrip`（53）以内。贪心本身保证这一点（单个三角形最多
    // 引入 9 根骨骼，永远能单独成条），这里是**兜底断言** —— 一旦哪天
    // 贪心被改坏，debug 构建与全部测试会立刻炸，而不是静默写出超限文件
    // 让引擎在 `shaderapidx9` 里崩掉。
    //
    // 用 `debug_assert!` 而不是硬错误：`--release` 下这条断言会被编译掉，
    // 正式产物不受影响；真正面向用户的兜底是 `MAX_STRIP_BONES`（127，
    // 见两个写出函数里的 `TooManyStripBones`）。
    debug_assert!(
        strips.iter().all(|s| s.pal.bsc.len() <= max_bones),
        "拆 strip 后仍有调色板超过 {max_bones} 的 strip —— plan_strips 的贪心有 bug"
    );
    strips
}

/// 收尾一条 strip：算出它的顶点列表（mesh 序、去重）、调色板与
/// 「组内局部槽位」索引，并把 `cur_*` 清空以便复用。
fn finish_strip(
    cur_bones: &mut Vec<i32>,
    cur_tris: &mut Vec<[u32; 3]>,
    vertices: &[&crate::model::Vertex],
) -> StripPlan {
    let mut used: Vec<u32> = Vec::new();
    for t in cur_tris.iter() {
        for &vi in t {
            if !used.contains(&vi) {
                used.push(vi);
            }
        }
    }
    used.sort_unstable();
    let refs: Vec<&crate::model::Vertex> = used.iter().map(|&vi| vertices[vi as usize]).collect();
    let pal = build_strip_palette(&refs);
    // 组顶点槽位 → 本 strip 内的局部下标。
    let local: HashMap<u32, u32> = used
        .iter()
        .enumerate()
        .map(|(i, &vi)| (vi, i as u32))
        .collect();
    let tris: Vec<[u32; 3]> = cur_tris
        .iter()
        .map(|t| [local[&t[0]], local[&t[1]], local[&t[2]]])
        .collect();
    cur_bones.clear();
    cur_tris.clear();
    StripPlan {
        v_off: 0,
        i_off: 0,
        verts: used,
        tris,
        pal,
    }
}

/// 把编译结果写成 VTX 字节（**默认选项** —— 不做缓存优化）。
///
/// 等价于 `write_vtx_with(compiled, VtxOptions::default())`。
/// 保留这个签名是为了让调用方（与全部既有测试）**一行不改**，
/// 从而保证默认产物与加本特性之前**逐字节相同**。
pub fn write_vtx(compiled: &CompiledModelDesc) -> Result<VtxWriteOutcome, VtxWriteError> {
    write_vtx_with(compiled, VtxOptions::default())
}

/// 把编译结果写成 VTX 字节，可指定 [`VtxOptions`]。
///
/// 按是否有 LOD 数据分派：
/// - **全部单 LOD** → `write_vtx_single`，与加多 LOD 支持之前**逐字节相同**
///   （那条路径一行未改）；
/// - **有 model 带 LOD** → `write_vtx_multi`。
pub fn write_vtx_with(
    compiled: &CompiledModelDesc,
    opts: VtxOptions,
) -> Result<VtxWriteOutcome, VtxWriteError> {
    let multi = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .any(|m| m.lods.as_ref().is_some_and(|l| l.is_multi()));
    if multi {
        write_vtx_multi(compiled, opts)
    } else {
        write_vtx_single(compiled, opts)
    }
}

/// 单 LOD 的 VTX 写出：每个 model 一个 LOD、每个 mesh 一个 strip group、
/// 每个 strip group 一个 tri-list strip。
fn write_vtx_single(
    compiled: &CompiledModelDesc,
    opts: VtxOptions,
) -> Result<VtxWriteOutcome, VtxWriteError> {
    let desc = &compiled.desc;
    let checksum = desc.checksum();
    let mesh_total: usize = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.meshes.len())
        .sum();

    // ---- 0. 拆 strip：每个 mesh 可能不止一条 ----
    //
    // 官方约定见 [`plan_strips`]：同一 strip group 的顶点/索引数组被各 strip
    // 按**连续子区间**瓜分，每条 strip 有自己的骨骼调色板。
    let mut mesh_plans: Vec<Vec<StripPlan>> = Vec::with_capacity(mesh_total);
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                let refs: Vec<&crate::model::Vertex> = mesh.vertices.iter().collect();
                let plans = plan_strips(&refs, &mesh.triangles, MAX_BONES_PER_STRIP);
                for p in &plans {
                    if p.pal.bsc.len() > MAX_STRIP_BONES {
                        return Err(VtxWriteError::TooManyStripBones {
                            model: m.name.clone(),
                            count: p.pal.bsc.len(),
                        });
                    }
                }
                mesh_plans.push(plans);
            }
        }
    }
    // 每个 mesh 的**第一条** strip 在 strip 表里的下标。
    let mut mesh_strip_at = Vec::with_capacity(mesh_total);
    let mut strip_cursor = 0usize;
    for plans in &mesh_plans {
        mesh_strip_at.push(strip_cursor);
        strip_cursor += plans.len();
    }
    let strip_total = strip_cursor;

    // ---- 1. 先算各段偏移 ----
    let bp_count = compiled.bodyparts.len();
    let model_total: usize = compiled.bodyparts.iter().map(|bp| bp.models.len()).sum();

    let bp_off = HEADER_SIZE;
    let model_off = bp_off + bp_count * BODY_PART_SIZE;
    let lod_off = model_off + model_total * MODEL_SIZE;
    let mesh_off = lod_off + model_total * MODEL_LOD_SIZE;
    let sg_off = mesh_off + mesh_total * MESH_SIZE;
    // 每个 mesh 一个 strip group，但可能有**多条** strip。
    let strip_off = sg_off + mesh_total * STRIP_GROUP_SIZE;
    let vertex_off = strip_off + strip_total * STRIP_SIZE;

    // 顶点区之后依次是索引区、bone state change 区、material replacement 区。
    // 逐 mesh 累加，同时记录每个 mesh 的起点。
    let mut mesh_vertex_at = Vec::with_capacity(mesh_total);
    let mut mesh_index_at = Vec::with_capacity(mesh_total);
    let mut mesh_stats = Vec::with_capacity(mesh_total);

    let mut vcursor = vertex_off;

    // 第一遍：算顶点区大小。
    //
    // ⚠️ 组顶点数**不等于** mesh 顶点数 —— 拆 strip 后同一 mesh 顶点可能在
    // 多条 strip 里各存一份（官方实测如此，见 [`StripPlan`]）。
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                let n = mesh.vertices.len();
                // ⚠️ 判据是 `>`（不是 `>=`）：`origMeshVertID` 是 `uint16`，
                // 能表达下标 `0..=65535` ⟹ **65536 个顶点是合法的**。
                // 早期写成 `n > u16::MAX`（= `n > 65535`）⟹ 把 65536 误拒，
                // 恰好少了一个 —— 见 `MAXSTUDIOVERTS_PER_MESH`。
                if n > crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH {
                    return Err(VtxWriteError::TooManyVertices {
                        model: m.name.clone(),
                        count: n,
                    });
                }
                mesh_vertex_at.push(vcursor);
                vcursor += mesh_plans[mesh_vertex_at.len() - 1]
                    .iter()
                    .map(|p| p.verts.len())
                    .sum::<usize>()
                    * VERTEX_SIZE;
            }
        }
    }
    // 索引区紧跟顶点区。索引总数与 mesh 三角形数一致（拆分只重排/重编号）。
    let mut icursor = vcursor;
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                let n = mesh.triangles.len() * 3;
                if n > i32::MAX as usize {
                    return Err(VtxWriteError::TooManyIndices {
                        model: m.name.clone(),
                        count: n,
                    });
                }
                mesh_index_at.push(icursor);
                icursor += n * 2;
            }
        }
    }
    // bone state change 区紧跟索引区。**每条 strip 一个调色板**，
    // 大小 = 该 strip 用到的唯一全局骨骼数（语料实测 6677/6677 恒等，
    // 见 [`build_strip_palette`]）。
    let mut bsc_cursor = icursor;
    let mut strip_bsc_at: Vec<usize> = Vec::with_capacity(strip_total);
    for plans in &mesh_plans {
        for p in plans {
            strip_bsc_at.push(bsc_cursor);
            bsc_cursor += p.pal.bsc.len() * BONE_STATE_CHANGE_SIZE;
        }
    }
    // mesh 统计按同一顺序收集。
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                mesh_stats.push(MeshVtxStat {
                    vertex_count: mesh.vertices.len(),
                    triangle_count: mesh.triangles.len(),
                });
            }
        }
    }
    // material replacement list（每 LOD 一个，内容为空）放在最后。
    let mat_repl_off = bsc_cursor;
    let total_len = mat_repl_off + MATERIAL_REPLACEMENT_LIST_SIZE;

    let mut buf = vec![0u8; total_len];

    // ---- 2. 头部 ----
    // vertexCacheSize：studiomdl 写 24（实测）。它是「优化后的顶点缓存
    // 大小」提示，引擎不依赖其正确性；用 24 与官方一致。
    put_i32(&mut buf, 0x00, VERSION);
    put_i32(&mut buf, 0x04, 24);
    put_u16(&mut buf, 0x08, 53); // maxBonesPerStrip（实测 53）
    put_u16(&mut buf, 0x0A, 9); // maxBonesPerTri（实测 9）
    put_i32(&mut buf, 0x0C, 3); // maxBonesPerVertex（MAX_NUM_BONES_PER_VERT）
    put_i32(&mut buf, 0x10, checksum);
    put_i32(&mut buf, 0x14, 1); // numLODs
    put_i32(&mut buf, 0x18, mat_repl_off as i32);
    put_i32(&mut buf, 0x1C, bp_count as i32);
    put_i32(&mut buf, 0x20, bp_off as i32);

    // ---- 3. 结构树 ----
    let mut model_cursor = 0usize;
    let mut mesh_cursor = 0usize;
    let mut bp_abs = bp_off;
    for bp in &compiled.bodyparts {
        put_i32(&mut buf, bp_abs, bp.models.len() as i32);
        put_i32(
            &mut buf,
            bp_abs + 4,
            (model_off + model_cursor * MODEL_SIZE) as i32 - bp_abs as i32,
        );
        for m in &bp.models {
            let model_abs = model_off + model_cursor * MODEL_SIZE;
            put_i32(&mut buf, model_abs, 1); // numLODs
            put_i32(
                &mut buf,
                model_abs + 4,
                (lod_off + model_cursor * MODEL_LOD_SIZE) as i32 - model_abs as i32,
            );
            let lod_abs = lod_off + model_cursor * MODEL_LOD_SIZE;
            put_i32(&mut buf, lod_abs, m.meshes.len() as i32);
            put_i32(
                &mut buf,
                lod_abs + 4,
                (mesh_off + mesh_cursor * MESH_SIZE) as i32 - lod_abs as i32,
            );
            put_f32(&mut buf, lod_abs + 8, 0.0); // switchPoint

            for ki in 0..m.meshes.len() {
                let mesh_abs = mesh_off + (mesh_cursor + ki) * MESH_SIZE;
                put_i32(&mut buf, mesh_abs, 1); // numStripGroups
                put_i32(
                    &mut buf,
                    mesh_abs + 4,
                    (sg_off + (mesh_cursor + ki) * STRIP_GROUP_SIZE) as i32 - mesh_abs as i32,
                );
                put_u8(&mut buf, mesh_abs + 8, 0); // mesh flags（非牙齿/眼睛）
            }
            model_cursor += 1;
            mesh_cursor += m.meshes.len();
        }
        bp_abs += BODY_PART_SIZE;
    }

    // ---- 4. strip group + strip + 顶点 + 索引 + bone state change ----
    let mut mi = 0usize;
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                let sg_abs = sg_off + mi * STRIP_GROUP_SIZE;
                let v_abs = mesh_vertex_at[mi];
                let i_abs = mesh_index_at[mi];

                let plans = &mesh_plans[mi];
                let first_strip = mesh_strip_at[mi];
                let st_abs = strip_off + first_strip * STRIP_SIZE;
                // 组顶点/索引数组 = 各 strip 的子区间首尾相接（见 [`StripPlan`]）。
                let nv: usize = plans.iter().map(|p| p.verts.len()).sum();
                let ni: usize = plans.iter().map(|p| p.tris.len() * 3).sum();

                // strip group
                put_i32(&mut buf, sg_abs, nv as i32);
                put_i32(&mut buf, sg_abs + 4, (v_abs - sg_abs) as i32);
                put_i32(&mut buf, sg_abs + 8, ni as i32);
                put_i32(&mut buf, sg_abs + 12, (i_abs - sg_abs) as i32);
                put_i32(&mut buf, sg_abs + 16, plans.len() as i32); // numStrips
                put_i32(&mut buf, sg_abs + 20, (st_abs - sg_abs) as i32);
                // `flags`：`optimize.cpp:968-981` 的 `ComputeStripGroupFlags`：
                //
                // ```c
                // pStripGroup->flags = 0;
                // if (isFlexed) { flags |= IS_FLEXED(0x01); flags |= IS_DELTA_FLEXED(0x04); }
                // if (isHWSkinned) flags |= IS_HWSKINNED(0x02);
                // ```
                //
                // ⟹ **有 VTA 载荷的 mesh 写 `0x6`（HWSKINNED|DELTA_FLEXED）**，
                //    没有的写 `0x2`。
                //
                // ⚠️ 是 `0x04 DELTA_FLEXED` 而**不是** `0x01 IS_FLEXED` ——
                // 源码注释明说「Going forward, DX9 models are delta flexed」。
                //
                // 语料判据（`probe_vtx_flex_flags.js`）：flags 直方图
                // **恰好 17 个 `0x6`**，与「有 flex 的 mesh 数」17 完全吻合；
                // 5568 个无 flex 的 mesh **无一**带 `0x04`（0 例外）。
                //
                // > 早先这里恒写 `0x02`（VTA 未实现时的正确值）。有 VTA 后
                // > 必须跟着变 —— 否则引擎不会走 flex 路径，形状不生效。
                let is_flexed = m.mesh_flexes.get(mi).is_some_and(|v| !v.is_empty());
                let sg_flags = if is_flexed {
                    STRIPGROUP_IS_HWSKINNED | STRIPGROUP_IS_DELTA_FLEXED
                } else {
                    STRIPGROUP_IS_HWSKINNED
                };
                put_u8(&mut buf, sg_abs + 24, sg_flags);

                // strip（tri-list）：一个 strip group 里可能有**多条** strip，
                // 各占组顶点/索引数组的一段**连续子区间**（见 [`StripPlan`]）。
                let mut v_cur = 0usize; // 组顶点数组写游标
                let mut i_cur = 0usize; // 组索引数组写游标（单位：索引）
                for (si, p) in plans.iter().enumerate() {
                    let st_abs = strip_off + (first_strip + si) * STRIP_SIZE;
                    let bsc_abs = strip_bsc_at[first_strip + si];
                    let pal = &p.pal;

                    put_i32(&mut buf, st_abs, (p.tris.len() * 3) as i32); // indexCount
                    put_i32(&mut buf, st_abs + 4, i_cur as i32); // indexOffset（组内起点）
                    put_i32(&mut buf, st_abs + 8, p.verts.len() as i32); // vertexCount
                    put_i32(&mut buf, st_abs + 12, v_cur as i32); // vertOffset（组内起点）
                    // `numBones` = **max(顶点 numBones)**，不是唯一骨骼数
                    // （`optimize.cpp:1484-1487` 的非 fixed-function 分支；
                    //  语料 6677/6677 成立）。
                    put_u16(&mut buf, st_abs + 16, pal.num_bones);
                    put_u8(&mut buf, st_abs + 18, STRIP_IS_TRILIST);
                    put_i32(&mut buf, st_abs + 19, pal.bsc.len() as i32); // boneStateChangeCount
                    put_i32(&mut buf, st_abs + 23, (bsc_abs - st_abs) as i32);

                    // 顶点：boneWeightIndex 指向该 VVD 顶点自己的权重数组槽位。
                    for (vi, &slot) in p.verts.iter().enumerate() {
                        let o = v_abs + (v_cur + vi) * VERTEX_SIZE;
                        let v = &mesh.vertices[slot as usize];
                        let bone_count = v.bones.len().min(3) as u8;
                        // 实测 studiomdl **恒写 [0, 1, 2]**（固定序列），不是
                        // 「0..bone_count」。它表示「槽位 k 对应 VVD 顶点里
                        // weight[k]/bone[k]」；写 0 填充会让引擎把槽位 1/2
                        // 也指向 weight[0]。
                        buf[o] = 0;
                        buf[o + 1] = 1;
                        buf[o + 2] = 2;
                        buf[o + 3] = bone_count.max(1);
                        put_u16(&mut buf, o + 4, slot as u16); // origMeshVertID
                        // boneId：**硬件**下标（见 [`build_strip_palette`]）。
                        // 写成全局下标会让引擎读越界的调色板项 ⟹ 顶点错乱。
                        buf[o + 6..o + 9].copy_from_slice(&pal.hw[vi]);
                    }

                    // 索引：tri-list，每三角形三个 u16。
                    // 索引值是**组内绝对顶点下标**（`p.tris` 已含 `v_off` 基址），
                    // 与官方实测一致 —— 见 [`StripPlan`]。
                    //
                    // `optimize_vtx` 打开时**逐 strip** 做顶点缓存优化
                    // （每条 strip 是组内一段连续子区间，跨 strip 重排会破坏
                    // 子区间划分）。默认关闭 ⇒ 这段是原序直写，
                    // 产物与本特性引入前**逐字节相同**。
                    if opts.optimize_vertex_cache {
                        let src: Vec<[u16; 3]> = p
                            .tris
                            .iter()
                            .map(|t| [t[0] as u16, t[1] as u16, t[2] as u16])
                            .collect();
                        let opt = optimize_group_indices(&src, nv)?;
                        for (ti, tri) in opt.iter().enumerate() {
                            let o = i_abs + (i_cur + ti * 3) * 2;
                            put_u16(&mut buf, o, tri[0]);
                            put_u16(&mut buf, o + 2, tri[1]);
                            put_u16(&mut buf, o + 4, tri[2]);
                        }
                    } else {
                        for (ti, tri) in p.tris.iter().enumerate() {
                            let o = i_abs + (i_cur + ti * 3) * 2;
                            put_u16(&mut buf, o, tri[0] as u16);
                            put_u16(&mut buf, o + 2, tri[1] as u16);
                            put_u16(&mut buf, o + 4, tri[2] as u16);
                        }
                    }

                    // bone state change：`{hardwareID, newBoneID}`。
                    // `hardwareID` **恒等于条目下标**（`optimize.cpp:876`；
                    // 语料 6677/6677 实测），所以直接写 `i`。
                    for (k, &gb) in pal.bsc.iter().enumerate() {
                        let q = bsc_abs + k * BONE_STATE_CHANGE_SIZE;
                        put_i32(&mut buf, q, k as i32);
                        put_i32(&mut buf, q + 4, gb);
                    }

                    v_cur += p.verts.len();
                    i_cur += p.tris.len() * 3;
                }

                mi += 1;
            }
        }
    }

    // ---- 5. material replacement list（每 LOD 一个，空表）----
    put_i32(&mut buf, mat_repl_off, 0);
    // 实测 studiomdl 的 `replacementOffset` 写 **0**，不是「指向数组末尾」。
    // 表为空时该字段无意义，写 0 与官方一致。
    put_i32(&mut buf, mat_repl_off + 4, 0);

    if buf.len() != total_len {
        return Err(VtxWriteError::Internal(crate::tr_fmt!(
            "the written length %{got} does not match the budget %{want}",
            got = buf.len(),
            want = total_len
        )));
    }
    Ok(VtxWriteOutcome {
        bytes: buf,
        mesh_stats,
        checksum,
    })
}

/// 多 LOD 的 VTX 写出。
///
/// # 与单 LOD 的结构差异
///
/// - 头部 `numLODs` = 各 model 的最大 LOD 数（实测与 VVD 的 `numLODs` 一致）；
/// - 每个 model 有 `numLODs` 个连续的 `ModelLODHeader_t`（单 LOD 时是 1 个）；
/// - 每个 LOD 有自己的 mesh 数组与 strip group —— **结构块数按 LOD 翻倍**；
/// - `materialReplacementList` 也是**每 LOD 一个**（单 LOD 时 1 个）。
///
/// # 每个 LOD 的顶点与索引怎么来
///
/// 用 `layout.mesh_local_id(mesh, unified)` 把该 LOD 三角形引用的统一顶点
/// 翻译成 `origMeshVertID`（= `finalMeshVertID`），然后：
///
/// - strip group 的顶点数组 = 该 LOD **实际用到**的 `origMeshVertID` 去重集合；
/// - 索引 = 该 LOD 的三角形，下标指向「顶点数组内的槽位」；
/// - 顶点记录里的 `boneId` / `boneCount` 取自统一池顶点（蒙皮信息与 LOD 无关）。
///
/// 实测依据（`dbg_lod_ids_exact.js`，230 个真实多 LOD 模型）：各 LOD 的
/// `origMeshVertID` 集合**不是**简单的嵌套关系，LOD 会各自挑选顶点
/// （例如 `boomer` 的 LOD0 用 `0-711,3248-5783`，LOD1 用 `0-3247`），
/// 所以必须按「该 LOD 自己的三角形」精确收集，不能套用某个区间。
fn write_vtx_multi(
    compiled: &CompiledModelDesc,
    opts: VtxOptions,
) -> Result<VtxWriteOutcome, VtxWriteError> {
    let desc = &compiled.desc;
    let checksum = desc.checksum();

    // 全局 LOD 数 = 各 model 的最大值（实测与 VVD 的 numLODs 一致）。
    let num_lods = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.lods.as_ref().map_or(1, |l| l.num_lods))
        .max()
        .unwrap_or(1)
        .max(1);

    // 全局 mesh 布局：与 `build_multi_lod_vvd` 用同一份算法，保证编号一致。
    let mut all: Vec<crate::lod::MeshLods> = Vec::new();
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            match &m.lods {
                Some(l) => all.extend(l.meshes.iter().cloned()),
                None => {
                    for mesh in &m.meshes {
                        all.push(crate::lod::MeshLods::single(
                            mesh.vertices.clone(),
                            mesh.triangles.clone(),
                        ));
                    }
                }
            }
        }
    }
    let layout = crate::lod::build_lod_layout(&all);

    let bp_count = compiled.bodyparts.len();
    let model_total: usize = compiled.bodyparts.iter().map(|bp| bp.models.len()).sum();
    let mesh_total: usize = all.len();
    // 每个 model 的 LOD 数可能不同（本实现里都是 num_lods，但按 model 取更稳）。
    let lod_header_total: usize = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.lods.as_ref().map_or(1, |l| l.num_lods))
        .sum();
    // 每个 LOD 的每个 mesh 一个 strip group，但**可能有不止一条 strip** ——
    // `strip_total` 要等拆完才知道（见下面的 `item_plans`），所以
    // `strip_off` / `vertex_off` 在那一块之后才算。
    let sg_total = mesh_total * num_lods;

    let bp_off = HEADER_SIZE;
    let model_off = bp_off + bp_count * BODY_PART_SIZE;
    let lod_off = model_off + model_total * MODEL_SIZE;
    let mesh_off = lod_off + lod_header_total * MODEL_LOD_SIZE;
    let sg_off = mesh_off + sg_total * MESH_SIZE;

    // ---- 逐 (model, lod, mesh) 展开成写入项 ----
    struct Item {
        /// 该 LOD 的 mesh 在统一池里的序号（用于查 layout）。
        unified_mesh: usize,
        lod: usize,
        /// 本 LOD 实际用到的统一顶点下标（去重、升序）。
        verts: Vec<u32>,
        /// 本 LOD 的三角形（下标指向 `verts` 内的槽位）。
        tris: Vec<[u32; 3]>,
        /// 本档是否 `nofacial`（QC 的 `nofacial`）。
        ///
        /// `true` ⟹ 本档的 strip group **不带** `0x04`（见写出处的说明）。
        no_facial: bool,
    }
    let mut items: Vec<Item> = Vec::with_capacity(sg_total);
    let mut mesh_stats = Vec::with_capacity(mesh_total);
    let mut unified_mesh = 0usize;
    // 每个统一 mesh 是否有 flex 载荷 —— 决定 strip group 是否带
    // `STRIPGROUP_IS_DELTA_FLEXED`（`0x04`）。索引 == `items[].unified_mesh`。
    let mut mesh_has_flex: Vec<bool> = Vec::new();
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for ki in 0..m.meshes.len() {
                mesh_has_flex.push(m.mesh_flexes.get(ki).is_some_and(|v| !v.is_empty()));
            }
        }
    }
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            let n_mesh = m.meshes.len();
            let model_lods = m.lods.as_ref().map_or(1, |l| l.num_lods);
            for ki in 0..n_mesh {
                let ml = &all[unified_mesh + ki];
                // 单 LOD 的 mesh_stats 统计（与单 LOD 路径同义：LOD 0 的顶点/三角形数）。
                mesh_stats.push(MeshVtxStat {
                    vertex_count: ml.lod_vertex_counts.first().copied().unwrap_or(0),
                    triangle_count: ml.triangles.first().map_or(0, |t| t.len()),
                });
            }
            for lod in 0..model_lods {
                for ki in 0..n_mesh {
                    let ml = &all[unified_mesh + ki];
                    let tris_u = ml.triangles.get(lod).cloned().unwrap_or_default();
                    // 收集本 LOD 用到的统一顶点（去重 + 升序，保证确定性）。
                    let mut used: Vec<u32> = Vec::new();
                    for t in &tris_u {
                        for &u in t {
                            if !used.contains(&u) {
                                used.push(u);
                            }
                        }
                    }
                    used.sort_unstable();
                    // 三角形下标改写为「verts 内的槽位」。
                    let mut slot: HashMap<u32, u32> = HashMap::with_capacity(used.len());
                    for (i, &u) in used.iter().enumerate() {
                        slot.insert(u, i as u32);
                    }
                    let tris: Vec<[u32; 3]> = tris_u
                        .iter()
                        .map(|t| [slot[&t[0]], slot[&t[1]], slot[&t[2]]])
                        .collect();
                    // 同前：`>` 而非 `>=`，65536 个顶点合法（下标 0..=65535）。
                    if used.len() > crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH {
                        return Err(VtxWriteError::TooManyVertices {
                            model: m.name.clone(),
                            count: used.len(),
                        });
                    }
                    if tris.len() * 3 > i32::MAX as usize {
                        return Err(VtxWriteError::TooManyIndices {
                            model: m.name.clone(),
                            count: tris.len() * 3,
                        });
                    }
                    items.push(Item {
                        unified_mesh: unified_mesh + ki,
                        lod,
                        verts: used,
                        tris,
                        // `nofacial`：本档禁用面部动画。
                        no_facial: m
                            .lods
                            .as_ref()
                            .is_some_and(|l| l.no_facial.get(lod).copied().unwrap_or(false)),
                    });
                }
            }
            unified_mesh += n_mesh;
        }
    }

    // ---- 拆 strip + 预算各数据区 ----
    //
    // 官方约定见 [`plan_strips`]：同一 strip group 的顶点/索引数组被各 strip
    // 按**连续子区间**瓜分，每条 strip 有自己的骨骼调色板。这里的 item 就是
    // 「一个 (model, lod, mesh)」，与单 LOD 路径的 mesh 同义。
    let mut item_plans: Vec<Vec<StripPlan>> = Vec::with_capacity(items.len());
    for it in &items {
        let refs: Vec<&crate::model::Vertex> = it
            .verts
            .iter()
            .map(|&u| &all[it.unified_mesh].vertices[u as usize])
            .collect();
        let plans = plan_strips(&refs, &it.tris, MAX_BONES_PER_STRIP);
        for p in &plans {
            if p.pal.bsc.len() > MAX_STRIP_BONES {
                return Err(VtxWriteError::TooManyStripBones {
                    model: format!("mesh {}", it.unified_mesh),
                    count: p.pal.bsc.len(),
                });
            }
        }
        item_plans.push(plans);
    }
    // 每个 item 的**第一条** strip 在 strip 表里的下标。
    let mut item_strip_at = Vec::with_capacity(items.len());
    let mut strip_cursor = 0usize;
    for plans in &item_plans {
        item_strip_at.push(strip_cursor);
        strip_cursor += plans.len();
    }
    let strip_total = strip_cursor;
    // strip 表在 strip group 表之后，顶点区在 strip 表之后。
    let strip_off = sg_off + sg_total * STRIP_GROUP_SIZE;
    let vertex_off = strip_off + strip_total * STRIP_SIZE;

    let mut item_vertex_at = Vec::with_capacity(items.len());
    let mut item_index_at = Vec::with_capacity(items.len());
    let mut vcursor = vertex_off;
    for plans in &item_plans {
        item_vertex_at.push(vcursor);
        vcursor += plans.iter().map(|p| p.verts.len()).sum::<usize>() * VERTEX_SIZE;
    }
    let mut icursor = vcursor;
    for it in &items {
        item_index_at.push(icursor);
        icursor += it.tris.len() * 6;
    }
    let mut bsc_cursor = icursor;
    let mut strip_bsc_at: Vec<usize> = Vec::with_capacity(strip_total);
    for plans in &item_plans {
        for p in plans {
            strip_bsc_at.push(bsc_cursor);
            bsc_cursor += p.pal.bsc.len() * BONE_STATE_CHANGE_SIZE;
        }
    }
    // material replacement list：**每 LOD 一个**。
    let mat_repl_off = bsc_cursor;
    let material_replacements = write_material_replacements(compiled, num_lods)?;
    let total_len = mat_repl_off + material_replacements.len();

    let mut buf = vec![0u8; total_len];

    // ---- 头部 ----
    put_i32(&mut buf, 0x00, VERSION);
    put_i32(&mut buf, 0x04, 24); // vertexCacheSize（实测 24）
    put_u16(&mut buf, 0x08, 53); // maxBonesPerStrip（实测 53）
    put_u16(&mut buf, 0x0A, 9); // maxBonesPerTri（实测 9）
    put_i32(&mut buf, 0x0C, 3); // maxBonesPerVertex
    put_i32(&mut buf, 0x10, checksum);
    put_i32(&mut buf, 0x14, num_lods as i32);
    put_i32(&mut buf, 0x18, mat_repl_off as i32);
    put_i32(&mut buf, 0x1C, bp_count as i32);
    put_i32(&mut buf, 0x20, bp_off as i32);

    // ---- 结构树：bodypart → model → LOD[] → mesh[] → stripgroup ----
    let mut model_cursor = 0usize;
    let mut lod_cursor = 0usize;
    let mut mesh_cursor = 0usize;
    let mut item_cursor = 0usize;
    let mut bp_abs = bp_off;
    for bp in &compiled.bodyparts {
        put_i32(&mut buf, bp_abs, bp.models.len() as i32);
        put_i32(
            &mut buf,
            bp_abs + 4,
            (model_off + model_cursor * MODEL_SIZE) as i32 - bp_abs as i32,
        );
        for m in &bp.models {
            let model_abs = model_off + model_cursor * MODEL_SIZE;
            let model_lods = m.lods.as_ref().map_or(1, |l| l.num_lods);
            let switch_points: Vec<f32> = m
                .lods
                .as_ref()
                .map(|l| l.switch_points.clone())
                .unwrap_or_else(|| vec![0.0]);
            put_i32(&mut buf, model_abs, model_lods as i32);
            put_i32(
                &mut buf,
                model_abs + 4,
                (lod_off + lod_cursor * MODEL_LOD_SIZE) as i32 - model_abs as i32,
            );
            for lod in 0..model_lods {
                let lod_abs = lod_off + (lod_cursor + lod) * MODEL_LOD_SIZE;
                put_i32(&mut buf, lod_abs, m.meshes.len() as i32);
                put_i32(
                    &mut buf,
                    lod_abs + 4,
                    (mesh_off + mesh_cursor * MESH_SIZE) as i32 - lod_abs as i32,
                );
                // switchPoint：LOD 0 恒 0；其余用编译期算好的值。
                let sp = switch_points.get(lod).copied().unwrap_or(0.0);
                put_f32(&mut buf, lod_abs + 8, sp);

                for ki in 0..m.meshes.len() {
                    let mesh_abs = mesh_off + (mesh_cursor + ki) * MESH_SIZE;
                    put_i32(&mut buf, mesh_abs, 1); // numStripGroups
                    put_i32(
                        &mut buf,
                        mesh_abs + 4,
                        (sg_off + (item_cursor + ki) * STRIP_GROUP_SIZE) as i32 - mesh_abs as i32,
                    );
                    put_u8(&mut buf, mesh_abs + 8, 0); // mesh flags
                }
                item_cursor += m.meshes.len();
                mesh_cursor += m.meshes.len();
            }
            model_cursor += 1;
            lod_cursor += model_lods;
        }
        bp_abs += BODY_PART_SIZE;
    }

    // ---- strip group + strip + 顶点 + 索引 + bsc ----
    for (ii, it) in items.iter().enumerate() {
        let sg_abs = sg_off + ii * STRIP_GROUP_SIZE;
        let v_abs = item_vertex_at[ii];
        let i_abs = item_index_at[ii];
        let plans = &item_plans[ii];
        let first_strip = item_strip_at[ii];
        let st_abs = strip_off + first_strip * STRIP_SIZE;
        // 组顶点/索引数组 = 各 strip 的子区间首尾相接（见 [`StripPlan`]）。
        let nv: usize = plans.iter().map(|p| p.verts.len()).sum();
        let ni: usize = plans.iter().map(|p| p.tris.len() * 3).sum();

        put_i32(&mut buf, sg_abs, nv as i32);
        put_i32(&mut buf, sg_abs + 4, (v_abs - sg_abs) as i32);
        put_i32(&mut buf, sg_abs + 8, ni as i32);
        put_i32(&mut buf, sg_abs + 12, (i_abs - sg_abs) as i32);
        put_i32(&mut buf, sg_abs + 16, plans.len() as i32); // numStrips
        put_i32(&mut buf, sg_abs + 20, (st_abs - sg_abs) as i32);
        // `stripGroup.flags`（**结构体偏移 +24**，`StripGroupHeader_t` 的最后一个字段）。
        //
        // `ComputeStripGroupFlags`（定义在 `optimize.cpp:968`）：
        //   `isFlexed`   ⟹ `0x04` `STRIPGROUP_IS_DELTA_FLEXED`
        //   `isHWSkinned`⟹ `0x02` `STRIPGROUP_IS_HWSKINNED`
        //
        // ⚠️ **不写 `0x01`（`IS_FLEXED`）** —— 反汇编 L4D2 的 `studiomdl.exe`
        // （`0x0041b9c8`：`MOV [EAX+0x3c],0x4`）与语料直方图
        // （3302 个 `.dx90.vtx` / 6625 个 strip group：`0x2`×6581、`0x6`×44，
        // **`0x01` 零次**）双重确认。SDK 源码在这点上**是过期的**。
        //
        // `nofacial` 的效果就落在这里：`forceNoFlex` 把 `triangleIsFlexed`
        // 强制为 `false`（`optimize.cpp:1311`），于是本档**不再有带 `0x04` 的组**。
        let sg_flags = if it.no_facial {
            STRIPGROUP_IS_HWSKINNED
        } else {
            let is_flexed = mesh_has_flex.get(it.unified_mesh).copied().unwrap_or(false);
            if is_flexed {
                STRIPGROUP_IS_HWSKINNED | STRIPGROUP_IS_DELTA_FLEXED
            } else {
                STRIPGROUP_IS_HWSKINNED
            }
        };
        put_u8(&mut buf, sg_abs + 24, sg_flags);

        // strip（tri-list）：一个 strip group 里可能有**多条** strip，
        // 各占组顶点/索引数组的一段**连续子区间**（见 [`StripPlan`]）。
        let mut v_cur = 0usize; // 组顶点数组写游标
        let mut i_cur = 0usize; // 组索引数组写游标（单位：索引）
        for (si, p) in plans.iter().enumerate() {
            let st_abs = strip_off + (first_strip + si) * STRIP_SIZE;
            let bsc_abs = strip_bsc_at[first_strip + si];
            let pal = &p.pal;

            put_i32(&mut buf, st_abs, (p.tris.len() * 3) as i32); // indexCount
            put_i32(&mut buf, st_abs + 4, i_cur as i32); // indexOffset（组内起点）
            put_i32(&mut buf, st_abs + 8, p.verts.len() as i32); // vertexCount
            put_i32(&mut buf, st_abs + 12, v_cur as i32); // vertOffset（组内起点）
            // `numBones` = max(顶点 numBones)（见 [`build_strip_palette`]）。
            put_u16(&mut buf, st_abs + 16, pal.num_bones);
            put_u8(&mut buf, st_abs + 18, STRIP_IS_TRILIST);
            put_i32(&mut buf, st_abs + 19, pal.bsc.len() as i32); // boneStateChangeCount
            put_i32(&mut buf, st_abs + 23, (bsc_abs - st_abs) as i32);

            // 顶点：`origMeshVertID` 用 `finalMeshVertID`（多 LOD 的关键）。
            //
            // ⚠️ `p.verts` 里存的是 **`it.verts` 的下标（槽位）**，不是统一顶点号
            // —— `plan_strips` 的输入 `it.tris` 就是这个空间的。必须先经
            // `it.verts[slot]` 翻回统一顶点号，才能查布局。
            for (vi, &slot) in p.verts.iter().enumerate() {
                let o = v_abs + (v_cur + vi) * VERTEX_SIZE;
                let u = it.verts[slot as usize];
                let v = &all[it.unified_mesh].vertices[u as usize];
                let bone_count = v.bones.len().min(3) as u8;
                buf[o] = 0;
                buf[o + 1] = 1;
                buf[o + 2] = 2;
                buf[o + 3] = bone_count.max(1);
                let local = layout.mesh_local_id(it.unified_mesh, u).ok_or_else(|| {
                    VtxWriteError::Internal(crate::tr_fmt!(
                        "the unified vertex %{u} of mesh %{mesh} is not in the layout (LOD %{lod})",
                        u = u,
                        mesh = it.unified_mesh,
                        lod = it.lod
                    ))
                })?;
                if local > u16::MAX as u32 {
                    return Err(VtxWriteError::TooManyVertices {
                        model: format!("mesh {}", it.unified_mesh),
                        count: local as usize,
                    });
                }
                put_u16(&mut buf, o + 4, local as u16);
                // `boneID[]`：**硬件**下标（见 [`build_strip_palette`]）。
                // 写成全局下标会让引擎读越界的调色板项 ⟹ 顶点错乱。
                buf[o + 6..o + 9].copy_from_slice(&pal.hw[vi]);
            }

            // 索引：值是**组内绝对顶点下标**（`p.tris` 已含 `v_off` 基址）。
            // 逐 strip 可选做缓存优化（跨 strip 重排会破坏子区间划分）。
            if opts.optimize_vertex_cache {
                let src: Vec<[u16; 3]> = p
                    .tris
                    .iter()
                    .map(|t| [t[0] as u16, t[1] as u16, t[2] as u16])
                    .collect();
                let opt = optimize_group_indices(&src, nv)?;
                for (ti, tri) in opt.iter().enumerate() {
                    let o = i_abs + (i_cur + ti * 3) * 2;
                    put_u16(&mut buf, o, tri[0]);
                    put_u16(&mut buf, o + 2, tri[1]);
                    put_u16(&mut buf, o + 4, tri[2]);
                }
            } else {
                for (ti, tri) in p.tris.iter().enumerate() {
                    let o = i_abs + (i_cur + ti * 3) * 2;
                    put_u16(&mut buf, o, tri[0] as u16);
                    put_u16(&mut buf, o + 2, tri[1] as u16);
                    put_u16(&mut buf, o + 4, tri[2] as u16);
                }
            }

            // bone state change：`{hardwareID, newBoneID}`，`hardwareID` 恒为条目下标。
            for (k, &gb) in pal.bsc.iter().enumerate() {
                let q = bsc_abs + k * BONE_STATE_CHANGE_SIZE;
                put_i32(&mut buf, q, k as i32);
                put_i32(&mut buf, q + 4, gb);
            }

            v_cur += p.verts.len();
            i_cur += p.tris.len() * 3;
        }
    }

    buf[mat_repl_off..].copy_from_slice(&material_replacements);

    if buf.len() != total_len {
        return Err(VtxWriteError::Internal(crate::tr_fmt!(
            "the written length %{got} does not match the budget %{want}",
            got = buf.len(),
            want = total_len
        )));
    }
    Ok(VtxWriteOutcome {
        bytes: buf,
        mesh_stats,
        checksum,
    })
}

/// 自洽性检查：各段必须落在文件内、且 mesh 数与 MDL 一致。
///
/// 与 MDL/VVD 的自检同样的理由 —— 宁可显式失败，也不要产出
/// 「能解析但进游戏不可见」的文件。
fn write_material_replacements(
    compiled: &CompiledModelDesc,
    levels: usize,
) -> Result<Vec<u8>, VtxWriteError> {
    let key = |name: &str| {
        name.replace('\\', "/")
            .to_ascii_lowercase()
            .trim_end_matches(".vmt")
            .to_owned()
    };
    let mut output = vec![0; levels * MATERIAL_REPLACEMENT_LIST_SIZE];
    for level in 1..levels {
        let mut replacements = std::collections::BTreeMap::<u16, String>::new();
        for model in compiled.desc.bodyparts.iter().flat_map(|part| &part.models) {
            let Some(lod) = model.lods.get(level - 1) else {
                continue;
            };
            for [source, target] in &lod.material_replacements {
                let slot = compiled
                    .desc
                    .materials
                    .textures
                    .iter()
                    .position(|texture| key(&texture.name) == key(source))
                    .and_then(|slot| u16::try_from(slot).ok())
                    .filter(|slot| *slot <= i16::MAX as u16)
                    .ok_or_else(|| {
                        VtxWriteError::Internal(format!(
                            "LOD material replacement references an absent material: {source}"
                        ))
                    })?;
                if target.is_empty() || target.chars().any(char::is_control) {
                    return Err(VtxWriteError::Internal(
                        "Invalid LOD replacement material name".into(),
                    ));
                }
                let target = target.replace('\\', "/");
                if let Some(previous) = replacements.insert(slot, target.clone())
                    && previous != target
                {
                    return Err(VtxWriteError::Internal(format!(
                        "Conflicting LOD material replacements for {source}"
                    )));
                }
            }
        }
        if replacements.is_empty() {
            continue;
        }
        let header = level * MATERIAL_REPLACEMENT_LIST_SIZE;
        let records = output.len();
        let count = replacements.len();
        output.resize(records + count * 6, 0);
        put_i32(&mut output, header, count as i32);
        put_i32(&mut output, header + 4, (records - header) as i32);
        for (index, (slot, target)) in replacements.into_iter().enumerate() {
            let record = records + index * 6;
            let string = output.len();
            output.extend_from_slice(target.as_bytes());
            output.push(0);
            put_u16(&mut output, record, slot);
            put_i32(&mut output, record + 2, (string - record) as i32);
        }
    }
    Ok(output)
}

pub fn check_invariants(
    vtx: &VtxWriteOutcome,
    compiled: &CompiledModelDesc,
) -> Result<(), VtxWriteError> {
    let b = &vtx.bytes;
    let expect_meshes: usize = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.meshes.len())
        .sum();
    if vtx.mesh_stats.len() != expect_meshes {
        return Err(VtxWriteError::Internal(crate::tr_fmt!(
            "mesh count mismatch: the VTX has %{vtx}, the MDL has %{mdl}",
            vtx = vtx.mesh_stats.len(),
            mdl = expect_meshes
        )));
    }
    let invalid = || VtxWriteError::Internal("invalid VTX structure or offset".into());
    let span = |start: usize, count: usize, stride: usize| -> Result<(), VtxWriteError> {
        let end = count
            .checked_mul(stride)
            .and_then(|n| start.checked_add(n))
            .ok_or_else(invalid)?;
        if end > b.len() {
            return Err(invalid());
        }
        Ok(())
    };
    let g = |o: usize| -> Result<usize, VtxWriteError> {
        span(o, 1, 4)?;
        usize::try_from(i32::from_le_bytes(b[o..o + 4].try_into().unwrap())).map_err(|_| invalid())
    };
    let relative = |base: usize, field: usize| -> Result<usize, VtxWriteError> {
        base.checked_add(g(field)?).ok_or_else(invalid)
    };
    span(0, 1, HEADER_SIZE)?;
    // 头部
    if g(0x00)? != VERSION as usize {
        return Err(VtxWriteError::Internal(crate::tr("version is not 7")));
    }
    if b[0x10..0x14] != vtx.checksum.to_le_bytes() {
        return Err(VtxWriteError::Internal(crate::tr(
            "the checksum does not match the MDL",
        )));
    }
    // 每个 body part / model / LOD / mesh / strip group 的偏移都要在文件内。
    let bp_count = g(0x1C)?;
    let bp_off = g(0x20)?;
    if bp_count != compiled.bodyparts.len() {
        return Err(VtxWriteError::Internal(
            "bodypart count differs from MDL".into(),
        ));
    }
    span(bp_off, bp_count, BODY_PART_SIZE)?;
    let global_lods = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.lods.as_ref().map_or(1, |l| l.num_lods))
        .max()
        .unwrap_or(1);
    if g(0x14)? != global_lods {
        return Err(VtxWriteError::Internal(
            "LOD count differs from compiled model".into(),
        ));
    }
    span(g(0x18)?, global_lods, MATERIAL_REPLACEMENT_LIST_SIZE)?;
    for (i, bp) in compiled.bodyparts.iter().enumerate() {
        let at = bp_off + i * BODY_PART_SIZE;
        let mc = g(at)?;
        let m_base = relative(at, at + 4)?;
        if mc != bp.models.len() {
            return Err(VtxWriteError::Internal(
                "bodygroup choice count differs from MDL".into(),
            ));
        }
        span(m_base, mc, MODEL_SIZE)?;
        for (k, model) in bp.models.iter().enumerate() {
            let ma = m_base + k * MODEL_SIZE;
            let lc = g(ma)?;
            let l_base = relative(ma, ma + 4)?;
            if lc != model.lods.as_ref().map_or(1, |l| l.num_lods) {
                return Err(VtxWriteError::Internal(
                    "bodygroup LOD count differs from MDL".into(),
                ));
            }
            span(l_base, lc, MODEL_LOD_SIZE)?;
            for l in 0..lc {
                let la = l_base + l * MODEL_LOD_SIZE;
                let kc = g(la)?;
                let k_base = relative(la, la + 4)?;
                if kc != model.meshes.len() {
                    return Err(VtxWriteError::Internal(
                        "LOD mesh count differs from MDL".into(),
                    ));
                }
                span(k_base, kc, MESH_SIZE)?;
                for kk in 0..kc {
                    let ka = k_base + kk * MESH_SIZE;
                    let gc = g(ka)?;
                    let g_base = relative(ka, ka + 4)?;
                    span(g_base, gc, STRIP_GROUP_SIZE)?;
                    for gg in 0..gc {
                        let ga = g_base + gg * STRIP_GROUP_SIZE;
                        // 顶点/索引/strip 数组也必须在文件内。
                        span(relative(ga, ga + 4)?, g(ga)?, VERTEX_SIZE)?;
                        span(relative(ga, ga + 12)?, g(ga + 8)?, 2)?;
                        span(relative(ga, ga + 20)?, g(ga + 16)?, STRIP_SIZE)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn put_i32(buf: &mut [u8], off: usize, v: i32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_f32(buf: &mut [u8], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u8(buf: &mut [u8], off: usize, v: u8) {
    buf[off] = v;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile;
    use crate::model::ModelDesc;

    const TOML: &str = r#"
[model]
name = "models/test/minimal.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"
"#;

    const SMD: &str = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
    1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000
end
triangles
myprop
  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000
  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000
end
"#;

    fn minimal() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-vtx-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), SMD).unwrap();
        let desc = ModelDesc::from_toml(TOML).unwrap();
        let c = compile(&desc, &d).expect("测试模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 造一个**多骨骼**模型：4 根骨骼、4 个顶点各绑不同骨骼。
    ///
    /// 用来验证「调色板项数 == 顶点用到的唯一全局骨骼数」这条恒等式
    /// （语料 6677/6677 成立）—— 单骨骼夹具（[`minimal`]）上它**恒真**，
    /// 是**空洞测试**，测不出任何东西。
    fn multi_bone() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-vtxmb-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let smd = r#"version 1
nodes
  0 "root" -1
  1 "b1" 0
  2 "b2" 1
  3 "b3" 2
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
    1 0.000000 0.000000 4.000000 0.000000 0.000000 0.000000
    2 0.000000 4.000000 8.000000 0.000000 0.000000 0.000000
    3 4.000000 4.000000 12.000000 0.000000 0.000000 0.000000
end
triangles
myprop
  0 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000
  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
  2 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 2 1.000000
  3 0.000000 8.000000 4.000000 0.000000 0.000000 1.000000 0.500000 0.500000 1 3 1.000000
end
"#;
        std::fs::write(d.join("myprop-ref.smd"), smd).unwrap();
        // 骨骼表必须与 SMD 的 `nodes` 段一致 —— 否则编译器会拒绝
        // （「有 3 根骨骼不在 [[bones]] 中」）。
        let toml = TOML.replace(
            "[[bones]]\nname = \"tip\"\nparent = \"root\"\n",
            "[[bones]]\nname = \"b1\"\nparent = \"root\"\n\n\
             [[bones]]\nname = \"b2\"\nparent = \"b1\"\n\n\
             [[bones]]\nname = \"b3\"\nparent = \"b2\"\n",
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("多骨骼夹具应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 造一个**足够大**的网格：`(n+1)²` 个顶点的规则栅格 → `2n²` 个三角形。
    ///
    /// 用栅格是因为它的**原序已经很缓存友好**（行优先），
    /// meshopt 仍有优化空间但不至于把顺序完全打乱 —— 正好能同时验证
    /// 「顺序变了」与「拓扑没变」。
    fn grid_model(n: usize) -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-vtxgrid-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        // ⚠️ mdlc 的 SMD 解析器**不接受引用行**（`<v0> <v1> <v2> <bone>`，
        // 4 token）—— 每个顶点必须是**完整的 12 token 定义行**。
        // （原始 SMD 格式允许混排，本实现从简，只支持全定义形式。）
        // 所以每个三角形的三个顶点都重复写完整定义。
        let mut smd = String::from(
            "version 1\nnodes\n  0 \"root\" -1\nend\nskeleton\n  time 0\n    0 0 0 0 0 0 0\nend\ntriangles\nmyprop\n",
        );
        let row = |i: usize| -> String {
            let fx = (i % (n + 1)) as f32;
            let fy = (i / (n + 1)) as f32;
            // 12 token：`bone x y z nx ny nz u v links bone1 weight1`
            format!(
                "  0 {fx} {fy} 0 0 0 1 {} {} 1 0 1.0\n",
                fx / n as f32,
                fy / n as f32
            )
        };
        for y in 0..n {
            for x in 0..n {
                let a = y * (n + 1) + x;
                let b = a + 1;
                let c = a + (n + 1);
                let e = c + 1;
                for tri in [[a, b, c], [b, e, c]] {
                    for v in tri {
                        smd.push_str(&row(v));
                    }
                }
            }
        }
        smd.push_str("end\n");

        std::fs::write(d.join("myprop-ref.smd"), &smd).unwrap();
        let toml = r#"
[model]
name = "models/test/grid.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[hitboxes]
set_name = "default"

[[hitboxes.boxes]]
bone = "root"
bbmin = [-1.0, -1.0, -1.0]
bbmax = [1.0, 1.0, 1.0]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("栅格模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 读第一个 strip group 的 `(origMeshVertID 表, 三角形列表)`。
    fn first_group(out: &VtxWriteOutcome) -> (Vec<u16>, Vec<[u16; 3]>) {
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bpo = g(0x20) as usize;
        let mo = bpo + g(bpo + 4) as usize;
        let lo = mo + g(mo + 4) as usize;
        let mh = lo + g(lo + 4) as usize;
        let sgo = mh + g(mh + 4) as usize;
        let num_verts = g(sgo) as usize;
        let vert_off = sgo + g(sgo + 4) as usize;
        let sg_index_off = sgo + g(sgo + 12) as usize;
        let strip_off = sgo + g(sgo + 20) as usize;
        let idx_count = g(strip_off) as usize;
        // ⚠️ strip 的 indexOffset 也**相对 strip group**
        let io = sg_index_off + g(strip_off + 4) as usize;
        let map = (0..num_verts)
            .map(|v| u16::from_le_bytes([b[vert_off + v * 9 + 4], b[vert_off + v * 9 + 5]]))
            .collect();
        let tr = (0..idx_count / 3)
            .map(|t| {
                let o = io + t * 6;
                [
                    u16::from_le_bytes([b[o], b[o + 1]]),
                    u16::from_le_bytes([b[o + 2], b[o + 3]]),
                    u16::from_le_bytes([b[o + 4], b[o + 5]]),
                ]
            })
            .collect();
        (map, tr)
    }

    fn sorted3(t: &[u16; 3]) -> [u16; 3] {
        let mut s = *t;
        s.sort_unstable();
        s
    }

    /// **默认必须与加本特性之前逐字节相同** —— 即 `write_vtx` 不做优化。
    ///
    /// 判据：`write_vtx` 与显式 `VtxOptions { optimize_vertex_cache: false }`
    /// 产物相同，且**与优化版不同**。
    #[test]
    fn vtx_optimization_is_off_by_default() {
        let c = grid_model(8);
        let def = write_vtx(&c).unwrap();
        let explicit_off = write_vtx_with(
            &c,
            VtxOptions {
                optimize_vertex_cache: false,
            },
        )
        .unwrap();
        let on = write_vtx_with(
            &c,
            VtxOptions {
                optimize_vertex_cache: true,
            },
        )
        .unwrap();
        assert_eq!(def.bytes, explicit_off.bytes, "默认应等价于显式关闭");
        assert_ne!(def.bytes, on.bytes, "打开优化后产物应当不同");
    }

    /// 优化**只重排三角形顺序**，不改拓扑、绕序、顶点池。
    #[test]
    fn vtx_optimization_only_reorders_triangles() {
        let c = grid_model(8);
        let def = write_vtx(&c).unwrap();
        let on = write_vtx_with(
            &c,
            VtxOptions {
                optimize_vertex_cache: true,
            },
        )
        .unwrap();

        // 长度必须相同（只重排，不增删）
        assert_eq!(def.bytes.len(), on.bytes.len(), "优化不应改变文件长度");

        let (map_a, tr_a) = first_group(&def);
        let (map_b, tr_b) = first_group(&on);
        assert_eq!(map_a, map_b, "origMeshVertID 表不应改变");
        assert_eq!(tr_a.len(), tr_b.len(), "三角形数不应改变");

        // 拓扑：无序集合相同
        let mut sa: Vec<_> = tr_a.iter().map(sorted3).collect();
        let mut sb: Vec<_> = tr_b.iter().map(sorted3).collect();
        sa.sort_unstable();
        sb.sort_unstable();
        assert_eq!(sa, sb, "三角形**集合**必须不变");

        // 绕序：未排序三元组多重集相同
        let mut ra = tr_a.clone();
        let mut rb = tr_b.clone();
        ra.sort_unstable();
        rb.sort_unstable();
        assert_eq!(ra, rb, "**绕序**必须不变（否则背面剔除会反过来）");

        // 顺序：应当变了（否则优化没生效）
        assert_ne!(tr_a, tr_b, "三角形顺序应当被重排（否则优化没生效）");
    }

    /// 顶点缓存 miss 数**必须下降** —— 这是「优化有效」的正面判据。
    #[test]
    fn vtx_optimization_reduces_cache_misses() {
        // 用 16 项 LRU 近似 GPU 的 post-transform cache。
        fn misses(tr: &[[u16; 3]], cap: usize) -> usize {
            let mut cache: Vec<u16> = Vec::new();
            let mut n = 0;
            for t in tr {
                for &v in t {
                    match cache.iter().position(|&c| c == v) {
                        Some(i) => {
                            cache.remove(i);
                            cache.push(v);
                        }
                        None => {
                            n += 1;
                            cache.push(v);
                            if cache.len() > cap {
                                cache.remove(0);
                            }
                        }
                    }
                }
            }
            n
        }
        let c = grid_model(10);
        let def = write_vtx(&c).unwrap();
        let on = write_vtx_with(
            &c,
            VtxOptions {
                optimize_vertex_cache: true,
            },
        )
        .unwrap();
        let (_, tr_a) = first_group(&def);
        let (_, tr_b) = first_group(&on);
        let ma = misses(&tr_a, 16);
        let mb = misses(&tr_b, 16);
        assert!(
            mb < ma,
            "优化后 miss 应下降：优化前 {ma}，优化后 {mb}（三角形 {} 个）",
            tr_a.len()
        );
    }

    /// 空网格（0 三角形）不应 panic。
    #[test]
    fn vtx_optimization_handles_empty_mesh() {
        let empty: Vec<[u16; 3]> = Vec::new();
        assert_eq!(optimize_group_indices(&empty, 0).unwrap(), empty);
    }

    /// **只能重排、不能改拓扑** —— 验证判据本身能区分这两种情况。
    ///
    /// meshopt 不会改拓扑，所以没法从外部触发守门分支；这里直接对
    /// **判据**做单元测试：同一套排序比较逻辑，对「同集合重排」必须判
    /// **相同**，对「改了拓扑」必须判**不同**。
    #[test]
    fn topology_guard_distinguishes_different_sets() {
        let a = [[0u16, 1, 2], [2, 3, 0]];
        let same_set = [[2u16, 3, 0], [0, 1, 2]]; // 只是顺序变了
        let other_set = [[0u16, 1, 2], [0, 3, 1]]; // 第二个三角形换了顶点

        let prep = |tr: &[[u16; 3]]| {
            let mut v: Vec<[u16; 3]> = tr.iter().map(sorted3).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(prep(&a), prep(&same_set), "同集合（仅重排）应判**相同**");
        assert_ne!(prep(&a), prep(&other_set), "改拓扑应判**不同**");
    }

    #[test]
    fn struct_sizes_match_verified_layout() {
        assert_eq!(HEADER_SIZE, 36);
        assert_eq!(BODY_PART_SIZE, 8);
        assert_eq!(MODEL_SIZE, 8);
        assert_eq!(MODEL_LOD_SIZE, 12);
        assert_eq!(MESH_SIZE, 9);
        assert_eq!(STRIP_GROUP_SIZE, 25);
        assert_eq!(STRIP_SIZE, 27);
        assert_eq!(VERTEX_SIZE, 9);
        assert_eq!(BONE_STATE_CHANGE_SIZE, 8);
    }

    #[test]
    fn writes_header_and_tree() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        assert_eq!(g(0x00), 7, "version");
        assert_eq!(g(0x10), out.checksum, "checksum 必须与 MDL 一致");
        assert_eq!(g(0x14), 1, "numLODs");
        assert_eq!(g(0x1C), 1, "bodyPartCount");
        assert_eq!(g(0x20), HEADER_SIZE as i32, "bodyPartOffset");
        // body part → model → LOD → mesh 的偏移链必须自洽。
        let bp = g(0x20) as usize;
        assert_eq!(g(bp), 1, "numModels");
        let m_abs = bp + g(bp + 4) as usize;
        assert_eq!(g(m_abs), 1, "numLODs");
        let l_abs = m_abs + g(m_abs + 4) as usize;
        assert_eq!(g(l_abs), 1, "numMeshes");
        let k_abs = l_abs + g(l_abs + 4) as usize;
        assert_eq!(g(k_abs), 1, "numStripGroups");
        check_invariants(&out, &c).expect("写出后必须自洽");
    }

    #[test]
    fn strip_group_flags_is_hwskinned_not_flexed() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        assert_eq!(
            b[sg_abs + 24],
            STRIPGROUP_IS_HWSKINNED,
            "无 flex 时必须用 HWSKINNED(0x02)；写 FLEXED(0x01) 会让引擎查不存在的 flex 数据"
        );
        assert_ne!(b[sg_abs + 24], STRIPGROUP_IS_FLEXED);
    }

    /// **有 VTA 载荷时 flags 必须是 `0x6`**（HWSKINNED | DELTA_FLEXED）。
    ///
    /// 官方带 VTA 的产物实测 `0x6`（`cmp_vta.js` 的 `vta_qc.dx90.vtx`），
    /// 语料 17 个有 flex 的 mesh **全部** `0x6`、5568 个无 flex 的**全部**
    /// `0x2`（`probe_vtx_flex_flags.js`，0 例外）。
    ///
    /// ⚠️ 关键是 **`0x04 DELTA_FLEXED`**，不是 `0x01 IS_FLEXED` ——
    /// `optimize.cpp:975` 注释：「Going forward, DX9 models are delta flexed」。
    #[test]
    fn strip_group_flags_is_delta_flexed_when_mesh_has_flexes() {
        use crate::flex::{ResolvedFlex, ResolvedVertAnim};
        let mut c = minimal();
        c.bodyparts[0].models[0].mesh_flexes = vec![vec![ResolvedFlex {
            flexdesc: 0,
            targets: [0.0, 1.0, 10.0, 11.0],
            flexpair: 0,
            vertanimtype: 0,
            vertanims: vec![ResolvedVertAnim {
                wrinkle: 0.0,
                index: 0,
                speed: 255,
                side: 0,
                delta: [0.0, 0.0, 1.0],
                ndelta: [0.0, 0.0, 0.0],
            }],
        }]];
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        let want = STRIPGROUP_IS_HWSKINNED | STRIPGROUP_IS_DELTA_FLEXED;
        assert_eq!(
            b[sg_abs + 24],
            want,
            "有 VTA 载荷时必须写 0x6（HWSKINNED|DELTA_FLEXED）；\
             否则引擎不走 flex 路径，形状不生效"
        );
        assert_eq!(want, 0x06, "0x6 是实测值");
    }

    /// `nofacial`：该档的 strip group **不再带 `0x04`**。
    ///
    /// oracle（`docs\_probe\smdl\lodnf.qc`，官方 studiomdl.exe 实测，
    /// 用 `dump_sg_flags.js` 逐 strip group 读 `+24`）：
    /// ```text
    /// lod0（facial）  : sg=1 flags=[0x2] ×1、sg=1 flags=[0x6] ×1
    /// lod1（nofacial）: sg=1 flags=[0x2] ×2      ← 0x04 全部消失
    /// ```
    ///
    /// ⚠️ **判据是「不再有 `0x04`」，不是「strip group 数下降」**：
    /// 本夹具的 mesh 全 flexed，所以两档都是 **1 个** strip group，
    /// 只有 flags 从 `0x6` 变成 `0x2` —— 只断言数量会漏掉。
    #[test]
    fn no_facial_drops_delta_flexed_flag() {
        use crate::flex::{ResolvedFlex, ResolvedVertAnim};
        let mk_flex = || {
            vec![vec![ResolvedFlex {
                flexdesc: 0,
                targets: [0.0, 1.0, 10.0, 11.0],
                flexpair: 0,
                vertanimtype: 0,
                vertanims: vec![ResolvedVertAnim {
                    wrinkle: 0.0,
                    index: 0,
                    speed: 255,
                    side: 0,
                    delta: [0.0, 0.0, 1.0],
                    ndelta: [0.0, 0.0, 0.0],
                }],
            }]]
        };
        // 逐档读 (numStripGroups, 各 strip group 的 flags)。
        let read = |b: &[u8], lod: usize| -> (i32, Vec<u8>) {
            let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
            let bp = g(0x20) as usize;
            let m_abs = bp + g(bp + 4) as usize;
            let l_abs = m_abs + g(m_abs + 4) as usize + lod * 12;
            let k_abs = l_abs + g(l_abs + 4) as usize;
            let nsg = g(k_abs);
            let mut sg = k_abs + g(k_abs + 4) as usize;
            let mut out = Vec::new();
            for _ in 0..nsg {
                out.push(b[sg + 24]);
                sg += STRIP_GROUP_SIZE;
            }
            (nsg, out)
        };

        // ---- facial（默认）：两档都应有 0x6 ----
        let mut c = multi_lod();
        c.bodyparts[0].models[0].mesh_flexes = mk_flex();
        let facial = write_vtx(&c).unwrap().bytes;
        for lod in 0..2 {
            let (nsg, fl) = read(&facial, lod);
            assert_eq!(nsg, 1, "lod{lod} 应只有 1 个 strip group");
            assert_eq!(
                fl,
                vec![STRIPGROUP_IS_HWSKINNED | STRIPGROUP_IS_DELTA_FLEXED],
                "lod{lod} facial 档必须是 0x6"
            );
        }

        // ---- nofacial 只作用在 LOD 1 ----
        let mut c2 = multi_lod();
        c2.bodyparts[0].models[0].mesh_flexes = mk_flex();
        c2.bodyparts[0].models[0].lods.as_mut().unwrap().no_facial = vec![false, true];
        let nf = write_vtx(&c2).unwrap().bytes;
        let (nsg0, fl0) = read(&nf, 0);
        assert_eq!(nsg0, 1);
        assert_eq!(
            fl0,
            vec![STRIPGROUP_IS_HWSKINNED | STRIPGROUP_IS_DELTA_FLEXED],
            "LOD 0 不受 nofacial 影响，仍是 0x6"
        );
        let (nsg1, fl1) = read(&nf, 1);
        assert_eq!(nsg1, 1, "全 flexed 的 mesh：数量**不变**（这正是易漏之处）");
        assert_eq!(
            fl1,
            vec![STRIPGROUP_IS_HWSKINNED],
            "LOD 1 nofacial ⟹ 必须丢掉 0x04（官方实测 0x2）"
        );
        assert!(
            !fl1.contains(&STRIPGROUP_IS_DELTA_FLEXED),
            "nofacial 档绝不能出现 DELTA_FLEXED"
        );
    }

    #[test]
    fn strip_is_trilist() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        let st_abs = sg_abs + g(sg_abs + 20) as usize;
        assert_eq!(b[st_abs + 18], STRIP_IS_TRILIST);
        // 索引数 = 三角形数 × 3
        assert_eq!(g(st_abs), 3, "1 个三角形 → 3 个索引");
    }

    #[test]
    fn vertex_orig_mesh_vert_id_matches_order() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        let st_abs = sg_abs + g(sg_abs + 20) as usize;
        let v_abs = sg_abs + g(sg_abs + 4) as usize;
        // `boneStateChange` 区在索引区之后；偏移由 strip 给出。
        let bsc_abs = st_abs + g(st_abs + 23) as usize;
        let num_bsc = g(st_abs + 19) as usize;
        // 调色板：`hardwareID` **恒等于条目下标**，`newBoneID` 是全局骨骼。
        let palette: Vec<i32> = (0..num_bsc)
            .map(|i| {
                let q = bsc_abs + i * BONE_STATE_CHANGE_SIZE;
                assert_eq!(g(q), i as i32, "hardwareID 必须等于条目下标");
                g(q + 4)
            })
            .collect();
        for i in 0..3usize {
            let o = v_abs + i * VERTEX_SIZE;
            let id = u16::from_le_bytes([b[o + 4], b[o + 5]]);
            assert_eq!(id as usize, i, "origMeshVertID 必须等于 mesh 内顶点序号");
            assert_eq!(b[o + 3], 1, "boneCount");
            // ⚠️ `boneID` 是**硬件下标**，不是全局骨骼下标。
            // 要经 `boneStateChange[boneID].newBoneID` 才是全局骨骼。
            let hw = b[o + 6] as usize;
            assert!(hw < num_bsc, "硬件下标 {hw} 越出调色板（{num_bsc} 项）");
            assert_eq!(
                palette[hw], 1,
                "boneID 经调色板应还原为全局骨骼 1（SMD 里是 1 = tip）"
            );
        }
    }

    /// `boneStateChange` 必须是**逐 strip 的调色板**，且能覆盖全部顶点。
    ///
    /// 早先这里恒写 1 项 `{0,0}`、并把 `boneID` 写成**全局**骨骼下标 ⟹
    /// 引擎读 `boneStateChange[3]`（越界）拿到垃圾骨骼，顶点蒙皮到错误骨骼
    /// —— 表现为**顶点错乱 / 模型撕裂**。
    #[test]
    fn bone_state_change_is_a_per_strip_palette() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        let st_abs = sg_abs + g(sg_abs + 20) as usize;
        let v_abs = sg_abs + g(sg_abs + 4) as usize;
        let bsc_abs = st_abs + g(st_abs + 23) as usize;
        let num_bsc = g(st_abs + 19) as usize;
        let num_verts = g(sg_abs) as usize;

        // 调色板必须非空，且 newBoneID 互不相同。
        assert!(num_bsc > 0, "调色板不能为空");
        let globals: Vec<i32> = (0..num_bsc).map(|i| g(bsc_abs + i * 8 + 4)).collect();
        let uniq: std::collections::HashSet<i32> = globals.iter().copied().collect();
        assert_eq!(uniq.len(), num_bsc, "调色板内 newBoneID 必须互不相同");

        // 每个顶点用到的硬件下标都必须落在调色板内（越界 = 引擎读到垃圾）。
        for i in 0..num_verts {
            let o = v_abs + i * VERTEX_SIZE;
            let n = b[o + 3] as usize;
            for k in 0..n {
                let hw = b[o + 6 + k] as usize;
                assert!(
                    hw < num_bsc,
                    "顶点 {i} 槽位 {k} 的硬件下标 {hw} 越界（{num_bsc}）"
                );
            }
            // 未用槽位必须为 0（官方口径，语料 6677/6677）。
            for k in n..3 {
                assert_eq!(b[o + 6 + k], 0, "顶点 {i} 未用槽位 {k} 应为 0");
            }
        }
        // `numBones` = max(顶点 numBones)，不是唯一骨骼数。
        let max_vb = (0..num_verts)
            .map(|i| b[v_abs + i * VERTEX_SIZE + 3] as i32)
            .max()
            .unwrap_or(0);
        assert_eq!(
            i32::from(i16::from_le_bytes([b[st_abs + 16], b[st_abs + 17]])),
            max_vb
        );
    }

    /// 调色板项数必须等于「strip 顶点用到的唯一全局骨骼数」，
    /// 而 `numBones` 必须等于「max(顶点 numBones)」—— **两者不是一回事**。
    ///
    /// 语料实测两条都 6677/6677 成立（`docs/_probe/survey_vtx_bonestate.js`）。
    ///
    /// ⚠️ **夹具必须让这两个量不等**，否则变异测试会逃逸：
    /// 本测试第一版用 [`minimal`]（1 顶点骨骼 / 1 唯一骨骼），
    /// 把 `numBones` 从 `max(顶点 numBones)` 变异成 `bsc.len()` 时
    /// **测试照样全绿**（两个值恒等）。[`multi_bone`] 的 4 个顶点各绑
    /// 1 根**不同**骨骼 ⟹ max=1 而 unique=4，才能真正区分。
    #[test]
    fn palette_size_equals_unique_bones_used() {
        let c = multi_bone();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        let st_abs = sg_abs + g(sg_abs + 20) as usize;
        let v_abs = sg_abs + g(sg_abs + 4) as usize;
        let bsc_abs = st_abs + g(st_abs + 23) as usize;
        let num_bsc = g(st_abs + 19) as usize;
        let num_verts = g(sg_abs) as usize;
        let palette: Vec<i32> = (0..num_bsc).map(|i| g(bsc_abs + i * 8 + 4)).collect();

        let mut used = std::collections::HashSet::new();
        let mut max_vb = 0i32;
        for i in 0..num_verts {
            let o = v_abs + i * VERTEX_SIZE;
            let n = b[o + 3] as i32;
            max_vb = max_vb.max(n);
            for k in 0..n as usize {
                used.insert(palette[b[o + 6 + k] as usize]);
            }
        }
        assert_eq!(
            used.len(),
            num_bsc,
            "调色板项数 {num_bsc} 应等于顶点用到的唯一全局骨骼数 {}",
            used.len()
        );
        // 夹具前提：这两个量必须**不等**，否则本测试是空洞的。
        assert_ne!(
            used.len() as i32,
            max_vb,
            "夹具无效：唯一骨骼数 == max(顶点 numBones)，区分不出两种口径"
        );
        let num_bones = i32::from(i16::from_le_bytes([b[st_abs + 16], b[st_abs + 17]]));
        assert_eq!(
            num_bones,
            max_vb,
            "numBones 必须是 max(顶点 numBones)={max_vb}，而不是唯一骨骼数 {}",
            used.len()
        );
    }

    /// tri-list 按三角形顺序写出，且**绕序是 Source 约定（CW）**。
    ///
    /// # ⚠️ 这条测试的期望值改过一次（R21）
    ///
    /// 原期望是 `[0, 1, 2]` —— 即「原样透传 SMD 顺序」。那是**错的**：
    /// Source 的正面是 CW（与 Blender/OpenGL 的 CCW 相反），官方在导入时
    /// 就把每个三角形的第 2、3 个顶点交换（`v1support.cpp:192-196`，
    /// `flip_triangles` **默认 1**）。
    ///
    /// 原样透传 ⟹ **每个三角形都反向** ⟹ 面法向整体朝内 ——
    /// 这就是用户报的「Blender 里面法向是反的」。
    ///
    /// 现在期望是 `[0, 2, 1]`（第 2、3 个交换）。**这条测试本身不会
    /// 告诉你哪个对** —— 它只是锁住当前行为；真正的判据是
    /// `docs/_probe/probe_winding_order.js` 对 6 个 `vm_test_group` 模型
    /// 的实测（官方与 SMD「同序 0 / 逆序 12996」）。
    #[test]
    fn indices_are_tri_list_in_source_winding() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        let k_abs = l_abs + g(l_abs + 4) as usize;
        let sg_abs = k_abs + g(k_abs + 4) as usize;
        let i_abs = sg_abs + g(sg_abs + 12) as usize;
        let idx: Vec<u16> = (0..3)
            .map(|k| u16::from_le_bytes([b[i_abs + k * 2], b[i_abs + k * 2 + 1]]))
            .collect();
        assert_eq!(
            idx,
            vec![0, 2, 1],
            "tri-list 应按 Source 约定（CW）写出 —— 第 2、3 个角交换。\
             得到 [0,1,2] 说明 `flip_triangles` 没生效（面法向会朝内）"
        );
        // 非空洞硬门：这三个下标必须**互不相同**，否则「交换」不可观测。
        assert!(
            idx[0] != idx[1] && idx[1] != idx[2] && idx[0] != idx[2],
            "三个角下标必须互异，否则测不出绕序：{idx:?}"
        );
    }

    #[test]
    fn material_replacement_list_is_empty() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let mr = g(0x18) as usize;
        assert_eq!(g(mr), 0, "numReplacements 应为 0");
        assert!(mr + MATERIAL_REPLACEMENT_LIST_SIZE <= b.len());
    }

    #[test]
    fn every_mesh_gets_its_own_strip_group() {
        // 两个材质 → 两个 mesh → 两个 strip group。
        let c = {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let d = std::env::temp_dir().join(format!("mdlc-vtx2-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let smd = SMD.replace(
                "myprop\n  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000\n  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000\n  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000\nend",
                "tex_a\n  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000\n  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000\n  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000\ntex_b\n  1 1.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000\n  1 2.000000 0.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000\n  1 1.000000 1.000000 0.000000 0.000000 0.000000 1.000000 0.000000 1.000000 1 1 1.000000\nend",
            );
            std::fs::write(d.join("myprop-ref.smd"), &smd).unwrap();
            let toml = TOML.replace(
                r#"textures = [{ name = "models/test/myprop" }]"#,
                "textures = [\n  { name = \"models/test/tex_a\" },\n  { name = \"models/test/tex_b\" },\n]",
            );
            let desc = ModelDesc::from_toml(&toml).unwrap();
            let c = compile(&desc, &d).unwrap();
            std::fs::remove_dir_all(&d).ok();
            c
        };
        let out = write_vtx(&c).unwrap();
        assert_eq!(out.mesh_stats.len(), 2);
        check_invariants(&out, &c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let l_abs = m_abs + g(m_abs + 4) as usize;
        assert_eq!(g(l_abs), 2, "numMeshes 应为 2");
    }

    /// 与真实 studiomdl 产物对照：结构字段必须逐项一致。
    ///
    /// ⚠️ **标了 `#[ignore]`**（需要官方 `.dx90.vtx`）。手动跑：
    /// `cargo test --release -- --ignored`
    #[test]
    #[ignore = "需要官方 myprop.dx90.vtx（MDLC_TEST_VTX）"]
    fn matches_studiomdl_reference_layout() {
        let p = crate::test_assets::require(
            "MDLC_TEST_VTX",
            r"E:\SteamLibrary\steamapps\common\Left 4 Dead 2\left4dead2\models\mymod\myprop.dx90.vtx",
            "官方 studiomdl 产物 myprop.dx90.vtx",
        );
        let refb = std::fs::read(&p).unwrap_or_else(|e| panic!("读不到 {}：{e}", p.display()));
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g =
            |buf: &[u8], o: usize| i32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
        // 头部：这些字段是常量，应与官方一致。
        for (name, off) in [
            ("version", 0x00),
            ("cacheSize", 0x04),
            ("maxBonesPerVert", 0x0C),
            ("numLODs", 0x14),
        ] {
            assert_eq!(
                g(b, off),
                g(&refb, off),
                "{name}（偏移 {off:#x}）与 studiomdl 不一致"
            );
        }
        assert_eq!(
            u16::from_le_bytes([b[0x08], b[0x09]]),
            u16::from_le_bytes([refb[0x08], refb[0x09]]),
            "maxBonesPerStrip"
        );
        assert_eq!(
            u16::from_le_bytes([b[0x0A], b[0x0B]]),
            u16::from_le_bytes([refb[0x0A], refb[0x0B]]),
            "maxBonesPerTri"
        );
        assert_eq!(g(b, 0x1C), g(&refb, 0x1C), "bodyPartCount");
        assert_eq!(g(b, 0x20), g(&refb, 0x20), "bodyPartOffset");
        // strip group 与 strip 的 flags。
        let sg = HEADER_SIZE + BODY_PART_SIZE + MODEL_SIZE + MODEL_LOD_SIZE + MESH_SIZE;
        assert_eq!(b[sg + 24], refb[sg + 24], "stripGroup.flags");
        let st = sg + STRIP_GROUP_SIZE;
        assert_eq!(b[st + 18], refb[st + 18], "strip.flags");
    }

    // -----------------------------------------------------------------
    // 多 LOD
    // -----------------------------------------------------------------

    /// 构造一个「2 个 LOD × 2 个 mesh」的最小多 LOD 模型。
    ///
    /// LOD 0 每个 mesh 有 4 个顶点（两个三角形），LOD 1 只有 3 个顶点
    /// （其中一个三角形）。材质名两边一致。
    fn multi_lod() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-vtx-lod-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        // 一个 mesh 的 4 顶点正方形（2 个三角形 = 6 个顶点行，SMD 是展开格式）。
        fn quad_smd(mat: &str, z: f32, coords: &[[f32; 2]]) -> String {
            let mut s = format!(
                "version 1\nnodes\n  0 \"root\" -1\nend\nskeleton\n  time 0\n    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\nend\ntriangles\n{mat}\n"
            );
            let pos = [
                [-8.0f32, -8.0, z],
                [8.0, -8.0, z],
                [8.0, 8.0, z],
                [-8.0, 8.0, z],
            ];
            // 三角形 (0,1,2) 与 (0,2,3) —— 每个三角形 3 个顶点行。
            // 12 个 token：`parentBone pos3 nrm3 uv2 links bone weight`。
            for tri in [[0usize, 1, 2], [0, 2, 3]] {
                for &i in &tri {
                    let p = pos[i];
                    s.push_str(&format!(
                        "  0 {:.6} {:.6} {:.6} 0.000000 0.000000 1.000000 {:.6} {:.6} 1 0 1.000000\n",
                        p[0], p[1], p[2], coords[i][0], coords[i][1]
                    ));
                }
            }
            s.push_str("end\n");
            s
        }
        // LOD 0：两个 mesh 各 4 顶点。
        let c = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        std::fs::write(d.join("lod0.smd"), quad_smd("tex_a", 0.0, &c)).unwrap();
        // LOD 1：同一网格（简化版用同样的 4 顶点，但材质集合一致）。
        std::fs::write(d.join("lod1.smd"), quad_smd("tex_a", 0.0, &c)).unwrap();

        let toml = r#"
[model]
name = "models/test/lod.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/tex_a" }]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "lod0.smd"

[[bodyparts.models.lods]]
smd = "lod1.smd"
switch_point = 30.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("多 LOD 模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 构造一个「LOD 1 只用部分顶点」的多 LOD 模型 —— 专门抓「槽位 ≠ 统一顶点号」。
    ///
    /// LOD 0 用满 4 个角（统一池 `0..4`）；LOD 1 只用其中 3 个角、**不含统一顶点 0**。
    /// 于是 LOD 1 的 `Item::verts`（统一顶点号）是 `[1, 2, 3]`，而 `Item::tris`
    /// 是槽位 `[0, 1, 2]` —— 两者**不相等**，`multi_lod()` 那个「两个 LOD 用同一份
    /// 顶点」的夹具里 `used == 0..n` 的恒等掩盖就消失了。
    fn multi_lod_missing_vertex() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-vtx-lodsub-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        // 四个角的 (pos, uv)。角 0 只在 LOD 0 出现。
        let pos = [
            [-8.0f32, -8.0, 0.0],
            [8.0, -8.0, 0.0],
            [8.0, 8.0, 0.0],
            [-8.0, 8.0, 0.0],
        ];
        let uv = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        // 12 个 token：`parentBone pos3 nrm3 uv2 links bone weight`。
        fn smd(mat: &str, tris: &[[usize; 3]], pos: &[[f32; 3]; 4], uv: &[[f32; 2]; 4]) -> String {
            let mut s = format!(
                "version 1\nnodes\n  0 \"root\" -1\nend\nskeleton\n  time 0\n    \
                 0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\nend\ntriangles\n{mat}\n"
            );
            for tri in tris {
                for &i in tri {
                    let p = pos[i];
                    let t = uv[i];
                    s.push_str(&format!(
                        "  0 {:.6} {:.6} {:.6} 0.000000 0.000000 1.000000 {:.6} {:.6} 1 0 1.000000\n",
                        p[0], p[1], p[2], t[0], t[1]
                    ));
                }
            }
            s.push_str("end\n");
            s
        }
        // LOD 0：两个三角形，用满 4 个角 ⟹ 统一池顺序 = 角 0,1,2,3。
        std::fs::write(
            d.join("lod0.smd"),
            smd("tex_a", &[[0, 1, 2], [0, 2, 3]], &pos, &uv),
        )
        .unwrap();
        // LOD 1：只剩一个三角形，**不含角 0** ⟹ 统一顶点号 = [1, 2, 3]。
        std::fs::write(d.join("lod1.smd"), smd("tex_a", &[[1, 2, 3]], &pos, &uv)).unwrap();

        let toml = r#"
[model]
name = "models/test/lodsub.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/tex_a" }]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "lod0.smd"

[[bodyparts.models.lods]]
smd = "lod1.smd"
switch_point = 30.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("多 LOD 子集模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    #[test]
    fn multi_lod_compiles_with_two_lods() {
        let c = multi_lod();
        let m = &c.bodyparts[0].models[0];
        let lods = m.lods.as_ref().expect("应有多 LOD 数据");
        assert_eq!(lods.num_lods, 2);
        assert_eq!(lods.switch_points.len(), 2);
        assert_eq!(lods.switch_points[0], 0.0, "LOD 0 的 switchPoint 恒为 0");
        assert_eq!(lods.switch_points[1], 30.0, "LOD 1 用显式值");
        assert_eq!(
            lods.meshes.len(),
            m.meshes.len(),
            "LOD 的 mesh 数应与 MDL 一致"
        );
    }

    /// 多 LOD 的 VTX 头部 `numLODs` 必须与 VVD 一致（实测 230/230）。
    #[test]
    fn multi_lod_vtx_num_lods_matches_vvd() {
        let c = multi_lod();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        assert_eq!(g(0x14), 2, "VTX numLODs 应为 2");

        let (vvd, layout) = crate::lod::build_multi_lod_vvd(&c, 123).unwrap();
        assert_eq!(vvd.header.num_lods, 2, "VVD numLODs 应为 2");
        assert_eq!(layout.num_lods, 2);
        assert_eq!(
            g(0x14),
            vvd.header.num_lods,
            "VTX 与 VVD 的 numLODs 必须一致"
        );
    }

    /// 多 LOD 的 VTX 结构树必须能被自检走通，且每 LOD 的 mesh 数正确。
    #[test]
    fn multi_lod_vtx_tree_is_walkable() {
        let c = multi_lod();
        let out = write_vtx(&c).unwrap();
        check_invariants(&out, &c).expect("多 LOD 的 VTX 必须自洽");
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        assert_eq!(g(m_abs), 2, "model 应有 2 个 LOD");
        let l0 = m_abs + g(m_abs + 4) as usize;
        let l1 = l0 + MODEL_LOD_SIZE;
        assert_eq!(g(l0), 1, "LOD 0 的 numMeshes");
        assert_eq!(g(l1), 1, "LOD 1 的 numMeshes");
        // switchPoint 必须写进对应 LOD。
        assert_eq!(
            b[l0 + 8..l0 + 12],
            0.0f32.to_le_bytes(),
            "LOD 0 switchPoint=0"
        );
        assert_eq!(
            b[l1 + 8..l1 + 12],
            30.0f32.to_le_bytes(),
            "LOD 1 switchPoint=30"
        );
        // 两个 LOD 的 mesh 数组必须不同（各有自己的 strip group）。
        let k0 = l0 + g(l0 + 4) as usize;
        let k1 = l1 + g(l1 + 4) as usize;
        assert_ne!(k0, k1, "两个 LOD 的 mesh 数组应分开");
    }

    /// 多 LOD 时 `origMeshVertID` 必须落在 `MDL.mesh.numvertices` 范围内。
    ///
    /// 实测判据：230/230 个真实多 LOD 模型满足
    /// `origMeshVertID < MDL.mesh.numvertices`（`probe_vtx_origmeshvertid.js`）。
    #[test]
    fn multi_lod_orig_mesh_vert_id_within_mesh_total() {
        let c = multi_lod();
        let out = write_vtx(&c).unwrap();
        check_invariants(&out, &c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);

        // 各 mesh 的跨 LOD 顶点总数（= layout 的 total_vertexes）。
        let mut all = Vec::new();
        for bp in &c.bodyparts {
            for m in &bp.models {
                if let Some(l) = &m.lods {
                    all.extend(l.meshes.iter().cloned());
                }
            }
        }
        let layout = crate::lod::build_lod_layout(&all);

        // 遍历 VTX 里每个 strip group 的每个顶点。
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        let n_lods = g(m_abs);
        let mut checked = 0usize;
        for l in 0..n_lods as usize {
            let lod_abs = m_abs + g(m_abs + 4) as usize + l * MODEL_LOD_SIZE;
            for k in 0..g(lod_abs) as usize {
                let k_abs = lod_abs + g(lod_abs + 4) as usize + k * MESH_SIZE;
                let sg_abs = k_abs + g(k_abs + 4) as usize;
                let nv = g(sg_abs) as usize;
                let v_abs = sg_abs + g(sg_abs + 4) as usize;
                let limit = layout.meshes[k].total_vertexes;
                for vi in 0..nv {
                    let o = v_abs + vi * VERTEX_SIZE;
                    let id = u16::from_le_bytes([b[o + 4], b[o + 5]]) as usize;
                    assert!(
                        id < limit,
                        "LOD {l} mesh {k} 的 origMeshVertID {id} 超出该 mesh 的顶点总数 {limit}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "应该检查到顶点");
    }

    /// 多 LOD 时索引必须指向本 strip group 的顶点槽位（不能越界）。
    #[test]
    fn multi_lod_indices_are_in_range() {
        let c = multi_lod();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp = g(0x20) as usize;
        let m_abs = bp + g(bp + 4) as usize;
        for l in 0..g(m_abs) as usize {
            let lod_abs = m_abs + g(m_abs + 4) as usize + l * MODEL_LOD_SIZE;
            for k in 0..g(lod_abs) as usize {
                let k_abs = lod_abs + g(lod_abs + 4) as usize + k * MESH_SIZE;
                let sg_abs = k_abs + g(k_abs + 4) as usize;
                let nv = g(sg_abs) as usize;
                let ni = g(sg_abs + 8) as usize;
                let i_abs = sg_abs + g(sg_abs + 12) as usize;
                for ii in 0..ni {
                    let v = u16::from_le_bytes([b[i_abs + ii * 2], b[i_abs + ii * 2 + 1]]) as usize;
                    assert!(v < nv, "LOD {l} mesh {k} 索引 {v} 越界（顶点数 {nv}）");
                }
            }
        }
    }

    /// ⭐ 多 LOD 的 `origMeshVertID` 必须由**统一顶点号**翻译而来。
    ///
    /// `plan_strips` 的 [`StripPlan::verts`] 存的是 `Item::verts` 的**槽位**，
    /// 不是统一顶点号（见 [`Item`] 的字段文档）；发射顶点时必须先
    /// `it.verts[slot]` 翻回去，再喂给 [`crate::lod::LodLayout::mesh_local_id`]。
    ///
    /// 夹具让两者**不相等**：LOD 1 只剩三角形 `[1,2,3]`（不含统一顶点 0），
    /// 于是槽位 `[0,1,2]` ≠ 统一顶点 `[1,2,3]`。`multi_lod()` 那种「两个 LOD
    /// 用同一份顶点」的夹具里 `used == 0..n`，恒等映射会把错位掩盖掉。
    ///
    /// 布局（由 [`crate::lod::build_lod_layout`] 的排序键决定）：LOD 1 独有
    /// 位为 0 的顶点排在前 ⟹ 角 1,2,3 拿 `finalMeshVertID` 0,1,2，角 0 拿 3。
    /// 因此正确的 LOD 1 `origMeshVertID` 是 `[0,1,2]`；把槽位当统一顶点号
    /// 会得到 `[3,0,1]` —— 角 0 根本不在 LOD 1 里。
    #[test]
    fn multi_lod_translates_slots_back_to_unified_vertices() {
        let c = multi_lod_missing_vertex();
        let out = write_vtx(&c).unwrap();
        check_invariants(&out, &c).expect("多 LOD 的 VTX 必须自洽");
        let groups = strip_groups(&out);
        assert_eq!(groups.len(), 2, "两个 LOD 各一个 strip group");

        // LOD 0 用满 4 个角：槽位 == 统一顶点号，两种写法同值。
        let (gv0, _, s0) = &groups[0];
        assert_eq!(*gv0, 4, "LOD 0 用到 4 个统一顶点");
        assert_eq!(s0.len(), 1);
        assert_eq!(
            s0[0].orig_ids,
            vec![3, 0, 1, 2],
            "LOD 0 的 origMeshVertID（角 0 因只属于 LOD 0 而排在最后）"
        );

        // ⭐ LOD 1：槽位 ≠ 统一顶点号，这里才能分辨对错。
        let (gv1, _, s1) = &groups[1];
        assert_eq!(*gv1, 3, "LOD 1 只用到 3 个统一顶点（不含角 0）");
        assert_eq!(s1.len(), 1, "3 根骨骼远小于 53，不该拆 strip");
        let s = &s1[0];
        assert_eq!(s.num_verts, 3);
        assert_eq!(
            s.orig_ids,
            vec![0, 1, 2],
            "LOD 1 的 origMeshVertID 必须由统一顶点 [1,2,3] 翻译而来；\
             写成槽位 [0,1,2] 会得到 [3,0,1]（multi-LOD 回归）"
        );

        // 交叉校验：每个 orig id 都必须是该 mesh 合法编号，且互不相同。
        let mut all = Vec::new();
        for bp in &c.bodyparts {
            for m in &bp.models {
                if let Some(l) = &m.lods {
                    all.extend(l.meshes.iter().cloned());
                }
            }
        }
        let layout = crate::lod::build_lod_layout(&all);
        let total = layout.meshes[0].total_vertexes;
        assert_eq!(total, 4, "跨 LOD 合并后该 mesh 共 4 个顶点");
        let mut seen: Vec<u16> = s.orig_ids.clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2], "LOD 1 的 3 个顶点应各占一个编号");
        assert!(
            s.orig_ids.iter().all(|&id| (id as usize) < total),
            "origMeshVertID 必须落在该 mesh 的顶点总数内"
        );
    }

    /// materialReplacementList 必须**每 LOD 一个**（单 LOD 时 1 个）。
    #[test]
    fn multi_lod_has_one_material_replacement_list_per_lod() {
        let c = multi_lod();
        let out = write_vtx(&c).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let mr = g(0x18) as usize;
        assert!(
            mr + 2 * MATERIAL_REPLACEMENT_LIST_SIZE <= b.len(),
            "两个空表应都在文件内"
        );
        for n in 0..2 {
            let o = mr + n * MATERIAL_REPLACEMENT_LIST_SIZE;
            assert_eq!(g(o), 0, "第 {n} 个表的 numReplacements");
        }
    }

    /// 单 LOD 路径必须**一行未变**：加多 LOD 支持后产物逐字节不变。
    #[test]
    fn single_lod_path_is_unchanged() {
        let c = minimal();
        let out = write_vtx(&c).unwrap();
        // 单 LOD：头部 numLODs = 1，且只有一个 materialReplacementList。
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        assert_eq!(g(0x14), 1);
        let mr = g(0x18) as usize;
        assert_eq!(
            b.len(),
            mr + MATERIAL_REPLACEMENT_LIST_SIZE,
            "单 LOD 的文件应在唯一的 materialReplacementList 之后结束"
        );
    }

    // ==== 拆 strip（`plan_strips`）====================================
    //
    // 官方约定见 [`StripPlan`]：同一 strip group 的顶点/索引数组被各 strip
    // 按连续子区间瓜分，每条 strip 有自己的骨骼调色板且
    // `numBoneStateChanges <= maxBonesPerStrip`（53）。
    //
    // 语料 3302/3302 个官方 `.dx90.vtx` 零违规
    // （`vtx_multistrip.js --validate`），本工程曾因单 strip 写出 `nBSC = 55`
    // 而进游戏崩溃 —— 下面这些测试守着这条不变量。

    /// 造一个**骨骼数可控**的模型：`n` 根骨骼、`n` 个顶点，
    /// 每个顶点独占一根骨骼，每个三角形吃掉 3 根**新**骨骼。
    ///
    /// 于是整组调色板 = `n` 根骨骼：`n > 53` 时必然要拆，`n <= 53` 时不该拆。
    fn many_bones(n: usize) -> CompiledModelDesc {
        assert!(n >= 6 && n.is_multiple_of(3), "骨骼数须是 ≥ 6 的 3 的倍数");
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-vtxwide-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();

        // SMD：链式骨架 + 每顶点独占一根骨骼。
        let mut smd = String::from("version 1\nnodes\n  0 \"root\" -1\n");
        for i in 1..n {
            smd.push_str(&format!("  {i} \"b{i}\" {}\n", i - 1));
        }
        smd.push_str("end\nskeleton\n  time 0\n");
        for i in 0..n {
            // 每根骨骼沿 +X 排开，位移互不相同。
            smd.push_str(&format!("    {i} {} 0 0 0 0 0\n", i as f32 * 2.0));
        }
        smd.push_str("end\ntriangles\nmyprop\n");
        for t in 0..n / 3 {
            for k in 0..3 {
                let v = t * 3 + k;
                // 12 token：`bone x y z nx ny nz u v links bone1 weight1`
                smd.push_str(&format!("  {v} {} 0 0 0 0 1 0 0 1 {v} 1.0\n", v as f32));
            }
        }
        smd.push_str("end\n");

        std::fs::write(d.join("myprop-ref.smd"), &smd).unwrap();
        let mut bones = String::from("[[bones]]\nname = \"root\"\n");
        for i in 1..n {
            let parent = if i == 1 {
                "root".to_string()
            } else {
                format!("b{}", i - 1)
            };
            bones.push_str(&format!(
                "\n[[bones]]\nname = \"b{i}\"\nparent = \"{parent}\"\n"
            ));
        }
        let toml = format!(
            r#"
[model]
name = "models/test/wide.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{{ name = "models/test/myprop" }}]

{bones}
[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"
"#
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("多骨骼宽模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 一个 strip 的关键字段（拆 strip 的验证用）。
    #[derive(Debug)]
    struct StripView {
        vert_off: usize,
        num_verts: usize,
        index_off: usize,
        num_indices: usize,
        n_bsc: usize,
        max_hw: u8,
        /// 该 strip 的索引值（**组内绝对**顶点下标）。
        indices: Vec<u16>,
        /// 该 strip 各顶点的 `origMeshVertID`（顺序 = 顶点数组顺序）。
        orig_ids: Vec<u16>,
    }

    /// 读出 VTX 里每个 strip group 的 `(组顶点数, 组索引数, strips)`。
    fn strip_groups(out: &VtxWriteOutcome) -> Vec<(usize, usize, Vec<StripView>)> {
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let mut groups = Vec::new();
        let bp = g(0x20) as usize;
        for i in 0..g(0x1c) as usize {
            let bp_abs = bp + i * BODY_PART_SIZE;
            let mo = bp_abs + g(bp_abs + 4) as usize;
            for j in 0..g(bp_abs) as usize {
                let mo_abs = mo + j * MODEL_SIZE;
                let lo = mo_abs + g(mo_abs + 4) as usize;
                for k in 0..g(mo_abs) as usize {
                    let lo_abs = lo + k * MODEL_LOD_SIZE;
                    let mh = lo_abs + g(lo_abs + 4) as usize;
                    for m in 0..g(lo_abs) as usize {
                        let mh_abs = mh + m * MESH_SIZE;
                        let sg = mh_abs + g(mh_abs + 4) as usize;
                        for s in 0..g(mh_abs) as usize {
                            let sg_abs = sg + s * STRIP_GROUP_SIZE;
                            let gv = g(sg_abs) as usize;
                            let gi = g(sg_abs + 8) as usize;
                            let vbase = sg_abs + g(sg_abs + 4) as usize;
                            let ibase = sg_abs + g(sg_abs + 12) as usize;
                            let st = sg_abs + g(sg_abs + 20) as usize;
                            let mut strips = Vec::new();
                            for t in 0..g(sg_abs + 16) as usize {
                                let st_abs = st + t * STRIP_SIZE;
                                let ni = g(st_abs) as usize;
                                let i_off = g(st_abs + 4) as usize;
                                let nv = g(st_abs + 8) as usize;
                                let v_off = g(st_abs + 12) as usize;
                                let indices: Vec<u16> = (0..ni)
                                    .map(|q| {
                                        let o = ibase + (i_off + q) * 2;
                                        u16::from_le_bytes([b[o], b[o + 1]])
                                    })
                                    .collect();
                                let max_hw = (0..nv)
                                    .flat_map(|v| {
                                        (0..3).map(move |k| v_off + v * VERTEX_SIZE + 6 + k)
                                    })
                                    .map(|o| b[vbase + o])
                                    .max()
                                    .unwrap_or(0);
                                let orig_ids: Vec<u16> = (0..nv)
                                    .map(|v| {
                                        let o = vbase + (v_off + v) * VERTEX_SIZE + 4;
                                        u16::from_le_bytes([b[o], b[o + 1]])
                                    })
                                    .collect();
                                strips.push(StripView {
                                    vert_off: v_off,
                                    num_verts: nv,
                                    index_off: i_off,
                                    num_indices: ni,
                                    n_bsc: g(st_abs + 19) as usize,
                                    max_hw,
                                    indices,
                                    orig_ids,
                                });
                            }
                            groups.push((gv, gi, strips));
                        }
                    }
                }
            }
        }
        groups
    }

    /// 调色板 ≤ 53 时**不拆**：一条 strip、顶点/索引从 0 起、原序不变。
    ///
    /// 这是「不引入回归」的判据 —— 拆 strip 之前所有产物都是这个形状。
    #[test]
    fn palette_within_max_bones_stays_a_single_strip() {
        let c = many_bones(30);
        let out = write_vtx(&c).unwrap();
        let groups = strip_groups(&out);
        assert_eq!(groups.len(), 1, "夹具只有一个 mesh");
        let (gv, gi, strips) = &groups[0];
        assert_eq!(strips.len(), 1, "30 根骨骼 < 53，不该拆 strip");
        let s = &strips[0];
        assert_eq!(s.vert_off, 0, "单 strip 的顶点应从 0 起");
        assert_eq!(s.index_off, 0, "单 strip 的索引应从 0 起");
        assert_eq!(s.n_bsc, 30, "调色板 = 该 strip 用到的唯一骨骼数");
        assert_eq!(s.num_verts, *gv, "单 strip 应覆盖整个组顶点数组");
        assert_eq!(s.num_indices, *gi, "单 strip 应覆盖整个组索引数组");
    }

    /// `plan_strips` 在小调色板上必须**恒等**：一条 strip、顶点原序、索引原样。
    #[test]
    fn plan_strips_keeps_small_palettes_intact() {
        let v: Vec<crate::model::Vertex> = (0..4)
            .map(|i| crate::model::Vertex {
                pos: [i as f32, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                bones: vec![[i as f32, 1.0]],
            })
            .collect();
        let refs: Vec<&crate::model::Vertex> = v.iter().collect();
        let tris = vec![[0u32, 1, 2], [1, 2, 3]];
        let plans = plan_strips(&refs, &tris, MAX_BONES_PER_STRIP);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].verts, vec![0, 1, 2, 3], "顶点必须保持原序");
        assert_eq!(plans[0].tris, tris, "索引必须原样（含绕序）");
        assert_eq!(plans[0].v_off, 0);
        assert_eq!(plans[0].i_off, 0);
    }

    /// ⭐ 核心不变量：调色板超 53 根骨骼时**必须拆**，且每条都不越界。
    ///
    /// 语料 3302/3302 个官方 `.dx90.vtx` 全部满足；本工程曾因单 strip
    /// 写出 `nBSC = 55`（`maxHw = 54`）而进游戏几秒后崩溃。
    #[test]
    fn oversized_palette_is_split_into_strips_within_max_bones() {
        let c = many_bones(60);
        let out = write_vtx(&c).unwrap();
        let groups = strip_groups(&out);
        assert_eq!(groups.len(), 1, "夹具只有一个 mesh");
        let (gv, gi, strips) = &groups[0];
        assert!(
            strips.len() > 1,
            "60 根骨骼应拆成多条 strip，实际只有 {} 条",
            strips.len()
        );

        let mut v_cur = 0usize;
        let mut i_cur = 0usize;
        for (n, s) in strips.iter().enumerate() {
            assert!(
                s.n_bsc <= MAX_BONES_PER_STRIP,
                "strip[{n}] 的 nBSC {} 超过头部声明的 {MAX_BONES_PER_STRIP}",
                s.n_bsc
            );
            assert!(
                (s.max_hw as usize) < MAX_BONES_PER_STRIP,
                "strip[{n}] 的硬件槽位 {} 超过 {}",
                s.max_hw,
                MAX_BONES_PER_STRIP - 1
            );
            assert_eq!(s.vert_off, v_cur, "strip[{n}] 的顶点子区间必须首尾相接");
            assert_eq!(s.index_off, i_cur, "strip[{n}] 的索引子区间必须首尾相接");
            v_cur += s.num_verts;
            i_cur += s.num_indices;
        }
        assert_eq!(v_cur, *gv, "各 strip 顶点数之和应等于组声明的顶点数");
        assert_eq!(i_cur, *gi, "各 strip 索引数之和应等于组声明的索引数");
    }

    /// 索引值是**组内绝对**顶点下标，且必须落在**本 strip** 的顶点子区间内。
    #[test]
    fn strip_indices_stay_inside_their_own_subrange() {
        let c = many_bones(60);
        let out = write_vtx(&c).unwrap();
        let (_, _, strips) = strip_groups(&out).pop().unwrap();
        for (n, s) in strips.iter().enumerate() {
            for &i in &s.indices {
                let i = i as usize;
                assert!(
                    i >= s.vert_off && i < s.vert_off + s.num_verts,
                    "strip[{n}] 的索引 {i} 不在自己的顶点子区间 [{}, {}) 内",
                    s.vert_off,
                    s.vert_off + s.num_verts
                );
            }
        }
    }

    /// 每个三角形的三个顶点必须落在**同一条** strip 里 —— 否则该三角形
    /// 跨了 draw call，渲染时半边缺失。
    #[test]
    fn every_triangle_lives_in_one_strip() {
        let c = many_bones(60);
        let out = write_vtx(&c).unwrap();
        let (_, _, strips) = strip_groups(&out).pop().unwrap();
        let owner = |i: u16| {
            strips
                .iter()
                .position(|x| (i as usize) >= x.vert_off && (i as usize) < x.vert_off + x.num_verts)
                .expect("索引应落在某条 strip 的顶点子区间内")
        };
        for (n, s) in strips.iter().enumerate() {
            assert_eq!(s.num_indices % 3, 0, "TRILIST 的索引数应是 3 的倍数");
            for t in s.indices.as_chunks::<3>().0 {
                assert!(
                    t.iter().all(|&i| owner(i) == n),
                    "strip[{n}] 里的三角形 {t:?} 跨越了多条 strip"
                );
            }
        }
    }
}
