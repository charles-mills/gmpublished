//! PHY 碰撞文件写出（Source 引擎 `.phy`，MDL v49 / L4D2）。
//!
//! ```text
//! +0      phyheader_t                    16 B  { size=16, id=0, solidCount, checksum }
//! +16     solid[0]                       32 B 头 + IVP_Compact_Surface(48) + ledge 区 + 树
//!         solid[1]                       stride = size + 4 = surfaceSize + 32
//!         ...
//!         text section                   KeyValues 文本，末尾 1 个 0x00
//! ```
//! ```text
//! c_point_offset(i) = Σ_{j >= i} (16 + 16 × nTri_j)
//! ```
//! ```text
//! hulls checked=2204   multi-hull solids=153
//! c_point_offset == 16 + 16*nTri :  774/2204   ← 只有单 hull 的那 774 个
//! c_point_offset -> 共享数组起点  : 2204/2204   ← 全部命中
//! c_point_offset == 后缀和        : 1583/1583   ← 全部多 hull 的 hull 都命中
//! ```
//! ```text
//!   A = ∫x'²dV / V      B = ∫y'²dV / V      C = ∫z'²dV / V
//!   rotation_inertia = [ √(B²+C²),  √(A²+C²),  √(A²+B²) ]
//! ```

use std::collections::HashMap;

use parry3d::mass_properties::MassProperties;
use parry3d::math::Vector;
use parry3d::transformation::vhacd::{VHACD, VHACDParameters};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// `phyheader_t` 的字节大小（也是 `phyheader_t::size` 的恒定取值）。
pub const PHY_HEADER_SIZE: usize = 16;
/// 每个 solid 记录里、`IVP_Compact_Surface` 之前的头字节数。
pub const SOLID_HEADER_SIZE: usize = 32;
/// `IVP_Compact_Surface` 的字节大小。
pub const COMPACT_SURFACE_SIZE: usize = 48;
/// `IVP_Compact_Ledge` 的字节大小。
pub const LEDGE_HEADER_SIZE: usize = 16;
/// `IVP_Compact_Triangle` 的字节大小。
pub const TRIANGLE_SIZE: usize = 16;
/// `IVP_Compact_Poly_Point` 的字节大小。
pub const POINT_SIZE: usize = 16;
/// `IVP_Compact_Ledgetree_Node` 的字节大小。
pub const LEDGETREE_NODE_SIZE: usize = 28;

/// **Source 单位（inch）→ IVP 内部单位（米）的换算系数**。
///
/// # 为什么 `.phy` 的点不是模型局部坐标
///
/// `.mdl` / `.vvd` / `.vtx` / SMD **全部**用 Source 单位（inch），
/// 但 `.phy` **不是** —— 它由 `vphysics.dll` 的 `CollideWrite`
/// （`collisionmodel.cpp:2345`）序列化，而 vphysics 内部是 **IVP**，
/// 用的是**米**。
///
/// # 判据（受控实验 + 语料全量）
///
/// **受控实验**（`msh1`，已知 50×10×50 inch 的长方体）：
///
/// ```text
/// studiomdl 报告的体积：17000 in^3
/// 官方 .phy 的点算出的体积：0.2786
/// 立方根(17000 / 0.2786) = 39.3691
/// 1 / 0.0254             = 39.3701        ← inch → meter
/// ```
///
/// 官方 `msh1.phy` 的点 bbox 实测
/// `[-0.127, -1.143, -0.127] .. [0.127, 0.127, 1.143]`
/// —— 而源是 `[-5,-5,-5] .. [45,5,45]`。各轴范围
/// `0.254 / 1.27 / 1.27` = `10×0.0254 / 50×0.0254 / 50×0.0254`，逐轴吻合。
///
/// **语料全量**（`probe_phy_unit.js`，1316 个可算的官方 `.phy`）：
/// `mdl hull 最大边 / phy 点最大边` 的中位数 **42.65**、
/// 落在 `39.37±25%` 内的 **66.9%**，而落在 `1.00±0.25` 内的 **0.0%**。
/// （比值不精确等于 39.37 是因为 `.mdl` 的 hull 来自**渲染**网格的顶点 AABB、
/// `.phy` 来自**碰撞**网格 —— 两者不是同一份几何。但量纲差 39 倍是
/// **数量级**差异，不会被几何差异淹没。）
///
/// # 这个 bug 为什么能藏这么久
///
/// `check_invariants` 查的是**布局自洽**（点数组长度、索引范围、
/// `c_point_offset` 后缀和、`surfaceSize == 48 + ledge + 28×nodes）。
/// 把点整体乘 39.37 **不破坏任何一条** —— 长度不变、索引不变、
/// 体积仍是正的。所以它**不会报错**，只会在游戏里表现为
/// 「碰撞体比模型大 39 倍」。
///
/// 同理，`phy_report.js` 报的「Σvolume」也只是跟着变大，
/// 而 `tor1` 的「22728.12 vs 22728.16」那种对比**恰好没暴露它** ——
/// 因为两边都是同一个实现的输出（官方 artifact vs mdlc），
/// 而当时代码里根本没有单位换算，比值又恰好接近 1……
///
/// > 唯一能抓到它的是**与官方产物逐点比对**。受控实验（已知尺寸的
/// > 立方体）是最快的路径：体积的立方根直接给出系数。
pub const SOURCE_TO_IVP: f32 = 0.0254;

/// 把 **Source 世界空间**（inch）的点换成 IVP 世界空间（米）。
///
/// # 这不是单纯的单位换算
///
/// 反编译 `vphysics.dll` 的 `BuildConvexFromVerts`（`0x10083000`，
/// `ConvexFromVerts` = `vtable[1]` = `0x10083230` 的第一段）得到逐字对应：
///
/// ```c
/// pfVar3 = malloc(0x10);
/// pfVar2 = verts[i];
/// fVar1  = pfVar2[1];
/// pfVar3[0] =  pfVar2[0] * DAT_1017d190;   //  +x
/// pfVar3[1] = -pfVar2[2] * DAT_1017d190;   //  −z
/// pfVar3[2] =  DAT_1017d190 * fVar1;       //  +y
/// ```
///
/// `DAT_1017d190` = `0x3cd013a9` = **0.0254**（inch → 米）。
/// 所以轴映射是 **`out = (−y, −z, x)`**，而不是恒等。
///
/// # 这个轴映射是怎么被独立证实的
///
/// 三个互相独立的实验：
///
/// 1. **`physign`**（`docs/_probe/gen_physign.js`）—— 一个**完全不对称**的
///    四面体 `(3,−1,−2) (−4,6,−1) (1,2,7) (−2,−5,3)`，序列姿态全 0。
///    24 种纯旋转给出的顶点集合**两两不同**，所以能唯一定案 ——
///    结果只有 `(-y,-z,x)` 吻合（`probe_physign.js`）。
/// 2. **`phyrot0/1/2`** —— 几何完全相同（同一个 40×20×60 盒子）、
///    只有**序列动画**的骨骼姿态不同。`.phy` 三者不同而 `.vvd` 三者相同
///    ⟹ 姿态确实来自**序列**（`g_panimation[0]`），不是碰撞 SMD 自己的姿态。
/// 3. **mikuw** —— 差分实测得到 `(z,−y,x)`，而它的序列姿态是 `Rx(90°)`，
///    且 `(-y,−z,x) ∘ Rx(90°) = (z,−y,x)`。**完全吻合。**
///
/// 全量验证见 `docs/_probe/probe_phy_verify.js`：14 个模型，
/// 13 个官方点 100% 命中（残差 < 1e-5），mikuw 38/40（中位残差 9.7e-8）。
///
/// # 为什么不能只写 `× 0.0254`
///
/// 少了这个旋转，碰撞体会相对渲染网格**转 90°** —— 玩家会撞到空气、
/// 穿过看得见的模型。`check_invariants` 抓不到：旋转不改变任何
/// 布局自洽性（长度、索引、体积都照旧合法）。
#[inline]
pub fn source_to_ivp_axis(v: [f32; 3]) -> [f32; 3] {
    [-v[1], -v[2], v[0]]
}

/// [`source_to_ivp_axis`] 之后再乘 [`SOURCE_TO_IVP`]。
#[inline]
pub fn to_ivp(v: [f32; 3]) -> [f32; 3] {
    let a = source_to_ivp_axis(v);
    [
        a[0] * SOURCE_TO_IVP,
        a[1] * SOURCE_TO_IVP,
        a[2] * SOURCE_TO_IVP,
    ]
}

/// `VPHYSICS_COLLISION_ID` = `MAKEID('V','P','H','Y')`。
pub const VPHYSICS_ID: u32 = 0x5948_5056;
/// `IVP_COMPACT_SURFACE_ID` = `MAKEID('I','V','P','S')`，写在 `dummy[2]`。
pub const IVP_COMPACT_SURFACE_ID: u32 = 0x5350_5649;
/// `VPHYSICS_COLLISION_VERSION`。
///
/// **不是** Ipion 的 `IVPS_VERSION`——那个在 `.phy` 里根本不存在，
/// IVP 的版本标识是 `dummy[2]` 里的 magic `'IVPS'`。
pub const VPHYSICS_COLLISION_VERSION: u16 = 0x0100;
/// `COLLIDE_POLY`。L4D2 的 774 个 solid 全是它。
pub const COLLIDE_POLY: i16 = 0;

/// `IVP_COMPACT_BOUNDINGBOX_STEP_SIZE` 的倒数：`box_sizes` 的量化网格步长。
const BOUNDINGBOX_STEP: f64 = 1.0 / 250.0;

/// 单个 hull 能容纳的最大三角形数。
///
/// 边 `(k,i)` 的线性编号是 `4k+1+i`，取值范围 `[1, 4·nTri−1]`，所以
/// `|opposite_index| ≤ 4·nTri − 2`。要塞进 15 位二补数（`−16384..=16383`），
/// 需要 `nTri ≤ 4096`。
///
/// 报告 §12 写的是 `IVP_MAX_TRIANGLES_PER_LEDGE = 8192`，那个数字是按
/// `±32767` 推出来的，**用在这个 15 位字段上会溢出**。这里取 4096。
pub const MAX_TRIANGLES_PER_HULL: usize = 4096;

/// `start_point_index` 是 16 位，所以共享点数组最多这么多点。
pub const MAX_POINTS_PER_SOLID: usize = u16::MAX as usize;

// ---------------------------------------------------------------------------
// 输入 / 输出类型
// ---------------------------------------------------------------------------

/// 写出错误。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PhyError {
    /// 凸包顶点少于 4 个，张不成体积。
    #[error(
        "{}",
        crate::tr_fmt!(
            "solid[%{solid}] has only %{points} vertices; a convex hull needs at least 4 non-coplanar points",
            solid = solid,
            points = points
        )
    )]
    HullTooFewPoints { solid: usize, points: usize },
    /// 凸包计算失败（共面 / 共线 / 含 NaN 等退化输入）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the convex hull of solid[%{solid}] is degenerate: %{detail}",
            solid = solid,
            detail = detail
        )
    )]
    HullDegenerate { solid: usize, detail: String },
    /// 面表引用了不存在的顶点。
    #[error(
        "{}",
        crate::tr_fmt!(
            "face %{face} of solid[%{solid}] references vertex %{index}, but there are only %{vertex_count} vertices",
            solid = solid,
            face = face,
            index = index,
            vertex_count = vertex_count
        )
    )]
    IndexOutOfRange {
        solid: usize,
        face: usize,
        index: u32,
        vertex_count: usize,
    },
    /// 同一条有向边出现了两次 —— 网格不是可定向流形。
    #[error(
        "{}",
        crate::tr_fmt!(
            "solid[%{solid}] has the directed edge %{from}->%{to} twice; the mesh is not an orientable manifold",
            solid = solid,
            from = from,
            to = to
        )
    )]
    DuplicateEdge { solid: usize, from: u32, to: u32 },
    /// 有向边找不到反向配对 —— 网格有洞（非闭合）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "solid[%{solid}] has no opposite edge for %{from}->%{to}; the mesh is not closed",
            solid = solid,
            from = from,
            to = to
        )
    )]
    OpenMesh { solid: usize, from: u32, to: u32 },
    /// 三角形太多，`opposite_index` 塞不进 15 位有符号。
    #[error(
        "{}",
        crate::tr_fmt!(
            "solid[%{solid}] has %{triangles} triangles, over the per-hull limit %{max} \
             (opposite_index is only 15 bits)",
            solid = solid,
            triangles = triangles,
            max = max
        )
    )]
    TooManyTriangles {
        solid: usize,
        triangles: usize,
        max: usize,
    },
    /// 顶点下标超出 `start_point_index:16`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "solid[%{solid}] has %{points} vertices, over the 16-bit index limit %{max}",
            solid = solid,
            points = points,
            max = MAX_POINTS_PER_SOLID
        )
    )]
    TooManyPoints { solid: usize, points: usize },
    /// 坐标里出现 NaN 或无穷。
    #[error(
        "{}",
        crate::tr_fmt!(
            "vertex %{point} of solid[%{solid}] contains NaN or infinity",
            solid = solid,
            point = point
        )
    )]
    NonFinitePoint { solid: usize, point: usize },
    /// 包围盒退化成一点或一条线（半径 0），无法量化 `box_sizes`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the bounding box of solid[%{solid}] has radius 0 (all vertices coincide); cannot quantize box_sizes",
            solid = solid
        )
    )]
    DegenerateBounds { solid: usize },
    /// 参数越界。
    #[error(
        "{}",
        crate::tr_fmt!(
            "invalid parameter %{what}: %{detail}",
            what = what,
            detail = detail
        )
    )]
    BadParameter { what: &'static str, detail: String },
    /// 焊接 SMD 三角形后顶点不足 4 个，张不成凸包。
    ///
    /// 由 [`weld_smd_triangles`] 产生 —— 碰撞 SMD 退化成一个点/一条线时
    /// 会撞上这条，而不是等到凸包计算里才失败。
    #[error(
        "{}",
        crate::tr_fmt!(
            "only %{points} distinct vertices after welding; a convex hull needs at least 4",
            points = points
        )
    )]
    WeldTooFewPoints { points: usize },
    /// 写出的字节自检失败 —— 属本实现的 bug，不是调用方的问题。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the written PHY failed its self-check (a bug in this implementation): %{err}",
            err = .0
        )
    )]
    SelfCheck(String),

    // ---- 下面这些由 [`build_phy_from_smd`] / [`build_ragdoll_phy_from_smd`] /
    //      [`decompose_connected_components`] 产生。它们的文案与改造前
    //      逐字节相同，只是从 `String` 变成了可 `match` 的变体。 ----
    /// 碰撞 SMD 里一个三角形都没有。
    #[error("{}", crate::tr_fmt!("the SMD has no triangles"))]
    NoTriangles,
    /// 算凸包失败（退化输入：共面 / 共线 / 含 NaN）。
    #[error(
        "{}",
        crate::tr_fmt!("cannot compute the convex hull: %{err}", err = .0)
    )]
    HullFailed(#[source] Box<PhyError>),
    /// `$concave` 回退成单凸包时，整体凸包也算不出来。
    #[error(
        "{}",
        crate::tr_fmt!("cannot compute the overall convex hull: %{err}", err = .0)
    )]
    OverallHullFailed(#[source] Box<PhyError>),
    /// 顶点太少，连整体凸包都张不成。
    #[error(
        "{}",
        crate::tr_fmt!(
            "only %{points} vertices; not even the overall convex hull can be formed (at least 4 are needed)",
            points = points
        )
    )]
    TooFewVerticesForHull { points: usize },
    /// 单 solid 的 prop 形态写了 `[physics.joint_overrides]`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "[physics.joint_overrides] has %{count} entries, but this is the single-solid prop form \
             (`joints = false`) -- per-joint parameters only make sense for a ragdoll. \
             Either remove it or set `joints` to true",
            count = count
        )
    )]
    JointOverridesOnProp { count: usize },
    /// 单 solid 的 prop 形态写了 `[physics.constraints]`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "[physics.constraints] has %{count} entries, but this is the single-solid prop form \
             (`joints = false`) -- `$jointconstrain` only makes sense for a ragdoll. \
             Either remove it or set `joints` to true",
            count = count
        )
    )]
    ConstraintsOnProp { count: usize },
    /// 单 solid 的 prop 形态写了碰撞规则。
    #[error(
        "{}",
        crate::tr_fmt!(
            "[physics.no_self_collisions] / [physics.collision_pairs] only make sense for a ragdoll \
             (with `joints = false` there is no \"do the multiple solids collide\" question). \
             Either remove it or set `joints` to true"
        )
    )]
    CollisionPairsOnProp,
    /// `$jointmerge` 指向了碰撞 SMD 里没有的骨骼名。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the bone pair (%{a}, %{b}) of [physics.merge][%{index}] has a name that is not in the \
             nodes of the collision SMD. Available bones: %{available}",
            index = index,
            a = format!("{a:?}"),
            b = format!("{b:?}"),
            available = available
        )
    )]
    MergeBoneUnknown {
        index: usize,
        a: String,
        b: String,
        available: String,
    },
    /// `$jointmerge` 会形成归并环。
    #[error(
        "{}",
        crate::tr_fmt!(
            "(%{a}, %{b}) of [physics.merge][%{index}] would form a merge cycle",
            index = index,
            a = format!("{a:?}"),
            b = format!("{b:?}")
        )
    )]
    MergeCycle { index: usize, a: String, b: String },
    /// 碰撞 SMD 里没有任何骨骼带碰撞几何。
    #[error("{}", crate::tr_fmt!("no bone has collision geometry"))]
    NoBoneGeometry,
    /// `[physics.joint_overrides]` 指向了没有碰撞几何的骨骼。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the bone %{bone} in [physics.joint_overrides] has no collision geometry. \
             Bones with collision geometry: %{available}",
            bone = format!("{bone:?}"),
            available = available
        )
    )]
    JointOverrideUnknownBone { bone: String, available: String },
    /// 所有骨骼的碰撞几何都张不成凸包。
    #[error(
        "{}",
        crate::tr_fmt!("the collision geometry of every bone fails to form a convex hull")
    )]
    NoHullFromAnyBone,
    /// `$automass` 需要材质密度表，mdlc 没有。
    #[error(
        "{}",
        crate::tr_fmt!(
            "`[physics].auto_mass` (`$automass`) needs the material density table \
             (`scripts/surfaceproperties_manifest.txt`) to compute the total mass, \
             and mdlc does not have that table. Please write `mass = <value>` explicitly"
        )
    )]
    AutoMassUnsupported,
    /// `write_phy_multi` 失败。
    #[error(
        "{}",
        crate::tr_fmt!("cannot write the PHY: %{err}", err = .0)
    )]
    WriteFailed(#[source] Box<PhyError>),
    /// `[physics.constraints]` 的逐条校验失败。
    #[error(transparent)]
    Constraint(#[from] ConstraintError),
}

/// `[physics.constraints]` 逐条校验的错误。
///
/// 与 [`PhyError`] 分开，是因为它由 [`crate::model::Physics::joint_constraints`]
/// 产生 —— 那是**描述文件**层面的校验，发生在碰几何之前。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConstraintError {
    /// `axis` 不是 `x` / `y` / `z`（官方只取首字母）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the axis=%{axis} of [physics.constraints][%{index}] is invalid: \
             it must be x / y / z (the official code only reads the first letter)",
            index = index,
            axis = format!("{axis:?}")
        )
    )]
    BadAxis { index: usize, axis: String },
    /// `kind` 不是 `free` / `fixed` / `limit`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the kind=%{kind} of [physics.constraints][%{index}] is invalid: \
             it must be free / fixed / limit",
            index = index,
            kind = format!("{kind:?}")
        )
    )]
    BadKind { index: usize, kind: String },
    /// `min > max`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "min=%{min} > max=%{max} for [physics.constraints][%{index}] (bone %{bone}, axis %{axis}); \
             the official compiler aborts with \"Invalid joint constraint\"",
            index = index,
            bone = format!("{bone:?}"),
            axis = axis,
            min = min,
            max = max
        )
    )]
    MinGreaterThanMax {
        index: usize,
        bone: String,
        axis: String,
        min: f32,
        max: f32,
    },
}

/// 一个凸块。
///
/// `vertices` 是模型局部坐标（`$collisionmodel` 的 SMD 顶点**不做任何变换**，
/// 实测 studiomdl 直接把它们喂给 `CollideFromConvex`），
/// `faces` 是 `[u32; 3]` 三角面，**CCW 外向**、闭合、可定向。
#[derive(Debug, Clone, PartialEq)]
pub struct PhyHull {
    /// 凸块顶点（模型局部坐标）。
    pub vertices: Vec<[f32; 3]>,
    /// 三角面（CCW 外向，索引指向 `vertices`）。
    pub faces: Vec<[u32; 3]>,
}

impl PhyHull {
    /// 从任意点集算凸包并构造一个凸块（parry3d quickhull）。
    ///
    /// 面表已经是 CCW 外向的，可以直接进 [`write_phy`]。
    /// 退化输入（少于 4 点、共面、共线、含 NaN）返回
    /// [`PhyError::HullDegenerate`] 而不是 panic —— 这是选
    /// `try_convex_hull` 而不是 `convex_hull` 的原因，后者在退化输入上直接
    /// `unwrap()`。
    pub fn from_points(points: &[[f32; 3]]) -> Result<Self, PhyError> {
        if points.len() < 4 {
            return Err(PhyError::HullTooFewPoints {
                solid: 0,
                points: points.len(),
            });
        }
        for (i, p) in points.iter().enumerate() {
            if !p.iter().all(|c| c.is_finite()) {
                return Err(PhyError::NonFinitePoint { solid: 0, point: i });
            }
        }
        let vs: Vec<Vector> = points
            .iter()
            .map(|p| Vector::new(p[0], p[1], p[2]))
            .collect();
        let (hull_pts, tris) = parry3d::transformation::try_convex_hull(&vs).map_err(|e| {
            PhyError::HullDegenerate {
                solid: 0,
                detail: e.to_string(),
            }
        })?;
        Ok(Self {
            vertices: hull_pts.iter().map(|v| [v.x, v.y, v.z]).collect(),
            faces: tris,
        })
    }

    /// 官方 `ConvexFromVerts` 的第二段：`BuildOuterHull( hull, 0.01 )`。
    ///
    /// # 官方算法（反编译 `vphysics.dll`）
    ///
    /// `ConvexFromVerts`（`vtable[1]` = `0x10083230`）是**两段**：
    ///
    /// ```c
    /// p1 = BuildConvexFromVerts(verts, n);   // 0x10083000：轴映射 + qhull
    /// p2 = BuildOuterHull(p1, 0.01);         // 0x10083110
    /// return p2 ? p2 : p1;
    /// ```
    ///
    /// `BuildOuterHull`（`0x10083110` → `FUN_100c08a0` → `FUN_100c0620`）：
    ///
    /// 1. 逐三角形建平面（法线归一化到 `1e-10`）；
    /// 2. 遍历**平面三元组**，Cramer 解 3×3 得交点；
    /// 3. 交点若满足**所有**平面 `n·p + d ≤ 1e-4` 就收下；
    /// 4. 收下时做 **半径 0.01 米** 的贪心去重（`FUN_100c0580`：
    ///    线性扫描已有代表点，首个 `距离² < r²` 即归并、丢弃新点）；
    /// 5. 对去重后的点**再跑一次 qhull**（`FUN_100c2490`）。
    ///
    /// # 单位
    ///
    /// 反编译里 `0.01` 与 `1e-4` 的**单位是米** —— 因为 `BuildConvexFromVerts`
    /// 已经乘过 0.0254。本函数的输入是 **Source 单位（inch）**，
    /// 所以两个阈值都除以 [`SOURCE_TO_IVP`]。
    ///
    /// 这个换算**不改变结果**：贪心判据是 `距离² < r²`，两边同乘 `k²`
    /// 后判据完全等价，所以点数与选中的点（按比例）都不变。
    ///
    /// # 验收
    ///
    /// `docs/_probe/probe_phy_simple.js`：`msh1` / `ucc1` / `ucc3` / `physign`
    /// 四个模型压实后的点集与官方 `.phy` **逐点 100% 命中**（残差 < 1e-7）。
    ///
    /// # 已知局限
    ///
    /// 贪心去重是**顺序相关**的（极大独立集不唯一），而顺序由 qhull 的
    /// facet 输出次序决定。mdlc 用 parry3d，facet 次序与官方 qhull 不同，
    /// 所以**点数多、结构复杂**的凸包（如 mikuw 的 84 顶点）结果会与官方
    /// 有差异（实测 84 → 49，官方 40）—— 两者都是合法的 1 cm 独立集。
    ///
    /// 平面数超过 [`MAX_COMPACT_PLANES`] 时退化成**只对凸包顶点做去重**
    /// （跳过三元组枚举）—— 那条路径是 `O(P⁴)`，对复杂网格会跑到分钟级。
    pub fn compact_outer_hull(&self) -> Self {
        let r = COMPACT_RADIUS_M / SOURCE_TO_IVP;
        let eps = COMPACT_INSIDE_EPS_M / SOURCE_TO_IVP;

        let planes = hull_planes(&self.vertices, &self.faces);
        if planes.len() < 4 {
            return self.clone();
        }

        let reps = if planes.len() <= MAX_COMPACT_PLANES {
            let mut reps: Vec<[f32; 3]> = Vec::new();
            for i in 0..planes.len() {
                for j in (i + 1)..planes.len() {
                    for k in (j + 1)..planes.len() {
                        let Some(p) = solve_three_planes(&planes[i], &planes[j], &planes[k]) else {
                            continue;
                        };
                        // 必须在**所有**平面内侧（容差 eps）。
                        if planes.iter().any(|q| dot3(q.n, p) + q.d > eps) {
                            continue;
                        }
                        push_unique(&mut reps, p, r);
                    }
                }
            }
            reps
        } else {
            // 回退：直接用凸包顶点（parry 给的已是真顶点）。
            let mut reps: Vec<[f32; 3]> = Vec::new();
            for &p in &self.vertices {
                push_unique(&mut reps, p, r);
            }
            reps
        };

        if reps.len() < 4 {
            return self.clone();
        }
        // 官方最后再跑一次 qhull；失败（退化）时保留原样。
        match Self::from_points(&reps) {
            Ok(hull) if hull.is_convex() => hull,
            _ => self.clone(),
        }
    }

    fn is_convex(&self) -> bool {
        const EPSILON: f64 = 0.001;
        self.faces.iter().all(|face| {
            let [a, b, c] = face.map(|index| self.vertices[index as usize].map(f64::from));
            let ab: [f64; 3] = std::array::from_fn(|axis| b[axis] - a[axis]);
            let ac: [f64; 3] = std::array::from_fn(|axis| c[axis] - a[axis]);
            let normal = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
            if !length.is_finite() || length == 0.0 {
                return false;
            }
            let tolerance = EPSILON * length;
            let mut positive = false;
            let mut negative = false;
            for vertex in &self.vertices {
                let distance = (0..3)
                    .map(|axis| (f64::from(vertex[axis]) - a[axis]) * normal[axis])
                    .sum::<f64>();
                if !distance.is_finite() {
                    return false;
                }
                positive |= distance > tolerance;
                negative |= distance < -tolerance;
                if positive && negative {
                    return false;
                }
            }
            true
        })
    }
}

/// 贪心去重的一步：与已有代表点距离² < r² 就丢弃，否则追加。
///
/// 与官方 `FUN_100c0580` 同构 —— **线性扫描、首个命中即返回**，
/// 所以结果与插入顺序有关。
fn push_unique(reps: &mut Vec<[f32; 3]>, p: [f32; 3], r: f32) {
    let rr = r * r;
    if reps.iter().any(|q| dist2(*q, p) < rr) {
        return;
    }
    reps.push(p);
}

/// 一个支撑平面：`n·x + d = 0`，`n` 单位且**朝外**（体内点 `n·x + d ≤ 0`）。
#[derive(Clone, Copy)]
struct HullPlane {
    n: [f32; 3],
    d: f32,
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// 逐三角形建支撑平面，并做官方 `FUN_100c03f0` 的**近平行合并**。
///
/// # 官方合并规则（反编译 `vphysics.dll` `FUN_100c03f0`）
///
/// 每加一个新平面 `(n, d)`，先扫已有平面：
///
/// ```c
/// if (0.9999 < dot(p.n, n)) {              // 方向夹角 < ~0.81°
///     if (p.d <= d && d != p.d) return;    // 已有更靠外 ⟹ 丢弃新平面
///     mark p for removal;                  // 新平面更靠外 ⟹ 删掉旧的
/// }
/// ```
///
/// 两个要点：
///
/// * 阈值是 **0.9999**（≈0.81°），不是「几乎完全相等」。
///   用 `1-1e-6`（≈0.08°）会留下大量几乎重合的平面，
///   三元组枚举里产生冗余候选点。
/// * 语义是「**保留更靠外的那个**」—— 不是「跳过重复」。
///   新平面更靠外时**要删掉**已有的那个。
///
/// # 朝向归一
///
/// 法线统一为**朝外**（体内点 `n·x + d ≤ 0`）。判据用
/// 「所有点都应落在 ≤0 一侧」，不能只看 `max > 0` ——
/// 浮点舍入会把本该是 0 的 max 变成 +2e-8，于是每个平面都被翻一次。
fn hull_planes(verts: &[[f32; 3]], faces: &[[u32; 3]]) -> Vec<HullPlane> {
    /// 官方 `FUN_100c03f0` 的方向阈值。
    const COS_MERGE: f32 = 0.9999;

    let mut out: Vec<HullPlane> = Vec::with_capacity(faces.len());
    for f in faces {
        let (a, b, c) = (
            verts[f[0] as usize],
            verts[f[1] as usize],
            verts[f[2] as usize],
        );
        let mut n = cross3(
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        // `!(len > 1e-10)` 而不是 `len <= 1e-10`：前者对 NaN 也成立
        // （NaN 会让退化法线漏过去，产生垃圾平面）。
        // 写成 `!` 形式会让 clippy 报 `neg_cmp_op_on_partial_ord`，
        // 所以显式写出 NaN 检查。
        let len = dot3(n, n).sqrt();
        if len.is_nan() || len <= 1e-10 {
            continue;
        }
        n = [n[0] / len, n[1] / len, n[2] / len];
        let mut d = -dot3(n, a);
        // 定朝向：朝外时「所有点的 n·x+d」应以负值为主。
        let mut mx = f32::NEG_INFINITY;
        let mut mn = f32::INFINITY;
        for p in verts {
            let s = dot3(n, *p) + d;
            if s > mx {
                mx = s;
            }
            if s < mn {
                mn = s;
            }
        }
        if mx.abs() > mn.abs() {
            n = [-n[0], -n[1], -n[2]];
            d = -d;
        }

        // 官方合并：近平行的只保留最靠外的那个。
        let mut drop = false;
        let mut remove: Vec<usize> = Vec::new();
        for (i, q) in out.iter().enumerate() {
            if dot3(q.n, n) > COS_MERGE {
                if q.d <= d && d != q.d {
                    drop = true;
                    break;
                }
                remove.push(i);
            }
        }
        if drop {
            continue;
        }
        for &i in remove.iter().rev() {
            out.swap_remove(i);
        }
        out.push(HullPlane { n, d });
    }
    out
}

/// 解三个平面的交点（Cramer 法则）。平行/退化时返回 `None`。
fn solve_three_planes(a: &HullPlane, b: &HullPlane, c: &HullPlane) -> Option<[f32; 3]> {
    let det = dot3(a.n, cross3(b.n, c.n));
    if det.abs() < 1e-12 {
        return None;
    }
    let n1 = cross3(b.n, c.n);
    let n2 = cross3(c.n, a.n);
    let n3 = cross3(a.n, b.n);
    Some([
        (-a.d * n1[0] - b.d * n2[0] - c.d * n3[0]) / det,
        (-a.d * n1[1] - b.d * n2[1] - c.d * n3[1]) / det,
        (-a.d * n1[2] - b.d * n2[2] - c.d * n3[2]) / det,
    ])
}

/// 官方 `BuildOuterHull` 的去重半径：**0.01 米**（反编译 `0x10083230`
/// 传给 `FUN_10083110` 的字面量）。
pub const COMPACT_RADIUS_M: f32 = 0.01;

/// 官方「点在所有平面内侧」的容差：**1e-4 米**（反编译 `FUN_100c0620`
/// 里的 `-0.0001` 比较）。
pub const COMPACT_INSIDE_EPS_M: f32 = 0.0001;

/// 三元组枚举的上限：`O(P³)` 个三元组、每个再查 `P` 个平面 ⟹ `O(P⁴)`。
/// 164 个平面约 1.2 亿次内层判断（可接受）；超过这个数就退化到
/// 只对凸包顶点去重。
pub const MAX_COMPACT_PLANES: usize = 256;

/// `$jointconstrain <骨骼> <轴> <类型> <min> <max> [friction]` 的**类型**。
///
/// 官方 `jointlimit_t`（`collisionmodel.cpp:75-77`）：
/// `JOINT_FREE = 0` / `JOINT_FIXED = 1` / `JOINT_LIMIT = 2`。
///
/// 三种类型在 `BuildRagdollConstraint`（`:2245-2256`）里落盘不同：
///
/// | 类型 | 落盘 |
/// |---|---|
/// | `limit` | `(min, max, friction/5)` |
/// | `fixed` | `(0, 0, 0)` —— **忽略 min/max/friction** |
/// | `free`  | `(-360, 360, friction/5)` —— **忽略 min/max** |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointLimitType {
    /// `free` → 落盘 `±360`。
    Free,
    /// `fixed` → 落盘全 `0`。
    Fixed,
    /// `limit` → 落盘 `(min, max, friction)`。
    Limit,
}

impl JointLimitType {
    /// 从 QC 文本解析（大小写不敏感，与官方 `stricmp` 一致）。
    pub fn parse(s: &str) -> Option<Self> {
        let l = s.to_ascii_lowercase();
        match l.as_str() {
            "free" => Some(Self::Free),
            "fixed" => Some(Self::Fixed),
            "limit" => Some(Self::Limit),
            _ => None,
        }
    }

    /// 官方写出的三个轴值。
    ///
    /// ⚠️ **`friction` 会先除以 5**（`AddConstraint`，`collisionmodel.cpp:539`
    /// 的 `friction * (1.0f/5.0f)`，注释说「编辑器里 friction 显示为 5 倍」）。
    /// 实测 `$jointconstrain ... 5` → 落盘 `1.000000`。
    pub fn axis_values(self, min: f32, max: f32, friction: f32) -> (f32, f32, f32) {
        match self {
            Self::Limit => (min, max, friction / 5.0),
            Self::Fixed => (0.0, 0.0, 0.0),
            Self::Free => (-360.0, 360.0, friction / 5.0),
        }
    }
}

/// 一条 `$jointconstrain`。
///
/// 官方存进 `CJointConstraint` 链表（**头插**，`AddConstraint` 里
/// `pConstraint->m_pNext = m_pConstraintList`）。顺序不影响结果 ——
/// `BuildRagdollConstraint` 遍历整条链，按 `index == ragdoll.childIndex`
/// 过滤，同轴后写覆盖先写。
#[derive(Debug, Clone, PartialEq)]
pub struct JointConstraint {
    /// 骨骼名（`$jointconstrain` 的第 1 个参数）。
    pub bone: String,
    /// 轴：`0 = x` / `1 = y` / `2 = z`。
    ///
    /// 官方算法是 `tolower(axis[0]) - 'x'`（`collisionmodel.cpp:1668`），
    /// 所以只有首字母有意义。
    pub axis: u8,
    /// 约束类型。
    pub kind: JointLimitType,
    /// 下限（度）。
    pub min: f32,
    /// 上限（度）。
    pub max: f32,
    /// 摩擦（**落盘前会 ÷5**）。
    pub friction: f32,
}

impl JointConstraint {
    /// 从轴字母解析 `0/1/2`（官方 `tolower(axis[0]) - 'x'`）。
    pub fn axis_from_char(c: char) -> Option<u8> {
        match c.to_ascii_lowercase() {
            'x' => Some(0),
            'y' => Some(1),
            'z' => Some(2),
            _ => None,
        }
    }

    /// 轴字母（用于诊断输出）。
    pub fn axis_char(&self) -> char {
        ['x', 'y', 'z'][self.axis as usize % 3]
    }
}

/// `$animatedfriction <min> <max> <timein> <timehold> <timeout>`。
///
/// ⚠️ **注意 QC 的参数顺序与落盘顺序不同**：
/// QC 是 `min max timein timehold timeout`，而落盘是
/// `animfrictiontimein` / `animfrictiontimeout` / `animfrictiontimehold`
/// —— `timeout` 与 `timehold` **对调**。
///
/// 依据：`CCmd_JoinAnimatedFriction`（`collisionmodel.cpp:1752-1760`）
/// 把第 3/4/5 个参数依次赋给 `m_flFrictionTimeIn` / `m_flFrictionTimeOut` /
/// `m_flFrictionTimeHold`；写出时（`:2452-2456`）按
/// `In → Out → Hold` 的顺序。
///
/// 受控实验 `animfric`（`$animatedfriction 100 500 0.1 0.2 1.0`）实测：
/// `timein=0.100000`、`timeout=1.000000`、`timehold=0.200000`
/// ⟹ 第 4 个参数 `0.2` 落进 `timehold`，第 5 个 `1.0` 落进 `timeout`。**证实对调**。
///
/// 两个 `*friction*` 字段是**整数**（`Safe_atoi`，不是 `atof`），
/// 但写出用 `KeyWriteFloat` ⟹ 落盘是 `"100.000000"`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimatedFriction {
    /// `animfrictionmin`（**整数**取值）。
    pub min: i32,
    /// `animfrictionmax`（**整数**取值）。
    pub max: i32,
    /// `animfrictiontimein`（秒）。
    pub time_in: f32,
    /// `animfrictiontimeout`（秒）—— QC 的**第 5** 个参数。
    pub time_out: f32,
    /// `animfrictiontimehold`（秒）—— QC 的**第 4** 个参数。
    pub time_hold: f32,
}

/// 一个 solid 的写出参数。
#[derive(Debug, Clone, PartialEq)]
pub struct PhySolid {
    /// 这个 solid 绑定的骨骼下标。
    ///
    /// - `Some(i)` → ledge 的 `client_data` 写 `i + 1`（ragdoll）。
    /// - `None` → 写 `0`，即"未绑定"哨兵（单 solid prop）。
    ///
    /// 实测 90/90 零例外：ragdoll 的每个 solid 的 `client_data` 恒等于
    /// `boneIndex + 1`。**text section 里的 `name` 是另一条独立的绑定路径，
    /// 两条都要写对。**
    pub bone_index: Option<u32>,
    /// Per-joint surface material; otherwise use the model material.
    pub surface_prop: Option<String>,
    /// text section 里 `"name"` 的值。ragdoll 用骨骼名，prop 用 `<model>_physbox`。
    pub name: String,
    /// `"parent"` 的骨骼名。`None` 时不写这一行，也不写对应的 `ragdollconstraint`。
    pub parent: Option<String>,
    /// `"massbias"`。只有 `!= 1.0` 时才写进文本段（与 studiomdl 一致）。
    pub mass_bias: f32,
    /// `"damping"` 的**本 solid 覆盖**。`None` = 用 [`PhyParams::damping`]。
    ///
    /// 对应官方 `$jointdamping`（`collisionmodel.cpp:658-665`）——
    /// 它在 `SetCollisionModelDefaults` **之后**改单个 solid 的值。
    /// 语料里 `damping` 在 0/39 个多 solid 文件里逐 solid 变化，
    /// 所以这项通常为 `None`，但接口要留。
    pub damping: Option<f32>,
    /// `"rotdamping"` 的本 solid 覆盖。`None` = 用 [`PhyParams::rot_damping`]。
    ///
    /// ⚠️ **这一项在语料里真的会变**：14/39 个多 solid 文件
    /// （boomer / hulk / hunter 等 ragdoll）里各 solid 互不相同。
    pub rot_damping: Option<f32>,
    /// `"inertia"` 的本 solid 覆盖。`None` = 用 [`PhyParams::inertia`]。
    ///
    /// 语料里 1/39 个多 solid 文件（`anim_common`）逐 solid 不同。
    pub inertia: Option<f32>,
}

impl PhySolid {
    /// 单 solid prop：不绑骨骼，名字取**碰撞 SMD 的 basename**。
    ///
    /// # ⚠️ 不是 `<model>_physbox`
    ///
    /// 官方 `ProcessSingleBody`（`collisionmodel.cpp:1563-1571`）：
    ///
    /// ```cpp
    /// char tmp[512];
    /// Q_FileBase( pmodel->filename, tmp, sizeof( tmp ) );  // ← 碰撞 SMD 的文件名
    /// pPhys->m_name = out;                                  //   去掉扩展名
    /// ```
    ///
    /// `pmodel` 是 **`$collisionmodel <smd>` 指向的那个 SMD**，
    /// 与 `$modelname` **无关**。受控实验（`docs/_probe/gen_phyname.js`）：
    ///
    /// | QC | 落盘 `"name"` |
    /// |---|---|
    /// | `$collisionmodel "physign_geo.smd"` | `"physign_geo"` |
    /// | `$collisionmodel "phyrot_phy.smd"` | `"phyrot_phy"` |
    /// | `$collisionmodel "msh1.smd"` | `"msh1"` |
    ///
    /// 语料 2459 个单 solid 里只有 **1142** 个恰好是 `<mdl>_physbox`
    /// —— 那只是因为它们的碰撞 SMD 正好叫 `<mdl>_physbox.smd`。
    pub fn prop(collision_smd_name: &str) -> Self {
        Self {
            bone_index: None,
            name: collision_smd_name.to_string(),
            parent: None,
            mass_bias: 1.0,
            damping: None,
            rot_damping: None,
            inertia: None,
            surface_prop: None,
        }
    }

    /// ragdoll 的一个骨骼组：绑到 `bone_index`。
    pub fn ragdoll(bone_index: u32, name: impl Into<String>, parent: Option<String>) -> Self {
        Self {
            bone_index: Some(bone_index),
            name: name.into(),
            parent,
            mass_bias: 1.0,
            damping: None,
            rot_damping: None,
            inertia: None,
            surface_prop: None,
        }
    }
}

/// 整个文件的写出参数。
#[derive(Debug, Clone, PartialEq)]
pub struct PhyParams<'a> {
    pub collision_text: Option<&'a str>,
    /// 模型名（不带扩展名）。
    pub model_name: &'a str,
    /// 配对 `.mdl` 的 `studiohdr_t::checksum`（`.mdl` 偏移 0x08）。
    ///
    /// 写错**不是致命错误**（引擎只给
    /// `WarningPhyFileChecksumDoesNotMatchMdlFileChecksum` 然后继续解析），
    /// 但会产生警告且部分工具链会拒绝，所以必须与 `.mdl` 逐位相同。
    pub checksum: u32,
    /// QC `$mass` 的等效总质量，写进 `editparams.totalmass`。
    ///
    /// ⚠️ **它同时是 `.mdl` 头部 `mass` 的值** —— 官方
    /// `write.cpp:2092` 是 `phdr->mass = GetCollisionModelMass();`，
    /// 而 `GetCollisionModelMass()` 返回 `g_JointedModel.m_totalMass`
    /// （`collisionmodel.cpp:2262-2265`）。也就是说 studiomdl 里
    /// **没有**独立的「模型质量」命令，`$mass` 只出现在
    /// `$collisionmodel {}` / `$collisionjoints {}` 块内
    /// （`collisionmodel.cpp:1781`）。
    ///
    /// 语料实测（`docs/_probe/probe_mass_law.js`，3333 个模型）：
    /// 有 `.phy` 的 2498 个里 **2483 个** `mdl.mass == phy.totalmass`，
    /// 其余 15 个的 `.phy` checksum 与 `.mdl` **不配对**（陈旧产物）；
    /// 没有 `.phy` 的 835 个里 **827 个** `mdl.mass == 1.0`。
    pub total_mass: f32,
    /// `"surfaceprop"`，如 `"wood_solid"` / `"flesh"` / `"default"`。
    pub surface_prop: &'a str,
    /// `"damping"`。
    pub damping: f32,
    /// `"rotdamping"`。
    pub rot_damping: f32,
    /// `"inertia"`。
    ///
    /// **与 `IVP_Compact_Surface::rotation_inertia` 无关**，这是 QC
    /// `$inertia` 的文本回显。L4D2 实测 prop 恒为 `1.0`、ragdoll 为 `2.0`/`10.0`。
    pub inertia: f32,
    /// 可选 `"drag"`。只有 `Some` 时才写（对应 studiomdl 的
    /// `m_dragCoefficient != -1`）。
    pub drag: Option<f32>,
    /// `editparams.rootname`。prop 写空串；ragdoll 写根骨骼名的小写。
    pub root_name: &'a str,
    /// 是否写 `editparams.concave "1"`。
    ///
    /// 实测：由 `$concave` 生成的 prop（`wood_fence` / `urban_puddle_model01a`）
    /// 有它，未经凸分解的（`l4d_gift` / `cone_helper`）没有。
    pub concave: bool,
    /// `rotation_inertia` 的缩放系数。见模块文档。
    ///
    /// `1.0` = 写真实的密度 1 惯性张量对角元；`0.0` = 写 `[0,0,0]`（最保守）。
    pub inertia_scale: f32,
    /// `$jointconstrain` 的全部记录（**跨 solid**，按骨骼名匹配）。
    ///
    /// 官方 `BuildRagdollConstraint` 对**每个** solid 遍历**整条**约束链，
    /// 只应用 `CollisionIndex(pList->m_pJointName) == ragdoll.childIndex`
    /// 的那些 —— 也就是说约束挂在**子** solid 上。
    pub constraints: Vec<JointConstraint>,
    /// `$animatedfriction`。`Some` ⟹ 写一个 `animatedfriction {}` 块。
    pub animated_friction: Option<AnimatedFriction>,
    /// `$noselfcollisions` ⟹ 写 `collisionrules { "selfcollisions" "0" }`。
    ///
    /// ⚠️ **与 `collision_pairs` 互斥**，且 `noself` **优先** ——
    /// 官方是 `if (m_noSelfCollisions) ... else if (m_pCollisionPairs)`
    /// （`collisionmodel.cpp:2422-2428`）。
    pub no_self_collisions: bool,
    /// `$jointcollide <a> <b>` 的配对（**骨骼名**，写出时转成 solid 下标）。
    ///
    /// 只有当 `no_self_collisions == false` 时才写。
    pub collision_pairs: Vec<(String, String)>,
    /// `$jointmerge <parent> <child>` 的**原始文本**（写出时原样回显）。
    ///
    /// 官方把 `"<parent>,<child>"` 直接 `strdup` 进 `m_mergeList`
    /// （`AddMergeCommand`，`collisionmodel.cpp:300-305`），写出时
    /// `Q_snprintf("%s,%s")` 拼回（`:2471`）—— **用的是 QC 里写的原始名字**，
    /// 不是解析后的骨骼名。实测 `$jointmerge "bone_mid" "bone_tip"` →
    /// `"jointmerge" "bone_mid,bone_tip"`。
    pub merge_list: Vec<(String, String)>,
    /// `$masscenter <x> <y> <z>`。
    ///
    /// 官方走 `CollideSetMassCenter`（`collisionmodel.cpp:361-363`），
    /// 它改的是**二进制** `IVP_Compact_Surface` 的质心，**不写文本段**。
    /// Forced mass-center overrides are not supported.
    pub mass_center: Option<[f32; 3]>,
    /// `$automass` ⟹ 总质量由**体积 × 密度**算出（`ComputeMass`）。
    ///
    /// 官方把 `m_totalMass` 置 `-1` 当哨兵（`SetAutoMass`，`:567-570`），
    /// `ComputeMass` 首句 `if (m_totalMass >= 0) return;` 于是才会真的算。
    /// 实测 `automass`（3 个 10³ 盒子、metal）：`totalmass = 5.309408`。
    pub auto_mass: bool,
}

impl<'a> PhyParams<'a> {
    /// 用模型名与 checksum 构造，其余字段取实测最常见的保守默认值。
    ///
    /// ⚠️ `total_mass` 的默认值是 **1.0**，不是 10.0。
    ///
    /// 源码依据：`CJointedModel::CJointedModel()` 里 `m_totalMass = 1.0`
    /// （`collisionmodel.cpp:252`），而 `ComputeMass()` 的**第一句**是
    /// `if ( m_totalMass >= 0 ) return;` —— `1.0 >= 0`，所以**直接返回**，
    /// 自动质量那条路只在显式 `$automass`（把值置 `-1`）时才会走。
    ///
    /// 语料印证：没有 `.phy` 的 835 个模型里 827 个 `mdl.mass == 1.0`
    /// （`probe_mass_law.js`）。
    pub fn new(model_name: &'a str, checksum: u32) -> Self {
        Self {
            collision_text: None,
            model_name,
            checksum,
            total_mass: 1.0,
            surface_prop: "default",
            damping: 0.0,
            rot_damping: 0.0,
            inertia: 1.0,
            drag: None,
            root_name: "",
            concave: false,
            inertia_scale: 1.0,
            constraints: Vec::new(),
            animated_friction: None,
            no_self_collisions: false,
            collision_pairs: Vec::new(),
            merge_list: Vec::new(),
            mass_center: None,
            auto_mass: false,
        }
    }
}

/// 焊接后的网格：`(顶点池, 索引面表)`。
///
/// 抽成别名是为了让 [`weld_smd_triangles`] 的签名可读
/// （否则触发 `clippy::type_complexity`）。
pub type WeldedMesh = (Vec<[f32; 3]>, Vec<[u32; 3]>);

/// 把一个 SMD 的三角形**焊接**成「顶点池 + 索引面表」。
///
/// # 为什么只做精确去重
///
/// SMD 的三角形顶点是**逐面独立**的（没有索引行，且 UV/法线接缝会让同一
/// 位置出现多次）。这里按「三个 float 的位模式完全相同」焊接 ——
/// **只做精确去重，不做 epsilon 合并**，否则会改动坐标、进而让
/// `upper_limit_radius` / `box_sizes` 与写入的点对不上。
///
/// 焊接后**退化的三角形**（三个下标不全不同）会被剔除：凸包计算不关心
/// 面表，但 VHACD 会关心。
///
/// 返回 `(顶点池, 面表)`；顶点数 < 4 时返回 [`PhyError::WeldTooFewPoints`]
/// （张不成凸包）。
pub fn weld_smd_triangles(smd: &crate::smd::Smd) -> Result<WeldedMesh, PhyError> {
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    let mut index_of: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
    let mut faces: Vec<[u32; 3]> = Vec::with_capacity(smd.triangles.len());
    for t in &smd.triangles {
        let mut f = [0u32; 3];
        for (k, v) in t.vertices.iter().enumerate() {
            let p = v.position;
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            f[k] = *index_of.entry(key).or_insert_with(|| {
                vertices.push(p);
                (vertices.len() - 1) as u32
            });
        }
        if f[0] != f[1] && f[1] != f[2] && f[0] != f[2] {
            faces.push(f);
        }
    }
    if vertices.len() < 4 {
        return Err(PhyError::WeldTooFewPoints {
            points: vertices.len(),
        });
    }
    Ok((vertices, faces))
}

/// 碰撞 SMD 的骨骼绑定信息：每个顶点的 `(骨骼名, 权重)` 列表。
///
/// 官方 `ConvertToWorldSpace`（`collisionmodel.cpp:708-738`）需要按
/// `localBoneweight[i]` 做**加权混合**，所以这里保留多骨骼绑定。
pub type SmdWeights = Vec<Vec<(String, f32)>>;

/// 从 SMD 抽出「每个顶点绑了哪些骨骼、权重多少」。
///
/// 骨骼用**名字**表示 —— 碰撞 SMD 与序列 SMD 的下标空间不一定相同，
/// 官方靠 `boneLocalToGlobal` 映射，本实现直接按名字对齐。
pub fn vertex_bone_weights(smd: &crate::smd::Smd) -> SmdWeights {
    let name_of: std::collections::HashMap<i32, &str> = smd
        .nodes
        .iter()
        .map(|n| (n.index, n.name.as_str()))
        .collect();
    smd.triangles
        .iter()
        .flat_map(|t| t.vertices.iter())
        .map(|v| {
            v.links
                .iter()
                .filter_map(|l| name_of.get(&l.bone).map(|n| ((*n).to_string(), l.weight)))
                .collect()
        })
        .collect()
}

/// 碰撞 SMD 每根骨骼的 `(参考姿态世界矩阵, 其逆)`，按骨骼名索引。
///
/// `boneToPose` 把「参考姿态下的顶点」搬到骨骼局部空间，
/// 逆矩阵把顶点搬回去。两者都预先算好，避免逐顶点求逆。
struct RestPose {
    world: std::collections::HashMap<String, crate::bone_math::Matrix3x4>,
    inverse: std::collections::HashMap<String, crate::bone_math::Matrix3x4>,
}

impl RestPose {
    fn of(smd: &crate::smd::Smd) -> Self {
        let nodes: HashMap<_, _> = smd.nodes.iter().map(|n| (n.index, n)).collect();
        let poses: HashMap<_, _> = smd
            .frames
            .first()
            .into_iter()
            .flat_map(|f| &f.poses)
            .map(|p| (p.bone, p))
            .collect();
        let mut w = HashMap::with_capacity(nodes.len());
        let mut i = HashMap::with_capacity(nodes.len());
        for node in &smd.nodes {
            // Node and pose IDs are independent of declaration order.
            let mut chain = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut current = Some(node);
            while let Some(n) = current {
                if !seen.insert(n.index) {
                    break;
                }
                chain.push(n);
                current = nodes.get(&n.parent).copied();
            }
            let mut world = crate::bone_math::IDENTITY;
            for n in chain.into_iter().rev() {
                let (position, rotation) = poses
                    .get(&n.index)
                    .map_or(([0.0; 3], [0.0; 3]), |p| (p.position, p.rotation));
                let local = crate::bone_math::local_transform(position, rotation);
                world = crate::bone_math::concat(&world, &local);
            }
            w.insert(node.name.clone(), world);
            i.insert(node.name.clone(), crate::bone_math::invert(&world));
        }
        Self {
            world: w,
            inverse: i,
        }
    }
}

/// 把一个顶点按骨骼绑定搬到世界空间（官方 `ConvertToWorldSpace` 的内层循环）。
///
/// ```c
/// worldVerts[i] = 0
/// for each (localBone, weight):
///     VectorITransform( vertex[i].position, boneToPose[localBone], tmp2 )  // → 骨骼局部
///     VectorTransform( tmp2, boneToWorld[globalBone], tmp )                // → 世界
///     worldVerts[i] += weight * tmp
/// ```
fn world_point(
    p: [f32; 3],
    links: &[(String, f32)],
    rest: &RestPose,
    pose_world: Option<&std::collections::HashMap<String, crate::bone_math::Matrix3x4>>,
) -> [f32; 3] {
    use crate::bone_math::IDENTITY;
    if links.is_empty() {
        return p;
    }
    let mut acc = [0.0f32; 3];
    for (name, weight) in links {
        // 序列姿态（世界）；没有序列时退化成参考姿态。
        let world = pose_world
            .and_then(|m| m.get(name.as_str()))
            .copied()
            .or_else(|| rest.world.get(name.as_str()).copied())
            .unwrap_or(IDENTITY);
        // 顶点从参考姿态搬到骨骼局部空间。
        //
        // ⚠️ 这一步不能省。碰撞 SMD 的顶点是**它自己参考姿态**下的，
        // 而 `boneToWorld` 是**序列姿态**下的；两者不同（mikuw 的序列把
        // bone 0 转了 90°）时直接乘会整体偏掉一个平移。
        // 实测漏掉它时 mikuw 命中 0/40，补上后 38/40。
        let local = match rest.inverse.get(name.as_str()) {
            Some(inv) => transform_point(inv, p),
            None => p,
        };
        let w = transform_point(&world, local);
        acc[0] += weight * w[0];
        acc[1] += weight * w[1];
        acc[2] += weight * w[2];
    }
    acc
}

/// 把碰撞 SMD 的顶点搬到**世界空间**（官方 `ConvertToWorldSpace`）。
///
/// # 官方算法（`collisionmodel.cpp:708-738`）
///
/// ```c
/// CalcBoneTransforms( g_panimation[0], 0, boneToWorld );   // ← 第一个序列的第 0 帧
/// for each vertex i:
///     worldVerts[i] = 0
///     for each (localBone, weight) in localBoneweight[i]:
///         globalBone = boneLocalToGlobal[localBone]
///         ConcatTransforms( boneToPose[localBone], srcRealign[globalBone], boneToPose )
///         VectorITransform( vertex[i].position, boneToPose, tmp2 )
///         VectorTransform( tmp2, boneToWorld[globalBone], tmp )
///         worldVerts[i] += weight * tmp
/// ```
///
/// # 姿态取自**序列**，不是碰撞 SMD
///
/// 这一点是实测定案的，不是从源码读出来的：`docs/_probe/gen_phyrot.js`
/// 造了三个几何完全相同、只有**序列**骨骼姿态不同的模型，
/// 结果 `.phy` 三者互不相同而 `.vvd` 三者相同。
///
/// # 参数
///
/// * `smd` —— 碰撞 SMD（提供顶点与它自己的参考姿态）。
/// * `weights` —— 与 `smd.triangles` 展平后**同序**的骨骼绑定。
/// * `pose_world` —— 序列第 0 帧的世界矩阵，键是骨骼名。
///   `None` 表示「没有序列」⟹ 退化成碰撞 SMD 自己的参考姿态。
pub fn to_world_space(
    smd: &crate::smd::Smd,
    weights: &SmdWeights,
    pose_world: Option<&std::collections::HashMap<String, crate::bone_math::Matrix3x4>>,
) -> Vec<[f32; 3]> {
    let rest = RestPose::of(smd);
    let mut out = Vec::with_capacity(weights.len());
    let mut flat = 0usize;
    for t in &smd.triangles {
        for v in &t.vertices {
            let links = weights.get(flat).map(|w| w.as_slice()).unwrap_or(&[]);
            flat += 1;
            out.push(world_point(v.position, links, &rest, pose_world));
        }
    }
    out
}

/// 从「第一个序列第 0 帧」算出每根骨骼的世界矩阵，按**骨骼名**索引。
///
/// # 为什么是「第一个序列」
///
/// 官方 `ConvertToWorldSpace`（`collisionmodel.cpp:713`）写的是
/// `CalcBoneTransforms( g_panimation[0], 0, boneToWorld )` ——
/// `g_panimation[0]` 就是 QC 里**第一个 `$sequence`**。
/// 碰撞 SMD 自己的姿态**完全不参与**。
///
/// 这一点用受控实验定过案（`docs/_probe/gen_phyrot.js`）：造三个几何
/// 完全相同、只有序列姿态不同的模型，`.phy` 三者互不相同而 `.vvd` 三者相同。
///
/// `frames[0]` 的骨骼顺序按 `bones`（模型骨骼表）排列 ——
/// 与 `compile.rs` 的产出约定一致。
pub fn sequence_pose_world(
    bones: &[crate::model::Bone],
    frame: &[crate::smd::SmdPose],
) -> std::collections::HashMap<String, crate::bone_math::Matrix3x4> {
    let mut pos = Vec::with_capacity(bones.len());
    let mut rot = Vec::with_capacity(bones.len());
    let mut parents = Vec::with_capacity(bones.len());
    let index_of: std::collections::HashMap<&str, usize> = bones
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.as_str(), i))
        .collect();
    for (i, b) in bones.iter().enumerate() {
        match frame.get(i) {
            Some(p) => {
                pos.push(p.position);
                rot.push(p.rotation);
            }
            None => {
                // 该帧没给这根骨骼 ⟹ 用它的参考姿态。
                // `Bone::rotation` 是**角度**（与 QC 一致），要转成弧度。
                pos.push(b.position.unwrap_or([0.0; 3]));
                rot.push(
                    b.rotation
                        .map(|r| [r[0].to_radians(), r[1].to_radians(), r[2].to_radians()])
                        .unwrap_or([0.0; 3]),
                );
            }
        }
        parents.push(
            b.parent
                .as_deref()
                .and_then(|p| index_of.get(p).copied())
                .map(|v| v as i32)
                .unwrap_or(-1),
        );
    }
    let world = crate::bone_math::compute_world(&pos, &rot, &parents);
    bones
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), world[i]))
        .collect()
}

/// 把**去重后**的顶点池搬到世界空间（[`weld_smd_triangles`] 的配套）。
///
/// [`to_world_space`] 按「三角形展平顺序」遍历，而这里拿到的是去重后的
/// 顶点池。做法：为每个唯一顶点记住**首次出现**时的骨骼绑定
/// —— 焊接就是按位置精确相等做的，同位置顶点的绑定必然相同。
fn world_space_verts(
    smd: &crate::smd::Smd,
    unique: &[[f32; 3]],
    pose_world: Option<&std::collections::HashMap<String, crate::bone_math::Matrix3x4>>,
) -> Vec<[f32; 3]> {
    let all = vertex_bone_weights(smd);
    let rest = RestPose::of(smd);

    // 位置（按位）→ 首次出现的展平下标
    let mut first: std::collections::HashMap<[u32; 3], usize> = std::collections::HashMap::new();
    let mut flat = 0usize;
    for t in &smd.triangles {
        for v in &t.vertices {
            let key = [
                v.position[0].to_bits(),
                v.position[1].to_bits(),
                v.position[2].to_bits(),
            ];
            first.entry(key).or_insert(flat);
            flat += 1;
        }
    }

    unique
        .iter()
        .map(|p| {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let links = first
                .get(&key)
                .and_then(|&i| all.get(i))
                .map(|w| w.as_slice())
                .unwrap_or(&[]);
            world_point(*p, links, &rest, pose_world)
        })
        .collect()
}

/// 用 `matrix3x4_t` 变换一个点。
fn transform_point(m: &crate::bone_math::Matrix3x4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3],
        m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7],
        m[8] * p[0] + m[9] * p[1] + m[10] * p[2] + m[11],
    ]
}

/// 把**去重后**的顶点池搬到世界空间（`weld_smd_triangles` 的配套）。
/// `build_phy_from_smd` / `build_ragdoll_phy_from_smd` 的**身份参数**。
///
/// 把三个字符串收成一个结构体，既让调用点自解释（`ident.surface_prop`
/// 比第 4 个位置参数清楚），也避免 `clippy::too_many_arguments`。
#[derive(Debug, Clone, Copy)]
pub struct PhyIdentity<'a> {
    /// 模型名（**不带扩展名**）。用于诊断输出。
    pub model_name: &'a str,
    /// **碰撞 SMD 的 basename**（不带目录与扩展名）。
    ///
    /// 它是 prop 形态 `"name"` 的取值来源（官方 `Q_FileBase`，
    /// `collisionmodel.cpp:1566`）—— 见 [`PhySolid::prop`]。
    pub collision_smd_name: &'a str,
    /// `$surfaceprop`（模型头 `+0x134` 的那个值）。
    ///
    /// prop 形态下 `GetSurfaceProp` 必然回落到它（solid 名是碰撞 SMD 的
    /// basename，查不到任何骨骼）—— 实测 2498/2498 个 `.phy` 恒等。
    pub surface_prop: &'a str,
}

/// 从**单个 SMD** 直接产出 `.phy` 字节（`$collisionmodel` 的单 solid prop 形态）。
///
/// 这是 `mdlc build` 接入 PHY 的入口，也是 `mdlc phy` 无选项时的等价路径。
///
/// # 参数
///
/// * `smd` —— 碰撞几何的来源（官方是 `$collisionmodel` 指向的**单独 SMD**）。
/// * `ident` —— 三个身份字符串（模型名 / 碰撞 SMD 名 / surfaceprop），
///   见 [`PhyIdentity`]。
/// * `checksum` —— 必须与配对的 `.mdl` 逐位相同，否则引擎报警告。
/// * `mass` —— `$mass` 等效总质量，写进 `editparams.totalmass`。
///   **同时也要写进 `.mdl` 头部的 `mass`** —— 官方 `write.cpp:2092` 是
///   `phdr->mass = GetCollisionModelMass();`，两者是**同一个值**。
/// * `phys` —— `[physics]` 表（`$collisionmodel {}` 块的参数）。
/// * `pose_world` —— 模型**第一个序列第 0 帧**的骨骼世界矩阵（按骨骼名）。
///
///   官方 `ConvertToWorldSpace` 用的是 `g_panimation[0]`，即第一个序列 ——
///   **不是**碰撞 SMD 自己的姿态（已由 `docs/_probe/gen_phyrot.js` 的三模型
///   对照实验定案）。传 `None` 时退化成用碰撞 SMD 自己的参考姿态，
///   这时若两者不同就会偏掉。
pub fn build_phy_from_smd(
    smd: &crate::smd::Smd,
    ident: PhyIdentity<'_>,
    checksum: u32,
    mass: f32,
    phys: &crate::model::Physics,
    pose_world: Option<&std::collections::HashMap<String, crate::bone_math::Matrix3x4>>,
) -> Result<Vec<u8>, PhyError> {
    validate_collision_skeleton(smd)?;
    let PhyIdentity {
        model_name,
        collision_smd_name,
        surface_prop,
    } = ident;
    if smd.triangles.is_empty() {
        return Err(PhyError::NoTriangles);
    }

    let hulls: Vec<PhyHull> = if phys.concave {
        decompose_connected_components_with_limit(
            smd,
            pose_world,
            phys.max_convex_pieces.unwrap_or(MAX_CONVEX_PIECES),
        )?
    } else {
        let (vertices, _faces) = weld_smd_triangles(smd)?;
        let vertices = world_space_verts(smd, &vertices, pose_world);
        vec![
            PhyHull::from_points(&vertices)
                .map_err(|e| PhyError::HullFailed(Box::new(e)))?
                // 官方 `ConvexFromVerts` 的第二段：`BuildOuterHull(hull, 0.01)`。
                .compact_outer_hull(),
        ]
    };

    // `joint_overrides` / `constraints` 只对 ragdoll 有意义 —— 单 solid 的 prop
    // 路径没有「joint」概念（官方 `InitCollisionModel` 只在
    // `ProcessJointedModel` 里被调用）。写了却不生效是最难查的一类问题，
    // 所以**显式报错**而不是静默忽略。
    if !phys.joint_overrides.is_empty() {
        return Err(PhyError::JointOverridesOnProp {
            count: phys.joint_overrides.len(),
        });
    }
    if !phys.skip.is_empty() {
        return Err(PhyError::BadParameter {
            what: "physics.skip",
            detail: "jointskip requires collisionjoints".into(),
        });
    }
    if !phys.constraints.is_empty() {
        return Err(PhyError::ConstraintsOnProp {
            count: phys.constraints.len(),
        });
    }
    if !phys.collision_pairs.is_empty() || phys.no_self_collisions {
        return Err(PhyError::CollisionPairsOnProp);
    }
    if phys.auto_mass {
        return Err(PhyError::AutoMassUnsupported);
    }
    if phys.mass_center.is_some() {
        return Err(PhyError::BadParameter {
            what: "physics.mass_center",
            detail: "forced physics mass centers are not supported".into(),
        });
    }

    let mut params = PhyParams::new(model_name, checksum);
    params.collision_text = phys.collision_text.as_deref();
    params.total_mass = mass;
    // `surfaceprop` 从**模型头**取（`GetSurfaceProp` 在 prop 形态下必然
    // 回落到 `s_pDefaultSurfaceProp`，也就是 `$surfaceprop`）——
    // 实测 2498/2498 个 `.phy` 的 surfaceprop 恒等于 `.mdl` 头部 `+0x134`。
    params.surface_prop = surface_prop;
    // 只有真的走了 `$concave` 才写 `concave "1"`，与实测的 studiomdl 行为一致。
    params.concave = phys.concave;
    // 单 solid 的 prop 路径没有「joint」概念，所以只有全局默认值。
    params.damping = phys.damping.unwrap_or(0.0);
    params.rot_damping = phys.rot_damping.unwrap_or(0.0);
    params.inertia = phys.inertia.unwrap_or(1.0);
    if let Some(d) = phys.drag {
        params.drag = Some(d);
    }
    // `editparams.rootname`：**只有显式写了 `$rootbone` 才用**，
    // 缺省是空串（语料 39 个 ragdoll 里 13 个空）。
    if let Some(r) = &phys.root_bone {
        params.root_name = r;
    }
    let grouped = vec![hulls];
    // ⚠️ prop 形态的 `name` 是**碰撞 SMD 的 basename**，不是模型名 ——
    // 见 [`PhySolid::prop`] 的文档与受控实验 `gen_phyname.js`。
    let solids = vec![PhySolid::prop(collision_smd_name)];
    write_phy_multi(&grouped, &solids, &params).map_err(|e| PhyError::WriteFailed(Box::new(e)))
}

// ---------------------------------------------------------------------------
// `$concave`：官方的**连通分量分解**
// ---------------------------------------------------------------------------

/// 官方焊接判据的法线阈值：`cos(2°)`（`studiomdl.cpp:6895`
/// `normal_blend = cos( DEG2RAD( 2.0 ));`）。
///
/// 只有法线夹角**小于 2°** 的共位顶点才会被焊成一个 —— 所以
/// **平面着色**的网格（每个面自带法线）在硬边处焊不上。
fn normal_blend() -> f32 {
    (2.0f32).to_radians().cos()
}

/// 官方 `BuildVertWeldTable`（`collisionmodel.cpp:945-966`）。
///
/// 对每个顶点 `i`，在 `j < i` 里找**第一个**满足
/// 「位置**完全相同**（`VectorCompare` 是 `operator==`，逐分量 `==`）
/// **且** `dot(normal_j, normal_i) > normal_blend`」的 `j`，
/// 命中则 `weld[i] = j`，否则 `weld[i] = i`。
///
/// ⚠️ 位置比较是**精确相等**，不是 epsilon 近似 —— 与
/// [`weld_smd_triangles`] 的按位去重口径一致。
fn build_weld_table(verts: &[([f32; 3], [f32; 3])]) -> Vec<u32> {
    let blend = normal_blend();
    let mut weld: Vec<u32> = (0..verts.len() as u32).collect();
    for (i, &(pi, ni)) in verts.iter().enumerate() {
        for (j, &(pj, nj)) in verts.iter().enumerate().take(i) {
            if pi != pj {
                continue;
            }
            let d = ni[0] * nj[0] + ni[1] * nj[1] + ni[2] * nj[2];
            if d > blend {
                weld[i] = j as u32;
                break;
            }
        }
    }
    weld
}

/// 官方 `MarkConnectedMeshes`（`collisionmodel.cpp:975-1050`）。
///
/// 并查集式地把「通过**焊接后**的共享顶点相连」的面归到同一组，
/// 组的 ID 是该连通分量里**最小的面号**。
///
/// 被焊掉的顶点（`weld[i] != i`）直接标 `-1`（已处理），
/// 所以只有**代表元**参与分组。
///
/// 返回值：每个顶点的组 ID；`-1` 表示「已被合并，跳过」。
fn mark_connected_meshes(weld: &[u32], faces: &[[u32; 3]]) -> Vec<i32> {
    let num_faces = faces.len();
    let sentinel = (num_faces + 1) as i32;
    let mut vert_id: Vec<i32> = (0..weld.len())
        .map(|i| if weld[i] as usize != i { -1 } else { sentinel })
        .collect();

    loop {
        let mut marked = 0usize;
        for (faceid, f) in faces.iter().enumerate() {
            let a = weld[f[0] as usize] as usize;
            let b = weld[f[1] as usize] as usize;
            let c = weld[f[2] as usize] as usize;
            // `newid = MIN(faceid, vertID[a], vertID[b], vertID[c])`
            let newid = (faceid as i32)
                .min(vert_id[a])
                .min(vert_id[b])
                .min(vert_id[c]);
            for &v in &[a, b, c] {
                if vert_id[v] != newid {
                    vert_id[v] = newid;
                    marked += 1;
                }
            }
        }
        if marked == 0 {
            break;
        }
    }
    vert_id
}

/// 官方 `IsApproximatelyPlanar`（`collisionmodel.cpp:1372-1422`）。
///
/// 少于 4 点直接算「平面」；否则找一个非退化的法线，把所有点投影上去，
/// 若厚度 `|max − min| > epsilon` 就说明是**三维**的。
///
/// 这个判据在 `$concave` 里起**否决**作用：任何一个分量被判成平面，
/// 官方就认定「模型没有 smoothing group」，**整份回退**成单凸包。
fn is_approximately_planar(points: &[[f32; 3]], epsilon: f32) -> bool {
    if points.len() < 4 {
        return true;
    }
    let mut v0 = 1usize;
    let mut v1 = 2usize;
    let mut normal = [0.0f32; 3];
    while v0 < points.len() && v1 < points.len() {
        let e0 = [
            points[v0][0] - points[0][0],
            points[v0][1] - points[0][1],
            points[v0][2] - points[0][2],
        ];
        let e1 = [
            points[v1][0] - points[0][0],
            points[v1][1] - points[0][1],
            points[v1][2] - points[0][2],
        ];
        normal = [
            e0[1] * e1[2] - e0[2] * e1[1],
            e0[2] * e1[0] - e0[0] * e1[2],
            e0[0] * e1[1] - e0[1] * e1[0],
        ];
        let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if len > 0.001 {
            break;
        }
        let e0_len = (e0[0] * e0[0] + e0[1] * e0[1] + e0[2] * e0[2]).sqrt();
        if e0_len < 0.001 {
            v0 += 1;
            v1 += 1;
        } else {
            v1 += 1;
        }
    }
    let dot = |p: &[f32; 3]| p[0] * normal[0] + p[1] * normal[1] + p[2] * normal[2];
    let mut min_d = dot(&points[0]);
    let mut max_d = min_d;
    for p in points {
        let d = dot(p);
        if d < min_d {
            min_d = d;
        } else if d > max_d {
            max_d = d;
        }
        if (max_d - min_d).abs() > epsilon {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// `$collisionjoints`：ragdoll（每骨骼一个 solid）
// ---------------------------------------------------------------------------

/// 一个 ragdoll solid 的分组结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RagdollGroup {
    /// 骨骼在 SMD 里的下标。
    pub bone: u32,
    /// 骨骼名（原样，不做大小写变换）。
    pub name: String,
    /// **修正后**的父骨骼名。
    ///
    /// 不是骨骼的直接父 —— 而是沿骨骼链上溯找到的第一个
    /// 「也在碰撞列表里」的祖先（官方 `FixParent`）。
    pub parent: Option<String>,
    /// 该组的面（下标指向焊接后的面表）。
    pub faces: Vec<u32>,
}

/// 按**骨骼**把碰撞网格分组（官方 `ProcessJointedModel` 的前半）。
///
/// # 判据（`FaceHasVertOnBone`，`collisionmodel.cpp:778-822`）
///
/// 一个面只要**任一顶点**的**任一** `links[].bone` 命中该骨骼，整面归它。
/// 这是**面级**归属 —— 不是按顶点分，所以相邻骨骼会共享顶点。
///
/// # 父的修正（`FixParent`，`collisionmodel.cpp:1130-1151`）
///
/// 沿骨骼链**上溯**，返回第一个在分组集合里的祖先名；找不到则 `None`。
/// 语料印证：`anim_common` 的 `Spine2` 直接父是 `Spine1`（无碰撞几何），
/// `.phy` 里写的是 `Spine`。
///
/// # `$jointmerge` 的骨骼归并（`m_bonemap`）
///
/// `merged` 是 `子骨骼下标 → 归并目标骨骼下标`。官方
/// `MergeBones`（`collisionmodel.cpp:307-327`）把 `m_bonemap[child]` 指向
/// **父所在并查集的根**，随后：
///
/// * `ShouldProcessBone`（`:330-338`）只在 `m_bonemap[i] == i` 时返回 true
///   ⟹ **子骨骼不再单独成 solid**；
/// * `RemapBone`（`:355`）返回 `m_bonemap[i]`
///   ⟹ `FaceHasVertOnBone` 查父骨骼时，**权重落在子骨骼上的面也算命中**
///   ⟹ 子的碰撞几何**并入**父。
///
/// 实测（受控实验 `merge`）：`$jointmerge "bone_mid" "bone_tip"`
/// ⟹ solid 数 **3 → 2**，且 `bone_mid` 的体积变成两个盒子之和。
///
/// # 顺序
///
/// Parents precede children regardless of SMD IDs or declaration order.
pub fn group_by_bone(smd: &crate::smd::Smd) -> Vec<RagdollGroup> {
    group_by_bone_merged(smd, &std::collections::HashMap::new())
}

/// 若把 `child → parent` 加进 `merged` 会不会形成环。
///
/// 官方 `MergeBones`（`collisionmodel.cpp:312-324`）靠
/// `safety > m_pModel->numbones` 跳出死循环 —— 那是**静默放弃**。
/// mdlc 选择**显式报错**：环会让归并结果依赖遍历顺序，是用户写错了。
fn would_cycle(
    merged: &std::collections::HashMap<i32, i32>,
    parent: i32,
    child: i32,
    limit: usize,
) -> bool {
    // 从 parent 沿现有归并链上溯，若能走到 child ⟹ 加这条边就成环。
    let mut cur = parent;
    let mut guard = 0usize;
    loop {
        if cur == child {
            return true;
        }
        match merged.get(&cur) {
            Some(&next) if next != cur && guard <= limit => {
                cur = next;
                guard += 1;
            }
            _ => return false,
        }
    }
}

/// [`group_by_bone`] 的带归并版本。`merged` 是 `子骨骼下标 → 目标骨骼下标`。
pub fn group_by_bone_merged(
    smd: &crate::smd::Smd,
    merged: &std::collections::HashMap<i32, i32>,
) -> Vec<RagdollGroup> {
    // 骨骼下标 → 名 / 父下标
    let mut name_of: std::collections::HashMap<i32, String> = std::collections::HashMap::new();
    let mut parent_of: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
    for n in &smd.nodes {
        name_of.insert(n.index, n.name.clone());
        parent_of.insert(n.index, n.parent);
    }

    // 归并：把 `child → target` 链解析到终点（防 `a→b→c` 只走一步）。
    // 官方 `MergeBones` 里那段 `while (m_bonemap[map] != map)` 就是干这个，
    // 并且带 `safety > numbones` 的死循环保护 —— 这里同样加上。
    let resolve = |mut b: i32| -> i32 {
        let mut guard = 0usize;
        while let Some(&t) = merged.get(&b) {
            if t == b || guard > smd.nodes.len() {
                break;
            }
            b = t;
            guard += 1;
        }
        b
    };

    // 每个面的归属：面只要有一个顶点绑到该骨骼就算它的。
    // ⚠️ 归属前先过一遍 `resolve` —— 这就是 `RemapBone` 的语义。
    let mut by_bone: std::collections::BTreeMap<i32, Vec<u32>> = std::collections::BTreeMap::new();
    for (fi, t) in smd.triangles.iter().enumerate() {
        let mut owners: Vec<i32> = Vec::new();
        for v in &t.vertices {
            for l in &v.links {
                let b = resolve(l.bone);
                if !owners.contains(&b) {
                    owners.push(b);
                }
            }
        }
        for b in owners {
            by_bone.entry(b).or_default().push(fi as u32);
        }
    }

    // 分组集合（用于 `FixParent` 的「是否在列表里」判断）。
    let in_list: std::collections::HashSet<i32> = by_bone.keys().copied().collect();

    let mut out = Vec::with_capacity(by_bone.len());
    for (bone, faces) in by_bone {
        // `FixParent`：沿链上溯到第一个在列表里的祖先。
        let mut parent: Option<String> = None;
        let mut cur = parent_of.get(&bone).copied().unwrap_or(-1);
        let mut traversed = 0;
        while cur >= 0 && traversed < smd.nodes.len() {
            traversed += 1;
            if in_list.contains(&cur) {
                parent = name_of.get(&cur).cloned();
                break;
            }
            cur = parent_of.get(&cur).copied().unwrap_or(-1);
        }
        out.push(RagdollGroup {
            bone: bone as u32,
            name: name_of
                .get(&bone)
                .cloned()
                .unwrap_or_else(|| format!("bone{bone}")),
            parent,
            faces,
        });
    }
    out.sort_by_cached_key(|group| {
        let mut depth = 0;
        let mut current = parent_of.get(&(group.bone as i32)).copied().unwrap_or(-1);
        while current >= 0 && depth < smd.nodes.len() {
            depth += 1;
            current = parent_of.get(&current).copied().unwrap_or(-1);
        }
        depth
    });
    out
}

fn ragdoll_merge_map(
    smd: &crate::smd::Smd,
    phys: &crate::model::Physics,
) -> Result<HashMap<i32, i32>, PhyError> {
    // ---- `$jointmerge`：先把骨骼归并表建出来（官方 `m_bonemap`） ----
    //
    // `MergeBones(parent, child)` 把 `m_bonemap[child]` 指向父所在并查集的根。
    // 效果：子骨骼不再单独成 solid，且权重落在子骨骼上的面**并入**父。
    // 键是**骨骼名**（`$jointmerge <父> <子>`），落盘时用原始名字。
    let mut merged: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
    if !phys.merge.is_empty() {
        let idx_of: std::collections::HashMap<String, i32> = smd
            .nodes
            .iter()
            .map(|n| (n.name.to_ascii_lowercase(), n.index))
            .collect();
        for (i, m) in phys.merge.iter().enumerate() {
            let (Some(&parent), Some(&child)) = (
                idx_of.get(&m.a.to_ascii_lowercase()),
                idx_of.get(&m.b.to_ascii_lowercase()),
            ) else {
                let mut v: Vec<&str> = smd.nodes.iter().map(|n| n.name.as_str()).collect();
                v.sort_unstable();
                return Err(PhyError::MergeBoneUnknown {
                    index: i,
                    a: m.a.clone(),
                    b: m.b.clone(),
                    available: v.join(", "),
                });
            };
            if parent == child {
                continue;
            }
            // 官方 `MergeBones` 里带 `safety > numbones` 的死循环保护，
            // 这里也要防「互相归并」造成的环。
            if would_cycle(&merged, parent, child, smd.nodes.len()) {
                return Err(PhyError::MergeCycle {
                    index: i,
                    a: m.a.clone(),
                    b: m.b.clone(),
                });
            }
            merged.insert(child, parent);
        }
    }

    Ok(merged)
}

fn ragdoll_groups(
    smd: &crate::smd::Smd,
    phys: &crate::model::Physics,
) -> Result<Vec<RagdollGroup>, PhyError> {
    validate_collision_skeleton(smd)?;
    for name in &phys.skip {
        if !smd.nodes.iter().any(|n| n.name.eq_ignore_ascii_case(name)) {
            return Err(PhyError::BadParameter {
                what: "physics.skip",
                detail: format!("unknown collision bone {name:?}"),
            });
        }
    }
    let merged = ragdoll_merge_map(smd, phys)?;
    Ok(group_by_bone_merged(smd, &merged)
        .into_iter()
        .filter(|g| !phys.skip.iter().any(|n| n.eq_ignore_ascii_case(&g.name)))
        .collect())
}

fn validate_collision_skeleton(smd: &crate::smd::Smd) -> Result<(), PhyError> {
    let invalid = |detail: String| PhyError::BadParameter {
        what: "collision skeleton",
        detail,
    };
    let mut nodes = HashMap::new();
    let mut names = std::collections::HashSet::new();
    for node in &smd.nodes {
        if node.index < 0
            || nodes.insert(node.index, node).is_some()
            || !names.insert(node.name.to_ascii_lowercase())
        {
            return Err(invalid(format!(
                "invalid or duplicate collision bone {:?}",
                node.name
            )));
        }
    }
    for node in &smd.nodes {
        let mut seen = std::collections::HashSet::new();
        let mut current = node;
        loop {
            if !seen.insert(current.index) {
                return Err(invalid(format!(
                    "collision bone {:?} has cyclic ancestry",
                    node.name
                )));
            }
            if current.parent == -1 {
                break;
            }
            current = nodes.get(&current.parent).copied().ok_or_else(|| {
                invalid(format!(
                    "collision bone {:?} has unknown parent {}",
                    current.name, current.parent
                ))
            })?;
        }
    }
    for frame in &smd.frames {
        let mut seen = std::collections::HashSet::new();
        for pose in &frame.poses {
            if !nodes.contains_key(&pose.bone)
                || !seen.insert(pose.bone)
                || !pose
                    .position
                    .iter()
                    .chain(&pose.rotation)
                    .all(|v| v.is_finite())
            {
                return Err(invalid(format!(
                    "invalid collision pose for bone {}",
                    pose.bone
                )));
            }
        }
    }
    for triangle in &smd.triangles {
        for vertex in &triangle.vertices {
            if !vertex.position.iter().all(|v| v.is_finite()) {
                return Err(invalid(
                    "collision geometry has nonfinite coordinates".into(),
                ));
            }
            for link in &vertex.links {
                if !nodes.contains_key(&link.bone) || !link.weight.is_finite() || link.weight < 0.0
                {
                    return Err(invalid(format!(
                        "invalid collision vertex binding to bone {}",
                        link.bone
                    )));
                }
            }
        }
    }
    Ok(())
}

fn ragdoll_hull(smd: &crate::smd::Smd, group: &RagdollGroup, rest: &RestPose) -> Option<PhyHull> {
    let mut vertices = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &face in &group.faces {
        for v in &smd.triangles[face as usize].vertices {
            if seen.insert(v.position.map(f32::to_bits)) {
                // Merged geometry uses the retained bone's local rest space.
                let inverse = rest
                    .inverse
                    .get(&group.name)
                    .expect("validated collision group has a rest transform");
                let local = transform_point(inverse, v.position);
                vertices.push(local);
            }
        }
    }
    if vertices.len() < 4 {
        return None;
    }
    PhyHull::from_points(&vertices)
        .ok()
        .map(|h| h.compact_outer_hull())
}

fn ragdoll_hulls(
    smd: &crate::smd::Smd,
    group: &RagdollGroup,
    rest: &RestPose,
    physics: &crate::model::Physics,
) -> Result<Vec<PhyHull>, PhyError> {
    if !physics.concave_per_joint {
        return Ok(ragdoll_hull(smd, group, rest).into_iter().collect());
    }
    let inverse = rest
        .inverse
        .get(&group.name)
        .expect("validated collision group has a rest transform");
    let triangles = group
        .faces
        .iter()
        .map(|&face| {
            let mut triangle = smd.triangles[face as usize].clone();
            for vertex in &mut triangle.vertices {
                vertex.position = transform_point(inverse, vertex.position);
                let n = vertex.normal;
                vertex.normal = [
                    inverse[0] * n[0] + inverse[1] * n[1] + inverse[2] * n[2],
                    inverse[4] * n[0] + inverse[5] * n[1] + inverse[6] * n[2],
                    inverse[8] * n[0] + inverse[9] * n[1] + inverse[10] * n[2],
                ];
                vertex.parent_bone = 0;
                vertex.links = vec![crate::smd::SmdBoneLink {
                    bone: 0,
                    weight: 1.0,
                }];
            }
            triangle
        })
        .collect();
    let local = crate::smd::Smd {
        version: 1,
        nodes: vec![crate::smd::SmdNode {
            index: 0,
            name: group.name.clone(),
            parent: -1,
        }],
        frames: Vec::new(),
        triangles,
    };
    decompose_connected_components_with_limit(
        &local,
        None,
        physics.max_convex_pieces.unwrap_or(MAX_CONVEX_PIECES),
    )
}

/// 从 SMD 产出 **ragdoll** 形态的 `.phy`（官方 `$collisionjoints`）。
///
/// 每根「有碰撞几何」的骨骼一个 solid，`client_data = 骨骼下标 + 1`，
/// `parent` 经 `FixParent` 修正，`rootname` 取 `$rootbone`。
///
/// 骨骼**没有**碰撞几何时官方直接跳过（`if (vertCount)`），
/// 所以 solid 数可能少于骨骼数。
///
/// # 逐 joint 参数
///
/// `$jointdamping` / `$jointrotdamping` / `$jointinertia` / `$jointmassbias`
/// 作用于**单个** joint（`collisionmodel.cpp:658-692`），键是**骨骼名**。
/// 语料实测这些覆盖真的会用到（`rotdamping` 在 14/39 个多 solid 文件里
/// 逐 solid 不同），所以这里逐 solid 解析后写进 [`PhySolid`]。
pub fn build_ragdoll_phy_from_smd(
    smd: &crate::smd::Smd,
    ident: PhyIdentity<'_>,
    checksum: u32,
    mass: f32,
    phys: &crate::model::Physics,
) -> Result<Vec<u8>, PhyError> {
    build_ragdoll_phy(smd, ident, checksum, mass, phys, None)
}

/// Build a ragdoll with collision solids bound by name to the final model skeleton.
pub fn build_ragdoll_phy_for_model(
    smd: &crate::smd::Smd,
    ident: PhyIdentity<'_>,
    checksum: u32,
    mass: f32,
    phys: &crate::model::Physics,
    bones: &[crate::model::Bone],
) -> Result<Vec<u8>, PhyError> {
    build_ragdoll_phy(smd, ident, checksum, mass, phys, Some(bones))
}

fn build_ragdoll_phy(
    smd: &crate::smd::Smd,
    ident: PhyIdentity<'_>,
    checksum: u32,
    mass: f32,
    phys: &crate::model::Physics,
    bones: Option<&[crate::model::Bone]>,
) -> Result<Vec<u8>, PhyError> {
    let PhyIdentity {
        model_name,
        surface_prop,
        ..
    } = ident;
    if smd.triangles.is_empty() {
        return Err(PhyError::NoTriangles);
    }

    let groups = ragdoll_groups(smd, phys)?;
    let rest = RestPose::of(smd);
    if groups.is_empty() {
        return Err(PhyError::NoBoneGeometry);
    }

    // 每组：把该组的面重映射到紧凑顶点池，再算凸包。
    let mut grouped: Vec<Vec<PhyHull>> = Vec::with_capacity(groups.len());
    let mut solids: Vec<PhySolid> = Vec::with_capacity(groups.len());
    let mut kept: Vec<&RagdollGroup> = Vec::with_capacity(groups.len());

    for g in &groups {
        let hulls = ragdoll_hulls(smd, g, &rest, phys)?;
        if hulls.is_empty() {
            continue;
        }
        grouped.push(hulls);
        // 逐 joint 参数：全局默认 + 该骨骼的覆盖（官方顺序：先默认后覆盖）。
        let override_name = phys
            .joint_overrides
            .iter()
            .find(|o| o.bone.eq_ignore_ascii_case(&g.name))
            .map_or(g.name.as_str(), |o| o.bone.as_str());
        let (damping, rot_damping, inertia, mass_bias) = phys.resolve_joint(override_name);
        let bone_index = match bones {
            Some(bones) => render_bone_index(bones, &g.name)? as u32,
            None => g.bone,
        };
        let mut s = PhySolid::ragdoll(bone_index, g.name.clone(), g.parent.clone());
        if let Some(bones) = bones {
            s.surface_prop = inherited_surface_prop(bones, &g.name).map(str::to_owned);
        }
        // 只在**真的与全局默认不同**时才记成 per-solid 覆盖 ——
        // 这样 `PhySolid` 的 `None` 语义保持「继承默认」，写出的字节不变。
        if damping != phys.damping.unwrap_or(0.0) {
            s.damping = Some(damping);
        }
        if rot_damping != phys.rot_damping.unwrap_or(0.0) {
            s.rot_damping = Some(rot_damping);
        }
        if inertia != phys.inertia.unwrap_or(1.0) {
            s.inertia = Some(inertia);
        }
        s.mass_bias = mass_bias;
        solids.push(s);
        kept.push(g);
    }

    if grouped.is_empty() {
        return Err(PhyError::NoHullFromAnyBone);
    }

    // Validate against solids remaining after merging, skipping and hull filtering.
    for override_ in &phys.joint_overrides {
        if !solids
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(&override_.bone))
        {
            return Err(PhyError::JointOverrideUnknownBone {
                bone: override_.bone.clone(),
                available: solids
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }
    for constraint in &phys.constraints {
        validate_solid_reference(&solids, "physics.constraints", &constraint.bone)?;
    }
    if !phys.no_self_collisions {
        for pair in &phys.collision_pairs {
            validate_solid_reference(&solids, "physics.collision_pairs", &pair.a)?;
            validate_solid_reference(&solids, "physics.collision_pairs", &pair.b)?;
        }
    }

    // 父名要指向**实际保留下来**的 solid；`FixParent` 已保证父在列表里，
    // 但那个父可能因为点数不足被上面丢弃 —— 再上溯一次兜底。
    let kept_names: std::collections::HashSet<&str> =
        kept.iter().map(|g| g.name.as_str()).collect();
    let name_to_parent: std::collections::HashMap<&str, i32> = smd
        .nodes
        .iter()
        .map(|n| (n.name.as_str(), n.parent))
        .collect();
    let index_to_name: std::collections::HashMap<i32, &str> = smd
        .nodes
        .iter()
        .map(|n| (n.index, n.name.as_str()))
        .collect();
    for s in solids.iter_mut() {
        let mut cur = s.parent.clone();
        while let Some(p) = cur {
            if kept_names.contains(p.as_str()) {
                s.parent = Some(p);
                break;
            }
            cur = name_to_parent
                .get(p.as_str())
                .copied()
                .filter(|&i| i >= 0)
                .and_then(|i| index_to_name.get(&i))
                .map(|x| (*x).to_string());
            if cur.is_none() {
                s.parent = None;
            }
        }
    }

    let mut params = PhyParams::new(model_name, checksum);
    params.collision_text = phys.collision_text.as_deref();
    params.total_mass = mass;
    params.surface_prop = surface_prop;
    params.damping = phys.damping.unwrap_or(0.0);
    params.rot_damping = phys.rot_damping.unwrap_or(0.0);
    params.inertia = phys.inertia.unwrap_or(1.0);
    if let Some(d) = phys.drag {
        params.drag = Some(d);
    }
    // 关节约束 / 碰撞规则 / 动画摩擦 / jointmerge。
    params.constraints = phys.joint_constraints()?;
    params.animated_friction = phys.animated_friction();
    params.no_self_collisions = phys.no_self_collisions;
    params.collision_pairs = phys
        .collision_pairs
        .iter()
        .map(|p| (p.a.clone(), p.b.clone()))
        .collect();
    params.merge_list = phys
        .merge
        .iter()
        .map(|p| (p.a.clone(), p.b.clone()))
        .collect();
    if let Some(mc) = phys.mass_center {
        params.mass_center = Some(mc);
    }
    // `$automass` 需要材质密度表（`scripts/surfaceproperties_manifest.txt`），
    // mdlc 没有 —— **显式报错**而不是静默写一个错的总质量。
    if phys.auto_mass {
        return Err(PhyError::AutoMassUnsupported);
    }
    // ⚠️ **`rootname` 默认是空串**，不要拿根 solid 的名去填。
    //
    // 依据：`editparams.rootname` 写的是 `g_JointedModel.m_rootName`，
    // 而它**只**由 `$rootbone` 设置（`CCmd_JointRoot`，
    // `collisionmodel.cpp:1745-1749`）。没有 `$rootbone` 就是空。
    //
    // 受控实验（`rjd1`/`rjd2`，无 `$rootbone`）实测官方产物
    // `"rootname" ""`。语料 39 个 ragdoll 里 13 个也是空的。
    //
    // `$rootbone " "`（一个空格）是常见写法（mikuw 的 QC 就是），
    // 落盘就是那个空格 —— **不要 trim**。
    if let Some(r) = &phys.root_bone {
        params.root_name = r;
    }
    write_phy_multi(&grouped, &solids, &params).map_err(|e| PhyError::WriteFailed(Box::new(e)))
}

/// 官方 `$concave` 的**凸块数上限**（`collisionmodel.cpp:1535`
/// `if ( elements.Size() > 20 )` → 回退成单凸包）。
const MAX_CONVEX_PIECES: usize = 20;

fn validate_solid_reference(
    solids: &[PhySolid],
    what: &'static str,
    name: &str,
) -> Result<(), PhyError> {
    if solids.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
        return Ok(());
    }
    Err(PhyError::BadParameter {
        what,
        detail: format!(
            "joint {name:?} has no retained collision solid; remove its parameters or target a retained joint"
        ),
    })
}

fn render_bone_index(bones: &[crate::model::Bone], name: &str) -> Result<usize, PhyError> {
    bones
        .iter()
        .position(|b| b.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| PhyError::BadParameter {
            what: "collision skeleton",
            detail: format!("collision solid {name:?} has no matching model bone"),
        })
}

fn inherited_surface_prop<'a>(bones: &'a [crate::model::Bone], name: &str) -> Option<&'a str> {
    let mut current = bones.iter().find(|b| b.name.eq_ignore_ascii_case(name));
    for _ in 0..bones.len() {
        let bone = current?;
        if let Some(prop) = bone.surface_prop.as_deref() {
            return Some(prop);
        }
        current = bone
            .parent
            .as_deref()
            .and_then(|parent| bones.iter().find(|b| b.name.eq_ignore_ascii_case(parent)));
    }
    None
}

/// Map final model bones to the exact solid ordering used by ragdoll export.
pub fn physics_bone_table_for_model(
    smd: &crate::smd::Smd,
    bones: &[crate::model::Bone],
    phys: &crate::model::Physics,
) -> Result<Vec<i32>, PhyError> {
    if !phys.joints {
        return Ok(vec![0; bones.len()]);
    }
    let rest = RestPose::of(smd);
    let groups: Vec<_> = ragdoll_groups(smd, phys)?
        .into_iter()
        .filter(|g| ragdoll_hull(smd, g, &rest).is_some())
        .collect();
    let mut direct = vec![None; bones.len()];
    for (solid, group) in groups.iter().enumerate() {
        direct[render_bone_index(bones, &group.name)?] = Some(solid as i32);
    }
    // A merge may target a bone other than the child's direct ancestor.
    let merged = ragdoll_merge_map(smd, phys)?;
    for node in &smd.nodes {
        let mut target = node.index;
        for _ in 0..smd.nodes.len() {
            match merged.get(&target) {
                Some(&next) if next != target => target = next,
                _ => break,
            }
        }
        if target != node.index
            && let Some(solid) = groups.iter().position(|g| g.bone == target as u32)
            && let Some(index) = bones
                .iter()
                .position(|b| b.name.eq_ignore_ascii_case(&node.name))
        {
            direct[index] = Some(solid as i32);
        }
    }
    let mut result = Vec::with_capacity(bones.len());
    for index in 0..bones.len() {
        let mut current = Some(index);
        let mut solid = None;
        for _ in 0..bones.len() {
            let Some(i) = current else { break };
            if let Some(found) = direct[i] {
                solid = Some(found);
                break;
            }
            current = bones[i]
                .parent
                .as_deref()
                .and_then(|name| bones.iter().position(|b| b.name.eq_ignore_ascii_case(name)));
        }
        result.push(solid.unwrap_or(0));
    }
    Ok(result)
}

/// 算出每根骨骼的 `physicsbone`（`mstudiobone_t` `+0xAC`）。
///
/// # 语义（`collisionmodel.cpp:2141-2184`）
///
/// ```text
/// ① 先全置 -1
/// ② 碰撞列表里每个 solid → 该骨骼的 physicsBoneIndex = **solid 下标**
/// ③ 未置位的骨骼沿父链上溯，取第一个已置位的祖先的值
/// ④ 都找不到 → 0
/// ```
///
/// # 与 `build_ragdoll_phy_from_smd` 的**分组口径必须一致**
///
/// solid 下标就是 `group_by_bone()` 结果**经过同样的过滤之后**的序号：
/// 官方对「顶点数不足 4」或「张不成凸包」的骨骼**直接跳过**
/// （`if (vertCount)` / `ConvexFromVerts` 返回 NULL），被跳过的骨骼
/// **不占 solid 下标**。所以这里必须复刻同一套过滤，否则下标会错位。
///
/// # 返回值
///
/// `Some(vec)` 长度 = `bone_count`，`None` 表示**没有任何碰撞几何**
/// （此时官方留全 0）。
///
/// # 语料判据（`probe_physicsbone_ragdoll_split.js`）
///
/// 排除 18 个 checksum 不配对的陈旧产物后**完美二分**：
/// 多 solid **38/38 非平凡**、单 solid **2459/2459 全 0**。
/// 单 solid 时本函数天然给出全 0（只有一个 solid 下标 0），与语料一致。
pub fn physics_bone_table(
    smd: &crate::smd::Smd,
    bone_count: usize,
    bone_parents: &[i32],
) -> Option<Vec<i32>> {
    let groups = group_by_bone(smd);
    if groups.is_empty() {
        return None;
    }

    // 与 `build_ragdoll_phy_from_smd` **完全相同**的过滤。
    let mut kept: Vec<&RagdollGroup> = Vec::with_capacity(groups.len());
    for g in &groups {
        let mut verts: Vec<[f32; 3]> = Vec::new();
        let mut index_of: std::collections::HashMap<[u32; 3], u32> =
            std::collections::HashMap::new();
        for &fi in &g.faces {
            for v in &smd.triangles[fi as usize].vertices {
                let p = v.position;
                let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
                index_of.entry(key).or_insert_with(|| {
                    verts.push(p);
                    (verts.len() - 1) as u32
                });
            }
        }
        if verts.len() < 4 {
            continue;
        }
        if PhyHull::from_points(&verts).is_err() {
            continue;
        }
        kept.push(g);
    }
    if kept.is_empty() {
        return None;
    }

    // ② solid 下标 → 骨骼
    let mut pb = vec![-1i32; bone_count];
    for (solid_idx, g) in kept.iter().enumerate() {
        let b = g.bone as usize;
        if b < bone_count {
            pb[b] = solid_idx as i32;
        }
    }
    // ③ 沿父链上溯；④ 找不到 → 0
    for i in 0..bone_count {
        if pb[i] >= 0 {
            continue;
        }
        let mut cur = bone_parents.get(i).copied().unwrap_or(-1);
        let mut found = -1i32;
        while cur >= 0 {
            let c = cur as usize;
            if c >= bone_count {
                break;
            }
            if pb[c] >= 0 {
                found = pb[c];
                break;
            }
            cur = bone_parents.get(c).copied().unwrap_or(-1);
        }
        pb[i] = if found >= 0 { found } else { 0 };
    }
    Some(pb)
}

/// 官方 `$concave` 的分解：**连通分量分解**，不是体分解。
///
/// # ⚠️ 这与 VHACD 是**两种不同的算法**
///
/// 反编译 `ProcessSingleBody`（`studiomdl.exe` 的 `FUN_004055a0`）确认官方路径是：
///
/// ```text
/// $concave ⟹ BuildVertWeldTable   （位置相同 且 法线夹角 < 2° 才焊接）
///         ⟹ MarkConnectedMeshes   （按共享焊接顶点做并查集）
///         ⟹ 每个连通分量各算一个凸包 ConvexFromVerts
/// ```
///
/// 所以对**连通**的凹体（U 形、圆环面），官方 `$concave` 给出的就是
/// **整个网格的凸包** —— 凹处被**填平**，而**不是**被拆成多块。
/// 只有网格有**多个互不相连的壳**时才会产出多个凸块。
///
/// 这与 [`decompose_concave`]（VHACD 体逼近）**结果不同**，实测：
///
/// | 网格 | 官方 `$concave` | VHACD |
/// |---|---|---|
/// | 光滑圆环面（连通、凹） | **1** 块，体积 22728.12 | 12 块，体积 19343.18 |
/// | 两个分离光滑球 | **2** 块，体积 4020.83（= 单球的 2 倍） | — |
///
/// 因为官方是**填平**、VHACD 是**保留凹口**，两者不能互相替代 ——
/// 用 VHACD 冒充 `$concave` 会让玩家卡进本该实心的区域。
///
/// # 两条回退规则（都会退化成「整网格一个凸包」）
///
/// 1. **任一分量是平面** —— 官方认为模型没设 smoothing group，
///    报 `Bad collision model, check your smoothing groups` 并 `elements.Purge()`。
///    平面着色的立方体必然命中（角上三个面法线互相垂直，`dot = 0 < cos2°`，
///    焊不上 ⟹ 每个面各自成分量 ⟹ 每个分量都是平面）。
/// 2. **分量数 > 20** —— 报 `COSTLY COLLISION MODEL`。
///
/// 两种情况下产物里都写 `concave "1"`（`m_allowConcave` 仍然是 true），
/// 只是几何退化成单凸包 —— 与官方一致。
pub fn decompose_connected_components(
    smd: &crate::smd::Smd,
    pose_world: Option<&std::collections::HashMap<String, crate::bone_math::Matrix3x4>>,
) -> Result<Vec<PhyHull>, PhyError> {
    decompose_connected_components_with_limit(smd, pose_world, MAX_CONVEX_PIECES)
}

fn decompose_connected_components_with_limit(
    smd: &crate::smd::Smd,
    pose_world: Option<&std::collections::HashMap<String, crate::bone_math::Matrix3x4>>,
    max_pieces: usize,
) -> Result<Vec<PhyHull>, PhyError> {
    if !(1..=1024).contains(&max_pieces) {
        return Err(PhyError::BadParameter {
            what: "max_convex_pieces",
            detail: "convex piece limit must be between 1 and 1024".into(),
        });
    }
    // 展平成「逐面顶点」：SMD 的三角形顶点是逐面独立的。
    //
    // ⚠️ 位置要先搬到**世界空间**（官方 `ProcessSingleBody:1436`
    // `ConvertToWorldSpace` 发生在**一切**之前，焊接/连通性判定都在
    // 世界空间里做）。姿态取自序列第 0 帧。
    let mut verts: Vec<([f32; 3], [f32; 3])> = Vec::with_capacity(smd.triangles.len() * 3);
    let mut faces: Vec<[u32; 3]> = Vec::with_capacity(smd.triangles.len());
    let all_weights = vertex_bone_weights(smd);
    let rest = RestPose::of(smd);
    let mut flat = 0usize;
    for t in &smd.triangles {
        let base = verts.len() as u32;
        for v in &t.vertices {
            let links = all_weights.get(flat).map(|w| w.as_slice()).unwrap_or(&[]);
            flat += 1;
            let wp = world_point(v.position, links, &rest, pose_world);
            verts.push((wp, v.normal));
        }
        faces.push([base, base + 1, base + 2]);
    }
    if verts.len() < 4 {
        return Err(PhyError::TooFewVerticesForHull {
            points: verts.len(),
        });
    }

    let weld = build_weld_table(&verts);
    let vert_id = mark_connected_meshes(&weld, &faces);

    // 官方：`for i in 0..numvertices`，跳过 `vertID[i] < 0 || > numfaces`，
    // 收集所有 `vertID[j] == id` 的点（并把它们标 -1 防止重复）。
    let num_faces = faces.len() as i32;
    let mut seen: Vec<bool> = vert_id.iter().map(|&v| v < 0 || v > num_faces).collect();
    let mut groups: Vec<Vec<[f32; 3]>> = Vec::new();
    for i in 0..verts.len() {
        if seen[i] {
            continue;
        }
        let id = vert_id[i];
        let mut pts: Vec<[f32; 3]> = Vec::new();
        for j in i..verts.len() {
            if vert_id[j] == id {
                pts.push(verts[j].0);
                seen[j] = true;
            }
        }
        // 官方 `if (vertCount > 2)` 才尝试建凸包。
        if pts.len() > 2 {
            groups.push(pts);
        }
    }

    // 回退规则 1：任一分量是平面 ⟹ 整份退化成单凸包。
    let any_planar = groups.iter().any(|g| is_approximately_planar(g, 0.5));
    // 回退规则 2：分量数超上限。
    let too_many = groups.len() > max_pieces;

    if any_planar || too_many || groups.is_empty() {
        let all: Vec<[f32; 3]> = verts.iter().map(|v| v.0).collect();
        return Ok(vec![
            PhyHull::from_points(&all)
                .map_err(|e| PhyError::OverallHullFailed(Box::new(e)))?
                .compact_outer_hull(),
        ]);
    }

    // 每个分量一个凸包。退化分量（`ConvexFromVerts` 返回 NULL）官方直接丢弃。
    //
    // ⚠️ 官方 `ProcessSingleBody:1518` 用的是 `ConvexFromVerts` ——
    // 那**包含** `BuildOuterHull(·, 0.01)` 压实，所以每个分量也要压。
    let mut hulls = Vec::with_capacity(groups.len());
    for g in &groups {
        if let Ok(h) = PhyHull::from_points(g) {
            hulls.push(h.compact_outer_hull());
        }
    }
    if hulls.is_empty() {
        let all: Vec<[f32; 3]> = verts.iter().map(|v| v.0).collect();
        return Ok(vec![
            PhyHull::from_points(&all)
                .map_err(|e| PhyError::OverallHullFailed(Box::new(e)))?
                .compact_outer_hull(),
        ]);
    }
    Ok(hulls)
}

/// 自检通过后返回的布局摘要。
///
/// 单元测试用它断言 §10.1 的长度恒等式；CLI 用它打印人类可读的统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhyLayout {
    /// 文件总字节数。
    pub file_size: usize,
    /// `phyheader_t::solidCount`。
    pub solid_count: usize,
    /// text section 的起点（`= 16 + Σ(size_i + 4)`）。
    pub solids_end: usize,
    /// text section 的字节数（含结尾的 `0x00`）。
    pub text_size: usize,
    /// 每个 solid 的 `surfaceSize`。
    pub surface_sizes: Vec<i32>,
    /// 每个 solid 的树节点数。
    pub node_counts: Vec<usize>,
    /// 每个 solid 的 ledge 区域字节数（不含 `IVP_Compact_Surface` 的 48 字节）。
    pub ledge_region_sizes: Vec<usize>,
}

// ---------------------------------------------------------------------------
// 公开写出入口
// ---------------------------------------------------------------------------

/// 每个 solid 一个凸块，写出完整的 `.phy` 字节。
///
/// 这是最常用的入口：单 solid prop、以及"每根骨骼一个凸包"的 ragdoll
/// 都走它。`hulls[i]` 对应 `solids[i]`，顺序就是二进制里 solid 的顺序，
/// 也是 text section 里 `"index"` 的顺序。
///
/// 一个 solid 需要多个凸块时用 [`write_phy_multi`]。
///
/// # 质量分配（照抄 studiomdl `collisionmodel.cpp`）
///
/// ```text
/// volume     = Σ 各 solid 体积（<= 0 时置 1）
/// solid.mass = (solid.volume × massbias / volume) × totalmass
/// if solid.mass < 1.0 { solid.mass = 1.0 }      // 下限钳制
/// ```
///
/// 单 solid 时这退化成 `mass == totalmass`，与真实文件一致
/// （`cone_helper` 的 `mass` 与 `totalmass` 都是 19.422348）。
///
/// # 返回值
///
/// 写出的字节**一定**已经过 [`check_invariants`] 全量校验；任何布局回归
/// 都会在这里炸掉，而不是等到引擎加载时才发现。
pub fn write_phy(
    hulls: &[PhyHull],
    solids: &[PhySolid],
    params: &PhyParams<'_>,
) -> Result<Vec<u8>, PhyError> {
    let grouped: Vec<Vec<PhyHull>> = hulls.iter().map(|h| vec![h.clone()]).collect();
    write_phy_multi(&grouped, solids, params)
}

/// 每个 solid 可以由**多个凸块**拼成，写出完整的 `.phy` 字节。
///
/// 多凸块时一个 solid 会写成递归 ledgetree（每个叶子挂一个 hull），
/// 所有凸块共用一份点数组。单个凸块时退化成单叶子节点树
/// （`offset_right_node = 0`），即 L4D2 里 621/774 个 solid 的形态。
///
/// 输入契约、质量分配与自检行为都与 [`write_phy`] 相同。
pub fn write_phy_multi(
    hulls: &[Vec<PhyHull>],
    solids: &[PhySolid],
    params: &PhyParams<'_>,
) -> Result<Vec<u8>, PhyError> {
    if params.mass_center.is_some() {
        return Err(PhyError::BadParameter {
            what: "mass_center",
            detail: "forced physics mass centers are not supported".into(),
        });
    }
    if params.auto_mass {
        return Err(PhyError::AutoMassUnsupported);
    }
    if hulls.is_empty() {
        return Err(PhyError::BadParameter {
            what: "hulls",
            detail: crate::tr("at least one solid is required"),
        });
    }
    if hulls.len() != solids.len() {
        return Err(PhyError::BadParameter {
            what: "solids",
            detail: crate::tr_fmt!(
                "%{hulls} solid hull groups but %{solids} solid parameters",
                hulls = hulls.len(),
                solids = solids.len()
            ),
        });
    }
    if !params.total_mass.is_finite() || params.total_mass <= 0.0 {
        return Err(PhyError::BadParameter {
            what: "total_mass",
            detail: crate::tr_fmt!(
                "must be a positive finite number, but got %{value}",
                value = params.total_mass
            ),
        });
    }
    if !params.inertia_scale.is_finite() || params.inertia_scale < 0.0 {
        return Err(PhyError::BadParameter {
            what: "inertia_scale",
            detail: crate::tr_fmt!(
                "must be a non-negative finite number, but got %{value}",
                value = params.inertia_scale
            ),
        });
    }
    for (what, value) in [
        ("damping", Some(params.damping)),
        ("rot_damping", Some(params.rot_damping)),
        ("inertia", Some(params.inertia)),
        ("drag", params.drag),
    ] {
        if let Some(value) = value {
            validate_nonnegative_number(what, value)?;
        }
    }
    for constraint in &params.constraints {
        if constraint.axis > 2
            || !constraint.min.is_finite()
            || !constraint.max.is_finite()
            || constraint.min > constraint.max
        {
            return Err(PhyError::BadParameter {
                what: "constraints",
                detail: format!("invalid axis or angular limits for {:?}", constraint.bone),
            });
        }
        validate_nonnegative_number("constraint friction", constraint.friction)?;
    }
    if let Some(friction) = &params.animated_friction {
        if friction.min < 0 || friction.max < friction.min {
            return Err(PhyError::BadParameter {
                what: "animated_friction",
                detail: "friction bounds must be nonnegative and ordered".into(),
            });
        }
        for value in [friction.time_in, friction.time_out, friction.time_hold] {
            validate_nonnegative_number("animated friction time", value)?;
        }
    }
    // text section 里任何字段混进 NUL 都会把它截断，引擎只会读到半截。
    for (i, s) in solids.iter().enumerate() {
        for (what, value) in [
            ("solid mass_bias", Some(s.mass_bias)),
            ("solid damping", s.damping),
            ("solid rot_damping", s.rot_damping),
            ("solid inertia", s.inertia),
        ] {
            if let Some(value) = value {
                validate_nonnegative_number(what, value)?;
            }
        }
        for (what, v) in [
            ("name", s.name.as_str()),
            ("parent", s.parent.as_deref().unwrap_or("")),
            ("surfaceprop", s.surface_prop.as_deref().unwrap_or("")),
        ] {
            if v.as_bytes().contains(&0) {
                return Err(PhyError::BadParameter {
                    what: "solids[].name/parent/surfaceprop",
                    detail: crate::tr_fmt!(
                        "the %{what} of solid[%{solid}] contains a NUL byte",
                        what = what,
                        solid = i
                    ),
                });
            }
        }
    }

    // ---- 0. 逐个 solid 预处理几何 ----
    let prepared: Vec<PreparedSolid> = hulls
        .iter()
        .enumerate()
        .map(|(i, hs)| prepare_solid(i, hs, params.inertia_scale))
        .collect::<Result<_, _>>()?;

    // ---- 1. 逐个 solid 组装 IVP 负载 ----
    let bodies: Vec<Vec<u8>> = prepared
        .iter()
        .enumerate()
        .map(|(i, p)| build_solid_body(i, p, solids[i].bone_index))
        .collect::<Result<_, _>>()?;

    // ---- 2. text section ----
    let text = build_text_section(&prepared, solids, params)?;

    // ---- 3. 组装 ----
    let mut out = Vec::with_capacity(PHY_HEADER_SIZE + text.len() + 64);
    put_i32(&mut out, PHY_HEADER_SIZE as i32); // size：恒为 sizeof(phyheader_t)
    put_i32(&mut out, 0); // id：恒为 0
    put_i32(
        &mut out,
        i32::try_from(prepared.len()).map_err(|_| PhyError::BadParameter {
            what: "hulls",
            detail: crate::tr("the solid count overflows i32"),
        })?,
    );
    put_u32(&mut out, params.checksum);

    for (p, body) in prepared.iter().zip(&bodies) {
        // 每 solid 的 32 字节头。`size = surfaceSize + 28`，所以 solid 记录的
        // 步长是 size + 4 == surfaceSize + 32，与实测的 stride 规则一致。
        put_i32(&mut out, p.surface_size + 28);
        put_u32(&mut out, VPHYSICS_ID);
        put_u16(&mut out, VPHYSICS_COLLISION_VERSION);
        put_i16(&mut out, COLLIDE_POLY);
        put_i32(&mut out, p.surface_size);
        // dragAxisAreas（R25）：官方 `params.buildDragAxisAreas = true`，
        // 由 `ConvertConvexToCollideParams` 算出「拖拽分数」。
        // 修前恒写 [1,1,1]（语料只有 9.2% 的 solid 真的三轴都是 1）。
        for v in p.drag_axis_areas {
            put_f32(&mut out, v);
        }
        put_i32(&mut out, 0); // axisMapSize：源码注释 "not yet supported"
        out.extend_from_slice(body);
    }

    out.extend_from_slice(&text);

    // ---- 4. 自检 ----
    //
    // 每次写出都跑一遍完整校验。这不是"测试代码"——它是防止未来改动悄悄
    // 破坏某个偏移的唯一防线，代价只有 O(文件大小)。
    check_invariants(&out)?;
    Ok(out)
}

fn validate_nonnegative_number(what: &'static str, value: f32) -> Result<(), PhyError> {
    if !value.is_finite() || value < 0.0 {
        return Err(PhyError::BadParameter {
            what,
            detail: format!("must be a nonnegative finite number, got {value}"),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `rotation_inertia` 的官方口径
// ---------------------------------------------------------------------------

/// 算出写进 `IVP_Compact_Surface::rotation_inertia` 的三个分量（R24）。
///
/// # 官方公式（语料 404 个 solid 上 **91.3% 命中到 1e-4**，中位 1.00000005）
///
/// ```text
///   A = ∫x'²dV / V      （关于质心、密度 1、已除以体积；单位 m²）
///   B = ∫y'²dV / V
///   C = ∫z'²dV / V
///
///   rotation_inertia = [ √(B² + C²),  √(A² + C²),  √(A² + B²) ]
/// ```
///
/// ⚠️ **不是**真惯量 `[B+C, A+C, A+B]` —— 而是「两项的**斜边**」。
/// IVP 把每个主惯量写成 `√(A_y² + A_z²)` 而不是 `A_y + A_z`。
///
/// # 三条独立渐近线（全部**精确**吻合，不是拟合）
///
/// | 形状 | `A:B:C` | 真惯量 `B+C` | 本公式 `√(B²+C²)` | `g = 官方/(B+C)` |
/// |---|---|---|---|---|
/// | 立方体 | `1:1:1` | `2A` | `√2·A` | **`1/√2 = 0.70710678`** |
/// | `t=2` 长方体 | `4:1:1` | `5` | `√17` | **`√17/5 = 0.8246211`** |
/// | 细长杆 | `t²:1:ε` | `≈t²` | `≈t²` | **`→ 1`** |
///
/// 实测锚点（`docs/_probe/artifacts/msh1.phy`，官方产物，50×10×50 inch）：
///
/// ```text
///   A = 5.376333e-3   B = C = 1.099017e-1
///   官方 rot = [1.554245e-1, 1.100331e-1, 1.100331e-1]
///   预测     = [1.554244e-1, 1.100331e-1, 1.100331e-1]   相对误差 1.3e-7 ~ 2.2e-7
///   rot[0]/(B+C) = 0.707107  ← x 轴垂直两边相等（50/50）⟹ 恰好 1/√2
///   rot[1]/(A+C) = 0.954502  ← y 轴垂直两边 10/50 ⟹ t=5
/// ```
///
/// # 修前的两处错误（R24）
///
/// `phy.rs` 原先写的是 `I = [B·V, A·V, C·V]` 的**对角元**（即未除以体积的
/// `[B+C, A+C, A+B]`）：
///
/// 1. **没除体积** ⟹ 整体差 `1/V` 倍。用户的枪 `V = 3.36e-3 m³` ⟹ **290×**
///    偏小（实测 mdlc `6.14e-6` vs Valve `1.78e-3`）。
/// 2. **用 `B+C` 而不是 `√(B²+C²)`** ⟹ 各向异性也差（枪上 y 轴差 **2.05%**）。
///
/// # 物理后果
///
/// IVP 用 `inv_inertia`：`rotation_inertia` 小 290× ⟹ 角冲量响应大 290×
/// ⟹ 一次接触就把物体转飞 ⟹ 一堆枪互相弹开、散得很远。
/// 这正是用户报的「好像有 1 个隐藏的巨大碰撞盒」——**是惯性问题，不是几何问题**
/// （hull 已独立确认为枪尺寸 `1.009 × 0.177 × 0.025 m`，
/// `upper_limit_radius` 两边相同，ledgetree 逐位相同）。
///
/// # 参数
///
/// * `inertia_mat` —— `MassProperties::from_trimesh(1.0, …)` 的
///   `reconstruct_inertia_matrix()`（**含质量**，即 `I = m·r²`）。
/// * `volume` —— 同一组 `MassProperties` 的 `mass()`，**就是体积**（m³）。
///   密度固定 1.0 ⟹ `mass == volume`。
/// * `inertia_scale` —— 安全阀（[`PhyParams::inertia_scale`]），默认 1.0。
///
/// ⚠️ **不要再乘/除 [`SOURCE_TO_IVP`]**：`all_pts` 已经是**米**，
/// 所以 `I/V` 直接就是官方口径（m²）。点缩放 `k` 时 `I → I·k⁵`、`V → V·k³`
/// ⟹ **`I/V → (I/V)·k²`**。
fn rotation_inertia_of(
    inertia_mat: &parry3d::math::Matrix3,
    volume: f32,
    inertia_scale: f32,
) -> [f32; 3] {
    // 二阶矩的对角元 ÷ 体积 = `A`、`B`、`C`（单位 m²）。
    //
    // `reconstruct_inertia_matrix()` 给的是**真惯量**：
    //   `I_xx = ∫(y²+z²)dV = B·V + C·V`
    // 而我们只要 `B` 和 `C` 各自 —— 用 `(I_yy + I_zz − I_xx)/2` 反解：
    //   `I_yy + I_zz − I_xx = 2·A·V`  ⟹ `A = (I_yy+I_zz−I_xx)/(2V)`
    //
    // ⚠️ **不能**直接用 `reconstruct_inertia_matrix()` 的对角元当 `A`/`B`/`C`
    // —— 那三个是「另两项之和」。这是本函数最容易写错的一步。
    let (ixx, iyy, izz) = (
        inertia_mat.x_axis.x,
        inertia_mat.y_axis.y,
        inertia_mat.z_axis.z,
    );
    let inv2v = if volume != 0.0 {
        1.0 / (2.0 * volume)
    } else {
        0.0
    };
    let a = (iyy + izz - ixx) * inv2v;
    let b = (ixx + izz - iyy) * inv2v;
    let c = (ixx + iyy - izz) * inv2v;

    [
        (b * b + c * c).sqrt() * inertia_scale,
        (a * a + c * c).sqrt() * inertia_scale,
        (a * a + b * b).sqrt() * inertia_scale,
    ]
}

// ---------------------------------------------------------------------------
// 预处理
// ---------------------------------------------------------------------------

/// 单个凸块预处理后的形态（下标已指向 solid 的共享点数组）。
struct PreparedHull {
    /// 三角面，`start_point_index` 是共享数组里的全局下标。
    tris: Vec<[u32; 3]>,
    /// 反向边表，下标 `3k + i` → `(k', i')`。用来算 `opposite_index`。
    reverse: Vec<[u32; 2]>,
    /// 本 hull 引用的点（升序去重）。
    used: Vec<u32>,
    /// 本 ledge 在 ledge 区里的字节偏移。
    offset: usize,
    /// 本凸块自己的 AABB（叶子节点几何 + 内部节点合并都用它）。
    aabb: ([f32; 3], [f32; 3]),
    /// 本凸块的体积。
    volume: f32,
}

/// 单个 solid 预处理后的形态。
struct PreparedSolid {
    /// solid 级共享点数组（所有凸块的并集，按首次出现顺序）。
    points: Vec<[f32; 3]>,
    hulls: Vec<PreparedHull>,
    /// 树节点（`2n−1` 个；单凸块时就是 1 个叶子）。
    nodes: Vec<TreeNode>,
    /// ledge 区总字节数。
    ledge_region_size: usize,
    /// `surfaceSize`。
    surface_size: i32,
    /// `mass_center`（模型坐标）。
    mass_center: [f32; 3],
    /// `rotation_inertia`。
    rotation_inertia: [f32; 3],
    /// `dragAxisAreas`（R25）—— 三个「拖拽分数」，恒在 `(0, 1]`。
    ///
    /// 见 [`drag_axis_areas`]。修前恒写 `[1,1,1]`。
    drag_axis_areas: [f32; 3],
    /// 本 solid 是否命中 `sizeRatio > 9` 的 `rotdamping` HACKHACK（R25）。
    ///
    /// 见 [`long_skinny_rotdamping`]。命中时文本段的 `rotdamping` 被**覆盖**
    /// 成 `1.0`（即使 QC 写了 0）。
    long_skinny: bool,
    /// `upper_limit_radius` = `max‖p − mass_center‖`，对**共享点数组全部点**取。
    upper_limit_radius: f32,
    /// solid 体积（各凸块之和）。
    volume: f32,
}

/// 一个 ledgetree 节点。
struct TreeNode {
    /// 右子相对**本节点**的字节偏移；`0` 表示叶子。
    /// 左子恒为 `this + 28`（隐式），所以不存。
    right_offset: i32,
    /// 该节点对应的 hull 下标；`None` 表示内部节点。
    hull: Option<usize>,
    center: [f32; 3],
    radius: f32,
    box_sizes: [u8; 3],
}

/// 校验一个 solid 的凸块输入并整理成写出需要的形态。
fn prepare_solid(
    solid: usize,
    hulls: &[PhyHull],
    inertia_scale: f32,
) -> Result<PreparedSolid, PhyError> {
    if hulls.is_empty() {
        return Err(PhyError::BadParameter {
            what: "hulls",
            detail: crate::tr_fmt!("solid[%{solid}] has no hull at all", solid = solid),
        });
    }

    // ---- 共享点数组：按首次出现顺序合并所有凸块的顶点 ----
    //
    // 实测：一个 solid 里所有 hull 共用一份点数组（2204/2204），
    // 每个 hull 的 `c_point_offset` 都指向它的起点。
    //
    // 去重只按"三个 float 的位模式完全相同"，**不做 epsilon 合并** ——
    // 后者会改动浮点值，而 `upper_limit_radius`、`box_sizes` 这些量
    // 必须与写进文件的点逐位对应。
    let mut points: Vec<[f32; 3]> = Vec::new();
    let mut seen: HashMap<[u32; 3], u32> = HashMap::new();
    let mut prepared_hulls = Vec::with_capacity(hulls.len());
    let mut cursor = 0usize;

    for (hi, hull) in hulls.iter().enumerate() {
        // ---- 顶点合法性 ----
        if hull.vertices.len() < 4 {
            return Err(PhyError::HullTooFewPoints {
                solid,
                points: hull.vertices.len(),
            });
        }
        for (i, p) in hull.vertices.iter().enumerate() {
            if !p.iter().all(|c| c.is_finite()) {
                return Err(PhyError::NonFinitePoint { solid, point: i });
            }
        }
        if hull.faces.is_empty() {
            return Err(PhyError::HullDegenerate {
                solid,
                detail: crate::tr_fmt!("the face list of hull %{hull} is empty", hull = hi),
            });
        }
        if hull.faces.len() > MAX_TRIANGLES_PER_HULL {
            return Err(PhyError::TooManyTriangles {
                solid,
                triangles: hull.faces.len(),
                max: MAX_TRIANGLES_PER_HULL,
            });
        }
        for (fi, f) in hull.faces.iter().enumerate() {
            for &idx in f {
                if idx as usize >= hull.vertices.len() {
                    return Err(PhyError::IndexOutOfRange {
                        solid,
                        face: fi,
                        index: idx,
                        vertex_count: hull.vertices.len(),
                    });
                }
            }
        }
        // 闭合流形校验：`opposite_index` 依赖"每条有向边恰好一次 + 反向边存在"。
        // 输入有洞或有重复边时写出的文件不报错，但引擎邻接查询会走错，
        // 表现为穿模或卡住。宁可在编译期失败。
        //
        // 注意：**必须对重映射之后的三角形做校验**，不能对原始面表做。
        // 顶点去重会把坐标完全相同的不同下标合并成一个，如果某个面因此
        // 退化成 `[0,0,0]`，原始面表看起来是闭合的、写出的却是垃圾。
        // 第一版就在这里漏掉了，被 `degenerate_bounds_are_rejected` 抓到。

        // 顶点重映射到共享数组。
        let mut tris: Vec<[u32; 3]> = Vec::with_capacity(hull.faces.len());
        let mut used: Vec<u32> = Vec::new();
        for f in &hull.faces {
            let mut t = [0u32; 3];
            for (k, &idx) in f.iter().enumerate() {
                let v = hull.vertices[idx as usize];
                // ---- Source 世界空间 → IVP 世界空间 ----
                //
                // ⚠️ **不只是乘 0.0254**：官方还做了一次轴映射
                // `(-y, -z, x)`。见 [`source_to_ivp_axis`] / [`to_ivp`]，
                // 那里有反编译原文与三个独立实验的交叉验证。
                // 漏掉旋转会让碰撞体相对渲染网格转 90°。
                let q = to_ivp(v);
                let key = [q[0].to_bits(), q[1].to_bits(), q[2].to_bits()];
                let g = *seen.entry(key).or_insert_with(|| {
                    points.push(q);
                    (points.len() - 1) as u32
                });
                t[k] = g;
                if !used.contains(&g) {
                    used.push(g);
                }
            }
            tris.push(t);
        }
        used.sort_unstable();

        // 流形校验走重映射后的面表（见上）。
        let reverse = build_reverse_edges(&tris, solid)?;

        // ---- 几何量 ----
        //
        // 算质量属性时要把三角形重映射到**本凸块**的紧凑下标，因为
        // `MassProperties` 要求顶点表覆盖全部被引用的下标。
        let local_pts: Vec<Vector> = used
            .iter()
            .map(|&i| {
                let p = points[i as usize];
                Vector::new(p[0], p[1], p[2])
            })
            .collect();
        let local_of: HashMap<u32, u32> = used
            .iter()
            .enumerate()
            .map(|(li, &g)| (g, li as u32))
            .collect();
        let local_tris: Vec<[u32; 3]> = tris
            .iter()
            .map(|t| [local_of[&t[0]], local_of[&t[1]], local_of[&t[2]]])
            .collect();
        let mass_props = MassProperties::from_convex_polyhedron(1.0, &local_pts, &local_tris);
        // ---- 体积：**换算回 Source 单位（inch³）** ----
        //
        // ⚠️ `points` 已经是米，所以 `mass_props.mass()` 给的是 **m³**。
        // 但 `.phy` 的 **text 段 `"volume"` 是 inch³** —— 那是 studiomdl
        // **自己**用 `physcollision->ConvexVolume()`（Source 单位）算出来
        // 再 `fprintf` 的（`collisionmodel.cpp:1174` 累加 → `2310` 打印），
        // 与 IVP 的点数组**不是同一条路径**。
        //
        // 实测判据（`probe_phy_text_volume_unit.js`，官方 `msh1.phy`）：
        //
        // ```text
        // text 段 "volume"      = 16999.996094   ← 就是 17000 in³
        // 点数组算出的体积      = 0.278580 m³
        // 0.278580 / 0.0254³    = 17000.00       ← 换算回 inch³ 完全吻合
        // ```
        //
        // 所以这里**必须除回去**：`volume` 是「Source 单位的体积」，
        // 而 `points` 是「IVP 单位的点」。同一个文件里两种单位并存。
        let volume = mass_props.mass().abs() / (SOURCE_TO_IVP * SOURCE_TO_IVP * SOURCE_TO_IVP);

        // 叶子节点几何用**本凸块引用的点**，这是 2051/2051 实测精确的规则。
        let aabb = aabb_of(&points, &used, solid)?;

        prepared_hulls.push(PreparedHull {
            offset: cursor,
            tris,
            reverse,
            used,
            aabb,
            volume,
        });
        cursor += LEDGE_HEADER_SIZE + TRIANGLE_SIZE * prepared_hulls.last().unwrap().tris.len();
    }

    if points.len() > MAX_POINTS_PER_SOLID {
        return Err(PhyError::TooManyPoints {
            solid,
            points: points.len(),
        });
    }

    // ---- ledge 区 = 各 ledge 头+三角形，紧跟一份共享点数组 ----
    let ledge_region_size = cursor + POINT_SIZE * points.len();

    // ---- 碰撞树 ----
    let nodes = build_tree(&prepared_hulls);

    // surfaceSize == 48 + ledge_region_size + 28 × node_count。
    let surface_size_usize =
        COMPACT_SURFACE_SIZE + ledge_region_size + LEDGETREE_NODE_SIZE * nodes.len();
    let surface_size = i32::try_from(surface_size_usize).map_err(|_| PhyError::BadParameter {
        what: "surfaceSize",
        detail: crate::tr_fmt!("%{size} overflows i32", size = surface_size_usize),
    })?;

    // ---- 整个 solid 的质量属性 ----
    //
    // `mass_center` / `rotation_inertia` 用**全部凸块合并后**的几何算，
    // 这样多凸块的 solid 也有正确的质心与惯性主轴。
    let all_pts: Vec<Vector> = points
        .iter()
        .map(|p| Vector::new(p[0], p[1], p[2]))
        .collect();
    let all_tris: Vec<[u32; 3]> = prepared_hulls
        .iter()
        .flat_map(|h| h.tris.iter().copied())
        .collect();
    let solid_props = MassProperties::from_trimesh(1.0, &all_pts, &all_tris);
    let mass_center_v = solid_props.local_com;
    let inertia_mat = solid_props.reconstruct_inertia_matrix();
    let rotation_inertia = rotation_inertia_of(&inertia_mat, solid_props.mass(), inertia_scale);

    // upper_limit_radius 以 mass_center 为中心、覆盖**共享点数组全部点**。
    let upper_limit_radius = points
        .iter()
        .map(|p| {
            let d = [
                p[0] - mass_center_v.x,
                p[1] - mass_center_v.y,
                p[2] - mass_center_v.z,
            ];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
        })
        .fold(0.0f32, f32::max);

    let volume = prepared_hulls.iter().map(|h| h.volume).sum();

    // ---- dragAxisAreas（R25）----
    //
    // ⚠️ 官方用 **Source 单位（inch）** 的包围盒算（`dragAreaEpsilon` 的
    // 默认值是 `0.25f = 0.5in × 0.5in`）。而 `points` 是**米**。
    // 但 `drag_axis_areas` 是**比值**（投影面积 / 截面积）⟹ 长度单位**约掉**，
    // 用米算与用 inch 算逐位等价。这里直接用米，不引入换算。
    let drag_axis_areas = drag_axis_areas(&points, &prepared_hulls, solid)?;

    // ---- `rotdamping` 的 HACKHACK（R25）----
    //
    // ⚠️ 这一条**也**用 Source 单位的包围盒 —— 但 `sizeRatio` 是
    // **尺度不变量**（分子分母同量纲）⟹ 用米算同样等价。
    let long_skinny = long_skinny_rotdamping(&points);

    Ok(PreparedSolid {
        points,
        hulls: prepared_hulls,
        nodes,
        ledge_region_size,
        surface_size,
        mass_center: [mass_center_v.x, mass_center_v.y, mass_center_v.z],
        rotation_inertia,
        drag_axis_areas,
        long_skinny,
        upper_limit_radius,
        volume,
    })
}

// ---------------------------------------------------------------------------
// `dragAxisAreas` 与 `rotdamping` 的 HACKHACK（R25）
// ---------------------------------------------------------------------------

/// `dragAxisAreas[3]` —— 三个「拖拽分数」（R25）。
///
/// # 官方出处（`collisionmodel.cpp:1162-1228`，`CreateCollide`）
///
/// ```c
/// convertconvexparams_t params;
/// params.Defaults();                       // dragAreaEpsilon = 0.25f（0.5in×0.5in）
/// params.buildOuterConvexHull = true;
/// params.buildDragAxisAreas   = true;      // ← mdlc 修前没做，恒写 [1,1,1]
/// ...
/// pBase->m_pCollisionData = physcollision->ConvertConvexToCollideParams( ... );
/// ```
///
/// `#if 0` 的调试输出（`:1222-1227`）说明了这两个字段是什么：
///
/// ```c
/// Msg("Drag epsilon is %.3f\n", params.dragAreaEpsilon );
/// Vector areas = physcollision->CollideGetOrthographicAreas( pBase->m_pCollisionData );
/// Msg("Drag fractions are %.3f %.3f %.3f\n", areas.x, areas.y, areas.z );
/// ```
///
/// **「Drag fractions」** ⟹ 是**比值**（分数），不是绝对面积。
///
/// # 公式
///
/// ```text
///   areas[k] = A_proj(k) / (size[j] · size[l])      {j,l} = 另两轴
/// ```
///
/// 其中 `A_proj(k)` 是凸体沿轴 `k` 的**正交投影面积**（轮廓面积），
/// 分母是 **OBB 在垂直 `k` 的平面上的截面积**。
///
/// # 轴序：`[0, 2, 1]`（**推导出来的，不是拟合**）
///
/// ⚠️ 官方的 `size` 来自 `bv.maxs - bv.mins`，是 **Source 轴序**的包围盒；
/// 而 `ConvertConvexToCollideParams` 收到的凸块已经过
/// `BuildConvexFromVerts` 的轴映射 `out = (x, −z, y)`。
/// 两个轴序之间差一个置换，推导如下：
///
/// ```text
///   Source size  S = (sx, sy, sz)
///   IVP    bbox  I = (sx, sz, sy)          ← 我的点数组量到的是这个
///   crossS = (sy·sz, sz·sx, sx·sy)
///   crossI = (I1·I2, I0·I2, I0·I1) = (sz·sy, sx·sy, sx·sz)
///          = (crossS[0], crossS[2], crossS[1])
/// ⟹ areas[k] = A_proj(S_k) / crossS[k] = ratioI[perm[k]],  perm = [0, 2, 1]
/// ```
///
/// 实测吻合：语料「可判别」子集（三轴两两差 > 5%，199 个 solid）上
/// `[0,2,1]` 的 `<5%` 命中率 **83.1%**，而恒等置换只有 **32.0%**。
///
/// # 投影面积：散度定理闭式（不需要网格射线投射）
///
/// ```text
///   A_proj(n) = 0.25 · Σ_tri |n · ((v1−v0) × (v2−v0))|
/// ```
///
/// ⚠️ **是 `0.25` 不是 `0.5`**：`(v1−v0)×(v2−v0)` 的模长 = **2×三角形面积**，
/// 而 `Σ|n·normal| = 2·A_proj` ⟹ 合起来乘 `0.25`。
/// 写成 `0.5` 的症状是**每个轴的相对误差都恰好 ≈ 0.5** —— 那不是「公式错」，
/// 是少乘了一个 2。
///
/// # 残差（已知，可接受）
///
/// 实测中位相对误差 **8.4e-3**、`q90` **5.2e-2**、`<1%` 命中 **53.9%**、
/// `<5%` **89.6%**（单 hull，n=2316）。残差来自官方用**网格射线投射近似**
/// （`dragAreaEpsilon` 的注释：「Basically you get a ray cast through each
/// square of epsilon surface area on each OBB side」），而这里用的是**精确闭式**。
/// 官方 38.9% 的轴偏小 ⟹ 射线投射**低估**投影面积（网格边界效应）。
///
/// **解析锚点**：轴对齐长方体上 `A_proj(k) == size[j]·size[l]` ⟹ 恒为 `1.0`。
/// 实测 **64/64 = 100%** —— 这条不依赖任何近似。
///
/// 需要绝对保守时把 [`PhyParams::inertia_scale`] 设成 `0.0` 不影响这里；
/// `dragAxisAreas` 没有对应的开关（官方也没有）。
fn drag_axis_areas(
    points: &[[f32; 3]],
    hulls: &[PreparedHull],
    solid: usize,
) -> Result<[f32; 3], PhyError> {
    if points.is_empty() {
        return Ok([1.0; 3]);
    }
    // 整个 solid 的 AABB（官方用 `bv`，也是整个 solid 的）。
    let mut mn = points[0];
    let mut mx = points[0];
    for p in points {
        for a in 0..3 {
            if p[a] < mn[a] {
                mn[a] = p[a];
            }
            if p[a] > mx[a] {
                mx[a] = p[a];
            }
        }
    }
    let size = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];

    // 投影面积：`0.25 · Σ|n_k|`，`n = (v1−v0) × (v2−v0)`（模长 = 2×面积）。
    let mut proj = [0.0f64; 3];
    for h in hulls {
        for t in &h.tris {
            let (i0, i1, i2) = (t[0] as usize, t[1] as usize, t[2] as usize);
            let (Some(p), Some(q), Some(r)) = (points.get(i0), points.get(i1), points.get(i2))
            else {
                return Err(PhyError::BadParameter {
                    what: "dragAxisAreas",
                    detail: crate::tr_fmt!(
                        "a triangle of solid[%{solid}] references an out-of-range vertex index",
                        solid = solid
                    ),
                });
            };
            let u = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
            let v = [r[0] - p[0], r[1] - p[1], r[2] - p[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            for k in 0..3 {
                proj[k] += f64::from(n[k]).abs();
            }
        }
    }
    for v in &mut proj {
        *v *= 0.25;
    }

    // ⚠️ **`size` 是 IVP 轴序**（`points` 已经过 `out = (x,−z,y)` 映射）。
    //
    // 官方 `areas` 的三个分量是 **Source 轴序**，而 `proj`/`cross` 都是 IVP 轴序
    // ⟹ 只需在最后按 `SRC` 把 IVP 轴映射到 Source 轴**一次**：
    //
    // ```text
    //   drag[k] = proj[SRC[k]] / crossI[SRC[k]]      SRC = [0, 2, 1]
    // ```
    //
    // ⚠️ **踩过的坑**：第一版写成 `proj[SRC[k]] / crossS[SRC[k]]`，
    // 其中 `crossS` 是**按 Source 轴序**定义的 `(s1·s2, s2·s0, s0·s1)`。
    // 但 `crossS[SRC[k]] == crossI[k]`（因为 `SRC` 自逆），
    // 于是分子按 `SRC` 索引、分母没按 ⟹ 两者错开一个 `SRC`
    // ⟹ **长方体上给出 0.0003 而不是 1.0**（本该是解析锚点）。
    // 判据：长方体必须给出**精确 1.0** —— 这条自检门当场抓住了它。
    //
    // `crossI[i] = size[(i+1)%3] · size[(i+2)%3]`（另两轴之积，与轴序无关的形式）。
    let cross_i = [
        f64::from(size[1]) * f64::from(size[2]),
        f64::from(size[2]) * f64::from(size[0]),
        f64::from(size[0]) * f64::from(size[1]),
    ];
    // IVP 轴 k 对应 Source 轴 `SRC[k]`。
    const SRC: [usize; 3] = [0, 2, 1];

    let mut out = [1.0f32; 3];
    for k in 0..3 {
        let j = SRC[k];
        let c = cross_i[j];
        if c > 0.0 {
            out[k] = (proj[j] / c) as f32;
            // 官方约定是分数，钳到 (0, 1]（浮点边界可能给出 1.0000001）。
            out[k] = out[k].clamp(f32::MIN_POSITIVE, 1.0);
        }
    }
    Ok(out)
}

/// 官方 `rotdamping` 的 HACKHACK：`sizeRatio > 9` ⟹ 强制 `1.0`（R25）。
///
/// # 官方出处（`collisionmodel.cpp:1206-1218`）
///
/// ```c
/// Vector tmp = size;
/// tmp[largest] = 0;
/// float len = tmp.Length();
/// if ( len > 0 ) {
///     float sizeRatio = size[largest] / len;
///     // HACKHACK: Hardcoded size ratio to induce damping
///     // This prevents long skinny objects from rolling endlessly
///     if ( sizeRatio > 9 ) pBase->m_rotdamping = 1.0f;
/// }
/// ```
///
/// `largest` 是**最长轴**的下标（`:1184-1191` 的循环里算出来的）。
/// 所以 `sizeRatio = size[largest] / |size 去掉 largest 分量|`。
///
/// # 这是**覆盖**，不是默认值
///
/// `m_rotdamping` 在 `SetCollisionModelDefaults`（`:579-581`）里已被
/// QC 的 `$rotdamping` 填过，`CreateCollide` 在这之后才跑 ⟹ QC 写了 `0`
/// 也会被改成 `1.0`。而文本段（`:2374`）写的是**覆盖后**的值。
///
/// # 判据（**不需要 oracle**：`sizeRatio` 是尺度与轴序不变量）
///
/// 分子分母同量纲 ⟹ 用米算与用 inch 算等价；轴序置换也不变。
/// 语料实测（`probe_phy_damping.js`，600 文件）：
///
/// | 组 | n | 文本段 `rotdamping == 1.0` |
/// |---|---|---|
/// | `sizeRatio > 9` | 32 | **32 (100.0%)** |
/// | `sizeRatio ≤ 9` | 623 | 10 (1.6%) |
///
/// 两组差 60 倍 ⟹ 规律成立。（`≤9` 那 10 个是 QC **自己**写了 `$rotdamping 1`。）
fn long_skinny_rotdamping(points: &[[f32; 3]]) -> bool {
    if points.is_empty() {
        return false;
    }
    let mut mn = points[0];
    let mut mx = points[0];
    for p in points {
        for a in 0..3 {
            if p[a] < mn[a] {
                mn[a] = p[a];
            }
            if p[a] > mx[a] {
                mx[a] = p[a];
            }
        }
    }
    let size = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];
    let mut largest = 0;
    for i in 0..3 {
        if size[i] > size[largest] {
            largest = i;
        }
    }
    let mut tmp = size;
    tmp[largest] = 0.0;
    let len = (tmp[0] * tmp[0] + tmp[1] * tmp[1] + tmp[2] * tmp[2]).sqrt();
    if len <= 0.0 {
        return false;
    }
    let size_ratio = size[largest] / len;
    size_ratio > 9.0
}
// ---------------------------------------------------------------------------
// 碰撞树
// ---------------------------------------------------------------------------

/// 按"左子恒为 `this + 28`、右子为 `this + offset_right_node`"的隐式布局
/// 递归排布 ledgetree 节点。
///
/// 节点总数 `2n − 1`（`n` 是凸块数），**恒为奇数**——与实测的节点数分布一致
/// （1、3、5、…、149）。
///
/// 布局的关键约束是"左子树必须紧跟在父节点之后"，所以递归时先占位、
/// 再建左子树，最后回填右子偏移。
fn build_tree(hulls: &[PreparedHull]) -> Vec<TreeNode> {
    let mut out: Vec<TreeNode> = Vec::with_capacity(hulls.len() * 2);
    let order: Vec<usize> = (0..hulls.len()).collect();
    let (root, _, _) = build_subtree(hulls, &order, &mut out);
    debug_assert_eq!(root, 0);
    out
}

/// 递归建子树，返回 `(本子树根节点下标, 子树 AABB 的 min, max)`。
fn build_subtree(
    hulls: &[PreparedHull],
    idx: &[usize],
    out: &mut Vec<TreeNode>,
) -> (usize, [f32; 3], [f32; 3]) {
    let me = out.len();
    // 先占位，这样左子树一定从 `me + 1` 开始（左子恒为 this+28 的要求）。
    out.push(TreeNode {
        right_offset: 0,
        hull: None,
        center: [0.0; 3],
        radius: 0.0,
        box_sizes: [0; 3],
    });

    if idx.len() == 1 {
        let h = &hulls[idx[0]];
        let (mn, mx) = h.aabb;
        let (center, radius, box_sizes) = node_geometry(mn, mx);
        out[me] = TreeNode {
            right_offset: 0, // 叶子
            hull: Some(idx[0]),
            center,
            radius,
            box_sizes,
        };
        return (me, mn, mx);
    }

    // 对半切。左右都非空，所以递归一定会终止。
    let mid = idx.len() / 2;
    let (left, lmn, lmx) = build_subtree(hulls, &idx[..mid], out);
    debug_assert_eq!(left, me + 1);
    let (right, rmn, rmx) = build_subtree(hulls, &idx[mid..], out);

    let mn = [lmn[0].min(rmn[0]), lmn[1].min(rmn[1]), lmn[2].min(rmn[2])];
    let mx = [lmx[0].max(rmx[0]), lmx[1].max(rmx[1]), lmx[2].max(rmx[2])];
    let (center, radius, box_sizes) = node_geometry(mn, mx);
    out[me] = TreeNode {
        // 右子偏移必须是 28 的倍数，且为正（0 会被当成"叶子"）。
        right_offset: ((right - me) * LEDGETREE_NODE_SIZE) as i32,
        hull: None,
        center,
        radius,
        box_sizes,
    };
    (me, mn, mx)
}

/// 由 AABB 算节点的 `center` / `radius` / `box_sizes`。
///
/// `box_sizes[i] = trunc(半边长 / (radius/250)) + 1`。
///
/// `trunc` 不是 `round`：报告里暴力测试了 7 种候选公式，截断是唯一拟合的。
/// 全部走 f64 —— 校验器是 JS（f64），读回 f32 之后用 f64 运算，
/// 这里必须走同一条路径，否则 `box_sizes` 可能在网格边界上差 1 格。
fn node_geometry(mn: [f32; 3], mx: [f32; 3]) -> ([f32; 3], f32, [u8; 3]) {
    let half = [
        (mx[0] as f64 - mn[0] as f64) * 0.5,
        (mx[1] as f64 - mn[1] as f64) * 0.5,
        (mx[2] as f64 - mn[2] as f64) * 0.5,
    ];
    let center = [
        ((mn[0] as f64 + mx[0] as f64) * 0.5) as f32,
        ((mn[1] as f64 + mx[1] as f64) * 0.5) as f32,
        ((mn[2] as f64 + mx[2] as f64) * 0.5) as f32,
    ];
    let radius = ((half[0] * half[0] + half[1] * half[1] + half[2] * half[2]).sqrt()) as f32;
    let mut box_sizes = [0u8; 3];
    if radius > 0.0 {
        let step = radius as f64 * BOUNDINGBOX_STEP;
        for (i, h) in half.iter().enumerate() {
            // half[i] <= radius 恒成立，所以 (h/step) <= 250，+1 <= 251。
            // clamp 只是防御性的，不会真的触发。
            box_sizes[i] = ((h / step).trunc() + 1.0).clamp(0.0, 255.0) as u8;
        }
    }
    (center, radius, box_sizes)
}

/// 组装一个 solid 的 IVP 负载：`IVP_Compact_Surface` + ledge 区 + 树。
fn build_solid_body(
    solid: usize,
    p: &PreparedSolid,
    bone_index: Option<u32>,
) -> Result<Vec<u8>, PhyError> {
    // client_data：有骨骼写 boneIndex + 1，无骨骼写 0。
    // 实测 90/90 零例外；`+1` 是为了让 0 表示"未绑定"。
    let client_data = match bone_index {
        Some(b) => {
            let plus_one = b.checked_add(1).ok_or_else(|| PhyError::BadParameter {
                what: "bone_index",
                detail: crate::tr_fmt!("bone index %{bone} +1 overflows u32", bone = b),
            })?;
            i32::try_from(plus_one).map_err(|_| PhyError::BadParameter {
                what: "bone_index",
                detail: crate::tr_fmt!("bone index %{bone} +1 overflows i32", bone = b),
            })?
        }
        None => 0,
    };

    let offset_ledgetree_root = (COMPACT_SURFACE_SIZE + p.ledge_region_size) as i32;

    // `c_point_offset` 的精确规则：从本 ledge 起、到最后一个 ledge 结束为止的
    // 所有 ledge 大小之和（见模块文档与下方注释）。
    let mut suffix_sizes: Vec<usize> = Vec::with_capacity(p.hulls.len());
    {
        let mut acc = 0usize;
        for h in p.hulls.iter().rev() {
            acc += LEDGE_HEADER_SIZE + TRIANGLE_SIZE * h.tris.len();
            suffix_sizes.push(acc);
        }
        suffix_sizes.reverse();
    }

    let mut body = Vec::with_capacity(p.surface_size as usize);

    // ---- IVP_Compact_Surface (48 B) ----
    put_vec3(&mut body, p.mass_center);
    put_vec3(&mut body, p.rotation_inertia);
    put_f32(&mut body, p.upper_limit_radius);
    // 位域：低 8 位 max_factor_surface_deviation，高 24 位 byte_size。
    // 容差取 250 —— 实测取值范围 135..251，250 是出现过的最大值。
    let byte_size = p.surface_size as u32 & 0x00FF_FFFF;
    put_u32(&mut body, (250u32 & 0xFF) | (byte_size << 8));
    put_i32(&mut body, offset_ledgetree_root);
    put_i32(&mut body, 0); // dummy[0]
    put_i32(&mut body, 0); // dummy[1]
    put_u32(&mut body, IVP_COMPACT_SURFACE_ID); // dummy[2] == 'IVPS'

    // ---- 各 ledge（头 + 三角形） ----
    //
    // `c_point_offset` 的**精确规则**（我在真实语料上复核过，见模块文档）：
    // 它是「本 ledge 到共享点数组起点」的距离，也就是
    // **从本 ledge 起、到最后一个 ledge 结束为止，所有 ledge 大小之和**。
    //
    // ```text
    // c_point_offset(i) = Σ_{j >= i} (16 + 16 × nTri_j)
    // ```
    //
    // 实测：2204/2204 个 hull 的 `ledge + c_point_offset` 都落在同一个地址
    // （共享点数组起点），其中 1583 个来自多 hull 的 solid。单 hull 时它退化
    // 成 `16 + 16×nTri` —— 这正是规格 §4.2 那条规则，也是它标注"774/774"
    // 的原因：那 774 个恰好全是单 hull 的 solid。
    for (hi, h) in p.hulls.iter().enumerate() {
        let c_point_offset = suffix_sizes[hi] as i32;
        // size_div_16 = 头(1) + 三角形 + 本 hull 引用的点数。
        let size_div_16 = 1 + h.tris.len() + h.used.len();

        put_i32(&mut body, c_point_offset);
        put_i32(&mut body, client_data);
        put_u32(
            &mut body,
            // has_chilren_flag:2 = 0（终结 ledge，所以 +0x04 当 client_data 用）
            // is_compact_flag:2 = 1
            (1u32 << 2)
                // dummy:4 = 0
                | ((size_div_16 as u32 & 0x00FF_FFFF) << 8),
        );
        put_i16(
            &mut body,
            i16::try_from(h.tris.len()).map_err(|_| PhyError::TooManyTriangles {
                solid,
                triangles: h.tris.len(),
                max: MAX_TRIANGLES_PER_HULL,
            })?,
        );
        put_i16(&mut body, 0); // for_future_use

        for (k, t) in h.tris.iter().enumerate() {
            // tri_index:12 | pierce_index:12 | material_index:7 | is_virtual:1
            // pierce_index 实测恒等于 tri_index；material_index 与 is_virtual 恒为 0。
            put_u32(&mut body, (k as u32 & 0x0FFF) | ((k as u32 & 0x0FFF) << 12));
            for (i, &start) in t.iter().enumerate() {
                // 边 i 的起点；终点是边 (i+1)%3 的起点（引擎的 next_table）。
                let r = h.reverse[3 * k + i];
                let (k2, i2) = (r[0], r[1]);
                let opp = (4 * k2 as i32 + 1 + i2 as i32) - (4 * k as i32 + 1 + i as i32);
                if !(-16384..16384).contains(&opp) {
                    return Err(PhyError::TooManyTriangles {
                        solid,
                        triangles: h.tris.len(),
                        max: MAX_TRIANGLES_PER_HULL,
                    });
                }
                put_u32(
                    &mut body,
                    (start & 0xFFFF) | (((opp as u32) & 0x7FFF) << 16),
                );
            }
        }
    }

    // ---- 共享点数组 ----
    // 第 4 个 float（源码里别名成 `void *client_data`）实测是残留垃圾，
    // 引擎不读，写 0。
    for pt in &p.points {
        put_vec3(&mut body, *pt);
        put_f32(&mut body, 0.0);
    }

    // ---- 碰撞树 ----
    for (i, n) in p.nodes.iter().enumerate() {
        put_i32(&mut body, n.right_offset);
        // offset_compact_ledge 相对**本节点**：ledge 在 body+48，节点在
        // body+offset_ledgetree_root+28i，所以是 48 − 节点绝对偏移（恒为负）。
        match n.hull {
            Some(hi) => {
                let node_abs = offset_ledgetree_root + (i * LEDGETREE_NODE_SIZE) as i32;
                put_i32(
                    &mut body,
                    COMPACT_SURFACE_SIZE as i32 - node_abs + p.hulls[hi].offset as i32,
                );
            }
            // 内部节点：offset_compact_ledge == 0 表示"本节点不对应 hull"。
            None => put_i32(&mut body, 0),
        }
        put_vec3(&mut body, n.center);
        put_f32(&mut body, n.radius);
        body.push(n.box_sizes[0]);
        body.push(n.box_sizes[1]);
        body.push(n.box_sizes[2]);
        body.push(0); // free_0
    }

    debug_assert_eq!(body.len(), p.surface_size as usize);
    Ok(body)
}

/// 建立有向边表、校验闭合可定向流形，并返回 `3k+i -> (k', i')` 的反向边表。
///
/// # 为什么必须校验
///
/// `opposite_index` 依赖"每条有向边恰好出现一次、且反向边也恰好出现一次"。
/// 输入有洞或有重复边时，写出的文件不会报错，但引擎的邻接查询会走错，
/// 表现为穿模或卡住。宁可在编译期失败，也不要产出这种文件。
fn build_reverse_edges(faces: &[[u32; 3]], solid: usize) -> Result<Vec<[u32; 2]>, PhyError> {
    let mut dir: HashMap<(u32, u32), (u32, u32)> = HashMap::with_capacity(faces.len() * 3);
    for (k, f) in faces.iter().enumerate() {
        for i in 0..3 {
            let (u, v) = (f[i], f[(i + 1) % 3]);
            // 退化三角形（两个下标相同）也会走到这里：它产生自环边。
            if u == v || dir.insert((u, v), (k as u32, i as u32)).is_some() {
                return Err(PhyError::DuplicateEdge {
                    solid,
                    from: u,
                    to: v,
                });
            }
        }
    }
    let mut out = Vec::with_capacity(faces.len() * 3);
    for f in faces {
        for i in 0..3 {
            let (u, v) = (f[i], f[(i + 1) % 3]);
            match dir.get(&(v, u)) {
                Some(&(k2, i2)) => out.push([k2, i2]),
                None => {
                    return Err(PhyError::OpenMesh {
                        solid,
                        from: u,
                        to: v,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// 一组点（按下标子集）的 AABB。
fn aabb_of(
    points: &[[f32; 3]],
    used: &[u32],
    solid: usize,
) -> Result<([f32; 3], [f32; 3]), PhyError> {
    let first = used.first().ok_or(PhyError::HullDegenerate {
        solid,
        detail: crate::tr("the hull references no vertex at all"),
    })?;
    let mut mn = points[*first as usize];
    let mut mx = mn;
    for &i in used {
        let p = points[i as usize];
        for a in 0..3 {
            if p[a] < mn[a] {
                mn[a] = p[a];
            }
            if p[a] > mx[a] {
                mx[a] = p[a];
            }
        }
    }
    Ok((mn, mx))
}

// ---------------------------------------------------------------------------
// text section
// ---------------------------------------------------------------------------

fn format_mass(mass: f32) -> String {
    let fixed = format!("{mass:.6}");
    if fixed
        .parse::<f32>()
        .is_ok_and(|rounded| rounded.to_bits() == mass.to_bits())
    {
        fixed
    } else {
        mass.to_string()
    }
}

/// 生成 text section（含结尾的单个 `0x00`）。
///
/// 格式实测自 331/331 个真实文件：以 `solid {` 开头、以 `}\n` 结尾、
/// 紧跟一个 `0x00`、内部没有别的 NUL。KeyValues 的键和值**都带引号**，
/// `\n` 分行，**没有逗号也没有分号**；`solid {` / `editparams {` 这类块名不带引号。
fn build_text_section(
    prepared: &[PreparedSolid],
    solids: &[PhySolid],
    params: &PhyParams<'_>,
) -> Result<Vec<u8>, PhyError> {
    // ---- 质量分配（studiomdl collisionmodel.cpp） ----
    let total_volume: f64 = prepared
        .iter()
        .zip(solids)
        .map(|(p, s)| f64::from(p.volume) * f64::from(s.mass_bias))
        .sum();
    if !total_volume.is_finite() || total_volume <= 0.0 {
        return Err(PhyError::BadParameter {
            what: "mass_bias",
            detail: "biased collision volume must be positive and finite".into(),
        });
    }
    let masses: Vec<f32> = prepared
        .iter()
        .zip(solids)
        .map(|(p, s)| {
            let m = ((f64::from(p.volume) * f64::from(s.mass_bias) / total_volume)
                * f64::from(params.total_mass)) as f32;
            // studiomdl：`if (pPhys->m_mass < 1.0) pPhys->m_mass = 1.0;`
            if m < 1.0 { 1.0 } else { m }
        })
        .collect();

    let mut s = String::new();
    for (i, (p, solid)) in prepared.iter().zip(solids).enumerate() {
        s.push_str("solid {\n");
        s.push_str(&format!("\"index\" \"{i}\"\n"));
        s.push_str(&format!("\"name\" \"{}\"\n", solid.name));
        if let Some(parent) = &solid.parent {
            s.push_str(&format!("\"parent\" \"{parent}\"\n"));
        }
        s.push_str(&format!("\"mass\" \"{}\"\n", format_mass(masses[i])));
        s.push_str(&format!(
            "\"surfaceprop\" \"{}\"\n",
            solid.surface_prop.as_deref().unwrap_or(params.surface_prop)
        ));
        // 逐 solid 的值 = 本 solid 的覆盖（`$jointdamping` 等）否则全局默认。
        let damping = solid.damping.unwrap_or(params.damping);
        let mut rot_damping = solid.rot_damping.unwrap_or(params.rot_damping);
        // R25：官方 `CreateCollide`（`collisionmodel.cpp:1206-1218`）的 HACKHACK
        // **覆盖** `m_rotdamping = 1.0f`（长条形物体不无限打滚）。
        // 它跑在 `SetCollisionModelDefaults`（`:579-581`，填 QC 值）**之后**
        // ⟹ 即使 QC 写了 `$rotdamping 0` 也会被改成 `1.0`。
        if p.long_skinny {
            rot_damping = 1.0;
        }
        let inertia = solid.inertia.unwrap_or(params.inertia);
        s.push_str(&format!("\"damping\" \"{damping:.6}\"\n"));
        s.push_str(&format!("\"rotdamping\" \"{rot_damping:.6}\"\n"));
        if let Some(drag) = params.drag {
            s.push_str(&format!("\"drag\" \"{drag:.6}\"\n"));
        }
        s.push_str(&format!("\"inertia\" \"{inertia:.6}\"\n"));
        s.push_str(&format!("\"volume\" \"{:.6}\"\n", p.volume));
        // 实测：只有 massbias != 1.0 才写这一行。
        if (solid.mass_bias - 1.0).abs() > f32::EPSILON {
            s.push_str(&format!("\"massbias\" \"{:.6}\"\n", solid.mass_bias));
        }
        s.push_str("}\n");
    }

    // ---- 有 parent 的 solid 各写一个 ragdollconstraint ----
    //
    // 实测 `parent` / `child` 都是 **solid 的 index**（不是骨骼下标）。
    //
    // ⚠️ **轴值默认全是 0**，不是「不限位」的 ±180。
    //
    // 依据：`BuildRagdollConstraint`（`collisionmodel.cpp:2215-2226`）的
    // **第一句**就是 `memset( &ragdoll, 0, sizeof(ragdoll) );` ——
    // 只有 QC 写了 `$jointconstrain` 才会把某个轴改成别的值。
    //
    // 约束挂在**子** solid 上：官方遍历整条约束链，只应用
    // `CollisionIndex(pList->m_pJointName) == ragdoll.childIndex` 的那些
    // （`collisionmodel.cpp:2242`）。同一轴后写覆盖先写。
    for (i, solid) in solids.iter().enumerate() {
        let Some(parent_name) = &solid.parent else {
            continue;
        };
        // 父 solid 按名字查找；找不到就退回 0（根），而不是 panic。
        let parent_idx = solids
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(parent_name))
            .unwrap_or(0);

        // 本 solid 的九个轴值，从默认全 0 起步。
        let mut axes = [(0.0f32, 0.0f32, 0.0f32); 3];
        for c in &params.constraints {
            // 约束按**骨骼名**匹配本 solid 的 `name`（ragdoll 的 name 就是骨骼名）。
            if !c.bone.eq_ignore_ascii_case(&solid.name) {
                continue;
            }
            let a = (c.axis as usize).min(2);
            axes[a] = c.kind.axis_values(c.min, c.max, c.friction);
        }

        s.push_str("ragdollconstraint {\n");
        s.push_str(&format!("\"parent\" \"{parent_idx}\"\n"));
        s.push_str(&format!("\"child\" \"{i}\"\n"));
        for (k, axis) in ["x", "y", "z"].iter().enumerate() {
            let (lo, hi, fr) = axes[k];
            s.push_str(&format!("\"{axis}min\" \"{lo:.6}\"\n"));
            s.push_str(&format!("\"{axis}max\" \"{hi:.6}\"\n"));
            s.push_str(&format!("\"{axis}friction\" \"{fr:.6}\"\n"));
        }
        s.push_str("}\n");
    }

    // ---- collisionrules（`$noselfcollisions` 优先于 `$jointcollide`） ----
    //
    // 官方 `collisionmodel.cpp:2422-2447`：`if (noSelfCollisions) {...}
    // else if (m_pCollisionPairs) {...}` —— **互斥且 noself 优先**。
    if params.no_self_collisions {
        s.push_str("collisionrules {\n");
        s.push_str("\"selfcollisions\" \"0\"\n");
        s.push_str("}\n");
    } else if !params.collision_pairs.is_empty() {
        s.push_str("collisionrules {\n");
        for (a, b) in &params.collision_pairs {
            // 官方把骨骼名解析成 solid 下标，**任一侧找不到就丢弃该对**
            // （`CollisionIndex` 返回 -1；`obj0 != obj1` 也是必要条件）。
            let ia = solids.iter().position(|s| s.name.eq_ignore_ascii_case(a));
            let ib = solids.iter().position(|s| s.name.eq_ignore_ascii_case(b));
            if let (Some(x), Some(y)) = (ia, ib)
                && x != y
            {
                s.push_str(&format!("\"collisionpair\" \"{x},{y}\"\n"));
            }
        }
        s.push_str("}\n");
    }

    // ---- animatedfriction ----
    if let Some(af) = &params.animated_friction {
        s.push_str("animatedfriction {\n");
        // 两个 *friction* 字段官方是 `Safe_atoi` 出来的**整数**，
        // 但用 `KeyWriteFloat` 写 ⟹ 落盘形如 `"100.000000"`。
        s.push_str(&format!("\"animfrictionmin\" \"{:.6}\"\n", af.min as f32));
        s.push_str(&format!("\"animfrictionmax\" \"{:.6}\"\n", af.max as f32));
        s.push_str(&format!("\"animfrictiontimein\" \"{:.6}\"\n", af.time_in));
        s.push_str(&format!("\"animfrictiontimeout\" \"{:.6}\"\n", af.time_out));
        s.push_str(&format!(
            "\"animfrictiontimehold\" \"{:.6}\"\n",
            af.time_hold
        ));
        s.push_str("}\n");
    }

    // ---- editparams ----
    s.push_str("editparams {\n");
    s.push_str(&format!("\"rootname\" \"{}\"\n", params.root_name));
    s.push_str(&format!(
        "\"totalmass\" \"{}\"\n",
        format_mass(params.total_mass)
    ));
    if params.concave {
        s.push_str("\"concave\" \"1\"\n");
    }
    // `jointmerge` 用的是 **QC 里写的原始名字**（`Q_snprintf("%s,%s")`），
    // 不是解析后的骨骼名 —— 实测 `"jointmerge" "bone_mid,bone_tip"`。
    for (parent, child) in &params.merge_list {
        s.push_str(&format!("\"jointmerge\" \"{parent},{child}\"\n"));
    }
    s.push_str("}\n");

    if let Some(text) = params.collision_text {
        if text.len() > 1_048_576 || text.contains('\0') {
            return Err(PhyError::BadParameter {
                what: "collision_text",
                detail: "collision text is too large or contains a NUL".into(),
            });
        }
        s.push_str(text.trim_end());
        s.push('\n');
    }
    let mut bytes = s.into_bytes();
    bytes.push(0); // 单个 NUL 终止符
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

/// 对已写出的 `.phy` 字节跑完整的不变量校验。
///
/// 检查的就是 `docs/phy-format-research.md` §10.1 末尾那 13 条硬约束
/// （外加 `client_data` 与共享点数组两条）。[`write_phy`] 每次都会调用它，
/// 所以任何布局回归都会在写出的那一刻炸掉，而不是等到引擎加载时才发现。
pub fn check_invariants(bytes: &[u8]) -> Result<PhyLayout, PhyError> {
    let bad = |m: String| Err(PhyError::SelfCheck(m));

    if bytes.len() < PHY_HEADER_SIZE {
        return bad(crate::tr_fmt!(
            "the file is only %{len} bytes; not even the 16-byte header fits",
            len = bytes.len()
        ));
    }
    // 约束 12：phyheader.size == 16、id == 0。
    let header_size = read_i32(bytes, 0);
    if header_size != PHY_HEADER_SIZE as i32 {
        return bad(crate::tr_fmt!(
            "phyheader.size should be 16, but is %{got}",
            got = header_size
        ));
    }
    let header_id = read_i32(bytes, 4);
    if header_id != 0 {
        return bad(crate::tr_fmt!(
            "phyheader.id should be 0, but is %{got}",
            got = header_id
        ));
    }
    let solid_count = read_i32(bytes, 8);
    if solid_count < 0 {
        return bad(crate::tr_fmt!(
            "phyheader.solidCount is negative: %{got}",
            got = solid_count
        ));
    }
    let solid_count = solid_count as usize;

    let mut base = PHY_HEADER_SIZE;
    let mut surface_sizes = Vec::with_capacity(solid_count);
    let mut node_counts = Vec::with_capacity(solid_count);
    let mut ledge_region_sizes = Vec::with_capacity(solid_count);

    for si in 0..solid_count {
        if base + SOLID_HEADER_SIZE > bytes.len() {
            return bad(crate::tr_fmt!(
                "the header of solid[%{solid}] goes past the end of the file",
                solid = si
            ));
        }
        let size = read_i32(bytes, base);
        let vphysics_id = read_u32(bytes, base + 4);
        let version = read_u16(bytes, base + 8);
        let model_type = read_i16(bytes, base + 10);
        let surface_size = read_i32(bytes, base + 12);
        let axis_map_size = read_i32(bytes, base + 28);

        // 约束 11。
        if vphysics_id != VPHYSICS_ID {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] vphysicsID should be 'VPHY', but is %{got}",
                solid = si,
                got = format!("{vphysics_id:#010x}")
            ));
        }
        if version != VPHYSICS_COLLISION_VERSION {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] version should be 0x0100, but is %{got}",
                solid = si,
                got = format!("{version:#06x}")
            ));
        }
        if model_type != COLLIDE_POLY {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] modelType should be 0, but is %{got}",
                solid = si,
                got = model_type
            ));
        }
        if axis_map_size != 0 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] axisMapSize should be 0, but is %{got}",
                solid = si,
                got = axis_map_size
            ));
        }
        // 约束 2。
        if size - surface_size != 28 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] size - surfaceSize should be 28, but is %{size} - %{surface}",
                solid = si,
                size = size,
                surface = surface_size
            ));
        }
        if surface_size < (COMPACT_SURFACE_SIZE + LEDGETREE_NODE_SIZE) as i32 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] surfaceSize is too small: %{size}",
                solid = si,
                size = surface_size
            ));
        }

        let body = base + SOLID_HEADER_SIZE;
        let solid_end = body + surface_size as usize;
        if solid_end > bytes.len() {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] goes past the end of the file (needs %{need}, only %{have})",
                solid = si,
                need = solid_end,
                have = bytes.len()
            ));
        }
        let region = body + COMPACT_SURFACE_SIZE;

        // 约束 3：offset_ledgetree_root 相对 IVP_Compact_Surface 起点。
        let offset_ledgetree_root = read_i32(bytes, body + 32);
        if offset_ledgetree_root < COMPACT_SURFACE_SIZE as i32 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] offset_ledgetree_root=%{offset} falls inside the surface",
                solid = si,
                offset = offset_ledgetree_root
            ));
        }
        let node_off = body + offset_ledgetree_root as usize;
        if node_off + LEDGETREE_NODE_SIZE > solid_end {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] offset_ledgetree_root=%{offset} makes the tree go past the end of the solid",
                solid = si,
                offset = offset_ledgetree_root
            ));
        }
        let node_len = solid_end - node_off;
        if !node_len.is_multiple_of(LEDGETREE_NODE_SIZE) {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] the tree region length %{len} is not a multiple of 28",
                solid = si,
                len = node_len
            ));
        }
        let node_count = node_len / LEDGETREE_NODE_SIZE;

        // 约束 1：surfaceSize == 48 + ledge_region_size + 28 × node_count。
        let ledge_region_size = node_off - region;
        if surface_size as usize
            != COMPACT_SURFACE_SIZE + ledge_region_size + LEDGETREE_NODE_SIZE * node_count
        {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] surfaceSize=%{size} != 48 + %{ledge} + 28×%{nodes}",
                solid = si,
                size = surface_size,
                ledge = ledge_region_size,
                nodes = node_count
            ));
        }
        if ledge_region_size == 0 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] the ledge region length is 0",
                solid = si
            ));
        }

        // byte_size == surfaceSize、max_factor_surface_deviation > 0、dummy[2] == 'IVPS'。
        let packed = read_u32(bytes, body + 28);
        if ((packed >> 8) & 0x00FF_FFFF) != surface_size as u32 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] IVP_Compact_Surface::byte_size != surfaceSize",
                solid = si
            ));
        }
        if (packed & 0xFF) == 0 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] max_factor_surface_deviation is 0",
                solid = si
            ));
        }
        if read_u32(bytes, body + 44) != IVP_COMPACT_SURFACE_ID {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] dummy[2] should be 'IVPS'",
                solid = si
            ));
        }
        if read_i32(bytes, body + 36) != 0 || read_i32(bytes, body + 40) != 0 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] dummy[0]/dummy[1] should be 0",
                solid = si
            ));
        }

        // ---- 走一遍树，收集 hull ----
        //
        // `offset_compact_ledge` **恒为负**（ledge 在低地址），所以这里
        // 必须用 i64 做有符号加法。第一版写成 `off + rel as usize`，
        // 负 rel 转成巨大的 usize 后立刻算术溢出 panic。
        let mut hull_offsets: Vec<usize> = Vec::new();
        let mut visited = vec![false; node_count];
        let mut stack = vec![node_off];
        while let Some(off) = stack.pop() {
            if off < node_off || off + LEDGETREE_NODE_SIZE > solid_end {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] the tree node @%{off} is out of range",
                    solid = si,
                    off = off
                ));
            }
            let idx = (off - node_off) / LEDGETREE_NODE_SIZE;
            if visited[idx] {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] the tree has a cycle @%{off}",
                    solid = si,
                    off = off
                ));
            }
            visited[idx] = true;
            let right = read_i32(bytes, off);
            let rel = read_i32(bytes, off + 4);
            if rel != 0 {
                let lo = off as i64 + rel as i64;
                if lo < region as i64 || lo + LEDGE_HEADER_SIZE as i64 > node_off as i64 {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] hull @%{off} falls outside the ledge region",
                        solid = si,
                        off = lo
                    ));
                }
                let lo = lo as usize;
                if !hull_offsets.contains(&lo) {
                    hull_offsets.push(lo);
                }
            }
            if right != 0 {
                if right % LEDGETREE_NODE_SIZE as i32 != 0 {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] offset_right_node=%{right} is not a multiple of 28",
                        solid = si,
                        right = right
                    ));
                }
                // 左子恒为 this + 28。
                stack.push(off + LEDGETREE_NODE_SIZE);
                let r = off as i64 + right as i64;
                if r < node_off as i64 || r + LEDGETREE_NODE_SIZE as i64 > solid_end as i64 {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] the right child node @%{right} is out of range",
                        solid = si,
                        right = r
                    ));
                }
                stack.push(r as usize);
            }
        }
        if visited.iter().any(|v| !v) {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] %{count} tree nodes were not reached",
                solid = si,
                count = visited.iter().filter(|v| !**v).count()
            ));
        }
        if hull_offsets.is_empty() {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] has no hull at all",
                solid = si
            ));
        }
        hull_offsets.sort_unstable();

        // ---- 约束：ledge 紧密排列 ----
        let mut cursor = region;
        for &lo in &hull_offsets {
            if lo != cursor {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] the ledge has a gap: expected %{want}, got %{got}",
                    solid = si,
                    want = cursor,
                    got = lo
                ));
            }
            let n_tri = read_i16(bytes, lo + 12);
            if n_tri <= 0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has n_triangles=%{n}",
                    solid = si,
                    off = lo,
                    n = n_tri
                ));
            }
            cursor = lo + LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri as usize;
            if cursor > node_off {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] the triangles of hull @%{off} go past the start of the tree",
                    solid = si,
                    off = lo
                ));
            }
        }
        // ---- 共享点数组正好填满 [最后一个 ledge 结束, 树起点) ----
        let shared_base = cursor;
        // 约束 10：16 字节对齐。
        if !(node_off - shared_base).is_multiple_of(POINT_SIZE) {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] the shared point array is not 16-byte aligned",
                solid = si
            ));
        }
        let n_points = (node_off - shared_base) / POINT_SIZE;
        if n_points == 0 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] the shared point array is empty",
                solid = si
            ));
        }

        for &lo in &hull_offsets {
            // 约束 4：`c_point_offset` 是「从本 ledge 起、到最后一个 ledge
            // 结束为止的所有 ledge 大小之和」，也就是本 ledge 到共享点数组
            // 起点的距离。单 hull 时退化成 `16 + 16×nTri`（规格 §4.2）。
            //
            // 这里按定义式验证：累加本 ledge 及其后所有 ledge 的大小。
            let mut expect_cpo = 0i64;
            for &l2 in hull_offsets.iter().filter(|&&l| l >= lo) {
                expect_cpo +=
                    (LEDGE_HEADER_SIZE + TRIANGLE_SIZE * read_i16(bytes, l2 + 12) as usize) as i64;
            }
            let cpo = read_i32(bytes, lo);
            if lo as i64 + cpo as i64 != shared_base as i64 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has c_point_offset=%{cpo} pointing at %{got}, but the shared array starts at %{want}",
                    solid = si,
                    off = lo,
                    cpo = cpo,
                    got = lo as i64 + cpo as i64,
                    want = shared_base
                ));
            }
            if cpo as i64 != expect_cpo {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has c_point_offset=%{cpo}, but the suffix sum says %{want}",
                    solid = si,
                    off = lo,
                    cpo = cpo,
                    want = expect_cpo
                ));
            }
            // 约束 6。
            let l2 = read_u32(bytes, lo + 8);
            if (l2 & 3) != 0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has has_chilren_flag != 0",
                    solid = si,
                    off = lo
                ));
            }
            if ((l2 >> 2) & 3) != 1 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has is_compact_flag != 1",
                    solid = si,
                    off = lo
                ));
            }
            if ((l2 >> 4) & 0xF) != 0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has dummy != 0",
                    solid = si,
                    off = lo
                ));
            }
            if read_i16(bytes, lo + 14) != 0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has for_future_use != 0",
                    solid = si,
                    off = lo
                ));
            }
        }

        // ---- 每个 hull 的流形 / tri_index / opposite_index / size_div_16 / 节点几何 ----
        for &lo in &hull_offsets {
            let n_tri = read_i16(bytes, lo + 12) as usize;

            let mut tris: Vec<[u32; 3]> = Vec::with_capacity(n_tri);
            for k in 0..n_tri {
                let o = lo + LEDGE_HEADER_SIZE + k * TRIANGLE_SIZE;
                // 约束 7：tri_index 从 0 起严格递增。
                if read_u32(bytes, o) & 0x0FFF != k as u32 {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] hull @%{off} triangle %{k} has tri_index != %{k}",
                        solid = si,
                        off = lo,
                        k = k
                    ));
                }
                let t = [0usize, 1, 2].map(|i| read_u32(bytes, o + 4 + i * 4) & 0xFFFF);
                for &v in &t {
                    if v as usize >= n_points {
                        return bad(crate::tr_fmt!(
                            "solid[%{solid}] hull @%{off} references point %{v}, but the shared array only has %{n} points",
                            solid = si,
                            off = lo,
                            v = v,
                            n = n_points
                        ));
                    }
                }
                tris.push(t);
            }

            // 约束 9：闭合可定向流形。
            let mut dir: HashMap<(u32, u32), usize> = HashMap::with_capacity(n_tri * 3);
            for t in &tris {
                for i in 0..3 {
                    *dir.entry((t[i], t[(i + 1) % 3])).or_insert(0) += 1;
                }
            }
            for (&(u, v), &c) in &dir {
                if c != 1 {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] hull @%{off} directed edge %{u}->%{v} appears %{c} times",
                        solid = si,
                        off = lo,
                        u = u,
                        v = v,
                        c = c
                    ));
                }
                if !dir.contains_key(&(v, u)) {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] hull @%{off} directed edge %{u}->%{v} has no opposite edge",
                        solid = si,
                        off = lo,
                        u = u,
                        v = v
                    ));
                }
            }

            // 约束 8：opposite_index 精确。
            let mut idx: HashMap<(u32, u32), (usize, usize)> = HashMap::with_capacity(n_tri * 3);
            for (k, t) in tris.iter().enumerate() {
                for i in 0..3 {
                    idx.insert((t[i], t[(i + 1) % 3]), (k, i));
                }
            }
            let mut used: Vec<u32> = Vec::new();
            for t in &tris {
                for &v in t {
                    if !used.contains(&v) {
                        used.push(v);
                    }
                }
            }
            for (k, t) in tris.iter().enumerate() {
                let o = lo + LEDGE_HEADER_SIZE + k * TRIANGLE_SIZE;
                for i in 0..3 {
                    let (k2, i2) = idx[&(t[(i + 1) % 3], t[i])];
                    let expect = (4 * k2 as i32 + 1 + i2 as i32) - (4 * k as i32 + 1 + i as i32);
                    let raw = (read_u32(bytes, o + 4 + i * 4) >> 16) & 0x7FFF;
                    // 15 位二补数还原。
                    let got = if raw & 0x4000 != 0 {
                        raw as i32 - 0x8000
                    } else {
                        raw as i32
                    };
                    if got != expect {
                        return bad(crate::tr_fmt!(
                            "solid[%{solid}] hull @%{off} triangle %{k} edge %{i} should have opposite_index %{want}, but has %{got}",
                            solid = si,
                            off = lo,
                            k = k,
                            i = i,
                            want = expect,
                            got = got
                        ));
                    }
                }
            }

            // 约束 5：size_div_16 == 1 + n_triangles + 该 hull 引用的点数。
            let size_div_16 = (read_u32(bytes, lo + 8) >> 8) & 0x00FF_FFFF;
            let expect = 1 + n_tri + used.len();
            if size_div_16 as usize != expect {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} has size_div_16=%{got} != 1+%{n_tri}+%{points}",
                    solid = si,
                    off = lo,
                    got = size_div_16,
                    n_tri = n_tri,
                    points = used.len()
                ));
            }

            // ---- 叶子节点几何（2051/2051 实测精确） ----
            let mut mn = [f32::INFINITY; 3];
            let mut mx = [f32::NEG_INFINITY; 3];
            for &v in &used {
                let p = shared_base + v as usize * POINT_SIZE;
                for a in 0..3 {
                    let c = read_f32(bytes, p + a * 4);
                    if c < mn[a] {
                        mn[a] = c;
                    }
                    if c > mx[a] {
                        mx[a] = c;
                    }
                }
            }
            let (expect_center, expect_radius, expect_box) = node_geometry(mn, mx);

            let node = find_node_for_hull(bytes, node_off, node_count, lo)?;
            for (a, &want) in expect_center.iter().enumerate() {
                let got = read_f32(bytes, node + 8 + a * 4);
                if (got - want).abs() > 1e-4 {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] hull @%{off} node center[%{axis}] should be %{want}, but is %{got}",
                        solid = si,
                        off = lo,
                        axis = a,
                        want = want,
                        got = got
                    ));
                }
            }
            let got_radius = read_f32(bytes, node + 20);
            if (got_radius - expect_radius).abs() > 1e-3 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} node radius should be %{want}, but is %{got}",
                    solid = si,
                    off = lo,
                    want = expect_radius,
                    got = got_radius
                ));
            }
            if got_radius <= 0.0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] hull @%{off} node radius is 0",
                    solid = si,
                    off = lo
                ));
            }
            for a in 0..3 {
                let got = bytes[node + 24 + a];
                if got != expect_box[a] {
                    return bad(crate::tr_fmt!(
                        "solid[%{solid}] hull @%{off} node box_sizes[%{axis}] should be %{want}, but is %{got}",
                        solid = si,
                        off = lo,
                        axis = a,
                        want = expect_box[a],
                        got = got
                    ));
                }
            }
            if bytes[node + 27] != 0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] node free_0 != 0",
                    solid = si
                ));
            }
        }

        // ---- 单节点树时 offset_compact_ledge == 48 − offset_ledgetree_root ----
        if node_count == 1 {
            let rel = read_i32(bytes, node_off + 4);
            let expect = COMPACT_SURFACE_SIZE as i32 - offset_ledgetree_root;
            if rel != expect {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] the offset_compact_ledge of a single-node tree should be %{want}, but is %{got}",
                    solid = si,
                    want = expect,
                    got = rel
                ));
            }
            if rel >= 0 {
                return bad(crate::tr_fmt!(
                    "solid[%{solid}] offset_compact_ledge=%{got} should be negative (the ledge lives at a lower address)",
                    solid = si,
                    got = rel
                ));
            }
        }

        // ---- upper_limit_radius == max‖p − mass_center‖ ----
        let mass_center = [
            read_f32(bytes, body),
            read_f32(bytes, body + 4),
            read_f32(bytes, body + 8),
        ];
        let upper_limit_radius = read_f32(bytes, body + 24);
        let mut max_d = 0.0f32;
        for k in 0..n_points {
            let p = shared_base + k * POINT_SIZE;
            let d = [
                read_f32(bytes, p) - mass_center[0],
                read_f32(bytes, p + 4) - mass_center[1],
                read_f32(bytes, p + 8) - mass_center[2],
            ];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if len > max_d {
                max_d = len;
            }
        }
        if (upper_limit_radius - max_d).abs() > 1e-4 {
            return bad(crate::tr_fmt!(
                "solid[%{solid}] upper_limit_radius should be %{want}, but is %{got}",
                solid = si,
                want = max_d,
                got = upper_limit_radius
            ));
        }

        surface_sizes.push(surface_size);
        node_counts.push(node_count);
        ledge_region_sizes.push(ledge_region_size);
        base = solid_end;
    }

    // ---- 约束 13：text section ----
    let solids_end = base;
    if solids_end >= bytes.len() {
        return bad(crate::tr("there is no text section"));
    }
    let text_size = bytes.len() - solids_end;
    let text = &bytes[solids_end..];
    if *text.last().unwrap() != 0 {
        return bad(crate::tr(
            "the text section does not end with a single 0x00",
        ));
    }
    if text[..text.len() - 1].contains(&0) {
        return bad(crate::tr("the text section contains a NUL byte"));
    }
    let body = std::str::from_utf8(&text[..text.len() - 1]).map_err(|e| {
        PhyError::SelfCheck(crate::tr_fmt!(
            "the text section is not UTF-8: %{err}",
            err = e
        ))
    })?;
    if !body.starts_with("solid {") {
        return bad(crate::tr(
            "the text section does not start with \"solid {\"",
        ));
    }
    if !body.ends_with('\n') {
        return bad(crate::tr("the text section does not end with a newline"));
    }
    let blocks = body.lines().filter(|l| l.starts_with("solid {")).count();
    if blocks != solid_count {
        return bad(crate::tr_fmt!(
            "the text section has %{blocks} \"solid {\" blocks, but solidCount=%{solid}",
            blocks = blocks,
            solid = solid_count
        ));
    }

    Ok(PhyLayout {
        file_size: bytes.len(),
        solid_count,
        solids_end,
        text_size,
        surface_sizes,
        node_counts,
        ledge_region_sizes,
    })
}

/// 找到引用 `hull_off` 的那个树节点。
fn find_node_for_hull(
    bytes: &[u8],
    node_off: usize,
    node_count: usize,
    hull_off: usize,
) -> Result<usize, PhyError> {
    for i in 0..node_count {
        let off = node_off + i * LEDGETREE_NODE_SIZE;
        let rel = read_i32(bytes, off + 4);
        if rel != 0 && (off as i64 + rel as i64) == hull_off as i64 {
            return Ok(off);
        }
    }
    Err(PhyError::SelfCheck(crate::tr_fmt!(
        "no node references hull @%{off}",
        off = hull_off
    )))
}

// ---------------------------------------------------------------------------
// 凸分解辅助（薄封装 parry3d，避免调用方直接依赖 parry 类型）
// ---------------------------------------------------------------------------

/// 把一个可能凹的网格近似凸分解成若干凸块（parry3d 的 VHACD）。
///
/// 每个返回的 [`PhyHull`] 都是闭合凸多面体，可以直接作为
/// [`write_phy_multi`] 里一个 solid 的多块输入，或各自作为独立 solid。
///
/// # 为什么用 VHACD 而不是自己写
///
/// 近似凸分解是体素化 + 递归切分的数值算法，自己实现既慢又容易在退化
/// 网格上炸掉。parry3d 是成熟的 Rust 物理几何库，直接用它的结果。
///
/// # 参数
///
/// `resolution` 是体素分辨率（越大越精细也越慢，游戏碰撞体 32..64 足够），
/// `max_hulls` 是凸块数上限（引擎侧每个 hull 都要建 ledge，别开太大）。
pub fn decompose_concave(
    vertices: &[[f32; 3]],
    faces: &[[u32; 3]],
    resolution: u32,
    max_hulls: u32,
) -> Result<Vec<PhyHull>, PhyError> {
    if vertices.is_empty() || faces.is_empty() {
        return Err(PhyError::BadParameter {
            what: "decompose_concave",
            detail: crate::tr("the vertex or face list is empty"),
        });
    }
    if vertices.len() < 4 {
        return Err(PhyError::HullTooFewPoints {
            solid: 0,
            points: vertices.len(),
        });
    }
    for (i, p) in vertices.iter().enumerate() {
        if !p.iter().all(|c| c.is_finite()) {
            return Err(PhyError::NonFinitePoint { solid: 0, point: i });
        }
    }
    for (fi, f) in faces.iter().enumerate() {
        for &idx in f {
            if idx as usize >= vertices.len() {
                return Err(PhyError::IndexOutOfRange {
                    solid: 0,
                    face: fi,
                    index: idx,
                    vertex_count: vertices.len(),
                });
            }
        }
    }
    let vs: Vec<Vector> = vertices
        .iter()
        .map(|p| Vector::new(p[0], p[1], p[2]))
        .collect();
    let params = VHACDParameters {
        resolution: resolution.max(8),
        max_convex_hulls: max_hulls.max(1),
        ..Default::default()
    };
    let decomposition = VHACD::decompose(&params, &vs, faces, false);
    let parts = decomposition.compute_convex_hulls(1);
    let mut out = Vec::with_capacity(parts.len());
    for (pts, tris) in parts {
        if pts.len() < 4 || tris.is_empty() {
            continue;
        }
        out.push(PhyHull {
            vertices: pts.iter().map(|v| [v.x, v.y, v.z]).collect(),
            faces: tris,
        });
    }
    if out.is_empty() {
        return Err(PhyError::HullDegenerate {
            solid: 0,
            detail: crate::tr("VHACD produced no valid convex hull at all"),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 字节写入 / 读取小工具
// ---------------------------------------------------------------------------

fn put_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_i16(buf: &mut Vec<u8>, v: i16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_vec3(buf: &mut Vec<u8>, v: [f32; 3]) {
    put_f32(buf, v[0]);
    put_f32(buf, v[1]);
    put_f32(buf, v[2]);
}

fn read_i32(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn read_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn read_i16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}

fn read_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn read_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn posed_ragdoll_source(sparse_reordered: bool) -> crate::smd::Smd {
        let (root, child) = if sparse_reordered { (9, 2) } else { (0, 1) };
        let nodes = if sparse_reordered {
            format!("{child} \"child\" {root}\n{root} \"root\" -1\n")
        } else {
            format!("{root} \"root\" -1\n{child} \"child\" {root}\n")
        };
        let mut text = format!(
            "version 1\nnodes\n{nodes}end\nskeleton\ntime 0\n{child} 4 0 0 0 0 0\n{root} 10 20 30 0 0 {}\nend\ntriangles\n",
            std::f32::consts::FRAC_PI_2
        );
        for (bone, offset) in [(root, 0.0), (child, 4.0)] {
            for face in cube_faces() {
                text.push_str("phys\n");
                for index in face {
                    let [x, y, z] = cube_vertices()[index as usize];
                    let point = [10.0 - y * 2.0, 20.0 + offset + x, 30.0 + z * 3.0];
                    text.push_str(&format!(
                        "{bone} {} {} {} 0 0 1 0 0 1 {bone} 1\n",
                        point[0], point[1], point[2]
                    ));
                }
            }
        }
        text.push_str("end\n");
        crate::smd::parse_smd(&text).unwrap()
    }

    // Decode single-hull PHY solids without writer helpers.
    fn independent_phy_geometry(bytes: &[u8]) -> Vec<(u32, Vec<[f32; 3]>)> {
        let u32_at = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let f32_at = |offset| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let mut offset = 16;
        let mut solids = Vec::new();
        for _ in 0..u32_at(8) {
            let ledge = offset + 32 + 48;
            let triangles = u16::from_le_bytes(bytes[ledge + 12..ledge + 14].try_into().unwrap());
            let points = ledge + u32_at(ledge) as usize;
            let mut used = std::collections::BTreeSet::new();
            for triangle in 0..triangles as usize {
                for edge in 0..3 {
                    used.insert(u32_at(ledge + 16 + triangle * 16 + 4 + edge * 4) & 0xffff);
                }
            }
            let vertices = used
                .into_iter()
                .map(|index| {
                    let p = points + index as usize * 16;
                    [
                        f32_at(p + 8) / 0.0254,
                        -f32_at(p) / 0.0254,
                        -f32_at(p + 4) / 0.0254,
                    ]
                })
                .collect();
            solids.push((u32_at(ledge + 4) - 1, vertices));
            offset += u32_at(offset) as usize + 4;
        }
        solids
    }

    #[test]
    fn ragdoll_vertices_are_stored_in_each_bones_local_rest_space() {
        for sparse_reordered in [false, true] {
            let smd = posed_ragdoll_source(sparse_reordered);
            let bytes = build_ragdoll_phy_from_smd(
                &smd,
                PhyIdentity {
                    model_name: "posed",
                    collision_smd_name: "posed",
                    surface_prop: "default",
                },
                0,
                100.0,
                &crate::model::Physics::default(),
            )
            .unwrap();
            let solids = independent_phy_geometry(&bytes);
            assert_eq!(solids.len(), 2);
            for (_, vertices) in solids {
                assert_eq!(vertices.len(), 8);
                for point in vertices {
                    for (axis, extent) in [1.0, 2.0, 3.0].into_iter().enumerate() {
                        assert!(
                            (point[axis].abs() - extent).abs() < 1e-4,
                            "sparse={sparse_reordered}, decoded local vertex {point:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ragdoll_solids_precede_children_with_lower_smd_ids() {
        let smd = posed_ragdoll_source(true);
        let phys = crate::model::Physics {
            joints: true,
            ..Default::default()
        };
        let bytes = build_ragdoll_phy_from_smd(
            &smd,
            PhyIdentity {
                model_name: "ordered",
                collision_smd_name: "ordered",
                surface_prop: "default",
            },
            0,
            100.0,
            &phys,
        )
        .unwrap();
        let solids = independent_phy_geometry(&bytes);
        assert_eq!(solids.iter().map(|s| s.0).collect::<Vec<_>>(), vec![9, 2]);
        let mut end = 16;
        for _ in &solids {
            end += u32::from_le_bytes(bytes[end..end + 4].try_into().unwrap()) as usize + 4;
        }
        let text = std::str::from_utf8(&bytes[end..bytes.len() - 1]).unwrap();
        assert!(text.contains("ragdollconstraint {\n\"parent\" \"0\"\n\"child\" \"1\""));
        let bones = [
            toml::from_str::<crate::model::Bone>("name='root'").unwrap(),
            toml::from_str::<crate::model::Bone>("name='child'\nparent='root'").unwrap(),
        ];
        assert_eq!(
            physics_bone_table_for_model(&smd, &bones, &phys).unwrap(),
            vec![0, 1]
        );
    }

    #[test]
    fn merged_ragdoll_vertices_use_the_retained_bones_rest_space() {
        let smd = posed_ragdoll_source(true);
        let mut phys = crate::model::Physics::default();
        phys.merge.push(crate::model::CollisionPairSpec {
            a: "root".into(),
            b: "child".into(),
        });
        let bytes = build_ragdoll_phy_from_smd(
            &smd,
            PhyIdentity {
                model_name: "merged",
                collision_smd_name: "merged",
                surface_prop: "default",
            },
            0,
            100.0,
            &phys,
        )
        .unwrap();
        let solids = independent_phy_geometry(&bytes);
        assert_eq!(solids.len(), 1);
        assert_eq!(solids[0].0, 9);
        let vertices = &solids[0].1;
        for axis in 0..3 {
            let min = vertices
                .iter()
                .map(|p| p[axis])
                .fold(f32::INFINITY, f32::min);
            let max = vertices
                .iter()
                .map(|p| p[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            let expected = [(-1.0, 5.0), (-2.0, 2.0), (-3.0, 3.0)][axis];
            assert!(
                (min - expected.0).abs() < 1e-4 && (max - expected.1).abs() < 1e-4,
                "axis={axis}, bounds={min}..{max}, expected={expected:?}"
            );
        }
    }

    #[test]
    fn joint_mass_bias_redistributes_total_mass_without_increasing_it() {
        let hulls = [cube(), cube()];
        let mut solids = [
            PhySolid::ragdoll(0, "root", None),
            PhySolid::ragdoll(1, "child", Some("root".into())),
        ];
        solids[0].mass_bias = 3.0;
        let mut params = PhyParams::new("biased", 0);
        params.total_mass = 100.0;
        let bytes = write_phy(&hulls, &solids, &params).unwrap();
        let mut end = 16;
        for _ in 0..2 {
            end += u32::from_le_bytes(bytes[end..end + 4].try_into().unwrap()) as usize + 4;
        }
        let text = std::str::from_utf8(&bytes[end..bytes.len() - 1]).unwrap();
        let masses: Vec<f32> = text
            .lines()
            .filter_map(|line| line.strip_prefix("\"mass\" \""))
            .map(|value| value.trim_end_matches('"').parse().unwrap())
            .collect();
        assert_eq!(masses.len(), 2);
        assert!((masses[0] - 75.0).abs() < 1e-4);
        assert!((masses[1] - 25.0).abs() < 1e-4);
        assert!((masses.iter().sum::<f32>() - 100.0).abs() < 1e-4);
    }

    #[test]
    fn invalid_physics_numbers_are_rejected_before_serialization() {
        let hulls = [cube()];
        for invalid in [f32::NAN, f32::INFINITY, -1.0] {
            let mut solid = PhySolid::prop("invalid");
            solid.mass_bias = invalid;
            assert!(matches!(
                write_phy(&hulls, &[solid], &PhyParams::new("invalid", 0)),
                Err(PhyError::BadParameter { .. })
            ));
            for field in 0..4 {
                let mut params = PhyParams::new("invalid", 0);
                match field {
                    0 => params.damping = invalid,
                    1 => params.rot_damping = invalid,
                    2 => params.inertia = invalid,
                    _ => params.drag = Some(invalid),
                }
                assert!(matches!(
                    write_phy(&hulls, &[PhySolid::prop("invalid")], &params),
                    Err(PhyError::BadParameter { .. })
                ));
            }
        }
        let mut zero_bias = PhySolid::prop("invalid");
        zero_bias.mass_bias = 0.0;
        assert!(matches!(
            write_phy(&hulls, &[zero_bias], &PhyParams::new("invalid", 0)),
            Err(PhyError::BadParameter { .. })
        ));
        let mut params = PhyParams::new("invalid", 0);
        params.constraints.push(JointConstraint {
            bone: "child".into(),
            axis: 0,
            kind: JointLimitType::Limit,
            min: f32::NAN,
            max: 20.0,
            friction: 1.0,
        });
        assert!(matches!(
            write_phy(&hulls, &[PhySolid::prop("invalid")], &params),
            Err(PhyError::BadParameter { .. })
        ));
    }

    #[test]
    fn unsupported_mass_center_and_automatic_prop_mass_cannot_succeed_silently() {
        let smd = posed_ragdoll_source(false);
        for joints in [false, true] {
            let phys = crate::model::Physics {
                mass_center: Some([1.0, 2.0, 3.0]),
                ..Default::default()
            };
            let identity = PhyIdentity {
                model_name: "unsupported",
                collision_smd_name: "unsupported",
                surface_prop: "default",
            };
            let result = if joints {
                build_ragdoll_phy_from_smd(&smd, identity, 0, 100.0, &phys)
            } else {
                build_phy_from_smd(&smd, identity, 0, 100.0, &phys, None)
            };
            assert!(result.is_err());
        }
        let phys = crate::model::Physics {
            auto_mass: true,
            ..Default::default()
        };
        assert!(matches!(
            build_phy_from_smd(
                &smd,
                PhyIdentity {
                    model_name: "unsupported",
                    collision_smd_name: "unsupported",
                    surface_prop: "default"
                },
                0,
                100.0,
                &phys,
                None
            ),
            Err(PhyError::AutoMassUnsupported)
        ));
        let mut params = PhyParams::new("unsupported", 0);
        params.mass_center = Some([1.0, 2.0, 3.0]);
        assert!(matches!(
            write_phy(&[cube()], &[PhySolid::prop("unsupported")], &params),
            Err(PhyError::BadParameter {
                what: "mass_center",
                ..
            })
        ));
    }

    #[test]
    fn malformed_collision_hierarchies_and_vertex_bindings_are_rejected() {
        let original = posed_ragdoll_source(false);
        for invalid in 0..3 {
            let mut smd = original.clone();
            match invalid {
                0 => smd.nodes[0].parent = 1,
                1 => smd.nodes[1].parent = 99,
                _ => smd.triangles[0].vertices[0].links[0].bone = 99,
            }
            let identity = PhyIdentity {
                model_name: "invalid",
                collision_smd_name: "invalid",
                surface_prop: "default",
            };
            assert!(matches!(
                build_ragdoll_phy_from_smd(
                    &smd,
                    identity,
                    0,
                    100.0,
                    &crate::model::Physics::default()
                ),
                Err(PhyError::BadParameter {
                    what: "collision skeleton",
                    ..
                })
            ));
            let identity = PhyIdentity {
                model_name: "invalid",
                collision_smd_name: "invalid",
                surface_prop: "default",
            };
            assert!(matches!(
                build_phy_from_smd(
                    &smd,
                    identity,
                    0,
                    100.0,
                    &crate::model::Physics::default(),
                    None
                ),
                Err(PhyError::BadParameter {
                    what: "collision skeleton",
                    ..
                })
            ));
        }
        // An empty ancestor cycle must also terminate in the infallible grouping API.
        let mut smd = original;
        smd.nodes[1].parent = 2;
        smd.nodes.push(crate::smd::SmdNode {
            index: 2,
            name: "unused".into(),
            parent: 2,
        });
        smd.triangles
            .retain(|triangle| triangle.vertices[0].parent_bone == 1);
        assert_eq!(group_by_bone(&smd).len(), 1);
    }

    /// `build_phy_from_smd`：从 SMD 端到端产出 `.phy`，且 checksum 配对。
    ///
    /// 这是 `mdlc build` 接入 PHY 的入口 —— 官方 QC 的
    /// `$collisionmodel <.smd>` 同样指向一个**单独的 SMD**。
    #[test]
    fn build_phy_from_smd_produces_valid_file() {
        // 一个 8 顶点立方体（12 个三角形），SMD 文本直接手写。
        let mut smd_text = String::from(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\nphys\n",
        );
        let v = cube_vertices();
        for f in cube_faces() {
            for &i in &f {
                let p = v[i as usize];
                smd_text.push_str(&format!("0 {} {} {} 0 0 1 0 0 1 0 1\n", p[0], p[1], p[2]));
            }
        }
        smd_text.push_str("end\n");
        let smd = crate::smd::parse_smd(&smd_text).expect("SMD 应能解析");

        let bytes = build_phy_from_smd(
            &smd,
            PhyIdentity {
                model_name: "testprop",
                collision_smd_name: "testprop",
                surface_prop: "metal",
            },
            0xDEAD_BEEF,
            1.0,
            &crate::model::Physics::default(),
            None,
        )
        .expect("应能写出 PHY");
        let layout = check_invariants(&bytes).expect("写出的 PHY 应通过 13 条硬约束");
        assert_eq!(layout.solid_count, 1, "prop 形态只有 1 个 solid");
        // checksum 必须落在 `phyheader_t` 的 `+0x0C`（不是 `+0x08`）。
        assert_eq!(
            u32::from_le_bytes(bytes[0x0C..0x10].try_into().unwrap()),
            0xDEAD_BEEF,
            "checksum 写在 +0x0C"
        );
        // `phyheader.size == 16`、`id == 0`。
        assert_eq!(i32::from_le_bytes(bytes[0..4].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0);
    }

    /// 空 SMD 必须报错，而不是产出一个畸形文件。
    #[test]
    fn build_phy_rejects_empty_smd() {
        let smd = crate::smd::parse_smd(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\nend\ntriangles\nend\n",
        )
        .expect("SMD 应能解析");
        assert!(
            build_phy_from_smd(
                &smd,
                PhyIdentity {
                    model_name: "x",
                    collision_smd_name: "x",
                    surface_prop: "default",
                },
                0,
                1.0,
                &crate::model::Physics::default(),
                None
            )
            .is_err()
        );
    }

    /// `$concave` 的**连通分量**语义：两个分离的壳 ⟹ 2 个凸块。
    ///
    /// 这是官方与 VHACD 的**分水岭** —— 官方按连通分量拆，
    /// 所以分离的壳一定分开；VHACD 是体逼近，对分离壳也可能给出别的数。
    #[test]
    fn concave_splits_disconnected_shells() {
        // 两个立方体，相距很远（明确不连通）。
        let smd = parse_two_cubes();
        let hulls = decompose_connected_components(&smd, None).expect("应能分解");
        assert_eq!(hulls.len(), 2, "两个分离的壳应产出 2 个凸块");
    }

    /// `$concave` 对**连通**的凹体产出 **1** 个凸块（= 整网格的凸包）。
    ///
    /// ⚠️ 这条是**反直觉**的，但正是官方的行为：
    /// 反编译 `ProcessSingleBody` 确认它是连通分量分解，
    /// 所以连通的凹体不会被拆开，凹处被**填平**。
    ///
    /// 与 `decompose_concave`（VHACD）对比：同一个网格 VHACD 会给多块。
    #[test]
    fn concave_keeps_connected_concave_body_as_single_hull() {
        // 一个 U 形棱柱（凹但连通），光滑着色（法线沿 +Z，同一面上一致）。
        let smd = parse_u_prism();
        let hulls = decompose_connected_components(&smd, None).expect("应能分解");
        assert_eq!(hulls.len(), 1, "连通的凹体在官方 $concave 下是 1 个凸块");

        // 对照：VHACD 会把它拆成多块（凹口保留）。
        let (verts, faces) = weld_smd_triangles(&smd).expect("应能焊接");
        let vhacd = decompose_concave(&verts, &faces, 64, 16).expect("VHACD 应能分解");
        assert!(
            vhacd.len() > 1,
            "VHACD 应把凹体拆成多块（实际 {}）—— 这正是它与官方语义的差别",
            vhacd.len()
        );
    }

    /// 质量默认值是 **1.0**（不是 10.0）。
    ///
    /// 源码：`CJointedModel::CJointedModel()` 的 `m_totalMass = 1.0`，
    /// 而 `ComputeMass()` 首句 `if (m_totalMass >= 0) return;` 直接返回。
    #[test]
    fn default_total_mass_is_one() {
        let p = PhyParams::new("m", 0);
        assert_eq!(p.total_mass, 1.0);
        let b = write_cube();
        let layout = check_invariants(&b).unwrap();
        let text = std::str::from_utf8(&b[layout.solids_end..b.len() - 1]).unwrap();
        assert!(text.contains("\"totalmass\" \"1.000000\""));
    }

    #[test]
    fn mass_text_roundtrips_small_and_high_precision_values() {
        for mass in [1e-7_f32, 1.234_567_8, 37.5] {
            let mut params = PhyParams::new("cube", 0);
            params.total_mass = mass;
            let bytes = write_phy(&[cube()], &[PhySolid::prop("cube")], &params).unwrap();
            let layout = check_invariants(&bytes).unwrap();
            let text = std::str::from_utf8(&bytes[layout.solids_end..bytes.len() - 1]).unwrap();
            let value = |key: &str| {
                let prefix = format!("\"{key}\" \"");
                text.lines()
                    .find_map(|line| line.strip_prefix(&prefix))
                    .unwrap()
                    .trim_end_matches('"')
                    .parse::<f32>()
                    .unwrap()
            };
            assert_eq!(value("totalmass").to_bits(), mass.to_bits());
            assert_eq!(value("mass").to_bits(), mass.max(1.0).to_bits());
            if mass.to_bits() == 37.5_f32.to_bits() {
                assert!(text.contains("\"totalmass\" \"37.500000\""));
                assert!(text.contains("\"mass\" \"37.500000\""));
            }
        }
    }

    /// 解析一个「两个分离立方体」的 SMD。
    fn parse_two_cubes() -> crate::smd::Smd {
        let mut s = String::from(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
        );
        for offset in [[0.0f32, 0.0, 0.0], [100.0, 0.0, 0.0]] {
            let v = cube_vertices();
            for f in cube_faces() {
                // 每个三角形前要有材质行（studiomdl 与 mdlc 都要求）。
                s.push_str("phys\n");
                for &i in &f {
                    let p = v[i as usize];
                    s.push_str(&format!(
                        "0 {} {} {} 0 0 1 0 0 1 0 1\n",
                        p[0] + offset[0],
                        p[1] + offset[1],
                        p[2] + offset[2]
                    ));
                }
            }
        }
        s.push_str("end\n");
        crate::smd::parse_smd(&s).expect("SMD 应能解析")
    }

    /// 解析一个 U 形棱柱（凹、连通、光滑着色）。
    ///
    /// 截面（XZ 平面，CCW）：
    /// ```text
    /// (-2,-2) (2,-2) (2,2) (1,2) (1,0) (-1,0) (-1,2) (-2,2)
    /// ```
    /// 沿 Y 从 −1 拉到 +1。法线全部取 +Z（同一平面上一致 ⟹ 能焊上）。
    fn parse_u_prism() -> crate::smd::Smd {
        let poly: [[f32; 2]; 8] = [
            [-2.0, -2.0],
            [2.0, -2.0],
            [2.0, 2.0],
            [1.0, 2.0],
            [1.0, 0.0],
            [-1.0, 0.0],
            [-1.0, 2.0],
            [-2.0, 2.0],
        ];
        let y0 = -1.0f32;
        let y1 = 1.0f32;
        let n = poly.len();
        let mut tris: Vec<[[f32; 3]; 3]> = Vec::new();
        let bot = |i: usize| [poly[i][0], y0, poly[i][1]];
        let top = |i: usize| [poly[i][0], y1, poly[i][1]];
        for i in 1..n - 1 {
            tris.push([bot(0), bot(i), bot(i + 1)]);
            tris.push([top(0), top(i + 1), top(i)]);
        }
        for i in 0..n {
            let j = (i + 1) % n;
            tris.push([top(i), top(j), bot(j)]);
            tris.push([top(i), bot(j), bot(i)]);
        }
        let mut s = String::from(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
        );
        for t in &tris {
            s.push_str("phys\n");
            for p in t {
                s.push_str(&format!("0 {} {} {} 0 0 1 0 0 1 0 1\n", p[0], p[1], p[2]));
            }
        }
        s.push_str("end\n");
        crate::smd::parse_smd(&s).expect("SMD 应能解析")
    }

    /// 单位立方体的 8 个顶点，坐标取 ±1（与 `phy-probe/gen-phy.js` 的 CUBE_V 同形）。
    fn cube_vertices() -> Vec<[f32; 3]> {
        vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ]
    }

    /// 立方体的 12 个三角面，**CCW 外向**。
    ///
    /// 顶点顺序直接抄自参考实现（那里已经用真实校验器验证过），
    /// 不自己重新推导朝向 —— 推导错了会让整个测试集一起跑偏。
    fn cube_faces() -> Vec<[u32; 3]> {
        vec![
            [4, 5, 6],
            [4, 6, 7], // +Z
            [1, 0, 3],
            [1, 3, 2], // -Z
            [5, 1, 2],
            [5, 2, 6], // +X
            [0, 4, 7],
            [0, 7, 3], // -X
            [7, 6, 2],
            [7, 2, 3], // +Y
            [0, 1, 5],
            [0, 5, 4], // -Y
        ]
    }

    fn cube() -> PhyHull {
        PhyHull {
            vertices: cube_vertices(),
            faces: cube_faces(),
        }
    }

    fn write_cube() -> Vec<u8> {
        let h = cube();
        let s = [PhySolid::prop("cube")];
        let p = PhyParams::new("cube", 0xdead_beef);
        write_phy(std::slice::from_ref(&h), &s, &p).expect("立方体必须能写出")
    }

    /// **`dragAxisAreas` 在文件里的绝对偏移**。
    ///
    /// ⚠️ **不是** `16 + SOLID_HEADER_SIZE`（= 48，那是 `IVP_Compact_Surface`
    /// 的起点）。`dragAxisAreas` 在 **solid 的 32 字节头**里、`+16` 处：
    ///
    /// ```text
    /// +0   i32 size
    /// +4   u32 VPHYSICS_ID
    /// +8   u16 version
    /// +10  i16 modelType
    /// +12  i32 surfaceSize
    /// +16  f32 dragAxisAreas[3]     ← 这里
    /// +28  i32 axisMapSize
    /// ```
    ///
    /// 所以绝对偏移 = `16（phyheader）+ 16` = **32**。
    ///
    /// 写成 `body + 16`（= 64）会读到 `rotation_inertia` —— 症状是
    /// 「立方体上得到 0.0003 而不是 1.0」，**看起来像公式错，其实是偏移错**。
    /// （与 §45.41「偏移错是最会伪装的一类探针 bug」同族。）
    const DRAG_AREAS_OFFSET: usize = 16 + 16;

    // -----------------------------------------------------------------
    // 1. 立方体 → 13 条硬约束自检
    // -----------------------------------------------------------------

    /// **核心判据**：立方体凸包的输出必须通过全部硬约束自检。
    ///
    /// `write_phy` 内部已经调用 `check_invariants`，所以这里再显式跑一遍
    /// 是为了让"这条测试到底在断言什么"一目了然。
    #[test]
    fn cube_passes_all_hard_constraints() {
        let bytes = write_cube();
        let layout = check_invariants(&bytes).expect("13 条硬约束必须全部通过");

        assert_eq!(layout.solid_count, 1);
        assert_eq!(layout.file_size, bytes.len());
        assert_eq!(layout.text_size, bytes.len() - layout.solids_end);
    }

    /// 约束 12：`phyheader` 三个常量字段。
    #[test]
    fn cube_header_matches_reference_bytes() {
        let b = write_cube();
        assert_eq!(read_i32(&b, 0), 16, "phyheader.size 恒为 16");
        assert_eq!(read_i32(&b, 4), 0, "phyheader.id 恒为 0");
        assert_eq!(read_i32(&b, 8), 1, "solidCount");
        assert_eq!(read_u32(&b, 12), 0xdead_beef, "checksum 必须原样写入");
    }

    /// 与参考实现 `gen-phy.js` 的实测对照值逐字段比对。
    ///
    /// 参考实现是**独立于本实现**写出来的（JS，且已用同一个校验器验证过），
    /// 所以这些数字能同时抓住两边的错误。
    #[test]
    fn cube_matches_reference_implementation_layout() {
        let b = write_cube();

        // gen-phy.js: `surfaceSize = 412`、`size = 440`。
        //
        // 推导：1 个 hull，12 三角形、8 个点
        //   ledge 区 = 16(头) + 16×12(三角形) + 16×8(点) = 16 + 192 + 128 = 336
        //   surfaceSize = 48 + 336 + 28×1 = 412
        assert_eq!(read_i32(&b, 16 + 12), 412, "surfaceSize");
        assert_eq!(read_i32(&b, 16), 440, "size == surfaceSize + 28");
        // 约束 2：solid 步长 == size + 4 == surfaceSize + 32。
        assert_eq!(440 + 4, 412 + 32, "solid 步长恒等式");

        let body = 16 + SOLID_HEADER_SIZE;
        assert_eq!(read_u32(&b, 16 + 4), VPHYSICS_ID, "vphysicsID == 'VPHY'");
        assert_eq!(read_u16(&b, 16 + 8), 0x0100, "version");
        assert_eq!(read_i16(&b, 16 + 10), 0, "modelType == COLLIDE_POLY");
        assert_eq!(read_i32(&b, 16 + 28), 0, "axisMapSize");
        assert_eq!(
            read_u32(&b, body + 44),
            IVP_COMPACT_SURFACE_ID,
            "dummy[2] == 'IVPS'"
        );

        // 约束 3：offset_ledgetree_root == 48 + ledge_region_size == 48 + 336。
        assert_eq!(read_i32(&b, body + 32), 384, "offset_ledgetree_root");

        // 头号陷阱：offset_compact_ledge == 48 − offset_ledgetree_root，恒为负。
        // 实测对照：urban_puddle 的 offset_ledgetree_root=480 → 该字段 = −432。
        let node = body + 384;
        assert_eq!(read_i32(&b, node), 0, "单节点树的 offset_right_node == 0");
        assert_eq!(
            read_i32(&b, node + 4),
            48 - 384,
            "offset_compact_ledge 相对节点自身"
        );
        assert_eq!(read_i32(&b, node + 4), -336);
    }

    /// 约束 4 / 5：`c_point_offset` 与 `size_div_16`。
    #[test]
    fn cube_ledge_fields_are_exact() {
        let b = write_cube();
        let region = 16 + SOLID_HEADER_SIZE + COMPACT_SURFACE_SIZE;
        let n_tri = read_i16(&b, region + 12) as usize;
        assert_eq!(n_tri, 12);

        // 单 hull 时 c_point_offset 恰好等于 16 + 16×nTri —— 这正是
        // 规格 §4.2 那条规则成立的唯一场合（多 hull 时它指向共享数组起点）。
        assert_eq!(
            read_i32(&b, region),
            (16 + 16 * n_tri) as i32,
            "单 hull 的 c_point_offset == 16 + 16×nTri"
        );

        let l2 = read_u32(&b, region + 8);
        assert_eq!(l2 & 3, 0, "has_chilren_flag == 0（终结 ledge）");
        assert_eq!((l2 >> 2) & 3, 1, "is_compact_flag == 1");
        assert_eq!((l2 >> 4) & 0xF, 0, "dummy == 0");
        // 约束 5：size_div_16 == 1 + nTri + 本 hull 引用的点数 == 1 + 12 + 8。
        assert_eq!((l2 >> 8) & 0x00FF_FFFF, 1 + 12 + 8, "size_div_16");
        assert_eq!(read_i16(&b, region + 14), 0, "for_future_use == 0");
    }

    /// 约束 7：`tri_index` 在每个 hull 内从 0 起严格递增，
    /// 且 `pierce_index == tri_index`、`material_index == 0`、`is_virtual == 0`。
    #[test]
    fn cube_tri_indices_are_sequential() {
        let b = write_cube();
        let region = 16 + SOLID_HEADER_SIZE + COMPACT_SURFACE_SIZE;
        for k in 0..12usize {
            let w0 = read_u32(&b, region + 16 + k * 16);
            assert_eq!(w0 & 0x0FFF, k as u32, "tri_index[{k}]");
            assert_eq!((w0 >> 12) & 0x0FFF, k as u32, "pierce_index[{k}]");
            assert_eq!((w0 >> 24) & 0x7F, 0, "material_index[{k}]");
            assert_eq!(w0 >> 31, 0, "is_virtual[{k}]");
        }
    }

    // -----------------------------------------------------------------
    // 2. opposite_index 的反向边公式（手算验证）
    // -----------------------------------------------------------------

    /// **手算验证** `opposite_index` 的反向边公式。
    ///
    /// 用一个 4 顶点的正四面体：只有 4 个三角形，`opposite_index` 可以
    /// 逐条手算出来，不依赖任何"跑一遍看结果"的自证。
    ///
    /// 边的线性编号是 `4k + 1 + i`（每个三角形的第 0 个 32 位字被表头占用），
    /// 所以 4 个三角形的 12 条边编号是 1,2,3 / 5,6,7 / 9,10,11 / 13,14,15。
    #[test]
    fn opposite_index_hand_computed_tetrahedron() {
        // 正四面体，CCW 外向。
        let verts = [
            [1.0f32, 1.0, 1.0],
            [1.0, -1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
        ];
        // 顶点表本身不参与公式验证（只验边表），但先确认它确实是 4 个点。
        assert_eq!(verts.len(), 4);
        let faces = [[0u32, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]];

        // 有向边 → (k, i)。手写出来，不用代码推导，这样公式错了测试也会错。
        let dir: HashMap<(u32, u32), (usize, usize)> = [
            ((0, 1), (0usize, 0usize)),
            ((1, 2), (0, 1)),
            ((2, 0), (0, 2)),
            ((0, 3), (1, 0)),
            ((3, 1), (1, 1)),
            ((1, 0), (1, 2)),
            ((0, 2), (2, 0)),
            ((2, 3), (2, 1)),
            ((3, 0), (2, 2)),
            ((1, 3), (3, 0)),
            ((3, 2), (3, 1)),
            ((2, 1), (3, 2)),
        ]
        .into_iter()
        .collect();

        // 每条边 (k,i) 的期望 opposite_index = (4k'+1+i') − (4k+1+i)。
        // 这里逐条列出反向边，再手算差值。
        let expect: [((usize, usize), (usize, usize)); 12] = [
            // (k,i) -> (k',i')，都是把 (u,v) 换成 (v,u)
            ((0, 0), (1, 2)), // 0->1 的反向是 1->0，在三角形 1 的边 2
            ((0, 1), (3, 2)), // 1->2 的反向是 2->1，在三角形 3 的边 2
            ((0, 2), (2, 0)), // 2->0 的反向是 0->2，在三角形 2 的边 0
            ((1, 0), (2, 2)), // 0->3 的反向是 3->0，在三角形 2 的边 2
            ((1, 1), (3, 0)), // 3->1 的反向是 1->3，在三角形 3 的边 0
            ((1, 2), (0, 0)), // 1->0 的反向是 0->1，在三角形 0 的边 0
            ((2, 0), (0, 2)), // 0->2 的反向是 2->0，在三角形 0 的边 2
            ((2, 1), (3, 1)), // 2->3 的反向是 3->2，在三角形 3 的边 1
            ((2, 2), (1, 0)), // 3->0 的反向是 0->3，在三角形 1 的边 0
            ((3, 0), (1, 1)), // 1->3 的反向是 3->1，在三角形 1 的边 1
            ((3, 1), (2, 1)), // 3->2 的反向是 2->3，在三角形 2 的边 1
            ((3, 2), (0, 1)), // 2->1 的反向是 1->2，在三角形 0 的边 1
        ];

        // 先确认手写的边表与输入的面表一致。
        for (k, f) in faces.iter().enumerate() {
            for i in 0..3 {
                let key = (f[i], f[(i + 1) % 3]);
                assert_eq!(dir[&key], (k, i), "手写边表与面表不符：{key:?}");
            }
        }

        for ((k, i), (k2, i2)) in expect {
            let id = |k: usize, i: usize| 4 * k as i32 + 1 + i as i32;
            let hand = id(k2, i2) - id(k, i);
            // 与实现走同一条代码路径。
            let reverse = build_reverse_edges(&faces, 0).unwrap();
            let got_pair = reverse[3 * k + i];
            assert_eq!(
                (got_pair[0] as usize, got_pair[1] as usize),
                (k2, i2),
                "边 ({k},{i}) 的反向边"
            );
            let got = id(got_pair[0] as usize, got_pair[1] as usize) - id(k, i);
            assert_eq!(got, hand, "边 ({k},{i}) 的 opposite_index");
            assert!(
                (-16384..16384).contains(&got),
                "opposite_index {got} 必须塞进 15 位有符号"
            );
        }

        // 抽两个具体的数验证一下算术本身：把编号写死，看差值是不是期望值。
        // 边 (0,0) 编号 1、反向边 (1,2) 编号 7 → 差 6。
        let id = |k: i32, i: i32| 4 * k + 1 + i;
        assert_eq!(id(1, 2) - id(0, 0), 6);
        // 边 (3,2) 编号 15、反向边 (0,1) 编号 2 → 差 −13。
        assert_eq!(id(0, 1) - id(3, 2), -13);
        // 编号必须落在 1..=15（4 个三角形 = 16 个字，第 0 个字是表头）。
        assert_eq!(id(0, 0), 1);
        assert_eq!(id(3, 2), 15);
    }

    /// 写出的四面体里，每一条边的 `opposite_index` 必须与手算值逐位相同。
    #[test]
    fn tetrahedron_written_opposite_indices_match_hand_computation() {
        let h = PhyHull {
            vertices: vec![
                [1.0f32, 1.0, 1.0],
                [1.0, -1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [-1.0, -1.0, 1.0],
            ],
            faces: vec![[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]],
        };
        let s = [PhySolid::prop("tetra")];
        let p = PhyParams::new("tetra", 1);
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();

        let region = 16 + SOLID_HEADER_SIZE + COMPACT_SURFACE_SIZE;
        // 手算：(k,i) 的 opposite_index 的绝对值 = 编号差。
        // 对 (0,0) 期望 +6、(3,2) 期望 −13（见上一条测试的推导）。
        let edge = |k: usize, i: usize| read_u32(&b, region + 16 + k * 16 + 4 + i * 4);
        let opp = |k: usize, i: usize| {
            let raw = (edge(k, i) >> 16) & 0x7FFF;
            if raw & 0x4000 != 0 {
                raw as i32 - 0x8000
            } else {
                raw as i32
            }
        };
        assert_eq!(opp(0, 0), 6);
        assert_eq!(opp(3, 2), -13);
        // 每对反向边的 opposite_index 互为相反数。
        assert_eq!(opp(0, 0), -opp(1, 2));
        assert_eq!(opp(3, 2), -opp(0, 1));
    }

    /// 15 位有符号的边界：`|opposite_index| ≤ 4·nTri − 2`。
    ///
    /// 这条测试把 `MAX_TRIANGLES_PER_HULL = 4096` 这个取值的理由钉住 ——
    /// 报告 §12 写的 8192 会让 `opposite_index` 溢出。
    #[test]
    fn opposite_index_bound_justifies_max_triangles() {
        let max_delta = |n_tri: i64| 4 * (n_tri - 1);
        assert!(
            max_delta(MAX_TRIANGLES_PER_HULL as i64) <= 16383,
            "nTri={MAX_TRIANGLES_PER_HULL} 时最大编号差 {} 必须 ≤ 16383",
            max_delta(MAX_TRIANGLES_PER_HULL as i64)
        );
        // 报告 §12 的 8192 会溢出 —— 这是规格里的一个错误，不是我们的实现问题。
        assert!(
            max_delta(8192) > 16383,
            "8192 个三角形时最大编号差 {} 超出 15 位有符号，说明 §12 的数字是错的",
            max_delta(8192)
        );
    }

    // -----------------------------------------------------------------
    // 3. 错误路径：必须返回 Err 而不是 panic
    // -----------------------------------------------------------------

    /// 空输入。
    #[test]
    fn empty_input_is_rejected() {
        let p = PhyParams::new("x", 0);
        let err = write_phy(&[], &[], &p).unwrap_err();
        assert!(
            matches!(err, PhyError::BadParameter { .. }),
            "实际：{err:?}"
        );

        // 有 solid 参数但没有凸包。
        let s = [PhySolid::prop("x")];
        let err = write_phy(&[], &s, &p).unwrap_err();
        assert!(
            matches!(err, PhyError::BadParameter { .. }),
            "实际：{err:?}"
        );
    }

    /// 凸包与 solid 数量不匹配。
    #[test]
    fn mismatched_counts_are_rejected() {
        let h = cube();
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &[], &p).unwrap_err();
        assert!(
            matches!(err, PhyError::BadParameter { .. }),
            "实际：{err:?}"
        );
    }

    /// 顶点太少（< 4）。
    #[test]
    fn too_few_vertices_is_rejected() {
        let h = PhyHull {
            vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            faces: vec![[0, 1, 2]],
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(err, PhyError::HullTooFewPoints { points: 3, .. }),
            "实际：{err:?}"
        );
    }

    /// **退化三角形**：三个下标里有重复 → 产生自环边 → `DuplicateEdge`。
    #[test]
    fn degenerate_triangle_is_rejected() {
        // 面 [0,0,1] 会产生自环边 0->0。
        let h = PhyHull {
            vertices: cube_vertices(),
            faces: vec![[0, 0, 1]],
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(err, PhyError::DuplicateEdge { from: 0, to: 0, .. }),
            "实际：{err:?}"
        );
    }

    /// 重复的有向边（同一朝向的两个面）→ `DuplicateEdge`。
    #[test]
    fn duplicated_directed_edge_is_rejected() {
        let h = PhyHull {
            vertices: cube_vertices(),
            faces: vec![[0, 1, 2], [0, 1, 2]],
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(err, PhyError::DuplicateEdge { .. }),
            "实际：{err:?}"
        );
    }

    /// **非闭合流形**：少一个面 → 有向边找不到反向边 → `OpenMesh`。
    #[test]
    fn open_mesh_is_rejected() {
        let mut faces = cube_faces();
        faces.pop(); // 去掉一个面，网格出现一个三角形的洞
        let h = PhyHull {
            vertices: cube_vertices(),
            faces,
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(matches!(err, PhyError::OpenMesh { .. }), "实际：{err:?}");
    }

    /// 面表引用了不存在的顶点。
    #[test]
    fn out_of_range_index_is_rejected() {
        let h = PhyHull {
            vertices: cube_vertices(),
            faces: vec![[0, 1, 99]],
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(
                err,
                PhyError::IndexOutOfRange {
                    index: 99,
                    vertex_count: 8,
                    ..
                }
            ),
            "实际：{err:?}"
        );
    }

    /// NaN / 无穷坐标。
    #[test]
    fn non_finite_coordinates_are_rejected() {
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut vs = cube_vertices();
            vs[3][1] = bad;
            let h = PhyHull {
                vertices: vs,
                faces: cube_faces(),
            };
            let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
            assert!(
                matches!(err, PhyError::NonFinitePoint { point: 3, .. }),
                "值 {bad} 应被拒绝，实际：{err:?}"
            );
        }
    }

    /// 包围盒退化（所有顶点重合）→ 必须报错，绝不能 panic。
    ///
    /// # 为什么断言的是"两种错误之一"而不是精确的 `DegenerateBounds`
    ///
    /// `DegenerateBounds` 要求 AABB 半径是 0，也就是该 hull 引用的点**全部
    /// 重合**。但点全部重合意味着每个三角形都退化成 `[0,0,0]`，而流形校验
    /// （在顶点去重**之后**做，见 `prepare_solid`）会先一步报 `DuplicateEdge`。
    ///
    /// 所以经公开 API 走不到 `DegenerateBounds` —— 它是一条防御性分支，
    /// 保留是为了万一将来去重逻辑改了，还有一层兜底。测试在这里断言的是
    /// "退化输入被拒绝且不 panic"，这才是真正要保证的性质。
    #[test]
    fn degenerate_bounds_are_rejected() {
        // 8 个完全相同的点：面表下标各异、看起来闭合，但去重后全部塌成 [0,0,0]。
        let v = vec![[1.0f32, 2.0, 3.0]; 8];
        let h = PhyHull {
            vertices: v,
            faces: cube_faces(),
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(
                err,
                PhyError::DegenerateBounds { .. } | PhyError::DuplicateEdge { .. }
            ),
            "退化的包围盒必须被拒绝，实际：{err:?}"
        );

        // 直接调用内部几何函数时，半径 0 确实走 DegenerateBounds 分支：
        // 手工构造一个 AABB 完全塌缩的节点几何，确认它不会除零 panic。
        let (center, radius, boxes) = node_geometry([1.0, 2.0, 3.0], [1.0, 2.0, 3.0]);
        assert_eq!(center, [1.0, 2.0, 3.0]);
        assert_eq!(radius, 0.0);
        assert_eq!(boxes, [0, 0, 0], "半径 0 时 box_sizes 全 0，不做除零");
    }

    /// 空面表。
    #[test]
    fn empty_faces_are_rejected() {
        let h = PhyHull {
            vertices: cube_vertices(),
            faces: Vec::new(),
        };
        let s = [PhySolid::prop("x")];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(err, PhyError::HullDegenerate { .. }),
            "实际：{err:?}"
        );
    }

    /// 参数越界。
    #[test]
    fn bad_parameters_are_rejected() {
        let h = cube();
        let s = [PhySolid::prop("x")];

        for mass in [0.0f32, -1.0, f32::NAN] {
            let mut p = PhyParams::new("x", 0);
            p.total_mass = mass;
            assert!(
                matches!(
                    write_phy(std::slice::from_ref(&h), &s, &p),
                    Err(PhyError::BadParameter { .. })
                ),
                "total_mass={mass} 应被拒绝"
            );
        }

        let mut p = PhyParams::new("x", 0);
        p.inertia_scale = -1.0;
        assert!(matches!(
            write_phy(std::slice::from_ref(&h), &s, &p),
            Err(PhyError::BadParameter { .. })
        ));
    }

    /// 名字里有 NUL 会截断 text section。
    #[test]
    fn nul_in_name_is_rejected() {
        let h = cube();
        let s = [PhySolid {
            bone_index: None,
            name: "bad\0name".into(),
            parent: None,
            mass_bias: 1.0,
            damping: None,
            rot_damping: None,
            inertia: None,
            surface_prop: None,
        }];
        let p = PhyParams::new("x", 0);
        let err = write_phy(std::slice::from_ref(&h), &s, &p).unwrap_err();
        assert!(
            matches!(err, PhyError::BadParameter { .. }),
            "实际：{err:?}"
        );
    }

    /// 退化点集算凸包：必须返回 Err 而不是 panic。
    ///
    /// 这是选 `try_convex_hull` 而不是 `convex_hull` 的直接理由 ——
    /// 后者在共面/共线输入上会 `unwrap()` 一个 `Err`，直接炸掉。
    #[test]
    fn degenerate_point_sets_do_not_panic() {
        // 全共面（z 恒为 0）。
        let coplanar: Vec<[f32; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.5, 0.5, 0.0],
        ];
        // 共线。
        let collinear: Vec<[f32; 3]> = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        ];
        // 全部重合。
        let coincident: Vec<[f32; 3]> = vec![[1.0, 1.0, 1.0]; 5];

        for (what, pts) in [
            ("共面", coplanar),
            ("共线", collinear),
            ("重合", coincident),
        ] {
            // 只要求"不 panic"：不同 parry 版本对退化的判定可能不同，
            // 断言具体的 Err 变体会让测试变脆。但**绝不能**是 panic。
            match PhyHull::from_points(&pts) {
                Ok(h) => {
                    // 万一算出来了，它必须仍然是可写出的（或者明确报错）。
                    assert!(!h.faces.is_empty(), "{what}：凸包不该没有面");
                }
                Err(e) => {
                    assert!(
                        matches!(
                            e,
                            PhyError::HullDegenerate { .. } | PhyError::HullTooFewPoints { .. }
                        ),
                        "{what}：意外的错误类型 {e:?}"
                    );
                }
            }
        }

        // 少于 4 点必须在进 parry 之前就被拦下。
        assert!(matches!(
            PhyHull::from_points(&[[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
            Err(PhyError::HullTooFewPoints { points: 3, .. })
        ));
    }

    /// 截断/损坏的字节必须报 `SelfCheck` 而不是 panic。
    #[test]
    fn truncated_bytes_are_reported_not_panicking() {
        let b = write_cube();
        // 从尾部一路截断到头部，每一段都必须返回 Err 或 Ok，绝不能 panic。
        for cut in 0..b.len().min(200) {
            let _ = check_invariants(&b[..cut]);
        }
        // 只剩头。
        assert!(matches!(
            check_invariants(&b[..16]),
            Err(PhyError::SelfCheck(_))
        ));
        // 空。
        assert!(matches!(check_invariants(&[]), Err(PhyError::SelfCheck(_))));
    }

    /// 篡改关键字段必须被自检抓到。
    #[test]
    fn tampered_bytes_are_caught() {
        let good = write_cube();

        // phyheader.size 改成别的。
        let mut b = good.clone();
        b[0] = 99;
        assert!(matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))));

        // phyheader.id 改成非 0。
        let mut b = good.clone();
        b[4] = 1;
        assert!(matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))));

        // vphysicsID 改坏。
        let mut b = good.clone();
        b[16 + 4] = 0;
        assert!(matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))));

        // size 与 surfaceSize 的关系被破坏。
        let mut b = good.clone();
        let size = read_i32(&b, 16) + 1;
        b[16..20].copy_from_slice(&size.to_le_bytes());
        assert!(matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))));

        // dummy[2] 不再是 'IVPS'。
        let body = 16 + SOLID_HEADER_SIZE;
        let mut b = good.clone();
        b[body + 44] = 0;
        assert!(matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))));

        // **头号陷阱**：把 offset_compact_ledge 改成 −offset_ledgetree_root
        // （也就是规格警告的那个错误写法），自检必须抓到。
        let node = body + read_i32(&good, body + 32) as usize;
        let mut b = good.clone();
        let wrong = -read_i32(&good, body + 32);
        b[node + 4..node + 8].copy_from_slice(&wrong.to_le_bytes());
        assert!(
            matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))),
            "offset_compact_ledge 写错基准必须被自检抓到"
        );

        // text section 的 NUL 被抹掉。
        let mut b = good.clone();
        let last = b.len() - 1;
        b[last] = b'x';
        assert!(matches!(check_invariants(&b), Err(PhyError::SelfCheck(_))));
    }

    // -----------------------------------------------------------------
    // 4. 长度恒等式
    // -----------------------------------------------------------------

    /// **同一个 `.phy` 里两种单位并存** —— 钉住「text `volume` 是 inch³、
    /// 点数组是 m³」这条容易搞混的规则。
    ///
    /// # 为什么容易搞混
    ///
    /// 点数组由 `vphysics.dll` 的 `CollideWrite` 序列化（IVP 内部单位 = 米），
    /// 而 text 段的 `"volume"` 是 **studiomdl 自己**用
    /// `physcollision->ConvexVolume()`（Source 单位 = inch）算出来再
    /// `fprintf` 的（`collisionmodel.cpp:1174` 累加 → `2310` 打印）。
    /// **两条路径，两种单位，同一个文件。**
    ///
    /// 实测（官方 `msh1.phy`，50×10×50 inch 的长方体）：
    ///
    /// ```text
    /// text 段 "volume"   = 16999.996094   ← 17000 in³
    /// 点数组算出的体积   = 0.278580 m³
    /// 0.278580 / 0.0254³ = 17000.00       ← 换算回 inch³ 完全吻合
    /// ```
    ///
    /// 若把 `volume` 也换算成米（或忘了把点换算成米），
    /// text 段就会与官方差 `61023` 倍 —— 而 `check_invariants` 查不出。
    #[test]
    fn text_volume_is_inches_cubed_while_points_are_metres() {
        let b = write_cube();
        // text 段的 `"volume"`：±1 立方体（边长 2 inch）体积 = 8 in³。
        let text = String::from_utf8_lossy(&b[b.len() - 400..]);
        let m = text.find("\"volume\"").expect("text 段必须有 volume");
        let rest = &text[m + "\"volume\"".len()..];
        // 格式是 `"volume" "8.000000"` —— 取引号之间的数。
        let q1 = rest.find('"').expect("volume 后应有引号");
        let q2 = rest[q1 + 1..].find('"').expect("volume 值应有结束引号");
        let v: f32 = rest[q1 + 1..q1 + 1 + q2].parse().expect("volume 应是数字");
        assert!(
            (v - 8.0).abs() < 1e-3,
            "±1 立方体的 text volume 应为 **8 in³**（不是 8×0.0254³），实际 {v}"
        );

        // 而点数组必须是米：±1 inch → ±0.0254 m。
        let body = 16 + SOLID_HEADER_SIZE;
        let ledge = body + COMPACT_SURFACE_SIZE;
        let n_tri = read_i16(&b, ledge + 0x0C) as usize;
        let pt_base = ledge + LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri;
        let v0 = read_f32(&b, pt_base);
        assert!(
            (v0.abs() - 0.0254).abs() < 1e-9,
            "点数组必须是米（±0.0254），实际 {v0}"
        );
    }

    /// 约束 1：`surfaceSize == 48 + ledge_region_size + 28 × node_count`。
    #[test]
    fn surface_size_identity_holds() {
        let b = write_cube();
        let layout = check_invariants(&b).unwrap();
        for si in 0..layout.solid_count {
            assert_eq!(
                layout.surface_sizes[si] as usize,
                COMPACT_SURFACE_SIZE
                    + layout.ledge_region_sizes[si]
                    + LEDGETREE_NODE_SIZE * layout.node_counts[si],
                "solid[{si}] 的 surfaceSize 恒等式"
            );
        }
    }

    /// 约束 2 + §0：`size == surfaceSize + 28`，`solidsEnd == 16 + Σ(size_i + 4)`，
    /// 且 `solidsEnd + textSize == fileSize`。
    #[test]
    fn file_layout_identities_hold() {
        let b = write_cube();
        let layout = check_invariants(&b).unwrap();

        let size = read_i32(&b, 16);
        assert_eq!(size as usize, layout.surface_sizes[0] as usize + 28);
        // solid 步长 = size + 4 = surfaceSize + 32。
        assert_eq!(size as usize + 4, layout.surface_sizes[0] as usize + 32);

        // solidsEnd = 16 + Σ(size_i + 4)
        let mut expect_end = PHY_HEADER_SIZE;
        let mut base = PHY_HEADER_SIZE;
        for _ in 0..layout.solid_count {
            let s = read_i32(&b, base);
            expect_end += s as usize + 4;
            base += s as usize + 4;
        }
        assert_eq!(layout.solids_end, expect_end, "solidsEnd");
        assert_eq!(
            layout.solids_end + layout.text_size,
            layout.file_size,
            "solidsEnd + textSize == fileSize"
        );
        assert_eq!(layout.file_size, b.len());
    }

    /// 立方体的 ledge 区长度与 `surfaceSize` 可以手算出来。
    ///
    /// ledge 区 = 16(头) + 16×12(三角形) + 16×8(点) = 336；
    /// surfaceSize = 48 + 336 + 28 = 412。
    #[test]
    fn cube_lengths_are_hand_computable() {
        let b = write_cube();
        let layout = check_invariants(&b).unwrap();

        let n_tri = 12usize;
        let n_pts = 8usize;
        let expect_ledge = LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri + POINT_SIZE * n_pts;
        assert_eq!(expect_ledge, 336);
        assert_eq!(layout.ledge_region_sizes[0], expect_ledge);

        let expect_surface = COMPACT_SURFACE_SIZE + expect_ledge + LEDGETREE_NODE_SIZE;
        assert_eq!(expect_surface, 412);
        assert_eq!(layout.surface_sizes[0] as usize, expect_surface);
        assert_eq!(layout.node_counts[0], 1);
    }

    // -----------------------------------------------------------------
    // 5. client_data（§6.1）
    // -----------------------------------------------------------------

    /// §6.1：有骨骼时 `client_data == boneIndex + 1`，无骨骼时 `== 0`。
    ///
    /// 实测 90/90 零例外。
    #[test]
    fn client_data_encodes_bone_index_plus_one() {
        let h = cube();
        let p = PhyParams::new("x", 0);
        let region = 16 + SOLID_HEADER_SIZE + COMPACT_SURFACE_SIZE;

        // 无骨骼 → 0。
        let s = [PhySolid::prop("x")];
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        assert_eq!(
            read_i32(&b, region + 4),
            0,
            "单 solid prop 的 client_data 必须是 0"
        );

        // 有骨骼 → boneIndex + 1。
        for bone in [0u32, 1, 5, 71] {
            let s = [PhySolid::ragdoll(bone, format!("bone{bone}"), None)];
            let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
            assert_eq!(
                read_i32(&b, region + 4),
                bone as i32 + 1,
                "boneIndex={bone} 的 client_data 应为 boneIndex+1"
            );
        }
    }

    // -----------------------------------------------------------------
    // 6. text section（§7）
    // -----------------------------------------------------------------

    /// text section 的形状：以 `solid {` 开头、以 `}\n\0` 结尾、内部无 NUL。
    #[test]
    fn text_section_shape_matches_spec() {
        let b = write_cube();
        let layout = check_invariants(&b).unwrap();
        let text = &b[layout.solids_end..];

        assert_eq!(*text.last().unwrap(), 0, "末尾单个 NUL");
        assert!(!text[..text.len() - 1].contains(&0), "内部无 NUL");
        let body = std::str::from_utf8(&text[..text.len() - 1]).unwrap();
        assert!(body.starts_with("solid {"));
        assert!(body.ends_with("}\n"));
        // KeyValues：键和值都带引号，没有逗号也没有分号。
        assert!(!body.contains(','), "KeyValues 里不该有逗号");
        assert!(!body.contains(';'), "KeyValues 里不该有分号");
        // prop 的 `name` 是**碰撞 SMD 的 basename**（官方 `Q_FileBase`），
        // **不是** `<model>_physbox`。
        //
        // 这里 `write_cube()` 传的碰撞 SMD 名就是 `"cube"` ⟹ 落盘 `"cube"`。
        // 受控实验 `gen_phyname.js` 证实：`$collisionmodel "physign_geo.smd"`
        // 的模型，`.phy` 里写的是 `"physign_geo"` 而不是 `<mdl>_physbox`。
        assert!(body.contains("\"name\" \"cube\""));
        assert!(body.contains("\"index\" \"0\""));
        assert!(body.contains("editparams {"));
        // 单 solid 时 mass == totalmass（studiomdl 的质量分配）。
        //
        // 默认值是 **1.0** —— `CJointedModel` 构造函数 `m_totalMass = 1.0`，
        // 而 `ComputeMass()` 首句 `if (m_totalMass >= 0) return;` 直接返回，
        // 所以**只有显式 `$mass` / `$automass` 才会变**。
        // 语料印证：没有 `.phy` 的 835 个模型里 827 个 `mdl.mass == 1.0`。
        assert!(body.contains("\"mass\" \"1.000000\""));
        assert!(body.contains("\"totalmass\" \"1.000000\""));
        // 没有 `concave` 时不该写 `concave "1"`。
        assert!(!body.contains("concave"));
    }

    /// 多 solid 时每个 solid 一个块，且 `ragdollconstraint` 的 `parent`/`child`
    /// 是 **solid 的 index**（不是骨骼下标）。
    #[test]
    fn multi_solid_text_section_and_constraints() {
        let h = cube();
        let hulls = vec![h.clone(), h.clone(), h];
        let solids = vec![
            PhySolid::ragdoll(3, "root_bone", None),
            PhySolid::ragdoll(7, "mid_bone", Some("root_bone".into())),
            PhySolid::ragdoll(9, "leaf_bone", Some("mid_bone".into())),
        ];
        let p = PhyParams::new("rag", 0x1234);
        let b = write_phy(&hulls, &solids, &p).unwrap();
        let layout = check_invariants(&b).unwrap();
        assert_eq!(layout.solid_count, 3);

        let body = std::str::from_utf8(&b[layout.solids_end..b.len() - 1]).unwrap();
        assert_eq!(body.matches("solid {").count(), 3);
        // 第一个 solid 没有 parent，另外两个有 → 2 个 ragdollconstraint。
        assert_eq!(body.matches("ragdollconstraint {").count(), 2);
        assert!(body.contains("\"parent\" \"0\"\n\"child\" \"1\""));
        assert!(body.contains("\"parent\" \"1\"\n\"child\" \"2\""));
        // 名字与 parent 都按骨骼名写。
        assert!(body.contains("\"name\" \"mid_bone\""));
        assert!(body.contains("\"parent\" \"root_bone\""));

        // 每个 solid 的 client_data 都是它自己的 boneIndex + 1。
        let mut base = PHY_HEADER_SIZE;
        for (i, bone) in [3i32, 7, 9].into_iter().enumerate() {
            let surf = read_i32(&b, base + 12);
            let region = base + SOLID_HEADER_SIZE + COMPACT_SURFACE_SIZE;
            assert_eq!(
                read_i32(&b, region + 4),
                bone + 1,
                "solid[{i}] 的 client_data"
            );
            base += surf as usize + SOLID_HEADER_SIZE;
        }
    }

    // -----------------------------------------------------------------
    // 6b. `$jointconstrain` / `collisionrules` / `animatedfriction` / `jointmerge`
    //     —— 全部数值取自真实 studiomdl 的受控实验（`gen_phy_joint.js`）
    // -----------------------------------------------------------------

    /// 三根链式骨骼的 ragdoll，用于下面几条 text-section 测试。
    fn ragdoll_three() -> (Vec<PhyHull>, Vec<PhySolid>) {
        let h = cube();
        let hulls = vec![h.clone(), h.clone(), h];
        let solids = vec![
            PhySolid::ragdoll(0, "bone_root", None),
            PhySolid::ragdoll(1, "bone_mid", Some("bone_root".into())),
            PhySolid::ragdoll(2, "bone_tip", Some("bone_mid".into())),
        ];
        (hulls, solids)
    }

    fn text_of(hulls: &[PhyHull], solids: &[PhySolid], p: &PhyParams<'_>) -> String {
        let b = write_phy(hulls, solids, p).unwrap();
        check_invariants(&b).unwrap();
        std::str::from_utf8(&b[check_invariants(&b).unwrap().solids_end..b.len() - 1])
            .unwrap()
            .to_string()
    }

    /// `$jointconstrain ... "limit" -10 10 5` ⟹ `xmin=-10 xmax=10 xfriction=**1**`。
    ///
    /// **friction 必须 ÷5** —— 官方 `AddConstraint`（`collisionmodel.cpp:539`）
    /// 是 `friction * (1.0f/5.0f)`，注释说「编辑器里 friction 显示为 5 倍」。
    /// 受控实验 `jc_limit`（真 studiomdl）实测 `xfriction = 1.000000`。
    ///
    /// **反向证伪**：把 `/ 5.0` 去掉，本测试立刻报 `10` ≠ `1`。
    #[test]
    fn jointconstrain_limit_divides_friction_by_five() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.constraints = vec![JointConstraint {
            bone: "bone_mid".into(),
            axis: 0,
            kind: JointLimitType::Limit,
            min: -10.0,
            max: 10.0,
            friction: 5.0,
        }];
        let body = text_of(&hulls, &solids, &p);
        // child=1 是 bone_mid。
        assert!(
            body.contains(
                "\"parent\" \"0\"\n\"child\" \"1\"\n\
                 \"xmin\" \"-10.000000\"\n\"xmax\" \"10.000000\"\n\"xfriction\" \"1.000000\""
            ),
            "limit 约束落盘应为 (-10, 10, 5/5=1)；实际：\n{body}"
        );
        // 未指定的 y/z 轴保持 0。
        assert!(
            body.contains(
                "\"ymin\" \"0.000000\"\n\"ymax\" \"0.000000\"\n\"yfriction\" \"0.000000\""
            )
        );
        assert!(
            body.contains(
                "\"zmin\" \"0.000000\"\n\"zmax\" \"0.000000\"\n\"zfriction\" \"0.000000\""
            )
        );
        // 子 solid（bone_tip，child=2）没有被约束 ⟹ 全 0。
        assert!(body.contains("\"parent\" \"1\"\n\"child\" \"2\"\n\"xmin\" \"0.000000\""));
    }

    /// `free` ⟹ `±360`（**忽略 min/max**）；`fixed` ⟹ 全 `0`（忽略 min/max/friction）。
    ///
    /// 受控实验 `jc_free` / `jc_fixed` 实测：
    /// * `free`  → `xmin=-360 xmax=360 xfriction=0.0`
    /// * `fixed` → 九值全 `0`
    ///
    /// **反向证伪**：若实现照抄 min/max，`free` 会落盘 7/9 而不是 ±360。
    #[test]
    fn jointconstrain_free_and_fixed_ignore_min_max() {
        let (hulls, solids) = ragdoll_three();

        let mut p = PhyParams::new("x", 0);
        p.constraints = vec![JointConstraint {
            bone: "bone_mid".into(),
            axis: 0,
            kind: JointLimitType::Free,
            min: 7.0,
            max: 9.0,
            friction: 0.0,
        }];
        let body = text_of(&hulls, &solids, &p);
        assert!(
            body.contains("\"xmin\" \"-360.000000\"\n\"xmax\" \"360.000000\""),
            "free 必须落盘 ±360（忽略 min/max=7/9）；实际：\n{body}"
        );

        let mut p2 = PhyParams::new("x", 0);
        p2.constraints = vec![JointConstraint {
            bone: "bone_mid".into(),
            axis: 0,
            kind: JointLimitType::Fixed,
            min: 7.0,
            max: 9.0,
            friction: 5.0,
        }];
        let body2 = text_of(&hulls, &solids, &p2);
        assert!(
            body2.contains(
                "\"xmin\" \"0.000000\"\n\"xmax\" \"0.000000\"\n\"xfriction\" \"0.000000\""
            ),
            "fixed 必须落盘全 0（忽略 min/max/friction）；实际：\n{body2}"
        );
    }

    /// 三轴互不干扰：`jc_three` 实测 x=(-10,10,1) y=(-20,20,2) z=(-30,30,3)。
    #[test]
    fn jointconstrain_three_axes_are_independent() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.constraints = vec![
            JointConstraint {
                bone: "bone_mid".into(),
                axis: 0,
                kind: JointLimitType::Limit,
                min: -10.0,
                max: 10.0,
                friction: 5.0,
            },
            JointConstraint {
                bone: "bone_mid".into(),
                axis: 1,
                kind: JointLimitType::Limit,
                min: -20.0,
                max: 20.0,
                friction: 10.0,
            },
            JointConstraint {
                bone: "bone_mid".into(),
                axis: 2,
                kind: JointLimitType::Limit,
                min: -30.0,
                max: 30.0,
                friction: 15.0,
            },
        ];
        let body = text_of(&hulls, &solids, &p);
        assert!(body.contains(
            "\"xmin\" \"-10.000000\"\n\"xmax\" \"10.000000\"\n\"xfriction\" \"1.000000\""
        ));
        assert!(body.contains(
            "\"ymin\" \"-20.000000\"\n\"ymax\" \"20.000000\"\n\"yfriction\" \"2.000000\""
        ));
        assert!(body.contains(
            "\"zmin\" \"-30.000000\"\n\"zmax\" \"30.000000\"\n\"zfriction\" \"3.000000\""
        ));
    }

    /// 约束挂在**子** solid 上：给 `bone_tip` 的约束只出现在 child=2 那块。
    #[test]
    fn jointconstrain_attaches_to_child_solid() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.constraints = vec![JointConstraint {
            bone: "bone_tip".into(),
            axis: 0,
            kind: JointLimitType::Limit,
            min: -7.0,
            max: 7.0,
            friction: 5.0,
        }];
        let body = text_of(&hulls, &solids, &p);
        assert!(body.contains("\"parent\" \"1\"\n\"child\" \"2\"\n\"xmin\" \"-7.000000\""));
        // child=1（bone_mid）那块必须仍是全 0。
        assert!(body.contains("\"parent\" \"0\"\n\"child\" \"1\"\n\"xmin\" \"0.000000\""));
    }

    /// `$noselfcollisions` ⟹ `collisionrules { "selfcollisions" "0" }`，
    /// 且块位置在 `ragdollconstraint` 之后、`editparams` 之前。
    #[test]
    fn no_self_collisions_writes_collisionrules() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.no_self_collisions = true;
        let body = text_of(&hulls, &solids, &p);
        assert!(body.contains("collisionrules {\n\"selfcollisions\" \"0\"\n}\n"));
        let rc = body.find("ragdollconstraint").unwrap();
        let cr = body.find("collisionrules").unwrap();
        let ep = body.find("editparams").unwrap();
        assert!(
            rc < cr && cr < ep,
            "collisionrules 必须夹在 ragdollconstraint 与 editparams 之间"
        );
    }

    /// `$jointcollide` ⟹ `"collisionpair" "<i>,<j>"`（**solid 下标**）。
    ///
    /// 受控实验 `collide` 实测 `collisionpair "1,2"`。
    ///
    /// **反向证伪**：把下标换成骨骼下标会得到 `1,2` 巧合相同 ——
    /// 所以这里用**乱序**骨骼名对（tip, root）验证它用的是 solid 下标
    /// （应得 `2,0` 而不是 `0,2` 或名字）。
    #[test]
    fn jointcollide_writes_solid_indices_in_order() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.collision_pairs = vec![("bone_mid".into(), "bone_tip".into())];
        let body = text_of(&hulls, &solids, &p);
        assert!(
            body.contains("collisionrules {\n\"collisionpair\" \"1,2\"\n}\n"),
            "{body}"
        );

        let mut p2 = PhyParams::new("x", 0);
        p2.collision_pairs = vec![("bone_tip".into(), "bone_root".into())];
        let body2 = text_of(&hulls, &solids, &p2);
        assert!(
            body2.contains("\"collisionpair\" \"2,0\""),
            "顺序必须保持 A,B（不能排序）；实际：\n{body2}"
        );
    }

    /// `no_self_collisions` **优先于** `collision_pairs`（官方 if/else if）。
    #[test]
    fn no_self_collisions_wins_over_collision_pairs() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.no_self_collisions = true;
        p.collision_pairs = vec![("bone_mid".into(), "bone_tip".into())];
        let body = text_of(&hulls, &solids, &p);
        assert!(body.contains("\"selfcollisions\" \"0\""));
        assert!(
            !body.contains("collisionpair"),
            "noself 优先时不该写 collisionpair"
        );
    }

    /// `$animatedfriction` ⟹ 独立的 `animatedfriction {}` 块，
    /// 且 `timeout` / `timehold` 的**落盘顺序**是 In → Out → Hold。
    ///
    /// 受控实验 `animfric`（`$animatedfriction 100 500 0.1 0.2 1.0`）实测：
    /// `timein=0.1`、`timeout=**1.0**`、`timehold=**0.2**` ——
    /// 即 QC 第 5 个参数落进 `timeout`、第 4 个落进 `timehold`（**对调**）。
    #[test]
    fn animated_friction_block_and_field_order() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.animated_friction = Some(AnimatedFriction {
            min: 100,
            max: 500,
            time_in: 0.1,
            // TOML 侧已是语义化名字：time_out 对应 QC 第 5 个参数。
            time_out: 1.0,
            time_hold: 0.2,
        });
        let body = text_of(&hulls, &solids, &p);
        assert!(
            body.contains(
                "animatedfriction {\n\
             \"animfrictionmin\" \"100.000000\"\n\
             \"animfrictionmax\" \"500.000000\"\n\
             \"animfrictiontimein\" \"0.100000\"\n\
             \"animfrictiontimeout\" \"1.000000\"\n\
             \"animfrictiontimehold\" \"0.200000\"\n\
             }\n"
            ),
            "{body}"
        );
    }

    /// `$jointmerge` ⟹ `editparams` 里多一行 `"jointmerge" "<父>,<子>"`。
    #[test]
    fn jointmerge_writes_raw_name_pair() {
        let (hulls, solids) = ragdoll_three();
        let mut p = PhyParams::new("x", 0);
        p.merge_list = vec![("bone_mid".into(), "bone_tip".into())];
        let body = text_of(&hulls, &solids, &p);
        assert!(
            body.contains("\"jointmerge\" \"bone_mid,bone_tip\"\n"),
            "{body}"
        );
    }

    /// `$jointmerge` 的**骨骼归并**：子骨骼的面并入父，solid 数 3 → 2。
    ///
    /// 判据取自受控实验 `merge`（真 studiomdl）：solid 数从 3 降到 2，
    /// 且父骨骼的体积累加。
    ///
    /// **反向证伪**：`group_by_bone`（不传归并表）必须仍得 3 组。
    #[test]
    fn jointmerge_merges_child_faces_into_parent() {
        // 三个盒子分属三根链式骨骼（bone_root / bone_mid / bone_tip）。
        let mut s = String::from("version 1\nnodes\n");
        s.push_str("0 \"bone_root\" -1\n1 \"bone_mid\" 0\n2 \"bone_tip\" 1\n");
        s.push_str("end\nskeleton\ntime 0\n");
        s.push_str("0 0 0 0 0 0 0\n1 0 0 10 0 0 0\n2 0 0 20 0 0 0\n");
        s.push_str("end\ntriangles\n");
        for (bone, cz) in [(0, 0), (1, 10), (2, 20)] {
            let c = cz as f32;
            // 一个 2×2×2 的盒子，六个面（每面两个三角形）。
            let v = [
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, 1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [-1.0, -1.0, 1.0],
                [1.0, -1.0, 1.0],
                [1.0, 1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ];
            let quads = [
                [0, 3, 2, 1],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [2, 3, 7, 6],
                [1, 2, 6, 5],
                [0, 4, 7, 3],
            ];
            for q in quads {
                for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                    s.push_str("phy\n");
                    for i in tri {
                        // SMD 三角形行：`parentBone x y z nx ny nz u v linksCount bone weight`
                        // ⚠️ 末尾的 `linksCount bone weight` 才是**蒙皮权重**，
                        // 而分组读的是 `links[].bone` —— 两者都要写对，
                        // 否则所有面都会归到骨骼 0。
                        s.push_str(&format!(
                            "{bone} {} {} {} 0 0 1 0 0 1 {bone} 1\n",
                            v[i][0],
                            v[i][1],
                            v[i][2] + c
                        ));
                    }
                }
            }
        }
        s.push_str("end\n");
        let smd = crate::smd::parse_smd(&s).expect("SMD 应能解析");

        // 不归并：3 组。
        assert_eq!(group_by_bone(&smd).len(), 3, "不归并时应是 3 根骨骼各一组");

        // 归并 bone_tip(2) → bone_mid(1)：只剩 2 组，
        // 且 bone_mid **累加**两边的面（自己 12 + bone_tip 的 12 = 24）。
        let mut merged = std::collections::HashMap::new();
        merged.insert(2i32, 1i32);
        let g = group_by_bone_merged(&smd, &merged);
        assert_eq!(g.len(), 2, "归并后应只剩 2 组；实际 {g:?}");
        let mid = g
            .iter()
            .find(|x| x.name == "bone_mid")
            .expect("应有 bone_mid");
        assert_eq!(
            mid.faces.len(),
            24,
            "bone_mid 应并吞 bone_tip 的 12 个面（12 自己 + 12 子 = 24）"
        );
        assert!(
            g.iter().all(|x| x.name != "bone_tip"),
            "bone_tip 不该再单独成组"
        );
    }

    /// 归并成环必须**显式报错**，而不是像官方那样静默跳出。
    ///
    /// 语义：`would_cycle(merged, parent, child)` 判断「新增边
    /// `child → parent`」会不会成环 —— 判据是从 `parent` 沿现有归并链
    /// 上溯能否走到 `child`。
    #[test]
    fn jointmerge_cycle_is_rejected() {
        // 已有 `2 → 1`（把骨骼 2 归并进骨骼 1）。
        let mut merged = std::collections::HashMap::new();
        merged.insert(2i32, 1i32);

        // 再加 `1 → 2`（parent=2, child=1）：从 parent=2 上溯会到 1，
        // 而 1 正是 child ⟹ **成环**。
        assert!(would_cycle(&merged, 2, 1, 10), "1→2 与已有的 2→1 成环");

        // `2 → 1`（parent=1, child=2）是重复边 ⟹ 不成环。
        assert!(!would_cycle(&merged, 1, 2, 10), "重复边不算环");

        // `0 → 1`（parent=1, child=0）：从 1 上溯无出边 ⟹ 不成环。
        assert!(!would_cycle(&merged, 1, 0, 10), "0→1 不成环");

        // 自环 `1 → 1` 由调用方提前 `continue` 掉，这里也确认它会被判成环。
        assert!(
            would_cycle(&merged, 1, 1, 10),
            "自环应判成环（调用方另行跳过）"
        );
    }

    /// `physics_bone_table`：**solid 下标 → 骨骼**，未命中的骨骼沿父链上溯。
    ///
    /// 判据取自受控实验（真实 `studiomdl.exe`）：
    /// * `rjd1`（3 骨骼各一个盒子）→ `physicsbone = [0, 1, 2]`
    /// * `rjd2`（中间骨骼**没有几何**）→ `physicsbone = [0, 0, 1]`
    ///   —— `bone_mid` 沿父链上溯到 `bone_root`（下标 0），
    ///   而 `bone_tip` 拿到的 solid 下标是 **1**（不是 2，因为它被跳过了）。
    #[test]
    fn physics_bone_maps_solids_and_walks_up_parents() {
        // 3 根链式骨骼；`bones[k]` 指定第 k 个盒子归属哪根骨骼，
        // 负值表示该盒子不存在。
        let build = |bones: [i32; 3]| {
            let mut s = String::from("version 1\nnodes\n");
            s.push_str("0 \"bone_root\" -1\n1 \"bone_mid\" 0\n2 \"bone_tip\" 1\n");
            s.push_str("end\nskeleton\ntime 0\n");
            s.push_str("0 0 0 0 0 0 0\n1 0 0 10 0 0 0\n2 0 0 20 0 0 0\n");
            s.push_str("end\ntriangles\nphys\n");
            for (k, &bone) in bones.iter().enumerate() {
                if bone < 0 {
                    continue;
                }
                let z = k as f32 * 20.0;
                let verts: Vec<[f32; 3]> = vec![
                    [-5.0, -5.0, z - 5.0],
                    [5.0, -5.0, z - 5.0],
                    [5.0, 5.0, z - 5.0],
                    [-5.0, 5.0, z - 5.0],
                    [-5.0, -5.0, z + 5.0],
                    [5.0, -5.0, z + 5.0],
                    [5.0, 5.0, z + 5.0],
                    [-5.0, 5.0, z + 5.0],
                ];
                let faces: [[usize; 3]; 12] = [
                    [0, 1, 2],
                    [0, 2, 3],
                    [4, 6, 5],
                    [4, 7, 6],
                    [0, 4, 5],
                    [0, 5, 1],
                    [1, 5, 6],
                    [1, 6, 2],
                    [2, 6, 7],
                    [2, 7, 3],
                    [3, 7, 4],
                    [3, 4, 0],
                ];
                for f in faces.iter() {
                    for &vi in f {
                        let p = verts[vi];
                        s.push_str(&format!(
                            "{bone} {} {} {} 0 0 1 0 0 1 {bone} 1\n",
                            p[0], p[1], p[2]
                        ));
                    }
                }
            }
            s.push_str("end\n");
            crate::smd::parse_smd(&s).expect("SMD 应能解析")
        };

        let parents = [-1i32, 0, 1];

        // 三根骨骼各有几何 → 下标就是骨骼序（对照官方 `rjd1`）。
        let smd = build([0, 1, 2]);
        let pb = physics_bone_table(&smd, 3, &parents).expect("应有表");
        assert_eq!(pb, vec![0, 1, 2], "rjd1 形态：physicsbone = [0,1,2]");

        // 中间骨骼没有几何 → 它上溯到 bone_root(0)，
        // 而 bone_tip 拿到的是 **1**（solid 下标，不是骨骼下标）。
        // 对照官方 `rjd2`：`[0, 0, 1]`。
        let smd = build([0, -1, 2]);
        let pb = physics_bone_table(&smd, 3, &parents).expect("应有表");
        assert_eq!(pb, vec![0, 0, 1], "rjd2 形态：中间骨骼上溯到父，下标不跳号");

        // 完全没有碰撞几何 → `None`（官方留全 0）。
        let empty = crate::smd::parse_smd(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\nend\ntriangles\nend\n",
        )
        .expect("SMD 应能解析");
        assert!(physics_bone_table(&empty, 1, &[-1]).is_none());
    }

    /// 单 solid 时 `physicsbone` 必须是**全 0** ——
    /// 实测语料 2459/2459 个单 solid 模型全 0（`probe_physicsbone_ragdoll_split.js`）。
    ///
    /// 这条锁住「只有 ragdoll 才填」这个二分。
    #[test]
    fn physics_bone_is_all_zero_for_single_solid() {
        // 只有 bone_root 有几何，另两根无。
        let s = {
            let mut s = String::from(
                "version 1\nnodes\n0 \"bone_root\" -1\n1 \"bone_mid\" 0\n2 \"bone_tip\" 1\n",
            );
            s.push_str("end\nskeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 10 0 0 0\n2 0 0 20 0 0 0\nend\ntriangles\nphys\n");
            let verts: Vec<[f32; 3]> = vec![
                [-5.0, -5.0, -5.0],
                [5.0, -5.0, -5.0],
                [5.0, 5.0, -5.0],
                [-5.0, 5.0, -5.0],
                [-5.0, -5.0, 5.0],
                [5.0, -5.0, 5.0],
                [5.0, 5.0, 5.0],
                [-5.0, 5.0, 5.0],
            ];
            let faces: [[usize; 3]; 12] = [
                [0, 1, 2],
                [0, 2, 3],
                [4, 6, 5],
                [4, 7, 6],
                [0, 4, 5],
                [0, 5, 1],
                [1, 5, 6],
                [1, 6, 2],
                [2, 6, 7],
                [2, 7, 3],
                [3, 7, 4],
                [3, 4, 0],
            ];
            for f in faces.iter() {
                for &vi in f {
                    let p = verts[vi];
                    s.push_str(&format!("0 {} {} {} 0 0 1 0 0 1 0 1\n", p[0], p[1], p[2]));
                }
            }
            s.push_str("end\n");
            crate::smd::parse_smd(&s).expect("SMD 应能解析")
        };
        let pb = physics_bone_table(&s, 3, &[-1, 0, 1]).expect("应有表");
        assert_eq!(pb, vec![0, 0, 0], "单 solid ⟹ 全 0");
    }

    /// `massbias != 1.0` 时才写 `massbias` 行；`drag` 只在 `Some` 时写；
    /// `concave` 只在 `true` 时写。
    #[test]
    fn optional_text_fields_follow_measured_rules() {
        let h = cube();
        let p_base = PhyParams::new("x", 0);

        // 默认：都不写。
        let s = [PhySolid::prop("x")];
        let b = write_phy(std::slice::from_ref(&h), &s, &p_base).unwrap();
        let layout = check_invariants(&b).unwrap();
        let body = std::str::from_utf8(&b[layout.solids_end..b.len() - 1]).unwrap();
        assert!(!body.contains("massbias"));
        assert!(!body.contains("drag"));
        assert!(!body.contains("concave"));

        // 全部打开。
        let mut p = PhyParams::new("x", 0);
        p.drag = Some(0.25);
        p.concave = true;
        let s = [PhySolid {
            bone_index: None,
            name: "x_physbox".into(),
            parent: None,
            mass_bias: 8.0,
            damping: None,
            rot_damping: None,
            inertia: None,
            surface_prop: None,
        }];
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        let layout = check_invariants(&b).unwrap();
        let body = std::str::from_utf8(&b[layout.solids_end..b.len() - 1]).unwrap();
        assert!(body.contains("\"massbias\" \"8.000000\""));
        assert!(body.contains("\"drag\" \"0.250000\""));
        assert!(body.contains("\"concave\" \"1\""));
    }

    // -----------------------------------------------------------------
    // 7. 多凸块 solid（递归 ledgetree）
    // -----------------------------------------------------------------

    /// 两个凸块拼成一个 solid：必须写成递归树，且节点数是 `2n−1`。
    #[test]
    fn multi_hull_solid_writes_recursive_tree() {
        // 两个分离的立方体。
        let a = cube();
        let mut b_verts = cube_vertices();
        for v in &mut b_verts {
            v[0] += 10.0; // 沿 X 平移 10
        }
        let b = PhyHull {
            vertices: b_verts,
            faces: cube_faces(),
        };

        let solids = [PhySolid::prop("two")];
        let p = PhyParams::new("two", 0);
        let bytes = write_phy_multi(&[vec![a, b]], &solids, &p).unwrap();
        let layout = check_invariants(&bytes).unwrap();

        assert_eq!(layout.solid_count, 1);
        // 2 个凸块 → 2×2−1 = 3 个节点（实测的节点数恒为奇数）。
        assert_eq!(layout.node_counts[0], 3);
        assert_eq!(layout.node_counts[0] % 2, 1, "节点数恒为奇数");

        // 根节点必须是内部节点（offset_right_node != 0）。
        let body = PHY_HEADER_SIZE + SOLID_HEADER_SIZE;
        let node_off = body + read_i32(&bytes, body + 32) as usize;
        let right = read_i32(&bytes, node_off);
        assert_ne!(right, 0, "两个凸块时根节点必须是内部节点");
        assert_eq!(
            right % LEDGETREE_NODE_SIZE as i32,
            0,
            "右子偏移是 28 的倍数"
        );

        // 约束 1 依然成立。
        assert_eq!(
            layout.surface_sizes[0] as usize,
            COMPACT_SURFACE_SIZE
                + layout.ledge_region_sizes[0]
                + LEDGETREE_NODE_SIZE * layout.node_counts[0]
        );

        // 两个 hull 共用一份点数组：共享点数组长度 == 16 个点（2×8，无重合）。
        // 每个 hull 的 c_point_offset 都指向同一个起点。
        let region = body + COMPACT_SURFACE_SIZE;
        let n_tri0 = read_i16(&bytes, region + 12) as usize;
        let hull1 = region + LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri0;
        let n_tri1 = read_i16(&bytes, hull1 + 12) as usize;
        let shared = hull1 + LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri1;
        assert_eq!(
            region as i64 + read_i32(&bytes, region) as i64,
            shared as i64,
            "第一个 hull 的 c_point_offset 指向共享数组起点"
        );
        assert_eq!(
            hull1 as i64 + read_i32(&bytes, hull1) as i64,
            shared as i64,
            "第二个 hull 的 c_point_offset 也指向同一个共享数组起点"
        );
        // 这里就是规格 §4.2 那条规则失效的地方：第二个 hull 的
        // c_point_offset 等于**从它自己到最后一个 ledge 结束**的字节数
        // （= 它自己的 16 + 16×nTri），因为它是最后一个 ledge。
        // 真正的区别在**第一个** hull 上：它的 c_point_offset 要把
        // 后面那个 ledge 也算进去。
        let cpo0 = read_i32(&bytes, region);
        let cpo1 = read_i32(&bytes, hull1);
        assert_eq!(
            cpo0 as usize,
            (LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri0)
                + (LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri1),
            "第一个 hull 的 c_point_offset 必须把后续 ledge 也算进去"
        );
        assert_eq!(
            cpo1 as usize,
            LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri1,
            "最后一个 hull 的 c_point_offset 退化成 16 + 16×nTri"
        );
        // 也就是说：多 hull 时第一个 hull 的 c_point_offset **不等于**
        // 16 + 16×nTri —— 这正是规格 §4.2 只对单 hull 成立的地方。
        assert_ne!(
            cpo0 as usize,
            LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri0,
            "多 hull 时第一个 hull 的 c_point_offset 不应等于 16 + 16×nTri"
        );
    }

    /// 5 个凸块 → 9 个节点，且每个叶子都恰好被引用一次。
    #[test]
    fn five_hulls_produce_nine_nodes() {
        let mut hulls = Vec::new();
        for i in 0..5 {
            let mut vs = cube_vertices();
            for v in &mut vs {
                v[0] += i as f32 * 5.0;
            }
            hulls.push(PhyHull {
                vertices: vs,
                faces: cube_faces(),
            });
        }
        let solids = [PhySolid::prop("five")];
        let p = PhyParams::new("five", 0);
        let bytes = write_phy_multi(&[hulls], &solids, &p).unwrap();
        let layout = check_invariants(&bytes).unwrap();
        assert_eq!(layout.node_counts[0], 9, "5 个凸块 → 2×5−1 = 9 个节点");
    }

    /// `merge_hulls` 拼出来的面表必须仍然闭合，索引必须正确偏移。
    #[test]
    fn merged_hulls_stay_closed() {
        let a = cube();
        let mut b_verts = cube_vertices();
        for v in &mut b_verts {
            v[0] += 10.0;
        }
        let b = PhyHull {
            vertices: b_verts,
            faces: cube_faces(),
        };
        let merged = PhyHull {
            vertices: [a.vertices.clone(), b.vertices.clone()].concat(),
            faces: [
                a.faces.clone(),
                b.faces
                    .iter()
                    .map(|f| [f[0] + 8, f[1] + 8, f[2] + 8])
                    .collect(),
            ]
            .concat(),
        };
        // 合并后依然能通过流形校验并写出。
        let solids = [PhySolid::prop("m")];
        let p = PhyParams::new("m", 0);
        let bytes = write_phy_multi(&[vec![merged]], &solids, &p).unwrap();
        check_invariants(&bytes).unwrap();
    }

    // -----------------------------------------------------------------
    // 8. 几何量的实测规则
    // -----------------------------------------------------------------

    /// **`.phy` 的点是米（IVP 单位），不是 inch** —— 钉住单位换算。
    ///
    /// # 为什么必须有这条测试
    ///
    /// 把点整体乘 39.37 **不破坏 `check_invariants` 的任何一条**：
    /// 点数组长度不变、索引范围不变、`c_point_offset` 后缀和不变、
    /// `surfaceSize == 48 + ledge + 28×nodes` 不变、体积仍是正的。
    ///
    /// 所以这个 bug **不会报错**，只会在游戏里表现为「碰撞体比模型大 39 倍」。
    ///
    /// # 判据：受控实验（已知尺寸）
    ///
    /// 官方 `msh1.phy`（源是 50×10×50 inch 的长方体）实测：
    ///
    /// ```text
    /// 点 bbox = [-0.127, -1.143, -0.127] .. [0.127, 0.127, 1.143]
    /// 各轴范围 = 0.254 / 1.27 / 1.27
    ///          = 10×0.0254 / 50×0.0254 / 50×0.0254
    /// studiomdl 报告体积 = 17000 in^3
    /// 点算出的体积       = 0.2786 m^3 = 17000 × 0.0254³
    /// ```
    ///
    /// 语料全量（`probe_phy_unit.js`，1316 个官方 `.phy`）：
    /// `mdl hull 最大边 / phy 点最大边` 落在 `39.37±25%` 内的 **66.9%**、
    /// 落在 `1.00±0.25` 内的 **0.0%**。
    #[test]
    fn phy_points_are_converted_from_inches_to_metres() {
        let b = write_cube();
        let body = 16 + SOLID_HEADER_SIZE;
        let ledge = body + COMPACT_SURFACE_SIZE;
        let sz_div = read_u32(&b, ledge + 8) >> 8;
        let n_tri = read_i16(&b, ledge + 0x0C) as usize;
        let n_pts = sz_div as usize - 1 - n_tri;
        assert!(n_pts > 0, "立方体应有顶点");

        let pt_base = ledge + LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri;
        // ±1 立方体的点应当是 ±0.0254（= ±1 inch 换算成米）。
        //
        // ⚠️ 这里写**字面量**而不是 `SOURCE_TO_IVP` —— 用常量会让测试变成
        // 恒等式（两边同时改就永远通过）。实测过：把常量改成 1.0 后，
        // 用常量的版本仍然「通过」，写字面量的版本立刻报错。
        const EXPECT: f32 = 0.0254;
        let mut seen_pos = false;
        let mut seen_neg = false;
        for i in 0..n_pts {
            let o = pt_base + i * POINT_SIZE;
            for k in 0..3 {
                let v = read_f32(&b, o + k * 4);
                let want = if v > 0.0 { EXPECT } else { -EXPECT };
                assert!(
                    (v - want).abs() < 1e-9,
                    "点[{i}][{k}] = {v}，应为 ±{EXPECT}（inch→米）。\
                     若这里失败，说明单位换算丢了 —— 而 check_invariants 查不出它"
                );
                if v > 0.0 {
                    seen_pos = true
                } else {
                    seen_neg = true
                }
            }
        }
        assert!(seen_pos && seen_neg, "±1 立方体应当正负都有");
    }

    /// **轴映射**：官方落盘的点是 `(-y, −z, x)(世界顶点) × 0.0254`，
    /// 不是恒等映射。
    ///
    /// # 为什么必须有这条
    ///
    /// 上面那条 `phy_points_are_converted_from_inches_to_metres` 用的是
    /// **±1 立方体** —— 它在 `(-y,−z,x)` 下**完全对称**（顶点集合不变），
    /// 所以那条测试对「轴映射丢了」**完全免疫**：只乘 0.0254 也能通过。
    ///
    /// 这正是这个 bug 能长期存在的原因之一。要抓它必须用**不对称**的形状。
    ///
    /// # 判据来源
    ///
    /// 官方 `physign.phy`（`docs/_probe/gen_physign.js` 生成的四面体，
    /// 序列姿态全 0）。顶点是
    /// `(3,−1,−2) (−4,6,−1) (1,2,7) (−2,−5,3)`，
    /// 24 种纯旋转给出的顶点集合**两两不同**，所以能唯一定案 ——
    /// `docs/_probe/probe_physign.js` 报出**唯一**解 `(-y,−z,x)`。
    ///
    /// 官方落盘值（÷0.0254 后）实测：
    /// ```text
    /// (−2,−7, 1) ( 5,−3,−2) ( 1, 2, 3) (−6, 1,−4)
    /// ```
    #[test]
    fn phy_points_use_official_axis_mapping() {
        // 四面体（与 physign 同形）
        const V: [[f32; 3]; 4] = [
            [3.0, -1.0, -2.0],
            [-4.0, 6.0, -1.0],
            [1.0, 2.0, 7.0],
            [-2.0, -5.0, 3.0],
        ];
        const FACES: [[u32; 3]; 4] = [[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]];

        let mut s = String::from(
            "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n",
        );
        for f in FACES {
            s.push_str("phy\n");
            for &i in &f {
                let p = V[i as usize];
                s.push_str(&format!("0 {} {} {} 0 0 1 0 0 1 0 1\n", p[0], p[1], p[2]));
            }
        }
        s.push_str("end\n");
        let smd = crate::smd::parse_smd(&s).expect("SMD 应能解析");

        let bytes = build_phy_from_smd(
            &smd,
            PhyIdentity {
                model_name: "physign",
                collision_smd_name: "physign",
                surface_prop: "metal",
            },
            0,
            1.0,
            &crate::model::Physics::default(),
            None,
        )
        .expect("应能写出 PHY");
        let body = 16 + SOLID_HEADER_SIZE;
        let ledge = body + COMPACT_SURFACE_SIZE;
        let sz_div = read_u32(&bytes, ledge + 8) >> 8;
        let n_tri = read_i16(&bytes, ledge + 0x0C) as usize;
        let n_pts = sz_div as usize - 1 - n_tri;
        assert_eq!(n_pts, 4, "四面体应有 4 个点");

        // 官方落盘值 ÷0.0254（即轴映射后的 inch 值），按集合比。
        //
        // ⚠️ 写字面量而不是用 `to_ivp()` 反算 —— 后者会让测试变成恒等式。
        const WANT: [[f32; 3]; 4] = [
            [-2.0, -7.0, 1.0],
            [5.0, -3.0, -2.0],
            [1.0, 2.0, 3.0],
            [-6.0, 1.0, -4.0],
        ];
        let pt_base = ledge + LEDGE_HEADER_SIZE + TRIANGLE_SIZE * n_tri;
        let got: Vec<[f32; 3]> = (0..n_pts)
            .map(|i| {
                let o = pt_base + i * POINT_SIZE;
                [
                    read_f32(&bytes, o) / SOURCE_TO_IVP,
                    read_f32(&bytes, o + 4) / SOURCE_TO_IVP,
                    read_f32(&bytes, o + 8) / SOURCE_TO_IVP,
                ]
            })
            .collect();

        for w in WANT {
            assert!(
                got.iter()
                    .any(|g| (0..3).all(|k| (g[k] - w[k]).abs() < 1e-4)),
                "落盘点里找不到 {w:?}。实际点集：{got:?}\n\
                 若全都差一个轴置换，说明 `to_ivp` 的轴映射丢了 —— \
                 只乘 0.0254 会让碰撞体相对渲染网格转 90°。"
            );
        }
    }

    #[test]
    fn compact_outer_hull_preserves_original_when_rebuilt_hull_is_nonconvex() {
        let mut state = 19_u64;
        let mut random = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 32) as u32) as f64 / u32::MAX as f64
        };
        let points = (0..1000)
            .map(|_| {
                let z = random() * 2.0 - 1.0;
                let angle = random() * std::f64::consts::TAU;
                let radius = (1.0 - z * z).sqrt();
                let [x, y, z] = [radius * angle.cos(), radius * angle.sin(), z];
                [
                    (x.signum() * x.abs().powf(0.1) * 300.0) as f32,
                    (y.signum() * y.abs().powf(0.1) * 100.0) as f32,
                    (z.signum() * z.abs().powf(0.1) * 80.0) as f32,
                ]
            })
            .collect::<Vec<_>>();
        let original = PhyHull::from_points(&points).unwrap();
        assert!(original.is_convex());
        let compact = original.compact_outer_hull();
        assert_eq!(compact.vertices, original.vertices);
        assert_eq!(compact.faces, original.faces);
    }

    /// 官方 `ConvexFromVerts` 的第二段 `BuildOuterHull(hull, 0.01)`：
    /// 1 cm 去重必须真的减少点数，且**不改变**凸包的外形。
    ///
    /// # 判据来源
    ///
    /// `docs/_probe/probe_phy_cmp.js`：`msh1` / `ucc1` / `ucc3` / `physign` /
    /// `phyrot0/1/2` 七个模型压实后的**点数与三角形数与官方 `.phy` 完全一致**
    /// （10/16、8/12、8/12、4/4、8/12 ×3），逐点残差 < 1.5e-7。
    ///
    /// 这里用最容易手算的 `physign` 四面体：它的 4 个点两两距离都远大于
    /// 1 cm，所以压实**不应**删掉任何点 —— 这条守的是「别过度删除」。
    #[test]
    fn compact_outer_hull_keeps_well_separated_points() {
        // 四面体，边长 ~10 inch = 0.254 m ≫ 0.01 m
        let pts: Vec<[f32; 3]> = vec![
            [3.0, -1.0, -2.0],
            [-4.0, 6.0, -1.0],
            [1.0, 2.0, 7.0],
            [-2.0, -5.0, 3.0],
        ];
        let h = PhyHull::from_points(&pts).expect("四面体应能成凸包");
        assert_eq!(h.vertices.len(), 4);
        let c = h.compact_outer_hull();
        assert_eq!(c.vertices.len(), 4, "点距远大于 1cm，不该被合并");
        assert_eq!(c.faces.len(), 4, "四面体有 4 个面");
    }

    /// 压实**必须真的删点** —— 一堆挤在 1 cm 内的点应当塌成少数几个。
    ///
    /// 这条守的是反方向：上面那条保证「别过度删除」，这条保证
    /// 「该删就删」。两条合起来才钉住 `COMPACT_RADIUS_M` 的量级。
    #[test]
    fn compact_outer_hull_merges_points_within_one_centimetre() {
        // 一个 10 inch 的立方体，但每个角上再撒 3 个距离 < 0.2 inch
        // （= 0.005 m < 0.01 m）的抖动点。抖动点都在凸包内部，
        // 所以凸包顶点仍是 8 个；压实的意义在**去重半径**，
        // 这里改用「两个几乎重合的立方体叠加」来构造。
        //
        // 更直接的构造：把立方体的 8 个角各自复制一份、偏移 0.1 inch
        // （= 0.00254 m < 0.01 m）。凸包仍是那 8 个角（副本在外侧时
        // 会取代原角），但两两距离 0.00254 m < r ⟹ 压实后点数应减少。
        let mut pts: Vec<[f32; 3]> = Vec::new();
        for &x in &[-1.0f32, 1.0] {
            for &y in &[-1.0f32, 1.0] {
                for &z in &[-1.0f32, 1.0] {
                    pts.push([x, y, z]);
                    pts.push([x + 0.1, y + 0.1, z + 0.1]);
                }
            }
        }
        let h = PhyHull::from_points(&pts).expect("应能成凸包");
        let c = h.compact_outer_hull();
        assert!(
            c.vertices.len() < h.vertices.len(),
            "16 个两两相距 0.1 inch（= 0.00254 m < r=0.01 m）的点应当被压实，\
             实际 {} → {}",
            h.vertices.len(),
            c.vertices.len()
        );
        assert!(
            c.vertices.len() >= 4,
            "压实后仍应是个立体，实际 {} 点",
            c.vertices.len()
        );
    }

    /// 压实的**单位必须是米**，不是 inch。
    ///
    /// # 为什么单列一条
    ///
    /// 反编译里 `BuildOuterHull` 拿到的 `0.01` 是**米** —— 因为
    /// `BuildConvexFromVerts` 已经乘过 0.0254。若误当成 inch 用，
    /// 等效半径会小 39 倍，几乎不删任何点（表现为 `.phy` 点数虚高）。
    ///
    /// # 构造
    ///
    /// 一个四棱双锥：底面正方形 `(±10, ±10, 0)`，两个顶点
    /// `(0, 0, 10)` 与 `(0, 0.2, 10)`。
    ///
    /// * 两顶点相距 **0.2 inch = 0.00508 m**。
    /// * `0.00508 m < 0.01 m` ⟹ **正确实现会合并它们**（6 点 → 5 点）。
    /// * 若把 `0.01` 误当 inch（`0.000254 m`），`0.00508 m` 远大于它
    ///   ⟹ **不会合并**（仍是 6 点）。
    ///
    /// 合并后仍有 5 个点（底面 4 个 + 1 个顶点），够张成凸包，
    /// 所以不会走 `reps.len() < 4` 的退化回退。
    #[test]
    fn compact_radius_is_metres_not_inches() {
        let mut pts: Vec<[f32; 3]> = Vec::new();
        for &x in &[-10.0f32, 10.0] {
            for &y in &[-10.0f32, 10.0] {
                pts.push([x, y, 0.0]);
            }
        }
        pts.push([0.0, 0.0, 10.0]);
        pts.push([0.0, 0.2, 10.0]);

        let h = PhyHull::from_points(&pts).expect("双锥应能成凸包");
        assert_eq!(h.vertices.len(), 6, "底面 4 角 + 2 顶点");
        let c = h.compact_outer_hull();
        assert_eq!(
            c.vertices.len(),
            5,
            "0.2 inch = 0.00508 m < r=0.01 m，两个顶点应当被合并（6 → 5）。\
             实际 {} 点 —— 若没合并，多半是把 0.01 当成 inch 用了。",
            c.vertices.len()
        );
    }

    /// 叶子节点几何：`center` = AABB 中心、`radius` = 外接球半径、
    /// `box_sizes[i] = trunc(半边长/(radius/250)) + 1`。
    ///
    /// 对 ±1 的立方体可以完全手算：half = (1,1,1)、radius = √3、
    /// box_sizes[i] = trunc(1/(√3/250)) + 1 = trunc(144.337) + 1 = 145。
    ///
    /// ⚠️ **半径要乘 [`SOURCE_TO_IVP`]** —— 输入是 Source 单位（inch），
    /// 落盘的几何量是 IVP 单位（米）。`box_sizes` 是**比值**，不受影响。
    #[test]
    fn cube_leaf_node_geometry_is_hand_computable() {
        let b = write_cube();
        let body = 16 + SOLID_HEADER_SIZE;
        let node = body + read_i32(&b, body + 32) as usize;

        let center = [
            read_f32(&b, node + 8),
            read_f32(&b, node + 12),
            read_f32(&b, node + 16),
        ];
        assert_eq!(center, [0.0, 0.0, 0.0], "±1 立方体的 AABB 中心是原点");

        let radius = read_f32(&b, node + 20);
        let want = 3.0f32.sqrt() * SOURCE_TO_IVP;
        assert!(
            (radius - want).abs() < 1e-7,
            "radius 应为 √3 × 0.0254 ≈ {want}，实际 {radius}"
        );

        // `box_sizes` 是「半边长 / (radius/250)」—— 单位在分子分母上抵消。
        let expect = ((1.0f64 / (3.0f64.sqrt() / 250.0)).trunc() + 1.0) as u8;
        assert_eq!(expect, 145);
        assert_eq!([b[node + 24], b[node + 25], b[node + 26]], [145, 145, 145]);
        assert_eq!(b[node + 27], 0, "free_0 == 0");
    }

    /// `upper_limit_radius == max‖p − mass_center‖`。
    ///
    /// 对 ±1 立方体：mass_center 是原点，最远的点是 (±1,±1,±1)，距离 √3。
    /// ⚠️ 同样要乘 [`SOURCE_TO_IVP`]（落盘是米）。
    #[test]
    fn cube_upper_limit_radius_is_hand_computable() {
        let b = write_cube();
        let body = 16 + SOLID_HEADER_SIZE;
        let mc = [
            read_f32(&b, body),
            read_f32(&b, body + 4),
            read_f32(&b, body + 8),
        ];
        assert!(
            mc.iter().all(|c| c.abs() < 1e-8),
            "立方体质心在原点，实际 {mc:?}"
        );
        let r = read_f32(&b, body + 24);
        let want = 3.0f32.sqrt() * SOURCE_TO_IVP;
        assert!(
            (r - want).abs() < 1e-7,
            "upper_limit_radius 应为 √3 × 0.0254 ≈ {want}，实际 {r}"
        );
    }

    /// `max_factor_surface_deviation` 是 250（实测取值范围 135..251，
    /// 250 是出现过的最大值）。
    #[test]
    fn max_factor_surface_deviation_is_250() {
        let b = write_cube();
        let body = 16 + SOLID_HEADER_SIZE;
        assert_eq!(read_u32(&b, body + 28) & 0xFF, 250);
        // 高 24 位是 byte_size == surfaceSize。
        assert_eq!(
            (read_u32(&b, body + 28) >> 8) & 0x00FF_FFFF,
            read_i32(&b, 16 + 12) as u32
        );
    }

    // -----------------------------------------------------------------
    // 9. parry3d 集成
    // -----------------------------------------------------------------

    /// `PhyHull::from_points` 对立方体的 8 个顶点应产出 12 个三角形、8 个顶点。
    #[test]
    fn convex_hull_of_cube_points() {
        let h = PhyHull::from_points(&cube_vertices()).expect("立方体的凸包必须能算出来");
        assert_eq!(h.vertices.len(), 8, "立方体的凸包顶点数");
        assert_eq!(h.faces.len(), 12, "立方体的凸包三角形数");

        // parry 的输出必须是闭合可定向流形，且 CCW 外向（体积为正）。
        let vol = MassProperties::from_convex_polyhedron(
            1.0,
            &h.vertices
                .iter()
                .map(|p| Vector::new(p[0], p[1], p[2]))
                .collect::<Vec<_>>(),
            &h.faces,
        )
        .mass();
        assert!(vol > 0.0, "凸包必须 CCW 外向（有向体积为正），实际 {vol}");
        // 边长 2 的立方体体积是 8。
        assert!((vol - 8.0).abs() < 1e-4, "立方体体积应为 8，实际 {vol}");
    }

    /// 凸包输入里带冗余内部点时，凸包必须把它们丢掉。
    #[test]
    fn convex_hull_drops_interior_points() {
        let mut pts = cube_vertices();
        pts.push([0.0, 0.0, 0.0]); // 内部点
        pts.push([0.1, 0.1, 0.1]); // 内部点
        let h = PhyHull::from_points(&pts).unwrap();
        assert_eq!(h.vertices.len(), 8, "内部点必须被丢掉");
        assert_eq!(h.faces.len(), 12);
    }

    /// VHACD 凸分解：对一个凹的 L 形网格必须产出多个凸块，
    /// 且每块都能作为合法的 `PhyHull` 写进 PHY。
    #[test]
    fn vhacd_decomposes_concave_mesh() {
        // 一个 L 形棱柱（沿 Z 挤出）。这是经典的凹形状。
        let outline: [[f32; 2]; 6] = [
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ];
        let mut vertices: Vec<[f32; 3]> = Vec::new();
        for z in [0.0f32, 1.0] {
            for p in &outline {
                vertices.push([p[0], p[1], z]);
            }
        }
        let n = outline.len() as u32;
        let mut faces: Vec<[u32; 3]> = Vec::new();
        // 底面（法线 -Z，所以绕序反转）与顶面（+Z）。
        for i in 1..n - 1 {
            faces.push([0, i + 1, i]);
            faces.push([n, n + i, n + i + 1]);
        }
        // 侧面。
        for i in 0..n {
            let j = (i + 1) % n;
            faces.push([i, j, n + j]);
            faces.push([i, n + j, n + i]);
        }

        let parts = decompose_concave(&vertices, &faces, 32, 8).expect("L 形必须能分解");
        assert!(parts.len() >= 2, "凹形状应拆成 ≥2 块，实际 {}", parts.len());
        for (i, p) in parts.iter().enumerate() {
            assert!(p.vertices.len() >= 4, "第 {i} 块的顶点数");
            assert!(!p.faces.is_empty(), "第 {i} 块的面表");
        }

        // 所有块必须能作为一个 solid 写进 PHY 并通过自检。
        let solids = [PhySolid::prop("lshape")];
        let mut params = PhyParams::new("lshape", 0);
        params.concave = true;
        let bytes = write_phy_multi(&[parts], &solids, &params).expect("分解结果必须能写出");
        let layout = check_invariants(&bytes).unwrap();
        assert_eq!(layout.solid_count, 1);
        assert_eq!(layout.node_counts[0] % 2, 1, "节点数恒为奇数");
    }

    /// 凸分解的空/坏输入必须返回 Err。
    #[test]
    fn decompose_rejects_bad_input() {
        assert!(matches!(
            decompose_concave(&[], &[], 32, 8),
            Err(PhyError::BadParameter { .. })
        ));
        assert!(matches!(
            decompose_concave(&[[0.0; 3], [1.0, 0.0, 0.0]], &[[0, 1, 0]], 32, 8),
            Err(PhyError::HullTooFewPoints { .. })
        ));
        // 面表引用越界下标。
        assert!(matches!(
            decompose_concave(&cube_vertices(), &[[0u32, 1, 99]], 32, 8),
            Err(PhyError::IndexOutOfRange { index: 99, .. })
        ));
    }

    // -----------------------------------------------------------------
    // 10. 确定性
    // -----------------------------------------------------------------

    /// 同样的输入必须逐字节产出同样的输出。
    ///
    /// 这条很重要：`points` 去重用了 `HashMap`，如果迭代顺序泄漏进输出，
    /// 每次编译都会得到不同的 `.phy`，增量构建和校验都会变得不可复现。
    #[test]
    fn output_is_deterministic() {
        let first = write_cube();
        for _ in 0..8 {
            assert_eq!(write_cube(), first, "重复写出必须逐字节相同");
        }
    }

    /// 顶点顺序不同（但几何相同）时输出可以不同，但都必须合法。
    #[test]
    fn permuted_vertices_still_valid() {
        // 把立方体的顶点顺序反过来（面表跟着重映射）。
        let map: Vec<u32> = (0..8).rev().collect();
        let mut vs = cube_vertices();
        vs.reverse();
        let faces = cube_faces()
            .into_iter()
            .map(|f| [map[f[0] as usize], map[f[1] as usize], map[f[2] as usize]])
            .collect();
        let h = PhyHull {
            vertices: vs,
            faces,
        };
        let s = [PhySolid::prop("perm")];
        let p = PhyParams::new("perm", 7);
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        let layout = check_invariants(&b).unwrap();
        // 几何没变，所以 surfaceSize 应该一样。
        assert_eq!(layout.surface_sizes[0], 412);
    }

    /// `inertia_scale = 0.0` 时 `rotation_inertia` 三个分量都是 0。
    #[test]
    fn inertia_scale_zero_writes_zeros() {
        let h = cube();
        let s = [PhySolid::prop("x")];
        let mut p = PhyParams::new("x", 0);
        p.inertia_scale = 0.0;
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        let body = 16 + SOLID_HEADER_SIZE;
        for a in 0..3 {
            assert_eq!(
                read_f32(&b, body + 12 + a * 4),
                0.0,
                "rotation_inertia[{a}]"
            );
        }
    }

    /// `rotation_inertia` = 官方的 `[√(B²+C²), √(A²+C²), √(A²+B²)]`（R24）。
    ///
    /// 这个立方体的坐标是 ±1，**边长是 2**。
    /// 边长 `a` 的立方体、密度 1：`A = B = C = a²/12`（单位 m²，**已除以体积**）。
    ///
    /// ⚠️ **除以体积后数值随 `SOURCE_TO_IVP²` 缩放，不是 `⁵`。**
    /// 点缩放 `k`：`I → I·k⁵`、`V → V·k³` ⟹ `I/V → (I/V)·k²`。
    /// （旧注释写的 `k⁵` 是**未除体积**时的口径 —— R24 之前这里就是错的。）
    ///
    /// 边长 2（inch 口径）：`A = B = C = 4/12 = 1/3`（inch²）。
    /// 换算成米：`1/3 × 0.0254² = 2.150533e-4`（m²）。
    /// 三个轴相同（各向同性）⟹ `√(A²+A²) = A·√2`。
    ///
    /// 实测锚点（官方 `docs/_probe/artifacts/msh1.phy`）：
    /// `A = 5.376333e-3`、`B = C = 1.099017e-1`
    /// ⟹ `rot[0] = √(B²+C²) = 1.554245e-1`（官方实测 `1.554245e-1`，相对差 **1.3e-7**）。
    #[test]
    fn cube_rotation_inertia_matches_official_hypotenuse_law() {
        let b = write_cube();
        let body = 16 + SOLID_HEADER_SIZE;
        // 边长 2、密度 1：A = B = C = a²/12 = 4/12 = 1/3（inch²）
        let second_moment = (2.0f32 * 2.0 / 12.0) * SOURCE_TO_IVP.powi(2);
        // 立方体各向同性 ⟹ rot[k] = √(A²+A²) = A·√2
        let expect = second_moment * std::f32::consts::SQRT_2;
        for a in 0..3 {
            let got = read_f32(&b, body + 12 + a * 4);
            let rel = if expect != 0.0 {
                (got - expect).abs() / expect
            } else {
                0.0
            };
            assert!(
                rel < 1e-4,
                "立方体的 rot[{a}] 应为 A·√2 = {expect}（A = a²/12 × 0.0254²），实际 {got}"
            );
        }
        // 三个轴必须相同（立方体各向同性）—— 这是真正重要的性质。
        // 容差放到 1e-4：parry 的特征分解是迭代的，实测三个轴相差约 2e-6。
        let i0 = read_f32(&b, body + 12);
        let i1 = read_f32(&b, body + 16);
        let i2 = read_f32(&b, body + 20);
        assert!(
            (i0 - i1).abs() < 1e-4 && (i1 - i2).abs() < 1e-4,
            "立方体各向同性：I = ({i0}, {i1}, {i2})"
        );
    }

    /// **`rotation_inertia` 必须已除以体积**（R24 的核心回归）。
    ///
    /// 判据：把同一个几何**整体缩放 `k` 倍**，`I/V` 必须随 `k²` 缩放
    /// （因为 `I → I·k⁵`、`V → V·k³`）。
    ///
    /// ⚠️ 修前写的是**未除体积**的 `I` ⟹ 随 `k⁵` 缩放。
    /// 这条测试**只靠缩放指数**就能分辨两种口径，不需要 oracle ——
    /// 是「自相矛盾的文件自己就能证明自己坏」那一类判据。
    #[test]
    fn rotation_inertia_scales_as_squared_length() {
        let read_rot = |scale: f32| -> [f32; 3] {
            let mut vs = cube_vertices();
            for v in &mut vs {
                for c in v.iter_mut() {
                    *c *= scale;
                }
            }
            let h = PhyHull {
                vertices: vs,
                faces: cube_faces(),
            };
            let s = [PhySolid::prop("scaled")];
            let p = PhyParams::new("scaled", 0);
            let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
            let body = 16 + SOLID_HEADER_SIZE;
            [
                read_f32(&b, body + 12),
                read_f32(&b, body + 16),
                read_f32(&b, body + 20),
            ]
        };
        let r1 = read_rot(1.0);
        let r3 = read_rot(3.0);
        assert!(r1[0] > 0.0 && r3[0] > 0.0, "两个缩放下都应写出正惯量");
        // 期望 r3/r1 == 3² = 9（`I/V` 是 m²）。
        let ratio = r3[0] / r1[0];
        assert!(
            (ratio - 9.0).abs() / 9.0 < 1e-3,
            "`I/V` 应随 `k²` 缩放：k=3 时比值应为 9，实际 {ratio}（\
             若约 243 = 3⁵ 则说明**忘了除以体积**）"
        );
    }

    /// **`rotation_inertia` 用 `√(B²+C²)` 而不是 `B+C`**（R24 的第二处）。
    ///
    /// 判据：**长方体**（各向异性）上，`√(B²+C²) < B+C` 严格成立
    /// ⟹ 若实现写成 `B+C`，写出值会**偏大**，且比值恰为 `(B+C)/√(B²+C²)`。
    ///
    /// 用 4:1:1 的长方体（X 拉长 4 倍，Y/Z 不变）：
    /// `A=4α, B=C=α` ⟹ `rot[0] = √(α²+α²) = α√2`，而 `B+C = 2α`。
    /// 比值 `2α/(α√2) = √2 = 1.4142136` —— 用这个**精确常数**判定。
    #[test]
    fn rotation_inertia_uses_hypotenuse_not_sum() {
        // X 方向拉长 4 倍的长方体。
        let mut vs = cube_vertices();
        for v in &mut vs {
            v[0] *= 4.0;
        }
        let h = PhyHull {
            vertices: vs,
            faces: cube_faces(),
        };
        let s = [PhySolid::prop("aniso")];
        let p = PhyParams::new("aniso", 0);
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        let body = 16 + SOLID_HEADER_SIZE;
        let got = [
            read_f32(&b, body + 12),
            read_f32(&b, body + 16),
            read_f32(&b, body + 20),
        ];
        // 参考：立方体（±1，边长 2）的 A = B = C = 4/12 = 1/3（inch²）。
        // 拉长 4 倍后 X 轴二阶矩 ×16 ⟹ A = 16/3，B = C = 1/3（inch²）。
        let k2 = SOURCE_TO_IVP * SOURCE_TO_IVP;
        let a = 16.0f32 / 3.0 * k2;
        let bc = 1.0f32 / 3.0 * k2;
        let expect = [
            (bc * bc + bc * bc).sqrt(), // √(B²+C²)
            (a * a + bc * bc).sqrt(),   // √(A²+C²)
            (a * a + bc * bc).sqrt(),   // √(A²+B²)
        ];
        for i in 0..3 {
            let rel = (got[i] - expect[i]).abs() / expect[i];
            assert!(
                rel < 1e-3,
                "rot[{i}] 应为 √(两项²) = {}，实际 {}（相对差 {rel}）",
                expect[i],
                got[i]
            );
        }
        // 关键判据：x 轴必须**小于** `B+C`，且比值恰为 √2。
        let sum_bc = 2.0 * bc;
        let ratio = sum_bc / got[0];
        assert!(
            (ratio - std::f32::consts::SQRT_2).abs() < 1e-3,
            "x 轴应为 √(B²+C²) 而非 B+C：`(B+C)/rot[0]` 应为 √2 = {}，实际 {ratio}",
            std::f32::consts::SQRT_2
        );
    }

    /// 长方体（非立方体）的 `rotation_inertia` 必须体现各向异性，
    /// 且长轴方向的惯量最小。
    ///
    /// ⚠️ **`ix < iy` 这条判据对 `B+C` 与 `√(B²+C²)` 两种口径都成立**
    /// （两者都是单调的），所以它**测不出** R24 —— 变异测试实测：
    /// 三个变异体下这条都通过。它的价值是钉住「各向异性方向」，
    /// 不是钉住「用和还是斜边」。后者由
    /// [`rotation_inertia_uses_hypotenuse_not_sum`] 负责。
    ///
    /// 这里**额外**用 4:1:1 的精确闭式值钉住数值本身，让它也能抓到 R24。
    #[test]
    fn box_rotation_inertia_is_anisotropic() {
        // X 方向拉长 4 倍。
        let mut vs = cube_vertices();
        for v in &mut vs {
            v[0] *= 4.0;
        }
        let h = PhyHull {
            vertices: vs,
            faces: cube_faces(),
        };
        let s = [PhySolid::prop("box")];
        let p = PhyParams::new("box", 0);
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        let body = 16 + SOLID_HEADER_SIZE;
        let (ix, iy, iz) = (
            read_f32(&b, body + 12),
            read_f32(&b, body + 16),
            read_f32(&b, body + 20),
        );
        // 边长 (8, 2, 2)：I_x = m(2²+2²)/12 最小，I_y = I_z = m(8²+2²)/12 最大。
        assert!(ix < iy, "绕长轴的惯量应最小：Ix={ix} Iy={iy}");
        assert!((iy - iz).abs() < 1e-3, "另外两轴应相同：Iy={iy} Iz={iz}");
        // 精确闭式：立方体（±1）的 A = B = C = 1/3 inch²；X 拉长 4 倍 ⟹ A ×16。
        let k2 = SOURCE_TO_IVP * SOURCE_TO_IVP;
        let a = 16.0f32 / 3.0 * k2;
        let bc = 1.0f32 / 3.0 * k2;
        let expect = [
            (bc * bc + bc * bc).sqrt(),
            (a * a + bc * bc).sqrt(),
            (a * a + bc * bc).sqrt(),
        ];
        for (i, (got, want)) in [(ix, expect[0]), (iy, expect[1]), (iz, expect[2])]
            .into_iter()
            .enumerate()
        {
            let rel = (got - want).abs() / want;
            assert!(
                rel < 1e-3,
                "rot[{i}] 应为 √(两项²) = {want}，实际 {got}（相对差 {rel}）"
            );
        }
    }

    /// **`dragAxisAreas` 必须算出真的分数，而不是恒 `[1,1,1]`**（R25）。
    ///
    /// # 解析锚点（**不需要 oracle**）
    ///
    /// 轴对齐长方体上，沿轴 `k` 的正交投影面积**恰好等于** OBB 截面积
    /// `size[j]·size[l]` ⟹ 三个分数**必须全是 `1.0`**。
    ///
    /// 这条是「公式对不对」的最强判据：它不依赖官方那套射线投射近似。
    /// 语料实测 **64/64 长方体全 1**。
    #[test]
    fn drag_axis_areas_are_one_for_boxes() {
        // 立方体（±1，边长 2）。
        let b = write_cube();
        for k in 0..3 {
            let got = read_f32(&b, DRAG_AREAS_OFFSET + k * 4);
            assert!(
                (got - 1.0).abs() < 1e-5,
                "长方体/立方体的 drag[{k}] 应为 1.0（投影面积 == OBB 截面积），实际 {got}"
            );
        }
        // 非立方体的长方体（X 拉长 4 倍）也必须是全 1 —— 这条才排除
        // 「只是碰巧立方体对称」的解释。
        let mut vs = cube_vertices();
        for v in &mut vs {
            v[0] *= 4.0;
        }
        let h = PhyHull {
            vertices: vs,
            faces: cube_faces(),
        };
        let s = [PhySolid::prop("box")];
        let p = PhyParams::new("box", 0);
        let b2 = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        for k in 0..3 {
            let got = read_f32(&b2, DRAG_AREAS_OFFSET + k * 4);
            assert!(
                (got - 1.0).abs() < 1e-5,
                "4:1:1 长方体的 drag[{k}] 应为 1.0，实际 {got}"
            );
        }
    }

    /// **轴序必须是 `[0, 2, 1]`**（R25）。
    ///
    /// 官方的 `size` 是 **Source 轴序**的包围盒，而点数组已经过
    /// `to_ivp` 的 `(x,−z,y)` 映射 ⟹ 两个轴序差一个 `[0,2,1]`。
    /// 推导见 [`drag_axis_areas`] 的文档。
    ///
    /// 夹具用**两个尺寸不同的长方体**（都按 IVP 轴序给，再逆映射回去），
    /// 使三个 `ratioI` 两两不同 —— 否则长方体/八面体/棱柱**都测不出轴序**
    /// （它们的投影面积恰好等于 OBB 截面积，`ratioI` 恒等）。
    ///
    /// 手算（IVP 轴序）：
    ///
    /// ```text
    ///   hull A: x∈[0,1] y∈[0,1] z∈[0,1]   → proj (1, 1, 1)
    ///   hull B: x∈[0,1] y∈[0,2] z∈[3,4]   → proj (2, 1, 2)
    ///   合并 bbox size = (1, 2, 4)
    ///   proj   = (3, 2, 3)
    ///   crossI = (2·4, 4·1, 1·2) = (8, 4, 2)
    ///   ratioI = (0.375, 0.5, 1.5 → 钳 1)
    ///   drag   = (ratioI[0], ratioI[2], ratioI[1]) = (0.375, 1.0, 0.5)
    /// ```
    ///
    /// 轴序若写成恒等，`drag[1]` 与 `drag[2]` 会**互换**。
    #[test]
    fn drag_axis_areas_use_the_source_axis_order() {
        // `to_ivp(v) = 0.0254 · (−v.y, −v.z, v.x)` 的逆。
        let inv = |q: [f32; 3]| -> [f32; 3] {
            [
                q[2] / SOURCE_TO_IVP,
                -q[0] / SOURCE_TO_IVP,
                -q[1] / SOURCE_TO_IVP,
            ]
        };
        let mk = |ivp: [[f32; 3]; 8]| PhyHull {
            vertices: ivp.iter().map(|q| inv(*q)).collect(),
            faces: cube_faces(),
        };
        // 两个长方体，**IVP 轴序**给出。
        let hull_a = mk([
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ]);
        let hull_b = mk([
            [0.0, 0.0, 3.0],
            [1.0, 0.0, 3.0],
            [1.0, 2.0, 3.0],
            [0.0, 2.0, 3.0],
            [0.0, 0.0, 4.0],
            [1.0, 0.0, 4.0],
            [1.0, 2.0, 4.0],
            [0.0, 2.0, 4.0],
        ]);
        let s = [PhySolid::prop("axisorder")];
        let p = PhyParams::new("axisorder", 0);
        // 两个凸块**同属一个 solid**（`write_phy` 把 `hulls` 当一个 solid 的凸块表，
        // 但要求 `solids.len() == hulls.len()`）⟹ 用 `write_phy_multi` 显式分组。
        let b = write_phy_multi(&[vec![hull_a, hull_b]], &s, &p).unwrap();
        let got = [
            read_f32(&b, DRAG_AREAS_OFFSET),
            read_f32(&b, DRAG_AREAS_OFFSET + 4),
            read_f32(&b, DRAG_AREAS_OFFSET + 8),
        ];
        // 核心判据：第 1 与第 2 位必须不同（轴序写错时互换）。
        assert!(
            (got[1] - got[2]).abs() > 0.05,
            "drag[1] 与 drag[2] 必须不同（期望 1.0 vs 0.5）—— \
             若相同说明夹具对称、测不出轴序；实际 {got:?}"
        );
        let expect = [0.375f32, 1.0, 0.5];
        for k in 0..3 {
            assert!(
                (got[k] - expect[k]).abs() < 0.02,
                "drag[{k}] 应为 {:.3}（手算 ratioI[SRC[{k}]]），实际 {:.6}",
                expect[k],
                got[k]
            );
        }
    }

    #[test]
    fn drag_axis_areas_drop_below_one_for_octahedron() {
        // 八面体：6 个顶点在 ±1 轴上，8 个三角面。
        let vs = vec![
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        let faces = vec![
            [0, 2, 4],
            [2, 1, 4],
            [1, 3, 4],
            [3, 0, 4],
            [2, 0, 5],
            [1, 2, 5],
            [3, 1, 5],
            [0, 3, 5],
        ];
        let h = PhyHull {
            vertices: vs,
            faces,
        };
        let s = [PhySolid::prop("octa")];
        let p = PhyParams::new("octa", 0);
        let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
        let got = [
            read_f32(&b, DRAG_AREAS_OFFSET),
            read_f32(&b, DRAG_AREAS_OFFSET + 4),
            read_f32(&b, DRAG_AREAS_OFFSET + 8),
        ];
        // 八面体各轴等价 ⟹ 三个分数都应为 0.5（菱形/正方形）。
        for (k, v) in got.iter().enumerate() {
            assert!(
                (v - 0.5).abs() < 1e-4,
                "八面体的 drag[{k}] 应为 0.5（菱形投影面积 2 / 正方形 4），实际 {v}"
            );
        }
    }

    /// **`dragAxisAreas` 必须随几何缩放不变**（R25）。
    ///
    /// 它是**比值**（投影面积 / 截面积，量纲相消）⟹ 整体缩放 `k` 倍后
    /// 三个分数**逐位不变**。
    ///
    /// ⚠️ 这条能抓住「误把长度单位换算进去」这类错误 ——
    /// 若实现里混进了 `SOURCE_TO_IVP` 的幂次，缩放就会改变结果。
    #[test]
    fn drag_axis_areas_are_scale_invariant() {
        // 八面体（`|x|+|y|+|z| ≤ 1`）缩放 `scale` 倍 —— 三个分数都应为 0.5，
        // 与 `scale` 无关。用八面体而不是切角立方体，因为**切角不改变投影面积**
        // （见 `drag_axis_areas_drop_below_one_for_octahedron` 的说明）。
        let read_drag = |scale: f32| -> [f32; 3] {
            let vs = vec![
                [scale, 0.0, 0.0],
                [-scale, 0.0, 0.0],
                [0.0, scale, 0.0],
                [0.0, -scale, 0.0],
                [0.0, 0.0, scale],
                [0.0, 0.0, -scale],
            ];
            let faces = vec![
                [0, 2, 4],
                [2, 1, 4],
                [1, 3, 4],
                [3, 0, 4],
                [2, 0, 5],
                [1, 2, 5],
                [3, 1, 5],
                [0, 3, 5],
            ];
            let h = PhyHull {
                vertices: vs,
                faces,
            };
            let s = [PhySolid::prop("scaled")];
            let p = PhyParams::new("scaled", 0);
            let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
            [
                read_f32(&b, DRAG_AREAS_OFFSET),
                read_f32(&b, DRAG_AREAS_OFFSET + 4),
                read_f32(&b, DRAG_AREAS_OFFSET + 8),
            ]
        };
        let d1 = read_drag(1.0);
        let d7 = read_drag(7.0);
        // 先证明夹具非空：分数必须都不是 1（否则不变性判据恒真）。
        for (k, v) in d1.iter().enumerate() {
            assert!(
                (v - 0.5).abs() < 1e-4,
                "夹具的 drag[{k}] = {v} 应为 0.5 —— 否则不变性判据空洞"
            );
        }
        for k in 0..3 {
            assert!(
                (d1[k] - d7[k]).abs() < 1e-4,
                "drag[{k}] 是比值，缩放后必须不变：scale=1 时 {}，scale=7 时 {}",
                d1[k],
                d7[k]
            );
        }
    }

    /// **`sizeRatio > 9` ⟹ 文本段 `rotdamping` 被覆盖成 `1.0`**（R25）。
    ///
    /// 官方 `collisionmodel.cpp:1206-1218` 的 HACKHACK：
    ///
    /// ```c
    /// Vector tmp = size; tmp[largest] = 0;
    /// float len = tmp.Length();
    /// if ( len > 0 ) {
    ///     float sizeRatio = size[largest] / len;
    ///     if ( sizeRatio > 9 ) pBase->m_rotdamping = 1.0f;
    /// }
    /// ```
    ///
    /// 判据（**不需要 oracle**）：`sizeRatio` 是尺度/轴序不变量。
    /// 语料实测 `>9` 的 32 个 solid **全部** `rotdamping == 1.0`，
    /// 而 `≤9` 的 623 个里只有 1.6%（QC 自己写的）。
    ///
    /// ⚠️ **这是覆盖，不是默认值**：QC 写 `$rotdamping 0` 也会被改成 `1.0`。
    /// 所以夹具里**显式把 QC 值设成 0**，判据才是「覆盖」而不是「恰好等于默认」。
    #[test]
    fn long_skinny_forces_rot_damping_to_one() {
        let read_rd = |verts: Vec<[f32; 3]>| -> f32 {
            let h = PhyHull {
                vertices: verts,
                faces: cube_faces(),
            };
            let s = [PhySolid::prop("skinny")];
            let mut p = PhyParams::new("skinny", 0);
            p.rot_damping = 0.0; // ← QC 显式写 0，用来判「覆盖」
            let b = write_phy(std::slice::from_ref(&h), &s, &p).unwrap();
            let sc = i32::from_le_bytes([b[8], b[9], b[10], b[11]]);
            let mut off = 16usize;
            for _ in 0..sc {
                let sz = i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
                off += sz as usize + 4;
            }
            let t = String::from_utf8_lossy(&b[off..]).to_string();
            let re_at = t.find("\"rotdamping\"").expect("文本段应有 rotdamping");
            let rest = &t[re_at + 12..];
            let a = rest.find('"').unwrap() + 1;
            let bq = rest[a..].find('"').unwrap() + a;
            rest[a..bq].parse::<f32>().expect("rotdamping 应是数字")
        };

        // 细长杆：size = (2, 2, 40) ⟹ largest = z(40)，
        // |size 去掉 z| = |(2,2,0)| = 2.828 ⟹ sizeRatio = 40/2.828 = 14.14 > 9 ✅
        let mut skinny = cube_vertices();
        for v in &mut skinny {
            v[2] *= 20.0; // z 从 ±1 变成 ±20 ⟹ size_z = 40
        }
        let sz = [2.0f32, 2.0, 40.0];
        let sr = sz[2] / (sz[0] * sz[0] + sz[1] * sz[1]).sqrt();
        assert!(sr > 9.0, "夹具的 sizeRatio 应为 {sr} > 9 —— 否则本测试空洞");
        let rd = read_rd(skinny);
        assert!(
            (rd - 1.0).abs() < 1e-6,
            "sizeRatio = {sr} > 9 ⟹ rotdamping 应被**覆盖**成 1.0，实际 {rd}"
        );

        // 对照：不细长的立方体（sizeRatio = 2/2.828 = 0.707 < 9）应保留 QC 的 0。
        let rd_cube = read_rd(cube_vertices());
        assert!(
            rd_cube.abs() < 1e-6,
            "sizeRatio < 9 时不得改动 QC 值：应保留 0.0，实际 {rd_cube}"
        );

        // ⚠️ **阈值必须是 9，不是别的数** —— 上面两组只证明「有个阈值」，
        // 抓不到「阈值写成 1」这类变异（变异测试实测 D 逃逸）。
        //
        // 构造一个 `1 < sizeRatio < 9` 的中间样本：阈值 1 会让它变成 1.0，
        // 阈值 9 会保留 QC 的 0。**这条才是阈值数值的判据。**
        //
        // `size = (2, 2, 2k)` ⟹ `sizeRatio = 2k / √8 = k/√2`。
        // 取 `k = 2` ⟹ `sizeRatio = 1.414`（在 (1, 9) 内）✅
        let mut mid = cube_vertices();
        for v in &mut mid {
            v[2] *= 2.0; // z 从 ±1 变成 ±2 ⟹ size_z = 4 ⟹ sizeRatio = 4/2.828 = 1.414
        }
        let sr_mid = 4.0f32 / (2.0f32 * 2.0 + 2.0 * 2.0).sqrt();
        assert!(
            sr_mid > 1.0 && sr_mid < 9.0,
            "中间样本的 sizeRatio 应为 {sr_mid}，必须落在 (1, 9) 内 —— 否则本判据无意义"
        );
        let rd_mid = read_rd(mid);
        assert!(
            rd_mid.abs() < 1e-6,
            "sizeRatio = {sr_mid}（1 与 9 之间）⟹ 不得覆盖，应保留 QC 的 0.0，实际 {rd_mid}。\
             若得到 1.0 说明阈值被写成了 1 而不是 9"
        );
    }

    /// 质量分配：多 solid 时按体积比例分，且下限钳到 1.0。
    #[test]
    fn mass_is_distributed_by_volume_with_floor() {
        // 一个大立方体 + 一个很小的立方体，total_mass = 100。
        let big = cube();
        let mut small_vs = cube_vertices();
        for v in &mut small_vs {
            for c in v.iter_mut() {
                *c *= 0.1;
            }
        }
        let small = PhyHull {
            vertices: small_vs,
            faces: cube_faces(),
        };
        let solids = [PhySolid::prop("big"), PhySolid::prop("small")];
        let mut p = PhyParams::new("two", 0);
        p.total_mass = 100.0;
        let b = write_phy(&[big, small], &solids, &p).unwrap();
        let layout = check_invariants(&b).unwrap();
        let body = std::str::from_utf8(&b[layout.solids_end..b.len() - 1]).unwrap();

        // 体积比 8 : 0.008 → 质量比约 1000:1。小的那个会被钳到 1.0。
        let masses: Vec<f32> = body
            .lines()
            .filter_map(|l| l.strip_prefix("\"mass\" \""))
            .filter_map(|v| v.strip_suffix('"'))
            .filter_map(|v| v.parse::<f32>().ok())
            .collect();
        assert_eq!(masses.len(), 2);
        assert!(masses[0] > 99.0, "大块的质量应接近 100，实际 {}", masses[0]);
        assert_eq!(masses[1], 1.0, "小块的质量应被钳到 1.0");
    }
}
