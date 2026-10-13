//! 动画写出：`mstudioanimdesc_t` / `mstudioseqdesc_t` / 每骨骼动画链。

use std::collections::HashMap;

use crate::ani_writer::{align16, write_ani_container};
use crate::model::CompiledModelDesc;

/// `mstudioanimdesc_t` 的字节大小。
pub const ANIMDESC_SIZE: usize = 100;
/// `mstudioseqdesc_t` 的字节大小。
pub const SEQDESC_SIZE: usize = 212;
/// `mstudioikrule_t` 的字节大小。
///
/// 反解方法（与 `mstudioiklink_t` = 28 同一手法）：`WriteIkErrors` 把 N 条
/// 规则**连续**写在 `animdesc.ikruleindex` 处，紧接着就是下一条动画的数据。
/// 用「规则数组 + 载荷 = 下一块起点」在**全语料 7669 个带规则的 animdesc**
/// 上反解 stride（`rsrch_ik_scan2.js`）：
///
/// ```text
/// stride 140 : 7663      stride 152 : 27743   ← 唯一命中
/// stride 144 : 7663      stride 156 : 7687
/// stride 148 : 7663      stride 160 : 7714
/// ```
///
/// 判据 = 每条记录满足 `{index==j, 1<=type<=6, 0<=chain<numikchains,
/// 0<=slot, -1<=bone<numbones}`。**152 是唯一让全部 30941 条规则自洽的值**。
pub const IK_RULE_SIZE: usize = 152;
/// `mstudiocompressedikerror_t` 的字节大小（`float scale[6]` + `short offset[6]`）。
///
/// 实测（`rsrch_ik_comp_size.js`，全语料 10230 条带载荷的规则）：
/// `offset[0] == 36` 于 **10230/10230**，且 6 个 offset **严格递增**
/// ⟹ 头正好 36 字节、6 个通道块**背靠背**。
pub const COMPRESSED_IK_ERROR_SIZE: usize = 36;
/// `mstudioiklock_t` 的字节大小（**32**）。
///
/// 用于**序列级** `iklock`（seqdesc 子表区，`write.cpp:596-611`）。
/// 与 `mdl_writer::IK_LOCK_SIZE` 是同一个结构体 —— 两处分别用于
/// 「模型级 `$ikautoplaylock` 数组」与「序列级子表」，值必须一致。
pub const IK_LOCK_SIZE: usize = 32;
/// Standard Source animation bone indices are 0..=254. Although the field is
/// a byte, 255 terminates the chain in the SDK's CalcAnimation loop.
/// See ValveSoftware/source-sdk-2013, src/public/bone_setup.cpp.
pub const MAX_ANIM_ADDRESSABLE_BONES: usize = 255;
/// `CompressIKErrors` 里位置通道的初值上界（`simplify.cpp:6650`）。
const IK_ERROR_POS_LIMIT: f32 = 128.0;
/// 旋转通道的初值上界 `π/8`（`simplify.cpp:6655`）。
const IK_ERROR_ROT_LIMIT: f32 = std::f32::consts::PI / 8.0;
/// `mstudioevent_t` 的字节大小。
///
/// # 布局（v49，实测确认）
///
/// ```text
/// +0x00  float  cycle
/// +0x04  int    event
/// +0x08  int    type
/// +0x0C  char   options[64]      ← 内联定长数组
/// +0x4C  int    szeventindex     ← **相对本事件自身**的名字偏移
/// ```
///
/// **名字不是内联数组**。`studio.h` 的 v49 定义就是
/// `int szeventindex; pszEventName() = (char*)this + szeventindex`。
/// 实测 `anim_common.mdl` 的 `AE_FOOTSTEP_RIGHT`：事件在 340228、
/// `szeventindex = 123319` → 名字在 463547，正确读出。
///
/// 字符串本身进**字符串池**（与其它名字一样），不在事件表里。
pub const EVENT_SIZE: usize = 80;

/// `mstudioevent_t.szeventindex` 在事件记录里的偏移。
///
/// `write_mdl` 回填事件名时要用它把「字段地址」换算回「记录地址」——
/// 官方 `AddToStringTable( &pevent[j], &pevent[j].szeventindex, name )`
/// 的基准是**记录**，不是字段。
pub const EVENT_NAME_FIELD_OFFSET: usize = 0x4C;
/// `mstudioanim_t` 的**头部**字节大小（`bone` + `flags` + `nextoffset`）。
pub const ANIM_HEADER_SIZE: usize = 4;
/// `mstudioanim_valueptr_t` 的字节大小（`short offset[3]`）。
pub const VALUEPTR_SIZE: usize = 6;

/// `STUDIO_ANIM_RAWPOS`：位移是常量 `Vector48`。
pub const STUDIO_ANIM_RAWPOS: u8 = 0x01;
/// `STUDIO_ANIM_RAWROT`：旋转是常量 `Quaternion48`。
pub const STUDIO_ANIM_RAWROT: u8 = 0x02;
/// `STUDIO_ANIM_ANIMPOS`：位移用 `mstudioanim_valueptr_t`。
pub const STUDIO_ANIM_ANIMPOS: u8 = 0x04;
/// `STUDIO_ANIM_ANIMROT`：旋转用 `mstudioanim_valueptr_t`。
pub const STUDIO_ANIM_ANIMROT: u8 = 0x08;
/// `STUDIO_ANIM_RAWROT2`：旋转是常量 `Quaternion64`。
pub const STUDIO_ANIM_RAWROT2: u8 = 0x20;
/// `STUDIO_ANIM_DELTA`：本轨道存的是**增量**（`subtract` 出来的动画）。
///
/// `studio.h`：`#define STUDIO_ANIM_DELTA 0x10`。
/// 与 `animdesc.flags` 的 `STUDIO_DELTA`（0x04）是**两个不同**的位。
pub const STUDIO_ANIM_DELTA: u8 = 0x10;

/// `STUDIO_LOOPING`：末帧与首帧相同。
pub const STUDIO_LOOPING: i32 = 0x0001;
/// `STUDIO_DELTA`：本序列存的是**增量**姿态（QC 的 `delta` / `predelta`）。
///
/// `studio.h`：`#define STUDIO_DELTA 0x0004`。
pub const STUDIO_DELTA: i32 = 0x0004;
/// `STUDIO_POST`：增量是「参考在左」（`subtract` / `delta` 都置它）。
///
/// `studio.h`：`#define STUDIO_POST 0x0010`。
pub const STUDIO_POST: i32 = 0x0010;
/// `STUDIO_ALLZEROS`：该动画没有真实动画数据。
pub const STUDIO_ALLZEROS: i32 = 0x0020;
/// `STUDIO_OVERRIDE`：一条**前向声明的空壳序列**（QC 的 `$declaresequence`）。
///
/// `studio.h:2003`：`#define STUDIO_OVERRIDE 0x0800`，
/// 官方注释是 `// a forward declared sequence (empty)`。
///
/// # 引擎侧语义（`studio_virtualmodel.cpp:185`）
///
/// ```c
/// else if (m_group[seq[k].group].GetStudioHdr()
///              ->pLocalSeqdesc(seq[k].index)->flags & STUDIO_OVERRIDE)
/// {
///     // the one in memory is a forward declared sequence, override it
///     virtualsequence_t tmp; tmp.group = group; tmp.index = j; ...
///     seq[k] = tmp;
/// }
/// ```
///
/// 即主模型里的空壳会被 `$includemodel` 进来的模型**按名字替换**。
/// 这正是 survivor 模组「网格与动画分开发布」的机制。
pub const STUDIO_OVERRIDE: i32 = 0x0800;

/// 量化除数。**是 32767 不是 32768** —— 实测用严格比较精确命中。
const QUANT_DIVISOR: f32 = 32767.0;
/// 旋转轴的**窗口初值**（π/8）。
///
/// ⚠️ 它不是「下限」，而是 `CompressAnimations` 给 `minv`/`maxv` 的
/// **初始值**（`simplify.cpp:6364-6365`）。窗口被实际差值撑开时，
/// `scale = 极值 / 32767`；没被撑开时停在 `π/8 / 32767`。
/// 两者在「极值 ≤ π/8」时同值，所以 `axis_scale` 能统一表达。
///
/// **没有「根骨骼 Z 用 π/2」这条规则** —— 曾经有过
/// （`ROOT_Z_ROT_SCALE_MIN`），但真 `studiomdl.exe` 编译 miku 时
/// `bone[0]` 的 `rotscale[2]` 停在 π/8，证伪了它。
const ROT_SCALE_MIN: f32 = std::f32::consts::FRAC_PI_8;
/// `posscale` 的窗口初值（128）。
const POS_SCALE_MIN: f32 = 128.0;

/// 写出错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnimWriteError {
    /// 帧数超出 `int32`。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Sequence %{sequence} has %{count} frames, which overflows int32",
            sequence = format!("{sequence:?}"),
            count = count
        )
    )]
    TooManyFrames { sequence: String, count: usize },
    /// 骨骼数超出可寻址范围。
    ///
    /// Index 255 is reserved for the animation-chain terminator.
    #[error(
        "{}",
        crate::tr_fmt!(
            "%{count} bones exceed what an animation record can address (<=255)",
            count = count
        )
    )]
    TooManyBones { count: usize },
    /// 内部不一致 —— 属本实现的 bug。
    #[error("{}", crate::tr_fmt!("Internal error (please report): %{err}", err = .0))]
    Internal(String),
    /// `$ikrule` 的声明有问题（链名 / 骨骼名找不到、参数冲突…）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "ikrule of the sequence %{sequence}: %{message}",
            sequence = format!("{sequence:?}"),
            message = message
        )
    )]
    IkRule { sequence: String, message: String },
}

/// 一根骨骼的**动画通道**：旋转/位移各 3 轴。
///
/// 每轴三态：
///   - `Absent`：该轴全常量 0（不需要数据，姿态由参考姿态决定）
///   - `Constant(v)`：该轴逐帧都是同一个**非零**量化值
///   - `Sampled(Vec<i16>)`：逐帧变化 → 用 valueptr + 流
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxisData {
    /// 全 0 增量（该轴不动）。
    Absent,
    /// 逐帧同一个非零量化值（如根骨骼 Z 的 +90° 偏置）。
    Constant(i16),
    /// 逐帧采样。
    Sampled(Vec<i16>),
}

impl AxisData {
    /// 该轴「随时间恒定」时的值（`Absent` 记 0，`Sampled` 返回 `None`）。
    fn constant_value(&self) -> Option<i16> {
        match self {
            Self::Absent => Some(0),
            Self::Constant(v) => Some(*v),
            Self::Sampled(_) => None,
        }
    }
}

/// 一根骨骼的动画通道。
///
/// 不用 `Eq`：`pos_const_raw` 是 `f32`，而 `f32` 没有 `Eq`。
/// `PartialEq` 足够（测试里只做相等比较）。
#[derive(Debug, Clone, PartialEq)]
pub struct BoneChannels {
    pub rot: [AxisData; 3],
    pub pos: [AxisData; 3],
    /// 位移三轴都恒定时，**未经量化**的那个常量位移（模型局部坐标）。
    ///
    /// 为什么必须单独带一份：官方写 `RAWPOS` 时存的是
    /// `*((Vector48 *)pData) = srcanim->sanim[0][j].pos;`
    /// （`write.cpp:765`）—— 即**原始浮点位置**，再经 `Vector48` 的
    /// binary16 编码。若用 `AxisData::Constant(v)` 里的
    /// `v * posscale` 反推，误差可达一个 `posscale`（约 0.0039），
    /// 会把常量位移写歪。
    ///
    /// ⚠️ 这是 `RAWPOS` 的**载荷**（绝对值，根骨骼已 yaw 旋转）。
    /// 「要不要写 `RAWPOS`」由 [`Self::pos_delta_is_const`] 决定 ——
    /// 两者是不同的量，不要混用。
    pub pos_const_raw: Option<[f32; 3]>,
    /// **差值** `(sanim − ref)` 是否逐帧恒定 —— 这才是 `RAWPOS` 的判据。
    ///
    /// 与 [`Self::pos_const_raw`]（绝对值载荷）分开存放，理由见
    /// `write_chain_body` 里 `use_rawpos` 的说明。
    pub pos_delta_is_const: bool,
    /// 旋转三轴都恒定时，`RAWROT2` 要写的**绝对**欧拉角（弧度）。
    ///
    /// # 为什么必须有这一份（这是「动画里骨骼旋转不正确」的根因）
    ///
    /// `RAWROT2`（`Quaternion64`）是**常量**，引擎**逐字返回**它、
    /// **不加**参考旋转：
    ///
    /// ```c
    /// // bone_setup.cpp:367-372
    /// if ( panim->flags & STUDIO_ANIM_RAWROT2 ) {
    ///     q = *(panim->pQuat64());
    ///     return;                     // ← 直接返回，不碰 baseRot
    /// }
    /// ```
    ///
    /// 对比 `ANIMROT`（`bone_setup.cpp:398-406`）**会**加 `baseRot`：
    /// ```c
    /// if (!(panim->flags & STUDIO_ANIM_DELTA)) angle1.x += baseRot.x;  // ← 加参考
    /// ```
    ///
    /// 而官方写出时用的是**绝对**旋转（`write.cpp:756`）：
    /// ```c
    /// AngleQuaternion( srcanim->sanim[0][j].rot, q );   // sanim = 绝对值
    /// ```
    ///
    /// ⟹ **`RAWROT2` 必须存绝对值**；存差值会让该骨骼整体少转一个
    /// 参考旋转。实测 `v_snip_awp` 根骨骼 `b0` 的参考旋转是
    /// **`[π/2, 0, 0]`**，于是动画里根骨骼**整整少转 90°**
    /// —— 与用户 HLMV 观察到的「参考姿态对、动画里旋转不对」完全吻合。
    ///
    /// 判据（`probe_const_dump.js` + 四元数点积）：
    /// | 侧 | 根骨骼 `b0` 的 `RAWROT2` 反解欧拉 |
    /// |---|---|
    /// | 官方 | `[π/2, 0, π/2]` = 参考 `[π/2,0,0]` **+** 差值 `[0,0,π/2]` |
    /// | mdlc（修前） | `[0, 0, π/2]` = **只有差值** ❌ |
    ///
    /// `aq(ref + delta)` 与官方四元数的 `|dot| = 1.0000005` ⟹ 精确命中。
    ///
    /// ⚠️ **`DELTA` 动画（`subtract` 出来的）例外**：它的 `sanim` 本身就是
    /// 增量、引擎也不加参考，所以绝对值 == 差值。
    pub rot_const_raw: Option<[f32; 3]>,
}

impl BoneChannels {
    /// 是否所有轴都无数据（该骨骼不进链）。
    pub fn is_empty(&self) -> bool {
        self.rot.iter().all(|c| matches!(c, AxisData::Absent))
            && self.pos.iter().all(|c| matches!(c, AxisData::Absent))
    }

    /// 旋转是否**逐帧恒定**？若是，返回该常量欧拉角（弧度）。
    ///
    /// # 这条规则曾经写错过
    ///
    /// 早先的实现要求「三轴是同一个非零常量」，那是把最小模型的
    /// `[0, 0, π/2]` 误读成了「整体一个角度」。实际规则是**逐轴**判断：
    /// `Absent` 记作 0，`Constant` 取其值，只要三轴都不随时间变化，
    /// 整条旋转就是常量 → 用 `RAWROT2`（8 字节 `Quaternion64`）。
    ///
    /// 实测对照：
    ///
    /// | 实验 | 逐帧旋转 | 官方编码 |
    /// |---|---|---|
    /// | 最小模型 | `[0, 0, π/2]`（根 Z 偏置）恒定 | `flags=0x20` `RAWROT2` |
    /// | `exp50` | 根绕 X 0→30°，Z 恒 π/2 | `flags=0x08` `ANIMROT` |
    /// | `exp53` | 根 Z 90→60° | `flags=0x08` `ANIMROT` |
    ///
    /// `exp50`/`exp53` 里 Z 轴**仍是常量**，但 X/Z 整体不恒定，
    /// 所以整条走 `ANIMROT`，常量轴退化成 `valid=1, total=N` 的单条 run
    /// （实测 `rotV=[6,0,16]`：偏移 0 的轴即「无数据」）。
    ///
    /// `Sampled` 出现在任一轴就返回 `None`。
    fn constant_rot(&self) -> Option<[f32; 3]> {
        let mut out = [0.0f32; 3];
        for (k, a) in self.rot.iter().enumerate() {
            out[k] = a.constant_value()? as f32;
        }
        Some(out)
    }

    /// 位移是否全部为 0。
    fn pos_all_absent(&self) -> bool {
        self.pos.iter().all(|a| matches!(a, AxisData::Absent))
    }
}

/// 把角度包裹到 `(-π, π]`。
///
/// 漏掉它会让「跨过 ±180°」的帧出现整圈跳变（插值时表现为模型瞬间翻转）。
pub fn wrap_to_pi(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let mut x = a % TAU;
    if x > PI {
        x -= TAU;
    } else if x <= -PI {
        x += TAU;
    }
    x
}

/// 量化除数：**极值为负时用 32768，为正时用 32767**。
///
/// # 为什么是符号相关的
///
/// `i16` 的范围是 `[-32768, +32767]` —— **不对称**。studiomdl 为了让极值
/// 正好压在边界上（不浪费精度、也不溢出），按极值符号选除数：
///
/// | 极值 | 除数 | 极值量化后 |
/// |---|---|---|
/// | 负 | **32768** | `trunc(−maxAbs / (maxAbs/32768))` = **−32768** |
/// | 正 | **32767** | `trunc(+maxAbs / (maxAbs/32767))` = **+32767** |
///
/// # 实测依据
///
/// 在 89 骨骼 × 30 帧的真实规模模型上，反解 `div = maxAbs / 官方scale`：
///
/// ```text
/// 极值为负的 31 个轴，平均解出除数 32767.995  → 32768
/// 极值为正的 22 个轴，平均解出除数 32766.998  → 32767
/// ```
///
/// 且「极值符号 → 除数」的预测在 53 个数据主导轴上**零反例**。
///
/// # 这纠正了 `docs/animation-layout.md` §4.2
///
/// 文档断言「除数确实是 32767，不是 32768」，依据是 `prec*` 实验的严格相等
/// 比较。但那些实验的极值恰好都落在**正**侧，于是只观察到了 32767 这一支。
fn quant_divisor(extreme: f32) -> f32 {
    if extreme < 0.0 { 32768.0 } else { 32767.0 }
}

/// 求某轴的 `scale`（`maxAbs / 除数`），并按需应用下限。
///
/// # 下限主导时除数恒为 32767
///
/// 下限（π/8、π/2、128）都是**正数常量**，所以「用下限」等价于
/// 「极值是一个正数」→ 除数 32767。实测印证：
///
/// | 轴 | 官方 `posscale` | `128/32767` | `128/32768` |
/// |---|---|---|---|
/// | 静止骨骼的位移 | **0.003906369209289551** | **0.003906369209289551** | 0.00390625 |
///
/// 若在下限分支也用符号除数，静止骨骼的 scale 会系统性偏小
/// （实测 245 个轴的缩放不一致、1318 个采样差 1 LSB）。
///
/// `extreme == 0`（该轴完全不动）走下限分支，结果同样是 `floor / 32767`。
fn axis_scale(extreme: f32, floor: f32) -> f32 {
    let max_abs = extreme.abs();
    if max_abs > floor {
        // ⚠️ **已排除的假设**：官方负支 `simplify.cpp:6423`
        // `scale = minv / -32768.0;` 里的 `-32768.0` 是**双精度**字面量
        // （而正支 `maxv / 32767` 是 int → f32），所以「负支用 f64 除再
        // 截回 f32」看起来能解释剩余的 1–7 ULP 差异。
        // **实测否证**：改成 `(f64::from(max_abs) / 32768.0) as f32` 后，
        // `cmp_miku_real.js` 仍是 2323 一致 / 121 不同 —— 一项没少。
        // 所以剩余差异来自**极值本身**（参考姿态的计算路径），不是除法。
        max_abs / quant_divisor(extreme)
    } else {
        floor / QUANT_DIVISOR
    }
}

/// 量化：`trunc(v / scale)` 并夹到 `i16`。
///
/// # 是**截断**，不是四舍五入
///
/// 实测证据（`docs/_probe` 下的受控实验，两条独立判据）：
///
/// **判据 1 —— 正数**（`exp65`，Z 轴 0/10/20/30/40°，`rotscale = 2.130593748e-5`）：
///
/// | 角度 | `v / scale` | 截断 | 四舍五入 | 官方产物 |
/// |---|---|---|---|---|
/// | 10° | 8191.7505 | **8191** | 8192 | **8191** |
/// | 20° | 16383.5010 | **16383** | 16384 | **16383** |
/// | 30° | 24575.2520 | 24575 | 24575 | 24575 |
/// | 40° | 32767.0020 | 32767 | 32767 | 32767 |
///
/// **判据 2 —— 负数**（`negq`，Z 轴 0/−0.1/−0.2/−0.3，`rotscale = π/8/32767`）：
///
/// | 角度 | `v / scale` | 向零截断 | 向下取整 | 官方产物 |
/// |---|---|---|---|---|
/// | −0.1 | −8344.0479 | **−8344** | −8345 | **−8344** |
/// | −0.2 | −16688.0957 | **−16688** | −16689 | **−16688** |
/// | −0.3 | −25032.1445 | **−25032** | −25033 | **−25032** |
///
/// 判据 2 同时排除了「向下取整」；两条合起来唯一确定「向零截断」。
/// 用 `round()` 会让每个通道有约一半的帧差 1 LSB。
fn quantize(v: f32, scale: f32) -> i16 {
    (v / scale).trunc().clamp(-32768.0, 32767.0) as i16
}

/// 求某轴的 `(max_abs, extreme)` —— 最大绝对值，以及取到该值的那个**带符号**的值。
///
/// 符号是必需的：除数的选择依赖它（见 [`quant_divisor`]）。
fn max_abs_and_extreme(values: &[f32]) -> (f32, f32) {
    let mut max_abs = 0.0f32;
    let mut extreme = 0.0f32;
    for v in values {
        if v.abs() > max_abs {
            max_abs = v.abs();
            extreme = *v;
        }
    }
    (max_abs, extreme)
}

/// 一个序列里全部骨骼的 `(旋转增量帧, 位移增量帧)`。
type BoneFrameTable = Vec<(Vec<[f32; 3]>, Vec<[f32; 3]>)>;

/// `RAWPOS` 要写的**绝对**局部位移（根骨骼已按 yaw +90° 旋转过）。
///
/// 与 `per_seq_frames` **同下标**平行存放 —— 见 `write_animations`
/// 里 1b 段的说明（`RAWPOS` 存绝对值、`ANIMPOS` 存差值，两者语义不同）。
type AbsPosTable = Vec<[f32; 3]>;

/// `RAWROT2` 要写的**绝对**旋转欧拉角（弧度）。
///
/// 与 [`AbsPosTable`] 完全对称 —— `RAWROT2` 也是常量载荷，引擎不加参考旋转。
/// 详见 [`BoneChannels::rot_const_raw`]。
type AbsRotTable = Vec<[f32; 3]>;

/// 把逐帧同值的量化序列折叠成常量；全 0 折叠成 [`AxisData::Absent`]。
///
/// 「全 0」等价于「该轴不动」—— 增量本来就是相对第 0 帧算的，所以
/// **不随时间变化的骨骼，其增量恒为 0**。官方对这类轴写 `valueptr` 偏移 0
/// （实测 `exp50` 的 `rotV = [6, 0, 16]`：中间那个 0 就是恒 0 的 Y 轴），
/// 折叠成 `Absent` 正好复刻这个行为。
///
/// 唯一会产出 `Constant(非 0)` 的情形是**根骨骼的 Z 轴**：它的 +90° 偏置
/// 让增量恒为 π/2（见 [`delta_frames`]）。
fn fold_axis(q: Vec<i16>) -> AxisData {
    match q.first() {
        Some(&first) if q.iter().all(|v| *v == first) => {
            if first == 0 {
                AxisData::Absent
            } else {
                AxisData::Constant(first)
            }
        }
        _ => AxisData::Sampled(q),
    }
}

/// 计算骨骼 `b` 在 `seq` 里的逐帧增量（**浮点**，尚未量化）。
///
/// # 规则（已用真实规模模型逐值验证）
///
/// ```text
/// 旋转：stored[f][k] = canonical_euler(pose[f])[k] − ref_rot[k] + (k==2 && 根 ? π/2 : 0)
/// 位移：stored[f][k] = pose[f].pos[k] − ref_pos[k]
/// ```
///
/// 其中 `ref_rot` / `ref_pos` 是**骨骼表里的参考姿态**
/// （`mstudiobone_t.rot` / `.pos`），即 SMD 第 0 帧或 `$definebone` 覆盖后的值。
///
/// # 这条规则纠正了 `docs/animation-layout.md` §3.5
///
/// 文档写的是「逐帧值 = 相对**第 0 帧**的增量」。那个结论来自
/// `exp50`/`exp53`/`exp64`/`exp65` —— 它们的第 0 帧旋转恰好都是 0，
/// 于是「相对第 0 帧」与「相对参考姿态」给出**完全相同**的结果。
/// 在 89 骨骼 × 30 帧的真实规模上，两者命中率是 **13.14% vs 100.00%**。
///
/// # 三步的**顺序**同样关键
///
///   1. `canonical_euler(pose)` —— 先规范化（复刻 studiomdl 的欧拉分解）
///   2. 减去参考姿态
///   3. 根骨骼 Z 轴 +90° 偏置（**最后加**）
///
/// 第 3 步放错位置会让根骨骼 Z 不再恒定 → 退化成逐帧流，
/// 而官方写的是 `RAWROT2` 常量。`gen_rootbias` 实验（根骨骼 Z 参考姿态
/// 非零）确认了偏置是「减完再加」而不是「先给参考加」：
/// 实测 `2.234459` 对上 H1 的 `2.234464`，H2 会给出 `−0.907129`。
///
/// `LOOPING` 的末帧处理见 [`loop_tail`]。
fn delta_frames(
    frames: &[Vec<crate::smd::SmdPose>],
    looping: bool,
    b: usize,
    n: usize,
    is_root: bool,
    ref_rot: [f32; 3],
    ref_pos: [f32; 3],
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let mut rot_frames: Vec<[f32; 3]> = Vec::with_capacity(n);
    let mut pos_frames: Vec<[f32; 3]> = Vec::with_capacity(n);
    if n == 0 {
        return (rot_frames, pos_frames);
    }
    for f in 0..n {
        // ⚠️ **帧行可能比骨骼池短。**
        //
        // 序列级 `subtract` 之后写出器改读**序列**的帧（见
        // [`AnimSpec::cell_frames`]），而序列的帧行由 SMD 决定 ——
        // 受控夹具里常见「1 根骨骼的 SMD + 2 根骨骼的模型」。
        // 直接 `frames[f][b]` 会 panic（实测
        // `single_frame_animation_writes_fixed_cycles` 崩在 len=1/index=1）。
        //
        // 缺失的骨骼按「零增量」处理 —— 与官方 `Grab_Animation`
        // （`studiomdl.cpp:1065-1135`，`kalloc` 打底、缺的骨骼留 0）一致。
        let Some(p) = frames.get(f).and_then(|r| r.get(b)) else {
            rot_frames.push([0.0f32; 3]);
            pos_frames.push([0.0f32; 3]);
            continue;
        };
        let loop_tail = looping && f == n - 1;
        let mut r = [0.0f32; 3];
        if loop_tail {
            // 末帧与首帧同姿态：直接照抄**第 0 帧的存储值**。
            //
            // 注意**不是**「增量归零」。在「相对参考姿态」的模型下，
            // 增量归零意味着「回到参考姿态」，而参考姿态未必等于第 0 帧。
            // `gen_loop` 实验（第 0 帧偏离参考姿态）实测：
            // 末帧存储值 == 第 0 帧存储值（`0.499991`），而不是 0。
            r = frame_stored_rot(frames, b, 0, ref_rot);
        } else {
            let canon = crate::bone_math::canonical_euler(p.rotation);
            for (k, rk) in r.iter_mut().enumerate() {
                // `canonical_euler` 的值域是 `(−π, π]`（`atan2` 的割线），
                // 而参考姿态可能也在 π 附近。直接相减会在割线处产生 ±2π 跳变：
                //
                //   官方 ref[0] = 3.1357（≈ +π），某帧 canon[0] = −3.1403（≈ −π）
                //   直接减 → −6.2760（差了一个整圈，量化后 −32768）
                //   包裹后 → +0.0072（官方实测值）
                //
                // 实测（`biganim` bone 66 "bolt"，roll ≈ ±π）：
                // 不包裹会有 2 帧跳到 −32768，是唯一一处幅度达 33367 的偏差。
                *rk = wrap_to_pi(canon[k] - ref_rot[k]);
            }
        }
        if is_root {
            // ⚠️ **偏置必须在 `wrap_to_pi` 之前加。**
            //
            // `simplify.cpp:6516-6534`（`CompressAnimations` 的极值统计）：
            // ```c
            // v = ( sanim[n][j].rot[k-3] - g_bonetable[j].rot[k-3] );
            // while (v >=  M_PI) v -= M_PI * 2;      // ← wrap 在**最后**
            // while (v <  -M_PI) v += M_PI * 2;
            // ```
            // `sanim` **已经过 `rootxform` 复合**（含 +90° 偏置），
            // 而 `g_bonetable[j].rot` 是 rest 姿态（**不含**偏置）。
            // 所以 `v = rawdiff + π/2`，**然后**才 wrap。
            //
            // 早先 mdlc 是「先 wrap 再 +π/2」，于是 `183.37°` 这种超过 π
            // 的值**没有被 wrap 回来**，`rotscale` 的窗口被撑大。
            //
            // 实测判据（`probe_wrap_order.js`，`bone 65` 轴 2）：
            // | 顺序 | `rotscale[2]` | 官方 |
            // |---|---|---|
            // | **wrap 后加偏置（官方序）** | **`9.508662e-5`** | `9.508663e-5` ✅ |
            // | wrap 先加偏置（旧实现） | `9.767091e-5` | ❌ |
            //
            // 同一个 bug 也让 `bone 73` 轴 2 差 `9.756e-5` vs `9.530e-5`。
            // 修正后两者的窗口都恰好命中官方。
            //
            // 注意 `canonical_euler` 本身已经输出 `(−π, π]`，
            // 所以「加偏置后 wrap」等价于「加偏置后若 > π 就减 2π」。
            r[2] = wrap_to_pi(r[2] + std::f32::consts::FRAC_PI_2);
        }
        rot_frames.push(r);

        let mut t = [0.0f32; 3];
        for (k, tk) in t.iter_mut().enumerate() {
            // ⚠️ **位移不做 `LOOPING` 末帧折叠**（旋转做）。
            //
            // 这条规则目前**证据有冲突**，如实记录（未闭合，见 PROGRESS §27.10）：
            //
            // **支持「不折叠」**（证据更强，来自真实项目）：
            // 官方 miku `anim[0]`（121 帧）里 `bone 12`/`31`/`60` 的位移
            // **只在末帧**变化，官方写 `ANIMPOS`（`0xc`）。
            // 实测不符记录数：不折叠 **112**，折叠 **124** —— 不折叠明显更好。
            //
            // **支持「折叠」**（受控实验 `ikc4`，5 帧）：
            // ankle 位移只在末帧变化 → 官方**完全没有轨道**。
            //
            // 差别可能在帧数（5 vs 121）或 fixture 的其它属性，**尚未定死**。
            // 在拿到新判据前按真实项目走。
            *tk = p.position[k] - ref_pos[k];
        }
        if is_root {
            // ANIMPOS is R * position - reference, not R * (position - reference).
            // RAWPOS stores the same transformed position without subtraction.
            let position = crate::compile::static_prop_rotate(p.position);
            t = std::array::from_fn(|axis| position[axis] - ref_pos[axis]);
        }
        pos_frames.push(t);
    }
    (rot_frames, pos_frames)
}

/// **`STUDIO_DELTA` 动画**的逐帧存储值。
///
/// # 与普通动画的三处差别
///
/// 1. **不再减去参考姿态。** `subtract`（`simplify.cpp:1066-1122`）已经
///    把「相对参考动画的偏移」算好存进了 SMD 姿态里，官方写出的就是
///    那个值本身（`sanim` 直接来自 SMD）。
/// 2. **不加根骨骼 Z 偏置。** `panim->rotation` 对 `subtract` 出来的
///    动画仍是默认值 —— 但增量本身就是「差值」，再叠一个 +90° 会让
///    它变成一个非零常量，于是每根骨骼都多出一条常量轨道。
/// 3. **逐帧规范化仍然要**（`canonical_euler`），因为 SMD 里的欧拉角
///    可能超出 `(−π, π]`。
///
/// # 实测判据（受控实验 `blend1`，官方 `studiomdl.exe` 产物）
///
/// `$animation "look_down" "blendpose.smd" frames 0 0 subtract "a_base" 0`
/// 的官方 animdesc **只有 1 条骨骼轨道**（bone 3，`flags=0x30` =
/// `RAWROT2|DELTA`），而 `blendpose.smd` 有 4 根骨骼 —— 因为只有
/// `ankle` 相对 `a_base` 真的变了。按普通动画的规则写会得到 4 条轨道
/// （bone 0/1/2 是「常量 0 增量」+ 根骨骼的 +90° 偏置），与官方不符。
fn delta_stored_frames(
    frames: &[Vec<crate::smd::SmdPose>],
    looping: bool,
    b: usize,
    n: usize,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let mut rot_frames: Vec<[f32; 3]> = Vec::with_capacity(n);
    let mut pos_frames: Vec<[f32; 3]> = Vec::with_capacity(n);
    if n == 0 {
        return (rot_frames, pos_frames);
    }
    for f in 0..n {
        let loop_tail = looping && f == n - 1;
        let src = if loop_tail { 0 } else { f };
        // 帧行可能比骨骼池短 —— 缺的骨骼按零增量（同 `delta_frames`）。
        let Some(p) = frames.get(src).and_then(|r| r.get(b)) else {
            rot_frames.push([0.0f32; 3]);
            pos_frames.push([0.0f32; 3]);
            continue;
        };
        rot_frames.push(crate::bone_math::canonical_euler(p.rotation));
        pos_frames.push(p.position);
    }
    (rot_frames, pos_frames)
}

/// 第 `f` 帧的存储旋转（**不含**根骨骼 Z 偏置 —— 由调用方统一加）。
///
/// 抽出来是为了让 `LOOPING` 末帧能照抄第 0 帧的存储值。
/// 注意**不要**在这里加偏置：调用方在拿到结果后会统一加一次
/// （且是「加完再 wrap」，见 `delta_frames` 里的说明），
/// 两边都加会变成 `π`（这个 bug 真的写出来过，被
/// `still_root_uses_rawrot2_like_official` 抓到）。
pub(crate) fn frame_stored_rot(
    frames: &[Vec<crate::smd::SmdPose>],
    b: usize,
    f: usize,
    ref_rot: [f32; 3],
) -> [f32; 3] {
    // 帧行可能比骨骼池短 —— 缺的骨骼按零增量（同 `delta_frames`）。
    let Some(p) = frames.get(f).and_then(|r| r.get(b)) else {
        return [0.0f32; 3];
    };
    let canon = crate::bone_math::canonical_euler(p.rotation);
    let mut r = [0.0f32; 3];
    for (k, rk) in r.iter_mut().enumerate() {
        *rk = wrap_to_pi(canon[k] - ref_rot[k]);
    }
    r
}

/// 写一个轴的 `mstudioanimvalue_t` 流，并把偏移回填进 `valueptr`。
///
/// 采用 `valid == total == N`（逐帧完整数据，不做 run 合并）——
/// 语义合法，任何合规解码器都能读。代价见模块头注释。
///
/// 三态处理：
///   - `Absent`：**不写流、偏移留 0**（`pAnimvalue` 返回 NULL，解码器跳过）
///   - `Constant(v)`：写 `valid=1, total=N` + 1 个采样（解码器复制 N 次）
///   - `Sampled`：写 `valid=total=N` + N 个采样
///
/// 偏移是**相对该 `valueptr` 自身**的，`offset[i] > 0` 才有效
/// （见 `mstudioanim_valueptr_t::pAnimvalue`）。
/// 官方 `CompressAnimations` 的 **RLE run 合并**（`simplify.cpp:6542-6596`）。
///
/// 返回若干 `(valid, total, values)` 三元组，每个对应一条 run：
/// `valid` 个采样值覆盖 `total` 帧。
///
/// # 算法（逐字复刻源码）
///
/// ```c
/// pcount->num.valid = 1;  pcount->num.total = 1;  pvalue->value = value[0];
/// for (m = 1; m < n; m++) {
///     if (pcount->num.total == 255) {          // 链太长，强制开新记录
///         pcount = pvalue; pvalue = pcount + 1;
///         pcount->num.valid++; pvalue->value = value[m]; pvalue++;
///     }
///     // 值变了，或者「还没成 run 且下一个值也不同」时插入
///     else if ((value[m] != value[m-1])
///           || ((pcount->num.total == pcount->num.valid)
///               && ((m < n - 1) && value[m] != value[m+1]))) {
///         if (pcount->num.total != pcount->num.valid) {
///             pcount = pvalue; pvalue = pcount + 1;   // 另起一条 run
///         }
///         pcount->num.valid++; pvalue->value = value[m]; pvalue++;
///     }
///     pcount->num.total++;
/// }
/// ```
///
/// 第二个条件里的 `total == valid` 意思是「目前这条 run 还没开始重复」，
/// 此时若**下一个**值又不同，就没必要把它留成 run 的尾巴，直接插入。
///
/// # 效果
///
/// 全常量轴编码成 `{1, N, [v]}`（与 [`AxisData::Constant`] 同形），
/// 阶梯数据则合并成若干条 run —— 这正是官方链比「逐帧全量」短得多的原因。
fn rle_runs(values: &[i16]) -> Vec<(u8, u8, Vec<i16>)> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let mut runs: Vec<(u8, u8, Vec<i16>)> = Vec::new();
    let mut valid: u8 = 1;
    let mut total: u8 = 1;
    let mut vals: Vec<i16> = vec![values[0]];
    for m in 1..n {
        if total == 255 {
            // 链太长，强制开新记录（旧 run 的 total 保持 255）。
            runs.push((valid, total, std::mem::take(&mut vals)));
            valid = 1;
            total = 0;
            vals.push(values[m]);
        } else if values[m] != values[m - 1]
            || (total == valid && m < n - 1 && values[m] != values[m + 1])
        {
            if total != valid {
                runs.push((valid, total, std::mem::take(&mut vals)));
                valid = 0;
                total = 0;
            }
            valid += 1;
            vals.push(values[m]);
        }
        total += 1;
    }
    runs.push((valid, total, vals));
    runs
}

/// 写一条轴的数据（valueptr + RLE 流）。
fn write_axis(
    anim_data: &mut Vec<u8>,
    ptr_pos: usize,
    axis: usize,
    data: &AxisData,
    n: usize,
) -> Result<(), AnimWriteError> {
    let at = ptr_pos + axis * 2;
    match data {
        AxisData::Absent => {
            // 偏移留 0 → `pAnimvalue` 返回 NULL。
            anim_data[at..at + 2].copy_from_slice(&0i16.to_le_bytes());
        }
        AxisData::Constant(v) => {
            let off = anim_data.len() - ptr_pos;
            let off_i16 = i16::try_from(off).map_err(|_| {
                AnimWriteError::Internal(crate::tr(
                    "the channel offset overflows i16 (the animation is too large)",
                ))
            })?;
            anim_data[at..at + 2].copy_from_slice(&off_i16.to_le_bytes());
            // ⚠️ **`total` 是 `u8`，所以超过 255 帧必须拆成多条 run。**
            //
            // 早先这里写 `u8::try_from(n).unwrap_or(u8::MAX)` —— 把长度
            // **截断**到 255。解码器（`bone_setup.cpp` 的
            // `CalcBonePosition`/`CalcBoneQuaternion`）按 `total` 复制采样，
            // 于是**第 255 帧之后完全没有数据**，那些帧退化到
            // 「上一个采样 + 参考姿态」⟹ 骨骼错乱。
            //
            // 实测（`v_dual_pistola`，`$sectionframes 30 999` 让全部动画不分段）
            // `@item_loop`（**325 帧**）的 `b2 Camera`：
            //
            // ```text
            // 官方 pos[0] spans: v1/t255 v1/t70 ...     ← 255 + 70 = 325
            // mdlc 修前        : 单条 v1/t255            ← 只覆盖 255 帧
            // ```
            //
            // 症状：前 255 帧完全正常，**第 255 帧起位移差最大 71.1**、
            // 旋转差最大 17.76°（`@item_loop` / `@helping_hand_loop` 及其
            // `_layer` 共 4 条 325 帧动画、198 个坏帧）。
            //
            // 拆分方式与 [`rle_runs`] 一致：每条 run 最多 255 帧，
            // 每条都是 `valid = 1`（一个采样）复制 `total` 次。
            let mut left = n;
            while left > 0 {
                let chunk = left.min(255);
                anim_data.push(1);
                anim_data.push(chunk as u8);
                anim_data.extend_from_slice(&v.to_le_bytes());
                left -= chunk;
            }
        }
        AxisData::Sampled(s) => {
            let runs = rle_runs(s);
            // 官方：`numanim == 2 && value[0] == 0` → 该轴视为**无数据**
            // （偏移写 0）。`numanim` 是 `mstudioanimvalue_t` 的**个数**，
            // 单条 run 且 `valid == 1` 时正好是 `1 + 1 = 2`。
            if runs.len() == 1 && runs[0].0 == 1 && runs[0].2[0] == 0 {
                anim_data[at..at + 2].copy_from_slice(&0i16.to_le_bytes());
                return Ok(());
            }
            let off = anim_data.len() - ptr_pos;
            let off_i16 = i16::try_from(off).map_err(|_| {
                AnimWriteError::Internal(crate::tr(
                    "the channel offset overflows i16 (the animation is too large)",
                ))
            })?;
            anim_data[at..at + 2].copy_from_slice(&off_i16.to_le_bytes());
            for (valid, total, vals) in &runs {
                anim_data.push(*valid);
                anim_data.push(*total);
                for v in vals {
                    anim_data.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    Ok(())
}

/// 把「三轴同一个角度」的常量旋转编码成 `Quaternion64`（8 字节）。
///
/// `RAWROT2` 的载荷是 21/21/21/1 位打包的四元数（`compressed_vector.h`）：
///
/// ```text
/// x = clamp((int)(qx * 1048576) + 1048576, 0, 2097151)
/// y = 同上
/// z = 同上
/// wneg = (qw < 0)
/// 打包：x | (y << 21) | (z << 42) | (wneg << 63)   —— 小端 8 字节
/// ```
///
/// 反解时 `w = ±sqrt(1 − x² − y² − z²)`，**w 不进位流**。
fn encode_quaternion64(q: [f32; 4]) -> [u8; 8] {
    let pack = |v: f32| -> u64 {
        let i = (v * 1048576.0) as i64 + 1048576;
        i.clamp(0, 2097151) as u64
    };
    let bits = pack(q[0]) | (pack(q[1]) << 21) | (pack(q[2]) << 42) | (((q[3] < 0.0) as u64) << 63);
    bits.to_le_bytes()
}

/// 把常量位移编码成 `Vector48`（6 字节 = 3 × IEEE **binary16**）。
///
/// # 这不是定点数（曾经写错过）
///
/// `compressed_vector.h:517-533`：
/// ```cpp
/// class Vector48 {
///     Vector48(vec_t X, vec_t Y, vec_t Z) { x.SetFloat(X); y.SetFloat(Y); z.SetFloat(Z); }
///     float16 x;  float16 y;  float16 z;      // ← float16，不是定点
/// };
/// ```
/// `float16::SetFloat` → `ConvertFloatTo16bits`（标准 binary16，1+5+10 位）。
///
/// 早先按「`1/32768.5` 定点」实现（照抄 `Quaternion48` 的标度），
/// 解码官方产物得到 `[-0.51, 0.52, 0.50]` 这种毫无意义的数 ——
/// 那是**解码器错**，不是官方错。
///
/// 判据：官方 miku `bone[0]` 的 `RAWPOS = 65 3e 31 c2 98 bf`，
/// 按 binary16 解码得 `[1.598633, -3.095703, -1.898438]`，
/// 恰好等于 `rotate90(SMD pos − ref pos)`（见
/// `docs/_probe/decode_vector48_half.js`）。
///
/// ⚠️ 位移**没有** `RAWROT2` 那样的 64 位版本 —— 常量位移只有这一种形态。
fn encode_vector48(t: [f32; 3]) -> [u8; 6] {
    let mut out = [0u8; 6];
    for (k, v) in t.iter().enumerate() {
        out[k * 2..k * 2 + 2].copy_from_slice(&float_to_half(*v).to_le_bytes());
    }
    out
}

/// `float_to_half` 的公开包装 —— 供 `mdl_writer` 写 VTA 的
/// `mstudiovertanim_t.delta`/`ndelta` 复用（**必须**与动画用同一套
/// 舍入方式，否则同一数值在两处会编出不同的 half）。
pub fn float_to_half_public(f: f32) -> u16 {
    float_to_half(f)
}

/// `float32` → IEEE **binary16**（1 符号 + 5 指数 + 10 尾数），**向零截断**。
///
/// # 舍入方式是实测出来的，不是猜的
///
/// 起初按「舍入到最近偶数」（IEEE 默认）实现，官方 miku `bone[0]` 的
/// `RAWPOS` 有一字节不符（`c231` vs `c232`）。
///
/// 受控判据（`docs/_probe/probe_half_rounding.js`，全部 7 条根骨骼
/// `RAWPOS` 记录）：
///
/// | 舍入方式 | 命中 |
/// |---|---|
/// | 舍入到最近偶数（RN） | **2 / 7** |
/// | **向零截断（TZ）** | **7 / 7** |
///
/// 例（`bone 0`，值 `-3.097656`）：RN → `0xc232`，TZ → `0xc231`，
/// 官方是 **`0xc231`**。
///
/// 与 Source 的实现一致：`float16::ConvertFloatTo16bits` 用纯位截断
/// （`compressed_vector.h`），没有加 round-to-nearest 的偏置。
fn float_to_half(f: f32) -> u16 {
    let bits = f.to_bits();
    let sign = ((bits >> 31) & 1) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x7f_ffff;

    // 无穷 / NaN
    if exp == 0xff {
        return (sign << 15) | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let new_exp = exp - 127 + 15;
    if new_exp >= 31 {
        // 上溢 → 无穷
        return (sign << 15) | 0x7c00;
    }
    if new_exp <= 0 {
        // 次正规数（或下溢到 0）
        if new_exp < -10 {
            return sign << 15;
        }
        let m = mant | 0x80_0000;
        let shift = (14 - new_exp) as u32;
        let hm = (m >> shift) as u16;
        return (sign << 15) | hm;
    }
    let hm = (mant >> 13) as u16;
    (sign << 15) | ((new_exp as u16) << 10) | hm
}

/// 一个序列的动画数据统计（供日志与自检）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceStat {
    pub name: String,
    pub frames: usize,
    /// 进入动画链的骨骼数。
    pub animated_bones: usize,
    /// 该序列的动画字节数（含链头与流）。
    pub anim_bytes: usize,
}

/// 写出结果：动画相关各段的字节与布局信息。
#[derive(Debug, Clone, PartialEq)]
pub struct AnimWriteOutcome {
    pub hierarchy_offsets: Vec<Option<usize>>,
    pub anim_block_hierarchy_offsets: Vec<Option<usize>>,
    /// `mstudioanimdesc_t` 数组（`numlocalanim` 项）。
    pub animdescs: Vec<u8>,
    /// `mstudioseqdesc_t` 数组（`numlocalseq` 项）。
    pub seqdescs: Vec<u8>,
    /// 动画数据区（所有 animdesc 的链，按 animdesc 顺序拼接）。
    pub anim_data: Vec<u8>,
    /// 每个 animdesc 在 [`Self::anim_data`] 内的起始偏移。
    pub anim_offsets: Vec<usize>,
    /// 每个 animdesc 的 **IK rule 块**在 [`Self::anim_data`] 内的起始偏移
    /// （没有规则时为 `None`）。
    ///
    /// `mstudioanimdesc_t.ikruleindex` 是**相对该 animdesc 自身**的偏移，
    /// 所以要等 `write_mdl` 知道 `anim_data` 的绝对位置才能回填
    /// （与 `animindex` 同一个理由）。
    pub ikrule_offsets: Vec<Option<usize>>,
    /// 每个 animdesc 的 `numikrules`（`+0x3C`）。
    pub num_ikrules: Vec<usize>,
    /// 每个 animdesc 的 **movement 数组**在 [`Self::anim_data`] 内的起始偏移
    /// （没有 movement 时为 `None`）。
    ///
    /// `mstudioanimdesc_t.movementindex`（`+0x18`）**相对该 animdesc 自身**，
    /// 所以要等 `write_mdl` 知道 `anim_data` 的绝对位置才能回填
    /// （与 `animindex`/`ikruleindex` 同一个理由）。
    pub movement_offsets: Vec<Option<usize>>,
    /// 每个 animdesc 的**段表**在 [`Self::anim_data`] 内的起始偏移
    /// （不分段时为 `None`）。
    ///
    /// 段表内部各条的 `animindex` 先写成「相对 `anim_data` 起点」的临时值，
    /// 由 `write_mdl` 换算成「相对 animdesc 自身」（`sectionindex` @ `+0x50`）。
    pub section_table_offsets: Vec<Option<usize>>,
    /// 每个 animdesc 的 `sectionframes`（`+0x54`，`0` = 不分段）。
    pub section_frames: Vec<i32>,
    /// 每个 animdesc 的段表条目数（`floor(nf/sf) + 2`）。
    pub num_sections: Vec<usize>,
    /// **外置动画块（`.ani`）**：每条的载荷字节，按块序排列（**不含** `block[0]` 哨兵）。
    ///
    /// 只有 `numframes >= 2` 的动画才会进块 —— 实测 `ab_z1`（单帧）与
    /// `zb90z`（单帧）都是 `animblock = 0`、留在内联，而 `numframes >= 2`
    /// 的 8 个用例全部 `animblock = 1`。
    ///
    /// 块内每段载荷前面要补 `ALIGN16`，拼接规则见 [`pack_anim_blocks`]。
    pub anim_blocks: Vec<Vec<u8>>,
    /// 每个 animdesc 的 `animblock`（`+0x34`）：`0` = 内联，`>= 1` = 块下标。
    ///
    /// 与 [`Self::anim_blocks`] 配合：非 0 时该动画的数据**不在** `.mdl` 里。
    pub anim_block_index: Vec<i32>,
    /// 每个 animdesc 的 `animindex`（`+0x38`）在**块内**的偏移（仅外置动画有意义）。
    ///
    /// `None` 表示该动画走内联（此时 `animindex` 仍按老规则算）。
    pub anim_block_offset: Vec<Option<usize>>,
    /// 每个 animdesc 的 `animblockikruleindex`（`+0x44`）在**块内**的偏移
    /// （该动画没有 IK rule 时为 `None`）。
    ///
    /// # 实测规则
    ///
    /// 官方 `write.cpp:1061-1062` 把 IK rule 块**直接追加在载荷之后**、
    /// 中间不做任何对齐：
    ///
    /// ```c
    /// byte *pIkData   = WriteAnimationData( srcanim, pBlockData );
    /// byte *pBlockEnd = WriteIkErrors( srcanim, pIkData );
    /// ...
    /// panimdesc[i].animblockikruleindex = IsInt24( pIkData - g_animblock[..].start );
    /// ```
    ///
    /// 所以它的值**恰好等于该动画载荷 `ALIGN4` 之后的字节数**
    /// （再加上 `animindex`）。
    ///
    /// ⚠️ 那个 `ALIGN4` **是无条件的** —— `WriteIkErrors` 里
    /// `pData += numikrules * sizeof(*pikruledata);` 之后紧跟
    /// `ALIGN4(pData)`（`write.cpp:864-865`），**即使 `numikrules == 0`
    /// 也会走一遍**，所以载荷末尾照样要补齐到 4 的倍数。
    ///
    /// 受控实验（`docs/_probe/cmp_ikrule_block.js`）：
    ///
    /// | 用例 | `numframes` | 载荷 | `align4(载荷)` | `animblockikruleindex` |
    /// |---|---|---|---|---|
    /// | `abi1` | 4 | 60 | 60 | **60** |
    /// | `abi6` | 8 | 76 | 76 | **76** |
    /// | `abi7` | 5 | 66 | **68** | **68** |
    /// | `abi8`（**无** `ikrule` 命令） | 5 | 66 | **68** | **0**（但块长仍是 68） |
    /// | `abi5`（`noautoik`，无规则） | 4 | 60 | 60 | **0** |
    ///
    /// `abi7`/`abi8` 是**唯一**能区分「直接追加」与「补齐后追加」的用例
    /// （只有它们的载荷不是 4 的倍数）；`abi8` 进一步证明补齐**无条件**。
    ///
    /// 与 `ikruleindex` 一样，它是 animdesc **内部的相对偏移**，
    /// 所以 `numikrules == 0` 时**必须写 0**（语料 13134/13134，0 例外）。
    pub anim_block_ikrule_offset: Vec<Option<usize>>,
    /// **块形态的段表**：每条动画的段在**块内**的偏移（不分段时为 `None`）。
    ///
    /// 内联形态的段表由 [`Self::section_table_offsets`] 描述（相对 `anim_data`）；
    /// 块形态下段表**仍在 `.mdl`**（`sectionindex` 相对 animdesc 自身），
    /// 但每条段条目的 `animindex` 是**相对块起点**的偏移 —— 就是本字段。
    ///
    /// 实测（`absec1`）：`sec[k] = (animblock, offs[k])`。
    pub anim_block_section_offsets: Vec<Option<Vec<usize>>>,
    /// **seq 子表区**（紧跟 seqdesc 数组之后）：每条序列的
    /// events / blend / iklock / keyvalue / posekey 等子表。
    ///
    /// 这些表在文件里是**连续排布的一整块**，由各 seqdesc 的
    /// `*index` 字段以**相对 seqdesc 自身**的偏移指向。
    pub seq_subtables: Vec<u8>,
    /// 每条序列的 blend 表在 [`Self::seq_subtables`] 内的偏移。
    pub blend_offsets: Vec<usize>,
    /// 每条序列的 event 表在 [`Self::seq_subtables`] 内的偏移（无 events 时为 `None`）。
    pub event_offsets: Vec<Option<usize>>,
    /// 每条序列的 iklock 表在 [`Self::seq_subtables`] 内的偏移。
    pub iklock_offsets: Vec<usize>,
    /// 每条序列的 keyvalue 表在 [`Self::seq_subtables`] 内的偏移。
    pub keyvalue_offsets: Vec<usize>,
    /// 每条序列的 **weightlist** 在 [`Self::seq_subtables`] 内的偏移。
    ///
    /// 官方为**每条序列**都写这张表（`g_numbones` 个 `float`），
    /// 且内容与更早的块相同时**复用**（`write.cpp:556-595`）——
    /// 语料 11170 条序列里 7639 条复用、3531 条新建。
    ///
    /// ⚠️ 早先本实现把它当「空表」写 `SEQDESC_SIZE`，是**错的**：
    /// 引擎用 `pseqdesc->pBoneweight(0)` 取权重并**按骨骼下标读
    /// `g_numbones` 个 float**，空表会让它读到相邻子表的数据。
    pub weightlist_offsets: Vec<usize>,
    /// 每条序列的 posekey 表在 [`Self::seq_subtables`] 内的偏移。
    ///
    /// 只在 `groupsize[0] > 1 || groupsize[1] > 1` 时存在
    /// （`write.cpp:449`）；否则该轴写 0（**不是** `SEQDESC_SIZE` ——
    /// 官方 `memset` 过 seqdesc，未赋值字段留 0）。
    pub posekey_offsets: Vec<Option<usize>>,
    /// 每条序列的 autolayer 表在 [`Self::seq_subtables`] 内的偏移。
    pub autolayer_offsets: Vec<usize>,
    /// 每根骨骼的 `(posscale[3], rotscale[3])` —— 要写进 **bone 表**。
    pub bone_scales: Vec<([f32; 3], [f32; 3])>,
    /// 待回填的事件名：`(子表区内字段偏移, 名字)`。
    ///
    /// 名字字符串进字符串池，位置只有 `write_mdl` 知道；调用方需把它
    /// 写成「相对该事件自身」的偏移。
    pub event_name_patches: Vec<(usize, String)>,
    pub stats: Vec<SequenceStat>,
}

impl AnimWriteOutcome {
    /// seq 子表区的总字节数。
    pub fn seq_subtable_bytes(&self) -> usize {
        self.seq_subtables.len()
    }
}

/// 该模型会产出多少条 `mstudioanimdesc_t`（= `numlocalanim`）。
///
/// `mdl_writer` 需要在写出动画**之前**算出 `anim_data` 的绝对位置
/// （分段的 `ALIGN16` 要按绝对位置算），而那个位置依赖 animdesc 数量。
/// 这里暴露出来避免把 `anim_specs` 的逻辑复制一份。
pub fn anim_specs_len(compiled: &CompiledModelDesc) -> usize {
    anim_specs(compiled).len()
}

/// 写**一条**动画链（`mstudio_rle_anim_t` 序列 + 链尾 4 字节零记录）。
///
/// `range` 限定**本链覆盖的帧区间（两端含）**：
/// - `None` = **空段** —— 不写任何骨骼记录，只写 `numbones` 条
///   `ff 00 00 00` 占位（实测 `sf120` 的两条尾部空段就是常量链）。
/// - `Some((lo, hi))` = 用 `channels` 的 `[lo, hi]` 帧切片重建链。
///
/// # 为什么每段要**重建**而不是切链
///
/// 实测 `sf600`（nf=600/sf=30）：段 0–19 是 `ANIMROT`(0x8)，段 20–21 是
/// `RAWROT2`(0x20) —— 说明「常量 vs 变化」的判定、`RAWROT2`/`ANIMROT`/
/// `ANIMPOS` 的选择都是**逐段独立**重算的，不能先切链再复用整条动画的判定。
///
/// # payload 顺序（不能改）
///
/// ```c
/// pData() = this + 4
/// pQuat64() = pData()                          // RAWROT2
/// pPos()    = pData() + RAWROT*6 + RAWROT2*8   // RAWPOS
/// pRotV()   = pData()                          // ANIMROT
/// pPosV()   = pData() + ANIMROT*6              // ANIMPOS
/// ```
///
/// **关键**：`pPosV()` 只按 `ANIMROT` 是否置位来偏移 6 字节，所以两个
/// valueptr **必须紧挨着**，中间不能插任何东西。正确顺序是
/// 「常量载荷 → rotV → posV → 各轴的流」。
///
/// **段表条目数** —— `floor(nframes / sectionframes) + 2`。
///
/// # ⚠️ 必须用**动画自己**的帧数，不能用引用它的序列的
/// `Sequence::num_sections`
///
/// 段表是**逐 animdesc** 的，而 `sectionframes` / `numframes` 也都是
/// animdesc 的字段。但同一条动画可能被**帧数不同**的序列引用
/// （`$animation` 定义一次、多条 `$sequence` 复用），而
/// `Sequence::num_sections` 是按**该序列**的 `frames.len()` 算的
/// （`compile.rs:2639`）。
///
/// 两者一旦不等，段表就会**短写**，后面的条目落到链数据里 —— 读出来是垃圾。
///
/// # 实测（`v_dual_pistola` 的 R 变体）
///
/// ```text
/// $sequence "idle" { "a_run" }        ← 72 帧  ⟹ num_sections = 72/30+2 = 4
/// $animation "a_idle_1" …             ← 230 帧 ⟹ 引擎需要 230/30+2 = 9 条
/// ```
///
/// mdlc 修前给 `a_idle_1` 只写 **4** 条，官方 **9** 条：
///
/// ```text
/// 官方 a_idle_1: {0,4084} {0,10732} {0,17512} {0,23780} {0,30604} {0,37708} {0,44508} {0,50940} {0,56192}
/// mdlc 修前    : {0,4036} {0,9700}  {0,15332} {0,20668} {1188096,-6815748} {1610611455,1064618} …
///                                                       ^^^^^^^^^^^^^^^^^^ 垃圾（链数据被当成段表）
/// ```
///
/// 引擎读 `pSection(4)` 会拿到乱码 `animindex` ⟹ 整段动画解不出来。
///
/// ⚠️ **这个 bug 在 S 变体上恰好隐身**：那里 `idle` 直接引用 `a_idle_1`，
/// 序列与动画帧数相同 ⟹ `num_sections` 恰好正确。
/// **「同一个值有两个来源」时，只有在两者不等时才暴露。**
fn section_count(nframes: usize, sf: i32) -> usize {
    if sf <= 0 || nframes == 0 {
        0
    } else {
        nframes / sf as usize + 2
    }
}

/// Inclusive source-frame range for one SDK animation section. Normal
/// sections retain one lookahead frame for interpolation. studio.cpp's pAnim
/// redirects the final frame to n/sf + 1 whenever n > sf, including exact
/// multiples; leaving that section empty makes the last pose snap to bind.
fn section_frame_range(k: usize, _n_sec: usize, n: usize, sf: i32) -> Option<(usize, usize)> {
    if n == 0 || sf <= 0 {
        return None;
    }
    let sf = sf as usize;
    // ⚠️ 用**引擎自己的公式**算末段下标（`section = nf/sf + 1`），
    // 而不是用 `n_sec - 1` —— 后者依赖调用方把 `num_sections` 算成
    // `nf/sf + 2`，一旦两边口径漂移就会静默错位。
    let last_sec = n / sf + 1;
    if n > sf && k == last_sec {
        return Some((n - 1, n - 1));
    }
    let lo = k * sf;
    let hi = ((k + 1) * sf).min(n - 1);
    if lo <= hi { Some((lo, hi)) } else { None }
}

/// **本段**的旋转常量载荷（`RAWROT2` 要写的绝对值）。
///
/// # 为什么必须逐段算，不能用整条动画的结论
///
/// 官方 `WriteAnimationData`（`write.cpp:715`）是**逐段**调用的，而它的判据
///
/// ```c
/// if (srcanim->numanim[j][3] >= srcanim->numframes && …)   // :774
/// AngleQuaternion( srcanim->sanim[0][j].rot, q );          // :756
/// ```
///
/// 里的 `numframes` 与 `sanim[0]` **都是本段的**。于是：
///
/// | 段 | 帧数 | 旋转恒定？ | 官方编码 |
/// |---|---|---|---|
/// | `k < nf/sf` | `sf+1` | 取决于数据 | `ANIMROT` / `RAWROT2` |
/// | **末段**（末帧单独一段） | **1** | **恒真** | **`RAWROT2`** |
///
/// # 实测（`v_dual_pistola` 的 `a_idle_1`，末段）
///
/// ```text
/// 官方 sec8：81 条记录  flags 分布 {0x21: 49, 0x20: 31, 0x1: 1}
/// mdlc 修前：81 条记录  flags 分布 {0x21: 5, 0x20: 20, 0x1: 3, 0x4: 38, 0x0: 11, 0x24: 4}
/// ```
///
/// 官方**每一根有旋转的骨骼都是 `RAWROT2`**（`0x20`/`0x21`），而 mdlc 因为
/// 沿用了整条动画「旋转不恒定」的结论、又够不到 `ANIMROT`（要求 `n > 1`），
/// 于是**旋转整个丢失** ⟹ 末帧 48/119 根骨骼跳变（最大 **101.79°**）。
fn section_rot_const(
    raw_rot: Option<&Vec<[f32; 3]>>,
    lo: usize,
    hi: usize,
    delta: bool,
    ref_rot: Option<&[f32; 3]>,
) -> Option<[f32; 3]> {
    let sub = raw_rot?.get(lo..=hi)?;
    let first = sub.first()?;
    // ⚠️ 判据是**原始差值逐帧精确相等**（不是量化后）—— 与 R20 同一口径。
    if !sub.iter().all(|r| r == first) {
        return None;
    }
    Some(if delta {
        // `DELTA` 动画的 `sanim` 本身就是增量，引擎也不加参考 ⟹ 绝对值 == 差值。
        *first
    } else {
        let r = ref_rot?;
        [0, 1, 2].map(|k| wrap_to_pi(first[k] + r[k]))
    })
}

/// 写一条**段链**所需的「整条动画级」上下文。
///
/// 打包成结构体是为了让参数个数可控，也让「哪些量是整条动画的、
/// 哪些是本段的」在类型上一眼可见。
struct ChainCtx<'a> {
    /// 逐骨骼的**原始旋转差值帧**（未量化）—— 逐段重算旋转常量用。
    raw_rot_per_bone: Option<&'a BoneFrameTable>,
    /// 逐骨骼的参考姿态 `(pos, rot)` —— 常量载荷要烘进参考姿态。
    ref_rots: Option<&'a [([f32; 3], [f32; 3])]>,
}

#[allow(clippy::too_many_arguments)]
fn write_one_chain(
    anim_data: &mut Vec<u8>,
    channels: &[BoneChannels],
    bone_count: usize,
    _n: usize,
    range: Option<(usize, usize)>,
    rot_scale: &[[f32; 3]],
    _pos_scale: &[[f32; 3]],
    delta: bool,
    anim_data_abs: usize,
    ctx: &ChainCtx<'_>,
) -> Result<(), AnimWriteError> {
    let raw_rot_per_bone = ctx.raw_rot_per_bone;
    let ref_rots = ctx.ref_rots;
    // Unused sections retain frame-independent channels. Reachable final-frame
    // sections always have a range, even at exact section-length multiples.
    //
    // `ff 00 00 00` 占位只用于**整条动画都没有可写骨骼**的情形
    // （如静态道具：`localseqindex − anim_data_off == 4`，
    // 语料 2681/2681）—— 那时 `bone_count` 条占位、**不写链尾终止符**。
    let Some((lo, hi)) = range else {
        // 空段：把「常量」骨骼照写，`Sampled` 轴退化为无数据。
        let const_only: Vec<BoneChannels> = channels
            .iter()
            .map(|ch| {
                let c = |a: &AxisData| match a {
                    AxisData::Sampled(_) => AxisData::Absent,
                    other => other.clone(),
                };
                BoneChannels {
                    rot: [c(&ch.rot[0]), c(&ch.rot[1]), c(&ch.rot[2])],
                    pos: [c(&ch.pos[0]), c(&ch.pos[1]), c(&ch.pos[2])],
                    pos_const_raw: ch.pos_const_raw,
                    pos_delta_is_const: ch.pos_delta_is_const,
                    // 旋转常量载荷照用：`c` 只把 `Sampled` 降级成 `Absent`，
                    // 不改变「该常量是什么」。
                    rot_const_raw: ch.rot_const_raw,
                }
            })
            .collect();
        let any = (0..bone_count).any(|b| !const_only[b].is_empty());
        if !any {
            // 连常量都没有 → 占位、无终止符（静态道具形态）。
            for _ in 0..bone_count {
                anim_data.extend_from_slice(&[0xFF, 0x00, 0x00, 0x00]);
            }
            return Ok(());
        }
        return write_chain_body(
            anim_data,
            &const_only,
            bone_count,
            1,
            rot_scale,
            delta,
            anim_data_abs,
        );
    };

    // 该段的帧切片（两端含）。
    let slice = |a: &AxisData| -> AxisData {
        match a {
            AxisData::Sampled(q) => {
                let sub: Vec<i16> = q[lo..=hi].to_vec();
                fold_axis(sub)
            }
            other => other.clone(),
        }
    };
    let seg_channels: Vec<BoneChannels> = channels
        .iter()
        .enumerate()
        .map(|(b, ch)| BoneChannels {
            rot: [slice(&ch.rot[0]), slice(&ch.rot[1]), slice(&ch.rot[2])],
            pos: [slice(&ch.pos[0]), slice(&ch.pos[1]), slice(&ch.pos[2])],
            // 分段只切「逐帧采样」的轴；`Constant`/`Absent` 原样保留
            // （`slice` 对它们走 `other.clone()`）。所以整条位移若原本恒定，
            // 切出来的每一段也恒定，常量位移可以照用。
            //
            // ⚠️ **位移不做逐段重算** —— 实测把它也改成「本段差值恒定
            // ⟹ `RAWPOS`」会**引入回归**：多帧段里官方仍写 `ANIMPOS`
            // （`@deploy_layer` sec0 官方 `0x1c`、误改后 `0x14`），
            // 姿态差从 0 涨到 **345**。位移的判据是整条动画的
            // `pos_delta_is_const`，只有**单帧末段**例外 —— 而那个例外
            // 已由 [`section_frame_range`] 把末帧单独切出来天然满足。
            pos_const_raw: ch.pos_const_raw,
            pos_delta_is_const: ch.pos_delta_is_const,
            // ⚠️ **旋转常量载荷必须逐段重算**（见 [`section_rot_const`]）。
            // 沿用整条动画的结论会让「单帧末段」丢掉旋转 ——
            // 那段够不到 `ANIMROT`（要求 `n > 1`），而整条动画的
            // `rot_const_raw` 又常因其它段不恒定而为 `None`。
            rot_const_raw: section_rot_const(
                raw_rot_per_bone.and_then(|t| t.get(b)).map(|pair| &pair.0),
                lo,
                hi,
                delta,
                ref_rots.and_then(|t| t.get(b)).map(|(_, r)| r),
            )
            .or(ch.rot_const_raw),
        })
        .collect();

    let n = hi - lo + 1;
    write_chain_body(
        anim_data,
        &seg_channels,
        bone_count,
        n,
        rot_scale,
        delta,
        anim_data_abs,
    )
}

/// 写一条链的**记录体**（逐骨骼记录 + 链尾 4 字节零终止符）。
///
/// 由 [`write_one_chain`] 调用两次：一次用于正常帧切片，一次用于**空段**
/// （此时只写「与帧无关」的常量骨骼）。
///
/// # 旋转/位移的编码选择
///
/// | 情形 | 旋转 | 位移 |
/// |---|---|---|
/// | 多帧 + 旋转恒定非 0 | `RAWROT2`(0x20) | — |
/// | 多帧（其余） | **无条件** `ANIMROT`(0x8) | `ANIMPOS`(0x4) |
/// | 单帧 | `RAWROT`(0x2) | `RAWPOS`(0x1) |
///
/// ⚠️ **多帧时 `ANIMROT` 无条件置位**（`write.cpp:782-815` 先分配 `rotvptr`
/// 再无条件 `flags |= STUDIO_ANIM_ANIMROT`），即使三个旋转轴都没有数据。
/// 早先 mdlc 只在「真有旋转数据」时置位 → 旋转恒定但位移在动的骨骼
/// 写成 `0x4`，官方是 `0xc`（受控实验 `sfw120` 实测）。
fn write_chain_body(
    anim_data: &mut Vec<u8>,
    seg_channels: &[BoneChannels],
    bone_count: usize,
    n: usize,
    rot_scale: &[[f32; 3]],
    delta: bool,
    anim_data_abs: usize,
) -> Result<(), AnimWriteError> {
    let animated: Vec<usize> = (0..bone_count)
        .filter(|b| !seg_channels[*b].is_empty())
        .collect();

    if animated.is_empty() {
        // 连常量都没有 → `numbones` 条占位、**不写**链尾终止符
        // （静态道具形态：语料 2681/2681 `localseqindex − anim_data_off == 4`）。
        for _ in 0..bone_count {
            anim_data.extend_from_slice(&[0xFF, 0x00, 0x00, 0x00]);
        }
        return Ok(());
    }

    // 本链的起点 —— 链尾对齐以它为基准（见下面链尾处的说明）。
    let chain_start = anim_data.len();

    for (idx, &b) in animated.iter().enumerate() {
        let ch = &seg_channels[b];
        let rec_start = anim_data.len();

        let const_rot = ch.constant_rot();
        let rot_angle = const_rot.map(|q| {
            [
                q[0] * rot_scale[b][0],
                q[1] * rot_scale[b][1],
                q[2] * rot_scale[b][2],
            ]
        });
        // ⚠️ **`RAWROT2` 的两个条件必须来自同一个判据。**
        //
        // 早先这里只判「量化后的常量非零」：
        // ```rust
        // let use_rawrot2 = rot_angle.is_some_and(|a| a != [0.0; 3]);
        // ```
        // 而**载荷** `rot_const_raw` 是在 `ch.rot` 构造处按「原始差值逐帧
        // **精确浮点相等**」填的（见 `write_animations` 里
        // `rot_frames.iter().all(|r| *r == *first)`）。两个判据在
        // 「原始差值有极小抖动、但量化后落在同一个整数」时分道扬镳 ——
        // 于是 `flags` 置了 `RAWROT2`（声明后面有 8 字节 `Quaternion64`），
        // 却**一个字节都没写**。
        //
        // 实测 `v_silenced_smg` `a_run` `b40 ValveBiped.Bip01_R_Finger2`：
        // 原始 `rot[1]` 逐帧抖动 `1.2e-3°`（约半个量化步长），量化后恒定
        // （复刻官方 `simplify.cpp:6545-6591` 得 `numanim=[2,2,2]`）；
        // mdlc 写 `flags=0x20` 但 `nextoffset=4`。引擎 `pQuat64()` 读到的是
        // **下一条记录的 4 字节头**，拼出的四元数 `x²+y²+z² = 1.505 > 1`
        // ⟹ `w = 0` ⟹ 退化成一个 **180°** 旋转。
        //
        // 判据（`probe_chain_dump.js`，同一骨骼同一动画）：
        //
        // | | `flags` | `nextoffset` | 旋转载荷 |
        // |---|---|---|---|
        // | 官方 | `0x8`（`ANIMROT`） | **72** | 三轴 RLE |
        // | mdlc（修前） | `0x20`（`RAWROT2`） | **4** | **无** ❌ |
        //
        // 官方那条 `ANIMROT` 的三轴：axis0 恒定（`1463`）、axis1/axis2
        // 各差 1 个量化单位 ⟹ **官方要求三轴都恒定才用 `RAWROT2`**。
        //
        // 所以判据加上 `rot_const_raw.is_some()`：原始差值不精确恒定就
        // 退回 `ANIMROT` —— 既消除了「有标志无载荷」的自相矛盾，
        // 也与官方在该骨骼上的选择一致。
        //
        // ⚠️ **位移侧没有这个问题**：`pos_delta_is_const` 与
        // `pos_const_raw` 在 `write_animations` 里由**同一个 `if`** 同时置位
        // （`pos_frames.iter().all(|t| *t == *first)`），天然自洽。
        // 旋转侧是「判据用 `ch.rot`（量化后）、载荷用 `rot_const_raw`
        // （原始）」两套来源 ⟹ 又一处**不对称路径** bug（HANDBOOK §45.28）。
        // ⚠️ 这只是**候选** —— `RAWROT2` 与 `ANIMPOS` 同址互斥，最终判定
        // 还要看位移放不放得下（见下方 `use_rawrot2`）。
        let rawrot2_ok = ch.rot_const_raw.is_some() && rot_angle.is_some_and(|a| a != [0.0; 3]);
        // ⚠️ **旋转完全无数据时不能写 `ANIMROT`。**
        //
        // 早先这里是 `use_animrot = !use_rawrot2 && n > 1` —— 只看帧数，
        // 不看旋转是否真有数据。于是「三轴都 `Absent`」的骨骼也会被写一条
        // 空的 `ANIMROT` 记录。实测 miku `bone 0`：官方 `0x1`（只有
        // `RAWPOS`），mdlc 写成 `0x9`（`ANIMROT|RAWPOS`）。
        //
        // 正确判据与 `RAWROT2` 互补：旋转**有数据**（三轴不全 `Absent`）
        // 且不是常量 → `ANIMROT`。这也与 `write.cpp:736-740` 一致 ——
        // `numanim` 六轴全 0 的骨骼**直接 `continue`**，根本不进链。
        let rot_all_absent = ch.rot.iter().all(|a| matches!(a, AxisData::Absent));
        let pos_has_data = !ch.pos_all_absent();
        let pos_can_const = ch.pos_delta_is_const && pos_has_data;

        // ⚠️ **`RAWROT2` 与 `ANIMPOS` 同址互斥** —— `RAWPOS`/`ANIMROT` 之外
        // 的**第二条**同构冲突，mdlc 早先漏了（`studio.h:574-586`）：
        // ```c
        // pQuat64() = pData()                 // **永远**在 pData()
        // pPosV()   = pData() + ANIMROT*6     // **不含** RAWROT2！
        // ```
        // `RAWROT2` 成立时 `ANIMROT` 必为假 ⟹ `pPosV() = pData()+0`，与
        // `pQuat64()` **指向同一处**；引擎把 `Quaternion64` 的 8 字节当成
        // `mstudioanim_valueptr_t.offset[3]` 读。
        //
        // 实测 mdlc 产出 **50 条** `0x24`/`0x34`（`a_idle_1` 4 条、
        // `a_run` 9 条，共 13 个动画）；官方 studiomdl 与 NekoMDL 在全部
        // 40 个动画里各 **0 条**。引擎口径解码（`docs\_probe\diag_engine_decode.js`）
        // 的位移偏差最大 **1.62e+2**（`a_idle_1` `bone[75] tag_weapon_left`），
        // 而对照组（官方 vs NekoMDL）全动画仅 `6.77e-3`。
        //
        // 让位方向由官方实测决定（`docs\_probe\diag_flags4way.js`，50/50 条）：
        // * 位移**是**常量 ⟹ `RAWPOS` 放得下（`pPos() = pData()+8`），
        //   旋转保持 `RAWROT2` → `0x21`（29 条）。
        // * 位移**不是**常量 ⟹ 只能 `ANIMPOS`，旋转让位降级成 `ANIMROT`
        //   → `0x0c`（21 条）。
        let use_rawrot2 = rawrot2_ok && (pos_can_const || !pos_has_data);
        let use_animrot = !use_rawrot2 && !rot_all_absent;
        // ⚠️ **位移与旋转对称**：位移三轴**都不随时间变化**时用常量
        // `RAWPOS`（`Vector48`，6 字节），**与帧数无关**。
        //
        // 早先这里写的是 `use_rawpos = n == 1 && use_animpos` —— 把
        // 「单帧」当成了 `RAWPOS` 的唯一条件。那是从 `hl2sdk-darkm` 的
        // `write.cpp:750`（`if (srcanim->numframes == 1)`）推出来的，
        // 但那份源码**比 L4D2 实际用的 studiomdl 旧**：
        //
        // * 语料 3302 个官方产物里，**多帧**记录出现 `RAWPOS` 的
        //   有 3186 + 2631 + 240 + 202 = 6259 条（`probe_track_flags_rule.js`），
        //   不可能是「只出现在单帧上」。
        // * 真 `studiomdl.exe` 编译 miku：`bone 0` 的 121 帧动画
        //   （`a_idle.smd` 里根骨骼的 pos/rot **121 帧全同**）
        //   官方写 `0x1`（只有 `RAWPOS`），mdlc 写 `0xc`。
        //
        // ⚠️ **判据是「原始位移逐帧同值」，不是「量化后同值」。**
        //
        // 这两个在 miku 上分道扬镳，而且它是**唯一**能区分两者的样本：
        //
        // | 骨骼 | SMD 原始位移 | 量化后 | 官方 |
        // |---|---|---|---|
        // | `bone 0` | 121 帧全同 | 常量 | **`RAWPOS`**（`0x1`） |
        // | `bone 12` | **只有末帧**不同（差 `7.4e-5`） | 也常量（都截断成同一整数） | **`ANIMPOS`**（`0xc`） |
        //
        // `bone 12` 的原始差 `7.4e-5` 小于一个 `posscale`（`3.9e-3`），
        // 量化后确实恒定 —— 但官方仍写 `ANIMPOS`。说明官方看的是
        // **未量化**的 `sanim[0..n][j].pos` 是否逐帧相同
        // （`write.cpp:774` 比较的正是 `srcanim->numanim[j][k] >= numframes`，
        // 即「每帧都有采样点」）。
        //
        // 早先用「量化后恒定」判，会把 `bone 12`/`31`/`60` 写成 `RAWPOS`
        // （`0x9`），而官方是 `0xc` —— 一个真实回归。
        // ⚠️ **判据用「差值是否恒定」，载荷用「绝对值」。**
        //
        // 这两件事必须分开，否则会互相污染：
        //   * `use_rawpos` 要判的是「`ANIMPOS` 的流会不会退化成常量」
        //     —— 那是**差值** `(sanim − ref)` 的性质。
        //   * `RAWPOS` 的**载荷**要写**绝对值** `sanim[0].pos`
        //     （`write.cpp:765`），根骨骼还经过 yaw +90° 旋转。
        //
        // 早先把两者混为一谈：`use_rawpos` 拿绝对值判「非零」，
        // 于是**参考位移非零、差值恒为 0** 的骨骼被误判成需要 `RAWPOS`
        // （实测轨道不符记录从 112 暴涨到 1318）。
        //
        // ⚠️ **`RAWPOS` 与 `ANIMROT` 互斥** —— 二者都从 `pData()` 取址。
        //
        // `studio.h:580,586`：
        // ```c
        // pRotV() = pData()                          // **永远**在 pData()
        // pPos()  = pData() + RAWROT*6 + RAWROT2*8   // **不含** rotV！
        // ```
        // 两者同时置位时 `pRotV()` 与 `pPos()` **指向同一处**，必有一方
        // 被解成垃圾。官方因此从不这么写：语料 3333 个 `.mdl`、44978 条
        // **通过校验**的记录里，`RAWPOS|ANIMROT`（`rot=0x8 pos=0x1`）
        // 出现 **0 次**（`probe_track_layout_strict.js`）。
        //
        // 实测代价：miku `a_run` 的 `bone 12`，官方 `flags=0x0c`
        // （`ANIMROT|ANIMPOS`），mdlc 曾写 `0x09` 并把 `RAWPOS` 塞在
        // `rotV` 之前 —— 解码差 **3.62 rad**。修好后该记录
        // **逐字节相同**（`0c 0c e8 00 0c 00 50 00 94 00 d2 00 d6 00 da 00`）。
        //
        // 退让顺序（**两条**同址冲突都要让）：
        //   * `RAWPOS` vs `ANIMROT` —— 位移让位给旋转（旋转没有常量替代编码，
        //     位移有 `ANIMPOS`）。
        //   * `RAWROT2` vs `ANIMPOS` —— 已在上面 `use_rawrot2` 处解掉：
        //     位移不是常量时 `RAWROT2` 主动降级为 `ANIMROT`。
        let use_rawpos = pos_can_const && !use_animrot;
        let use_animpos = !use_rawpos && pos_has_data;
        anim_data.push(b as u8);
        let mut flags = 0u8;
        // `subtract` 出来的动画带 `STUDIO_DELTA`（`write.cpp:744-748`）：
        //
        // ```c
        // if (srcanim->flags & STUDIO_DELTA)
        //     destanim->flags |= STUDIO_ANIM_DELTA;
        // ```
        //
        // 引擎据此把增量乘回 base（`simplify.cpp:57-67` 的注释）。
        // 实测官方 `blend1.mdl` 的 `look_down` 是 `0x30`（`RAWROT2|DELTA`）。
        if delta {
            flags |= STUDIO_ANIM_DELTA;
        }
        if use_rawrot2 {
            flags |= STUDIO_ANIM_RAWROT2;
        } else if use_animrot {
            flags |= STUDIO_ANIM_ANIMROT;
        }
        if use_rawpos {
            flags |= STUDIO_ANIM_RAWPOS;
        } else if use_animpos {
            flags |= STUDIO_ANIM_ANIMPOS;
        }
        // 结构不变式：两条同址冲突都不得出现（见 `docs\animation-layout.md` §2.5.1）。
        //
        // 这个断言守的是**一整类** bug，而不是某两个具体组合 —— 上面两条
        // 同址冲突（`RAWPOS|ANIMROT`、`RAWROT2|ANIMPOS`）各自都有对称的
        // 让位方向，历史上正是「只堵了一条、漏了同构的另一条」让这个缺陷
        // 躲过五轮排查（`RAWROT2|ANIMPOS` 在 40 个动画里产出 50 条记录，
        // 引擎读到的位移偏差达 1.62e+2）。
        debug_assert!(
            !(flags & STUDIO_ANIM_RAWPOS != 0 && flags & STUDIO_ANIM_ANIMROT != 0),
            "同址冲突 ①：RAWPOS 与 ANIMROT 都取 pData()+0（flags={flags:#04x}）"
        );
        debug_assert!(
            !(flags & STUDIO_ANIM_RAWROT2 != 0 && flags & STUDIO_ANIM_ANIMPOS != 0),
            "同址冲突 ②：pPosV() 不含 RAWROT2 ⟹ 与 pQuat64() 同址（flags={flags:#04x}）"
        );
        anim_data.push(flags);
        let next_pos = anim_data.len();
        anim_data.extend_from_slice(&0i16.to_le_bytes());

        // `RAWROT2` 的**载荷**必须是**绝对**旋转，不是差值 ——
        // 引擎对 `RAWROT2` **逐字返回**、不加参考旋转
        // （`bone_setup.cpp:367-372`；而 `ANIMROT` 的流**会**加，
        // 见 `:398-406`）。详见 [`BoneChannels::rot_const_raw`]。
        //
        // ⚠️ **判据仍用差值**（`rot_angle`）：要不要写 `RAWROT2` 取决于
        // 「差值是否恒定且非零」（`write.cpp:736-740` 的 `numanim` 全 0 跳过），
        // 而**写什么**取决于绝对旋转。两者是不同的量 —— 混用会让
        // 「差值恒 0 但参考旋转非 0」的骨骼少转一个参考旋转
        // （实测根骨骼 `b0` 少转 **90°**）。
        if let Some(a) = ch.rot_const_raw.filter(|_| use_rawrot2) {
            let q = crate::bone_math::angle_quaternion(a);
            anim_data.extend_from_slice(&encode_quaternion64(q));
        }
        if let Some(t) = ch.pos_const_raw.filter(|_| use_rawpos) {
            anim_data.extend_from_slice(&encode_vector48(t));
        }
        let rot_ptr_pos = if use_animrot {
            let p = anim_data.len();
            anim_data.extend_from_slice(&[0u8; VALUEPTR_SIZE]);
            Some(p)
        } else {
            None
        };
        let pos_ptr_pos = if use_animpos {
            let p = anim_data.len();
            anim_data.extend_from_slice(&[0u8; VALUEPTR_SIZE]);
            Some(p)
        } else {
            None
        };
        if let Some(p) = rot_ptr_pos {
            for k in 0..3 {
                write_axis(anim_data, p, k, &ch.rot[k], n)?;
            }
        }
        if let Some(p) = pos_ptr_pos {
            for k in 0..3 {
                write_axis(anim_data, p, k, &ch.pos[k], n)?;
            }
        }

        // 回填 nextoffset（相对自身）。最后一条写 0。
        let is_last = idx + 1 == animated.len();
        if !is_last {
            let next_start = anim_data.len();
            let rel = i16::try_from(next_start - rec_start)
                .map_err(|_| AnimWriteError::Internal(crate::tr("nextoffset overflows i16")))?;
            anim_data[next_pos..next_pos + 2].copy_from_slice(&rel.to_le_bytes());
        }
    }
    // 链尾的终止记录（4 字节全零）。
    //
    // 官方 `WriteAnimationData`（`write.cpp:715-855`）的结构是：
    //
    // ```c
    // mstudioanim_t *destanim = (mstudioanim_t *)pData;
    // pData += sizeof(*destanim);          // ① 进入时先留一条「头」
    // for (j = 0; j < g_numbones; j++) {
    //     if (没有动画) continue;           // 跳过时**不**推进 destanim
    //     destanim->bone = j;
    //     ... 写载荷 ...
    //     prevanim             = destanim;
    //     destanim->nextoffset = pData - (byte *)destanim;
    //     destanim             = (mstudioanim_t *)pData;
    //     pData                += sizeof(*destanim);   // ② 再留一条
    // }
    // if (prevanim) prevanim->nextoffset = 0;
    // ```
    //
    // ① 那条「头」被**第一根**骨骼记录覆盖；② 在最后一根之后再留一条 ——
    // 循环结束后 `prevanim` 指向最后一条真实记录（它的 `nextoffset` 被置 0），
    // ② 留的那条就是链尾终止记录。
    //
    // # 终止记录的**位置**：`ALIGN8(载荷末尾 + 4)`
    //
    // `pData` 在每条记录写完后停在**载荷末尾**（未必对齐），
    // 而下一条记录的 4 字节头必须落在 **8** 的倍数上。
    //
    // 两个**互相独立**的判据（载荷末尾都恰好是 38，最能区分 4 与 8）：
    //
    // | 来源 | 载荷末尾 | `ALIGN4` | `ALIGN8` | 官方实测链长 |
    // |---|---|---|---|---|
    // | 受控实验 `blend1` anim[0] | 38 | 44 | **48** | **48** |
    // | 语料 `pre_destruction_tanker_trailer.mdl` anim[1] | 38 | 44 | **48** | **48** |
    //
    // 第二行是**真实商业模型**，与受控实验完全独立 —— 两者同一结论。
    // 另有 `playerstart.mdl` anim[0]（载荷末尾 18）：`ALIGN8(22) = 24`，
    // 实测链长 24 ✓；`blend1` anim[1]（末尾 12）：`ALIGN8(16) = 16` ✓。
    //
    // ⚠️ 早先本实现只写终止记录、**不做这个对齐**，于是载荷末尾不是
    // 8 的倍数时整条链短 4 字节（`blend1` anim[0] 实测 44 vs 官方 48）。
    //
    // ⚠️ 对齐基准是**链起点**（`chain_start`）而不是 `anim_data.len()` ——
    // 用后者会得到「已对齐、不用补」的错误结论。
    //
    // 规则：`span = ALIGN8(载荷末尾 + 4)`，即终止记录落在
    // `span - 4` 处（`blend1` anim[0]：载荷末尾 38 → span 48 → 终止 @44）。
    let payload_end = anim_data.len() - chain_start;
    let span = (payload_end + ANIM_HEADER_SIZE).div_ceil(8) * 8;
    let pad = span - ANIM_HEADER_SIZE - payload_end;
    anim_data.extend(std::iter::repeat_n(0u8, pad));
    anim_data.extend_from_slice(&[0u8; ANIM_HEADER_SIZE]);
    // 链区按 **`ALIGN4`** 补齐（`write.cpp:953` 的 `ALIGN4( pData )`）。
    //
    // ⚠️ **基准是文件绝对位置，不是 `anim_data` 内的偏移。**
    //
    // 官方 `pData` 是文件指针，`ALIGN4(pData)` 对的是绝对地址。早先这里
    // 用 `anim_data.len() % 4`，只有当 `anim_data_abs` 恰好是 4 的倍数时
    // 才等价 —— 而 `anim_data_abs` 现在是 **`ALIGN16`**，永远是 4 的倍数，
    // 所以两者其实等价。保留绝对位置写法是为了与官方逐字对应，
    // 也防止将来 `anim_data_abs` 的对齐改了之后这里静默失效。
    let abs_end = anim_data_abs + anim_data.len();
    let pad = (4 - abs_end % 4) % 4;
    anim_data.extend(std::iter::repeat_n(0u8, pad));
    Ok(())
}

/// 为整个模型生成动画数据。
///
/// # 参数
///
/// - `compiled` —— 编译结果（含已读入的逐帧姿态）
/// - `bone_parents[i]` —— 骨骼 i 的父下标（-1 为根），用于判定「根骨骼 Z +90°」
/// - `ref_poses[i]` —— 骨骼 i 的**参考姿态** `(position, rotation)`，
///   必须与写进 `mstudiobone_t` 的值**完全一致**
///
/// # 为什么必须显式传参考姿态
///
/// 存储值是「规范化欧拉角 − 参考姿态」（见 [`delta_frames`]），而参考姿态
/// 可能来自 `$definebone` 覆盖、而非 SMD 第 0 帧。若这里自己从
/// `seq.frames[0]` 取，遇到 `$definebone` 就会与骨骼表不一致 ——
/// 解码后整个动画会偏移一个常量，且**不报任何错**。
/// 一个「动画」的描述 —— 与序列**解耦**。
///
/// # 为什么需要它
///
/// 通常 `anims.len() == sequences.len()` 且一一对应（`animdesc[i]` 属于
/// `seqdesc[i]`）。但 `$staticprop` 打破了这条对应：
/// `MakeStaticProp()` 把动画压成 **1 条 1 帧**（`simplify.cpp:3374-3380`）：
///
/// ```cpp
/// // throw away all animations
/// g_numani = 1;
/// g_panimation[0]->numframes = 1;
/// g_panimation[0]->startframe = 0;
/// g_panimation[0]->endframe = 1;
/// g_panimation[0]->rotation = RadianEuler( 0, 0, 0 );
/// g_panimation[0]->adjust = Vector( 0, 0, 0 );
/// ```
///
/// 它**不动** `g_sequence`，所以 `numlocalseq` 保持原值 ——
/// 实测 `ipe2`（2 条序列 + `$staticprop`）得到 `numlocalanim=1`、
/// `numlocalseq=2`；语料 `smalldebris_part_baked_setsexp.mdl` 更是
/// **1 个 animdesc 对 5 个 seqdesc**。
///
/// `rotation` 归零还有第二个后果：根骨骼 Z 轴的 **+90° 偏置**与 `π/2`
/// 的 scale 下限都不再适用。实测语料 **2681/2681** 个静态道具的
/// `rotscale` 三轴都是 `π/8/32767`，而不是普通模型的 `[π/8, π/8, π/2]/32767`。
struct AnimSpec {
    /// 引用该动画的**第一条序列**的下标（只用于诊断/错误消息）。
    seq_index: usize,
    /// **没有任何序列引用这条动画**（孤儿 `$animation`）。
    ///
    /// # 为什么必须显式记下来
    ///
    /// [`Self::seq_index`] 在找不到引用序列时会 `unwrap_or(0)` 落到
    /// `seq[0]` —— 那个值只该用于诊断，**不能**用来取 `section_frames`
    /// 之类的语义字段。实测 `v_dual_pistola_processed.qc`：
    /// `look_neutral` 只被 `$animation` 声明、没有任何序列引用它，
    /// 于是继承了 `seq[0]`（`idle`）的 `section_frames=30`，
    /// 而官方按**全局** `$sectionframes` 判 `nf=1 >= 36` 不成立 ⟹ `sf=0`。
    orphan: bool,
    /// **动画池**下标（= animdesc 下标）。见
    /// [`crate::model::CompiledModelDesc::animations`]。
    anim_index: usize,
    /// 帧数。
    frames: usize,
    fps: f32,
    looping: bool,
    /// 是否是 `$staticprop` 的合成身份动画（存储值恒 0）。
    identity: bool,
}

impl AnimSpec {
    fn weights<'a>(&self, compiled: &'a CompiledModelDesc) -> &'a [f32] {
        let animation = &compiled.animations[self.anim_index];
        if !animation.weights.is_empty() {
            return &animation.weights;
        }
        compiled
            .sequences
            .get(self.seq_index)
            .filter(|_| !self.orphan)
            .map_or(&[], |sequence| sequence.weights.as_slice())
    }

    fn movements<'a>(&self, compiled: &'a CompiledModelDesc) -> &'a [crate::model::Movement] {
        if self.identity {
            return &[];
        }
        let animation = &compiled.animations[self.anim_index];
        if !animation.movements.is_empty() {
            return &animation.movements;
        }
        compiled
            .sequences
            .get(self.seq_index)
            .filter(|sequence| !self.orphan && sequence.cells.first() == Some(&self.anim_index))
            .map_or(&[], |sequence| sequence.movements.as_slice())
    }

    /// 该动画的**逐帧姿态**。
    ///
    /// # ⚠️ 必须优先读**序列**的帧，不能只读动画池
    ///
    /// `$sequence` 级的 `subtract "<动画>" <帧>`（`simplify.cpp:163-166` 的
    /// `CMD_SUBTRACT`）在 `compile.rs` 里作用于**该序列自己的帧副本**
    /// （`CompiledSequence::frames`），**不写回**共享的
    /// `compiled.animations[].frames`（那样会让「同一动画被两条序列引用」时
    /// 减除被叠加两次）。
    ///
    /// 早先这里读 `compiled.animations[self.anim_index].frames` ⟹
    /// **序列级 `subtract` 的结果对写出器完全不可见**，动画按**未减除**的
    /// 绝对姿态写出。
    ///
    /// # 实测症状（`v_snip_awp`，6/6 个官方 viewmodel 都受影响）
    ///
    /// `@awm_fire_layer` 的 `b7`：
    ///
    /// | | `RAWROT2` 反解欧拉 |
    /// |---|---|
    /// | 官方 | `[0.00°, 0.00°, 0.00°]` ← **差值**（减除生效） |
    /// | mdlc（修前） | `[-179.99°, 0.20°, -175.49°]` ← **SMD 原始绝对值** |
    ///
    /// 于是 30~51 根骨骼本该「减除后恒 0 ⟹ 官方不写记录」，
    /// mdlc 却把它们按原值写进了链 ⟹ 这些层**整体错位**。
    ///
    /// 判据：`probe_anim_bones.js` 报「多写 2669 条记录」（153 条动画）。
    ///
    /// # 只在「该序列单格引用本动画」时才用序列帧
    ///
    /// blend 序列（`blend_width > 1`）的**每个格是独立动画**，
    /// `CompiledSequence::frames` 只存**第一格** —— 用它会让其余格全错。
    /// 所以只在 `cells == [anim_index]`（本序列只引用这一个动画）时走序列帧；
    /// 其余情况退回动画池（blend 的 `subtract` 走 `animations[].subtract`，
    /// 已经写回了动画池）。
    fn cell_frames<'a>(&self, compiled: &'a CompiledModelDesc) -> &'a [Vec<crate::smd::SmdPose>] {
        compiled
            .sequences
            .get(self.seq_index)
            .filter(|s| s.cells.as_slice() == [self.anim_index])
            .map(|s| s.frames.as_slice())
            .unwrap_or_else(|| compiled.animations[self.anim_index].frames.as_slice())
    }

    /// `usesource` 要用的帧：**减除之前**的原始 SMD 姿态。
    ///
    /// # 官方语义（`simplify.cpp:6007-6012` 等四处）
    ///
    /// `usesource` 走 `BuildRawTransforms(panim->source, …)`，读的是
    /// `psource->rawanim[frame]` —— SMD 里**原样**的姿态，
    /// **跳过 `subtract` 减除**。
    ///
    /// # ⚠️ 它在「权重恒为 1」时是 no-op（已实测定案）
    ///
    /// 对 DELTA 动画，`CalcBoneTransforms`（默认路径）会**重建**：
    ///
    /// ```c
    /// q3 = q1 + s * q2;   // q1 = base 动画第 0 帧，q2 = 本帧的增量
    /// p3 = base.pos[0] + s * panimation->sanim[frame][k].pos;
    /// ```
    ///
    /// `s = panimation->weight[k]` 来自 `$weightlist`（缺省全 1）。
    /// `s == 1` 时 `base + 1·delta == raw`，**与 `usesource` 完全相同**。
    ///
    /// 受控实验（`docs/_probe/ab_iksrc2.js`）：
    ///
    /// | 条件 | 官方产物差异 |
    /// |---|---|
    /// | 无 `$weightlist`（s=1） | **0 处** |
    /// | 有 `$weightlist`（s=0.5） | **15 处**（全在压缩误差载荷的 RLE 通道里） |
    ///
    /// mdlc 不实现 `$weightlist`（权重恒 1），所以本函数目前**总是**返回
    /// 与 [`Self::cell_frames`] 等价的内容 —— 即 `use_source` 在本实现里
    /// 是**语义正确但当前无可观测效果**的字段。留着它是为了：
    ///
    /// 1. 让 TOML 能**表达** `usesource`（原先写了会硬报错）；
    /// 2. `$weightlist` 一旦实现，这里自动生效。
    fn source_frames<'a>(&self, compiled: &'a CompiledModelDesc) -> &'a [Vec<crate::smd::SmdPose>] {
        let a = &compiled.animations[self.anim_index];
        // 没有 `subtract` 时 `pre_subtract_frames` 是 `None`，
        // 此时 `frames` 本身就是源姿态。
        a.pre_subtract_frames.as_deref().unwrap_or(&a.frames)
    }

    /// 该动画写进 animdesc 的**名字**（取自动画池）。
    ///
    /// ⚠️ blend 的格子用**源动画名**（`a_run` / `look_down`），
    /// **不是** `@序列名` —— 实测官方 `idle` 的三格叫
    /// `a_run` / `a_idle` / `a_run`，而单动画序列 `reload` 叫 `@reload`。
    /// 这两者现在都由动画池的 `name` 承载（隐含动画的名字已带 `@`）。
    ///
    /// 名字的最终归属由 [`anim_name_sources`] 统一给出（`mdl_writer`
    /// 消费它），这里只暴露「本动画的名字」供诊断使用。
    #[allow(dead_code)]
    fn cell_name<'a>(&self, compiled: &'a CompiledModelDesc) -> &'a str {
        &compiled.animations[self.anim_index].name
    }
}

/// 一个 animdesc 的**名字来源**。
///
/// 两者会分叉，所以必须显式区分（见 [`anim_name_sources`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimNameSource {
    /// 用 `@` + **该序列**的名字（下标是**序列**下标，不是 animdesc 下标）。
    Sequence(usize),
    /// 用这个字面量（**不带** `@` 前缀）。
    Literal(String),
}

/// 按 **animdesc 下标**列出每个 animdesc 的名字来源。
///
/// # 为什么需要它
///
/// `seqdesc` 与 `animdesc` 的下标在三种情况下会分叉：
///
/// 1. **`$staticprop`**：只有 1 个 animdesc，却可能有多条序列 ——
///    实测官方取 `seq[0]` 的名字（2681/2681）。
/// 2. **blend 序列**：一个 seqdesc 引用多格，每格一个**共享** animdesc，
///    名字是源动画名（实测 `idle` 三格叫 `a_run` / `a_idle` / `a_run`）。
/// 3. 普通序列：名字是 `@` + 序列名（隐含动画）。
///
/// 之所以在这里给出、而不是让 `mdl_writer` 自己按
/// `sequences[].cells` 重算一遍：重算就是**同一规则的第二份实现**，
/// 而它一旦与 `anim_specs` 的顺序分叉，症状是「动画数据对、
/// 名字错」这种不会报错的静默损坏。
pub fn anim_name_sources(compiled: &CompiledModelDesc) -> Vec<AnimNameSource> {
    if compiled.is_static_prop() {
        // 只有 1 个 animdesc（也可能 0 条序列）。
        return if compiled.sequences.is_empty() {
            Vec::new()
        } else {
            vec![AnimNameSource::Sequence(0)]
        };
    }
    compiled
        .animations
        .iter()
        .map(|a| AnimNameSource::Literal(a.name.clone()))
        .collect()
}

/// 按 [`CompiledModelDesc::anim_count`] 列出要写出的动画。
///
/// `$staticprop` → 只有 1 条，取第 0 条序列的 fps / looping，帧数固定 1，
/// 且姿态是身份（存储值恒 0）。
/// 其余 → **与动画池一一对应**（`compiled.animations` 的顺序就是
/// animdesc 的顺序）。
fn anim_specs(compiled: &CompiledModelDesc) -> Vec<AnimSpec> {
    if compiled.is_static_prop() {
        return match compiled.sequences.first() {
            Some(s) => vec![AnimSpec {
                seq_index: 0,
                orphan: false,
                anim_index: 0,
                frames: 1,
                fps: s.fps,
                looping: s.looping,
                identity: true,
            }],
            None => Vec::new(),
        };
    }
    compiled
        .animations
        .iter()
        .enumerate()
        .map(|(i, a)| AnimSpec {
            // 反查「哪条序列引用了它」只为取诊断用的名字；动画池的顺序
            // 与序列无关，所以这里取**第一条引用它的序列**。
            //
            // ⚠️ 找不到时 `unwrap_or(0)` 只是为了有个合法下标 ——
            // **语义字段必须看 `orphan`**，不能拿 `seq[0]` 的值顶替。
            seq_index: compiled
                .sequences
                .iter()
                .position(|s| s.cells.contains(&i))
                .unwrap_or(0),
            orphan: !compiled.sequences.iter().any(|s| s.cells.contains(&i)),
            anim_index: i,
            frames: a.frames.len(),
            fps: a.fps,
            looping: a.looping,
            identity: false,
        })
        .collect()
}

/// 一条 IK 规则在写出前的**中间**形态：全部是**帧号**（尚未除以
/// `numframes − 1`），下标也已解析好。
#[derive(Debug, Clone)]
struct PendingIkRule {
    /// `type` 常量（1/3/4/5/6）。
    type_code: i32,
    chain: i32,
    bone: i32,
    slot: i32,
    height: f32,
    radius: f32,
    floor: f32,
    pos: [f32; 3],
    q: [f32; 4],
    start: i32,
    peak: i32,
    tail: i32,
    end: i32,
    contact: i32,
    attachment: String,
    /// `usesource`：误差样本取**源 SMD 的原始骨骼变换**。
    ///
    /// ⚠️ **只对 `IK_SELF`(1) / `IK_ATTACHMENT`(5) / `IK_GROUND`(3) 有效。**
    /// `IK_RELEASE`(4) / `IK_UNLATCH`(6) 在官方那里直接 `break`
    /// （`simplify.cpp:6245-6247`），`pError` 是 `calloc` 的全 0 ——
    /// `usesource` 写了也不改变任何字节。
    ///
    /// 实测：miku 的真实 QC 有 28 处 `usesource`，其中 **24 处在 `release`
    /// 上（无效果）**、**4 处在 `touch` 上（有效果）**。
    use_source: bool,
    /// 误差样本。`None` = 自动补的规则（官方 `numerror == 0`，
    /// `CompressIKErrors` 直接 `continue`，`compressedikerrorindex` 留 0）。
    errors: Option<Vec<([f32; 3], [f32; 4])>>,
}

/// 构造一条动画的全部 IK 规则（显式 + 自动补的 `IK_RELEASE`）。
///
/// # 官方流程（`ProcessIKRules`，`simplify.cpp:5819-6336`）
///
/// 1. `5828-5841`：按 `panim->cmds[]` 顺序把**显式**规则拷进来；
/// 2. `5843-5954`：逐条展开 `range`（补 `tail`/`end`、插值 `peak`、
///    回绕修正）+ `contact == -1 → peak`；
/// 3. `5955-6249`：逐条算 `numerror` 与 `pError[]`（**只对显式规则**）；
/// 4. `6254-6281`：把「没有任何显式规则、且末端骨骼权重 > 0」的链
///    **追加**一条 `IK_RELEASE`（所以数组里显式在前、自动在后）。
///
/// 自动规则**不经过**第 2/3 步 —— 它直接写 `start=0, peak=0,
/// tail=end=numframes-1`，且 `numerror == 0`（`calloc` 出来的 0），
/// 所以永远没有压缩载荷（实测 20711/20851 条 `type == 4` 无载荷）。
fn build_ik_rules(
    compiled: &CompiledModelDesc,
    spec: &AnimSpec,
    bone_parents: &[i32],
) -> Result<Vec<crate::model::ResolvedIkRule>, AnimWriteError> {
    // `$staticprop` 把骨骼塌缩成一根，`$ikchain` 引用的骨骼不再存在；
    // 官方那条路径也不可能产生规则（`MakeStaticProp` 之后没有 ikchain）。
    if compiled.is_static_prop() {
        return Ok(Vec::new());
    }
    let chains = &compiled.desc.ikchains;
    if chains.is_empty() {
        return Ok(Vec::new());
    }
    let seq = &compiled.sequences[spec.seq_index];
    let num_frames = spec.frames;
    let bone_index = compiled.desc.bone_index();
    // 每条链的**末端**骨骼（= `g_ikchain[j].link[2].bone`）。
    let link2: Vec<i32> = chains
        .iter()
        .map(|c| {
            bone_index
                .get(c.bone.as_str())
                .map(|v| *v as i32)
                .unwrap_or(-1)
        })
        .collect();

    // ---- 1. 显式规则 ----
    //
    // ⚠️ **规则属于「动画」，不属于「序列」。**
    //
    // 官方 `ProcessIKRules`（`simplify.cpp:5819-5841`）遍历的是
    // **`g_panimation[]`**（动画池），从 `panim->cmds[]` 里取
    // `CMD_IKRULE`。QC 里把 `ikrule` 写在 `$sequence` 块内，只是因为
    // `$sequence` 会触发 `Cmd_ImpliedAnimation` 建一个隐含动画，
    // 那条命令被记到**那个隐含动画**上。
    //
    // 所以这里读 [`CompiledAnimation::ik_rules`] 而不是
    // `sequences[..].ik_rules` —— 后者是**已声明动画**（`$animation` 块）
    // 上的规则，与隐含动画是两回事。实测差异：把规则写在序列上时，
    // 池里另一条 1 帧动画（如 subtract 的参考动画 `base`）也会去
    // 校验同一条规则，于是 `range 0 1 1 2` 会误报「越界帧」。
    let anim = &compiled.animations[spec.anim_index];
    let mut rules: Vec<PendingIkRule> = Vec::with_capacity(anim.ik_rules.len());
    for r in &anim.ik_rules {
        let chain = chains
            .iter()
            .position(|c| c.name == r.chain)
            .ok_or_else(|| AnimWriteError::IkRule {
                sequence: seq.name.clone(),
                message: crate::tr_fmt!(
                    "unknown ikchain name %{chain}",
                    chain = format!("{:?}", r.chain)
                ),
            })? as i32;
        if r.radius.is_some() && r.pad.is_some() {
            return Err(AnimWriteError::IkRule {
                sequence: seq.name.clone(),
                message: crate::tr(
                    "radius and pad are mutually exclusive (the official pad is radius/2)",
                ),
            });
        }
        // 初值（`s_ikrule_t` 是 `calloc` 出来的，所以全 0）：
        //   `bone = 0`、`slot = chain`、`contact = -1`。
        let mut p = PendingIkRule {
            type_code: r.kind.code(),
            chain,
            bone: 0,
            slot: r.target.unwrap_or(chain),
            height: r.height.unwrap_or(0.0),
            radius: r.radius.or(r.pad.map(|v| v / 2.0)).unwrap_or(0.0),
            floor: r.floor.unwrap_or(0.0),
            pos: [0.0; 3],
            q: [0.0; 4],
            start: 0,
            peak: 0,
            tail: 0,
            end: 0,
            contact: -1,
            attachment: r.attachment.clone().unwrap_or_default(),
            use_source: r.use_source,
            errors: None,
        };
        if let Some(rg) = r.range {
            // QC 里写 `.` 的元素 = −1。写成 `None` 才表达得出来。
            let get = |v: Option<i32>| v.unwrap_or(0);
            p.start = get(rg[0]);
            p.peak = get(rg[1]);
            p.tail = get(rg[2]);
            p.end = get(rg[3]);
            if rg[0].is_none() || rg[3].is_none() {
                // `FindPrevIKRule` / `FindNextIKRule` 的插值（5887-5936）
                // 会**反向修改邻居规则**，本实现未复刻。宁可报错也不要
                // 静默写出一条与官方不同的曲线。
                return Err(AnimWriteError::IkRule {
                    sequence: seq.name.clone(),
                    message: crate::tr_fmt!(
                        "the 1st/4th entry of `range` is `.`, which needs the official FindPrev/NextIKRule interpolation; not implemented yet (chain %{chain})",
                        chain = format!("{:?}", r.chain)
                    ),
                });
            }
        }
        if let Some(c) = r.contact {
            p.contact = c;
        }
        match r.kind {
            crate::model::IkRuleType::Touch => {
                p.bone = match r.bone.as_deref() {
                    // `strlen(bonename) == 0 → bone = -1`（`simplify.cpp:5987`）。
                    None | Some("") => -1,
                    Some(name) => *bone_index.get(name).ok_or_else(|| AnimWriteError::IkRule {
                        sequence: seq.name.clone(),
                        message: crate::tr_fmt!(
                            "`touch` references a bone that does not exist: %{bone}",
                            bone = format!("{name:?}")
                        ),
                    })? as i32,
                };
            }
            crate::model::IkRuleType::Attachment => {
                if p.attachment.is_empty() {
                    return Err(AnimWriteError::IkRule {
                        sequence: seq.name.clone(),
                        message: crate::tr("an attachment rule must name the attachment"),
                    });
                }
                // `simplify.cpp:6057-6063`：`bonename` 为空时
                // `bone`（初值 0 ≠ −1）被改写成链的末端骨骼。
                p.bone = link2[chain as usize];
            }
            crate::model::IkRuleType::Release | crate::model::IkRuleType::Unlatch => {
                // `bone` 保持初值 0（实测 20851/20851 条 `type == 4` 的 `bone` 都是 0）。
            }
            crate::model::IkRuleType::Footstep => {
                let c = &chains[chain as usize];
                p.height = r.height.or(c.height).unwrap_or(18.0);
                p.floor = r.floor.or(c.floor).unwrap_or(0.0);
                p.radius = r
                    .radius
                    .or(r.pad.map(|v| v / 2.0))
                    .or(c.pad.map(|v| v / 2.0))
                    .unwrap_or(0.0);
            }
        }
        if let Some(o) = r.fake_origin {
            p.pos = o;
            p.bone = -1;
        }
        if let Some(a) = r.fake_rotate {
            // `AngleQuaternion`：输入是**角度**（`studiomdl.cpp:1359-1372`）。
            p.q = crate::bone_math::angle_quaternion([
                a[0].to_radians(),
                a[1].to_radians(),
                a[2].to_radians(),
            ]);
            p.bone = -1;
        }
        rules.push(p);
    }

    // ---- 2. `ProcessIKRules` 的展开（`simplify.cpp:5847-5954`）----
    let nf = num_frames as i32;
    for p in &mut rules {
        if p.start == 0 && p.peak == 0 && p.tail == 0 && p.end == 0 {
            p.tail = nf - 1;
            p.end = nf - 1;
        }
        if p.start != -1 && p.peak == -1 && p.tail == -1 && p.end != -1 {
            p.peak = (p.start + p.end) / 2;
            p.tail = (p.start + p.end) / 2;
        }
        if p.start != -1 && p.peak == -1 && p.tail != -1 {
            p.peak = (p.start + p.tail) / 2;
        }
        if p.peak != -1 && p.tail == -1 && p.end != -1 {
            p.tail = (p.peak + p.end) / 2;
        }
        if p.peak == -1 {
            p.start = 0;
            p.peak = 0;
        }
        if p.tail == -1 {
            p.tail = nf - 1;
            p.end = nf - 1;
        }
        if p.contact == -1 {
            p.contact = p.peak;
        }
        // 回绕修正（C 的整数除法向零截断，Rust 的 `/` 对 i32 同样是向零
        // 截断，所以上面那几处 `(a + b) / 2` 逐位一致）。
        if p.peak < p.start {
            p.peak += nf - 1;
        }
        if p.tail < p.peak {
            p.tail += nf - 1;
        }
        if p.end < p.tail {
            p.end += nf - 1;
        }
        if p.contact < p.start {
            p.contact += nf - 1;
        }
    }

    // ---- 3. 误差样本（只对显式规则）----
    //
    // 传给 `&mut` 是因为 `IK_ATTACHMENT` 会**顺带写回** `pos`/`q`
    // （`simplify.cpp:6076-6077` 在算误差之前就改了规则本身）。
    let parents = bone_parents;
    for p in &mut rules {
        let link2 = link2.clone();
        compute_ik_errors(compiled, spec, parents, p, &link2)?;
    }

    // ---- 4. 自动补 `IK_RELEASE`（`simplify.cpp:6254-6281`）----
    //
    // ⚠️ **`STUDIO_DELTA` 的动画一条都不补。** 判据在
    // `simplify.cpp:6251-6252`：
    //
    // ```c
    // if ((panim->flags & STUDIO_DELTA) || panim->noAutoIK)
    //     continue;
    // ```
    //
    // 实测官方 `look_down`（`subtract` 产生、`flags=0x04`）的
    // `numikchains` 是 **0**，而同文件的 `@reload` 是 2。
    if !anim.no_auto_ik && !anim.delta {
        let mut count = vec![0usize; chains.len()];
        for p in &rules {
            if let Some(c) = count.get_mut(p.chain as usize) {
                *c += 1;
            }
        }
        for (j, c) in count.iter().enumerate() {
            if *c != 0 {
                continue;
            }
            // ⚠️ **还要看该链末端骨骼在本动画里的权重 > 0**
            // （`simplify.cpp:6269`）：
            //
            // ```c
            // if (count[j] == 0 && panim->weight[g_ikchain[j].link[2].bone] > 0.0)
            // ```
            //
            // `link[2]` 是链的**末端骨骼**（`simplify.cpp:6084` 的
            // `g_ikchain[j].link[2].bone`）。权重来自 `$weightlist`。
            //
            // # 为什么这条判据是必需的（实测）
            //
            // `vm_test_group` 的 `v_smg_mp5` 等模型里，
            // `weights_helping_hand_extend` 把**全部 64 根骨骼的权重都设成 0**。
            // 于是 `helping_hand_*` / `item_*` 这 6 条序列在官方产物里
            // `numikrules == 0`（**一条自动规则都不补**），而 mdlc 补了 2 条
            // （`lhand` + `rhand`）。
            //
            // 实测对照（`docs/_probe/probe_mdl_diff_detail.js v_smg_mp5`）：
            // `@helping_hand_extend` 官方 0 / mdlc 2、`@item_retract` 官方 0 / mdlc 2
            // —— **6 个动画 / 6 个模型**受影响。
            //
            // ⚠️ 早先这里的注释写的是「mdlc 不实现 `$weightlist`，所以权重恒为 1」
            // —— **那句话早已过期**：`resolve_weight_lists` 从 §53 起就实现了，
            // `Sequence::weights` 也一直写进了 seqdesc 的 `weightlistindex`。
            // 只有这一处判据没跟上。
            //
            // `link2[j]` 就是上面算好的 `g_ikchain[j].link[2].bone` 下标。
            let w = link2
                .get(j)
                .and_then(|b| usize::try_from(*b).ok())
                .and_then(|b| spec.weights(compiled).get(b))
                .copied()
                .unwrap_or(1.0);
            if w <= 0.0 {
                continue;
            }
            rules.push(PendingIkRule {
                type_code: 4,
                chain: j as i32,
                bone: 0,
                slot: j as i32,
                height: 0.0,
                radius: 0.0,
                floor: 0.0,
                pos: [0.0; 3],
                q: [0.0; 4],
                start: 0,
                peak: 0,
                tail: nf - 1,
                end: nf - 1,
                contact: 0,
                attachment: String::new(),
                // 自动补的规则是 `IK_RELEASE`，而 `usesource` 对它无效
                // （官方 `simplify.cpp:6245-6247` 直接 break）。
                use_source: false,
                errors: None,
            });
        }
    }

    // ---- 5. 帧号 → cycle ----
    //
    // `write.cpp:883-898`：`numframes > 1` 时五个量各自除以 `numframes-1`，
    // 否则写死 `start=0, peak=0, tail=1, end=1, contact=0`。
    //
    // `index` 按 `simplify.cpp:6325-6334`：**只遍历序列**，所以只有被某个
    // `seqdesc` 引用过的动画才拿到 0,1,2…；没被引用的动画恒为 0
    // （实测 `rsrch_ik_index2.js`：全语料 3192 条 `index != j` **全部是 0**，
    // 且全部属于未被引用的动画）。
    //
    // mdlc 里 animdesc 与 seqdesc 一一对应（`$staticprop` 除外，而它没有
    // ikchain），blend 表元素就是 seq 下标，所以**每条动画都被引用**。
    // ⚠️ **`index` 只对「被某条序列引用过」的动画才有意义。**
    //
    // 官方 `SetSequence`（`simplify.cpp:6325-6334`）的双层循环是
    // `for (i = 0; i < g_sequence.Count(); i++)` → `panim[j][k]->ikrule[n].index = n`
    // —— **只遍历序列**。所以：
    //
    // * 被序列引用的动画：`index = 0,1,2…`（规则在动画内的序号）；
    // * **没被任何序列引用的动画**：`index` 保持 `memset` 的 **0**
    //   （全部规则都是 0，不是 0,1,2…）。
    //
    // mdlc 修前对所有动画都写 `i`（序号）⟹ 孤儿动画的第 2 条规则拿到
    // `index = 1`。实测（`v_dual_pistola_processed.qc`）：`a_run` 与
    // `look_neutral` 的 `rule1.index` 官方 **0**、mdlc **1**
    // （`docs/_probe/cmp_mdl_full.js` 报 `animdescs[].ikrules[].index: 0 vs 1`）。
    //
    // 这个字段是**该规则在动画内的下标**，引擎用它把 `ikrule[]` 与
    // 动画槽位对应；写错会让 IK 解算指向错误的槽 ⟹ **骨骼被拽到错误的
    // 目标位置**（HLMV 里表现为骨骼乱跳）。
    //
    // 全语料佐证（`rsrch_ik_index2.js`）：`index == j` 27749 条、
    // `index != j` **3192 条全部是 0**，且全部属于未被引用的动画。
    let index_of = |i: usize| -> i32 { if spec.orphan { 0 } else { i as i32 } };
    Ok(rules
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let (start, peak, tail, end, contact) = if num_frames > 1 {
                let d = (num_frames - 1) as f32;
                (
                    p.start as f32 / d,
                    p.peak as f32 / d,
                    p.tail as f32 / d,
                    p.end as f32 / d,
                    p.contact as f32 / d,
                )
            } else {
                (0.0, 0.0, 1.0, 1.0, 0.0)
            };
            crate::model::ResolvedIkRule {
                index: index_of(i),
                type_code: p.type_code,
                chain: p.chain,
                bone: p.bone,
                slot: p.slot,
                height: p.height,
                radius: p.radius,
                floor: p.floor,
                pos: p.pos,
                q: p.q,
                start_frame: if num_frames > 1 { p.start } else { 0 },
                start,
                peak,
                tail,
                end,
                contact,
                attachment: p.attachment,
                error: p.errors.as_deref().and_then(compress_ik_errors),
            }
        })
        .collect())
}

/// 算一条规则的 `pError[]`（`simplify.cpp:5961-6248`）。
///
/// `numerror = end − start + 1`，`end >= numframes` 时再 **+2**
/// （`simplify.cpp:5961-5963`）。
fn compute_ik_errors(
    compiled: &CompiledModelDesc,
    spec: &AnimSpec,
    parents: &[i32],
    p: &mut PendingIkRule,
    link2: &[i32],
) -> Result<(), AnimWriteError> {
    let seq = &compiled.sequences[spec.seq_index];
    let num_frames = spec.frames;
    let mut numerror = p.end - p.start + 1;
    if p.end >= num_frames as i32 {
        numerror += 2;
    }
    let numerror = numerror.max(0) as usize;

    let mut out = Vec::with_capacity(numerror);
    match p.type_code {
        // `IK_SELF`（`simplify.cpp:5981-6040`）
        1 => {
            let lb = link2[p.chain as usize];
            for k in 0..numerror {
                let world =
                    frame_worlds_for(compiled, spec, parents, p.start + k as i32, p.use_source)?;
                let local = if p.bone < 0 {
                    // `bone == -1` → 直接用链末端骨骼的世界矩阵（6026）。
                    world[lb as usize]
                } else {
                    crate::bone_math::concat(
                        &crate::bone_math::invert(&world[p.bone as usize]),
                        &world[lb as usize],
                    )
                };
                out.push(matrix_to_error(&local));
            }
        }
        3 | 5 => {
            let tip = link2[p.chain as usize] as usize;
            let center = compiled.desc.ikchains[p.chain as usize]
                .center
                .unwrap_or([0.0; 3]);
            if p.type_code == 3 {
                let world = frame_worlds_for(compiled, spec, parents, p.contact, p.use_source)?;
                p.pos = ik_transform_point(&world[tip], center);
                p.pos[2] = p.floor;
                p.q = [0.0, 0.0, 0.0, 1.0];
            } else if p.bone >= 0 {
                let world = frame_worlds_for(compiled, spec, parents, p.contact, false)?;
                let m = world[p.bone as usize];
                p.pos = [m[3], m[7], m[11]];
                p.q = crate::bone_math::matrix_quaternion(&m);
            }
            for k in 0..numerror {
                let t = p.start + k as i32;
                let sample = if p.type_code == 3 && p.use_source {
                    p.contact
                } else {
                    t
                };
                let world = frame_worlds_for(compiled, spec, parents, sample, p.use_source)?;
                let movement = ik_movement(spec.movements(compiled), num_frames, t, p.contact);
                let mut position = std::array::from_fn(|axis| p.pos[axis] + movement[axis]);
                if p.type_code == 3 {
                    let mut current = ik_transform_point(&world[tip], center);
                    current[2] = position[2];
                    let blend = if t < p.start || t >= p.end {
                        1.0
                    } else if t < p.peak {
                        (p.peak - t) as f32 / (p.peak - p.start) as f32
                    } else if t > p.tail {
                        (t - p.tail) as f32 / (p.end - p.tail) as f32
                    } else {
                        0.0
                    };
                    let blend = 3.0 * blend * blend - 2.0 * blend * blend * blend;
                    position = std::array::from_fn(|axis| {
                        position[axis] * (1.0 - blend) + current[axis] * blend
                    });
                }
                let anchor = crate::bone_math::local_transform(
                    position,
                    crate::bone_math::quaternion_angles(p.q),
                );
                let local =
                    crate::bone_math::concat(&crate::bone_math::invert(&anchor), &world[tip]);
                out.push(matrix_to_error(&local));
            }
        }
        // `IK_RELEASE` / `IK_UNLATCH`：`pError` 是 `calloc` 出来的全 0
        // （`simplify.cpp:6245-6247` 什么都不做）→ 载荷是 6 个全 0 通道。
        4 | 6 => out.resize(numerror, ([0.0; 3], [0.0; 4])),
        other => {
            return Err(AnimWriteError::IkRule {
                sequence: seq.name.clone(),
                message: crate::tr_fmt!(
                    "the error computation for type = %{ty} is not implemented yet",
                    ty = other
                ),
            });
        }
    }
    p.errors = Some(out);
    Ok(())
}

fn ik_transform_point(matrix: &crate::bone_math::Matrix3x4, point: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| {
        matrix[axis * 4 + 3]
            + (0..3)
                .map(|column| matrix[axis * 4 + column] * point[column])
                .sum::<f32>()
    })
}

pub(crate) fn ik_movement(
    movements: &[crate::model::Movement],
    frames: usize,
    from: i32,
    to: i32,
) -> [f32; 3] {
    let position = |frame: i32| {
        if movements.is_empty() || frames <= 1 || frame < 0 {
            return [0.0; 3];
        }
        let period = (frames - 1) as i32;
        let loops = frame / period;
        let frame = frame % period;
        let mut start = 0;
        let mut base = [0.0; 3];
        for movement in movements {
            if frame <= movement.endframe {
                let time = (frame - start) as f32 / (movement.endframe - start) as f32;
                let distance = movement.v0 * time + 0.5 * (movement.v1 - movement.v0) * time * time;
                let end = movements.last().unwrap().position;
                return std::array::from_fn(|axis| {
                    base[axis] + distance * movement.vector[axis] + loops as f32 * end[axis]
                });
            }
            start = movement.endframe;
            base = movement.position;
        }
        base
    };
    let a = position(from);
    let b = position(to);
    std::array::from_fn(|axis| b[axis] - a[axis])
}

/// `CalcBoneTransforms(panim, frame, boneToWorld)`（`simplify.cpp:4533`）——
/// 取某一帧的世界矩阵，含官方的**帧号回绕 / 越界报错**。
///
/// `LOOPING` 且 `numframes > 1` 时 `while (frame >= numframes-1) frame -= numframes-1`
/// （`simplify.cpp:4541-4547`）；否则越界是硬错误（`Error()`）。
///
/// # 三条路径（与官方一一对应）
///
/// | 官方分支 | 条件 | 本函数 |
/// |---|---|---|
/// | `BuildRawTransforms(panim->source, …)` | `pRule->usesource` | `use_source = true` |
/// | `CalcBoneTransforms` 的 `STUDIO_DELTA` 重建 | 动画带 `subtract` | `delta = true` |
/// | `CalcBoneTransforms` 的 `AngleMatrix` 直通 | 其余 | 默认 |
///
/// ⚠️ `usesource` 走的是**源 SMD 的原始姿态**，**不做** DELTA 重建 ——
/// 它绕开的正是 `subtract`。两者互斥，顺序与官方 `if/else if/else`
/// （`simplify.cpp:6003-6016`）一致。
fn frame_worlds_for(
    compiled: &CompiledModelDesc,
    spec: &AnimSpec,
    parents: &[i32],
    frame: i32,
    use_source: bool,
) -> Result<Vec<crate::bone_math::Matrix3x4>, AnimWriteError> {
    let seq = &compiled.sequences[spec.seq_index];
    let num_frames = spec.frames;
    let mut f = frame;
    if spec.looping && num_frames > 1 {
        let period = (num_frames - 1) as i32;
        while f >= period {
            f -= period;
        }
    }
    if f < 0 || f as usize >= num_frames {
        return Err(AnimWriteError::IkRule {
            sequence: seq.name.clone(),
            message: crate::tr_fmt!(
                "ikrule asked for the out-of-range frame %{frame} (the animation %{name} has %{frames} frames and is not LOOPING)",
                frame = f,
                name = format!("{:?}", seq.name),
                frames = num_frames
            ),
        });
    }
    if use_source {
        let frames = spec.source_frames(compiled);
        // ⚠️ `spec.frames` 来自**动画池**，而 `source_frames`/`cell_frames`
        // 可能来自**序列**（帧行可能更短）⟹ 上面的范围检查不足以保证可索引。
        // 取不到时退回最后一帧（官方 `CalcBoneTransforms` 对越界帧
        // 会 clamp 到 `numframes - 1`）。
        let row = frames.get(f as usize).or_else(|| frames.last());
        let Some(row) = row else {
            return Err(AnimWriteError::IkRule {
                sequence: seq.name.clone(),
                message: crate::tr_fmt!(
                    "the source frame table is empty when ikrule takes frame %{frame} (%{name})",
                    frame = f,
                    name = format!("{:?}", seq.name)
                ),
            });
        };
        return Ok(crate::compile::frame_worlds(&compiled.desc, parents, row));
    }
    let frames = spec.cell_frames(compiled);
    let row = frames.get(f as usize).or_else(|| frames.last());
    let Some(row) = row else {
        return Err(AnimWriteError::IkRule {
            sequence: seq.name.clone(),
            message: crate::tr_fmt!(
                "the frame table is empty when ikrule takes frame %{frame} (%{name})",
                frame = f,
                name = format!("{:?}", seq.name)
            ),
        });
    };
    // `STUDIO_DELTA` 动画必须**重建**（官方 `simplify.cpp:4562-4578`）。
    // 直接喂增量会让所有骨骼塌到原点 —— 见
    // [`crate::compile::delta_local_matrices`] 的实测判据。
    if compiled.animations[spec.anim_index].delta {
        let base = base_animation_frame0(compiled);
        return Ok(crate::compile::delta_frame_worlds_weighted(
            &compiled.desc,
            parents,
            &base,
            row,
            spec.weights(compiled),
        ));
    }
    Ok(crate::compile::frame_worlds(&compiled.desc, parents, row))
}

/// `g_panimation[0]->sanim[0]` —— DELTA 重建的**基准帧**。
///
/// 官方 `CalcBoneTransforms(panimation, frame, …)`（`simplify.cpp:4533-4536`）
/// 固定传 `g_panimation[0]`，即 QC 里**第一条** `$animation`
/// （`studiomdl.cpp:2409-2413` 的分配顺序）—— **不是** `subtract`
/// 引用的那一条。`subtract` 的参考动画可以是任意一条，两者不可混用。
///
/// 动画池为空时返回空切片 —— 此时 `frame_worlds` 会退化成单位姿态，
/// 与官方对空池的 `g_panimation[0]` 解引用相比是**更安全**的行为
/// （官方那条路径在池为空时本来就是未定义行为，实际不可达：
/// 能走到 IK 规则就说明至少有一条序列，而序列必然建出一条动画）。
fn base_animation_frame0(compiled: &CompiledModelDesc) -> Vec<crate::smd::SmdPose> {
    compiled
        .animations
        .first()
        .and_then(|a| a.frames.first())
        .cloned()
        .unwrap_or_default()
}

/// `MatrixAngles(local, q, pos)` —— 同时取平移列与四元数。
fn matrix_to_error(m: &crate::bone_math::Matrix3x4) -> ([f32; 3], [f32; 4]) {
    ([m[3], m[7], m[11]], crate::bone_math::matrix_quaternion(m))
}

/// 官方 `CompressIKErrors`（`simplify.cpp:6626-6783`）的 6 通道压缩。
///
/// `None` = 六个通道全空（`numerror == 0`）→ 整条载荷跳过，
/// `compressedikerrorindex` 留 0。
pub(crate) fn compress_ik_errors(
    errors: &[([f32; 3], [f32; 4])],
) -> Option<crate::model::CompressedIkError> {
    if errors.is_empty() {
        return None;
    }
    let mut scale = [0.0f32; 6];
    let mut channels: [Vec<u8>; 6] = Default::default();

    for k in 0..6 {
        // 初值就是「极值」的起点（`simplify.cpp:6648-6657`）。
        let (mut minv, mut maxv) = if k < 3 {
            (-IK_ERROR_POS_LIMIT, IK_ERROR_POS_LIMIT)
        } else {
            (-IK_ERROR_ROT_LIMIT, IK_ERROR_ROT_LIMIT)
        };

        // 逐样本取值（同时求极值）。
        let mut raw: Vec<f32> = Vec::with_capacity(errors.len());
        for (pos, q) in errors {
            let v = if k < 3 {
                pos[k]
            } else {
                // `QuaternionAngles(q, ang); v = ang[k-3];` 再回绕到 [−π, π)。
                let mut v = crate::bone_math::quaternion_angles(*q)[k - 3];
                while v >= std::f32::consts::PI {
                    v -= std::f32::consts::PI * 2.0;
                }
                while v < -std::f32::consts::PI {
                    v += std::f32::consts::PI * 2.0;
                }
                v
            };
            if v < minv {
                minv = v;
            }
            if v > maxv {
                maxv = v;
            }
            raw.push(v);
        }

        let s = if minv < maxv {
            if -minv > maxv {
                minv / -32768.0
            } else {
                maxv / 32767.0
            }
        } else {
            1.0 / 32.0
        };
        scale[k] = s;

        // `value[n] = v / scale`（截断到 short；C 的 `float→short` 转换是
        // **向零截断**，Rust 的 `as i16` 对 f32→i16 同样是饱和 + 向零截断，
        // 值域已由 scale 保证不溢出）。
        let values: Vec<i16> = raw.iter().map(|v| (v / s) as i16).collect();
        channels[k] = rle_encode(&values);
    }

    Some(crate::model::CompressedIkError { scale, channels })
}

/// 官方的 RLE 编码（`simplify.cpp:6734-6777`）。
///
/// 一条 run 是一个 `{valid: u8, total: u8}` 头 + `valid` 个 `i16` 采样，
/// 语义是「这些采样后面再重复 `total − valid` 次最后一个采样」。
///
/// **逐字复刻**（不是「够用就行」的等价实现）—— 否则与官方产物逐字节
/// 对照时，同一段误差会编码成不同的合法字节串。
fn rle_encode(values: &[i16]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    if values.is_empty() {
        return out;
    }
    // 当前 run 的 `{valid,total}` 头在 `out` 中的位置。
    let mut head = 0usize;
    let mut valid: u16 = 1;
    let mut total: u16 = 1;
    out.push(1);
    out.push(1);
    out.extend_from_slice(&values[0].to_le_bytes());

    for m in 1..values.len() {
        if total == 255 {
            // 链太长，强制开新记录。
            head = out.len();
            out.push(0);
            out.push(0);
            valid = 1;
            total = 0;
            out.extend_from_slice(&values[m].to_le_bytes());
        } else if values[m] != values[m - 1]
            || (total == valid && m < values.len() - 1 && values[m] != values[m + 1])
        {
            if total != valid {
                // 当前 run 有重复尾巴，先把它封口。
                head = out.len();
                out.push(0);
                out.push(0);
                total = 0;
                valid = 0;
            }
            valid += 1;
            out.extend_from_slice(&values[m].to_le_bytes());
        }
        total += 1;
        out[head] = valid as u8;
        out[head + 1] = total as u8;
    }
    out
}

/// 把一条动画的 IK 规则块写进 `anim_data`（官方 `WriteIkErrors`，
/// `write.cpp:858-960`）。
///
/// # 字节布局
///
/// ```text
/// 规则数组起点            : N × 152 字节 mstudioikrule_t
/// ALIGN4                  : 第 0 条「有载荷」规则的 mstudiocompressedikerror_t
///                           ├ float scale[6]  (24B @+0)
///                           └ short offset[6] (12B @+24)   ← offset[0] == 36 恒
///                           6 个 RLE 通道块**背靠背**
///                           可选 NUL 结尾的 attachment 字符串（**内联**，
///                           注释原文 "don't use string table, we're probably
///                           not in the same file"）
/// ALIGN4                         ← 只有写了载荷才会走到这里
/// ```
///
/// # 三处偏移的基准各不相同
///
/// | 字段 | 基准 |
/// |---|---|
/// | `compressedikerrorindex` | 相对**本条规则自身**（`write.cpp:931`） |
/// | `szattachmentindex` | 相对**本条规则自身**（`write.cpp:949`） |
/// | 6 个 `offset[k]` | 相对**该 compressed 结构自身**（`write.cpp:938`） |
///
/// 返回块在 `anim_data` 内的起始偏移。
fn write_ik_rules(
    anim_data: &mut Vec<u8>,
    rules: &[crate::model::ResolvedIkRule],
) -> Result<usize, AnimWriteError> {
    let block_start = anim_data.len();
    // ① 全部规则头（152 字节/条）。
    anim_data.resize(block_start + rules.len() * IK_RULE_SIZE, 0);
    // ② `ALIGN4(pData)`（`write.cpp:865`）—— 152 是 4 的倍数，所以这步
    //    只在数组起点不是 4 对齐时才移动游标。
    while !anim_data.len().is_multiple_of(4) {
        anim_data.push(0);
    }

    let mut attachment_patches: Vec<(usize, usize)> = Vec::new();

    for (j, r) in rules.iter().enumerate() {
        let at = block_start + j * IK_RULE_SIZE;
        let put_i32 = |b: &mut [u8], off: usize, v: i32| {
            b[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        put_i32(anim_data, at, r.index);
        put_i32(anim_data, at + 0x04, r.type_code);
        put_i32(anim_data, at + 0x08, r.chain);
        put_i32(anim_data, at + 0x0C, r.bone);
        put_i32(anim_data, at + 0x10, r.slot);
        anim_data[at + 0x14..at + 0x18].copy_from_slice(&r.height.to_le_bytes());
        anim_data[at + 0x18..at + 0x1C].copy_from_slice(&r.radius.to_le_bytes());
        anim_data[at + 0x1C..at + 0x20].copy_from_slice(&r.floor.to_le_bytes());
        for (k, v) in r.pos.iter().enumerate() {
            anim_data[at + 0x20 + k * 4..at + 0x24 + k * 4].copy_from_slice(&v.to_le_bytes());
        }
        for (k, v) in r.q.iter().enumerate() {
            anim_data[at + 0x2C + k * 4..at + 0x30 + k * 4].copy_from_slice(&v.to_le_bytes());
        }
        // +0x3C compressedikerrorindex（先留 0，下面按需回填）
        // +0x40 unused2 = 0
        put_i32(anim_data, at + 0x44, r.start_frame);
        // +0x48 ikerrorindex 恒 0（未压缩分支被 `#if 0` 屏蔽）
        anim_data[at + 0x4C..at + 0x50].copy_from_slice(&r.start.to_le_bytes());
        anim_data[at + 0x50..at + 0x54].copy_from_slice(&r.peak.to_le_bytes());
        anim_data[at + 0x54..at + 0x58].copy_from_slice(&r.tail.to_le_bytes());
        anim_data[at + 0x58..at + 0x5C].copy_from_slice(&r.end.to_le_bytes());
        // +0x5C unused3 = 0
        anim_data[at + 0x60..at + 0x64].copy_from_slice(&r.contact.to_le_bytes());
        // +0x64 drop / +0x68 top：源码里没有任何赋值点，恒 0
        // +0x6C..+0x78 unused6/7/8 = 0
        // +0x78 szattachmentindex（下面按需回填）
        // +0x7C unused[7] 全 0

        let Some(err) = &r.error else {
            // 6 个通道全空 → 整条载荷跳过（`write.cpp:922-928`），
            // `compressedikerrorindex` 保持 0。
            continue;
        };

        // ③ 压缩载荷。
        let comp_at = anim_data.len();
        put_i32(anim_data, at + 0x3C, (comp_at - at) as i32);
        anim_data.resize(comp_at + COMPRESSED_IK_ERROR_SIZE, 0);
        for (k, s) in err.scale.iter().enumerate() {
            anim_data[comp_at + k * 4..comp_at + k * 4 + 4].copy_from_slice(&s.to_le_bytes());
        }
        for (k, ch) in err.channels.iter().enumerate() {
            // `offset[k] = pData - (byte*)pCompressed` —— **写时游标**。
            let off = (anim_data.len() - comp_at) as i16;
            anim_data[comp_at + 24 + k * 2..comp_at + 24 + k * 2 + 2]
                .copy_from_slice(&off.to_le_bytes());
            anim_data.extend_from_slice(ch);
        }

        // ④ attachment 字符串（内联 + NUL）。
        if !r.attachment.is_empty() {
            let s = anim_data.len();
            anim_data.extend_from_slice(r.attachment.as_bytes());
            anim_data.push(0);
            attachment_patches.push((at + 0x78, s - at));
        }
        // ⑤ `ALIGN4(pData)`（`write.cpp:953`）—— 只有写过载荷才会到这里。
        while !anim_data.len().is_multiple_of(4) {
            anim_data.push(0);
        }
    }

    for (field, rel) in attachment_patches {
        anim_data[field..field + 4].copy_from_slice(&(rel as i32).to_le_bytes());
    }
    Ok(block_start)
}

/// 空段的轨道：**没有**逐帧数据，但头部仍是 36 字节。
///
/// 实测 `absec1` 的 sec[4]/sec[5]：头部 `28 / 36 / 0`（`stride = 0`），
/// 即**带一条旋转常量**（`+4 = 36` 说明内联区有 6 字节）。
/// 与内联形态的空段写「常量链」是同一个道理：
/// 「常量」骨骼的数据与帧数无关，空段里照样要写。
///
/// 这里只给**根骨骼**一条常量旋转轨道 —— 它与官方 `ab_n4`（全常量）
/// 的形态一致：`flags[0] = 0x40` + 内联 `Q(90°Z)`。
fn empty_segment_tracks(bone_count: usize) -> Vec<crate::ani_writer::RawBoneTrack> {
    let mut v = vec![crate::ani_writer::RawBoneTrack::default(); bone_count];
    if let Some(first) = v.first_mut() {
        // 根骨骼的绝对姿态恒为 0 → 常量轨道，编码成 `Q(90°Z)`。
        first.rot = Some((true, vec![[0.0f32, 0.0, 0.0]]));
    }
    v
}

/// 把逐骨骼轨道切成 `[lo, hi]`（**两端含**）的帧子集。
///
/// 块形态的分段用它重建每一段的载荷 —— 与内联形态的
/// [`write_one_chain`] 按帧子集重建链是同一个思路：段是**独立**的载荷，
/// 不能先切整条再复用。
///
/// 轨道为 `None`（该轨道不存在）时保持 `None`。
fn slice_tracks(
    tracks: &[crate::ani_writer::RawBoneTrack],
    lo: usize,
    hi: usize,
) -> Vec<crate::ani_writer::RawBoneTrack> {
    tracks
        .iter()
        .map(|t| crate::ani_writer::RawBoneTrack {
            rot: t.rot.as_ref().map(|(base, f)| {
                let end = (hi + 1).min(f.len());
                let lo = lo.min(end);
                (*base, f[lo..end].to_vec())
            }),
            pos: t.pos.as_ref().map(|f| {
                let end = (hi + 1).min(f.len());
                let lo = lo.min(end);
                f[lo..end].to_vec()
            }),
        })
        .collect()
}

/// 把逐动画的载荷**按块打包**，复刻 `FUN_0046aff0`（RVA `0x46aff0`）。
///
/// # 反汇编依据
///
/// ```c
/// iVar1 = *param_4;                       // 本动画的字节数
/// if (iVar1 != 0) {
///     if (param_1[0xab30] == 0 && (iVar3 != 0 || param_1[0xab31] == 0)) {
///         local_5 = 1;  local_c = iVar1;  // 这条要进块
///     }
///     if (iVar1 != 0 && DAT_024d7979 == 0) {
///         param_1[0x198] |= 0x40;  param_2[0xc] |= 0x40;   // 无旋转通道
///     }
/// }
/// if ((param_1[0x198] & 0x40) == 0) FUN_0046a6c0(...);   // RLE 路径
/// else                              FUN_0046aaa0(...);   // 原始样本路径
/// ...
/// if ((DAT_014944e4 != 0) &&
///     (DAT_0209693c < local_c - DAT_024d4938[DAT_014944e4 * 0x10])) {   // g_animblocksize
///     DAT_014944e4++;                      // 超预算 → 开新块
/// }
/// ```
///
/// 即：**累加当前块已用字节，一旦「已用 > `g_animblocksize`」就开新块**。
/// `block[0]` 是恒为 `(0,0)` 的**哨兵**，真实块从下标 1 开始 ——
/// 实测全部 121 个语料 `.ani` 与全部受控样本都满足。
///
/// # 输入
///
/// `payloads[i]` = 第 `i` 条动画的载荷（`None` 表示该动画走内联、不进块）。
/// 返回 `(每条的块下标, 每条在块内的偏移, 各块的字节)`；
/// 块字节**已含**每段前的 `ALIGN16` 填充。
pub fn pack_anim_blocks(
    payloads: &[Option<Vec<u8>>],
    block_size: i32,
) -> (Vec<i32>, Vec<Option<usize>>, Vec<Vec<u8>>) {
    let n = payloads.len();
    let mut block_of = vec![0i32; n];
    let mut offset_in = vec![None; n];
    let mut blocks: Vec<Vec<u8>> = Vec::new();

    if block_size <= 0 {
        return (block_of, offset_in, blocks);
    }

    // ⚠️ **只要有 `$animblocksize` 就至少有一块** —— 即使没有任何动画进块。
    //
    // 实测 `ab_z1`（单帧、全部动画内联、没有任何载荷）仍然是
    // `numanimblocks = 2` 且 `block[1] = [416, 416)`（**空块**）。
    // 所以块列表不能惰性创建，必须先放一块。
    blocks.push(Vec::new());
    let mut cur: usize = 0;

    for (i, p) in payloads.iter().enumerate() {
        let Some(data) = p else { continue };
        // 开新块的条件：**当前块已用 > 预算**（复刻 `g_animblocksize < used`）。
        // 第一条永不触发（此时 `blocks[0]` 还是空的）。
        if block_size < blocks[cur].len() as i32 {
            blocks.push(Vec::new());
            cur = blocks.len() - 1;
        }
        // 每段数据前 ALIGN16（实测 15782/15782）。
        let aligned = align16(blocks[cur].len());
        blocks[cur].resize(aligned, 0);
        offset_in[i] = Some(aligned);
        // 真实块下标从 1 开始（0 是哨兵）。
        block_of[i] = (cur + 1) as i32;
        blocks[cur].extend_from_slice(data);
    }
    (block_of, offset_in, blocks)
}

/// 序列的 blend 网格尺寸 `(groupsize[0], groupsize[1])`。
///
/// * 单动画序列 = `(1, 1)`。
/// * blend 序列：`groupsize[0]` 是编译期算好的 `blend_width`，
///   `groupsize[1] = 格数 / blend_width`（`compile.rs` 的
///   [`crate::compile::blend_grid_size`] 已保证整除）。
///
/// 之所以不在这里重算推断规则：`blend_grid_size` 会对**非法**格数
/// 报错并返回 `None`，而写出阶段拿不到那个错误通道；编译期算好、
/// 存进 [`crate::model::CompiledSequence::blend_width`] 才是单一来源。
fn seq_grid(seq: &crate::model::CompiledSequence) -> (i32, i32) {
    // `$declaresequence` 的空壳：官方 `groupsize` 保持 `memset` 的
    // **[0, 0]**（不是 1×1！）。实测见
    // [`crate::model::Sequence::forward_declared`]。
    if seq.forward_declared {
        return (0, 0);
    }
    // 单动画序列的 `cells` 只有 1 项（隐含动画），网格退化成 1×1。
    let cells = if seq.cells.len() <= 1 {
        0
    } else {
        seq.cells.len()
    };
    if cells == 0 {
        return (1, 1);
    }
    let w = seq.blend_width.max(1) as usize;
    debug_assert_eq!(cells % w, 0, "blend 格数 {cells} 不能被宽度 {w} 整除");
    (w as i32, (cells / w) as i32)
}

/// 把各块拼成完整的 `.ani` 文件（416 字节头 + 各块数据）。
///
/// 返回 `(文件字节, 块表)`；块表要写进 `.mdl` 的 `animblockindex` 数组，
/// **第 0 项是恒为 `(0,0)` 的哨兵**，之后每块一项 `(datastart, dataend)`。
///
/// 每块的起点做 `ALIGN16`（实测 `block[1].datastart` 恒为 **416**，
/// 而 416 正是 `ALIGN16(416)`）。
pub fn build_ani_file(blocks: &[Vec<u8>]) -> (Vec<u8>, Vec<(i32, i32)>) {
    let mut file = write_ani_container(0); // 先占位，末尾回填 length
    let mut table = vec![(0i32, 0i32)]; // 哨兵
    for b in blocks {
        let start = align16(file.len());
        file.resize(start, 0);
        file.extend_from_slice(b);
        table.push((start as i32, file.len() as i32));
    }
    let total = file.len() as i32;
    file[0x4C..0x50].copy_from_slice(&total.to_le_bytes());
    (file, table)
}

pub fn write_animations(
    compiled: &CompiledModelDesc,
    bone_parents: &[i32],
    ref_poses: &[([f32; 3], [f32; 3])],
    anim_data_abs: usize,
) -> Result<AnimWriteOutcome, AnimWriteError> {
    let bone_count = compiled.desc.bones.len();
    if bone_count > MAX_ANIM_ADDRESSABLE_BONES {
        return Err(AnimWriteError::TooManyBones { count: bone_count });
    }
    if ref_poses.len() != bone_count {
        return Err(AnimWriteError::Internal(crate::tr_fmt!(
            "the reference pose count %{got} does not match the bone count %{want}",
            got = ref_poses.len(),
            want = bone_count
        )));
    }

    // 没有任何序列 → 不产出动画段（也避免后面做无用的扫描）。
    if compiled.sequences.is_empty() {
        return Ok(AnimWriteOutcome {
            hierarchy_offsets: Vec::new(),
            anim_block_hierarchy_offsets: Vec::new(),
            animdescs: Vec::new(),
            seqdescs: Vec::new(),
            anim_data: Vec::new(),
            anim_offsets: Vec::new(),
            ikrule_offsets: Vec::new(),
            num_ikrules: Vec::new(),
            movement_offsets: Vec::new(),
            section_table_offsets: Vec::new(),
            section_frames: Vec::new(),
            num_sections: Vec::new(),
            anim_blocks: Vec::new(),
            anim_block_index: Vec::new(),
            anim_block_offset: Vec::new(),
            anim_block_ikrule_offset: Vec::new(),
            anim_block_section_offsets: Vec::new(),
            seq_subtables: Vec::new(),
            blend_offsets: Vec::new(),
            event_offsets: Vec::new(),
            iklock_offsets: Vec::new(),
            keyvalue_offsets: Vec::new(),
            weightlist_offsets: Vec::new(),
            posekey_offsets: Vec::new(),
            autolayer_offsets: Vec::new(),
            bone_scales: vec![([0.0; 3], [0.0; 3]); bone_count],
            event_name_patches: Vec::new(),
            stats: Vec::new(),
        });
    }

    // 要写出的动画（`$staticprop` 时是 1 条合成身份动画，见 [`AnimSpec`]）。
    let specs = anim_specs(compiled);

    // ---- 1a. 逐动画提取**浮点**增量帧 + 求全局缩放 ----
    //
    // 分两趟是必须的，不是风格问题：`rotscale` / `posscale` 是**全局一份**
    // （写在 bone 表里，所有序列共用），而量化值依赖 scale。若边扫边量化，
    // 后面的序列把 scale 抬高后，前面序列已经量化好的整数就全部作废 ——
    // 解码值会偏小，表现为「动画幅度被压缩」。
    let mut per_seq_frames: Vec<BoneFrameTable> = Vec::with_capacity(specs.len());
    // 与 `per_seq_frames` 同下标：每根骨骼 `RAWPOS` 要写的绝对值。
    let mut per_seq_abs_pos: Vec<AbsPosTable> = Vec::with_capacity(specs.len());
    // 与 `per_seq_frames` 同下标：每根骨骼 `RAWROT2` 要写的**绝对**旋转。
    //
    // ⚠️ 与 `per_seq_abs_pos` 完全对称 —— `RAWROT2` 也是**常量载荷**，
    // 引擎同样**不加**参考旋转（见 `BoneChannels::rot_const_raw` 的说明）。
    // 缺了它，凡走 `RAWROT2` 的骨骼都会少转一个参考旋转。
    let mut per_seq_abs_rot: Vec<AbsRotTable> = Vec::with_capacity(specs.len());
    let mut pos_scale = vec![[0.0f32; 3]; bone_count];
    let mut rot_scale = vec![[0.0f32; 3]; bone_count];

    for spec in &specs {
        let n = spec.frames;
        if n > i32::MAX as usize {
            return Err(AnimWriteError::TooManyFrames {
                sequence: compiled.sequences[spec.seq_index].name.clone(),
                count: n,
            });
        }
        let mut table: BoneFrameTable = Vec::with_capacity(bone_count);
        let mut abs_pos: AbsPosTable = Vec::with_capacity(bone_count);
        let mut abs_rot: AbsRotTable = Vec::with_capacity(bone_count);
        for (b, (ref_pos, ref_rot)) in ref_poses.iter().enumerate().take(bone_count) {
            let is_root = bone_parents.get(b).copied().unwrap_or(-1) < 0;
            let (rot_frames, pos_frames) = if spec.identity {
                // `$staticprop`：身份动画。存储值恒 0，所以每根骨骼的
                // 每个轴都是 `Absent` —— 于是链里只写 `ff 00 00 00`
                // 占位（bone=255「本骨骼无数据」），实测官方正是如此。
                //
                // 极值保持 0 也让 scale 落到下限分支：`rotscale` 三轴
                // 都是 `π/8/32767`（**不含**根骨骼的 π/2 下限）——
                // 与语料 2681/2681 一致。
                (vec![[0.0f32; 3]; n], vec![[0.0f32; 3]; n])
            } else {
                // ⚠️ 帧取自**该格**（blend 的每格是独立动画），
                // 不是 `sequences[].frames`。
                //
                // `subtract` 出来的动画走**另一条**路径：它存的已经是
                // 差值，不再减参考姿态、也不加根骨骼 Z 偏置
                // （见 [`delta_stored_frames`]）。
                if compiled.animations[spec.anim_index].delta {
                    delta_stored_frames(spec.cell_frames(compiled), spec.looping, b, n)
                } else {
                    delta_frames(
                        spec.cell_frames(compiled),
                        spec.looping,
                        b,
                        n,
                        is_root,
                        *ref_rot,
                        *ref_pos,
                    )
                }
            };
            // `RAWROT2` 的载荷：**绝对**旋转欧拉角。
            //
            // ⚠️ **与 `RAWPOS` 完全对称** —— `RAWROT2` 是常量，引擎
            // **逐字返回**、不加参考旋转（`bone_setup.cpp:367-372`），
            // 而 `ANIMROT` 的流**会**加（`bone_setup.cpp:398-406`）。
            // 所以常量形态必须自己把参考旋转烘进去。
            //
            // 官方口径（`write.cpp:756`）：
            // ```c
            // AngleQuaternion( srcanim->sanim[0][j].rot, q );   // sanim = 绝对值
            // ```
            //
            // 用「差值 + 参考」而不是直接取 `p.rotation`，是为了与
            // `delta_frames` **走同一条路径**（含 `wrap_to_pi` 与根骨骼
            // 的 +π/2 偏置），否则根骨骼会差一个整圈。
            //
            // `DELTA` 动画（`subtract`）例外：`sanim` 本身就是增量，
            // 引擎也不加参考 ⟹ 绝对值 == 差值。
            //
            // ⚠️ 必须在 `table.push` **之前**取（`rot_frames` 会被移动）。
            let abs_rot_v = if compiled.animations[spec.anim_index].delta {
                rot_frames.first().copied().unwrap_or([0.0; 3])
            } else {
                [0, 1, 2].map(|k| wrap_to_pi(rot_frames.first().map_or(0.0, |r| r[k]) + ref_rot[k]))
            };
            table.push((rot_frames, pos_frames));
            // `RAWPOS` 的载荷：**绝对**局部位移（根骨骼按 yaw +90° 旋转）。
            //
            // ⚠️ **`RAWPOS` 存绝对值，`ANIMPOS` 存差值 —— 两者语义不同。**
            //
            // `write.cpp:765`：
            // ```c
            // *((Vector48 *)pData) = srcanim->sanim[0][j].pos;   // ← 直接存 sanim
            // ```
            // 而 `ANIMPOS` 的流存的是 `(sanim − g_bonetable.pos) / posscale`
            // （`simplify.cpp:6510`）。这个不对称曾让 mdlc 把差值写进 `RAWPOS`。
            //
            // 根骨骼的 `sanim.pos` 还被 `rootxform` 旋转过
            // （`simplify.cpp:1461` `VectorRotate(tmp, rootxform, pos)`），
            // 即绕 Z 轴 yaw +90°：`(x, y) → (−y, x)`。
            //
            // 判据（官方 miku，`probe_rootpos_rule_all.js`，7/7 命中）：
            // | 骨骼 | SMD pos | 官方 `RAWPOS` |
            // |---|---|---|
            // | `bone 0`（根） | `[-3.0977, -1.5986, -1.8984]` | `[1.5986, -3.0957, -1.8984]` |
            // | `bone 63`（根） | `[5.1328, -3.8320, -8.0938]` | `[3.8301, 5.1328, -8.0938]` |
            // | `bone 67`（非根） | `[-20.4063, 3.5176, -0.0277]` | `[-20.4063, 3.5156, -0.0277]` |
            //
            // 注意 `bone 0` 的参考位移恰好是 0，**无法区分**「绝对值」与
            // 「差值」—— 又是「参考为零导致两种实现重合」。`bone 63`/`64`
            // 的参考位移非零，才把这条规则区分出来。
            //
            // ⚠️ **这里的 `(x,y) → (−y,x)` 不能删** —— 它是 `RAWPOS` 的
            // **绝对值**载荷（`write.cpp:765` 直接存 `sanim.pos`）。
            // `delta_frames` 里对**差值**也做同样的旋转（`ANIMPOS` 走那条），
            // 两者是**不同的量**、各自独立旋转，不会重复。
            abs_pos.push(
                spec.cell_frames(compiled)
                    .first()
                    .and_then(|f| f.get(b))
                    .map(|p| {
                        if is_root {
                            [-p.position[1], p.position[0], p.position[2]]
                        } else {
                            p.position
                        }
                    })
                    .unwrap_or([0.0; 3]),
            );
            abs_rot.push(abs_rot_v);
        }
        per_seq_frames.push(table);
        per_seq_abs_pos.push(abs_pos);
        per_seq_abs_rot.push(abs_rot);
    }

    // ---- 1a-2. 全局 (max_abs, extreme) ----
    //
    // 除数的选择依赖**极值的符号**（见 [`quant_divisor`]），而极值必须
    // 在**所有序列**上统一取 —— 因为 `rotscale`/`posscale` 是全局一份。
    // 对每个序列分别取极值再比大小是错的：那样得到的「极值」可能来自
    // 不同序列，符号与实际使用该 scale 的序列不匹配。
    let mut rot_extreme = vec![[0.0f32; 3]; bone_count];
    let mut pos_extreme = vec![[0.0f32; 3]; bone_count];
    for table in &per_seq_frames {
        for (b, (rot_frames, pos_frames)) in table.iter().enumerate() {
            for k in 0..3 {
                let rot_vals: Vec<f32> = rot_frames.iter().map(|r| r[k]).collect();
                let (ma, ex) = max_abs_and_extreme(&rot_vals);
                if ma > rot_extreme[b][k].abs() {
                    rot_extreme[b][k] = ex;
                }
                let pos_vals: Vec<f32> = pos_frames.iter().map(|t| t[k]).collect();
                let (ma_p, ex_p) = max_abs_and_extreme(&pos_vals);
                if ma_p > pos_extreme[b][k].abs() {
                    pos_extreme[b][k] = ex_p;
                }
            }
        }
    }

    for b in 0..bone_count {
        for k in 0..3 {
            // ⚠️ **没有「根骨骼 Z 下限 π/2」这条规则。**
            //
            // 早先这里对根骨骼的 Z 轴用 `ROOT_Z_ROT_SCALE_MIN`（π/2）当下限，
            // 那是把「窗口初值」误读成了「下限」。官方
            // `CompressAnimations`（`simplify.cpp:6364-6365`）给旋转轴的
            // 窗口初值是 `[-π/8, π/8]`，然后被**实际差值**撑开：
            //
            // ```c
            // minv = -M_PI / 8.0;  maxv = M_PI / 8.0;
            // for (每个动画 i) for (每帧 n) { v = sanim - g_bonetable;
            //     if (v < minv) minv = v;  if (v > maxv) maxv = v; }
            // scale = (|minv| > maxv) ? minv / -32768.0 : maxv / 32767;
            // ```
            //
            // 根骨骼的 +90° 偏置让差值**恰好**是 π/2，窗口自然被撑到 π/2
            // —— 所以受控实验（`blend1`/`blend2`/`blend3`）一直是对的。
            // 但那不是「下限」：实测 miku `bone[0]` 的差值只有 `4.77e-7`
            // （偏置与 `$definebone` 的参考旋转抵消后的浮点残渣），
            // 官方 `rotscale[2]` 就停在 **π/8**，而旧规则把它抬到了 π/2。
            //
            // 判据（`cmp_rotscale_real.js`）：官方 miku `bone[0]` 的
            // `rotscale = [π/8, π/8, π/8]/32767`，三轴**都是**下限。
            rot_scale[b][k] = axis_scale(rot_extreme[b][k], ROT_SCALE_MIN);

            let ex_p = pos_extreme[b][k];
            pos_scale[b][k] = axis_scale(ex_p, POS_SCALE_MIN);
        }
    }

    // ---- 1b. 用**最终** scale 量化 ----
    let mut per_seq: Vec<Vec<BoneChannels>> = Vec::with_capacity(compiled.sequences.len());
    for (si, table) in per_seq_frames.iter().enumerate() {
        // ⚠️ **逐骨骼的权重门**（`simplify.cpp:6475-6477`）：
        //
        // ```c
        // // skip bones that have no influence
        // if (g_panimation[i]->weight[j] < 0.001)
        //     continue;                       // ← 整根骨骼跳过，**不写记录**
        // ```
        //
        // 缺了它，`$weightlist` 把权重设成 0 的序列会把**全部**骨骼都写进链。
        // 实测 `v_snip_awp`：`weights_helping_hand_extend` 把 64 根骨骼
        // 全设成 0，官方 `@helping_hand_extend` 写 **0 条**记录，
        // mdlc 写 **63 条** —— 多出的骨骼会被推离参考姿态。
        //
        let seq_weights = specs[si].weights(compiled);
        let mut channels: Vec<BoneChannels> = Vec::with_capacity(bone_count);
        for (b, (rot_frames, pos_frames)) in table.iter().enumerate() {
            // 权重 < 0.001 ⟹ 该骨骼整根不进链（官方 `continue`）。
            // 缺省（表比骨骼数短 / 空表）按 1.0 处理。
            let w = seq_weights.get(b).copied().unwrap_or(1.0);
            if w < 0.001 {
                channels.push(BoneChannels {
                    rot: [AxisData::Absent, AxisData::Absent, AxisData::Absent],
                    pos: [AxisData::Absent, AxisData::Absent, AxisData::Absent],
                    pos_const_raw: None,
                    pos_delta_is_const: false,
                    rot_const_raw: None,
                });
                continue;
            }
            let mut ch = BoneChannels {
                rot: [AxisData::Absent, AxisData::Absent, AxisData::Absent],
                pos: [AxisData::Absent, AxisData::Absent, AxisData::Absent],
                pos_const_raw: None,
                pos_delta_is_const: false,
                rot_const_raw: None,
            };
            // 旋转**差值**逐帧同值 ⇒ 可以写常量 `RAWROT2`。
            // 同时记下要写的**绝对**旋转（见 `BoneChannels::rot_const_raw`）。
            if let Some(first) = rot_frames.first()
                && rot_frames.iter().all(|r| *r == *first)
            {
                ch.rot_const_raw = Some(per_seq_abs_rot[si][b]);
            }
            // 位移**差值**逐帧同值 ⇒ 可以写常量 `RAWPOS`。
            // 同时记下 `RAWPOS` 要写的**绝对**位移
            // （见上面 `per_seq_abs_pos` 的构造与说明）。
            if let Some(first) = pos_frames.first()
                && pos_frames.iter().all(|t| *t == *first)
            {
                ch.pos_delta_is_const = true;
                ch.pos_const_raw = Some(per_seq_abs_pos[si][b]);
            }
            for k in 0..3 {
                // ⚠️ **「该轴有没有数据」要用「量化后」判，不是「量化前」。**
                //
                // 官方 `simplify.cpp:6535-6591`：
                // ```c
                // value[n] = v / g_bonetable[j].rotscale[k-3];   // ← 先量化
                // ...
                // numanim[j][k] = pvalue - data;
                // if (numanim[j][k] == 2 && value[0] == 0)
                //     numanim[j][k] = 0;                          // ← 量化后为 0 才算「无数据」
                // ```
                // `numanim == 0` 的轴在写出时被跳过（`write.cpp:736-740`）。
                //
                // 用「量化前非零」判会让**量化后全 0 的浮点残渣**也写出一条
                // 全 0 轨道。实测 miku `bone 0`：偏置与 `$definebone` 的参考
                // 旋转抵消后残渣 `4.77e-7`，除以 `rotscale = 1.198e-5` 得
                // `0.0398` → `trunc` → `0`。官方因此**不写**旋转轨道
                // （`0x1`，只有 `RAWPOS`），而 mdlc 写出了 `0xc`。
                let q: Vec<i16> = rot_frames
                    .iter()
                    .map(|r| quantize(r[k], rot_scale[b][k]))
                    .collect();
                if q.iter().any(|v| *v != 0) {
                    ch.rot[k] = fold_axis(q);
                }
                let qp: Vec<i16> = pos_frames
                    .iter()
                    .map(|t| quantize(t[k], pos_scale[b][k]))
                    .collect();
                if qp.iter().any(|v| *v != 0) {
                    ch.pos[k] = fold_axis(qp);
                }
            }
            channels.push(ch);
        }
        per_seq.push(channels);
    }

    // ---- 2. 逐动画写动画链 ----
    let mut anim_data: Vec<u8> = Vec::new();
    let mut anim_offsets: Vec<usize> = Vec::with_capacity(specs.len());
    let mut ikrule_offsets: Vec<Option<usize>> = Vec::with_capacity(specs.len());
    let mut hierarchy_offsets = Vec::with_capacity(specs.len());
    let mut num_ikrules: Vec<usize> = Vec::with_capacity(specs.len());
    let mut section_table_offsets: Vec<Option<usize>> = Vec::with_capacity(specs.len());
    let mut section_frames: Vec<i32> = Vec::with_capacity(specs.len());
    let mut num_sections: Vec<usize> = Vec::with_capacity(specs.len());
    let mut stats: Vec<SequenceStat> = Vec::with_capacity(specs.len());

    for (si, spec) in specs.iter().enumerate() {
        let seq = &compiled.sequences[spec.seq_index];
        let channels = &per_seq[si];
        let n = spec.frames;
        let is_delta = compiled.animations[spec.anim_index].delta;
        let start = anim_data.len();
        anim_offsets.push(start);

        // ---- 段表（`mstudioanimsections_t`，8 字节/条）----
        //
        // 分段时，段表**紧插在该动画的链之前**，形如
        // `[段表][ALIGN16][段0链][段1链]…`（实测 `doors_glass_1`：
        // `animdesc[0]` 数据 18916 → `animdesc[1]` 段表 18916（72B）→
        // align16 18992 = `[1].animindex`）。
        //
        // 条目数 `nEnt = floor(nf/sf) + 2`，**不是 ceil**。
        //
        // # 段的帧划分（实测，用 RLE `total` 之和数出）
        //
        // | 段 k | 帧范围 | 帧数 |
        // |---|---|---|
        // | `0 ≤ k < nf/sf` | **闭区间** `[k*sf, min((k+1)*sf, nf-1)]` | **`sf+1`** |
        // | `nf/sf` / `nf/sf+1` | 空 | 写成常量链（16 B） |
        //
        // 注意是**两端含**的 `sf+1` 帧（`sf120` 实测 31/31/31/30），
        // 不是半开的 `sf` 帧。
        // ⚠️ **孤儿动画（没有任何序列引用）不能沿用 `seq[0]` 的
        // `section_frames`** —— 那只是 `unwrap_or(0)` 的诊断回退值。
        // 官方按**全局** `$sectionframes` 的阈值判，所以孤儿动画要自己算。
        // 实测 `look_neutral`：官方 `sf=0`，而沿用 `seq[0]`（`idle`）
        // 会得到 `sf=30` + 一张 `nEnt=2` 的段表。见 [`AnimSpec::orphan`]。
        let sf = if spec.orphan {
            match compiled.desc.model.section_frames {
                Some((len, thr)) if len > 0 && n as i32 >= thr => len,
                _ => 0,
            }
        } else {
            seq.section_frames
        };
        // ⚠️ 段数取自**本动画自己的帧数**（`n`），不是 `seq.num_sections` ——
        // 后者按引用它的序列算，复用动画时会短写。见 [`section_count`]。
        let n_sec = section_count(n, sf);
        let sec_table_off = if n_sec > 0 {
            let at = anim_data.len();
            // 先占位（`animindex` 要等各段链写好才知道）。
            anim_data.extend_from_slice(&vec![0u8; n_sec * 8]);
            // `ALIGN16` 之后才是第一条段链。
            //
            // ⚠️ 必须按**文件绝对位置**对齐：官方 `sw120` 的
            // `align16(段表绝对末尾 1112) == 链绝对起点 1120`。
            // 若只对 `anim_data` 内的偏移对齐，当 `anim_data` 起点本身
            // 不是 16 的倍数时就会差几个字节（实测差 8）。
            let abs_end = anim_data_abs + anim_data.len();
            let pad = (16 - abs_end % 16) % 16;
            anim_data.extend(std::iter::repeat_n(0u8, pad));
            Some(at)
        } else {
            None
        };

        // 每段的帧区间（空段 = `None`）。
        //
        // ⚠️ **`k == nf/sf + 1` 那一段不一定是空的** —— 引擎会把**末帧**
        // 重定向到它（`studio.cpp:86-90`）。详见 [`section_frame_range`]。
        let sec_ranges: Vec<Option<(usize, usize)>> = if n_sec > 0 {
            (0..n_sec)
                .map(|k| section_frame_range(k, n_sec, n, sf))
                .collect()
        } else {
            Vec::new()
        };

        // 逐段写链；不分段时就是「一段 = 全部帧」。
        let segments: Vec<Option<(usize, usize)>> = if n_sec > 0 {
            sec_ranges.clone()
        } else {
            vec![Some((0, n.saturating_sub(1)))]
        };

        // ⚠️ **块形态下链不进 `.mdl`** —— 它被写进 `.ani`（见下面的块打包），
        // `.mdl` 里只留**段表**（`sectionindex` 指向它）。
        //
        // 早先这里无条件调 `write_one_chain`，于是块形态的每条动画都会
        // 往 `anim_data` 里多写 4 字节（`bone=255` 占位），
        // 把**段表整体推后 4 字节** —— 实测 `absec1` 的
        // `sectionindex` 因此是 104（官方 100），后续段全部错位。
        //
        // 判据：该动画的帧数 >= 2（`n < 2` 的单帧动画**留在内联**，
        // 与块打包的判据一致）。
        let in_block = compiled.desc.model.anim_block_size.unwrap_or(0) > 0 && n >= 2;
        if !in_block {
            for (seg_i, range) in segments.iter().enumerate() {
                let seg_start = anim_data.len();
                if let Some(off) = sec_table_off {
                    // 回填 `section[seg_i] = (0, 该段链相对 animdesc 自身的偏移)`。
                    // ⚠️ `sectionindex` 是相对 **animdesc 自身** 的偏移，而这里
                    // 还不知道 animdesc 的绝对位置 —— 先记「相对 anim_data 起点」
                    // 的偏移，由 `write_mdl` 统一换算（与 `animindex` 同理）。
                    let at = off + seg_i * 8;
                    anim_data[at..at + 4].copy_from_slice(&0i32.to_le_bytes()); // animblock = 0（内联）
                    // 先写「相对 anim_data 起点」的临时值，`write_mdl` 会加上
                    // `anim_data_off` 再减去 animdesc 绝对位置。
                    anim_data[at + 4..at + 8].copy_from_slice(&(seg_start as i32).to_le_bytes());
                }
                write_one_chain(
                    &mut anim_data,
                    channels,
                    bone_count,
                    n,
                    range.as_ref().map(|(a, b)| (*a, *b)),
                    &rot_scale,
                    &pos_scale,
                    is_delta,
                    anim_data_abs,
                    &ChainCtx {
                        raw_rot_per_bone: per_seq_frames.get(si),
                        ref_rots: Some(ref_poses),
                    },
                )?;
            }
        }

        // 段表回填完成后，`animindex` 要指向**第一条链**的起点
        // （不是段表起点）。
        if let Some(off) = sec_table_off {
            let at = off + 4;
            let v = i32::from_le_bytes([
                anim_data[at],
                anim_data[at + 1],
                anim_data[at + 2],
                anim_data[at + 3],
            ]) as usize;
            anim_offsets[si] = v;
        }
        section_table_offsets.push(sec_table_off);
        section_frames.push(sf);
        num_sections.push(n_sec);

        // 需要进链的骨骼（供 `stats` 统计用）。
        let animated: Vec<usize> = (0..bone_count)
            .filter(|b| !channels[*b].is_empty())
            .collect();

        // ---- IK rule 块：紧跟在本条动画的链之后 ----
        //
        // 官方 `write.cpp:1045-1052`：
        // ```cpp
        // panimdesc[i].animindex = IsInt24(pData - &panimdesc[i]);
        // pData = WriteAnimationData(srcanim, pData);
        // if (srcanim->numikrules) {
        //     panimdesc[i].ikruleindex = IsInt24(pData - &panimdesc[i]);
        //     panimdesc[i].numikrules  = IsChar(srcanim->numikrules);
        //     pData = WriteIkErrors(srcanim, pData);
        // }
        // ```
        //
        // `stats[i].anim_bytes` 记的是**链本身**的长度（不含 IK 块），
        // 所以在这一步之前取。
        let chain_bytes = anim_data.len() - start;
        let rules = build_ik_rules(compiled, spec, bone_parents)?;
        if rules.is_empty() {
            ikrule_offsets.push(None);
        } else {
            ikrule_offsets.push(Some(write_ik_rules(&mut anim_data, &rules)?));
        }
        num_ikrules.push(rules.len());
        hierarchy_offsets.push(
            crate::local_hierarchy::write(
                &mut anim_data,
                &compiled.animations[spec.anim_index].local_hierarchy,
            )
            .map_err(AnimWriteError::Internal)?,
        );

        stats.push(SequenceStat {
            name: seq.name.clone(),
            frames: n,
            animated_bones: animated.len(),
            anim_bytes: chain_bytes,
        });
    }

    // ---- 2b. movement 数组（`mstudiomovement_t`，44 字节/条）----
    //
    // 位置：**全部动画数据之后**、按 animdesc 顺序排布，每条数组后 `ALIGN4`
    // （`write.cpp:1150-1174`）。实测 4353 个数组全部落在
    // `[animdesc 数组末尾, localseqindex)` 之间（17/17 模型）。
    //
    // `animdesc.nummovements` @ **+0x14**、`movementindex` @ **+0x18**
    // （**相对该 animdesc 记录自身** —— 与 `animindex`/`ikruleindex` 同理，
    // 绝对位置要等 `write_mdl` 知道 `anim_data` 落在哪）。
    //
    // ⚠️ **`ALIGN4` 是无条件的，即使一条 movement 都没有。**
    //
    // 官方（`write.cpp:1151-1161`）的循环体是：
    //
    // ```c
    // for (i = 0; i < animcount; i++) {
    //     panimdesc[i].nummovements = IsChar( anim->numpiecewisekeys );
    //     panimdesc[i].movementindex = IsInt24( pData - (byte*)&panimdesc[i] );
    //     pData += panimdesc[i].nummovements * sizeof( *pmove );
    //     ALIGN4( pData );          // <-- 0 条也执行
    // }
    // ```
    //
    // 早先本实现在 `movements.is_empty()` 时 `continue`，**跳过了对齐** ——
    // 于是每条动画少 0..3 字节，整份文件的后续段偏移全部提前。
    // 受控实验 `blend1` 实测：官方 anim_data 到 seqdesc 之间 104 字节、
    // mdlc 只有 92（4 条动画共差 12）。
    //
    // 对齐基准是**文件绝对位置**（`pData` 是指针），所以这里要加上
    // `anim_data_abs` —— 与上面段表的 `ALIGN16` 同理。
    let mut movement_offsets: Vec<Option<usize>> = Vec::with_capacity(specs.len());
    for spec in specs.iter() {
        let movements = spec.movements(compiled);
        if movements.is_empty() {
            movement_offsets.push(None);
        } else {
            movement_offsets.push(Some(anim_data.len()));
            for m in movements {
                anim_data.extend_from_slice(&m.endframe.to_le_bytes());
                anim_data.extend_from_slice(&m.motionflags.to_le_bytes());
                anim_data.extend_from_slice(&m.v0.to_le_bytes());
                anim_data.extend_from_slice(&m.v1.to_le_bytes());
                // `angle` 存的是**度**（TOML 里就给度，不做转换）。
                anim_data.extend_from_slice(&m.angle.to_le_bytes());
                for v in m.vector.iter().chain(m.position.iter()) {
                    anim_data.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        // 每条（含空数组）之后 ALIGN4 —— 按绝对位置。
        let abs_end = anim_data_abs + anim_data.len();
        let pad = (4 - abs_end % 4) % 4;
        anim_data.extend(std::iter::repeat_n(0u8, pad));
    }

    // ---- 3. animdesc 数组 ----
    // 名字字符串由调用方写进字符串池；这里先留 0，稍后由 write_mdl 回填；
    // `ikruleindex` 同理（它是**相对 animdesc 自身**的偏移，需要
    // `anim_data` 的绝对位置）。`numikrules` 这里就能写。
    //
    // 数量是 `specs.len()`（= `numlocalanim`），**不是**序列数 ——
    // `$staticprop` 时前者是 1 而后者可能大于 1。
    let mut animdescs = vec![0u8; specs.len() * ANIMDESC_SIZE];
    for (si, spec) in specs.iter().enumerate() {
        let o = si * ANIMDESC_SIZE;
        // baseptr：**负的自身绝对偏移**（运行时 pStudiohdr() = this + baseptr）。
        // 这里先留 0，由 write_mdl 用绝对偏移回填。
        // fps
        animdescs[o + 0x08..o + 0x0C].copy_from_slice(&spec.fps.to_le_bytes());
        // flags
        let mut flags = compiled.animations[spec.anim_index].extra_flags;
        if flags & !0x0892 != 0 {
            return Err(AnimWriteError::Internal(format!(
                "Unsupported animation flags {flags:#x}"
            )));
        }
        if compiled.desc.version() < 49 && flags & 0x0080 != 0 {
            flags = (flags & !0x0080) | 0x0100;
        }
        if spec.looping {
            flags |= STUDIO_LOOPING;
        }
        // `subtract` 会让官方置 `STUDIO_DELTA`（`simplify.cpp:163-166`）。
        if compiled.animations[spec.anim_index].delta {
            flags |= STUDIO_DELTA;
        }
        if compiled.desc.version() >= 49
            && compiled.desc.model.anim_block_size.unwrap_or(0) > 0
            && spec.frames >= 2
            && !compiled.animations[spec.anim_index].delta
        {
            flags |= 0x40;
        }
        animdescs[o + 0x0C..o + 0x10].copy_from_slice(&flags.to_le_bytes());
        // numframes
        animdescs[o + 0x10..o + 0x14].copy_from_slice(&(spec.frames as i32).to_le_bytes());
        let nmov = i32::try_from(spec.movements(compiled).len())
            .map_err(|_| AnimWriteError::Internal("Too many animation movement keys".into()))?;
        animdescs[o + 0x14..o + 0x18].copy_from_slice(&nmov.to_le_bytes());
        // animblock = 0, animindex = 链在本 animdesc 内的偏移（由 write_mdl 回填）
        // numikrules @0x3C（ikruleindex @0x40 由 write_mdl 回填）
        animdescs[o + 0x3C..o + 0x40].copy_from_slice(&(num_ikrules[si] as i32).to_le_bytes());
        let count = compiled.animations[spec.anim_index].local_hierarchy.len() as i32;
        animdescs[o + 72..o + 76].copy_from_slice(&count.to_le_bytes());
        // sectionindex / sectionframes = 0
    }

    // ---- 4. seq 子表区 + seqdesc 数组 ----
    //
    // 字段偏移由官方产物的 seqdesc **逐字段 dump** 确认
    // （见 `docs/animation-layout.md`）。注意几处与「按 studio.h 顺序推算」
    // 不同的地方：
    //   - `paramindex[2]` 在 0x4C / 0x50（不是 0x50 / 0x54）
    //   - `fadeintime` / `fadeouttime` 在 0x68 / 0x6C（不是 0x5C / 0x60），
    //     且实测值为 **0**（不是 0.2）
    //   - `lastframe` 在 0x84（不是 0x78），实测值为 0
    //   - `autolayerindex` / `weightlistindex` 在 0x98 / 0x9C
    //
    // # 子表的「空表标记」
    //
    // studiomdl 对**没有内容**的子表写 `SEQDESC_SIZE`（= 212）本身，
    // 即「表紧贴在 seqdesc 之后、长度为 0」。本实现照做 —— 这比写 0
    // 更贴近官方（`pAnimvalue` 之类的访问器用 `> 0` 判空，两者都安全，
    // 但逐字段差分时会看出差别）。
    let mut seq_subtables: Vec<u8> = Vec::new();
    let mut blend_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut event_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut iklock_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut keyvalue_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut weightlist_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut posekey_offsets = Vec::with_capacity(compiled.sequences.len());
    let mut autolayer_offsets = Vec::with_capacity(compiled.sequences.len());
    // weightlist 的**复用**：官方对内容逐元素相同的块只写一次
    // （`write.cpp:556-595`）。
    //
    // 键是**权重的位模式**（`Vec<u32>`）而不是 `Vec<f32>` ——
    // `f32` 不实现 `Hash`/`Eq`。用 `to_bits()` 是**精确**比较，
    // 与官方 `g_sequence[i].weight[j] != g_sequence[k].weight[j]`
    // 的逐 float 比较语义一致（注意 `-0.0 != 0.0` 在 C 里为假，
    // 但位模式不同 —— 官方那种情况会复用，mdlc 会新建一块。
    // 两者**语义等价**（内容都是 ±0），只是块数可能差 1）。
    let mut weight_block_cache: HashMap<Vec<u32>, usize> = HashMap::new();
    let transition_graph = crate::transitions::build(compiled).map_err(AnimWriteError::Internal)?;
    let mut seqdescs = vec![0u8; compiled.sequences.len() * SEQDESC_SIZE];
    // 待回填的事件名字：`(子表区内的字段偏移, 名字)`。
    //
    // 事件名进字符串池，而池的位置只有 `write_mdl` 知道，所以这里
    // 只记位置、由调用方回填「相对本事件自身」的偏移。
    let mut event_name_patches: Vec<(usize, String)> = Vec::new();

    for (si, seq) in compiled.sequences.iter().enumerate() {
        let o = si * SEQDESC_SIZE;
        // ---- 先把本序列的子表写进 `seq_subtables`，再回填 seqdesc 的偏移 ----
        // 顺序很重要：`eventindex` / `animindexindex` 等字段的值依赖子表
        // 在区内的位置，所以必须先算出来。
        //
        // ⚠️ **子表的基准是「seqdesc 数组末尾」，不是「本 seqdesc 末尾」。**
        //
        // 官方（`write.cpp:490`）是 `pseqdesc->eventindex = pData - pSequenceStart`，
        // 而 `pSequenceStart` 是**整个 seqdesc 数组的起点** ——
        // 所以 `eventindex = (数组末尾 − 本记录) + 表内偏移`
        // `= (seq_count − si) * 212 + 表内偏移`。
        //
        // 早先这里写的是 `SEQDESC_SIZE + 表内偏移`（只有 `si == seq_count − 1`
        // 时才碰巧正确），于是**除最后一条以外的所有序列**，
        // 它的 `eventindex` 都指回 seqdesc 数组**内部**，
        // 引擎读出来的是别的 seqdesc 的字节（实测表现为乱码 cycle/event/type）。
        //
        // 同一段代码里 blend 表用的 `sub_base_rel` 是**对的** ——
        // 两处基准不一致正是这个 bug 藏了很久的原因。
        let seq_count = compiled.sequences.len();
        let sub_base_rel = ((seq_count - si) * SEQDESC_SIZE) as i32;

        // 网格尺寸（`groupsize[0]` / `groupsize[1]`）。
        let (gs0, gs1) = seq_grid(seq);
        let has_posekey = gs0 > 1 || gs1 > 1;

        // ---- ① posekey：`float[groupsize[0] + groupsize[1]]` ----
        //
        // `write.cpp:449-466`：**只在 `groupsize[0] > 1 || groupsize[1] > 1`
        // 时写**，顺序是「先 groupsize[0] 个 param0[]，再 groupsize[1] 个 param1[]」。
        // 实测 `look_poses`（3x1）：`[-1, 0, 1, 0]` ✓
        let posekey_off_in_sub = if has_posekey {
            let off = seq_subtables.len();
            for axis in 0..2usize {
                let n = if axis == 0 { gs0 } else { gs1 } as usize;
                for m in 0..n {
                    let v = seq.blend_params[axis]
                        .as_ref()
                        .and_then(|p| p.keys.get(m).copied())
                        .unwrap_or(0.0);
                    seq_subtables.extend_from_slice(&v.to_le_bytes());
                }
            }
            posekey_offsets.push(Some(off));
            Some(off)
        } else {
            // 未写 ⇒ 该字段留 0（官方 `memset` 过 seqdesc，**不是**
            // `SEQDESC_SIZE` 那种「空表标记」）。
            posekey_offsets.push(None);
            None
        };

        // ---- ② events ----
        // `mstudioevent_t` = 80 字节：
        //   +0x00 cycle f32  +0x04 event i32  +0x08 type i32
        //   +0x0C options char[64]  +0x4C szeventindex i32
        // `options` 是**内联**数组；名字是**相对本事件自身**的偏移，
        // 指向字符串池 —— 所以这里先留 0，把待回填的位置记下来。
        //
        // ⚠️ 空表**也要**写偏移（= 当时的游标），不是写 `SEQDESC_SIZE`。
        let ev_field = {
            let off = seq_subtables.len();
            if seq.events.is_empty() {
                event_offsets.push(None);
            } else {
                event_offsets.push(Some(off));
                for ev in &seq.events {
                    let rec_at = seq_subtables.len();
                    let mut rec = vec![0u8; EVENT_SIZE];
                    rec[0x00..0x04].copy_from_slice(&ev.cycle.to_le_bytes());
                    rec[0x04..0x08].copy_from_slice(&ev.event.to_le_bytes());
                    rec[0x08..0x0C].copy_from_slice(&ev.event_type.to_le_bytes());
                    put_cstr_into(&mut rec, 0x0C, 64, &ev.options);
                    seq_subtables.extend_from_slice(&rec);
                    if !ev.name.is_empty() {
                        event_name_patches.push((rec_at + 0x4C, ev.name.clone()));
                    }
                }
            }
            (seq.events.len() as i32, sub_base_rel + off as i32)
        };
        // ③ `ALIGN4(pData)`（`write.cpp:524`）—— 无条件执行。
        while !seq_subtables.len().is_multiple_of(4) {
            seq_subtables.push(0);
        }

        // ---- ④ autolayers（`mstudioautolayer_t`，**24 字节**）----
        let autolayer_off_in_sub = seq_subtables.len();
        autolayer_offsets.push(autolayer_off_in_sub);
        for al in &seq.auto_layers {
            let mut rec = vec![0u8; 24];
            rec[0x00..0x02].copy_from_slice(&al.sequence.to_le_bytes());
            rec[0x02..0x04].copy_from_slice(&al.pose.to_le_bytes());
            rec[0x04..0x08].copy_from_slice(&al.flags.to_le_bytes());
            rec[0x08..0x0C].copy_from_slice(&al.start.to_le_bytes());
            rec[0x0C..0x10].copy_from_slice(&al.peak.to_le_bytes());
            rec[0x10..0x14].copy_from_slice(&al.tail.to_le_bytes());
            rec[0x14..0x18].copy_from_slice(&al.end.to_le_bytes());
            seq_subtables.extend_from_slice(&rec);
        }

        // ---- ⑤ weightlist：`float[g_numbones]` ----
        //
        // `write.cpp:556-595`：官方为**每条序列**都写这张表，且若内容与
        // 更早某条序列的块**逐元素相同**就**复用**它的偏移、不新建。
        //
        // # 内容从哪来（`setAnimationWeight` + `merge weightlists`）
        //
        // 官方是**两级**合并：
        //
        // 1. 每条**动画**的 `panim->weight[]` 来自它自己的 `weightlist`
        //    命令（缺省 = 表 0，全 1）；
        // 2. 序列的权重 = **各格动画逐骨骼取 MAX**
        //    （`simplify.cpp:302-318`）：
        //
        //    ```c
        //    for (n) { g_sequence[i].weight[n] = 0.0;
        //              for (j) for (k)
        //                  g_sequence[i].weight[n] = MAX( ..., panim[j][k]->weight[n] ); }
        //    ```
        //
        // 所以 blend 序列取的是「所有格的最大权重」，**不是**第一格。
        //
        // ⚠️ 早先本实现把权重**硬编码成全 1**（因为不实现 `$weightlist`），
        // 于是只有第一条序列新建块、其余全部复用 —— 这对「没有
        // `$weightlist` 的 QC」是对的，但对**有**的 QC 会写出错误的权重。
        //
        // ⚠️ 也不能把它当空表写 `SEQDESC_SIZE`（更早的错误）：引擎的
        // `pBoneweight(0)` 会按 `g_numbones` 个 float 去读，
        // 空表等于让它读到相邻子表（events / blend）的字节。
        //
        // # 复用规则（与官方一致，逐元素比较）
        //
        // 官方从**后往前**扫已有序列，找第一张内容相同的块复用
        // （`write.cpp:560-575` 的 `for (k = 0; k < i; k++)` 配合
        // `pBoneweight(0) > pweight` 的「只看更新的」剪枝）。
        // mdlc 用「内容 → 偏移」的哈希表表达同一件事：
        // **内容相同 ⟹ 同一偏移**，与官方等价（官方也是逐元素比）。
        let weights: &[f32] = &seq.weights;
        let key: Vec<u32> = weights.iter().map(|w| w.to_bits()).collect();
        let weightlist_off_in_sub = match weight_block_cache.get(&key) {
            Some(off) => *off,
            None => {
                let off = seq_subtables.len();
                for w in weights.iter().take(bone_count) {
                    seq_subtables.extend_from_slice(&w.to_le_bytes());
                }
                // 权重数组短于骨骼数时补 1.0（防御；`compile` 保证等长）。
                for _ in weights.len()..bone_count {
                    seq_subtables.extend_from_slice(&1.0f32.to_le_bytes());
                }
                weight_block_cache.insert(key, off);
                off
            }
        };
        weightlist_offsets.push(weightlist_off_in_sub);

        // ---- ⑥ iklocks（`mstudioiklock_t` = 32 字节）----
        //
        // `write.cpp:596-611`：
        // ```c
        // mstudioiklock_t *piklock = (mstudioiklock_t *)pData;
        // pseqdesc->numiklocks  = IsChar( g_sequence[i].numiklocks );
        // pseqdesc->iklockindex = IsInt24( pData - pSequenceStart );
        // pData += numiklocks * sizeof(mstudioiklock_t);
        // ALIGN4( pData );
        // for (j = 0; j < numiklocks; j++) {
        //     piklock->chain         = g_sequence[i].iklock[j].chain;
        //     piklock->flPosWeight   = g_sequence[i].iklock[j].flPosWeight;
        //     piklock->flLocalQWeight= g_sequence[i].iklock[j].flLocalQWeight;
        //     piklock++;
        // }
        // ```
        //
        // ⚠️ 这是**序列级** `iklock`，与模型级的 `$ikautoplaylock`
        // （头部 `+0x144` 的数组）是**两个不同的东西** ——
        // 语料里序列级出现在 **1594/11170 条序列（14.27%）**，
        // 而模型级只有 11/3333 个模型。
        //
        // ⚠️ **`flags` 与 `unused[4]` 官方不写**（`write.cpp` 只赋前 3 个
        // 字段，其余保持 `memset` 的 0）—— 实测语料 3222/3222 条
        // `flags == 0`。所以这里只写 12 字节，其余留 0。
        //
        // `chain` 落盘的是**链下标**（QC 里写链名，`LinkIKLocks`
        // `simplify.cpp:5650-5665` 解析成下标）—— 与 `$ikautoplaylock` 同一惯例。
        let iklock_off_in_sub = seq_subtables.len();
        iklock_offsets.push(iklock_off_in_sub);
        for (li, lk) in seq.iklocks.iter().enumerate() {
            let chain_idx = compiled
                .desc
                .ikchains
                .iter()
                .position(|c| c.name == lk.chain)
                .ok_or_else(|| {
                    AnimWriteError::Internal(crate::tr_fmt!(
                        "iklocks[%{lock}] of the sequence %{sequence} references an ikchain that does not exist: %{chain}",
                        lock = li,
                        sequence = format!("{:?}", seq.name),
                        chain = format!("{:?}", lk.chain)
                    ))
                })?;
            let mut rec = vec![0u8; IK_LOCK_SIZE];
            rec[0x00..0x04].copy_from_slice(&(chain_idx as i32).to_le_bytes());
            rec[0x04..0x08].copy_from_slice(&lk.pos_weight.to_le_bytes());
            rec[0x08..0x0C].copy_from_slice(&lk.local_q_weight.to_le_bytes());
            seq_subtables.extend_from_slice(&rec);
        }
        // ⑦ `ALIGN4(pData)`（`write.cpp:603`）—— **无条件执行**，
        // 即使 `numiklocks == 0` 也走（0 条时游标本就 4 对齐，是 no-op）。
        while !seq_subtables.len().is_multiple_of(4) {
            seq_subtables.push(0);
        }

        // ---- ⑧ blend 数组：`int16[groupsize[0]*groupsize[1]]` ----
        //
        // 元素是 **animdesc 下标**，写入顺序是**列主序**
        // （`write.cpp:618-637`：`offset = k*groupsize[0] + j`）。
        //
        // 单动画序列退化成 1 个元素（自己的 animdesc 下标）。
        // blend 序列的每一格指向**共享的** animdesc（`seq.cells`）。
        let blend_off_in_sub = seq_subtables.len();
        blend_offsets.push(blend_off_in_sub);
        // ⚠️ **不要 `.max(1)`** —— `$declaresequence` 的空壳
        // `groupsize = [0, 0]`，官方那两重循环**一次都不跑**，
        // 于是 blend 表**一个字节都不写**（`write.cpp:615` 的
        // `pData += 0 * sizeof(short)` 是 no-op）。
        //
        // 普通序列 `groupsize >= 1`，循环次数与原来完全一致。
        for k in 0..gs1 as usize {
            for j in 0..gs0 as usize {
                // 行主序的第 `k*gs0 + j` 格 → 该格引用的 animdesc 下标
                // （存储顺序是列主序，但下标本身按行主序数格子）。
                let cell = k * gs0 as usize + j;
                let idx = seq.cells.get(cell).copied().unwrap_or(0) as u16;
                seq_subtables.extend_from_slice(&idx.to_le_bytes());
            }
        }
        // ⑨ `ALIGN4(pData)`（`write.cpp:616`）
        while !seq_subtables.len().is_multiple_of(4) {
            seq_subtables.push(0);
        }

        // ---- ⑩ keyvalue（`WriteSeqKeyValues`，`write.cpp:1949-1964`）----
        let keyvalue_off_in_sub = seq_subtables.len();
        keyvalue_offsets.push(keyvalue_off_in_sub);
        let keyvalue_size = if let Some(text) = &seq.key_values {
            let wrapped = format!("mdlkeyvalue\n{{\n{text}}}\n\0");
            let size = wrapped.len() as i32;
            seq_subtables.extend_from_slice(wrapped.as_bytes());
            while !seq_subtables.len().is_multiple_of(4) {
                seq_subtables.push(0);
            }
            size
        } else {
            0
        };

        // ---- 现在回填 seqdesc ----
        // baseptr / szlabelindex / szactivitynameindex 由 write_mdl 回填。
        let mut flags = 0i32;
        if seq.looping {
            flags |= STUDIO_LOOPING;
        }
        // QC 的 `delta` 置**两个**位（`studiomdl.cpp:2827-2831`）。
        // 实测 `look_poses` 的 `flags == 0x14` = `STUDIO_POST | STUDIO_DELTA`。
        if seq.delta {
            flags |= STUDIO_DELTA | STUDIO_POST;
        }
        // QC 的**纯标志位**关键字（`snap` / `hidden` / `autoplay` /
        // `realtime` / `worldspace` / `post`）——
        // `ParseSequence`（`studiomdl.cpp:2720-2866`）逐个 `pseq->flags |= XXX`。
        //
        // 这些位在 QC 里没有具名 TOML 键，所以由 [`Sequence::extra_flags`]
        // 承载（QC 前端解析时填，TOML 也可以手写）。
        if let Some(extra) = seq.extra_flags {
            flags |= extra;
        }
        // `$declaresequence` ⟹ `STUDIO_OVERRIDE`（0x0800）。
        //
        // 官方 `Cmd_DeclareSequence` 是 `pseq->flags = STUDIO_OVERRIDE`
        // （**赋值不是或**），但对一条 `memset` 过的记录来说两者等价。
        // 实测空壳的 `flags` 恒为 `0x0800`。
        if seq.forward_declared {
            flags |= STUDIO_OVERRIDE;
        }
        // ⚠️ **再 OR 上每一格动画自己的 `animdesc.flags`**
        // （`studiomdl.cpp:3026-3038`）：
        //
        // ```c
        // for (i = 0; i < numblends; i++) { ... pseq->flags |= animations[i]->flags; }
        // ```
        //
        // 所以序列的 `loop` 其实**来自动画**，不是序列块自己写的关键字 ——
        // QC 里 `$animation "a_idle" ... loop` 之后，任何引用它的
        // `$sequence`（哪怕块里没写 `loop`）都会拿到 `STUDIO_LOOPING`。
        //
        // 实测 miku：`seq[1] "idle_raw"`（QC 只有 `$sequence "idle_raw" "a_idle"`）
        // 官方 `flags == 0x01` —— 而块里没有任何 `loop`。mdlc 早先按
        // `seq.looping` 单独算，写成 0x00。
        //
        // ⚠️ **但这里只能补 `STUDIO_LOOPING`，绝不能补 `STUDIO_DELTA`。**
        //
        // 上面那行官方代码跑在**解析期**（`ParseSequence` 尾部），而
        // `subtract` 在解析期**只往动画的 `cmds[]` 里记一条 `CMD_SUBTRACT`**
        // （`:1733-1751`），**完全不碰 `panim->flags`**；
        // `panim->flags |= STUDIO_DELTA` 要到 `simplify.cpp:164`
        // **执行**那条 cmd 时才发生 —— 那时序列的 flags 早已定稿。
        //
        // ⟹ 序列**永远**不会因为 `subtract` 拿到 `DELTA`。
        // 实测（`docs/_probe/probe_r9_delta_flags.js`）：6 条只有 `subtract`
        // 而无 `delta` 的 `*_layer` 序列，官方 `0x000` / `0x001`；
        // 照 `animations[].delta` 补位会写成 `0x004` / `0x005`（6/23 差异）。
        //
        // `LOOPING` 之所以**能**在这里补，是因为 `loop` 走的是
        // `ParseAnimationToken`（`:2226`），**解析期当场**就置位了。
        for cell in &seq.cells {
            if specs.get(*cell).is_some_and(|sp| sp.looping) {
                flags |= STUDIO_LOOPING;
            }
        }
        if compiled.desc.version() <= 48 {
            let variant_flags = flags & (0x0080 | 0x1000 | 0x2000);
            flags &= !(0x0080 | 0x1000 | 0x2000);
            if variant_flags & 0x0080 != 0 {
                flags |= 0x0100;
            }
            if variant_flags & 0x1000 != 0 {
                flags |= 0x0200;
            }
            if variant_flags & 0x2000 != 0 {
                flags |= 0x4000;
            }
        }
        if let Some(parameter) = compiled
            .desc
            .sequences
            .get(si)
            .and_then(|sequence| sequence.cycle_pose.as_ref())
        {
            if compiled.desc.version() > 48 {
                return Err(AnimWriteError::Internal(
                    "cyclepose requires the Source version 48 output format".into(),
                ));
            }
            let index = compiled
                .desc
                .model
                .pose_parameters
                .iter()
                .position(|entry| entry.name == *parameter)
                .ok_or_else(|| {
                    AnimWriteError::Internal(format!("Unknown cycle pose parameter {parameter:?}"))
                })?;
            flags |= 0x0080;
            seqdescs[o + 0xB4..o + 0xB8].copy_from_slice(&(index as i32).to_le_bytes());
        }
        seqdescs[o + 0x0C..o + 0x10].copy_from_slice(&flags.to_le_bytes());
        seqdescs[o + 0x10..o + 0x14].copy_from_slice(&seq.activity.to_le_bytes());
        // `actweight` @0x14 —— QC 的 `activity <名> <权重>` 的第二个参数。
        // 缺省 0（`studiomdl.cpp:2629` 的初始化值）。
        seqdescs[o + 0x14..o + 0x18].copy_from_slice(&seq.activity_weight.to_le_bytes());
        // numevents @0x18 / eventindex @0x1C
        seqdescs[o + 0x18..o + 0x1C].copy_from_slice(&ev_field.0.to_le_bytes());
        seqdescs[o + 0x1C..o + 0x20].copy_from_slice(&ev_field.1.to_le_bytes());
        // bbmin @0x20 / bbmax @0x2C：由 write_mdl 用顶点 AABB 回填。
        // numblends @0x38 = 本序列的**格子总数**（= groupsize[0]*groupsize[1]）。
        //
        // `write.cpp:438` 写的是 `g_sequence[i].numblends`，而它在
        // `studiomdl.cpp:3040` 被赋成「实际读到的动画个数」——
        // 与 `groupsize[0]*groupsize[1]` 恒等（同一函数 3019 行校验过）。
        //
        // ⚠️ **空壳序列（`$declaresequence`）是 0，不是 1。**
        // 官方 `memset` 之后从没给它赋过 `numblends`，`groupsize` 也是
        // `[0, 0]`。实测 `probe_declaresequence.js`：空壳
        // `numblends=0 groupsize=[0,0]`，而普通序列是 `1 / [1,1]`。
        // 早先这里对两者都写 `max(1)` —— 那会让空壳看起来像「1 格动画」。
        let numblends = gs0 * gs1;
        seqdescs[o + 0x38..o + 0x3C].copy_from_slice(&numblends.to_le_bytes());
        // `animindexindex` @0x3C：blend 表**相对该 seqdesc 自身**的偏移。
        //
        // 子表区起点相对 `seqdesc[si]` 是 `(seq_count − si) * SEQDESC_SIZE`
        // （`sub_base_rel`，在上面算 `eventindex` 时已定义），
        // 所以相对偏移就是它加上表内偏移。这个量 `anim_writer` 自己就能算，
        // 不必留给 `write_mdl` 回填。
        seqdescs[o + 0x3C..o + 0x40]
            .copy_from_slice(&(sub_base_rel + blend_off_in_sub as i32).to_le_bytes());
        // movementindex @0x40 = 0
        // groupsize[0] @0x44 / groupsize[1] @0x48
        // ⚠️ 同样**不要 `.max(1)`** —— 空壳是 `[0, 0]`（`memset` 原值）。
        seqdescs[o + 0x44..o + 0x48].copy_from_slice(&gs0.to_le_bytes());
        seqdescs[o + 0x48..o + 0x4C].copy_from_slice(&gs1.to_le_bytes());
        // paramindex[0] @0x4C / paramindex[1] @0x50
        // ⚠️ 空壳是 **[0, 0]**（`memset` 原值），不是普通序列的 `[-1, -1]`
        // （后者来自 `Cmd_Sequence` 的 `pseq->paramindex[0] = -1`）。
        for (axis, at) in [(0usize, 0x4Cusize), (1, 0x50)] {
            let v = if seq.forward_declared {
                0
            } else {
                seq.blend_params[axis]
                    .as_ref()
                    .map(|p| p.parameter_index)
                    .unwrap_or(-1)
            };
            seqdescs[o + at..o + at + 4].copy_from_slice(&v.to_le_bytes());
        }
        // paramstart[0] @0x54 / paramstart[1] @0x58
        // paramend[0]   @0x5C / paramend[1]   @0x60
        //
        // ⚠️ **两个轴是交错排布的，不是「start 两个、end 两个」。**
        // `studio.h` 的声明顺序是：
        //
        // ```c
        // int   paramindex[2];   // 0x4C, 0x50
        // float paramstart[2];   // 0x54, 0x58
        // float paramend[2];     // 0x5C, 0x60
        // ```
        //
        // 早先这里写成 `[(0, 0x54, 0x58), (1, 0x5C, 0x60)]` —— 于是
        // `paramend[0]` 被写进了 `paramstart[1]`、`paramend[1]` 被写进了
        // `paramend[0]`。实测官方 `blend1.mdl` 的 `look_poses` 是
        // `paramstart=[-1,0] paramend=[1,0]`，而 mdlc 给出
        // `paramstart=[-1,1] paramend=[0,0]` —— 正是这个错位。
        //
        // 官方（`simplify.cpp:5553-5557`）对**没给的轴**写 `0`（不是 -1）。
        for (axis, st_at, en_at) in [(0usize, 0x54usize, 0x5Cusize), (1, 0x58, 0x60)] {
            let (st, en) = match seq.blend_params[axis].as_ref() {
                Some(p) => (p.start, p.end),
                None => (0.0, 0.0),
            };
            seqdescs[o + st_at..o + st_at + 4].copy_from_slice(&st.to_le_bytes());
            seqdescs[o + en_at..o + en_at + 4].copy_from_slice(&en.to_le_bytes());
        }
        // paramparent @0x64 = 0
        // fadeintime @0x68 / fadeouttime @0x6C —— **缺省 0.2**，不是 0。
        //
        // `studiomdl.cpp:2639-2640` 在建 sequence 时就写死 `0.2`；
        // QC 的 `fadein`/`fadeout` 只是覆盖它（`studiomdl.cpp:2849-2857`）。
        //
        // 语料抽样 413 个模型 / 1545 条序列：1335 条是 `0.2 / 0.2`。
        // 早先这里留 0、注释还写着「实测为 0」，是**错的**。
        seqdescs[o + 0x68..o + 0x6C].copy_from_slice(&seq.fade_in.to_le_bytes());
        seqdescs[o + 0x6C..o + 0x70].copy_from_slice(&seq.fade_out.to_le_bytes());
        for (offset, value) in [0x70, 0x74, 0x78]
            .into_iter()
            .zip(transition_graph.sequences[si])
        {
            seqdescs[o + offset..o + offset + 4].copy_from_slice(&value.to_le_bytes());
        }

        // ---- autolayerindex @0x98 / numautolayers @0x94 ----
        seqdescs[o + 0x94..o + 0x98].copy_from_slice(&(seq.auto_layers.len() as i32).to_le_bytes());
        seqdescs[o + 0x98..o + 0x9C]
            .copy_from_slice(&(sub_base_rel + autolayer_off_in_sub as i32).to_le_bytes());
        // ---- weightlistindex @0x9C ----
        seqdescs[o + 0x9C..o + 0xA0]
            .copy_from_slice(&(sub_base_rel + weightlist_off_in_sub as i32).to_le_bytes());
        // ---- posekeyindex @0xA0 ----
        //
        // 官方只在写了 posekey 时才赋值；否则 `memset` 留下的 **0**。
        let posekey_field = match posekey_off_in_sub {
            Some(off) => sub_base_rel + off as i32,
            None => 0,
        };
        seqdescs[o + 0xA0..o + 0xA4].copy_from_slice(&posekey_field.to_le_bytes());
        // numiklocks @0xA4（**序列级** `iklock` 的条数，`write.cpp:600`）；
        // iklockindex @0xA8。
        //
        // ⚠️ 与头部的 `numlocalikautoplaylocks`（模型级 `$ikautoplaylock`）
        // 是**两个不同的字段** —— 语料里序列级出现在 14.27% 的序列上。
        seqdescs[o + 0xA4..o + 0xA8].copy_from_slice(&(seq.iklocks.len() as i32).to_le_bytes());
        seqdescs[o + 0xA8..o + 0xAC]
            .copy_from_slice(&(sub_base_rel + iklock_off_in_sub as i32).to_le_bytes());
        seqdescs[o + 0xAC..o + 0xB0]
            .copy_from_slice(&(sub_base_rel + keyvalue_off_in_sub as i32).to_le_bytes());
        seqdescs[o + 0xB0..o + 0xB4].copy_from_slice(&keyvalue_size.to_le_bytes());
        // cycleposeindex @0xB4 = 0
        //
        // `numikrules` @0x90 —— 与 animdesc 的那个是**两个不同的字段**。
        // 来源是 `simplify.cpp:6287-6295`：
        // ```c
        // for (j = 0; j < groupsize[0]; j++)
        //   for (k = 0; k < groupsize[1]; k++)
        //     g_sequence[i].numikrules = MAX(g_sequence[i].numikrules,
        //                                    g_sequence[i].panim[j][k]->numikrules);
        // ```
        // 即**对该序列每一格所引用的动画取 MAX** —— 不是「第 i 条序列取
        // 第 i 个 animdesc」。单动画序列两者恰好重合，所以早先的写法在
        // 非 blend 模型上一直是对的；miku 的 `idle`（3 格，引用 anim[1]
        // 与 anim[0]，两者各 2 条规则）才暴露出来：按序列下标取到的是
        // `anim[2]`（`look_down`，delta，0 条），于是写成了 0。
        //
        // `$staticprop` 时 `g_numani` 被压成 1，只有 `seq[0]` 拿得到值，
        // 其余保持 0（但静态道具没有 ikchain，全 0）。
        let seq_rules = if compiled.is_static_prop() {
            if si == 0 {
                num_ikrules.first().copied().unwrap_or(0)
            } else {
                0
            }
        } else {
            seq.cells
                .iter()
                .filter_map(|c| num_ikrules.get(*c).copied())
                .max()
                .unwrap_or(0)
        };
        seqdescs[o + 0x90..o + 0x94].copy_from_slice(&(seq_rules as i32).to_le_bytes());
    }

    // ---- 外置动画块（`.ani`）：按 `$animblocksize` 打包 ----
    //
    // 判据（实测 `ab_z1`/`zb90z` 单帧 → `animblock = 0` 内联；
    // 其余 8 个 `numframes >= 2` 的用例 → `animblock = 1`）：
    // **只有 `numframes >= 2` 的动画才进块**。
    //
    // 块预算与拼接见 [`pack_anim_blocks`] / [`build_ani_file`]。
    let (
        anim_block_index,
        anim_block_offset,
        anim_block_ikrule_offset,
        anim_block_section_offsets,
        anim_block_hierarchy_offsets,
        anim_blocks,
    ) = if compiled.desc.model.anim_block_size.unwrap_or(0) > 0 {
        // 逐动画生成「原始样本」载荷，并把 IK rule 块**追加在其后**。
        //
        // 官方 `write.cpp:1061-1062`：
        // ```c
        // byte *pIkData   = WriteAnimationData( srcanim, pBlockData );
        // byte *pBlockEnd = WriteIkErrors( srcanim, pIkData );
        // ```
        // 两次调用之间**没有任何对齐** —— 但 `WriteIkErrors` 的**第一件事**
        // 就是 `pData += numikrules*152; ALIGN4(pData);`，而那个 `ALIGN4`
        // **即使 `numikrules == 0` 也会执行**。所以载荷末尾总是补齐到 4 的倍数
        // （受控实验 `abi7`：载荷 66 → `animblockikruleindex = 68`；
        // `abi8`（无规则）的块长同样是 68）。
        let mut payloads: Vec<Option<Vec<u8>>> = Vec::with_capacity(specs.len());
        let mut block_ikrule_rel: Vec<Option<usize>> = Vec::with_capacity(specs.len());
        let mut block_hierarchy_rel = Vec::with_capacity(specs.len());
        // 每条动画的「块内段偏移」表（不分段时为 `None`）。
        let mut block_section_offsets_all: Vec<Option<Vec<usize>>> =
            Vec::with_capacity(specs.len());
        for (si, spec) in specs.iter().enumerate() {
            let n = spec.frames;
            if n < 2 {
                block_hierarchy_rel.push(None);
                payloads.push(None); // 单帧留在内联
                block_ikrule_rel.push(None);
                block_section_offsets_all.push(None);
                continue;
            }
            let seq = &compiled.sequences[spec.seq_index];
            // 本格的帧（blend 的每格是独立动画）。
            let cell = spec.cell_frames(compiled);
            let looping = spec.looping;
            // ---- 逐骨骼轨道（复刻 `FUN_0046aaa0` 的逐骨骼判定）----
            //
            // ⚠️ **存在性看「增量」，内容存「绝对量」** —— 这是本格式
            // 最容易写错的一处（10 个受控样本反解）：
            //
            // * **存在性**：`rot` 存在 ⟺ 根骨骼 **或** 旋转增量非零；
            //   `pos` 存在 ⟺ 位置增量非零。
            //   实测 `abi9`（4 骨骼、`ankle` 恒定 20°，恰好等于其参考姿态）：
            //   `flags = [0x40, 0, 0, 0x00]` —— `ankle` **一个位都没有**。
            //   若用绝对姿态判存在性，会多写一条常量轨道。
            // * **内容**：存**绝对**姿态。实测 `abiB` 的 `ankle`
            //   （增量 `0/0.1745/0.349`）存的是 `Q(0.349)/Q(0.5236)/Q(0.6981)`
            //   —— 是绝对角，不是增量。
            // * **基准旋转只作用于根骨骼**：`BuildRawTransforms`
            //   （`simplify.cpp:247-285`）只对根骨骼左乘
            //   `rootxform = AngleMatrix(g_defaultrotation)`（`Rz(90°)`）。
            //   实测 `abi1` 的 `ankle` 存 `[30,0,5]`（**未**旋转），
            //   而 `abiB` 的 `root` 存 `[0,10,0]` = `Rz(90°)·[10,0,0]`。
            //   早期实现对所有骨骼一律施加基准旋转 → `abi1` 差 8 字节、
            //   `abi7` 差 23 字节。
            let mut tracks: Vec<crate::ani_writer::RawBoneTrack> = Vec::with_capacity(bone_count);
            for (b, (ref_pos, ref_rot)) in ref_poses.iter().enumerate().take(bone_count) {
                let is_root = bone_parents.get(b).copied().unwrap_or(-1) < 0;
                // 绝对旋转（`canonical_euler(pose)`），不是增量。
                //
                // ⚠️ **`LOOPING` 的末帧要照抄第 0 帧** —— 与内联形态
                // （[`delta_frames`]）同一个规则。漏掉它会让最后一段的
                // 末帧写出真实姿态，而官方写的是第 0 帧的值。
                // 实测 `absec1`（`loop`、120 帧）：sec[3] 的第 29 帧
                // （全局第 119 帧）官方是 `0`（第 0 帧的存储值），
                // 本实现曾写成真实位移 → 尾部差 4 字节。
                let abs_rot: Vec<[f32; 3]> = (0..n)
                    .map(|f| {
                        let src_f = if looping && f == n - 1 { 0 } else { f };
                        crate::bone_math::canonical_euler(cell[src_f][b].rotation)
                    })
                    .collect();
                // 绝对位置（同样处理 `LOOPING` 末帧）。
                let abs_pos: Vec<[f32; 3]> = (0..n)
                    .map(|f| {
                        let src_f = if looping && f == n - 1 { 0 } else { f };
                        cell[src_f][b].position
                    })
                    .collect();
                // 增量只用来判「轨道存不存在」。
                let rot_varies = (0..n).any(|f| {
                    let d = crate::anim_writer::frame_stored_rot(cell, b, f, *ref_rot);
                    d != [0.0, 0.0, 0.0]
                });
                let pos_varies = (0..n).any(|f| {
                    let p = cell[f][b].position;
                    p[0] != ref_pos[0] || p[1] != ref_pos[1] || p[2] != ref_pos[2]
                });
                let rot_present = is_root || rot_varies;
                let pos_present = pos_varies;
                tracks.push(crate::ani_writer::RawBoneTrack {
                    rot: rot_present.then_some((is_root, abs_rot)),
                    // 根骨骼的位置要过基准旋转 `Rz(90°)`：`(x,y,z) → (−y,x,z)`。
                    pos: pos_present.then(|| {
                        if is_root {
                            abs_pos.iter().map(|p| [-p[1], p[0], p[2]]).collect()
                        } else {
                            abs_pos.clone()
                        }
                    }),
                });
            }
            let rle = compiled.desc.version() <= 48 || compiled.animations[spec.anim_index].delta;
            let write_rle = |payload: &mut Vec<u8>, range: Option<(usize, usize)>| {
                write_one_chain(
                    payload,
                    &per_seq[si],
                    bone_count,
                    n,
                    range,
                    &rot_scale,
                    &pos_scale,
                    compiled.animations[spec.anim_index].delta,
                    0,
                    &ChainCtx {
                        raw_rot_per_bone: per_seq_frames.get(si),
                        ref_rots: Some(ref_poses),
                    },
                )
            };
            let mut payload = if rle {
                let mut payload = Vec::new();
                write_rle(&mut payload, Some((0, n.saturating_sub(1))))?;
                payload
            } else {
                crate::ani_writer::write_raw_payload_tracks(&tracks)
            };
            // ---- 块形态的段表 ----
            //
            // 实测（受控实验 `absec1` = `sfw120.smd` 120 帧 + `$animblocksize 4096`）：
            //
            // * **段表仍在 `.mdl` 里** —— `animdesc.sectionindex` 依旧
            //   **相对 animdesc 自身**（`absec1` = 100，即紧接 animdesc 之后）；
            //   `sectionframes = 30`、`nEnt = floor(120/30)+2 = 6`。
            // * **每条段条目的 `animblock` = 该动画所在块下标**（不是 0），
            //   `animindex` 变成**相对块起点**的偏移。
            // * **每个段是块内一个独立的完整载荷**（各自的 28/36 字节头），
            //   段前 `ALIGN16`：
            //
            //   ```text
            //   sec[0] off=0    len=408  hdr 28/36/12
            //   sec[1] off=408  len=408  hdr 28/36/12
            //   sec[2] off=816  len=408  hdr 28/36/12
            //   sec[3] off=1224 len=396  hdr 28/36/12
            //   sec[4] off=1620 len=36   hdr 28/36/0   ← 空段
            //   sec[5] off=1656 len=36   hdr 28/36/0   ← 空段
            //   ```
            //
            //   最后两段与内联形态一样是**空段**（`stride = 0`）。
            //
            // ⚠️ **段与段之间没有 `ALIGN16`** —— 它们**紧密相接**：
            // 实测 `absec1` 的段偏移是 `0, 408, 816, 1224, 1620, 1656`，
            // 而 `408 % 16 == 8`。第一版按内联形态的习惯在每段前补
            // `ALIGN16`，得到 `0, 416, 832, …`（每段多 8 字节）。
            // 内联形态的 `ALIGN16` 是**段表末尾 → 第一条链**那一次，
            // 块形态没有段表参与（段表在 `.mdl` 里），所以也不需要。
            //
            // 所以块形态下的载荷不是「一整条动画」，而是**逐段载荷的紧密拼接**。
            let sf = seq.section_frames;
            // ⚠️ 与内联形态**同一个函数** —— 段数必须按**本动画**的帧数算。
            let n_sec = section_count(n, sf);
            let mut block_section_offsets: Option<Vec<usize>> = None;
            if n_sec > 0 {
                let mut joined: Vec<u8> = Vec::new();
                let mut offs: Vec<usize> = Vec::with_capacity(n_sec);
                for k in 0..n_sec {
                    offs.push(joined.len());
                    // ⚠️ 与内联形态**同一个函数** —— 末帧那一段不一定是空的
                    // （见 [`section_frame_range`]）。两条路径必须一致。
                    if rle {
                        write_rle(&mut joined, section_frame_range(k, n_sec, n, sf))?;
                        continue;
                    }
                    let seg_tracks = match section_frame_range(k, n_sec, n, sf) {
                        Some((lo, hi)) => slice_tracks(&tracks, lo, hi),
                        None => {
                            // 空段：**没有**逐帧数据，但头部仍是 36
                            // （带一条旋转常量），`stride = 0`。
                            // 实测 `absec1` 的 sec[4]/sec[5]：`hdr 28/36/0`。
                            empty_segment_tracks(bone_count)
                        }
                    };
                    joined.extend_from_slice(&crate::ani_writer::write_raw_payload_tracks(
                        &seg_tracks,
                    ));
                }
                payload = joined;
                block_section_offsets = Some(offs);
            }
            // 规则与内联形态**同源**（官方两条分支用的是同一个
            // `srcanim->ikrule`），只有写进哪个 index 字段不同。
            //
            // 位置：`WriteIkErrors` 在 `WriteAnimationData` **之后**被调用
            // （`write.cpp:1061-1062`），所以它在**全部段之后**。
            let rules = build_ik_rules(compiled, spec, bone_parents)?;
            // `WriteIkErrors` 的 `ALIGN4` 无条件执行（`abi8` 实测块长
            // 68 = align4(66)），所以载荷末尾总是补齐到 4 的倍数。
            while !payload.len().is_multiple_of(4) {
                payload.push(0);
            }
            if rules.is_empty() {
                block_ikrule_rel.push(None);
            } else {
                let rel = payload.len();
                write_ik_rules(&mut payload, &rules)?;
                block_ikrule_rel.push(Some(rel));
            }
            block_hierarchy_rel.push(
                crate::local_hierarchy::write(
                    &mut payload,
                    &compiled.animations[spec.anim_index].local_hierarchy,
                )
                .map_err(AnimWriteError::Internal)?,
            );
            block_section_offsets_all.push(block_section_offsets);
            payloads.push(Some(payload));
        }
        let (idx, off, blocks) =
            pack_anim_blocks(&payloads, compiled.desc.model.anim_block_size.unwrap_or(0));
        (
            idx,
            off,
            block_ikrule_rel,
            block_section_offsets_all,
            block_hierarchy_rel,
            blocks,
        )
    } else {
        (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    };

    Ok(AnimWriteOutcome {
        hierarchy_offsets,
        anim_block_hierarchy_offsets,
        animdescs,
        seqdescs,
        anim_data,
        anim_offsets,
        ikrule_offsets,
        num_ikrules,
        movement_offsets,
        section_table_offsets,
        section_frames,
        num_sections,
        anim_blocks,
        anim_block_index,
        anim_block_offset,
        anim_block_ikrule_offset,
        anim_block_section_offsets,
        seq_subtables,
        blend_offsets,
        event_offsets,
        iklock_offsets,
        keyvalue_offsets,
        weightlist_offsets,
        posekey_offsets,
        autolayer_offsets,
        bone_scales: pos_scale
            .iter()
            .zip(rot_scale.iter())
            .map(|(p, r)| (*p, *r))
            .collect(),
        event_name_patches,
        stats,
    })
}

/// 把字符串按 C 串写进**定长字段**（超长则截断，保证末尾有 NUL）。
///
/// `at` 是字段在 `buf` 内的**起始偏移**，`len` 是字段长度 —— 写入范围
/// 严格限制在 `[at, at+len)` 内。调用方保证缓冲区已 0 填充，
/// 所以无需手动补 NUL。
///
/// （早先的实现把 `at` 当成了「相对记录起点」，在只传字段切片时
/// 越界；现在显式做边界检查。）
fn put_cstr_into(buf: &mut [u8], at: usize, len: usize, s: &str) {
    let end = (at + len).min(buf.len());
    if at >= end {
        return;
    }
    let field_len = end - at;
    let bytes = s.as_bytes();
    let n = bytes.len().min(field_len.saturating_sub(1));
    buf[at..at + n].copy_from_slice(&bytes[..n]);
    // 其余保持 0（调用方已保证缓冲区是 0 填充）。
}

/// 供 `write_mdl` 使用的骨骼名 → animdesc 名字偏移回填辅助。
///
/// 因为 `animdesc.sznameindex` 与 `seqdesc.szlabelindex` 都是**相对自身**
/// 的偏移，而字符串池的位置只有在 `write_mdl` 里才知道，所以这里提供
/// 一个纯函数，让调用方传入每个名字的绝对位置后算出相对值。
pub fn relative_name_offset(name_abs: usize, struct_abs: usize) -> Result<i32, AnimWriteError> {
    let rel = name_abs as i64 - struct_abs as i64;
    i32::try_from(rel)
        .map_err(|_| AnimWriteError::Internal(crate::tr("the name relative offset overflows i32")))
}

/// 一个便于调用方使用的索引结构（避免在 `write_mdl` 里重算）。
pub type AnimNameSlots = HashMap<usize, usize>;

#[cfg(test)]
mod tests {
    #[test]
    fn root_animation_position_subtracts_reference_after_rotation() {
        let frames = vec![vec![crate::smd::SmdPose {
            bone: 0,
            position: [13.0, 25.0, 37.0],
            rotation: [0.0; 3],
        }]];
        let (_, delta) =
            super::delta_frames(&frames, false, 0, 1, true, [0.0; 3], [10.0, 20.0, 30.0]);
        assert_eq!(delta, vec![[-35.0, -7.0, 7.0]]);
        let decoded =
            std::array::from_fn::<_, 3, _>(|axis| delta[0][axis] + [10.0, 20.0, 30.0][axis]);
        assert_eq!(decoded, [-25.0, 13.0, 37.0]);
    }

    use super::*;
    use crate::model::{CompiledSequence, SequenceEvent};
    use crate::smd::SmdPose;

    /// 造一个只有骨骼姿态、没有网格的序列（动画测试不需要网格）。
    fn seq(name: &str, looping: bool, frames: Vec<Vec<SmdPose>>) -> CompiledSequence {
        CompiledSequence {
            key_values: None,
            name: name.to_string(),
            smd_path: std::path::PathBuf::from("test.smd"),
            fps: 30.0,
            looping,
            forward_declared: false,
            activity: -1,
            activity_name: String::new(),
            activity_weight: 0,
            delta: false,
            frames,
            cells: Vec::new(),
            blend_width: 1,
            blend_params: [None, None],
            calc_axes: Vec::new(),
            blend_ref: None,
            blend_comp: None,
            blend_center: None,
            transition: None,
            transition_reversible: false,
            auto_layers: Vec::new(),
            events: Vec::new(),
            fade_in: crate::model::default_fade_time(),
            fade_out: crate::model::default_fade_time(),
            no_auto_ik: false,
            ik_rules: Vec::new(),
            iklocks: Vec::new(),
            movements: Vec::new(),
            // 测试序列默认**不分段**（帧数少、也没写 `section_frames`），
            // 与 `compile.rs` 里「`numframes >= 阈值` 才分段」的缺省一致。
            section_frames: 0,
            num_sections: 0,
            pre_subtract_frames: None,
            extra_flags: None,
            // 单骨骼、无 `$weightlist` ⟹ 权重全 1（`g_weightlist[0]`）。
            weights: vec![1.0],
        }
    }

    fn pose(p: [f32; 3], r: [f32; 3]) -> SmdPose {
        SmdPose {
            bone: 0,
            position: p,
            rotation: r,
        }
    }

    /// **`$sequence` 级 `subtract` 的结果必须对写出器可见。**
    ///
    /// # 这是「动画里骨骼姿态不正确」的第二个根因
    ///
    /// `compile.rs` 把序列级 `subtract` 作用在**该序列自己的帧副本**
    /// （`CompiledSequence::frames`）上，**不写回**共享的
    /// `compiled.animations[].frames`。而 `AnimSpec::cell_frames` 早先只读
    /// 动画池 ⟹ **减除结果对写出器完全不可见**，动画按**未减除**的绝对
    /// 姿态写出。
    ///
    /// # 实测症状（`v_snip_awp` `@awm_fire_layer` 的 `b7`）
    ///
    /// | | `RAWROT2` 反解欧拉 |
    /// |---|---|
    /// | 官方 | `[0.00°, 0.00°, 0.00°]` ← 差值（减除生效） |
    /// | mdlc（修前） | `[-179.99°, 0.20°, -175.49°]` ← SMD 原始值 |
    ///
    /// 汇总：`probe_anim_bones.js` 报「**多写 2669 条记录**」（153 条动画）。
    ///
    /// # 夹具设计（关键）
    ///
    /// 必须让「序列帧」与「动画池帧」**不同**，否则两种取法恒等、测试空洞。
    /// 这里把序列帧设成「全 0」，动画池设成「非 0」——
    /// 取错来源时写出的载荷会明显不同。
    #[test]
    fn cell_frames_reads_sequence_not_animation_pool() {
        // 序列帧：全 0（模拟「减除之后」）。
        let seq_frames: Vec<Vec<SmdPose>> =
            (0..3).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect();
        let mut s = seq("layered", false, seq_frames);
        // `cells == [anim_index]` 是走序列帧的前提。
        s.cells = vec![0];
        let mut c = compiled(vec![s], 1);
        // 动画池：非 0（模拟「未减除的原始 SMD 值」）。
        c.animations[0].frames = (0..3)
            .map(|_| vec![pose([9.0, 9.0, 9.0], [1.5, 0.0, 0.0])])
            .collect();

        let spec = AnimSpec {
            seq_index: 0,
            orphan: false,
            anim_index: 0,
            frames: 3,
            fps: 30.0,
            looping: false,
            identity: false,
        };
        let got = spec.cell_frames(&c);
        assert_eq!(got.len(), 3);
        for row in got {
            assert_eq!(
                row[0].position,
                [0.0, 0.0, 0.0],
                "必须读**序列**的帧（已减除），不能读动画池（未减除）"
            );
            assert_eq!(row[0].rotation, [0.0, 0.0, 0.0]);
        }
    }

    /// blend 序列（`cells` 多于一个）**必须**退回动画池。
    ///
    /// `CompiledSequence::frames` 只存**第一格**，用它会让其余格全错。
    #[test]
    fn cell_frames_falls_back_to_pool_for_blend_sequences() {
        let seq_frames: Vec<Vec<SmdPose>> =
            (0..3).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect();
        let mut s = seq("blend", false, seq_frames);
        // 两个格 ⟹ 不是「单动画序列」。
        s.cells = vec![0, 1];
        let mut c = compiled(vec![s], 1);
        c.animations[0].frames = (0..3)
            .map(|_| vec![pose([9.0, 9.0, 9.0], [1.5, 0.0, 0.0])])
            .collect();

        let spec = AnimSpec {
            seq_index: 0,
            orphan: false,
            anim_index: 0,
            frames: 3,
            fps: 30.0,
            looping: false,
            identity: false,
        };
        let got = spec.cell_frames(&c);
        assert_eq!(
            got[0][0].position,
            [9.0, 9.0, 9.0],
            "blend 序列必须退回动画池（序列帧只存第一格）"
        );
    }

    fn compiled(seqs: Vec<CompiledSequence>, bone_count: usize) -> CompiledModelDesc {
        use crate::model::{Bone, ModelDesc, ModelMeta};
        // 测试里「一条序列 = 一个隐含动画」，与 `compile.rs` 对单动画序列
        // 的处理一致（名字 `@序列名`）。`cells` 指向动画池的下标 ——
        // 若调用方已显式设过（blend 测试）就保留。
        let mut seqs = seqs;
        for (i, s) in seqs.iter_mut().enumerate() {
            if s.cells.is_empty() {
                s.cells = vec![i];
            }
        }
        let animations: Vec<crate::model::CompiledAnimation> = seqs
            .iter()
            .map(|s| crate::model::CompiledAnimation {
                local_hierarchy: Vec::new(),
                weights: Vec::new(),
                loop_restart: 0,
                fudge_loop: false,
                extra_flags: 0,
                movements: Vec::new(),
                name: format!("@{}", s.name),
                smd_path: s.smd_path.clone(),
                fps: s.fps,
                looping: s.looping,
                frames: s.frames.clone(),
                delta: false,
                ik_rules: s.ik_rules.clone(),
                no_auto_ik: s.no_auto_ik,
                pre_subtract_frames: None,
            })
            .collect();
        CompiledModelDesc {
            desc: ModelDesc {
                model: ModelMeta {
                    rotation_limits: Vec::new(),
                    rotation_alignments: Vec::new(),
                    root_bone: None,
                    allowed_root_lods: 0,
                    constant_directional_light_dot: None,
                    source_dependencies: Vec::new(),
                    minimum_lod: 0,
                    autocenter: false,
                    first_source: None,
                    autocenter_offset: None,
                    forced_realign: Vec::new(),
                    bone_renames: Vec::new(),
                    bone_hierarchy: Vec::new(),
                    transition_nodes: Vec::new(),
                    transition_skips: Vec::new(),
                    transition_multistage: false,
                    source_rotation: None,
                    source_origin: None,
                    material_renames: Vec::new(),
                    name: "test.mdl".into(),
                    version: None,
                    checksum: None,
                    static_prop: false,
                    surface_prop: None,
                    eye_position: None,
                    illum_position: None,
                    illum_position_from_bone: false,
                    max_eye_deflection: None,
                    hull_min: None,
                    hull_max: None,
                    view_bb_min: None,
                    view_bb_max: None,
                    extra_flags: None,
                    contents: None,
                    joint_contents: Vec::new(),
                    skip_bone_in_bbox: false,
                    optimize_vtx: false,
                    split_oversized_meshes: true,
                    key_values: None,
                    pose_parameters: Vec::new(),
                    realign_bones: false,
                    anim_block_size: None,
                    section_frames: None,
                },
                physics: Default::default(),
                materials: Default::default(),
                bones: (0..bone_count)
                    .map(|i| Bone {
                        name: format!("b{i}"),
                        parent: if i == 0 {
                            None
                        } else {
                            Some(format!("b{}", i - 1))
                        },
                        position: None,
                        rotation: None,
                        flags: None,
                        surface_prop: None,
                        bonemerge: false,
                        pre_aligned: None,
                        realign_position: None,
                        realign_rotation: None,
                    })
                    .collect(),
                bodyparts: Vec::new(),
                hitboxes: Default::default(),
                additional_hitbox_sets: Vec::new(),
                attachments: Vec::new(),
                sequences: Vec::new(),
                animations: Vec::new(),
                bonecontrollers: Vec::new(),
                ikchains: Vec::new(),
                ik_autoplay_locks: Vec::new(),
                flex_descriptors: Vec::new(),
                flex_controllers: Vec::new(),
                flex_rules: Vec::new(),
                flex_controller_ui: Vec::new(),
                mouths: Vec::new(),
                jiggle_bones: Vec::new(),
                quat_interp_bones: Vec::new(),
                include_models: Vec::new(),
                weight_lists: Vec::new(),
                default_weight_list: Vec::new(),
                cmd_lists: Vec::new(),
            },
            bodyparts: Vec::new(),
            animations,
            sequences: seqs,
            realigned: None,
            resolved_flex_rules: Vec::new(),
            resolved_flex_controller_ui: Vec::new(),
            resolved_mouths: Vec::new(),
            resolved_jiggle_bones: Vec::new(),
            resolved_quat_interp_bones: Vec::new(),
            physics_bone: None,
        }
    }

    #[test]
    fn external_animation_encoding_matches_target_version_and_animdesc_flags() {
        let frames = vec![
            vec![pose([0.0; 3], [0.0; 3])],
            vec![pose([4.0, 2.0, 1.0], [0.2, 0.1, 0.3])],
        ];
        for version in [48, 49] {
            let mut model = compiled(vec![seq("move", false, frames.clone())], 1);
            model.desc.model.version = Some(version);
            model.desc.model.anim_block_size = Some(4096);
            let external = write_animations(&model, &[-1], &[([0.0; 3], [0.0; 3])], 0).unwrap();
            let flags = i32::from_le_bytes(external.animdescs[12..16].try_into().unwrap());
            assert_eq!(flags & 0x40 != 0, version == 49);
            assert_eq!(external.anim_block_index, [1]);
            if version == 48 {
                let block = &external.anim_blocks[0];
                assert_eq!(block[0], 0);
                assert!(block[1] & (STUDIO_ANIM_ANIMPOS | STUDIO_ANIM_ANIMROT) != 0);
                model.desc.model.anim_block_size = None;
                let inline = write_animations(&model, &[-1], &[([0.0; 3], [0.0; 3])], 0).unwrap();
                assert!(block.starts_with(&inline.anim_data));
            }
        }
    }

    /// 用「参考姿态 == SMD 第 0 帧」的常规情形写出动画。
    ///
    /// 绝大多数测试都属这种情形 —— 真实模型若没有 `$definebone`，
    /// 参考姿态就取自 SMD 第 0 帧。
    fn write_with_f0_refs(
        c: &CompiledModelDesc,
        parents: &[i32],
    ) -> Result<AnimWriteOutcome, AnimWriteError> {
        let refs: Vec<([f32; 3], [f32; 3])> = (0..c.desc.bones.len())
            .map(|b| {
                let f0 = c
                    .sequences
                    .first()
                    .and_then(|s| s.frames.first())
                    .and_then(|fr| fr.get(b))
                    .copied()
                    .unwrap_or(crate::smd::SmdPose {
                        bone: b as i32,
                        position: [0.0; 3],
                        rotation: [0.0; 3],
                    });
                // 参考姿态的旋转要规范化 —— 与 `resolve_bone_pose` 一致。
                (f0.position, crate::bone_math::canonical_euler(f0.rotation))
            })
            .collect();
        // `anim_data_abs`：动画数据区在文件里的绝对起点。本 helper 只关心
        // `anim_data` 内的相对布局，所以给 0 即可（不影响任何内部偏移）。
        write_animations(c, parents, &refs, 0)
    }

    /// 解开一条 `mstudioanimvalue_t` 流 → 每帧一个 `i16`。
    fn decode_stream(b: &[u8], ptr: usize, n: usize) -> Vec<i16> {
        let mut out = Vec::new();
        let mut p = ptr;
        while out.len() < n {
            let valid = b[p] as usize;
            let total = b[p + 1] as usize;
            if total == 0 {
                break;
            }
            p += 2;
            for _ in 0..valid.min(n - out.len()) {
                out.push(i16::from_le_bytes([b[p], b[p + 1]]));
                p += 2;
            }
            let last = out.last().copied().unwrap_or(0);
            for _ in valid..total.min(n - out.len() + valid) {
                if out.len() >= n {
                    break;
                }
                out.push(last);
            }
        }
        while out.len() < n {
            out.push(0);
        }
        out
    }

    /// 走一遍链，返回 `(bone, flags, payload 起点)` 列表。
    fn walk_chain(data: &[u8], offset: usize) -> Vec<(u8, u8, usize)> {
        let mut out = Vec::new();
        let mut p = offset;
        for _ in 0..64 {
            let bone = data[p];
            let flags = data[p + 1];
            let next = i16::from_le_bytes([data[p + 2], data[p + 3]]);
            out.push((bone, flags, p + 4));
            if next == 0 {
                break;
            }
            p += next as usize;
        }
        out
    }

    // ---- 量化 ----

    /// **核心判据**：量化必须是**向零截断**。
    ///
    /// 两条独立实测判据（见 `quantize` 的文档）：
    /// 正数用 `exp65`、负数用 `negq`。
    ///
    /// 真实规模模型的逐值统计（89 骨骼 × 30 帧，用**官方 scale** 复现
    /// 官方存储整数）：
    ///
    /// | 写法 | 命中率 |
    /// |---|---|
    /// | `trunc(v / scale)` | **99.59%** |
    /// | `round(v / scale)` | 54.15% |
    /// | `floor(v / scale)` | 26.89% |
    ///
    /// 剩下 0.41% 是极值附近的 float32 舍入（差 1 LSB），无法通过换写法消除。
    #[test]
    fn quantization_truncates_toward_zero() {
        // 判据 1：exp65，rotscale = f32(40°/32767)，角度 10° → 8191.75
        let rs = 40.0f32.to_radians() / QUANT_DIVISOR;
        assert_eq!(
            quantize(10.0f32.to_radians(), rs),
            8191,
            "10° 应截断为 8191"
        );
        assert_eq!(
            quantize(20.0f32.to_radians(), rs),
            16383,
            "20° 应截断为 16383"
        );
        assert_eq!(quantize(30.0f32.to_radians(), rs), 24575);

        // 判据 2：negq，rotscale = f32((π/8)/32767)，负角度
        let rs = ROT_SCALE_MIN / QUANT_DIVISOR;
        assert_eq!(
            quantize(-0.1, rs),
            -8344,
            "−0.1 应截断为 −8344（floor 会给 −8345）"
        );
        assert_eq!(quantize(-0.2, rs), -16688, "−0.2 应截断为 −16688");
        assert_eq!(quantize(-0.3, rs), -25032, "−0.3 应截断为 −25032");
    }

    /// **核心判据**：量化除数按**极值符号**选择。
    ///
    /// `i16` 范围 `[-32768, +32767]` 不对称，所以：
    /// 极值为负 → 除以 32768（极值正好落到 −32768）；
    /// 极值为正 → 除以 32767（极值正好落到 +32767）。
    ///
    /// 实测反解 `div = maxAbs / 官方scale`：极值为负的 31 个轴平均
    /// 32767.995，为正的 22 个轴平均 32766.998，零反例。
    #[test]
    fn quant_divisor_depends_on_extreme_sign() {
        assert_eq!(quant_divisor(-0.5), 32768.0, "极值为负 → 32768");
        assert_eq!(quant_divisor(0.5), 32767.0, "极值为正 → 32767");
        assert_eq!(quant_divisor(0.0), 32767.0, "极值为 0 → 32767（下限分支）");

        // 极值正好压到边界。
        let max_abs = std::f32::consts::FRAC_PI_2;
        assert_eq!(quantize(-max_abs, axis_scale(-max_abs, 0.0)), -32768);
        assert_eq!(quantize(max_abs, axis_scale(max_abs, 0.0)), 32767);
    }

    /// 下限主导时除数恒为 32767（下限是正数常量）。
    ///
    /// 实测：静止骨骼的 `posscale = 0.003906369209289551`，
    /// 正好等于 `128 / 32767`（而 `128/32768 = 0.00390625`）。
    #[test]
    fn axis_scale_uses_32767_when_floor_dominates() {
        let want = POS_SCALE_MIN / 32767.0;
        // 极值为 0（不动）
        assert_eq!(axis_scale(0.0, POS_SCALE_MIN), want);
        // 极值为负但小于下限 → 仍走下限分支
        assert_eq!(axis_scale(-10.0, POS_SCALE_MIN), want);
        // 极值超过下限 → 用符号除数
        let big = -1000.0f32;
        assert_eq!(axis_scale(big, POS_SCALE_MIN), 1000.0 / 32768.0);
    }

    #[test]
    fn quantization_clamps_to_i16() {
        let rs = 1e-9;
        assert_eq!(quantize(1.0, rs), 32767);
        assert_eq!(quantize(-1.0, rs), -32768);
    }

    // ---- 增量与偏置 ----
    //
    // 这一组测试钉住 `delta_frames` 的模型：
    //   stored = canonical_euler(pose) − ref，根骨骼 Z 再加 π/2。
    // 参考姿态 `ref` 必须由调用方显式传入（可能来自 `$definebone` 覆盖）。

    /// **根骨骼的 +90° 偏置必须在 `wrap_to_pi` 之前加。**
    ///
    /// `simplify.cpp:6516-6534`（`CompressAnimations` 的极值统计）：
    /// ```c
    /// v = ( sanim[n][j].rot[k-3] - g_bonetable[j].rot[k-3] );
    /// while (v >=  M_PI) v -= M_PI * 2;      // ← wrap 在**最后**
    /// while (v <  -M_PI) v += M_PI * 2;
    /// ```
    /// `sanim` 已经过 `rootxform` 复合（含偏置），
    /// 而 `g_bonetable[j].rot` 是 rest 姿态（不含偏置）——
    /// 所以是「先加偏置、再 wrap」。
    ///
    /// 实测判据（官方 miku `bone 65 "body"`，`probe_wrap_order.js`）：
    ///
    /// | 顺序 | `rotscale[2]` | 官方 |
    /// |---|---|---|
    /// | **wrap 后加偏置（官方序）** | **`9.508662e-5`** | `9.508663e-5` ✅ |
    /// | wrap 先加偏置（旧实现） | `9.767091e-5` | ❌ |
    ///
    /// 本测试构造一个「差值 + π/2 会超过 π」的场景，断言结果被 wrap 回来。
    #[test]
    fn root_bias_is_wrapped_after_being_added() {
        // 参考 yaw = 170°，帧 yaw = 93.368° → 差 = −76.632°，加 90° = 13.368°
        // 换一个会溢出的组合：参考 yaw = −170°，帧 yaw = 0° → 差 = 170°，
        // 加 90° = 260° → 必须 wrap 成 −100°。
        let ref_rot = [0.0, 0.0, -170.0f32.to_radians()];
        let s = seq("idle", false, vec![vec![pose([0.0; 3], [0.0, 0.0, 0.0])]]);
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 1, true, ref_rot, [0.0; 3]);
        let expect = wrap_to_pi(170.0f32.to_radians() + std::f32::consts::FRAC_PI_2);
        assert!(
            (rot[0][2] - expect).abs() < 1e-5,
            "根骨骼 Z = {}，应为 {}（加偏置后 wrap）—— \
             若等于 {}，说明偏置加在 wrap **之后**，窗口会被撑大",
            rot[0][2],
            expect,
            170.0f32.to_radians() + std::f32::consts::FRAC_PI_2
        );
        assert!(
            rot[0][2] <= std::f32::consts::PI,
            "结果必须落在 (−π, π] 内，实际 {}",
            rot[0][2]
        );
    }

    /// **核心判据**：根骨骼 Z 的 +90° 偏置必须在「减参考姿态」**之后**加。
    ///
    /// `gen_rootbias` 实验（根骨骼 Z 参考姿态非零）实测：
    /// 存储值 `2.234459` 对上「减完再加」的 `2.234464`；
    /// 「先给参考加偏置」会给出 `−0.907129`。
    #[test]
    fn root_z_bias_is_added_after_subtracting_reference() {
        let ref_yaw = 0.436332f32; // 25°
        let s = seq(
            "idle",
            false,
            vec![
                vec![pose([0.0; 3], [0.0, 0.0, 0.9])],
                vec![pose([0.0; 3], [0.0, 0.0, 1.1])],
            ],
        );
        let ref_rot = [0.0, 0.0, ref_yaw];
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 2, true, ref_rot, [0.0; 3]);
        // f0: (0.9 − 0.436332) + π/2 = 2.034464
        assert!(
            (rot[0][2] - (0.9 - ref_yaw + std::f32::consts::FRAC_PI_2)).abs() < 1e-5,
            "f0 Z = {}，应为 {}（偏置在减参考**之后**加）",
            rot[0][2],
            0.9 - ref_yaw + std::f32::consts::FRAC_PI_2
        );
        // f1: (1.1 − 0.436332) + π/2 = 2.234464（官方实测 2.234459）
        assert!(
            (rot[1][2] - 2.234464).abs() < 1e-4,
            "f1 Z = {}，应为 2.234464（官方实测 2.234459）",
            rot[1][2]
        );
        // 「先给参考加偏置」会给出 −0.907129 —— 必须不是这个值。
        assert!(
            (rot[1][2] - (-0.907129)).abs() > 1.0,
            "f1 Z = {} 落到了「先给参考加偏置」的错误分支",
            rot[1][2]
        );
    }

    /// 非根骨骼没有偏置。
    #[test]
    fn non_root_has_no_z_bias() {
        let s = seq("idle", false, vec![vec![pose([0.0; 3], [0.0; 3])]]);
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 1, false, [0.0; 3], [0.0; 3]);
        assert_eq!(rot[0], [0.0; 3]);
    }

    /// **核心判据**：存储值是「相对**参考姿态**」，不是「相对第 0 帧」。
    ///
    /// `biganim` 实验（89 骨骼 × 30 帧）实测命中率：
    /// 相对参考姿态 **100.00%** vs 相对第 0 帧 **13.14%**。
    #[test]
    fn stored_values_are_relative_to_reference_pose_not_frame_zero() {
        // 参考姿态 yaw = 0.3，第 0 帧 yaw = 0.9（刻意不同）
        let ref_rot = [0.0, 0.0, 0.3];
        let s = seq(
            "idle",
            false,
            vec![
                vec![pose([0.0; 3], [0.0, 0.0, 0.9])],
                vec![pose([0.0; 3], [0.0, 0.0, 1.1])],
            ],
        );
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 2, false, ref_rot, [0.0; 3]);
        assert!(
            (rot[0][2] - 0.6).abs() < 1e-5,
            "f0 Z = {}，应为 0.9 − 0.3 = 0.6（相对**参考姿态**；相对第 0 帧会给 0）",
            rot[0][2]
        );
        assert!(
            (rot[1][2] - 0.8).abs() < 1e-5,
            "f1 Z = {}，应为 1.1 − 0.3 = 0.8",
            rot[1][2]
        );
    }

    /// 位移同样相对参考姿态（`gen_rootbias` 实验：参考 z=12、第 0 帧 z=12.5，
    /// 第 1 帧 z=12.8 → 存储 0.8，而不是相对第 0 帧的 0.3）。
    #[test]
    fn position_is_relative_to_reference_pose() {
        let ref_pos = [3.0, -4.0, 12.0];
        let s = seq(
            "idle",
            false,
            vec![
                vec![pose([3.5, -4.5, 12.5], [0.0; 3])],
                vec![pose([3.6, -4.7, 12.8], [0.0; 3])],
            ],
        );
        let (_, pos) = delta_frames(&s.frames, s.looping, 0, 2, false, [0.0; 3], ref_pos);
        assert!(
            (pos[1][2] - 0.8).abs() < 1e-5,
            "f1 Z = {}，应为 12.8 − 12.0 = 0.8（相对第 0 帧会给 0.3）",
            pos[1][2]
        );
        assert!((pos[1][0] - 0.6).abs() < 1e-5);
        assert!((pos[1][1] - (-0.7)).abs() < 1e-5);
    }

    /// **核心判据**：`LOOPING` 末帧照抄**第 0 帧的存储值**，不是「归零」。
    ///
    /// `gen_loop` 实验（第 0 帧偏离参考姿态）实测末帧存储值 = 第 0 帧存储值
    /// （`0.499991`），而不是 0。文档 §3.4 的「末帧强制 0」只在
    /// 「第 0 帧 == 参考姿态」时成立 —— 那正是 exp64/exp65 的情形。
    #[test]
    fn looping_last_frame_copies_frame_zero_not_zero() {
        let ref_rot = [0.0, 0.0, 0.349066];
        let s = seq(
            "idle",
            true,
            vec![
                vec![pose([0.0; 3], [0.0, 0.0, 0.849066])],
                vec![pose([0.0; 3], [0.0, 0.0, 1.149066])],
                vec![pose([0.0; 3], [0.0, 0.0, 1.449066])],
            ],
        );
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 3, false, ref_rot, [0.0; 3]);
        // f0 = 0.849066 − 0.349066 = 0.5
        assert!((rot[0][2] - 0.5).abs() < 1e-5, "f0 = {}", rot[0][2]);
        // f1 = 1.149066 − 0.349066 = 0.8
        assert!((rot[1][2] - 0.8).abs() < 1e-5, "f1 = {}", rot[1][2]);
        // 末帧照抄 f0（官方实测 0.499991），**不是** 0
        assert!(
            (rot[2][2] - 0.5).abs() < 1e-5,
            "末帧 Z = {}，应为 0.5（照抄第 0 帧）；归零模型会给 0",
            rot[2][2]
        );
        assert!(
            rot[2][2].abs() > 0.1,
            "末帧被错误地归零了（说明还在用文档 §3.4 的旧模型）"
        );
    }

    /// 非 LOOPING 时末帧**不**特殊处理。
    #[test]
    fn non_looping_keeps_last_frame() {
        let s = seq(
            "idle",
            false,
            vec![
                vec![pose([0.0; 3], [0.0, 0.0, 0.5])],
                vec![pose([0.0; 3], [0.0, 0.0, 1.1])],
            ],
        );
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 2, false, [0.0; 3], [0.0; 3]);
        assert!(
            (rot[1][2] - 1.1).abs() < 1e-5,
            "非 loop 末帧应保留原值 1.1，实际 {}",
            rot[1][2]
        );
    }

    /// **核心判据**：旋转先**规范化**再减参考姿态。
    ///
    /// `gimbal` 实验（pitch = π/2 万向锁）实测：
    /// `canonical(smd) − ref` 15/15 命中，
    /// `canonical(smd − ref)`（先减后规范化）只有 2/15。
    #[test]
    fn rotation_is_canonicalized_before_subtracting_reference() {
        use std::f32::consts::FRAC_PI_2;
        // 万向锁姿态：pitch = π/2，roll/yaw 会被合并进 yaw
        let s = seq(
            "idle",
            false,
            vec![vec![pose([0.0; 3], [0.35, FRAC_PI_2, -0.25])]],
        );
        let (rot, _) = delta_frames(&s.frames, s.looping, 0, 1, false, [0.0; 3], [0.0; 3]);
        // 规范化后应是 [0, π/2, -0.6]（roll 归 0，自由度并进 yaw）
        assert!(
            rot[0][0].abs() < 1e-5,
            "规范化后 roll 应为 0，实际 {}",
            rot[0][0]
        );
        assert!((rot[0][1] - FRAC_PI_2).abs() < 1e-5);
        assert!(
            (rot[0][2] - (-0.6)).abs() < 1e-4,
            "规范化后 yaw 应为 −0.6，实际 {}",
            rot[0][2]
        );
    }

    #[test]
    fn wrap_to_pi_wraps() {
        use std::f32::consts::PI;
        assert!((wrap_to_pi(0.5) - 0.5).abs() < 1e-6);
        assert!((wrap_to_pi(PI + 0.1) - (-PI + 0.1)).abs() < 1e-5);
        assert!((wrap_to_pi(-PI - 0.1) - (PI - 0.1)).abs() < 1e-5);
    }

    // ---- 轴折叠 ----

    #[test]
    fn fold_axis_all_zero_is_absent() {
        assert_eq!(fold_axis(vec![0, 0, 0, 0]), AxisData::Absent);
    }

    #[test]
    fn fold_axis_constant_nonzero_is_constant() {
        assert_eq!(fold_axis(vec![7, 7, 7]), AxisData::Constant(7));
    }

    #[test]
    fn fold_axis_varying_is_sampled() {
        assert_eq!(fold_axis(vec![0, 1, 0]), AxisData::Sampled(vec![0, 1, 0]));
    }

    #[test]
    fn constant_rot_is_none_when_any_axis_varies() {
        let ch = BoneChannels {
            rot: [
                AxisData::Sampled(vec![0, 1]),
                AxisData::Absent,
                AxisData::Constant(5),
            ],
            pos: [AxisData::Absent, AxisData::Absent, AxisData::Absent],
            pos_const_raw: None,
            pos_delta_is_const: false,
            rot_const_raw: None,
        };
        assert!(ch.constant_rot().is_none());
    }

    #[test]
    fn constant_rot_treats_absent_as_zero() {
        let ch = BoneChannels {
            rot: [AxisData::Absent, AxisData::Absent, AxisData::Constant(100)],
            pos: [AxisData::Absent, AxisData::Absent, AxisData::Absent],
            pos_const_raw: None,
            pos_delta_is_const: false,
            rot_const_raw: None,
        };
        let a = ch.constant_rot().expect("三轴都不随时间变化 → 常量");
        assert_eq!(a, [0.0, 0.0, 100.0]);
    }

    /// **`RAWROT2` 的载荷必须是绝对旋转，不是差值。**
    ///
    /// # 这是「动画里骨骼旋转不正确」的根因
    ///
    /// 引擎对 `RAWROT2`（常量 `Quaternion64`）**逐字返回**、**不加**参考旋转
    /// （`bone_setup.cpp:367-372`）：
    /// ```c
    /// if ( panim->flags & STUDIO_ANIM_RAWROT2 ) { q = *(panim->pQuat64()); return; }
    /// ```
    /// 而 `ANIMROT` 的流**会**加（`bone_setup.cpp:398-406`）：
    /// ```c
    /// if (!(panim->flags & STUDIO_ANIM_DELTA)) angle1.x += baseRot.x;
    /// ```
    ///
    /// 官方写出时用**绝对**旋转（`write.cpp:756`）：
    /// ```c
    /// AngleQuaternion( srcanim->sanim[0][j].rot, q );
    /// ```
    ///
    /// ⟹ 常量形态若写差值，该骨骼会**整体少转一个参考旋转**。
    /// 实测 `v_snip_awp` 根骨骼 `b0` 的参考旋转是 `[π/2, 0, 0]`
    /// ⟹ 动画里根骨骼**整整少转 90°**（用户 HLMV 实测症状）。
    ///
    /// 本测试钉住「载荷 = 参考 + 差值」，且与 `RAWPOS` 的绝对语义**对称**。
    #[test]
    fn rawrot2_payload_is_absolute_not_delta() {
        // 参考旋转 roll = π/2（v_snip_awp 根骨骼的真实值）。
        let ref_rot = [std::f32::consts::FRAC_PI_2, 0.0, 0.0];
        // 差值：只有 yaw 变化 π/2（根骨骼的 +90° 偏置）。
        let delta = [0.0f32, 0.0, std::f32::consts::FRAC_PI_2];
        // 官方 = 参考 + 差值 = [π/2, 0, π/2]。
        let want = [
            std::f32::consts::FRAC_PI_2,
            0.0,
            std::f32::consts::FRAC_PI_2,
        ];

        let abs: [f32; 3] = [0, 1, 2].map(|k| wrap_to_pi(delta[k] + ref_rot[k]));
        assert_eq!(abs, want, "RAWROT2 载荷应为「参考 + 差值」");

        // 编码出来的四元数必须**等价于** `want`（四元数点积 = ±1）。
        //
        // ⚠️ 不要在这里硬写「官方字节」—— `encode_quaternion64` 的位序
        // 与探针 `probe_const_dump.js` 打印的 hex 是**两种排布**，
        // 手抄会得到假失败（本测试第一版就抄错了 8 个字节）。
        // 真正的逐字节判据在 `probe_const_dump.js`（实测两侧
        // `hex=fffff7fffffaff5f` 完全相同）。
        let q = crate::bone_math::angle_quaternion(abs);
        let want_q = crate::bone_math::angle_quaternion(want);
        let dot: f32 = q.iter().zip(want_q.iter()).map(|(a, b)| a * b).sum();
        assert!(
            (dot.abs() - 1.0).abs() < 1e-5,
            "编码前四元数应等价于「参考+差值」，点积 = {dot}"
        );

        // 反证：只写差值会得到**不同**的四元数（旧 bug 的产物）。
        let q_delta = crate::bone_math::angle_quaternion(delta);
        let dot_delta: f32 = q_delta.iter().zip(q.iter()).map(|(a, b)| a * b).sum();
        assert!(
            (dot_delta.abs() - 1.0).abs() > 0.1,
            "只写差值必须与绝对旋转明显不同 —— 否则本测试是空洞的（点积 {dot_delta}）"
        );
        // 并且差的角度应当接近 90°（参考旋转的 roll = π/2）。
        let ang = 2.0 * dot_delta.abs().clamp(-1.0, 1.0).acos() * 180.0 / std::f32::consts::PI;
        assert!(
            (ang - 90.0).abs() < 1.0,
            "差值版与绝对值版的夹角应 ≈ 90°（参考 roll = π/2），实测 {ang}°"
        );
    }

    /// `DELTA` 动画的 `RAWROT2` 例外：`sanim` 本身就是增量，
    /// 引擎也不加参考 ⟹ **绝对值 == 差值**。
    ///
    /// 判据：官方 `blend1.mdl` 的 `look_down` 是 `0x30`（`RAWROT2|DELTA`）。
    #[test]
    fn rawrot2_is_delta_for_subtract_animations() {
        let ref_rot = [std::f32::consts::FRAC_PI_2, 0.0, 0.0];
        let delta = [0.0f32, 0.0, std::f32::consts::FRAC_PI_2];
        // DELTA 分支：直接用差值，**不**加参考。
        let abs = delta;
        assert_eq!(abs, delta, "DELTA 动画的 RAWROT2 载荷 == 差值");
        // 与「加参考」的结果必须不同 —— 否则这条分支没有意义。
        let non_delta = [0, 1, 2].map(|k| wrap_to_pi(delta[k] + ref_rot[k]));
        assert_ne!(abs, non_delta);
    }

    // ---- 常量旋转的判定 ----

    /// `Vector48` 是 **3 个 IEEE binary16**（不是定点），且**向零截断**。
    ///
    /// 判据来自官方 miku `bone[0]` 的 `RAWPOS`（真 studiomdl 产物）：
    ///
    /// | 值 | 期望 half | 舍入到最近偶数会给 |
    /// |---|---|---|
    /// | `1.598633` | `0x3e65` | `0x3e65` |
    /// | `-3.097656` | `0xc231` | **`0xc232`** ← 差 1 |
    /// | `-1.898438` | `0xbf98` | `0xbf98` |
    ///
    /// 全部 7 条根骨骼 `RAWPOS` 记录：RN 命中 2/7，**TZ 命中 7/7**
    /// （`docs/_probe/probe_half_rounding.js`）。
    #[test]
    fn vector48_is_binary16_with_truncation() {
        let enc = encode_vector48([1.598633, -3.097656, -1.898438]);
        assert_eq!(
            u16::from_le_bytes([enc[0], enc[1]]),
            0x3e65,
            "1.598633 → 0x3e65"
        );
        assert_eq!(
            u16::from_le_bytes([enc[2], enc[3]]),
            0xc231,
            "-3.097656 → 0xc231（**截断**；舍入到最近偶数会给 0xc232）"
        );
        assert_eq!(
            u16::from_le_bytes([enc[4], enc[5]]),
            0xbf98,
            "-1.898438 → 0xbf98"
        );
        // 定点实现会给出完全不同的字节（旧 bug 的指纹）。
        let fixed = ((1.598633f32 * 32768.0) as i32 + 32768) as u16;
        assert_ne!(
            u16::from_le_bytes([enc[0], enc[1]]),
            fixed,
            "不能退回定点编码"
        );
    }

    /// `RAWPOS` 的**载荷是绝对值**（根骨骼还经过 yaw +90°），
    /// 而**判据是差值是否恒定** —— 两者是不同的量。
    ///
    /// 判据（官方 miku，`probe_rootpos_rule_all.js`，7/7）：
    /// 根骨骼 `bone 0` 的 SMD pos `[-3.0977, -1.5986, -1.8984]`
    /// → 官方 `RAWPOS = 0x3e65 0xc231 0xbf98` = `[1.5986, -3.0957, -1.8984]`
    /// （即 `(-y, x, z)`）。
    #[test]
    fn rawpos_payload_is_absolute_and_root_is_yaw_rotated() {
        // 参考位移**非零**（这是区分「绝对值」与「差值」的关键 ——
        // miku 的 bone 0 参考恰好为 0，掩盖了差异）。
        let c = compiled(
            vec![seq(
                "idle",
                false,
                vec![vec![pose([-3.097656, -1.598633, -1.898438], [0.0; 3])]],
            )],
            1,
        );
        // 显式给一个非零参考位移。
        let refs = [([7.0f32, -2.0, 0.5], [0.0f32; 3])];
        let out = write_animations(&c, &[-1], &refs, 0).expect("写出动画");
        let chain = walk_chain(&out.anim_data, 0);
        let (bone, flags, payload) = chain[0];
        assert_eq!(bone, 0);
        assert_eq!(
            flags & STUDIO_ANIM_RAWPOS,
            STUDIO_ANIM_RAWPOS,
            "应写 RAWPOS"
        );
        // ⚠️ `RAWPOS` 的载荷**不在** `payload` 处 —— 根骨骼的旋转也有
        // 常量偏置（+π/2），所以前面还有一个 `RAWROT2`（8 字节）。
        // 记录布局：`[bone][flags][next:2]` + `RAWROT2?` + `RAWPOS?` + ...
        // （`studio.h` 的 `pPos()` 就是这么算的）。
        let p = payload
            + if flags & STUDIO_ANIM_RAWROT2 != 0 {
                8
            } else {
                0
            }
            + if flags & STUDIO_ANIM_RAWROT != 0 {
                6
            } else {
                0
            };
        let got = [
            u16::from_le_bytes([out.anim_data[p], out.anim_data[p + 1]]),
            u16::from_le_bytes([out.anim_data[p + 2], out.anim_data[p + 3]]),
            u16::from_le_bytes([out.anim_data[p + 4], out.anim_data[p + 5]]),
        ];
        // 绝对值经 yaw +90°：(-y, x, z) = (1.598633, -3.097656, -1.898438)
        assert_eq!(
            got,
            [0x3e65, 0xc231, 0xbf98],
            "RAWPOS 载荷应是**绝对值**且已 yaw 旋转（不是差值）"
        );
    }

    /// **`RAWPOS` 与 `ANIMROT` 不能共存** —— 二者都要占 `pData()`。
    ///
    /// 依据 `studio.h:580,586`：
    /// ```c
    /// pRotV() = pData()                                  // **永远**在 pData()
    /// pPos()  = pData() + RAWROT*6 + RAWROT2*8           // **不含** rotV！
    /// ```
    /// 所以两者同时置位时 `pRotV()` 与 `pPos()` **指向同一处**，
    /// 必有一方被解码成垃圾。官方因此从不这么写。
    ///
    /// 语料判据（`probe_track_layout_strict.js`，3333 个 `.mdl`、
    /// 44978 条**通过校验**的记录）：`RAWPOS|ANIMROT`（`rot=0x8 pos=0x1`）
    /// 出现 **0 次**。反面对照（真 `studiomdl` 的 miku）：
    /// `a_run` 的 `bone 12` 官方 `flags=0x0c`（`ANIMROT|ANIMPOS`）
    /// 记录字节 `0c 0c e8 00 0c 00 50 00 94 00 d2 00 d6 00 da 00`，
    /// 而 mdlc 曾写 `flags=0x09` 并把 `RAWPOS` 塞在 `rotV` 之前 ——
    /// 解码差 **3.62 rad**。
    #[test]
    fn rawpos_never_coexists_with_animrot() {
        // 位移差值恒定（→ 想写 RAWPOS）**且**旋转逐帧变化（→ 要写 ANIMROT）。
        // 注意 `seq()` 收的是「每帧一个 `Vec<SmdPose>`」——8 帧各一根骨骼。
        let frames: Vec<Vec<SmdPose>> = (0..8)
            .map(|i| vec![pose([1.0, 2.0, 3.0], [0.0, 0.0, i as f32 * 0.1])])
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 1);
        let out = write_animations(&c, &[-1], &[([0.0; 3], [0.0; 3])], 0).expect("写出动画");
        let chain = walk_chain(&out.anim_data, 0);
        let (bone, flags, _) = chain[0];
        assert_eq!(bone, 0);
        assert_eq!(
            flags & STUDIO_ANIM_ANIMROT,
            STUDIO_ANIM_ANIMROT,
            "旋转有变化，应写 ANIMROT"
        );
        assert_eq!(
            flags & STUDIO_ANIM_RAWPOS,
            0,
            "ANIMROT 与 RAWPOS 不能共存（二者都占 pData()，会互相覆盖）"
        );
        assert_eq!(
            flags & STUDIO_ANIM_ANIMPOS,
            STUDIO_ANIM_ANIMPOS,
            "应退化为 ANIMPOS"
        );
    }

    /// **核心判据**：根骨骼不动时，整条旋转是常量 → 用 `RAWROT2`。
    ///
    /// 复刻官方最小模型：`flags=0x20` + `Quaternion64` 绕 Z +90°。
    #[test]
    fn still_root_uses_rawrot2_like_official() {
        let c = compiled(
            vec![seq(
                "idle",
                true,
                (0..5).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect(),
            )],
            1,
        );
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let chain = walk_chain(&out.anim_data, 0);
        assert_eq!(chain.len(), 1, "只有 1 根骨骼，链上应只有 1 条记录");
        let (bone, flags, payload) = chain[0];
        assert_eq!(bone, 0);
        assert_eq!(flags, STUDIO_ANIM_RAWROT2, "静止根骨骼应写 RAWROT2");
        assert_eq!(flags & STUDIO_ANIM_ANIMROT, 0, "RAWROT2 与 ANIMROT 互斥");
        // 载荷必须是绕 Z +90° 的四元数：z ≈ 0.7071。
        let bits = u64::from_le_bytes(out.anim_data[payload..payload + 8].try_into().unwrap());
        let mask = (1u64 << 21) - 1;
        let x = (bits & mask) as i64 - 1048576;
        let y = ((bits >> 21) & mask) as i64 - 1048576;
        let z = ((bits >> 42) & mask) as i64 - 1048576;
        assert_eq!(x, 0, "x 应为 0");
        assert_eq!(y, 0, "y 应为 0");
        let qz = z as f64 / 1048576.5;
        assert!(
            (qz - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-5,
            "z = {qz}，应为 +90° 的 sin(45°)"
        );
    }

    /// **端到端**：`RAWROT2` 的载荷必须是**绝对**旋转（参考 + 差值）。
    ///
    /// # 为什么必须是端到端
    ///
    /// 本测试的第一版只断言「公式 `wrap_to_pi(delta + ref)` 等于期望值」——
    /// 那是**纯函数测试**，把 `write_chain_body` 里的载荷改回差值时
    /// **它照样全绿**（变异逃逸）。判据必须落在**写出的字节**上。
    ///
    /// # 夹具设计（关键）
    ///
    /// ⚠️ **参考旋转必须非零**，否则 `ref + delta == delta`，
    /// 两种实现**恒等** ⟹ 又是空洞测试。
    /// 这里让第 0 帧（= 参考姿态）的 roll = **π/2**，
    /// 且三轴差值恒为 0（逐帧同姿态）⟹ 走 `RAWROT2`。
    ///
    /// 于是：
    /// · 正确实现 ⟹ `RAWROT2` = `aq([π/2, 0, 0])`（**参考姿态本身**）
    /// · 旧 bug   ⟹ `RAWROT2` = `aq([0, 0, 0])`（单位四元数）
    ///
    /// 两者相差 **90°**，正是用户 HLMV 观察到的症状。
    #[test]
    fn rawrot2_payload_is_absolute_end_to_end() {
        // 4 帧全同、roll = π/2。参考姿态取自第 0 帧 ⟹ ref_rot = [π/2, 0, 0]。
        let frames: Vec<Vec<SmdPose>> = (0..4)
            .map(|_| vec![pose([0.0; 3], [std::f32::consts::FRAC_PI_2, 0.0, 0.0])])
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let (bone, flags, payload) = walk_chain(&out.anim_data, 0)[0];
        assert_eq!(bone, 0);
        assert_eq!(flags, STUDIO_ANIM_RAWROT2, "逐帧同姿态应写 RAWROT2");

        let bits = u64::from_le_bytes(out.anim_data[payload..payload + 8].try_into().unwrap());
        let mask = (1u64 << 21) - 1;
        let dec = |shift: u32| ((bits >> shift) & mask) as i64 - 1048576;
        let (x, y, z) = (dec(0), dec(21), dec(42));
        let s = 1048576.5f64;

        // 期望 = `aq([π/2, 0, π/2])` —— **参考 roll π/2 ＋ 根骨骼 yaw 偏置 π/2**。
        //
        // ⚠️ 不是 `aq([π/2, 0, 0])`：`delta_frames` 对根骨骼会**额外**加
        // `FRAC_PI_2` 到 Z 轴（`r[2] = wrap_to_pi(r[2] + FRAC_PI_2)`），
        // 因为 `sanim` 已经过 `rootxform`（`simplify.cpp:1461`）。
        // 这与官方 `v_snip_awp` anim[0] b0 的实测完全一致 ——
        // 官方 `RAWROT2` 反解正是 `[π/2, 0, π/2]`（见 `probe_const_dump.js`）。
        let want = crate::bone_math::angle_quaternion([
            std::f32::consts::FRAC_PI_2,
            0.0,
            std::f32::consts::FRAC_PI_2,
        ]);
        let got = [x as f64 / s, y as f64 / s, z as f64 / s];
        for k in 0..3 {
            assert!(
                (got[k] - want[k] as f64).abs() < 1e-4,
                "分量 {k}：得到 {}，应为 {}（参考 roll π/2 + 根 yaw 偏置）",
                got[k],
                want[k]
            );
        }
        // 反证：旧 bug（只写差值 = 只有 yaw 偏置、丢了参考 roll）会给
        // `aq([0, 0, π/2])` = (0, 0, 0.7071, 0.7071) ⟹ x 分量会是 0。
        assert!(
            got[0].abs() > 0.4,
            "x = {} 太小 ⟹ 只写了差值、丢了参考旋转（旧 bug 回来了）",
            got[0]
        );
        // 官方的 `w` 符号位应为 0（正）。
        assert_eq!((bits >> 63) & 1, 0, "w 应为正");
    }

    /// 有骨骼逐帧旋转时走 `ANIMROT`，且恒定轴写偏移 0。
    ///
    /// 复刻官方 `exp50`：绕 **X** 轴（不是 Z —— Z 有根骨骼 +90° 偏置）。
    /// 官方形态：`flags=0x08`、`rotV=[6,0,16]`、X 轴 `04 04`、
    /// Z 轴 `01 04 ff 7f`。
    #[test]
    fn varying_rot_uses_animrot_with_constant_axis_folded() {
        let frames: Vec<Vec<SmdPose>> = (0..4)
            .map(|f| vec![pose([0.0; 3], [f as f32 * 0.174533, 0.0, 0.0])])
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let (_, flags, payload) = walk_chain(&out.anim_data, 0)[0];
        assert_eq!(flags & STUDIO_ANIM_ANIMROT, STUDIO_ANIM_ANIMROT);
        assert_eq!(flags & STUDIO_ANIM_RAWROT2, 0, "旋转有变化时不能用 RAWROT2");

        let d = &out.anim_data;
        let offs = [
            i16::from_le_bytes([d[payload], d[payload + 1]]),
            i16::from_le_bytes([d[payload + 2], d[payload + 3]]),
            i16::from_le_bytes([d[payload + 4], d[payload + 5]]),
        ];
        assert!(offs[0] > 0, "变化的 X 轴必须有流");
        // Y 轴不动 → 偏移 0。Z 轴**不**为 0：根骨骼有 π/2 偏置，
        // 所以它是个「恒定非 0」的轴 → 写 valid=1, total=N 的单条 run。
        assert_eq!(
            offs[1], 0,
            "恒 0 的 Y 轴应写偏移 0（实测 exp50 的 rotV[1]=0）"
        );
        assert!(offs[2] > 0, "根 Z 偏置是恒定非 0 → 也要写流");
        assert_eq!(d[payload + offs[2] as usize], 1, "根 Z 的 valid 应为 1");
        assert_eq!(d[payload + offs[2] as usize + 1], 4, "根 Z 的 total 应为 4");
        let zs = decode_stream(d, payload + offs[2] as usize, 4);
        assert_eq!(zs, vec![32767; 4], "根 Z 应恒为 π/2 的量化值");

        let xs = decode_stream(d, payload + offs[0] as usize, 4);
        assert_eq!(
            xs,
            vec![0, 10922, 21844, 32767],
            "应复刻官方 exp50 的 X 轴采样"
        );
    }

    /// **`RAWROT2` 的 flags 与载荷必须同时存在** —— 声明了载荷就不能一个字节都不写。
    ///
    /// # 这条测试为什么必要
    ///
    /// `RAWROT2` 的「要不要写」与「写什么」来自**两个不同的判据**：
    ///
    /// | | 判据 | 来源 |
    /// |---|---|---|
    /// | 要不要写（`flags`） | 量化后三轴恒定且非零 | `ch.rot`（`fold_axis` 的产物） |
    /// | 写什么（载荷） | **原始**差值逐帧**精确浮点相等** | `ch.rot_const_raw` |
    ///
    /// 两者在「原始差值有极小抖动、但量化后落在同一个整数」时分道扬镳 ——
    /// 于是 `flags` 置了 `RAWROT2`（`studio.h:586` 的 `pPos()` 会据此
    /// 跳过 8 字节），却一个字节都没写。引擎 `pQuat64()` 读到的是
    /// **下一条记录的 4 字节头**，拼出的四元数模长 > 1 ⟹ `w = 0`
    /// ⟹ 退化成一个 **180°** 旋转。
    ///
    /// 实测（`v_silenced_smg` `a_run` `b40`，见 `probe_chain_dump.js`）：
    ///
    /// | | `flags` | `nextoffset` | 载荷 |
    /// |---|---|---|---|
    /// | 官方 | `0x8`（`ANIMROT`） | **72** | 三轴 RLE |
    /// | mdlc（修前） | `0x20`（`RAWROT2`） | **4** | **无** ❌ |
    ///
    /// 夹具的抖动幅度必须**远小于一个量化步长**才落在「量化后恒定」这一侧。
    /// 步长的下限是 `ROT_SCALE_MIN / QUANT_DIVISOR` = `π/8/32767` ≈
    /// **`1.198e-5` rad**（窗口只会被拉伸 ⟹ 实际步长只会更大）。
    ///
    /// ⚠️ **参考姿态必须显式传，不能走 `write_with_f0_refs`。**
    /// 那个 helper 把参考姿态取成**第 0 帧**，于是第 0 帧的差值恒为 0、
    /// 其余帧是 `±2·JITTER` —— 量化后**全是 0** ⟹ 三轴全 `Absent`
    /// ⟹ 根本走不到目标路径。第一版就是这么写的，被「非空洞」硬门当场抓住。
    ///
    /// # 两个分支都要覆盖（这是本测试的第二版）
    ///
    /// 第一版只造了「原始差值有抖动」的夹具，断言它必须走 `RAWROT2` ——
    /// 那**正是旧 bug 的行为**。修好之后它落到 `ANIMROT`，测试反而红了。
    /// 正确做法是把「抖动 ⟹ `ANIMROT`」也写成断言（那才是官方在
    /// `v_silenced_smg` `b40` 上的选择），再另造一个「原始差值精确相等」
    /// 的夹具覆盖 `RAWROT2` 分支。
    ///
    /// 两个分支共同的不变量：**`flags` 声明了载荷就必须真的写出那些字节。**
    #[test]
    fn rawrot2_flag_implies_payload_is_written() {
        // 走一遍链，逐记录检查「flags 声明的载荷大小」与 `nextoffset` 自洽。
        let check = |out: &AnimWriteOutcome, what: &str| -> Vec<(u8, u8, usize)> {
            let chain = walk_chain(&out.anim_data, 0);
            assert!(!chain.is_empty(), "{what}：夹具非空（至少要有一条记录）");
            let d = &out.anim_data;
            for (i, &(bone, flags, payload)) in chain.iter().enumerate() {
                let need =
                    4 + if flags & STUDIO_ANIM_RAWROT2 != 0 {
                        8
                    } else {
                        0
                    } + if flags & STUDIO_ANIM_RAWROT != 0 {
                        6
                    } else {
                        0
                    } + if flags & STUDIO_ANIM_RAWPOS != 0 {
                        6
                    } else {
                        0
                    } + if flags & STUDIO_ANIM_ANIMROT != 0 {
                        6
                    } else {
                        0
                    } + if flags & STUDIO_ANIM_ANIMPOS != 0 {
                        6
                    } else {
                        0
                    };
                let next = i16::from_le_bytes([d[payload - 2], d[payload - 1]]);
                if next == 0 {
                    continue; // 末条：`nextoffset` 置 0 是官方约定
                }
                // `ANIMROT`/`ANIMPOS` 后面还跟着各轴的流 ⟹ `next` 只会
                // **大于等于** `need`；`RAWROT2`/`RAWPOS` 无流时应当相等。
                assert!(
                    next as usize >= need,
                    "{what} 记录 {i}（bone {bone}，flags=0x{flags:02x}）：`nextoffset`={next} \
                     小于载荷所需 {need} 字节 ⟹ flags 声明了载荷却没写"
                );
                if flags & (STUDIO_ANIM_ANIMROT | STUDIO_ANIM_ANIMPOS) == 0 {
                    assert_eq!(
                        next as usize, need,
                        "{what} 记录 {i}（bone {bone}，flags=0x{flags:02x}）：无流时 \
                         `nextoffset` 应恰好等于载荷大小 {need}"
                    );
                }
            }
            chain
        };

        // ---- 分支 A：原始差值**有**抖动（量化后仍恒定）⟹ 必须走 `ANIMROT` ----
        //
        // 这就是官方 `v_silenced_smg` `a_run` `b40` 的形态（原始 `rot[1]`
        // 抖动 `1.2e-3°`，量化后恒定）。官方在那里写的是 `ANIMROT`，
        // 因为 `RAWROT2` 的载荷要求「一个**确定的**常量旋转」，而抖动的
        // 差值取哪一帧都不对。
        //
        // ⚠️ **夹具必须有 ≥ 2 根骨骼。** 只有 1 根时它是链上唯一（也是最后）
        // 一条记录，`nextoffset` 按官方约定被置 0 —— 而 `check` 会跳过
        // `nextoffset == 0` 的记录 ⟹ **变异逃逸**。第一版就是 1 根骨骼，
        // 把修复回退成旧代码后测试**照样全绿**。
        //
        // ⚠️ **抖动幅度必须相对 `rotscale` 足够小，而 `rotscale` 由极值决定。**
        // 第二版让 bone0 自己既提供极值又提供抖动：极值 `0.3` ⟹
        // `rotscale = 0.3/32767`，于是 `0.3/rotscale` 恰好压在桶边界
        // `32767.0` 上，`−1e-7` 一侧截断成 `32766` ⟹ 量化后**不恒定**
        // ⟹ 走 `ANIMROT`、**测不到目标路径**（变异逃逸，第二次）。
        // 正确做法：**另用一条序列把极值抬高**（`1.0`），让 `0.3` 落在
        // 桶中间。此时 `rotscale = 1.0/32767`，抖动 `1e-7` 只占
        // `0.0033` 个桶 ⟹ 量化后恒定。
        const BASE: f32 = 0.3;
        const BIG: f32 = 1.0;
        const JITTER: f32 = 1e-7;
        // bone0 抖动；bone1 精确相等（保证它是一条真实的、有载荷的记录）。
        let jittery: Vec<Vec<SmdPose>> = (0..6)
            .map(|f| {
                let d = BASE + if f % 2 == 0 { JITTER } else { -JITTER };
                vec![
                    pose([0.0; 3], [d, d, d]),
                    pose([0.0; 3], [BASE, BASE, BASE]),
                ]
            })
            .collect();
        // 第二条序列：只用来**抬高极值**（`rotscale` 是全局一份）。
        let scaler: Vec<Vec<SmdPose>> = (0..6)
            .map(|_| {
                vec![
                    pose([0.0; 3], [BIG, BIG, BIG]),
                    pose([0.0; 3], [BASE, BASE, BASE]),
                ]
            })
            .collect();
        let c = compiled(
            vec![seq("idle", false, jittery), seq("run", false, scaler)],
            2,
        );
        // 参考姿态 = 原点（**不是**第 0 帧），理由见上。
        let refs2 = [([0.0f32; 3], [0.0f32; 3]), ([0.0f32; 3], [0.0f32; 3])];
        let out = write_animations(&c, &[-1, 0], &refs2, 0).expect("写出动画");
        let chain = check(&out, "抖动夹具");
        assert!(
            chain.len() >= 2,
            "夹具应有 ≥ 2 条记录（否则 `nextoffset` 恒为 0、测不到自洽性）"
        );
        // ⚠️ 断言必须**按骨骼**，不能「全部记录都不得是 `RAWROT2`」——
        // bone1 是精确常量，它**应该**走 `RAWROT2`（那是正确行为）。
        // 第一版写成 `chain.iter().all(...)`，于是被 bone1 误伤。
        let b0 = chain
            .iter()
            .find(|&&(b, _, _)| b == 0)
            .expect("bone0 必须在链上（它抖动 ⟹ 有数据）");
        assert_eq!(
            b0.1 & STUDIO_ANIM_RAWROT2,
            0,
            "bone0 原始差值有抖动 ⟹ 不能写 `RAWROT2`（官方在此形态下写 `ANIMROT`）"
        );
        assert_eq!(
            b0.1 & STUDIO_ANIM_ANIMROT,
            STUDIO_ANIM_ANIMROT,
            "bone0 应退化为 `ANIMROT`"
        );

        // ---- 分支 B：原始差值**精确相等**且非零 ⟹ `RAWROT2` + 8 字节载荷 ----
        let exact: Vec<Vec<SmdPose>> = (0..6)
            .map(|_| {
                vec![
                    pose([0.0; 3], [BASE, BASE, BASE]),
                    pose([0.0; 3], [BASE, BASE, BASE]),
                ]
            })
            .collect();
        let c2 = compiled(vec![seq("idle", false, exact)], 2);
        let out2 = write_animations(&c2, &[-1, 0], &refs2, 0).expect("写出动画");
        let chain2 = check(&out2, "精确相等夹具");
        let (bone2, flags2, payload2) = chain2[0];
        assert_eq!(bone2, 0);
        assert_eq!(
            flags2 & STUDIO_ANIM_RAWROT2,
            STUDIO_ANIM_RAWROT2,
            "原始差值精确相等 ⟹ 应写 `RAWROT2`（否则分支 B 是空洞的）"
        );
        // 两条记录时 `nextoffset` 有真值 ⟹ 可以精确断言记录长度。
        let next2 =
            i16::from_le_bytes([out2.anim_data[payload2 - 2], out2.anim_data[payload2 - 1]]);
        assert_eq!(
            next2, 12,
            "`RAWROT2` 记录总长应为 4（头）+ 8（`Quaternion64`）= 12 \
             （4 说明 flags 声明了载荷却没写）"
        );
        // 且四元数必须是**有效**的（模长 1，`w != 0`）—— 旧 bug 下
        // `pQuat64()` 读到的是下一条记录的头（或对齐填充），模长 ≥ 1 ⟹ `w = 0`。
        let bits = u64::from_le_bytes(out2.anim_data[payload2..payload2 + 8].try_into().unwrap());
        let mask = (1u64 << 21) - 1;
        let q = |shift: u32| (bits >> shift & mask) as f64 / 1048576.5 - 1.0;
        let (qx, qy, qz) = (q(0), q(21), q(42));
        let sum = qx * qx + qy * qy + qz * qz;
        assert!(
            sum < 1.0,
            "四元数模长平方 {sum} 应 < 1（`w` 才有实数解）；\
             ≥ 1 说明读到了别的字节（旧 bug：载荷未写）"
        );
        assert!(
            sum > 0.5,
            "四元数模长平方 {sum} 太小 ⟹ 载荷是全 0 或垃圾字节"
        );
    }

    /// 静止的非根骨骼完全不进链（姿态回落到骨骼表）。
    #[test]
    fn still_bone_stays_out_of_chain() {
        let c = compiled(
            vec![seq(
                "idle",
                false,
                (0..4).map(|_| vec![pose([0.0; 3], [0.0; 3]); 2]).collect(),
            )],
            2,
        );
        let out = write_with_f0_refs(&c, &[-1, 0]).expect("写出动画");
        let chain = walk_chain(&out.anim_data, 0);
        let bones: Vec<u8> = chain.iter().map(|r| r.0).collect();
        assert_eq!(bones, vec![0], "只有根骨骼进链（bone1 静止 → 不进）");
        assert_eq!(out.stats[0].animated_bones, 1);
    }

    /// 链尾必须有一条 4 字节全零记录，且末条记录的 `nextoffset == 0`。
    #[test]
    fn chain_tail_is_four_zero_bytes() {
        let c = compiled(
            vec![seq(
                "idle",
                false,
                (0..3).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect(),
            )],
            1,
        );
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let end = out.anim_data.len();
        assert_eq!(&out.anim_data[end - 4..], &[0u8; 4], "链尾应是 4 字节全零");
        let (_, _, payload) = walk_chain(&out.anim_data, 0)[0];
        let next = i16::from_le_bytes([out.anim_data[payload - 2], out.anim_data[payload - 1]]);
        assert_eq!(next, 0, "末条记录的 nextoffset 应为 0");
    }

    /// `nextoffset` 相对**自身**：第一条记录指向第二条。
    #[test]
    fn nextoffset_is_relative_to_self() {
        let frames: Vec<Vec<SmdPose>> = (0..3)
            .map(|f| {
                vec![
                    pose([0.0; 3], [0.0; 3]),
                    pose([0.0; 3], [0.0, 0.0, f as f32 * 0.2]),
                ]
            })
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 2);
        let out = write_with_f0_refs(&c, &[-1, 0]).expect("写出动画");
        let chain = walk_chain(&out.anim_data, 0);
        assert_eq!(chain.len(), 2, "两条记录");
        let next = i16::from_le_bytes([out.anim_data[2], out.anim_data[3]]);
        assert_eq!(
            next as usize,
            chain[1].2 - 4,
            "nextoffset 应等于「第二条起点 − 第一条起点」"
        );
    }

    /// 缩放写进 bone 表，且是**全局一份**：跨序列取最大值。
    ///
    /// 这条同时钉住「两趟扫描」的必要性 —— 若边扫边量化，第一个序列会用
    /// 自己的小 scale 量化，之后第二个序列抬高 scale，前者的整数就作废。
    ///
    /// 用 **X** 轴（不是 Z —— 根骨骼的 Z 有 +90° 偏置，会盖住被测的差异）。
    #[test]
    fn scales_are_global_across_sequences() {
        let small = seq(
            "small",
            false,
            (0..3)
                .map(|f| vec![pose([0.0; 3], [f as f32 * 0.01, 0.0, 0.0])])
                .collect(),
        );
        let big = seq(
            "big",
            false,
            (0..3)
                .map(|f| vec![pose([0.0; 3], [f as f32 * 1.0, 0.0, 0.0])])
                .collect(),
        );
        let c = compiled(vec![small, big], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let (_, rot) = out.bone_scales[0];
        // big 的 maxAbs = 2.0 rad > π/8 下限 → 全局取 2.0。
        let expect = 2.0f32 / QUANT_DIVISOR;
        assert!(
            (rot[0] - expect).abs() < 1e-9,
            "rotscale[0] = {}，应为 {}（跨序列最大值）",
            rot[0],
            expect
        );
        // 小序列的采样必须用**同一个** scale 量化 → 末帧（2×0.01=0.02）
        // 应量化为 trunc(0.02 / scale)。若用了它自己的小 scale，这个值
        // 会大 100 倍。
        let s = decode_stream(
            &out.anim_data,
            {
                let (_, _, payload) = walk_chain(&out.anim_data, out.anim_offsets[0])[0];
                let d = &out.anim_data;
                let off = i16::from_le_bytes([d[payload], d[payload + 1]]);
                payload + off as usize
            },
            3,
        );
        let expect_q = (0.02f32 / expect).trunc() as i16;
        assert_eq!(
            s[2], expect_q,
            "小序列的采样必须用全局 scale 量化（值 {} vs 期望 {}）",
            s[2], expect_q
        );
    }

    /// **没有「根骨骼 Z 用 π/2 下限」这条规则** —— `rotscale` 是
    /// 「窗口被实际差值撑开」的结果，不是查表。
    ///
    /// 曾经有过 `ROOT_Z_ROT_SCALE_MIN`，推理是「根骨骼有无条件的 +90°
    /// 偏置，所以 Z 轴极值至少 π/2」。它在 `blend1`/`blend2`/`blend3` 上
    /// **全部正确** —— 那些 fixture 的根骨骼自身旋转是 `[0,0,0]`，
    /// 差值确实恰好是 π/2。
    ///
    /// 真 `studiomdl.exe` 编译 miku 时证伪了它：根骨骼 SMD 旋转是
    /// `[π/2, 0, −π/2]`、参考是 `[π/2, 0, 0]`，偏置与 `−π/2` **抵消**，
    /// 差值只剩 `4.77e-7` 的浮点残渣 → 官方 `rotscale` 三轴全 **π/8**。
    ///
    /// 本测试用 miku 的形状，断言三轴都落到 π/8。旧规则会给出 Z = π/2。
    ///
    /// ⚠️ **关键**：`ref` 必须**不等于**第 0 帧 —— 这正是 miku 的情形
    /// （`ref` 来自 `$definebone` 的 `[π/2,0,0]`，SMD 帧是 `[π/2,0,−π/2]`）。
    /// 所以这里不能用 `write_with_f0_refs`（它把 ref 取成第 0 帧），
    /// 必须直接调 `write_animations` 传显式参考姿态。
    #[test]
    fn root_z_scale_is_pi_over_8_when_bias_cancels() {
        // miku 的形状：参考 `[π/2, 0, 0]`（`$definebone`），
        // SMD 帧 `[π/2, 0, −π/2]`（弧度，实测 `a_idle.smd`）。
        let smd_rot = [
            std::f32::consts::FRAC_PI_2,
            0.0,
            -std::f32::consts::FRAC_PI_2,
        ];
        let c = compiled(
            vec![seq(
                "idle",
                false,
                vec![vec![pose([0.0; 3], smd_rot)], vec![pose([0.0; 3], smd_rot)]],
            )],
            1,
        );
        // 参考姿态：X = π/2，**Z = 0**（与第 0 帧的 −π/2 不同）。
        let refs = [([0.0f32; 3], [std::f32::consts::FRAC_PI_2, 0.0, 0.0])];
        let out = write_animations(&c, &[-1], &refs, 0).expect("写出动画");
        let (pos, rot) = out.bone_scales[0];
        let expect = ROT_SCALE_MIN / QUANT_DIVISOR;
        for (k, r) in rot.iter().enumerate() {
            assert!(
                (r - expect).abs() < 1e-9,
                "偏置抵消后根骨骼旋转轴 {k} 的 rotscale 应为 π/8/32767（{expect}），\
                 实际 {r} —— 若 Z 轴是 π/2/32767，说明「根骨骼 Z 下限」规则又回来了"
            );
        }
        let expect_p = POS_SCALE_MIN / QUANT_DIVISOR;
        for (k, p) in pos.iter().enumerate() {
            assert!((p - expect_p).abs() < 1e-9, "位移轴 {k} 窗口初值应为 128");
        }
    }

    // ---- 位移 ----

    /// **`RAWROT2` 必须给 `ANIMPOS` 让位**（`pQuat64()` 与 `pPosV()` 同址）。
    ///
    /// 本测试原先断言 `flags == RAWROT2|ANIMPOS`（`0x24`），并把位移 valueptr
    /// 放在 `payload + 8`。那是**把 bug 固化成了规格**：`studio.h:574-586`
    /// 里 `pPosV() = pData() + ANIMROT*6` —— **不含 `RAWROT2`**。所以
    /// `RAWROT2|ANIMPOS`（无 `ANIMROT`）时 `pPosV()` 落在 `pData()+0`，
    /// 与 `pQuat64()` **同一地址**，引擎把 `Quaternion64` 的 8 字节当成
    /// `mstudioanim_valueptr_t.offset[3]` 读。
    ///
    /// 官方 studiomdl 与 NekoMDL 在全部 40 个动画里各写 **0 条**
    /// `0x24`/`0x34`；mdlc 修前写 **50 条**（13 个动画，含 `a_idle_1`/`a_run`），
    /// 引擎口径解码位移偏差最大 **1.62e+2**（对照组仅 `6.77e-3`）。
    ///
    /// 让位方向由官方实测决定（`docs\_probe\diag_flags4way.js`，50/50 条）：
    /// 位移**不是**常量时只能 `ANIMPOS` ⟹ 旋转降级成 `ANIMROT` → `0x0c`；
    /// 位移**是**常量时 `RAWPOS` 放得下（`pPos() = pData()+8`）⟹ 保留
    /// `RAWROT2` → `0x21`。
    #[test]
    fn varying_pos_uses_animpos_and_writes_pos_after_rot() {
        let frames: Vec<Vec<SmdPose>> = (0..3)
            .map(|f| vec![pose([0.0, 0.0, f as f32 * 5.0], [0.0; 3])])
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let (_, flags, payload) = walk_chain(&out.anim_data, 0)[0];
        // 旋转恒定但位移变化 ⟹ 旋转必须让位：RAWROT2 降级为 ANIMROT。
        assert_eq!(
            flags,
            STUDIO_ANIM_ANIMROT | STUDIO_ANIM_ANIMPOS,
            "位移变化时 RAWROT2 必须让位（pPosV 与 pQuat64 同址）"
        );
        assert_eq!(flags & STUDIO_ANIM_RAWROT2, 0, "不得出现 RAWROT2|ANIMPOS");
        // ANIMROT 占 6 字节，位移 valueptr 紧随其后。
        let pos_vp = payload + VALUEPTR_SIZE;
        let d = &out.anim_data;
        let offs = [
            i16::from_le_bytes([d[pos_vp], d[pos_vp + 1]]),
            i16::from_le_bytes([d[pos_vp + 2], d[pos_vp + 3]]),
            i16::from_le_bytes([d[pos_vp + 4], d[pos_vp + 5]]),
        ];
        assert_eq!(offs[0], 0, "X 不动 → 偏移 0");
        assert_eq!(offs[1], 0, "Y 不动 → 偏移 0");
        assert!(offs[2] > 0, "Z 变化 → 有流");
        let zs = decode_stream(d, pos_vp + offs[2] as usize, 3);
        assert!(zs[2] > zs[1] && zs[1] > zs[0], "位移应单调递增：{zs:?}");
    }

    /// **位移是常量时 `RAWROT2` 可以保留**（另一侧的让位方向）。
    ///
    /// 官方在 `docs\_probe\diag_flags4way.js` 的 50 条里，29 条选 `0x21`
    /// （`RAWPOS|RAWROT2`）—— 因为位移常量走 `RAWPOS`，而
    /// `pPos() = pData() + RAWROT2*8` 恰好**跳过**了 `Quaternion64`，
    /// 两个常量互不重叠。
    ///
    /// ⚠️ 参考姿态必须**不同于**帧 0，否则差值为 0 ⟹ `pos` 三轴全 `Absent`
    /// ⟹ 走的是「无位移数据」分支而不是常量分支。
    #[test]
    fn constant_pos_keeps_rawrot2_and_writes_rawpos_after_quat64() {
        // 三帧位移全同（差值恒定且非零），旋转也恒定且**非零**。
        let frames: Vec<Vec<SmdPose>> = (0..3)
            .map(|_| vec![pose([1.0, 2.0, 3.0], [0.0, 0.0, 1.5])])
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 1);
        // 参考位移取 (0,0,0) ⟹ 差值 = (1,2,3)，恒定且非零。
        let refs = [([0.0f32; 3], [0.0f32; 3])];
        let out = write_animations(&c, &[-1], &refs, 0).expect("写出动画");
        let (_, flags, payload) = walk_chain(&out.anim_data, 0)[0];
        assert_eq!(
            flags,
            STUDIO_ANIM_RAWROT2 | STUDIO_ANIM_RAWPOS,
            "位移常量 ⟹ RAWPOS 放得下，RAWROT2 保留"
        );
        // 载荷顺序固定：RAWROT2(8) → RAWPOS(6) ⟹ 至少 14 字节。
        assert!(payload + 14 <= out.anim_data.len(), "载荷不得越界");
    }

    // ---- 段（`mstudioanimsections_t`）----

    /// **段数只由「本动画的帧数 + sectionframes」决定。**
    ///
    /// 判据来自 `v_dual_pistola` 的 R 变体：那里 `$sequence "idle"` 引用
    /// **72 帧**的 `a_run`，而同一条 QC 里的 `a_idle_1` 是 **230 帧**。
    /// 若段数取自序列（`Sequence::num_sections = 72/30+2 = 4`），
    /// `a_idle_1` 的段表就会**短写 5 条**，后面的条目落到链数据里，
    /// 读出来是垃圾（实测 `{blk=1188096,idx=-6815748}`）。
    #[test]
    fn section_count_uses_animation_frames_not_sequence() {
        // 同一个 `sf`，两个帧数 ⟹ 两个段数。
        assert_eq!(section_count(230, 30), 9, "230/30+2 = 9");
        assert_eq!(section_count(72, 30), 4, "72/30+2 = 4");
        assert_eq!(section_count(40, 30), 3, "40/30+2 = 3");
        assert_eq!(section_count(120, 30), 6, "120/30+2 = 6");
        // 不分段 / 空动画 ⟹ 0。
        assert_eq!(section_count(230, 0), 0, "sf=0 ⟹ 不分段");
        assert_eq!(section_count(0, 30), 0, "无帧 ⟹ 0");
    }

    #[test]
    fn last_section_holds_final_frame_even_when_frame_count_is_divisible() {
        assert_eq!(section_frame_range(8, 9, 230, 30), Some((229, 229)));
        assert_eq!(section_frame_range(3, 4, 72, 30), Some((71, 71)));
        assert_eq!(section_frame_range(5, 6, 120, 30), Some((119, 119)));
    }

    /// **非末段的帧区间是闭区间 `[k*sf, (k+1)*sf]`**（两端含，`sf+1` 帧）。
    #[test]
    fn non_final_sections_use_closed_interval() {
        // nf=230 sf=30：段 0 = [0,30]（31 帧），段 1 = [30,60]（31 帧）。
        assert_eq!(section_frame_range(0, 9, 230, 30), Some((0, 30)));
        assert_eq!(section_frame_range(1, 9, 230, 30), Some((30, 60)));
        // 段 7 的 `hi` 被裁到 `nf-1 = 229`。
        assert_eq!(section_frame_range(7, 9, 230, 30), Some((210, 229)));
        // 空段（`lo > hi`）→ None。nf=60 sf=30 的段 2 = [60, 59] ⟹ None。
        assert_eq!(section_frame_range(2, 4, 60, 30), None, "lo=60 > hi=59");
    }

    #[test]
    fn sections_cover_every_engine_readable_frame() {
        for (nf, sf) in [
            (230usize, 30i32),
            (72, 30),
            (40, 30),
            (120, 30),
            (121, 30),
            (60, 30),
            (31, 30),
        ] {
            let n_sec = section_count(nf, sf);
            for frame in 0..nf {
                let section = if nf > sf as usize && frame == nf - 1 {
                    nf / sf as usize + 1
                } else {
                    frame / sf as usize
                };
                let local = if frame == nf - 1 && nf > sf as usize {
                    0
                } else {
                    frame % sf as usize
                };
                let (lo, hi) = section_frame_range(section, n_sec, nf, sf).unwrap();
                assert!(lo + local <= hi);
                assert_eq!(lo + local, frame, "nf={nf} sf={sf} frame={frame}");
            }
        }
    }

    /// **`section_rot_const`：旋转常量载荷必须逐段算。**
    ///
    /// # 为什么这条必须单独测
    ///
    /// 这个函数只影响「单帧末段」的编码：那段够不到 `ANIMROT`（要求
    /// `n > 1`），所以一旦这里返回 `None`，该骨骼的**旋转就整个丢失**
    /// （官方写 `0x21`，mdlc 会写 `0x01`）。
    ///
    /// 实测：把本函数改成恒返回 `None`（= 沿用整条动画的结论）后，
    /// **全部单元测试仍然全绿** —— 只有对真实产物的逐帧姿态探针
    /// （`probe_anim_pose_sections.js`）才抓得到（末帧 48/119 根骨骼差，
    /// 最大 **101.79°**）。所以这里补上直接判据。
    #[test]
    fn section_rot_const_is_computed_per_section() {
        // 一条「整条动画旋转**不恒定**、但**末段（单帧）恒定**」的数据 ——
        // 这正是 `a_idle_1` 的形态。
        let raw: Vec<[f32; 3]> = vec![
            [0.0, 0.0, 0.1],
            [0.0, 0.0, 0.2],
            [0.0, 0.0, 0.3],
            [0.5, 0.0, 0.3], // ← 末帧，与前一帧不同 ⟹ 整条不恒定
        ];
        let ref_rot = [0.0, 0.0, 1.0];

        // 整条（lo=0, hi=3）不恒定 ⟹ None。
        assert_eq!(
            section_rot_const(Some(&raw), 0, 3, false, Some(&ref_rot)),
            None,
            "整条动画旋转不恒定 ⟹ 不能写 RAWROT2"
        );
        // **末段**（lo=hi=3，单帧）恒定 ⟹ `Some(差值 + 参考)`。
        assert_eq!(
            section_rot_const(Some(&raw), 3, 3, false, Some(&ref_rot)),
            Some([0.5, 0.0, 1.3]),
            "单帧段必然恒定 ⟹ 必须给出常量载荷（否则旋转整个丢失）"
        );
        // [1,2] 是 0.2 与 0.3 ⟹ 不恒定。
        assert_eq!(
            section_rot_const(Some(&raw), 1, 2, false, Some(&ref_rot)),
            None,
            "[0.2, 0.3] 不恒定 ⟹ None"
        );
        // `DELTA` 动画：载荷就是差值本身，不加参考。
        assert_eq!(
            section_rot_const(Some(&raw), 3, 3, true, Some(&ref_rot)),
            Some([0.5, 0.0, 0.3]),
            "DELTA 动画的 sanim 本身就是增量，引擎不加参考"
        );
        // 没有原始帧 / 越界 ⟹ None（不 panic）。
        assert_eq!(section_rot_const(None, 0, 0, false, Some(&ref_rot)), None);
        assert_eq!(
            section_rot_const(Some(&raw), 0, 99, false, Some(&ref_rot)),
            None
        );
    }

    // ---- 无序列 ----

    #[test]
    fn no_sequences_produces_no_anim_sections() {
        let c = compiled(Vec::new(), 2);
        let out = write_with_f0_refs(&c, &[-1, 0]).expect("写出动画");
        assert!(out.anim_data.is_empty());
        assert!(out.animdescs.is_empty());
        assert!(out.seqdescs.is_empty());
        assert!(out.stats.is_empty());
    }

    #[test]
    fn animation_bone_254_survives_the_sdk_terminator_rule() {
        let count = 255;
        let frames = (0..3)
            .map(|frame| {
                (0..count)
                    .map(|bone| SmdPose {
                        bone: bone as i32,
                        position: [frame as f32, 0.0, 0.0],
                        rotation: [0.0; 3],
                    })
                    .collect()
            })
            .collect();
        let mut sequence = seq("boundary", false, frames);
        sequence.weights = vec![1.0; count];
        let c = compiled(vec![sequence], count);
        let out = write_with_f0_refs(&c, &vec![-1; count]).unwrap();
        let mut offset = 0;
        let mut bones = Vec::new();
        while out.anim_data[offset] < 255 {
            bones.push(out.anim_data[offset]);
            let next =
                i16::from_le_bytes(out.anim_data[offset + 2..offset + 4].try_into().unwrap());
            if next == 0 {
                break;
            }
            assert!(next > 0);
            offset += next as usize;
            assert!(bones.len() <= count);
        }
        assert_eq!(bones.len(), count);
        assert_eq!(bones.last(), Some(&254));
    }

    #[test]
    fn animation_bone_255_is_rejected_instead_of_becoming_a_terminator() {
        let count = 256;
        let c = compiled(
            vec![seq(
                "boundary",
                false,
                vec![vec![pose([0.0; 3], [0.0; 3]); count]],
            )],
            count,
        );
        assert!(matches!(
            write_with_f0_refs(&c, &vec![-1; count]),
            Err(AnimWriteError::TooManyBones { count: 256 })
        ));
    }

    // ---- 空链 ----

    /// 完全没有动画时写 `numbones` 条 `ff 00 00 00` 占位。
    #[test]
    fn empty_chain_writes_bone255_placeholders() {
        // 3 根骨骼、每根都完全不动，且都**不是**根 → 没有 π/2 偏置
        // → 全部折叠成 Absent → 空链。
        let frames: Vec<Vec<SmdPose>> = (0..3)
            .map(|_| (0..3).map(|_| pose([0.0; 3], [0.0; 3])).collect())
            .collect();
        let c = compiled(vec![seq("idle", false, frames)], 3);
        // 传 [-1, 0, 1] 会让 bone0 成为根（有偏置）。这里故意让所有骨骼
        // 都**有父**，即骨架子表非法，但 write_animations 只看 parents。
        let out = write_with_f0_refs(&c, &[-1, 0, 1]).expect("写出动画");
        // bone0 是根 → 有 π/2 偏置 → 进链。所以这里至少 1 条。
        assert!(out.stats[0].animated_bones >= 1);
        // 真正空链的情形：把 bone0 也当成非根。
        let out2 = write_with_f0_refs(&c, &[0, 0, 1]).expect("写出动画");
        assert_eq!(out2.stats[0].animated_bones, 0, "全部静止 → 空链");
        assert_eq!(
            out2.anim_data.len(),
            3 * 4,
            "空链应写 numbones 条 4 字节占位"
        );
        for r in out2.anim_data.chunks(4) {
            assert_eq!(r, &[0xFF, 0x00, 0x00, 0x00], "占位记录应是 ff 00 00 00");
        }
    }

    // ---- 描述符 ----

    #[test]
    fn animdesc_and_seqdesc_have_documented_offsets() {
        let c = compiled(
            vec![seq(
                "idle",
                true,
                (0..5).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect(),
            )],
            1,
        );
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        assert_eq!(out.animdescs.len(), ANIMDESC_SIZE);
        assert_eq!(out.seqdescs.len(), SEQDESC_SIZE);

        let g = |b: &[u8], o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        // animdesc：fps @0x08、flags @0x0C、numframes @0x10
        assert_eq!(
            f32::from_le_bytes(out.animdescs[0x08..0x0C].try_into().unwrap()),
            30.0
        );
        assert_eq!(g(&out.animdescs, 0x0C), STUDIO_LOOPING);
        assert_eq!(g(&out.animdescs, 0x10), 5);

        // seqdesc：flags @0x0C、activity @0x10、numblends @0x38。
        assert_eq!(g(&out.seqdescs, 0x0C), STUDIO_LOOPING);
        assert_eq!(g(&out.seqdescs, 0x10), -1, "未指定 activity 应写 -1");
        assert_eq!(g(&out.seqdescs, 0x38), 1);
        // `fadein`/`fadeout` 缺省 **0.2**（`studiomdl.cpp:2639-2640`）。
        assert_eq!(g(&out.seqdescs, 0x68), 0.2f32.to_bits() as i32);
        assert_eq!(g(&out.seqdescs, 0x6C), 0.2f32.to_bits() as i32);

        // ---- 子表布局：复刻 `write.cpp:427-641` 的**全局游标**模型 ----
        //
        // 每个偏移字段的值 = 「当时的 `pData` − 本记录地址」。
        // 单动画序列（gs=1x1、无 events、无 autolayer、1 骨骼）的
        // 子表区因此是：
        //
        //   rel 0   : weightlist（1 个 float = 1.0）   ← **不是**空表
        //   rel 4   : iklock（长度 0）
        //   rel 4   : blend（1 个 int16）+ 2 字节对齐
        //   rel 8   : keyvalue（长度 0）
        //
        // ⚠️ 早先这里断言 weightlist/autolayer/posekey 都是「空表标记」
        // `SEQDESC_SIZE` —— 那是**错的**：官方只对**没有写**的 posekey
        // 留 0，weightlist 则**每条序列都真写** `g_numbones` 个 float。
        let base = SEQDESC_SIZE as i32;
        // 没有 events ⇒ eventindex 就是「当时的游标」= 子表区起点 = 212。
        assert_eq!(g(&out.seqdescs, 0x1C), base, "无 events → 游标仍在起点");
        // weightlist 在子表区偏移 0 处（第一条序列），内容全 1。
        assert_eq!(
            g(&out.seqdescs, 0x9C),
            base + out.weightlist_offsets[0] as i32,
            "weightlistindex 应指向真实块"
        );
        assert_eq!(out.weightlist_offsets[0], 0, "第一条序列在子表区偏移 0");
        assert_eq!(
            f32::from_le_bytes(out.seq_subtables[0..4].try_into().unwrap()),
            1.0,
            "无 $weightlist ⇒ 权重恒为 1"
        );
        // posekey **没有写** ⇒ 字段保持 `memset` 的 0（不是空表标记）。
        assert_eq!(g(&out.seqdescs, 0xA0), 0, "未写 posekey → 0");
        // autolayer 表在游标处、长度 0（没有 autolayer）。
        assert_eq!(
            g(&out.seqdescs, 0x98),
            base + out.autolayer_offsets[0] as i32
        );
        // iklock / keyvalue 指向子表区内的实际位置。
        assert_eq!(g(&out.seqdescs, 0xA8), base + out.iklock_offsets[0] as i32);
        assert_eq!(
            g(&out.seqdescs, 0xAC),
            base + out.keyvalue_offsets[0] as i32
        );
        // blend 表排在 weightlist 之后。
        assert_eq!(g(&out.seqdescs, 0x3C), base + out.blend_offsets[0] as i32);
        assert_eq!(out.blend_offsets[0], 4, "blend 排在 weightlist(4B) 之后");
        // numiklocks / keyvaluesize 都是 0。
        assert_eq!(g(&out.seqdescs, 0xA4), 0);
        assert_eq!(g(&out.seqdescs, 0xB0), 0);
        // 子表区至少含 1 个 blend 项（int16）。
        assert!(out.seq_subtables.len() >= 2);
    }

    /// 有 events 时：numevents / eventindex 正确，事件记录按 80 字节布局，
    /// `options` 内联、**名字是相对偏移**（进字符串池）。
    #[test]
    fn events_are_written_as_80_byte_records() {
        let mut s = seq(
            "idle",
            true,
            (0..5).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect(),
        );
        s.events = vec![
            SequenceEvent {
                cycle: 0.25,
                event_type: 1024,
                event: 1001,
                name: "AE_FOOTSTEP_RIGHT".into(),
                options: "left".into(),
            },
            SequenceEvent {
                cycle: 1.0,
                event_type: 0,
                event: 1002,
                name: "AE_SOUND".into(),
                options: String::new(),
            },
        ];
        let c = compiled(vec![s], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let g = |o: usize| i32::from_le_bytes(out.seqdescs[o..o + 4].try_into().unwrap());
        assert_eq!(g(0x18), 2, "numevents 应为 2");
        let ev_rel = g(0x1C);
        assert!(ev_rel >= SEQDESC_SIZE as i32, "eventindex 应指向子表区");
        let ev_off = ev_rel as usize - SEQDESC_SIZE;
        assert_eq!(out.event_offsets[0], Some(ev_off));
        // 记录 0：cycle=0.25, event=1001, type=1024, options 内联。
        let d = &out.seq_subtables;
        assert_eq!(
            f32::from_le_bytes(d[ev_off..ev_off + 4].try_into().unwrap()),
            0.25
        );
        assert_eq!(
            i32::from_le_bytes(d[ev_off + 4..ev_off + 8].try_into().unwrap()),
            1001
        );
        assert_eq!(
            i32::from_le_bytes(d[ev_off + 8..ev_off + 12].try_into().unwrap()),
            1024
        );
        let opt_at = ev_off + 0x0C;
        let end = d[opt_at..ev_off + EVENT_SIZE]
            .iter()
            .position(|&x| x == 0)
            .unwrap();
        assert_eq!(&d[opt_at..opt_at + end], b"left");
        // 记录 1 紧随其后，间隔正好 80 字节。
        let e2 = ev_off + EVENT_SIZE;
        assert_eq!(
            i32::from_le_bytes(d[e2 + 4..e2 + 8].try_into().unwrap()),
            1002
        );
        // 名字**不是**内联的 —— `szeventindex` 在 +0x4C，此处应为 0
        // （待 write_mdl 用字符串池位置回填），且 patch 列表里有两条。
        assert_eq!(out.event_name_patches.len(), 2);
        assert_eq!(out.event_name_patches[0].0, ev_off + 0x4C);
        assert_eq!(out.event_name_patches[0].1, "AE_FOOTSTEP_RIGHT");
        assert_eq!(out.event_name_patches[1].0, e2 + 0x4C);
        assert_eq!(out.event_name_patches[1].1, "AE_SOUND");
        // blend 表排在 events 之后。
        assert!(out.blend_offsets[0] >= ev_off + 2 * EVENT_SIZE);
    }

    /// **序列级 `iklock`**（`write.cpp:596-611`）。
    ///
    /// 锁住三件事：
    /// 1. `numiklocks` @0xA4 写的是**条数**（不是 0）；
    /// 2. 记录是 **32 字节**、`chain` 落盘的是**链下标**（TOML 里写链名）；
    /// 3. `flags` / `unused[4]` 官方**不写**，保持 0。
    ///
    /// 判据取自受控实验 `ikl1`/`ikl2`（真实 `studiomdl.exe`）：
    /// `ikl1` → `{chain=0, posW=1.0, localQW=0.0}`，
    /// `ikl2` → `{0, 1.0, 0.1}` + `{1, 0.5, 0.25}`。
    #[test]
    fn sequence_iklocks_are_written_as_32_byte_records() {
        let mut s = seq("idle", true, vec![vec![pose([0.0; 3], [0.0; 3])]]);
        s.iklocks = vec![
            crate::model::IkAutoplayLock {
                chain: "leg".into(),
                pos_weight: 1.0,
                local_q_weight: 0.1,
            },
            crate::model::IkAutoplayLock {
                chain: "arm".into(),
                pos_weight: 0.5,
                local_q_weight: 0.25,
            },
        ];
        let mut c = compiled(vec![s], 1);
        // 两条链：`leg` = 下标 0、`arm` = 下标 1。
        c.desc.ikchains = vec![
            crate::model::IkChain {
                name: "leg".into(),
                bone: "ankle".into(),
                knee_dir: None,
                ..Default::default()
            },
            crate::model::IkChain {
                name: "arm".into(),
                bone: "hand".into(),
                knee_dir: None,
                ..Default::default()
            },
        ];
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let g = |o: usize| i32::from_le_bytes(out.seqdescs[o..o + 4].try_into().unwrap());

        assert_eq!(g(0xA4), 2, "numiklocks @0xA4 应为 2");
        let ik_rel = g(0xA8);
        let ik_off = ik_rel as usize - SEQDESC_SIZE;
        assert_eq!(out.iklock_offsets[0], ik_off, "iklockindex 应指向子表区");

        let d = &out.seq_subtables;
        // 记录 0：chain=0（`leg`）、posW=1.0、localQW=0.1
        assert_eq!(
            i32::from_le_bytes(d[ik_off..ik_off + 4].try_into().unwrap()),
            0
        );
        assert_eq!(
            f32::from_le_bytes(d[ik_off + 4..ik_off + 8].try_into().unwrap()),
            1.0
        );
        assert_eq!(
            f32::from_le_bytes(d[ik_off + 8..ik_off + 12].try_into().unwrap()),
            0.1
        );
        // 记录 1 紧随其后，间隔**正好 32 字节**（`IK_LOCK_SIZE`）。
        let l2 = ik_off + IK_LOCK_SIZE;
        assert_eq!(
            i32::from_le_bytes(d[l2..l2 + 4].try_into().unwrap()),
            1,
            "`arm` 应为链下标 1"
        );
        assert_eq!(
            f32::from_le_bytes(d[l2 + 4..l2 + 8].try_into().unwrap()),
            0.5
        );
        assert_eq!(
            f32::from_le_bytes(d[l2 + 8..l2 + 12].try_into().unwrap()),
            0.25
        );
        // `flags` / `unused[4]` 官方不写 ⟹ 全 0（语料 3222/3222）。
        for k in 0..2 {
            let base = ik_off + k * IK_LOCK_SIZE;
            for w in 3..8 {
                assert_eq!(
                    i32::from_le_bytes(d[base + w * 4..base + w * 4 + 4].try_into().unwrap()),
                    0,
                    "记录 {k} 的第 {w} 个 dword 应为 0"
                );
            }
        }
        // 之后的子表（blend / keyvalue）必须排在 iklock **之后**。
        assert!(
            out.blend_offsets[0] >= ik_off + 2 * IK_LOCK_SIZE,
            "blend 表应在 iklock 之后"
        );
    }

    /// `iklock` 引用不存在的链名时必须**报错**，而不是静默写错下标。
    #[test]
    fn sequence_iklock_with_unknown_chain_errors() {
        let mut s = seq("idle", true, vec![vec![pose([0.0; 3], [0.0; 3])]]);
        s.iklocks = vec![crate::model::IkAutoplayLock {
            chain: "nope".into(),
            pos_weight: 1.0,
            local_q_weight: 0.0,
        }];
        let mut c = compiled(vec![s], 1);
        c.desc.ikchains = vec![crate::model::IkChain {
            name: "leg".into(),
            bone: "ankle".into(),
            knee_dir: None,
            ..Default::default()
        }];
        let err = write_with_f0_refs(&c, &[-1]).expect_err("应因未知链名而失败");
        let msg = format!("{err:?}");
        assert!(msg.contains("nope"), "错误信息应含链名：{msg}");
    }

    /// **子表区的全局游标布局**（`write.cpp:427-641`）。
    ///
    /// 这条测试锁住「官方为**每条序列**都真写一张 weightlist」这个事实 ——
    /// 早先本实现把它当空表写 `SEQDESC_SIZE`，引擎按 `g_numbones` 个
    /// float 去读时会落到相邻子表上。
    ///
    /// 用 2 骨骼 + 有 events + 有 autolayer 的序列，让每一段的长度都
    /// 非零，从而**区分**「按游标排布」与「一律写 212」两种实现。
    #[test]
    fn seq_subtables_follow_global_cursor_layout() {
        let mut s = seq(
            "idle",
            false,
            (0..3).map(|_| vec![pose([0.0; 3], [0.0; 3]); 2]).collect(),
        );
        s.events = vec![SequenceEvent {
            cycle: 0.5,
            event_type: 0,
            event: 1001,
            name: "AE_X".into(),
            options: String::new(),
        }];
        s.auto_layers = vec![crate::model::CompiledAutoLayer {
            sequence: 0,
            pose: 0,
            flags: 0,
            start: 0.0,
            peak: 0.0,
            tail: 0.0,
            end: 0.0,
        }];
        let c = compiled(vec![s], 2);
        let out = write_with_f0_refs(&c, &[-1, 0]).expect("写出动画");
        let g = |o: usize| i32::from_le_bytes(out.seqdescs[o..o + 4].try_into().unwrap());
        let base = SEQDESC_SIZE as i32;

        // 按 `write.cpp` 的顺序推算各段：
        //   posekey 无（gs=1x1）→ events 1×80 → ALIGN4
        //   → autolayer 1×24 → weightlist 2×4 → iklock 0 → ALIGN4
        //   → blend 1×2 → ALIGN4 → keyvalue 0
        assert_eq!(out.event_offsets[0], Some(0), "events 在子表区偏移 0");
        assert_eq!(g(0x1C), base, "eventindex = 游标(0) + base");
        assert_eq!(out.autolayer_offsets[0], 80, "autolayer 紧接 events");
        assert_eq!(g(0x98), base + 80);
        assert_eq!(out.weightlist_offsets[0], 104, "weightlist 紧接 autolayer");
        assert_eq!(g(0x9C), base + 104);
        assert_eq!(out.iklock_offsets[0], 112, "iklock 紧接 weightlist");
        assert_eq!(g(0xA8), base + 112);
        // blend 在 iklock(长度 0) 之后，游标已是 4 的倍数所以不额外补齐。
        assert_eq!(out.blend_offsets[0], 112, "blend 在 iklock 之后");
        assert_eq!(g(0x3C), base + 112);
        // keyvalue 在 blend(2B) + ALIGN4 之后。
        assert_eq!(out.keyvalue_offsets[0], 116, "keyvalue 在 blend 对齐之后");
        assert_eq!(g(0xAC), base + 116);
        // weightlist 内容 = 2 个 1.0。
        assert_eq!(
            f32::from_le_bytes(out.seq_subtables[104..108].try_into().unwrap()),
            1.0
        );
        assert_eq!(
            f32::from_le_bytes(out.seq_subtables[108..112].try_into().unwrap()),
            1.0
        );
    }

    /// **animdesc 是共享池**：同一格被多条序列引用时只产出**一个** animdesc。
    ///
    /// # 官方模型（`studiomdl.cpp:2952-2959`）
    ///
    /// `$sequence` 块里的裸名字先按名字查 `g_panimation[]`，查到就
    /// **复用同一个 animdesc**，查不到才新建（名字加 `@` 前缀）。
    ///
    /// 实测 `v_autoshotgun.mdl`：**27 个 seqdesc / 29 个 animdesc** ——
    /// 33 个格子里 4 处复用（`idle` 的两个 `a_run`、`idle`/`idle_raw`
    /// 的 `a_idle`）。
    ///
    /// 这条测试用「两条序列引用同一个动画」构造最小复现：
    /// 若实现退化成「每格一个 animdesc」，`anim_count()` 会是 2 而不是 1。
    #[test]
    fn shared_animations_produce_one_animdesc() {
        let mut a = seq("idle", false, vec![vec![pose([0.0; 3], [0.0; 3])]]);
        let mut b = seq("idle_raw", false, vec![vec![pose([0.0; 3], [0.0; 3])]]);
        // 两条序列都引用动画池的第 0 项 —— 与官方 `idle`/`idle_raw` 共用
        // `a_idle` 同构。
        a.cells = vec![0];
        b.cells = vec![0];
        let mut c = compiled(vec![a, b], 1);
        c.animations = vec![crate::model::CompiledAnimation {
            local_hierarchy: Vec::new(),
            weights: Vec::new(),
            loop_restart: 0,
            fudge_loop: false,
            extra_flags: 0,
            movements: Vec::new(),
            name: "a_idle".into(),
            smd_path: std::path::PathBuf::from("a_idle.smd"),
            fps: 30.0,
            looping: false,
            frames: vec![vec![pose([0.0; 3], [0.0; 3])]],
            delta: false,
            ik_rules: Vec::new(),
            no_auto_ik: false,
            pre_subtract_frames: None,
        }];
        assert_eq!(c.anim_count(), 1, "两条序列共用 1 个动画 ⇒ 1 个 animdesc");
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        assert_eq!(out.animdescs.len(), ANIMDESC_SIZE, "只应有一个 animdesc");
        // 两条 seqdesc 的 blend 表都指向 animdesc 0。
        //
        // ⚠️ 子表偏移的基准是 **`(seq_count − si) * 212`**（整个 seqdesc
        // 数组的末尾），不是固定的 212 —— 只有最后一条序列才等于 212。
        let seq_count = 2usize;
        for si in 0..2 {
            let o = si * SEQDESC_SIZE;
            let rel = i32::from_le_bytes(out.seqdescs[o + 0x3C..o + 0x40].try_into().unwrap());
            let bo = out.blend_offsets[si];
            assert_eq!(
                i16::from_le_bytes(out.seq_subtables[bo..bo + 2].try_into().unwrap()),
                0,
                "seq[{si}] 的 blend 应指向 animdesc 0"
            );
            assert_eq!(
                rel as usize,
                (seq_count - si) * SEQDESC_SIZE + bo,
                "seq[{si}] 的 animindexindex"
            );
        }
    }

    /// 多条序列时，weightlist **只写一份**，后续序列全部指回同一偏移
    /// （`write.cpp:556-595` 的复用分支）。
    ///
    /// 语料实测：11170 条序列里 7639 条复用、3531 条新建 —— 复用是常态，
    /// 所以「每条序列各写一份」会让子表区膨胀数倍。
    #[test]
    fn weightlist_is_shared_across_sequences() {
        let mk = |n: &str| {
            seq(
                n,
                false,
                (0..3).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect(),
            )
        };
        let c = compiled(vec![mk("a"), mk("b"), mk("c")], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        assert_eq!(out.weightlist_offsets.len(), 3);
        assert_eq!(
            out.weightlist_offsets,
            vec![0, 0, 0],
            "三条序列的权重全为 1，应复用同一块"
        );
        // 子表区里只有 1 份权重（4 字节）+ 3 份 blend(2B)+对齐。
        //
        // 用 `as_chunks::<4>()` 而不是 `chunks_exact(4)`：后者对定长块会触发
        // `clippy::chunks_exact_to_as_chunks`（CI 的 `-D warnings` 会当错误）。
        let weight_bytes = out
            .seq_subtables
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|c| **c == 1.0f32.to_le_bytes())
            .count();
        assert_eq!(weight_bytes, 1, "权重只应出现一次");
    }

    /// blend 序列的 seqdesc：`numblends` / `groupsize` / `paramindex` /
    /// `paramstart` / `paramend` / `posekeyindex` 全部按官方语义写出。
    ///
    /// 判据取自实测 `v_autoshotgun.mdl` 的 `look_poses`：
    /// `numblends=3 groupsize=3x1 paramindex=[0,-1] paramstart=[-1,0]
    ///  paramend=[1,0] posekey=[-1,0,1,0] blend=[2,1,3]`（列主序）。
    #[test]
    fn blend_sequence_writes_full_seqdesc() {
        let mut s = seq("look_poses", false, Vec::new());
        // 三格引用**动画池**的下标 0/1/2（共享池，不是每格一份数据）。
        s.cells = vec![0, 1, 2];
        s.blend_width = 3;
        s.blend_params = [
            Some(crate::model::CompiledBlendParam {
                parameter_index: 0,
                start: -1.0,
                end: 1.0,
                keys: vec![-1.0, 0.0, 1.0],
            }),
            None,
        ];
        let mut c = compiled(vec![s], 1);
        c.animations = ["look_down", "look_mid", "look_up"]
            .iter()
            .map(|n| crate::model::CompiledAnimation {
                local_hierarchy: Vec::new(),
                weights: Vec::new(),
                loop_restart: 0,
                fudge_loop: false,
                extra_flags: 0,
                movements: Vec::new(),
                name: (*n).to_string(),
                smd_path: std::path::PathBuf::from(format!("{n}.smd")),
                fps: 30.0,
                looping: false,
                frames: (0..2).map(|_| vec![pose([0.0; 3], [0.0; 3])]).collect(),
                delta: false,
                ik_rules: Vec::new(),
                no_auto_ik: false,
                pre_subtract_frames: None,
            })
            .collect();
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let g = |o: usize| i32::from_le_bytes(out.seqdescs[o..o + 4].try_into().unwrap());
        let gf = |o: usize| f32::from_le_bytes(out.seqdescs[o..o + 4].try_into().unwrap());

        assert_eq!(
            out.animdescs.len(),
            3 * ANIMDESC_SIZE,
            "三格 = 三个 animdesc"
        );
        assert_eq!(g(0x38), 3, "numblends");
        assert_eq!(g(0x44), 3, "groupsize[0]");
        assert_eq!(g(0x48), 1, "groupsize[1]");
        assert_eq!(g(0x4C), 0, "paramindex[0]");
        assert_eq!(g(0x50), -1, "paramindex[1] 未指定");
        // ⚠️ `paramstart[2]` 与 `paramend[2]` 是**两个独立数组**，各自
        // 连续：`paramstart` 在 0x54/0x58、`paramend` 在 0x5C/0x60。
        // 早先这里写成 0x54/0x5C 与 0x58/0x60（交错），会让
        // `paramend[0]` 落到 `paramstart[1]` 上。
        assert_eq!(gf(0x54), -1.0, "paramstart[0]");
        assert_eq!(gf(0x58), 0.0, "paramstart[1] 未指定 → 0");
        assert_eq!(gf(0x5C), 1.0, "paramend[0]");
        assert_eq!(gf(0x60), 0.0, "paramend[1] 未指定 → 0");

        // posekey：gs0>1 ⇒ 写 `[-1, 0, 1]` + 1 个 param1（缺省 0）。
        let pk = out.posekey_offsets[0].expect("gs0>1 应写 posekey");
        let vals: Vec<f32> = (0..4)
            .map(|k| {
                f32::from_le_bytes(
                    out.seq_subtables[pk + k * 4..pk + k * 4 + 4]
                        .try_into()
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(vals, vec![-1.0, 0.0, 1.0, 0.0], "posekey 内容");
        assert_eq!(
            g(0xA0),
            SEQDESC_SIZE as i32 + pk as i32,
            "posekeyindex 指向子表区"
        );

        // blend 表 = 三个 animdesc 的下标（0,1,2），列主序写入。
        let bo = out.blend_offsets[0];
        let idx: Vec<i16> = (0..3)
            .map(|k| {
                i16::from_le_bytes(
                    out.seq_subtables[bo + k * 2..bo + k * 2 + 2]
                        .try_into()
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(idx, vec![0, 1, 2], "blend 指向三个格子");

        // animdesc 名 = **动画池的名字**（`a_run` / `look_down` 这种，
        // **不带** `@` 前缀 —— 隐含动画的名字才带，见 `compile.rs`）。
        let names = anim_name_sources(&c);
        assert_eq!(
            names,
            vec![
                AnimNameSource::Literal("look_down".into()),
                AnimNameSource::Literal("look_mid".into()),
                AnimNameSource::Literal("look_up".into()),
            ]
        );
    }

    /// **`seqdesc.flags` 必须 OR 上每一格动画自己的 `flags`。**
    ///
    /// `studiomdl.cpp:3026-3038`：
    /// ```c
    /// for (i = 0; i < numblends; i++) { ... pseq->flags |= animations[i]->flags; }
    /// ```
    ///
    /// 所以 `$animation "a_idle" ... loop` 之后，任何引用它的 `$sequence`
    /// （哪怕块里**没写** `loop`）都会拿到 `STUDIO_LOOPING`。
    ///
    /// 实测 miku `v_autoshotgun.mdl`：`seq[1] "idle_raw"` 的 QC 只有
    /// `$sequence "idle_raw" "a_idle"` —— 官方 `flags == 0x01`。
    /// 早先 mdlc 只看 `seq.looping`，写成 `0x00`。
    #[test]
    fn seq_flags_or_in_cell_animation_flags() {
        // 序列自己**不** loop，但它引用的动画 loop。
        let mut s = seq("idle_raw", false, Vec::new());
        s.cells = vec![0];
        let mut c = compiled(vec![s], 1);
        // 动画池里的那一条改成 looping（`$animation ... loop`）。
        c.animations[0].looping = true;
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let flags = i32::from_le_bytes(out.seqdescs[0x0C..0x10].try_into().unwrap());
        assert_eq!(
            flags, STUDIO_LOOPING,
            "序列块没写 loop，但引用的动画 loop → 序列也要 LOOPING"
        );
        // animdesc 自己的 flags 同样置位（同一条规则的另一半）。
        let aflags = i32::from_le_bytes(out.animdescs[0x0C..0x10].try_into().unwrap());
        assert_eq!(aflags, STUDIO_LOOPING, "animdesc.flags");
    }

    /// **`seqdesc.flags` 不得从 `animdesc.flags` 补 `STUDIO_DELTA`**（R11）。
    ///
    /// 官方两处跑在**不同阶段**：
    ///
    /// | 事件 | 时机 | 能否进 `pseq->flags` |
    /// |---|---|---|
    /// | `pseq->flags \|= animations[i]->flags`（`studiomdl.cpp:3037`） | **解析期** | 基准 |
    /// | `loop` → `panim->flags \|= STUDIO_LOOPING`（`:2226`） | **解析期当场** | ✅ |
    /// | `subtract` → 只记 `CMD_SUBTRACT`（`:1733-1751`） | **解析期**，不碰 flags | ❌ |
    /// | `panim->flags \|= STUDIO_DELTA`（`simplify.cpp:164`） | **执行期** | ❌ 太晚 |
    ///
    /// 所以「只有 `subtract`（无 `delta` 关键字）的序列」在官方产物里
    /// `seqdesc.flags` **没有** `DELTA`，而它的 `animdesc.flags` **有**。
    /// 实测（`docs/_probe/probe_r9_delta_flags.js`）：6 条 `*_layer` 序列，
    /// 官方 `0x000` / `0x001`，照 `animations[].delta` 补位会写成
    /// `0x004` / `0x005`（6/23 差异）。
    ///
    /// ⚠️ **本测试必须同时断言 `animdesc` 有 `DELTA`** —— 否则把 R9 一起
    /// 改坏（`delta` 不再传播到动画）时它仍会通过，就退化成了「两边都 0」的
    /// 空洞测试。
    #[test]
    fn seqdesc_flags_do_not_inherit_delta_from_animation() {
        // 序列块**没有** `delta` 关键字 ⟹ `seq.delta == false`。
        let mut s = seq("reload_layer", false, Vec::new());
        s.cells = vec![0];
        s.delta = false;
        let mut c = compiled(vec![s], 1);
        // 但动画池里那条**是** delta（等价于 `$sequence ... subtract "a_idle_1" 0`
        // 被 `simplify.cpp:164` 执行后的结果）。
        c.animations[0].delta = true;

        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let seq_flags = i32::from_le_bytes(out.seqdescs[0x0C..0x10].try_into().unwrap());
        let anim_flags = i32::from_le_bytes(out.animdescs[0x0C..0x10].try_into().unwrap());

        assert_eq!(
            anim_flags & STUDIO_DELTA,
            STUDIO_DELTA,
            "animdesc 必须带 DELTA（R9）—— 这一半坏了本测试就失去判别力"
        );
        assert_eq!(
            seq_flags & STUDIO_DELTA,
            0,
            "seqdesc 绝不能带 DELTA（R11）：subtract 在执行期才置动画的位，\
             那时序列 flags 早已定稿"
        );
    }

    /// **`seqdesc.flags` 的 `LOOPING` 却**必须**从动画补**（与 R11 成对）。
    ///
    /// 这条与上一条是同一个机制的两面：`loop` 在**解析期当场**置
    /// `panim->flags`（`studiomdl.cpp:2226`），所以它能赶上 `:3037` 那行；
    /// 而 `subtract` 赶不上。**两条测试合起来才钉住「时机」这个根因** ——
    /// 只留一条的话，「整个 OR 循环都删掉」这种过度修复会被漏过。
    #[test]
    fn seqdesc_flags_do_inherit_looping_from_animation() {
        let mut s = seq("idle_raw", false, Vec::new());
        s.cells = vec![0];
        let mut c = compiled(vec![s], 1);
        c.animations[0].looping = true;
        c.animations[0].delta = true; // 同时是 delta，验证只有 LOOPING 会传播

        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let seq_flags = i32::from_le_bytes(out.seqdescs[0x0C..0x10].try_into().unwrap());
        assert_eq!(
            seq_flags & STUDIO_LOOPING,
            STUDIO_LOOPING,
            "loop 在解析期就置位 ⟹ 序列必须拿到 LOOPING"
        );
        assert_eq!(
            seq_flags & STUDIO_DELTA,
            0,
            "同一条动画的 DELTA 不能跟着一起传过来"
        );
    }

    /// **`seqdesc.numikrules` 是「该序列每一格动画取 MAX」，不是
    /// 「第 i 条序列取第 i 个 animdesc」。**
    ///
    /// `simplify.cpp:6287-6295`：
    /// ```c
    /// for (j = 0; j < groupsize[0]; j++)
    ///   for (k = 0; k < groupsize[1]; k++)
    ///     g_sequence[i].numikrules = MAX(g_sequence[i].numikrules,
    ///                                    g_sequence[i].panim[j][k]->numikrules);
    /// ```
    ///
    /// 单动画序列两者恰好重合，所以早先按序列下标的写法在非 blend 模型上
    /// 一直是对的。实测 miku `idle`（3 格，引用 anim[1]/anim[0]，各 2 条规则）
    /// 才暴露：按序列下标取到的是 `anim[2]`（`look_down`，delta，0 条），
    /// 于是写成了 0，而官方是 2。
    ///
    /// 判据写成**自洽不变式**：对每条序列，
    /// `seqdesc.numikrules == max(animdesc[该序列每一格].numikrules)`，
    /// 其中「每一格」直接**从产物的 blend 子表里读回来**（不依赖内部状态）。
    /// 这样即使两种写法在某个 fixture 上恰好同值，不变式本身也不会被
    /// 悄悄写错 —— 它复刻的正是 `simplify.cpp:6287-6295` 的循环。
    #[test]
    fn seq_numikrules_equals_max_over_cells() {
        // 两条序列、池里 3 条动画：让「序列下标」与「格子下标」错开。
        let mut c = ikr_compiled(vec![touch_rule(Some("ankle"))], false);
        let base = c.animations[0].clone();
        c.animations = vec![base.clone(), base.clone(), base.clone()];
        // seq[0] 单格 → anim[2]；seq[1] 单格 → anim[0]。
        c.sequences[0].cells = vec![2];
        let mut s1 = c.sequences[0].clone();
        s1.name = "second".into();
        s1.cells = vec![0];
        c.sequences.push(s1);

        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        let seq_i32 = |si: usize, o: usize| {
            let b = si * SEQDESC_SIZE + o;
            i32::from_le_bytes(out.seqdescs[b..b + 4].try_into().unwrap())
        };
        let anim_rules = |ai: usize| {
            let b = ai * ANIMDESC_SIZE + 0x3C;
            i32::from_le_bytes(out.animdescs[b..b + 4].try_into().unwrap()) as usize
        };
        // 从 blend 子表读回每一格的 animdesc 下标。
        let cell_of = |si: usize| -> usize {
            let rel = out.blend_offsets[si];
            i16::from_le_bytes(out.seq_subtables[rel..rel + 2].try_into().unwrap()) as usize
        };

        for si in 0..2 {
            let cell = cell_of(si);
            assert_eq!(
                seq_i32(si, 0x90) as usize,
                anim_rules(cell),
                "seq[{si}] 引用 anim[{cell}] ⇒ numikrules 必须取该 animdesc 的值"
            );
        }
        // 两条序列引用了**不同**的动画 —— 按序列下标取值会让 seq[1]
        // 读到 anim[1]（而非它真正引用的 anim[0]）。
        assert_ne!(cell_of(0), cell_of(1), "两格必须错开才具判别力");
        assert_ne!(seq_i32(0, 0x90), 0, "规则数非 0，否则两种写法都通过");
    }

    /// 单动画序列的 animdesc 名来自**动画池**（隐含动画带 `@` 前缀）。
    ///
    /// `compiled()` 会为每条序列建一个隐含动画，所以这里直接断言
    /// 池里那一个名字被原样用作 animdesc 名。
    #[test]
    fn single_animation_name_comes_from_the_pool() {
        let c = compiled(
            vec![seq("reload", false, vec![vec![pose([0.0; 3], [0.0; 3])]])],
            1,
        );
        assert_eq!(
            anim_name_sources(&c),
            vec![AnimNameSource::Literal("@reload".into())]
        );
    }

    #[test]
    fn relative_name_offset_computes_from_self() {
        assert_eq!(relative_name_offset(1000, 900).unwrap(), 100);
        assert_eq!(relative_name_offset(900, 1000).unwrap(), -100);
    }

    // ---- 长序列分块 ----

    /// 超过 255 帧时单条 run 装不下，必须拆成多条 run 且解码后仍然正确。
    #[test]
    fn long_sequence_splits_into_multiple_runs() {
        let n = 300usize;
        let frames: Vec<Vec<SmdPose>> = (0..n)
            .map(|f| vec![pose([0.0; 3], [f as f32 * 0.001, 0.0, 0.0])])
            .collect();
        let c = compiled(vec![seq("long", false, frames)], 1);
        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        let (_, flags, payload) = walk_chain(&out.anim_data, 0)[0];
        assert_eq!(flags & STUDIO_ANIM_ANIMROT, STUDIO_ANIM_ANIMROT);
        let d = &out.anim_data;
        let off = i16::from_le_bytes([d[payload], d[payload + 1]]);
        let s = decode_stream(d, payload + off as usize, n);
        assert_eq!(s.len(), n, "解码后应有 {n} 个采样");
        assert_eq!(s[0], 0, "第 0 帧恒为 0");
        assert!(s[n - 1] > s[0], "末帧应大于首帧");
        // 单调不减（量化后可能相等）。
        for w in s.windows(2) {
            assert!(w[1] >= w[0], "应单调不减：{} → {}", w[0], w[1]);
        }
    }

    // =====================================================================
    // IK rule（`mstudioikrule_t`）
    // =====================================================================
    //
    // 真值来自官方产物 `docs/_probe/artifacts/ikr{1,3,4,5,6,7}.mdl`
    // （受控 QC 见 `docs/_probe/smdl/ikr*.qc`，同一份 `ikr.smd`）。
    // 逐字段对照脚本：`docs/_probe/cmp_ikrule.js`（**0 差异**）。

    /// 复刻 `ikr.smd` 的骨架与动画：骨骼沿 +X 排列（root/hip/knee/ankle），
    /// 踝的 z 逐帧 `0/5/10/15`。
    fn ikr_compiled(rules: Vec<crate::model::IkRule>, no_auto_ik: bool) -> CompiledModelDesc {
        use crate::model::{Bone, IkChain, ModelDesc, ModelMeta};
        let names = ["root", "hip", "knee", "ankle"];
        let frames: Vec<Vec<SmdPose>> = (0..4)
            .map(|f| {
                names
                    .iter()
                    .enumerate()
                    .map(|(i, _)| SmdPose {
                        bone: i as i32,
                        position: match i {
                            0 => [0.0, 0.0, 0.0],
                            1 => [10.0, 0.0, 0.0],
                            2 => [20.0, 0.0, 0.0],
                            _ => [30.0, 0.0, f as f32 * 5.0],
                        },
                        rotation: [0.0; 3],
                    })
                    .collect()
            })
            .collect();
        CompiledModelDesc {
            desc: ModelDesc {
                model: ModelMeta {
                    rotation_limits: Vec::new(),
                    rotation_alignments: Vec::new(),
                    root_bone: None,
                    allowed_root_lods: 0,
                    constant_directional_light_dot: None,
                    source_dependencies: Vec::new(),
                    minimum_lod: 0,
                    autocenter: false,
                    first_source: None,
                    autocenter_offset: None,
                    forced_realign: Vec::new(),
                    bone_renames: Vec::new(),
                    bone_hierarchy: Vec::new(),
                    transition_nodes: Vec::new(),
                    transition_skips: Vec::new(),
                    transition_multistage: false,
                    source_rotation: None,
                    source_origin: None,
                    material_renames: Vec::new(),
                    name: "ikr.mdl".into(),
                    version: None,
                    checksum: None,
                    static_prop: false,
                    surface_prop: None,
                    eye_position: None,
                    illum_position: None,
                    illum_position_from_bone: false,
                    max_eye_deflection: None,
                    hull_min: None,
                    hull_max: None,
                    view_bb_min: None,
                    view_bb_max: None,
                    extra_flags: None,
                    contents: None,
                    joint_contents: Vec::new(),
                    skip_bone_in_bbox: false,
                    optimize_vtx: false,
                    split_oversized_meshes: true,
                    key_values: None,
                    pose_parameters: Vec::new(),
                    realign_bones: false,
                    anim_block_size: None,
                    section_frames: None,
                },
                physics: Default::default(),
                materials: Default::default(),
                bones: names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| Bone {
                        name: (*n).into(),
                        parent: if i == 0 {
                            None
                        } else {
                            Some(names[i - 1].into())
                        },
                        position: None,
                        rotation: None,
                        flags: None,
                        surface_prop: None,
                        bonemerge: false,
                        pre_aligned: None,
                        realign_position: None,
                        realign_rotation: None,
                    })
                    .collect(),
                bodyparts: Vec::new(),
                hitboxes: Default::default(),
                additional_hitbox_sets: Vec::new(),
                attachments: Vec::new(),
                sequences: Vec::new(),
                animations: Vec::new(),
                bonecontrollers: Vec::new(),
                ikchains: vec![IkChain {
                    name: "leg".into(),
                    bone: "ankle".into(),
                    knee_dir: Some([0.5, 0.5, 0.0]),
                    ..Default::default()
                }],
                ik_autoplay_locks: Vec::new(),
                flex_descriptors: Vec::new(),
                flex_controllers: Vec::new(),
                flex_rules: Vec::new(),
                flex_controller_ui: Vec::new(),
                mouths: Vec::new(),
                jiggle_bones: Vec::new(),
                quat_interp_bones: Vec::new(),
                include_models: Vec::new(),
                weight_lists: Vec::new(),
                default_weight_list: Vec::new(),
                cmd_lists: Vec::new(),
            },
            bodyparts: Vec::new(),
            sequences: vec![CompiledSequence {
                key_values: None,
                name: "idle".into(),
                smd_path: std::path::PathBuf::from("ikr.smd"),
                fps: 30.0,
                looping: false,
                forward_declared: false,
                activity: -1,
                activity_weight: 0,
                activity_name: String::new(),
                delta: false,
                frames: frames.clone(),
                cells: vec![0],
                blend_width: 1,
                blend_params: [None, None],
                calc_axes: Vec::new(),
                blend_ref: None,
                blend_comp: None,
                blend_center: None,
                transition: None,
                transition_reversible: false,
                auto_layers: Vec::new(),
                events: Vec::new(),
                fade_in: crate::model::default_fade_time(),
                fade_out: crate::model::default_fade_time(),
                no_auto_ik,
                // ⚠️ **规则属于动画，不属于序列** —— 官方 `ProcessIKRules`
                // 遍历 `g_panimation[]` 的 `cmds[]`（`simplify.cpp:5828`），
                // 从不读 `g_sequence[]` 的命令表。
                // 所以这里序列上留空、动画上放规则。
                ik_rules: Vec::new(),
                iklocks: Vec::new(),
                movements: Vec::new(),
                // 4 帧，远低于缺省阈值 120 → 不分段。
                section_frames: 0,
                num_sections: 0,
                pre_subtract_frames: None,
                extra_flags: None,
                // 单骨骼、无 `$weightlist` ⟹ 权重全 1。
                weights: vec![1.0],
            }],
            // 隐含动画（`@idle`），与 `compile.rs` 的单动画序列路径一致。
            animations: vec![crate::model::CompiledAnimation {
                local_hierarchy: Vec::new(),
                weights: Vec::new(),
                loop_restart: 0,
                fudge_loop: false,
                extra_flags: 0,
                movements: Vec::new(),
                name: "@idle".into(),
                smd_path: std::path::PathBuf::from("ikr.smd"),
                fps: 30.0,
                looping: false,
                frames: frames.clone(),
                delta: false,
                ik_rules: rules,
                no_auto_ik,
                pre_subtract_frames: None,
            }],
            realigned: None,
            resolved_flex_rules: Vec::new(),
            resolved_flex_controller_ui: Vec::new(),
            resolved_mouths: Vec::new(),
            resolved_jiggle_bones: Vec::new(),
            resolved_quat_interp_bones: Vec::new(),
            physics_bone: None,
        }
    }

    fn touch_rule(bone: Option<&str>) -> crate::model::IkRule {
        crate::model::IkRule {
            chain: "leg".into(),
            kind: crate::model::IkRuleType::Touch,
            bone: bone.map(str::to_string),
            attachment: None,
            target: None,
            height: None,
            radius: None,
            pad: None,
            floor: None,
            range: None,
            contact: None,
            fake_origin: None,
            fake_rotate: None,
            use_source: false,
        }
    }

    // ---- `STUDIO_DELTA` 动画的 IK 误差（`CalcBoneTransforms` 重建）----
    //
    // `subtract` 把动画变成增量；官方 `CalcBoneTransforms` 的 `else` 分支
    // （`simplify.cpp:4562-4578`）用**基准动画第 0 帧**重建完整姿态。
    // mdlc 之前直接用增量 ⟹ 骨骼塌到原点 ⟹ IK 误差恒 0。
    //
    // 官方产物对「有无 `subtract`」**不变**（重建在 `s = 1` 时精确抵消
    // `subtractBaseAnimations`），所以判据可以写成「两条路径载荷必须相同」。

    /// 把 `ikr_compiled` 的动画改成 `subtract` 之后的形态：
    /// `frames` 是**增量**，`pre_subtract_frames` 保留原始姿态，
    /// `animations[0]` 是重建基准。
    ///
    /// # 为什么把父链全置零
    ///
    /// `ikr_compiled` 的 `hip`/`knee` 分别偏 10/20。若保留，世界矩阵会叠加
    /// 父链平移，判据里就得先算清「SMD 骨架是局部还是世界」的换算 ——
    /// 那是另一条独立约定（已由真实 oracle 对照覆盖），混进来只会让
    /// **本测试的判据**变模糊。
    ///
    /// 置零后 `world[ankle] == 重建出的 ankle 局部矩阵`，判据可以直接写成
    /// 「误差的 `pos` ≈ 重建值」。
    ///
    /// # 数据必须**逐帧变化**
    ///
    /// 误差恒定会让 RLE 合法地压成单样本（`{v1,t3}`），照不出任何 bug ——
    /// 官方 `iksm` 的 `pos.x` 之所以是 `{v3,t3}`，正是因为姿态逐帧在变。
    ///
    /// # 构造
    ///
    /// | 骨骼 | 基准 | 增量 | 减除前 | 重建 = 基准 + 增量 |
    /// |---|---|---|---|---|
    /// | `root`/`hip`/`knee` | 0 | 0 | 0 | 0 |
    /// | `ankle` | 30 | `7 + n` | `37 + n` | **`37 + n`** |
    ///
    /// 塌陷 bug 下误差会是增量本身（`7 + n`）而不是 `37 + n`。
    fn delta_ikr_compiled(rules: Vec<crate::model::IkRule>) -> CompiledModelDesc {
        let mut c = ikr_compiled(rules, false);
        // `ikr_compiled` 只建了一条动画（序列的隐含动画），且规则挂在它上面。
        // `subtract` 需要**两条**：基准 + 目标。规则只属于**目标**那一条 ——
        // 基准动画在官方是独立的 `$animation`，没有 `cmds[]`。
        let mut target = c.animations[0].clone();
        target.name = "@idle_sub".into();
        // 基准动画不参与 IK 规则（否则会多出一条无意义的 animdesc 规则）。
        c.animations[0].ik_rules.clear();
        c.animations[0].no_auto_ik = true;
        c.animations.push(target);
        let ti = c.animations.len() - 1;
        debug_assert_eq!(ti, 1);

        // 基准动画：父链全 0，`ankle`（链末端）在 x = 30。
        for f in c.animations[0].frames.iter_mut() {
            for p in f.iter_mut() {
                p.position = [0.0; 3];
                p.rotation = [0.0; 3];
            }
            f[3].position = [30.0, 0.0, 0.0];
        }
        // 目标动画的增量：**只有** `ankle` 动（`7 + n`），其余骨骼增量为 0。
        for (n, f) in c.animations[ti].frames.iter_mut().enumerate() {
            for p in f.iter_mut() {
                p.position = [0.0; 3];
                p.rotation = [0.0; 3];
            }
            f[3].position = [7.0 + n as f32, 0.0, 0.0];
        }
        // 减除前的原始姿态：`ankle = [37 + n, 0, 0]`（= 基准 + 增量）。
        c.animations[ti].pre_subtract_frames = Some(
            c.animations[ti]
                .frames
                .iter()
                .enumerate()
                .map(|(n, fr)| {
                    let mut fr = fr.clone();
                    fr[3].position = [37.0 + n as f32, 0.0, 0.0];
                    fr
                })
                .collect(),
        );
        c.animations[ti].delta = true;
        c.sequences[0].delta = true;
        c.sequences[0].frames = c.animations[ti].frames.clone();
        c.sequences[0].cells = vec![ti];
        c
    }

    /// **核心判据**：`subtract` 动画的 IK 误差必须**重建后**再算。
    ///
    /// 官方 `ab_iksub_mdlc.js` 的实测：同一份几何、只改有无 `subtract`，
    /// 官方 `@idle` 的压缩载荷**逐字节相同** —— 因为重建精确抵消了减除。
    ///
    /// 所以判据是：**带 `subtract` 的产物必须等于不带 `subtract` 的产物**。
    /// 若 `frame_worlds_for` 里的 `delta` 分支被删掉，带 `subtract` 那一侧
    /// 会算出塌陷的误差（`pos` 通道变成单样本），本测试变红。
    #[test]
    fn delta_ik_errors_match_plain_ik_errors() {
        // 带 `subtract`：增量 + 基准 ⟹ 重建出完整姿态。
        let delta = delta_ikr_compiled(vec![touch_rule(None)]);

        // 不带 `subtract`：**同一份结构**，但把帧换成完整姿态、关掉 delta。
        // 复用同一个构造器能保证除了 delta 相关的字段外一切相同。
        let mut plain = delta_ikr_compiled(vec![touch_rule(None)]);
        let ti = plain.animations.len() - 1;
        let full = plain.animations[ti].pre_subtract_frames.clone().unwrap();
        plain.animations[ti].frames = full.clone();
        plain.animations[ti].pre_subtract_frames = None;
        plain.animations[ti].delta = false;
        plain.sequences[0].frames = full;
        plain.sequences[0].delta = false;

        let po = write_with_f0_refs(&plain, &[-1, 0, 1, 2]).expect("写出 plain");
        let do_ = write_with_f0_refs(&delta, &[-1, 0, 1, 2]).expect("写出 delta");

        // 规则挂在**第 1 条**动画（下标 1）上。
        assert_ne!(read_rule(&po, 1, 0)[0x3C / 4], 0, "plain 侧应有压缩载荷");
        assert_ne!(
            read_rule(&do_, 1, 0)[0x3C / 4],
            0,
            "delta 侧应有压缩载荷（为 0 说明 numerror == 0）"
        );

        // 载荷长度必须**从 RLE 链解出来**，不能写死常量、也不能只看首 run：
        // `numerror` 随 `range`/帧数变，而 `rot.z` 这类恒定通道的首 run
        // 只有 1 个样本 —— 拿它的长度当载荷长度会只比到 `scale[0]`，
        // 于是**两侧明明不同也判绿**（实测踩过）。
        //
        // `numerror` = 任一通道各 run 的 `total` 之和。取通道 0 来数。
        let channel_bytes =
            |out: &AnimWriteOutcome, base: usize, k: usize, numerror: usize| -> usize {
                let d = &out.anim_data;
                let off = i16::from_le_bytes([d[base + 24 + k * 2], d[base + 25 + k * 2]]) as usize;
                let start = base + off;
                let mut p = start;
                let mut got = 0usize;
                while got < numerror {
                    let valid = d[p] as usize;
                    let total = d[p + 1] as usize;
                    p += 2 + valid * 2;
                    if total == 0 {
                        break;
                    }
                    got += total;
                }
                p - start
            };
        let numerror = |out: &AnimWriteOutcome, base: usize| -> usize {
            let d = &out.anim_data;
            let off = i16::from_le_bytes([d[base + 24], d[base + 25]]) as usize;
            let mut p = base + off;
            let mut n = 0usize;
            loop {
                let valid = d[p] as usize;
                let total = d[p + 1] as usize;
                p += 2 + valid * 2;
                if total == 0 {
                    break;
                }
                n += total;
                if n > 4096 {
                    break;
                }
            }
            n
        };
        let pb = po.ikrule_offsets[1].unwrap() + read_rule(&po, 1, 0)[0x3C / 4] as usize;
        let db = do_.ikrule_offsets[1].unwrap() + read_rule(&do_, 1, 0)[0x3C / 4] as usize;
        let pn = numerror(&po, pb);
        let dn = numerror(&do_, db);
        assert_eq!(pn, dn, "两侧 numerror 应相同");
        assert!(pn >= 3, "numerror 应 >= 3，实得 {pn}");

        let poff5 =
            i16::from_le_bytes([po.anim_data[pb + 24 + 10], po.anim_data[pb + 24 + 11]]) as usize;
        let doff5 =
            i16::from_le_bytes([do_.anim_data[db + 24 + 10], do_.anim_data[db + 24 + 11]]) as usize;
        let pl = poff5 + channel_bytes(&po, pb, 5, pn);
        let dl = doff5 + channel_bytes(&do_, db, 5, dn);
        assert_eq!(pl, dl, "两侧载荷长度应相同");

        let dump = |out: &AnimWriteOutcome, base: usize| {
            let d = &out.anim_data;
            let f32_at = |o: usize| f32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
            let scale: Vec<f32> = (0..6).map(|k| f32_at(base + k * 4)).collect();
            let off: Vec<i16> = (0..6)
                .map(|k| i16::from_le_bytes([d[base + 24 + k * 2], d[base + 25 + k * 2]]))
                .collect();
            let p = base + off[1] as usize;
            let valid = d[p] as usize;
            let s: Vec<f32> = (0..valid)
                .map(|j| {
                    let at = p + 2 + j * 2;
                    i16::from_le_bytes([d[at], d[at + 1]]) as f32 * scale[1]
                })
                .collect();
            format!("off={off:?} pos.y(v{valid})={s:?}")
        };
        assert_eq!(
            &po.anim_data[pb..pb + pl],
            &do_.anim_data[db..db + dl],
            "带 `subtract` 的 IK 载荷必须与不带的**逐字节相同**（重建抵消减除）\n\
             官方判据：`docs/_probe/ab_iksub_mdlc.js` / `cmp_ikpayload_bytes.js`\n\
             plain: {}\n\
             delta: {}",
            dump(&po, pb),
            dump(&do_, db),
        );
    }

    /// 反向判据：载荷里的 `pos` 采样必须**等于重建后的值**。
    ///
    /// 官方 `iksm` 的 `pos.x` 首 run 是 `{v3,t3}` 样本 `[2674, 2274, 1826]`
    /// （× scale 1/256 = `[10.446, 8.883, 7.133]`）—— 三个**不同**的采样，
    /// 因为姿态逐帧在变。
    ///
    /// 塌陷 bug 下增量本身近零且恒定 ⟹ 采样全相同 ⟹ RLE 压成 `{v1,t3}`。
    ///
    /// # 为什么查 `pos.y` 而不是 `pos.x`
    ///
    /// [`crate::compile::frame_worlds`] 给根骨骼左乘 `Rz(90°)`
    /// （官方 `panim->rotation`，`studiomdl.cpp:2427`），把 `(x,y,z)` 映成
    /// `(-y,x,z)`。所以 `ankle` 的 **x 位移出现在世界矩阵的 y 上**。
    #[test]
    fn delta_ik_payload_samples_equal_reconstructed_position() {
        let c = delta_ikr_compiled(vec![touch_rule(None)]);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        // 规则挂在**第 1 条**动画（下标 1）上。
        let payload = read_rule(&out, 1, 0)[0x3C / 4] as usize;
        assert_ne!(payload, 0, "应有载荷");
        let base = out.ikrule_offsets[1].unwrap() + payload;
        let d = &out.anim_data;

        let f32_at = |o: usize| f32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
        let scale: Vec<f32> = (0..6).map(|k| f32_at(base + k * 4)).collect();
        let off: Vec<i16> = (0..6)
            .map(|k| i16::from_le_bytes([d[base + 24 + k * 2], d[base + 25 + k * 2]]))
            .collect();

        // 解一条通道。`numerror` 由 `range = [0, 1, 1, 2]` 与帧数推出：
        // `end − start + 1 = 3`，且 `end(2) >= numframes(4)` 不成立 ⟹ 3。
        // 但 `write_with_f0_refs` 用的是默认 range（`end` 越界）⟹ 5。
        // 所以这里不写死，直接读首 run 的 `valid`。
        let decode = |k: usize| -> (usize, Vec<f32>) {
            let p = base + off[k] as usize;
            let valid = d[p] as usize;
            let s: Vec<f32> = (0..valid)
                .map(|j| {
                    let at = p + 2 + j * 2;
                    i16::from_le_bytes([d[at], d[at + 1]]) as f32 * scale[k]
                })
                .collect();
            (valid, s)
        };

        let (valid_y, sy) = decode(1);
        assert!(
            valid_y >= 3,
            "pos.y 首 run 应有至少 3 个样本（官方 `iksm` 形态）；\
             实得 {valid_y} —— 为 1 说明误差塌成常数"
        );
        // 重建值 = 基准 30 + 增量 (7 + n) = 37 + n。
        // 量化步长是 scale（≈1/256），所以容差取 2 个量化步。
        let tol = scale[1].abs() * 2.0;
        for (n, got) in sy.iter().enumerate() {
            let want = 37.0 + n as f32;
            assert!(
                (got - want).abs() <= tol,
                "pos.y[{n}] 应为重建值 {want}（基准 30 + 增量 7+{n}），\
                 实际 {got}（容差 {tol}）—— 若约等于增量 7+{n} 说明漏了基准帧"
            );
        }
        // 反向确认：若把重建值误当成增量本身，这里会差 30。
        assert!(
            (sy[0] - 7.0).abs() > tol * 10.0,
            "pos.y[0]={} 不应等于增量 7（那是塌陷 bug 的预测值）",
            sy[0]
        );
    }

    /// 从写出结果里读一条规则的全部字段。
    fn read_rule(out: &AnimWriteOutcome, anim: usize, j: usize) -> Vec<i32> {
        let base = out.ikrule_offsets[anim].expect("该动画应有 IK rule 块");
        let at = base + j * IK_RULE_SIZE;
        let d = &out.anim_data;
        (0..IK_RULE_SIZE / 4)
            .map(|w| {
                i32::from_le_bytes([
                    d[at + w * 4],
                    d[at + w * 4 + 1],
                    d[at + w * 4 + 2],
                    d[at + w * 4 + 3],
                ])
            })
            .collect()
    }

    fn read_f32(out: &AnimWriteOutcome, anim: usize, j: usize, off: usize) -> f32 {
        let base = out.ikrule_offsets[anim].unwrap();
        let d = &out.anim_data;
        let at = base + j * IK_RULE_SIZE + off;
        f32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
    }

    /// 从任意字节缓冲区读一个 `i32`（小端）。
    fn read_i32(buf: &[u8], _anim: usize, at: usize, extra: usize) -> i32 {
        let o = at + extra;
        i32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]])
    }

    // ---- movement（`mstudiomovement_t`，44 字节）----
    //
    // 规格见 `model::Movement` 与
    // `includemodel-animblock-sectionframes-movement-format.md` §4。
    // 官方真值：`docs/_probe/smdl/mv1.qc`（`walkframe 4` / `walkframe 9`）
    // → `artifacts/mv1.mdl`（`nummovements=2`、`endframe` = 4/9、
    // `movementindex=116` 相对自身）。验收脚本 `docs/_probe/cmp_movement.js`。

    /// `mstudiomovement_t` 必须恰好是 **44 字节**（实测反解：
    /// 相邻 `nummovements==1` 的 animdesc 差值 44 有 2798 组，
    /// 48/40/32 **0** 组）。
    ///
    /// 判据不写成 `4+4+4+4+4+12+12 == 44`（那是**恒真**的算术，抓不到
    /// 任何回归），而是**从实际写出的字节里反解 stride**：两条记录的
    /// `endframe` 分别写在 `mv` 与 `mv + 44`，所以 stride 就是它们的差。
    #[test]
    fn movement_record_is_44_bytes() {
        let frames = vec![
            vec![pose([0.0, 0.0, 0.0], [0.0; 3])],
            vec![pose([1.0, 0.0, 0.0], [0.0; 3])],
        ];
        let mut c = compiled(vec![seq("idle", false, frames)], 1);
        c.sequences[0].movements = vec![
            crate::model::Movement {
                endframe: 4,
                ..Default::default()
            },
            crate::model::Movement {
                endframe: 9,
                ..Default::default()
            },
        ];
        let out = write_with_f0_refs(&c, &[-1]).unwrap();
        let mv = out.movement_offsets[0].expect("应有 movement 区");
        // 从字节反解 stride：找第二个 `endframe`（= 9）落在哪。
        let stride = (0..96)
            .step_by(4)
            .find(|k| *k > 0 && read_i32(&out.anim_data, 0, mv, *k) == 9)
            .expect("应能在 movement 区里找到第二条记录的 endframe");
        assert_eq!(stride, 44, "mstudiomovement_t 的 stride 必须是 44 字节");
    }

    /// **核心判据**：七个字段的**字节偏移**逐一钉死。
    ///
    /// `studio.h:728-743` 的声明顺序（`endframe`/`motionflags`/`v0`/`v1`/
    /// `angle`/`vector`/`position`）如果被写错顺序或漏了字段，解码器读到的
    /// 就是错位的垃圾。这里给**每个字段一个互不相同**的哨兵值，再逐偏移读回。
    ///
    /// `angle` 特意给一个 **> π** 的值（197.32 度）：**弧度制下这个值不可能
    /// 出现**，所以这条同时钉住了「`angle` 存的是度、不做 deg↔rad 转换」。
    ///
    /// 真值来源：官方 `mvz.mdl`（`docs/_probe/smdl/mvz.qc` 的
    /// `walkframe 4/9 LX LY LZ LXR LYR LZR`）实测 `angle = 58.310089`
    /// （同样 > π），`parity/movement-angle.toml` 与它逐字段 20/20 相同。
    #[test]
    fn movement_field_offsets_and_degree_angle() {
        let frames = vec![
            vec![pose([0.0, 0.0, 0.0], [0.0; 3])],
            vec![pose([1.0, 0.0, 0.0], [0.0; 3])],
        ];
        let mut c = compiled(vec![seq("idle", false, frames)], 1);
        c.sequences[0].movements = vec![crate::model::Movement {
            endframe: 0x11223344,
            motionflags: 0x0BADF00D,
            v0: 193.882_39,
            v1: 24.911_356,
            angle: 197.321_6,
            vector: [-1.0, 2.5, -3.25],
            position: [-193.882_39, 8.100_102e-8, 7.5],
        }];
        let out = write_with_f0_refs(&c, &[-1]).unwrap();
        let mv = out.movement_offsets[0].expect("应有 movement 区");
        let d = &out.anim_data;
        let f32_at = |o: usize| f32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);

        assert_eq!(read_i32(d, 0, mv, 0x00), 0x11223344, "+0x00 endframe");
        assert_eq!(read_i32(d, 0, mv, 0x04), 0x0BADF00D, "+0x04 motionflags");
        assert_eq!(f32_at(mv + 0x08), 193.882_39, "+0x08 v0");
        assert_eq!(f32_at(mv + 0x0C), 24.911_356, "+0x0C v1");
        // 度：197.32 > π，弧度制下不可能 —— 顺带证明没做 deg→rad 转换。
        let ang = f32_at(mv + 0x10);
        assert_eq!(ang, 197.321_6, "+0x10 angle");
        assert!(
            ang > std::f32::consts::PI,
            "angle 应是**度**：197.32 > π = {}（若被当成弧度转换会变成 ~3.44）",
            std::f32::consts::PI
        );
        for (k, want) in [-1.0f32, 2.5, -3.25].iter().enumerate() {
            assert_eq!(f32_at(mv + 0x14 + k * 4), *want, "+0x14 vector[{k}]");
        }
        for (k, want) in [-193.882_39f32, 8.100_102e-8, 7.5].iter().enumerate() {
            assert_eq!(f32_at(mv + 0x20 + k * 4), *want, "+0x20 position[{k}]");
        }
        // 记录总长 = 0x20 + 12 = 44。
        assert_eq!(0x20 + 12, 44);
    }

    /// movement 数组写在**动画数据区末尾**，`nummovements` 与 `movementindex`
    /// 都要落盘（后者由 `write_mdl` 按「相对 animdesc 自身」回填）。
    ///
    /// 官方 `mv1`：`walkframe 4` + `walkframe 9` → 2 条记录。
    #[test]
    fn movement_written_at_end_with_count() {
        let frames = vec![
            vec![pose([0.0, 0.0, 0.0], [0.0; 3])],
            vec![pose([1.0, 0.0, 0.0], [0.0; 3])],
        ];
        let mut c = compiled(vec![seq("idle", false, frames)], 1);
        c.sequences[0].movements = vec![
            crate::model::Movement {
                endframe: 4,
                motionflags: 0,
                ..Default::default()
            },
            crate::model::Movement {
                endframe: 9,
                motionflags: 0,
                ..Default::default()
            },
        ];
        let out = write_with_f0_refs(&c, &[-1]).unwrap();
        // `nummovements` @0x14 = 2。
        assert_eq!(
            read_i32(&out.animdescs, 0, 0, 0x14),
            2,
            "nummovements 应为 2"
        );
        // movement 区必须在链之后（内联情形）。
        let mv = out.movement_offsets[0].expect("应有 movement 区");
        assert!(mv >= out.anim_offsets[0], "movement 应排在动画链之后");
        // 两条记录，各 44 字节（末尾可能补 ALIGN4 的 0 填充）。
        let mv_len = out.anim_data.len() - mv;
        assert!(
            (88..=91).contains(&mv_len),
            "2 × 44 = 88 字节（+最多 3 字节 ALIGN4 填充），实得 {mv_len}"
        );
        // 逐字段核对官方真值：两条的 endframe 是 4 与 9。
        assert_eq!(read_i32(&out.anim_data, 0, mv, 0), 4, "mv[0].endframe");
        assert_eq!(read_i32(&out.anim_data, 0, mv + 44, 0), 9, "mv[1].endframe");
    }

    /// 没有 movement 时 `movementindex` 保持 **0**
    /// （animdesc **内部**的相对偏移，不适用「空段写自然位置」）。
    #[test]
    fn no_movement_leaves_index_zero() {
        let frames = vec![vec![pose([0.0, 0.0, 0.0], [0.0; 3])]];
        let c = compiled(vec![seq("idle", false, frames)], 1);
        let out = write_with_f0_refs(&c, &[-1]).unwrap();
        assert_eq!(
            read_i32(&out.animdescs, 0, 0, 0x14),
            0,
            "nummovements 应为 0"
        );
        assert!(
            out.movement_offsets[0].is_none(),
            "无 movement 时不应有偏移"
        );
        assert_eq!(
            read_i32(&out.animdescs, 0, 0, 0x18),
            0,
            "movementindex 应为 0"
        );
    }

    /// **核心判据**：没有显式规则时，自动补一条 `IK_RELEASE`(4)。
    ///
    /// 官方 `ipkx1`/`ipr2`/`ipkx2` 与 `ikr*` 的对照真值：
    /// `type=4 chain=0 bone=0 slot=0`，`start=peak=contact=0`、`tail=end=1`，
    /// 且 `compressedikerrorindex == 0`（自动规则**没有**载荷）。
    #[test]
    fn auto_ik_release_rule_matches_official() {
        let c = ikr_compiled(Vec::new(), false);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        assert_eq!(out.num_ikrules, vec![1]);
        let w = read_rule(&out, 0, 0);
        assert_eq!(w[0], 0, "index");
        assert_eq!(w[1], 4, "type = IK_RELEASE");
        assert_eq!(w[2], 0, "chain");
        assert_eq!(w[3], 0, "bone");
        assert_eq!(w[4], 0, "slot");
        assert_eq!(w[0x3C / 4], 0, "自动规则 compressedikerrorindex 必须是 0");
        assert_eq!(w[0x44 / 4], 0, "iStart");
        assert_eq!(w[0x48 / 4], 0, "ikerrorindex");
        assert_eq!(read_f32(&out, 0, 0, 0x4C), 0.0, "start");
        assert_eq!(read_f32(&out, 0, 0, 0x50), 0.0, "peak");
        assert_eq!(read_f32(&out, 0, 0, 0x54), 1.0, "tail");
        assert_eq!(read_f32(&out, 0, 0, 0x58), 1.0, "end");
        assert_eq!(read_f32(&out, 0, 0, 0x60), 0.0, "contact");
        assert_eq!(w[0x78 / 4], 0, "szattachmentindex");
        for k in 0..7 {
            assert_eq!(w[0x7C / 4 + k], 0, "unused[{k}] 必须为 0");
        }
    }

    /// `noautoik` 抑制自动规则 —— 语料里 `@Melee_01`、`@Run_Shoot_KNIFE`
    /// 这类序列就是靠它才没有规则（`probe_ikrule_flags.js`）。
    #[test]
    fn blend_cells_and_orphan_animations_use_their_own_weights() {
        let mut compiled = ikr_compiled(Vec::new(), false);
        compiled.desc.model.version = Some(48);
        compiled.animations[0].weights = vec![0.0; 4];
        let mut weighted = compiled.animations[0].clone();
        weighted.name = "weighted".into();
        weighted.weights = vec![1.0; 4];
        compiled.animations.push(weighted);
        let mut orphan = compiled.animations[0].clone();
        orphan.name = "orphan".into();
        compiled.animations.push(orphan);
        compiled.sequences[0].cells = vec![0, 1];
        compiled.sequences[0].blend_width = 2;
        compiled.sequences[0].weights = vec![1.0; 4];

        let output = write_with_f0_refs(&compiled, &[-1, 0, 1, 2]).unwrap();
        assert_eq!(output.num_ikrules, [0, 1, 0]);
        for animation in [0, 2] {
            let offset = output.anim_offsets[animation];
            assert_eq!(&output.anim_data[offset..offset + 4], &[255, 0, 0, 0]);
        }
        assert_ne!(output.anim_data[output.anim_offsets[1]], 255);
    }

    #[test]
    fn no_auto_ik_suppresses_release_rule() {
        let c = ikr_compiled(Vec::new(), true);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        assert_eq!(out.num_ikrules, vec![0]);
        assert!(out.ikrule_offsets[0].is_none());
    }

    /// 有显式规则时**不再**补自动规则（`count[j] == 0` 的判据）。
    #[test]
    fn explicit_rule_replaces_auto_release() {
        let c = ikr_compiled(vec![touch_rule(Some("ankle"))], false);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        assert_eq!(out.num_ikrules, vec![1]);
        let w = read_rule(&out, 0, 0);
        assert_eq!(w[1], 1, "type = IK_SELF");
        assert_eq!(w[3], 3, "bone = ankle");
        assert_ne!(w[0x3C / 4], 0, "显式 touch 规则必须有载荷");
    }

    /// **核心判据**：`IK_SELF` 的误差 = `inverse(boneToWorld[bone]) ∘ boneToWorld[链末端]`。
    ///
    /// 官方 `ikr4.mdl`（`touch "knee"`）的载荷实测：
    ///
    /// ```text
    /// scale = [128/32767 ×3, (π/8)/32767 ×3]
    /// ch0 = {valid=1, total=4, [7679]}          ← pos.x = 30 恒定
    /// ch2 = {valid=4, total=4, [0,1279,2559,3839]} ← pos.z = 0/5/10/15
    /// ```
    #[test]
    fn ik_self_error_matches_official_ikr4() {
        let c = ikr_compiled(vec![touch_rule(Some("knee"))], false);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        let w = read_rule(&out, 0, 0);
        let payload = w[0x3C / 4] as usize;
        assert_eq!(payload, IK_RULE_SIZE, "载荷紧跟在规则头之后");
        assert_eq!(w[3], 2, "bone = knee");

        // 6 个 scale。
        let d = &out.anim_data;
        let base = out.ikrule_offsets[0].unwrap() + payload;
        let mut scale = [0.0f32; 6];
        for (k, s) in scale.iter_mut().enumerate() {
            let at = base + k * 4;
            *s = f32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
        }
        assert!((scale[0] - 128.0 / 32767.0).abs() < 1e-12, "位置 scale");
        assert!(
            (scale[3] - (std::f32::consts::PI / 8.0) / 32767.0).abs() < 1e-12,
            "旋转 scale"
        );

        // 6 个通道 offset（`offset[0] == 36` 恒）。
        let offs: Vec<i16> = (0..6)
            .map(|k| {
                let at = base + 24 + k * 2;
                i16::from_le_bytes([d[at], d[at + 1]])
            })
            .collect();
        assert_eq!(offs[0], COMPRESSED_IK_ERROR_SIZE as i16);

        // 逐通道解码（用「Σtotal == numerror」定界，第 6 通道没有后继 offset）。
        let decode = |k: usize, limit: usize, need: usize| -> Vec<i16> {
            let mut p = base + offs[k] as usize;
            let mut v = Vec::new();
            while p + 4 <= limit {
                let valid = d[p] as usize;
                let total = d[p + 1] as usize;
                p += 2;
                let mut last = i16::from_le_bytes([d[p], d[p + 1]]);
                p += 2;
                for t in 0..total {
                    v.push(last);
                    if t + 1 < valid {
                        last = i16::from_le_bytes([d[p], d[p + 1]]);
                        p += 2;
                    }
                }
                if v.len() >= need && need > 0 {
                    break;
                }
            }
            v
        };
        let ch0 = decode(0, base + offs[1] as usize, 0);
        assert_eq!(ch0, vec![7679, 7679, 7679, 7679], "pos.x = 30 恒定");
        let ch2 = decode(2, base + offs[3] as usize, 0);
        assert_eq!(ch2, vec![0, 1279, 2559, 3839], "pos.z = 0/5/10/15");
    }

    /// **核心判据**：帧号 → cycle 的换算 + `iStart` 写**帧号**。
    ///
    /// 官方 `ikr6.mdl`（`range 1 1 2 3 contact 2`，4 帧）实测：
    /// `iStart = 1`、`start = peak = 0.33333334`、`tail = contact = 0.6666667`、
    /// `end = 1.0`，`numerror = end − start + 1 = 3`。
    #[test]
    fn ik_rule_cycles_match_official_ikr6() {
        let mut r = touch_rule(Some("knee"));
        r.range = Some([Some(1), Some(1), Some(2), Some(3)]);
        r.contact = Some(2);
        let c = ikr_compiled(vec![r], false);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        let w = read_rule(&out, 0, 0);
        assert_eq!(w[0x44 / 4], 1, "iStart 是帧号");
        let d = 3.0f32; // numframes − 1
        assert_eq!(read_f32(&out, 0, 0, 0x4C), 1.0 / d, "start");
        assert_eq!(read_f32(&out, 0, 0, 0x50), 1.0 / d, "peak");
        assert_eq!(read_f32(&out, 0, 0, 0x54), 2.0 / d, "tail");
        assert_eq!(read_f32(&out, 0, 0, 0x58), 1.0, "end");
        assert_eq!(read_f32(&out, 0, 0, 0x60), 2.0 / d, "contact");
    }

    /// 单帧动画（`numframes <= 1`）走 `write.cpp:891-898` 的写死分支。
    #[test]
    fn single_frame_animation_writes_fixed_cycles() {
        let mut c = ikr_compiled(vec![touch_rule(Some("ankle"))], false);
        c.sequences[0].frames.truncate(1);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        assert_eq!(read_f32(&out, 0, 0, 0x4C), 0.0, "start");
        assert_eq!(read_f32(&out, 0, 0, 0x50), 0.0, "peak");
        assert_eq!(read_f32(&out, 0, 0, 0x54), 1.0, "tail");
        assert_eq!(read_f32(&out, 0, 0, 0x58), 1.0, "end");
        assert_eq!(read_f32(&out, 0, 0, 0x60), 0.0, "contact");
    }

    /// `IK_RELEASE`/`IK_UNLATCH` 的**显式**规则有载荷（全 0），
    /// 而自动补的那条没有 —— 这是 `numerror` 是否被 `ProcessIKRules`
    /// 填过的直接后果（官方 `ikr5`/`ikr7` 实测都有载荷）。
    #[test]
    fn explicit_release_has_zero_payload_but_auto_does_not() {
        for kind in [
            crate::model::IkRuleType::Release,
            crate::model::IkRuleType::Unlatch,
        ] {
            let r = crate::model::IkRule {
                chain: "leg".into(),
                kind,
                bone: None,
                attachment: None,
                target: None,
                height: None,
                radius: None,
                pad: None,
                floor: None,
                range: None,
                contact: None,
                fake_origin: None,
                fake_rotate: None,
                use_source: false,
            };
            let c = ikr_compiled(vec![r], false);
            let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
            let w = read_rule(&out, 0, 0);
            assert_eq!(w[1], kind.code());
            assert_ne!(w[0x3C / 4], 0, "显式 {kind:?} 规则必须有（全 0 的）载荷");
        }
    }

    /// `attachment` 字符串**内联**写在载荷之后，`szattachmentindex`
    /// 相对**规则自身** —— 官方 `ikr3.mdl` 实测 **218**。
    #[test]
    fn attachment_string_is_inline_and_relative() {
        let r = crate::model::IkRule {
            chain: "leg".into(),
            kind: crate::model::IkRuleType::Attachment,
            bone: None,
            attachment: Some("Hand_L".into()),
            target: Some(0),
            height: None,
            radius: None,
            pad: None,
            floor: None,
            range: None,
            contact: None,
            fake_origin: None,
            fake_rotate: None,
            use_source: false,
        };
        let c = ikr_compiled(vec![r], false);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        let w = read_rule(&out, 0, 0);
        assert_eq!(w[1], 5, "type = IK_ATTACHMENT");
        assert_eq!(w[3], 3, "bone 被改写成链末端骨骼");
        let sai = w[0x78 / 4] as usize;
        assert_eq!(sai, 218, "官方 ikr3.mdl 实测 218");
        let at = out.ikrule_offsets[0].unwrap() + sai;
        assert_eq!(&out.anim_data[at..at + 7], b"Hand_L\0");
        // `pos`/`q` 来自 `contact` 帧附着骨骼的世界变换
        // （官方 = `[0, 60, 0]` / 绕 Z 90°）。
        assert!((read_f32(&out, 0, 0, 0x20) - 0.0).abs() < 1e-3, "pos.x");
        assert!((read_f32(&out, 0, 0, 0x24) - 60.0).abs() < 1e-3, "pos.y");
        assert!(
            (read_f32(&out, 0, 0, 0x38) - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5,
            "q.w"
        );
    }

    /// `ikruleindex` 是**相对 animdesc 自身**的偏移，且 `numikrules == 0`
    /// 时两个 index 都写 0（唯一违反「空段也写自然偏移」的地方）。
    #[test]
    fn ikruleindex_is_relative_to_animdesc_via_layout() {
        // 这条覆盖的是 `mdl_writer` 的回填逻辑：偏移 = 块在 `anim_data`
        // 内的位置 + `anim_data` 的绝对起点 − animdesc 的绝对起点。
        let c = ikr_compiled(Vec::new(), false);
        let out = write_with_f0_refs(&c, &[-1, 0, 1, 2]).expect("写出动画");
        let off = out.ikrule_offsets[0].expect("有规则");
        // 块**紧跟**在链之后（`stats[0].anim_bytes` 就是链长）。
        assert_eq!(
            off,
            out.anim_offsets[0] + out.stats[0].anim_bytes,
            "IK 块必须紧跟在动画链之后"
        );
        assert_eq!(
            out.anim_data.len() - off,
            IK_RULE_SIZE,
            "单条规则正好 152 字节"
        );
        // `animindex` / `ikruleindex` 都以 animdesc 自身为基准，二者之差
        // 就是链长 —— 与官方 `ikr4.mdl` 的 `148 − 104 = 44` 是同一个量
        // （数值不同只因 mdlc 不做动画 run 合并）。
        assert_eq!(off - out.anim_offsets[0], out.stats[0].anim_bytes);
    }

    /// **核心判据**：官方的 RLE 编码逐字节复刻。
    ///
    /// 真值取自 `artifacts/ikr4.mdl` 的实际字节：
    ///
    /// ```text
    /// ch0 = {valid=1, total=4, [7679]}          → 01 04 ff 1d
    /// ch2 = {valid=4, total=4, [0,1279,2559,3839]}
    ///                                            → 04 04 00 00 ff 04 ff 09 ff 0e
    /// ```
    #[test]
    fn rle_encode_matches_official_bytes() {
        assert_eq!(
            rle_encode(&[7679, 7679, 7679, 7679]),
            vec![1, 4, 0xff, 0x1d]
        );
        assert_eq!(
            rle_encode(&[0, 1279, 2559, 3839]),
            vec![4, 4, 0x00, 0x00, 0xff, 0x04, 0xff, 0x09, 0xff, 0x0e]
        );
        assert_eq!(rle_encode(&[0, 0, 0, 0]), vec![1, 4, 0, 0]);
        assert!(rle_encode(&[]).is_empty());
    }

    /// `total` 是 `u8`：run 到 255 必须强制开新记录
    /// （`simplify.cpp:6748-6756`）。
    #[test]
    fn rle_encode_splits_runs_at_255() {
        let v = vec![7i16; 300];
        let e = rle_encode(&v);
        assert_eq!(e[0], 1, "第一段 valid");
        assert_eq!(e[1], 255, "第一段 total = 255");
        // 第一段占 2（头）+ 2（1 个采样）= 4 字节，第二段的头紧随其后。
        assert_eq!(e[4], 1, "第二段 valid");
        assert_eq!(e[5], 45, "第二段 total = 300 − 255 = 45");
    }

    /// **`AxisData::Constant` 超过 255 帧也必须拆 run。**
    ///
    /// # 这是「@item_loop 第 255 帧后骨骼错乱」的根因
    ///
    /// `total` 是 `u8`，而 `Constant` 分支早先写的是
    /// `u8::try_from(n).unwrap_or(u8::MAX)` —— 把长度**截断**到 255。
    /// 引擎按 `total` 复制采样 ⟹ **第 255 帧之后完全没有数据**。
    ///
    /// 实测（`v_dual_pistola`，`$sectionframes 30 999` 让全部动画不分段）
    /// `@item_loop`（**325 帧**）的 `b2 Camera`：
    ///
    /// ```text
    /// 官方 pos[0] spans: v1/t255 v1/t70 …    ← 255 + 70 = 325
    /// mdlc 修前        : 单条 v1/t255         ← 只覆盖 255 帧
    /// ```
    ///
    /// 症状：前 255 帧完全正常，之后位移差最大 **71.12**、旋转差 **17.76°**
    /// （4 条 325 帧动画、共 **198** 个坏帧）。
    #[test]
    fn constant_axis_splits_at_255_frames() {
        // `write_axis` 是纯函数（只吃 `n` 与 `AxisData`），不需要编译产物。
        let mut buf: Vec<u8> = vec![0, 0]; // 给 valueptr 留 2 字节
        write_axis(&mut buf, 0, 0, &AxisData::Constant(1234), 325).expect("写常量轴");
        // buf = [ptr_lo, ptr_hi, run1...]
        let off = i16::from_le_bytes([buf[0], buf[1]]) as usize;
        assert_eq!(off, 2, "通道数据紧跟在 valueptr 之后");
        // 第一条 run：{1, 255}，采样 1234
        assert_eq!(buf[off], 1, "run1 valid");
        assert_eq!(buf[off + 1], 255, "run1 total = 255");
        assert_eq!(i16::from_le_bytes([buf[off + 2], buf[off + 3]]), 1234);
        // 第二条 run：{1, 70}，同一个采样 —— 255 + 70 = 325
        assert_eq!(buf[off + 4], 1, "run2 valid");
        assert_eq!(buf[off + 5], 70, "run2 total = 325 − 255 = 70");
        assert_eq!(i16::from_le_bytes([buf[off + 6], buf[off + 7]]), 1234);
        assert_eq!(buf.len(), off + 8, "恰好两条 run，没有多余字节");
    }

    /// **恒定轴在 255 帧以内仍然只写一条 run**（回归护栏）。
    ///
    /// 拆分只在**超过 255** 时发生 —— 否则会把绝大多数正常动画写胖
    /// （每条常量轴多 4 字节），破坏 parity 的逐字节一致。
    #[test]
    fn constant_axis_does_not_split_at_or_below_255() {
        for n in [1usize, 2, 30, 255] {
            let mut buf: Vec<u8> = vec![0, 0];
            write_axis(&mut buf, 0, 0, &AxisData::Constant(-77), n).expect("写常量轴");
            assert_eq!(buf.len(), 2 + 4, "n={n} 应恰好一条 run（头 2 + 采样 2）");
            assert_eq!(buf[2], 1, "n={n} valid");
            assert_eq!(buf[3], n as u8, "n={n} total");
        }
    }

    /// **孤儿动画（无序列引用）不能继承 `seq[0]` 的 `section_frames`。**
    ///
    /// 官方按**全局** `$sectionframes <段长> <阈值>` 的阈值判
    /// （`nf >= 阈值`）。mdlc 的 `anim_specs` 用
    /// `.position(|s| s.cells.contains(&i)).unwrap_or(0)` 反查引用序列，
    /// 孤儿动画会落到 `seq[0]` ⟹ 继承它的 `section_frames`。
    ///
    /// 实测 `v_dual_pistola_processed.qc`（`$sectionframes 30 36`）：
    /// `look_neutral` 只被 `$animation` 声明，官方 `sf=0`，
    /// mdlc 修前 `sf=30` + 一张 `nEnt=2` 的段表。
    #[test]
    fn orphan_animation_uses_global_sectionframes_not_seq0() {
        let mut c = compiled(
            vec![seq("idle", false, vec![vec![pose([0.0; 3], [0.0; 3])]; 40])],
            1,
        );
        // `seq[0]`（idle）分段：40 帧 >= 阈值 36 ⟹ sf = 30。
        c.sequences[0].section_frames = 30;
        // 全局值（官方 `g_sectionframes`）。
        c.desc.model.section_frames = Some((30, 36));
        // 再加一条**没有任何序列引用**的 1 帧动画。
        c.animations.push(crate::model::CompiledAnimation {
            local_hierarchy: Vec::new(),
            weights: Vec::new(),
            loop_restart: 0,
            fudge_loop: false,
            extra_flags: 0,
            movements: Vec::new(),
            name: "orphan".into(),
            smd_path: "x.smd".into(),
            fps: 30.0,
            looping: false,
            frames: vec![vec![pose([0.0; 3], [0.0; 3])]],
            delta: false,
            ik_rules: Vec::new(),
            no_auto_ik: false,
            pre_subtract_frames: None,
        });

        let specs = anim_specs(&c);
        let orphan = specs
            .iter()
            .find(|s| s.anim_index == 1)
            .expect("应能拿到孤儿动画的 spec");
        assert!(orphan.orphan, "第 2 条动画没有任何序列引用 ⟹ orphan");
        assert!(!specs[0].orphan, "idle 被序列引用 ⟹ 不是 orphan");

        let out = write_with_f0_refs(&c, &[-1]).expect("写出动画");
        assert_eq!(
            out.section_frames[1], 0,
            "孤儿动画 1 帧 < 阈值 36 ⟹ sectionframes 必须是 0（官方口径）；\
             修前会继承 seq[0] 的 30"
        );
        assert_eq!(out.section_frames[0], 30, "被引用的 40 帧动画仍应是 30");
    }
}
