//! MDL（`IDST` v49）写出。
//!
//! ```text
//! [studiohdr_t 第0+第1部分  408 字节]
//! [骨骼表       n × 216]
//! [材质表       n × 64]
//! [body part 表 n × 16]
//!   → [model 表  n × 148]
//!       → [mesh 表 n × 116]
//! [字符串池（骨骼名 / 材质名 / surfaceprop / $cdmaterials）]
//! ```

use std::collections::HashMap;

use crate::anim_writer;
use crate::compile::resolve_bone_pose;
use crate::model::{CompiledModelDesc, ModelDesc, Vertex};

/// `studiohdr_t` 第 0 + 第 1 部分的字节大小。
pub const HDR_PART1_SIZE: usize = 0x198;
/// `studiohdr2_t` 的字节大小（256 = 头部 + `reserved[56]`）。
///
/// 实测 3333/3333 个真实模型都把 `studiohdr2` 放在 **408**（即
/// `HDR_PART1_SIZE`），紧跟头部之后。
pub const STUDIOHDR2_SIZE: usize = 256;

// ===========================================================================
// 上限常量：**只放「二进制格式可表示的最大值」**
// ===========================================================================
//
// # 为什么不再复刻 studiomdl 的人为上限
//
// 早期实现照抄了 studiomdl 的 `MAXSTUDIOVERTS = 65536`（`write.cpp:1642`
// 的「逐 model」检查）。但那**不是格式约束** ——
// `mstudiomodel_t.numvertices` 是 `int32`，能表达到 21 亿。
// 官方之所以卡在 65536，是因为引擎的**运行时顶点缓存**假定如此
// （`write.cpp:1644` 的注释：`so that we don't screw up decal vert caching
// in the runtime`），不是文件写不出来。
//
// 本项目的定位是「**能产出格式合法的二进制**」，所以只保留
// **位宽 / 偏移算术**推出的硬上限；studiomdl 的人为上限一律不复刻。
// 于是：**能表示的都写出来**，超出的报错必须能指出**是哪个字段装不下**。

/// VVD 顶点里**骨骼下标**字段的格式上限。
///
/// # 为什么约束的是「被引用的下标」而不是「骨骼总数」
///
/// VVD 的 `mstudioboneweight_t.bone[]` 是**有符号** `char`
/// （`hl2sdk-doi/public/studio.h:1236`，`STUDIO_VERSION 49` —— **与语料版本
/// 一致**；⚠️ **不是** `hl2sdk-darkm`，那是 v44、不含 `studiohdr2`），
/// 能表达 `0..=127` ⟹ **被顶点引用的下标必须 ≤ 127**。
///
/// VTX 侧的 `boneID[]` 同样是 `char`（`hl2sdk-doi/public/optimize.h:48`）。
///
/// ⚠️ 但**骨骼总数可以超过 128** —— 只要被引用的下标都在范围内。
/// 这不是推演，是实测（`docs/_probe/diag_bone_use.js`）：
///
/// | 产物 | 骨骼总数 | 实际用到的最大下标 |
/// |---|---|---|
/// | mdlc 的 `linnea-export` | **134** | **118** |
/// | 官方 NekoMDL 的 `survivor_teenangst` | 122 | **119** |
///
/// 两者都**没有**越界。骨骼表里那些没被顶点引用的骨骼
/// （`ValveBiped.forward`、jigglebone、attachment 等）只出现在
/// `mstudiobone_t` 数组里，不占 VVD 的下标空间。
///
/// # ⚠️ 一个被实测推翻的中间版本（留作教训）
///
/// 最初实现的是「骨骼**总数** ≤ 128」（照抄官方 `g_numbones >= MAXSTUDIOBONES`）。
/// 那让 `linnea-export`（134 根）**编译失败** —— 而它在改动前能编，
/// 于是 101 个 parity 用例掉了 1 个（53 个产物消失）。
/// **若没跑全量 parity，这个回归会直接进提交。**
///
/// 所以本常量约束**下标**，检查点在 VVD 写出时逐顶点做。
pub const MAX_BONE_INDEX_IN_VERTEX: usize = 127;

/// `MAXSTUDIOBONES`：骨骼**表**条数在**官方 studiomdl** 里的上限
/// （`hl2sdk-doi/public/studio.h:75`；darkm 是 `:63`）。
///
/// # 这是**引擎**的上限，不是格式上限 —— 所以**不作为拒绝条件**
///
/// 官方 `g_numbones >= MAXSTUDIOBONES` 在 **128 根**就报
/// `Too many bones used in model, used 128, max 128` —— 因为引擎的
/// `g_bonetable[128]` 会越界。而**文件格式**能装下更多：
/// `mstudiobone_t` 数组的长度字段是 `int32`。
///
/// 本项目只保证「产出格式合法的二进制」，所以**不复刻**这条：
/// 骨骼总数可以超过 128（实测 `linnea-export` 就是 134 根），
/// 只要被引用的下标 ≤ [`MAX_BONE_INDEX_IN_VERTEX`]。
///
/// 保留该常量供文档/对照使用。
pub const MAXSTUDIOBONES: usize = 128;

/// **每 mesh** 顶点数的格式上限：VTX 的 `origMeshVertID` 是 `uint16`。
///
/// `uint16` 能表达 `0..=65535` ⟹ 下标覆盖 65536 个顶点 ⟹ **上限 65536**。
///
/// ⚠️ 判据是 `n > MAXSTUDIOVERTS_PER_MESH`（**不是 `>=`**）：
/// 65536 个顶点时下标是 `0..65535`，**全部装得下**，合法。
/// 早期实现写成 `n > u16::MAX`（= `n > 65535`）⟹ 把 65536 个顶点**误拒**，
/// 恰好少了一个。
pub const MAXSTUDIOVERTS_PER_MESH: usize = 65536;

/// **每 model** 顶点数的格式上限 —— 由 `vertexindex` 的**偏移算术**推出。
///
/// `mstudiomodel_t.vertexindex` 是 `int32`，语义是**相对 VVD 顶点块的
/// 字节偏移**（不是顶点下标！见 [`crate::mdl_writer`] 的说明）。
/// 每个顶点 [`VERTEX_STRIDE`] = 48 字节，所以：
///
/// ```text
/// 上限 = floor(i32::MAX / 48) = 44_739_242
/// ```
///
/// 这不是「studiomdl 允许多少」，而是「**这个字段装得下多少**」。
/// 实际上还有第二道约束：`mstudiomodel_t.numvertices` 本身是 `int32`，
/// 上限 `i32::MAX`（更大），所以**字节偏移这一条先到**。
///
/// 对照：官方 `MAXSTUDIOVERTS = 65536` 只有这个数的 **0.15%**。
pub const MAXSTUDIOVERTS_PER_MODEL: usize = (i32::MAX as usize) / VERTEX_STRIDE;

/// **材质表条数**的格式上限：skin 表是**有符号** `short`。
///
/// `studiohdr_t::pSkinref`（`hl2sdk-doi/public/studio.h:2297`）：
///
/// ```c
/// inline short *pSkinref( int i ) const {
///     return (short *)(((byte *)this) + skinindex) + i;
/// }
/// ```
///
/// `mstudiomesh_t.material` 虽是 `int`，但引擎取材质要经 skin 表
/// （`skinref[material + family*numskinref]`）⟹ 材质下标必须装得进
/// 有符号 16 位 ⟹ **上限 32768**（下标 `0..=32767`）。
///
/// ⚠️ 这里查的是**材质表条数**（`numtextures`），不是被引用的最大下标：
/// 只要条数 ≤ 32768，任何合法下标都 ≤ 32767。
pub const MAXSTUDIOTEXTURES: usize = 32768;

/// `mstudiobone_t` 的字节大小。
pub const BONE_SIZE: usize = 216;
/// `mstudiotexture_t` 的字节大小。
pub const TEXTURE_SIZE: usize = 64;
/// `mstudiobodyparts_t` 的字节大小。
pub const BODY_PART_SIZE: usize = 16;
/// `mstudiomodel_t` 的字节大小。
pub const MODEL_SIZE: usize = 148;
/// `mstudiomesh_t` 的字节大小。
pub const MESH_SIZE: usize = 116;
/// `mstudiohitboxset_t` 的字节大小。
pub const HITBOX_SET_SIZE: usize = 12;
/// `mstudiobonecontroller_t` 的字节大小。
///
/// ```text
/// +0x00 int bone        +0x04 int type      +0x08 int start
/// +0x0C int end         +0x10 int rest      +0x14 int inputfield
/// +0x18..0x37 unused[8]
/// ```
pub const BONE_CONTROLLER_SIZE: usize = 56;
/// `mstudiobbox_t` 的字节大小。
pub const HITBOX_SIZE: usize = 68;
/// `mstudioattachment_t` 的字节大小。
///
/// 实测确认：官方 `v_autoshotgun.mdl` 的 attachment 表起于 22168、
/// hitbox set 起于 22904，跨度 736 / 8 项 = **92 字节/项**。
pub const ATTACHMENT_SIZE: usize = 92;
/// `mstudiomodel_t.name` 内联字符串的长度。
pub const MODEL_NAME_LEN: usize = 64;
/// VVD 顶点 stride —— `vertexindex` 要乘它。
pub const VERTEX_STRIDE: usize = 48;
/// VVD 切线 stride（`Vector4D`）—— `tangentsindex` 要乘它。
///
/// `studio.h:1327-1332` `GetGlobalTangentIndex`：
/// ```cpp
/// return ( i + ( modelptr->tangentsindex / sizeof( Vector4D ) ) );
/// ```
/// ⟹ `tangentsindex` 与 `vertexindex` 同构，是**相对 VVD 切线块的字节偏移**，
/// 不是下标。
pub const TANGENT_STRIDE: usize = 16;
/// `mstudioposeparamdesc_t` 的字节大小。
///
/// ```text
/// +0x00 int sznameindex   +0x04 int flags    +0x08 float start
/// +0x0C float end         +0x10 float loop
/// ```
/// `studio.h:599-607`。
pub const POSE_PARAM_SIZE: usize = 20;
/// `STUDIO_LOOPING` —— `mstudioposeparamdesc_t.flags` 的循环位。
///
/// `studio.h:1992`：`#define STUDIO_LOOPING 0x0001`。
/// QC 的 `wrap` / `loop` 关键字置位（`studiomdl.cpp:476-486`）。
pub const STUDIO_LOOPING: i32 = 0x0001;
/// `mstudioikchain_t` 的字节大小。
///
/// ```text
/// +0x00 int sznameindex   +0x04 int linktype
/// +0x08 int numlinks      +0x0C int linkindex
/// ```
/// `studio.h:1180-1190`。
pub const IK_CHAIN_SIZE: usize = 16;
/// `mstudioiklink_t` 的字节大小 —— **28**，不是 24。
///
/// ```text
/// +0x00 int bone   +0x04 Vector kneeDir(12)   +0x10 Vector unused0(12)
/// ```
/// `studio.h:1171-1178`。
///
/// 语料实测：只有 28 能让「链数组 + 链接区」精确对上下一段起点
/// （78/78 命中；20/24/32/36 全 0 —— `docs/_probe/probe_ikchain.js`）。
pub const IK_LINK_SIZE: usize = 28;
/// 每条 IK 链的链接数 —— 恒为 3（`simplify.cpp:5611`）。
///
/// 三段分别是「末端 / 膝肘 / 胯肩」，由骨骼表自动推出。
pub const IK_LINK_COUNT: usize = 3;
/// `mstudioiklock_t` 的字节大小。
///
/// ```text
/// +0x00 int chain   +0x04 float flPosWeight   +0x08 float flLocalQWeight
/// +0x0C int flags   +0x10 int unused[4]
/// ```
/// `write.cpp:1569-1575` **只写前三个字段**，`flags`/`unused` 保持 0
/// （语料 22/22 实测）。
pub const IK_LOCK_SIZE: usize = 32;

/// `mstudioflexdesc_t` 的字节大小（`+0x00 int szFACSindex`）。
pub const FLEXDESC_SIZE: usize = 4;
/// `mstudiojigglebone_t` 的字节大小 —— **120** = 30 个 4 字节槽。
///
/// 语料反解：22 个模型的 `ALIGN4(boneindex + numbones*216) + N*120`
/// 精确等于 `bonecontrollerindex`（含 1/3/9/12/13 条记录的各种组合，
/// `rsrch_jiggle_layout.js`）。
///
/// 字段偏移见 [`crate::model::JiggleBone`] —— ⚠️ 调研报告 §1.5 的表格
/// **整体错位**，以实测为准。
pub const JIGGLE_BONE_SIZE: usize = 120;

/// `mstudioquatinterpbone_t` 的字节大小（`control` + `numtriggers` + `triggerindex`）。
///
/// ```text
/// +0x00 int control        // 用来查触发器的**控制**骨骼下标
/// +0x04 int numtriggers
/// +0x08 int triggerindex   // 相对**本记录自身**
/// ```
///
/// 实测（`rsrch_proc_detail.js`，`survivor_producer.mdl`）：
/// 8 条记录的数组是 `18376..18472`，正好 `8 * 12 = 96` 字节。
pub const QUATINTERP_BONE_SIZE: usize = 12;

/// `mstudioquatinterpinfo_t` 的字节大小（一条触发器）。
///
/// ```text
/// +0x00 float      inv_tolerance   // **1.0 / tolerance**（write.cpp:259）
/// +0x04 Quaternion trigger         // 16 B
/// +0x14 Vector     pos             // 12 B
/// +0x20 Quaternion quat            // 16 B
/// ```
///
/// `4 + 16 + 12 + 16 = 48`。实测：`survivor_producer` 的 36 条触发器
/// 占 `20200 - 18472 = 1728 = 36 * 48` 字节。
pub const QUATINTERP_INFO_SIZE: usize = 48;
/// `mstudiomodelgroup_t` 的字节大小（`$includemodel`）。
///
/// ```text
/// +0x00 int szlabelindex（**恒 0**，语料 47/47 —— 官方从没填过）
/// +0x04 int sznameindex（**相对本记录自身**）
/// ```
pub const MODEL_GROUP_SIZE: usize = 8;

/// `mstudiolinearbone_t` 的**头部**字节数（`numbones` + 9 个 `*index` + `unused[6]`）。
///
/// ```text
/// +0x00 int numbones            （== 头部 numbones，实测 517/517）
/// +0x04 int flagsindex
/// +0x08 int parentindex
/// +0x0C int posindex
/// +0x10 int quatindex
/// +0x14 int rotindex
/// +0x18 int posetoboneindex
/// +0x1C int posscaleindex
/// +0x20 int rotscaleindex
/// +0x24 int qalignmentindex
/// +0x28 int unused[6]           （官方**恒不写**，实测 517/517 全零）
/// ```
///
/// ⚠️ 这 9 个子数组的**顺序**与 `studio.h:310-338` 的字段声明顺序一致，
/// 但**不要**据此推断字节布局 —— 下面的步长是**实测反解**的
/// （`probe_linearbone_formula.js`，517/517 命中）。
pub const LINEARBONE_HEADER_SIZE: usize = 64;

/// 每根骨骼在 `linearbone` 段里占的字节数（各子数组紧密排列、**无填充**）。
///
/// 实测公式（`probe_linearbone_formula.js`，**517/517**）：
///
/// ```text
/// flagsindex      = 64
/// parentindex     = 64 +   4n
/// posindex        = 64 +   8n
/// quatindex       = 64 +  20n
/// rotindex        = 64 +  36n
/// posetoboneindex = 64 +  48n
/// posscaleindex   = 64 +  96n
/// rotscaleindex   = 64 + 108n
/// qalignmentindex = 64 + 120n
/// 段总大小        = 64 + 136n
/// ```
///
/// 各数组的元素大小依次是 4 / 4 / 12 / 16 / 12 / 48 / 12 / 12 / 16 字节，
/// 恰好紧密相接（`4+4+12+16+12+48+12+12+16 == 136`）。
pub const LINEARBONE_PER_BONE: usize = 136;

/// 单个子数组的（**相对头部之后**的偏移系数, 元素字节数），顺序与 9 个 `*index` 一致。
///
/// ⚠️ 写成文件里的 `*index` 值时要**加上 [`LINEARBONE_HEADER_SIZE`]**：
/// `idx[k] = 64 + coeff[k] * numbones`。实测 `flagsindex` 恒为 **64**（不是 0）。
///
/// 用于写出各子数组 —— 索引与步长都从这里取，避免两处各写一份而漂移。
pub const LINEARBONE_ARRAYS: [(usize, usize); 9] = [
    (0, 4),    // flags       int[n]
    (4, 4),    // parent      int[n]
    (8, 12),   // pos         Vector[n]
    (20, 16),  // quat        Quaternion[n]
    (36, 12),  // rot         RadianEuler[n]
    (48, 48),  // poseToBone  matrix3x4_t[n]
    (96, 12),  // posscale    Vector[n]
    (108, 12), // rotscale    Vector[n]
    (120, 16), // qalignment  Quaternion[n]
];

/// `mstudioanimblock_t` 的字节大小（`datastart` + `dataend` 两个 `int`）。
///
/// 与 [`MODEL_GROUP_SIZE`] 同为 8，但**语义不同**（那是两个 `int` 偏移），
/// 所以单独定义，避免以后调整其中一个时误伤另一个。
pub const ANIMBLOCK_SIZE: usize = 8;

/// `mstudiosrcbonetransform_t` 的字节大小。
///
/// ```text
/// +0x00 int          sznameindex    // 相对**本记录自身**
/// +0x04 matrix3x4_t  pretransform   // 48 B
/// +0x34 matrix3x4_t  posttransform  // 48 B
/// ```
///
/// `4 + 48 + 48 = 100`。实测：官方 578 个 artifacts 里 7 个有该段
/// （`ipr2`/`ipr3`/`ipk1`/`ipk2`/`ipk3`/`ipq3`/`ipq5`），
/// 段长全部等于 `100 * numbones`。
///
/// # 数值来源（本轮破解）
///
/// | 字段 | 值 |
/// |---|---|
/// | `sznameindex` | **普通骨骼名**（`root`/`a`/`b`/`hip`…）—— 不是 `<名>_JOINT` |
/// | `pretransform` | `M(srcWorld⁻¹)` |
/// | `posttransform` | `M(newWorld)`（重排后的世界矩阵） |
///
/// 判据：`ipr2` 的 `root` 得到**精确的 `Rz(90°)`**（与 `g_defaultrotation` 吻合）；
/// `ipq3`（`$definebone` + `$realignbones`）的 `a`/`b` 得到 `[0,0,±10]` 平移，
/// 正是 `$definebone` 写的参考姿态。
///
/// > ⚠️ 旧文档说名字形态是 `<骨骼名>_JOINT` —— **那是误读**。
/// > `probe_srcbonetransform.js` 实测语料 3328/3328 条名字**全部命中骨骼名**，
/// > 说明 `door_rbdproxy__JOINT` 这类**骨骼本身**就叫这个名字，
/// > 官方并没有拼接后缀。
pub const SRCOBONETRANSFORM_SIZE: usize = 100;
/// `mstudioflexcontroller_t` 的字节大小。
///
/// ```text
/// +0x00 int sztypeindex   +0x04 int sznameindex   +0x08 int localToGlobal(恒-1)
/// +0x0C float min         +0x10 float max
/// ```
pub const FLEXCONTROLLER_SIZE: usize = 20;
/// `mstudioflexrule_t` 的字节大小（`flex`/`numops`/`opindex`，opindex 相对自身）。
pub const FLEXRULE_SIZE: usize = 12;
/// `mstudioflexop_t` 的字节大小（`op` + `union{int index; float value}`）。
pub const FLEXOP_SIZE: usize = 8;
/// `mstudioflexcontrollerui_t` 的字节大小。
///
/// ```text
/// +0x00 int sznameindex   +0x04 int szindex0   +0x08 int szindex1   +0x0C int szindex2
/// +0x10 u8 remaptype(恒0) +0x11 u8 stereo      +0x12 u8 unused[2]
/// ```
pub const FLEXCONTROLLERUI_SIZE: usize = 20;
/// `mstudioeyeball_t` 的字节大小 —— **172**，不是 `studio.h` 注释里看起来的尺寸。
///
/// 语料反解：172 是**唯一**能让全部 8 条 eyeball 记录解析出合法
/// `bone`/`radius`/单位 `up`/单位 `forward` 的尺寸（`rsrch_eyeball_size.js`，
/// 4/4 模型 8/8 条命中，其余 64 个候选全 0）。
pub const EYEBALL_SIZE: usize = 172;
/// `mstudiomouth_t` 的字节大小（`bone`/`forward`/`flexdesc`）。
pub const MOUTH_SIZE: usize = 20;

/// `mstudioflex_t` 的字节大小 —— **60**。
///
/// 实测依据：受控实验 `b_f1.mdl` 的 `mesh[0] flexindex=116`、
/// `vertindex=60`，逐字段解出的记录长度恰好 60。
///
/// 布局（**逐字段数出来的**，不是照抄 `studio.h` 的字段顺序猜测）：
///
/// ```text
/// +0x00 int   flexdesc
/// +0x04 float target0
/// +0x08 float target1
/// +0x0C float target2
/// +0x10 float target3
/// +0x14 int   numverts      ← **+0x14**，不是 +0x18
/// +0x18 int   vertindex     （相对**本记录自身**）
/// +0x1C int   flexpair
/// +0x20 u8    vertanimtype + u8 unusedchar[3]
/// +0x24 int   unused[6]     （24 字节）
/// ```
pub const FLEX_SIZE: usize = 60;
/// `mstudiovertanim_t` 的字节大小（`vertanimtype = 0`）。
///
/// ```text
/// +0x00 u16 index
/// +0x02 u8  speed
/// +0x03 u8  side
/// +0x04 i16 delta[3]   （IEEE binary16）
/// +0x0A i16 ndelta[3]  （IEEE binary16）
/// ```
pub const VERTANIM_SIZE: usize = 16;
/// `mstudiovertanim_wrinkle_t` 的字节大小（`vertanimtype = 1`）。
///
/// 比 NORMAL 多一个 `short wrinkledelta`（`studio.h:1007-1011`）。
/// mdlc 目前**不产出**这一型（语料里只有 `survivor_gambler` 有，
/// 且来自 DMX 工具链，不在本实现范围）—— 留着是为了读语料时能算对步长。
pub const VERTANIM_WRINKLE_SIZE: usize = 18;

/// `STUDIOHDR_FLAGS_STATIC_PROP`。
pub const FLAG_STATIC_PROP: i32 = 1 << 4;

/// 一个 mesh 的 flex 载荷字节数（含 `ALIGN4`，见 [`crate::layout::SectionCounts::flex_bytes`]）。
///
/// 空载荷返回 **0** —— 与 `write.cpp:1753` 的 `if (pmesh[m].numflexes)`
/// 一致：没有形状的 mesh **不写任何字节**（连 flex 数组头都不写）。
pub fn flex_payload_bytes(flexes: &[crate::flex::ResolvedFlex]) -> usize {
    if flexes.is_empty() {
        return 0;
    }
    let mut n = flexes.len() * FLEX_SIZE;
    n = align4(n);
    for f in flexes {
        let stride = vertanim_stride(f.vertanimtype);
        n += align4(f.vertanims.len() * stride);
    }
    n
}

/// `ALIGN4`：向上取整到 4 的倍数（`write.cpp:61` 的宏）。
#[inline]
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// 一条 flex 的 vertanim 步长（`mstudioflex_t::VertAnimSizeBytes()`）。
#[inline]
fn vertanim_stride(vertanimtype: u8) -> usize {
    if vertanimtype == 1 {
        VERTANIM_WRINKLE_SIZE
    } else {
        VERTANIM_SIZE
    }
}

/// `f32` → IEEE **binary16** 的位模式（**向零截断**）。
///
/// 复用 [`crate::anim_writer`] 的同一套转换 —— 那里已经用受控实验
/// （`probe_half_rounding.js`，7/7）证明 Source 用的是**向零截断**
/// 而非「舍入到最近偶数」。
///
/// `mstudiovertanim_t` 的 `delta`/`ndelta` 就是这一型
/// （`studio.h:986-997` 的 `SetDeltaFloat` 走 `float16::SetFloat`）。
fn f32_to_half_bits(v: f32) -> u16 {
    crate::anim_writer::float_to_half_public(v)
}

/// 写一个 mesh 的 flex 载荷，返回写完后的游标。
///
/// # 布局（`write.cpp:1753-1814`）
///
/// ```text
/// mstudioflex_t[numflexes]          （60 字节/条，数组整体对齐到 4）
/// 逐条 flex（顺序 == flexes 顺序）：
///     mstudiovertanim_t[numverts]   （16 字节/条）
///     ALIGN4
/// ```
///
/// 每条 flex 的 `vertindex` 是**相对该 flex 记录自身**的偏移
/// （`write.cpp:1771` 的 `(pData - (byte *)pflex)`）。
fn write_mesh_flexes(
    buf: &mut [u8],
    base: usize,
    flexes: &[crate::flex::ResolvedFlex],
    layout: Option<(&crate::lod::LodLayout, usize)>,
) -> Result<usize, WriteError> {
    if flexes.is_empty() {
        return Ok(base);
    }
    let mut cur = base;

    // 先写整段 flex 数组头。
    for (i, f) in flexes.iter().enumerate() {
        let fo = cur + i * FLEX_SIZE;
        put_i32(buf, fo, f.flexdesc);
        for (k, t) in f.targets.iter().enumerate() {
            put_f32(buf, fo + 0x04 + k * 4, *t);
        }
        put_i32(buf, fo + 0x14, f.vertanims.len() as i32);
        // vertindex 稍后回填（要等 vertanim 写完才知道相对偏移）。
        put_i32(buf, fo + 0x1C, f.flexpair);
        buf[fo + 0x20] = f.vertanimtype;
        // unusedchar[3] @0x21 与 unused[6] @0x24 保持 0（缓冲区已零初始化）。
    }
    cur += flexes.len() * FLEX_SIZE;
    cur = base + align4(cur - base);

    // 再逐条写 vertanim 数组。
    for (i, f) in flexes.iter().enumerate() {
        let fo = base + i * FLEX_SIZE;
        let vertindex = cur - fo;
        put_i32(buf, fo + 0x18, vertindex as i32);

        let stride = vertanim_stride(f.vertanimtype);
        for (v, a) in f.vertanims.iter().enumerate() {
            let vo = cur + v * stride;
            if vo + stride > buf.len() {
                return Err(WriteError::Internal(crate::tr_fmt!(
                    "the flex vertanim write is out of range (%{vo} + %{stride} > %{len})",
                    vo = vo,
                    stride = stride,
                    len = buf.len()
                )));
            }
            let index = match layout {
                Some((layout, mesh)) => layout.mesh_local_id(mesh, a.index).ok_or_else(|| {
                    WriteError::Internal(format!("Flex vertex {} is outside mesh {mesh}", a.index))
                })?,
                None => a.index,
            };
            let index = u16::try_from(index).map_err(|_| {
                WriteError::Internal(format!("Flex vertex {index} exceeds the mesh vertex limit"))
            })?;
            buf[vo..vo + 2].copy_from_slice(&index.to_le_bytes());
            buf[vo + 2] = a.speed;
            buf[vo + 3] = a.side;
            for (k, d) in a.delta.iter().enumerate() {
                let bits = f32_to_half_bits(*d);
                buf[vo + 4 + k * 2..vo + 6 + k * 2].copy_from_slice(&bits.to_le_bytes());
            }
            for (k, d) in a.ndelta.iter().enumerate() {
                let bits = f32_to_half_bits(*d);
                buf[vo + 0x0A + k * 2..vo + 0x0C + k * 2].copy_from_slice(&bits.to_le_bytes());
            }
            // WRINKLE 的 wrinkledelta 恒 0（本实现不产出该型）。
        }
        cur += align4(f.vertanims.len() * stride);
    }

    Ok(cur)
}

/// `STUDIOHDR_FLAGS_AUTOGENERATED_HITBOX` —— 注意它是 flags 的**最低位**。
///
/// `studio.h:2080`：`#define STUDIOHDR_FLAGS_AUTOGENERATED_HITBOX ( 1 << 0 )`。
/// **不是** 0x2000（早先按 0x2000 判定，得到 0 个命中）。
pub const FLAG_AUTOGENERATED_HITBOX: i32 = 1 << 0;

/// `CONTENTS_SOLID` —— `s_nDefaultContents` 的初值，也是 `contents` 的默认值。
///
/// `studiomdl.cpp:5031`：`static int s_nDefaultContents = CONTENTS_SOLID;`
pub const CONTENTS_SOLID: i32 = 1;

/// `BONE_USED_BY_HITBOX`：该骨骼（或其子骨骼）被某个 hitbox 使用。
pub const BONE_USED_BY_HITBOX: i32 = 0x0000_0100;
/// `BONE_USED_BY_ATTACHMENT`：该骨骼（或其子骨骼）被某个附着点使用。
pub const BONE_USED_BY_ATTACHMENT: i32 = 0x0000_0200;
/// `BONE_USED_BY_VERTEX_LOD0`：该骨骼被 LOD0 的蒙皮顶点使用。
pub const BONE_USED_BY_VERTEX_LOD0: i32 = 0x0000_0400;
/// `BONE_USED_BY_BONE_MERGE`：该骨骼可被 bone merge 合并（`$bonemerge`）。
///
/// 实测真实 L4D2 QC 里 `$bonemerge` 出现 **167 次**（仅次于 `$definebone`），
/// survivor 模型全部依赖它 —— 没有它，玩家的手/武器无法正确合并到角色上。
pub const BONE_USED_BY_BONE_MERGE: i32 = 0x0004_0000;
/// `BONE_ALWAYS_PROCEDURAL`：该骨骼带程序化规则（`$jigglebone` 等）。
///
/// `TagProceduralBones`（`simplify.cpp:3936/3962/4012`）置位。
/// 实测语料 **169/169** 根程序化骨骼都带它，0 例外（`rsrch_proc_flags.js`）。
pub const BONE_ALWAYS_PROCEDURAL: i32 = 0x0000_0004;

/// 骨骼缺省 flags —— 与 studiomdl 在「骨骼被顶点使用 + 自动生成 hitbox」
/// 时的取值一致（实测 `v_autoprop` 产物 `bones[0].flags == 1280 == 0x500`）。
pub const DEFAULT_BONE_FLAGS: i32 = BONE_USED_BY_VERTEX_LOD0 | BONE_USED_BY_HITBOX;

/// 头部字段的绝对偏移。
pub mod off {
    pub const ID: usize = 0x00;
    pub const VERSION: usize = 0x04;
    pub const CHECKSUM: usize = 0x08;
    pub const NAME: usize = 0x0C;
    pub const NAME_LEN: usize = 64;
    pub const LENGTH: usize = 0x4C;
    pub const EYE_POSITION: usize = 0x50;
    pub const ILLUM_POSITION: usize = 0x5C;
    pub const HULL_MIN: usize = 0x68;
    pub const HULL_MAX: usize = 0x74;
    pub const VIEW_BB_MIN: usize = 0x80;
    pub const VIEW_BB_MAX: usize = 0x8C;
    pub const FLAGS: usize = 0x98;
    pub const BONE_COUNT: usize = 0x9C;
    pub const BONE_OFFSET: usize = 0xA0;
    pub const BONE_CONTROLLER_COUNT: usize = 0xA4;
    pub const BONE_CONTROLLER_OFFSET: usize = 0xA8;
    pub const HITBOX_SET_COUNT: usize = 0xAC;
    pub const HITBOX_SET_OFFSET: usize = 0xB0;
    pub const LOCAL_ANIM_COUNT: usize = 0xB4;
    pub const LOCAL_ANIM_OFFSET: usize = 0xB8;
    pub const LOCAL_SEQ_COUNT: usize = 0xBC;
    pub const LOCAL_SEQ_OFFSET: usize = 0xC0;
    pub const ACTIVITY_LIST_VERSION: usize = 0xC4;
    pub const EVENTS_INDEXED: usize = 0xC8;
    pub const TEXTURE_COUNT: usize = 0xCC;
    pub const TEXTURE_OFFSET: usize = 0xD0;
    pub const CD_TEXTURE_COUNT: usize = 0xD4;
    pub const CD_TEXTURE_OFFSET: usize = 0xD8;
    pub const SKIN_REFERENCE_COUNT: usize = 0xDC;
    pub const SKIN_FAMILY_COUNT: usize = 0xE0;
    pub const SKIN_OFFSET: usize = 0xE4;
    pub const BODY_PART_COUNT: usize = 0xE8;
    pub const BODY_PART_OFFSET: usize = 0xEC;
    pub const LOCAL_ATTACHMENT_COUNT: usize = 0xF0;
    pub const LOCAL_ATTACHMENT_OFFSET: usize = 0xF4;
    pub const LOCAL_NODE_COUNT: usize = 0xF8;
    pub const LOCAL_NODE_OFFSET: usize = 0xFC;
    pub const LOCAL_NODE_NAME_OFFSET: usize = 0x100;
    pub const FLEX_DESC_COUNT: usize = 0x104;
    pub const FLEX_DESC_OFFSET: usize = 0x108;
    pub const FLEX_CONTROLLER_COUNT: usize = 0x10C;
    pub const FLEX_CONTROLLER_OFFSET: usize = 0x110;
    pub const FLEX_RULE_COUNT: usize = 0x114;
    pub const FLEX_RULE_OFFSET: usize = 0x118;
    pub const IK_CHAIN_COUNT: usize = 0x11C;
    pub const IK_CHAIN_OFFSET: usize = 0x120;
    pub const MOUTH_COUNT: usize = 0x124;
    pub const MOUTH_OFFSET: usize = 0x128;
    pub const LOCAL_POSE_PARAM_COUNT: usize = 0x12C;
    pub const LOCAL_POSE_PARAM_OFFSET: usize = 0x130;
    pub const SURFACE_PROP_OFFSET: usize = 0x134;
    pub const KEY_VALUE_OFFSET: usize = 0x138;
    pub const KEY_VALUE_SIZE: usize = 0x13C;
    pub const LOCAL_IK_AUTOPLAY_LOCK_COUNT: usize = 0x140;
    pub const LOCAL_IK_AUTOPLAY_LOCK_OFFSET: usize = 0x144;
    /// 质量（`$mass`）。实测 studiomdl 未指定时写 **1.0**，不是 0。
    pub const MASS: usize = 0x148;
    /// `$contents`：骨骼内容标志（`solid` = 1）。
    pub const CONTENTS: usize = 0x14C;
    pub const INCLUDEMODEL_COUNT: usize = 0x150;
    pub const INCLUDEMODEL_OFFSET: usize = 0x154;
    pub const ANIMBLOCK_NAME_OFFSET: usize = 0x15C;
    pub const ANIMBLOCK_COUNT: usize = 0x160;
    pub const ANIMBLOCK_OFFSET: usize = 0x164;
    pub const ANIMBLOCK_INDEX: usize = 0x168;
    /// `bonetablebynameindex`：指向 `byte[numbones]` 的**骨骼名索引表**。
    ///
    /// 表内容 = 骨骼下标按**骨骼名（大小写不敏感）升序**排列的结果，
    /// 由 `bone_table_by_name` 生成。**不是恒等置换** —— 骨骼表本身是
    /// 拓扑序（父在子前），与名字序无关。
    pub const BONE_TABLE_NAME_OFFSET: usize = 0x16C;
    /// `pVertexBase`（`void*`）：**只在工具进程内使用的运行时指针**
    /// （`perfstats.cpp:231` / `vradstaticprops.cpp:1913` 之类），
    /// **从不落盘** —— 官方产物恒为 0。
    ///
    /// > 早先这里叫 `VERIFICATION_HASH`，是个**误读**：`studio.h:2179-2180`
    /// > 明确写着 `void *pVertexBase; void *pIndexBase;`。
    pub const VERTEX_BASE: usize = 0x170;
    /// `pIndexBase`（`void*`）：同 [`VERTEX_BASE`]，官方产物恒为 0。
    ///
    /// > 早先这里叫 `NUM_BONE_TABLE_NAME` 并**被写入 `bone_count`**，
    /// > 是个**误读**（`studio.h:2180`）。
    pub const INDEX_BASE: usize = 0x174;
    /// `constdirectionallightdot`（`byte`）。其后 3 字节依次是 `rootLOD` /
    /// `numAllowedRootLODs` / `unused[1]` —— **都是独立 byte，不是 int**
    /// （`studio.h:2185-2198`）。
    ///
    /// > 早先这里叫 `NUM_VERIFICATION_HASH`。
    pub const CONST_DIRECTIONAL_LIGHT_DOT: usize = 0x178;
    pub const ROOT_LOD: usize = 0x179;
    pub const ALLOWED_ROOT_LODS: usize = 0x17a;
    /// `numflexcontrollerui`：`mstudioflexcontrollerui_t`（20 字节/条）的条数。
    ///
    /// `write.cpp`(ep1) 里**根本没有**这一段 —— 它由 DMX 的
    /// `CDmeGlobalFlexControllerOperator` 产生，**没有任何 QC 命令**能写出它
    /// （靠「链式自然偏移」在语料上反解：`ikchainindex ==
    /// flexcontrolleruiindex + n*20`，78/78）。mdlc 不产出，恒为 0。
    pub const FLEX_CONTROLLER_UI_COUNT: usize = 0x180;
    /// `flexcontrolleruiindex`。
    ///
    /// **空段也要写自然偏移** —— 与其余 17 个可选段同一条铁律。
    /// 官方 `ip_official.mdl` 的 `+0x184` = **1472**，而该文件的
    /// `ikchainindex` 同样是 1472（ui 数组为空时两者重合）。
    ///
    /// 早先 mdlc 整个字段都没写（留 0），逐字段对照时是一处假差异。
    pub const FLEX_CONTROLLER_UI_OFFSET: usize = 0x184;
    pub const STUDIO_HDR2_OFFSET: usize = 0x190;
}

/// 骨骼字段的偏移（相对该骨骼自身）。
///
/// # `sznameindex` 是相对骨骼自身的
///
/// 这一点与「头部字段一律绝对偏移」相反，实测确认：官方
/// `v_autoshotgun.mdl` 的 `bone[0].sznameindex = 614136`，而骨骼表起点是
/// 664 —— 文件偏移 614136 处是空串，**664 + 614136 = 614800** 处才是
/// `ValveBiped.ValveBiped`。所以写名字时必须写**相对值**。
/// 同理 `surfacepropidx`（+0xB0）也是相对值。
///
/// 这里刻意列出**全部**字段偏移（含 MVP 暂不写的 procType / physicsBone /
/// contents 等）—— 它们是后续阶段（jigglebone、物理骨骼、`$jointcontents`）
/// 的接入点，删掉会让那些实现重新推导一遍偏移。
#[allow(dead_code)]
mod bone_off {
    pub const NAME_INDEX: usize = 0x00;
    pub const PARENT: usize = 0x04;
    /// 6 个 `bonecontroller` 下标，占 0x08..0x1F。
    pub const BONE_CONTROLLER: usize = 0x08;
    pub const POSITION: usize = 0x20;
    pub const QUAT: usize = 0x2C;
    pub const ROTATION: usize = 0x3C;
    pub const POSITION_SCALE: usize = 0x48;
    pub const ROTATION_SCALE: usize = 0x54;
    pub const POSE_TO_BONE: usize = 0x60;
    pub const Q_ALIGNMENT: usize = 0x90;
    pub const FLAGS: usize = 0xA0;
    pub const PROC_TYPE: usize = 0xA4;
    pub const PROC_INDEX: usize = 0xA8;
    pub const PHYSICS_BONE: usize = 0xAC;
    pub const SURFACE_PROP_INDEX: usize = 0xB0;
    pub const CONTENTS: usize = 0xB4;
    pub const UNUSED: usize = 0xB8;
}

/// 材质字段的偏移（相对该材质自身）。
///
/// 实测确认：官方 `v_autoshotgun.mdl` 的 `texture[0].sznameindex = 17701`，
/// 而材质表起点是 601204 —— 相对解释（601204+17701）才是 `accessory_clip`，
/// 所以名字偏移是**相对该材质自身**的。
/// `flags` 在 **0x04**（曾误以为是 0x40，那其实是下一项的名字偏移）。
#[allow(dead_code)]
mod tex_off {
    pub const NAME_INDEX: usize = 0x00;
    pub const FLAGS: usize = 0x04;
    /// `$cdmaterials` 之外的运行时字段，MVP 不写。
    pub const USED: usize = 0x08;
    /// `mstudiotexture_t` 的字节大小。
    pub const SIZE: usize = 64;
}

/// model 字段的偏移（相对该 model 自身）。
///
/// `meshindex` 是**相对该 model 自身**的偏移（与 body part 的 `modelindex`、
/// mesh 的 `vertexoffset` 一致的「逐层相对」规则）。
/// `vertexindex` 例外 —— 它是**相对 VVD 顶点块的字节偏移**，不是相对 model。
#[allow(dead_code)]
mod model_off {
    pub const NAME: usize = 0x00;
    pub const NAME_LEN: usize = 64;
    pub const TYPE: usize = 0x40;
    pub const BOUNDING_RADIUS: usize = 0x44;
    pub const NUM_MESHES: usize = 0x48;
    pub const MESH_INDEX: usize = 0x4C;
    pub const NUM_VERTICES: usize = 0x50;
    pub const VERTEX_INDEX: usize = 0x54;
    pub const TANGENT_INDEX: usize = 0x58;
    pub const NUM_ATTACHMENTS: usize = 0x5C;
    pub const ATTACHMENT_INDEX: usize = 0x60;
    pub const NUM_EYEBALLS: usize = 0x64;
    pub const EYEBALL_INDEX: usize = 0x68;
}

/// mesh 字段的偏移（相对该 mesh 自身）。
///
/// # 实测定案的布局（`studio.h` 的声明与实测一致）
///
/// ```text
/// +0x00  int      material
/// +0x04  int      modelindex
/// +0x08  int      numvertices
/// +0x0C  int      vertexoffset
/// +0x10  int      numflexes
/// +0x14  int      flexindex
/// +0x18  int      materialtype
/// +0x1C  int      materialparam
/// +0x20  int      meshid          ← **mesh 的全局序号**（0,1,2…）
/// +0x24  Vector   center          （实测 5585/5585 全零）
/// +0x30  ptr      vertexdata.modelvertexdata（运行时填充的指针）
/// +0x34  int[8]   vertexdata.numLODVertexes
/// ```
///
/// **`0x20` 是 `meshid`，不是 `numBones`。** 实测 5585/5585 个真实 mesh
/// 的 `0x20` 恰好等于它在该 model 内的序号（0,1,2…），
/// 而骨骼数完全对不上（`probe_mesh_offsets.js`）。
/// 早先把 `0x20` 当 `numBones`、`0x24` 当 `boneIds[8]` 是**错的** ——
/// 那会让引擎读到一个非法的 meshid（只在单 mesh 且骨骼数恰为 0 时看不出来）。
mod mesh_off {
    pub const MATERIAL: usize = 0x00;
    pub const MODEL_INDEX: usize = 0x04;
    pub const NUM_VERTICES: usize = 0x08;
    pub const VERTEX_OFFSET: usize = 0x0C;
    pub const NUM_FLEXES: usize = 0x10;
    pub const FLEX_INDEX: usize = 0x14;
    pub const MATERIAL_TYPE: usize = 0x18;
    pub const MATERIAL_PARAM: usize = 0x1C;
    /// `meshid`：mesh 的**全局序号**（跨 bodypart/model 连续编号）。
    pub const MESH_ID: usize = 0x20;
    /// `center`（`Vector`，12 字节）。实测真实文件恒为 0。
    pub const CENTER: usize = 0x24;
    /// `vertexdata.numLODVertexes[8]`（**累计值**，见 `crate::lod`）。
    pub const NUM_LOD_VERTEXES: usize = 0x34;
}

/// body part 字段的偏移（相对该 body part 自身）。
/// 实测确认：官方 `v_autoshotgun.mdl` 的 `bodyPartOffset = 595240`、
/// `bp[0].sznameindex = 23459` —— 相对解释（595240+23459）才是
/// `nahida_themed_autoshotgun`。
///
/// `modelindex` 也是**相对该 body part 自身**的偏移（实测
/// `bp[0].modelIndex = 144`，而 595240+144 正是 model 表起点）。
mod bp_off {
    pub const NAME_INDEX: usize = 0x00;
    pub const NUM_MODELS: usize = 0x04;
    pub const BASE: usize = 0x08;
    pub const MODEL_INDEX: usize = 0x0C;
}

/// `mstudiohitboxset_t` 的字段偏移（相对自身）。
mod hbset_off {
    pub const NAME_INDEX: usize = 0x00;
    pub const NUM_HITBOXES: usize = 0x04;
    pub const HITBOX_INDEX: usize = 0x08;
}

/// `mstudiobbox_t` 的字段偏移（相对自身）。
#[allow(dead_code)]
mod hbox_off {
    pub const BONE: usize = 0x00;
    pub const GROUP: usize = 0x04;
    pub const BB_MIN: usize = 0x08;
    pub const BB_MAX: usize = 0x14;
    pub const NAME_INDEX: usize = 0x20;
    /// `unused[8]` 占 0x24..0x43。
    pub const UNUSED: usize = 0x24;
}

/// `mstudioattachment_t` 的字段偏移（相对自身）。
///
/// 实测确认（官方 `v_autoshotgun.mdl`）：`sznameindex`@0x00、
/// `flags`@0x04、`localbone`@0x08、`local`（`matrix3x4_t`，12 float）@0x0C，
/// 其后 `unused[8]` 填满 92 字节。
#[allow(dead_code)]
mod at_off {
    pub const NAME_INDEX: usize = 0x00;
    pub const FLAGS: usize = 0x04;
    pub const LOCAL_BONE: usize = 0x08;
    pub const LOCAL: usize = 0x0C;
    /// `unused[8]` 占 0x3C..0x5B。
    pub const UNUSED: usize = 0x3C;
}

/// 写出过程中的错误。
///
/// # 命名约定（本模块的核心设计）
///
/// 每个 `TooMany*` 都对应一个**二进制字段装不下**的硬约束，
/// 并在文档里写明**是哪个字段、多少位**。**不再有**「复刻 studiomdl
/// 人为上限」的错误 —— 见模块顶部「上限常量」一节的说明。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// 描述文件不合法（由 `validate` 产生）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The description file has %{count} error(s):\n%{list}",
            count = .0.len(),
            list = .0.iter().map(|e| format!("  - {e}\n")).collect::<String>()
        )
    )]
    Invalid(Vec<String>),
    /// 内部不一致（例如偏移溢出）—— 属于本实现的 bug，不应发生。
    #[error("{}", crate::tr_fmt!("Internal error (please report): %{err}", err = .0))]
    Internal(String),
    /// 名字超过内联字段长度。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The path %{path} is too long: %{len} bytes, limit %{max}",
            path = path,
            len = len,
            max = max
        )
    )]
    NameTooLong {
        path: String,
        len: usize,
        max: usize,
    },
    /// **每 mesh 顶点数**超过 [`MAXSTUDIOVERTS_PER_MESH`]。
    ///
    /// 格式依据：VTX 的 `Vertex_t.origMeshVertID` 是 `uint16`
    /// ⟹ 下标 `0..=65535` ⟹ 上限 **65536**（判据是 `>`，不是 `>=`）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The mesh %{model} has %{count} vertices, over the format limit of %{max} \
             (`origMeshVertID` in VTX is uint16, indices 0..%{last}) -- \
             please split that mesh into several material slots",
            model = model,
            count = count,
            max = max,
            last = max - 1
        )
    )]
    TooManyMeshVertices {
        model: String,
        count: usize,
        max: usize,
    },
    /// **每 model 顶点数**超过 [`MAXSTUDIOVERTS_PER_MODEL`]。
    ///
    /// 格式依据：`mstudiomodel_t.vertexindex` 是 `int32` 且语义为
    /// **字节偏移**，每顶点 48 字节 ⟹ 上限 `i32::MAX / 48`。
    ///
    /// ⚠️ 这个上限是 **4473 万**，不是 studiomdl 的 65536 ——
    /// 后者是引擎运行时顶点缓存的假定，不是文件格式约束。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The path %{path} has %{count} vertices (deduplicated across LODs), over the \
             format limit of %{max} (`mstudiomodel_t.vertexindex` is an int32 byte offset, \
             %{stride} bytes per vertex) -- please split that model into several \
             `[[bodyparts.models]]`",
            path = path,
            count = count,
            max = max,
            stride = VERTEX_STRIDE
        )
    )]
    TooManyModelVertices {
        path: String,
        count: usize,
        max: usize,
    },
    /// **材质表条数**超过 [`MAXSTUDIOTEXTURES`]。
    ///
    /// 格式依据：skin 表是**有符号 `short`**（`hl2sdk-doi/public/studio.h:2297`）
    /// ⟹ 材质下标最大 32767 ⟹ 表最多 32768 条。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The material table has %{count} entries, over the format limit of %{max} \
             (indices 0..%{last}) -- the skin table `pSkinref` is a **signed short**",
            count = count,
            max = max,
            last = max - 1
        )
    )]
    TooManyTextures { count: usize, max: usize },
    /// **单个 strip group 的索引条数**超过 `int32`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The index count %{count} of a single strip group of %{model} overflows int32",
            model = model,
            count = count
        )
    )]
    TooManyIndices { model: String, count: usize },
}

/// 一个 model 在 VVD 顶点块里的起点（供写 VVD 时对齐使用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVertexSpan {
    /// `bodyparts[bi].models[mi]`。
    pub path: String,
    /// 起始顶点下标。
    pub start: usize,
    /// 顶点数。
    pub count: usize,
}

/// 写出结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    pub bytes: Vec<u8>,
    /// 各 model 的顶点区间 —— 写 VVD 时必须用同一套编号，否则
    /// `vertexindex` 与顶点数据对不上。
    pub spans: Vec<ModelVertexSpan>,
    /// 写入的 checksum（供 VVD/VTX/PHY 复用）。
    pub checksum: i32,
    /// `$animblocksize` 的 `.ani` 文件字节（`None` = 该模型不产出 `.ani`）。
    ///
    /// 由调用方写到 `<stem>.ani`。注意**块表已经写进 `.mdl`**，
    /// 这里只是同一个 `build_ani_file` 的返回值 —— 两处必须来自**同一次**调用，
    /// 否则块偏移会和 `.mdl` 里记的对不上。
    pub ani: Option<Vec<u8>>,
}

/// 规范化 `$cdmaterials` 路径：正斜杠 → 反斜杠，并补上结尾分隔符。
///
/// 实测 studiomdl 产物：QC 里写 `$cdmaterials "models/mymod"`，
/// 落到文件里是 `models\mymod\` —— **反斜杠且带结尾分隔符**。
pub fn normalize_cd_material(path: &str) -> String {
    let mut s = path.replace('/', "\\");
    if !s.is_empty() && !s.ends_with('\\') {
        s.push('\\');
    }
    s
}

/// 规范化材质名：若带 `$cdmaterials` 前缀则剥掉，并去掉扩展名。
///
/// 实测 studiomdl 产物：QC 里 `$cdmaterials "models/mymod"`，
/// 材质名写成 `models/mymod/myprop`，落到文件里是 **`myprop`** ——
/// 即相对于 cd 目录的名字。
///
/// # ⚠️ 保留**正斜杠**，不转成反斜杠
///
/// 语料实测（3301 个 `.mdl`，`verify_cdtexture_root.js`）：
///
/// | 纹理名形态 | 数量 |
/// |---|---|
/// | 含 **`/`** | **1460** |
/// | 含 **`\`** | **0** |
/// | 裸名（无分隔符） | 1841 |
///
/// **零个模型用反斜杠。** 官方把材质名**原样**存进纹理表 ——
/// 只有 `$cdmaterials`（搜索路径）才走 `Q_FixSlashes` 变成反斜杠。
///
/// 转成反斜杠会与官方产物**逐字节不同**（`moranyue/.../frown` vs
/// `moranyue\...\frown`），且因为含 `/` 而触发「根目录搜索哨兵」的判断
/// 也会跟着错。**匹配时两种分隔符都要接受**（见 `lookup_key`），
/// 但**写出时保留原样**。
pub fn normalize_texture_name(name: &str, cd_paths: &[String]) -> String {
    // 前缀比较时统一成反斜杠，但**返回值基于原串**，只做剥离。
    let norm = name.replace('/', "\\");
    for cd in cd_paths {
        let cdn = normalize_cd_material(cd);
        if let Some(rest) = norm.strip_prefix(&cdn) {
            return strip_vtf_ext(rest);
        }
        // 也接受不带结尾分隔符的写法。
        let cdn_no_sep = cdn.trim_end_matches('\\');
        if let Some(rest) = norm.strip_prefix(cdn_no_sep) {
            return strip_vtf_ext(rest.trim_start_matches('\\'));
        }
    }
    strip_vtf_ext(name)
}

/// 材质名的**匹配键**：规范化 + 统一分隔符 + 取 basename 兜底。
///
/// 匹配要宽松（SMD 里是裸名、TOML 里可能是带路径的全名），
/// 但**写出时必须用 [`normalize_texture_name`] 的原样结果**。
pub fn texture_match_key(name: &str, cd_paths: &[String]) -> String {
    let n = normalize_texture_name(name, cd_paths).replace('/', "\\");
    n.rsplit('\\').next().unwrap_or(&n).to_string()
}

fn strip_vtf_ext(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    for ext in [".vmt", ".vtf"] {
        if lower.ends_with(ext) {
            return s[..s.len() - ext.len()].to_string();
        }
    }
    s.to_string()
}

/// 生成 `bonetablebyname`：骨骼下标按**骨骼名升序**排列的 `byte[numbones]`。
///
/// # 官方语义
///
/// `write.cpp:389-399`（`WriteBoneInfo` 尾部）：
/// ```cpp
///     byte *pBoneTable = pData;
///     phdr->bonetablebynameindex = (pData - pStart);
///     // make a table in bone order and sort it with qsort
///     for ( i = 0; i < phdr->numbones; i++ ) pBoneTable[i] = i;
///     qsort( pBoneTable, phdr->numbones, sizeof(byte), BoneNameCompare );
/// ```
/// 比较器 `write.cpp:162-169`：
/// ```cpp
/// static int BoneNameCompare( const void *elem1, const void *elem2 )
/// {
///     int index1 = *(byte *)elem1;
///     int index2 = *(byte *)elem2;
///     return strcmpi( g_bonetable[index1].name, g_bonetable[index2].name );
/// }
/// ```
/// 即「**大小写不敏感**的名字字典序」。
///
/// # 为什么不能写恒等置换
///
/// 引擎唯一的消费者是 `Studio_BoneIndexByName`（`bone_setup.cpp:5725-5750`），
/// 它对这张表做**二分查找**（`Q_stricmp( pbones[pBoneTable[mid]].pszName(), pName )`）。
/// 表没排序 ⟹ 二分失效 ⟹ 绝大多数查找返回 −1。
/// 实测用引擎算法逐名复现：mdlc 修前 **114/119 个骨骼名查不到**，
/// 官方与 NekoMDL 都是 119/119。
///
/// 直接后果是 `bone_merge_cache.cpp:69-71` 的
/// `if ( parentBoneIndex < 0 ) continue;` 让骨骼进不了 `m_MergedBones`，
/// 进而 `:90-93` 把 `m_nFollowBoneSetupMask` 清零 —— **所有 `$bonemerge` 失效**。
///
/// # 稳定性
///
/// `qsort` 本身不稳定，但官方语料里**忽略大小写的重名不存在**
/// （`btb_verify.js` 实测本模型 119 根：`忽略大小写后重名: []`），
/// 排序结果因此唯一确定，稳定排序与 `qsort` 等价。
/// 这里用 `sort_by`（稳定）以保证确定性。
///
/// # Panics
///
/// 骨骼数 > 256 时 panic。这不是新增约束 ——
/// `anim_writer` 已把「`mstudioanim_t.bone` 是 `byte`」这条真格式约束
/// 钉在 256（见 `anim_bone_addressing_limit_is_byte`），
/// 走到这里时骨骼数必然 ≤ 256。
fn bone_table_by_name(bones: &[crate::model::Bone]) -> Vec<u8> {
    assert!(
        bones.len() <= 256,
        "骨骼数 {} 超出 `bonetablebyname` 的 byte 寻址范围",
        bones.len()
    );
    let mut order: Vec<u8> = (0..bones.len() as u16)
        .map(|i| u8::try_from(i).expect("上面已断言 ≤ 256"))
        .collect();
    // `strcmpi` = 逐字符 `tolower` 后比较。SMD 骨骼名都是 ASCII，
    // 用 `to_ascii_lowercase()` 得到与 `strcmpi` 相同的字节序列。
    order.sort_by(|&a, &b| {
        bones[a as usize]
            .name
            .to_ascii_lowercase()
            .cmp(&bones[b as usize].name.to_ascii_lowercase())
    });
    order
}

/// 计算每根骨骼的 `flags`。
///
/// # 为什么必须按用途算，而不是写常量
///
/// 实测官方产物：`$hbox 0 "root" ...` + `$attachment "muzzle" "tip" ...`
/// 得到 `bone[0].flags = 0x40600`（VERTEX_LOD0|ATTACHMENT|BONE_MERGE|…）、
/// `bone[1].flags = 0x200`（只有 ATTACHMENT）。
/// 也就是说：
///   - 骨骼**被顶点使用**才有 `VERTEX_LOD0`；
///   - 被 hitbox 引用才有 `HITBOX`；
///   - 被附着点引用才有 `ATTACHMENT`；
///   - 这些标志**沿父链向上传播**（`tip` 的附着点让 `root` 也有 ATTACHMENT）。
///
/// 早先一律写 `0x500`，导致未被顶点使用的骨骼被误标为「被顶点使用」，
/// 引擎可能据此做出错误的骨骼剔除。
fn compute_bone_flags(
    desc: &ModelDesc,
    compiled: &CompiledModelDesc,
    bone_index: &HashMap<&str, usize>,
) -> Vec<i32> {
    let n = desc.bones.len();
    let mut flags = vec![0i32; n];

    // 骨骼名 → 下标；父链（用于向上传播）。
    let parents: Vec<i32> = desc
        .bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => bone_index.get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect();

    // 沿父链向上打标志（含自身）。
    let mark = |start: usize, bit: i32, flags: &mut Vec<i32>| {
        let mut cur = start as i32;
        // 加个上限防环（校验已保证父骨骼在前，这里只是防御）。
        let mut guard = 0;
        while cur >= 0 && (cur as usize) < n && guard <= n {
            flags[cur as usize] |= bit;
            cur = parents[cur as usize];
            guard += 1;
        }
    };

    // 1) 被顶点使用的骨骼 —— **逐档**打 `BONE_USED_BY_VERTEX_LOD0 << n`。
    //
    // 官方分两处（见 HANDBOOK 第 41 节）：
    //   * `MarkBonesUsedByLod`（`UnifyLODs.cpp:1090`）对**重映射之后**的权重打标，
    //     **不走父链**；
    //   * `MarkParentBoneLODs`（`simplify.cpp:7270`）在 `UnifyLODs()` 之后
    //     单独一趟，把 `BONE_USED_BY_VERTEX_MASK`（0x0003FC00）内的位沿父链上传。
    //
    // 这里等价地「逐位打标 + 立即沿父链上传」：上传只做并集且向上封闭，
    // 与「先全部打标、最后统一上传」结果相同。
    //
    // ⚠️ 单 LOD 时只有 bit 0，与加这个特性之前**逐字节相同**
    // （`mark(i, BONE_USED_BY_VERTEX_LOD0)` 的老路径）。
    let mut lod_usage: Vec<u32> = vec![0u32; n];
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            match &m.lods {
                // 多 LOD：字典在统一顶点时已经算好了逐档使用位。
                Some(l) => {
                    for (i, u) in l.bone_lod_usage.iter().enumerate() {
                        if i < n {
                            lod_usage[i] |= *u;
                        }
                    }
                }
                // 单 LOD：只有 LOD 0，直接从网格顶点取。
                None => {
                    for mesh in &m.meshes {
                        for v in &mesh.vertices {
                            for pair in &v.bones {
                                let b = pair[0] as usize;
                                if b < n {
                                    lod_usage[b] |= 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for (i, usage) in lod_usage.iter().enumerate() {
        // bit n ⟹ `BONE_USED_BY_VERTEX_LOD0 << n`。
        let mut bits = *usage;
        let mut n_lod = 0;
        while bits != 0 {
            if bits & 1 != 0 {
                mark(i, BONE_USED_BY_VERTEX_LOD0 << n_lod, &mut flags);
            }
            bits >>= 1;
            n_lod += 1;
        }
    }

    // 2) 被 hitbox 引用。
    for hb in desc.hitbox_sets().flat_map(|set| &set.boxes) {
        if let Some(&i) = bone_index.get(hb.bone.as_str()) {
            mark(i, BONE_USED_BY_HITBOX, &mut flags);
        }
    }

    // 3) 被附着点引用。
    for at in &desc.attachments {
        if let Some(&i) = bone_index.get(at.bone.as_str()) {
            mark(i, BONE_USED_BY_ATTACHMENT, &mut flags);
        }
    }

    // 3b) **IK 链的三段骨骼** —— `LinkIKChains` 对每段都打
    //     `BONE_USED_BY_ATTACHMENT`（`simplify.cpp:5619/5627/5635`）：
    //
    //     ```cpp
    //     g_ikchain[i].link[2].bone = k;  g_bonetable[k].flags |= BONE_USED_BY_ATTACHMENT;
    //     k = g_bonetable[k].parent;      g_bonetable[k].flags |= BONE_USED_BY_ATTACHMENT;
    //     k = g_bonetable[k].parent;      g_bonetable[k].flags |= BONE_USED_BY_ATTACHMENT;
    //     ```
    //
    //     这不是笔误 —— IK 链确实需要附着点语义（引擎按附着点求解末端）。
    //     漏掉它的症状：`bone[].flags` 少 `0x200`（实测 `ipk1`：mdlc 1280
    //     对官方 1792，差值正是 `BONE_USED_BY_ATTACHMENT`）。
    //
    //     `mark` 会沿父链继续上传，与官方一致（官方也在父链上逐个置位，
    //     只是恰好只走两代）。
    for ch in &desc.ikchains {
        if let Some(&tip) = bone_index.get(ch.bone.as_str()) {
            mark(tip, BONE_USED_BY_ATTACHMENT, &mut flags);
        }
    }

    // 4) $bonemerge。
    for (i, b) in desc.bones.iter().enumerate() {
        if b.bonemerge {
            flags[i] |= BONE_USED_BY_BONE_MERGE;
        }
    }

    // 5) 程序化骨骼（`$jigglebone`）→ `BONE_ALWAYS_PROCEDURAL`（`0x04`）。
    //
    // `TagProceduralBones`（`simplify.cpp:3936/3962/4012`）对每根程序化骨骼置位。
    // 实测语料 **169/169** 根程序化骨骼都带 `0x04`，0 例外
    // （`rsrch_proc_flags.js`）。`procindex` 指向 jiggle 块。
    //
    // 漏掉的症状：`bone[].flags` 少 `0x04`（实测 `jig1`：mdlc 0x500
    // 对官方 0x504）。
    for j in &compiled.resolved_jiggle_bones {
        let i = j.bone as usize;
        if i < flags.len() {
            flags[i] |= BONE_ALWAYS_PROCEDURAL;
        }
    }
    // quatinterp（proctype 2）同样由 `TagProceduralBones` 置位
    // （`simplify.cpp:3962` 那一行是 `g_bonetable[...].flags |= BONE_ALWAYS_PROCEDURAL`）。
    for q in &compiled.resolved_quat_interp_bones {
        let i = q.bone as usize;
        if i < flags.len() {
            flags[i] |= BONE_ALWAYS_PROCEDURAL;
        }
    }

    flags
}

/// 字符串池的内容与各项的池内偏移。
///
/// 抽出来是为了让 [`write_mdl`] 能**先知道池有多长**再算段布局
/// （布局依赖 `string_bytes`）。
struct StringPool {
    string_buf: Vec<u8>,
    bone_name_offsets: Vec<usize>,
    /// 每根骨骼的 surfaceprop 在池内的偏移；`None` = 该骨骼不写。
    bone_sp_offsets: Vec<Option<usize>>,
    tex_name_offsets: Vec<usize>,
    bp_name_offsets: Vec<usize>,
    hb_set_name_offsets: Vec<usize>,
    hb_name_offsets: Vec<usize>,
    at_name_offsets: Vec<usize>,
    /// 序列名的池内偏移（含 `@` 前缀）。
    seq_name_offsets: Vec<usize>,
    /// 每条序列的 **activity 名**的池内偏移（按序列下标）。
    ///
    /// 与 `seqdesc.activity`（i32，恒 -1）**不是**同一个东西 ——
    /// 官方 `Option_Activity` 只把 QC 里的名字原样存进字符串池，
    /// 编号留给游戏 DLL 在加载时查表填。
    activity_name_offsets: Vec<usize>,
    /// 每个 **animdesc** 的名字在字符串池内的偏移（按 animdesc 下标）。
    ///
    /// 与 `seq_name_offsets` 分开：`$staticprop` 与 blend 序列都会让
    /// 两者的下标错位（见 `write_mdl` 里的说明）。
    anim_name_offsets: Vec<usize>,
    /// 事件名 → 池内偏移（**已去重**）。
    event_name_offsets: std::collections::HashMap<String, usize>,
    /// 姿势参数名的池内偏移（按 `desc.model.pose_parameters` 顺序）。
    poseparam_name_offsets: Vec<usize>,
    /// IK 链名的池内偏移（按 `desc.ikchains` 顺序）。
    ikchain_name_offsets: Vec<usize>,
    /// 头部 surfaceprop 的池内偏移（空串时为 0）。
    header_sp_offset: usize,
    surface_prop: String,
    /// `$cdmaterials` 各项的池内偏移。
    cd_offsets: Vec<usize>,
    /// flexdesc 名的池内偏移（按 `desc.flex_descriptors` 顺序，**已按名去重**）。
    flexdesc_name_offsets: Vec<usize>,
    /// flexcontroller 名的池内偏移（按 `desc.flex_controllers` 顺序，去重）。
    fc_name_offsets: Vec<usize>,
    /// flexcontroller 类型名的池内偏移（按 `desc.flex_controllers` 顺序，去重）。
    fc_type_offsets: Vec<usize>,
    /// flexcontrollerui 名的池内偏移（按 `resolved_flex_controller_ui` 顺序，去重）。
    fcui_name_offsets: Vec<usize>,
    /// `$includemodel` 名字的池内偏移（按 `desc.include_models` 顺序）。
    includemodel_name_offsets: Vec<usize>,
    /// `$animblocksize` 的 `.ani` 路径名（`models/<模型名>.ani`）的池内偏移。
    ///
    /// **没有 `$animblocksize` 时是 0** —— 即指向池里的**空串**，不是「无值」。
    /// 官方 `g_animblockname` 初值为空串且 `AddToStringTable` 无条件执行
    /// （`write.cpp:1850`），实测 3212/3212 个无块模型都指向空串。
    /// 实测 `ab_z4`：`+0x15C = 1644` → `"models/mymod/ab_z4.ani"`。
    animblock_name_offset: usize,
    transition_name_offsets: Vec<usize>,
}

/// 按**官方注册顺序**把各类名字写进字符串池，并记下每项的池内偏移。
///
/// # 顺序不是随意的 —— 它决定池内容，进而决定所有 `sz*index`
///
/// 官方 `AddToStringTable`（`write.cpp:101-121`）先**线性查找全表**，
/// 命中就复用已有地址；`WriteStringTable`（127-155）按**注册顺序**写出。
/// 所以池内顺序 == `AddToStringTable` 的调用顺序，也就是
/// `WriteModelFiles`（`write.cpp:2096-2146`）里各写出函数的执行顺序：
///
/// ```text
/// ① WriteBoneInfo     : surfaceprop → 逐骨骼(名字, surfaceprop)
///                       → 附着点名 → hitboxset 名 → hitbox 名
/// ② WriteAnimations   : 逐 animdesc 的名字
/// ③ WriteSequenceInfo : 逐序列(label, activity 名) → 逐事件名 → xnode 名
/// ④ WriteModel        : bodypart 名 → flexdesc → flexcontroller(名, 类型)
///                       → ikchain 名 → poseparam 名
///                       → includemodel 名 → animblock 名
/// ⑤ WriteTextures     : 材质名 → `$cdmaterials`
/// ```
///
/// 实测（`probe_pool_order.js`）：把「实际池内容」与上述预测逐项比对，
/// **3333/3333** 个模型的池内容与顺序完全一致。
///
/// # 空串统一落到池偏移 0
///
/// `strings[0]` 在 `BeginStringTable` 里被预先注册为空串
/// （`write.cpp:88-94`），所以任何 `""` 都会 `strcmp` 命中它 ——
/// **不会**各占一个 NUL。见 [`put_str`]。
fn build_string_pool(
    desc: &ModelDesc,
    compiled: &CompiledModelDesc,
    anim: &anim_writer::AnimWriteOutcome,
    transitions: &crate::transitions::Graph,
) -> Result<StringPool, WriteError> {
    let bone_count = desc.bones.len();
    // ---- 骨骼**总数**：**不设格式上限** ----
    //
    // 早期版本在这里查 `bone_count > MAXSTUDIOBONES`（照抄官方
    // `g_numbones >= MAXSTUDIOBONES`），结果让 `linnea-export`（134 根）
    // **编译失败** —— 而它改动前能编，101 个 parity 用例掉了 1 个。
    //
    // 实测证明那条检查是**错的**（`docs/_probe/diag_bone_use.js`）：
    //   · mdlc 的 134 根模型：VVD 里实际用到的最大下标 = **118**
    //   · 官方 122 根产物：最大下标 = **119**
    // 都没越界。没被顶点引用的骨骼只出现在 `mstudiobone_t` 数组里，
    // 不占 VVD 的下标空间 ⟹ 真正的约束是
    // [`MAX_BONE_INDEX_IN_VERTEX`]（逐顶点查，在 VVD 写出时做）。
    let mut bone_name_offsets = Vec::with_capacity(bone_count);
    let mut bone_sp_offsets = Vec::with_capacity(bone_count);
    let mut bone_name_len_max = 0usize;
    for b in &desc.bones {
        bone_name_len_max = bone_name_len_max.max(b.name.len());
    }
    if bone_name_len_max > 63 {
        return Err(WriteError::NameTooLong {
            path: "bones[*].name".into(),
            len: bone_name_len_max,
            max: 63,
        });
    }

    let surface_prop = desc.model.surface_prop.clone().unwrap_or_default();
    let mut string_buf: Vec<u8> = Vec::new();

    // ---- 池的第 0 字节：**无条件**一个 NUL，代表「空串」 ----
    //
    // 源码 `write.cpp:88-94`（`BeginStringTable`）：
    //
    // ```c
    // strings[0].base = NULL;
    // strings[0].ptr = NULL;
    // strings[0].string = "";      // ← 预先注册的空串
    // strings[0].dupindex = -1;
    // numStrings = 1;
    // ```
    //
    // 以及 `WriteStringTable`（127-132）：
    //
    // ```c
    // // force null at first address
    // strings[0].addr = pData;
    // *pData = '\0';
    // pData++;
    // ```
    //
    // 两条合起来 ⟹ **池偏移 0 恒为空串**，且池首字节恒为 0。
    // `AddToStringTable` 对 `""` 会 `strcmp` 命中 `strings[0]`，
    // 于是**所有空串共享偏移 0**，不会各占一个 NUL。
    //
    // 实测（`probe_pool_start2.js`）：池起点 == 公式预测 **3331/3333**
    // （另 2 个差 3/5 字节，是尚未查清的 `linearbone` 形态差异）。
    //
    // > 早先 mdlc 既不在池首写 NUL、又给每个空串单独追加 NUL ——
    // > 两处都会让后续所有 `sz*index` 错位。
    string_buf.push(0);

    // 空串一律复用池偏移 0 —— 官方 `AddToStringTable` 对 `""` 会
    // `strcmp` 命中预先注册的 `strings[0]`，**不会**各占一个 NUL。
    // 这由下面的 `register` 闭包统一处理（预登记 `"" → 0`）。

    // ===================================================================
    // 以下**严格按官方 `AddToStringTable` 的调用顺序**注册
    // （见 [`build_string_pool`] 的文档）。顺序错了会让所有 `sz*index`
    // 与官方不一致 —— 虽然语义仍自洽，但无法逐字段对照。
    // ===================================================================
    let mut pool_dedup: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // 预先登记空串 → 池偏移 0（官方 `BeginStringTable` 的 `strings[0]`）。
    pool_dedup.insert(String::new(), 0);
    let register = |string_buf: &mut Vec<u8>,
                    dedup: &mut std::collections::HashMap<String, usize>,
                    name: &str|
     -> usize {
        if let Some(&o) = dedup.get(name) {
            return o;
        }
        let o = string_buf.len();
        string_buf.extend_from_slice(name.as_bytes());
        string_buf.push(0);
        dedup.insert(name.to_string(), o);
        o
    };
    macro_rules! reg {
        ($s:expr) => {
            register(&mut string_buf, &mut pool_dedup, $s)
        };
    }

    // ---- ① WriteBoneInfo ----
    // 头部 surfaceprop 是**第一个**注册的（`write.cpp:186`，在骨骼循环之前）。
    let header_sp_offset = reg!(&surface_prop);
    let bone_surface_props =
        crate::contents::bone_surface_props(desc).map_err(WriteError::Internal)?;
    for (b, surface) in desc.bones.iter().zip(bone_surface_props) {
        bone_name_offsets.push(reg!(&b.name));
        bone_sp_offsets.push(surface.map(|surface| reg!(surface)));
    }
    let mut at_name_offsets = Vec::with_capacity(desc.attachments.len());
    for at in &desc.attachments {
        at_name_offsets.push(reg!(&at.name));
    }
    let mut hb_set_name_offsets = Vec::new();
    let mut hb_name_offsets = Vec::new();
    for set in desc.hitbox_sets() {
        hb_set_name_offsets.push(reg!(set.set_name.as_deref().unwrap_or("default")));
        for hb in &set.boxes {
            hb_name_offsets.push(reg!(hb.name.as_deref().unwrap_or("")));
        }
    }

    // ---- ② WriteAnimations：逐 animdesc 的名字 ----
    //
    // 规则由 `anim_writer::anim_name_sources` **单独给出** —— 不在这里按
    // `sequences[].blends` 重算，因为那会成为同一规则的第二份实现，
    // 一旦与 `anim_specs` 的顺序分叉就是「动画数据对、名字错」的静默损坏。
    let mut anim_name_offsets: Vec<usize> = Vec::with_capacity(compiled.anim_count());
    // 序列下标 → `@name` 的池内偏移（供隐式动画复用）。
    let mut at_seq_name_offsets: Vec<usize> = Vec::with_capacity(compiled.sequences.len());
    for src in anim_writer::anim_name_sources(compiled) {
        match src {
            anim_writer::AnimNameSource::Sequence(si) => {
                while at_seq_name_offsets.len() <= si {
                    at_seq_name_offsets.push(0);
                }
                let o = if at_seq_name_offsets[si] != 0 {
                    at_seq_name_offsets[si]
                } else {
                    let o = reg!(&format!("@{}", compiled.sequences[si].name));
                    at_seq_name_offsets[si] = o;
                    o
                };
                anim_name_offsets.push(o);
            }
            anim_writer::AnimNameSource::Literal(name) => {
                anim_name_offsets.push(reg!(&name));
            }
        }
    }
    debug_assert_eq!(
        anim_name_offsets.len(),
        compiled.anim_count(),
        "animdesc 名数量应等于 anim_count"
    );

    // ---- ③ WriteSequenceInfo：逐序列 (label, activity) → 逐事件名 ----
    //
    // ⚠️ **`seqdesc.szlabelindex` 用裸序列名**（`g_sequence[i].name`，
    // `write.cpp:431`），而 animdesc 的名字带 `@` 前缀 —— 两者是池里
    // **两个不同的串**，不是「同一串跳过首字符」。
    //
    // 语料判据（`probe_seq_label_prefix.js`）：11170 条序列的 label
    // **没有一个**以 `@` 开头；8965 个 animdesc 名**全部**以 `@` 开头。
    //
    // > 早先 mdlc 只存一份带 `@` 的串，label 用「该串 +1」——
    // > 池里因此少一个真串、多一个重复串，所有后续偏移错位。
    let mut seq_name_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut activity_name_offsets = Vec::with_capacity(compiled.sequences.len());
    for s in &compiled.sequences {
        seq_name_offsets.push(reg!(&s.name));
        activity_name_offsets.push(reg!(&s.activity_name));
    }
    // 动画事件名。同一事件名会在多条序列里重复出现（实测
    // `AE_FOOTSTEP_RIGHT` 出现上千次），`AddToStringTable` 自然去重。
    let mut event_name_offsets: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (_, name) in &anim.event_name_patches {
        if let std::collections::hash_map::Entry::Vacant(e) = event_name_offsets.entry(name.clone())
        {
            e.insert(reg!(name));
        }
    }

    let transition_name_offsets = transitions.names.iter().map(|name| reg!(name)).collect();

    // ---- ④ WriteModel ----
    let mut bp_name_offsets = Vec::with_capacity(compiled.bodyparts.len());
    for bp in &compiled.bodyparts {
        bp_name_offsets.push(reg!(&bp.name));
    }
    let mut flexdesc_name_offsets = Vec::with_capacity(desc.flex_descriptors.len());
    for f in &desc.flex_descriptors {
        flexdesc_name_offsets.push(reg!(&f.name));
    }
    let mut fc_name_offsets = Vec::with_capacity(desc.flex_controllers.len());
    let mut fc_type_offsets = Vec::with_capacity(desc.flex_controllers.len());
    for f in &desc.flex_controllers {
        fc_name_offsets.push(reg!(&f.name));
        fc_type_offsets.push(reg!(&f.kind));
    }
    // `mstudioflexcontrollerui_t` 是 L4D2 独有段（episode1 的 `write.cpp`
    // 里没有），紧跟 flexcontroller 注册。
    let mut fcui_name_offsets = Vec::with_capacity(compiled.resolved_flex_controller_ui.len());
    for u in &compiled.resolved_flex_controller_ui {
        fcui_name_offsets.push(reg!(&u.name));
    }
    let mut ikchain_name_offsets = Vec::with_capacity(desc.ikchains.len());
    for c in &desc.ikchains {
        ikchain_name_offsets.push(reg!(&c.name));
    }
    let mut poseparam_name_offsets = Vec::with_capacity(desc.model.pose_parameters.len());
    for p in &desc.model.pose_parameters {
        poseparam_name_offsets.push(reg!(&p.name));
    }
    let mut includemodel_name_offsets = Vec::with_capacity(desc.include_models.len());
    for n in &desc.include_models {
        includemodel_name_offsets.push(reg!(n));
    }
    // `$animblocksize` 的 `.ani` 路径名。
    //
    // 实测 `ab_z4`（`$modelname "mymod/ab_z4.mdl"`）头部 `+0x15C` 指向
    // `"models/mymod/ab_z4.ani"` —— 即 `"models/" + 去掉 .mdl 的模型名 + ".ani"`。
    //
    // # 没有动画块时**指向空串**，不是 0
    //
    // `g_animblockname` 是 `studiomdl` 的全局 `char[]`，初值全 0
    // （即空串）—— 而 `AddToStringTable( phdr, &phdr->szanimblocknameindex,
    // g_animblockname )`（`write.cpp:1850`）**无条件**执行，于是
    // `szanimblocknameindex` 总是一个**有效偏移**，指向池里的空串。
    //
    // 语料判据（`probe_empty_section_offsets.js`）：
    // `numanimblocks == 0` 的 **3212/3212** 个模型全部指向空串，
    // **0 个写 0**；`numanimblocks != 0` 的 121/121 指向 `*.ani`。
    //
    // > 早先 mdlc 用 `Option`，空时写 0 —— 与官方不符。
    // > `mikuw` 实测：官方 3516（空串），mdlc 0。
    let animblock_name_offset = if anim.anim_blocks.is_empty() {
        0
    } else {
        let name = desc.model.name.replace('\\', "/");
        let base = name.trim_end_matches(".mdl");
        let base = base.strip_prefix("models/").unwrap_or(base);
        reg!(&format!("models/{base}.ani"))
    };

    // ---- ⑤ WriteTextures：材质名 → `$cdmaterials` ----
    let mut tex_name_offsets = Vec::with_capacity(desc.materials.textures.len());
    for t in &desc.materials.textures {
        let n = normalize_texture_name(&t.name, &desc.materials.search_paths);
        tex_name_offsets.push(reg!(&n));
    }
    let mut cd_offsets = Vec::with_capacity(desc.materials.search_paths.len());
    for p in &desc.materials.search_paths {
        cd_offsets.push(reg!(&normalize_cd_material(p)));
    }
    // ⭐ **FBX 几何源 ⟹ 末尾追加一条空串**（L4D2 导入器的私有行为）。
    //
    // 这条规则是**真 exe 单变量阶梯**测出来的，不是猜的
    // （`docs\_probe\oracle_cdtex_{texture,multi,order,scope,path}.js`）：
    //
    // | 用例 | 几何源 | `$cdmaterials` | `numcdtextures` |
    // |---|---|---|---|
    // | C0 | FBX | 无 | 1 = `[""]` |
    // | C1 | FBX | 1 条 | 2 = `["models\mymod\", ""]` |
    // | C3 | FBX | 2 条 | 3 = `[real, real, ""]` |
    // | C1 | **SMD** | 1 条 | **1** = `["models\mymod\"]` |
    // | C0 | **SMD** | 无 | 1 = 合成 `models/<outname>/` |
    //
    // 三条已钉死的细节：
    //
    // 1. **恒在最后**。与 `$body` 写在 `$cdmaterials` 之前还是之后无关
    //    （N1 ≡ N2）；把两条 `$cdmaterials` 夹住 `$body` 仍得
    //    `["models\aaa\", "models\zzz\", ""]`（P1）⟹ 是**写出阶段的追加**，
    //    不是「加载 FBX 时 append」。
    // 2. **恒为 `""`**，**不是** FBX 的目录 —— FBX 放子目录
    //    （`$body "sub/box.fbx"`）或 `$pushd "sub"` 时追加值仍是 `""`
    //    （Q2/Q4/Q6）。
    // 3. **per-model 一条**，不是 per-file —— 两个 FBX 几何源（Q5）也只追加
    //    一条。
    //
    // 触发范围（P2/P3/P4 + M6）：`$bodygroup { studio "x.fbx" }` ✅、
    // `$lod … replacemodel` ✅、`$collisionmodel "x.fbx"` ❌、FBX 只作
    // **动画源** ❌。⟹ 判据正好是「**bodypart 的网格源或任一 LOD 的网格源**
    // 里有没有 `.fbx`」—— `$collisionmodel` 与动画源都不在这两个位置里，
    // 所以这里只看 `bodyparts` 与 `lods`。
    //
    // ⚠️ 判据用**扩展名**（[`crate::compile::SourceKind::of`]），不用
    // `SourceKind` 以外的信息 —— R34 之后扩展名就是可靠的格式信号
    // （见 `docs/fbx-support.md` §4.3）。
    //
    // # 这条规则其实是「DMX 导入器家族」的，不是 FBX 专属
    //
    // 后续用**只换源格式、其余 QC 逐字相同**的受控实验钉死了触发面
    // （`target\dmxprobe\`，官方 `studiomdl.exe` 真跑）：
    //
    // | 几何源 | 官方日志 | `numcdtextures` |
    // |---|---|---|
    // | `.smd` | `grabbing box.smd` | **1** |
    // | `.obj` | `grabbing box.obj` | **1** |
    // | `.dmx` | `DMX Model box.dmx` | **2** |
    // | `.fbx` | `DMX Model box.fbx` | **2** |
    //
    // ⟹ `.fbx` 只是 **DMX 导入器的一个前端**（日志逐字 `DMX Model …fbx`），
    // 所以继承了 DMX 的哨兵；`.obj` 走的是另一条 loader（`grabbing …obj`），
    // 所以没有。另外把 DMX 里的材质名从 `debug/debugempty` 改成与 cd 路径
    // **匹配**的 `models/mymod/face` 后重编，哨兵**依然追加** ⟹ 是
    // **无条件**的，与材质名能否匹配上 cd 路径无关。
    //
    // ⚠️ **glTF 故意不触发这条规则**（用户裁决，见
    // `docs/gltf-support.md` §6.5）：官方对 glTF 零支持、没有 oracle 可问；
    // 与其按「同族的 DMX 会追加」外推，不如按「只写 `$cdmaterials` 给出的
    // 条目」这条**能从 QC 直接读出来的**规则来 —— 后者不需要用户猜
    // mdlc 的内部推断。代价是同一 Blender 场景的 `.fbx` 与 `.glb` 产物
    // 在 cdtexture 表上差一条，已记入偏离表。
    let has_fbx_geometry = desc.bodyparts.iter().any(|bp| {
        bp.models.iter().any(|m| {
            crate::compile::SourceKind::of(std::path::Path::new(&m.smd))
                == Ok(crate::compile::SourceKind::Fbx)
        }) || bp.models.iter().any(|m| {
            m.lods.iter().any(|l| {
                l.smd.as_deref().is_some_and(|s| {
                    crate::compile::SourceKind::of(std::path::Path::new(s))
                        == Ok(crate::compile::SourceKind::Fbx)
                })
            })
        })
    });
    if has_fbx_geometry {
        cd_offsets.push(reg!(""));
    }
    // ⛔ **不要在这里做任何**其他**推断。**
    //
    // 我一度根据语料相关性（「纹理名含 `/` ⟺ 有空 cdtexture」，
    // 1460/0/0/1841 完美分离）写了一条「自动加根目录哨兵」的规则。
    // **那是把相关性当成了因果。**
    //
    // 受控实验（`docs\_probe\smdl\cdexp{1,2,3}.qc`）证明**SMD 源**的真实机制是
    // **QC 里显式写了 `$cdmaterials ""`**：
    //
    // | QC | `numcdtextures` | 内容 |
    // |---|---|---|
    // | `$cdmaterials "models/mymod"` | 1 | `["models\mymod\"]` |
    // | `+ $cdmaterials ""`（在后） | 2 | `["models\mymod\", ""]` |
    // | `$cdmaterials ""`（在**前**） | 2 | **`["", "models\mymod\"]`** |
    //
    // ⟹ 空串就是 `Cmd_CDMaterials`（`studiomdl.cpp:6505`）对空 token
    // 的正常处理：`strdup("")` 然后 `numcdtextures++`。
    // **位置也原样保留**（cdexp3 里空串在前）。
    //
    // 语料里之所以「含 `/` 的模型都有空串」，是因为**那些 QC 都写了
    // `$cdmaterials ""`**（Crowbar 反编译出来的 QC 常见形态）——
    // 相关性的来源在这里，不在纹理名本身。
    //
    // ⇒ **SMD 源的表达方式是 TOML 的 `search_paths` 里写一个 `""`**，
    // 由用户显式给出；写出器**不做任何推断**。
    // 见 `normalize_cd_material("")` == `""`。
    //
    // 上面那条 FBX 规则是**例外**，而且它不是推断 —— 它是官方导入器自己
    // 干的事（源码不可得，但 5 个探针、20+ 个用例一致复现）。

    Ok(StringPool {
        string_buf,
        bone_name_offsets,
        bone_sp_offsets,
        tex_name_offsets,
        bp_name_offsets,
        hb_set_name_offsets,
        hb_name_offsets,
        at_name_offsets,
        seq_name_offsets,
        activity_name_offsets,
        anim_name_offsets,
        event_name_offsets,
        poseparam_name_offsets,
        ikchain_name_offsets,
        header_sp_offset,
        surface_prop,
        cd_offsets,
        flexdesc_name_offsets,
        fc_name_offsets,
        fc_type_offsets,
        fcui_name_offsets,
        includemodel_name_offsets,
        animblock_name_offset,
        transition_name_offsets,
    })
}

/// 把编译结果写成 MDL 字节。
pub fn write_mdl(compiled: &CompiledModelDesc) -> Result<WriteOutcome, WriteError> {
    let desc = &compiled.desc;
    let transitions = crate::transitions::build(compiled).map_err(WriteError::Internal)?;
    let checksum = desc.checksum();
    let version = desc.version();

    // ---- 1. 段偏移：交给声明式的 `layout` 模块算 ----
    //
    // 顺序与空段语义都集中在 `layout::SectionOffsets::compute` 里，
    // 本函数只负责「填计数」和「按算出的偏移写字节」。
    // 加新段时改 `layout` 一处即可，这里不用动。
    let anim_count = compiled.anim_count();
    let seq_count = compiled.seq_count();
    let at_count = desc.attachments.len();
    let hb_count = desc.hitbox_sets().map(|set| set.boxes.len()).sum();
    // hitbox set 的数量：**不能**用 `hb_count == 0` 判断。
    //
    // 自动生成路径下，官方在过滤前就建好了 set 并置了标志，所以
    // 「set 存在但 0 box」是合法形态 —— 实测语料 166 个模型如此，
    // 且全部带 `0x1` 标志（`probe_hbox_empty_set.js`）。
    // 用户显式写了 `$hboxset`（`set_name` 有值）时同理。
    let hb_set_count = desc.hitbox_sets().count();
    let bc_count = desc.bonecontrollers.len();
    let bone_count = desc.bones.len();
    let texture_count = desc.materials.textures.len();
    // ---- 材质表条数：**格式**上限（skin 表是有符号 short）----
    //
    // 见 [`MAXSTUDIOTEXTURES`]。引擎取材质要经 skin 表
    // （`skinref[material + family*numskinref]`），而有符号 short 最大 32767。
    if texture_count > MAXSTUDIOTEXTURES {
        return Err(WriteError::TooManyTextures {
            count: texture_count,
            max: MAXSTUDIOTEXTURES,
        });
    }
    let bp_count = compiled.bodyparts.len();
    let model_total: usize = compiled.bodyparts.iter().map(|bp| bp.models.len()).sum();
    let mesh_total: usize = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.meshes.len())
        .sum();

    // 先生成动画字节（才知道动画各区有多长）。
    let bone_parents: Vec<i32> = {
        let bi = desc.bone_index();
        desc.bones
            .iter()
            .map(|b| match b.parent.as_deref() {
                Some(p) => bi.get(p).map(|v| *v as i32).unwrap_or(-1),
                None => -1,
            })
            .collect()
    };
    // 参考姿态：必须与稍后写进 `mstudiobone_t` 的值**完全一致**，
    // 因为动画存储值是「规范化欧拉角 − 参考姿态」。
    // 用 `resolve_bone_pose` 而不是直接取 SMD 第 0 帧 —— 前者会应用
    // `$definebone` 式的显式覆盖（TOML 里的 `position`/`rotation`）。
    let ref_poses: Vec<([f32; 3], [f32; 3])> = (0..desc.bones.len())
        .map(|i| resolve_bone_pose(desc, compiled, i))
        .collect();
    // `anim_data` 在文件里的绝对起点 —— 分段的 `ALIGN16` 要按**绝对位置**
    // 算（`align16(段表绝对末尾) == 链绝对起点`），所以提前算出来传进去。
    //
    // 它只依赖 `localanim`（= `bonetablename + bones`）之前的段，与
    // `anim_data_bytes` 无关，所以能在这里安全地先算。
    //
    // ⚠️ 公式来自 `layout::anim_data_start`，**不要内联展开** ——
    // 早先这里抄了一份 `layout.rs` 的公式，加 quatinterp 时两处只改了一处，
    // 分段产物会**静默错位**（布局自检抓不到，因为两处各自都「自洽」）。
    let anim_data_abs = crate::layout::anim_data_start(&crate::layout::SectionCounts {
        bones: bone_count,
        bonecontrollers: bc_count,
        attachments: at_count,
        hitbox_sets: hb_set_count,
        hitboxes: hb_count,
        anims: anim_count,
        jiggle_bones: compiled.resolved_jiggle_bones.len(),
        quat_interp_bones: compiled.resolved_quat_interp_bones.len(),
        quat_interp_triggers: compiled
            .resolved_quat_interp_bones
            .iter()
            .map(|q| q.triggers.len())
            .sum(),
        // 内联 → `ALIGN16`；外部 `.ani` 块 → `ALIGN4`（见 `anim_data_start`）。
        anim_in_block: compiled.desc.model.anim_block_size.unwrap_or(0) > 0,
        ..Default::default()
    });
    let anim = anim_writer::write_animations(compiled, &bone_parents, &ref_poses, anim_data_abs)
        .map_err(|e| {
            WriteError::Internal(crate::tr_fmt!(
                "failed to write the animation: %{err}",
                err = e
            ))
        })?;

    // 字符串池长度要先知道才能算布局，所以先收集字符串。
    // （`build_string_pool` 返回池内容与各项的池内偏移。）
    let pool = build_string_pool(desc, compiled, &anim, &transitions)?;

    // `$keyvalues`：实测 735/3333 (22.1%) 的真实模型有它。
    //
    // 落盘格式（实测 `furnituretable001a_chunk01.mdl`，size=53）：
    //   `mdlkeyvalue\n{\nprop_data {\n"base" "Wooden.Tiny"  }\n}\n\0`
    // 开头是 **`mdlkeyvalue` 裸词**（**没有**前导引号），
    // 然后是换行 + `{` + 换行，结尾 `}\n\0`。
    // `keyvaluesize` **包含**结尾的 NUL。
    //
    // # 前导引号是一个真实 bug（曾经写错，语料 735/735 反证）
    //
    // 早先这里写的是 `b"\"mdlkeyvalue\n{\n"`（多一个 `"`），
    // 于是 `keyvaluesize` 恒比官方多 **1**，整个字符串池随之后移 1 字节。
    //
    // 源码依据 `Option_KeyValues`（`studiomdl.cpp:5763`）：
    //
    // ```c
    // AppendKeyValueText( pKeyValue, "mdlkeyvalue\n{\n" );   // ← 无引号
    // ```
    //
    // 引号是**逐 token** 加的（同函数 5784 行：`nLevel > 1` 时
    // `"\"" + token + "\" "`），只作用于块内的键值，不作用于这个前缀。
    //
    // 语料判据（`probe_keyvalue_quote.js`）：735 个有 keyvalues 的模型
    // **首字节是 `"` 的有 0 个**。
    //
    // 它排在**字符串池之前**（见 `layout.rs` 的 `keyvalues` 说明）。
    let kv_text = desc.model.key_values.as_deref().unwrap_or("");
    let kv_bytes: Vec<u8> = if kv_text.is_empty() {
        Vec::new()
    } else {
        let mut v = Vec::new();
        v.extend_from_slice(b"mdlkeyvalue\n{\n");
        v.extend_from_slice(kv_text.as_bytes());
        v.extend_from_slice(b"}\n\0");
        v
    };

    // skin 表：`uint16[numskinref * numskinfamilies]`。
    //
    // `numskinref` 恒等于 `numtextures`（语料 3301/3301）——
    // 它由 `BuildTextureGroups`（`studiomdl.cpp:686` 的
    // `g_numskinref = g_numtextures;`）决定。
    //
    // `numskinfamilies` 来自 `$texturegroup`（`Cmd_TextureGroup`，
    // `studiomdl.cpp:4741`）：没写时是 **1**（`studiomdl.cpp:684`）。
    let skin_families = if desc.materials.skin_families.is_empty() {
        1
    } else {
        desc.materials.skin_families.len()
    };
    let skin_entries = texture_count * skin_families;

    let layout = crate::layout::SectionOffsets::compute(&crate::layout::SectionCounts {
        bones: bone_count,
        bonecontrollers: bc_count,
        attachments: at_count,
        hitbox_sets: hb_set_count,
        hitboxes: hb_count,
        anims: anim_count,
        seqs: seq_count,
        transition_nodes: transitions.names.len(),
        anim_data_bytes: anim.anim_data.len(),
        seq_subtable_bytes: anim.seq_subtable_bytes(),
        bodyparts: bp_count,
        models: model_total,
        meshes: mesh_total,
        textures: texture_count,
        cdtextures: pool.cd_offsets.len(),
        keyvalues_bytes: kv_bytes.len(),
        string_bytes: pool.string_buf.len(),
        skin_entries,
        poseparams: desc.model.pose_parameters.len(),
        ikchains: desc.ikchains.len(),
        // 每条链恒有 3 个链接（`simplify.cpp:5611` 的 `numlinks = 3`），
        // 语料 278/278 条链一致。
        iklink_bytes: desc.ikchains.len() * IK_LINK_COUNT * IK_LINK_SIZE,
        iklocks: desc.ik_autoplay_locks.len(),
        // flex 系列 / mouth：真实计数（不再是 0）。
        flexdescs: desc.flex_descriptors.len(),
        flexcontrollers: desc.flex_controllers.len(),
        flexrules: compiled.resolved_flex_rules.len(),
        // flexop 区**紧密排列**：全部 op 块背靠背，每条 8 字节、本就 4 对齐
        // （`write.cpp:1511-1535` 的逐条 ALIGN4 对 8 字节 op 是 no-op，
        // 实测 fx2：abs=2152/2160/2216 无缝）。
        flexop_bytes: compiled
            .resolved_flex_rules
            .iter()
            .map(|r| r.ops.len() * FLEXOP_SIZE)
            .sum(),
        // flexcontrollerui：**含自动生成的**（每条 flexcontroller 一条）。
        flex_controller_uis: compiled.resolved_flex_controller_ui.len(),
        mouths: compiled.resolved_mouths.len(),
        // jigglebone：**不走段头**，是骨骼数组的延伸（`layout.rs` 的
        // `proc_start = ALIGN4(bone + bones*216)`，在 `bonecontroller` 之前）。
        jiggle_bones: compiled.resolved_jiggle_bones.len(),
        // `$includemodel`（`mstudiomodelgroup_t`，8 字节/条）。
        includemodels: desc.include_models.len(),
        // `$animblocksize` 外置动画块表（`mstudioanimblock_t`，8 字节/条）。
        //
        // **含开头的 `(0,0)` 哨兵** —— 实测 `ab_z1`（全部动画内联、载荷为空）
        // 仍然是 `numanimblocks = 2`：1 个哨兵 + 1 个（空）真实块。
        // 所以「有 `$animblocksize`」⟺「至少 2 项」。
        animblocks: if anim.anim_blocks.is_empty() {
            0
        } else {
            anim.anim_blocks.len() + 1
        },
        // eyeball 字节数（穿插在 mesh 段内，见 layout.rs 的 `eyeball_bytes`）。
        eyeball_bytes: compiled
            .bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .map(|m| m.eyeballs.len() * EYEBALL_SIZE)
            .sum(),
        // VTA 载荷字节数（同样穿插在 mesh 段内，见 `layout.rs::flex_bytes`）。
        //
        // 与写出侧**必须逐字节一致** —— 少算一个字节，后面
        // includemodel / animblock / texture 与字符串池全部错位。
        flex_bytes: compiled
            .bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .flat_map(|m| &m.mesh_flexes)
            .map(|v| flex_payload_bytes(v))
            .sum(),
        // `linearbone`：骨骼加速结构，**触发条件是 `bones >= 2`**。
        //
        // 实测语料完美二分（`probe_linearbone_trigger.js`）：
        // `linearboneindex != 0` ⟺ `numbones >= 2`（517 / 0 / 0 / 2816）。
        // 官方产物同样二分（578 个 artifacts：192 个 `nb>=2` 全有、
        // 386 个 `nb==1` 全无，**0 例外**）。
        has_linearbone: desc.bones.len() >= 2,
        // quatinterp（proctype 2）：记录数 + 触发器总数。
        // 与 jiggle 一样不走段头，是骨骼数组的延伸（proctype 升序，2 在前）。
        quat_interp_bones: compiled.resolved_quat_interp_bones.len(),
        quat_interp_triggers: compiled
            .resolved_quat_interp_bones
            .iter()
            .map(|q| q.triggers.len())
            .sum(),
        // `srcbonetransform`：**只有 `srcRealign` 非单位阵的骨骼**才有记录
        // （不是「跑过重排就每根一条」）。
        has_srcbonetransform: compiled.srcbonetransform_present(),
        srcbonetransform_count: compiled.srcbonetransform_bones().len(),
        // 内联 → `ALIGN16`；外部 `.ani` 块 → `ALIGN4`。
        anim_in_block: desc.model.anim_block_size.unwrap_or(0) > 0,
        // 以下段的实现是「框架已留位、功能待补」——填 0 表示本实现
        // 暂不产出。填上真实数量后 `layout` 会自动把它们排进正确位置。
        ..Default::default()
    });
    // 框架级自检：顺序写错会在这里立刻暴露，而不是产出畸形文件。
    layout.check_monotonic().map_err(|e| {
        WriteError::Internal(crate::tr_fmt!(
            "the section layout self-check failed: %{err}",
            err = e
        ))
    })?;

    // 解出本函数后面要用的名字（保持下面的代码可读）。
    let studiohdr2_off = layout.studiohdr2;
    let bone_off = layout.bone;
    let bone_controller_off = layout.bonecontroller;
    let attachment_off = layout.attachment;
    let hitbox_set_off = layout.hitboxset;
    let hb_boxes_off = hitbox_set_off + hb_set_count * HITBOX_SET_SIZE;
    let bonetablename_off = layout.bonetablename;
    let anim_off = layout.localanim;
    let anim_data_off = layout.anim_data;
    let seq_off = layout.localseq;
    let seq_sub_off = layout.seq_subtables;
    let bp_off = layout.bodypart;
    let model_off = layout.model;
    let mesh_off = layout.mesh;
    let texture_off = layout.texture;
    let strings_off = layout.strings;
    let kv_off = layout.keyvalues;
    let cd_array_off = layout.cdtexture_array;
    let skin_off = layout.skin;
    // `linearbone` 是否存在 —— 判据与 `SectionCounts` 里那一份**同源**
    // （都来自 `desc.bones.len() >= 2`），这里只是取来给写出用。
    let has_linearbone = desc.bones.len() >= 2;
    // `srcbonetransform` 是否存在 —— 同样与 `SectionCounts` 里的那份**同源**。
    let has_srcbonetransform = compiled.srcbonetransform_present();
    let srcbonetransform_count = compiled.srcbonetransform_bones().len();
    let total_len = layout.total;
    let localnode_off = layout.localnode;
    let localnodename_off = layout.localnodename;
    let flexdesc_off = layout.flexdesc;
    let flexcontroller_off = layout.flexcontroller;
    let flexrule_off = layout.flexrule;
    let flexcontrollerui_off = layout.flexcontrollerui;
    let ikchain_off = layout.ikchain;
    let mouth_off = layout.mouth;
    let poseparam_off = layout.poseparam;
    let ikautoplaylock_off = layout.ikautoplaylock;
    let includemodel_off = layout.includemodel;
    let animblock_off = layout.animblock;
    let mut buf = vec![0u8; total_len];

    let string_buf = &pool.string_buf;
    let bone_name_offsets = &pool.bone_name_offsets;
    let bone_sp_offsets = &pool.bone_sp_offsets;
    let tex_name_offsets = &pool.tex_name_offsets;
    let bp_name_offsets = &pool.bp_name_offsets;
    let hb_set_name_offsets = &pool.hb_set_name_offsets;
    let hb_name_offsets = &pool.hb_name_offsets;
    let at_name_offsets = &pool.at_name_offsets;
    let seq_name_offsets = &pool.seq_name_offsets;
    let activity_name_offsets = &pool.activity_name_offsets;
    let anim_name_offsets = &pool.anim_name_offsets;
    let event_name_offsets = &pool.event_name_offsets;
    let header_sp_offset = pool.header_sp_offset;
    let surface_prop = pool.surface_prop.clone();
    let cd_offsets = &pool.cd_offsets;

    // ---- 2. 顶点编号：所有 model 顺序拼接，供 VVD 对齐 ----
    //
    // # 多 LOD 时编号不同
    //
    // 单 LOD：`count` = 该 model 各 mesh 的 LOD 0 顶点数之和，
    // `start` = 顺序累加值 —— 与 `flatten_vertices` 完全一致。
    //
    // 多 LOD：VVD 顶点块是**跨 LOD 去重并按 LOD 排序**的池，所以
    // `count` 必须是「跨全部 LOD 去重后的总数」，`start` 必须是该 model
    // 在**排序池**里的字节偏移。用单 LOD 的算法会让
    // `mstudiomesh_t.numvertices` 偏小、`vertexindex` 错位 ——
    // 表现为引擎读到错误的顶点（不报错）。
    let multi = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .any(|m| m.lods.as_ref().is_some_and(|l| l.is_multi()));
    let lod_layout = if multi {
        let mut all = Vec::new();
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
        Some(crate::lod::build_lod_layout(&all))
    } else {
        None
    };

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for (bi, bp) in compiled.bodyparts.iter().enumerate() {
        for (mi, m) in bp.models.iter().enumerate() {
            let count: usize = m.meshes.iter().map(|k| k.vertices.len()).sum();
            spans.push(ModelVertexSpan {
                path: format!("bodyparts[{bi}].models[{mi}]"),
                start: cursor,
                count,
            });
            cursor += count;
        }
    }

    // 多 LOD 时用布局算出的真实编号覆盖 spans（以及后面写 mesh 用的值）。
    let model_vtx = lod_layout.as_ref().map(|l| {
        let counts: Vec<usize> = compiled
            .bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .map(|m| m.meshes.len())
            .collect();
        crate::lod::model_vertex_layout(l, &counts)
    });
    if let Some(mv) = &model_vtx {
        for (i, s) in spans.iter_mut().enumerate() {
            s.start = mv[i].model_vertex_index / VERTEX_STRIDE;
            s.count = mv[i].model_num_vertices;
        }
    }

    // 字符串池内偏移 → 文件绝对偏移。
    let abs = |rel: usize| -> Result<i32, WriteError> {
        let v = strings_off
            .checked_add(rel)
            .ok_or_else(|| WriteError::Internal(crate::tr("the string offset overflows")))?;
        i32::try_from(v)
            .map_err(|_| WriteError::Internal(crate::tr("the string offset overflows i32")))
    };
    let _ = &abs;

    // ---- 4. 头部 ----
    put_bytes(&mut buf, off::ID, b"IDST");
    put_i32(&mut buf, off::VERSION, version);
    put_i32(&mut buf, off::CHECKSUM, checksum);
    // ⚠️ **头部的 `name[64]` 用 `$modelname` 的原文，不做分隔符规范化。**
    //
    // 官方 `Cmd_Modelname`（`studiomdl.cpp:890-910`）把 token **原样**拷进
    // `g_pname`，`write.cpp` 再原样写进 `name[64]` ⟹ QC 写 `\` 就落 `\`。
    //
    // 实测语料：**含反斜杠 1780 个、含正斜杠 1503 个**（`survey_mdl_name_kneedir.js`）
    // —— 两种都有，取决于当年编译时 QC 里写的是什么。
    // `vm_test_group` 的 QC 写的是 `v_models\v_smg.mdl`，官方产物就是
    // `v_models\v_smg.mdl`，而 mdlc 的 `output_name()` 规范化成了 `/`。
    //
    // ⚠️ **不能改 `output_name()`** —— 它同时是**磁盘输出路径**的构造依据
    // （`v_models/v_smg.mdl`），改成反斜杠会破坏输出布局。
    // 所以这里单独用原文。
    put_cstr(&mut buf, off::NAME, off::NAME_LEN, &desc.model.name)?;
    put_i32(&mut buf, off::LENGTH, total_len as i32);

    // 包围盒：显式值优先，否则用**摆好姿势**的序列包围盒
    // （`CalcSequenceBoundingBoxes`，`simplify.cpp:7049`）。
    //
    // # 为什么不是静止姿势的顶点 AABB
    //
    // `write.cpp:2071-2087` 把 `g_sequence[0].bmin/bmax` 直接写进
    // `hull_min/hull_max`，而那个盒子是**逐帧摆姿势**算出来的，并且
    // 还把每根骨骼的**渲染包围盒**（含自动 hitbox 的 bbox）并了进去。
    //
    // 差别在实测里很明显：`ip`（几何 z ∈ [-8,-8]）用静止姿势 AABB 得
    // `hull_max[2] = -8`，官方是 **0** —— 因为自动 hitbox 从全 0 起步，
    // 把 `bbmax[2]` 撑到了 0。用错口径会让 hull 偏小。
    //
    // `illumposition` 的回退同样依赖这个盒子（见下），所以两者必须一致。
    let render_bounds = crate::compile::bone_render_bounds(desc, compiled);
    let pose_bounds = if compiled.sequences.is_empty() {
        None
    } else {
        crate::compile::sequence_pose_bounds(desc, compiled, 0, &render_bounds)
    };
    let (auto_min, auto_max) = pose_bounds
        .or_else(|| compiled.bounds())
        .unwrap_or(([0.0; 3], [0.0; 3]));
    let hull_min = desc.model.hull_min.unwrap_or(auto_min);
    let hull_max = desc.model.hull_max.unwrap_or(auto_max);
    put_vec3(
        &mut buf,
        off::EYE_POSITION,
        desc.model
            .eye_position
            .map(qc_axis_to_model)
            .unwrap_or([0.0; 3]),
    );
    // illumposition：显式值走轴变换；**缺省**则回退到「第 0 个 sequence 的
    // 包围盒中心」（`simplify.cpp:7204` 的 `SetIlluminationPosition`）。
    //
    // 回退路径**不经过** `Cmd_Illumposition`，所以中心值**不再**做轴变换
    // —— 它直接取自 `g_sequence[0].bmin/bmax`。
    //
    // 时序上有个坑：`SetIlluminationPosition()` 在 `SimplifyModel()` 里被调用
    // （simplify.cpp:7325），**早于** `write.cpp:2071` 用显式 `$bbox` 覆盖
    // `g_sequence[0].bmin/bmax`。所以回退取的必须是**自动算出**的包围盒，
    // 不能用显式覆盖后的 `hull_min/hull_max` —— 因此这里用 `auto_min/auto_max`。
    //
    // 语料验证：无 `.phy` 且 illum 非零的 778 个真实模型里，768 个满足
    // `illumposition == (hull_min+hull_max)/2`（中位数偏差 0.0000）；
    // 剩余偏差大的都是写死了 `$illumposition` 的（见
    // `docs/_probe/verify_illum_fallback.js`）。多数模型不写 `$bbox`，
    // 此时 auto 与 hull 相同，两种写法等价。
    // `$illumposition` 有**两种**形式，落盘语义不同（`oracle_illumposition6.js`）：
    //
    // * `$illumposition x y z` ⟹ 落盘 `[-y, x, z]`（做轴变换）；
    // * `$illumposition x y z <骨骼>` ⟹ 落盘**原样 `[x, y, z]`**、**不做**轴变换。
    //
    // 判据是 `ModelMeta::illum_position_from_bone`（QC 前端在吃到第 4 个 token
    // 时置位）。TOML 表达不了 4 参数形式，所以 TOML 侧恒走轴变换分支。
    let illum = match desc.model.illum_position {
        Some(v) if desc.model.illum_position_from_bone => v,
        Some(v) => qc_axis_to_model(v),
        None if !compiled.sequences.is_empty() => [
            (auto_min[0] + auto_max[0]) * 0.5,
            (auto_min[1] + auto_max[1]) * 0.5,
            (auto_min[2] + auto_max[2]) * 0.5,
        ],
        None => [0.0; 3],
    };
    put_vec3(&mut buf, off::ILLUM_POSITION, illum);
    put_vec3(&mut buf, off::HULL_MIN, hull_min);
    put_vec3(&mut buf, off::HULL_MAX, hull_max);
    // Clipping bounds are separate from the movement hull in studiohdr_t.
    // Unspecified clipping bounds retain StudioMDL's zero default.
    put_vec3(
        &mut buf,
        off::VIEW_BB_MIN,
        desc.model.view_bb_min.unwrap_or([0.0; 3]),
    );
    put_vec3(
        &mut buf,
        off::VIEW_BB_MAX,
        desc.model.view_bb_max.unwrap_or([0.0; 3]),
    );

    let mut flags = desc.model.extra_flags.unwrap_or(0);
    if desc.model.static_prop {
        flags |= FLAG_STATIC_PROP;
    }
    if desc.model.constant_directional_light_dot.is_some() {
        flags |= 1 << 13;
    }
    put_i32(&mut buf, off::FLAGS, flags);
    // `$contents`：**默认就是 `CONTENTS_SOLID`（= 1）**，不是 0。
    //
    // 源码：`studiomdl.cpp:5031` 的 `static int s_nDefaultContents = CONTENTS_SOLID;`
    // 与 `write.cpp:187` 的 `phdr->contents = GetDefaultContents();` ——
    // 无条件默认，与 `$contents` 是否书写、是否自动生成 hitbox **都无关**。
    //
    // 语料印证（`probe_contents_mass.js`）：3333 个模型里 **3281 个是 1**，
    // 其余 52 个是 `CONTENTS_GRATE`（= 8，全部带 `.phy`，来自显式
    // `$contents "grate"`）。四格交叉（静态道具/普通 × 有/无 `.phy`）
    // **全部**是 1 —— 没有任何一格是 0。
    //
    // 早先写 0 并把差异归因于 `AUTOGENERATED_HITBOX` 是**错的**：
    // hitbox 只影响 `bone[].flags` 的 `BONE_USED_BY_HITBOX` 位。
    let contents = desc.model.contents.unwrap_or(CONTENTS_SOLID);
    put_i32(&mut buf, off::CONTENTS, contents);
    // 质量：`.mdl` 的 mass **就是**碰撞模型的总质量
    // （`write.cpp:2092` `phdr->mass = GetCollisionModelMass();`），
    // 所以这里与 `.phy` 的 `editparams.totalmass` 共用 `effective_mass()`。
    // 实测默认值是 **1.0**（不是 0）—— 语料 827/835 个无 `.phy` 的模型都是 1.0。
    put_f32(&mut buf, off::MASS, desc.physics.effective_mass());

    put_i32(&mut buf, off::BONE_COUNT, bone_count as i32);
    put_i32(&mut buf, off::BONE_OFFSET, bone_off as i32);
    // `bonecontroller`：实测 3333/3333 个真实模型的**计数为 0**，
    // 但偏移字段写「该段应处的位置」（= 骨骼表末尾），不是 0。
    // 这是 studiomdl 对空段的统一约定 —— 见下面的空段偏移表。
    put_i32(&mut buf, off::BONE_CONTROLLER_COUNT, bc_count as i32);
    put_i32(
        &mut buf,
        off::BONE_CONTROLLER_OFFSET,
        bone_controller_off as i32,
    );
    // `bonetablename`：`byte[numbones]` 的**骨骼名索引表**。
    //
    // # 为什么必须排序，而不能写恒等置换
    //
    // 官方 `write.cpp:389-399`（`WriteBoneInfo` 尾部）：
    // ```cpp
    //     for ( i = 0; i < phdr->numbones; i++ ) pBoneTable[i] = i;
    //     qsort( pBoneTable, phdr->numbones, sizeof(byte), BoneNameCompare );
    // ```
    // 比较器是 `write.cpp:162-169` 的 `strcmpi`（**大小写不敏感**）。
    //
    // 引擎侧唯一的消费者 `Studio_BoneIndexByName`（`bone_setup.cpp:5725-5750`）
    // 对这张表做**二分查找**：
    // ```cpp
    //     int mid = (start + end) >> 1;
    //     int cmp = Q_stricmp( pbones[pBoneTable[mid]].pszName(), pName );
    // ```
    // 表未排序 ⟹ 二分退化，绝大多数查找返回 −1。
    //
    // 实测（`docs/_probe/` 的 `bsearch_sim.js`，用引擎算法逐名复现）：
    //   官方 119/119 命中；mdlc 修前 **5/119**（114 个返回 −1）；NekoMDL 119/119。
    //
    // 后果链：`bone_merge_cache.cpp:69-71`
    // `int parentBoneIndex = Studio_BoneIndexByName( m_pFollowHdr, pOwnerBones[i].pszName() );`
    // `if ( parentBoneIndex < 0 ) continue;` ⟹ 骨骼不入 `m_MergedBones`
    // ⟹ `:90-93` `if ( !m_MergedBones.Count() ) m_nFollowBoneSetupMask = 0;`
    // ⟹ **55 条 `$bonemerge` 全部失效**，`C_BaseAnimating::LookupBone`
    // （`c_baseanimating.cpp:1043`）对所有调用方也一律失败。
    put_i32(
        &mut buf,
        off::BONE_TABLE_NAME_OFFSET,
        bonetablename_off as i32,
    );
    buf[bonetablename_off..bonetablename_off + bone_count]
        .copy_from_slice(&bone_table_by_name(&desc.bones));
    // `pVertexBase`(0x170) / `pIndexBase`(0x174)：**只在工具进程内使用的
    // 运行时指针，从不落盘**（`studio.h:2179-2180`），官方产物恒为 0。
    // 实测 13/13 个官方 `.mdl` 两个字段都是 0（`corpus_170.js`）。
    // 清零循环已经覆盖了它们，这里不再重复写。
    // `studiohdr2`：实测 3333/3333 个真实模型都有，且固定放在 408。
    put_i32(&mut buf, off::STUDIO_HDR2_OFFSET, studiohdr2_off as i32);
    // 下面这些段的**计数为 0**（本实现不产出），但**偏移要写「该段应处
    // 的位置」而不是 0** —— 实测 3333 个真实模型一致如此
    // （只有 31 个「纯动画」模型的 localanim/localseq 写 0）。
    //
    // 早先的实现把这些字段一律写 0，与官方产物逐字段对照时会产生
    // 十几处「差异」—— 那些不是语义错误，但会掩盖真正的差异。
    let as_i32 = |v: usize| v as i32;
    put_i32(
        &mut buf,
        off::LOCAL_NODE_COUNT,
        transitions.names.len() as i32,
    );
    for (index, offset) in pool.transition_name_offsets.iter().enumerate() {
        put_i32(
            &mut buf,
            localnodename_off + index * 4,
            (strings_off + offset) as i32,
        );
    }
    buf[localnode_off..localnode_off + transitions.routes.len()]
        .copy_from_slice(&transitions.routes);
    put_i32(&mut buf, off::LOCAL_NODE_OFFSET, as_i32(localnode_off));
    put_i32(
        &mut buf,
        off::LOCAL_NODE_NAME_OFFSET,
        as_i32(localnodename_off),
    );
    put_i32(&mut buf, off::FLEX_DESC_OFFSET, as_i32(flexdesc_off));
    put_i32(
        &mut buf,
        off::FLEX_CONTROLLER_OFFSET,
        as_i32(flexcontroller_off),
    );
    put_i32(&mut buf, off::FLEX_RULE_OFFSET, as_i32(flexrule_off));
    // `mstudioflexcontrollerui_t`：计数是**真实值**（含每条 flexcontroller
    // 自动产生的那条），偏移是 flexop 区末尾（空段时与 `ikchainindex` 重合，
    // 也是自然位置 —— 官方空段时同写法，`ip_official.mdl` 的 `+0x184` = 1472
    // = 同文件的 `ikchainindex`）。
    put_i32(
        &mut buf,
        off::FLEX_CONTROLLER_UI_COUNT,
        as_i32(compiled.resolved_flex_controller_ui.len()),
    );
    put_i32(
        &mut buf,
        off::FLEX_CONTROLLER_UI_OFFSET,
        as_i32(flexcontrollerui_off),
    );
    put_i32(&mut buf, off::IK_CHAIN_OFFSET, as_i32(ikchain_off));
    put_i32(&mut buf, off::MOUTH_OFFSET, as_i32(mouth_off));
    put_i32(
        &mut buf,
        off::LOCAL_POSE_PARAM_OFFSET,
        as_i32(poseparam_off),
    );
    put_i32(
        &mut buf,
        off::LOCAL_IK_AUTOPLAY_LOCK_OFFSET,
        as_i32(ikautoplaylock_off),
    );
    put_i32(&mut buf, off::INCLUDEMODEL_OFFSET, as_i32(includemodel_off));
    put_i32(&mut buf, off::ANIMBLOCK_OFFSET, as_i32(animblock_off));
    for o in [
        // `FLEX_DESC_COUNT`/`FLEX_CONTROLLER_COUNT`/`FLEX_RULE_COUNT`/`MOUTH_COUNT`
        // **不在这里** —— 它们有真实值，由下面的段写出逻辑填。
        off::IK_CHAIN_COUNT,
        off::LOCAL_POSE_PARAM_COUNT,
        off::LOCAL_IK_AUTOPLAY_LOCK_COUNT,
        // `INCLUDEMODEL_COUNT` **不在这里** —— 有真实值（`$includemodel`）。
        off::ANIMBLOCK_COUNT,
        off::ANIMBLOCK_NAME_OFFSET,
        off::ANIMBLOCK_INDEX,
        // `pVertexBase` / `pIndexBase`：工具期指针，**从不落盘**，官方恒 0。
        off::VERTEX_BASE,
        off::CONST_DIRECTIONAL_LIGHT_DOT,
        // `KEY_VALUE_OFFSET` / `KEY_VALUE_SIZE` **不在这里** —— 它们有真实值，
        // 由第 11 步无条件写出（**空段也要写自然偏移**，实测 3333/3333）。
        off::SURFACE_PROP_OFFSET,
        off::TEXTURE_COUNT,
        off::TEXTURE_OFFSET,
        off::CD_TEXTURE_COUNT,
        off::CD_TEXTURE_OFFSET,
        off::SKIN_REFERENCE_COUNT,
        off::SKIN_FAMILY_COUNT,
        off::SKIN_OFFSET,
        off::BODY_PART_COUNT,
        off::BODY_PART_OFFSET,
    ] {
        put_i32(&mut buf, o, 0);
    }
    buf[off::CONST_DIRECTIONAL_LIGHT_DOT] = desc.model.constant_directional_light_dot.unwrap_or(0);
    buf[off::ALLOWED_ROOT_LODS] = compiled.desc.model.allowed_root_lods;
    put_i32(&mut buf, off::TEXTURE_COUNT, texture_count as i32);
    put_i32(&mut buf, off::TEXTURE_OFFSET, texture_off as i32);
    put_i32(&mut buf, off::CD_TEXTURE_COUNT, cd_offsets.len() as i32);
    put_i32(&mut buf, off::CD_TEXTURE_OFFSET, cd_array_off as i32);
    // skin 表（`mstudiotextureref_t`）：`numskinref` 恒等于 `numtextures`。
    put_i32(&mut buf, off::SKIN_REFERENCE_COUNT, texture_count as i32);
    put_i32(&mut buf, off::SKIN_FAMILY_COUNT, skin_families as i32);
    put_i32(&mut buf, off::SKIN_OFFSET, skin_off as i32);
    put_i32(&mut buf, off::BODY_PART_COUNT, bp_count as i32);
    put_i32(&mut buf, off::BODY_PART_OFFSET, bp_off as i32);
    // hitbox set / 附着点。
    //
    // **空段也要写自然位置，不能写 0** —— 这是 studiomdl 的统一约定，
    // 实测 3333/3333（`docs/_probe/verify_empty_offsets.js` 逐段验证了
    // 17 个可选段，**没有一个**在空时写 0）：
    //
    // ```text
    // bonecontroller  空 3333 个 → 全部自然位置
    // attachment      空 3007 个 → 全部自然位置
    // flexdesc/flexcontroller/flexrule  空 3325 个 → 全部自然位置
    // ikchain/mouth/poseparam/ikautoplaylock  空 3247~3322 个 → 全部自然位置
    // includemodel/animblock  空 3212~3286 个 → 全部自然位置
    // ```
    put_i32(&mut buf, off::HITBOX_SET_COUNT, hb_set_count as i32);
    put_i32(&mut buf, off::HITBOX_SET_OFFSET, hitbox_set_off as i32);
    put_i32(&mut buf, off::LOCAL_ATTACHMENT_COUNT, at_count as i32);
    put_i32(
        &mut buf,
        off::LOCAL_ATTACHMENT_OFFSET,
        attachment_off as i32,
    );
    // 姿势参数（`mstudioposeparamdesc_t`，20 字节/条）。
    //
    // 字段布局（`studio.h:599-607`）：`sznameindex` `flags` `start` `end` `loop`。
    // `sznameindex` 是**相对该记录自身**的偏移（`AddToStringTable` 语义），
    // 与 `mstudiobone_t.sznameindex` 一致。
    //
    // `flags` 只写 `STUDIO_LOOPING`（`0x1`）—— 实测语料 285 条里只有 0 与 1
    // 两种取值（`probe_poseparam.js`）。
    let pose_params = &desc.model.pose_parameters;
    put_i32(
        &mut buf,
        off::LOCAL_POSE_PARAM_COUNT,
        pose_params.len() as i32,
    );
    for (i, p) in pose_params.iter().enumerate() {
        let base = poseparam_off + i * POSE_PARAM_SIZE;
        let rel = abs(pool.poseparam_name_offsets[i])? - base as i32;
        put_i32(&mut buf, base, rel);
        // `flags` 与 `loop` 由 `loop_mode` **一并**决定 —— 官方产物里
        // 两者永远同进同退（语料 285 条无交叉），所以不给出独立开关。
        let (flags, loop_value) = match p.loop_mode {
            Some(m) => (STUDIO_LOOPING, m.loop_value(p.start, p.end)),
            None => (0, 0.0),
        };
        put_i32(&mut buf, base + 4, flags);
        put_f32(&mut buf, base + 8, p.start);
        put_f32(&mut buf, base + 12, p.end);
        put_f32(&mut buf, base + 16, loop_value);
    }
    // ---- 9a. `$includemodel`（`mstudiomodelgroup_t`，8 字节/条）----
    //
    // 布局：`int szlabelindex`（**恒 0**，语料 47/47 —— 官方从没填过 label，
    // 于是 `pszLabel()` 解出空串）+ `int sznameindex`（**相对该记录自身**）。
    //
    // 段位置：**在 `texture` 之前**（`write.cpp:1826-1835` 写在 `WriteModel()`
    // 函数体末尾，而 `WriteTextures()` 之后才被调用）。实测公式
    // `includemodelindex == animblockindex - 8*numincludemodels` **3333/3333**。
    //
    // 官方**只写名字**，从不读被包含的 `.mdl`（合并是引擎运行时做的）。
    put_i32(
        &mut buf,
        off::INCLUDEMODEL_COUNT,
        as_i32(desc.include_models.len()),
    );
    for (i, name) in desc.include_models.iter().enumerate() {
        let base = includemodel_off + i * MODEL_GROUP_SIZE;
        // `szlabelindex` 恒 0 —— 不写（缓冲区已是 0），但显式记一笔意图。
        put_i32(&mut buf, base, 0);
        let rel = abs(pool.includemodel_name_offsets[i])? - base as i32;
        put_i32(&mut buf, base + 4, rel);
        let _ = name;
    }

    // ---- 9b. `$animblocksize` 外置动画块表（`mstudioanimblock_t`，8 字节/条）----
    //
    // 布局：`int datastart` + `int dataend`，都是 **`.ani` 文件内**的绝对偏移。
    // 第 0 项恒为 `(0, 0)` 的**哨兵**，真实块从下标 1 开始。
    //
    // 头部三个字段：
    //   `+0x15C animblocknameindex` —— `"models/<模型名>.ani"`（**绝对**偏移）
    //   `+0x160 numanimblocks`      —— 块数（**含哨兵**；有 `$animblocksize` 时 >= 2）
    //   `+0x164 animblockindex`     —— 表在 `.mdl` 内的绝对偏移
    //
    // 实测 `ab_z4`：`+0x15C = 1644`、`numanimblocks = 2`、
    // `animblockindex = 1520`，表为 `[0,0) [416,468)`。
    //
    // ⚠️ 表项的偏移是**相对 `.ani` 文件**的，不是相对 `.mdl` —— 所以这里是
    // 唯一一处「段内容引用了另一个文件」的地方。
    //
    // `animblocknameindex` **无条件**写 —— 没有块时它指向池里的空串
    // （实测 3212/3212），不是 0。
    put_i32(
        &mut buf,
        off::ANIMBLOCK_NAME_OFFSET,
        abs(pool.animblock_name_offset)?,
    );
    let mut ani_file: Option<Vec<u8>> = None;
    if !anim.anim_blocks.is_empty() {
        let (file, table) = anim_writer::build_ani_file(&anim.anim_blocks);
        put_i32(&mut buf, off::ANIMBLOCK_COUNT, as_i32(table.len()));
        for (i, (start, end)) in table.iter().enumerate() {
            let base = animblock_off + i * ANIMBLOCK_SIZE;
            put_i32(&mut buf, base, *start);
            put_i32(&mut buf, base + 4, *end);
        }
        ani_file = Some(file);
    }

    // IK 链（`mstudioikchain_t` 16 字节 + 紧跟其后的 `mstudioiklink_t[3]`）。
    //
    // 布局（`write.cpp:1537-1560`）：
    //   1. 先写**全部** `mstudioikchain_t`（16 字节/条）；
    //   2. 再依次写每条链的链接块（`numlinks * 28` 字节），**链间连续**；
    //   3. `linkindex` 是**相对该链自身**的偏移（`pData - (byte*)pikchain`）。
    //
    // 三段链由**骨骼表**推出，不是 QC 给的（`simplify.cpp:5604-5638`）：
    //   `link[2]` = `$ikchain` 里写的末端骨骼
    //   `link[1]` = 它的父（膝/肘）
    //   `link[0]` = 父的父（胯/肩）
    // 父链不足两代时官方是**硬错误**（"too close to root"）；mdlc 在这里
    // 直接拒绝，不产出畸形文件。
    // 三段链的骨骼下标：末端、父、祖父。
    //
    // **注意**：即使 `desc.ikchains` 为空，偏移字段也要写**自然位置**而不是 0
    // —— 实测 3333/3333 个真实模型如此（`probe_iklock_empty_off.js`：
    // 有链无锁的 67 个模型 `localikautoplaylockindex == 自然位置` 67/67；
    // 无链无锁的 3255 个 `offLock == offIK` 3255/3255，无一写 0）。
    // 这是 studiomdl 对**所有**空段的统一约定，见 `layout.rs` 的文档。
    put_i32(&mut buf, off::IK_CHAIN_COUNT, desc.ikchains.len() as i32);
    put_i32(&mut buf, off::IK_CHAIN_OFFSET, ikchain_off as i32);
    let mut link_cursor = ikchain_off + desc.ikchains.len() * IK_CHAIN_SIZE;
    let ik_bone_index = desc.bone_index();
    for (i, ch) in desc.ikchains.iter().enumerate() {
        let base = ikchain_off + i * IK_CHAIN_SIZE;
        let rel = abs(pool.ikchain_name_offsets[i])? - base as i32;
        put_i32(&mut buf, base, rel);
        // `linktype` 恒为 0（语料 278/278）。
        put_i32(&mut buf, base + 4, 0);
        put_i32(&mut buf, base + 8, IK_LINK_COUNT as i32);
        put_i32(&mut buf, base + 12, (link_cursor - base) as i32);
        // 三段链的骨骼下标：末端、父、祖父。
        let tip = *ik_bone_index.get(ch.bone.as_str()).ok_or_else(|| {
            WriteError::Internal(crate::tr_fmt!(
                "the ikchain %{chain} references a bone that does not exist: %{bone}",
                chain = format!("{:?}", ch.name),
                bone = format!("{:?}", ch.bone)
            ))
        })?;
        let mid = usize::try_from(bone_parents[tip]).map_err(|_| {
            WriteError::Internal(crate::tr_fmt!(
                "the ikchain %{name} is too close to the root bone; there is no knee/elbow",
                name = format!("{:?}", ch.name)
            ))
        })?;
        let root = usize::try_from(bone_parents[mid]).map_err(|_| {
            WriteError::Internal(crate::tr_fmt!(
                "the ikchain %{name} is too close to the root bone; there is no hip/shoulder",
                name = format!("{:?}", ch.name)
            ))
        })?;
        // 写出顺序是 `link[0]`（胯/肩）→ `link[1]`（膝/肘）→ `link[2]`（末端）。
        // 只有 `link[0]` 会拿到 `kneeDir`（`studiomdl.cpp:4521-4529`）。
        for (k, (bone, knee)) in [(root, ch.knee_dir), (mid, None), (tip, None)]
            .into_iter()
            .enumerate()
        {
            let lp = link_cursor + k * IK_LINK_SIZE;
            put_i32(&mut buf, lp, bone as i32);
            put_vec3(&mut buf, lp + 4, knee.unwrap_or([0.0; 3]));
        }
        link_cursor += IK_LINK_COUNT * IK_LINK_SIZE;
    }
    // IK 自动播放锁：`chain` 写的是**链下标**（名字在 `LinkIKLocks` 里解析）。
    put_i32(
        &mut buf,
        off::LOCAL_IK_AUTOPLAY_LOCK_COUNT,
        desc.ik_autoplay_locks.len() as i32,
    );
    // 同样写**自然位置**（= 链接区末尾），空时也不写 0 —— 语料 3322/3322。
    put_i32(
        &mut buf,
        off::LOCAL_IK_AUTOPLAY_LOCK_OFFSET,
        ikautoplaylock_off as i32,
    );
    for (i, lk) in desc.ik_autoplay_locks.iter().enumerate() {
        let base = ikautoplaylock_off + i * IK_LOCK_SIZE;
        let chain_idx = desc
            .ikchains
            .iter()
            .position(|c| c.name == lk.chain)
            .ok_or_else(|| {
                WriteError::Internal(crate::tr_fmt!(
                    "ikautoplaylock references an ikchain that does not exist: %{chain}",
                    chain = format!("{:?}", lk.chain)
                ))
            })?;
        put_i32(&mut buf, base, chain_idx as i32);
        put_f32(&mut buf, base + 4, lk.pos_weight);
        put_f32(&mut buf, base + 8, lk.local_q_weight);
        // `flags` 与 `unused[4]` 官方**不写**，保持 0（语料 22/22）。
    }

    // ---- 9b. flex 系列 / mouth ----
    //
    // 写出顺序（`write.cpp:1480-1587` + 语料反解的 flexcontrollerui 段）：
    //   flexdesc(4B) → flexcontroller(20B) → flexrule 数组(12B) → 全部 flexop(8B，
    //   每条 rule 内联、紧密排列) → flexcontrollerui(20B) → … → mouth(20B)
    //
    // 所有 `sz*index` 都**相对本记录自身**（`AddToStringTable` 语义）。
    //
    // flexdesc：`szFACSindex`（相对自身）。
    put_i32(
        &mut buf,
        off::FLEX_DESC_COUNT,
        as_i32(desc.flex_descriptors.len()),
    );
    for (i, _f) in desc.flex_descriptors.iter().enumerate() {
        let base = flexdesc_off + i * FLEXDESC_SIZE;
        let rel = abs(pool.flexdesc_name_offsets[i])? - base as i32;
        put_i32(&mut buf, base, rel);
    }
    // flexcontroller：`sztypeindex`/`sznameindex`（相对自身），
    // `localToGlobal` 恒 -1（`write.cpp:1506` 硬编码，语料 278/278）。
    put_i32(
        &mut buf,
        off::FLEX_CONTROLLER_COUNT,
        as_i32(desc.flex_controllers.len()),
    );
    for (i, f) in desc.flex_controllers.iter().enumerate() {
        let base = flexcontroller_off + i * FLEXCONTROLLER_SIZE;
        let type_rel = abs(pool.fc_type_offsets[i])? - base as i32;
        let name_rel = abs(pool.fc_name_offsets[i])? - base as i32;
        put_i32(&mut buf, base, type_rel);
        put_i32(&mut buf, base + 4, name_rel);
        put_i32(&mut buf, base + 8, -1);
        put_f32(&mut buf, base + 12, f.min);
        put_f32(&mut buf, base + 16, f.max);
    }
    // flexrule 数组 + 内联 flexop（**紧密排列**）。
    //
    // `opindex[i]` **相对本 rule 自身**，精确公式（语料 502/502 命中，
    // `rsrch_flex_ops.js`；实测 fx2 的 [36,32,76] 与之完全一致）：
    //
    // ```text
    // opindex[i] = numflexrules*12 - i*12 + (Σ_{k<i} numops[k]) * 8
    // ```
    //
    // 即：所有 op 块在文件里**连续紧密**排列（每条 8 字节、本就 4 对齐，
    // `write.cpp:1532` 的逐条 ALIGN4 是 no-op），`opindex` 只是因为基准是
    // 「自己那条 rule」而显得往回跳。第 0 条的 `opindex` 恰为 `numflexrules*12`。
    let n_rules = compiled.resolved_flex_rules.len();
    put_i32(&mut buf, off::FLEX_RULE_COUNT, as_i32(n_rules));
    // 先整体占位规则数组，再逐条把 op 块写在数组之后。
    let mut op_cursor = flexrule_off + n_rules * FLEXRULE_SIZE;
    let mut cum_ops = 0usize; // Σ_{k<i} numops[k]
    for (i, r) in compiled.resolved_flex_rules.iter().enumerate() {
        let base = flexrule_off + i * FLEXRULE_SIZE;
        put_i32(&mut buf, base, r.flex);
        put_i32(&mut buf, base + 4, r.ops.len() as i32);
        // 公式与「游标 - 本 rule 起点」等价；用游标直接算，更直观且不易错。
        let opindex = (op_cursor - base) as i32;
        debug_assert_eq!(
            opindex,
            (n_rules * FLEXRULE_SIZE - i * FLEXRULE_SIZE + cum_ops * FLEXOP_SIZE) as i32,
            "opindex 公式应与游标一致"
        );
        put_i32(&mut buf, base + 8, opindex);
        // 写该 rule 的 op 块（紧跟其后，紧密排列）。
        for op in &r.ops {
            put_i32(&mut buf, op_cursor, op.op);
            match op.d {
                crate::model::FlexOpData::Index(idx) => put_i32(&mut buf, op_cursor + 4, idx),
                crate::model::FlexOpData::Value(v) => put_f32(&mut buf, op_cursor + 4, v),
                crate::model::FlexOpData::None => put_i32(&mut buf, op_cursor + 4, 0),
            }
            op_cursor += FLEXOP_SIZE;
        }
        cum_ops += r.ops.len();
    }
    // flexcontrollerui：`sznameindex` 相对自身；`szindex0/1` 相对自身但
    // 指向 flexcontroller 数组（故恒负）。`remaptype` 恒 0、`szindex2` 恒 0。
    //
    // 段起点 = flexop 区末尾（`flexcontrollerui_off`），已紧密排列好。
    for (i, u) in compiled.resolved_flex_controller_ui.iter().enumerate() {
        let base = flexcontrollerui_off + i * FLEXCONTROLLERUI_SIZE;
        let name_rel = abs(pool.fcui_name_offsets[i])? - base as i32;
        put_i32(&mut buf, base, name_rel);
        // `szindex0` 指向 fc[u.fc0]（相对本 ui 记录自身，恒负）。
        let fc0_abs = flexcontroller_off + u.fc0 as usize * FLEXCONTROLLER_SIZE;
        put_i32(&mut buf, base + 4, fc0_abs as i32 - base as i32);
        // `szindex1`：单声道写 0；stereo 对指向 fc[u.fc1]。
        let sz1 = match u.fc1 {
            Some(fc1) => {
                let fc1_abs = flexcontroller_off + fc1 as usize * FLEXCONTROLLER_SIZE;
                fc1_abs as i32 - base as i32
            }
            None => 0,
        };
        put_i32(&mut buf, base + 8, sz1);
        put_i32(&mut buf, base + 12, 0); // szindex2 恒 0
        buf[base + 16] = 0; // remaptype 恒 0（FLEXCONTROLLER_REMAP_PASSTHRU）
        buf[base + 17] = u8::from(u.stereo);
        // unused[2] 保持 0。
    }
    // mouth：`bone`/`forward`/`flexdesc`（无字符串字段）。
    put_i32(
        &mut buf,
        off::MOUTH_COUNT,
        as_i32(compiled.resolved_mouths.len()),
    );
    for (i, m) in compiled.resolved_mouths.iter().enumerate() {
        let base = mouth_off + i * MOUTH_SIZE;
        put_i32(&mut buf, base, m.bone);
        put_vec3(&mut buf, base + 4, m.forward);
        put_i32(&mut buf, base + 16, m.flexdesc);
    }

    // 动画：animdesc 与 seqdesc 一一对应（本实现每个序列一个动画）。
    put_i32(&mut buf, off::LOCAL_ANIM_COUNT, anim_count as i32);
    put_i32(
        &mut buf,
        off::LOCAL_ANIM_OFFSET,
        if anim_count == 0 { 0 } else { anim_off as i32 },
    );
    put_i32(&mut buf, off::LOCAL_SEQ_COUNT, seq_count as i32);
    // ⚠️ `localseqindex` 的条件是 **`seq_count == 0`**，不是 `anim_count == 0`。
    //
    // 早先这里跟着 `localanimindex` 一起按 `anim_count == 0` 写 0 ——
    // 对普通模型两者同进同退（没有动画就没有序列），所以一直没暴露。
    // **`$declaresequence` 打破了这个巧合**：空壳序列**没有动画**
    // （`numlocalanim = 0`）但**有序列**（`numlocalseq > 0`）。
    //
    // 实测官方（只有 2 条空壳的 QC）：
    // ```text
    // numlocalanim=0  localanimindex=1532   ← 有值！
    // numlocalseq =2  localseqindex =1532
    // ```
    // 即 `localanimindex` **也**不是 0 —— 它指向 `ALIGN4(bonetablename
    // + numbones)`（= 1532），只是因为 `numlocalanim == 0` 而没有数组。
    //
    // 所以两个字段都**不再看 `anim_count`**：布局已经把偏移算好了
    // （`layout.rs` 的 `localanim` / `localseq` 不依赖数量）。
    // 真实 survivor 产物 `survivor_teenangst.mdl` 也是这个形态。
    put_i32(&mut buf, off::LOCAL_SEQ_OFFSET, seq_off as i32);
    // surfaceprop 是**文件绝对偏移**（头部字段一律绝对）。
    put_i32(
        &mut buf,
        off::SURFACE_PROP_OFFSET,
        if surface_prop.is_empty() {
            0
        } else {
            abs(header_sp_offset)?
        },
    );

    // ---- 4b. studiohdr2 ----
    //
    // 实测 3333/3333 个真实模型都有 `studiohdr2`，且**固定放在 408**。
    // 字段（`studiohdr2_t`）：
    //
    // ```text
    // +0x00 numsrcbonetransform       +0x04 srcbonetransformindex
    // +0x08 illumpositionattachmentindex  +0x0C flMaxEyeDeflection
    // +0x10 linearboneindex           +0x14 sznameindex
    // +0x18 m_nBoneFlexDriverCount    +0x1C m_nBoneFlexDriverIndex
    // +0x20 reserved[56]
    // ```
    //
    // **`linearboneindex` 与 `sznameindex` 是相对 `studiohdr2` 自身**的偏移
    // （与头部其它 index 的「绝对偏移」不同）。
    //
    // `linearboneindex`（`+0x10`）在**第 13 步**写 —— 那时段内容已经落盘，
    // 这里只写 0 占位（段不存在时它本来就是 0）。
    // `sznameindex`（`+0x14`）实测 0% 模型有 —— studiomdl 不写它。
    put_i32(&mut buf, studiohdr2_off, 0);
    put_i32(&mut buf, studiohdr2_off + 0x04, 0);
    // `illumpositionattachmentindex`（`+0x08`）—— `$illumposition x y z <骨骼>`
    // 合成的那个 `__illumPosition` 附着点在附着点表里的 **1 起**下标；
    // 没写过 4 参数形式就是 **0**。
    //
    // 官方在解析到该命令时把它写成「当时的附着点数」，所以写多次时**最后一次胜出**
    // —— 等价于「**最后一个**合成附着点的 1 起下标」（`oracle_illumposition5.js`
    // 的 `illum_twice`：两个附着点、`attIdx == 2`）。
    //
    // 顺序敏感：`att_then_illum` 得 2、`illum_then_att` 得 1
    // （`oracle_illumposition4.js`、`oracle_illumposition7.js` 的 `A illum B` 得 2）。
    let illum_att_index = desc
        .attachments
        .iter()
        .rposition(|at| at.synthetic)
        .map_or(0, |i| i as i32 + 1);
    put_i32(&mut buf, studiohdr2_off + 0x08, illum_att_index);
    // `flMaxEyeDeflection`（`+0x0C`）—— `$maxeyedeflection`。
    //
    // 官方（反汇编 `0x00450270`）落盘 `cos(deg2rad(输入))`，
    // 所以 TOML 收**度**、这里做转换（见 [`ModelMeta::max_eye_deflection`]）。
    //
    // 不写该命令时官方留 **0**（`.bss` 未初始化），引擎读到 0 会回退到
    // `cos(30°)` —— 所以缺省与写 30 渲染等价、仅字节不同。
    put_f32(
        &mut buf,
        studiohdr2_off + 0x0C,
        desc.model
            .max_eye_deflection
            .map_or(0.0, |deg| (deg.to_radians()).cos()),
    );
    put_i32(&mut buf, studiohdr2_off + 0x10, 0);
    put_i32(&mut buf, studiohdr2_off + 0x14, 0);
    put_i32(&mut buf, studiohdr2_off + 0x18, 0);
    put_i32(&mut buf, studiohdr2_off + 0x1C, 0);

    // ---- 5. 骨骼表 ----
    let bone_index: HashMap<&str, usize> = desc.bone_index();
    // 按用途计算 flags（顶点 / hitbox / 附着点 / bonemerge，且沿父链传播）。
    let computed_flags = compute_bone_flags(desc, compiled, &bone_index);
    let bone_contents = crate::contents::bone_contents(desc).map_err(WriteError::Internal)?;
    let calculated_alignments;
    let alignments = if desc.model.rotation_alignments.len() == desc.bones.len() {
        &desc.model.rotation_alignments
    } else {
        calculated_alignments = crate::bone_alignment::calculate(compiled);
        &calculated_alignments
    };
    for (i, b) in desc.bones.iter().enumerate() {
        let base = bone_off + i * BONE_SIZE;
        let parent = match b.parent.as_deref() {
            Some(p) => *bone_index.get(p).ok_or_else(|| {
                WriteError::Internal(crate::tr_fmt!(
                    "the parent bone %{bone} did not pass validation",
                    bone = format!("{p:?}")
                ))
            })? as i32,
            None => -1,
        };
        // **相对骨骼自身**的名字偏移：绝对位置（strings_off + 池内偏移）减去骨骼起点。
        let name_rel = (strings_off + bone_name_offsets[i]) as i64 - base as i64;
        put_i32(
            &mut buf,
            base + bone_off::NAME_INDEX,
            i32::try_from(name_rel).map_err(|_| {
                WriteError::Internal(crate::tr("the bone name relative offset overflows i32"))
            })?,
        );
        put_i32(&mut buf, base + bone_off::PARENT, parent);
        // 6 个 `bonecontroller` 槽：**默认全 `-1`**，不是 0。
        //
        // 源码：`write.cpp:287-292` —— 官方先把整张表置 `-1`，
        // 再按 `g_bonecontroller[i].bone` 把**实际用到**的槽填上下标：
        //
        // ```cpp
        // for (i = 0; i < g_numbones; i++)
        //     for (j = 0; j < 6; j++)
        //         pbone[i].bonecontroller[j] = -1;
        // ```
        //
        // 实测：这个真实项目的官方产物里 **118/118** 根骨骼的
        // `bonecontroller[0]` 都是 `-1`（`num bonecontrollers = 0`）。
        // 写 0 会被引擎当成「指向下标 0 的控制器」。
        for k in 0..6 {
            put_i32(&mut buf, base + bone_off::BONE_CONTROLLER + k * 4, -1);
        }
        // 参考姿态：描述显式值优先，否则取自 SMD 的 skeleton 第 0 帧。
        let (bone_pos, bone_rot) = resolve_bone_pose(desc, compiled, i);
        put_vec3(&mut buf, base + bone_off::POSITION, bone_pos);
        // quat 与 rot 必须**同时**写：引擎的 InitPose 读 quat，
        // 留 0 是非法四元数（模长 0，无法归一化），会让参考姿态失效。
        // 实测：官方 bone[0] 的 rot=[π/2,0,0] 对应 quat=[0.7071,0,0,0.7071]。
        let mut q = crate::bone_math::angle_quaternion(bone_rot);
        if let Some(alignment) = alignments[i] {
            q = crate::bone_alignment::align(alignment, q);
            for (component, value) in alignment.into_iter().enumerate() {
                put_f32(
                    &mut buf,
                    base + bone_off::Q_ALIGNMENT + component * 4,
                    value,
                );
            }
        }
        for (k, v) in q.iter().enumerate() {
            put_f32(&mut buf, base + bone_off::QUAT + k * 4, *v);
        }
        put_vec3(&mut buf, base + bone_off::ROTATION, bone_rot);
        // 参考姿态的 positionScale / rotationScale 是**全局一份**，
        // 由动画写出器计算（解码器用它把 i16 采样还原成弧度/位置）。
        // 没有动画时写 1.0（此时 scale 不参与运算）。
        if anim_count > 0 {
            let (ps, rs) = anim.bone_scales[i];
            put_vec3(&mut buf, base + bone_off::POSITION_SCALE, ps);
            put_vec3(&mut buf, base + bone_off::ROTATION_SCALE, rs);
        } else {
            for k in 0..3 {
                put_f32(&mut buf, base + bone_off::POSITION_SCALE + k * 4, 1.0);
                put_f32(&mut buf, base + bone_off::ROTATION_SCALE + k * 4, 1.0);
            }
        }
        // 骨骼 flags：显式值优先，否则用**按用途计算**的结果。
        // 写 0 会让骨骼被判定为「未被任何东西使用」，可能被优化掉；
        // 一律写常量则会把未使用的骨骼误标为已使用。
        let bone_flags = b.flags.unwrap_or(computed_flags[i])
            | if alignments[i].is_some() {
                crate::bone_alignment::FIXED_ALIGNMENT
            } else {
                0
            };
        put_i32(&mut buf, base + bone_off::FLAGS, bone_flags);
        put_i32(&mut buf, base + bone_off::CONTENTS, bone_contents[i]);
        let bone_sp = match bone_sp_offsets[i] {
            Some(rel) => i32::try_from((strings_off + rel) as i64 - base as i64).map_err(|_| {
                WriteError::Internal(crate::tr("the surfaceprop relative offset overflows i32"))
            })?,
            None if !surface_prop.is_empty() => i32::try_from(
                (strings_off + header_sp_offset) as i64 - base as i64,
            )
            .map_err(|_| {
                WriteError::Internal(crate::tr("the inherited surfaceprop offset overflows i32"))
            })?,
            None => 0,
        };
        put_i32(&mut buf, base + bone_off::SURFACE_PROP_INDEX, bone_sp);
        // `physicsbone`（+0xAC）：「该骨骼受哪个碰撞 solid 的物理模拟驱动」。
        //
        // 官方 `write.cpp:195` 是 `pbone[i].physicsbone =
        // g_bonetable[i].physicsBoneIndex;`，而那个索引由
        // `collisionmodel.cpp:2141-2184` 填（见 [`CompiledModelDesc::physics_bone`]）。
        //
        // **没有碰撞模型 / 单 solid 时官方留 0** —— 实测（排除陈旧产物）
        // 单 solid 2459/2459 全 0、多 solid 38/38 非平凡，完美二分。
        // 所以 `None` 时**不写**（保持 `vec![0u8; ...]` 的初值）。
        if let Some(pb) = &compiled.physics_bone
            && let Some(v) = pb.get(i)
        {
            put_i32(&mut buf, base + bone_off::PHYSICS_BONE, *v);
        }
    }

    // ---- 5a. 程序化骨骼块（quatinterp，proctype 2）----
    //
    // **proctype 升序**：quatinterp（2）排在 jiggle（5）**之前**
    // （`write.cpp:215-265` 的写出序是 axisinterp → quatinterp → aimat）。
    //
    // # 两层结构（与 jiggle 的单层不同）
    //
    // ```text
    // 记录数组 N * 12  →  ALIGN4  →  逐记录的 Σtriggers*48
    // ```
    //
    // `triggerindex` **相对该记录自身**（`write.cpp:254`），
    // 第一个记录的触发器**紧接在记录数组之后**（中间只有那个 `ALIGN4`）。
    //
    // 实测（`rsrch_proc_detail.js`，`survivor_producer.mdl`）：
    // `boneEnd=18376` → 记录数组 `18376..18472`（8*12）
    // → `ALIGN4` = 18472 → 触发器 18472..20200（36*48）
    // → jiggle 数组 20200..21640 → `bonecontrollerindex = 21640` ✓
    //
    // `procindex` **相对该骨骼记录自身**，`proctype` 恒 2
    // （`STUDIO_PROC_QUATINTERP`）。`TagProceduralBones` 给每根
    // 程序化骨骼置 `BONE_ALWAYS_PROCEDURAL`（`0x04`），与 jiggle 同一处处理。
    //
    // 记录顺序 = **TOML 书写顺序**（与 jiggle 同一个约定）。
    if !compiled.resolved_quat_interp_bones.is_empty() {
        // `ALIGN4` 的本地版本（`ani_writer::align4` 是私有的）。
        let align4 = |v: usize| (v + 3) & !3;
        let proc_start = align4(layout.bone + bone_count * BONE_SIZE);
        let records_end =
            proc_start + compiled.resolved_quat_interp_bones.len() * QUATINTERP_BONE_SIZE;
        // 触发器区起点 = `ALIGN4(记录数组末尾)`。
        let mut trig = align4(records_end);
        for (i, q) in compiled.resolved_quat_interp_bones.iter().enumerate() {
            let rec = proc_start + i * QUATINTERP_BONE_SIZE;
            let bone_rec = layout.bone + q.bone as usize * BONE_SIZE;
            put_i32(&mut buf, bone_rec + bone_off::PROC_TYPE, 2);
            // 相对骨骼记录**自身**。
            put_i32(
                &mut buf,
                bone_rec + bone_off::PROC_INDEX,
                (rec as i64 - bone_rec as i64) as i32,
            );
            put_i32(&mut buf, rec, q.control);
            put_i32(&mut buf, rec + 4, q.triggers.len() as i32);
            // `triggerindex` 相对**本记录自身**。
            put_i32(&mut buf, rec + 8, (trig as i64 - rec as i64) as i32);
            // 逐条触发器（48 字节，顺序见 `ResolvedQuatInterpTrigger`）。
            for t in &q.triggers {
                put_f32(&mut buf, trig, t.inv_tolerance);
                for (k, v) in t.trigger.iter().enumerate() {
                    put_f32(&mut buf, trig + 4 + k * 4, *v);
                }
                for (k, v) in t.pos.iter().enumerate() {
                    put_f32(&mut buf, trig + 0x14 + k * 4, *v);
                }
                for (k, v) in t.quat.iter().enumerate() {
                    put_f32(&mut buf, trig + 0x20 + k * 4, *v);
                }
                trig += QUATINTERP_INFO_SIZE;
            }
        }
    }

    // ---- 5a-bis. 程序化骨骼块（jigglebone，proctype 5）----
    //
    // **唯一不走段头的一项**：紧跟骨骼数组（`ALIGN4` 后），在 `bonecontroller`
    // **之前**（`write.cpp:214-285` 的 `WriteBoneInfo`）。实测 25/25 个模型
    // 满足「首个程序化块起点 == `ALIGN4(boneindex + numbones*216)`」，
    // 且 jiggle 块末尾 == `bonecontrollerindex`（`rsrch_jiggle_layout.js`）。
    //
    // `procindex` **相对该骨骼记录自身**（`write.cpp:222`），
    // `proctype` 恒 5（`STUDIO_PROC_JIGGLE`）。
    //
    // 记录顺序 = **QC 书写顺序**（不是骨骼下标序！实测 `jig8`：
    // QC 写 knee(2)→ankle(3)→hip(1)，文件里 @1528 knee / @1648 ankle /
    // @1768 hip —— 调研报告 §6.4 的「按骨骼下标升序」是**错的**）。
    {
        // 程序化块起点：`ALIGN4(骨骼数组末尾)`。
        //
        // ⚠️ jiggle 排在 **quatinterp 之后**（proctype 升序），所以起点要
        // 跳过 quatinterp 的两层结构：`N*12 → ALIGN4 → Σtriggers*48`。
        let align4 = |v: usize| (v + 3) & !3;
        let proc_start = align4(layout.bone + bone_count * BONE_SIZE);
        let proc_start = if compiled.resolved_quat_interp_bones.is_empty() {
            proc_start
        } else {
            let records_end =
                proc_start + compiled.resolved_quat_interp_bones.len() * QUATINTERP_BONE_SIZE;
            align4(records_end)
                + compiled
                    .resolved_quat_interp_bones
                    .iter()
                    .map(|q| q.triggers.len())
                    .sum::<usize>()
                    * QUATINTERP_INFO_SIZE
        };
        for (i, j) in compiled.resolved_jiggle_bones.iter().enumerate() {
            let rec = proc_start + i * JIGGLE_BONE_SIZE;
            let bone_rec = layout.bone + j.bone as usize * BONE_SIZE;
            put_i32(&mut buf, bone_rec + bone_off::PROC_TYPE, 5);
            // 相对骨骼记录**自身**。
            put_i32(
                &mut buf,
                bone_rec + bone_off::PROC_INDEX,
                (rec as i64 - bone_rec as i64) as i32,
            );
            // 30 个 4 字节槽，顺序见 `ResolvedJiggleBone`（**实测偏移**）。
            put_i32(&mut buf, rec, j.flags);
            let vals = [
                j.length,
                j.tip_mass,
                j.yaw_stiffness,
                j.yaw_damping,
                j.pitch_stiffness,
                j.pitch_damping,
                j.along_stiffness,
                j.along_damping,
                j.angle_limit,
                j.min_yaw,
                j.max_yaw,
                j.yaw_friction,
                j.yaw_bounce,
                j.min_pitch,
                j.max_pitch,
                j.pitch_friction,
                j.pitch_bounce,
                j.base_mass,
                j.base_stiffness,
                j.base_damping,
                j.base_min_left,
                j.base_max_left,
                j.base_left_friction,
                j.base_min_up,
                j.base_max_up,
                j.base_up_friction,
                j.base_min_forward,
                j.base_max_forward,
                j.base_forward_friction,
            ];
            debug_assert_eq!(vals.len(), 29, "120 - 4(flags) = 116 = 29 * 4");
            for (k, v) in vals.iter().enumerate() {
                put_f32(&mut buf, rec + 4 + k * 4, *v);
            }
        }
    }

    // ---- 5b. poseToBone：必须在所有骨骼姿态解析完之后统一算 ----
    //
    // 这一步之前只写「单位旋转 + 平移取负」，对 `rotation = 0` 的合成模型
    // 看不出问题，但真实模型的第一根骨骼几乎都带 π/2 旋转，那样写会让
    // 顶点被错误旋转 —— 模型在游戏里扭曲，且编译器不会报任何错。
    //
    // ⚠️ `ptb` **不能**关在块里：第 9 步的 `absolute` 附着点要用它做左乘
    // （官方 `LinkAttachments()`，`simplify.cpp:5388`）。
    //
    // 矩阵本身走 [`crate::compile::bone_pose_to_bone`] —— 与 `calcblend`
    // 共用唯一实现（`absolute` 附着点的 `local` 和落盘的那一份必须是**同一个**
    // 矩阵，两份实现漂移过一次）。
    let ptb = {
        let mut parents: Vec<i32> = Vec::with_capacity(bone_count);
        for b in desc.bones.iter() {
            parents.push(match b.parent.as_deref() {
                Some(name) => *bone_index.get(name).ok_or_else(|| {
                    WriteError::Internal(crate::tr_fmt!(
                        "the parent bone %{bone} did not pass validation",
                        bone = format!("{name:?}")
                    ))
                })? as i32,
                None => -1,
            });
        }
        let ptb = crate::compile::bone_pose_to_bone(desc, compiled, &parents);
        for (i, m) in ptb.iter().enumerate() {
            let base = bone_off + i * BONE_SIZE;
            for (k, v) in m.iter().enumerate() {
                put_f32(&mut buf, base + bone_off::POSE_TO_BONE + k * 4, *v);
            }
        }
        ptb
    };

    for (index, controller) in desc.bonecontrollers.iter().enumerate() {
        let bone = *bone_index.get(controller.bone.as_str()).ok_or_else(|| {
            WriteError::Internal("The bone controller target did not pass validation".into())
        })?;
        let slot = controller.axis_slot().ok_or_else(|| {
            WriteError::Internal("The bone controller axis did not pass validation".into())
        })?;
        put_i32(
            &mut buf,
            bone_off + bone * BONE_SIZE + bone_off::BONE_CONTROLLER + slot * 4,
            index as i32,
        );
        let base = bone_controller_off + index * BONE_CONTROLLER_SIZE;
        put_i32(&mut buf, base, bone as i32);
        put_i32(&mut buf, base + 4, controller.type_.unwrap_or(1));
        put_f32(&mut buf, base + 8, controller.start.unwrap_or(0.0));
        put_f32(&mut buf, base + 12, controller.end.unwrap_or(0.0));
        put_i32(&mut buf, base + 16, controller.rest.unwrap_or(0));
        put_i32(&mut buf, base + 20, controller.input_field.unwrap_or(0));
    }

    // ---- 6. 材质表 ----
    for (i, t) in desc.materials.textures.iter().enumerate() {
        let base = texture_off + i * TEXTURE_SIZE;
        // 名字偏移同样是**相对该材质自身**的。
        let rel = (strings_off + tex_name_offsets[i]) as i64 - base as i64;
        put_i32(
            &mut buf,
            base + tex_off::NAME_INDEX,
            i32::try_from(rel).map_err(|_| {
                WriteError::Internal(crate::tr("the material name relative offset overflows i32"))
            })?,
        );
        put_i32(&mut buf, base + tex_off::FLAGS, t.flags.unwrap_or(0));
    }

    // ---- 7. body part / model / mesh ----
    let mut model_cursor = 0usize;
    let mut flex_mesh_cursor = 0usize;
    // mesh 段的**字节**游标：每个 model 占 `nummeshes*116 + numeyeballs*172`，
    // eyeball 数组紧跟该 model 的 mesh 数组之后（穿插在 mesh 段内，
    // `eyeballindex == meshindex + nummeshes*116`，语料 3620/3620）。
    let mut mesh_region_cursor = 0usize;
    // `mstudiomesh_t.meshid`：mesh 的**全局序号**，跨 bodypart/model 连续。
    // 实测 5585/5585 个真实 mesh 的 0x20 恰好等于这个序号。
    let mut mesh_id = 0usize;
    for (bi, bp) in compiled.bodyparts.iter().enumerate() {
        let base = bp_off + bi * BODY_PART_SIZE;
        // 名字与 modelindex 都是**相对该 body part 自身**的。
        let name_rel = (strings_off + bp_name_offsets[bi]) as i64 - base as i64;
        put_i32(
            &mut buf,
            base + bp_off::NAME_INDEX,
            i32::try_from(name_rel).map_err(|_| {
                WriteError::Internal(crate::tr(
                    "the body part name relative offset overflows i32",
                ))
            })?,
        );
        put_i32(&mut buf, base + bp_off::NUM_MODELS, bp.models.len() as i32);
        put_i32(&mut buf, base + bp_off::BASE, bp.base);
        put_i32(
            &mut buf,
            base + bp_off::MODEL_INDEX,
            ((model_off + model_cursor * MODEL_SIZE) - base) as i32,
        );

        for m in &bp.models {
            let mbase = model_off + model_cursor * MODEL_SIZE;
            let span = &spans[model_cursor];
            // 该 model 的 mesh 数组起点（mesh 段内，eyeball 穿插排布）。
            let this_mesh_off = mesh_off + mesh_region_cursor;
            // 名字在编译期已定好（显式 name，否则 SMD 文件名）。
            let name = m.name.clone();
            put_cstr(
                &mut buf,
                mbase + model_off::NAME,
                model_off::NAME_LEN,
                &name,
            )?;
            put_i32(&mut buf, mbase + model_off::TYPE, 0);
            put_f32(&mut buf, mbase + model_off::BOUNDING_RADIUS, 0.0);
            put_i32(
                &mut buf,
                mbase + model_off::NUM_MESHES,
                m.meshes.len() as i32,
            );
            // meshindex 是**相对该 model 自身**的偏移。
            put_i32(
                &mut buf,
                mbase + model_off::MESH_INDEX,
                (this_mesh_off - mbase) as i32,
            );
            // ---- 每 model 顶点数：**格式**上限（不是 studiomdl 的 65536）----
            //
            // `span.count` 是该 model 跨全部 LOD 去重后的顶点总数
            // （= `pmodel[i].numvertices` 的对应量，见 `ModelVertexSpan`）。
            //
            // 上限由 `vertexindex` 的**字节偏移算术**推出：
            // `i32::MAX / VERTEX_STRIDE` ≈ 4473 万 —— 见
            // [`MAXSTUDIOVERTS_PER_MODEL`] 的推导。
            //
            // ⚠️ 这里**不再**用官方的 `MAXSTUDIOVERTS = 65536`：
            // 那是引擎运行时顶点缓存的假定（`write.cpp:1644` 的注释：
            // `so that we don't screw up decal vert caching in the runtime`），
            // **不是文件格式约束**。本项目只保证「产出格式合法的二进制」，
            // 所以能表示的都写出来。
            //
            // 逐 mesh 的 u16 约束仍由 `vtx_writer` 负责（那个是真格式约束）。
            if span.count > MAXSTUDIOVERTS_PER_MODEL {
                return Err(WriteError::TooManyModelVertices {
                    path: format!("bodyparts[{bi}].models[{model_cursor}]"),
                    count: span.count,
                    max: MAXSTUDIOVERTS_PER_MODEL,
                });
            }
            // `vertexindex` 的字节偏移也必须装得进 i32 —— 与上面的上限
            // 是同一个算术，这里显式再查一次以防将来改动 `VERTEX_STRIDE`。
            let vertex_byte_offset = span.start * VERTEX_STRIDE;
            if vertex_byte_offset > i32::MAX as usize {
                return Err(WriteError::Internal(crate::tr_fmt!(
                    "the vertexindex byte offset %{offset} of bodyparts[%{bi}].models[%{mi}] overflows int32",
                    offset = vertex_byte_offset,
                    bi = bi,
                    mi = model_cursor
                )));
            }
            // `tangentsindex` 是同一个算术、更小的 stride（16 < 48），
            // 所以上面的检查成立时它必然也成立；这里显式再查一次以防将来
            // `TANGENT_STRIDE` 被改大。
            let tangent_byte_offset = span.start * TANGENT_STRIDE;
            if tangent_byte_offset > i32::MAX as usize {
                return Err(WriteError::Internal(crate::tr_fmt!(
                    "the tangentsindex byte offset %{offset} of bodyparts[%{bi}].models[%{mi}] overflows int32",
                    offset = tangent_byte_offset,
                    bi = bi,
                    mi = model_cursor
                )));
            }
            put_i32(&mut buf, mbase + model_off::NUM_VERTICES, span.count as i32);
            // **相对 VVD 顶点块的字节偏移**，不是顶点下标。
            put_i32(
                &mut buf,
                mbase + model_off::VERTEX_INDEX,
                (span.start * VERTEX_STRIDE) as i32,
            );
            // `tangentsindex` 与 `vertexindex` **同构**：相对 VVD **切线块**的
            // 字节偏移。官方 `write.cpp:1666-1674` 是并排的两段：
            //
            // ```cpp
            // ALIGN16( externalVertexIndex );
            // pmodel[i].vertexindex   = (int)externalVertexIndex;
            // externalVertexIndex   += pmodel[i].numvertices * sizeof(mstudiovertex_t); // 48
            // ALIGN4( externalTangentsIndex );
            // pmodel[i].tangentsindex = (int)externalTangentsIndex;
            // externalTangentsIndex += pmodel[i].numvertices * sizeof(Vector4D);        // 16
            // ```
            //
            // 实测（`v_dual_pistolA`）：官方 = 0 / 374080 / 443248 / 817328 / 886496，
            // 每个都恰好是前面各 model `numvertices × 16` 的累计；NekoMDL 同样正确。
            //
            // ⚠️ 早先这里写死 0，理由是「切线块在 VVD 里，MDL 不需要知道」——
            // **是错的**：引擎的 `GetGlobalTangentIndex`（`studio.h:1327-1332`）
            // 用 `tangentsindex / sizeof(Vector4D)` 当**全局切线下标基准**，
            // 恒为 0 会让 model[1..] 全部去读 model[0] 的切线数据。
            // 这个字段还被 `cmp_mdl_full.js` 的 `LAYOUT_FIELDS` 吞掉，
            // 与 `bonetablebynameindex` 是**同一类盲区**。
            put_i32(
                &mut buf,
                mbase + model_off::TANGENT_INDEX,
                (span.start * TANGENT_STRIDE) as i32,
            );
            // eyeball：`numeyeballs` + `eyeballindex`（相对该 model 自身，
            // 紧跟该 model 的 mesh 数组之后 = `meshindex + nummeshes*116`）。
            let this_eyeball_off = this_mesh_off + m.meshes.len() * MESH_SIZE;
            put_i32(
                &mut buf,
                mbase + model_off::NUM_EYEBALLS,
                m.eyeballs.len() as i32,
            );
            put_i32(
                &mut buf,
                mbase + model_off::EYEBALL_INDEX,
                (this_eyeball_off - mbase) as i32,
            );

            // mesh 的 vertexoffset 是**相对该 model**的顶点下标，
            // 不是全局下标。实测依据：官方 `v_autoshotgun.mdl` 的
            // bp[2].model[0] 起点是 VVD 顶点 99996，但它的 mesh[0].vertexoffset
            // 是 **0**（不是 99996）。
            //
            // 多 LOD 时用 `model_vtx` 算好的值 —— 那时 `numvertices` 是
            // **跨全部 LOD 去重后**的总数，不是 LOD 0 的顶点数
            // （实测 53/53 个真实 fixup 模型的 R6 不变式）。
            //
            // VTA 载荷区紧跟**本 model 的 eyeball 数组**之后
            // （`write.cpp:1683-1722`：eyeball 写完才轮到 flexes）。
            //
            // ⚠️ 这里要**两个**游标：
            //   * `flex_alloc` —— 在 mesh 循环里分配各 mesh 的 `flexindex`；
            //   * `flex_cursor` —— mesh 循环**之后**真正的写入位置。
            // 用同一个变量会让 mesh 循环把它推到末尾，随后的写入直接越界
            // （实测症状：`put_i32 越界: off=2476 len=2456`）。
            let flex_base = this_eyeball_off + m.eyeballs.len() * EYEBALL_SIZE;
            let mut flex_alloc = flex_base;
            let mut mesh_vertex_cursor = 0usize;
            for (ki, mesh) in m.meshes.iter().enumerate() {
                let kbase = this_mesh_off + ki * MESH_SIZE;
                // 多 LOD 时取布局算出的 (总数, 偏移)，否则用 LOD 0 的。
                let (num_vertices, vertex_offset) = match &model_vtx {
                    Some(mv) => mv[model_cursor].meshes[ki],
                    None => (mesh.vertices.len(), mesh_vertex_cursor),
                };
                put_i32(&mut buf, kbase + mesh_off::MATERIAL, mesh.material as i32);
                // modelindex = **model 绝对位置 − mesh 绝对位置**（负值）。
                // 实测：bp[0] 是 -2572、bp[1] 是 -4512、bp[2] 是 -4480 ——
                // 各不相同，所以**不能**写死 -148（那只是「单 mesh 且
                // mesh 紧跟 model」时的巧合）。
                put_i32(
                    &mut buf,
                    kbase + mesh_off::MODEL_INDEX,
                    mbase as i32 - kbase as i32,
                );
                put_i32(
                    &mut buf,
                    kbase + mesh_off::NUM_VERTICES,
                    num_vertices as i32,
                );
                put_i32(
                    &mut buf,
                    kbase + mesh_off::VERTEX_OFFSET,
                    vertex_offset as i32,
                );
                // `numflexes` / `flexindex`：该 mesh 的 VTA 载荷。
                //
                // `flexindex` 是**相对本 mesh 自身**的偏移（`write.cpp:1755`
                // 的 `IsInt24(pData - (byte *)&pmesh[m])`），指向紧跟在该
                // model 的 eyeball 数组之后的 flex 区。
                //
                // ⚠️ 这段代码原先**硬写 0**（VTA 未实现）。现在写真实值：
                // 0 表示该 mesh 没有形状 —— 完全正常，语料里
                // `survivor_coach` 的 5 个 mesh 就有 2 个是 0。
                let mesh_flexes = m.mesh_flexes.get(ki).map(|v| v.as_slice()).unwrap_or(&[]);
                if mesh_flexes.is_empty() {
                    put_i32(&mut buf, kbase + mesh_off::NUM_FLEXES, 0);
                    put_i32(&mut buf, kbase + mesh_off::FLEX_INDEX, 0);
                } else {
                    put_i32(
                        &mut buf,
                        kbase + mesh_off::NUM_FLEXES,
                        mesh_flexes.len() as i32,
                    );
                    // 该 mesh 的 flex 区起点（绝对），相对本 mesh 折算。
                    put_i32(
                        &mut buf,
                        kbase + mesh_off::FLEX_INDEX,
                        (flex_alloc - kbase) as i32,
                    );
                    flex_alloc += flex_payload_bytes(mesh_flexes);
                } // eyeball 打标：被 eyeball 引用的 mesh 写 `materialtype=1`/
                // `materialparam=j`（`write.cpp:1689-1690`），其余写 0。
                // 实测 8/8 条 eyeball 都有对应打标 mesh，且无其它 mesh
                // `materialtype != 0`（`rsrch_flex_chain2.js`）。
                let (mat_type, mat_param) = match mesh.eyeball_tag {
                    Some(j) => (1, j as i32),
                    None => (0, 0),
                };
                put_i32(&mut buf, kbase + mesh_off::MATERIAL_TYPE, mat_type);
                put_i32(&mut buf, kbase + mesh_off::MATERIAL_PARAM, mat_param);
                // `meshid`：mesh 的**全局序号**（跨 bodypart/model 连续）。
                // 实测 5585/5585 个真实 mesh 的 0x20 恰好等于这个序号。
                put_i32(&mut buf, kbase + mesh_off::MESH_ID, mesh_id as i32);
                // `center`：实测真实文件恒为 0，不写（缓冲区已零初始化）。
                let _ = mesh_off::CENTER;
                // `numLODVertexes[8]`（累计值）。单 LOD 时全部槽位都是
                // `numvertices`（实测：`[8,8,8,8,8,8,8,8]`）；
                // 多 LOD 时用布局算出的逐 LOD 累计值 + 尾部 ripple。
                match &model_vtx {
                    Some(mv) => {
                        let row = &mv[model_cursor].mesh_lod_counts[ki];
                        let n_lods = row.len();
                        for (n, v) in row.iter().enumerate() {
                            put_i32(&mut buf, kbase + mesh_off::NUM_LOD_VERTEXES + n * 4, *v);
                        }
                        // 尾部槽位 ripple 成最后一个有效值。
                        let last = row.last().copied().unwrap_or(num_vertices as i32);
                        for n in n_lods..crate::vvd::MAX_NUM_LODS {
                            put_i32(&mut buf, kbase + mesh_off::NUM_LOD_VERTEXES + n * 4, last);
                        }
                    }
                    None => {
                        for n in 0..crate::vvd::MAX_NUM_LODS {
                            put_i32(
                                &mut buf,
                                kbase + mesh_off::NUM_LOD_VERTEXES + n * 4,
                                num_vertices as i32,
                            );
                        }
                    }
                }
                mesh_id += 1;
                mesh_vertex_cursor += mesh.vertices.len();
            }

            // ---- 该 model 的 eyeball 数组（紧跟其 mesh 数组之后）----
            //
            // `mstudioeyeball_t` = **172 字节**（反解 65 个候选唯一命中，
            // `rsrch_eyeball_size.js`）。`sznameindex` 恒 0（空串），
            // `texture`/`unused*` 恒 0。
            for (ej, eb) in m.eyeballs.iter().enumerate() {
                let ebase = this_eyeball_off + ej * EYEBALL_SIZE;
                put_i32(&mut buf, ebase, 0); // sznameindex 恒 0（空串）
                put_i32(&mut buf, ebase + 0x04, eb.bone);
                put_vec3(&mut buf, ebase + 0x08, eb.org);
                put_f32(&mut buf, ebase + 0x14, eb.zoffset);
                put_f32(&mut buf, ebase + 0x18, eb.radius);
                put_vec3(&mut buf, ebase + 0x1C, eb.up);
                put_vec3(&mut buf, ebase + 0x28, eb.forward);
                put_i32(&mut buf, ebase + 0x34, 0); // texture 恒 0
                put_i32(&mut buf, ebase + 0x38, 0); // unused1
                put_f32(&mut buf, ebase + 0x3C, eb.iris_scale);
                put_i32(&mut buf, ebase + 0x40, 0); // unused2
                for (k, v) in eb.upperflexdesc.iter().enumerate() {
                    put_i32(&mut buf, ebase + 0x44 + k * 4, *v);
                }
                for (k, v) in eb.lowerflexdesc.iter().enumerate() {
                    put_i32(&mut buf, ebase + 0x50 + k * 4, *v);
                }
                for (k, v) in eb.uppertarget.iter().enumerate() {
                    put_f32(&mut buf, ebase + 0x5C + k * 4, *v);
                }
                for (k, v) in eb.lowertarget.iter().enumerate() {
                    put_f32(&mut buf, ebase + 0x68 + k * 4, *v);
                }
                put_i32(&mut buf, ebase + 0x74, eb.upperlidflexdesc);
                put_i32(&mut buf, ebase + 0x78, eb.lowerlidflexdesc);
                // unused[4] @0x7C / m_bNonFACS @0x8C / unused3[3] @0x8D /
                // unused4[7] @0x90 全保持 0（缓冲区已零初始化）。
            }

            // ---- 该 model 的 VTA 载荷（紧跟其 eyeball 数组之后）----
            //
            // `write.cpp:1721-1822`：对每个 mesh，若 `numflexes > 0` 就写
            // `mstudioflex_t[numflexes]` + 各自的 `mstudiovertanim_t[]`。
            // mesh 的 `flexindex` 已在上面填好（相对该 mesh 自身）。
            let mut flex_cursor = flex_base;
            for (mesh, mesh_flexes) in m.mesh_flexes.iter().enumerate() {
                if mesh_flexes.is_empty() {
                    continue;
                }
                flex_cursor = write_mesh_flexes(
                    &mut buf,
                    flex_cursor,
                    mesh_flexes,
                    lod_layout
                        .as_ref()
                        .map(|layout| (layout, flex_mesh_cursor + mesh)),
                )?;
            }

            flex_mesh_cursor += m.meshes.len();
            model_cursor += 1;
            // mesh 段游标：mesh 数组 + 本 model 的 eyeball + **VTA 载荷**。
            // 漏掉 flex 字节会让下一个 model 的 mesh 数组整体前移 ——
            // 表现为后续 model 的材质/顶点全错，而**不报任何错**。
            mesh_region_cursor += m.meshes.len() * MESH_SIZE
                + m.eyeballs.len() * EYEBALL_SIZE
                + m.mesh_flexes
                    .iter()
                    .map(|v| flex_payload_bytes(v))
                    .sum::<usize>();
        }
    }

    // ---- 8. hitbox set / attachment ----
    let mut hitbox_cursor = 0;
    for (index, set) in desc.hitbox_sets().enumerate() {
        let base = hitbox_set_off + index * HITBOX_SET_SIZE;
        // 名字偏移是**相对自身**的（与骨骼/材质/body part 一致）。
        let rel = (strings_off + hb_set_name_offsets[index]) as i64 - base as i64;
        put_i32(
            &mut buf,
            base + hbset_off::NAME_INDEX,
            i32::try_from(rel).map_err(|_| {
                WriteError::Internal(crate::tr(
                    "the hitbox set name relative offset overflows i32",
                ))
            })?,
        );
        put_i32(
            &mut buf,
            base + hbset_off::NUM_HITBOXES,
            set.boxes.len() as i32,
        );
        // `hitboxindex` 实测是**相对 hitbox set 自身**的偏移
        // （官方 `v_autoshotgun.mdl` 的 hitboxset[0].hitboxindex = 12）。
        put_i32(
            &mut buf,
            base + hbset_off::HITBOX_INDEX,
            (hb_boxes_off + hitbox_cursor * HITBOX_SIZE - base) as i32,
        );
        hitbox_cursor += set.boxes.len();
    }
    for (i, hb) in desc.hitbox_sets().flat_map(|set| &set.boxes).enumerate() {
        let base = hb_boxes_off + i * HITBOX_SIZE;
        let bone = *bone_index.get(hb.bone.as_str()).ok_or_else(|| {
            WriteError::Internal(crate::tr_fmt!(
                "the hitbox bone %{bone} did not pass validation",
                bone = format!("{:?}", hb.bone)
            ))
        })? as i32;
        put_i32(&mut buf, base + hbox_off::BONE, bone);
        put_i32(&mut buf, base + hbox_off::GROUP, hb.group.unwrap_or(0));
        put_vec3(&mut buf, base + hbox_off::BB_MIN, hb.bbmin);
        put_vec3(&mut buf, base + hbox_off::BB_MAX, hb.bbmax);
        // 名字偏移也是**相对自身**的。
        let rel = (strings_off + hb_name_offsets[i]) as i64 - base as i64;
        put_i32(
            &mut buf,
            base + hbox_off::NAME_INDEX,
            i32::try_from(rel).map_err(|_| {
                WriteError::Internal(crate::tr("the hitbox name relative offset overflows i32"))
            })?,
        );
        // unused[8] 保持 0（buf 已零初始化）。
    }

    for (i, at) in desc.attachments.iter().enumerate() {
        let base = attachment_off + i * ATTACHMENT_SIZE;
        let rel = (strings_off + at_name_offsets[i]) as i64 - base as i64;
        put_i32(
            &mut buf,
            base + at_off::NAME_INDEX,
            i32::try_from(rel).map_err(|_| {
                WriteError::Internal(crate::tr(
                    "the attachment name relative offset overflows i32",
                ))
            })?,
        );
        put_i32(&mut buf, base + at_off::FLAGS, at.flags.unwrap_or(0));
        // ⚠️ 合成附着点（`$illumposition x y z <骨骼>`）绑的骨骼**可能被收骨判据
        // 丢掉** —— 它的保活语义是 `rigid`，官方 `TagUsedBones()` 沿父链上溯找
        // 第一根有顶点权重的骨骼（`simplify.cpp:3472-3484`），上溯无果就什么都不标。
        // 该骨骼随后被丢，`LinkAttachments()` 的阶段 2（`simplify.cpp:5341-5376`）
        // 沿父链上溯到第一根**直命中全局骨骼表**的祖先，附着点落那根祖先的
        // 全局下标，`local` 被重算成
        // `inverse(source 第 0 帧 world[祖先]) ∘ source 第 0 帧 world[原始] ∘ local`。
        //
        // 这两个值由 `qc::parse` 的 `finish()` 在**骨骼表建好之后**算出来
        // （修正矩阵依赖某个 source 第 0 帧的姿态，只有解析期拿得到 ——
        // 官方用的是 `g_source[j]->boneToPose[k]`，**不是**全局骨骼表的
        // `g_bonetable[k].boneToPose`，见 `simplify.cpp:5350`），存在
        // [`crate::model::Attachment::resolved`] 里。
        //
        // 实测 `docs/_probe/oracle_illumposition8.js` / `oracle_illumposition10.js`：
        // `$illumposition 0 0 0 b2`（b2 零引用、父是 b0）⟹ 附着点 `→bone 0`；
        // `docs/_probe/diff_illumposition.js` 的 `illum_drop`（夹具父链 `i-1`，
        // b2 的父是存活的 b1）⟹ `bone=1 local=[1,0,0,0,0,1,0,0,0,0,1,20]`。
        //
        // 普通附着点不需要这条：非 rigid 的会保活自身，rigid 的会保活祖先，
        // 两者都一定在骨骼表里（`validate()` 也会挡）。
        let (bone, corr) = match &at.resolved {
            Some((gi, corr)) => (*gi, Some(*corr)),
            None => (
                match bone_index.get(at.bone.as_str()) {
                    Some(b) => *b,
                    None if at.synthetic => 0,
                    None => {
                        return Err(WriteError::Internal(crate::tr_fmt!(
                            "the attachment bone %{bone} did not pass validation",
                            bone = format!("{:?}", at.bone)
                        )));
                    }
                },
                None,
            ),
        };
        put_i32(&mut buf, base + at_off::LOCAL_BONE, bone as i32);
        // `local` 是 matrix3x4_t。
        //
        // ⚠️ **不再在这里重算** —— 走 [`crate::compile::attachment_local_matrix`]，
        // 与 `calcblend` 共用唯一实现。两份实现漂移过一次：这里的旧版本只写
        // 「`angle_matrix(rot)` + 平移列」，于是 `$attachment ... absolute`
        // 的 `local` 既没有 `AngleIMatrix( g_defaultrotation )` 的旋转，
        // 也没有 `LinkAttachments()` 的 `poseToBone` 左乘 —— 实测官方
        // 用户的 `eyes` 附着点偏差高达 **61.57**（`docs/_probe/att_abs_check.js`）。
        let m = crate::compile::attachment_local_matrix(desc, at, &ptb, bone);
        // 阶段 2 的修正矩阵放在**最外层**：官方 `:5385` 是
        // `ConcatTransforms( boneToPose, g_attachment[i].local, world )`，
        // 而 `g_attachment[i].local` 此刻已经被 `:5388` 的
        // `ConcatTransforms( poseToBone, world, g_attachment[i].local )` 覆写过，
        // 也就是修正先作用于 `local`、再参与外层的 `boneToPose`。
        let m = match &corr {
            Some(c) => crate::bone_math::concat(c, &m),
            None => m,
        };
        for (k, v) in m.iter().enumerate() {
            put_f32(&mut buf, base + at_off::LOCAL + k * 4, *v);
        }
        // unused[8] 保持 0。
    }

    // ---- 10. 动画：animdesc / 动画链 / seqdesc / seq 子表 ----
    //
    // ⚠️ 条件是 **`anim_count > 0 || seq_count > 0`**，不是 `anim_count > 0`。
    //
    // 早先只看 `anim_count` —— 对普通模型两者同进同退（没动画就没序列），
    // 所以一直没暴露。**`$declaresequence` 打破了这个巧合**：
    // 一个「只有空壳序列」的 QC 有 `numlocalseq > 0` 而 `numlocalanim == 0`，
    // 于是整个 seqdesc 数组（含 label、`STUDIO_OVERRIDE`、
    // `±9999` 包围盒）**一个字节都没写**。
    //
    // 实测官方（只有 2 条空壳）：`numlocalseq=2`、`numlocalanim=0`，
    // seqdesc 数组**照写不误**（`survivor_teenangst.mdl` 是 937 条
    // 序列 / 4 个动画的同形态极端样本）。
    if anim_count > 0 || seq_count > 0 {
        buf[anim_off..anim_off + anim.animdescs.len()].copy_from_slice(&anim.animdescs);
        buf[anim_data_off..anim_data_off + anim.anim_data.len()].copy_from_slice(&anim.anim_data);
        buf[seq_off..seq_off + anim.seqdescs.len()].copy_from_slice(&anim.seqdescs);
        // seq 子表区（events / blend / iklock / keyvalue）紧跟 seqdesc 数组。
        buf[seq_sub_off..seq_sub_off + anim.seq_subtables.len()]
            .copy_from_slice(&anim.seq_subtables);
        // 回填事件名。
        //
        // ⚠️ **基准是「事件记录自身」，不是「`szeventindex` 字段自身」。**
        //
        // 官方 `AddToStringTable`（`write.cpp:101-110`）写的是
        // `*ptr = pData - base`，而调用点是
        // `AddToStringTable( &pevent[j], &pevent[j].szeventindex, name )`
        // —— **`base` 是整条事件记录 `pevent[j]` 的地址**，
        // 不是那个 `int` 字段的地址。
        //
        // 早先这里用 `field_abs`（= 记录 + 0x4C）作基准，于是每个偏移
        // 都多了 0x4C —— 引擎按「记录 + 偏移」解析时会读到字符串池里
        // **错位 0x4C 的字节**。
        //
        // 实测判据（`docs/_probe/_verify_event_name.js`，官方产物）：
        //   官方 `AE_MUZZLEFLASH`：按「记录」基准 = `"AE_MUZZLEFLASH"` ✓
        //                          按「字段」基准 = `"FFECT"` ✗
        //   mdlc 侧则**正好相反** —— 两边基准不同，症状就是事件名错位。
        for (field_off, name) in &anim.event_name_patches {
            let str_rel = event_name_offsets
                .get(name.as_str())
                .copied()
                .ok_or_else(|| {
                    WriteError::Internal(crate::tr(
                        "the event name did not make it into the string pool",
                    ))
                })?;
            // `field_off` 指向 `szeventindex` 字段（记录 + 0x4C），
            // 所以记录起点是它减去 0x4C。
            let rec_abs = seq_sub_off + field_off - anim_writer::EVENT_NAME_FIELD_OFFSET;
            let name_abs = strings_off + str_rel;
            put_i32(
                &mut buf,
                seq_sub_off + field_off,
                i32::try_from(name_abs as i64 - rec_abs as i64).map_err(|_| {
                    WriteError::Internal(crate::tr("the event name relative offset overflows i32"))
                })?,
            );
        }

        // 每条序列的**摆好姿势**包围盒（`CalcSequenceBoundingBoxes`）。
        //
        // 早先这里给所有序列写同一个**静止姿势**的顶点 AABB —— 语义不对：
        // 有动画时摆姿势的盒子会明显不同（`ipe1` 的 `seq[1]` 就是例子：
        // 官方 `bbmax=[19.8,10,7]`，静止姿势只有 `[0,10,7]`）。
        let seq_bounds: Vec<([f32; 3], [f32; 3])> = (0..compiled.sequences.len())
            .map(|i| {
                // `$declaresequence` 的空壳**没有动画** ⟹
                // `CalcSequenceBoundingBoxes`（`simplify.cpp:7173-7201`）
                // 的两重循环一次都不跑 ⟹ `bmin/bmax` 停在**初值**
                // `[9999, 9999, 9999] / [-9999, -9999, -9999]`
                // （`simplify.cpp:7180-7181`）。
                //
                // ⚠️ 这正是那个「倒置包围盒」哨兵 —— 实测官方空壳就是它，
                // 不是 `[0,0,0]`。真实 survivor 产物 933 条全是这个值。
                if compiled.sequences[i].forward_declared {
                    return ([9999.0; 3], [-9999.0; 3]);
                }
                crate::compile::sequence_pose_bounds(desc, compiled, i, &render_bounds)
                    .unwrap_or_else(|| compiled.bounds().unwrap_or(([0.0; 3], [0.0; 3])))
            })
            .collect();

        // ---- animdesc 数组（数量 = `numlocalanim`）----
        //
        // 与 seqdesc **分开循环**：`$staticprop` 时前者是 1 而后者可能大于 1
        // （`ipe2` 是 1 vs 2，语料里还有 1 vs 5）。早先的实现用
        // `compiled.sequences` 同时驱动两者，会在静态道具上越界。
        //
        // animdesc 的 `@name` 取**该 animdesc 对应的名字**（见
        // `anim_name_offsets` 的构造）—— 普通模型是 `@seq[i]`，
        // `$staticprop` 全部取 `@seq[0]`，blend 的每格取**源动画名**。
        //
        // 这里用下标循环而不是 `iter().enumerate()`：循环体按**同一个
        // `ai`** 索引 `anim_name_offsets` / `anim_block_index` /
        // `anim_offsets` / `ikrule_offsets` / `movement_offsets` /
        // `section_frames` 等七八个并行数组，改成迭代器反而看不出
        // 「它们必须同下标」这条约束。
        #[allow(clippy::needless_range_loop)]
        for ai in 0..anim_count {
            let ao = anim_off + ai * anim_writer::ANIMDESC_SIZE;
            // `baseptr` = **负的自身绝对偏移** —— 运行时
            // `pStudiohdr() = (byte*)this + baseptr`，必须指回文件头。
            put_i32(&mut buf, ao, -(ao as i32));
            // 名字（**相对自身**的偏移）。
            let at_name_abs = strings_off + anim_name_offsets[ai];
            put_i32(
                &mut buf,
                ao + 0x04,
                anim_writer::relative_name_offset(at_name_abs, ao)
                    .map_err(|e| WriteError::Internal(e.to_string()))?,
            );
            // `animblock`（`+0x34`）/ `animindex`（`+0x38`）。
            //
            // - `animblock == 0`：数据**内联**在 `.mdl`，`animindex` 是链相对
            //   **animdesc 自身** 的偏移（下面那支）。
            // - `animblock >= 1`：数据在 `.ani` 的第 `animblock` 块里，
            //   `animindex` 是**相对该块起点**的偏移（`write.cpp:1085`）——
            //   此时 `.mdl` 里**根本没有这条链**。
            //
            // 判据见 `anim_writer`：只有 `numframes >= 2` 的动画才进块
            // （实测 `ab_z1`/`zb90z` 单帧 → `animblock = 0` 内联）。
            let ext_block = anim.anim_block_index.get(ai).copied().unwrap_or(0);
            if ext_block != 0 {
                put_i32(&mut buf, ao + 0x34, ext_block);
                let off = anim
                    .anim_block_offset
                    .get(ai)
                    .copied()
                    .flatten()
                    .unwrap_or(0);
                put_i32(&mut buf, ao + 0x38, off as i32);
            } else {
                // `animindex` = 链相对 **animdesc 自身** 的偏移。
                put_i32(
                    &mut buf,
                    ao + 0x38,
                    (anim_data_off + anim.anim_offsets[ai]) as i32 - ao as i32,
                );
            }
            // `ikruleindex`（`+0x40`）= IK rule 块相对 **animdesc 自身** 的偏移。
            //
            // 官方是 `IsInt24(pData - (byte*)&panimdesc[i])`（`write.cpp:1049`），
            // 而且**只在 `numikrules != 0` 时才写**（`write.cpp:1047`）——
            // 这是「空段也要写自然偏移」那条统一约定的**唯一例外**
            // （因为这两个字段是 animdesc **内部的相对偏移**，不是段头部的
            // `*index`）。实测 13134 个 `numikrules == 0` 的 animdesc，
            // `ikruleindex` 与 `animblockikruleindex` **全是 0**，0 例外。
            //
            // `animblockikruleindex`（`+0x44`）= IK rule 块相对**该动画所在
            // `.ani` 块起点**的偏移（`write.cpp:1124`）。
            //
            // 官方 `write.cpp:1061-1062` 把 IK rule 块**直接追加在载荷之后**：
            // ```c
            // byte *pIkData   = WriteAnimationData( srcanim, pBlockData );
            // byte *pBlockEnd = WriteIkErrors( srcanim, pIkData );
            // ...
            // panimdesc[i].animblockikruleindex = IsInt24( pIkData - g_animblock[..].start );
            // ```
            // 所以它 == `animindex` + `ALIGN4(载荷长度)`。
            //
            // ⚠️ `WriteIkErrors` 的 `ALIGN4`（`write.cpp:865`）**无条件执行**，
            // 所以载荷末尾总是补齐到 4 的倍数 —— 受控实验 `abi7`（载荷 66）
            // 得到 **68**，`abi8`（无规则）的块长同样是 68。
            //
            // 与 `ikruleindex` 一样，它是 animdesc **内部的相对偏移**，
            // 所以 `numikrules == 0` 时**保持 0**（语料 13134/13134，0 例外）。
            if let Some(rel) = anim.anim_block_ikrule_offset.get(ai).copied().flatten() {
                let base = anim
                    .anim_block_offset
                    .get(ai)
                    .copied()
                    .flatten()
                    .unwrap_or(0);
                put_i32(&mut buf, ao + 0x44, (base + rel) as i32);
            }
            // ⚠️ **块形态下不写内联的 `ikruleindex`** —— 官方两条分支互斥
            // （`write.cpp:1043` 的 `if (!pBlockStart) ... else ...`）：
            // 走了块分支就**根本不会**执行写 `ikruleindex` 的那几行。
            // 受控实验 `abi1`/`abi2`/`abi4`/`abi7`：官方 `ikruleindex` 恒 **0**
            // 而 `animblockikruleindex` 非 0；语料 7495 个块内 animdesc 同样
            // `ikruleindex == 0`（`ikruleindex != 0` 只有 174 个内联形态）。
            if ext_block == 0
                && let Some(off) = anim.ikrule_offsets.get(ai).copied().flatten()
            {
                put_i32(
                    &mut buf,
                    ao + 0x40,
                    (anim_data_off + off) as i32 - ao as i32,
                );
            }
            if ext_block == 0 {
                if let Some(off) = anim.hierarchy_offsets.get(ai).copied().flatten() {
                    put_i32(&mut buf, ao + 76, (anim_data_off + off) as i32 - ao as i32);
                }
            } else if let Some(off) = anim.anim_block_hierarchy_offsets.get(ai).copied().flatten() {
                let base = anim
                    .anim_block_offset
                    .get(ai)
                    .copied()
                    .flatten()
                    .unwrap_or(0);
                put_i32(&mut buf, ao + 76, (base + off) as i32);
            }
            // `movementindex`（`+0x18`）= movement 数组相对 **animdesc 自身**
            // 的偏移（`write.cpp:1157` 的 `IsInt24(pData - (byte*)&panimdesc[i])`）。
            //
            // 与 `ikruleindex` 同类：它是 animdesc **内部的相对偏移**，
            // 所以没有 movement 时**保持 0**（不适用「空段写自然位置」那条约定）。
            // 实测语料 `nummovements != 0` 有 17 个模型 / 4353 个 animdesc，
            // 其余一律 0。
            if let Some(off) = anim.movement_offsets.get(ai).copied().flatten() {
                put_i32(
                    &mut buf,
                    ao + 0x18,
                    (anim_data_off + off) as i32 - ao as i32,
                );
            }
            // ---- sectionframes / sectionindex（分段动画）----
            //
            // `sectionframes` @ **+0x54**（直接值，`0` = 不分段）。
            // `sectionindex`  @ **+0x50**（**相对该 animdesc 自身**，
            // 指向 `mstudioanimsections_t[]`）。
            //
            // ⚠️ **不分段时 `sectionindex` 必须写 0** —— 这是全项目
            // 「空段写自然位置」那条统一约定的**唯一例外**
            // （实测 19145 个不分段 animdesc **全部** 为 0，
            // 且 `sectionframes == 0 ⟺ sectionindex == 0`）。
            //
            // 段表内容：`anim_writer` 写的是「相对 `anim_data` 起点」的
            // 临时偏移（`animblock = 0` 内联），这里统一换算成
            // 「相对 animdesc 自身」。
            if anim.section_frames[ai] > 0 && anim.num_sections[ai] > 0 {
                put_i32(&mut buf, ao + 0x54, anim.section_frames[ai]);
                if ext_block != 0 {
                    // ---- 块形态：段表仍在 `.mdl`，但条目指向块内 ----
                    //
                    // 实测（受控实验 `absec1`）：`sectionindex` **依旧相对
                    // animdesc 自身**（= 100，紧接 animdesc 之后）；
                    // 每条段条目 `(animblock, animindex)` 里
                    // `animblock` = **该动画所在块下标**、
                    // `animindex` = **相对块起点**的偏移。
                    //
                    // 段表**排在块内载荷之后**、`ALIGN16` 之前？
                    // 不 —— 实测它就在 `.mdl` 的 animdesc 之后（100 处），
                    // 与内联形态同一个位置。
                    let sec_off = anim.section_table_offsets[ai]
                        .expect("分段时应有段表偏移（块形态也写 .mdl）");
                    put_i32(
                        &mut buf,
                        ao + 0x50,
                        (anim_data_off + sec_off) as i32 - ao as i32,
                    );
                    let table_abs = anim_data_off + sec_off;
                    let offs = anim
                        .anim_block_section_offsets
                        .get(ai)
                        .and_then(|o| o.as_ref());
                    for k in 0..anim.num_sections[ai] {
                        let e = table_abs + k * 8;
                        put_i32(&mut buf, e, ext_block);
                        let v = offs.and_then(|o| o.get(k)).copied().unwrap_or(0);
                        put_i32(&mut buf, e + 4, v as i32);
                    }
                } else {
                    let sec_off = anim.section_table_offsets[ai].expect("分段时应有段表偏移");
                    put_i32(
                        &mut buf,
                        ao + 0x50,
                        (anim_data_off + sec_off) as i32 - ao as i32,
                    );
                    // 逐条把 `animindex` 从「相对 anim_data」换算成
                    // 「相对 animdesc 自身」。
                    let table_abs = anim_data_off + sec_off;
                    for k in 0..anim.num_sections[ai] {
                        let e = table_abs + k * 8;
                        let raw =
                            i32::from_le_bytes([buf[e + 4], buf[e + 5], buf[e + 6], buf[e + 7]])
                                as usize;
                        put_i32(&mut buf, e + 4, (anim_data_off + raw) as i32 - ao as i32);
                    }
                }
            } else {
                // 不分段：`sectionframes = 0` 且 `sectionindex = 0`
                // —— 全项目唯一「空段写 0」的例外。
                put_i32(&mut buf, ao + 0x54, 0);
                put_i32(&mut buf, ao + 0x50, 0);
            }
        }

        // ---- seqdesc 数组（数量 = `numlocalseq`）----
        for si in 0..compiled.sequences.len() {
            let so = seq_off + si * anim_writer::SEQDESC_SIZE;
            // `baseptr` = **负的自身绝对偏移**。
            put_i32(&mut buf, so, -(so as i32));
            // seqdesc 的 label 是**裸名字**（`write.cpp:431` 用
            // `g_sequence[i].name`），与 animdesc 的 `@name` 是池里
            // **两个不同的串** —— 不是「同一串跳过首字符」。
            //
            // 语料判据（`probe_seq_label_prefix.js`）：11170 条序列的 label
            // **没有一个**以 `@` 开头；8965 个 animdesc 名**全部**以 `@` 开头。
            let label_abs = strings_off + seq_name_offsets[si];
            put_i32(
                &mut buf,
                so + 0x04,
                anim_writer::relative_name_offset(label_abs, so)
                    .map_err(|e| WriteError::Internal(e.to_string()))?,
            );
            // `szactivitynameindex`：QC 的 `activity <名> <权重>` 的**名字**。
            //
            // ⚠️ 与 `activity`（i32，恒 -1）**不是**同一个东西 ——
            // 官方 `Option_Activity` 只存名字，编号留给游戏 DLL 在加载时填。
            // 实测官方 `v_autoshotgun.mdl`：`activityname="ACT_VM_RELOAD"`
            // 而 `activity == -1`。
            let act_abs = strings_off + activity_name_offsets[si];
            put_i32(
                &mut buf,
                so + 0x08,
                anim_writer::relative_name_offset(act_abs, so)
                    .map_err(|e| WriteError::Internal(e.to_string()))?,
            );
            // `animindexindex` 已由 `anim_writer` 算好（子表区紧跟在
            // seqdesc 数组之后，所以相对偏移可以纯计算得出）。
            // eventindex 同理 —— 两者都在 `anim_writer` 里填。
            //
            // bbmin / bbmax：**只有第 0 条序列**拿得到包围盒。
            //
            // `CalcSequenceBoundingBoxes()`（`simplify.cpp:7061`）只遍历
            // `g_panimation[i]`（`i < g_numani`），而 `MakeStaticProp()`
            // 把 `g_numani` 压成 1，所以静态道具的第 1..N 条序列**保持
            // 初值 0**。实测语料唯一的「静态道具 + 多序列」样本
            // `smalldebris_part_baked_setsexp.mdl`：`seq[0]` 有真实包围盒、
            // `seq[1..4]` 全是 `[0,0,0]`。
            //
            // 普通模型所有序列都有值，照旧全写。
            if compiled.is_static_prop() && si > 0 {
                put_vec3(&mut buf, so + 0x20, [0.0; 3]);
                put_vec3(&mut buf, so + 0x2C, [0.0; 3]);
            } else {
                let (bmin, bmax) = seq_bounds[si];
                put_vec3(&mut buf, so + 0x20, bmin);
                put_vec3(&mut buf, so + 0x2C, bmax);
            }
        }
    }

    // ---- 11. 字符串池 + $cdmaterials 偏移数组 + keyvalues ----
    buf[strings_off..strings_off + string_buf.len()].copy_from_slice(string_buf);
    // keyvalues 文本段（`keyvaluesize` **包含**结尾 NUL）。
    if !kv_bytes.is_empty() {
        buf[kv_off..kv_off + kv_bytes.len()].copy_from_slice(&kv_bytes);
    }
    // **无条件**写偏移 —— 空段也要给自然位置。
    //
    // 实测 3333/3333：`keyvalueindex == align4(skinindex + 2*numskinref
    // *numskinfamilies)`，**没有一个模型写 0**（`ipq2` 的官方产物是 2240）。
    // 早先这里被 `if !kv_bytes.is_empty()` 包着，空 keyvalues 的模型就留 0 ——
    // 又一处「空段写 0」的违反。
    put_i32(&mut buf, off::KEY_VALUE_OFFSET, kv_off as i32);
    put_i32(&mut buf, off::KEY_VALUE_SIZE, kv_bytes.len() as i32);
    for (i, rel) in cd_offsets.iter().enumerate() {
        let abs = abs(*rel)?;
        put_i32(&mut buf, cd_array_off + i * 4, abs);
    }

    // ---- 12. skin 表：`short[numskinfamilies][numskinref]` ----
    //
    // 每个 family 是一张「材质槽位 → texture 下标」的重映射表。
    // 引擎按 skin family 选纹理：`pTexture(pSkinref(family)[mesh.material])`。
    //
    // 默认（不写 `$texturegroup`）时只有 **1** 个 family，且内容是**恒等映射**
    // `[0, 1, 2, …]` —— 实测语料 3110/3110 个单 family 模型全是恒等，
    // 0 个例外。
    //
    // 这来自 `BuildTextureGroups`（`studiomdl.cpp:662-686`）：
    // ```cpp
    // for (i = 0; i < MAXSTUDIOSKINS; i++)
    //     for (j = 0; j < MAXSTUDIOSKINS; j++)
    //         g_skinref[i][j] = j;              // 先全部填恒等
    // for (i = 0; i < g_numtexturelayers[0]; i++)   // 再用 $texturegroup 覆盖
    //     for (j = 0; j < g_numtexturereps[0]; j++)
    //         g_skinref[i][g_texturegroup[0][0][j]] = g_texturegroup[0][i][j];
    // ```
    let mut skin_buf: Vec<u8> = Vec::with_capacity(skin_entries * 2);
    if desc.materials.skin_families.is_empty() {
        for i in 0..texture_count {
            skin_buf.extend_from_slice(&(i as u16).to_le_bytes());
        }
    } else {
        for fam in &desc.materials.skin_families {
            for i in 0..texture_count {
                // 显式给了就用；不足或越界的回退到恒等映射
                // （与 `g_skinref[i][j] = j` 的初值语义一致）。
                let v = fam.get(i).copied().unwrap_or(i as i32);
                let v = if v < 0 || v as usize >= texture_count {
                    i as i32
                } else {
                    v
                };
                skin_buf.extend_from_slice(&(v as i16).to_le_bytes());
            }
        }
    }
    buf[skin_off..skin_off + skin_buf.len()].copy_from_slice(&skin_buf);

    // ---- 13. `linearbone`（骨骼加速结构）----
    //
    // `studiohdr2.linearboneindex`（`+0x10`）指向它，**相对 `studiohdr2` 自身**
    // （实测 517/517 —— 而头部其它 `*index` 一律是文件绝对偏移）。
    //
    // ## 触发条件
    //
    // `linearboneindex != 0` ⟺ **`numbones >= 2`**。实测语料完美二分
    // （517 / 0 / 0 / 2816，`probe_linearbone_trigger.js`），官方 artifacts
    // 同样二分（192 个 `nb>=2` 全有、386 个 `nb==1` 全无，**0 例外**）。
    // 所以 `bones < 2` 时**写 0** —— 这是「空段写自然偏移」那条统一约定的
    // **又一处例外**（与 `ikruleindex`/`sectionindex` 同类：它们是
    // 结构体内部的相对偏移，不是段头部的 `*index`）。
    //
    // ## 内容：**逐位等于骨骼表**（这就是本段最容易做错的地方）
    //
    // 9 个子数组全部是骨骼表对应字段的**线性副本**，实测 517/517 逐位相同
    // （`probe_linearbone_content.js`）：
    //
    // | linearbone 数组 | 来源（`mstudiobone_t`） |
    // |---|---|
    // | `flags` | `+0xA0 flags` |
    // | `parent` | `+0x04 parent` |
    // | `pos` | `+0x20 pos` |
    // | `quat` | `+0x2C quat` |
    // | `rot` | `+0x3C rot` |
    // | `poseToBone` | `+0x60 poseToBone` |
    // | `posscale` | `+0x48 posscale` |
    // | `rotscale` | `+0x54 rotscale` |
    // | `qalignment` | `+0x90 qalignment`（语料 517/517 **全零**） |
    //
    // 所以这里**直接按字节拷贝**，而不是重新计算 —— 重算必然会引入
    // 浮点/顺序差异，而副本是逐位判据。
    //
    // ## 段位置
    //
    // `keyvalues → linearbone → 字符串池`（实测 517/517：`keyvalues` 非空时
    // 其末尾 `<=` 本段起点，0 例外；无 `srcbonetransform` 时**恰好等于**
    // `ALIGN4(keyvalues 末尾)`，449/449）。
    //
    // ---- 12b. `srcbonetransform`（源骨骼变换对）----
    //
    // `studiohdr2 +0x00` = `numsrcbonetransform`、`+0x04` = `srcbonetransformindex`
    // （**相对 `studiohdr2` 自身**）。段位：`keyvalues → srcbonetransform → linearbone`。
    //
    // 每条 100 字节：`sznameindex`（相对本记录自身）+ 两个 48 字节矩阵。
    //
    // # 数值来源（本轮破解，10/10 条官方记录命中）
    //
    // `studio.h:1713-1715` 的说明是：
    // > Src bone transforms are transformations that will convert .dmx or
    // > .smd-based animations into .mdl-based animations.
    // > NOTE: The operation you should apply is:
    // >       `pretransform * bone transform * posttransform`
    //
    // 实测两条**等价**的刻画（`docs/_probe/solve_sbt3.js`）：
    //
    // ```text
    // posttransform  = srcRealign[bone]                 // 就是骨骼表的 srcRealign
    // pretransform   = inv(srcRealign[parent])          // 10/10 命中
    //                  (根骨骼 = 单位阵)
    // ```
    //
    // 另一条等价形式（对**非** pre-aligned 骨骼，因为那里
    // `srcRealign = srcWorld⁻¹ ∘ destWorld`）：
    //
    // ```text
    // pretransform  = inv(destParentWorld) * srcParentWorld
    // posttransform = inv(srcOwnWorld)    * destOwnWorld
    // ```
    //
    // 两者在 `ipr2`/`ipr3`（6/6 记录逐元素命中）与 `ipq3`/`ipq5`（4/4）上都成立。
    // 这里用**直接取 `srcRealign`** 的形式 —— 它同时覆盖 pre-aligned 路径
    // （`$definebone` 的显式 `srcRealign`），而世界矩阵形式在 pre-aligned 上
    // 会差一个平移（源骨架该取 `$definebone` 姿态而非 SMD 姿态）。
    if has_srcbonetransform {
        let sbt_base = layout.srcbonetransform;
        let identity = crate::bone_math::IDENTITY;
        // `srcRealign` 来自 `RealignedBones`；没有重排时全为单位阵。
        let empty: Vec<crate::bone_math::Matrix3x4> = Vec::new();
        let src_realign: &[crate::bone_math::Matrix3x4] = compiled
            .realigned
            .as_ref()
            .map(|r| r.src_realign.as_slice())
            .unwrap_or(empty.as_slice());
        // **只有 `srcRealign` 或 `newWorld` 非单位阵的骨骼**才有记录
        // （顺序 = 骨骼下标升序）。
        let sbt_bones = compiled.srcbonetransform_bones();
        for (rec_i, &i) in sbt_bones.iter().enumerate() {
            let rec = sbt_base + rec_i * SRCOBONETRANSFORM_SIZE;
            let str_rel = bone_name_offsets.get(i).copied().ok_or_else(|| {
                WriteError::Internal(crate::tr(
                    "the bone name did not make it into the string pool",
                ))
            })?;
            let name_abs = strings_off + str_rel;
            put_i32(
                &mut buf,
                rec,
                i32::try_from(name_abs as i64 - rec as i64).map_err(|_| {
                    WriteError::Internal(crate::tr(
                        "the srcbonetransform name offset overflows i32",
                    ))
                })?,
            );
            let parent = bone_parents.get(i).copied().unwrap_or(-1);
            let parent_realign = if parent >= 0 {
                src_realign
                    .get(parent as usize)
                    .copied()
                    .unwrap_or(identity)
            } else {
                identity
            };
            let own_realign = src_realign.get(i).copied().unwrap_or(identity);
            // `pretransform = inv(srcRealign[parent])`（根骨骼 = 单位阵）。
            let pre = crate::bone_math::invert(&parent_realign);
            // `posttransform = srcRealign[bone]`。
            let post = own_realign;
            for (k, v) in pre.iter().enumerate() {
                put_f32(&mut buf, rec + 4 + k * 4, *v);
            }
            for (k, v) in post.iter().enumerate() {
                put_f32(&mut buf, rec + 4 + 48 + k * 4, *v);
            }
        }
    }
    // `numsrcbonetransform` / `srcbonetransformindex`。
    //
    // ⚠️ **`srcbonetransformindex` 是文件绝对偏移**，**不是**相对 `studiohdr2`
    // —— 与 `linearboneindex`（相对 `studiohdr2`）**不同**。
    // `studio.h:2444` 的访问器是
    // `(mstudiosrcbonetransform_t *)(((byte *)this) + pStudioHdr2()->srcbonetransformindex)`
    // —— `this` 是 **`studiohdr_t`**（文件头），不是 `studiohdr2`。
    //
    // 实测判据（官方 artifacts）：`ipr2` 的 `srcbonetransformindex = 2480`，
    // 而 2480 处的字节正是合法记录（`0b030000 0000803f …` = `sznameindex=779`、
    // `pretransform` 首元素 1.0）；若当成相对值会指到 `408+2480=2888`（越界）。
    //
    // ⚠️ **段不存在时 index 仍要写自然位置**（本项目「空段写自然偏移」铁律）——
    // 实测 `ipr1`/`ipq2`/`ipkx1` 的 `numsrcbonetransform == 0`
    // 但 `srcbonetransformindex != 0`（= `ALIGN4(keyvalues 末尾)`，
    // `ipr1` 是 2228，恰好等于同文件 `linearbone` 的相对值 + 408）。
    put_i32(&mut buf, studiohdr2_off, srcbonetransform_count as i32);
    put_i32(
        &mut buf,
        studiohdr2_off + 0x04,
        layout.srcbonetransform as i32,
    );

    if has_linearbone {
        let lb_base = layout.linearbone;
        // 头部：numbones + 9 个 `*index`（**相对本段自身**）+ unused[6]（恒 0）。
        //
        // ⚠️ 索引 = `LINEARBONE_HEADER_SIZE + coeff*n`（**要加上 64 的头部**）——
        // 实测 `flagsindex` 恒为 **64**，不是 0。漏掉这个偏移会让所有索引
        // 小 64，读出的是头部自身（判据：`idx[flags] == 64`）。
        put_i32(&mut buf, lb_base, bone_count as i32);
        for (k, (coeff, _)) in LINEARBONE_ARRAYS.iter().enumerate() {
            put_i32(
                &mut buf,
                lb_base + 4 + k * 4,
                (LINEARBONE_HEADER_SIZE + coeff * bone_count) as i32,
            );
        }
        // `unused[6]`：官方**从不写**（实测 517/517 全零），保持缓冲区初值 0。
        //
        // 各子数组：逐字节从骨骼表拷贝。
        for (coeff, elem_size) in LINEARBONE_ARRAYS {
            let dst = lb_base + LINEARBONE_HEADER_SIZE + coeff * bone_count;
            let field = match (coeff, elem_size) {
                (0, 4) => bone_off::FLAGS,
                (4, 4) => bone_off::PARENT,
                (8, 12) => bone_off::POSITION,
                (20, 16) => bone_off::QUAT,
                (36, 12) => bone_off::ROTATION,
                (48, 48) => bone_off::POSE_TO_BONE,
                (96, 12) => bone_off::POSITION_SCALE,
                (108, 12) => bone_off::ROTATION_SCALE,
                (120, 16) => bone_off::Q_ALIGNMENT,
                _ => {
                    return Err(WriteError::Internal(crate::tr_fmt!(
                        "the linearbone array (coeff=%{coeff}, elem=%{elem}) has no matching bone field",
                        coeff = coeff,
                        elem = elem_size
                    )));
                }
            };
            for i in 0..bone_count {
                let src = bone_off + i * BONE_SIZE + field;
                let d = dst + i * elem_size;
                buf.copy_within(src..src + elem_size, d);
            }
        }
    }
    // `linearboneindex` **相对 `studiohdr2` 自身**；段不存在时写 0。
    put_i32(
        &mut buf,
        studiohdr2_off + 0x10,
        if has_linearbone {
            (layout.linearbone - studiohdr2_off) as i32
        } else {
            0
        },
    );

    Ok(WriteOutcome {
        bytes: buf,
        spans,
        checksum,
        ani: ani_file,
    })
}

/// 按 model 的顶点区间，把所有顶点摊平成 VVD 顺序。
///
/// 与 [`write_mdl`] 使用**同一套编号**（都由 `spans` 驱动），
/// 否则 `vertexindex` 会与顶点数据错位。
pub fn flatten_vertices(compiled: &CompiledModelDesc, spans: &[ModelVertexSpan]) -> Vec<Vertex> {
    let mut out = Vec::new();
    let mut si = 0usize;
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            let span = &spans[si];
            debug_assert_eq!(span.start, out.len(), "顶点编号与 spans 必须一致");
            for mesh in &m.meshes {
                out.extend_from_slice(&mesh.vertices);
            }
            si += 1;
        }
    }
    out
}

// ---------- 小工具 ----------

fn put_bytes(buf: &mut [u8], off: usize, v: &[u8]) {
    buf[off..off + v.len()].copy_from_slice(v);
}

fn put_i32(buf: &mut [u8], off: usize, v: i32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_f32(buf: &mut [u8], off: usize, v: f32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_vec3(buf: &mut [u8], off: usize, v: [f32; 3]) {
    for (k, c) in v.iter().enumerate() {
        put_f32(buf, off + k * 4, *c);
    }
}

/// QC 的 `$eyeposition` / `$illumposition` 参数 → 模型坐标系。
///
/// studiomdl 对这两个命令做同一个轴变换（`studiomdl.cpp:845-882`）：
///
/// ```text
/// eyeposition[1] = 第 1 个参数;
/// eyeposition[0] = -第 0 个参数;
/// eyeposition[2] = 第 2 个参数;
/// ```
///
/// 即 `(x, y, z) -> (-y, x, z)` —— 绕 Z 轴 +90°，Z 不变。源码注释解释为
/// 「rotate points into frame of reference so g_model points down the
/// positive x axis」，同时坦承「these coords are bogus」。
///
/// 基向量受控实验（`docs/_probe/gen_illumpos_basis.js`）四个探针全部命中：
/// `(1,0,0)->(0,1,0)`、`(0,1,0)->(-1,0,0)`、`(0,0,1)->(0,0,1)`、
/// `(2,7,-3)->(-7,2,-3)`。
///
/// **注意**：缺省回退路径（`SetIlluminationPosition`）**不**走这里 ——
/// 见 [`write_mdl`] 里对 `illumposition` 的处理。
fn qc_axis_to_model(v: [f32; 3]) -> [f32; 3] {
    [-v[1], v[0], v[2]]
}

fn put_cstr(buf: &mut [u8], off: usize, len: usize, s: &str) -> Result<(), WriteError> {
    if s.len() >= len {
        return Err(WriteError::NameTooLong {
            // ⚠️ 格式规格（`X`）必须在传参时用 `format!` 做好：`tr_fmt!` 的
            // 占位符只认 `%{名}`，写成 `%{off:X}` 不会被替换（见 §103.2）。
            path: crate::tr_fmt!(
                "the inline string at offset %{off}",
                off = format!("0x{off:X}")
            ),
            len: s.len(),
            max: len - 1,
        });
    }
    let b = s.as_bytes();
    buf[off..off + b.len()].copy_from_slice(b);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::compile;

    /// 与测试 SMD 配套的描述（网格在 SMD 里）。
    const TEST_TOML: &str = r#"
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

    /// 两根骨骼、一个三角形的最小 SMD。
    const TEST_SMD: &str = r#"version 1
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

    /// 编译一份最小模型（描述 + SMD → IR）。
    ///
    /// 每次调用用**唯一**的临时目录：测试是并行跑的，共用一个目录会让
    /// 某个用例的清理删掉另一个用例正在读的 SMD。
    fn minimal() -> CompiledModelDesc {
        compile_desc(TEST_TOML, "mdlc-writer")
    }

    /// 编译一份**自定义 TOML** 的最小模型（复用 `TEST_SMD` 夹具）。
    ///
    /// 与 `minimal` 一样用**唯一**临时目录（测试并行跑）。
    fn compile_desc(toml: &str, tag: &str) -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD).unwrap();
        let desc = crate::model::ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("测试模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 两 model 夹具（第二个 model 复用同一份 SMD）。
    fn two_model(toml: &str) -> CompiledModelDesc {
        compile_desc(toml, "mdlc-writer2")
    }

    /// 多 LOD 的最小模型（**不写 `smd`** ⟹ 复用 LOD 0 的网格 + 骨骼选项）。
    ///
    /// 与 `vtx_writer` 里同名辅助的区别：这里刻意**不带独立 LOD SMD**，
    /// 因为本测试只关心「`numLODs=2` 时骨骼 flags 是否带 LOD1 位」。
    fn multi_lod() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-wmlod-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD).unwrap();
        let toml = format!("{TEST_TOML}\n[[bodyparts.models.lods]]\nswitch_point = 20.0\n");
        let desc = crate::model::ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("多 LOD 测试模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// `mstudiobone_t.bonecontroller[0..6]` 默认**全是 `-1`**，不是 0。
    ///
    /// 源码：`write.cpp:287-292` 先把整张表置 `-1`，再按
    /// `g_bonecontroller[i].bone` 填实际用到的槽。
    ///
    /// 语料判据（`docs/_probe/probe_bonecontroller_default.js`）：
    /// **3333 个模型 / 15757 根骨骼，全部 6 槽都是 `-1`**，零例外
    /// （`numbonecontrollers` 在 L4D2 语料里恒为 0）。
    ///
    /// 写 0 会让引擎以为存在一个指向下标 0 的控制器。
    #[test]
    fn bone_controller_slots_default_to_minus_one() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let rd = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let bone_off = rd(off::BONE_OFFSET) as usize;
        let nb = rd(off::BONE_COUNT) as usize;
        assert!(nb > 0);
        for i in 0..nb {
            for k in 0..6 {
                let v = rd(bone_off + i * BONE_SIZE + bone_off::BONE_CONTROLLER + k * 4);
                assert_eq!(v, -1, "bone[{i}].bonecontroller[{k}] 应为 -1");
            }
        }
    }

    /// 事件名偏移的基准是**事件记录自身**，不是 `szeventindex` 字段。
    ///
    /// 官方 `AddToStringTable`（`write.cpp:101-110`）写 `*ptr = pData - base`，
    /// 调用点是 `AddToStringTable( &pevent[j], &pevent[j].szeventindex, name )`
    /// —— `base` 是整条记录。
    ///
    /// 这条测试构造**两条以上**序列且**只有前面的**带事件：
    /// `eventindex` 的基准（seqdesc 数组末尾）与事件名基准（记录自身）
    /// 两者都只有在 `seq_count > 1` 时才会暴露。
    #[test]
    fn event_index_and_name_use_official_bases() {
        // `minimal()` 没有序列（只有 bodypart），所以先补一条。
        let toml =
            format!("{TEST_TOML}\n[[sequences]]\nname = \"idle\"\nsmd = \"myprop-ref.smd\"\n");
        let mut c = {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let d = std::env::temp_dir().join(format!("mdlc-evt-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("myprop-ref.smd"), TEST_SMD).unwrap();
            let desc = crate::model::ModelDesc::from_toml(&toml).unwrap();
            let c = compile(&desc, &d).expect("测试模型应能编译");
            std::fs::remove_dir_all(&d).ok();
            c
        };
        assert!(!c.sequences.is_empty(), "测试模型应有一条序列");
        // 造 3 条序列，只有第 0 条有事件 —— 这样两条基准都非平凡。
        let base_seq = c.sequences[0].clone();
        c.sequences = vec![base_seq.clone(), base_seq.clone(), base_seq];
        c.sequences[0].events = vec![crate::model::SequenceEvent {
            cycle: 0.5,
            event_type: 1024,
            event: 0,
            name: "AE_MUZZLEFLASH".into(),
            options: "1".into(),
        }];
        let out = write_mdl(&c).unwrap();
        let b = &out.bytes;
        let rd = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());

        let seq_off = rd(off::LOCAL_SEQ_OFFSET) as usize;
        let n_seq = rd(off::LOCAL_SEQ_COUNT) as usize;
        assert_eq!(n_seq, 3);

        let numevents = rd(seq_off + 0x18);
        let eventindex = rd(seq_off + 0x1C);
        assert_eq!(numevents, 1);
        // `eventindex` 必须指向 **seqdesc 数组之后** 的子表区 ——
        // 相对偏移至少是 `(n_seq - 0) * 212`。
        assert!(
            eventindex >= (n_seq * anim_writer::SEQDESC_SIZE) as i32,
            "eventindex={eventindex} 应 >= {}（指向 seqdesc 数组之后）",
            n_seq * anim_writer::SEQDESC_SIZE
        );

        let ev = seq_off + eventindex as usize;
        let sz = rd(ev + anim_writer::EVENT_NAME_FIELD_OFFSET);
        // 名字偏移**相对事件记录自身**。
        let name_at = ev + sz as usize;
        let end = b[name_at..].iter().position(|&x| x == 0).unwrap() + name_at;
        assert_eq!(
            std::str::from_utf8(&b[name_at..end]).unwrap(),
            "AE_MUZZLEFLASH",
            "事件名偏移的基准应是事件记录自身"
        );
    }

    #[test]
    fn struct_sizes_match_verified_layout() {
        assert_eq!(HDR_PART1_SIZE, 408);
        assert_eq!(BONE_SIZE, 216);
        assert_eq!(TEXTURE_SIZE, 64);
        assert_eq!(BODY_PART_SIZE, 16);
        assert_eq!(MODEL_SIZE, 148);
        assert_eq!(MESH_SIZE, 116);
    }

    #[test]
    fn offsets_are_stable() {
        assert_eq!(off::ID, 0x00);
        assert_eq!(off::CHECKSUM, 0x08);
        assert_eq!(off::NAME, 0x0C);
        assert_eq!(off::LENGTH, 0x4C);
        assert_eq!(off::FLAGS, 0x98);
        assert_eq!(off::BONE_COUNT, 0x9C);
        assert_eq!(off::BODY_PART_COUNT, 0xE8);
        assert_eq!(off::STUDIO_HDR2_OFFSET, 0x190);
        assert_eq!(HDR_PART1_SIZE, 0x198);
    }

    #[test]
    fn writes_valid_header() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        assert_eq!(&b[0..4], b"IDST");
        assert_eq!(i32::from_le_bytes(b[4..8].try_into().unwrap()), 49);
        assert_eq!(
            i32::from_le_bytes(b[8..12].try_into().unwrap()),
            out.checksum
        );
        assert_eq!(
            i32::from_le_bytes(b[off::LENGTH..off::LENGTH + 4].try_into().unwrap()) as usize,
            b.len(),
            "头部 length 必须等于实际文件长度"
        );
    }

    /// 从写出的 MDL 里读出骨骼表起点。
    ///
    /// **不要**在测试里硬编码 `HDR_PART1_SIZE` —— 段布局会随新增段
    /// （`studiohdr2`、`bonetablename`…）而变化。从头部字段读才是稳的。
    fn bone_off_of(b: &[u8]) -> usize {
        i32::from_le_bytes(
            b[off::BONE_OFFSET..off::BONE_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize
    }

    #[test]
    fn section_offsets_are_consistent() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        // studiohdr2 必须紧跟头部第一部分（Crowbar 硬编码 0x198）。
        assert_eq!(g(off::STUDIO_HDR2_OFFSET), HDR_PART1_SIZE);
        // 骨骼表紧随 studiohdr2。
        assert_eq!(g(off::BONE_OFFSET), HDR_PART1_SIZE + STUDIOHDR2_SIZE);
        // bonetablename 在 `attachment` 与 `hitboxset` **之后**。
        //
        // 权威顺序（3333 个真实模型实测）：
        //   studiohdr2 → bone → bonecontroller → attachment → hitboxset
        //   → bonetablename → …
        //
        // `minimal()` 没写 `$hbox`，但 `compile()` 会**自动生成**一个
        // `default` set（`SetupHitBoxes`），所以这里确实有 1 个 hitboxset。
        // `off::HITBOX_SET_OFFSET` 是**头部里存放「hitbox set 起点」的字段**，
        // 不是 set 本身 —— 要先读它拿到 set 的绝对位置，再读 `numhitboxes`。
        let hb_sets = g(off::HITBOX_SET_COUNT);
        let set_at = g(off::HITBOX_SET_OFFSET);
        let hb_boxes = i32::from_le_bytes(
            b[set_at + hbset_off::NUM_HITBOXES..set_at + hbset_off::NUM_HITBOXES + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_eq!(hb_sets, 1, "自动生成的 hitbox set 应有 1 个");
        // 段顺序：bone 数组 → attachment 数组 → hitboxset（+ 其 box 数组）
        //          → bonetablename
        let bone_bytes = g(off::BONE_COUNT) * BONE_SIZE;
        let at_bytes = g(off::LOCAL_ATTACHMENT_COUNT) * ATTACHMENT_SIZE;
        assert_eq!(
            g(off::BONE_TABLE_NAME_OFFSET),
            g(off::BONE_OFFSET)
                + bone_bytes
                + at_bytes
                + hb_sets * HITBOX_SET_SIZE
                + hb_boxes * HITBOX_SIZE,
            "bonetablename 应紧跟 hitboxset（含其 box 数组）；\
             bone_bytes={bone_bytes} at_bytes={at_bytes} hb_sets={hb_sets} hb_boxes={hb_boxes}"
        );
        // `pVertexBase` / `pIndexBase` 是**工具期指针，从不落盘**，
        // 官方产物恒为 0（`studio.h:2179-2180`；13/13 官方 `.mdl` 实测）。
        //
        // ⚠️ 这里曾经断言的是 `g(off::NUM_BONE_TABLE_NAME) == 2` ——
        // 那个常量名是**误读**，2 就是骨骼数被错写进了 `pIndexBase`。
        assert_eq!(g(off::VERTEX_BASE), 0, "pVertexBase 必须为 0");
        assert_eq!(g(off::INDEX_BASE), 0, "pIndexBase 必须为 0");
        // `bonetablename` 的内容 = 按名字（大小写不敏感）升序的骨骼下标。
        // `minimal()` 只有 `root` / `tip`，名字序恰好等于下标序。
        let bt_at = g(off::BONE_TABLE_NAME_OFFSET);
        assert_eq!(
            &b[bt_at..bt_at + 2],
            &[0u8, 1u8],
            "bonetablename 应为名字序（此处 root < tip ⟹ 恒等）"
        );
        // 各段必须落在文件内且**按权威顺序递增**。
        // 注意 `bodypart` 在 `texture` **之前** —— 这与「结构体在前、
        // 材质在后」的直觉相反，但 3333 个真实模型一致如此。
        let order = [
            off::STUDIO_HDR2_OFFSET,
            off::BONE_OFFSET,
            off::BODY_PART_OFFSET,
            off::TEXTURE_OFFSET,
        ];
        for w in order.windows(2) {
            assert!(
                g(w[0]) < g(w[1]),
                "段顺序应为递增：{:#X} → {:#X}",
                w[0],
                w[1]
            );
        }
        for o in order {
            assert!(g(o) < b.len(), "段偏移 {o:#X} 越界");
        }
    }

    /// **`bonetablebyname` 必须是「按名字升序的骨骼下标」，不是恒等置换。**
    ///
    /// 这是「idle 动画骨骼位置错误」的**根因回归钉**。
    ///
    /// 官方 `write.cpp:389-399` 先写 `pBoneTable[i] = i`，再
    /// `qsort(..., BoneNameCompare)`；比较器是 `strcmpi`（大小写不敏感）。
    /// 引擎 `Studio_BoneIndexByName`（`bone_setup.cpp:5725-5750`）对这张表做
    /// **二分查找**，表没排序就会让 114/119 个骨骼名查不到
    /// （`bone_merge_cache.cpp:69-71` 的 `if ( parentBoneIndex < 0 ) continue;`）。
    ///
    /// 这里直接调 `bone_table_by_name`，用**乱序**的名字验证排序真的发生。
    #[test]
    fn bone_table_by_name_sorts_case_insensitively() {
        let mk = |names: &[&str]| -> Vec<crate::model::Bone> {
            names
                .iter()
                .map(|n| crate::model::Bone {
                    name: (*n).to_string(),
                    ..minimal().desc.bones[0].clone()
                })
                .collect()
        };

        // 乱序 + 大小写混排：小写化后应为 aaa < Bbb < ccc < Ddd。
        let bones = mk(&["ccc", "AAA", "ddd", "Bbb"]);
        let t = bone_table_by_name(&bones);
        assert_eq!(
            t,
            vec![1u8, 3, 0, 2],
            "应按小写名字升序：AAA(1) Bbb(3) ccc(0) ddd(2)"
        );

        // `strcmpi` 的大小写不敏感性：纯 byte 序会把 'B'(0x42) 排在
        // 'a'(0x61) 之前，`strcmpi` 则按 'b' vs 'a' 比较。
        let bones = mk(&["b", "A"]);
        assert_eq!(
            bone_table_by_name(&bones),
            vec![1u8, 0],
            "大小写不敏感：A(a) < b(b)；纯 byte 序会给出 [0, 1]"
        );

        // 与官方语料一致的形状：`_`(0x5F) 在小写化后落在字母之间。
        // `bone_x` vs `ValveBiped...` —— 'b' < 'v'，所以 bone_* 在前。
        let bones = mk(&["ValveBiped.Bip01", "bone_a", "j_gun"]);
        assert_eq!(
            bone_table_by_name(&bones),
            vec![1u8, 2, 0],
            "bone_a(b) < j_gun(j) < ValveBiped(v)"
        );
    }

    /// **回归钉**：产物里写出的 `bonetablename` 不能是恒等置换。
    ///
    /// 用名字**逆序**的骨骼表编译，恒等置换会「恰好正确」，
    /// 所以这里特意让名字序与拓扑序不同，再断言两者不相等。
    #[test]
    fn bonetablebyname_is_not_the_identity_permutation() {
        let bones = ["zeta", "alpha", "mid"];
        let t = bone_table_by_name(
            &bones
                .iter()
                .map(|n| crate::model::Bone {
                    name: (*n).to_string(),
                    ..minimal().desc.bones[0].clone()
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(t, vec![1u8, 2, 0], "alpha(1) < mid(2) < zeta(0)");
        assert_ne!(t, vec![0u8, 1, 2], "绝不能是恒等置换 —— 那正是修前的 bug");
    }

    /// `pVertexBase` / `pIndexBase` 落盘必须是 0（工具期指针不持久化）。
    ///
    /// 修前 `pIndexBase` 被写成了 `bone_count`（2）。
    #[test]
    fn tool_time_pointers_are_zero_on_disk() {
        let b = write_mdl(&minimal()).unwrap().bytes;
        let g = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(g(off::VERTEX_BASE), 0);
        assert_eq!(g(off::INDEX_BASE), 0);
        // 顺带钉住后面 4 个独立 byte（`constdirectionallightdot` 等）也是 0。
        assert_eq!(g(off::CONST_DIRECTIONAL_LIGHT_DOT), 0);
    }

    #[test]
    fn bone_parent_resolves_to_index() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bo = bone_off_of(b);
        let base = bo + BONE_SIZE; // 第二根骨骼
        let parent = i32::from_le_bytes(
            b[base + bone_off::PARENT..base + bone_off::PARENT + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(parent, 0, "tip 的父骨骼应是 root（下标 0）");
        // 根骨骼的 parent 必须是 -1。
        let root_parent = i32::from_le_bytes(
            b[bo + bone_off::PARENT..bo + bone_off::PARENT + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(root_parent, -1);
    }

    #[test]
    fn bone_offsets_match_verified_layout() {
        // 这些偏移由官方 v_autoshotgun.mdl 实测确认：
        // bone[0].rot @0x3C = [1.5708, 0, 0]（π/2，明显是欧拉角），
        // bone[0].sznameindex = 614136 而骨骼表在 664 → 相对值。
        assert_eq!(bone_off::NAME_INDEX, 0x00);
        assert_eq!(bone_off::PARENT, 0x04);
        assert_eq!(bone_off::POSITION, 0x20);
        assert_eq!(bone_off::QUAT, 0x2C);
        assert_eq!(bone_off::ROTATION, 0x3C);
        assert_eq!(bone_off::POSITION_SCALE, 0x48);
        assert_eq!(bone_off::ROTATION_SCALE, 0x54);
        assert_eq!(bone_off::POSE_TO_BONE, 0x60);
        assert_eq!(bone_off::Q_ALIGNMENT, 0x90);
        assert_eq!(bone_off::FLAGS, 0xA0);
        assert_eq!(bone_off::SURFACE_PROP_INDEX, 0xB0);
        // poseToBone 的 12 个 float 必须落在 qAlignment 之前。
        const _: () = assert!(bone_off::POSE_TO_BONE + 12 * 4 <= bone_off::Q_ALIGNMENT);
    }

    #[test]
    fn bone_name_index_is_relative_not_absolute() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bo = bone_off_of(b);
        let rel = i32::from_le_bytes(
            b[bo + bone_off::NAME_INDEX..bo + bone_off::NAME_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        // 相对值必须落在文件内，且 骨骼起点+相对值 处应是 "root"。
        let at = bo + rel;
        assert!(at < b.len(), "相对名字偏移指到了文件外");
        let end = b[at..].iter().position(|&x| x == 0).unwrap();
        assert_eq!(&b[at..at + end], b"root", "名字偏移必须是相对骨骼自身的");
    }

    #[test]
    fn bone_position_lands_at_0x20() {
        let mut d = minimal();
        // 参考姿态来自 SMD（tip 在 z=8）；这里改成显式值验证落盘位置。
        d.desc.bones[1].position = Some([1.0, 2.0, 3.0]);
        let out = write_mdl(&d).unwrap();
        let bo = bone_off_of(&out.bytes);
        let base = bo + BONE_SIZE; // 第二根骨骼
        let g =
            |o: usize| f32::from_le_bytes(out.bytes[base + o..base + o + 4].try_into().unwrap());
        assert_eq!(g(bone_off::POSITION), 1.0);
        assert_eq!(g(bone_off::POSITION + 4), 2.0);
        assert_eq!(g(bone_off::POSITION + 8), 3.0);
    }

    #[test]
    fn bone_scales_are_one() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bo = bone_off_of(b);
        for k in 0..3 {
            let p = f32::from_le_bytes(
                b[bo + bone_off::POSITION_SCALE + k * 4..][..4]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(p, 1.0, "positionScale 必须是 1，否则骨骼缩成 0");
            let r = f32::from_le_bytes(
                b[bo + bone_off::ROTATION_SCALE + k * 4..][..4]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(r, 1.0, "rotationScale 必须是 1");
        }
    }

    #[test]
    fn vertex_index_is_byte_offset_not_index() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bp_abs = i32::from_le_bytes(
            b[off::BODY_PART_OFFSET..off::BODY_PART_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        // body part 的 modelIndex 是**相对该 body part 自身**的偏移。
        let mrel = i32::from_le_bytes(
            b[bp_abs + bp_off::MODEL_INDEX..bp_abs + bp_off::MODEL_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let moff = bp_abs + mrel;
        let vindex = i32::from_le_bytes(
            b[moff + model_off::VERTEX_INDEX..moff + model_off::VERTEX_INDEX + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(vindex, 0, "首个 model 从顶点 0 开始");
        let num_vertices = i32::from_le_bytes(
            b[moff + model_off::NUM_VERTICES..moff + model_off::NUM_VERTICES + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(num_vertices, 3);
    }

    /// **`tangentsindex` 与 `vertexindex` 同构**：都是「相对 VVD 外部数据块的
    /// 累计字节偏移」，不是下标，也不是相对 model 的偏移。
    ///
    /// `studio.h:1327-1332`：
    /// ```cpp
    /// inline int mstudio_modelvertexdata_t::GetGlobalTangentIndex( int i ) const
    /// {
    ///   mstudiomodel_t *modelptr = (mstudiomodel_t *)((byte *)this - offsetof(mstudiomodel_t, vertexdata));
    ///   Assert( ( modelptr->tangentsindex % sizeof( Vector4D ) ) == 0 );
    ///   return ( i + ( modelptr->tangentsindex / sizeof( Vector4D ) ) );
    /// }
    /// ```
    ///
    /// 官方 `write.cpp:1666-1674` 并排维护这两个累计量：
    /// ```cpp
    /// ALIGN16( externalVertexIndex );
    /// pmodel[i].vertexindex   = (int)externalVertexIndex;
    /// externalVertexIndex   += pmodel[i].numvertices * sizeof(mstudiovertex_t); // 48
    /// ALIGN4( externalTangentsIndex );
    /// pmodel[i].tangentsindex = (int)externalTangentsIndex;
    /// externalTangentsIndex += pmodel[i].numvertices * sizeof(Vector4D);        // 16
    /// ```
    ///
    /// 实测（`v_dual_pistolA`，5 个 model）官方 `tangentsindex` =
    /// `0 / 374080 / 443248 / 817328 / 886496`，逐个等于前面各 model
    /// `numvertices × 16` 的累计。**早先 mdlc 恒写 0**，会让 `model[1..]`
    /// 全部去读 `model[0]` 的切线数据；NekoMDL 与官方一致。
    #[test]
    fn tangent_index_is_byte_offset_not_index() {
        // 两个 model 复用同一份 SMD ⟹ 第二个 model 的累计偏移必然非零。
        let toml = format!("{TEST_TOML}\n[[bodyparts.models]]\nsmd = \"myprop-ref.smd\"\n");
        let out = write_mdl(&two_model(&toml)).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);

        let bp_abs = g(off::BODY_PART_OFFSET) as usize;
        let mrel = g(bp_abs + bp_off::MODEL_INDEX) as usize;
        assert_eq!(g(bp_abs + bp_off::NUM_MODELS), 2, "夹具应有 2 个 model");

        let mut expect_vertex = 0usize;
        let mut expect_tangent = 0usize;
        for m in 0..2 {
            let moff = bp_abs + mrel + m * MODEL_SIZE;
            let nv = g(moff + model_off::NUM_VERTICES) as usize;
            assert_eq!(
                g(moff + model_off::VERTEX_INDEX) as usize,
                expect_vertex,
                "model[{m}] 的 vertexindex 应是累计 48×顶点数"
            );
            assert_eq!(
                g(moff + model_off::TANGENT_INDEX) as usize,
                expect_tangent,
                "model[{m}] 的 tangentsindex 应是累计 16×顶点数（不是 0，也不是下标）"
            );
            expect_vertex += nv * VERTEX_STRIDE;
            expect_tangent += nv * TANGENT_STRIDE;
        }
        assert!(
            expect_tangent > 0,
            "第二个 model 的 tangentsindex 必须非零，否则这个用例什么都没测"
        );
    }

    #[test]
    fn mesh_index_is_relative_to_model() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bp_abs = i32::from_le_bytes(
            b[off::BODY_PART_OFFSET..off::BODY_PART_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let mrel = i32::from_le_bytes(
            b[bp_abs + bp_off::MODEL_INDEX..bp_abs + bp_off::MODEL_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let moff = bp_abs + mrel;
        let krel = i32::from_le_bytes(
            b[moff + model_off::MESH_INDEX..moff + model_off::MESH_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        // 相对解释必须落在文件内，且该处 material 应是 0。
        let koff = moff + krel;
        assert!(koff < b.len(), "mesh 相对偏移指到了文件外");
        let material = i32::from_le_bytes(
            b[koff + mesh_off::MATERIAL..koff + mesh_off::MATERIAL + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(material, 0, "meshindex 必须是相对 model 的偏移");
    }

    /// **`mstudiomesh_t` 偏移 0x20 是 `meshid`（全局序号），不是 `numBones`。**
    ///
    /// 实测依据：5585/5585 个真实 mesh 的 `0x20` 恰好等于它在 model 内的序号
    /// （`probe_mesh_offsets.js`）。早先把它当 `numBones`、把 `0x24` 当
    /// `boneIds[8]` 是错的 —— 那会让引擎读到一个非法的 `meshid`。
    #[test]
    fn mesh_id_is_a_sequential_ordinal_not_bone_count() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp_abs = g(off::BODY_PART_OFFSET) as usize;
        let m_abs = bp_abs + g(bp_abs + bp_off::MODEL_INDEX) as usize;
        let k_abs = m_abs + g(m_abs + model_off::MESH_INDEX) as usize;
        assert_eq!(
            g(k_abs + mesh_off::MESH_ID),
            0,
            "单 mesh 时 meshid 应为 0（不是骨骼数）"
        );
        // `center` 实测恒为 0。
        for i in 0..3 {
            let v = f32::from_le_bytes(
                b[k_abs + mesh_off::CENTER + i * 4..k_abs + mesh_off::CENTER + i * 4 + 4]
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(v, 0.0, "center[{i}] 应为 0");
        }
    }

    /// 单 LOD 时 `numLODVertexes[8]` 的**八个槽位都要填** `numvertices`。
    ///
    /// 实测官方产物是 `[8,8,8,8,8,8,8,8]`；留 0 会让引擎的 LOD 切换
    /// 读到「该 mesh 在该 LOD 有 0 个顶点」。
    #[test]
    fn mesh_num_lod_vertexes_filled_for_single_lod() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bp_abs = g(off::BODY_PART_OFFSET) as usize;
        let m_abs = bp_abs + g(bp_abs + bp_off::MODEL_INDEX) as usize;
        let k_abs = m_abs + g(m_abs + model_off::MESH_INDEX) as usize;
        let nv = g(k_abs + mesh_off::NUM_VERTICES);
        assert!(nv > 0, "测试模型应有顶点");
        for n in 0..crate::vvd::MAX_NUM_LODS {
            assert_eq!(
                g(k_abs + mesh_off::NUM_LOD_VERTEXES + n * 4),
                nv,
                "单 LOD 时 numLODVertexes[{n}] 应为 numvertices"
            );
        }
    }

    #[test]
    fn bodypart_name_index_is_relative() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bp_abs = i32::from_le_bytes(
            b[off::BODY_PART_OFFSET..off::BODY_PART_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let rel = i32::from_le_bytes(
            b[bp_abs + bp_off::NAME_INDEX..bp_abs + bp_off::NAME_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let at = bp_abs + rel;
        assert!(at < b.len(), "body part 名相对偏移指到了文件外");
        let end = b[at..].iter().position(|&x| x == 0).unwrap();
        assert_eq!(&b[at..at + end], b"body");
    }

    #[test]
    fn texture_name_index_is_relative() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let tex_abs = i32::from_le_bytes(
            b[off::TEXTURE_OFFSET..off::TEXTURE_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let rel = i32::from_le_bytes(
            b[tex_abs + tex_off::NAME_INDEX..tex_abs + tex_off::NAME_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let at = tex_abs + rel;
        assert!(at < b.len(), "材质名相对偏移指到了文件外");
        let end = b[at..].iter().position(|&x| x == 0).unwrap();
        // 落盘时已剥掉 $cdmaterials 前缀（"models/test/"）→ 只剩 "myprop"。
        assert_eq!(&b[at..at + end], b"myprop");
        // flags 在 0x04，不在 0x40。
        assert_eq!(tex_off::FLAGS, 0x04);
    }

    #[test]
    fn cd_materials_entries_are_absolute_offsets() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let cd_arr = i32::from_le_bytes(
            b[off::CD_TEXTURE_OFFSET..off::CD_TEXTURE_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let entry = i32::from_le_bytes(b[cd_arr..cd_arr + 4].try_into().unwrap()) as usize;
        // entry 是**绝对**偏移，直接指向字符串。
        assert!(entry < b.len(), "cdmaterials 项越界");
        let end = b[entry..].iter().position(|&x| x == 0).unwrap();
        // 落盘时已规范化为反斜杠 + 结尾分隔符（与 studiomdl 一致）。
        assert_eq!(&b[entry..entry + end], b"models\\test\\");
    }

    #[test]
    fn normalizes_cd_material_paths() {
        assert_eq!(normalize_cd_material("models/mymod"), "models\\mymod\\");
        assert_eq!(normalize_cd_material("models\\mymod\\"), "models\\mymod\\");
        assert_eq!(normalize_cd_material(""), "");
    }

    #[test]
    fn strips_cd_prefix_from_texture_names() {
        let cd = vec!["models/mymod".to_string()];
        assert_eq!(normalize_texture_name("models/mymod/myprop", &cd), "myprop");
        assert_eq!(
            normalize_texture_name("models\\mymod\\myprop", &cd),
            "myprop"
        );
        assert_eq!(
            normalize_texture_name("models/mymod/myprop.vmt", &cd),
            "myprop"
        );
        // 不在 cd 之下的名字**原样保留**（含正斜杠）。
        //
        // ⚠️ 这里**不转反斜杠** —— 语料 3301 个 `.mdl` 实测：
        // 纹理名含 `/` 的有 **1460** 个，含 `\` 的 **0** 个。
        // 只有 `$cdmaterials`（搜索路径）才走 `Q_FixSlashes`。
        assert_eq!(normalize_texture_name("other/tex", &cd), "other/tex");
        // 匹配键才统一分隔符并取 basename。
        assert_eq!(texture_match_key("other/tex", &cd), "tex");
        assert_eq!(texture_match_key("models/mymod/myprop", &cd), "myprop");
    }

    /// QC 的 `$cdmaterials ""` 会**原样**写出一条空 cdtexture。
    ///
    /// 受控实验（`docs\_probe\smdl\cdexp{1,2,3}.qc`）：
    ///
    /// | QC | `numcdtextures` | 内容 |
    /// |---|---|---|
    /// | 只有 `$cdmaterials "models/mymod"` | 1 | `["models\mymod\"]` |
    /// | 再写一条 `$cdmaterials ""`（在后） | 2 | `["models\mymod\", ""]` |
    /// | `$cdmaterials ""` 写在**前** | 2 | **`["", "models\mymod\"]`** |
    ///
    /// ⟹ 空串就是 `Cmd_CDMaterials` 对空 token 的正常处理
    /// （`studiomdl.cpp:6505`：`strdup("")` + `numcdtextures++`），
    /// **位置也原样保留**。
    ///
    /// ⛔ **绝不能自动推断**：我一度按语料相关性（「纹理名含 `/` ⟺
    /// 有空串」，1460/0/0/1841）自动追加空串 —— 那是把相关性当因果。
    /// 真实机制是 QC 里**显式写了** `$cdmaterials ""`。
    #[test]
    fn empty_cd_material_is_explicit_not_inferred() {
        assert_eq!(normalize_cd_material(""), "");
        // 写出器**不**因为「纹理名带路径」而追加空串。
        let mut d = minimal();
        d.desc.materials.textures = vec![
            crate::model::Texture {
                name: "a/b".into(),
                flags: None,
            },
            crate::model::Texture {
                name: "c".into(),
                flags: None,
            },
        ];
        let out = write_mdl(&d).unwrap();
        let b = &out.bytes;
        let rd = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(
            rd(off::CD_TEXTURE_COUNT),
            d.desc.materials.search_paths.len() as i32,
            "cdtextures 数应等于 search_paths 数，不做任何推断"
        );

        // 显式写空串才会多一条。
        let mut d2 = minimal();
        d2.desc.materials.search_paths = vec!["models/mymod".into(), String::new()];
        let out2 = write_mdl(&d2).unwrap();
        let b2 = &out2.bytes;
        let rd2 = |o: usize| i32::from_le_bytes(b2[o..o + 4].try_into().unwrap());
        assert_eq!(rd2(off::CD_TEXTURE_COUNT), 2, "显式空串应写出 2 条");
        let cd_at = rd2(off::CD_TEXTURE_OFFSET) as usize;
        let s = |o: usize| {
            let mut e = o;
            while e < b2.len() && b2[e] != 0 {
                e += 1;
            }
            String::from_utf8_lossy(&b2[o..e]).to_string()
        };
        assert_eq!(s(rd2(cd_at + 4) as usize), "", "第 2 条应是空串");
    }

    /// **FBX 几何源 ⟹ `cdtextures` 末尾多一条空串**（L4D2 导入器的私有行为）。
    ///
    /// 受控实验（真 exe，`docs\_probe\oracle_cdtex_{texture,multi,order,scope,path}.js`）：
    ///
    /// | 用例 | 几何源 | `$cdmaterials` | `numcdtextures` |
    /// |---|---|---|---|
    /// | C0 | FBX | 无 | 1 = `[""]` |
    /// | C1 | FBX | 1 条 | 2 = `["models\mymod\", ""]` |
    /// | C3 | FBX | 2 条 | 3 = `[real, real, ""]` |
    /// | C1 | **SMD** | 1 条 | **1** |
    /// | C0 | **SMD** | 无 | 1 = 合成 `models/<outname>/` |
    ///
    /// 触发范围：`$bodygroup { studio }` ✅ / `$lod … replacemodel` ✅ /
    /// `$collisionmodel` ❌ / FBX 只作动画源 ❌。
    ///
    /// 本测试**直接改 IR 里的源路径**，不碰真 FBX 文件 —— 写出器读的是
    /// `desc.bodyparts[].models[].smd` 这个**字符串**，判据只看扩展名，
    /// 所以不需要让 `ufbx` 真去解析一个文件。这样测试就不依赖
    /// `docs/_probe/`（那棵树在 `.gitignore` 里，CI 上没有）。
    #[test]
    fn fbx_geometry_appends_an_empty_cdtexture() {
        let cd_count = |d: &CompiledModelDesc| -> i32 {
            let out = write_mdl(d).unwrap();
            let b = &out.bytes;
            i32::from_le_bytes(
                b[off::CD_TEXTURE_COUNT..off::CD_TEXTURE_COUNT + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let cd_last = |d: &CompiledModelDesc| -> String {
            let out = write_mdl(d).unwrap();
            let b = &out.bytes;
            let rd = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            let n = rd(off::CD_TEXTURE_COUNT) as usize;
            let at = rd(off::CD_TEXTURE_OFFSET) as usize;
            let o = rd(at + (n - 1) * 4) as usize;
            let mut e = o;
            while e < b.len() && b[e] != 0 {
                e += 1;
            }
            String::from_utf8_lossy(&b[o..e]).to_string()
        };

        // ① SMD 几何源（`minimal()` 就是）：恰好等于 `search_paths` 的条数。
        let smd = minimal();
        assert_eq!(
            cd_count(&smd),
            smd.desc.materials.search_paths.len() as i32,
            "SMD 几何源不应被追加空串"
        );

        // ② 把 `$model` 的源换成 FBX ⟹ +1 条，且最后一条是空串。
        let mut fbx = minimal();
        fbx.desc.bodyparts[0].models[0].smd = "box.fbx".into();
        assert_eq!(
            cd_count(&fbx),
            fbx.desc.materials.search_paths.len() as i32 + 1,
            "FBX 几何源应追加一条"
        );
        assert_eq!(cd_last(&fbx), "", "追加的应是空串，不是 FBX 的目录");

        // ③ `$lod … replacemodel` 也触发（P3），即使 `$model` 本身是 SMD。
        let lod_of = |smd: &str| crate::model::LodModel {
            reverse: false,
            removed: false,
            removed_materials: Vec::new(),
            material_replacements: Vec::new(),
            smd: Some(smd.into()),
            switch_point: Some(20.0),
            bone_tree_collapse: Vec::new(),
            replace_bone: Vec::new(),
            no_facial: false,
        };
        let mut lod = minimal();
        lod.desc.bodyparts[0].models[0].lods.push(lod_of("box.fbx"));
        assert_eq!(
            cd_count(&lod),
            lod.desc.materials.search_paths.len() as i32 + 1,
            "LOD 的 replacemodel 指向 FBX 时也应追加"
        );

        // ④ 多个 FBX 几何源仍只追加**一条**（per-model，不是 per-file；Q5/M2）。
        let mut two = minimal();
        two.desc.bodyparts[0].models[0].smd = "box.fbx".into();
        two.desc.bodyparts[0].models[0]
            .lods
            .push(lod_of("sub/box.fbx"));
        assert_eq!(
            cd_count(&two),
            two.desc.materials.search_paths.len() as i32 + 1,
            "两个 FBX 几何源也只追加一条"
        );

        // ⑤ 大小写不敏感（Windows 上 `.FBX` 与 `.fbx` 是同一个文件）。
        let mut upper = minimal();
        upper.desc.bodyparts[0].models[0].smd = "BOX.FBX".into();
        assert_eq!(
            cd_count(&upper),
            upper.desc.materials.search_paths.len() as i32 + 1,
            "扩展名判据应大小写不敏感"
        );
    }

    #[test]
    fn default_bone_flags_match_studiomdl() {
        // 实测 studiomdl 产物 bones[0].flags == 1280 == 0x500
        // （VERTEX_LOD0 | HITBOX），这是「有 hitbox + 被顶点使用」时的组合。
        assert_eq!(DEFAULT_BONE_FLAGS, 1280);
        // `minimal()` 的描述里没写 `$hbox`，所以 `compile()` 会走
        // **自动生成**路径（`SetupHitBoxes`）—— 于是骨骼仍带 HITBOX 位。
        //
        // 实测官方 `ip_official.mdl`（同样没写 `$hbox`）：
        // `bones[0].flags == 0x500`。
        //
        // 要验证「真的没有 hitbox 时不带 HITBOX 位」，得显式清掉
        // `autogenerated`（模拟 `$skipboneinbbox` 那种一个 box 都没通过的
        // 情形 —— 但注意那时**骨骼 flags 依然不带** HITBOX，因为
        // `simplify.cpp:7002` 只对**实际生成的 box** 打标）。
        let out = write_mdl(&minimal()).unwrap();
        let bo = bone_off_of(&out.bytes);
        let flags = i32::from_le_bytes(
            out.bytes[bo + bone_off::FLAGS..bo + bone_off::FLAGS + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            flags, DEFAULT_BONE_FLAGS,
            "自动生成 hitbox 后骨骼应带 HITBOX 位（0x500）"
        );

        // 再验证「确实没有 hitbox」的情形：把 boxes 与 autogenerated 都清掉。
        let mut d = minimal();
        d.desc.hitboxes = Default::default();
        let out = write_mdl(&d).unwrap();
        let bo = bone_off_of(&out.bytes);
        let flags = i32::from_le_bytes(
            out.bytes[bo + bone_off::FLAGS..bo + bone_off::FLAGS + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            flags, BONE_USED_BY_VERTEX_LOD0,
            "无 hitbox 时不应有 HITBOX 位（旧实现一律写 0x500，是错的）"
        );
    }

    /// 多 LOD 时骨骼必须带**逐档**的 `BONE_USED_BY_VERTEX_LODn` 位。
    ///
    /// oracle（`docs\_probe\smdl\lodchk.qc` = `$staticprop` +
    /// `$lod 20 { replacemodel "ipf" "ipf-lod1" }`，官方 studiomdl.exe 实测）：
    /// `numbones=1`，`flags = 0xd00`
    /// = `HITBOX(0x100) | VERTEX_LOD0(0x400) | VERTEX_LOD1(0x800)`。
    ///
    /// ⚠️ 加这个特性之前 mdlc 只写 **`0x500`**（缺 LOD1 位）——
    /// 这是个**真实的既有 bug**：引擎会认为这些骨骼「不被低细节档使用」。
    #[test]
    fn multi_lod_bones_get_per_lod_vertex_flags() {
        let out = write_mdl(&multi_lod()).unwrap();
        let bo = bone_off_of(&out.bytes);
        let flags = i32::from_le_bytes(
            out.bytes[bo + bone_off::FLAGS..bo + bone_off::FLAGS + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            flags, 0xd00,
            "官方 lodchk 实测 0xd00（HITBOX|LOD0|LOD1）；\
             缺 LOD1 位是既有 bug"
        );
        assert_eq!(
            flags & BONE_USED_BY_VERTEX_LOD0,
            BONE_USED_BY_VERTEX_LOD0,
            "LOD0 位必须在"
        );
        assert_eq!(flags & 0x800, 0x800, "LOD1 位必须在（本次修复点）");
    }

    /// 单 LOD 时**只有** LOD0 位 —— 与加这个特性之前逐字节相同。
    #[test]
    fn single_lod_bones_keep_only_lod0_bit() {
        let out = write_mdl(&minimal()).unwrap();
        let bo = bone_off_of(&out.bytes);
        let flags = i32::from_le_bytes(
            out.bytes[bo + bone_off::FLAGS..bo + bone_off::FLAGS + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            flags & 0x0003_FC00,
            BONE_USED_BY_VERTEX_LOD0,
            "单 LOD 的 VERTEX 位掩码里只应有 LOD0（0x400），实际 0x{flags:x}"
        );
    }

    #[test]
    fn bone_flags_are_computed_per_use() {
        // 给一个 hitbox + 一个附着点，验证 flags 按用途组合且沿父链传播。
        let mut d = minimal();
        d.desc.hitboxes.boxes.push(crate::model::Hitbox {
            bone: "root".into(),
            group: None,
            bbmin: [-1.0, -1.0, -1.0],
            bbmax: [1.0, 1.0, 1.0],
            name: None,
        });
        d.desc.attachments.push(crate::model::Attachment {
            name: "muzzle".into(),
            bone: "tip".into(),
            position: None,
            rotation: None,
            absolute: false,
            absolute_rotation: None,
            rigid: false,
            flags: None,
            synthetic: false,
            resolved: None,
        });
        d.desc.bones[0].bonemerge = true;
        let out = write_mdl(&d).unwrap();
        let b = &out.bytes;
        let bo = bone_off_of(b);
        let f = |i: usize| {
            i32::from_le_bytes(
                b[bo + i * BONE_SIZE + bone_off::FLAGS..][..4]
                    .try_into()
                    .unwrap(),
            )
        };
        // root：被顶点用 + hitbox + bonemerge + （tip 的附着点沿父链传上来）
        let root = f(0);
        assert_ne!(root & BONE_USED_BY_VERTEX_LOD0, 0, "root 被顶点使用");
        assert_ne!(root & BONE_USED_BY_HITBOX, 0, "root 有 hitbox");
        assert_ne!(root & BONE_USED_BY_BONE_MERGE, 0, "root 有 bonemerge");
        assert_ne!(
            root & BONE_USED_BY_ATTACHMENT,
            0,
            "tip 的附着点应沿父链传播到 root"
        );
        // tip：被顶点使用（测试 SMD 的顶点绑定到 tip）+ attachment
        // + **自动生成的 hitbox**。
        //
        // 早先这里断言 0x600，理由是「hitbox 绑的是 root」。但
        // `compile()` 现在会走**自动生成**路径（`minimal()` 没写 `$hbox`），
        // 而自动生成的 box 是**按骨骼**产的 —— `tip` 也被顶点覆盖，
        // 所以它同样拿到一个 box，于是带 `BONE_USED_BY_HITBOX`。
        //
        // 实测官方印证（`iph1`，4 根骨骼都有顶点覆盖）：
        // `BONE[0..3].flags` **全部** `0x500`。
        let tip = f(1);
        assert_eq!(
            tip,
            BONE_USED_BY_VERTEX_LOD0 | BONE_USED_BY_ATTACHMENT | BONE_USED_BY_HITBOX,
            "tip 应为 VERTEX_LOD0 | ATTACHMENT | HITBOX（0x700），实际 0x{tip:X}"
        );
    }

    /// 从写出的 MDL 里读出第 `i` 个附着点的 `local` 矩阵（12 个 f32）。
    fn attachment_local_of(b: &[u8], i: usize) -> [f32; 12] {
        let base = i32::from_le_bytes(
            b[off::LOCAL_ATTACHMENT_OFFSET..off::LOCAL_ATTACHMENT_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize
            + i * ATTACHMENT_SIZE;
        let mut m = [0.0f32; 12];
        for (k, v) in m.iter_mut().enumerate() {
            *v = f32::from_le_bytes(b[base + at_off::LOCAL + k * 4..][..4].try_into().unwrap());
        }
        m
    }

    /// 从写出的 MDL 里读出第 `i` 个附着点的 `flags`。
    fn attachment_flags_of(b: &[u8], i: usize) -> i32 {
        let base = i32::from_le_bytes(
            b[off::LOCAL_ATTACHMENT_OFFSET..off::LOCAL_ATTACHMENT_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize
            + i * ATTACHMENT_SIZE;
        i32::from_le_bytes(b[base + at_off::FLAGS..][..4].try_into().unwrap())
    }

    /// ⭐ `$attachment ... absolute` 的 `local` =
    /// `poseToBone[bone] ∘ AngleIMatrix(g_defaultrotation)`（**平移列最后写**）。
    ///
    /// 官方两步：① 解析期 `AngleIMatrix( g_defaultrotation, local )`
    /// （`studiomdl.cpp:5246`）；② `LinkAttachments()` 里
    /// `ConcatTransforms( poseToBone, world, local )`（`simplify.cpp:5388`）。
    /// 平移列在**最后**才写进 `local`（`studiomdl.cpp:5305-5307`），所以第 ② 步
    /// 左乘的是「**已带平移**的 `AngleIMatrix`」。
    ///
    /// 官方产物裁决（`docs/_probe/att_abs_check.js`，`_official_user_qc3.mdl`
    /// 的 `eyes` 附着点）：公式预测与实测最大偏差 **8.368e-7**。
    ///
    /// 夹具：`tip` 在 SMD 参考姿态里位于 `(0,0,8)`、旋转为 0，父为 `root`
    /// （原点、旋转 0）⟹ `poseToBone[tip]` = 平移 `(0,0,-8)`。
    #[test]
    fn absolute_attachment_local_is_pose_to_bone_times_angle_imatrix() {
        let toml = format!(
            "{TEST_TOML}
[[attachments]]
name = \"eyes\"
bone = \"tip\"
position = [1.0, -3.62, 61.57]
absolute = true
"
        );
        let d = compile_desc(&toml, "mdlc-writer-abs");
        let out = write_mdl(&d).unwrap();
        let m = attachment_local_of(&out.bytes, 0);

        // `AngleIMatrix( [0,0,π/2] )` 的旋转 = `[0,1,0, -1,0,0, 0,0,1]`
        // （= `AngleMatrix` 的转置），再左乘平移 `(0,0,-8)` 的 `poseToBone`
        // ⟹ 平移列 `(1.0, -3.62, 61.57 - 8.0)`。
        let want = [
            0.0, 1.0, 0.0, 1.0, //
            -1.0, 0.0, 0.0, -3.62, //
            0.0, 0.0, 1.0, 53.57,
        ];
        for (k, (got, exp)) in m.iter().zip(want.iter()).enumerate() {
            assert!(
                (got - exp).abs() < 1e-4,
                "absolute 附着点的 local[{k}] 应为 {exp}，实际 {got}（完整矩阵 {m:?}）"
            );
        }
    }

    /// 非 `absolute` 的附着点 `local` = 解析期原样（`poseToBone` 与 `boneToPose`
    /// 在官方 `LinkAttachments()` 里相互抵消，`simplify.cpp:5334/5388`）。
    ///
    /// 这条是**反向**判据：若把 `absolute` 的左乘误用到所有附着点上，
    /// `mouth` 这类普通附着点的平移列会被 `poseToBone` 平移走 —— 立刻失败。
    #[test]
    fn plain_attachment_local_ignores_pose_to_bone() {
        let toml = format!(
            "{TEST_TOML}
[[attachments]]
name = \"mouth\"
bone = \"tip\"
position = [0.8, -5.8, -0.15]
"
        );
        let d = compile_desc(&toml, "mdlc-writer-plain");
        let out = write_mdl(&d).unwrap();
        let m = attachment_local_of(&out.bytes, 0);
        // 旋转为 0 ⟹ 单位旋转；平移列原样（**不**减 8）。
        let want = [
            1.0, 0.0, 0.0, 0.8, //
            0.0, 1.0, 0.0, -5.8, //
            0.0, 0.0, 1.0, -0.15,
        ];
        for (k, (got, exp)) in m.iter().zip(want.iter()).enumerate() {
            assert!(
                (got - exp).abs() < 1e-5,
                "非 absolute 附着点的 local[{k}] 应为 {exp}，实际 {got}（完整矩阵 {m:?}）"
            );
        }
    }

    /// `absolute` **不落盘**：`mstudioattachment_t.flags` 里只有 `world_align`。
    ///
    /// 官方 `write.cpp:350` 写的是 `g_attachment[i].flags`，而 `absolute`/`rigid`
    /// 落在**另一个**字段 `g_attachment[i].type`（`studiomdl.h:277-278`）
    /// ⟹ 产物里恒为 0。真 studiomdl 裁决
    /// （`docs/_probe/oracle_attachment_flags.js`）：官方对 `absolute`/`rigid`
    /// 都写 `0x0`，只有 `world_align` 写 `0x10000`。
    #[test]
    fn attachment_flags_only_carry_world_align() {
        let toml = format!(
            "{TEST_TOML}
[[attachments]]
name = \"eyes\"
bone = \"tip\"
position = [1.0, 2.0, 3.0]
absolute = true

[[attachments]]
name = \"muzzle\"
bone = \"tip\"
position = [0.0, 0.0, 0.0]
rigid = true

[[attachments]]
name = \"flash\"
bone = \"tip\"
position = [0.0, 0.0, 0.0]
flags = 65536
"
        );
        let d = compile_desc(&toml, "mdlc-writer-atflags");
        let out = write_mdl(&d).unwrap();
        assert_eq!(attachment_flags_of(&out.bytes, 0), 0, "absolute 不落盘");
        assert_eq!(attachment_flags_of(&out.bytes, 1), 0, "rigid 不落盘");
        assert_eq!(
            attachment_flags_of(&out.bytes, 2),
            0x10000,
            "world_align 落 ATTACHMENT_FLAG_WORLD_ALIGN"
        );
    }

    /// `local` 的**旋转来源**由 `absolute_rotation` 决定，与 `absolute` 解耦。
    ///
    /// 官方用同一个 `local` 矩阵、由选项**顺序**覆盖（`studiomdl.cpp:5246` 的
    /// `AngleIMatrix` vs `:5268` 的 `AngleMatrix`）—— 两种情形的 `IS_ABSOLUTE`
    /// 都置位，只有旋转来源不同。mdlc 拆成 `absolute` + `absolute_rotation`。
    ///
    /// 这条是**反向**判据：`use_imatrix` 若恒 `true`，普通附着点的旋转会被换成
    /// `AngleIMatrix( g_defaultrotation )`（`Rz(-90°)`）；若恒 `false`，
    /// `absolute` 的旋转会变成 `AngleMatrix( rotation )`。两者都被下面钉住。
    #[test]
    fn attachment_rotation_source_depends_on_absolute_rotation() {
        let toml = format!(
            "{TEST_TOML}
[[attachments]]
name = \"plain\"
bone = \"tip\"
position = [0.0, 0.0, 0.0]
rotation = [11.0, 22.0, 33.0]

[[attachments]]
name = \"abs_uses_imatrix\"
bone = \"tip\"
position = [0.0, 0.0, 0.0]
rotation = [11.0, 22.0, 33.0]
absolute = true

[[attachments]]
name = \"abs_uses_rotate\"
bone = \"tip\"
position = [0.0, 0.0, 0.0]
rotation = [11.0, 22.0, 33.0]
absolute = true
absolute_rotation = false
"
        );
        let d = compile_desc(&toml, "mdlc-writer-rot");
        let out = write_mdl(&d).unwrap();
        let plain = attachment_local_of(&out.bytes, 0);
        let abs_im = attachment_local_of(&out.bytes, 1);
        let abs_rot = attachment_local_of(&out.bytes, 2);

        // `abs_uses_imatrix` 的旋转必须**完全忽略** `rotation`
        // ⟹ 与 `plain`（用 `rotation`）的旋转部分不同。
        assert_ne!(
            &plain[0..3],
            &abs_im[0..3],
            "`absolute` 未带 `rotate` ⟹ 旋转应来自 `AngleIMatrix( g_defaultrotation )`，\
             不该等于 `AngleMatrix( rotation )`（plain={plain:?} abs={abs_im:?}）"
        );
        // `abs_uses_rotate`（`absolute_rotation = false`）的旋转来自 `rotation`
        // ⟹ 与 `plain` 的旋转部分一致（只差 `poseToBone` 左乘的平移）。
        for k in [0usize, 1, 2, 4, 5, 6, 8, 9, 10] {
            assert!(
                (plain[k] - abs_rot[k]).abs() < 1e-5,
                "`absolute_rotation = false` ⟹ 旋转来自 `AngleMatrix( rotation )`，\
                 应与 plain 一致；local[{k}] plain={} abs_rot={}",
                plain[k],
                abs_rot[k]
            );
        }
        // `absolute` 仍要做 `poseToBone` 左乘（`tip` 在 `(0,0,8)` ⟹ 平移 z 减 8）。
        assert!(
            (abs_rot[11] - (plain[11] - 8.0)).abs() < 1e-4,
            "`absolute` 的左乘必须照做（`absolute_rotation` 只管旋转来源）：\
             plain[11]={} abs_rot[11]={}",
            plain[11],
            abs_rot[11]
        );
    }

    #[test]
    fn bone_surface_prop_inherits_header() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bo = bone_off_of(b);
        let rel = i32::from_le_bytes(
            b[bo + bone_off::SURFACE_PROP_INDEX..bo + bone_off::SURFACE_PROP_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_ne!(rel, 0, "骨骼应继承头部的 surfaceprop");
        let at = bo + rel;
        let end = b[at..].iter().position(|&x| x == 0).unwrap();
        assert_eq!(&b[at..at + end], b"metal");
    }

    #[test]
    fn mesh_model_index_is_negative_model_size() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let bp_abs = i32::from_le_bytes(
            b[off::BODY_PART_OFFSET..off::BODY_PART_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let mrel = i32::from_le_bytes(
            b[bp_abs + bp_off::MODEL_INDEX..bp_abs + bp_off::MODEL_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let moff = bp_abs + mrel;
        let krel = i32::from_le_bytes(
            b[moff + model_off::MESH_INDEX..moff + model_off::MESH_INDEX + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let koff = moff + krel;
        let mi = i32::from_le_bytes(
            b[koff + mesh_off::MODEL_INDEX..koff + mesh_off::MODEL_INDEX + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(mi, -(MODEL_SIZE as i32));
    }

    #[test]
    fn flatten_vertices_matches_spans() {
        let d = minimal();
        let out = write_mdl(&d).unwrap();
        let flat = flatten_vertices(&d, &out.spans);
        assert_eq!(flat.len(), d.total_vertices());
        assert_eq!(out.spans[0].start, 0);
        assert_eq!(out.spans[0].count, 3);
    }

    #[test]
    fn write_mdl_takes_prevalidated_input() {
        // 校验已前移到 compile()，write_mdl 不再自己校验；
        // 传一个「描述层非法」的输入应当由 compile 拦下，而不是写坏文件。
        let d = std::env::temp_dir().join(format!("mdlc-writer-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD).unwrap();
        // 描述里没有任何骨骼 → compile 必须报错。
        let bad = TEST_TOML.replace(
            "[[bones]]\nname = \"root\"\n\n[[bones]]\nname = \"tip\"\nparent = \"root\"\n",
            "",
        );
        let desc = crate::model::ModelDesc::from_toml(&bad).unwrap();
        let errs = compile(&desc, &d).unwrap_err();
        assert!(
            errs.iter().any(|e| e.at() == "bones"),
            "应在 compile 阶段报「没有骨骼」：{errs:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- skin 表 ----

    /// 默认（不写 `$texturegroup`）时：1 个 family、内容为**恒等映射**。
    ///
    /// 实测语料 **3110/3110** 个单 family 模型全是恒等，0 个例外。
    /// `numskinref` 恒等于 `numtextures`（3301/3301）。
    #[test]
    fn skin_table_defaults_to_identity() {
        let d = minimal();
        let out = write_mdl(&d).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());

        assert_eq!(
            g(off::SKIN_REFERENCE_COUNT),
            1,
            "numskinref 应等于 numtextures"
        );
        assert_eq!(g(off::SKIN_FAMILY_COUNT), 1, "默认只有 1 个 family");

        let skin_at = g(off::SKIN_OFFSET) as usize;
        let v = i16::from_le_bytes(out.bytes[skin_at..skin_at + 2].try_into().unwrap());
        assert_eq!(v, 0, "单 family 恒等映射的第 0 项应是 0");
    }

    /// **布局判据**：`$cdmaterials` 数组紧跟 texture 数组，skin 表紧跟其后。
    ///
    /// 实测 3301/3301 个真实模型满足：
    ///   `cdtextureindex == textureindex + numtextures*64`
    ///   `skinindex     == cdtextureindex + numcdtextures*4`
    ///
    /// 早先 mdlc 把 `$cdmaterials` 数组放在**文件末尾**，两个字段都对不上。
    #[test]
    fn cdtexture_array_and_skin_follow_texture() {
        let d = minimal();
        let out = write_mdl(&d).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());

        let tex_at = g(off::TEXTURE_OFFSET) as usize;
        let n_tex = g(off::TEXTURE_COUNT) as usize;
        let n_cd = g(off::CD_TEXTURE_COUNT) as usize;
        let cd_at = g(off::CD_TEXTURE_OFFSET) as usize;
        let skin_at = g(off::SKIN_OFFSET) as usize;

        assert_eq!(
            cd_at,
            tex_at + n_tex * TEXTURE_SIZE,
            "$cdmaterials 数组应紧跟 texture 数组"
        );
        assert_eq!(skin_at, cd_at + n_cd * 4, "skin 表应紧跟 $cdmaterials 数组");
    }

    /// 多 family：按 `[materials].skin_families` 逐项写出。
    ///
    /// 官方受控实验 `sking1`（`$texturegroup` 2 个 family）实测：
    /// `family[0] = [0,1,2]`、`family[1] = [2,1,0]`。
    #[test]
    fn skin_table_writes_multiple_families() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-skin-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // 3 个材质、1 根骨骼的最小 SMD。
        let smd = r#"version 1
nodes
  0 "root" -1
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
end
triangles
body_a
  0 0 0 0 0 0 1 0 0 1 0 1.000000
  0 10 0 0 0 0 1 1 0 1 0 1.000000
  0 0 10 0 0 0 1 0 1 1 0 1.000000
end
"#;
        std::fs::write(d.join("s.smd"), smd).unwrap();
        let toml = r#"
[model]
name = "models/test/skin.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [
  { name = "models/test/body_a" },
  { name = "models/test/body_b" },
  { name = "models/test/body_c" },
]
skin_families = [[0, 1, 2], [2, 1, 0]]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "s.smd"
"#;
        let desc = crate::model::ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());

        assert_eq!(g(off::SKIN_REFERENCE_COUNT), 3);
        assert_eq!(g(off::SKIN_FAMILY_COUNT), 2);
        let skin_at = g(off::SKIN_OFFSET) as usize;
        let vals: Vec<i16> = (0..6)
            .map(|i| i16::from_le_bytes(out.bytes[skin_at + i * 2..][..2].try_into().unwrap()))
            .collect();
        assert_eq!(
            vals,
            vec![0, 1, 2, 2, 1, 0],
            "应逐项写出 family[0]=[0,1,2]、family[1]=[2,1,0]"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 越界 / 负数 / 条目不足的 family 项回退到**恒等映射**。
    ///
    /// 这对应 `BuildTextureGroups` 的初值
    /// `g_skinref[i][j] = j`（`studiomdl.cpp:666`）—— 没被 `$texturegroup`
    /// 覆盖的槽位保持恒等。
    #[test]
    fn skin_family_out_of_range_falls_back_to_identity() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-skinfb-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let smd = r#"version 1
nodes
  0 "root" -1
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
end
triangles
body_a
  0 0 0 0 0 0 1 0 0 1 0 1.000000
  0 10 0 0 0 0 1 1 0 1 0 1.000000
  0 0 10 0 0 0 1 0 1 1 0 1.000000
end
"#;
        std::fs::write(d.join("s.smd"), smd).unwrap();
        // 第 2 项越界（99）、第 3 项缺失（只有 2 项）→ 都应回退到恒等。
        let toml = r#"
[model]
name = "models/test/skinfb.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [
  { name = "models/test/body_a" },
  { name = "models/test/body_b" },
  { name = "models/test/body_c" },
]
skin_families = [[0, 99]]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "s.smd"
"#;
        let desc = crate::model::ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let skin_at = g(off::SKIN_OFFSET) as usize;
        let vals: Vec<i16> = (0..3)
            .map(|i| i16::from_le_bytes(out.bytes[skin_at + i * 2..][..2].try_into().unwrap()))
            .collect();
        assert_eq!(vals, vec![0, 1, 2], "越界与缺失项都应回退到恒等");
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 姿势参数（`$poseparameter`）----

    /// 编译任意 TOML 描述（用与 [`minimal`] 相同的 `myprop-ref.smd`）。
    ///
    /// 与 `minimal` 一样用**唯一**临时目录（测试并行跑）。
    fn build_from_toml(toml: &str) -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-toml-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD).unwrap();
        let desc = crate::model::ModelDesc::from_toml(toml).expect("TOML 应能解析");
        let c = compile(&desc, &d).expect("测试模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 编译一份带姿势参数的最小模型（描述 + SMD → IR）。
    ///
    /// 与 [`minimal`] 一样用**唯一**临时目录（测试并行跑）。
    fn minimal_with_pose_params(params: &str) -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-pp-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD).unwrap();
        let toml = format!(
            r#"
[model]
name = "models/test/pp.mdl"
surface_prop = "metal"
{params}

[materials]
search_paths = ["models/test"]
textures = [{{ name = "models/test/myprop" }}]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"
"#
        );
        let desc = crate::model::ModelDesc::from_toml(&toml).expect("TOML 应能解析");
        let c = compile(&desc, &d).expect("测试模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 读一条 `mstudioposeparamdesc_t`。
    fn read_pose_param(out: &WriteOutcome, i: usize) -> (String, i32, f32, f32, f32) {
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let f = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let base = g(off::LOCAL_POSE_PARAM_OFFSET) as usize + i * POSE_PARAM_SIZE;
        let name_at = base + g(base) as usize;
        let mut end = name_at;
        while out.bytes[end] != 0 {
            end += 1;
        }
        let name = String::from_utf8(out.bytes[name_at..end].to_vec()).unwrap();
        (name, g(base + 4), f(base + 8), f(base + 12), f(base + 16))
    }

    /// 三种 `loop_mode` 形态逐字段对照官方 `ipp1.mdl`。
    ///
    /// 官方 QC（`docs/_probe/smdl/ipp1.qc`）：
    /// ```text
    /// $poseparameter "move_yaw"   -180 180 wrap
    /// $poseparameter "body_pitch"  -90  45
    /// $poseparameter "lean"          0   1 loop 0.5
    /// ```
    /// 官方产物实测（`dump_poseparam.js artifacts/ipp1.mdl`）：
    /// ```text
    /// [0] "move_yaw"   flags=1 start=-180 end=180 loop=360   ← wrap: loop = end-start
    /// [1] "body_pitch" flags=0 start=-90  end=45  loop=0     ← 无关键字
    /// [2] "lean"       flags=1 start=0    end=1   loop=0.5   ← loop <值>
    /// ```
    #[test]
    fn pose_params_match_official_ipp1() {
        let c = minimal_with_pose_params(
            r#"
[[model.pose_parameters]]
name = "move_yaw"
start = -180.0
end = 180.0
loop_mode = "wrap"

[[model.pose_parameters]]
name = "body_pitch"
start = -90.0
end = 45.0

[[model.pose_parameters]]
name = "lean"
start = 0.0
end = 1.0
loop_mode = 0.5
"#,
        );
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());

        assert_eq!(g(off::LOCAL_POSE_PARAM_COUNT), 3);
        assert_eq!(
            read_pose_param(&out, 0),
            ("move_yaw".into(), 1, -180.0, 180.0, 360.0)
        );
        assert_eq!(
            read_pose_param(&out, 1),
            ("body_pitch".into(), 0, -90.0, 45.0, 0.0)
        );
        assert_eq!(read_pose_param(&out, 2), ("lean".into(), 1, 0.0, 1.0, 0.5));
    }

    /// `flags` 与 `loop` 必须**同进同退** —— 官方语料 285 条无交叉。
    ///
    /// 语料分布（`docs/_probe/probe_poseparam.js`，86 个模型）：
    /// `flags=1` 96 条（`loop != 0`）、`flags=0` 189 条（`loop == 0`）。
    /// `loop_mode` 省略时两者都必须归零。
    #[test]
    fn pose_param_flags_and_loop_move_together() {
        let c = minimal_with_pose_params(
            r#"
[[model.pose_parameters]]
name = "a"
start = -1.0
end = 1.0

[[model.pose_parameters]]
name = "b"
start = 0.0
end = 360.0
loop_mode = "wrap"
"#,
        );
        let out = write_mdl(&c).unwrap();
        for i in 0..2 {
            let (_, flags, _, _, lp) = read_pose_param(&out, i);
            assert_eq!(
                flags == STUDIO_LOOPING,
                lp != 0.0,
                "第 {i} 条：flags 的 LOOPING 位与 loop 值必须同进同退"
            );
        }
        assert_eq!(
            read_pose_param(&out, 1).4,
            360.0,
            "wrap 的 loop = end - start"
        );
    }

    /// **官方怪癖**：`$poseparameter` 不做重名去重，每条命令都新建槽位。
    ///
    /// `studiomdl.cpp:452` 把**命令名**（`"$poseparameter"`）传给
    /// `LookupPoseParameter`，而真正的参数名要到下一行才读 —— 所以永远查不到，
    /// 于是总分配新槽位。实测 `ipp3.qc` 写两次 `$poseparameter "x"` 得到
    /// **2 条**同名记录（`numlocalposeparameters=2`）。
    ///
    /// mdlc 用 `Vec` 存，天然保留重复。这条测试把该行为**钉死**，
    /// 防止以后有人「顺手」加去重。
    #[test]
    fn pose_param_duplicate_names_are_not_deduplicated() {
        let c = minimal_with_pose_params(
            r#"
[[model.pose_parameters]]
name = "x"
start = 0.0
end = 1.0

[[model.pose_parameters]]
name = "x"
start = 5.0
end = 9.0
"#,
        );
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        assert_eq!(g(off::LOCAL_POSE_PARAM_COUNT), 2, "重名不应被合并");
        assert_eq!(read_pose_param(&out, 0).0, "x");
        assert_eq!(read_pose_param(&out, 1).0, "x");
        assert_eq!(read_pose_param(&out, 1).3, 9.0, "第二条的 end 应保留");
    }

    /// `sznameindex` 是**相对该记录自身**的偏移，不是文件绝对偏移。
    ///
    /// 与 `mstudiobone_t.sznameindex` 同属 `AddToStringTable` 语义
    /// （`write.cpp:1599`）。判据有两条：
    ///
    /// 1. 把存的值当**相对偏移**解析，能读出正确的名字；
    /// 2. 存的值**小于**记录自身的绝对地址 —— 若误写成绝对偏移，
    ///    它会是一个远大于记录地址的数（字符串池在文件尾部）。
    ///
    /// 官方 `ipp1.mdl` 实测：记录 @1480，`sznameindex=284` → 名字 @1764；
    /// 而 284 ≪ 1480，正是相对偏移的特征。
    #[test]
    fn pose_param_name_offset_is_relative_to_itself() {
        let c = minimal_with_pose_params(
            r#"
[[model.pose_parameters]]
name = "move_yaw"
start = -180.0
end = 180.0
loop_mode = "wrap"
"#,
        );
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let base = g(off::LOCAL_POSE_PARAM_OFFSET) as usize;
        let stored = g(base);
        let name_at = base + stored as usize;

        assert!(stored > 0, "sznameindex 应为正偏移");
        assert!(
            (stored as usize) < base,
            "sznameindex={stored} 应小于记录地址 {base}（相对偏移，不是绝对偏移）"
        );
        assert_eq!(
            read_pose_param(&out, 0).0,
            "move_yaw",
            "按相对偏移应能读出正确名字"
        );
        assert!(name_at > base, "解析出的名字地址应在记录之后");
    }

    /// **段顺序判据**：`ikautoplaylock` 必须在 `mouth`/`poseparam` **之前**。
    ///
    /// `write.cpp:1562-1604` 的写出次序是
    /// `ikchain → ikautoplaylock → mouth → poseparam`。
    /// 语料 48 个多段模型**全部**满足（`probe_mid_order.js`）。
    ///
    /// 早先 mdlc 按 `studio.h` 的**字段声明顺序**排成
    /// `mouth → poseparam → ikautoplaylock`，因为当时计数全是 0、
    /// 偏移恰好重合而一直没暴露。这条测试防止回归。
    #[test]
    fn ikautoplaylock_precedes_mouth_and_poseparam() {
        let counts = crate::layout::SectionCounts {
            mouths: 2,
            poseparams: 3,
            iklocks: 4,
            ..Default::default()
        };
        let l = crate::layout::SectionOffsets::compute(&counts);
        assert!(
            l.ikautoplaylock <= l.mouth,
            "ikautoplaylock @{} 应在 mouth @{} 之前",
            l.ikautoplaylock,
            l.mouth
        );
        assert!(
            l.mouth <= l.poseparam,
            "mouth @{} 应在 poseparam @{} 之前",
            l.mouth,
            l.poseparam
        );
        // 段跨度也要正确（不是全挤在一点）。
        assert_eq!(l.mouth, l.ikautoplaylock + 4 * 32, "mouth 紧跟 4 条 iklock");
        assert_eq!(l.poseparam, l.mouth + 2 * 20, "poseparam 紧跟 2 条 mouth");
        l.check_monotonic().expect("段顺序自检应通过");
    }

    // ---- IK 链 / 链接 / 自动播放锁 ----

    /// 编译一份带 IK 链的最小模型。
    ///
    /// SMD 的骨骼**沿 X 轴**排列（`pos = [10,0,0]` 等），因为 `$ikchain`
    /// 会触发 `RealignBones`；mdlc 尚未实现重对齐，用已对齐骨骼才能把
    /// IK 段本身隔离出来验收。
    fn minimal_with_ik(extra: &str) -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-ik-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // 4 根骨骼、每条骨骼一个三角形（studiomdl 会剔除未被引用的骨骼）。
        let smd = r#"version 1
nodes
0 "root" -1
1 "hip" 0
2 "knee" 1
3 "ankle" 2
end
skeleton
time 0
0 0 0 0 0 0 0
1 10 0 0 0 0 0
2 20 0 0 0 0 0
3 30 0 0 0 0 0
end
triangles
myprop
0 0 -8 0 0 0 1 0 0 1 0 1
0 0 8 0 0 0 1 1 0 1 0 1
0 10 0 8 0 0 1 0.5 1 1 0 1
myprop
1 10 -8 0 0 0 1 0 0 1 1 1
1 10 8 0 0 0 1 1 0 1 1 1
1 20 0 8 0 0 1 0.5 1 1 1 1
myprop
2 20 -8 0 0 0 1 0 0 1 2 1
2 20 8 0 0 0 1 1 0 1 2 1
2 30 0 8 0 0 1 0.5 1 1 2 1
myprop
3 30 -8 0 0 0 1 0 0 1 3 1
3 30 8 0 0 0 1 1 0 1 3 1
3 40 0 8 0 0 1 0.5 1 1 3 1
end
"#;
        std::fs::write(d.join("s.smd"), smd).unwrap();
        let toml = format!(
            r#"
[model]
name = "models/test/ik.mdl"
surface_prop = "metal"
{extra}

[materials]
search_paths = ["models/test"]
textures = [{{ name = "models/test/myprop" }}]

[[bones]]
name = "root"

[[bones]]
name = "hip"
parent = "root"

[[bones]]
name = "knee"
parent = "hip"

[[bones]]
name = "ankle"
parent = "knee"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "s.smd"
"#
        );
        let desc = crate::model::ModelDesc::from_toml(&toml).expect("TOML 应能解析");
        let c = compile(&desc, &d).expect("测试模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 读一条 `mstudioikchain_t`。
    fn read_ikchain(out: &WriteOutcome, i: usize) -> (String, i32, i32, i32) {
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let base = g(off::IK_CHAIN_OFFSET) as usize + i * IK_CHAIN_SIZE;
        let name_at = base + g(base) as usize;
        let mut end = name_at;
        while out.bytes[end] != 0 {
            end += 1;
        }
        let name = String::from_utf8(out.bytes[name_at..end].to_vec()).unwrap();
        (name, g(base + 4), g(base + 8), g(base + 12))
    }

    /// 读一条 `mstudioiklink_t`（`k` = 0/1/2）。
    fn read_iklink(out: &WriteOutcome, chain: usize, k: usize) -> (i32, [f32; 3]) {
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let f = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let n = g(off::IK_CHAIN_COUNT) as usize;
        let cbase = g(off::IK_CHAIN_OFFSET) as usize + chain * IK_CHAIN_SIZE;
        // 链接区从**全部**链数组之后开始，链间连续。
        let mut cur = g(off::IK_CHAIN_OFFSET) as usize + n * IK_CHAIN_SIZE;
        for c in 0..chain {
            let cb = g(off::IK_CHAIN_OFFSET) as usize + c * IK_CHAIN_SIZE;
            cur += g(cb + 8) as usize * IK_LINK_SIZE;
        }
        let _ = cbase;
        let lp = cur + k * IK_LINK_SIZE;
        (g(lp), [f(lp + 4), f(lp + 8), f(lp + 12)])
    }

    /// IK 链逐字段对照官方 `ipkx1.mdl`。
    ///
    /// 官方 QC（`docs/_probe/smdl/ipkx1.qc`）：
    /// `$ikchain "leg" "ankle" knee 0.5 0.5 0`
    ///
    /// 官方产物（`dump_ikchain.js artifacts/ipkx1.mdl`）：
    /// ```text
    /// chain[0] name="leg" linktype=0 numlinks=3 linkindex=16
    ///   link[0] bone=1 kneeDir=[0.5,0.5,0]   ← 祖父 hip
    ///   link[1] bone=2 kneeDir=[0,0,0]       ← 父   knee
    ///   link[2] bone=3 kneeDir=[0,0,0]       ← 末端 ankle
    /// ```
    #[test]
    fn ik_chain_matches_official_ipkx1() {
        let c = minimal_with_ik(
            r#"
[[ikchains]]
name = "leg"
bone = "ankle"
knee_dir = [0.5, 0.5, 0.0]
"#,
        );
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());

        assert_eq!(g(off::IK_CHAIN_COUNT), 1);
        assert_eq!(read_ikchain(&out, 0), ("leg".into(), 0, 3, 16));
        assert_eq!(
            read_iklink(&out, 0, 0),
            (1, [0.5, 0.5, 0.0]),
            "link[0] = 祖父 hip"
        );
        assert_eq!(read_iklink(&out, 0, 1), (2, [0.0; 3]), "link[1] = 父 knee");
        assert_eq!(
            read_iklink(&out, 0, 2),
            (3, [0.0; 3]),
            "link[2] = 末端 ankle"
        );
    }

    /// 两条链时 `linkindex` 是**相对该链自身**的偏移，链接区链间连续。
    ///
    /// 官方 `ipkx3.mdl` 实测：`chain[0].linkindex = 32`（= 2*16）、
    /// `chain[1].linkindex = 100`（= 32 + 3*28 − 16）。第二条链的链接
    /// 排在第一条之后，**不重叠**。
    #[test]
    fn ik_chain_linkindex_is_relative_and_contiguous() {
        let c = minimal_with_ik(
            r#"
[[ikchains]]
name = "leg"
bone = "ankle"
knee_dir = [0.5, 0.5, 0.0]

[[ikchains]]
name = "leg2"
bone = "knee"
"#,
        );
        let out = write_mdl(&c).unwrap();
        let (_, _, _, li0) = read_ikchain(&out, 0);
        let (_, _, _, li1) = read_ikchain(&out, 1);
        assert_eq!(li0, 32, "第一条链的 linkindex = 2*16");
        assert_eq!(
            li1,
            32 + 3 * IK_LINK_SIZE as i32 - 16,
            "第二条链的 linkindex 应跳过第一条的全部链接"
        );
        // 第二条链的三段：祖父 root(0)、父 hip(1)、末端 knee(2)。
        assert_eq!(read_iklink(&out, 1, 0).0, 0);
        assert_eq!(read_iklink(&out, 1, 1).0, 1);
        assert_eq!(read_iklink(&out, 1, 2).0, 2);
    }

    /// IK 自动播放锁：`chain` 写**链下标**（不是名字）。
    ///
    /// `LinkIKLocks`（`simplify.cpp:5650-5665`）把 `$ikautoplaylock` 的
    /// 名字解析成链下标。官方 `ipkx2.mdl` 实测：
    /// `chain=0 flPosWeight=1 flLocalQWeight=0.1 flags=0 unused[4]=0`。
    #[test]
    fn ik_autoplay_lock_resolves_chain_name_to_index() {
        let c = minimal_with_ik(
            r#"
[[ikchains]]
name = "leg"
bone = "ankle"

[[ikchains]]
name = "leg2"
bone = "knee"

[[ik_autoplay_locks]]
chain = "leg2"
pos_weight = 1.0
local_q_weight = 0.1
"#,
        );
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let f = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        assert_eq!(g(off::LOCAL_IK_AUTOPLAY_LOCK_COUNT), 1);
        let base = g(off::LOCAL_IK_AUTOPLAY_LOCK_OFFSET) as usize;
        assert_eq!(g(base), 1, "\"leg2\" 应解析成链下标 1");
        assert_eq!(f(base + 4), 1.0);
        assert_eq!(f(base + 8), 0.1);
        // `flags` 与 `unused[4]` 官方不写，必须保持 0（语料 22/22）。
        assert_eq!(g(base + 12), 0, "flags 应为 0");
        for k in 0..4 {
            assert_eq!(g(base + 16 + k * 4), 0, "unused[{k}] 应为 0");
        }
    }

    /// IK 链的三段骨骼都要打 `BONE_USED_BY_ATTACHMENT`（`0x200`）。
    ///
    /// `simplify.cpp:5619/5627/5635` 对每段都置位。漏掉它的症状是
    /// `bone[].flags` 少 `0x200`（实测 `ipk1`：mdlc 1280 对官方 1792）。
    #[test]
    fn ik_chain_bones_get_attachment_flag() {
        let c = minimal_with_ik(
            r#"
[[ikchains]]
name = "leg"
bone = "ankle"
"#,
        );
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let bone_at = g(off::BONE_OFFSET) as usize;
        for i in 0..4 {
            let flags = g(bone_at + i * BONE_SIZE + bone_off::FLAGS);
            assert_ne!(
                flags & BONE_USED_BY_ATTACHMENT,
                0,
                "bone[{i}] 应带 BONE_USED_BY_ATTACHMENT（IK 链三段之一）"
            );
        }
    }

    /// **空段也要写自然位置，不能写 0。**
    ///
    /// 这是 studiomdl 的**无条件**统一约定 —— `verify_empty_offsets.js`
    /// 逐段验证了 17 个可选段（3333 个模型），**没有一个**在空时写 0：
    ///
    /// ```text
    /// bonecontroller   空 3333 个 → 全部自然位置
    /// attachment       空 3007 个 → 全部自然位置
    /// flexdesc/flexcontroller/flexrule  空 3325 个 → 全部自然位置
    /// ikchain/mouth/poseparam/ikautoplaylock  空 3247~3322 个 → 全部自然位置
    /// includemodel/animblock  空 3212~3286 个 → 全部自然位置
    /// ```
    ///
    /// 早先 mdlc 对 `attachment`/`ikchain`/`ikautoplaylock` 在空时特判写 0
    /// —— 那是错的。（`hitboxset` 不在本用例的空段之列：mdlc 会自动生成
    /// 一个 `default` set，见 `AUTOGENERATED_HITBOX`。）
    #[test]
    fn empty_sections_still_get_natural_offsets() {
        let out = write_mdl(&minimal()).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());

        // 这个最小模型没有任何 attachment / ikchain / iklock / 控制器 / flex。
        for (name, cnt_off, off_off) in [
            (
                "attachment",
                off::LOCAL_ATTACHMENT_COUNT,
                off::LOCAL_ATTACHMENT_OFFSET,
            ),
            ("ikchain", off::IK_CHAIN_COUNT, off::IK_CHAIN_OFFSET),
            (
                "ikautoplaylock",
                off::LOCAL_IK_AUTOPLAY_LOCK_COUNT,
                off::LOCAL_IK_AUTOPLAY_LOCK_OFFSET,
            ),
            (
                "bonecontroller",
                off::BONE_CONTROLLER_COUNT,
                off::BONE_CONTROLLER_OFFSET,
            ),
            ("flexdesc", off::FLEX_DESC_COUNT, off::FLEX_DESC_OFFSET),
            ("mouth", off::MOUTH_COUNT, off::MOUTH_OFFSET),
            (
                "poseparam",
                off::LOCAL_POSE_PARAM_COUNT,
                off::LOCAL_POSE_PARAM_OFFSET,
            ),
        ] {
            assert_eq!(g(cnt_off), 0, "{name} 在本用例里应为空");
            assert_ne!(g(off_off), 0, "{name} 为空时偏移也应写自然位置，不能写 0");
        }
    }

    #[test]
    fn static_prop_sets_flag() {
        let mut d = minimal();
        d.desc.model.static_prop = true;
        let out = write_mdl(&d).unwrap();
        let flags = i32::from_le_bytes(out.bytes[off::FLAGS..off::FLAGS + 4].try_into().unwrap());
        assert_ne!(flags & FLAG_STATIC_PROP, 0);
    }

    // ---- `$staticprop` 的几何 / 骨骼 / 动画塌缩 ----
    //
    // 全部结论都有官方 studiomdl 的受控实验产物佐证
    // （`docs/_probe/smdl/{ipa2,ipe2,ipf4}.qc` → `docs/_probe/artifacts/`）。

    /// 带 `$staticprop` 的最小描述（单骨骼，与 `ipa2.qc` 同构）。
    fn static_prop_desc() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-sp-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // **单骨骼** SMD —— 与 `[[bones]]` 一致（多一根 `tip` 会被
        // 「SMD 里有骨骼不在 [[bones]] 中」拦下）。
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD_ONE_BONE).unwrap();
        let toml = r#"
[model]
name = "models/test/sp.mdl"
surface_prop = "metal"
static_prop = true

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"

[[sequences]]
name = "idle"
smd = "myprop-ref.smd"
fps = 30.0
"#;
        let desc = crate::model::ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("静态道具应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// **单骨骼**的最小 SMD，顶点与 [`TEST_SMD`] 相同。
    ///
    /// 顶点：`(-8,-8,0) (8,-8,0) (0,8,0)`，法线全 `(0,0,1)`。
    /// `Rz(90°)` 后应为 `(8,-8,0) (8,8,0) (-8,0,0)`。
    ///
    /// 顶点行格式：`<parentBone> <pos3> <nrm3> <uv2> <links> <bone> <weight>`。
    const TEST_SMD_ONE_BONE: &str = r#"version 1
nodes
  0 "root" -1
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
end
triangles
myprop
  0 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000
  0 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 0 1.000000
  0 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 0 1.000000
end
"#;

    /// **核心判据**：`$staticprop` 把几何旋转 `Rz(90°)`，即
    /// `(x, y, z) -> (-y, x, z)`。
    ///
    /// 官方实测（`ipa1` 无 / `ipa2` 有 `$staticprop`，同一份 SMD）：
    ///
    /// | 顶点 | 无 | 有 |
    /// |---|---|---|
    /// | 1 | `(10,0,3)` | `(0,10,3)` |
    /// | 2 | `(0,20,7)` | `(-20,0,7)` |
    ///
    /// 法线 `(1,0,0)` → `(0,1,0)`。
    #[test]
    fn static_prop_rotates_vertices_by_rz90() {
        let c = static_prop_desc();
        let verts: Vec<Vertex> = c
            .bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .flat_map(|m| &m.meshes)
            .flat_map(|m| m.vertices.clone())
            .collect();
        assert!(!verts.is_empty(), "应至少有 1 个顶点");

        // TEST_SMD 的三角形顶点：(-8,-8,0) (8,-8,0) (0,8,0)。
        // `Rz(90°)` 把 `(x,y,z)` 映射到 `(-y,x,z)`，所以旋转后应是
        // (8,-8,0) (8,8,0) (-8,0,0)。
        let want_pos = [[8.0, -8.0, 0.0], [8.0, 8.0, 0.0], [-8.0, 0.0, 0.0]];
        for (i, w) in want_pos.iter().enumerate() {
            assert_eq!(verts[i].pos, *w, "顶点 {i} 应被 Rz(90°) 旋转");
        }
        // 法线：TEST_SMD 全是 (0,0,1) —— Z 轴在 Rz(90°) 下不变。
        for (i, v) in verts.iter().enumerate() {
            assert_eq!(v.normal, [0.0, 0.0, 1.0], "顶点 {i} 的法线 Z 轴应不变");
        }
    }

    /// `$staticprop` 把所有顶点权重归到骨骼 0，且只留一组。
    ///
    /// 实测官方 `ipa2.vvd`：`bones=[0,0,0] boneCount=1 weight=[1,0,0]`。
    #[test]
    fn static_prop_collapses_vertex_weights_to_bone_zero() {
        let c = static_prop_desc();
        for bp in &c.bodyparts {
            for m in &bp.models {
                for mesh in &m.meshes {
                    for v in &mesh.vertices {
                        assert_eq!(
                            v.bones,
                            vec![[0.0, 1.0]],
                            "静态道具的每个顶点都应只绑骨骼 0、权重 1"
                        );
                    }
                }
            }
        }
    }

    /// `$staticprop` 把骨骼表塌缩成**单根**名为 `static_prop` 的骨骼，
    /// 且位置/旋转归零。
    ///
    /// 实测语料 **2681/2681** 个静态道具满足
    /// `numbones==1 && bone[0].name=="static_prop" && bone[0].parent==-1`；
    /// 652 个普通模型里 0 个假阳性。
    #[test]
    fn static_prop_collapses_bone_table() {
        let c = static_prop_desc();
        assert_eq!(c.desc.bones.len(), 1, "应只剩 1 根骨骼");
        let b = &c.desc.bones[0];
        assert_eq!(b.name, crate::model::STATIC_PROP_BONE);
        assert_eq!(b.parent, None, "唯一的骨骼应是根");
        assert_eq!(b.position, Some([0.0; 3]), "位置应归零");
        assert_eq!(b.rotation, Some([0.0; 3]), "旋转应归零");
    }

    /// `$staticprop` 把动画压成 **1 条 1 帧**，但**保留**全部序列。
    ///
    /// 实测官方 `ipe2`（2 条序列 + `$staticprop`）：
    /// `numlocalanim=1`、`numlocalseq=2`、`anim[0].numframes=1`。
    /// 语料里 `smalldebris_part_baked_setsexp.mdl` 更是 1 个 animdesc 对
    /// 5 个 seqdesc。
    #[test]
    fn static_prop_keeps_sequences_but_collapses_anims() {
        let c = static_prop_desc();
        assert_eq!(c.seq_count(), 1);
        assert_eq!(c.anim_count(), 1, "静态道具只有 1 条动画");

        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        assert_eq!(g(off::LOCAL_ANIM_COUNT), 1);
        assert_eq!(g(off::LOCAL_SEQ_COUNT), 1);

        // animdesc 的 numframes 必须是 1（`g_panimation[0]->numframes = 1`）。
        let anim_off = g(off::LOCAL_ANIM_OFFSET) as usize;
        let numframes = g(anim_off + 0x10);
        assert_eq!(numframes, 1, "静态道具的动画应被压成 1 帧");
    }

    /// 静态道具的动画链是 `ff 00 00 00`（`bone=255` 占位），**恰好 4 字节**。
    ///
    /// 实测语料 **2681/2681** 个静态道具：链头 4 字节是 `ff 00 00 00`，
    /// 且 `localseqindex - anim_data_off == 4`（即链区只有这 4 字节）。
    #[test]
    fn static_prop_anim_chain_is_four_byte_placeholder() {
        let c = static_prop_desc();
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let anim_off = g(off::LOCAL_ANIM_OFFSET) as usize;
        let seq_off = g(off::LOCAL_SEQ_OFFSET) as usize;
        let chain = anim_off + g(anim_off + 0x38) as usize;

        assert_eq!(
            &out.bytes[chain..chain + 4],
            &[0xFF, 0x00, 0x00, 0x00],
            "静态道具的动画链应是 bone=255 的 4 字节占位"
        );
        assert_eq!(
            seq_off - chain,
            4,
            "链区应恰好 4 字节（后面紧跟 seqdesc 数组）"
        );
    }

    /// **核心判据**：`animdesc.movementindex`（`+0x18`）是**相对该 animdesc
    /// 记录自身**的偏移，不是文件绝对偏移。
    ///
    /// 这条只能在 `write_mdl` 这一层验（`anim_writer` 还不知道 `anim_data`
    /// 落在文件的哪个位置）。判据用**两个** animdesc：如果误写成绝对偏移，
    /// 两条的 `movementindex` 会相差整整一个 animdesc 的距离（100 字节），
    /// 而正确实现下它们相差的是「记录间距 − 100」。
    ///
    /// 真值来源：官方 `mv1.mdl`（`walkframe 4` / `walkframe 9`）实测
    /// `movementindex = 116`（相对自身，绝对 1648），
    /// 且 `abs == off + movementindex` 落在 `[animdesc 末尾, localseqindex)`。
    #[test]
    fn animdesc_movementindex_is_relative_to_self() {
        let c = two_seq_movement_desc();
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let anim_off = g(off::LOCAL_ANIM_OFFSET) as usize;
        let anim_count = g(off::LOCAL_ANIM_COUNT) as usize;
        let seq_off = g(off::LOCAL_SEQ_OFFSET) as usize;
        let anim_desc_end = anim_off + anim_count * 100;
        assert_eq!(anim_count, 2, "本用例应有 2 条动画");

        for ai in 0..anim_count {
            let ao = anim_off + ai * 100;
            let nm = g(ao + 0x14);
            let mi = g(ao + 0x18);
            assert_eq!(nm, 2, "anim[{ai}] 应有 2 条 movement");
            // 相对自身 ⇒ 绝对位置 = 记录自身偏移 + 相对偏移。
            let abs = (ao as i64 + mi as i64) as usize;
            assert!(
                abs >= anim_desc_end && abs + nm as usize * 44 <= seq_off,
                "anim[{ai}]：movement 数组绝对位置 {abs} 必须落在 \
                 [animdesc 末尾 {anim_desc_end}, localseqindex {seq_off}) 内"
            );
            assert_eq!(abs % 4, 0, "anim[{ai}]：movement 数组起点必须 4 对齐");
            // 记录内容可读（endframe = 4 / 9）。
            assert_eq!(g(abs), 4, "anim[{ai}].mv[0].endframe");
            assert_eq!(g(abs + 44), 9, "anim[{ai}].mv[1].endframe");
        }

        // 若把 `movementindex` 误写成**绝对**偏移，则 mi 会等于 abs 本身，
        // 于是 `mi - (ao - anim_off)` 这类量会明显偏大。用一个直接的判别：
        // 正确实现下 mi 必须 < 文件长度，且**不等于** abs。
        let mi0 = g(anim_off + 0x18);
        let abs0 = (anim_off as i64 + mi0 as i64) as usize;
        assert_ne!(
            mi0 as usize, abs0,
            "movementindex 是**相对自身**的，不应等于绝对位置"
        );
    }

    /// 两条序列、各带 2 条 movement 的描述（走真实 `compile()` 路径）。
    fn two_seq_movement_desc() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-mv-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.smd"), TEST_SMD).unwrap();
        std::fs::write(d.join("b.smd"), TEST_SMD).unwrap();
        let toml = r#"
[model]
name = "models/test/mv.mdl"
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
smd = "a.smd"

[[sequences]]
name = "idle"
smd = "a.smd"
fps = 30.0

[[sequences.movements]]
endframe = 4
motionflags = 192
v0 = 193.88239
v1 = 24.911356
angle = 197.3216
vector = [-1.0, 2.5, -3.25]
position = [-193.88239, 0.0, 7.5]

[[sequences.movements]]
endframe = 9
motionflags = 7

[[sequences]]
name = "walk"
smd = "b.smd"
fps = 30.0

[[sequences.movements]]
endframe = 4
motionflags = 192

[[sequences.movements]]
endframe = 9
motionflags = 7
"#;
        let desc = crate::model::ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// 静态道具的 `rotscale` **不含**根骨骼的 π/2 下限。    ///
    /// `MakeStaticProp()` 把 `g_panimation[0]->rotation` 置 0
    /// （`simplify.cpp:3379`），根骨骼 Z 的 +90° 偏置随之消失。
    ///
    /// 实测语料 **2681/2681** 个静态道具的 `rotscale` 三轴都是
    /// `π/8/32767`，没有一个用 `π/2`。
    #[test]
    fn static_prop_has_no_root_z_rotation_bias() {
        let c = static_prop_desc();
        let out = write_mdl(&c).unwrap();
        let g = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let bone_off = g(off::BONE_OFFSET) as usize;
        let f = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let rot_z = f(bone_off + bone_off::ROTATION_SCALE + 2 * 4);
        let want = std::f32::consts::FRAC_PI_8 / 32767.0;
        assert!(
            (rot_z - want).abs() < 1e-12,
            "静态道具的 rotscale[2] 应是 π/8/32767（{want}），实际 {rot_z}"
        );
    }

    /// 非静态道具**必须不受**上述塌缩影响（防止把普通模型改坏）。
    #[test]
    fn non_static_prop_is_unaffected() {
        let c = minimal();
        assert_eq!(c.desc.bones.len(), 2, "普通模型保留两根骨骼");
        assert_eq!(c.desc.bones[0].name, "root");
        assert_eq!(c.anim_count(), c.seq_count());
        // 关键：顶点位置不应被旋转。
        let v0 = &c.bodyparts[0].models[0].meshes[0].vertices[0];
        assert_eq!(v0.pos, [-8.0, -8.0, 0.0], "普通模型的顶点应保持原样");
    }

    #[test]
    fn hull_uses_explicit_value_when_given() {
        let mut d = minimal();
        d.desc.model.hull_min = Some([-1.0, -2.0, -3.0]);
        d.desc.model.hull_max = Some([1.0, 2.0, 3.0]);
        let out = write_mdl(&d).unwrap();
        let g = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        assert_eq!(g(off::HULL_MIN), -1.0);
        assert_eq!(g(off::HULL_MAX + 8), 3.0);
    }

    /// `$eyeposition` / `$illumposition` 的轴变换必须是 `(x,y,z) -> (-y,x,z)`。
    ///
    /// 钉住官方 studiomdl 的实测值：输入 `$eyeposition 4 5 6` 与
    /// `$illumposition 1 2 3`，产物是 `[-5,4,6]` 与 `[-2,1,3]`
    /// （受控实验见 `docs/_probe/gen_illumpos.js` 与
    /// `gen_illumpos_basis.js`，基向量四个探针全部命中）。
    #[test]
    fn eye_and_illum_positions_use_qc_axis_swap() {
        let mut d = minimal();
        d.desc.model.eye_position = Some([4.0, 5.0, 6.0]);
        d.desc.model.illum_position = Some([1.0, 2.0, 3.0]);
        let out = write_mdl(&d).unwrap();
        let g = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        assert_eq!(
            [
                g(off::EYE_POSITION),
                g(off::EYE_POSITION + 4),
                g(off::EYE_POSITION + 8)
            ],
            [-5.0, 4.0, 6.0]
        );
        assert_eq!(
            [
                g(off::ILLUM_POSITION),
                g(off::ILLUM_POSITION + 4),
                g(off::ILLUM_POSITION + 8)
            ],
            [-2.0, 1.0, 3.0]
        );
    }

    /// 轴变换的基向量行为：`(1,0,0)->(0,1,0)`、`(0,1,0)->(-1,0,0)`、
    /// `(0,0,1)->(0,0,1)`，且对第三分量线性（`(2,7,-3)->(-7,2,-3)`）。
    #[test]
    fn qc_axis_to_model_is_rz_plus_90() {
        assert_eq!(qc_axis_to_model([1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]);
        assert_eq!(qc_axis_to_model([0.0, 1.0, 0.0]), [-1.0, 0.0, 0.0]);
        assert_eq!(qc_axis_to_model([0.0, 0.0, 1.0]), [0.0, 0.0, 1.0]);
        assert_eq!(qc_axis_to_model([2.0, 7.0, -3.0]), [-7.0, 2.0, -3.0]);
    }

    /// `$illumposition` 缺省时回退到「第 0 个 sequence 包围盒中心」，
    /// 且**不再**做轴变换（`simplify.cpp:7204` 的 `SetIlluminationPosition`
    /// 不经过 `Cmd_Illumposition`）。
    ///
    /// 语料验证：无 `.phy` 且 illum 非零的 778 个真实模型里，768 个满足
    /// `illumposition == (hull_min + hull_max) / 2`，中位数偏差 0.0000。
    #[test]
    fn illum_defaults_to_sequence_bbox_center_without_axis_swap() {
        let mut d = minimal();
        // 让几何包围盒不对称，这样「中心」与「原点」可区分
        d.desc.model.illum_position = None;
        let out = write_mdl(&d).unwrap();
        let g = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let ill = [
            g(off::ILLUM_POSITION),
            g(off::ILLUM_POSITION + 4),
            g(off::ILLUM_POSITION + 8),
        ];
        let hmin = [g(off::HULL_MIN), g(off::HULL_MIN + 4), g(off::HULL_MIN + 8)];
        let hmax = [g(off::HULL_MAX), g(off::HULL_MAX + 4), g(off::HULL_MAX + 8)];
        let has_seq = !d.sequences.is_empty();
        if has_seq {
            for k in 0..3 {
                assert!(
                    (ill[k] - (hmin[k] + hmax[k]) * 0.5).abs() < 1e-4,
                    "illum[{k}] 应等于包围盒中心：illum={ill:?} hull=[{hmin:?},{hmax:?}]"
                );
            }
        } else {
            assert_eq!(ill, [0.0, 0.0, 0.0], "无 sequence 时 illum 应为 0");
        }
    }

    #[test]
    fn surface_prop_string_is_written() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let sp = i32::from_le_bytes(
            b[off::SURFACE_PROP_OFFSET..off::SURFACE_PROP_OFFSET + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_ne!(sp, 0, "surfaceprop 应写入");
        let end = b[sp..].iter().position(|&x| x == 0).unwrap();
        assert_eq!(&b[sp..sp + end], b"metal");
    }

    #[test]
    fn model_name_longer_than_64_is_rejected() {
        let mut d = minimal();
        // 名字现在由 compile 定好（显式 name 或 SMD 文件名）。
        d.bodyparts[0].models[0].name = "x".repeat(80);
        let err = write_mdl(&d).unwrap_err();
        assert!(matches!(err, WriteError::NameTooLong { .. }), "{err:?}");
    }

    // =======================================================================
    // 格式上限（**不是** studiomdl 的人为上限）
    //
    // 设计原则见模块顶部「上限常量」一节：只保留「二进制字段装不下」的
    // 硬约束。studiomdl 的 `MAXSTUDIOVERTS = 65536`（引擎运行时顶点缓存
    // 的假定）**已移除**。
    // =======================================================================

    /// 造一个 model：`n_mesh` 个 mesh，各 `per_mesh` 个顶点。
    /// 返回 (desc, 总顶点数)。
    fn desc_with_vertices(n_mesh: usize, per_mesh: usize) -> (CompiledModelDesc, usize) {
        use crate::model::{Mesh, Texture};
        let mut d = minimal();
        let mut meshes = Vec::new();
        for k in 0..n_mesh {
            let verts: Vec<Vertex> = (0..per_mesh)
                .map(|i| Vertex {
                    pos: [(k * per_mesh + i) as f32, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    bones: vec![[0.0, 1.0]],
                })
                .collect();
            meshes.push(Mesh {
                material: k,
                vertices: verts,
                triangles: Vec::new(),
                eyeball_tag: None,
            });
        }
        d.desc.materials.textures = (0..n_mesh)
            .map(|k| Texture {
                name: format!("m{k}"),
                flags: None,
            })
            .collect();
        d.bodyparts[0].models[0].meshes = meshes;
        (d, n_mesh * per_mesh)
    }

    /// **每 model 顶点数不再是 65536** —— 官方那个值是引擎运行时顶点缓存
    /// 的假定，不是格式约束。格式上限由 `vertexindex` 的字节偏移推出。
    ///
    /// 这条测试钉住「**不再**在 65536 处拒绝」：65536 与 100000 都必须通过。
    #[test]
    fn model_vertex_limit_is_format_derived_not_studiomdl() {
        // 65536：官方会拒，但**格式装得下** ⟹ 必须通过
        let (d, n) = desc_with_vertices(1, 65536);
        assert_eq!(n, 65536);
        assert!(
            write_mdl(&d).is_ok(),
            "65536 个顶点在格式上完全合法（`numvertices` 是 int32）—— \
             不应因复刻 studiomdl 的 MAXSTUDIOVERTS 而拒绝"
        );
        // 100000：同样应当通过
        let (d, n) = desc_with_vertices(1, 100_000);
        assert_eq!(n, 100_000);
        assert!(write_mdl(&d).is_ok(), "100000 个顶点应当合法");
    }

    /// 格式上限本身仍然生效：`i32::MAX / 48` 之上必须拒绝。
    ///
    /// 用一个**假**的 model 直接调检查逻辑（真造 4473 万个顶点会 OOM），
    /// 所以这里断言常量本身与推导一致。
    #[test]
    fn model_vertex_format_limit_constant_is_derived_from_offset() {
        assert_eq!(
            MAXSTUDIOVERTS_PER_MODEL,
            (i32::MAX as usize) / VERTEX_STRIDE,
            "上限必须由 vertexindex 的字节偏移推出"
        );
        // 上限个顶点时字节偏移恰好还在 i32 内
        assert!(MAXSTUDIOVERTS_PER_MODEL * VERTEX_STRIDE <= i32::MAX as usize);
        // 再多一个就越界
        assert!((MAXSTUDIOVERTS_PER_MODEL + 1) * VERTEX_STRIDE > i32::MAX as usize);
        // 它比 studiomdl 的 65536 大**三个数量级** —— 证明没在复刻官方值。
        // 用 `const` 块让 clippy 的 `assertions_on_constants` 满意
        // （它提示「常量断言应在编译期求值」，那正是我们想要的）。
        const {
            assert!(
                MAXSTUDIOVERTS_PER_MODEL > 1_000_000,
                "格式上限应远大于 studiomdl 的 65536"
            )
        };
    }

    /// **骨骼总数没有格式上限** —— 只要被引用的下标 ≤ 127。
    ///
    /// # 这条测试来自一次真实回归
    ///
    /// 中间版本查的是「骨骼**总数** ≤ 128」（照抄官方
    /// `g_numbones >= MAXSTUDIOBONES`），结果让 `parity/linnea-export.toml`
    /// （**134 根**）编译失败 —— 而它改动前能编。
    /// 实测证明那条判据是错的：134 根里只有 95 根被顶点引用，
    /// **最大引用下标 118**（官方 122 根产物是 119）。
    ///
    /// 所以这里钉住「134 根必须通过」。
    #[test]
    fn bone_count_can_exceed_legacy_128_within_animation_limit() {
        // ⚠️ hitbox 引用 `"tip"`，换骨骼表后会先撞「hitbox 骨骼未通过校验」，
        // 所以清空 hitbox，只留骨骼数这一个自变量。
        let (mut d, _) = desc_with_vertices(1, 10);
        d.desc.bones = (0..134)
            .map(|i| crate::model::Bone {
                name: format!("b{i}"),
                parent: if i == 0 {
                    None
                } else {
                    Some(format!("b{}", i - 1))
                },
                ..minimal().desc.bones[0].clone()
            })
            .collect();
        d.desc.hitboxes.boxes.clear();
        assert!(
            write_mdl(&d).is_ok(),
            "134 根骨骼应当合法 —— 真实模型 `linnea-export` 就是 134 根，\
             而它只引用了下标 0..118（未被引用的骨骼不占 VVD 下标空间）"
        );

        // The standard animation encoding reserves index 255 as a terminator.
        let (mut d2, _) = desc_with_vertices(1, 10);
        d2.desc.bones = (0..255)
            .map(|i| crate::model::Bone {
                name: format!("b{i}"),
                parent: if i == 0 {
                    None
                } else {
                    Some(format!("b{}", i - 1))
                },
                ..minimal().desc.bones[0].clone()
            })
            .collect();
        d2.desc.hitboxes.boxes.clear();
        if let Err(e) = write_mdl(&d2) {
            panic!("255 bones rejected: {e}")
        }
    }

    #[test]
    fn anim_bone_addressing_reserves_the_255_terminator() {
        let mk = |n: usize| {
            let (mut d, _) = desc_with_vertices(1, 10);
            d.desc.bones = (0..n)
                .map(|i| crate::model::Bone {
                    name: format!("b{i}"),
                    parent: if i == 0 {
                        None
                    } else {
                        Some(format!("b{}", i - 1))
                    },
                    ..minimal().desc.bones[0].clone()
                })
                .collect();
            d.desc.hitboxes.boxes.clear();
            d
        };
        assert!(
            write_mdl(&mk(255)).is_ok(),
            "indices 0..254 are addressable"
        );
        let err = write_mdl(&mk(256)).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("256") && msg.contains("255"),
            "256 bones must report the reserved-index limit: {msg}"
        );
    }

    /// **被引用的骨骼下标**必须 ≤ 127 —— 这才是真正的格式约束。
    ///
    /// 用 `vvd::Vvd::to_bytes` 直接构造一个越界的顶点来验证：
    /// 下标 200 会被 `char` 读成 −56。
    #[test]
    fn bone_index_in_vertex_is_signed_char_boundary() {
        use crate::vvd::{Vvd, VvdHeader, VvdTangent, VvdVertex};

        let mk = |bone0: u8| Vvd {
            header: VvdHeader {
                checksum: 0,
                num_lods: 1,
                num_lod_vertexes: [1, 0, 0, 0, 0, 0, 0, 0],
                num_fixups: 0,
                // 无 fixup 的形态：`fixup_table_start == vertex_data_start == 64`，
                // 但 `tangent_data_start` 仍按公式递推 ——
                // `ALIGN16(64 + 1×48) = ALIGN16(112) = 112`。
                // （第一版这里写 64，只有在 debug 下 `debug_assert_eq!` 才会抓到；
                //  release 把 debug_assert 编掉了，所以长期没暴露。）
                fixup_table_start: 64,
                vertex_data_start: 64,
                tangent_data_start: 112,
            },
            vertices: vec![VvdVertex {
                weight: [1.0, 0.0, 0.0],
                bone: [bone0, 0, 0],
                bone_count: 1,
                position: [0.0; 3],
                normal: [0.0, 0.0, 1.0],
                tex_coord: [0.0, 0.0],
            }],
            tangents: vec![VvdTangent {
                xyz: [1.0, 0.0, 0.0],
                w: -1.0,
            }],
            fixups: Vec::new(),
        };

        // 127：合法（`char` 能表达的最大非负值）
        assert!(
            mk(MAX_BONE_INDEX_IN_VERTEX as u8).to_bytes().is_ok(),
            "下标 {MAX_BONE_INDEX_IN_VERTEX} 应当合法"
        );

        // 128：非法 —— 会被 `char` 读成 −128
        // ⚠️ 文案在 `to_bytes()` 那一刻就渲染好了（`crate::tr_fmt!`），
        // 所以语言必须在**调用**那一刻生效 —— 事后包 `with_english` 是空操作。
        let msg =
            crate::test_locale::with_english(|| format!("{}", mk(128).to_bytes().unwrap_err()));
        assert!(
            msg.contains("128") && msg.contains("signed char"),
            "应当拒绝下标 128 并说明原因，实际：{msg}"
        );

        // 200：同样非法（会被读成 −56）
        assert!(mk(200).to_bytes().is_err(), "下标 200 应当被拒绝");
    }

    /// **材质表条数**：上限 32768（skin 表是有符号 short）。
    ///
    /// 用「上限 + 1」条空材质名来触发 —— 不写真 32768 个 mesh。
    #[test]
    fn texture_limit_is_signed_short_boundary() {
        use crate::model::Texture;
        let (mut d, _) = desc_with_vertices(1, 10);
        // 恰好 32768 条：合法
        d.desc.materials.textures = (0..MAXSTUDIOTEXTURES)
            .map(|i| Texture {
                name: format!("t{i}"),
                flags: None,
            })
            .collect();
        assert!(
            write_mdl(&d).is_ok(),
            "{MAXSTUDIOTEXTURES} 条材质应当合法（下标 0..{}）",
            MAXSTUDIOTEXTURES - 1
        );
        // 32769 条：非法
        d.desc.materials.textures.push(Texture {
            name: "extra".to_string(),
            flags: None,
        });
        let err = write_mdl(&d).unwrap_err();
        match err {
            WriteError::TooManyTextures { count, max } => {
                assert_eq!(count, MAXSTUDIOTEXTURES + 1);
                assert_eq!(max, MAXSTUDIOTEXTURES);
            }
            other => panic!("应当拒绝 {} 条材质，得到 {other:?}", MAXSTUDIOTEXTURES + 1),
        }
    }

    /// **每 mesh 顶点数**：上限 65536（VTX 的 `origMeshVertID` 是 uint16）。
    ///
    /// ⚠️ 判据是 `>` 而不是 `>=`：65536 个顶点时下标是 `0..=65535`，
    /// **全部装得下**。旧版写成 `n > u16::MAX`（= `n > 65535`）
    /// ⟹ 把 65536 误拒，恰好少一个。
    ///
    /// 这条只钉常量语义（真造 65536 顶点走 VTX 太慢）：
    /// 边界值本身由 `vtx_writer` 的两处 `>` 检查保证。
    #[test]
    fn mesh_vertex_format_limit_is_uint16_inclusive() {
        assert_eq!(
            MAXSTUDIOVERTS_PER_MESH,
            u16::MAX as usize + 1,
            "上限必须是 uint16 能表达的**下标个数**（0..=65535 ⟹ 65536 个）"
        );
        // 合法的最大下标
        assert_eq!(MAXSTUDIOVERTS_PER_MESH - 1, u16::MAX as usize);
    }

    /// 检查的是**单个 model**，不是全模型总量。
    #[test]
    fn model_vertex_limit_allows_multiple_models_under_limit() {
        use crate::model::{Mesh, Texture};
        let mut d = minimal();
        // 两个 model，各 40000 顶点
        let base = d.bodyparts[0].models[0].clone();
        let mk_model = |tag: f32| {
            let verts: Vec<Vertex> = (0..40000)
                .map(|i| Vertex {
                    pos: [tag, i as f32, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    bones: vec![[0.0, 1.0]],
                })
                .collect();
            let mut m = base.clone();
            m.name = format!("m{tag}");
            m.meshes = vec![Mesh {
                material: 0,
                vertices: verts,
                triangles: Vec::new(),
                eyeball_tag: None,
            }];
            m
        };
        d.bodyparts[0].models = vec![mk_model(0.0), mk_model(100.0)];
        d.desc.materials.textures = vec![Texture {
            name: "m0".to_string(),
            flags: None,
        }];
        assert!(
            write_mdl(&d).is_ok(),
            "两个 model 各 40000 顶点应当合法（**逐 model** 查，不看总量）"
        );
    }

    // ---- linearbone（骨骼加速结构，`studiohdr2.linearboneindex`）----
    //
    // 完整规格见 `LINEARBONE_HEADER_SIZE` / `LINEARBONE_ARRAYS` 的文档。
    // 语料证据：`probe_linearbone_{trigger,layout,content,formula,order,place}.js`。

    /// `linearboneindex` —— **相对 `studiohdr2`（固定 408）**，不是文件绝对偏移。
    fn linearbone_rel(b: &[u8]) -> i32 {
        let o = 408 + 0x10;
        i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }

    /// `linearbone` 段的绝对起点；段不存在时为 `None`。
    fn linearbone_base(b: &[u8]) -> Option<usize> {
        let rel = linearbone_rel(b);
        (rel != 0).then(|| 408 + rel as usize)
    }

    /// **单骨骼**的普通（非 `$staticprop`）最小模型 —— 钉「没有 linearbone」那一侧。
    fn one_bone_desc() -> CompiledModelDesc {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-lb1-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("myprop-ref.smd"), TEST_SMD_ONE_BONE).unwrap();
        let toml = r#"
[model]
name = "models/test/onebone.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"
"#;
        let desc = crate::model::ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("单骨骼模型应能编译");
        std::fs::remove_dir_all(&d).ok();
        c
    }

    /// `srcbonetransform`：**段不存在时 `srcbonetransformindex` 仍写自然位置**。
    ///
    /// 这是本项目「空段写自然偏移」铁律的又一处应用 ——
    /// 实测官方 `ipr1`/`ipq2`/`ipkx1` 的 `numsrcbonetransform == 0`
    /// 但 `srcbonetransformindex != 0`。
    ///
    /// ⚠️ 该字段是**文件绝对偏移**（`studio.h:2444` 的
    /// `(byte*)this + pStudioHdr2()->srcbonetransformindex`，`this` 是
    /// `studiohdr_t`），**不是**相对 `studiohdr2` —— 与 `linearboneindex` 不同。
    #[test]
    fn srcbonetransform_index_is_absolute_and_never_zero() {
        let rd = |b: &[u8], o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        // 1 根骨骼、无重排 → 0 条记录。
        let one = write_mdl(&one_bone_desc()).unwrap();
        let h2 = rd(&one.bytes, off::STUDIO_HDR2_OFFSET) as usize;
        assert_eq!(
            rd(&one.bytes, h2),
            0,
            "没有重排时 numsrcbonetransform 应为 0"
        );
        let idx = rd(&one.bytes, h2 + 0x04) as usize;
        assert_ne!(idx, 0, "段不存在时 index 仍要写自然位置");
        assert!(idx < one.bytes.len(), "index 必须是文件内的绝对偏移");
        // 绝对偏移应当 ≥ studiohdr2 的绝对位置（它排在文件后部）。
        assert!(
            idx > h2,
            "srcbonetransformindex({idx}) 应是文件绝对偏移，应大于 studiohdr2({h2})"
        );
    }

    /// `$maxeyedeflection` → `studiohdr2.flMaxEyeDeflection`（`+0x0C`）。
    ///
    /// # 判据（反汇编 `0x00450270` + 官方受控实验 `med1`/`med2`）
    ///
    /// 官方 handler 是 `atof(token) × π × (1/180)` 再 `fcos`
    /// ⟹ 落盘 **`cos(deg2rad(输入的度数))`**。
    ///
    /// | QC | 官方产物 | f32 |
    /// |---|---|---|
    /// | `$maxeyedeflection 30`（`med1`） | `0.8660253882408142` | `cos(30°)` |
    /// | `$maxeyedeflection 45`（`med2`） | `0.7071067690849304` | `cos(45°)` |
    ///
    /// 用 **45°** 是关键：它能一刀切开三种假设 ——
    /// 「原样落盘 45」得 `45.0`、「恒 cos(30°)」得 `0.866…`，
    /// 只有真的做 `cos(deg2rad(x))` 才得 `0.707…`。
    ///
    /// **不写时必须是 0** —— 官方 `.bss` 未初始化，引擎读到 0 才回退
    /// `cos(30°)`（`studio.h:2173`）。
    #[test]
    fn max_eye_deflection_is_cosine_of_degrees() {
        let rd = |b: &[u8], o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let rf = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());

        // 不写 → 0
        let one = write_mdl(&one_bone_desc()).unwrap();
        let h2 = rd(&one.bytes, off::STUDIO_HDR2_OFFSET) as usize;
        assert_eq!(
            rf(&one.bytes, h2 + 0x0C),
            0.0,
            "不写 $maxeyedeflection 时必须留 0（引擎据此回退 cos30°）"
        );

        // 30° → f32(cos 30°)，逐位对照官方 `med1`
        let mut d = one_bone_desc();
        d.desc.model.max_eye_deflection = Some(30.0);
        let w = write_mdl(&d).unwrap();
        let h2 = rd(&w.bytes, off::STUDIO_HDR2_OFFSET) as usize;
        assert_eq!(
            rf(&w.bytes, h2 + 0x0C),
            0.866_025_4_f32,
            "30° 应落盘 f32(cos 30°) = 0.8660253882408142"
        );

        // 45° → f32(cos 45°)，逐位对照官方 `med2`
        let mut d = one_bone_desc();
        d.desc.model.max_eye_deflection = Some(45.0);
        let w = write_mdl(&d).unwrap();
        let h2 = rd(&w.bytes, off::STUDIO_HDR2_OFFSET) as usize;
        assert_eq!(
            rf(&w.bytes, h2 + 0x0C),
            0.707_106_77_f32,
            "45° 应落盘 f32(cos 45°) = 0.7071067690849304"
        );
        // 反向证伪：若实现是「原样落盘」会得 45.0，若是「恒 cos30°」会得 0.866
        assert!(
            (rf(&w.bytes, h2 + 0x0C) - 45.0).abs() > 1.0,
            "必须真的做了 cos 转换，而不是原样落盘"
        );
    }

    /// quatinterp（proctype 2）：**两层结构** —— 记录数组 `N*12`，
    /// 然后 `ALIGN4`，再是逐记录的触发器区 `Σtriggers*48`。
    ///
    /// 判据来自官方受控实验 `qi1`（`docs/_probe/smdl/qi1.qc` + `qi1.vrd`）：
    /// 4 骨骼、2 条 quatinterp 记录（1 + 2 个触发器）→
    /// `boneEnd = 1528`、记录数组 `1528..1552`、触发器区 `1552..1696`、
    /// **`bonecontrollerindex = 1696`**。
    ///
    /// 语料 4 个模型（`survivor_gambler`/`coach`/`mechanic`/`producer`）
    /// 的同一公式 **4/4 命中**。
    #[test]
    fn quatinterp_two_level_layout() {
        let toml = r#"
[model]
name = "mymod/qi.mdl"

[materials]
textures = [{ name = "myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[quat_interp_bones]]
bone = "tip"
control = "root"
base_pos = [1.0, 2.0, 3.0]

[[quat_interp_bones.triggers]]
tolerance = 90.0
trigger = [10.0, 20.0, 30.0]
angles = [40.0, 50.0, 60.0]
pos = [0.5, 0.25, 0.125]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"

[[sequences]]
name = "idle"
smd = "myprop-ref.smd"
fps = 30.0
"#;
        let out = write_mdl(&build_from_toml(toml)).unwrap();
        let b = &out.bytes;
        let rd = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let rf = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let nb = rd(0x9C) as usize; // `numbones`（`off` 里没有具名常量）
        let bi = rd(off::BONE_OFFSET) as usize;
        let bone_end = bi + nb * BONE_SIZE;
        let proc_start = (bone_end + 3) & !3;
        // 单条记录：control = root(0)，1 个触发器。
        assert_eq!(rd(proc_start), 0, "第 0 条记录的 control = root(0)");
        assert_eq!(rd(proc_start + 4), 1, "第 0 条记录 1 个触发器");
        let rec_end = proc_start + QUATINTERP_BONE_SIZE;
        let trig = (rec_end + 3) & !3;
        assert_eq!(
            rd(proc_start + 8) as usize,
            trig - proc_start,
            "triggerindex 相对本记录自身"
        );
        // `inv_tolerance = 1 / deg2rad(90)` —— **倒数**，不是 tolerance 本身。
        let want = 1.0f32 / (90.0f32).to_radians();
        assert!((rf(trig) - want).abs() < 1e-6, "inv_tolerance 是倒数");
        // `pos` = base_pos + pos。
        assert!((rf(trig + 0x14) - 1.5).abs() < 1e-6);
        assert!((rf(trig + 0x18) - 2.25).abs() < 1e-6);
        assert!((rf(trig + 0x1C) - 3.125).abs() < 1e-6);
        // `proctype = 2` + `BONE_ALWAYS_PROCEDURAL`。
        let bo = bi + BONE_SIZE; // bone[1] = tip
        assert_eq!(rd(bo + bone_off::PROC_TYPE), 2, "tip 的 proctype");
        assert_ne!(
            rd(bo + bone_off::FLAGS) & BONE_ALWAYS_PROCEDURAL,
            0,
            "程序化骨骼要带 0x04"
        );
        // 块末尾 == bonecontrollerindex。
        let bci = rd(off::BONE_CONTROLLER_OFFSET) as usize;
        assert_eq!(
            (trig + QUATINTERP_INFO_SIZE + 3) & !3,
            bci,
            "程序化块末尾应等于 bonecontrollerindex"
        );
    }

    /// **触发条件**：`linearboneindex != 0` ⟺ `numbones >= 2`。
    ///
    /// 实测语料完美二分（517 / 0 / 0 / 2816，`probe_linearbone_trigger.js`），
    /// 578 个官方 artifacts 同样二分、**0 例外**。
    ///
    /// ⚠️ 判据是**骨骼数**，不是「是不是静态道具」：语料里另有 10 个
    /// `numbones == 1` 的**非**静态道具（`air_node`、`brokenglass_piece` 等）
    /// 同样没有该段。
    #[test]
    fn linearbone_present_iff_two_or_more_bones() {
        let two = write_mdl(&minimal()).unwrap();
        assert_ne!(linearbone_rel(&two.bytes), 0, "2 根骨骼必须有 linearbone");

        let one = write_mdl(&one_bone_desc()).unwrap();
        assert_eq!(
            linearbone_rel(&one.bytes),
            0,
            "1 根骨骼**不能**有 linearbone（该字段必须写 0）"
        );
    }

    /// `*index` 公式：`idx[k] = 64 + coeff[k]*n`，且 `idx[0] == 64`。
    ///
    /// 实测 517/517（`probe_linearbone_formula.js`）。
    /// 漏掉那个 64 会让所有索引小 64、读出头部自身（真踩过）。
    #[test]
    fn linearbone_indices_match_verified_formula() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let base = linearbone_base(b).expect("应有 linearbone");
        let n = i32::from_le_bytes(b[base..base + 4].try_into().unwrap()) as usize;
        assert_eq!(n, 2, "linearbone.numbones 应等于头部 numbones");

        for (k, (coeff, _)) in LINEARBONE_ARRAYS.iter().enumerate() {
            let o = base + 4 + k * 4;
            let got = i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
            let want = (LINEARBONE_HEADER_SIZE + coeff * n) as i32;
            assert_eq!(got, want, "idx[{k}] 不符合实测公式");
        }
        // `flagsindex` 恒为 64 —— 最容易漏掉的一项。
        assert_eq!(
            i32::from_le_bytes(b[base + 4..base + 8].try_into().unwrap()),
            64
        );
        // `unused[6]`：官方从不写（实测 517/517 全零）。
        for k in 0..6 {
            let o = base + 0x28 + k * 4;
            assert_eq!(
                i32::from_le_bytes(b[o..o + 4].try_into().unwrap()),
                0,
                "unused[{k}] 应为 0"
            );
        }
    }

    /// 段位置：起点 = `ALIGN4(keyvalues 末尾)`、`% 4 == 0`、且整段在字符串池之前。
    ///
    /// 实测：无 `srcbonetransform` 的 449 个模型**恰好**满足第一条（449/449）。
    #[test]
    fn linearbone_sits_between_keyvalues_and_strings() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let base = linearbone_base(b).expect("应有 linearbone");
        let g = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
        let kv = g(off::KEY_VALUE_OFFSET);
        let kv_size = g(off::KEY_VALUE_SIZE);
        assert_eq!(
            base,
            (kv + kv_size).div_ceil(4) * 4,
            "起点应为 ALIGN4(keyvalues 末尾)"
        );
        assert_eq!(base % 4, 0, "段起点必须 4 对齐");
        assert!(
            base + LINEARBONE_HEADER_SIZE + LINEARBONE_PER_BONE * 2 <= g(off::SURFACE_PROP_OFFSET),
            "linearbone 整段必须在字符串池之前"
        );
    }

    /// **核心判据**：9 个子数组逐位等于骨骼表的对应字段。
    ///
    /// 实测 517/517（`probe_linearbone_content.js`）。这条比「与官方逐字节相同」
    /// 更基础 —— 官方两侧可能同时因浮点噪声而彼此不同，但
    /// 「副本必须等于**本文件自己的**源」是无条件的。
    #[test]
    fn linearbone_arrays_are_byte_copies_of_the_bone_table() {
        let out = write_mdl(&minimal()).unwrap();
        let b = &out.bytes;
        let base = linearbone_base(b).expect("应有 linearbone");
        let bone = bone_off_of(b);
        let n = i32::from_le_bytes(b[base..base + 4].try_into().unwrap()) as usize;

        // 骨骼表内的字段偏移，顺序与 `LINEARBONE_ARRAYS` **一一对应**。
        let fields = [
            bone_off::FLAGS,
            bone_off::PARENT,
            bone_off::POSITION,
            bone_off::QUAT,
            bone_off::ROTATION,
            bone_off::POSE_TO_BONE,
            bone_off::POSITION_SCALE,
            bone_off::ROTATION_SCALE,
            bone_off::Q_ALIGNMENT,
        ];
        assert_eq!(fields.len(), LINEARBONE_ARRAYS.len());

        for ((coeff, elem), field) in LINEARBONE_ARRAYS.iter().zip(fields.iter()) {
            let dst = base + LINEARBONE_HEADER_SIZE + coeff * n;
            for i in 0..n {
                // ⚠️ 骨骼表是 **216 字节/记录**的 AoS，必须按记录步长走 ——
                // 连续走会得到「全不相等」的假象（写验收脚本时真踩过）。
                let src = bone + i * BONE_SIZE + field;
                assert_eq!(
                    &b[dst + i * elem..dst + (i + 1) * elem],
                    &b[src..src + elem],
                    "数组 (coeff={coeff}) 的 bone[{i}] 不是骨骼表的逐位副本"
                );
            }
        }
    }
}
