//! VTA 形状（flex）解析 —— `.vta` 帧 → `mstudioflex_t` + `mstudiovertanim_t` 载荷。
//!
//! ```text
//! .vta（逐帧顶点位置）  ──┐
//!                        ├─► 就近匹配 ─► 差量 ─► smoothstep ─► speed ─► 载荷
//! 参考 SMD（模型顶点）  ──┘
//! ```
//! ```c
//! // frame 0 is special.  Always assume zero vertex animations
//! if (g_flexkey[i].frame == 0) numsrcanims = 0;
//! ```

use crate::model::{Flex, Vertex};
use crate::vta::Vta;
use rustc_hash::FxHashMap;

/// 就近匹配的距离阈值（平方距离）—— `simplify.cpp:2298` 的 `dist < 0.15`。
///
/// ⚠️ 是**平方**距离。源码里 `dist = tmp.LengthSqr()`，然后比 `0.15`。
pub const MATCH_DIST_SQR: f32 = 0.15;

/// 差量过小的门槛（`simplify.cpp:2539`）：
/// `DotProduct(delta,delta) > 0.001² || DotProduct(ndelta,ndelta) > 0.001`。
///
/// 即**位置**用 `1e-3` 的平方、**法线**直接用 `1e-3` —— 两者口径不同，
/// 这是源码里就这样写的（注释说「currently this is set to the float16
/// min value. Sucky.」）。
pub const MIN_DELTA_SQR: f32 = 0.001 * 0.001;
pub const MIN_NDELTA_SQR: f32 = 0.001;

/// 一条已解析的 vertanim（`mstudiovertanim_t` 的语义值，尚未编码）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedVertAnim {
    pub wrinkle: f32,
    /// **该 mesh 内**的顶点下标（不是 model 全局下标）。
    pub index: u32,
    /// 0..=255。`255 * speed`（`write.cpp:1790`）。
    pub speed: u8,
    /// 0..=255。`255 * side`（`write.cpp:1791`）。
    pub side: u8,
    /// 位置差量（半精度落盘）。
    pub delta: [f32; 3],
    /// 法线差量（半精度落盘）。
    pub ndelta: [f32; 3],
}

/// 一条已解析的 flex（`mstudioflex_t` + 它的 vertanim 数组）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedFlex {
    /// `mstudioflex_t.flexdesc` —— flexdesc 下标。
    pub flexdesc: i32,
    /// `mstudioflex_t.target0..3`。
    pub targets: [f32; 4],
    /// `mstudioflex_t.flexpair` —— 配对的另一个 desc 下标（0 = 未配对）。
    pub flexpair: i32,
    /// `mstudioflex_t.vertanimtype`（0 = NORMAL 16 字节，1 = WRINKLE 18 字节）。
    pub vertanimtype: u8,
    pub vertanims: Vec<ResolvedVertAnim>,
}

/// 一个 mesh 的 flex 载荷（写进该 mesh 的数据区）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MeshFlexes {
    pub flexes: Vec<ResolvedFlex>,
}

/// 解析一条 flex 规格时可能出现的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct FlexError {
    pub message: String,
}

fn ferr(message: impl Into<String>) -> FlexError {
    FlexError {
        message: message.into(),
    }
}

/// 一条 flex 在**模型顶点池**里的匹配结果。
///
/// `mapping[v]` = VTA 顶点 `v` 对应的全部模型顶点（池内下标）。
/// 空 `Vec` 表示该 VTA 顶点在模型里找不到匹配。
pub type VanimMap = Vec<Vec<usize>>;

/// 把 `.vta` 的第 0 帧顶点就近匹配到模型顶点池。
///
/// # 与 studiomdl 的差异（有意为之，且更严格）
///
/// studiomdl 对每个**模型顶点**记「最近的那个 VTA 顶点」
/// （`model_to_vanim_vert_imap`），再反转成 `vanim_map`。
/// 由于「最近」可能有并列，反转时用的是「先到先得 + 同距时比法线点积」。
/// 本实现直接对每个 VTA 顶点找全部 `dist < 0.15` 的模型顶点，
/// 语义等价（同一组 `(模型顶点, VTA 顶点)` 对），但**不依赖遍历顺序**。
///
/// `model_verts` 是该 model 的顶点池（顺序 = 写进 VVD 的顺序）。
///
/// # 为什么用均匀网格
///
/// 朴素写法是 `V_vta × V_model` 的双重循环。真实模型上这两个数都是
/// **十万量级**（`main.smd` 是 180180 × 540540 ≈ 9.7e10），单次就要
/// 一分半，而调用方还会**每条 flex 调一次**。
///
/// 这里按 `cell = sqrt(MATCH_DIST_SQR)`（即匹配半径本身）分格：
/// 任一点 `q` 满足 `|p − q| < cell` ⟹ 三个轴的格子下标至多差 **1**，
/// 所以只扫 `p` 所在格的 `3×3×3` 邻域就是**精确**结果，不是近似剪枝。
///
/// 结果与朴素实现**逐位相同**：外层仍按 `frame0` 原序遍历 VTA 顶点，
/// 而每个模型顶点 `k` 的 `best_of_model[k]` 槽位互相独立，
/// 所以「同距同点积时先到先得」的并列裁决与遍历顺序无关。
pub fn build_vanim_map(vta: &Vta, model_verts: &[Vertex]) -> VanimMap {
    let mut map: VanimMap = vec![Vec::new(); vta.num_vertices];
    let frame0 = vta.frame(0).unwrap_or(&[]);
    if frame0.is_empty() || model_verts.is_empty() {
        return map;
    }

    // 格子边长 = 匹配半径。用 `sqrt` 而不是另选常数，保证「距离 < cell
    // ⟹ 格号差 ≤ 1」这条推导严格成立。
    let cell = MATCH_DIST_SQR.sqrt();
    let key = |p: &[f32; 3]| -> (i32, i32, i32) {
        (
            (p[0] / cell).floor() as i32,
            (p[1] / cell).floor() as i32,
            (p[2] / cell).floor() as i32,
        )
    };

    // ---- 建网格：格子 → 该格内模型顶点下标（保持升序）----
    //
    // 键是整数三元组、一次编译查 486 万次，用 SipHash 是纯开销；
    // 换 `FxHashMap` 后同一 span 从 130.60 ms 降到 75.85 ms（1.72×）。
    // 网格**从不被迭代**，格内顺序由 `entry().or_default().push()` 的
    // 插入顺序决定，所以哈希值不可能泄漏到产物里。
    let mut grid: FxHashMap<(i32, i32, i32), Vec<usize>> = FxHashMap::default();
    for (k, mv) in model_verts.iter().enumerate() {
        // 非有限坐标无法定位格子，也永远匹配不上 —— 直接不入网格。
        if !mv.pos.iter().all(|c| c.is_finite()) {
            continue;
        }
        grid.entry(key(&mv.pos)).or_default().push(k);
    }

    // 每个模型顶点只归给**最近**的 VTA 顶点（同距时取法线点积大的），
    // 这样与 studiomdl 的 `model_to_vanim_vert_imap` 语义一致。
    let mut best_of_model: Vec<Option<(f32, f32, usize)>> = vec![None; model_verts.len()];
    for v in frame0 {
        let vi = v.index as usize;
        if vi >= map.len() || !v.pos.iter().all(|c| c.is_finite()) {
            continue;
        }
        let (cx, cy, cz) = key(&v.pos);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(bucket) = grid.get(&(cx + dx, cy + dy, cz + dz)) else {
                        continue;
                    };
                    for &k in bucket {
                        let mv = &model_verts[k];
                        let d = [
                            mv.pos[0] - v.pos[0],
                            mv.pos[1] - v.pos[1],
                            mv.pos[2] - v.pos[2],
                        ];
                        let dist = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                        // ⚠️ NaN 必须**显式跳过**。官方 `simplify.cpp:2298` 是
                        // `if (dist < 0.15)`：NaN 让条件为假 ⟹ 跳过。
                        // 而写成 `dist >= T` 时 NaN 同样为假 ⟹ 反而**接受**，
                        // 那是旧实现的一个潜在 bug（NaN 顶点会被当成匹配上）。
                        // 这里用「is_nan() || >=」而不是 `!(dist < T)`，
                        // 只为满足 `clippy::neg_cmp_op_on_partial_ord`；两者对
                        // IEEE 浮点完全等价，但前者把意图写在了明面上。
                        if dist.is_nan() || dist >= MATCH_DIST_SQR {
                            continue;
                        }
                        let dot = mv.normal[0] * v.normal[0]
                            + mv.normal[1] * v.normal[1]
                            + mv.normal[2] * v.normal[2];
                        let better = match best_of_model[k] {
                            None => true,
                            Some((bd, bdot, _)) => dist < bd || (dist == bd && dot > bdot),
                        };
                        if better {
                            best_of_model[k] = Some((dist, dot, vi));
                        }
                    }
                }
            }
        }
    }

    for (k, slot) in best_of_model.iter().enumerate() {
        if let Some((_, _, vi)) = *slot {
            map[vi].push(k);
        }
    }
    map
}

/// smoothstep 缩放（`simplify.cpp:2477-2510`）。
///
/// `split == 0` ⟹ 恒 `1.0`（不分割）。
/// `split > 0`：`x < -split` ⟹ 1；`x > split` ⟹ 0；其间 `3t²-2t³`，
/// 其中 `t = (split - x) / (2*split)`。
/// `split < 0`：**镜像**（判断方向反过来，`t` 的式子不变）。
fn split_scale(x: f32, split: f32) -> f32 {
    if split > 0.0 {
        if x > split {
            0.0
        } else if x < -split {
            1.0
        } else {
            let t = (split - x) / (2.0 * split);
            3.0 * t * t - 2.0 * t * t * t
        }
    } else if split < 0.0 {
        if x < split {
            0.0
        } else if x > -split {
            1.0
        } else {
            let t = (split - x) / (2.0 * split);
            3.0 * t * t - 2.0 * t * t * t
        }
    } else {
        1.0
    }
}

/// 解析一条 flex 规格成载荷。
///
/// # 参数
///
/// - `flex`：TOML 里的规格；
/// - `vta`：已解析的 `.vta`；
/// - `model_verts`：该 model 的**顶点池**（顺序 = VVD 顺序）；
/// - `mesh_of_vertex`：顶点池下标 → `(mesh 序号, 该 mesh 内的局部下标)`；
/// - `flexdesc`：`pair` 已展开后的 flexdesc 下标 `(主, 配对)`。
///   `pair == false` 时第二项应为 `0`；
/// - `spec_index`：用于错误定位。
///
/// # 返回
///
/// 每个 mesh 一份 [`MeshFlexes`]，顺序与 mesh 顺序一致（空的就是没有形状）。
#[allow(clippy::too_many_arguments)]
pub fn resolve_flex(
    flex: &Flex,
    vta: &Vta,
    model_verts: &[Vertex],
    mesh_of_vertex: &[(usize, u32)],
    flexdesc: (i32, i32),
    spec_index: usize,
) -> Result<Vec<MeshFlexes>, FlexError> {
    let map = build_vanim_map(vta, model_verts);
    resolve_flex_mapped(flex, vta, &map, mesh_of_vertex, flexdesc, spec_index)
}

/// 与 [`resolve_flex`] 相同，但复用**已算好的** `vanim_map`。
///
/// `vanim_map` 只取决于 `(vta 第 0 帧, model 顶点池)`，**与 flex 无关** ——
/// 所以同一 `(model, vta)` 下的所有 flex 应当共用一份。真实模型上
/// `build_vanim_map` 是十万×十万量级，逐条 flex 重算是纯浪费
/// （`main.vta` 42 条 flex 共用一个文件，重算 42 次曾让编译从秒级涨到分钟级）。
///
/// > 调用方若要在同一 `.vta` 上跑多条 flex，应当用
/// > [`resolve_flex_indexed`] 并把 [`BaseIndex`] 也缓存起来 ——
/// > 本函数每次都会重建一份基准表。
pub fn resolve_flex_mapped(
    flex: &Flex,
    vta: &Vta,
    map: &VanimMap,
    mesh_of_vertex: &[(usize, u32)],
    flexdesc: (i32, i32),
    spec_index: usize,
) -> Result<Vec<MeshFlexes>, FlexError> {
    let base_index = BaseIndex::build(vta);
    resolve_flex_indexed(
        flex,
        vta,
        &base_index,
        map,
        mesh_of_vertex,
        flexdesc,
        spec_index,
    )
}

/// 第 0 帧的「VTA 顶点下标 → 帧内位置」索引，即**差量基准表**。
///
/// # 为什么单独抽成一个可复用的值
///
/// [`resolve_flex_indexed`] 原先每次调用都现场建一份
/// `HashMap<u32, &VtaVert>`。这份表**只取决于 `vta.frame(0)`**，
/// 与 flex 完全无关，于是同一 `.vta` 下的每条 flex 都在重建同一份内容。
///
/// 实测（`parity/linnea-export.toml`，第 0 帧 180180 个顶点、42 条 flex
/// 共用一份 `main.vta`）：**757 万次带 SipHash 的插入**，
/// 占一次完整编译的 **约 25%**（117.8 ms / 460 ms）。
///
/// 换成按 `index` 直接寻址的扁平数组后，建表退化成一次线性写
/// （无哈希、无 rehash），查询退化成一次数组读。
pub struct BaseIndex {
    /// `slots[i]` = 第 0 帧里 `index == i` 的**第一条**在帧内的位置；
    /// [`NO_BASE`] 表示第 0 帧没有这个下标。
    slots: Vec<usize>,
}

/// 「第 0 帧没有这个下标」的哨兵。
///
/// 用 `usize::MAX` 而不是 `Option<usize>`，把每槽从 16 B 压到 8 B ——
/// 18 万顶点是 1.4 MB，比 2.9 MB 更容易留在 cache 里。
const NO_BASE: usize = usize::MAX;

impl BaseIndex {
    /// 按 `vta` 的**第 0 帧**建表。
    pub fn build(vta: &Vta) -> Self {
        let base = vta.frame(0).unwrap_or(&[]);
        let mut slots = vec![NO_BASE; vta.num_vertices];
        for (i, b) in base.iter().enumerate() {
            let k = b.index as usize;
            // ⚠️ 必须显式「**先到先得**」：重复下标保留**第一条**。
            //
            // 这与原先 `HashMap::entry().or_insert()` 的语义一致，
            // 也对应更早那版 `base.iter().find(|b| b.index == vi)`
            // （`find` 返回第一条）。**不能**改成「后者覆盖」——
            // 那会让差量基准取错条目，且**不会报任何错**，
            // 只表现为形状偏移。
            if k < slots.len() && slots[k] == NO_BASE {
                slots[k] = i;
            }
        }
        Self { slots }
    }

    /// 第 0 帧里 `index == vi` 的那一条在帧内的位置。
    #[inline]
    fn pos_of(&self, vi: u32) -> Option<usize> {
        match self.slots.get(vi as usize) {
            Some(&p) if p != NO_BASE => Some(p),
            _ => None,
        }
    }
}

/// 与 [`resolve_flex_mapped`] 相同，但复用**已算好的**差量基准表。
///
/// `base_index` 只取决于 `vta` 的第 0 帧（见 [`BaseIndex`]），
/// 所以同一 `.vta` 下的所有 flex 应当共用一份。
#[allow(clippy::too_many_arguments)]
pub fn resolve_flex_indexed(
    flex: &Flex,
    vta: &Vta,
    base_index: &BaseIndex,
    map: &VanimMap,
    mesh_of_vertex: &[(usize, u32)],
    flexdesc: (i32, i32),
    spec_index: usize,
) -> Result<Vec<MeshFlexes>, FlexError> {
    let at = format!("bodyparts[..].models[..].flexes[{spec_index}]");

    let n_meshes = mesh_of_vertex.iter().map(|(m, _)| m + 1).max().unwrap_or(0);
    let mut out = vec![MeshFlexes::default(); n_meshes];

    // ⛔ `frame 0` 恒空 —— 与其静默产出 0 载荷，不如显式报错。
    // 这正是 L4D2 那个坑的症状来源（见 HANDBOOK 38.4）。
    //
    // ⚠️ 例外：官方 `Option_Eyelid` 的 `neutral 0` 会**故意**注册一条
    // frame 0 的 flexkey，语义就是「载荷清零」（`simplify.cpp:2453-2457`），
    // 所以它合法且**必然**产出空载荷 —— 那条路径由 `from_eyelid` 标记。
    if flex.frame == 0 {
        if flex.from_eyelid {
            return Ok(out);
        }
        return Err(ferr(format!(
            "{at}：frame = 0 是**特殊帧**，studiomdl 会强制把载荷清零\
             （`simplify.cpp:2453-2457`），产物里 `numflexes` 恒为 0。\
             请改用 frame = 1（或更大）。"
        )));
    }
    if flex.frame < 0 {
        return Err(ferr(format!("{at}：frame 不能为负（{}）", flex.frame)));
    }

    // ---- 相对帧号：TOML 的 frame 直接就是相对帧号 ----
    let rel = flex.frame;
    let n_frames = i32::try_from(vta.num_frames()).unwrap_or(i32::MAX);
    if rel >= n_frames {
        return Err(ferr(format!(
            "{at}：frame = {rel} 超出 .vta 的帧范围（共 {n_frames} 帧，\
             即相对帧 0..={}；`.vta` 的 skeleton 是 time {}..{}）",
            n_frames - 1,
            vta.start_frame,
            vta.end_frame
        )));
    }
    let src = vta.frame(rel).unwrap_or(&[]);
    let base = vta.frame(0).unwrap_or(&[]);

    // 第 0 帧必须以位置为索引（它是差量基准）。
    // `.vta` 允许某帧缺少某个顶点 —— 缺的按「与第 0 帧相同」处理。
    //
    // ⚠️ 原先这里是 `base.iter().find(|b| b.index == vi)`，即**每个源顶点
    // 线性扫一遍第 0 帧**（180180 × 180180 ≈ 3.2e10），单条 flex 就要
    // 几十秒。改成先建一次「下标 → 条目」表，整体降到 O(n)。
    //
    // ⚠️ 必须显式「**先到先得**」：`find` 返回**第一条**匹配，
    // 而 `HashMap` 的 `collect` 在键重复时保留**最后**一条 —— 两者语义不同。
    // 虽然规范 `.vta` 的 `index` 在帧内唯一，但这里不能依赖它：
    // 重复下标会让差量基准取错条目，且**不会报任何错**，只表现为形状偏移。
    // （该裁决在 [`BaseIndex::build`] 里，仍是先到先得。）
    let base_of =
        |vi: u32| -> Option<&crate::vta::VtaVert> { base_index.pos_of(vi).map(|p| &base[p]) };

    // ---- 逐顶点算差量 ----
    // 先收集 (mesh, 局部下标, delta, ndelta, side)，再统一算 speed。
    struct Pending {
        mesh: usize,
        local: u32,
        delta: [f32; 3],
        ndelta: [f32; 3],
        side: f32,
    }
    let mut pending: Vec<Pending> = Vec::new();

    for sv in src {
        let vi = sv.index as usize;
        let Some(targets) = map.get(vi) else {
            continue;
        };
        if targets.is_empty() {
            continue;
        }
        let Some(b) = base_of(sv.index) else {
            // 第 0 帧没有这个顶点 ⟹ 无基准 ⟹ studiomdl 也匹配不上。
            continue;
        };

        let raw_scale = split_scale(sv.pos[0], flex.split);

        // `simplify.cpp:2512-2522`：
        //   配对  ⟹ 位移**全量**（scale = 1），左右靠 side 通道分配；
        //   非配对 ⟹ 单侧（side = 0），位移按 smoothstep **缩放**。
        let (scale, side) = if flex.pair {
            (1.0, 1.0 - raw_scale)
        } else {
            (raw_scale, 0.0)
        };

        let delta = [
            (sv.pos[0] - b.pos[0]) * scale,
            (sv.pos[1] - b.pos[1]) * scale,
            (sv.pos[2] - b.pos[2]) * scale,
        ];
        let ndelta = [
            (sv.normal[0] - b.normal[0]) * scale,
            (sv.normal[1] - b.normal[1]) * scale,
            (sv.normal[2] - b.normal[2]) * scale,
        ];

        // `scale == 0` 的顶点被**丢弃**（`simplify.cpp:2524` 的 `if (scale > 0 …)`）。
        //
        // 注意 `pair = true` 时 `scale` 恒为 1，所以配对不会丢顶点 ——
        // 与受控实验 p02（6 个顶点全保留）一致。
        if scale <= 0.0 {
            continue;
        }
        // 差量太小的也丢弃（`simplify.cpp:2539`）。
        let d2 = delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2];
        let n2 = ndelta[0] * ndelta[0] + ndelta[1] * ndelta[1] + ndelta[2] * ndelta[2];
        if d2 <= MIN_DELTA_SQR && n2 <= MIN_NDELTA_SQR {
            continue;
        }

        for &k in targets {
            let Some(&(mesh, local)) = mesh_of_vertex.get(k) else {
                continue;
            };
            pending.push(Pending {
                mesh,
                local,
                delta,
                ndelta,
                side,
            });
        }
    }

    // ---- speed：按 `decay` 归一（`simplify.cpp:2575-2599`）----
    // `scale` = 本 flex 全部 delta 里最长的那个；为 0 时取 0.01。
    let mut max_len = 0.0f32;
    for p in &pending {
        let l =
            (p.delta[0] * p.delta[0] + p.delta[1] * p.delta[1] + p.delta[2] * p.delta[2]).sqrt();
        if l > max_len {
            max_len = l;
        }
    }
    if max_len == 0.0 {
        max_len = 0.01;
    }

    // ---- 按 mesh 分组 ----
    // 顺序：**与 studiomdl 一致地按顶点下标升序**（`write.cpp:1784-1809`
    // 是按 `g_flexkey[j].vanim[k]` 的原始顺序写的，而那个数组是按
    // VTA 顶点的处理顺序累出来的）。
    for (mesh, slot) in out.iter_mut().enumerate() {
        let mut anims: Vec<ResolvedVertAnim> = pending
            .iter()
            .filter(|p| p.mesh == mesh)
            .map(|p| {
                let len =
                    (p.delta[0] * p.delta[0] + p.delta[1] * p.delta[1] + p.delta[2] * p.delta[2])
                        .sqrt();
                let speed = if flex.decay == 0.0 {
                    1.0
                } else {
                    (len / (max_len * flex.decay)).clamp(0.0, 1.0)
                };
                ResolvedVertAnim {
                    wrinkle: 0.0,
                    index: p.local,
                    speed: (255.0 * speed) as u8,
                    side: (255.0 * p.side.clamp(0.0, 1.0)) as u8,
                    delta: p.delta,
                    ndelta: p.ndelta,
                }
            })
            .collect();
        if anims.is_empty() {
            continue;
        }
        // `write.cpp` 是按 vanim 原序写的；为稳定性按顶点下标排序。
        anims.sort_by_key(|a| a.index);

        // `target0..3`（`studiomdl.cpp:3585-3588` + `write.cpp:1765-1769`）：
        // 缺省 [0, 1, 10, 11]；`position` 覆盖 target1。
        // 实测确认缺省就是 [0,1,10,11]，与 studio.h 的注释不符。
        //
        // ⚠️ 官方 `Option_Eyelid` 的三条 flexkey **各有各的分段 targets**
        // （`[-11,-10,a,b]` / `[a,b,c,d]` / `[c,d,10,11]`），由 `Flex::targets`
        // 显式携带 —— 它绝不能走这条缺省。
        let targets = flex.targets.unwrap_or([0.0, flex.position, 10.0, 11.0]);

        slot.flexes.push(ResolvedFlex {
            flexdesc: flexdesc.0,
            targets,
            flexpair: flexdesc.1,
            vertanimtype: 0,
            vertanims: anims,
        });
    }

    Ok(out)
}

/// `flexpair` 的 R/L desc 名（`FUN_0045a7b0`：先 R 后 L）。
pub fn pair_names(name: &str) -> (String, String) {
    (format!("{name}R"), format!("{name}L"))
}

/// **朴素**参考实现：`V_vta × V_model` 双重循环，无空间索引。
///
/// 只用于测试 —— 生产路径走 [`build_vanim_map`] 的网格版。保留它是为了
/// 用差分测试证明「优化后逐位相同」，而不是靠肉眼看代码。
#[cfg(test)]
pub(crate) fn build_vanim_map_naive(vta: &Vta, model_verts: &[Vertex]) -> VanimMap {
    let mut map: VanimMap = vec![Vec::new(); vta.num_vertices];
    let frame0 = vta.frame(0).unwrap_or(&[]);
    let mut best_of_model: Vec<Option<(f32, f32, usize)>> = vec![None; model_verts.len()];
    for v in frame0 {
        let vi = v.index as usize;
        if vi >= map.len() {
            continue;
        }
        for (k, mv) in model_verts.iter().enumerate() {
            let d = [
                mv.pos[0] - v.pos[0],
                mv.pos[1] - v.pos[1],
                mv.pos[2] - v.pos[2],
            ];
            let dist = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            // 与生产路径一致：NaN 显式跳过（官方是 `dist < 0.15`）。
            if dist.is_nan() || dist >= MATCH_DIST_SQR {
                continue;
            }
            let dot = mv.normal[0] * v.normal[0]
                + mv.normal[1] * v.normal[1]
                + mv.normal[2] * v.normal[2];
            let better = match best_of_model[k] {
                None => true,
                Some((bd, bdot, _)) => dist < bd || (dist == bd && dot > bdot),
            };
            if better {
                best_of_model[k] = Some((dist, dot, vi));
            }
        }
    }
    for (k, slot) in best_of_model.iter().enumerate() {
        if let Some((_, _, vi)) = *slot {
            map[vi].push(k);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smd::SmdPose;
    use crate::vta::parse_vta;

    fn vert(pos: [f32; 3], normal: [f32; 3]) -> Vertex {
        Vertex {
            pos,
            normal,
            uv: [0.0, 0.0],
            bones: vec![[0.0, 1.0]],
        }
    }

    /// 两个受控顶点，X 覆盖 smoothstep 的三种区间。
    fn two_verts() -> Vec<Vertex> {
        vec![
            vert([-2.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([0.5, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ]
    }

    fn vta_text(dz_frame1: f32) -> String {
        format!(
            "\
version 1
nodes
0 \"w\" -1
end
skeleton
time 0
0 0 0 0 0 0 0
time 1
0 0 0 0 0 0 0
end
vertexanimation
time 0
0 -2.000000 0.000000 0.000000 0.000000 0.000000 1.000000
1 0.500000 0.000000 0.000000 0.000000 0.000000 1.000000
time 1
0 -2.000000 0.000000 {dz_frame1:.6} 0.000000 0.000000 1.000000
1 0.500000 0.000000 {dz_frame1:.6} 0.000000 0.000000 1.000000
end
"
        )
    }

    fn flex(vta: &str, name: &str, frame: i32) -> Flex {
        Flex {
            vta: vta.to_string(),
            name: name.to_string(),
            frame,
            pair: false,
            split: 0.0,
            position: 1.0,
            decay: 1.0,
            targets: None,
            from_eyelid: false,
        }
    }

    fn map2() -> Vec<(usize, u32)> {
        vec![(0, 0), (0, 1)]
    }

    #[test]
    fn split_uses_animated_positions_for_scale_and_pair_side() {
        let mut vta = parse_vta(&vta_text(1.0)).unwrap();
        vta.frames[1][0].pos[0] = 0.5;
        let verts = two_verts();
        let mut specification = flex("x.vta", "shape", 1);
        specification.split = 1.0;
        specification.decay = 0.0;
        let unpaired = resolve_flex(&specification, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        let vertex = &unpaired[0].flexes[0].vertanims[0];
        assert!((vertex.delta[2] - 0.15625).abs() < 1e-6);
        specification.pair = true;
        let paired = resolve_flex(&specification, &vta, &verts, &map2(), (0, 1), 0).unwrap();
        let vertex = &paired[0].flexes[0].vertanims[0];
        assert_eq!(vertex.side, 215);
        assert_eq!(vertex.delta[2], 1.0);
    }

    /// 基本路径：两个顶点都沿 +Z 位移 10，`pair=false` ⟹ 全量、`side=0`。
    #[test]
    fn resolves_basic_delta() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].flexes.len(), 1);
        let fx = &out[0].flexes[0];
        assert_eq!(fx.flexdesc, 0);
        assert_eq!(
            fx.targets,
            [0.0, 1.0, 10.0, 11.0],
            "缺省 target 是 [0,1,10,11]"
        );
        assert_eq!(fx.vertanims.len(), 2);
        for a in &fx.vertanims {
            assert!((a.delta[2] - 10.0).abs() < 1e-3, "delta.z 应为 10：{a:?}");
            assert_eq!(a.side, 0, "非配对时 side 必须为 0");
            assert_eq!(a.speed, 255, "单一时长归一后 speed 应为 255");
        }
    }

    /// ⛔ `frame = 0` 必须**显式报错**，不能静默产出空载荷。
    ///
    /// 这是 L4D2 那个坑的核心：静默产出 0 载荷的症状与「特性没实现」
    /// 完全一样，所以我们要把它变成可见的错误。
    #[test]
    fn frame_zero_is_rejected_not_silently_empty() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.frame = 0;
        let e = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).expect_err("frame 0 必须报错");
        assert!(
            e.message.contains("特殊帧") || e.message.contains("frame = 0"),
            "错误信息应解释 frame 0 的问题：{}",
            e.message
        );
    }

    /// `frame` 超出 `.vta` 帧数必须报错（对应 studiomdl 的 `Frame MdlError`）。
    #[test]
    fn frame_out_of_range_is_rejected() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let f = flex("x.vta", "vanim", 5);
        let e = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).expect_err("超范围必须报错");
        assert!(e.message.contains("超出"), "{}", e.message);
    }

    /// **就近匹配的阈值是硬的** —— 顶点挪远就不该有载荷。
    ///
    /// 这正是受控实验里「对照组也失败」的成因（VTA 顶点位置离模型
    /// 顶点超过 0.15）。
    #[test]
    fn distant_vertices_do_not_match() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        // 模型顶点挪到 X = 100，与 VTA 的 -2 / 0.5 相距极远。
        let verts = vec![
            vert([100.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([100.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ];
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        assert!(
            out[0].flexes.is_empty(),
            "距离超过阈值时不应产生任何 flex 载荷"
        );
    }

    /// `split` 缩放：X = -2 的顶点 scale = 1，X = 0.5 的顶点 scale = 0.15625。
    ///
    /// `t = (1 - 0.5) / 2 = 0.25`，`3t² - 2t³ = 0.1875 - 0.03125 = 0.15625`。
    #[test]
    fn split_scales_delta_by_smoothstep() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.split = 1.0;
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        let fx = &out[0].flexes[0];
        assert_eq!(fx.vertanims.len(), 2, "两个顶点的 scale 都 > 0");
        let by_idx: Vec<_> = fx.vertanims.iter().collect();
        assert!(
            (by_idx[0].delta[2] - 10.0).abs() < 1e-3,
            "X=-2 ⟹ scale=1 ⟹ delta 10，实际 {:?}",
            by_idx[0].delta
        );
        assert!(
            (by_idx[1].delta[2] - 1.5625).abs() < 1e-3,
            "X=0.5 ⟹ scale=0.15625 ⟹ delta 1.5625，实际 {:?}",
            by_idx[1].delta
        );
        assert_eq!(by_idx[1].side, 0, "非配对时 side 恒 0");
    }

    /// `scale == 0` 的顶点被**丢弃**（`simplify.cpp:2524`）。
    #[test]
    fn split_zero_scale_vertices_are_dropped() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        // X = 2 的顶点在 split=1.0 时 scale = 0 ⟹ 应被丢弃。
        let verts = vec![
            vert([-2.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([2.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ];
        let text = "\
version 1
nodes
0 \"w\" -1
end
skeleton
time 0
0 0 0 0 0 0 0
time 1
0 0 0 0 0 0 0
end
vertexanimation
time 0
0 -2.000000 0.000000 0.000000 0.000000 0.000000 1.000000
1 2.000000 0.000000 0.000000 0.000000 0.000000 1.000000
time 1
0 -2.000000 0.000000 10.000000 0.000000 0.000000 1.000000
1 2.000000 0.000000 10.000000 0.000000 0.000000 1.000000
end
";
        let vta2 = parse_vta(text).unwrap();
        let _ = vta;
        let mut f = flex("x.vta", "vanim", 1);
        f.split = 1.0;
        let out = resolve_flex(&f, &vta2, &verts, &map2(), (0, 0), 0).unwrap();
        let fx = &out[0].flexes[0];
        assert_eq!(fx.vertanims.len(), 1, "scale==0 的顶点应被丢弃");
        assert_eq!(fx.vertanims[0].index, 0, "留下的应是 X=-2 那个");
    }

    /// `pair = true` ⟹ **全量位移** + `side` 通道（受控实验 p02）。
    ///
    /// X = -2 ⟹ side = 1-1 = 0；X = 0.5 ⟹ side = 1-0.15625 = 0.84375 ⟹ 215。
    #[test]
    fn pair_gives_full_delta_and_side_channel() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.pair = true;
        f.split = 1.0;
        // `pair` 时 flexdesc = (R 的下标, L 的下标)；`flexpair` 字段落 L。
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 1), 1).unwrap();
        let fx = &out[0].flexes[0];
        assert_eq!(fx.flexdesc, 0, "flexdesc 落 R");
        assert_eq!(fx.flexpair, 1, "flexpair 字段落 L");
        for a in &fx.vertanims {
            assert!(
                (a.delta[2] - 10.0).abs() < 1e-3,
                "配对时 delta 应为全量 10，实际 {:?}",
                a.delta
            );
        }
        let sides: Vec<u8> = fx.vertanims.iter().map(|a| a.side).collect();
        assert_eq!(sides, vec![0, 215], "side 应为 255*(1-scale)");
    }

    /// **`pair` 与 `split` 正交** —— `split = 0` 时配对仍然全量且 `side` 全 0。
    ///
    /// 这就是受控实验 `q4`（`numflexdesc=2` 但 `side` 全 0），
    /// 也是「`pair` 可以是 bool」的依据。
    #[test]
    fn pair_and_split_are_orthogonal() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.pair = true;
        f.split = 0.0;
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 1).unwrap();
        let fx = &out[0].flexes[0];
        assert_eq!(fx.vertanims.len(), 2, "split=0 不丢弃顶点");
        for a in &fx.vertanims {
            assert!((a.delta[2] - 10.0).abs() < 1e-3);
            assert_eq!(a.side, 0, "split=0 ⟹ 1-scale = 0 ⟹ side 全 0");
        }
    }

    /// `decay == 0` ⟹ `speed = 255`（`simplify.cpp:2591-2594`）。
    #[test]
    fn decay_zero_gives_full_speed() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.decay = 0.0;
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        for a in &out[0].flexes[0].vertanims {
            assert_eq!(a.speed, 255);
        }
    }

    /// 多对多匹配：两个模型顶点重合时，一个 VTA 顶点应喂给两者。
    ///
    /// 依据 `simplify.cpp:2229-2230` 的注释（材质边界上顶点会被复制）。
    #[test]
    fn one_vanim_vertex_can_feed_multiple_model_vertices() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        // 两个顶点位置完全相同（模拟材质边界复制）。
        let verts = vec![
            vert([-2.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([-2.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ];
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &[(0, 0), (0, 1)], (0, 0), 0).unwrap();
        // VTA 顶点 0 应喂给模型顶点 0；顶点 1 因「同距时取先到的」只归一个。
        assert!(
            !out[0].flexes.is_empty(),
            "重合顶点应至少匹配上一个模型顶点"
        );
    }

    /// 形状落到**正确的 mesh**（`mesh_of_vertex` 决定分组）。
    #[test]
    fn flexes_are_grouped_into_the_right_mesh() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        // 顶点 0 → mesh 0，顶点 1 → mesh 1。
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &[(0, 0), (1, 0)], (0, 0), 0).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].flexes.len(), 1, "mesh 0 应有一条 flex");
        assert_eq!(out[1].flexes.len(), 1, "mesh 1 应有一条 flex");
        assert_eq!(out[0].flexes[0].vertanims[0].index, 0);
        assert_eq!(
            out[1].flexes[0].vertanims[0].index, 0,
            "各自 mesh 内局部下标"
        );
    }

    /// `position` 覆盖 `target1`（受控实验 m6：`targets=[0,0.5,10,11]`）。
    #[test]
    fn position_overrides_target1() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.position = 0.5;
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        assert_eq!(out[0].flexes[0].targets, [0.0, 0.5, 10.0, 11.0]);
    }

    /// R/L 命名顺序：**先 R 后 L**（`FUN_0045a7b0`）。
    #[test]
    fn pair_names_are_r_then_l() {
        let (r, l) = pair_names("AU1");
        assert_eq!(r, "AU1R");
        assert_eq!(l, "AU1L");
    }

    /// 位置差量为 0 但法线变了 ⟹ 仍然要产出（`simplify.cpp:2539` 是 `||`）。
    #[test]
    fn normal_only_change_still_produces_payload() {
        let text = "\
version 1
nodes
0 \"w\" -1
end
skeleton
time 0
0 0 0 0 0 0 0
time 1
0 0 0 0 0 0 0
end
vertexanimation
time 0
0 -2.000000 0.000000 0.000000 0.000000 0.000000 1.000000
time 1
0 -2.000000 0.000000 0.000000 0.000000 0.600000 0.800000
end
";
        let vta = parse_vta(text).unwrap();
        let verts = vec![vert([-2.0, 0.0, 0.0], [0.0, 0.0, 1.0])];
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &[(0, 0)], (0, 0), 0).unwrap();
        assert_eq!(out[0].flexes.len(), 1, "仅法线变化也应产出");
        let a = &out[0].flexes[0].vertanims[0];
        assert!(a.delta[2].abs() < 1e-6, "位置差量应为 0");
        assert!((a.ndelta[1] - 0.6).abs() < 1e-3, "法线差量应为 0.6");
    }

    /// 完全没变化的形状 ⟹ 没有 vertanim ⟹ 不产生 flex（`numverts == 0`）。
    #[test]
    fn identical_frame_produces_no_vertanims() {
        let vta = parse_vta(&vta_text(0.0)).unwrap();
        let verts = two_verts();
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        assert!(
            out[0].flexes.is_empty(),
            "差量为 0 的形状不应产出 flex 记录"
        );
    }

    /// `split < 0` 时左右镜像（`simplify.cpp:2494-2510`）。
    #[test]
    fn negative_split_mirrors() {
        let vta = parse_vta(&vta_text(10.0)).unwrap();
        let verts = two_verts();
        let mut f = flex("x.vta", "vanim", 1);
        f.split = -1.0;
        let out = resolve_flex(&f, &vta, &verts, &map2(), (0, 0), 0).unwrap();
        let fx = &out[0].flexes[0];
        // split=-1：x=-2 < -1 ⟹ scale=0（丢弃）；x=0.5 > 1? 否 ⟹ 区间内。
        // t = (-1 - 0.5) / (2*-1) = 0.75；3t²-2t³ = 1.6875 - 0.84375 = 0.84375
        assert_eq!(
            fx.vertanims.len(),
            1,
            "X=-2 在负 split 下 scale=0，应被丢弃"
        );
        assert_eq!(fx.vertanims[0].index, 1);
        assert!(
            (fx.vertanims[0].delta[2] - 8.4375).abs() < 1e-3,
            "镜像后 scale=0.84375 ⟹ delta 8.4375，实际 {:?}",
            fx.vertanims[0].delta
        );
    }

    // 让未使用的 import 在测试里也合法。
    #[allow(dead_code)]
    fn _uses(_: SmdPose) {}

    // ---- `build_vanim_map` 网格版 vs 朴素版的差分测试 ----
    //
    // 网格版是性能优化（`main.smd` 上 9.7e10 → 毫秒级），但必须**逐位**
    // 等价。这里用朴素版当 oracle，覆盖空间索引最容易出错的几类输入。

    /// 造一个 `.vta`，第 0 帧给出 `pts`（下标即数组下标）。
    ///
    /// ⚠️ **必须分开写 `skeleton` 与 `vertexanimation` 两段。**
    /// 顶点数据只认 `vertexanimation` 段里的 `time`；`skeleton` 段里的
    /// 7 字段行是**骨骼姿态**，会被解析器直接忽略。
    ///
    /// 我第一版把数据行放在 `skeleton` 段、且没写 `vertexanimation`，
    /// 于是 `frames` 始终为空 ⟹ `frame(0)` 返回 `None` ⟹ 两边都得到
    /// **全空的 `VanimMap`**，差分测试全部**空洞通过**。
    /// 所以这里末尾加一条 `assert`，让「夹具是空的」变成显式失败。
    fn vta_of(pts: &[[f32; 3]]) -> Vta {
        let mut s = String::from("version 1\nnodes\nend\nskeleton\n");
        // `skeleton` 要求 time 0..=1 连续（缺帧会报错）。
        for t in 0..=1 {
            s.push_str(&format!("time {t}\n0 0 0 0 0 0 0\n"));
        }
        s.push_str("end\nvertexanimation\n");
        for t in 0..=1 {
            s.push_str(&format!("time {t}\n"));
            for (i, p) in pts.iter().enumerate() {
                s.push_str(&format!(
                    "{i} {:.6} {:.6} {:.6} 0.000000 0.000000 1.000000\n",
                    p[0], p[1], p[2]
                ));
            }
        }
        s.push_str("end\n");
        let v = parse_vta(&s).unwrap();
        // 防空洞：第 0 帧必须有数据，否则下面的差分测试毫无意义。
        assert_eq!(
            v.num_frames(),
            2,
            "夹具应有 2 帧；frames 为空说明 `vertexanimation` 段没写对"
        );
        if !pts.is_empty() {
            assert_eq!(
                v.frame(0).map(|f| f.len()),
                Some(pts.len()),
                "夹具第 0 帧的顶点数应与 pts 一致"
            );
        }
        v
    }

    /// 差分：网格版必须与朴素版逐位相同。
    ///
    /// ⚠️ 同时断言**结果非空** —— 否则夹具写错（例如 `frames` 为空）时
    /// 两边都返回全空 map，测试会**空洞通过**。这正是我第一版踩的坑。
    fn assert_map_matches_naive(pts: &[[f32; 3]], verts: &[Vertex], label: &str) {
        let vta = vta_of(pts);
        let got = build_vanim_map(&vta, verts);
        let want = build_vanim_map_naive(&vta, verts);
        assert_eq!(got.len(), want.len(), "{label}: map 长度不同");
        for i in 0..want.len() {
            assert_eq!(got[i], want[i], "{label}: map[{i}] 不同");
        }
        if !pts.is_empty() && !verts.is_empty() {
            assert!(
                want.iter().any(|v| !v.is_empty()),
                "{label}: 没有任何匹配 —— 夹具无效，差分测试是空洞的"
            );
        }
    }

    /// 随机点云 —— 大量点落在格子边界与阈值边界附近。
    ///
    /// 用固定种子的 LCG，保证可复现（不引第三方 rand）。
    #[test]
    fn grid_map_matches_naive_on_random_cloud() {
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0
        };
        for round in 0..8 {
            let n = 60;
            // 尺度跨过 cell（= sqrt(0.15) ≈ 0.387）：有的点挤在一起，
            // 有的散开，逼出「同格/邻格/远格」三种情形。
            let scale = [0.05f32, 0.2, 0.387, 0.5, 1.0, 3.0, 10.0, 40.0][round];
            let pts: Vec<[f32; 3]> = (0..n)
                .map(|_| [next() * scale, next() * scale, next() * scale])
                .collect();
            let verts: Vec<Vertex> = (0..n)
                .map(|i| {
                    // 一半模型顶点就落在 VTA 点上（距离 0），一半加随机抖动。
                    let base = pts[i % pts.len()];
                    let j = if i % 2 == 0 { 0.0 } else { scale * 0.3 };
                    vert(
                        [
                            base[0] + next() * j,
                            base[1] + next() * j,
                            base[2] + next() * j,
                        ],
                        [0.0, 0.0, 1.0],
                    )
                })
                .collect();
            assert_map_matches_naive(&pts, &verts, &format!("随机 scale={scale}"));
        }
    }

    /// 精确落在格子边界上（`x / cell` 恰为整数）—— 浮点取整最容易偏一格。
    #[test]
    fn grid_map_matches_naive_on_cell_boundaries() {
        let cell = MATCH_DIST_SQR.sqrt();
        let mut pts = Vec::new();
        for i in -2..=2 {
            for j in -2..=2 {
                for k in -2..=2 {
                    pts.push([i as f32 * cell, j as f32 * cell, k as f32 * cell]);
                }
            }
        }
        // 模型顶点：正好在格点上，以及格点 ± 一点点（跨格）。
        let mut verts = Vec::new();
        for p in &pts {
            verts.push(vert([p[0], p[1], p[2]], [0.0, 0.0, 1.0]));
            verts.push(vert([p[0] + 1e-6, p[1], p[2]], [0.0, 0.0, 1.0]));
            verts.push(vert([p[0] - 1e-6, p[1], p[2]], [0.0, 0.0, 1.0]));
        }
        assert_map_matches_naive(&pts, &verts, "格点边界");
    }

    /// 距离恰好在阈值两侧（`dist == MATCH_DIST_SQR` 必须**排除**）。
    #[test]
    fn grid_map_matches_naive_on_threshold_edges() {
        let d = MATCH_DIST_SQR.sqrt();
        let pts = vec![[0.0, 0.0, 0.0]];
        let mut verts = Vec::new();
        // 恰好等于阈值（应排除）、略小（应命中）、略大（应排除）。
        for eps in [-1e-6f32, 0.0, 1e-6] {
            verts.push(vert([d + eps, 0.0, 0.0], [0.0, 0.0, 1.0]));
            verts.push(vert([0.0, d + eps, 0.0], [0.0, 0.0, 1.0]));
            verts.push(vert([0.0, 0.0, d + eps], [0.0, 0.0, 1.0]));
            // 斜对角：各轴 d/sqrt(3) ⟹ 距离恰为 d
            let s = (d + eps) / 3.0f32.sqrt();
            verts.push(vert([s, s, s], [0.0, 0.0, 1.0]));
        }
        assert_map_matches_naive(&pts, &verts, "阈值边界");
    }

    /// 并列（同距、不同法线点积）—— 裁决必须与遍历顺序一致。
    #[test]
    fn grid_map_matches_naive_on_ties() {
        let pts = vec![[0.0, 0.0, 0.0]];
        let mut verts = Vec::new();
        // 8 个点，与原点距离全等（各轴 ±0.1），法线点积各不相同。
        for sx in [-0.1f32, 0.1] {
            for sy in [-0.1f32, 0.1] {
                for sz in [-0.1f32, 0.1] {
                    let n = [sx * 10.0, sy * 10.0, sz * 10.0];
                    verts.push(vert([sx, sy, sz], n));
                }
            }
        }
        assert_map_matches_naive(&pts, &verts, "并列裁决");
    }

    /// 非有限坐标（NaN/±inf）—— 网格版会跳过，朴素版靠比较自然淘汰，
    /// 两者必须给出同样的结果（都不匹配）。
    #[test]
    fn grid_map_matches_naive_on_non_finite() {
        let pts = vec![
            [0.0, 0.0, 0.0],
            [f32::NAN, 0.0, 0.0],
            [f32::INFINITY, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        ];
        let verts = vec![
            vert([0.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([f32::NAN, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([f32::INFINITY, 0.0, 0.0], [0.0, 0.0, 1.0]),
            vert([1.0, 1.0, 1.0], [0.0, 0.0, 1.0]),
        ];
        assert_map_matches_naive(&pts, &verts, "非有限坐标");
    }

    /// 空输入与「第 0 帧为空」不能 panic。
    #[test]
    fn grid_map_handles_empty() {
        let vta = vta_of(&[]);
        assert!(build_vanim_map(&vta, &[]).is_empty());
        assert_eq!(build_vanim_map(&vta, &[]), build_vanim_map_naive(&vta, &[]));
        let vta2 = vta_of(&[[0.0, 0.0, 0.0]]);
        assert!(build_vanim_map(&vta2, &[]).iter().all(|v| v.is_empty()));
    }

    /// ⛔ 第 0 帧里**重复下标**时，差量基准必须取**第一条** ——
    /// 与原来的 `base.iter().find(|b| b.index == vi)` 一致。
    ///
    /// 这是把线性 `find` 换成 `HashMap` 时最容易踩的坑：
    /// `HashMap` 的 `collect` 在键重复时保留**最后**一条，语义正好相反，
    /// 而且**不会报错** —— 只表现为形状整体偏移。
    ///
    /// 规范 `.vta` 的 `index` 在帧内唯一，但这里不能依赖它。
    #[test]
    fn duplicate_base_index_uses_first_entry() {
        // 第 0 帧故意让下标 0 出现两次，位置差很多。
        let text = "\
version 1
nodes
0 \"w\" -1
end
skeleton
time 0
0 0 0 0 0 0 0
time 1
0 0 0 0 0 0 0
end
vertexanimation
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000
0 5.000000 0.000000 0.000000 0.000000 0.000000 1.000000
time 1
0 0.000000 0.000000 10.000000 0.000000 0.000000 1.000000
end
";
        let vta = parse_vta(text).unwrap();
        assert_eq!(vta.num_frames(), 2, "夹具应有 2 帧");
        assert_eq!(
            vta.frame(0).map(|f| f.len()),
            Some(2),
            "第 0 帧应有 2 条（重复下标 0）"
        );
        // 模型顶点在**第一条**的位置（原点）附近。
        let verts = vec![vert([0.0, 0.0, 0.0], [0.0, 0.0, 1.0])];
        let f = flex("x.vta", "vanim", 1);
        let out = resolve_flex(&f, &vta, &verts, &[(0, 0)], (0, 0), 0).unwrap();
        let fx = &out[0].flexes[0];
        assert_eq!(fx.vertanims.len(), 1);
        // 基准取**第一条**（z=0）⟹ delta.z = 10 − 0 = 10。
        // 若误取第二条（z=0 但 x=5），delta.x 会变成 −5。
        let a = &fx.vertanims[0];
        assert!(
            (a.delta[2] - 10.0).abs() < 1e-3,
            "应以第 0 帧**第一条**为基准 ⟹ delta.z=10，实际 {:?}",
            a.delta
        );
        assert!(
            a.delta[0].abs() < 1e-3,
            "delta.x 应为 0（说明没取到第二条 x=5 的条目），实际 {}",
            a.delta[0]
        );
    }

    /// 大点云：证明网格版**确实**比朴素版快（否则这个优化没意义）。
    ///
    /// 朴素版是 `O(V_vta × V_model)`。取 `n = 6000` ⟹ 3.6e7 次距离计算，
    /// 足够让平方项压过网格版建表的常数开销（`HashMap` 插入/查询）。
    ///
    /// ⚠️ 阈值取 2× 而不是 4×：小规模下网格版的 `HashMap` 常数占比高，
    /// 断言太紧会在慢机器上抖动误报。**真正的大头在真实数据上**：
    /// `main.smd` 是 180180 × 540540 量级，实测编译 432 s → 0.9 s。
    #[test]
    fn grid_map_is_faster_than_naive() {
        let mut seed = 0x9e37_79b9u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0
        };
        let n = 6000;
        let pts: Vec<[f32; 3]> = (0..n)
            .map(|_| [next() * 50.0, next() * 50.0, next() * 50.0])
            .collect();
        // 每个模型顶点都紧贴在某个 VTA 顶点上 ⟹ 全部命中，工作量是实打实的。
        let verts: Vec<Vertex> = (0..n)
            .map(|i| {
                let b = pts[i];
                vert([b[0] + 1e-4, b[1], b[2]], [0.0, 0.0, 1.0])
            })
            .collect();
        let vta = vta_of(&pts);

        let t0 = std::time::Instant::now();
        let naive = build_vanim_map_naive(&vta, &verts);
        let t_naive = t0.elapsed();

        let t1 = std::time::Instant::now();
        let grid = build_vanim_map(&vta, &verts);
        let t_grid = t1.elapsed();

        assert_eq!(grid, naive, "快慢无所谓，结果必须一致");
        // 非空洞：必须真的匹配上了东西。
        assert!(
            naive.iter().any(|v| !v.is_empty()),
            "没有任何匹配 —— 夹具无效"
        );
        assert!(
            t_grid * 2 < t_naive,
            "网格版没有明显更快：naive={t_naive:?} grid={t_grid:?}"
        );
    }
}
