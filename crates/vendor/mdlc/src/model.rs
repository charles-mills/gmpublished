//! 模型描述 IR —— **与 QC 无关**的中间表示。

use serde::{Deserialize, Serialize};

/// 一个模型的完整描述（TOML 的顶层）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDesc {
    pub model: ModelMeta,
    /// **物理 / 碰撞模型**（`$collisionmodel` / `$collisionjoints` 块）。
    ///
    /// 独立成一张表，不再堆在 `[model]` 里 —— 见 [`Physics`] 的说明。
    #[serde(default)]
    pub physics: Physics,
    /// `$cdmaterials`：材质搜索目录。
    #[serde(default)]
    pub materials: Materials,
    /// 骨骼表。根骨骼必须排在最前，且 `parent` 只能指向**更靠前**的骨骼
    /// （studiomdl 要求父骨骼先于子骨骼出现）。
    #[serde(default)]
    pub bones: Vec<Bone>,
    /// body part → model → mesh 树。
    #[serde(default)]
    pub bodyparts: Vec<BodyPart>,
    /// hitbox set（`$hboxset` / `$hbox`）。
    #[serde(default)]
    pub hitboxes: Hitboxes,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_hitbox_sets: Vec<Hitboxes>,
    /// 附着点（`$attachment`）。
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// 序列（`$sequence`）。
    #[serde(default)]
    pub sequences: Vec<Sequence>,
    /// 已声明的动画（`$animation`）。
    ///
    /// # 为什么单独一张表
    ///
    /// 官方把 animdesc 放在一个**全局池** `g_panimation[]` 里，
    /// `$sequence` 只是**引用**它（`studiomdl.cpp:2952-2959`）。
    /// 所以「一个 `$animation` 一个 animdesc」，同一格被多条序列引用
    /// 也只有**一份**数据。
    ///
    /// 顺序 = 写出顺序 = **本数组的顺序**，隐含动画（`$sequence` 里
    /// 直接给 SMD 路径的那些）排在**全部显式动画之后**。
    #[serde(default)]
    pub animations: Vec<Animation>,
    /// 骨骼控制器（`$controller`）。
    ///
    /// 实测 **3333/3333** 个真实 L4D2 模型的 `numbonecontrollers == 0`
    /// —— 这个特性在 L4D2 里已废弃。但段本身仍占位（偏移字段指向
    /// 骨骼表末尾），所以实现里保留这个段。
    #[serde(default)]
    pub bonecontrollers: Vec<BoneController>,
    /// IK 链（`$ikchain`）。
    #[serde(default)]
    pub ikchains: Vec<IkChain>,
    /// IK 自动播放锁（`$ikautoplaylock`）。
    #[serde(default)]
    pub ik_autoplay_locks: Vec<IkAutoplayLock>,
    /// flexdesc（`mstudioflexdesc_t`，QC 里由 `flex`/`eyelid`/`mouth`/`localvar` 等注册）。
    #[serde(default)]
    pub flex_descriptors: Vec<FlexDescriptor>,
    /// flexcontroller（`mstudioflexcontroller_t`，QC 的 `flexcontroller`）。
    #[serde(default)]
    pub flex_controllers: Vec<FlexController>,
    /// flexrule（`mstudioflexrule_t` + 内联 `mstudioflexop_t[]`，QC 的 `%<flex> = <expr>`）。
    #[serde(default)]
    pub flex_rules: Vec<FlexRule>,
    /// flexcontrollerui（`mstudioflexcontrollerui_t`）。
    ///
    /// **每条 `[[flex_controllers]]` 会自动产生一条对应的 ui**（见
    /// [`FlexControllerUi`] 文档）；本表仅在需要 stereo 对 / 自定义名时
    /// **额外**给（DMX 形态）。
    #[serde(default)]
    pub flex_controller_ui: Vec<FlexControllerUi>,
    /// mouth（`mstudiomouth_t`，QC 的 `$model { … mouth … }`）。全局数组。
    #[serde(default)]
    pub mouths: Vec<Mouth>,
    /// jigglebone（`mstudiojigglebone_t`，**120 字节**；QC 的 `$jigglebone`）。
    ///
    /// **L4D2 独有** —— episode1 源码 grep `jiggle` 零命中。
    /// 不走段头：它是**骨骼数组的延伸**，块起点
    /// `ALIGN4(boneindex + numbones*216)`，在 `bonecontroller` **之前**。
    #[serde(default)]
    pub jiggle_bones: Vec<JiggleBone>,
    /// quatinterp（`mstudioquatinterpbone_t` + `mstudioquatinterpinfo_t`；
    /// QC 的 `$proceduralbones <.vrd>` 里 `proctype 2` 的那些）。
    ///
    /// 与 jigglebone 一样**不走段头** —— 是骨骼数组的延伸，块起点
    /// `ALIGN4(boneindex + numbones*216)`，按 **proctype 升序**排在
    /// `bonecontroller` 之前。quatinterp（2）在 jiggle（5）**之前**。
    ///
    /// 语料频率：**4 个模型 / 42 根骨骼 / 172 个触发器**
    /// （`survivor_gambler` 16、`survivor_coach` 10、`survivor_mechanic` 8、
    /// `survivor_producer` 8）。
    #[serde(default)]
    pub quat_interp_bones: Vec<QuatInterpBone>,
    /// `$includemodel`（`mstudiomodelgroup_t`，8 字节/条）。顶层数组。
    ///
    /// # 官方**只写名字**，从不读被包含的 `.mdl`
    ///
    /// 全 studiomdl 里 `g_numincludemodels` 只出现 2 次
    /// （`write.cpp` 写出、`simplify.cpp:7222` 一个合法性检查）——
    /// 合并骨骼/序列是**引擎**运行时做的（`virtualmodel_t`），
    /// studiomdl 不参与。所以这里只存路径字符串，指不存在的文件也能编译。
    ///
    /// # 官方自动加 `models/` 前缀
    ///
    /// `Cmd_IncludeModel`（`studiomdl.cpp:5961-5967`）把 `"models/"` 直接
    /// `strcat` 到 token 前面 —— QC 里写 `"infected/anim_boomer.mdl"`
    /// 会得到 `models/infected/anim_boomer.mdl`。
    /// **TOML 里写完整路径**（含 `models/`），不替用户补前缀 ——
    /// 语料 47/47 的名字都以 `models/` 开头，写全更明确。
    #[serde(default)]
    pub include_models: Vec<String>,
    /// **权重表**（QC 的 `$weightlist`）。
    ///
    /// # 它做什么
    ///
    /// 每条序列在 `mstudioseqdesc_t.weightlistindex`（`+0x9C`）指向一张
    /// `float[numbones]`，引擎用它做**增量动画（`delta`）的重建缩放**：
    ///
    /// ```c
    /// float s = panimation->weight[k];
    /// QuaternionMA( q1, s, q2, q3 );        // q3 = q1 按 s 插值到 q2
    /// p3 = base.pos + s * delta.pos;
    /// ```
    ///
    /// 所以 `s = 0` 的骨骼**完全不参与**该序列的增量叠加（保持基准姿态），
    /// `s = 0.5` 只叠加一半。这正是模组作者用它做「只让上半身动」的手法
    /// （实测真实模组：`v_katana` 的 `empty`、`v_autoshotgun` 的
    /// `weights_fire_layer`、survivor 的 `INJUREDIDLENOISE`）。
    ///
    /// # 索引 0 是隐式的（`$defaultweightlist`）
    ///
    /// 官方 `g_weightlist[0]` 是一张**隐式默认表**，不需要手写：
    ///
    /// * 表 0 = 全 1；
    /// * 具名表 `i != 0` = 根骨骼 **0**、其余骨骼**沿父链继承**。
    ///
    /// 见 [`WeightList`] 的完整算法与实测判据。
    ///
    /// # 放在顶层而不是 `[model]`
    ///
    /// 与 `[[bones]]` / `[[bodyparts]]` 同级 —— 它是**模型级**的表集合，
    /// 序列按名字引用它。
    #[serde(default)]
    pub weight_lists: Vec<WeightList>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_weight_list: Vec<WeightEntry>,
    /// 命令列表（QC 的 `$cmdlist "<名>" { ... }`）。
    ///
    /// # 语义
    ///
    /// 官方 `Cmd_Cmdlist`（`studiomdl.cpp:2317-2378`）在**解析期**把
    /// `{ }` 里的命令逐条解析进 `g_cmdlist[].cmds[]`（`MAXSTUDIOCMDS` = 64）；
    /// 之后动画里的 `cmdlist "<名>"` 只是把这些命令**按值拷贝**过去
    /// （`ParseAnimationToken` `:2282-2298`）。
    ///
    /// ⚠️ **没有重名检查**（同名取**先定义**的那个），也**没有上限检查**
    /// —— 官方数组是 `g_cmdlist[MAXSTUDIOANIMS]`（2000）。
    #[serde(default)]
    pub cmd_lists: Vec<CmdList>,
}

/// 一张具名命令列表（QC 的 `$cmdlist "<名>" { ... }`）。
///
/// 见 [`ModelDesc::cmd_lists`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CmdList {
    /// 列表名。动画里用 `cmdlist "<名>"` 引用（`stricmp`，大小写不敏感）。
    pub name: String,
    /// 命令序列，**按 QC 源码顺序**。
    ///
    /// 顺序是有意义的：官方 `processAnimations()`（`simplify.cpp:154-289`）
    /// 就是**按这个顺序**逐条执行的，没有按种类排序。
    #[serde(default)]
    pub cmds: Vec<AnimCmd>,
}

/// 一条动画命令 —— 官方 `s_animcmd_t`（`studiomdl.h:361-453`）的 Rust 表示。
///
/// # 为什么不用官方那种 `int cmd` + `union` 的写法
///
/// 官方的 `union u` 里有**三处成员写错**（`ikfixup` 写 `u.ikrule.pRule`、
/// `numframes` 写 `u.compress.frames`），靠 union 同址才「碰巧能用」。
/// 用带载荷的 enum 表达同样语义，且不会把这类笔误当成语义。
///
/// 判别值见 `studiomdl.h:325-344` 的 `CMD_*` 宏。⚠️ `CMD_SPLINEDELTA`(14)
/// 与 `CMD_SETBONE`(18) 官方**从未赋值**（`splinedelta` 关键字产生的是
/// `CMD_LINEARDELTA` + `STUDIO_AL_SPLINE`），故这里没有对应变体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnimCmd {
    /// `weightlist <名>` —— 官方 `CMD_WEIGHTS`。名字必须命中
    /// `g_weightlist[1..]`（下标 0 是 `$defaultweightlist`，**不可按名选中**）。
    Weights {
        /// 权重表名（QC 里 `$weightlist` 的键）。
        weight_list: String,
    },
    /// `subtract <动画> <帧>` / `presubtract <动画> <帧>` —— 官方 `CMD_SUBTRACT`。
    ///
    /// 两者**只有 `post` 一位不同**：`subtract` 置 `STUDIO_POST`，
    /// `presubtract` 不置（`studiomdl.cpp:1750`）。
    Subtract {
        /// 参考动画名。
        reference: String,
        /// 参考动画的第几帧。
        frame: i32,
        /// 是否置 `STUDIO_POST`（`subtract` = true，`presubtract` = false）。
        #[serde(default)]
        post: bool,
    },
    /// `alignto` / `align` / `alignboneto` —— 官方 `CMD_AO`。
    Align {
        /// 参考动画名。
        reference: String,
        /// 控制位掩码（`STUDIO_X|STUDIO_Y` 等）。
        motion_type: i32,
        src_frame: i32,
        dest_frame: i32,
        /// 只有 `alignboneto` 写它；`alignto`/`align` 官方置 `NULL`（= 根骨骼）。
        #[serde(default)]
        bone: Option<String>,
    },
    /// `match <动画>` —— 官方 `CMD_MATCH`。
    Match {
        /// 参考动画名。
        reference: String,
    },
    /// `fixuploop <起> <止>` —— 官方 `CMD_FIXUP`。
    FixupLoop { start: i32, end: i32 },
    /// `rotateto <角度>` —— 官方 `CMD_ANGLE`。
    Angle { angle: f32 },
    /// `ikfixup <链> <类型> ...` —— 官方 `CMD_IKFIXUP`。
    ///
    /// 与 [`Self::IkRule`] 的区别：官方对它是**栈拷贝**后直接
    /// `fixupIKErrors`，**不进** `panim->ikrule[]` 数组。
    IkFixup {
        /// IK 规则载荷。
        rule: IkRule,
    },
    /// `ikrule <链> <类型> ...` —— 官方 `CMD_IKRULE`。
    ///
    /// `processAnimations()` 里是**显式 no-op**（注释 `// processed later`），
    /// 真正收集在 `ProcessIKRules()`（`simplify.cpp:5819-5841`）——
    /// 它把规则拷进 `panim->ikrule[]`，与 `$sequence` 块里内联写的
    /// `ikrule` **完全等价**。
    IkRule {
        /// IK 规则载荷。
        rule: IkRule,
    },
    /// `walkframe <末帧> <控制位...>` —— 官方 `CMD_MOTION`。
    ///
    /// ⚠️ 官方 `s_motion_t.iStartFrame` **从未被解析路径写过** ——
    /// 它来自 `processAnimations()` 里那个滚动游标 `startframe`。
    Motion { motion_type: i32, end_frame: i32 },
    /// `walkalignto` / `walkalign` —— 官方 `CMD_REFMOTION`。
    RefMotion {
        motion_type: i32,
        end_frame: i32,
        src_frame: i32,
        /// 参考动画名。
        reference: String,
        ref_frame: i32,
    },
    /// `derivative <缩放>` —— 官方 `CMD_DERIVATIVE`。
    Derivative { scale: f32 },
    /// `noanimation` —— 官方 `CMD_NOANIMATION`（不读任何 token）。
    NoAnimation,
    /// `lineardelta` / `splinedelta` —— 官方 `CMD_LINEARDELTA`。
    ///
    /// `lineardelta` 置 `STUDIO_AL_POST`(0x10)；`splinedelta` 再或上
    /// `STUDIO_AL_SPLINE`(0x40)。⚠️ 两者都是 `CMD_LINEARDELTA`，
    /// `CMD_SPLINEDELTA` 是死常量。
    LinearDelta { flags: i32 },
    /// `compress <帧数>` —— 官方 `CMD_COMPRESS`。
    Compress { frames: i32 },
    /// `numframes <帧数>` —— 官方 `CMD_NUMFRAMES`。
    ///
    /// ⚠️ 官方写的是 `u.compress.frames`（union 成员笔误，同址无害）；
    /// 这里按语义存。
    NumFrames { frames: i32 },
    LocalHierarchy {
        bone: String,
        parent: String,
        range: [i32; 4],
    },
    /// `counterrotate <骨骼>` / `counterrotateto <pitch> <yaw> <roll> <骨骼>`。
    CounterRotate {
        /// 骨骼名。
        bone: String,
        /// `counterrotateto` 的目标角；`counterrotate` 为 `None`。
        #[serde(default)]
        target_angle: Option<[f32; 3]>,
    },
    /// `worldspaceblend` / `worldspaceblendloop` —— 官方 `CMD_WORLDSPACEBLEND`。
    WorldSpaceBlend {
        /// 参考动画名。
        reference: String,
        start_frame: i32,
        /// `worldspaceblendloop` = true。
        loops: bool,
    },
    /// `matchblend <动画> <src> <dest> <pre> <post>` —— 官方 `CMD_MATCHBLEND`。
    MatchBlend {
        /// 参考动画名。
        reference: String,
        src_frame: i32,
        dest_frame: i32,
        dest_pre: i32,
        dest_post: i32,
    },
}

/// 一张具名权重表（QC 的 `$weightlist "<名>" { <骨骼> <权重> ... }`）。
///
/// # 语义（`buildAnimationWeights`，`simplify.cpp:1646-1719`）
///
/// **不是**「没列出的骨骼就是 1」。真实算法分三步：
///
/// ```c
/// // ① 初始化
/// if (i == 0) {                       // 隐式默认表
///     root    -> 1
///     child   -> -1                   // 「未初始化」哨兵
/// } else {
///     root    -> 0
///     child   -> g_weightlist[0].weight[j]   // ← 此刻表 0 的子骨骼还是 -1！
/// }
/// // ② 显式条目覆盖
/// weight[findGlobalBone(name)] = w
/// // ③ 沿父链补齐（j 升序，父一定在子之前）
/// if (weight[j] < 0) weight[j] = weight[parent[j]]
/// ```
///
/// ⚠️ **第 ① 步的 `-1` 是关键的**：`i != 0` 的表抄表 0 时，表 0 的子骨骼
/// **尚未**被第 ③ 步补齐（第 ③ 步在**全部**表初始化完之后才跑），
/// 所以抄到的是 `-1` ⟹ 最终由**第 ③ 步沿父链继承**决定。
///
/// # 实测判据（`docs/_probe/probe_weightlist_semantics.js`）
///
/// 骨骼链 `root → mid → leaf → tip`，读官方产物的
/// `seqdesc.weightlistindex` 指向的 `float[4]`：
///
/// | QC | root | mid | leaf | tip |
/// |---|---|---|---|---|
/// | 无 `$weightlist` | 1.0 | 1.0 | 1.0 | 1.0 |
/// | `$weightlist WL root 1` | 1.0 | 1.0 | 1.0 | 1.0 |
/// | `$weightlist WL root 0` | 0.0 | 0.0 | 0.0 | 0.0 |
/// | `$weightlist WL mid 0.5` | **0.0** | 0.5 | **0.5** | **0.5** |
/// | `$weightlist WL leaf 0` | 0.0 | 0.0 | 0.0 | 0.0 |
///
/// `mid 0.5` 那行最能说明问题：**根是 0**（不是 1），
/// 而 `leaf`/`tip` 跟着 `mid` 变成 0.5（沿父链继承）。
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightList {
    /// 表名。序列/动画通过它引用。
    pub name: String,
    /// 显式条目。**没列出的骨骼走父链继承**（不是「默认 1」）。
    #[serde(default)]
    pub bones: Vec<WeightEntry>,
}

/// 权重表里的一条（QC 的 `<骨骼> <权重> [posweight <权重>]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightEntry {
    /// 骨骼名。
    pub bone: String,
    /// 旋转权重 —— 落盘进 `seqdesc.weightlistindex` 指向的 `float[]`。
    ///
    /// 官方缺省 = 与 `weight` 相同（`Option_Weightlist` 的
    /// `boneposweight[i] = boneweight[i]`）。
    pub weight: f32,
    /// 位置权重（QC 的 `posweight <v>`）。
    ///
    /// # 它**不落盘**
    ///
    /// `write.cpp:587` 只写 `weight`（`pweight[j] = g_sequence[i].weight[j]`），
    /// `posweight` 只参与编译期的 `solveBone` / IK 误差计算。
    ///
    /// 实测佐证：`probe_weightlist_semantics.js` 里
    /// `$weightlist WL mid 0.5 posweight 0.25` 的**落盘数组**
    /// 与 `mid 0.5` 完全相同（都是 `[0, 0.5, 0.5, 0.5]`）。
    ///
    /// 缺省 = 与 [`Self::weight`] 相同。
    #[serde(default)]
    pub pos_weight: Option<f32>,
}

impl WeightEntry {
    /// 位置权重（缺省取 `weight`）。
    pub fn pos_weight(&self) -> f32 {
        self.pos_weight.unwrap_or(self.weight)
    }
}

/// 一张权重表的条目上限 —— **实测**官方 L4D2 `MAXWEIGHTSPERLIST`。
///
/// 超出时 `Option_Weightlist` 报 `Too many bones (128) in weightlist '%s'`
/// （`studiomdl.cpp:3310-3313`）。
///
/// # ⚠️ 这个值是**跑出来的**，不是从 SDK 头文件抄的
///
/// `hl2sdk-episode1/utils/studiomdl/studiomdl.h:980` 写的是 **16**，
/// 但真 `studiomdl.exe` 接受 **128** 条、第 129 条才报错
/// （`docs/_probe/cmp_weightlist_errors.js` + 手工二分：
/// 127 OK / 128 OK / 129 ERROR，2000 条时报的还是 `(128)`）。
///
/// 这正是本项目反复强调的：**SDK 头文件不是二进制**。
/// 按 16 去写校验会把真实模组（作者会列上百根骨骼）误判为非法。
pub const MAX_WEIGHT_ENTRIES: usize = 128;

/// 权重表**张数**上限 —— **实测**官方 L4D2 `MAXWEIGHTLISTS`，
/// **含隐式的表 0**（`studiomdl.cpp:6886` `g_numweightlist = 1`）。
/// 所以手写表最多 127 张。
///
/// 超出时 `Cmd_Weightlist` 报 `Too many weightlist commands (128)`。
///
/// ⚠️ 同样是实测值：episode1 的 `studiomdl.h:979` 写的是 **32**，
/// 而真二进制是 128（127 张 OK / 128 张 ERROR）。
pub const MAX_WEIGHT_LISTS: usize = 128;

/// 一条 `$jigglebone`（`mstudiojigglebone_t`，**120 字节** = 30 个 4 字节槽）。
///
/// # 字段偏移（实测，见 `PROGRESS.md` §11.1）
///
/// 调研报告 §1.5 的表格**整体错位**（把 `0x38` 当 padding、
/// `0x3C` 当 `minPitch`）—— 这里的顺序以 `jig1.mdl` 的 QC 值逐槽反解为准：
/// `minPitch = -20° = -0.34906585` 落在 `0x38`。
///
/// # 语义：**扁平记录 + token 顺序「后写覆盖」**
///
/// ⚠️ 这一条是反编译 + 受控实验一起钉死的，**反直觉**：
/// 官方解析器并**不**为三个块各存一份、再按固定块优先级合并。
/// 它只有**一个扁平记录**（`+0x88` 起 30 个槽），每个键**按 QC 里出现的
/// 先后顺序**直接写进那一个记录，**后写覆盖先写**；`flags` 则是**按位 OR 累积**
/// （但 `is_flexible` 进入时置的 `0x20` 会被 `allow_length_flex` 清掉，
/// 所以 `flags` 也依赖顺序）。
///
/// 决定性对照（两个夹具只有**块顺序**不同，值完全一样，结果却不同）：
///
/// | 夹具 | QC 写法 | 实测 `min_yaw`/`max_yaw` |
/// |---|---|---|
/// | `jig47` | `is_rigid { yaw_constraint -10 20 }` → `has_base_spring { yaw_constraint -30 40 }` | `−30°`/`40°` |
/// | `jig48` | `has_base_spring { yaw_constraint -30 40 }` → `is_rigid { yaw_constraint -10 20 }` | `−10°`/`20°` |
///
/// ⟹ **任何「固定块合并顺序」的实现都无法同时复现这两个**。
///
/// # 两种输入形式（互斥，`validate()` 会拒绝同时出现）
///
/// * **`writes`**（QC 路径）：官方语义的**无损**表示 —— 一个按 token 顺序排的
///   写入日志。QC 解析器只填它。
/// * **三个块**（TOML 路径）：手写 TOML 的**便捷**语法，按固定顺序
///   `is_flexible` → `is_rigid` → `has_base_spring` 依次应用（同块内按字段序）。
///   它**表达不了** `jig47`/`jig48` 那种顺序差异 —— 需要精确顺序时用 `writes`。
///
/// 两条路径最终都收敛到同一个扁平记录，**角度输入都是「度」、写盘转弧度**
/// （`angle_constraint 60` → `π/3`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct JiggleBone {
    /// 目标骨骼名（编译期解析成下标）。
    pub bone: String,
    /// `is_flexible { … }` 块（存在 ⟹ `flags |= 0x01`）。
    #[serde(default)]
    pub is_flexible: Option<JiggleFlexible>,
    /// `is_rigid { … }` 块（存在 ⟹ `flags |= 0x02`）。
    #[serde(default)]
    pub is_rigid: Option<JiggleRigid>,
    /// `has_base_spring { … }` 块（存在 ⟹ `flags |= 0x40`）。
    #[serde(default)]
    pub has_base_spring: Option<JiggleBaseSpring>,
    /// **按 QC token 顺序**的写入日志 —— 官方语义的无损表示。
    ///
    /// 非空 ⟹ 三个块被**忽略**（`validate()` 会拒绝两者同时出现）。
    /// 空 ⟹ 走三个块的固定顺序合并。
    ///
    /// `qc2toml` 会把它原样写出来，所以 QC → TOML → 编译是**无损**的。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<JiggleWrite>,
}

/// `$jigglebone` 里**一条**按 token 顺序记录的写入。
///
/// `key` 用**官方键名**（小写）；三个块名也走同一条通道，`values` 为空，
/// 表示「进入该块」（置 `flags`）。这样 `flags` 的累积顺序也一并被保真。
///
/// ⚠️ `values` 存的是 QC 里的**原始数值**（角度仍是度）——
/// 单位换算与钳位统一在 `compile::resolve_jiggle_bones` 里做一次，
/// 避免 QC 侧和 resolve 侧各转一次（那正是原来的双转弧度 bug）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JiggleWrite {
    /// 官方键名（小写）；`is_flexible`/`is_rigid`/`has_base_spring` 表示块进入。
    pub key: String,
    /// 该键的数值：标量 1 个、对偶键 2 个、`allow_length_flex` 与块进入 0 个。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<f32>,
}

impl JiggleBone {
    /// 把三个「便捷块」展开成**按固定顺序**的写入日志（TOML 路径用）。
    ///
    /// 顺序是 `is_flexible` → `is_rigid` → `has_base_spring`，块内按结构体
    /// 字段声明序。这个顺序**表达不了** `jig47`/`jig48` 那种「块间交错」
    /// 的顺序差异 —— 需要精确顺序请直接用 [`JiggleBone::writes`]。
    ///
    /// `writes` 非空时**原样返回**（QC 路径的日志优先，两者互斥由
    /// `validate()` 保证）。
    ///
    /// # 为什么要有这一步
    ///
    /// 官方的语义是「一个扁平记录 + token 顺序后写覆盖」。把 TOML 的块
    /// 也展开成同一个日志，`resolve_jiggle_bones` 就**只有一条代码路径**
    /// —— 不会出现「只改了某一条路径」的 bug（本仓库 R23/R26b/R26c 三次
    /// 都栽在同一个形态上）。
    pub fn effective_writes(&self) -> Vec<JiggleWrite> {
        if !self.writes.is_empty() {
            return self.writes.clone();
        }
        let mut w = Vec::new();
        // 缺省值随字段序展开；`None` 的字段不写（保持 resolve 侧缺省）。
        if let Some(f) = &self.is_flexible {
            let mut push = |key: &str, values: Vec<f32>| {
                w.push(JiggleWrite {
                    key: key.into(),
                    values,
                })
            };
            push("is_flexible", vec![]);
            for (k, v) in [
                ("length", f.length),
                ("tip_mass", f.tip_mass),
                ("yaw_stiffness", f.yaw_stiffness),
                ("yaw_damping", f.yaw_damping),
                ("pitch_stiffness", f.pitch_stiffness),
                ("pitch_damping", f.pitch_damping),
                ("along_stiffness", f.along_stiffness),
                ("along_damping", f.along_damping),
                ("angle_constraint", f.angle_constraint),
            ] {
                if let Some(v) = v {
                    push(k, vec![v]);
                }
            }
            if f.allow_length_flex {
                push("allow_length_flex", vec![]);
            }
            if let Some([a, b]) = f.yaw_constraint {
                push("yaw_constraint", vec![a, b]);
            }
            if let Some(v) = f.yaw_friction {
                push("yaw_friction", vec![v]);
            }
            if let Some(v) = f.yaw_bounce {
                push("yaw_bounce", vec![v]);
            }
            if let Some([a, b]) = f.pitch_constraint {
                push("pitch_constraint", vec![a, b]);
            }
            if let Some(v) = f.pitch_friction {
                push("pitch_friction", vec![v]);
            }
            if let Some(v) = f.pitch_bounce {
                push("pitch_bounce", vec![v]);
            }
        }
        if let Some(r) = &self.is_rigid {
            let mut push = |key: &str, values: Vec<f32>| {
                w.push(JiggleWrite {
                    key: key.into(),
                    values,
                })
            };
            push("is_rigid", vec![]);
            for (k, v) in [
                ("length", r.length),
                ("tip_mass", r.tip_mass),
                ("angle_constraint", r.angle_constraint),
            ] {
                if let Some(v) = v {
                    push(k, vec![v]);
                }
            }
            if let Some([a, b]) = r.yaw_constraint {
                push("yaw_constraint", vec![a, b]);
            }
            if let Some(v) = r.yaw_friction {
                push("yaw_friction", vec![v]);
            }
            if let Some(v) = r.yaw_bounce {
                push("yaw_bounce", vec![v]);
            }
            if let Some([a, b]) = r.pitch_constraint {
                push("pitch_constraint", vec![a, b]);
            }
            if let Some(v) = r.pitch_friction {
                push("pitch_friction", vec![v]);
            }
            if let Some(v) = r.pitch_bounce {
                push("pitch_bounce", vec![v]);
            }
        }
        if let Some(b) = &self.has_base_spring {
            let mut push = |key: &str, values: Vec<f32>| {
                w.push(JiggleWrite {
                    key: key.into(),
                    values,
                })
            };
            push("has_base_spring", vec![]);
            if let Some(v) = b.base_mass {
                push("base_mass", vec![v]);
            }
            if let Some(v) = b.base_stiffness {
                push("stiffness", vec![v]);
            }
            if let Some(v) = b.base_damping {
                push("damping", vec![v]);
            }
            if let Some([a, c]) = b.base_left {
                push("left_constraint", vec![a, c]);
            }
            if let Some(v) = b.base_left_friction {
                push("left_friction", vec![v]);
            }
            if let Some([a, c]) = b.base_up {
                push("up_constraint", vec![a, c]);
            }
            if let Some(v) = b.base_up_friction {
                push("up_friction", vec![v]);
            }
            if let Some([a, c]) = b.base_forward {
                push("forward_constraint", vec![a, c]);
            }
            if let Some(v) = b.base_forward_friction {
                push("forward_friction", vec![v]);
            }
        }
        w
    }
}

/// `is_flexible { … }` 块。
///
/// 角度字段（`angle_constraint`/`yaw_constraint`/`pitch_constraint`）用**度**。
/// `flags`：块存在 ⟹ `0x01`；`0x20 LENGTH` **无条件**置位；
/// `yaw_constraint` ⟹ `0x04`；`pitch_constraint` ⟹ `0x08`；
/// `angle_constraint` ⟹ `0x10`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct JiggleFlexible {
    /// `length`（缺省 **10**）。
    #[serde(default)]
    pub length: Option<f32>,
    /// `tip_mass`（缺省 0）。
    #[serde(default)]
    pub tip_mass: Option<f32>,
    /// `yaw_stiffness`（缺省 **100**）。
    #[serde(default)]
    pub yaw_stiffness: Option<f32>,
    /// `yaw_damping`（缺省 0）。
    #[serde(default)]
    pub yaw_damping: Option<f32>,
    /// `pitch_stiffness`（缺省 **100**）。
    #[serde(default)]
    pub pitch_stiffness: Option<f32>,
    /// `pitch_damping`（缺省 0）。
    #[serde(default)]
    pub pitch_damping: Option<f32>,
    /// `along_stiffness`（缺省 **100**）。
    #[serde(default)]
    pub along_stiffness: Option<f32>,
    /// `along_damping`（缺省 0）。
    #[serde(default)]
    pub along_damping: Option<f32>,
    /// `angle_constraint <度>`（缺省 0）→ `angleLimit`。
    #[serde(default)]
    pub angle_constraint: Option<f32>,
    /// `allow_length_flex` —— **清除 `flags` 的 `0x20 LENGTH` 位**。
    ///
    /// ⚠️ 与调研报告 §6.2 的结论**相反**：报告说 `0x20` 在
    /// `is_flexible`/`is_rigid` 下「无条件置位」，并拿 `jig4`(`0x01`) 当
    /// 「唯一的反例」。实测 `jig4.qc` 里写的正是 `allow_length_flex`
    /// （报告漏看了这个键），而 `jig6` 的 `is_flexible`（**没有**该键）
    /// 是 `0x21` —— 所以规则是：
    /// **块出现 ⟹ 默认置 `0x20`；写了 `allow_length_flex` ⟹ 清掉它。**
    /// 语义上也自洽：允许自由伸缩 = 没有长度约束。
    #[serde(default)]
    pub allow_length_flex: bool,
    /// `yaw_constraint <min> <max>`（**度**）。
    #[serde(default)]
    pub yaw_constraint: Option<[f32; 2]>,
    /// `yaw_friction`（缺省 0）。
    #[serde(default)]
    pub yaw_friction: Option<f32>,
    /// `yaw_bounce`（缺省 0）。
    #[serde(default)]
    pub yaw_bounce: Option<f32>,
    /// `pitch_constraint <min> <max>`（**度**）。
    #[serde(default)]
    pub pitch_constraint: Option<[f32; 2]>,
    /// `pitch_friction`（缺省 0）。
    #[serde(default)]
    pub pitch_friction: Option<f32>,
    /// `pitch_bounce`（缺省 0）。
    #[serde(default)]
    pub pitch_bounce: Option<f32>,
}

/// `is_rigid { … }` 块。`flags |= 0x02`，**`0x20 LENGTH` 同样无条件置位**。
///
/// # ⚠️ 它与 `is_flexible` / `has_base_spring` 的键集**不是**超集关系
///
/// 反编译 `FUN_004549d0`（`is_rigid`）显示它**不调用**共享子解析器
/// `FUN_004545b0`，而是把全部键**内联**在自己体内。因此：
///
/// * 它**接受** `yaw_constraint` / `yaw_friction` / `yaw_bounce` /
///   `pitch_constraint` / `pitch_friction` / `pitch_bounce`
///   （这 6 个原来在 mdlc 里**缺失**，导致官方 QC 里的写法被静默丢弃 ——
///   例如 `v_silenced_smg.qc` 的 `ValveBiped.strap`）。
/// * 它**不接受** `allow_length_flex`（那是 `is_flexible` 独有；
///   夹具 `jig45` 实测官方 abort：`invalid syntax 'allow_length_flex'`）。
///
/// 实测 `jig43`（`is_rigid { yaw_friction 7 / pitch_bounce 8 /
/// yaw_constraint -30 40 / pitch_constraint -20 50 }`）→
/// `flags = 0x2e`（`RIGID|YAW|PITCH|LENGTH`，**无 `0x10`**，因为没写
/// `angle_constraint`），四个角度槽都是**弧度**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct JiggleRigid {
    /// `length`（缺省 **10**）。
    #[serde(default)]
    pub length: Option<f32>,
    /// `tip_mass`（缺省 0）。
    #[serde(default)]
    pub tip_mass: Option<f32>,
    /// `angle_constraint <度>`（缺省 0）→ `angleLimit` + `flags |= 0x10`。
    #[serde(default)]
    pub angle_constraint: Option<f32>,
    /// `yaw_constraint <min> <max>`（**度**）→ `flags |= 0x04`。
    #[serde(default)]
    pub yaw_constraint: Option<[f32; 2]>,
    /// `yaw_friction`（缺省 0）。
    #[serde(default)]
    pub yaw_friction: Option<f32>,
    /// `yaw_bounce`（缺省 0）。
    #[serde(default)]
    pub yaw_bounce: Option<f32>,
    /// `pitch_constraint <min> <max>`（**度**）→ `flags |= 0x08`。
    #[serde(default)]
    pub pitch_constraint: Option<[f32; 2]>,
    /// `pitch_friction`（缺省 0）。
    #[serde(default)]
    pub pitch_friction: Option<f32>,
    /// `pitch_bounce`（缺省 0）。
    #[serde(default)]
    pub pitch_bounce: Option<f32>,
}

/// `has_base_spring { … }` 块（`flags |= 0x40`）。
///
/// 三组 `min/max/friction` 的缺省都是 **±100 / 0**。
///
/// # ⚠️ 字段名是 mdlc 自创的 `base_*` 前缀，与官方 QC 键名不同
///
/// 官方 QC 里写的是**裸键**（见 `D:\SOURCE\SOURCEMDLS\linnea_replaces_zoey\
/// models\survivors\decompiled\jigglebones.qci`）：
///
/// | 官方 QC 键 | mdlc TOML 字段 |
/// |---|---|
/// | `base_mass` | `base_mass`（同名） |
/// | `stiffness` | `base_stiffness` |
/// | `damping` | `base_damping` |
/// | `left_constraint` | `base_left` |
/// | `up_constraint` | `base_up` |
/// | `forward_constraint` | `base_forward` |
/// | `left_friction` | `base_left_friction` |
/// | `up_friction` | `base_up_friction` |
/// | `forward_friction` | `base_forward_friction` |
///
/// **这个 `base_*` 命名必须保留** —— `parity\*.toml`（101 个）与
/// `qc2toml` 的产物都用它，改名会改动全部 parity 快照。
/// 只有 **QC 解析侧**需要认官方裸键（`writes[].key` 存官方键名）。
///
/// # ⚠️ 三个 `*_constraint` **不是角度**，不转弧度
///
/// 实测 `jig38`：QC 写 `left_constraint -0.5 0.5` → 盘上
/// `baseMinLeft = -0.5` `baseMaxLeft = 0.5`（**原值**）。
/// 而 `yaw_constraint`/`pitch_constraint` 是角度（转弧度）——
/// 原 mdlc 把 `base_left`/`base_up`/`base_forward` 也 `to_rad` 了，是 bug。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct JiggleBaseSpring {
    /// `base_mass`（缺省 0）。
    #[serde(default)]
    pub base_mass: Option<f32>,
    /// `base_stiffness`（缺省 **100**）。
    #[serde(default)]
    pub base_stiffness: Option<f32>,
    /// `base_damping`（缺省 0）。
    #[serde(default)]
    pub base_damping: Option<f32>,
    /// `base_left <min> <max>`（缺省 **−100 / 100**）。
    #[serde(default)]
    pub base_left: Option<[f32; 2]>,
    /// `base_left_friction`（缺省 0）。
    #[serde(default)]
    pub base_left_friction: Option<f32>,
    /// `base_up <min> <max>`（缺省 **−100 / 100**）。
    #[serde(default)]
    pub base_up: Option<[f32; 2]>,
    /// `base_up_friction`（缺省 0）。
    #[serde(default)]
    pub base_up_friction: Option<f32>,
    /// `base_forward <min> <max>`（缺省 **−100 / 100**）。
    #[serde(default)]
    pub base_forward: Option<[f32; 2]>,
    /// `base_forward_friction`（缺省 0）。
    #[serde(default)]
    pub base_forward_friction: Option<f32>,
}

/// 一条 quatinterp 骨骼（`mstudioquatinterpbone_t`，**12 字节**）。
///
/// # 语义（QC 的 `.vrd` / `$proceduralbones`）
///
/// `Grab_QuatInterpBones`（`studiomdl.cpp:6297-6430`）解析的是
/// `<helper> <bone> <parent> <controlparent> <control>` 分节，
/// 每节下若干 `<trigger> tol tx ty tz ax ay az px py pz`：
///
/// ```text
/// <helper> "bone" "parent" "ctrlparent" "control"
///     <trigger> tol  tx ty tz  ax ay az  px py pz
///     ...
/// ```
///
/// * `<trigger>` 的 **`tx ty tz` 与 `ax ay az` 都是「度」**，
///   解析时 `DEG2RAD`；`tol` 也是度。
/// * `trigger[k]` = `AngleQuaternion(deg2rad(t))`（**触发**四元数）
/// * `quat[k]`    = `AngleQuaternion(deg2rad(a))`（**目标**四元数）
/// * `pos[k]`     = `<basepos>` + `(px,py,pz)`（`VectorAdd`）
/// * 写盘时 `inv_tolerance = 1.0 / tolerance`（**倒数**，`write.cpp:259`）
///
/// 所以 TOML 里**直接给度**（与 `Eyeball.zoffset`/`Movement.angle` 等
/// 「给文件里的终值」惯例一致 —— 但这里的终值是**弧度**，
/// 由编译期转换，因为官方也是编译期转的）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct QuatInterpBone {
    /// 目标骨骼名（编译期解析成下标）。
    pub bone: String,
    /// **控制**骨骼名 —— 它的局部变换用来查触发器（编译期解析成下标）。
    pub control: String,
    /// `<basepos> x y z`（缺省 `[0,0,0]`）—— 每个触发器的 `pos` 都加它。
    #[serde(default)]
    pub base_pos: Option<[f32; 3]>,
    /// 触发器列表（至少 1 条）。
    #[serde(default)]
    pub triggers: Vec<QuatInterpTrigger>,
}

/// 一条 quatinterp 触发器（`mstudioquatinterpinfo_t`，**48 字节**）。
///
/// ```text
/// +0x00 float      inv_tolerance   // 写盘时 = 1 / tolerance
/// +0x04 Quaternion trigger         // 16 B，AngleQuaternion(deg2rad(trigger_angles))
/// +0x14 Vector     pos             // 12 B，base_pos + pos
/// +0x20 Quaternion quat            // 16 B，AngleQuaternion(deg2rad(target_angles))
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct QuatInterpTrigger {
    /// `<trigger>` 的 `tol`（**度**）—— 影响半径。
    ///
    /// 写盘是 `inv_tolerance = 1.0 / deg2rad(tol)`。
    pub tolerance: f32,
    /// `<trigger>` 的 `tx ty tz`（**度**）→ 触发四元数。
    pub trigger: [f32; 3],
    /// `<trigger>` 的 `ax ay az`（**度**）→ 目标四元数。
    pub angles: [f32; 3],
    /// `<trigger>` 的 `px py pz` —— 与 `base_pos` **相加**后落盘。
    #[serde(default)]
    pub pos: Option<[f32; 3]>,
}
/// 一条 `$ikautoplaylock`（`mstudioiklock_t`，32 字节）。
///
/// # 语义
///
/// QC 写的是**链名**（`studiomdl.cpp:4549-4561` 把 token 存进 `.name`），
/// `LinkIKLocks`（`simplify.cpp:5650-5665`）再把名字解析成**链下标**写进
/// `mstudioiklock_t.chain`。所以 TOML 里也写链名，由写出器解析成下标。
///
/// # 语料形态
///
/// 只有 **11/3333** 个模型有（全是 survivor 的 `anim_*.mdl`），每条 2 个锁，
/// 取值完全一致：`flPosWeight = 1.0`、`flLocalQWeight = 0.1`、
/// `flags = 0`、`unused[4]` 全零（`probe_iklock.js`，22/22 命中）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct IkAutoplayLock {
    /// 被锁定的**链名**（必须与某个 [`IkChain::name`] 相同）。
    pub chain: String,
    /// `flPosWeight`。
    #[serde(default)]
    pub pos_weight: f32,
    /// `flLocalQWeight`。
    #[serde(default)]
    pub local_q_weight: f32,
}

/// 一条 `$ikchain`（`mstudioikchain_t`，16 字节 + 紧跟的 `mstudioiklink_t[3]`）。
///
/// # 语义
///
/// IK 链把「末端骨骼」和它的**两代祖先**组成三段链（`LinkIKChains`，
/// `simplify.cpp:5604-5638`）：`link[2]` = 末端骨骼、`link[1]` = 父（膝/肘）、
/// `link[0]` = 祖父（胯/肩）。所以 QC 里**只需要写末端骨骼名**，
/// 三段链是自动推出来的。
///
/// 语料实测（78 个有 ikchain 的模型 / 278 条链）：`numlinks` **恒为 3**，
/// `linktype` **恒为 0**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct IkChain {
    /// 链名（如 `rhand`、`lfoot`）。序列的 `iklock` 与 `$ikautoplaylock`
    /// 按这个名字引用链。
    pub name: String,
    /// **末端**骨骼名。父/祖父由骨骼表自动推出。
    pub bone: String,
    /// `link[0]`（胯/肩）的理想弯曲方向。
    ///
    /// 对应 QC 的 `knee <x> <y> <z>`（`studiomdl.cpp:4521-4529`）。
    /// 只有 `link[0]` 会被赋值 —— `link[1]`/`link[2]` 的 `kneeDir` 保持 0。
    /// 语料里 278 条链中只有 9 条的 `kneeDir` 非零（其余 18 条被抽样到的是零）。
    #[serde(default)]
    pub knee_dir: Option<[f32; 3]>,
    #[serde(default)]
    pub center: Option<[f32; 3]>,
    #[serde(default)]
    pub height: Option<f32>,
    #[serde(default)]
    pub floor: Option<f32>,
    #[serde(default)]
    pub pad: Option<f32>,
}

/// `$ikrule` 的类型（QC 关键字 → `IK_*` 常量，`studiomdl.cpp:1242-1273`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IkRuleType {
    /// `touch <bone>` → `IK_SELF` = **1**。
    ///
    /// 误差 = `inverse(boneToWorld[bone]) ∘ boneToWorld[链末端骨骼]`，
    /// 逐帧计算。语料里这是**有载荷规则的主力**（10062/30941）。
    Touch,
    /// `footstep` → `IK_GROUND` = **3**.
    Footstep,
    /// `attachment <name>` → `IK_ATTACHMENT` = **5**。
    Attachment,
    /// `release` → `IK_RELEASE` = **4**。自动补的规则全是这个类型。
    Release,
    /// `unlatch` → `IK_UNLATCH` = **6**。
    Unlatch,
}

impl IkRuleType {
    /// 写进 `mstudioikrule_t.type` 的整数。
    pub fn code(self) -> i32 {
        match self {
            Self::Touch => 1,
            Self::Footstep => 3,
            Self::Release => 4,
            Self::Attachment => 5,
            Self::Unlatch => 6,
        }
    }
}

/// 一条 `$ikrule`（`mstudioikrule_t`，**152 字节**）。
///
/// # 结构体布局（实测反解：`rsrch_ik_scan2.js` 在全语料 7669 个带规则的
/// animdesc 上，只有 stride **152** 能让全部 30941 条规则自洽）
///
/// | 偏移 | 字段 | 说明 |
/// |---|---|---|
/// | `0x00` | `index` | 规则序号；**未被任何序列引用的动画写 0**（见 `index` 字段说明） |
/// | `0x04` | `type` | 见 [`IkRuleType`] |
/// | `0x08` | `chain` | IK 链下标 |
/// | `0x0C` | `bone` | 目标骨骼；`type == 4` 恒 0，`Touch` 可为 −1 |
/// | `0x10` | `slot` | 缺省 == `chain`，QC 的 `target` 覆盖 |
/// | `0x14` | `height` | 仅 `footstep`；语料全 0 |
/// | `0x18` | `radius` | 仅 `footstep`/`attachment` |
/// | `0x1C` | `floor` | 仅 `footstep`；语料全 0 |
/// | `0x20` | `pos` | `Vector` |
/// | `0x2C` | `q` | `Quaternion` |
/// | `0x3C` | `compressedikerrorindex` | **相对本条规则自身**；0 = 无载荷 |
/// | `0x44` | `iStart` | 起始**帧号** |
/// | `0x48` | `ikerrorindex` | **恒 0**（未压缩分支被 `#if 0` 屏蔽） |
/// | `0x4C`…`0x60` | `start`/`peak`/`tail`/`end`/`contact` | **cycle**（除以 `numframes−1`） |
/// | `0x64`/`0x68` | `drop`/`top` | 恒 0（源码无赋值点） |
/// | `0x78` | `szattachmentindex` | **相对本条规则自身**，指向**内联**字符串 |
/// | `0x7C` | `unused[7]` | 全 0 |
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IkRule {
    /// IK 链名（必须与某个 [`IkChain::name`] 相同）。
    pub chain: String,
    /// 规则类型。TOML 里写 `type = "touch"` 等。
    #[serde(rename = "type")]
    pub kind: IkRuleType,
    /// `touch <bone>` 的目标骨骼名。
    ///
    /// 省略时官方把 `bone` 置 **−1**（`simplify.cpp:5987-5990`：
    /// `strlen(bonename) == 0` → `bone = -1`），此时误差直接用
    /// `boneToWorld[链末端骨骼]`。
    #[serde(default)]
    pub bone: Option<String>,
    /// `attachment <name>` 的附着点名（**内联**写进规则之后，不走字符串池）。
    #[serde(default)]
    pub attachment: Option<String>,
    /// `target <n>`：覆盖 `slot`；省略 = `slot = chain 的下标`。
    #[serde(default)]
    pub target: Option<i32>,
    /// `height <v>`。`footstep` 时缺省从链继承。
    #[serde(default)]
    pub height: Option<f32>,
    /// `radius <v>`。
    #[serde(default)]
    pub radius: Option<f32>,
    /// `pad <v>`：等价于 `radius <v/2>`（`studiomdl.cpp:1326`）。
    /// 与 [`Self::radius`] 同时写会报错。
    #[serde(default)]
    pub pad: Option<f32>,
    /// `floor <v>`。
    #[serde(default)]
    pub floor: Option<f32>,
    /// `range <start> <peak> <tail> <end>`：四个**帧号**。
    ///
    /// 数组元素写 `None` 表示 QC 里的 `.`（= −1，交给 `ProcessIKRules` 插值）。
    /// 整项省略 = 四个都是 0 → `ProcessIKRules` 把 `tail`/`end` 展开成
    /// `numframes − 1`（`simplify.cpp:5847-5851`）。
    #[serde(default)]
    pub range: Option<[Option<i32>; 4]>,
    /// `contact <帧号>`。省略 = −1 → `contact = peak`。
    #[serde(default)]
    pub contact: Option<i32>,
    /// `fakeorigin <x> <y> <z>`：强制 `pos` 并把 `bone` 置 **−1**。
    #[serde(default)]
    pub fake_origin: Option<[f32; 3]>,
    /// `fakerotate <pitch> <yaw> <roll>`（**度**）：强制 `q` 并把 `bone` 置 **−1**。
    #[serde(default)]
    pub fake_rotate: Option<[f32; 3]>,
    /// `usesource`：误差样本取**源 SMD 的原始骨骼变换**，而不是处理后的动画。
    ///
    /// # 官方依据（`simplify.cpp:6007-6012` 等四处）
    ///
    /// 算 `pError[]` 时有三个互斥来源：
    ///
    /// ```c
    /// if (pRule->usesequence)      CalcSeqTransforms( n, t, boneToWorld );
    /// else if (pRule->usesource) { BuildRawTransforms( panim->source, ... , srcBoneToWorld );
    ///                              TranslateAnimations( panim->source, srcBoneToWorld, boneToWorld ); }
    /// else                         CalcBoneTransforms( panim, t, boneToWorld );
    /// ```
    ///
    /// * **默认**（两者都不写）—— `CalcBoneTransforms`：**处理后的**动画帧
    ///   （含 `subtract` 减除、`RealignBones` 重排等）。
    /// * **`usesource`** —— `BuildRawTransforms` 直接读 `psource->rawanim[frame]`，
    ///   即 SMD 里**原样**的骨骼姿态，**跳过一切后处理**；
    ///   再经 `TranslateAnimations` 用 `srcRealign` 搬到全局骨骼空间。
    /// * **`usesequence`** —— 用**整个序列**的变换（本实现未实现，见下）。
    ///
    /// # 为什么这个区分重要
    ///
    /// 官方 `$sequence` 的 `subtract` 会从每一帧里减掉基准帧，`RealignBones`
    /// 会重排骨骼轴 —— 两者都改变 `CalcBoneTransforms` 的结果。
    /// `usesource` 的意义正是**绕开它们**，拿「作者在 SMD 里摆的姿势」算误差。
    ///
    /// 真实项目里很常见：miku 的 `v_shotgun_spas.qc` 里 **26 处** `usesource`
    /// （`ikrule "lhand" touch "body" usesource` 等）。
    ///
    /// # `usesequence` 为什么不一起做
    ///
    /// 它需要 `CalcSeqTransforms(sequence, frame, …)`（`simplify.cpp:4852`），
    /// 那会遍历**整条序列**的 blend/分层结构；而 `usesource` 只需一帧。
    /// L4D2 语料里 `usesequence` 出现 **0 次**，故只做 `usesource`。
    ///
    /// 两者在 QC 里互斥（`studiomdl.cpp:1338-1347` 各自把对方置 false）。
    #[serde(default)]
    pub use_source: bool,
}

/// 一条 flexdesc（`mstudioflexdesc_t`，**4 字节**）。
///
/// 只有 `szFACSindex` 一个字段（字符串池偏移）。`g_flexdesc[]` 由
/// `Add_Flexdesc()`（`studiomdl.cpp:3529-3552`）**按名字去重**（`stricmp`），
/// 所以同名只出现一次；`[[flex_rules]]` / eyeball / mouth 都按名字引用它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexDescriptor {
    /// FACS 名（如 `upper_right_raiser`、`mouth`）。落盘进字符串池，
    /// **全局按首次注册去重复用**（`AddToStringTable` 语义）。
    pub name: String,
}

/// 一条 flexcontroller（`mstudioflexcontroller_t`，**20 字节**）。
///
/// `localToGlobal` **恒 -1**（`write.cpp:1506` 硬编码，语料 278/278），
/// 不暴露给用户。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexController {
    /// 控制器名 → `sznameindex`。
    pub name: String,
    /// 控制器类型 → `sztypeindex`（如 `eyelid`、`mouth`、`lid`）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 取值下限。QC 缺省 `0.0`（`studiomdl.cpp:3839`）。
    #[serde(default)]
    pub min: f32,
    /// 取值上限。QC 缺省 `1.0`（`studiomdl.cpp:3840`）。
    #[serde(default = "default_flexcontroller_max")]
    pub max: f32,
}

/// `flexcontroller.max` 的缺省值（QC 缺省 1.0）。
pub fn default_flexcontroller_max() -> f32 {
    1.0
}

/// 一个 flexrule 的后缀 op（`mstudioflexop_t`，**8 字节**）。
///
/// 文件里存的就是**后缀（逆波兰）**序列（`studio.h:1222-1229` +
/// `write.cpp:1525-1529`），TOML 直接给后缀，不实现 QC 中缀表达式解析器。
///
/// # `d.index` 的语义由 `op` 决定
///
/// | `op` | `d` 指向 |
/// |---|---|
/// | `const` | `value`（float） |
/// | `fetch1` | **flexcontroller 下标**（按名查） |
/// | `fetch2` | **flexdesc 下标**（按名查） |
/// | 其余（`add`/`sub`/…） | 不用 |
///
/// 用**具名键** `controller` / `flexdesc` 而不是裸 `index` —— 因为
/// `FETCH1` 指 flexcontroller、`FETCH2` 指 flexdesc，裸下标无法表达
/// 「指向哪个数组」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexOp {
    /// 操作码（字符串枚举，见 [`FlexOpKind`]）。
    pub op: FlexOpKind,
    /// `const` 的浮点值。
    #[serde(default)]
    pub value: Option<f32>,
    /// `fetch1` 的 flexcontroller 名。
    #[serde(default)]
    pub controller: Option<String>,
    /// `fetch2` 的 flexdesc 名。
    #[serde(default)]
    pub flexdesc: Option<String>,
}

/// flexop 的操作码 —— `studio.h:3049-3069` 的 `STUDIO_*`。
///
/// 用字符串枚举而不是数字，避免用户记编号。serde 的 snake_case 重命名
/// 把 `TwoWay0` 映射成 `"2way_0"`、`DmeLowerEyelid` 成 `"dme_lower_eyelid"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlexOpKind {
    #[serde(rename = "const")]
    Const,
    #[serde(rename = "fetch1")]
    Fetch1,
    #[serde(rename = "fetch2")]
    Fetch2,
    #[serde(rename = "add")]
    Add,
    #[serde(rename = "sub")]
    Sub,
    #[serde(rename = "mul")]
    Mul,
    #[serde(rename = "div")]
    Div,
    #[serde(rename = "neg")]
    Neg,
    #[serde(rename = "exp")]
    Exp,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "close")]
    Close,
    #[serde(rename = "comma")]
    Comma,
    #[serde(rename = "max")]
    Max,
    #[serde(rename = "min")]
    Min,
    #[serde(rename = "2way_0")]
    TwoWay0,
    #[serde(rename = "2way_1")]
    TwoWay1,
    #[serde(rename = "nway")]
    NWay,
    #[serde(rename = "combo")]
    Combo,
    #[serde(rename = "dominate")]
    Dominate,
    #[serde(rename = "dme_lower_eyelid")]
    DmeLowerEyelid,
    #[serde(rename = "dme_upper_eyelid")]
    DmeUpperEyelid,
}

impl FlexOpKind {
    /// 写进 `mstudioflexop_t.op` 的整数（`studio.h:3049-3069`）。
    pub fn code(self) -> i32 {
        match self {
            Self::Const => 1,
            Self::Fetch1 => 2,
            Self::Fetch2 => 3,
            Self::Add => 4,
            Self::Sub => 5,
            Self::Mul => 6,
            Self::Div => 7,
            Self::Neg => 8,
            Self::Exp => 9,
            Self::Open => 10,
            Self::Close => 11,
            Self::Comma => 12,
            Self::Max => 13,
            Self::Min => 14,
            Self::TwoWay0 => 15,
            Self::TwoWay1 => 16,
            Self::NWay => 17,
            Self::Combo => 18,
            Self::Dominate => 19,
            Self::DmeLowerEyelid => 20,
            Self::DmeUpperEyelid => 21,
        }
    }

    /// 该 op 的 `d` 字段是「下标」还是「float 值」还是「不用」。
    pub fn operand(self) -> FlexOperand {
        match self {
            Self::Const => FlexOperand::Value,
            Self::Fetch1 | Self::Fetch2 => FlexOperand::Index,
            _ => FlexOperand::None,
        }
    }
}

/// [`FlexOpKind::operand`] 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexOperand {
    /// `const`：`d.value` 是 float。
    Value,
    /// `fetch1`/`fetch2`：`d.index` 是数组下标。
    Index,
    /// 其余：`d` 不用（写 0）。
    None,
}

/// 一条 flexrule（`mstudioflexrule_t`，**12 字节** + 内联 `mstudioflexop_t[]`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexRule {
    /// 目标 flexdesc 名（按名查下标；**允许重复**，语料 486 唯一 / 16 重复）。
    pub flex: String,
    /// 后缀 op 序列。
    #[serde(default)]
    pub ops: Vec<FlexOp>,
}

/// 一条 flexcontrollerui（`mstudioflexcontrollerui_t`，**20 字节**）。
///
/// # 自动生成
///
/// **每条 `[[flex_controllers]]` 会自动产生一条对应的 ui**（name = fc 名，
/// `szindex0` 指向该 fc 自身，`stereo = 0`）—— 官方 `flexcontroller` 命令
/// 会顺带生成 ui 记录（实测 fx2.qc 只写 3 个 fc，官方产出 3 条 ui）。
///
/// 本表**仅在需要 stereo 对 / 自定义名时额外给**（DMX 形态，
/// `CDmeGlobalFlexControllerOperator`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlexControllerUi {
    /// UI 名 → `sznameindex`。**必填**（实测不可推导：survivor 系是 FACS 码）。
    pub name: String,
    /// 是否 stereo（左右声道一对）。
    #[serde(default)]
    pub stereo: bool,
    /// 左声道 flexcontroller 名（按名查 fc 下标）。
    ///
    /// **写盘时 `szindex0` 指向「下标更大」那条**（语料 108/108：
    /// `szindex0` 指大下标、`szindex1` 指小 1 的，`szindex0 == szindex1 - 20`）。
    /// 所以 `left`/`right` 的语义是「一对」，写出器按实际下标大小分配。
    #[serde(default)]
    pub left: Option<String>,
    /// 右声道 flexcontroller 名。
    #[serde(default)]
    pub right: Option<String>,
}

/// 一个 eyeball 的 lid 三元组里的一项（lowerer / neutral / raiser）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EyeballLidEntry {
    /// 该档位的 flexdesc 名（如 `upper_right_lowerer`）。
    pub flexdesc: String,
    /// 目标值（`uppertarget`/`lowertarget` 的一档）。
    pub target: f32,
}

/// 一个 eyeball 的上/下眼睑（lid 三元组）。
///
/// 语义是「lowerer / neutral / raiser 三元组」（`studiomdl.cpp:3782-3799`），
/// 用嵌套表而不是 `upperflexdesc[3]` 数组 —— 命名比下标安全。
///
/// `lid_flexdesc` 是「基准」flexdesc（`<type>`，如 `upper_right`），落进
/// `upperlidflexdesc`/`lowerlidflexdesc`；三档的 flexdesc 落进
/// `upperflexdesc[3]`/`lowerflexdesc[3]`。语料 8/8 满足
/// `upperlidflexdesc + 1 == upperflexdesc[0]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EyeballLid {
    /// 基准 flexdesc 名（`<type>`，如 `upper_right`）。
    pub lid_flexdesc: String,
    pub lowerer: EyeballLidEntry,
    pub neutral: EyeballLidEntry,
    pub raiser: EyeballLidEntry,
}

/// 一个 eyeball（`mstudioeyeball_t`，**172 字节**），挂在某个 model 上。
///
/// # `up`/`forward`/`org` 是骨骼空间量
///
/// `up`/`forward` **不暴露** —— 由 `boneToPose` 逆旋转 `(0,0,1)`/`(0,1,0)`
/// 算出（`studiomdl.cpp:3448-3452`）；`org` 用 `boneToPose` 逆变换。
/// TOML 里给的 `org` 是 **SMD/世界空间**，编译期转成骨骼空间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Eyeball {
    /// QC 名，**不落盘**（仅 `eyelid … eyeball <name>` 用来匹配）。
    #[serde(default)]
    pub name: Option<String>,
    /// 绑定的骨骼名。
    pub bone: String,
    /// 世界空间位置（编译期经 `boneToPose` 逆变换成骨骼空间）。
    pub org: [f32; 3],
    /// 眼球材质名，用来定位 mesh → `materialtype=1`/`materialparam=j`。
    pub material: String,
    /// 半径（QC 的 `diameter/2` 由 QC 前端做，这里直接给终值）。
    pub radius: f32,
    /// `zoffset`（QC 的 `tan(deg2rad(zangle))` 由 QC 前端做）。
    #[serde(default)]
    pub zoffset: f32,
    /// `iris_scale`（QC 的 `1/pupil_scale` 由 QC 前端做）。
    #[serde(default = "default_iris_scale")]
    pub iris_scale: f32,
    /// 上眼睑三元组。
    #[serde(default)]
    pub upper_lid: Option<EyeballLid>,
    /// 下眼睑三元组。
    #[serde(default)]
    pub lower_lid: Option<EyeballLid>,
}

/// `eyeball.iris_scale` 的缺省值。
pub fn default_iris_scale() -> f32 {
    1.0
}

/// 一个 mouth（`mstudiomouth_t`，**20 字节**），全局（不在 model 里）。
///
/// 文件里**没有 `sznameindex`** —— QC 的 `bonename` 只用于内部
/// `LinkMouths()` 解析成 `bone`（`simplify.cpp:5407-5421`），不落盘。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mouth {
    /// **显式下标** —— `g_mouth[index]`，决定 `g_nummouths = max(index+1)`
    /// （`studiomdl.cpp:3813-3814`）。
    pub index: i32,
    /// flexdesc 名（按名查下标，如 `mouth`）。
    pub flexdesc: String,
    /// 骨骼名（编译期解析成下标；查不到是硬错误）。
    pub bone: String,
    /// `forward` 向量 —— **直接给，无任何变换**（`studiomdl.cpp:3826-3830`）。
    pub forward: [f32; 3],
}

/// 一个骨骼控制器（`mstudiobonecontroller_t`，56 字节）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoneController {
    /// 目标骨骼名。
    pub bone: String,
    /// STUDIO_X/Y/Z/XR/YR/ZR, optionally STUDIO_RLOOP.
    #[serde(default)]
    pub type_: Option<i32>,
    /// 取值范围。
    #[serde(default)]
    pub start: Option<f32>,
    #[serde(default)]
    pub end: Option<f32>,
    /// 静止值。
    #[serde(default)]
    pub rest: Option<i32>,
    /// 输入字段下标。
    #[serde(default)]
    pub input_field: Option<i32>,
}

impl BoneController {
    pub fn axis_slot(&self) -> Option<usize> {
        let kind = self.type_.unwrap_or(1);
        if kind & !0x803f != 0 {
            return None;
        }
        let slot = match kind & 0x3f {
            1 => 0,
            2 => 1,
            4 => 2,
            8 => 3,
            16 => 4,
            32 => 5,
            _ => return None,
        };
        (slot >= 3 || kind & 0x8000 == 0).then_some(slot)
    }
}

/// 一个序列（`$sequence`）。
///
/// 动画数据来自 SMD 的**多帧 `skeleton` 段** —— 第 0 帧是参考姿态，
/// 后续帧是逐帧姿态。本实现按「逐帧完整数据」写（不做 run 压缩），
/// 见 [`crate::anim_writer`] 的说明。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    #[serde(default)]
    pub loop_restart: i32,
    #[serde(default)]
    pub fudge_loop: bool,
    #[serde(default = "default_motion_rollback")]
    pub motion_rollback: f32,
    #[serde(default)]
    pub motion_type: i32,
    #[serde(default)]
    pub cycle_pose: Option<String>,
    #[serde(default)]
    pub key_values: Option<String>,
    /// Inclusive source frame range, distinct from forced output `num_frames`.
    #[serde(default)]
    pub frames: Option<[i32; 2]>,
    /// Global scale for implicit sources; named references keep their animation's scale.
    #[serde(default)]
    pub input_scale: Option<f32>,
    /// 序列名（QC 里 `$sequence <name> <smd>` 的第一个参数）。
    pub name: String,
    /// 动画 SMD 路径，**相对描述文件所在目录**。
    ///
    /// ⚠️ 对 `$declaresequence` 的空壳（[`Self::forward_declared`]）
    /// **必须为空** —— 官方连 `panim` 都不分配。所以这个字段有
    /// `#[serde(default)]`：空壳不用写它，`validate()` 会反过来
    /// 拒绝「空壳 + 非空 smd」的组合。
    #[serde(default)]
    pub smd: String,
    /// 帧率，默认 30（与 studiomdl 一致）。
    #[serde(default)]
    pub fps: Option<f32>,
    /// `loop`：末帧与首帧相同（写出时末帧会被强制置 0）。
    #[serde(default)]
    pub looping: bool,
    /// QC 的 `delta`（`studiomdl.cpp:2827-2831`）：置 `STUDIO_DELTA | STUDIO_POST`。
    ///
    /// # 两个位一起置，不能只置一个
    ///
    /// ```c
    /// else if (stricmp("delta", token) == 0)
    /// {
    ///     pseq->flags |= STUDIO_DELTA;
    ///     pseq->flags |= STUDIO_POST;
    /// }
    /// ```
    ///
    /// 实测 `look_poses` 的 `seqdesc.flags == 0x14` = `0x10 | 0x04`
    /// —— 正是 `STUDIO_POST | STUDIO_DELTA`。
    ///
    /// 引擎侧这两个位一起决定「本序列存的是相对参考姿态的增量，
    /// 播放时要乘回 base」（`simplify.cpp:57-67` 的注释）。
    #[serde(default)]
    pub delta: bool,
    /// QC 的 `activity` 名。L4D2 的 QC 里常见 `ACT_*`；不写则写 -1。
    #[serde(default)]
    pub activity: Option<String>,
    /// `mstudioseqdesc_t.actweight`（`+0x14`）—— QC 的
    /// `activity <名> <权重>` 的**第二个**参数。
    ///
    /// 源码：`Option_Activity`（`studiomdl.cpp:1166-1178`）读**两个** token，
    /// 第二个写进 `psequence->actweight`；`ParseSequence` 初始化时是 **0**
    /// （`studiomdl.cpp:2629`）。
    ///
    /// # 为什么不能忽略
    ///
    /// 真实项目（`v_shotgun_spas.qc`）里 **24/24** 条序列都写了
    /// `activity ACT_XXX 1`，忽略它会让 `actweight` 全部变成 0 ——
    /// 实测对照官方产物 24/24 不符。
    ///
    /// `activity` 没写时官方是 0；写了 `activity` 但没给权重时
    /// `verify_atoi` 读到的是**下一个 token**（或空），行为不确定 ——
    /// 所以 TOML 用独立字段，不写就是 0。
    #[serde(default)]
    pub activity_weight: i32,
    /// 动画事件（QC 的 `$sequence ... { event <帧> <名> <参数> }`）。
    ///
    /// 实测 **54.1%** 的 L4D2 模型有事件 —— 是最高频的动画子特性。
    /// 游戏逻辑（脚步声、枪声、粒子）大量依赖它。
    #[serde(default)]
    pub events: Vec<SequenceEvent>,
    /// `mstudioseqdesc_t.fadeintime`（+0x68）。QC 的 `$sequence ... fadein <v>`。
    ///
    /// **缺省 0.2**（`studiomdl.cpp:2639-2640` 在建 sequence 时就写死，
    /// `fadein` 只是覆盖它）。
    ///
    /// 语料抽样 413 个模型 / 1545 条序列：**1335 条是 `0.2 / 0.2`**，
    /// 其余是显式覆盖（`0.5`、`0.75`、`0.05`…）。
    ///
    /// > 早先 mdlc 写 0，代码注释还写着「实测为 0（不是 0.2）」——
    /// > 那是**错的**，对照官方产物与语料都被推翻。
    #[serde(default = "default_fade_time")]
    pub fade_in: f32,
    /// `mstudioseqdesc_t.fadeouttime`（+0x6C）。QC 的
    /// `$sequence ... fadeout <v>`。缺省同 [`Self::fade_in`]。
    #[serde(default = "default_fade_time")]
    pub fade_out: f32,
    /// QC 的 `$declaresequence`：一条**前向声明的空壳序列**。
    ///
    /// # 官方语义（`Cmd_DeclareSequence`，`studiomdl.cpp:3204-3218`）
    ///
    /// ```c
    /// s_sequence_t *pseq = &g_sequence[ g_sequence.AddToTail() ];
    /// memset( pseq, 0, sizeof( s_sequence_t ) );   // ← **全零**
    /// pseq->flags = STUDIO_OVERRIDE;               // ← 0x0800
    /// GetToken( false );
    /// strcpyn( pseq->name, token );
    /// ```
    ///
    /// 只有三件事：占一个序列槽、`memset` 清零、置 `STUDIO_OVERRIDE`。
    /// **没有** `panim`、**没有**动画、`groupsize = [0, 0]`。
    ///
    /// # 它为什么存在（survivor 模组的核心机制）
    ///
    /// 引擎加载时在 `studio_virtualmodel.cpp:185` 做**跨模型序列替换**：
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
    /// 即：主模型里声明一堆**空壳**，真正的动画在 `$includemodel`
    /// 进来的 `anim_<survivor>.mdl` 里，引擎按名字**替换**掉空壳。
    /// 这样主模型的网格/骨骼/flex 与动画可以分开发布。
    ///
    /// # 实测（真实 survivor 产物 + 受控实验）
    ///
    /// `docs/_probe/probe_declaresequence.js`（受控实验，真 studiomdl）
    /// 与 `dump_override_seqdesc.js`（真实 937 条序列的 survivor 模型）：
    ///
    /// | 字段 | 空壳的值 | 普通序列 |
    /// |---|---|---|
    /// | `flags` | **0x0800** | 0x0000 起 |
    /// | `numblends` | **0** | 1 |
    /// | `groupsize` | **[0, 0]** | [1, 1] |
    /// | `activity` | **0** | -1 |
    /// | `paramindex` | **[0, 0]** | [-1, -1] |
    /// | `fadeintime`/`fadeouttime` | **0** | 0.2 |
    /// | `bbmin`/`bbmax` | **`[9999,9999,9999]` / `[-9999,…]`** | 真实包围盒 |
    /// | `weightlist` | **全 0** | 全 1 |
    ///
    /// 那 7 个「不同」全部来自同一个原因：**`memset` 之后没人再动它**。
    /// 普通序列的 `-1` / `0.2` / 真实包围盒分别来自
    /// `Cmd_Sequence` 的初始化、`ParseSequence` 的 `0.2`、
    /// `CalcSequenceBoundingBoxes` —— 而空壳**一个都没跑**。
    ///
    /// ⚠️ `weightlist` 是**全 0 而不是全 1**：`simplify.cpp:302-318`
    /// 先置 `weight[n] = 0`，再用 `groupsize` 的双层循环取 MAX；
    /// 空壳的 `groupsize = [0,0]` ⟹ **循环一次都不跑** ⟹ 保持 0。
    ///
    /// # 语法陷阱（实测）
    ///
    /// 声明之后**不能**用 `$sequence <同名>` 去填 —— 官方会报
    /// `no animations found`（`ParseSequence` 发现 `numblends == 0`）。
    /// 空壳就是空壳，填充是**引擎运行时**用 `$includemodel` 的模型做的。
    #[serde(default)]
    pub forward_declared: bool,
    /// QC 的 `noautoik`（`studiomdl.cpp:2265`，对应 `panim->noAutoIK`）。
    ///
    /// 置位时**不**自动补 `IK_RELEASE` 规则（`simplify.cpp:6251`）。
    ///
    /// # 为什么必须能表达
    ///
    /// 「有 `$ikchain` 的模型里，每个动画都自动多一条 `type = 4` 的规则」
    /// 这个直觉是**错的**。语料实测（`probe_ikrule_auto.js`，78 个有
    /// `$ikchain` 的模型 / 15942 个 animdesc）：
    ///
    /// ```text
    /// numikrules == numikchains : 5831
    /// numikrules <  numikchains : 10111
    /// numikrules >  numikchains : 0
    /// ```
    ///
    /// 少于链数的那 10111 个 animdesc 分三类
    /// （`probe_ikrule_flags.js "\infected\anim_boomer.mdl"` 逐条对齐）：
    ///
    /// 1. `animdesc.flags & STUDIO_DELTA`（**0x04**）—— `@Melee_01_Delta` 等
    ///    全部 0 条规则。mdlc 目前没有 delta 动画，所以这条在 TOML 里
    ///    不可表达（也不需要）。
    /// 2. **`noautoik`** —— 如 `@Melee_01`、`@Run_Shoot_KNIFE`
    ///    （`flags = 0x40`，非 DELTA）都是 0 条。**就是本字段。**
    /// 3. `$weightlist` 把某些骨骼的权重置 0 —— 如 `@Melee_01_Layer` 是
    ///    **2/4** 条（4 条链只有 2 条被自动补）。mdlc 不实现 `$weightlist`，
    ///    所以这一条也不可表达。
    ///
    /// 也就是说：**TOML 的默认值（false）等价于「无 `$weightlist`、
    /// 非 delta」的官方行为**，`true` 覆盖第 2 类。
    #[serde(default)]
    pub no_auto_ik: bool,
    /// `$ikrule` 列表（QC 里 `$sequence` 块内的 `ikrule` 命令）。
    ///
    /// 数组顺序 = 写出顺序：**显式规则在前，自动补的 `IK_RELEASE` 在后**
    /// （`ProcessIKRules` 先拷贝 `cmds[]`，`simplify.cpp:6262-6281` 再追加）。
    #[serde(default)]
    pub ik_rules: Vec<IkRule>,
    /// 序列级命令（QC `$sequence` 块内的 `cmdlist "<名>"` 展开）。
    ///
    /// # 为什么在序列上而不是在动画上
    ///
    /// 官方**没有**「序列级命令」这个概念：`s_sequence_t` 里根本没有
    /// `cmds` 字段（`studiomdl.h:610-668`）。序列块里的 `cmdlist` 走的是
    /// `ParseSequence` 的动画级分派（`studiomdl.cpp:2944`），
    /// 落在 `pseq->panim[0][0]` —— **第一格动画**上。
    ///
    /// mdlc 在解析期还不知道第一格是哪个动画（`blends` 只是名字，
    /// 编译期才解析），所以和 [`Self::ik_rules`] 一样先记在序列上，
    /// 编译期再挂到第一格（`compile.rs` 的 `first_cell`）。
    #[serde(default)]
    pub cmds: Vec<AnimCmd>,
    /// 缩放（QC `scale <f>`）。见 [`Animation::scale`]。
    #[serde(default)]
    pub scale: Option<f32>,
    /// 位移（QC `origin <x> <y> <z>`）。见 [`Animation::adjust`]。
    #[serde(default)]
    pub adjust: Option<[f32; 3]>,
    /// 旋转（QC `rotate` / `angles`，**弧度**）。见 [`Animation::rotation`]。
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// **序列级** IK 锁（QC `$sequence` 块内的 `iklock <链名> <posW> <localQW>`）。
    ///
    /// # 与 `[[ik_autoplay_locks]]` 的区别（**两个不同的东西**）
    ///
    /// | | 模型级 `$ikautoplaylock` | **序列级 `iklock`**（本字段） |
    /// |---|---|---|
    /// | QC 位置 | 顶层命令 | **`$sequence { … }` 块内** |
    /// | 落盘位置 | 头部数组（`+0x144`） | **seqdesc 子表区**（`+0xA8`） |
    /// | 语料频率 | **11/3333 模型**（0.33%） | **16/3333 模型**，但 **1594/11170 序列（14.27%）** |
    ///
    /// 源码：`studiomdl.cpp:2873-2885`（解析）、`write.cpp:596-611`（写出）。
    ///
    /// # ⚠️ 这是**结构性**的，不只是「一个字段」
    ///
    /// `mstudioiklock_t` 记录**占子表区空间**（32 字节/条，之后 `ALIGN4`）。
    /// 不写它们会让其后的 **blend 数组 / keyvalue 块偏移整体前移** ——
    /// 所以它影响的是子表布局，而非单个字段。
    ///
    /// # 基准（已由语料反解 + 源码双证）
    ///
    /// `iklockindex` **相对本 seqdesc 记录自身** ——
    /// `write.cpp:429` 的 `pSequenceStart = (byte *)pseqdesc` 在循环**内**赋值。
    /// 实测两种假设对比：相对自身 **3222/3222 合法**，
    /// 相对数组起点只有 1787 合法 + 384 个 NaN。
    ///
    /// # 语料取值
    ///
    /// `chain` 恒为合法链下标（越界 0 次）；`flPosWeight = 1`（3201/3222）；
    /// `flLocalQWeight = 0`（3205/3222）。
    #[serde(default)]
    pub iklocks: Vec<IkAutoplayLock>,
    /// **blend 网格**：每一格的**动画名**，**行主序**。
    ///
    /// # 什么时候用
    ///
    /// QC 的 `$sequence` 块里写**多个动画名**时就是 blend：
    ///
    /// ```text
    /// $sequence "idle" {
    ///     "a_run"            <- 3 个动画 = 3 格
    ///     "a_idle"
    ///     "a_run"
    ///     blend "move_x" -1 1
    ///     blendwidth 3
    /// }
    /// ```
    ///
    /// 此时[`Self::smd`]被忽略（但仍要求非空，用第一格即可）。
    ///
    /// # 名字指向 `[[animations]]`
    ///
    /// 这些名字是 **`$animation` 名**，必须在 [`ModelDesc::animations`]
    /// 里声明过 —— 官方在 `$sequence` 块里遇到一个名字时**先查已有的
    /// `$animation`**（`studiomdl.cpp:2952-2959`），查到就复用**同一个
    /// animdesc**；查不到才建一个隐含动画（名字加 `@` 前缀）。
    ///
    /// 所以「同一格被多条序列引用」在文件里只有**一个** animdesc ——
    /// 这正是 `v_autoshotgun.mdl` 里 27 个 seqdesc 只有 29 个 animdesc
    /// （而不是 33 个）的原因：`idle` 与 `idle_raw` 共用 `a_idle`，
    /// `idle` 的两个 `a_run` 格共用同一个 `a_run`。
    ///
    /// # 顺序
    ///
    /// **行主序**：`blends[k * blend_width + j]` 对应网格的
    /// 第 `k` 行第 `j` 列 —— 与 QC 里动画名的书写顺序一致
    /// （`simplify.cpp:3026-3038` 的 `j = i % groupsize[0]`、`k = i / groupsize[0]`）。
    ///
    /// # 与 `blend_width` 的关系
    ///
    /// 格子总数 = `blends.len()`；`groupsize[0] = blend_width`、
    /// `groupsize[1] = len / blend_width`（`simplify.cpp:3015-3023`）。
    /// `blend_width` 省略时按 `simplify.cpp:2994-3014` 推断：
    /// 少于 4 格 ⟹ `groupsize[0] = 格数`、`groupsize[1] = 1`；
    /// 否则要求完全平方数，开方成方阵。
    #[serde(default)]
    pub blends: Vec<String>,
    /// blend 网格的**宽度**（`groupsize[0]`）。QC 的 `blendwidth <n>`。
    ///
    /// 省略时按格数推断，见 [`Self::blends`]。
    #[serde(default)]
    pub blend_width: Option<i32>,
    /// blend 的**参数轴**（QC 的 `blend <参数名> <start> <end>`）。
    ///
    /// 最多 2 个：第 0 个对应 X 轴（`groupsize[0]`），
    /// 第 1 个对应 Y 轴（`groupsize[1]`）。
    /// **轴的顺序就是数组顺序**，与 QC 里 `blend` 命令的先后一致。
    ///
    /// # ⚠️ `blend` 与 `calcblend` 共用这一个列表
    ///
    /// 官方两个关键字用的是**同一个槽位分配规则**
    /// （`studiomdl.cpp:2734-2737` 与 `:2756-2759`，逐字相同）：
    ///
    /// ```c
    /// i = 0;
    /// if (pseq->paramindex[0] != -1) { i = 1; }   // 槽 0 被占 ⟹ 用槽 1
    /// ```
    ///
    /// 即**按 QC 出现顺序**占槽，与是哪个关键字**无关**。
    /// 所以 `blend "pa" …` 后面跟 `calcblend "pb" …` 会占槽 0 / 槽 1，
    /// 反过来写也一样 —— 用一个有序列表就能精确表达。
    /// 每项是不是 calc 由 [`BlendParam::attachment`] 区分。
    #[serde(default)]
    pub blend_params: Vec<BlendParam>,
    /// `blendref`（QC 的 `blendref <动画名>`）—— `pseq->paramanim`。
    ///
    /// # 角色：`calcblend` 的**「零点」参考动画**
    ///
    /// 官方用它算附着点的「零位姿」（`simplify.cpp:5488`）：
    ///
    /// ```c
    /// CalcBoneTransforms( pseq->paramanim, 0, boneToWorld );
    /// ConcatTransforms( boneToWorld[k0], g_attachment[n0].local, boneToWorldMid );
    /// MatrixInvert( boneToWorldMid, worldToBoneMid );
    /// ```
    ///
    /// 为空时**回落到 `g_panimation[0]`**（`simplify.cpp:5476-5479`），
    /// 即 QC 里**第一条** `$animation`。
    #[serde(default)]
    pub blend_ref: Option<String>,
    /// `blendcomp`（QC 的 `blendcomp <动画名>`）—— `pseq->paramcompanim`。
    ///
    /// # 角色：逐格算值时给 `CalcBoneTransforms` 的**基准动画**
    ///
    /// 官方（`simplify.cpp:5526`）：
    ///
    /// ```c
    /// CalcBoneTransforms( pseq->panim[m[0]][m[1]], pseq->paramcompanim, 0, boneToWorld );
    /// ```
    ///
    /// 这是 3 参数版，`pbaseanimation` 只在动画带 `STUDIO_DELTA` 时才被读
    /// （`simplify.cpp:4568` 的 `pbaseanimation->sanim[0][k]`）——
    /// 非 delta 动画下它与结果无关。
    ///
    /// 为空时回落到 `paramanim`（`simplify.cpp:5481-5484`）。
    #[serde(default)]
    pub blend_comp: Option<String>,
    /// `blendcenter`（QC 的 `blendcenter <动画名>`）—— `pseq->paramcenter`。
    ///
    /// # 角色：决定「另一根轴」取网格的哪一格
    ///
    /// 官方先在网格里找它的位置 `(i0, i1)`，找不到就取中点
    /// （`simplify.cpp:5503-5521`）：
    ///
    /// ```c
    /// if (pseq->paramcenter != NULL) { 找到 (i0,i1) 使 panim[i0][i1] == paramcenter ⟹ m[0]=i0, m[1]=i1; }
    /// if (!found) { m[1-iPose] = pseq->groupsize[1-iPose] / 2; }
    /// ```
    ///
    /// ⚠️ **命中时 `m[0]` 与 `m[1]` 被同时写死**，于是两层循环里
    /// 「另一根轴」也跟着 `blendcenter` 走 —— 这是官方的原样行为
    /// （`m[iPose]` 在循环里被覆盖，`m[1-iPose]` 保持 `blendcenter` 给的值）。
    #[serde(default)]
    pub blend_center: Option<String>,
    #[serde(default)]
    pub transition: Option<[String; 2]>,
    #[serde(default)]
    pub transition_reversible: bool,
    /// **自动层**（QC 的 `addlayer <序列名>`）。
    ///
    /// 自动层让引擎在播本序列时**叠加**另一条序列。
    /// 实测本项目 `idle` 有 1 层（叠加 `look_poses`）。
    #[serde(default)]
    pub auto_layers: Vec<AutoLayer>,
    /// 逐段移动键（`mstudiomovement_t`，**44 字节/条**）。
    ///
    /// # 语义
    ///
    /// QC 侧由脚步/位移数据生成（`$sequence` 的 movement 相关处理），
    /// 落盘在**动画数据区末尾**、按 animdesc 顺序排布，每条数组后 `ALIGN4`
    /// （`write.cpp:1150-1174`）。
    /// `animdesc.nummovements` @ **+0x14**、`movementindex` @ **+0x18**
    /// （**相对该 animdesc 记录自身**）。
    ///
    /// # 与 `seqdesc.movementindex` 的区别
    ///
    /// `mstudioseqdesc_t.movementindex`（+0x40）在语料里**恒 0**（3333/3333），
    /// 真正承载 movement 的是 **animdesc** 的两个字段
    /// （语料 17 个模型 / 4353 个 animdesc 非 0）。
    #[serde(default)]
    pub movements: Vec<Movement>,
    /// **每段帧数**（QC 的 `$sectionframes <每段帧数> <阈值>` 的**第一个**参数）。
    ///
    /// # 触发规则（实测完美分离）
    ///
    /// ```text
    /// numframes >= 阈值(默认 120)  ⟹  sectionframes = 本值(默认 30)
    /// ```
    ///
    /// 默认值是 studiomdl 的 `.data` **静态初值**（`rsrch_inc_pe5.js`：
    /// file `0x6e68f8` = `0x78` = 120、`0x6e68f4` = `0x1e` = 30）。
    /// 语料里 `sectionframes` **只有 30 一个值**（1702/1702）。
    ///
    /// **省略 = 用官方默认（30 / 阈值 120）**，不是「不分段」。
    #[serde(default)]
    pub section_frames: Option<i32>,
    /// 分段**阈值**（帧数，QC 的**第二个**参数）。省略 = **120**。
    ///
    /// > QC 只给一个数字官方**直接报错不产生产物**（受控实验 `$sectionframes 15`
    /// > 编译失败）—— 所以 TOML 用两个独立字段而不是「一个可选值」。
    #[serde(default)]
    pub section_threshold: Option<i32>,
    /// **额外的 `seqdesc.flags` 位**（QC 里没有具名键的那些）。
    ///
    /// # 为什么需要这个字段
    ///
    /// QC 的 `$sequence` 块里有一批**纯标志位**关键字，它们只做
    /// `pseq->flags |= XXX`（`ParseSequence`，`studiomdl.cpp:2720-2866`）：
    ///
    /// | QC 关键字 | `studio.h` 常量 | 值 |
    /// |---|---|---|
    /// | `snap` | `STUDIO_SNAP` | `0x0002` |
    /// | `autoplay` | `STUDIO_AUTOPLAY` | `0x0008` |
    /// | `post` | `STUDIO_POST` | `0x0010` |
    /// | `realtime` | `STUDIO_REALTIME` | `0x0080` |
    /// | `hidden` | `STUDIO_HIDDEN` | `0x0400` |
    /// | `worldspace` | `STUDIO_WORLD` | `0x2000` |
    ///
    /// `looping`（`STUDIO_LOOPING`）与 `delta`（`STUDIO_DELTA|STUDIO_POST`）
    /// 已有具名字段，**不写在这里**（避免双重表达）。
    ///
    /// # 语料频率（实测，`probe_seq_flags_corpus.js`）
    ///
    /// `snap` 与 `hidden` 在真实项目里常见（`snap` 55 处 / `hidden` 45 处，
    /// 见 `PROGRESS.md` §46 的命令普查），所以不是「0 样本」特性。
    ///
    /// # 与 `animdesc.flags` 的关系
    ///
    /// 这些位**只**影响 `seqdesc`；`animdesc` 的对应位由
    /// `$animation` 块决定（见 [`Animation`]）。官方在
    /// `ParseSequence` 末尾把每格 `animdesc.flags` **OR 进** `seqdesc.flags`，
    /// 所以两边可能叠加 —— 见 `anim_writer.rs` 的回填处。
    #[serde(default)]
    pub extra_flags: Option<i32>,
    /// 本序列用的**权重表**名（QC 的 `$sequence ... weightlist "<名>"`）。
    ///
    /// 省略 = 用隐式默认表（全 1）。
    ///
    /// # 作用（`setAnimationWeight`，`simplify.cpp:1721-1729`）
    ///
    /// 把该表拷进本序列各动画的 `panim->weight[]`，供
    /// `CalcBoneTransforms` 的 DELTA 重建缩放用：
    /// `q3 = slerp(q_base, q_delta, s)`、`p3 = base + s*delta`。
    ///
    /// 所以它**只对 `delta` 动画有可观测效果** —— 非 delta 动画走
    /// `AngleMatrix(sanim.rot, sanim.pos)`，根本不读 `weight[]`。
    /// 实测：`simplify.cpp:1124-1150` 的「用权重清零无变化骨骼」
    /// 那段被 `#if 0` 包住了。
    #[serde(default)]
    pub weight_list: Option<String>,
    /// `$sequence` 块里的 `subtract "<参考动画>" [帧]`
    /// （官方 `ParseCmdlistToken` 的 `CMD_SUBTRACT`，`studiomdl.cpp:1733-1751`）。
    ///
    /// # 为什么 `$sequence` 也有这个选项
    ///
    /// 官方 `ParseSequence` 在 `numblends || isAppend` 时把整个 token
    /// 交给 `ParseAnimationToken`（`studiomdl.cpp:2944`），而后者会走到
    /// `ParseCmdlistToken` —— 所以 `subtract` / `numframes` / `weightlist`
    /// 这些「动画选项」在 `$sequence` 里**同样合法**。真实工程大量使用：
    /// `$sequence "deploy_layer" "al_deploy" snap fadeout 0.2
    /// subtract "a_idle" 0 delta ...`。
    ///
    /// ⚠️ 修复前 `subtract` 被当成**动画名**压进 [`Self::blends`]，
    /// 于是整条序列被误判成 blend 网格并报
    /// 「blend 格数 5 不是完全平方数」。见 `PROGRESS.md` §53。
    #[serde(default)]
    pub subtract: Option<String>,
    /// `subtract` 取参考动画的第几帧。缺省 0。
    #[serde(default)]
    pub subtract_frame: Option<i32>,
    /// `$sequence` 块里的 `numframes <N>`
    /// （官方 `ParseCmdlistToken` 的 `CMD_NUMFRAMES`，`studiomdl.cpp:2104-2111`）。
    ///
    /// 官方语义是**强制帧数**（`simplify.cpp` 把动画重采样到 N 帧）。
    /// 与 [`Self::section_frames`] 无关 —— 后者是「每段多少帧」。
    #[serde(default)]
    pub num_frames: Option<i32>,
    /// `srcstack "walk"`：**按名字选动画栈**（仅对 FBX 源有意义）。
    ///
    /// `None` = 取第一条 —— 那是官方的行为，且 `$sequence` 的名字
    /// **完全不参与**栈选择（`docs/fbx-support.md` §1.8：3 个样本 × 3 个
    /// 序列名，全部取第一条栈）。
    ///
    /// ⚠️ 名字对不上时报错并列出全部可用栈名（不静默回落）——
    /// 静默回落正是官方最让人困惑的地方。
    #[serde(default)]
    pub src_stack: Option<String>,
    /// `srcfps 30`：FBX 动画的**重采样率**。`None` = `30`（官方固定值）。
    ///
    /// ⚠️ 与 [`Self::fps`] 不是一回事：那个是**动画的播放帧率**
    /// （落进 `animdesc.fps`，**不改帧数**），这个是**从源文件采几帧**
    /// （`ufbx::bake_anim` 的 `resample_rate`）。
    /// 官方对 FBX 恒定 30 fps 采样，无论源文件写的是 24 / 30 / 60
    /// （`docs/fbx-support.md` §1.5）。
    #[serde(default)]
    pub src_fps: Option<f32>,
}

/// blend 网格的**一格** —— 指向 [`ModelDesc::animations`] 里的一个动画。
///
/// 保留这个类型只是为了 TOML 里能写内联表；[`Sequence::blends`] 现在
/// 收的是**名字字符串**，内联表形式由 `animations` 承载。
pub type BlendCellName = String;

/// 一条 blend 参数轴（QC 的 `blend <参数名> <start> <end>`）。
///
/// # 落盘
///
/// 写进 `mstudioseqdesc_t`：
///
/// * `paramindex[i]` = 参数在 `[[model.pose_parameters]]` 里的**下标**
/// * `paramstart[i]` / `paramend[i]` = 这里的 `start` / `end`
/// * `param0[]` / `param1[]` = **线性插值**出来的逐格取值，写进
///   `posekeyindex` 指向的数组
///
/// # `param0` 的公式（`simplify.cpp:5579-5590`）
///
/// ```text
/// for m in 0..groupsize[i]:
///     f = m / (groupsize[i] - 1)
///     param_i[m] = start * (1 - f) + end * f
/// ```
///
/// 实测 `look_poses`（`start=-1, end=1, groupsize[0]=3`）⟹ `[-1, 0, 1]` ✓
///
/// # 两种来源（QC 的 `blend` 与 `calcblend`）
///
/// [`Self::attachment`] 为 `None` 时是纯 `blend`：`start`/`end` 来自 QC，
/// `param0[]` 走上面的线性插值。
///
/// 为 `Some` 时是 `calcblend`：`start`/`end` 与 `param0[]` **全部是算出来的**
/// （官方 `CalcPoseParameters`，`simplify.cpp:5448-5596`），此时 QC 里
/// 那两个数字**根本不存在**，字段值在编译期被覆盖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BlendParam {
    /// 参数名（必须与某个 `[[model.pose_parameters]].name` 相同）。
    ///
    /// 也接受**直接写下标**（`"0"` / `"1"`）—— 与 [`IkRule::chain`]
    /// 的「名字或下标」惯例一致。
    pub parameter: String,
    /// 该轴的起始值（QC `blend` 的第二个数字）。
    ///
    /// `calcblend` 轴忽略它（值由编译期算出）。
    #[serde(default)]
    pub start: f32,
    /// 该轴的结束值（QC `blend` 的第三个数字）。
    ///
    /// `calcblend` 轴忽略它（值由编译期算出）。
    #[serde(default)]
    pub end: f32,
    /// `calcblend` 的附着点名（`Some` ⟹ 本轴是 `calcblend`）。
    ///
    /// 官方 `LookupAttachment` 查不到时报
    /// `Unknown calcblend attachment "<名>"`（`studiomdl.cpp:2766-2770`）。
    #[serde(default)]
    pub attachment: Option<String>,
    /// `calcblend` 的控制轴名（`X` / `Y` / `Z` / `XR` / `YR` / `ZR`）。
    ///
    /// 大小写不敏感（官方 `lookupControl` 用 `stricmp`）。
    ///
    /// ⚠️ **缺省是「无控制轴」，不是 `X`。** 官方
    /// `Cmd_Sequence`（`studiomdl.cpp:2624`）先 `memset(pseq, 0, …)`，
    /// 而 `paramcontrol[i]` **只在 `calcblend` 分支被赋值**
    /// （`:2773`）。所以纯 `blend` 的轴 `paramcontrol == 0`，
    /// 而 `CalcPoseParameterValue` 对 0 **不匹配任何 case** ⟹ 返回 `0.0`
    /// （`simplify.cpp:5445`）。这正是「只写 `blendwidth` 不写 `blend`」
    /// 的序列会报 `calcblend failed` 的根因 —— 见
    /// [`CompiledSequence::calc_axes`] 的说明。
    ///
    /// 用 `None` 表达「0」；无法识别的名字也回落到 `None`
    /// （官方 `lookupControl` 返回 `-1`，而 `-1` 同样不匹配任何 case ⟹ 也是 0）。
    #[serde(default)]
    pub control: Option<String>,
}

/// 一条自动层（QC 的 `addlayer <序列名>`）。
///
/// # 落盘：`mstudioautolayer_t`（**24 字节**）
///
/// ```text
/// +0x00 short iSequence   被叠加的序列下标
/// +0x02 short iPose       用哪个 pose 参数驱动（QC 的 blendlayer）
/// +0x04 int   flags       STUDIO_AL_* 位
/// +0x08 float start       影响开始
/// +0x0C float peak        全权重开始
/// +0x10 float tail        全权重结束
/// +0x14 float end         影响结束
/// ```
///
/// # 四个时间量的单位（`write.cpp:539-551`）
///
/// **不带 `STUDIO_AL_POSE` 时除以 `numframes − 1` 转成 cycle**；
/// 带 `STUDIO_AL_POSE` 时原样写。
///
/// 实测本项目 `idle` 的那一层四个量全是 **0**（QC 只写了 `addlayer look_poses`，
/// 没给时间），所以这里默认 0。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AutoLayer {
    /// 被叠加的序列名（必须与某个 `[[sequences]].name` 相同）。
    pub sequence: String,
    /// `iPose`：用哪个 pose 参数驱动。默认 0。
    #[serde(default)]
    pub pose: i16,
    /// `STUDIO_AL_*` 位标志。`STUDIO_AL_POSE` = **0x4000**。
    ///
    /// 只有置了 `0x4000` 时四个时间量才**不**转 cycle（`write.cpp:539`）。
    #[serde(default)]
    pub flags: i32,
    /// 影响开始（帧号；带 `STUDIO_AL_POSE` 时是原值）。
    #[serde(default)]
    pub start: f32,
    /// 全权重开始。
    #[serde(default)]
    pub peak: f32,
    /// 全权重结束。
    #[serde(default)]
    pub tail: f32,
    /// 影响结束。
    #[serde(default)]
    pub end: f32,
}

/// 一条 movement 键（`mstudiomovement_t`，**44 字节**）。
///
/// ```text
/// +0x00 int    endframe      本段结束帧
/// +0x04 int    motionflags   STUDIO_MOVEMENT_* 位标志组合
/// +0x08 float  v0            起始速度
/// +0x0C float  v1            结束速度
/// +0x10 float  angle         本段结束时的 YAW 旋转（**度**）
/// +0x14 Vector vector        相对本段初始角度的移动向量
/// +0x20 Vector position      相对动画起点
/// ```
///
/// # 两条实测结论
///
/// 1. **`angle` 直接是「度」** —— 源码是 `RAD2DEG(rot[2])`，即源数据弧度、
///    落盘转度。TOML 遵循本项目「给文件里的终值」的惯例，所以这里收**度**，
///    不做转换（与 [`Eyeball`] 的 `zoffset`/`iris_scale` 同理）。
/// 2. **`motionflags` 不是常量** —— 全语料直方图
///    `192(0xC0)→3291、7→1242、448(0x1C0)→136、256→126、64→109、2240(0x8C0)→97…`
///    是 Source `STUDIO_MOVEMENT_*` 位标志的组合，**不要从 `v0`/`v1`/`angle`
///    反推**，直接给整数值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Movement {
    /// `endframe`。
    pub endframe: i32,
    /// `motionflags`（`STUDIO_MOVEMENT_*` 位组合，**直接给整数**）。
    #[serde(default)]
    pub motionflags: i32,
    /// `v0` 起始速度。
    #[serde(default)]
    pub v0: f32,
    /// `v1` 结束速度。
    #[serde(default)]
    pub v1: f32,
    /// `angle` 本段结束时的 YAW 旋转（**度**）。
    #[serde(default)]
    pub angle: f32,
    /// `vector` 相对本段初始角度的移动向量。
    #[serde(default)]
    pub vector: [f32; 3],
    /// `position` 相对动画起点。
    #[serde(default)]
    pub position: [f32; 3],
}

pub fn default_motion_rollback() -> f32 {
    0.3
}

/// `fadein`/`fadeout` 的缺省值（`studiomdl.cpp:2639-2640`）。
pub fn default_fade_time() -> f32 {
    0.2
}

/// 一个动画事件（`mstudioevent_t`，80 字节）。
///
/// 对应 QC 里 `$sequence` 块内的 `{ event <帧号> <事件名> <参数> }`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceEvent {
    /// 事件发生的时间点，**归一化到 `[0,1]`**（不是帧号）。
    ///
    /// 实测：studiomdl 把 QC 里的帧号除以 `numframes - 1`。
    pub cycle: f32,
    /// 事件类型编号。`$sequence` 的 `event` 指令写 0；
    /// `$sequence` 的 `footstep`/`playermovement` 等会写别的值。
    #[serde(default)]
    pub event_type: i32,
    /// 事件编号。QC 里写的是**事件名**，studiomdl 会查表转成编号；
    /// 本实现直接接受编号（名字到编号的表不在本实现范围内）。
    #[serde(default)]
    pub event: i32,
    /// 事件名（内联 `char[64]`）。
    #[serde(default)]
    pub name: String,
    /// 事件参数（内联 `char[64]`）。
    #[serde(default)]
    pub options: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForcedBoneRealign {
    pub bone: String,
    pub rotation: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoneRotationLimit {
    pub bone: String,
    #[serde(default)]
    pub sequences: Vec<String>,
}

/// `[model]` 段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMeta {
    #[serde(default)]
    pub rotation_limits: Vec<BoneRotationLimit>,
    #[serde(skip)]
    pub rotation_alignments: Vec<Option<[f32; 4]>>,
    #[serde(default)]
    pub root_bone: Option<String>,
    #[serde(default)]
    pub allowed_root_lods: u8,
    #[serde(default)]
    pub constant_directional_light_dot: Option<u8>,
    #[serde(skip)]
    pub source_dependencies: Vec<std::path::PathBuf>,
    #[serde(default)]
    pub minimum_lod: u32,
    #[serde(default)]
    pub autocenter: bool,
    #[serde(skip)]
    pub first_source: Option<String>,
    #[serde(skip)]
    pub autocenter_offset: Option<[f32; 3]>,
    #[serde(default)]
    pub forced_realign: Vec<ForcedBoneRealign>,
    #[serde(default)]
    pub bone_renames: Vec<[String; 2]>,
    #[serde(default)]
    pub bone_hierarchy: Vec<[String; 2]>,
    #[serde(default)]
    pub transition_nodes: Vec<String>,
    #[serde(default)]
    pub transition_skips: Vec<[String; 2]>,
    #[serde(default)]
    pub transition_multistage: bool,
    /// Final QC transform used when flattening a static prop. Rotation is in radians.
    #[serde(default)]
    pub source_rotation: Option<[f32; 3]>,
    #[serde(default)]
    pub source_origin: Option<[f32; 3]>,
    /// Material substitutions applied to output texture slots after geometry binding.
    #[serde(default)]
    pub material_renames: Vec<[String; 2]>,
    /// 输出路径，相对游戏目录，如 `models/props_c17/canister01a.mdl`。
    /// 反斜杠会被规范化为正斜杠。
    pub name: String,
    /// MDL 版本，默认 49（L4D2）。
    #[serde(default)]
    pub version: Option<i32>,
    /// 四件套配对的 checksum。**不写则自动生成**（见 [`ModelDesc::checksum`]）
    /// —— 它不是内容哈希，只是一个配对令牌。
    #[serde(default)]
    pub checksum: Option<i32>,
    /// `$staticprop`。
    #[serde(default)]
    pub static_prop: bool,
    /// `$surfaceprop`。
    #[serde(default)]
    pub surface_prop: Option<String>,
    /// `$eyeposition`。
    #[serde(default)]
    pub eye_position: Option<[f32; 3]>,
    /// `$illumposition`。
    ///
    /// 官方有**两种**形式，落盘语义不同（`oracle_illumposition6.js`）：
    ///
    /// * `$illumposition x y z` ⟹ 落盘 `[-y, x, z]`（做轴变换）；
    /// * `$illumposition x y z <骨骼>` ⟹ 落盘**原样 `[x, y, z]`**，并新建合成
    ///   附着点（见 [`Attachment::synthetic`]）。判据是
    ///   [`Self::illum_position_from_bone`]。
    ///
    /// TOML 表达不了第 4 个 token，所以 TOML 侧永远走 3 参数的轴变换语义。
    #[serde(default)]
    pub illum_position: Option<[f32; 3]>,
    /// [`Self::illum_position`] 是否来自 **4 参数**形式（第 4 个 token 是骨骼名）。
    ///
    /// 为真时 [`Self::illum_position`] 落盘**不做轴变换**。该标志由 QC 前端设置，
    /// TOML 表达不了（`#[serde(skip)]`）。
    #[serde(skip)]
    pub illum_position_from_bone: bool,
    ///
    /// # 语义（反汇编 `0x00450270` 确证）
    ///
    /// 官方 handler 就三条有效指令：
    ///
    /// ```asm
    /// 0x004502a3  call 0x5b6796          ; atof(token)
    /// 0x004502a8  fmul qword [0x9764a0]  ; × π
    /// 0x004502b1  fmul qword [0x97c988]  ; × (1/180)
    /// 0x004502b7  fcos                   ; cos(...)
    /// 0x004502b9  fstp dword [0x14a7708] ; → g_flMaxEyeDeflection（float）
    /// ```
    ///
    /// 两个常量实测为 **π** 与 **1/180** ⟹
    /// **落盘值 = `cos(deg2rad(输入的度数))`**。
    ///
    /// 判据：`$maxeyedeflection 30` → `cos(30°)` → f32
    /// `0.8660253882408142`，与语料 4 个 survivor 的
    /// `flMaxEyeDeflection` **逐位相同**。
    ///
    /// # TOML 收**度**（与 `jiggle_bones` / `quat_interp_bones` 同惯例）
    ///
    /// 那两处也是「TOML 写 QC 里的原始角度、落盘时转换」——
    /// 让用户直接写 `30` 比写 `0.8660254` 更贴近 QC 且不易写错。
    ///
    /// # 缺省 = **不写（保持 0）**
    ///
    /// `g_flMaxEyeDeflection` 在 `.bss`（未初始化）⟹ 初值 **0**，
    /// 而引擎读 0 时**回退到 `cos(30°)`**（`studio.h:2173`：
    /// `flMaxEyeDeflection != 0.0f ? flMaxEyeDeflection : 0.866f`）。
    ///
    /// 所以「不写」与「写 30」在**渲染上等价**，只是字节不同 ——
    /// 语料 3333 个模型里只有 **4 个**（survivor）显式写了它。
    /// mdlc 缺省保持 0，与官方对「没写该命令的 QC」的行为一致。
    #[serde(default)]
    pub max_eye_deflection: Option<f32>,
    /// 移动包围盒；不写则由顶点自动计算。
    #[serde(default)]
    pub hull_min: Option<[f32; 3]>,
    #[serde(default)]
    pub hull_max: Option<[f32; 3]>,
    /// `$cbox` clipping bounds in model space, independent of the movement hull.
    #[serde(default)]
    pub view_bb_min: Option<[f32; 3]>,
    #[serde(default)]
    pub view_bb_max: Option<[f32; 3]>,
    /// 额外的 `STUDIOHDR_FLAGS_*` 位（`static_prop` 会自动置位）。
    #[serde(default)]
    pub extra_flags: Option<i32>,
    /// `$contents`：骨骼内容标志（如 `solid` = 1）。
    ///
    /// **默认是 1（`CONTENTS_SOLID`），不是 0。**
    ///
    /// 源码：`studiomdl.cpp:5031` 的
    /// `static int s_nDefaultContents = CONTENTS_SOLID;`
    /// 与 `write.cpp:187` 的 `phdr->contents = GetDefaultContents();` ——
    /// 无条件默认，与是否自动生成 hitbox **无关**。
    ///
    /// 语料印证（`probe_contents_mass.js`）：3333 个模型里 **3281 个是 1**，
    /// 其余 52 个是 `CONTENTS_GRATE`（= 8，来自显式 `$contents "grate"`）。
    #[serde(default)]
    pub contents: Option<i32>,
    #[serde(default)]
    pub joint_contents: Vec<(String, i32)>,
    /// `$skipboneinbbox`：自动 hitbox / 包围盒**不用骨骼原点**。
    ///
    /// `Cmd_SkipBoneInBBox`（`studiomdl.cpp:5706`）把
    /// `g_bUseBoneInBBox` 置 false，于是 `SetupHitBoxes()` 的 bbox 从
    /// `±9999` 起步（而不是默认的**全 0**）。
    ///
    /// 差别很直观：默认（全 0 起步）时**原点恒在 bbox 内**，
    /// 所以产物里 `bbmin` 常见 `0.00`；置了本项之后 bbox 就是几何的真实
    /// AABB，不含原点的 box 也会保留。
    ///
    /// 语料：3104 个自动生成 hitbox 的模型里，**21 个**是这个形态
    /// （`probe_skipboneinbbox.js` 用「原点是否在 box 内」干净二分：
    /// 3082 全含 / 21 全不含 / 1 混合），且那 21 个**全部**是
    /// `static_prop`（多为 `*_chunk*.mdl` 碎片）。
    #[serde(default)]
    pub skip_bone_in_bbox: bool,
    /// **VTX 顶点缓存优化**（`meshopt_optimizeVertexCache`）。默认 `false`。
    ///
    /// 打开后，写 `.dx90.vtx` 时对**每个 strip group**单独重排三角形顺序，
    /// 以减少顶点着色器的调用次数（提高 GPU post-transform cache 命中率）。
    ///
    /// # 默认关闭的理由
    ///
    /// 本项目的验收基准是**真 `studiomdl.exe`**（`HANDBOOK.md` 教训 75），
    /// 而开这个开关会**改变索引顺序**，使产物与真 studiomdl 不再逐字节一致。
    /// 它是**纯性能优化** —— 三角形集合、绕序、顶点池、
    /// `origMeshVertID`、几何 / UV / 法线 / 骨骼绑定全部不变。
    ///
    /// # 与官方其他实现的关系
    ///
    /// * 真 `studiomdl.exe` 的对应开关是 `-nvtristrip`（走 NvTriStrip）；
    /// * 第三方 `nekomdl` **默认**走 meshoptimizer，`-nvtristrip` 才回退。
    ///
    /// 实测（`probe_vtx_three_way.js`）：真 studiomdl 与 nekomdl 的
    /// `origMeshVertID` **都是非平凡排列**（21/21 个 strip group），
    /// 说明**两者都会重排**。所以打开本项是**更接近官方行为**的，
    /// 只是不会逐字节相同 —— meshopt 与 NvTriStrip 是两套不同算法。
    ///
    /// # 三个等价入口
    ///
    /// | 入口 | 形态 | 方向 |
    /// |---|---|---|
    /// | TOML | `[model] optimize_vtx = true` | 双向 |
    /// | 命令行 | `--optimize-vtx`（见 `main.rs`） | **只能开** |
    /// | QC | `$optimizevtx` | **只能开** |
    ///
    /// 后两者是 `||` 关系（`main.rs` 的
    /// `desc.model.optimize_vtx || cli_optimize_vtx`），所以一旦在命令行
    /// 或 QC 里打开就**没法关**（缺省本来就是关的，没有关的需求）。
    ///
    /// ⚠️ `$optimizevtx` 是 **mdlc 自己的扩展**，官方与 NekoMDL 都没有
    /// 这个 QC 命令（官方是 `-nvtristrip` 命令行开关）——
    /// 真 `studiomdl.exe` 对它报 `bad command`。
    #[serde(default)]
    pub optimize_vtx: bool,
    /// **顶点超限时自动拆分**（默认 **`true`**）。
    ///
    /// # 为什么需要它
    ///
    /// VTX 的 `Vertex_t.origMeshVertID` 是 `uint16`，所以**一个 mesh 最多
    /// 65536 个顶点**（见 [`crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH`]）。
    /// 一个 mesh 对应**一个材质**，所以某个材质如果本身就有几十万顶点
    /// （真实案例：某改模工程的 `chain` 段有 305,703 顶点），
    /// 官方 `studiomdl` 会直接拒绝（`ERROR: too many indices in source`）。
    ///
    /// 打开本项时，mdlc 会把这种 mesh **按三角形顺序切成多个 mesh**，
    /// 每块不超过上限，全部放在**同一个 model** 里、**都指向同一个材质**。
    ///
    /// # 为什么拆成「同 model 内的多个 mesh」而不是新 bodypart
    ///
    /// 第三方 `nekomdl` 的 `$maxverts` 扩展是拆成**新 bodypart**
    /// （命名 `clamped1`/`clamped2`…，见 `docs/_probe/nekomdl_maxverts_findings.js`）。
    /// 本实现不那样做，因为：
    ///
    /// 1. **bodypart 数量会改变引擎的 bodygroup 语义** ——
    ///    `$bodygroup` 的选择是按下标走的，凭空多出几个 bodypart 会让
    ///    原本的 bodygroup 编号错位；
    /// 2. NekoMDL 自己的产物里出现了**重名 bodypart**（实测两个 `clamped1`），
    ///    说明那个命名方案本身不够严谨；
    /// 3. `mesh.material` 只是 `pSkinref[]` 的**下标**，两个 mesh 用同一下标
    ///    完全合法 —— 引擎渲染结果与拆分前**逐像素相同**。
    ///
    /// # 默认打开的理由
    ///
    /// 它只在**本来就会编译失败**的情况下生效（mesh 超限），
    /// 对不超限的模型**完全不碰**（见 `split_oversized_meshes` 的提前返回），
    /// 所以对既有产物零影响。开着它，用户不必先撞一次墙再去查文档。
    ///
    /// 关掉它（`split_oversized_meshes = false`）则遇到超限 mesh 时**直接报错**，
    /// 报错信息会给出两条可行路径。
    ///
    /// # 三个等价入口
    ///
    /// | 入口 | 形态 | 方向 |
    /// |---|---|---|
    /// | TOML | `[model] split_oversized_meshes = false` | 双向 |
    /// | QC | `$nosplitoversizedmeshes` | **只能关** |
    /// | QC | `$splitoversizedmeshes` | **只能开**（把 `$include` 关掉的再开回来） |
    ///
    /// 两个 QC 命令都是 **mdlc 自己的扩展** —— 官方与 NekoMDL 都没有
    /// （NekoMDL 的 `$maxverts` 做的是另一件事：拆成**新 bodypart**）。
    /// 真 `studiomdl.exe` 对它们报 `bad command`。
    #[serde(default = "default_true")]
    pub split_oversized_meshes: bool,
    /// `$keyvalues` 块的内容（**不含外层 `mdlkeyvalue` 包装**）。
    ///
    /// 实测 735/3333 (22.1%) 的真实模型有 keyvalues。落盘格式是：
    ///
    /// ```text
    /// "mdlkeyvalue\n{\n" + 本字段 + "}\n\0"
    /// ```
    ///
    /// 例如 `prop_data { "base" "Wooden.Tiny" }` 会写成
    /// `mdlkeyvalue\n{\nprop_data {\n"base" "Wooden.Tiny"  }\n}\n\0`。
    ///
    /// ⚠️ 前缀 `mdlkeyvalue` 是**裸词，没有前导引号** ——
    /// 引号只逐 token 加在块内的键值上（`studiomdl.cpp:5763` vs `5784`）。
    /// 语料判据：735 个有 keyvalues 的模型里首字节是 `"` 的有 **0** 个。
    ///
    /// 这里让调用方直接给**内层文本**，由写出器负责包装 ——
    /// 因为 studiomdl 的包装格式（缩进、双空格、结尾换行）很特殊，
    /// 不适合让用户手写。
    #[serde(default)]
    pub key_values: Option<String>,
    /// `$poseparameter` 列表（`mstudioposeparamdesc_t`，20 字节/条）。
    ///
    /// # 语义
    ///
    /// 姿势参数是**混合动画的驱动轴**：`mstudioseqdesc_t` 用
    /// `paramindex[2]` / `paramstart[2]` / `paramend[2]` 指向它们，
    /// 引擎按参数值在 `groupsize[0] × groupsize[1]` 的 blend 网格里插值。
    ///
    /// # 顺序即下标
    ///
    /// QC 里 `$poseparameter` 的**书写顺序就是下标**，序列通过名字
    /// （`$sequence ... { ... }` 的 blend 引用）或隐式顺序关联。
    /// 这里按 `Vec` 顺序写出，下标 = 位置。
    ///
    /// # 语料形态
    ///
    /// 实测 86/3333 (2.6%) 的模型有姿势参数，`numlocalposeparameters`
    /// 从 1 到 **11**（`anim_biker.mdl`）。`flags` 只有两种取值：
    /// **0**（189 条）与 **1**（`STUDIO_LOOPING`，96 条）；
    /// `flags == 1` 时 `loop` 恒非 0，`flags == 0` 时 `loop` 恒为 0。
    #[serde(default)]
    pub pose_parameters: Vec<PoseParameter>,
    /// `$realignbones`：把骨骼轴重对齐的判据放宽到**整副骨架**。
    ///
    /// # 两条触发路径（`RealignBones`，`simplify.cpp:4224-4286`）
    ///
    /// 1. **`$ikchain`** —— 总是对每条链的相邻两段填 `childbone[]`，
    ///    与这个开关**无关**；
    /// 2. **`$realignbones`**（本字段）—— 额外把「父骨骼只有唯一子骨骼」
    ///    的所有骨骼也纳入。
    ///
    /// 实测：受控实验 `ipr2`（只写 `$ikchain`）与 `ipr3`（只写
    /// `$realignbones`）在一条单链骨架上产出**逐位相同**的结果 ——
    /// 因为两条路径填出的 `childbone[]` 恰好一致。
    ///
    /// # 判据与算法
    ///
    /// 见 [`crate::bone_math::realign_bones`]。判据是子骨骼的**局部**位移
    /// 不沿 +X（`d - x > 0.01`），满足才重排该骨骼的局部基。
    #[serde(default)]
    pub realign_bones: bool,
    /// `$animblocksize`：**动画块大小（KB）**，写 `.ani` 外置文件的阈值。
    ///
    /// # 语义（`studiomdl.cpp:1479-1487`）
    ///
    /// ```cpp
    /// g_animblocksize = verify_atoi( token );
    /// if (g_animblocksize < 1024) g_animblocksize *= 1024;   // 单位是 KB
    /// ```
    ///
    /// **缺省 `0` ⟹ 完全不写 `.ani`**（`numanimblocks = 0`）——
    /// 这也是 L4D2 语料里 3212/3333 个模型的情形。
    /// `$animblocksize 4` = **4096 字节**（`< 1024` 时自动 ×1024）。
    ///
    /// ⚠️ 与 `section_frames` 不同，它是**模型级**设置（不是每条序列），
    /// 所以放在 `[model]` 而不是 `[[sequences]]`。
    ///
    /// # 判据不是「大于阈值」
    ///
    /// 实测（`qabs1.qc`，`$animblocksize 4`）：**只要非 0 就一定产生 `.ani`**，
    /// 即便所有动画加起来远小于 4096 字节 —— 见 [`crate::anim_writer`] 的说明。
    #[serde(default)]
    pub anim_block_size: Option<i32>,
    /// **顶层 `$sectionframes <每段帧数> <阈值>` 的全局值**。
    ///
    /// # 为什么必须是全局
    ///
    /// 官方把它存成**两个全局变量**（`g_sectionframes` / `g_sectionthreshold`），
    /// 每条动画写出时**现取**。而 mdlc 的 IR 是逐序列字段，早先只在
    /// `Parser::finish` 里把它回填给**序列**（`qc/parse.rs`）—— 于是
    /// **没有被任何 `$sequence` 引用的 `$animation`** 拿不到它。
    ///
    /// # 实测（`v_dual_pistola_processed.qc`，`$sectionframes 30 36`）
    ///
    /// `look_neutral` 只被 `$animation` 声明、**没有任何序列引用它**：
    ///
    /// ```text
    /// 官方 animdesc[2] look_neutral: nf=1  sectionframes=0   sectionindex=0
    /// mdlc 修前                    : nf=1  sectionframes=30  sectionindex=71804
    /// ```
    ///
    /// 原因是 `anim_specs` 反查引用序列时用了
    /// `.position(|s| s.cells.contains(&i)).unwrap_or(0)` —— 找不到就落到
    /// **`seq[0]`（`idle`）**，于是继承了 `idle` 的 `section_frames=30`。
    ///
    /// `sectionframes=30` 而 `nf=1` 会让段表长成
    /// `nEnt = 1/30 + 2 = 2` 条 —— 官方那 4 条 `look_*`（都是 1 帧）
    /// 全是 `sf=0 / sectionindex=0`，只有 `look_neutral` 因为孤儿身份
    /// 走错分支。（数值上 1 帧动画的段表不影响姿态，但字段与官方不一致，
    /// 且多出 `nEnt*8` 字节。）
    ///
    /// 有它之后，孤儿动画按**全局值**判 `nf >= 阈值`，与官方同一条规则。
    #[serde(default)]
    pub section_frames: Option<(i32, i32)>,
}

/// **物理 / 碰撞模型**参数 —— `[physics]` 表。
///
/// # 为什么从 `[model]` 里拆出来
///
/// 原先 `collision_smd` / `collision_mass` / `mass` / `concave` /
/// `collision_joints` 五个键全堆在 `[model]` 里，问题是：
///
/// 1. **`mass` 有两份、语义重叠** —— `[model].mass` 与
///    `[model].collision_mass` 官方是**同一个值**（`write.cpp:2092`
///    `phdr->mass = GetCollisionModelMass();`），两个键并存只会让人
///    不知道该写哪个。现在只有 `[physics].mass` 一个。
/// 2. **前缀噪音** —— `collision_*` 前缀只是为了在 `[model]` 里不撞名，
///    放进独立表后可以直接叫 `smd` / `joints`。
/// 3. **参数会继续长** —— `$damping` / `$rotdamping` / `$inertia` /
///    `$drag` / `$rootbone` / `$jointconstrain` 都要加，
///    堆在 `[model]` 里会把「模型元信息」和「碰撞配置」混在一起。
///
/// # QC 对应关系
///
/// ```text
/// $collisionmodel "phys.smd" { $mass 10 $concave $damping 0.05 }   →  [physics]
/// $collisionjoints "phys.smd" { $mass 500 $rotdamping 3 }          →  [physics]
/// ```
///
/// # 旧键已移除（**不保留兼容**）
///
/// `[model]` 里的 `mass` / `collision_smd` / `collision_mass` /
/// `concave` / `collision_joints` 五个键**已删除**。
/// `ModelMeta` 带 `deny_unknown_fields`，所以旧 TOML 会**直接报错**
/// 并指出未知键 —— 比静默忽略或静默兼容更容易发现问题。
///
/// 仓库内 9 个用到这些键的 TOML 已一并迁移。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Physics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision_text: Option<String>,
    /// `$collisionmodel <smd>` / `$collisionjoints <smd>` 指向的**碰撞 SMD**。
    ///
    /// # 为什么是独立的 SMD
    ///
    /// 官方 QC 里碰撞几何来自 **`$collisionmodel <.smd>`**，它**指向另一个
    /// SMD 文件**（通常叫 `<name>_phys.smd` 或 `phys.smd`），与
    /// `$body`/`$sequence` 用的渲染网格是**两份数据**。碰撞网格通常刻意做得
    /// 比渲染网格简单（少几千个三角形），而且经常是**凸的、闭合的**。
    ///
    /// 所以本项也接受一个**单独的 SMD 路径**，而不是复用渲染网格 ——
    /// 复用会让 `.phy` 又大又慢，且与官方产物不可比。
    ///
    /// # 与 `[[bodyparts.models]].smd` 的关系
    ///
    /// 碰撞 SMD 的**骨骼名不必与渲染 SMD 相同**（官方也不要求），
    /// 因为 `.phy` 只存凸包几何 + 可选的 `boneIndex`，不引用骨骼名 ——
    /// 除非走 ragdoll（`joints = true`）路径，那时才按 `parentBone` 分组。
    #[serde(default)]
    pub smd: Option<String>,
    /// `$mass`：碰撞模型的等效总质量。
    ///
    /// ⚠️ 它**同时**写进两个地方，因为官方就是同一个值：
    ///
    /// * `.phy` 的 `editparams.totalmass`
    /// * `.mdl` 头部 `+0x148` 的 `mass`
    ///
    /// 依据：`write.cpp:2092` 的 `phdr->mass = GetCollisionModelMass();`
    /// 返回的就是 `g_JointedModel.m_totalMass`。studiomdl 里**没有**
    /// 顶层 `$mass` 命令，它只出现在 `$collisionmodel {}` /
    /// `$collisionjoints {}` 块里。
    ///
    /// 语料实测（`docs/_probe/probe_mass_law.js`）：有 `.phy` 的 2498 个模型里
    /// **2483 个** `mdl.mass == phy.totalmass`（其余 15 个的 `.phy` 是陈旧产物）。
    /// 没有 `.phy` 的 835 个里 **827 个** `mdl.mass == 1.0`。
    #[serde(default)]
    pub mass: Option<f32>,
    /// `$concave`：按**连通分量**把碰撞网格拆成多个凸块。
    ///
    /// # ⚠️ 这不是 VHACD 式的「体分解」
    ///
    /// 反编译 `ProcessSingleBody` 确认官方算法是：
    ///
    /// ```text
    /// 顶点焊接（位置相同 且 法线夹角 < 2°）
    ///   → 按共享焊接顶点做并查集（连通分量）
    ///   → 每个连通分量各算一个凸包
    /// ```
    ///
    /// 所以对**连通**的凹体（U 形、圆环面），官方 `$concave` 给出的就是
    /// **整个网格的凸包** —— 凹处被**填平**。只有网格有**多个互不相连的
    /// 壳**时才会产出多个凸块。
    ///
    /// 实测（`docs/_probe/gen_concave_torus.js`）：
    ///
    /// | 网格 | 官方 `$concave` | VHACD |
    /// |---|---|---|
    /// | 光滑圆环面（连通、凹） | **1** 块，体积 22728.12 | 12 块，体积 19343.18 |
    /// | 两个分离光滑球 | **2** 块，体积 4020.83 | — |
    ///
    /// **两者不可互换** —— 官方填平、VHACD 保留凹口。用 VHACD 冒充
    /// `$concave` 会让玩家卡进本该实心的区域。本字段走官方语义
    /// （[`crate::phy::decompose_connected_components`]）。
    ///
    /// # 两条回退规则
    ///
    /// 命中任一条都会**退化成整网格一个凸包**（但产物里仍写 `concave "1"`）：
    ///
    /// 1. **任一分量是平面** —— 官方判定模型没设 smoothing group。
    ///    平面着色的网格必然命中（硬边处法线夹角 ≥ 2° 焊不上，
    ///    每个面各自成分量，而面是平的）。
    /// 2. **分量数 > 20** —— `COSTLY COLLISION MODEL`。
    ///
    /// # 语料频率
    ///
    /// `concave "1"` 出现在 **1903/2498** 个有 `.phy` 的模型里
    /// （`docs/_probe/probe_phy_editparams.js`）。
    #[serde(default)]
    pub concave: bool,
    #[serde(default)]
    pub concave_per_joint: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_convex_pieces: Option<usize>,
    /// `$collisionjoints`：把碰撞模型拆成**每根骨骼一个 solid** 的 ragdoll。
    ///
    /// # 与 `$collisionmodel` 的区别
    ///
    /// 官方 `Cmd_CollisionJoints`（`studiomdl.cpp:6532`）与
    /// `Cmd_CollisionModel`（`6527`）只差一个 `separateJoints` 参数：
    ///
    /// ```cpp
    /// void Cmd_CollisionModel()  { DoCollisionModel( false ); }
    /// void Cmd_CollisionJoints() { DoCollisionModel( true );  }
    /// ```
    ///
    /// `true` ⟹ `ProcessJointedModel`：**遍历每根骨骼**，
    /// 收集「有任何顶点权重落在这根骨骼上」的面（`CopyFaceVertsByBone`），
    /// 每根骨骼算一个凸包、写一个 solid，`client_data = 骨骼下标 + 1`。
    ///
    /// `false` ⟹ `ProcessSingleBody`：整个网格一个 solid（prop 形态）。
    ///
    /// # 分组判据（`FaceHasVertOnBone`，`collisionmodel.cpp:778-822`）
    ///
    /// 一个面只要**任一顶点**的**任一** `links[].bone` 等于该骨骼，
    /// 整个面就归这根骨骼 —— 注意是**面**级归属，不是顶点级。
    /// 同一根骨骼的顶点是「所有归属它的面的三个顶点」的并集。
    ///
    /// # `parent` 的修正（`FixCollisionHierarchy`）
    ///
    /// 骨骼的**直接**父骨骼未必也在碰撞模型里（比如 `Spine2` 的父是
    /// `Spine1`，但 `Spine1` 没有碰撞几何）。官方 `FixParent`
    /// （`collisionmodel.cpp:1130-1151`）会**沿骨骼链往上找**第一个
    /// 真正在碰撞列表里的祖先，把它当父。
    ///
    /// # 与 `concave` 的关系
    ///
    /// 两者**互斥**：ragdoll 每个骨骼本来就是一个凸包，
    /// `$concave` 只作用于单 solid 的 prop 路径。
    #[serde(default)]
    pub joints: bool,
    /// `$damping <v>`：**默认**阻尼（逐 solid 写出）。
    ///
    /// # 官方默认与写法
    ///
    /// `CJointedModel` 构造函数 `m_defaultDamping = 0`（`collisionmodel.cpp:261`），
    /// 每个 solid 从它继承（`SetCollisionModelDefaults`，`:579`），
    /// 最终逐 solid 写进文本段（`:2373` `KeyWriteFloat(fp, "damping", ...)`）。
    ///
    /// **语料实测**（`probe_phy_editparams.js`，2498 个 `.phy`）：
    /// `0.000000` 86.9%、`0.050000` 9.5%、`0.150000` 1.8%、
    /// `0.010000` 1.4%、`5.0` 0.4%、`1.0` 0.03%。
    /// 所以 **13% 的真实模型用了非默认值** —— 不是可忽略的字段。
    ///
    /// # 与 `$jointdamping` 的区别
    ///
    /// `$damping` 改**默认值**（所有 solid 共享）；
    /// `$jointdamping <joint> <v>` 改**单个** joint
    /// （`CJointedModel::JointDamping`，`:658`）。
    /// 语料实测：`damping` 在 **0/39** 个多 solid 文件里逐 solid 不同
    /// ⟹ 只需全局键，见 [`Self::joint_overrides`] 的说明。
    #[serde(default)]
    pub damping: Option<f32>,
    /// `$rotdamping <v>`：**默认**旋转阻尼（逐 solid 写出）。
    ///
    /// 官方默认 `0`（`collisionmodel.cpp:262`）。
    ///
    /// **语料实测**：`0.000000` 81.3%，其余 15 种取值（`0.4` 5.7%、
    /// `1.0` 5.7%、`3.0` 1.7% …）。
    ///
    /// ⚠️ **这一项在语料里逐 solid 变化**：**14/39** 个多 solid 文件
    /// （boomer / hulk / hunter 等 ragdoll）里各 solid 的 `rotdamping`
    /// 互不相同 —— 那些 QC 用了 `$jointrotdamping`。
    /// 所以全局键 + [`Self::joint_overrides`] **两个都要**。
    #[serde(default)]
    pub rot_damping: Option<f32>,
    /// `$inertia <v>`：**默认**惯性缩放（逐 solid 写出）。
    ///
    /// 官方默认 **`1.0`**（`collisionmodel.cpp:263`）。
    ///
    /// ⚠️ **与 `IVP_Compact_Surface::rotation_inertia` 无关** ——
    /// 后者是几何量，这里是 QC `$inertia` 的文本回显。
    ///
    /// **语料实测**：`1.000000` 86.7%、`2.000000` 6.5%、`10.000000` 6.3%、
    /// `5.25` 0.3%、`12.0` 0.1%。
    ///
    /// 语料里 **1/39** 个多 solid 文件（`anim_common`）逐 solid 不同。
    #[serde(default)]
    pub inertia: Option<f32>,
    /// `$drag <v>`：默认空气阻力系数。
    ///
    /// 官方默认 **`-1`**（表示「不写这一行」）——
    /// `collisionmodel.cpp:2376` 的 `if (pPhys->m_dragCoefficient != -1)`。
    ///
    /// **语料实测：`drag` 在 2498 个 `.phy` 里出现 0 次**
    /// ⟹ 保留字段以便表达，但真实模型不用它。
    #[serde(default)]
    pub drag: Option<f32>,
    /// `$rootbone <name>`：指定 ragdoll 的根骨骼名。
    ///
    /// 官方 `CCmd_JointRoot`（`collisionmodel.cpp:1745-1749`）只是
    /// `strcpy(joints.m_rootName, pBone)`，最终写进
    /// `editparams.rootname`（`:2462`）。
    ///
    /// ⚠️ **缺省是空串**，不是根 solid 的名字 —— 语料 39 个 ragdoll 里
    /// 13 个是空的。空串时**不写**这一行（写空串与不写等价）。
    ///
    /// `$rootbone " "`（一个空格）是常见写法（mikuw 就是），
    /// 落盘就是那个空格 —— **不要 trim**。
    #[serde(default)]
    pub root_bone: Option<String>,
    /// **逐 joint 的覆盖**：`$jointdamping` / `$jointrotdamping` /
    /// `$jointinertia` / `$jointmassbias`。
    ///
    /// # 为什么需要
    ///
    /// 官方这三个命令都作用于**单个 joint**（`collisionmodel.cpp:658-692`），
    /// 而语料里确实用到了：
    ///
    /// | 项 | 逐 solid 不同的多-solid 文件数 |
    /// |---|---|
    /// | `rotdamping` | **14 / 39** |
    /// | `inertia` | 1 / 39 |
    /// | `massbias` | **21 / 39** |
    /// | `damping` | 0 / 39 |
    ///
    /// 只给全局键会让 boomer / hulk / hunter 这类 ragdoll 的
    /// `rotdamping` 全写成默认值 —— 表现是布娃娃「转得太顺」或「转不动」。
    ///
    /// # 键是**骨骼名**
    ///
    /// 官方 `$jointdamping <jointName> <v>` 的 `jointName` 是
    /// **骨骼名**（`InitCollisionModel(*this, pJointName)` 按名字找），
    /// 与 `.phy` 文本段里 solid 的 `"name"` 是同一个名字。
    /// 这里同样用名字，不用下标 —— 下标在增删骨骼时会失效。
    #[serde(default)]
    pub joint_overrides: Vec<JointOverride>,
    /// `$jointconstrain <骨骼> <轴> <类型> <min> <max> [friction]`。
    ///
    /// # 为什么重要
    ///
    /// 语料实测：**307/325 个 `ragdollconstraint` 块（94.5%）有非零轴**
    /// （`docs/_probe/probe_phy_jointconstrain_corpus.js`）——
    /// 也就是说绝大多数 ragdoll 的关节限位不实现就是错的，
    /// 布娃娃会「软趴趴」地乱转。
    ///
    /// # 官方语义（`CCmd_JointConstrain`，`collisionmodel.cpp:1651-1695`）
    ///
    /// ```text
    /// $jointconstrain <jointName> <axis> <type> <limitMin> <limitMax> [friction]
    /// ```
    ///
    /// * `axis` —— 只有**首字母**有意义（`tolower(axis[0]) - 'x'`）。
    /// * `type` —— `free` / `fixed` / `limit`（其它值只警告、不记录）。
    /// * `friction` 省略时官方默认 **`"1.0"`**（`:1848-1851`）。
    /// * 落盘前 friction **÷5**（`AddConstraint`，`:539`）。
    ///
    /// 三种类型的落盘（`BuildRagdollConstraint`，`:2245-2256`）：
    ///
    /// | type | 落盘 |
    /// |---|---|
    /// | `limit` | `(min, max, friction/5)` |
    /// | `fixed` | `(0, 0, 0)` |
    /// | `free`  | `(-360, 360, friction/5)` |
    #[serde(default)]
    pub constraints: Vec<JointConstraintSpec>,
    /// `$animatedfriction <min> <max> <timein> <timehold> <timeout>`。
    ///
    /// ⚠️ **QC 参数顺序与落盘顺序不同**：第 4/5 个参数
    /// （`timehold`/`timeout`）在官方代码里被**对调**赋给
    /// `m_flFrictionTimeOut` / `m_flFrictionTimeHold`
    /// （`CCmd_JoinAnimatedFriction`，`collisionmodel.cpp:1752-1760`）。
    /// 本 TOML 用**语义化字段名**，由 `to_animated_friction()` 负责对应，
    /// 免得用户踩同一个坑。
    ///
    /// 语料频率 **1.16%**（29/2498）。
    #[serde(default)]
    pub animated_friction: Option<AnimatedFrictionSpec>,
    /// `$noselfcollisions`：同一模型内部碰撞体之间不碰撞。
    ///
    /// 落盘 `collisionrules { "selfcollisions" "0" }`。
    /// ⚠️ **与 [`Self::collision_pairs`] 互斥**，本项**优先**
    /// （官方 `if (m_noSelfCollisions) ... else if (m_pCollisionPairs)`）。
    ///
    /// 语料频率 **0.60%**（15/2498）。
    #[serde(default)]
    pub no_self_collisions: bool,
    /// `$jointcollide <骨骼A> <骨骼B>`：**显式**允许这一对碰撞体互相碰撞。
    ///
    /// 落盘 `collisionrules { "collisionpair" "<i>,<j>" }`（下标是 solid 下标）。
    /// 只有 [`Self::no_self_collisions`] 为 false 时才写出。
    ///
    /// 语料：只有 `anim_common.phy` 用到（377 条 `collisionpair`）。
    #[serde(default)]
    pub collision_pairs: Vec<CollisionPairSpec>,
    /// `$jointmerge <父骨骼> <子骨骼>`：把子骨骼的碰撞几何**并入**父骨骼。
    ///
    /// 落盘 `editparams { "jointmerge" "<父>,<子>" }` ——
    /// 用的是 **QC 里写的原始名字**（官方 `strdup` 后原样拼回），
    /// 不是解析后的骨骼名。
    ///
    /// 实测（受控实验 `merge`）：`$jointmerge "bone_mid" "bone_tip"`
    /// ⟹ `"jointmerge" "bone_mid,bone_tip"`，且 **solid 数从 3 降到 2**
    /// （子骨骼不再单独成 solid）。
    ///
    /// 语料：**0 次**（`probe_phy_jointconstrain_corpus.js`）。
    #[serde(default)]
    pub merge: Vec<CollisionPairSpec>,
    /// Collision joints omitted from the physical skeleton (`$jointskip`).
    #[serde(default)]
    pub skip: Vec<String>,
    /// Uniform scale captured when the collision source is declared.
    #[serde(default)]
    pub input_scale: Option<f32>,
    /// `$masscenter <x> <y> <z>`：**强制**质心位置。
    ///
    /// 官方走 `CollideSetMassCenter`（`collisionmodel.cpp:361-363`），
    /// 改的是二进制 `IVP_Compact_Surface` 的 `mass_center` 与
    /// `upper_limit_radius`（后者 = 以新质心为中心的最大半径）。
    ///
    /// ⚠️ 受控实验里写它会触发 studiomdl 的
    /// `EXCEPTION_ACCESS_VIOLATION`（`masscenter` 用例），
    /// 说明该路径在本构建里不稳定；本项**保留表达能力**但默认不写。
    ///
    /// 语料：**0 次**。
    #[serde(default)]
    pub mass_center: Option<[f32; 3]>,
    /// `$automass`：总质量按**体积 × 材质密度**自动算。
    ///
    /// 官方 `SetAutoMass` 把 `m_totalMass` 置 `-1` 当哨兵，`ComputeMass`
    /// 首句 `if (m_totalMass >= 0) return;` 于是才会真的算
    /// （`collisionmodel.cpp:567-570 / 590-622`）。
    ///
    /// 实测（受控实验 `automass`，3 个 10³ 盒子 + `metal`）：
    /// `totalmass = 5.309408`（而显式 `$mass 100` 时是 `100.000000`）。
    ///
    /// ⚠️ 需要**材质密度表**（`scripts/surfaceproperties_manifest.txt`）。
    /// mdlc 没有该表，所以本项为 true 时**显式报错**而不是静默写错值。
    ///
    /// 语料：**0 次**。
    #[serde(default)]
    pub auto_mass: bool,
}

/// TOML 里的一条 `$jointconstrain`。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointConstraintSpec {
    /// 骨骼名。
    pub bone: String,
    /// 轴：`"x"` / `"y"` / `"z"`（只有首字母有意义，与官方一致）。
    pub axis: String,
    /// 类型：`"free"` / `"fixed"` / `"limit"`。
    pub kind: String,
    /// 下限（度）。`free`/`fixed` 时被忽略。
    #[serde(default)]
    pub min: f32,
    /// 上限（度）。`free`/`fixed` 时被忽略。
    #[serde(default)]
    pub max: f32,
    /// 摩擦。省略时官方默认 **`1.0`**（落盘 `0.2`）。
    #[serde(default)]
    pub friction: Option<f32>,
}

/// TOML 里的一对骨骼（`$jointcollide` / `$jointmerge` 共用）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollisionPairSpec {
    /// 第一个骨骼名。
    pub a: String,
    /// 第二个骨骼名。
    pub b: String,
}

/// TOML 里的 `$animatedfriction`（**语义化字段名**，顺序已纠正）。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnimatedFrictionSpec {
    /// `animfrictionmin`（整数）。
    pub min: i32,
    /// `animfrictionmax`（整数）。
    pub max: i32,
    /// `animfrictiontimein`（秒）。
    pub time_in: f32,
    /// `animfrictiontimeout`（秒）—— QC 的第 **5** 个参数。
    pub time_out: f32,
    /// `animfrictiontimehold`（秒）—— QC 的第 **4** 个参数。
    pub time_hold: f32,
}

/// 单个 joint（骨骼）的物理参数覆盖。
///
/// 对应 QC 的 `$jointdamping` / `$jointrotdamping` / `$jointinertia` /
/// `$jointmassbias`（`collisionmodel.cpp:1855-1868`）。
///
/// # 只对 ragdoll 有意义
///
/// 这些命令走 `CJointedModel::InitCollisionModel`，而 `InitCollisionModel`
/// **只在 `ProcessJointedModel`（ragdoll）里被调用** —— 单 solid 的 prop
/// 路径没有「joint」概念。所以 `joints = false` 时写这一项会被忽略
/// （本实现会在编译时**报错**而不是静默忽略）。
///
/// # 语料实测的取值
///
/// `boomer.phy` 的 `rotdamping` 是 `3,5,6,2,4,1,7`（7 个 solid 各不相同），
/// 对应的 QC 就是一串 `$jointrotdamping`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointOverride {
    /// 骨骼名（必须能在 `[[bones]]` 里找到，且该骨骼有碰撞几何）。
    pub bone: String,
    /// `$jointdamping <bone> <v>`。
    #[serde(default)]
    pub damping: Option<f32>,
    /// `$jointrotdamping <bone> <v>`。
    #[serde(default)]
    pub rot_damping: Option<f32>,
    /// `$jointinertia <bone> <v>`。
    #[serde(default)]
    pub inertia: Option<f32>,
    /// `$jointmassbias <bone> <v>`。
    ///
    /// 官方默认 `1.0`；只有 `!= 1.0` 才写进文本段
    /// （`collisionmodel.cpp:2382`）。
    #[serde(default)]
    pub mass_bias: Option<f32>,
}

impl Physics {
    /// 写进 `.mdl` 头部 `+0x148` **与** `.phy` `editparams.totalmass` 的质量。
    ///
    /// 两者**必须是同一个值** —— 官方 `write.cpp:2092` 是
    /// `phdr->mass = GetCollisionModelMass();`，即 `.mdl` 的 mass 直接取自
    /// 碰撞模型的总质量。所以这里只算一次，两个写出器都调它。
    ///
    /// 缺省 **1.0**（`CJointedModel` 构造函数 `m_totalMass = 1.0`，
    /// 且 `ComputeMass()` 首句 `if (m_totalMass >= 0) return;` 直接返回）。
    ///
    /// 语料实测（`docs/_probe/probe_mass_law.js`）：
    /// 没有 `.phy` 的 835 个模型里 **827 个**是 1.0。
    pub fn effective_mass(&self) -> f32 {
        self.mass.unwrap_or(1.0)
    }

    /// 某个 joint 的最终参数 = 全局默认 + 该 joint 的覆盖。
    ///
    /// 官方是**先** `SetCollisionModelDefaults` 填全局默认、
    /// **后**按 QC 顺序应用 `$jointdamping` 等（`collisionmodel.cpp:579-581`
    /// 与 `658-692`），所以覆盖总是赢。
    ///
    /// `bone` 是**骨骼名**（与 `.phy` 文本段里 solid 的 `"name"` 同一个）。
    pub fn resolve_joint(&self, bone: &str) -> (f32, f32, f32, f32) {
        let mut damping = self.damping.unwrap_or(0.0);
        let mut rot = self.rot_damping.unwrap_or(0.0);
        let mut inertia = self.inertia.unwrap_or(1.0);
        let mut bias = 1.0f32;
        if let Some(o) = self.joint_overrides.iter().find(|o| o.bone == bone) {
            if let Some(v) = o.damping {
                damping = v;
            }
            if let Some(v) = o.rot_damping {
                rot = v;
            }
            if let Some(v) = o.inertia {
                inertia = v;
            }
            if let Some(v) = o.mass_bias {
                bias = v;
            }
        }
        (damping, rot, inertia, bias)
    }

    /// 把 TOML 的 `constraints` 转成 `phy::JointConstraint` 列表。
    ///
    /// # 校验（都**显式报错**，不静默跳过）
    ///
    /// * 轴必须是单个 `x`/`y`/`z` 字母 —— 官方是 `tolower(axis[0]) - 'x'`，
    ///   所以 `"x"`/`"X"`/`"xyz"` 都合法（只看首字母），但空串非法。
    /// * 类型必须是 `free`/`fixed`/`limit` —— 官方对其它值只
    ///   `MdlWarning` 然后**丢弃**该条；mdlc 选择报错，因为静默丢弃
    ///   会让用户以为约束生效了。
    /// * `min <= max` —— 官方 `MdlError("Invalid joint constraint")`
    ///   （`collisionmodel.cpp:1670`）。
    ///
    /// `friction` 省略时官方默认 **`1.0`**（`collisionmodel.cpp:1848-1851`）。
    pub fn joint_constraints(
        &self,
    ) -> Result<Vec<crate::phy::JointConstraint>, crate::phy::ConstraintError> {
        let mut out = Vec::with_capacity(self.constraints.len());
        for (i, c) in self.constraints.iter().enumerate() {
            let axis = c
                .axis
                .chars()
                .next()
                .and_then(crate::phy::JointConstraint::axis_from_char)
                .ok_or_else(|| crate::phy::ConstraintError::BadAxis {
                    index: i,
                    axis: c.axis.clone(),
                })?;
            let kind = crate::phy::JointLimitType::parse(&c.kind).ok_or_else(|| {
                crate::phy::ConstraintError::BadKind {
                    index: i,
                    kind: c.kind.clone(),
                }
            })?;
            if c.min > c.max {
                return Err(crate::phy::ConstraintError::MinGreaterThanMax {
                    index: i,
                    bone: c.bone.clone(),
                    axis: c.axis.clone(),
                    min: c.min,
                    max: c.max,
                });
            }
            out.push(crate::phy::JointConstraint {
                bone: c.bone.clone(),
                axis,
                kind,
                min: c.min,
                max: c.max,
                // 官方省略 friction 时用 "1.0"（落盘 0.2）。
                friction: c.friction.unwrap_or(1.0),
            });
        }
        Ok(out)
    }

    /// 把 TOML 的 `animated_friction` 转成 `phy::AnimatedFriction`。
    ///
    /// 字段名已经是**语义化**的（`time_out` / `time_hold`），
    /// 所以这里只做直通 —— 对调发生在 TOML 的键名设计上，
    /// 而不是在这里做第二次交换（那会把它换回去）。
    pub fn animated_friction(&self) -> Option<crate::phy::AnimatedFriction> {
        self.animated_friction
            .map(|a| crate::phy::AnimatedFriction {
                min: a.min,
                max: a.max,
                time_in: a.time_in,
                time_out: a.time_out,
                time_hold: a.time_hold,
            })
    }
}

impl ModelMeta {}

/// 一条 `$poseparameter`（`mstudioposeparamdesc_t`，20 字节）。
///
/// 字段布局：`sznameindex`(4) `flags`(4) `start`(4) `end`(4) `loop`(4)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PoseParameter {
    /// 参数名（如 `move_yaw`、`body_pitch`）。
    pub name: String,
    /// 取值下限（对应 `mstudioposeparamdesc_t.start`）。
    #[serde(default)]
    pub start: f32,
    /// 取值上限（对应 `end`）。
    #[serde(default)]
    pub end: f32,
    /// 循环模式；不写 = 不循环（`flags` 的 `STUDIO_LOOPING` 位为 0、`loop` 为 0）。
    ///
    /// 对应 QC 的两个关键字（`studiomdl.cpp:476-486`）：
    ///
    /// ```text
    /// $poseparameter "move_yaw" -180 180 wrap        → loop_mode = "wrap"
    /// $poseparameter "lean"      0    1   loop 0.5   → loop_mode = 0.5
    /// $poseparameter "body_pitch" -90 45             → （省略）
    /// ```
    ///
    /// # 为什么用枚举而不是两个独立字段
    ///
    /// `flags & STUDIO_LOOPING` 与 `loop != 0` 在官方产物里**永远同进同退**
    /// （语料 285 条：96 条 `flags=1, loop≠0`，189 条 `flags=0, loop=0`，
    /// 没有交叉）。写成两个独立字段会允许用户造出官方从不产出的组合；
    /// 用枚举让这种状态**在类型上就不可表达**。
    #[serde(default)]
    pub loop_mode: Option<PoseLoop>,
}

/// 姿势参数的循环模式 —— 对应 QC 的 `wrap` / `loop <值>`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PoseLoop {
    /// QC 的 `wrap`：`loop = end - start`（`studiomdl.cpp:479`）。
    ///
    /// TOML 里写字符串 `"wrap"`。
    Wrap(PoseWrap),
    /// QC 的 `loop <值>`：显式给循环范围（`studiomdl.cpp:485`）。
    ///
    /// TOML 里直接写数字，如 `loop_mode = 0.5`。
    Explicit(f32),
}

/// `PoseLoop::Wrap` 的标记类型 —— 让它能匹配 TOML 字符串 `"wrap"`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PoseWrap {
    Wrap,
}

impl PoseLoop {
    /// 算出 `mstudioposeparamdesc_t.loop` 的值。
    pub fn loop_value(self, start: f32, end: f32) -> f32 {
        match self {
            PoseLoop::Wrap(_) => end - start,
            PoseLoop::Explicit(v) => v,
        }
    }
}

/// `[materials]` 段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Materials {
    /// 材质搜索目录（QC 的 `$cdmaterials`）。
    #[serde(default)]
    pub search_paths: Vec<String>,
    /// `$texturegroup`：skin 家族表。
    ///
    /// 每个内层 `Vec<i32>` 是一个 family，元素是「材质槽位 → texture 下标」
    /// 的重映射；`family[0]` 通常就是恒等映射 `[0,1,2,…]`。
    ///
    /// # 语义（`Cmd_TextureGroup`，`studiomdl.cpp:4741`）
    ///
    /// QC 的写法是**按组逐层**列出纹理名：
    ///
    /// ```text
    /// $texturegroup "skinfamilies"
    /// {
    ///     { "body_a" "body_b" }
    ///     { "skin1"  "skin2"  }
    /// }
    /// ```
    ///
    /// 大括号的**每一层**是一个 family（`group` 从 0 起），
    /// 最终 `g_skinref[i][g_texturegroup[0][0][j]] = g_texturegroup[0][i][j]`
    /// —— 即**用 family 0 的第 j 项作为槽位下标**，把第 i 个 family 的第 j 项
    /// 写进去（`studiomdl.cpp:670-676`）。
    ///
    /// # 为什么这里直接存「下标数组」而不是名字
    ///
    /// TOML 里写名字需要额外解析 + 与 `[materials].textures` 做名字匹配，
    /// 而 `[materials].textures` 的顺序**就是** texture 下标。
    /// 直接写下标更明确、也不会有「名字拼错」这种静默失败。
    ///
    /// # 实测语料分布
    ///
    /// `numskinfamilies`：1 → 3142、2 → 96、4 → 40、5 → 28、8 → 12、
    /// 3 → 10、32 → 2、12 → 2。空数组 = 单 family 恒等映射。
    ///
    /// 典型多 family 样本（`common_female_tanktop_jeans.mdl`，12 个材质）：
    ///
    /// ```text
    /// family[0] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    /// family[1] = [3, 4, 5, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    /// family[2] = [6, 7, 8, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    /// ```
    ///
    /// 即只有前 3 个槽位（衣服）在变，其余（头/手/腿）保持恒等。
    #[serde(default)]
    pub skin_families: Vec<Vec<i32>>,
    /// 材质表。mesh 通过下标引用这里。
    #[serde(default)]
    pub textures: Vec<Texture>,
}

/// 一个材质。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Texture {
    /// 材质名，相对 `search_paths` 之一，如 `models/props/canister01a`。
    pub name: String,
    #[serde(default)]
    pub flags: Option<i32>,
}

/// 一根骨骼（`mstudiobone_t`）。
///
/// `position` / `rotation` 可省略 —— 省略时**自动取自 SMD 的 skeleton
/// 第 0 帧**（真实模型几十根骨骼，手抄姿态不现实）。显式给出则覆盖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bone {
    pub name: String,
    /// 父骨骼名。**用名字而非下标** —— 下标在增删骨骼时会失效，
    /// 而名字不会；写文件时才解析成下标。
    #[serde(default)]
    pub parent: Option<String>,
    /// 参考姿态位置。省略则取 SMD。
    #[serde(default)]
    pub position: Option<[f32; 3]>,
    /// 参考姿态旋转（**角度**，不是弧度 —— 与 QC 一致，写出时转成弧度）。
    /// 省略则取 SMD（SMD 里是弧度，会原样搬运）。
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    #[serde(default)]
    pub flags: Option<i32>,
    /// 该骨骼的 surfaceprop（QC 的 `$jointsurfaceprop`）。
    #[serde(default)]
    pub surface_prop: Option<String>,
    /// `$bonemerge`：该骨骼允许被 bone merge。
    ///
    /// 放在骨骼上而不是顶层数组，是为了避开 TOML 的一个陷阱：
    /// **顶层键必须写在所有 `[[表]]` 之前**，否则会被解析成上一个表的字段。
    /// 真实 QC 里 `$bonemerge` 出现 167 次，survivor 模型全靠它。
    #[serde(default)]
    pub bonemerge: bool,
    /// `$definebone` / `$importbone` 的 **`bPreAligned`** 标志。
    ///
    /// 置位的骨骼被 `RealignBones` **整个跳过**（`simplify.cpp:4299`）：
    /// `pos`/`rot` 原样保留，`srcRealign` 保持 `$definebone` 给的值
    /// （不写就是单位阵）。
    ///
    /// # 为什么省略时按「有没有显式姿态」推断
    ///
    /// 官方**唯一**能指定骨骼参考姿态的命令就是 `$definebone`，而它
    /// **总是**置 `bPreAligned`。实测（`docs/_probe`）：
    ///
    /// | 受控实验 | 写法 | 结果 |
    /// |---|---|---|
    /// | `ipq1` | 不写 `$definebone` + `$realignbones` | **重排**（`a.pos=[10,0,0]`） |
    /// | `ipq2` | 三根都写 **6** 个数字 + `$realignbones` | **一根都不重排** |
    /// | `ipq3` | 三根都写 **12** 个数字 + `$realignbones` | **一根都不重排** |
    /// | `ipq6` | 写 `$ikchain`（另一条填 `childbone[]` 的路径） | 同样被跳过 |
    /// | `ipq4` | `$definebone` 给 `[5,6,7]`/`(10,20,30)` | 文件里就是这两个值 |
    ///
    /// 于是「TOML 写了 `position`/`rotation`」⇔「QC 写了 `$definebone`」
    /// ⇔ `bPreAligned = true`。
    ///
    /// 省略（`None`）时按上述规则**推断**；显式 `true`/`false` 可覆盖。
    #[serde(default)]
    pub pre_aligned: Option<bool>,
    /// `$definebone` **后 6 个数字**的平移部分 —— `srcRealign` 的平移列。
    ///
    /// 只有 `$definebone` 写满 12 个数字时才存在；6 数字形式下
    /// `srcRealign` 是**单位阵**（`studiomdl.cpp:5949`）。
    /// 实测 `ipq5`：给 `1 2 3 10 20 30`，`MatrixAngles(srcRealign)`
    /// dump 回来**逐位相同**。
    #[serde(default)]
    pub realign_position: Option<[f32; 3]>,
    /// `$definebone` 后 6 个数字的旋转部分，**角度**，顺序与
    /// [`Self::rotation`] 一致（即 `RadianEuler` 的 `[roll, pitch, yaw]`）。
    ///
    /// > QC 的 `$definebone` 参数顺序是 `<x> <y> <z> <pitch> <yaw> <roll>`，
    /// > 所以 `pitch = rot[1]`、`yaw = rot[2]`、`roll = rot[0]`。
    /// > 实测 `ipq4`：QC 给 `(10,20,30)` → 骨骼表 `rot=[30°,10°,20°]`。
    #[serde(default)]
    pub realign_rotation: Option<[f32; 3]>,
}

impl Bone {
    /// 该骨骼是否被 `RealignBones` 跳过（`simplify.cpp:4299`）。
    ///
    /// 省略 `pre_aligned` 时按「是否写了显式参考姿态」推断 ——
    /// 官方唯一能写参考姿态的命令 `$definebone` 总是置该标志。
    pub fn is_pre_aligned(&self) -> bool {
        self.pre_aligned
            .unwrap_or(self.position.is_some() || self.rotation.is_some())
    }

    /// 显式给出的 `srcRealign`（`$definebone` 的后 6 个数字）。
    ///
    /// `None` = 官方在该情形下写**单位阵**（6 数字形式，或根本没有
    /// `$definebone`）。
    pub fn explicit_src_realign(&self) -> Option<crate::bone_math::Matrix3x4> {
        if self.realign_position.is_none() && self.realign_rotation.is_none() {
            return None;
        }
        let pos = self.realign_position.unwrap_or([0.0; 3]);
        let deg = self.realign_rotation.unwrap_or([0.0; 3]);
        Some(crate::bone_math::local_transform(
            pos,
            [
                deg[0].to_radians(),
                deg[1].to_radians(),
                deg[2].to_radians(),
            ],
        ))
    }
}

/// 一个 body part（`mstudiobodyparts_t`）。
///
/// 每个 model 引用一个 SMD；SMD 里的材质名决定 mesh 的划分
/// （同一材质名的三角形归入同一个 mesh），与 QC 一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyPart {
    pub name: String,
    /// bodygroup 预设的权重基数（`$bodygroup` 的 `studio`/`blank` 编码）。
    #[serde(default)]
    pub base: Option<i32>,
    pub models: Vec<BodyModel>,
}

/// body part 里的一个 model（`mstudiomodel_t`）。
///
/// **网格来自 SMD 文件**，不写在描述里 —— 真实模型有几万到几十万顶点，
/// 内联会让描述文件膨胀到几百 MB 且无法用文本工具处理。
/// 这与 QC 的 `$bodygroup { studio "x.smd" }` 设计一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyModel {
    /// Uniform source scale captured from `$scale` or the studio `scale` option.
    #[serde(default)]
    pub input_scale: Option<f32>,
    /// 源 SMD 路径，**相对描述文件所在目录**（也接受绝对路径）。
    /// 也是 **LOD 0**（最精细）的网格。
    pub smd: String,
    /// 该 model 的名字（内联 `char[64]`）。
    ///
    /// 实测 studiomdl 写的是**源 SMD 的文件名**（如 `myprop-ref.smd`）；
    /// 不写则自动取 `smd` 的文件名部分，与 studiomdl 行为一致。
    #[serde(default)]
    pub name: Option<String>,
    /// **是否翻转三角形绕序**（`flip_triangles`）。
    ///
    /// # 默认是 `true` —— 官方**默认翻转**
    ///
    /// `studiomdl.cpp:6893`（`ResetModel`）：
    /// ```c
    /// flip_triangles = 1;                    // ← 全局默认**开**
    /// ```
    /// `Cmd_Body` / `Cmd_Studio`（`:926` / `:7040`）在**每条** `$body`/`$studio`
    /// 开头都重置成 `1`，只有显式写了 `reverse` 才置 `0`（`:935`）。
    ///
    /// 翻转发生在 `v1support.cpp:192-196`（`Grab_UpdateFace`）：
    /// ```c
    /// if (flip_triangles) { j = pFace->b; pFace->b = pFace->c; pFace->c = j; }
    /// ```
    /// 即**交换第 2、3 个顶点**（`a` 不动）。
    ///
    /// # 为什么必须有这一位（这是「Blender 里面法向反了」的根因）
    ///
    /// Source 引擎的**正面是 CW**（顺时针）—— 与 Blender/OpenGL 的 CCW 约定
    /// **相反**。官方在**导入时**就把绕序翻过来，所以产物里存的是 CW；
    /// mdlc 原先原样透传 SMD 顺序（CCW）⟹ **每个三角形都反向**
    /// ⟹ 逐顶点法线仍然正确、但**面法向整体朝内**。
    ///
    /// 判据（`docs/_probe/probe_winding_order.js`，6 个 `vm_test_group` 模型）：
    ///
    /// | | 与 SMD 原始顺序「同序」 | 「逆序」 |
    /// |---|---|---|
    /// | 官方 | **0** | 12996（逐三角形精确反转） |
    /// | mdlc（修前） | 6905 | **0** |
    ///
    /// 另：官方语料 **396/398** 个模型的「几何面法线 vs 顶点法线」是**反向**
    /// ⟹ 这就是 Source 的正确约定，不是数据错。
    ///
    /// ⚠️ **TOML 侧默认也是 `true`**（与 QC 一致）。要复刻「SMD 本身已是
    /// CW」的老资产才需要设 `false`。
    #[serde(default = "default_true")]
    pub flip_triangles: bool,
    /// **LOD 1..N 的网格**（QC 的 `$lod <距离> replacemodel <lodN.smd> <lod0.smd>`）。
    ///
    /// 顺序即 LOD 顺序：`lods[0]` 是 LOD 1，`lods[1]` 是 LOD 2……
    /// 最精细的 LOD 0 由 [`Self::smd`] 给出。
    ///
    /// # 语义（实测自 230 个真实多 LOD 模型）
    ///
    /// - **每个 LOD 是一个完整独立的 SMD**，不是「删掉一些三角形」。
    ///   studiomdl 用 `UnifyLODs` 把它们合并成一个统一顶点池，
    ///   跨 LOD 精确去重后按 LOD 归属排序，再写 fixup 表。
    /// - 各 LOD 的**材质名必须一致**（同一个 mesh 在各 LOD 里都得存在），
    ///   否则 mesh 划分对不上，`mstudiomesh_t` 的对应关系会错位。
    /// - 每个 LOD 的顶点数应当**单调不增**（LOD 越高越粗），
    ///   实测 230/230 个模型都满足。不满足不会报错，但会让 LOD 切换
    ///   出现「越切越精细」的反直觉行为。
    #[serde(default)]
    pub lods: Vec<LodModel>,
    /// 该 model 的 eyeball 列表（QC 的 `$model { … eyeball … }`）。
    ///
    /// `mstudiomodel_t.eyeballindex` 指向**该 model 自己的 eyeball 数组**，
    /// 紧跟该 model 的 mesh 数组之后（`eyeballindex == meshindex + nummeshes*116`，
    /// 语料 3620/3620）。被引用的 mesh 会打标 `materialtype=1`/`materialparam=j`。
    #[serde(default)]
    pub eyeballs: Vec<Eyeball>,
    /// 该 model 的 **VTA 顶点动画形状**（QC 的 `$model { flexfile … flex … }`）。
    ///
    /// 每条把**一个 `.vta` 文件的一帧**绑定成一个具名形状（flexdesc），
    /// 并产出 `mstudioflex_t` + `mstudiovertanim_t` 载荷。
    ///
    /// 顺序即 flexdesc 的注册顺序，**很重要** —— 落盘的
    /// `mstudioflex_t.flexdesc` 就是这个顺序里的下标。
    #[serde(default)]
    pub flexes: Vec<Flex>,
    /// `srcpart "name"`：**只取这些名字的网格**（可重复）。
    ///
    /// 空 = 取源文件里全部网格并合并 —— 那是官方的行为
    /// （`docs/fbx-support.md` §1.10：一个 FBX 里的多块网格被合并进同一个
    /// `mstudiomodel_t`，部件边界丢失）。
    ///
    /// 名字是 **FBX 里 mesh 节点的名字**（`node.element.name`），不是
    /// `$body` 给的名字。名字对不上时 `src\fbx.rs` 会报
    /// 「{} 里没有任何三角形（网格）」并提示核对 `srcpart`。
    #[serde(default)]
    pub src_parts: Vec<String>,
    /// `srcmaterial "name"`：网格**没有**材质时用这个名字。
    ///
    /// `None` = 官方的兜底 `debug/debugempty`（`src\fbx.rs` 的
    /// [`crate::fbx::FALLBACK_MATERIAL`]）。
    #[serde(default)]
    pub src_material: Option<String>,
    /// `srcscale <f>`：顶点与骨骼位移的缩放。`None` = `1.0`（官方不做单位换算）。
    ///
    /// ⚠️ 官方**不**按 FBX 的 `unit_meters` 换算 —— `rig.fbx` 的
    /// `Skeleton` 局部缩放 `(100,100,100)` 会进骨骼表（`docs/fbx-support.md` §1.7）。
    /// `srcscale` 是给「源文件单位真的错了」准备的逃生门，不是常规选项。
    ///
    /// ⚠️ 它**不是**用来对齐两套导入器口径的：`srcscale` 是**整体缩放**
    /// （顶点与骨骼同乘，比值不变）⟹ 补偿不了「骨骼比网格大 100 倍」那个
    /// 不自洽。后者已由 `crate::fbx::bone_offset` 修掉（§4.6 偏离 12）。
    #[serde(default)]
    pub src_scale: Option<f32>,
    /// `srcaxis "y"` / `"z"`：**强制**上轴。`None` = 不干预（官方原样透传）。
    ///
    /// ⚠️ 只在源文件自身轴向坏掉（根变换丢了）时才需要 —— 只要根节点变换与
    /// 几何自洽，官方与 mdlc 的默认行为就已经正确，无论 Y-up 还是 Z-up
    /// （`docs/fbx-support.md` §1.7b）。
    #[serde(default)]
    pub src_axis: Option<String>,
    /// `srcshapekey "name"`：**只取这些名字的形变目标**（可重复）。
    ///
    /// 空 = 取全部，帧号 = 文件顺序 + 1（官方行为，`docs/fbx-support.md` §1.6b）。
    /// 写了之后帧号按**本列表的顺序** 1..N。
    #[serde(default)]
    pub src_shape_keys: Vec<String>,
    /// `srcshapekeyorder "a" "b"`：显式指定帧号顺序。
    ///
    /// 与 [`Self::src_shape_keys`] 的区别：那个是**过滤 + 定序**（列表之外的
    /// 一律丢掉），这个是**只定序**（列表之外的按文件顺序排在其后）。
    #[serde(default)]
    pub src_shape_key_order: Vec<String>,
    /// `srcshapekeyignore`：**完全忽略形变目标**（不产出任何 flex）。
    #[serde(default)]
    pub src_shape_key_ignore: bool,
}

/// 一条 VTA 形状绑定（QC 的 `flexfile "<vta>" flex "<名>" frame <n>`）。
///
/// # 与 QC 的对应
///
/// QC 把这件事拆成两条语句，因为 `flexfile` 设的是一个**粘性**变量：
///
/// ```text
/// $model body "ref.smd" {
///     flexfile "cloth" flex "AU1" frame 1     ← 设定 .vta + 绑定第 1 帧
///     flex "AU2" frame 2                      ← 复用同一个 .vta 的第 2 帧
/// }
/// ```
///
/// 本结构把两者合成一条 —— 每条自带 `vta`，因此**不需要**粘性变量，
/// 也就不存在「`flexfile` 必须在前」那个反直觉的顺序问题
/// （机制见 HANDBOOK 38.3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flex {
    /// `.vta` 路径，**相对描述文件所在目录**（也接受绝对路径）。
    pub vta: String,
    /// flexdesc 名（`<名>`）。`pair = true` 时实际注册的是
    /// **`<名>R` 与 `<名>L`** 两个 desc（见 [`Self::pair`]）。
    pub name: String,
    /// 取 `.vta` 的**相对帧号**（`time (start_frame + frame)`）。
    ///
    /// ⛔ **必须 > 0。** `simplify.cpp:2453-2457` 有一条特殊规则：
    ///
    /// ```c
    /// // frame 0 is special.  Always assume zero vertex animations
    /// if (g_flexkey[i].frame == 0) numsrcanims = 0;
    /// ```
    ///
    /// 即 `frame 0` 的载荷**恒为空**，产物里 `mesh.numflexes` 会是 0 ——
    /// 而 VTA 依然被正常载入、flexdesc 名字也正确，症状与
    /// 「这个特性没实现」完全一样。这是整个 VTA 里最坑的一条。
    ///
    /// 缺省 `1`（**故意不是 0**）—— 让「不写 frame」也能出载荷。
    #[serde(default = "default_flex_frame")]
    pub frame: i32,
    /// 是否生成 **R/L 两个** desc（QC 的 `flexpair`）。
    ///
    /// `true` ⟹ 注册 `<名>R` 与 `<名>L`（**先 R 后 L**，`flexpair` 字段
    /// 指向 `L`），并启用 `side` 通道做左右分配；`false` ⟹ 只注册 `<名>`。
    ///
    /// # 为什么是 bool 而不是「split 值」
    ///
    /// QC 的 `flexpair "<名>" <s>` 用一个 `<s>` 同时表达两件事，容易让人
    /// 以为它们是一个参数。但受控实验（`gen_vta_pairsplit.js`）证明
    /// **两者正交**：`flexpair "vanim" 1 frame 1 split 0` 产出
    /// **`numflexdesc=2` 但 `side` 全 0** —— 即「生成几个 desc」与
    /// 「smoothstep 分割点」互不影响。所以拆成 `pair` + [`Self::split`]
    /// 是**更准确**的表达，也比 QC 更有表达力。
    #[serde(default)]
    pub pair: bool,
    /// smoothstep 分割点（QC 的 `split`）。`0.0` = 不分割。
    ///
    /// 语义（`simplify.cpp:2477-2510`）：设顶点 X 坐标，
    /// `x < -split` ⟹ `scale = 1`；`x > split` ⟹ `scale = 0`；
    /// 其间 `t = (split - x) / (2*split)`、`scale = 3t² - 2t³`。
    /// `scale == 0` 的顶点会被**丢弃**（不写 vertanim）。
    ///
    /// **符号有意义**：`split < 0` 时左右**镜像**（实测 `side` 通道
    /// 从 255 递减到 0）。
    #[serde(default)]
    pub split: f32,
    /// flexcontroller 初值（QC 的 `position`）。落进 `mstudioflex_t.target1`。
    ///
    /// QC 缺省 `1.0`（`studiomdl.cpp:3586`），**不是** 0。
    #[serde(default = "default_flex_position")]
    pub position: f32,
    /// 衰减（QC 的 `decay`）。只影响每条 vertanim 的 `speed` 通道
    /// （`simplify.cpp:2589-2599`）：`decay == 0` ⟹ `speed = 1.0`（即 255）；
    /// 否则 `speed = clamp(|delta| / (max|delta| * decay), 0, 1)`。
    ///
    /// QC 缺省 `1.0`（`studiomdl.cpp:3591`）。
    #[serde(default = "default_flex_position")]
    pub decay: f32,
    /// 显式的 `target0..3`（`mstudioflex_t` 的四个 `target`）。
    ///
    /// `None` ⟹ 用缺省的 `[0.0, position, 10.0, 11.0]`（普通 `flex`）。
    /// `Some` ⟹ 用给定值 —— 官方 `Option_Eyelid` 生成的三条 flexkey
    /// **各有各的分段 targets**，不是缺省那组。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<[f32; 4]>,
    /// 该 flex 由官方 `eyelid` 生成（`Option_Eyelid`，`studiomdl.cpp:3645-3802`）。
    ///
    /// 唯一作用是**豁免 `frame 0` 的硬报错**：官方 eyelid 的
    /// `neutral 0` 会注册一条 frame 0 的 flexkey，语义是「载荷清零」
    /// （`simplify.cpp:2453-2457` 的 `if (g_flexkey[i].frame == 0) numsrcanims = 0;`），
    /// **不是错误**。手写的 `flex "x" frame 0` 仍然报错 —— 那几乎总是笔误。
    #[serde(skip)]
    pub from_eyelid: bool,
}

/// `Flex::frame` 的缺省值 —— **1，不是 0**（见 [`Flex::frame`] 的说明）。
pub fn default_flex_frame() -> i32 {
    1
}

/// `Flex::position` / `Flex::decay` 的缺省值（QC 均为 `1.0`）。
pub fn default_flex_position() -> f32 {
    1.0
}

/// `serde` 的 `default = "..."` 用：布尔字段缺省为 **`true`**。
///
/// 目前只有 [`ModelMeta::split_oversized_meshes`] 用它 —— 那个开关
/// 默认打开（见该字段的说明）。
pub fn default_true() -> bool {
    true
}

/// 一个 LOD 档（QC 的 `$lod <阈值> { … }` 块）。
///
/// # 两种机制（**不是同一种**）
///
/// | QC | TOML | 机制 |
/// |---|---|---|
/// | `replacemodel <lodN.smd> <lod0.smd>` | `smd = "lodN.smd"` | **换一整份网格** |
/// | `bonetreecollapse` / `replacebone` | `bone_tree_collapse` / `replace_bone` | **同一份网格 + 骨骼坍缩** |
///
/// 两者可以同时用（官方 `GetLODSources` 在没有 `replacemodel` 时
/// 回退到 LOD 0 的源）。**只写骨骼选项时不写 `smd`**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LodModel {
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub removed: bool,
    #[serde(default)]
    pub removed_materials: Vec<String>,
    #[serde(default)]
    pub material_replacements: Vec<[String; 2]>,
    /// 该 LOD 的 SMD 路径（QC 的 `replacemodel`），**相对描述文件所在目录**。
    ///
    /// **可省略** —— 省略时本档复用 LOD 0 的网格，只应用骨骼选项
    /// （官方 `GetLODSources`：`if (!pSource && !found) pSource = pSrcModel->source;`）。
    #[serde(default)]
    pub smd: Option<String>,
    /// 切换到该 LOD 的屏幕高度阈值（`ModelLODHeader_t.switchPoint`）。
    ///
    /// 实测 230 个真实模型的取值：`0`（LOD 0 恒为 0）、`10/15/20/25/30/40/
    /// 50/60/65/80/100/150/200`，以及 **`-1`**（表示「不切换」，
    /// 出现在 `hgibs` 这类最后一个 LOD 与上一个相同的情况）。
    ///
    /// 不写则自动推算：LOD 0 写 0，其余按 `20 * 2^(n-1)` 递减
    /// （20/40/80/…）。**这只是合理缺省，不是 studiomdl 的算法** ——
    /// studiomdl 的 `switchPoint` 来自 QC 里显式写的距离。
    #[serde(default)]
    pub switch_point: Option<f32>,
    /// `bonetreecollapse` 的骨骼名列表（QC 同名命令）。
    ///
    /// # 语义（实测，见 HANDBOOK 第 41 节）
    ///
    /// 把**该骨骼的全部后代**重定向到它自己 ——
    /// 但**该骨骼本身不被替换**。所以：
    ///
    /// - 作用在**叶子**上是 **no-op**（受控实验 `lodcbc`：
    ///   `numLODVertexes=[9,9]`，与空 `$lod {}` 块完全相同）；
    /// - 作用在有子节点的骨骼上才会改写（`lodcbm`：`[12,9]`）。
    ///
    /// ⚠️ **它不会让顶点数下降**（除非重映射恰好让顶点变得相同）。
    /// 官方 `survivor_teenangst` 的 `numLODVertexes` 逐级下降来自
    /// **LOD 1/2/3 各有一份独立 SMD**（`replacemodel`），不是这个。
    #[serde(default)]
    pub bone_tree_collapse: Vec<String>,
    /// `replacebone` 的 `[源骨骼, 目标骨骼]` 列表（QC 同名命令）。
    ///
    /// 比 `bonetreecollapse` 更通用：可以只重定向**单根**骨骼，
    /// 也可以把骨骼接到**非祖先**上。
    ///
    /// 链会被**自动折叠到末端**（官方 `FixupReplacedBones`）：
    /// `A→B`、`B→C` 等价于 `A→C`、`B→C`。
    #[serde(default)]
    pub replace_bone: Vec<[String; 2]>,
    /// `nofacial`：本档禁用面部动画（QC 同名命令）。
    ///
    /// # 产物级判据
    ///
    /// 官方把 `triangleIsFlexed` 强制为 `false`（`optimize.cpp:1311`），
    /// 于是**所有三角形落进同一个 strip group** ⟹
    /// **`numStripGroups` 从 2 降到 1**。
    ///
    /// 实测 `survivor_TeenAngst.dx90.vtx`：lod0/1/2 都有 4 个
    /// `sg=2,flags=0` 的 mesh，**lod3（正是 `nofacial` 那档）全部变成 `sg=1`**。
    ///
    /// ⚠️ **只在模型真有 flex 载荷时才有可观测差异** ——
    /// 没有 VTA 的模型本来就只有一个 strip group。
    #[serde(default)]
    pub no_facial: bool,
}

/// `[hitboxes]` 段：hitbox set 与其中的 hitbox。
///
/// 实测真实 L4D2 模型 **100% 都有** hitbox set（默认名为 `default`），
/// 没有它命中判定失效（子弹/近战打不中）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Hitboxes {
    /// hitbox set 的名字，默认 `default`（与 studiomdl 一致）。
    #[serde(default)]
    pub set_name: Option<String>,
    /// 单个 hitbox。
    #[serde(default)]
    pub boxes: Vec<Hitbox>,
    /// 该 set 是**自动生成**的（用户没写 `$hbox`），由 `compile()` 填。
    ///
    /// # 为什么需要这个字段
    ///
    /// `SetupHitBoxes()`（`simplify.cpp:6884`）在 `g_hitboxsets.Size() == 0` 时
    /// **总是**建一个名为 `default` 的 set，并置
    /// `gflags |= STUDIOHDR_FLAGS_AUTOGENERATED_HITBOX`（**0x1**）。
    /// 之后才做「三轴厚度都 > 1」的过滤（`simplify.cpp:6950`）——
    /// 所以**即使一个 box 都没通过**，set 与标志位**依然存在**。
    ///
    /// 实测语料：166 个模型的 hitbox set 存在但 `numhitboxes == 0`，
    /// 且这 166 个**全部**带 `0x1` 标志（`probe_hbox_empty_set.js`）。
    ///
    /// 因此不能用 `boxes.is_empty()` 来判断「要不要写 set」——
    /// 那样会漏掉这 166 个形态。
    #[serde(default)]
    pub autogenerated: bool,
}

/// 一个 hitbox（`mstudiobbox_t`，68 字节）。
///
/// `bone` 用**骨骼名**（写文件时解析成下标）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hitbox {
    /// 绑定的骨骼名。
    pub bone: String,
    /// 相交分组（同组的 hitbox 不互相检测）。
    #[serde(default)]
    pub group: Option<i32>,
    /// 包围盒（**骨骼空间**）。
    pub bbmin: [f32; 3],
    pub bbmax: [f32; 3],
    /// hitbox 名（`$hbox` 的第 3 个参数）。
    #[serde(default)]
    pub name: Option<String>,
}

/// 一个附着点（`mstudioattachment_t`，92 字节）。
///
/// 实测 L4D2 全部武器模型依赖它（挂枪口火焰、弹壳抛出点等）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    /// 附着点名字（如 `muzzle`）。
    pub name: String,
    /// 绑定的骨骼名。
    pub bone: String,
    /// 相对骨骼的位置。
    #[serde(default)]
    pub position: Option<[f32; 3]>,
    /// 相对骨骼的旋转（**角度**）。
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// `absolute`：官方 `IS_ABSOLUTE`（`studiomdl.h:277`）。
    ///
    /// 它做两件事：① 官方在**解析期**就把 `local` 的旋转覆盖成
    /// `AngleIMatrix( g_defaultrotation )`（`studiomdl.cpp:5246`）；
    /// ② `LinkAttachments()` 里把 `local` 左乘附着点骨骼的 `poseToBone`
    /// （`simplify.cpp:5379-5388`）⟹ 写出的 `local` 描述的是**世界坐标**下的
    /// 绝对位姿，与骨骼姿态无关。
    ///
    /// ⚠️ 该位**不落盘**（官方 `write.cpp:350` 只写 `flags`）。
    #[serde(default)]
    pub absolute: bool,
    /// `absolute` 是否**覆盖**了 `local` 的旋转。
    ///
    /// 官方 `Cmd_Attachment` 的选项循环是「**最后写入者胜**」：
    /// `absolute` 写 `AngleIMatrix( g_defaultrotation )`（`studiomdl.cpp:5246`），
    /// `rotate` 写 `AngleMatrix( angles )`（`:5268`）。所以
    /// `... 0 0 0 absolute` 的旋转来自前者，而 `... 0 0 0 absolute rotate 1 2 3`
    /// 来自后者 —— **两种情形的 `IS_ABSOLUTE` 都置位**（第 ② 步照做），
    /// 只有旋转来源不同。官方用同一个 `local` 矩阵自然表达了这点，
    /// mdlc 把旋转拆成了 `rotation` 欧拉角，所以需要这一个额外的 bit。
    ///
    /// `None`（缺省）⟹ **跟随 [`Self::absolute`]** —— 这是 TOML 作者只写
    /// `absolute = true` 时想要的语义（也是官方最常见的写法）。
    #[serde(default)]
    pub absolute_rotation: Option<bool>,
    /// `rigid`：官方 `IS_RIGID`（`studiomdl.h:278`）。
    ///
    /// 同样**不落盘**，只影响骨骼保活：`TagUsedBones()` 对 rigid 附着点会沿父链
    /// 上溯到第一根被顶点引用的骨骼（`simplify.cpp:3472-3484`）。
    #[serde(default)]
    pub rigid: bool,
    /// 直接写进 `mstudioattachment_t.flags` 的位。
    ///
    /// 官方这里**只**可能落 `ATTACHMENT_FLAG_WORLD_ALIGN`（`0x10000`，
    /// `studiomdl.cpp:5255` + `write.cpp:350`）—— `absolute`/`rigid` 走的是另一个
    /// 字段 `type`，不写进产物。实测（`docs/_probe/oracle_attachment_flags.js`）：
    /// 官方对 `absolute`/`rigid` 都写 `0x0`，只有 `world_align` 写 `0x10000`。
    #[serde(default)]
    pub flags: Option<i32>,
    /// 是否是 `$illumposition x y z <骨骼>` **合成**出来的那个附着点。
    ///
    /// 官方在 4 参数形式下新建一个名为 `__illumPosition` 的附着点：绑该骨骼、
    /// 零旋转、平移就是那三个坐标、`type` 带 `IS_RIGID`，并让
    /// `studiohdr2.illumpositionattachmentindex`（`+0x08`）指向它（**1 起**下标）。
    /// 实测见 `docs/_probe/oracle_illumposition3.js` … `oracle_illumposition10.js`。
    ///
    /// 与普通附着点的三处不同：
    ///
    /// ① 官方**允许重名** —— 写两次 `$illumposition` 就有两个 `__illumPosition`
    ///    （`oracle_illumposition5.js` 的 `illum_twice`），所以它不参与
    ///    [`ModelDesc::validate`] 的重名检查；
    /// ② 它绑的骨骼**可能被收骨判据丢掉** —— `IS_RIGID` 让 `TagUsedBones()` 沿父链
    ///    上溯找第一根有顶点权重的骨骼（`simplify.cpp:3472-3484`），上溯无果就
    ///    什么都不标，该骨骼随后被丢、再由 `MapSourcesToGlobalBonetable()` 静默
    ///    重映射到骨骼 0（`simplify.cpp:4180`）。实测 `oracle_illumposition8/10.js`：
    ///    `$illumposition 0 0 0 b2`（b2 零引用）⟹ `numbones` 少一根、附着点落 bone 0。
    ///    写出侧靠 [`Attachment::resolved`] 承载这个结果；
    /// ③ 它是 `#[serde(skip)]` 的 —— TOML 表达不了 4 参数形式，所以 TOML 侧
    ///    永远走 3 参数的轴变换语义（见 [`crate::mdl_writer`] 的 illum 分支）。
    #[serde(skip)]
    pub synthetic: bool,
    /// `LinkAttachments()` **阶段 2** 的写出侧覆盖值：`(全局骨骼下标, matrix3x4_t)`。
    ///
    /// 官方 `LinkAttachments()`（`simplify.cpp:5313-5401`）分两阶段解析附着点的骨骼：
    ///
    /// - **阶段 1**（`:5324-5339`）：骨骼**直命中**全局骨骼表 ⟹ `bone = k`，而
    ///   `:5385` 左乘的 `boneToPose` 与 `:5388` 左乘的 `poseToBone` 恰好互为逆
    ///   （`:5334-5335` 就是同一个矩阵求逆），`local` **原样落盘**。
    /// - **阶段 2**（`:5341-5376`）：没直命中 ⟹ 到每个 source 的 `localBone[]` 里按
    ///   `stricmp` 找，找到后**沿父链上溯**到第一根直命中全局骨骼表的祖先：
    ///   `while (k != -1 && g_source[j]->boneGlobalToLocal[g_source[j]->boneLocalToGlobal[k]] != k) k = g_source[j]->localBone[k].parent;`
    ///   （判据的来历：`MapSourcesToGlobalBonetable()` 只在**直命中**时置
    ///   `boneGlobalToLocal`，被丢的骨骼保持 `-1` ⟹ 判据恒真，`simplify.cpp:4186-4215`）。
    ///   然后 `bone = boneLocalToGlobal[上溯后]`，并把 `local` 重算成
    ///   `poseToBone(上溯后)⁻¹ ∘ boneToPose(原始) ∘ local` —— 注意 `:5365` 求逆用的是
    ///   **上溯后**的 `boneToPose`，而 `:5385` 左乘用的仍是**原始**骨骼的 `boneToPose`
    ///   （局部变量 `k` 被改了，`boneToPose` 没有）。
    ///
    /// 上溯到 `k == -1`（整条链都没直命中）⟹
    /// `MdlError( "unable to find valid bone for attachment %s:%s\n" )`（`:5360-5362`），
    /// 由 [`crate::qc::parse`] 的 `finish` 在解析期报出。
    ///
    /// `boneToPose` 是**该 source 第 0 帧**的局部姿态沿父链累积
    /// （`Build_Reference()`，`studiomdl.cpp:728-762`）—— 注意 `simplify.cpp:5350`
    /// 取的是 `g_source[j]->boneToPose[k]` 而**不是**全局骨骼表的
    /// `g_bonetable[k].boneToPose`，所以这个值只能在解析期算好。
    ///
    /// # 本字段的存法
    ///
    /// 存 `(上溯后的全局骨骼下标, 修正矩阵)`，其中
    /// `修正 = inverse(source 第 0 帧 world[上溯后]) ∘ source 第 0 帧 world[原始]`。
    /// 写出侧把它左乘到 `crate::compile::attachment_local_matrix` 的结果上
    /// （那条路径已经处理了 `$staticprop` 的 `ConcatTransforms`，所以修正放在外面）。
    ///
    /// 实测 `docs/_probe/diff_illumposition.js` 的 `illum_drop` 变体（夹具父链 `i-1`，
    /// b2 零引用被丢）：官方 `bone=1 local=[1,0,0,0,0,1,0,0,0,0,1,20]` —— 上溯到 b1、
    /// 平移 30−10 = 20。`oracle_illumposition8/10.js` 的夹具里非根骨骼的父都是 b0，
    /// 所以那里落 `bone=0`、平移 20−0 = 20，**同一条规则**。
    ///
    /// `None` ⟹ 阶段 1（骨骼直命中全局骨骼表）或普通附着点，走
    /// `crate::compile::attachment_local_matrix` 原样落盘。
    #[serde(skip)]
    pub resolved: Option<(usize, [f32; 12])>,
}

// ---------------------------------------------------------------------------
// 以下类型**不是** TOML 的一部分 —— 它们是 SMD 编译后的中间产物。
// 之所以和描述类型放在同一个模块，是因为它们共同构成「编译器的 IR」，
// 而写出器只依赖后者。
// ---------------------------------------------------------------------------

/// `$staticprop` 塌缩后唯一那根骨骼的名字。
///
/// `simplify.cpp:3282` 的 `strcpy( psource->localBone[0].name, "static_prop" )`。
/// 实测语料 **2681/2681** 个静态道具的 `bone[0].name` 都是它。
pub const STATIC_PROP_BONE: &str = "static_prop";

/// 一个 mesh（`mstudiomesh_t`），对应一个材质。
///
/// 由 SMD 中**同一材质名**的三角形归并而成，与 studiomdl 的划分一致。
///
/// # 多 LOD 时这里是「LOD 0 的视图」
///
/// 多 LOD 的模型，VVD 顶点块存的是**跨 LOD 统一去重并按 LOD 排序**的池，
/// 不是这个结构里的顺序。本结构始终描述 **LOD 0**（最精细）的顶点与三角形
/// —— 它仍然有用（材质划分、包围盒、单 LOD 路径），但**写多 LOD 的 VVD/MDL
/// 时必须走 [`CompiledModel::lods`]**，否则顶点数与偏移全对不上。
#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    /// 材质在 `[materials].textures` 里的下标。
    pub material: usize,
    pub vertices: Vec<Vertex>,
    /// 三角形，每项是三个**顶点下标**（指向本 mesh 的 `vertices`）。
    pub triangles: Vec<[u32; 3]>,
    /// eyeball 打标：`Some(j)` = 该 mesh 被第 j 个 eyeball 引用，
    /// 写 `materialtype=1`/`materialparam=j`（`write.cpp:1689-1690`）；
    /// `None` = 写 0。由编译期从 `material` 名匹配出来。
    pub eyeball_tag: Option<usize>,
}

/// 一个 model 的多 LOD 数据（每个 mesh 一份 [`crate::lod::MeshLods`]）。
///
/// **顺序与 [`CompiledModel::meshes`] 严格一一对应** —— 写出器靠这个
/// 对应关系填 `mstudiomesh_t` 的各字段，错位会让材质贴到错误的面上。
#[derive(Debug, Clone, PartialEq)]
pub struct ModelLods {
    /// 每个 mesh 的统一顶点池 + 各 LOD 三角形。顺序 == `CompiledModel::meshes`。
    pub meshes: Vec<crate::lod::MeshLods>,
    /// LOD 数（含 LOD 0）。
    pub num_lods: usize,
    /// 每个 LOD 的 `switchPoint`（`ModelLODHeader_t.switchPoint`）。
    ///
    /// `switch_points[0]` 恒为 0（LOD 0 总是被选中）。
    pub switch_points: Vec<f32>,
    /// 每根骨骼被哪些 LOD 的**顶点**使用（bit n = LOD n）。
    ///
    /// 对应官方的 `MarkBonesUsedByLod`（`UnifyLODs.cpp:1090`），
    /// 它把 `BONE_USED_BY_VERTEX_LOD0 << nLodID` 置进 `g_bonetable[].flags`。
    ///
    /// ⚠️ **它是按「重映射之后」的权重标记的**
    /// （`UnifyLODs.cpp:1163-1168` 的顺序：remap → collapse → sort → mark）。
    /// 且**不沿父链传播** —— 那是 `MarkParentBoneLODs`（`simplify.cpp:7270`）
    /// 单独一趟做的事，由写出器补（见 `mdl_writer::compute_bone_flags`）。
    pub bone_lod_usage: Vec<u32>,
    /// **逐档**的 `nofacial`（下标 = LOD 号，`[0]` 恒 `false`）。
    ///
    /// 官方是逐档的（`scriptLOD.GetFacialAnimationEnabled()`，
    /// `optimize.cpp:2084` → `ProcessMesh(..., forceNoFlex, ...)`）。
    ///
    /// # 产物级判据（**只有 `.dx90.vtx` 变**）
    ///
    /// `forceNoFlex` 把 `triangleIsFlexed` 强制为 `false`
    /// （`optimize.cpp:1311`），于是**所有** `isFlexed=1` 的那两趟
    /// 都收不到三角形、被 `FastRemove` 掉 ⟹ 该档**不再有 strip group 带
    /// `STRIPGROUP_IS_DELTA_FLEXED`（`0x04`）**：
    ///
    /// ```text
    /// 全 flexed 的 mesh： 1 SG [0x06]  →  1 SG [0x02]   （数量不变！）
    /// 混合的 mesh：       2 SG [0x06,0x02] → 1 SG [0x02]
    /// ```
    ///
    /// ⚠️ **判据是「不再有 `0x04`」，不是「strip group 数下降」** ——
    /// 后者会漏掉第一类。
    ///
    /// `.mdl` 与 `.vvd` 必须**逐字节不变**：`numflexes`/`flexindex` 来自
    /// 全局 `g_flexkey[]`（与 LOD 无关），`optimize.cpp` 也从不写 `gflags`；
    /// `.vvd` 的 `numLODVertexes` 只依赖「每个 LOD 引用了哪些顶点」，
    /// 而 `nofacial` 只是**重新分组三角形**，不增不减顶点。
    pub no_facial: Vec<bool>,
}

impl ModelLods {
    /// 是否是多 LOD（`num_lods > 1`）。
    pub fn is_multi(&self) -> bool {
        self.num_lods > 1
    }
}

/// 一个顶点（`mstudiovertex_t`）。
#[derive(Debug, Clone, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// 骨骼绑定：`[[骨骼下标, 权重], ...]`，最多 3 组。
    /// 权重之和为 1。
    pub bones: Vec<[f32; 2]>,
}

/// 一个 model 的**编译结果**（网格已从 SMD 读出）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledModel {
    /// 源 SMD 路径（用于报错定位）。
    pub smd_path: std::path::PathBuf,
    /// 写进 `mstudiomodel_t.name` 的名字。
    pub name: String,
    /// 参考姿态（来自 SMD 的 `skeleton` 第 0 帧）。
    pub poses: Vec<crate::smd::SmdPose>,
    /// **源骨架第 0 帧的世界矩阵**，下标 = **骨骼表下标**。
    ///
    /// `None` = 该骨骼在源里**没有精确同名**的节点（官方
    /// `boneGlobalToLocal[k] == -1`）。
    ///
    /// # 为什么必须存下来
    ///
    /// 官方 `RemapVerticesToGlobalBones()`（`simplify.cpp:5156-5241`）把顶点
    /// 从「源空间」搬进「参考姿态空间」用的 `destBoneToWorld[k]` 是
    /// `TranslateAnimations()`（`:1498-1564`）给的：
    ///
    /// ```c
    /// int q = psource->boneGlobalToLocal[k];
    /// if (q == -1) { ...用 g_bonetable 的局部姿态沿**全局**父链累乘... }
    /// else         { ConcatTransforms( srcBoneToWorld[q], g_bonetable[k].srcRealign, destBoneToWorld[k] ); }
    /// ```
    ///
    /// 而 `srcBoneToWorld` 来自 `BuildRawTransforms(psource, 0, …)`
    /// （`:1432-1492`），它走的是 **`psource->localBone[k].parent`（源文件自己的
    /// 父链）**，与 `g_bonetable` 的父链**无关**。
    ///
    /// ⚠️ 两条父链会分叉的典型场景：`$definebone "X" "" …` 把 `X` **重新声明为根**。
    /// 此时骨骼表里 `X.parent == -1`，源里 `X` 却仍有祖先（例如 FBX 的骨架根
    /// `Skeleton`，带 −90° 绕 X）。用骨骼表的父链重建 `srcBoneToWorld` 会**丢掉**
    /// 那个祖先 ⟹ `srcBoneToWorld` 与 `boneToPose` 恰好相等 ⟹ `M_k` 被判成单位阵
    /// ⟹ **顶点完全不被搬动**，而骨骼表已经搬了 ⟹ 网格与骨骼错开一个 90°。
    ///
    /// ⚠️ 源父链信息在 `compile()` 之后就没了（`poses` 已改写成骨骼表下标，
    /// 且只含命中的骨骼），所以必须在这里存下算好的世界矩阵。
    pub source_world: Vec<Option<crate::bone_math::Matrix3x4>>,
    pub meshes: Vec<Mesh>,
    /// **多 LOD 数据**。`None` 表示单 LOD —— 此时写出器走原来的路径，
    /// 产物与加这个字段之前**逐字节相同**（有测试钉住）。
    ///
    /// `Some` 时 `lods.meshes` 与 `meshes` 一一对应。
    pub lods: Option<ModelLods>,
    /// 该 model 的已解析 eyeball（`up`/`forward`/`org` 已是骨骼空间）。
    ///
    /// 写出到「紧跟该 model 的 mesh 数组之后」，`mstudiomodel_t.eyeballindex`
    /// 指过去（`eyeballindex == meshindex + nummeshes*116`）。
    pub eyeballs: Vec<CompiledEyeball>,
    /// **VTA 载荷**：每个 mesh 一份，顺序与 [`Self::meshes`] 严格一一对应。
    ///
    /// 写出到「该 model 的 eyeball 数组之后」，每个 mesh 的
    /// `mstudiomesh_t.flexindex` 指过去（**相对该 mesh 自身**）。
    /// 空 `Vec` = 该 mesh 没有形状（`numflexes = 0`，完全正常 ——
    /// 语料里 `survivor_coach` 的 5 个 mesh 就有 2 个是 0）。
    pub mesh_flexes: Vec<Vec<crate::flex::ResolvedFlex>>,
    /// **每个 mesh 的「焊接顶点 → 源控制点号」平行表**。
    ///
    /// 与 [`Self::meshes`] 一一对应，内层与 `Mesh::vertices` 一一对应。
    /// SMD 源没有控制点概念，此时**整个 `Vec` 是空的**（`Vec::new()`），
    /// 不是「一堆空内层」。
    ///
    /// ⚠️ 这张表只在 [`crate::compile`] 的 flex 解析阶段有效 ——
    /// `split_oversized_meshes` 会重排顶点，之后它与 `meshes` 就不再对应。
    /// 之所以仍然放在 IR 上而不是当场用掉：flex 解析发生在 bodypart 循环
    /// **之后**，中间隔着 `remap_vertices_to_reference_pose` 等若干阶段，
    /// 没有别的通道能把这份信息带过去。
    pub mesh_src_index: Vec<Vec<u32>>,
    /// 该 model 的几何源是 FBX 时，从文件里读出的 shape key。
    ///
    /// 官方把 FBX 的 shape key **全自动**注册成 flex（`docs/fbx-support.md`
    /// §1.6b），所以这里不需要 QC 写任何 flex 语法。
    pub shape_keys: Vec<crate::fbx::FbxShapeKey>,
}

/// 一个 body part 的编译结果。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledBodyPart {
    pub name: String,
    pub base: i32,
    pub models: Vec<CompiledModel>,
}

/// **编译输入**：描述文件 + 已读入的 SMD。
///
/// 把「描述」与「网格」分开是有意的 —— 描述层不需要知道 SMD 的存在，
/// 而写出器只需要这一个结构。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledModelDesc {
    pub desc: ModelDesc,
    pub bodyparts: Vec<CompiledBodyPart>,
    /// 已读入的序列动画（含逐帧骨骼姿态）。
    pub sequences: Vec<CompiledSequence>,
    /// **动画池** —— 一个 `$animation`（或一个隐含动画）一条。
    ///
    /// 顺序 = 写出顺序：**显式 `[[animations]]` 在前**（按声明顺序），
    /// 之后是单动画序列产生的**隐含动画**（按序列顺序，名字是
    /// `@序列名`）。见 [`Animation`] 的说明。
    pub animations: Vec<CompiledAnimation>,
    /// **骨骼轴重对齐**后的局部参考姿态与 `srcRealign`（`RealignBones`）。
    ///
    /// `None` = 该模型不触发重对齐（没有 `$ikchain` 也没有 `$realignbones`）。
    /// 此时 [`crate::compile::resolve_bone_pose`] 走原来的路径。
    ///
    /// `Some((poses, src_realign))`：
    /// - `poses[i]` 是骨骼 `i` 重排后的 `(pos, rot)`，**已经**是最终值；
    ///   写出器、`poseToBone`、自动 hitbox、动画参考姿态都必须用它，
    ///   否则骨骼表与 `poseToBone` 会互相矛盾（模型扭曲且不报错）。
    /// - `src_realign[i]` 是 `srcWorld⁻¹ ∘ newWorld`，动画构建**非参考帧**
    ///   姿态时用它把源骨架搬到重排后的空间。
    pub realigned: Option<RealignedBones>,
    /// 已解析的 flexrule（名字已换成下标）。
    pub resolved_flex_rules: Vec<ResolvedFlexRule>,
    /// 已解析的 flexcontrollerui（**含自动生成的**）。
    ///
    /// 顺序 = 写出顺序：**自动生成的在前**（每条 `[[flex_controllers]]` 一条，
    /// 与 fc 同序），用户显式写的 `[[flex_controller_ui]]` 在后。
    pub resolved_flex_controller_ui: Vec<ResolvedFlexControllerUi>,
    /// 已解析的 mouth（按下标升序排列，长度 = `max(index)+1`）。
    ///
    /// 数组里可能有「空洞」（某下标没被任何 `[[mouths]]` 填到）——
    /// 空洞写全 0（与官方 `g_mouth[index]` 未初始化一致）。
    pub resolved_mouths: Vec<ResolvedMouth>,
    /// 已解析的 jigglebone（`flags` 算好、角度转弧度、缺省填好）。
    ///
    /// 顺序 = 写出顺序 = **QC 书写顺序**（实测 `jig8`：
    /// QC 按 knee(2)→ankle(3)→hip(1) 写，文件里 @1528 knee / @1648 ankle /
    /// **@1768 hip** —— 报告 §6.4 的「按骨骼下标升序」是错的）。
    pub resolved_jiggle_bones: Vec<ResolvedJiggleBone>,
    /// 已解析的 quatinterp（名字已换成下标、角度已转弧度、`inv_tolerance` 已取倒数）。
    ///
    /// 顺序 = 写出顺序 = **TOML 书写顺序**（与 `resolved_jiggle_bones` 同一个约定）。
    pub resolved_quat_interp_bones: Vec<ResolvedQuatInterpBone>,
    /// **每根骨骼的 `physicsbone`**（`mstudiobone_t` `+0xAC`）——
    /// 「该骨骼受哪个碰撞 solid 的物理模拟驱动」。
    ///
    /// # 来源与语义（`collisionmodel.cpp:2141-2184`）
    ///
    /// ```text
    /// ① 先全置 -1
    /// ② 碰撞列表里每个 solid → 该骨骼的 physicsBoneIndex = **solid 下标**
    /// ③ 未置位的骨骼沿父链上溯找第一个已置位的祖先
    /// ④ 都找不到 → 0
    /// ```
    ///
    /// `mdl_writer` 只写第 ② 步的直接命中；第 ③ 步（沿父链上溯）
    /// 在构造这张表时就做完，所以这里存的是**最终值**。
    ///
    /// # 为什么是 `Option`
    ///
    /// `None` = **不写该字段**（保持 0）—— 适用于没有碰撞模型、
    /// 或单 solid 的情形。实测（`probe_physicsbone_ragdoll_split.js`，
    /// 排除 18 个 checksum 不配对的陈旧产物后）：
    ///
    /// | 形态 | 命中 |
    /// |---|---|
    /// | 多 solid（ragdoll） | **38/38 非平凡** |
    /// | 单 solid（prop） | **2459/2459 全 0** |
    ///
    /// 完美二分 ⟹ **只有 ragdoll 才需要填这张表**。
    pub physics_bone: Option<Vec<i32>>,
}

impl CompiledModelDesc {
    /// 是否产出 `mstudiosrcbonetransform_t` 数组
    /// （`studiohdr2.srcbonetransformindex`）。
    ///
    /// # 触发条件（本轮破解，见 `HANDBOOK.md` 第 21.6 节）
    ///
    /// **跑了 `RealignBones`**（由 `$ikchain` 或 `$realignbones` 触发，
    /// 即 [`Self::realigned`] 为 `Some`）**或** 写了 `$definebone`
    /// （`pre_aligned` 骨骼）—— 两者都会让「源骨架」与「最终骨架」分叉，
    /// 而该段正是把源姿态搬到最终空间的变换对。
    ///
    /// 记录数 == **骨骼数**（每根一条，含没被重排的骨骼）。
    ///
    /// # 判据
    ///
    /// 官方 578 个 artifacts 里**恰好 7 个**有该段，全部是这类用例：
    /// `ipr2`（`$ikchain`）/ `ipr3`（`$realignbones`）/ `ipk1`/`ipk2`/`ipk3`
    /// （`$ikchain`）/ `ipq3`/`ipq5`（`$definebone`）。
    /// **对照组 `ipr1`**（同一份 SMD、不触发重排、不写 `$definebone`）
    /// 的 `numsrcbonetransform == 0`。
    pub fn srcbonetransform_present(&self) -> bool {
        !self.srcbonetransform_bones().is_empty()
    }

    /// 需要写进 `srcbonetransform` 数组的**骨骼下标**（升序）。
    ///
    /// # 判据：`srcRealign` 或 `newWorld` **至少一个不是单位阵**
    ///
    /// 官方只给「真的被搬动过」的骨骼写记录。10 个官方用例全部吻合
    /// （`pre = M(srcWorld⁻¹)`、`post = M(newWorld)`）：
    ///
    /// | 用例 | 骨骼 | 记录 | 被排除的 |
    /// |---|---|---|---|
    /// | `ipr2`/`ipr3` | `root,a,b` | **3** | 无 |
    /// | `ipk1`/`ipk2` | `root,hip,knee,ankle` | **3** | `root`（两根都是单位阵） |
    /// | `ipk3` | 同上 | **4** | 无 |
    /// | `ipq3`/`ipq5` | `root,a,b` | **2** | `root`（两根都是单位阵） |
    /// | `ipr1`/`ipq2`/`ipkx1` | 3~4 | **0** | 全部 |
    ///
    /// ⚠️ **判据必须同时看两个矩阵**：`ipr2` 的 `root` 的
    /// `srcRealign` 是**单位阵**（它没被重排），但 `newWorld` 是
    /// `Rz(90°)`（`g_defaultrotation`）→ **它仍然有记录**。
    /// 只看 `srcRealign` 会漏掉它（mdlc 曾因此得到 2 而不是 3）。
    ///
    /// 反过来 `ipk1` 的 `root`：`srcRealign` 与 `newWorld` **都是单位阵**
    /// （根骨骼的参考姿态就是原点、无旋转）→ 正确地被排除。
    pub fn srcbonetransform_bones(&self) -> Vec<usize> {
        let Some(r) = &self.realigned else {
            return Vec::new();
        };
        let bone_parents = crate::compile::bone_parents(&self.desc);
        let final_poses: Vec<([f32; 3], [f32; 3])> = (0..self.desc.bones.len())
            .map(|i| crate::compile::resolve_bone_pose(&self.desc, self, i))
            .collect();
        let final_pos: Vec<[f32; 3]> = final_poses.iter().map(|p| p.0).collect();
        let final_rot: Vec<[f32; 3]> = final_poses.iter().map(|p| p.1).collect();
        let new_world = crate::bone_math::compute_world(&final_pos, &final_rot, &bone_parents);
        let is_identity = |m: &crate::bone_math::Matrix3x4| {
            m.iter()
                .zip(crate::bone_math::IDENTITY.iter())
                .all(|(a, b)| a == b)
        };
        r.src_realign
            .iter()
            .enumerate()
            .filter(|(i, sr)| {
                // `srcRealign` 不是单位阵（被搬动过）……
                !is_identity(sr)
                    // ……或者最终世界矩阵不是单位阵（如根骨骼的 Rz(90°)）。
                    || new_world.get(*i).is_some_and(|w| !is_identity(w))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// 骨骼轴重对齐的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RealignedBones {
    /// 重排后的**局部**参考姿态（`pos`、`rot`）。
    pub poses: Vec<([f32; 3], [f32; 3])>,
    /// `srcRealign` 矩阵（`simplify.cpp:4397`）。
    pub src_realign: Vec<crate::bone_math::Matrix3x4>,
}

/// blend 网格的**一格**（= 一个 animdesc）。
///
/// ⚠️ 格子的 animdesc 名字用的是**源动画名**（如 `a_run` / `look_down`），
/// **不是** `@序列名` —— 实测官方 `idle` 的三格叫
/// `a_run` / `a_idle` / `a_run`，而单动画序列 `reload` 的 animdesc 叫 `@reload`。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledBlendCell {
    /// animdesc 名（源 SMD 名，不含扩展名）。
    pub name: String,
    /// 该格的帧。
    pub frames: Vec<Vec<crate::smd::SmdPose>>,
    /// 该格的 SMD 路径（诊断用）。
    pub smd_path: std::path::PathBuf,
}

/// `calcblend` 的控制轴位（`studio.h:3072-3084`，`STUDIO_X` 起）。
///
/// # 为什么用位而不是枚举
///
/// 官方 `lookupControl`（`studiomdl.cpp:395-415`）返回的是**位**，
/// 直接存进 `pseq->paramcontrol[i]`，再由 `CalcPoseParameterValue`
/// 的 `switch` 消费（`simplify.cpp:5428-5446`）。用位值能逐字对上，
/// 也自然表达「无法识别的名字 ⟹ `-1` ⟹ 不匹配任何 case」。
///
/// # ⚠️ `0` 是合法且**最常见**的取值
///
/// `memset` 之后 `paramcontrol[i] == 0`，而 `switch(0)` 不匹配任何 case
/// ⟹ `CalcPoseParameterValue` 返回 **`0.0`**（`:5445` 的兜底 `return 0.0`）。
/// 这不是「未设置」的错误状态，而是官方**确实会走到**的一条路径 ——
/// 「只写 `blendwidth` 不写 `blend`」时就是它。
pub mod control {
    /// `X` —— 附着点相对位置的 X 分量。
    pub const X: i32 = 0x0000_0001;
    /// `Y`。
    pub const Y: i32 = 0x0000_0002;
    /// `Z`。
    pub const Z: i32 = 0x0000_0004;
    /// `XR` —— 附着点相对朝向的 X 分量（**度**）。
    pub const XR: i32 = 0x0000_0008;
    /// `YR`。
    pub const YR: i32 = 0x0000_0010;
    /// `ZR`。
    pub const ZR: i32 = 0x0000_0020;

    /// `lookupControl`（`studiomdl.cpp:395-415`）的等价实现。
    ///
    /// **大小写不敏感**（官方用 `stricmp`）。无法识别时返回 `-1`
    /// —— 与官方一致；`-1` 在 `switch` 里同样落到 `return 0.0`。
    ///
    /// ⚠️ 官方还认 `LX/LY/LZ/LXR/LYR/LZR/LM/LQ`（局部轴与线性），
    /// 但 `CalcPoseParameterValue` 的 `switch` **只处理前 6 个** ——
    /// 其余一样返回 `0.0`。所以这里只需识别前 6 个，
    /// 其余（含拼错的）一律回落到「不匹配」。
    pub fn lookup(name: &str) -> i32 {
        if name.eq_ignore_ascii_case("X") {
            X
        } else if name.eq_ignore_ascii_case("Y") {
            Y
        } else if name.eq_ignore_ascii_case("Z") {
            Z
        } else if name.eq_ignore_ascii_case("XR") {
            XR
        } else if name.eq_ignore_ascii_case("YR") {
            YR
        } else if name.eq_ignore_ascii_case("ZR") {
            ZR
        } else {
            -1
        }
    }
}

/// 一条**待算**的 `calcblend` 轴。
///
/// # 为什么不能在序列循环里就地算
///
/// 官方 `CalcPoseParameters`（`simplify.cpp:7315`）跑在 `ProcessData` 的
/// **末尾** —— 那时所有序列、动画、附着点都已就绪。而 `blendref` /
/// `blendcomp` / `blendcenter` 走的是 `LookupAnimation`，
/// **会回落到序列池**（`studiomdl.cpp:2381-2397`）⟹ 必须等序列建完。
///
/// 所以本结构是「先记下要算什么」，由 [`CompiledModelDesc`] 建好之后的
/// 一趟后处理消费。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledCalcAxis {
    /// 网格的哪一根轴（0 或 1）。
    pub axis: usize,
    /// `calcblend` 的附着点名。
    ///
    /// `None` = **官方 `memset` 残留**那条路径：本轴没有对应的
    /// `blend`/`calcblend`，于是 `paramattachment[iPose]` 保持 **0**
    /// （`≠ -1` ⟹ 误入 calc 分支），而 `paramcontrol[iPose]` 同样是 **0**。
    /// `CalcPoseParameterValue(0, …)` 恒返回 `0.0`
    /// ⟹ 每一格都是 0 ⟹ 必然触发 `calcblend failed`。
    pub attachment: Option<String>,
    /// 控制轴名（`X` / `Y` / `Z` / `XR` / `YR` / `ZR`）。
    ///
    /// `None` = 官方 `memset` 的 **0**（或 `lookupControl` 不认识 ⟹ `-1`），
    /// 两者在 `switch` 里都落到 `return 0.0`。
    pub control: Option<String>,
}

/// blend 的一个参数轴（已解析）。
///
/// 落盘时 `paramindex[i]` = [`Self::parameter_index`]，
/// `paramstart[i]`/`paramend[i]` = `start`/`end`，
/// 而 `param0[]`/`param1[]` 是**线性插值**出来的逐格取值
/// （`simplify.cpp:5579-5590`）。
///
/// ⚠️ `calcblend` 的轴（[`BlendParam::attachment`] 非空）在编译期被
/// **整个改写** —— `start`/`end`/`keys` 三个都换成 `CalcPoseParameters`
/// 算出的实测值，线性插值**不参与**。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledBlendParam {
    /// `mstudioseqdesc_t.paramindex[i]`：参数在
    /// `[[model.pose_parameters]]` 里的下标。
    pub parameter_index: i32,
    /// 该轴起始值（`paramstart[i]`）。
    pub start: f32,
    /// 该轴结束值（`paramend[i]`）。
    pub end: f32,
    /// 逐格取值（写进 `posekeyindex` 数组的该轴那一段）。
    ///
    /// 长度 = `groupsize[该轴]`。
    pub keys: Vec<f32>,
}

/// 一条自动层（已解析成下标）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAutoLayer {
    /// `iSequence`：被叠加序列的下标。
    pub sequence: i16,
    /// `iPose`。
    pub pose: i16,
    /// `STUDIO_AL_*` 位。
    pub flags: i32,
    /// 四个时间量。**已按 `write.cpp:539-551` 转成落盘值**
    /// （不带 `STUDIO_AL_POSE` 时是 cycle，带时是原值）。
    pub start: f32,
    pub peak: f32,
    pub tail: f32,
    pub end: f32,
}

/// 一个**已声明的动画**（QC 的 `$animation`）。
///
/// # 为什么需要独立于 `[[sequences]]`
///
/// 官方的 `mstudioanimdesc_t` 数组**不是**「一条序列一个」，而是
/// **一个 `$animation` 一个** —— `g_panimation[]` 是一个全局池，
/// `$sequence` 块里的裸名字只是**引用**它（`studiomdl.cpp:2952-2959`：
/// 先按名字在池里查，查到就复用同一个 animdesc）。
///
/// 实测 `v_autoshotgun.mdl`：**27 个 seqdesc / 29 个 animdesc** ——
///
/// ```text
/// $animation "a_idle" ...        -> animdesc 0   （被 idle_raw 与 idle 共用）
/// $animation "a_run"  ...        -> animdesc 1   （被 idle 的**两格**共用）
/// $animation "look_down"/"look_mid"/"look_up" -> animdesc 2/3/4
/// $sequence "idle_raw" "a_idle"  -> 复用 0，**不新增**
/// $sequence "idle" { "a_run" "a_idle" "a_run" } -> 复用 1/0/1，**不新增**
/// $sequence "reload" { "reload.smd" } -> 池里没有 -> 新建 @reload = animdesc 5
/// ```
///
/// 所以 27 条序列里前 3 条不产生新动画，`5 + 24 = 29`。
///
/// 早先 mdlc 把「每格一个 animdesc」直接当成「每序列一格一个」，
/// 于是同一格被引用两次就写出两个 animdesc（实测多出 2 个：
/// `numlocalanim` 31 vs 官方 29），并让 `rotscale` 的全局极值算错。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Animation {
    #[serde(default)]
    pub forward_declared: bool,
    #[serde(default)]
    pub loop_restart: i32,
    #[serde(default)]
    pub fudge_loop: bool,
    #[serde(default)]
    pub extra_flags: i32,
    #[serde(default = "default_motion_rollback")]
    pub motion_rollback: f32,
    #[serde(default)]
    pub motion_type: i32,
    /// 动画名（QC `$animation` 的第一个参数）。
    ///
    /// `$sequence` 的 [`Sequence::blends`] 就是按这个名字引用的。
    pub name: String,
    /// 源 SMD 路径，**相对描述文件所在目录**。
    pub smd: String,
    /// 帧率。省略 = **30**（`Cmd_ImpliedAnimation` 的 `panim->fps = 30`）。
    #[serde(default)]
    pub fps: Option<f32>,
    /// `loop`：置 `STUDIO_LOOPING`。
    #[serde(default)]
    pub looping: bool,
    /// 取帧区间 `[start, end]`（**闭区间**，QC 的 `frames a b`）。
    ///
    /// 省略 = 全部帧。越界会被**夹到**源范围
    /// （`studiomdl.cpp:2250-2254`）。
    #[serde(default)]
    pub frames: Option<[i32; 2]>,
    /// 减除参考**动画名**（QC 的 `subtract "x"`）。
    ///
    /// 见 [`crate::compile`] 里 `subtract_base_frames` 的说明。
    #[serde(default)]
    pub subtract: Option<String>,
    /// 减除时取参考动画的第几帧（QC 的 `subtract "x" <帧>`）。缺省 0。
    #[serde(default)]
    pub subtract_frame: Option<i32>,
    /// `$animation` 块里的 `$ikrule`（与 [`Sequence::ik_rules`] 同构）。
    #[serde(default)]
    pub ik_rules: Vec<IkRule>,
    /// QC 的 `noautoik`。
    #[serde(default)]
    pub no_auto_ik: bool,
    /// 本动画用的**权重表**名（QC 的 `$animation ... weightlist "<名>"`）。
    ///
    /// 见 [`Sequence::weight_list`]。官方把 `weightlist` 当作
    /// `s_animcmd_t` 的一种（`CMD_WEIGHTS`），挂在**动画**上；
    /// 序列级关键字只是把它记进序列的 `cmds[]`，最终
    /// `setAnimationWeight(panim, index)` 作用在动画上。
    #[serde(default)]
    pub weight_list: Option<String>,
    /// 本动画的命令序列（QC 的 `cmdlist "<名>"` 展开 + 内联命令）。
    ///
    /// 见 [`Sequence::cmds`]。官方把命令挂在 `s_animation_t.cmds[]` 上
    /// （**序列本身没有 `cmds` 字段** —— `studiomdl.h:610-668`），
    /// 所以序列级的 `cmdlist` 其实写在它的第一格动画上。
    #[serde(default)]
    pub cmds: Vec<AnimCmd>,
    /// 缩放（QC `scale <f>`，`studiomdl.cpp:2219-2223`）。
    ///
    /// 官方 `panim->scale` 初值 **1.0**（`Cmd_Animation` `:2428`），
    /// 只在 [`crate::compile`] 的源姿态换算里被消费
    /// （`ConvertAnimation` → `BuildRawTransforms`）。
    #[serde(default)]
    pub scale: Option<f32>,
    /// 位移（QC `origin <x> <y> <z>`，`studiomdl.cpp:2193-2203`）。
    ///
    /// 官方 `panim->adjust` 初值 **`(0,0,0)`**（`g_defaultadjust`）。
    #[serde(default)]
    pub adjust: Option<[f32; 3]>,
    /// 旋转（QC `rotate <z>` / `angles <x> <y> <z>`，
    /// `studiomdl.cpp:2204-2218`）。**弧度**，按 `RadianEuler` 的 `(x,y,z)`。
    ///
    /// ⚠️ 官方初值是 `g_defaultrotation = RadianEuler(0, 0, π/2)`
    /// （`studiomdl.cpp:6883`）—— 缺省时 `rotation.z` 是 **90°**，
    /// 不是 0。`rotate` 与 `angles` 都把 `z` 写成
    /// `DEG2RAD(值 + 90)`，即**补上那 90°**；`x`/`y` 直接写。
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// `srcstack "walk"`：**按名字选动画栈**（仅对 FBX 源有意义）。
    ///
    /// 见 [`Sequence::src_stack`] —— `$animation` 引用 FBX 时同样支持，
    /// 因为两者最终都走 `read_source_frames`。
    #[serde(default)]
    pub src_stack: Option<String>,
    /// `srcfps 30`：FBX 动画的**重采样率**。见 [`Sequence::src_fps`]。
    #[serde(default)]
    pub src_fps: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledLocalHierarchy {
    pub bone: i32,
    pub parent: i32,
    pub range: [f32; 4],
    pub start_frame: i32,
    pub poses: Vec<([f32; 3], [f32; 4])>,
}

/// 一个**已编译的动画**（animdesc 的载荷）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAnimation {
    pub local_hierarchy: Vec<CompiledLocalHierarchy>,
    pub weights: Vec<f32>,
    pub loop_restart: i32,
    pub fudge_loop: bool,
    pub extra_flags: i32,
    pub movements: Vec<Movement>,
    /// animdesc 名（QC `$animation` 名，或隐含动画的 `@序列名`）。
    pub name: String,
    pub smd_path: std::path::PathBuf,
    pub fps: f32,
    pub looping: bool,
    /// 逐帧姿态（已按 `desc.bones` 顺序排列、已取帧区间、已减除参考）。
    pub frames: Vec<Vec<crate::smd::SmdPose>>,
    /// `STUDIO_DELTA` —— 本动画存的是**增量**姿态。
    ///
    /// # 什么时候置位（`simplify.cpp:163-166`）
    ///
    /// ```c
    /// case CMD_SUBTRACT:
    ///     panim->flags |= STUDIO_DELTA;
    ///     subtractBaseAnimations( ... );
    /// ```
    ///
    /// 即**只要用了 `subtract` 就置位**。它有两个后果：
    ///
    /// 1. 写进 `animdesc.flags`（实测官方 `look_down` 是 `0x04`）；
    /// 2. **抑制自动补的 IK 规则** —— `ProcessIKRules` 对
    ///    `flags & STUDIO_DELTA` 的动画**一条都不补**
    ///    （`simplify.cpp:6251` 附近的判据），所以官方 `look_down`
    ///    的 `numikchains` 是 **0**，而同文件的 `@reload` 是 2。
    pub delta: bool,
    /// `$animation` 块里声明的 ikrule（隐含动画为空）。
    pub ik_rules: Vec<IkRule>,
    pub no_auto_ik: bool,
    /// **减除之前**的逐帧姿态。理由见
    /// [`CompiledSequence::pre_subtract_frames`]。
    ///
    /// 放在这里是因为**包围盒要按格取** —— blend 的每一格是独立动画，
    /// 而序列级的字段只存得下第一格。
    pub pre_subtract_frames: Option<Vec<Vec<crate::smd::SmdPose>>>,
}

/// 一个序列的**编译结果**（动画帧已从 SMD 读出）。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledSequence {
    pub key_values: Option<String>,
    pub name: String,
    pub smd_path: std::path::PathBuf,
    pub fps: f32,
    pub looping: bool,
    /// 本序列是 `$declaresequence` 的**空壳**，见
    /// [`Sequence::forward_declared`]。
    ///
    /// 置位时写出器走**完全不同的分支**：`numblends=0`、
    /// `groupsize=[0,0]`、`activity=0`、`paramindex=[0,0]`、
    /// `fadein/out=0`、`bbmin/bbmax=±9999`、`weightlist` 全 0，
    /// 并置 `flags |= STUDIO_OVERRIDE`。
    ///
    /// 这些「异常值」不是特例代码 —— 它们是官方 `memset` 之后
    /// **一个字段都没被赋值**的自然结果（见该字段的完整实测表）。
    pub forward_declared: bool,
    /// `activity` 的原始值（L4D2 的 activity 编号表不在本实现范围内，
    /// 因此默认写 -1 = 未指定，与 studiomdl 在 QC 未写 activity 时一致）。
    pub activity: i32,
    /// `mstudioseqdesc_t.szactivitynameindex`（`+0x08`）指向的**名字**。
    ///
    /// # 与 [`Self::activity`] 是**两个独立字段**
    ///
    /// 官方 `Option_Activity`（`studiomdl.cpp:1166-1178`）只做两件事：
    ///
    /// ```c
    /// strcpy( psequence->activityname, token );   // 名字原样存字符串
    /// psequence->actweight = verify_atoi(token);  // 第二个 token 是权重
    /// ```
    ///
    /// 它**从不**给 `psequence->activity` 赋值 —— 那个字段在
    /// `ParseSequence` 初始化时是 **-1**（`studiomdl.cpp:2631` 的
    /// `psequence->activity = -1; // -1 is the default for 'no activity'`），
    /// 由**游戏 DLL 在加载时**按名字填。
    ///
    /// 实测官方 `v_autoshotgun.mdl`：`seq[3]` 的
    /// `activityname = "ACT_VM_RELOAD"` 而 `activity == -1`。
    ///
    /// ⚠️ 早先本实现只写 `activity = -1`、**完全丢弃名字**
    /// （`szactivitynameindex` 指向空串）—— 实测 miku 27/27 条序列
    /// 的 activity 名全空，与官方不符。
    pub activity_name: String,
    /// `mstudioseqdesc_t.actweight`（`+0x14`）。见
    /// [`Sequence::activity_weight`]。
    pub activity_weight: i32,
    /// QC 的 `delta`：`flags` 里的 `STUDIO_DELTA | STUDIO_POST`。
    /// 见 [`Sequence::delta`]。
    pub delta: bool,
    /// 逐帧姿态。`frames[f]` 是按 `desc.bones` 顺序排列的骨骼姿态。
    ///
    /// **单位是弧度 / 原始位置**，与 SMD 的 `skeleton` 段一致。
    ///
    /// blend 序列（[`Self::cells`] 非空）时这里是**第一格**的帧 ——
    /// 用于 `bbmin`/`bbmax` 之外的场合；每格自己的帧在
    /// [`CompiledAnimation::frames`] 里。
    pub frames: Vec<Vec<crate::smd::SmdPose>>,
    /// **blend 网格的每一格**指向 [`CompiledModelDesc::animations`] 的
    /// **下标**，顺序是**行主序**（`cells[k * width + j]`）。
    ///
    /// 空 = 单动画序列（它自己的 animdesc 由 `@序列名` 隐含产生）。
    ///
    /// ⚠️ 同一格被多条序列引用时下标**相同** —— 那是**同一个**
    /// animdesc，不是两份数据（官方 `g_panimation[]` 是共享池）。
    pub cells: Vec<usize>,
    /// blend 网格的宽度（`groupsize[0]`）。单动画序列为 1。
    pub blend_width: i32,
    /// 两个参数轴。`[None, None]` = 不是 blend 序列。
    ///
    /// 元素是 `(参数下标, param0[], param1[])` 里的**该轴那一个**数组。
    pub blend_params: [Option<CompiledBlendParam>; 2],
    /// **待算**的 `calcblend` 轴（见 [`CompiledCalcAxis`]）。
    ///
    /// # ⚠️ 这里记录的是「官方会走到 calc 分支」的全部情形
    ///
    /// 不只是「QC 写了 `calcblend`」，还包括官方那个著名的
    /// **未初始化字段**路径：
    ///
    /// ```text
    /// groupsize[iPose] > 1  &&  paramattachment[iPose] != -1
    /// ```
    ///
    /// `Cmd_Sequence`（`studiomdl.cpp:2624`）只 `memset` + 初始化
    /// `paramindex`/`groupsize`/`fadein`/`fadeout`，**`paramattachment`
    /// 与 `paramcontrol` 都不在初始化列表里** ⟹ 都是 **0**。
    /// 只有 `blend` 会写 `paramattachment[i] = -1`（`:2742`）。
    ///
    /// 所以**只写 `blendwidth` 而没写 `blend`** 时：
    /// `groupsize[0] = 3 > 1` 成立，`paramattachment[0] == 0 != -1` 也成立
    /// ⟹ 进入 calc 分支 ⟹ `paramcontrol[0] == 0` ⟹ 每格 `v = 0.0`
    /// ⟹ `|paramstart − paramend| = 0 < 0.01` ⟹
    /// **`ERROR: calcblend failed in <序列>`**（`simplify.cpp:5566-5569`）。
    ///
    /// 这正是本字段要复刻的行为：空列表 = 官方不会进 calc 分支。
    pub calc_axes: Vec<CompiledCalcAxis>,
    /// `blendref`（QC 的 `blendref <名>`）—— 原始名字。
    ///
    /// ⚠️ **留到后处理再解析**：官方 `LookupAnimation` 先查动画池、
    /// **再回落到序列池**（`studiomdl.cpp:2381-2397`），而序列池要等
    /// 全部序列建完才存在。
    pub blend_ref: Option<String>,
    /// `blendcomp`（QC 的 `blendcomp <名>`）—— 原始名字。
    ///
    /// `None` ⟹ 官方回落到 `paramanim`（即 `blend_ref` 解析出的值）。
    pub blend_comp: Option<String>,
    /// `blendcenter`（QC 的 `blendcenter <名>`）在网格里的位置 `[i0, i1]`。
    ///
    /// **已解析**（建序列时就能做：只需比对格子的动画下标）。
    /// `None` = 没写，或写了但**不在网格里** —— 官方两种都取中点
    /// （`simplify.cpp:5518-5521` 的 `if (!found)`）。
    pub blend_center: Option<[usize; 2]>,
    pub transition: Option<[String; 2]>,
    pub transition_reversible: bool,
    /// 自动层（`mstudioautolayer_t`）。
    pub auto_layers: Vec<CompiledAutoLayer>,
    /// 动画事件（已从描述层搬过来，便于写出器直接消费）。
    pub events: Vec<SequenceEvent>,
    /// `mstudioseqdesc_t.fadeintime`（+0x68）。缺省 **0.2**，见
    /// [`Sequence::fade_in`]。
    pub fade_in: f32,
    /// `mstudioseqdesc_t.fadeouttime`（+0x6C）。缺省 **0.2**。
    pub fade_out: f32,
    /// QC 的 `noautoik` —— 抑制自动补 `IK_RELEASE`，见
    /// [`Sequence::no_auto_ik`]。
    pub no_auto_ik: bool,
    /// `$ikrule` 列表，见 [`Sequence::ik_rules`]。
    pub ik_rules: Vec<IkRule>,
    /// **序列级** IK 锁，见 [`Sequence::iklocks`]。
    ///
    /// 与 [`Self::ik_rules`] 一样直接透传描述层 —— 链名→下标的解析
    /// 推迟到写出时（`mdl_writer` 已持有 `ikchains` 表，且
    /// `ik_autoplay_locks` 也是这么做的，保持两处一致）。
    pub iklocks: Vec<IkAutoplayLock>,
    /// 逐段移动键，见 [`Sequence::movements`]。
    pub movements: Vec<Movement>,
    /// **每段帧数**（`sectionframes`，`+0x54`）。`0` = 不分段。
    ///
    /// 由 [`Sequence::section_frames`] / [`Sequence::section_threshold`]
    /// 在编译期按 `numframes >= 阈值` 决定（见 `compile.rs`）。
    pub section_frames: i32,
    /// **段表条目数**（`floor(numframes/sectionframes) + 2`）。
    ///
    /// **不是 `ceil`** —— 引擎索引的最大下标是 `numframes/sectionframes + 1`
    /// （`studio.cpp:345`），所以条目数要 +2。
    pub num_sections: usize,
    /// **减除之前**的逐帧姿态（`subtract` 前的原始 SMD 姿态）。
    ///
    /// # 为什么必须单独留一份
    ///
    /// 官方的**包围盒**走 `CalcSequenceBoundingBoxes`
    /// （`simplify.cpp:7049-7202`），它用
    /// `AngleMatrix(sanim[j][k].rot, sanim[j][k].pos)` **直接**建局部矩阵，
    /// **不**走 `CalcBoneTransforms` 的 DELTA 合成分支
    /// （`simplify.cpp:4558-4577`）。对 `subtract` 出来的动画，`sanim`
    /// 存的是**增量**，于是官方那边 `bonetransform` 近单位阵 ⟹
    /// `posetransform = inverse(boneToPose)` ⟹ 顶点被映射到**参考姿态**附近
    /// —— 得到一个**全身大小**的盒子。
    ///
    /// **决定性判据**（`docs/_probe/probe_nosub_bbox.js`）：把 `subtract`
    /// 从 miku 的 TOML 里去掉再编译，
    ///
    /// | 序列 | 带 `subtract` | 去掉 `subtract` |
    /// |---|---|---|
    /// | `look_poses` | **44276588 ULP** | **51 ULP** |
    ///
    /// 即「官方包围盒 = 用**未减除**的姿态算」。所以这里留一份减除前的帧
    /// 专供包围盒，而 `frames` 仍是减除后的（写动画链要它）。
    ///
    /// 非 `subtract` 序列此字段为 `None`（直接用 `frames`）。
    pub pre_subtract_frames: Option<Vec<Vec<crate::smd::SmdPose>>>,
    /// **额外的 `seqdesc.flags` 位**，见 [`Sequence::extra_flags`]。
    ///
    /// `None` = 没有额外位。写出器把它 OR 进算出来的 `flags`。
    pub extra_flags: Option<i32>,
    /// **已解析的逐骨骼权重**（`float[numbones]`），落盘进
    /// `seqdesc.weightlistindex` 指向的块。
    ///
    /// # 怎么算出来的（两级合并）
    ///
    /// 1. 每格动画的权重来自它自己的 `weight_list`（缺省 = 全 1）；
    /// 2. 本序列 = **各格逐骨骼取 MAX**（`simplify.cpp:302-318`）。
    ///
    /// 见 [`crate::compile::resolve_weight_lists`]。
    pub weights: Vec<f32>,
}

/// 一条**已解析**的 IK 规则：名字都换成了下标，帧号也过了
/// `ProcessIKRules`（`simplify.cpp:5843-5954`）的展开。
///
/// 这是 [`IkRule`] 的「可直接写字节」形态。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedIkRule {
    /// `mstudioikrule_t.index`。
    ///
    /// **不是**规则序号那么简单：`simplify.cpp:6325-6334` 只遍历
    /// **序列**去写 `index`，所以**没被任何 `seqdesc` 引用过的动画**
    /// 其 `index` 恒为 **0**（实测 `rsrch_ik_index2.js`：全语料
    /// `index == j` 27749 条、`!= j` 3192 条，那 3192 条**全部是 0**，
    /// 且全部属于未被引用的动画）。
    pub index: i32,
    /// `type`（1/3/4/5/6）。
    pub type_code: i32,
    pub chain: i32,
    pub bone: i32,
    pub slot: i32,
    pub height: f32,
    pub radius: f32,
    pub floor: f32,
    pub pos: [f32; 3],
    pub q: [f32; 4],
    /// 起始**帧号**（`iStart`）。
    pub start_frame: i32,
    /// **cycle** 形态的四个量（写出时已经除以 `numframes−1`）。
    pub start: f32,
    pub peak: f32,
    pub tail: f32,
    pub end: f32,
    pub contact: f32,
    /// `attachment` 字符串（**内联**写，不走字符串池）。
    pub attachment: String,
    /// 压缩误差载荷：`None` = `compressedikerrorindex` 留 **0**。
    ///
    /// 官方在「6 个通道全空」时整条跳过（`write.cpp:922-928`）。
    /// 自动补的 `IK_RELEASE` 规则 `numerror == 0`，永远走这条路。
    pub error: Option<CompressedIkError>,
}

/// `mstudiocompressedikerror_t`（**36 字节**）+ 紧随其后的 6 个 RLE 通道。
///
/// 反解证据（`rsrch_ik_comp_size.js`，全语料 10230 条带载荷的规则）：
/// `offset[0] == 36` 于 **10230/10230**，且 6 个 offset 严格递增 ——
/// 头正好 36 字节、6 个通道块背靠背。
#[derive(Debug, Clone, PartialEq)]
pub struct CompressedIkError {
    /// `float scale[6]`。
    pub scale: [f32; 6],
    /// 每个通道的 RLE 流（已编码的 `mstudioanimvalue_t` 字节）。
    pub channels: [Vec<u8>; 6],
}

/// 一条**已解析**的 flexop：`op` 是编号，`d` 已定下（下标或 float 值）。
///
/// 这是 [`FlexOp`] 的「可直接写字节」形态 —— 编译期把
/// `controller`/`flexdesc` 名解析成下标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedFlexOp {
    /// `mstudioflexop_t.op`（`STUDIO_*` 编号）。
    pub op: i32,
    /// `d.index`（`fetch1`/`fetch2`）或 `d.value` 的位模式（`const`）。
    /// 其余 op 写 0。
    pub d: FlexOpData,
}

/// [`ResolvedFlexOp::d`] 的两种形态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlexOpData {
    /// `d.index`（flexcontroller / flexdesc 下标）。
    Index(i32),
    /// `d.value`（float）。
    Value(f32),
    /// 不用（写 0）。
    None,
}

/// 一条**已解析**的 flexrule：`flex` 是 flexdesc 下标，ops 已解析。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedFlexRule {
    /// `mstudioflexrule_t.flex`（flexdesc 下标；允许重复）。
    pub flex: i32,
    /// 已解析的后缀 op 序列。
    pub ops: Vec<ResolvedFlexOp>,
}

/// 一条**已解析**的 flexcontrollerui：`szindex0/1` 指向的 fc 下标已定。
///
/// 字段给的是 **fc 下标**，写出器再换算成「相对 ui 记录自身」的负偏移。
/// `fc0`/`fc1` 是 `szindex0`/`szindex1` 指向的 fc 下标；
/// 单声道（`stereo == false`）时 `fc1` 为 `None`（`szindex1` 写 0）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedFlexControllerUi {
    /// UI 名（写进字符串池，`sznameindex` 相对自身）。
    pub name: String,
    /// `szindex0` 指向的 fc 下标（stereo 对里是**下标更大**那条）。
    pub fc0: i32,
    /// `szindex1` 指向的 fc 下标；`None` = 写 0（单声道）。
    pub fc1: Option<i32>,
    /// `stereo`（u8）。
    pub stereo: bool,
}

/// 一条**已解析**的 mouth：`bone`/`flexdesc` 已解析成下标。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedMouth {
    /// `mstudiomouth_t.bone`（骨骼下标）。
    pub bone: i32,
    /// `forward`（直接给，无变换）。
    pub forward: [f32; 3],
    /// `flexdesc`（flexdesc 下标）。
    pub flexdesc: i32,
}

/// 一条**已解析**的 jigglebone：`flags` 算好、角度已转弧度、缺省已填。
///
/// 这是 [`JiggleBone`] 的「可直接写字节」形态，字段顺序 == `mstudiojigglebone_t`
/// 的 30 个 4 字节槽（**实测偏移**，见 [`JiggleBone`] 的说明）。
///
/// **顺序敏感**：写出器按这个顺序连续写 30 个 4 字节值，
/// 所以字段声明顺序**必须**与文件一致（`flags` 是 i32，其余 29 个是 f32）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedJiggleBone {
    /// 目标骨骼下标（用于回填 `proctype`/`procindex`）。
    pub bone: i32,
    /// `+0x00 flags`（i32）。
    pub flags: i32,
    /// `+0x04`。
    pub length: f32,
    /// `+0x08`。
    pub tip_mass: f32,
    /// `+0x0C`。
    pub yaw_stiffness: f32,
    /// `+0x10`。
    pub yaw_damping: f32,
    /// `+0x14`。
    pub pitch_stiffness: f32,
    /// `+0x18`。
    pub pitch_damping: f32,
    /// `+0x1C`。
    pub along_stiffness: f32,
    /// `+0x20`。
    pub along_damping: f32,
    /// `+0x24`。
    pub angle_limit: f32,
    /// `+0x28`。
    pub min_yaw: f32,
    /// `+0x2C`。
    pub max_yaw: f32,
    /// `+0x30`。
    pub yaw_friction: f32,
    /// `+0x34`。
    pub yaw_bounce: f32,
    /// `+0x38`。
    pub min_pitch: f32,
    /// `+0x3C`。
    pub max_pitch: f32,
    /// `+0x40`。
    pub pitch_friction: f32,
    /// `+0x44`。
    pub pitch_bounce: f32,
    /// `+0x48`。
    pub base_mass: f32,
    /// `+0x4C`。
    pub base_stiffness: f32,
    /// `+0x50`。
    pub base_damping: f32,
    /// `+0x54`。
    pub base_min_left: f32,
    /// `+0x58`。
    pub base_max_left: f32,
    /// `+0x5C`。
    pub base_left_friction: f32,
    /// `+0x60`。
    pub base_min_up: f32,
    /// `+0x64`。
    pub base_max_up: f32,
    /// `+0x68`。
    pub base_up_friction: f32,
    /// `+0x6C`。
    pub base_min_forward: f32,
    /// `+0x70`。
    pub base_max_forward: f32,
    /// `+0x74`。
    pub base_forward_friction: f32,
}

/// 一条**已解析**的 quatinterp 骨骼（`mstudioquatinterpbone_t` 12 字节 + 触发器区）。
///
/// 与 [`ResolvedJiggleBone`] 一样，这是「可直接写字节」的形态：
/// 名字已换成下标、角度已转弧度、`inv_tolerance` 已算成倒数。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedQuatInterpBone {
    /// 目标骨骼下标（用于回填 `proctype`/`procindex`）。
    pub bone: i32,
    /// `+0x00 control` —— **控制**骨骼下标。
    pub control: i32,
    /// `+0x08 triggerindex` —— 相对**本记录自身**的偏移。
    ///
    /// 由写出器按「记录数组 + `ALIGN4` + 逐个触发块」算出来，所以这里不填。
    /// 结构体里保留字段是为了让「12 字节」的形状显式可见。
    pub triggers: Vec<ResolvedQuatInterpTrigger>,
}

/// 一条**已解析**的 quatinterp 触发器（`mstudioquatinterpinfo_t`，48 字节）。
///
/// 字段顺序 == 文件布局（顺序敏感，写出器按此连续写）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedQuatInterpTrigger {
    /// `+0x00` —— **`1.0 / tolerance`**（官方写盘时取倒数，`write.cpp:259`）。
    pub inv_tolerance: f32,
    /// `+0x04` —— 触发四元数 `[x,y,z,w]`。
    pub trigger: [f32; 4],
    /// `+0x14` —— `base_pos + pos`。
    pub pos: [f32; 3],
    /// `+0x20` —— 目标四元数 `[x,y,z,w]`。
    pub quat: [f32; 4],
}

/// `mstudiojigglebone_t.flags` 的位（`studio.h`，受控实验 `jig{1,2,4,6,7,8,43}` 钉死）。
///
/// ⚠️ **`LENGTH(0x20)` 在 `is_flexible` 与 `is_rigid` 下都无条件置位** ——
/// 只要**块**出现就置位，但 `allow_length_flex` 会把它**清掉**
/// （`is_flexible` 独有键，实测 `jig4` = `0x01`）。
///
/// `flags` 是**按位 OR 累积**的（不是按块覆盖），所以块/键的出现顺序
/// 只影响 `0x20` 这一位（被 `allow_length_flex` 清除时）。
pub mod jiggle_flags {
    /// `is_flexible` 块出现。
    pub const IS_FLEXIBLE: i32 = 0x01;
    /// `is_rigid` 块出现。
    pub const IS_RIGID: i32 = 0x02;
    /// `yaw_constraint` 出现（`is_flexible` **或** `is_rigid` 内）。
    pub const HAS_YAW_CONSTRAINT: i32 = 0x04;
    /// `pitch_constraint` 出现（`is_flexible` **或** `is_rigid` 内）。
    pub const HAS_PITCH_CONSTRAINT: i32 = 0x08;
    /// `angle_constraint` 出现。
    pub const HAS_ANGLE_CONSTRAINT: i32 = 0x10;
    /// `is_flexible`/`is_rigid` 出现 ⟹ **自动置位**（`allow_length_flex` 可清）。
    pub const HAS_LENGTH_CONSTRAINT: i32 = 0x20;
    /// `has_base_spring` 块出现。
    pub const HAS_BASE_SPRING: i32 = 0x40;
}

/// jigglebone 的缺省值（受控实验实测，报告 §6.3 —— **这节未发现错误**）。
pub mod jiggle_defaults {
    /// `length` 缺省。
    pub const LENGTH: f32 = 10.0;
    /// `*Stiffness` 缺省（yaw/pitch/along/base 都是 100）。
    pub const STIFFNESS: f32 = 100.0;
    /// `base*` 三轴的 max 缺省。
    pub const BASE_MAX: f32 = 100.0;
    /// `base*` 三轴的 min 缺省。
    pub const BASE_MIN: f32 = -100.0;
    /// stiffness / damping 的**钳位上限**。
    ///
    /// 反编译 `FUN_004542d0`（被 `is_flexible` 的 6 个 stiffness/damping 与
    /// `has_base_spring` 的 `stiffness`/`damping` 调用）：
    /// `if v <= 1000.0 { if 0.0 <= v { v } else { 0.0 } } else { 1000.0 }`。
    ///
    /// 实测 `jig35`：`yaw_stiffness 5000 → 1000`、`yaw_damping -5 → 0`、
    /// `pitch_stiffness 1000.5 → 1000`、`along_stiffness -100 → 0`；
    /// `jig36`：`stiffness 5000 → 1000`、`damping -3 → 0`。
    ///
    /// ⚠️ **`yaw_friction`/`yaw_bounce`/`pitch_friction`/`pitch_bounce` 不钳位**
    /// （实测 `jig41`/`jig42` 的 `5000`/`6000`/`-7` 全原值透传）。
    pub const STIFFNESS_MAX: f32 = 1000.0;
}

/// `stiffness`/`damping` 的官方钳位（`[0, 1000]`）。
///
/// 见 [`jiggle_defaults::STIFFNESS_MAX`]。`NaN` 归 0（官方比较在 NaN 上
/// 两个分支都落空、返回 `fVar3`，其值为 `1000.0`——但 QC 里写不出 NaN，
/// 这里取更安全的 0）。
pub fn clamp_stiffness(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, jiggle_defaults::STIFFNESS_MAX)
    }
}

/// **官方 `$jigglebone` 接受的键全集**（含三个块名）。
///
/// 由反编译 + 27 个受控夹具（`jig1`–`jig50`）共同钉死：
///
/// | 键 | `is_flexible` | `is_rigid` | `has_base_spring` |
/// |---|---|---|---|
/// | `tip_mass` `length` `angle_constraint` `yaw_constraint` `yaw_friction` `yaw_bounce` `pitch_constraint` `pitch_friction` `pitch_bounce` | ✓（共享子解析器） | ✓（内联） | ✓（共享子解析器） |
/// | `yaw_stiffness` `yaw_damping` `pitch_stiffness` `pitch_damping` `along_stiffness` `along_damping` | ✓ | ✗ | ✗ |
/// | `allow_length_flex` | ✓ | ✗ | ✗ |
/// | `stiffness` `damping` `base_mass` `left_constraint` `up_constraint` `forward_constraint` `left_friction` `up_friction` `forward_friction` | ✗ | ✗ | ✓ |
///
/// 官方对**任何**不在此表的键（含顶层裸键）走 `_ => abort`
/// （`$jigglebone: invalid syntax '%s'` + `Aborted Processing`），
/// **不是**静默忽略。实测 `jig5`/`jig31` 报 `invalid syntax 'tip_mass'`。
///
/// ⚠️ mdlc 的 TOML 侧键名是自创的 `base_*` 前缀（见 [`JiggleBaseSpring`]），
/// 与官方 QC 键名不同 —— 本表只约束 **QC 侧**与 `writes[].key`。
pub const KNOWN_JIGGLE_KEYS: [&str; 28] = [
    // 三个块名（`writes` 里表示「进入该块」）。
    "is_flexible",
    "is_rigid",
    "has_base_spring",
    // 三块共享的 9 个通用键。
    "tip_mass",
    "length",
    "angle_constraint",
    "yaw_constraint",
    "yaw_friction",
    "yaw_bounce",
    "pitch_constraint",
    "pitch_friction",
    "pitch_bounce",
    // is_flexible 独有的 7 个。
    "yaw_stiffness",
    "yaw_damping",
    "pitch_stiffness",
    "pitch_damping",
    "along_stiffness",
    "along_damping",
    "allow_length_flex",
    // has_base_spring 独有的 9 个（注意：**裸键**，无 `base_` 前缀）。
    "stiffness",
    "damping",
    "base_mass",
    "left_constraint",
    "up_constraint",
    "forward_constraint",
    "left_friction",
    "up_friction",
    "forward_friction",
];

/// 一个**已解析**的 eyeball：`up`/`forward`/`org` 已转成骨骼空间，
/// lid 的 flexdesc 名已解析成下标。
///
/// 字段顺序与 `mstudioeyeball_t`（172 字节）一致。
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledEyeball {
    /// `bone`（骨骼下标）。
    pub bone: i32,
    /// `org`（骨骼空间）。
    pub org: [f32; 3],
    /// `zoffset`。
    pub zoffset: f32,
    /// `radius`。
    pub radius: f32,
    /// `up`（骨骼空间单位向量，`boneToPose` 逆旋转 `(0,0,1)`）。
    pub up: [f32; 3],
    /// `forward`（骨骼空间单位向量，`boneToPose` 逆旋转 `(0,1,0)`）。
    pub forward: [f32; 3],
    /// `iris_scale`。
    pub iris_scale: f32,
    /// `upperflexdesc[3]`（lowerer/neutral/raiser）。
    pub upperflexdesc: [i32; 3],
    /// `lowerflexdesc[3]`。
    pub lowerflexdesc: [i32; 3],
    /// `uppertarget[3]`。
    pub uppertarget: [f32; 3],
    /// `lowertarget[3]`。
    pub lowertarget: [f32; 3],
    /// `upperlidflexdesc`（基准 `<type>` 的下标）。
    pub upperlidflexdesc: i32,
    /// `lowerlidflexdesc`。
    pub lowerlidflexdesc: i32,
}

impl CompiledSequence {
    /// 帧数。
    pub fn num_frames(&self) -> usize {
        self.frames.len()
    }
}

impl CompiledModelDesc {
    /// 是否 `$staticprop` 模型。
    pub fn is_static_prop(&self) -> bool {
        self.desc.model.static_prop
    }

    /// `mstudioanimdesc_t` 的数量（写进头部 `numlocalanim`）。
    ///
    /// # 为什么与 [`Self::seq_count`] 分开
    ///
    /// 两条独立的理由让它们不相等：
    ///
    /// 1. **`$staticprop`**：`MakeStaticProp()` 把动画压成 **1 条 1 帧**
    ///    （`simplify.cpp:3375-3376` 的 `g_numani = 1` /
    ///    `g_panimation[0]->numframes = 1`），却**不动** `g_sequence`。
    ///    实测 `ipe2`（2 条序列 + `$staticprop`）→ `numlocalanim=1`、
    ///    `numlocalseq=2`；语料 `smalldebris_part_baked_setsexp.mdl` 是
    ///    **1 个 animdesc 对 5 个 seqdesc**。
    /// 2. **共享动画**：animdesc 是**一个 `$animation` 一个**，
    ///    多条序列引用同一格只占**一份**。实测 `v_autoshotgun.mdl`：
    ///    27 seqdesc / **29 animdesc** —— 33 个格子里有 4 处是复用
    ///    （`idle` 的两个 `a_run` 各一次、`idle`/`idle_raw` 的 `a_idle`
    ///    两次），所以只多出 2 个。
    pub fn anim_count(&self) -> usize {
        if self.is_static_prop() && !self.sequences.is_empty() {
            return 1;
        }
        self.animations.len()
    }

    /// 每个序列的 blend 网格在 animdesc 数组里的**下标**。
    ///
    /// `starts[i]` = 序列 `i` 的第一格的下标；`starts[seq_count]` = 总数。
    /// 单动画序列只有 1 个元素（隐含动画）。
    ///
    /// `$staticprop` 时全部指向 0（只有 1 个 animdesc，所有序列共用）。
    pub fn anim_starts(&self) -> Vec<usize> {
        let static_prop = self.is_static_prop();
        let mut out = Vec::with_capacity(self.sequences.len() + 1);
        let mut cur = 0usize;
        for s in &self.sequences {
            out.push(if static_prop {
                0
            } else {
                s.cells.first().copied().unwrap_or(0)
            });
            if !static_prop {
                cur += 1;
            }
        }
        out.push(if static_prop { 1 } else { cur });
        out
    }

    /// `mstudioseqdesc_t` 的数量（写进头部 `numlocalseq`）。
    pub fn seq_count(&self) -> usize {
        self.sequences.len()
    }

    /// 顶点总数。
    pub fn total_vertices(&self) -> usize {
        self.bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .flat_map(|m| &m.meshes)
            .map(|m| m.vertices.len())
            .sum()
    }

    /// 三角形总数。
    pub fn total_triangles(&self) -> usize {
        self.bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .flat_map(|m| &m.meshes)
            .map(|m| m.triangles.len())
            .sum()
    }

    /// 参考姿态下全部顶点的包围盒。
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut any = false;
        for v in self
            .bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .flat_map(|m| &m.meshes)
            .flat_map(|m| &m.vertices)
        {
            any = true;
            for k in 0..3 {
                min[k] = min[k].min(v.pos[k]);
                max[k] = max[k].max(v.pos[k]);
            }
        }
        any.then_some((min, max))
    }
}

/// `mdlc template` 打印的最小模板（带注释，可直接改）。
pub const TEMPLATE_TOML: &str = r#"# mdlc 模型描述文件（自有输入格式）
#
# 网格**不写在这里** —— 由 SMD 文件承载，本文件只引用它们
# （与 QC 的 $bodygroup { studio "x.smd" } 一致）。
#
# 想直接用 QC 而不写 TOML：`mdlc build-qc model.qc --out <目录>`
# （QC 前端已实现；也可用 `mdlc qc2toml model.qc` 把它转成本格式）。
#
# 与 QC 的对应关系：
#   [model].name             <- $modelname
#   [model].static_prop      <- $staticprop
#   [model].surface_prop     <- $surfaceprop
#   [model].eye_position     <- $eyeposition
#   [model].illum_position   <- $illumposition
#   [model].contents         <- $contents
#   [physics].mass           <- $mass（在 $collisionmodel / $collisionjoints 块内）
#   [materials].search_paths <- $cdmaterials
#   [[bones]]                <- $definebone
#   [[bodyparts]]            <- $bodygroup + $body
#   [[bodyparts.models]].smd <- studio "x.smd"
#   [[bodyparts.models.lods]] <- $lod <距离> replacemodel <lodN.smd> <lod0.smd>
#   [[bones]].surface_prop   <- $jointsurfaceprop
#   [[ikchains]]             <- $ikchain
#   [[ik_autoplay_locks]]    <- $ikautoplaylock
#   [[sequences.ik_rules]]   <- $sequence 块内的 ikrule
#   [[sequences]].no_auto_ik <- $sequence 块内的 noautoik
#   [[weight_lists]]         <- $weightlist
#   [[sequences]].weight_list <- $sequence ... weightlist "名"
#   [[sequences]].forward_declared <- $declaresequence（空壳）

# ⚠️ 顶层裸键（上面这些 `key = value`）必须写在所有 `[[表]]` 之前。
#    这是 TOML 的作用域规则，不是 mdlc 的要求。
#    同理 `[[weight_lists]]` 放在这里也只是为了可读性 ——
#    它是**顶层**表，**不是** `[model]` 的字段。

[model]
# 输出路径，相对游戏目录。反斜杠会被自动规范化为正斜杠。
name = "models/mymod/myprop.mdl"
# 可省略，默认 49（L4D2）。支持 44 / 48 / 49。
# version = 49
# 可省略；省略时按模型名生成一个稳定值。
# 它不是内容哈希，只是 .mdl/.vvd/.vtx/.phy 四件套的配对令牌。
# checksum = 12345
static_prop = true
surface_prop = "metal"
# ⚠️ 质量**不在** [model] 里 —— 它在 [physics] 的 `mass`（对应
#    $collisionmodel / $collisionjoints 块内的 `$mass`）。在 [model] 下写
#    `mass = 1.0` 会因为 deny_unknown_fields 直接解析失败。
# 可省略，默认 1（CONTENTS_SOLID）。$contents 的 "solid" = 1，会写进头部与每根骨骼。
# contents = 1
# 可省略；省略时由 SMD 顶点自动计算包围盒。
# hull_min = [-8.0, -8.0, 0.0]
# hull_max = [ 8.0,  8.0, 16.0]
# 可省略。额外的 STUDIOHDR_FLAGS_* 位（static_prop 会自动置 0x10）。
# extra_flags = 0
#
# ---- 两个「优化 / 兜底」开关 ----
#
# 顶点缓存优化：重排每个 strip group 的索引顺序，让 GPU 的后变换顶点缓存
# 命中率更高。**只改索引顺序**，顶点池与三角形集合都不变（渲染结果相同）。
# 可省略，默认 false（保持与既有产物逐字节相同）。
# QC 侧等价命令：$optimizevtx（mdlc 扩展）；命令行：--optimize-vtx。
# optimize_vtx = true
#
# 顶点超限自动拆分：VTX 的 origMeshVertID 是 uint16 ⟹ **一个 mesh（= 一个
# 材质）最多 65536 个顶点**。打开本项时，超过上限的 mesh 会被**按三角形
# 顺序切成多个 mesh**，全部留在**同一个 model** 里、**共用原材质下标**，
# 所以不改变 $bodygroup 语义，渲染结果与拆分前逐像素相同。
# 可省略，**默认 true**。关掉它则遇到超限 mesh 直接报错。
# QC 侧等价命令：$nosplitoversizedmeshes 关 / $splitoversizedmeshes 开
# （都是 mdlc 扩展）。
# split_oversized_meshes = false

[materials]
# 材质搜索目录（$cdmaterials）。落盘时会规范化成反斜杠 + 结尾分隔符。
search_paths = ["models/mymod"]
# 材质表。SMD 里的材质名会按**首次出现顺序**映射到这里；
# 名字带 search_paths 前缀时落盘会自动剥掉（与 studiomdl 一致）。
textures = [
  { name = "models/mymod/myprop" },
]

# 骨骼：根骨骼必须排在最前；父骨骼只能指向更靠前的骨骼。
# position / rotation 留空时**自动取自 SMD 的 skeleton 第 0 帧**；
# 显式写出则覆盖 SMD 的值（SMD 的旋转是弧度，这里写角度）。
[[bones]]
name = "root"
# position = [0.0, 0.0, 0.0]
# rotation = [0.0, 0.0, 0.0]
# 可省略；省略时按**用途**逐根计算（被顶点使用 / 被 hitbox 使用 /
# 被 attachment 使用 / 被 ikchain 使用 / bonemerge）并沿父链向上传播，
# 与官方逐位一致。**不是**无条件的 0x500 —— 0x500
# （BONE_USED_BY_VERTEX_LOD0 | BONE_USED_BY_HITBOX）只是「该骨骼确实
# 被顶点与 hitbox 使用」时的结果，一律写它会把未被顶点使用的骨骼误标。
# flags = 1280
# 可省略；省略时继承 [model].surface_prop。
# surface_prop = "metal"
# $bonemerge：允许该骨骼被 bone merge（L4D2 survivor 模型靠它合并武器/手）。
# bonemerge = true

# ---------------------------------------------------------------------------
# 权重表（QC 的 `$weightlist "<名>" { <骨骼> <权重> ... }`）
#
# ⚠️ 这是**顶层**表（与 [[bones]] 同级），**不是** [model] 的字段 ——
#    它是模型级的表集合，序列按名字引用它。
#
# 用途：引擎做**增量动画（delta）的重建缩放**：
#     float s = panimation->weight[k];
#     QuaternionMA( q1, s, q2, q3 );      // q3 = q1 按 s 插值到 q2
#     p3 = base.pos + s * delta.pos;
# 所以 `s = 0` 的骨骼**完全不参与**该序列的增量叠加（保持基准姿态）。
# 模组作者用它做「只让上半身动」（真实例子：`v_katana` 的 `empty`、
# `v_autoshotgun` 的 `weights_fire_layer`）。
#
# ⚠️ 语义**不是**「没列出的骨骼就是 1」。官方算法（simplify.cpp:1646-1719）：
#     ① 具名表的**根骨骼默认 0**（不是 1），子骨骼初始为哨兵 -1；
#     ② 显式条目覆盖；
#     ③ 沿父链补齐：子骨骼**继承父骨骼**的值。
# 例：骨骼链 root → mid → leaf → tip，只写 `mid 0.5` 得到
#     [0, 0.5, 0.5, 0.5]（root 是 **0**，leaf/tip 跟着 mid 变 0.5）。
# 显式列出的骨骼能**覆盖**继承（再写 `tip 0` 就得到 [0, 0.5, 0.5, 0]）。
#
# 骨名与表名都是**大小写不敏感**的（官方用 stricmp）。
# 官方上限：每表 128 条、共 127 张表（**实测**，不是头文件里的 16/32）。
# 引用不存在的骨骼是**硬错误**（即使这张表没被任何序列引用）。
# ---------------------------------------------------------------------------
# [[weight_lists]]
# name = "upper"
#
# [[weight_lists.bones]]
# bone = "mid"
# weight = 0.5
# 位置权重（QC 的 posweight）。**不落盘** —— 只参与编译期 IK 误差计算。
# 缺省 = 与 weight 相同。
# pos_weight = 0.25

[[bodyparts]]
name = "body"
# bodygroup 预设的权重基数，可省略，默认 1。
base = 1

[[bodyparts.models]]
# 网格来源。相对路径以**描述文件所在目录**为基准。
# 这是 **LOD 0**（最精细）的网格。
smd = "myprop-ref.smd"
# 可省略。studiomdl 把源 SMD 文件名写进 mstudiomodel_t.name；
# 省略时自动取 smd 的文件名部分，与 studiomdl 行为一致。
# name = "myprop-ref.smd"

# ---------------------------------------------------------------------------
# 多 LOD（QC 的 $lod <距离> replacemodel <lodN.smd> <lod0.smd>）
#
# 每个 LOD 是一个**完整独立的 SMD**（不是「删掉一些三角形」）。
# 本实现把它们跨 LOD 精确去重合并成一个顶点池，按 LOD 归属排序，
# 并生成 fixup 表 —— 与 studiomdl 的 UnifyLODs 一致。
#
# 三条约束（违反会显式报错，不会静默贴错材质）：
#   1. 各 LOD 的**材质集合必须一致**（同一个 mesh 在各 LOD 里都得存在）；
#   2. 每个 LOD 的顶点数应当**单调不增**（LOD 越高越粗）；
#   3. 最多 8 层（含 LOD 0），引擎的 MAX_NUM_LODS 上限。
#
# 注意：本实现**不做网格简化** —— LOD 的 SMD 要你自己准备。
# 不写 lods 时走单 LOD 路径，产物与加这个特性之前逐字节相同。
# ---------------------------------------------------------------------------
# [[bodyparts.models.lods]]
# smd = "myprop-lod1.smd"
# 可省略。切换到该 LOD 的屏幕高度阈值（ModelLODHeader_t.switchPoint）。
# LOD 0 恒为 0；其余缺省按 20/40/80… 推算。
# 实测真实取值：10/15/20/25/30/40/50/60/65/80/100/150/200，以及 -1（不切换）。
# switch_point = 20.0

# [[bodyparts.models.lods]]
# smd = "myprop-lod2.smd"
# switch_point = 40.0

# ---------------------------------------------------------------------------
# hitbox（命中判定）。实测真实 L4D2 模型 100% 都有，没有它子弹打不中。
# 包围盒是**骨骼空间**的；可用 hlmv 的 hitbox 视图核对。
# ---------------------------------------------------------------------------
[hitboxes]
# hitbox set 名，可省略，默认 "default"。
# set_name = "default"

# [[hitboxes.boxes]]
# bone = "root"
# bbmin = [-8.0, -8.0, -8.0]
# bbmax = [ 8.0,  8.0,  8.0]
# group = 0            # 相交分组，可省略
# name = "body"        # hitbox 名，可省略

# ---------------------------------------------------------------------------
# 附着点（挂枪口火焰、弹壳抛出点等）。L4D2 全部武器模型依赖它。
# position / rotation 相对绑定的骨骼；rotation 是**角度**。
#
# `absolute` / `rigid` 对应官方的 `IS_ABSOLUTE` / `IS_RIGID` —— 它们
# **不落盘**（官方写进 `g_attachment[].type`，而 `write.cpp:350` 只写
# `flags`），但都影响产物：`absolute` 让 `local` 变成世界坐标下的绝对位姿
# （官方 `studiomdl.cpp:5246` + `simplify.cpp:5388`），`rigid` 让骨骼保活时
# 沿父链上溯到第一根被顶点引用的骨骼（`simplify.cpp:3472-3484`）。
#
# `flags` 直接落进 `mstudioattachment_t.flags`，官方那里**只**可能是
# `ATTACHMENT_FLAG_WORLD_ALIGN`（`0x10000`）。
# ---------------------------------------------------------------------------
# [[attachments]]
# name = "muzzle"
# bone = "tip"
# position = [0.0, 0.0, 8.0]
# rotation = [0.0, 0.0, 0.0]
# absolute = false          # 缺省 false；`absolute` 的旋转来自 g_defaultrotation
# rigid = false             # 缺省 false
# flags = 0                 # 缺省 0；`world_align` ⟹ 65536 (0x10000)

# ---------------------------------------------------------------------------
# $bonemerge：允许该骨骼被合并（L4D2 的 survivor 模型大量使用）。
# 写在 [[bones]] 里（`bonemerge = true`），不另设顶层键 ——
# TOML 的顶层键必须写在所有 [[表]] 之前，容易踩坑。
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# IK 链与 IK rule。L4D2 语料里 2.3% 的模型有 $ikchain、1.53% 有 ikrule。
#
# 自动补规则：**只要模型有 [[ikchains]]**，官方会给每个「没有任何显式
# ikrule 的链」自动追加一条 type = 4（IK_RELEASE）的规则
# （simplify.cpp:6254-6281）。所以绝大多数情况下你**不需要**写
# [[sequences.ik_rules]]，产物里就已经有正确的规则了。
# 要抑制它（对应 QC 的 noautoik）就在该序列上写 no_auto_ik = true。
#
# 官方 QC：
#   $ikchain "leg" "ankle" knee 0.5 0.5 0
#   $ikautoplaylock "leg" 1.0 0.1
#   $sequence idle "x.smd" { ikrule "leg" touch "knee" }
# ---------------------------------------------------------------------------
# [[ikchains]]
# name = "leg"          # 链名，ikrule / ikautoplaylock 按它引用
# bone = "ankle"        # **末端**骨骼；父（膝）与祖父（胯）自动推出
# knee_dir = [0.5, 0.5, 0.0]   # 对应 QC 的 knee <x y z>，可省略

# [[ik_autoplay_locks]]
# chain = "leg"         # 写链名，落盘时解析成链下标
# pos_weight = 1.0
# local_q_weight = 0.1

# ---------------------------------------------------------------------------
# [[sequences.ik_rules]] —— 每条规则一个表，顺序 = 写出顺序。
#
# type 取值与它对 QC 关键字的对应：
#   "touch"      <- ikrule <链> touch <骨骼>      误差 = 该骨骼与链末端的相对变换
#   "attachment" <- ikrule <链> attachment <名字>  + 可选的 target <槽位>
#   "release"    <- ikrule <链> release
#   "unlatch"    <- ikrule <链> unlatch
#   "footstep"   <- ikrule <链> footstep          **尚未实现**（语料 0 次）
#
# 省略 range 时 tail/end 自动展开成「末帧」（numframes − 1）。
# range 的四个数是**帧号**；写成 `range = [1, 1, 2, 3]`。
# ---------------------------------------------------------------------------
# [[sequences.ik_rules]]
# chain = "leg"
# type = "touch"
# bone = "knee"
# # 以下全部可省略（省略 = 官方默认）
# # target = 0                   # slot，缺省 = 链下标
# # range = [0, 0, 3, 3]         # [start, peak, tail, end]，帧号
# # contact = 2                  # 帧号，缺省 = peak
# # height = 12.0 / radius = 4.0 / pad = 8.0 / floor = 0.0
# # fake_origin = [0.0, 0.0, 0.0]   # 强制 pos 并把 bone 置 -1
# # fake_rotate = [0.0, 0.0, 0.0]   # 度；强制 q 并把 bone 置 -1

# ---------------------------------------------------------------------------
# 序列。单动画序列写 `smd = "..."`；blend 序列写 `blends = [...]`。
# ---------------------------------------------------------------------------
# [[sequences]]
# name = "idle"
# smd = "idle.smd"
# fps = 30.0
# looping = true
# # 按名字引用 [[weight_lists]] 里的表；缺省 = 隐式全 1 表。
# # 表名大小写不敏感。引用不存在的表会报错（官方也是硬错误）。
# weight_list = "upper"

# ---------------------------------------------------------------------------
# `$declaresequence` —— **前向声明的空壳序列**（survivor 模组核心机制）。
#
# 主模型里声明一堆空名字，真正的动画在 `$includemodel` 进来的
# `anim_<survivor>.mdl` 里；引擎加载时按名字**替换**掉空壳
# （`studio_virtualmodel.cpp:185` 的 `STUDIO_OVERRIDE` 分支）。
# 于是网格/骨骼/flex 与几百条动画可以**分开发布**。
#
# ⚠️ 空壳**不要写 smd**（官方连 `panim` 都不分配），写了会被 `validate()` 拒。
# ⚠️ 空壳**不能**用 `$sequence <同名>` 去填 —— 官方会报
#    `no animations found`（`ParseSequence` 检查 `numblends == 0`）。
#    填充是**引擎运行时**做的事，不是编译期。
# 实测真实工程：`Zoey_$DeclareSequence.qci` 933 条空壳，
# 编出的 `.mdl` 里 `numlocalseq = 936`（含 3 条实体序列）。
# ---------------------------------------------------------------------------
# [[sequences]]
# name = "Idle_Standing_Align"
# forward_declared = true
"#;

/// 校验/规范化时发现的错误。
///
/// # 为什么带 `kind`
///
/// 先前它只有 `path` + `message: String`，调用方想知道「到底是哪一类问题」
/// 只能去 `message.contains("找不到骨骼")` —— 而 `message` 是**给终端用户看的
/// 中文**，改一个字的措辞就会让下游的字符串匹配静默失效。
///
/// [`DescErrorKind`] 把**反复出现的几类**变成可 `match` 的变体，其余落到
/// [`DescErrorKind::Other`]（它仍是字符串 —— 那些检查只产出一句话，
/// 没有更细的结构可暴露）。
///
/// ⚠️ **文案逐字节未变**：每个变体的 `Display` 都复刻了原先的 `format!`，
/// 所以 `to_string()` 与改造前完全相同。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{path}: {kind}")]
pub struct DescError {
    /// 出错的字段路径，如 `bodyparts[0].models[1].meshes[0].triangles[3]`。
    pub path: String,
    /// 问题种类。
    pub kind: DescErrorKind,
}

/// [`DescError`] 的问题种类。
///
/// 变体只覆盖**反复出现**的几类（实测 48 个校验点里 35 个落在前八个变体上）；
/// 剩下的 13 个各自只有一句话，落在 [`Self::Other`]。
///
/// 判据：**`path` 与种类正交** —— `path` 是运行时拼出来的
/// （`format!("{bpath}.models[{mi}]")` 之类），而**每个变体都需要它**，
/// 所以它是 [`DescError`] 的字段而不是变体载荷。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescErrorKind {
    /// 必填字段为空。
    ///
    /// `message` 保留原文 —— 各处的措辞并不统一（"must not be empty"、
    /// "the mesh name must not be empty"、"must not be empty -- it must
    /// point to an SMD file"…）。归一化会改动既有文案，而 `match` 看的是
    /// **变体**，原话用 `to_string()` 就能读到。
    ///
    /// `message` 是**可翻译片段**：值本身是英文源文，渲染时经
    /// [`crate::tr`] 过一遍译文表（与 [`Self::BoneNotFound`] 的
    /// `what` / `note` 同一个技巧）。它是 `&'static str`，所以能在属性里
    /// 直接过译文表 —— 不像 [`Self::Other`] 那样只能在构造点翻译。
    #[error("{}", crate::tr(message))]
    Empty { message: &'static str },

    /// 引用了不存在的骨骼。
    ///
    /// `what` 是限定语（`"parent "` → 「找不到父骨骼」，空串 → 「找不到骨骼」），
    /// `note` 是少数几处额外的补充说明。**两者都是可翻译片段** ——
    /// 值本身是英文源文，渲染时经 [`crate::tr`] 过一遍译文表。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Cannot find the %{what}bone %{bone}%{note}",
            what = crate::tr(what),
            bone = format!("{bone:?}"),
            note = crate::tr(note)
        )
    )]
    BoneNotFound {
        what: &'static str,
        bone: String,
        note: &'static str,
    },

    /// 父骨骼排在本骨骼之后（官方要求父骨骼必须先出现）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "The parent bone %{bone} comes after this bone; parents must come first",
            bone = format!("{bone:?}")
        )
    )]
    ParentBoneOutOfOrder { bone: String },

    /// 引用了不存在的权重表。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Cannot find the weight list %{name}",
            name = format!("{name:?}")
        )
    )]
    WeightListNotFound { name: String },

    /// 名字重复。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Duplicate %{what}: %{name}",
            what = crate::tr(what),
            name = format!("{name:?}")
        )
    )]
    Duplicate { what: &'static str, name: String },

    /// 数值不是有限数（NaN / ±inf）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Not a finite number%{value}",
            value = .value.as_ref().map(|v| crate::tr_fmt!(": %{v}", v = v)).unwrap_or_default()
        )
    )]
    NotFinite { value: Option<String> },

    /// 数值必须是**正**有限数（`fps` / `src_scale` / `src_fps` 之类）。
    ///
    /// `detail` 是「实际」与数值之间的那个连接词 —— 历史上有「实际**为** x」
    /// 与「实际 x」两种措辞，都在用，所以原样保留。它也是可翻译片段。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Must be a positive finite number, but got %{detail}%{value}",
            detail = crate::tr(detail),
            value = value
        )
    )]
    NotPositive { detail: &'static str, value: String },

    /// 版本号不在支持范围（44 / 48 / 49）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Only 44 / 48 / 49 are supported, but the version is %{version}",
            version = version
        )
    )]
    UnsupportedVersion { version: i32 },

    /// 其余检查：只产出一句话，没有更细的结构。
    ///
    /// ⚠️ `message` 是**运行时 `format!` 出来的**（带数值、带常量），
    /// 所以属性里写不出 `crate::tr(message)` —— 译文必须在**构造点**
    /// 用 [`crate::tr_fmt`] 做好。改这里的文案要连着改构造点。
    #[error("{message}")]
    Other { message: String },
}

/// TOML 解析 / 序列化的失败原因。
///
/// # 为什么不是 `String`
///
/// [`ModelDesc::from_toml`] 与 [`ModelDesc::to_toml`] 是 README「作为依赖使用」
/// 一节教第三方调用的入口，而 `toml` 自己的错误对象**带出错位置的 span**
/// （[`toml::de::Error::span`] 给出源文本里的字节区间，[`toml::de::Error::message`]
/// 给出不含行列号的纯描述）。先前实现用 `format!("TOML 解析失败：{e}")` 把它
/// 压成字符串，调用方就再也拿不到位置 —— GUI 想高亮出错的那一列只能去
/// 正则匹配中文前缀。这里把源错误**原样保留**，`Display` 仍是原来的文案。
///
/// ```no_run
/// # use mdlc::model::{ModelDesc, TomlError};
/// # let text = "version = 49\n";
/// match ModelDesc::from_toml(text) {
///     Err(TomlError::Parse(e)) => {
///         if let Some(span) = e.span() {
///             println!("出错位置：{}..{}", span.start, span.end);
///         }
///         println!("{}", e.message());
///     }
///     _ => {}
/// }
/// ```
///
/// ⚠️ [`Self::Parse`] 只可能来自 [`ModelDesc::from_toml`]，[`Self::Serialize`]
/// 只可能来自 [`ModelDesc::to_toml`] —— 两类操作各自只有一个失败分支。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TomlError {
    /// `toml::from_str` 失败（语法错 / 未知字段 / 类型不符）。
    ///
    /// 源错误里保留着行列信息：`Display` 会渲染成带 `^` 指示的多行文本。
    #[error("{}", crate::tr_fmt!("TOML parse failed: %{err}", err = .0))]
    Parse(#[from] toml::de::Error),
    /// `toml::to_string_pretty` 失败。
    #[error("{}", crate::tr_fmt!("TOML serialization failed: %{err}", err = .0))]
    Serialize(#[from] toml::ser::Error),
}

/// 顶点允许的最大骨骼数（`MAX_NUM_BONES_PER_VERT`）。
pub const MAX_BONES_PER_VERT: usize = 3;

impl ModelDesc {
    pub fn hitbox_sets(&self) -> impl Iterator<Item = &Hitboxes> {
        std::iter::once(&self.hitboxes)
            .chain(&self.additional_hitbox_sets)
            .filter(|set| set.set_name.is_some() || set.autogenerated || !set.boxes.is_empty())
    }

    /// 从 TOML 文本解析。
    ///
    /// 失败时给出 [`TomlError`] —— 它**保留着 `toml` 的源错误**，所以调用方
    /// 能拿到出错位置的 span，而不是只有一个字符串。
    pub fn from_toml(text: &str) -> Result<Self, TomlError> {
        Ok(toml::from_str(text)?)
    }

    /// 序列化为 TOML 文本（`mdlc qc2toml` 靠它把 QC 落成描述文件）。
    pub fn to_toml(&self) -> Result<String, TomlError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// MDL 版本（默认 49）。
    pub fn version(&self) -> i32 {
        self.model.version.unwrap_or(49)
    }

    /// checksum：写了就用写的，否则用模型名的哈希生成一个**稳定**的值。
    ///
    /// 稳定性很重要：同一份描述两次编译必须得到同一个 checksum，
    /// 否则「重编译后旧的四件套还能配对」这件事就不成立。
    /// 用 FNV-1a 而不是 `DefaultHasher` —— 后者不保证跨进程稳定。
    pub fn checksum(&self) -> i32 {
        if let Some(c) = self.model.checksum {
            return c;
        }
        let mut h: u32 = 0x811c_9dc5;
        for b in self.model.name.as_bytes() {
            h ^= *b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h as i32
    }

    /// 输出路径（正斜杠规范化）。
    pub fn output_name(&self) -> String {
        self.model.name.replace('\\', "/")
    }

    pub(crate) fn valid_output_name(name: &str) -> bool {
        let normalized = name.replace('\\', "/");
        !normalized.is_empty()
            && !normalized.contains([':', '\0'])
            && normalized.split('/').all(|part| {
                !part.is_empty() && part != "." && part != ".." && !part.trim().is_empty()
            })
    }

    /// Named sequence references update a shared animation before source loading.
    pub(crate) fn apply_sequence_source_overrides(&mut self) {
        for sequence in &self.sequences {
            let reference = sequence.blends.first().unwrap_or(&sequence.smd);
            if let Some(animation) = self
                .animations
                .iter_mut()
                .find(|a| a.name.eq_ignore_ascii_case(reference))
            {
                if sequence.frames.is_some() {
                    animation.frames = sequence.frames;
                }
                if sequence.scale.is_some() {
                    animation.scale = sequence.scale;
                }
                if sequence.adjust.is_some() {
                    animation.adjust = sequence.adjust;
                }
                if sequence.rotation.is_some() {
                    animation.rotation = sequence.rotation;
                }
            }
        }
    }

    /// 骨骼名 → 下标。
    pub fn bone_index(&self) -> std::collections::HashMap<&str, usize> {
        self.bones
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name.as_str(), i))
            .collect()
    }

    /// 顶点总数（全部 body part / model / mesh 之和）。
    ///
    /// 网格来自 SMD，这里返回的是**描述文件里能声明的量**：0。
    /// 真实顶点数请用 [`CompiledModelDesc::total_vertices`]。
    pub fn total_vertices(&self) -> usize {
        0
    }

    /// 三角形总数。同 [`Self::total_vertices`] —— 网格在 SMD 里。
    pub fn total_triangles(&self) -> usize {
        0
    }

    /// 全面校验。
    ///
    /// **宁可报错也不要写出坏文件**：这里检查的每一条，如果漏掉，
    /// 产出的模型要么在游戏里扭曲、要么让 studiomdl/引擎拒绝加载，
    /// 而且都不会给出指向根因的提示。
    pub fn validate(&self) -> Result<(), Vec<DescError>> {
        let mut errs = Vec::new();

        for (index, limit) in self.model.rotation_limits.iter().enumerate() {
            if self.model.static_prop
                || !self
                    .bones
                    .iter()
                    .any(|bone| bone.name.eq_ignore_ascii_case(&limit.bone))
            {
                errs.push(DescError {
                    path: format!("model.rotation_limits[{index}]"),
                    kind: DescErrorKind::Other {
                        message: "$limitrotation requires an existing bone in a nonstatic model"
                            .into(),
                    },
                });
            }
        }

        if self.model.static_prop && !self.model.forced_realign.is_empty() {
            errs.push(DescError {
                path: "model.forced_realign".into(),
                kind: DescErrorKind::Other {
                    message: "$forcerealign cannot target a skeleton collapsed by $staticprop"
                        .into(),
                },
            });
        }
        if !self.model.forced_realign.is_empty() {
            let unsupported_command = |command: &AnimCmd| {
                !matches!(
                    command,
                    AnimCmd::Weights { .. }
                        | AnimCmd::IkRule { .. }
                        | AnimCmd::IkFixup { .. }
                        | AnimCmd::NoAnimation
                        | AnimCmd::Compress { .. }
                        | AnimCmd::NumFrames { .. }
                )
            };
            if self.animations.iter().any(|animation| {
                animation.subtract.is_some()
                    || animation.extra_flags & 0x0004 != 0
                    || animation.motion_type != 0
                    || animation.cmds.iter().any(unsupported_command)
            }) || self.sequences.iter().any(|sequence| {
                sequence.delta
                    || sequence.extra_flags.unwrap_or(0) & 0x0004 != 0
                    || sequence.subtract.is_some()
                    || sequence.motion_type != 0
                    || sequence.cmds.iter().any(unsupported_command)
            }) {
                errs.push(DescError {
                    path: "model.forced_realign".into(),
                    kind: DescErrorKind::Other {
                        message: "$forcerealign with delta, root motion, or coordinate-changing animation commands is not supported".into(),
                    },
                });
            }
        }
        for (index, forced) in self.model.forced_realign.iter().enumerate() {
            if !self
                .bones
                .iter()
                .any(|bone| bone.name.eq_ignore_ascii_case(&forced.bone))
                || forced.rotation.iter().any(|value| !value.is_finite())
            {
                errs.push(DescError {
                    path: format!("model.forced_realign[{index}]"),
                    kind: DescErrorKind::Other {
                        message: "Forced realignment requires an existing bone and finite rotation"
                            .into(),
                    },
                });
            }
        }
        for (index, [child, parent]) in self.model.bone_hierarchy.iter().enumerate() {
            if self.model.bone_hierarchy[index + 1..]
                .iter()
                .any(|[later, _]| later.eq_ignore_ascii_case(child))
            {
                continue;
            }
            let valid = self
                .bones
                .iter()
                .find(|bone| bone.name.eq_ignore_ascii_case(child))
                .is_some_and(|bone| {
                    bone.parent
                        .as_deref()
                        .unwrap_or("")
                        .eq_ignore_ascii_case(parent)
                });
            if !valid {
                errs.push(DescError {
                    path: format!("model.bone_hierarchy[{index}]"),
                    kind: DescErrorKind::Other {
                        message: "Hierarchy must match the resolved model skeleton".into(),
                    },
                });
            }
        }

        for (name, value) in [
            ("model.source_origin", self.model.source_origin),
            ("model.source_rotation", self.model.source_rotation),
        ] {
            if value.is_some_and(|value| value.iter().any(|v| !v.is_finite())) {
                errs.push(DescError {
                    path: name.into(),
                    kind: DescErrorKind::Other {
                        message: "must contain finite coordinates".into(),
                    },
                });
            }
        }

        if self.model.name.trim().is_empty() {
            errs.push(DescError {
                path: "model.name".into(),
                kind: DescErrorKind::Empty {
                    message: "must not be empty",
                },
            });
        }
        if !Self::valid_output_name(&self.model.name) {
            errs.push(DescError {
                path: "model.name".into(),
                kind: DescErrorKind::Other {
                    message: "must be a relative model path without empty, '.' or '..' components, drive prefixes or NUL characters".into(),
                },
            });
        }
        if !matches!(self.version(), 44 | 48 | 49) {
            errs.push(DescError {
                path: "model.version".into(),
                kind: DescErrorKind::UnsupportedVersion {
                    version: self.version(),
                },
            });
        }

        match (self.model.view_bb_min, self.model.view_bb_max) {
            (None, None) => {}
            (Some(min), Some(max)) if (0..3).all(|axis| {
                min[axis].is_finite() && max[axis].is_finite() && min[axis] <= max[axis]
            }) => {}
            _ => errs.push(DescError {
                path: "model.view_bb_min/view_bb_max".into(),
                kind: DescErrorKind::Other {
                    message: "clipping bounds require both finite ordered minimum and maximum coordinates".into(),
                },
            }),
        }

        // ---- 骨骼 ----
        if self.bones.is_empty() {
            errs.push(DescError {
                path: "bones".into(),
                kind: DescErrorKind::Other {
                    message: "at least one bone is required (even a static prop needs its \
                              single root bone)"
                        .into(),
                },
            });
        }
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (i, b) in self.bones.iter().enumerate() {
            let path = format!("bones[{i}]");
            if b.name.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty",
                    },
                });
            }
            if seen.insert(b.name.as_str(), i).is_some() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Duplicate {
                        what: "bone name",
                        name: b.name.clone(),
                    },
                });
            }
            if let Some(p) = &b.parent {
                match seen.get(p.as_str()) {
                    // 父骨骼必须先出现：这是 studiomdl 的硬性要求，
                    // 也是「参考姿态按顺序求解」的前提。
                    Some(&pi) if pi < i => {}
                    Some(_) => errs.push(DescError {
                        path: format!("{path}.parent"),
                        kind: DescErrorKind::ParentBoneOutOfOrder { bone: p.clone() },
                    }),
                    None => errs.push(DescError {
                        path: format!("{path}.parent"),
                        kind: DescErrorKind::BoneNotFound {
                            what: "parent ",
                            bone: p.clone(),
                            note: "",
                        },
                    }),
                }
            }
            for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                let bad = b.position.is_some_and(|p| !p[k].is_finite())
                    || b.rotation.is_some_and(|r| !r[k].is_finite());
                if bad {
                    errs.push(DescError {
                        path: format!("{path}.{axis}"),
                        kind: DescErrorKind::NotFinite { value: None },
                    });
                }
            }
        }

        // ---- body part / model / mesh ----
        //
        // ⚠️ **故意不检查 `bodyparts` 为空** —— 官方没有这个检查，而且
        // **纯动画工程**（只有 `$modelname` + `$sequence`/`$animation`，
        // 没有任何 `$body`/`$model`/`$bodygroup`）是**合法输入**。
        //
        // 官方证据（`docs/_probe/oracle_no_body.js`，真 `studiomdl.exe` 实测）：
        //   * 同一份 QC **去掉** `$body` 那行 ⟹ `exit=0`、产出 MDL 1048 B、
        //     `numbodyparts=0`、**不产出 `.vvd`/`.dx90.vtx`**；
        //   * **加回** `$body` ⟹ `exit=0`、MDL 2200 B、`numbodyparts=1`、VVD+VTX 都有。
        //   * 源码侧：`write.cpp:2203-2204` `if (phdr->numbodyparts == 0) return;`
        //     —— **优雅早退**（跳过 `WriteVertices`），不是报错；
        //     `write.cpp:1462` `phdr->numbodyparts = IsChar( g_numbodyparts );`
        //     直接写 0；`studiomdl.cpp:995`/`:1040`/`:4238` 的
        //     `if (g_numbodyparts == 0)` 是 `base = 1` 的**初始化**，不是校验。
        //
        // 实测用户工程 `incap_anim_fix`（`$modelname survivors/anim_zoey_float_fix.mdl`
        // + 22 条 `$sequence`，零 `$body`）原本就卡在这条过严的校验上。
        for (bi, bp) in self.bodyparts.iter().enumerate() {
            let bpath = format!("bodyparts[{bi}]");
            if bp.name.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{bpath}.name"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty",
                    },
                });
            }
            if bp.models.is_empty() {
                errs.push(DescError {
                    path: format!("{bpath}.models"),
                    kind: DescErrorKind::Other {
                        message: "at least one model is required".into(),
                    },
                });
            }
            for (mi, m) in bp.models.iter().enumerate() {
                let mpath = format!("{bpath}.models[{mi}]");
                // 网格在 SMD 里，这里只校验引用本身是否说得通。
                if m.smd.trim().is_empty()
                    && !(m.smd.is_empty() && m.name.as_deref() == Some("blank"))
                {
                    errs.push(DescError {
                        path: format!("{mpath}.smd"),
                        kind: DescErrorKind::Empty {
                            message: "must not be empty -- it must point to an SMD file",
                        },
                    });
                }
                if let Some(n) = &m.name
                    && n.len() >= 64
                {
                    errs.push(DescError {
                        path: format!("{mpath}.name"),
                        kind: DescErrorKind::Other {
                            message: crate::tr_fmt!(
                                "too long: %{len} bytes, limit 63",
                                len = n.len()
                            ),
                        },
                    });
                }
                // ---- LOD ----
                // 引擎的 `MAX_NUM_LODS` 是 8，含 LOD 0 在内最多 8 层。
                if m.lods.len() + 1 > crate::vvd::MAX_NUM_LODS {
                    errs.push(DescError {
                        path: format!("{mpath}.lods"),
                        kind: DescErrorKind::Other {
                            message: crate::tr_fmt!(
                                "too many LODs: %{count} levels (LOD 0 included), the engine limit is %{max}",
                                count = m.lods.len() + 1,
                                max = crate::vvd::MAX_NUM_LODS
                            ),
                        },
                    });
                }
                for (li, lod) in m.lods.iter().enumerate() {
                    let lpath = format!("{mpath}.lods[{li}]");
                    // `smd` 可省略（= 复用 LOD 0 的网格 + 只应用骨骼选项），
                    // 但写了就必须非空且不等于 LOD 0。
                    if let Some(s) = lod.smd.as_deref() {
                        if s.trim().is_empty() {
                            errs.push(DescError {
                                path: format!("{lpath}.smd"),
                                kind: DescErrorKind::Empty {
                                    message: "must not be empty -- either omit it (to reuse \
                                              the LOD 0 mesh) or point it to an SMD file",
                                },
                            });
                        } else if s == m.smd && !lod.reverse {
                            errs.push(DescError {
                                path: format!("{lpath}.smd"),
                                kind: DescErrorKind::Other {
                                    message: crate::tr_fmt!(
                                        "same as the LOD 0 smd (%{smd}) -- for the same mesh \
                                         **omit** smd and only write the bone options",
                                        smd = format!("{:?}", m.smd)
                                    ),
                                },
                            });
                        }
                    }
                    // 骨骼选项引用必须在 [[bones]] 里存在 ——
                    // 官方对未知骨骼只打 warning 然后**整块变 no-op**，
                    // 静默失效最难排查，所以这里升级成错误。
                    for b in lod
                        .bone_tree_collapse
                        .iter()
                        .chain(lod.replace_bone.iter().flat_map(|p| p.iter()))
                    {
                        if !seen.contains_key(b.as_str()) {
                            errs.push(DescError {
                                path: format!("{lpath}.bone_tree_collapse/replace_bone"),
                                kind: DescErrorKind::BoneNotFound {
                                    what: "",
                                    bone: b.clone(),
                                    note: "",
                                },
                            });
                        }
                    }
                    if let Some(sp) = lod.switch_point
                        && !sp.is_finite()
                    {
                        errs.push(DescError {
                            path: format!("{lpath}.switch_point"),
                            kind: DescErrorKind::NotFinite {
                                value: Some(format!("{sp}")),
                            },
                        });
                    }
                }
            }
        }

        for (index, chain) in self.ikchains.iter().enumerate() {
            if chain
                .center
                .is_some_and(|center| center.iter().any(|v| !v.is_finite()))
                || [chain.height, chain.floor, chain.pad]
                    .into_iter()
                    .flatten()
                    .any(|value| !value.is_finite())
                || chain.pad.is_some_and(|value| value < 0.0)
            {
                errs.push(DescError {
                    path: format!("ikchains[{index}]"),
                    kind: DescErrorKind::Other {
                        message: "IK chain settings must be finite with a nonnegative pad".into(),
                    },
                });
            }
        }

        for (index, controller) in self.bonecontrollers.iter().enumerate() {
            if controller.axis_slot().is_none()
                || !seen.contains_key(controller.bone.as_str())
                || !(0..=4).contains(&controller.input_field.unwrap_or(0))
                || !(0..=255).contains(&controller.rest.unwrap_or(0))
                || !controller.start.unwrap_or(0.0).is_finite()
                || !controller.end.unwrap_or(0.0).is_finite()
            {
                errs.push(DescError {
                    path: format!("bonecontrollers[{index}]"),
                    kind: DescErrorKind::Other {
                        message: "invalid bone controller bone, axis, input, rest value, or bounds"
                            .into(),
                    },
                });
            }
        }

        // ---- hitbox ----
        for (i, hb) in self.hitbox_sets().flat_map(|set| &set.boxes).enumerate() {
            let path = format!("hitboxes.boxes[{i}]");
            if !seen.contains_key(hb.bone.as_str()) {
                errs.push(DescError {
                    path: format!("{path}.bone"),
                    kind: DescErrorKind::BoneNotFound {
                        what: "",
                        bone: hb.bone.clone(),
                        note: "",
                    },
                });
            }
            for (k, axis) in ["x", "y", "z"].iter().enumerate() {
                if !hb.bbmin[k].is_finite() || !hb.bbmax[k].is_finite() {
                    errs.push(DescError {
                        path: format!("{path}.{axis}"),
                        kind: DescErrorKind::NotFinite { value: None },
                    });
                }
            }
            // 包围盒必须非退化 —— 零体积的 hitbox 等于没有命中判定。
            if (0..3).all(|k| hb.bbmin[k] >= hb.bbmax[k]) {
                errs.push(DescError {
                    path: format!("{path}.bbmin"),
                    kind: DescErrorKind::Other {
                        message: "degenerate bounding box (every bbmin component is >= bbmax)"
                            .into(),
                    },
                });
            }
        }

        // ---- attachment ----
        let mut at_names: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (i, at) in self.attachments.iter().enumerate() {
            let path = format!("attachments[{i}]");
            if at.name.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty",
                    },
                });
            }
            // 合成附着点（`$illumposition x y z <骨骼>`）两条都豁免：
            // 官方允许重名（`oracle_illumposition5.js` 的 `illum_twice`），
            // 且它绑的骨骼可能被收骨判据丢掉后重映射到骨骼 0
            // （`oracle_illumposition8/10.js`）。详见 [`Attachment::synthetic`]。
            if !at.synthetic && at_names.insert(at.name.as_str(), i).is_some() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Duplicate {
                        what: "attachment name",
                        name: at.name.clone(),
                    },
                });
            }
            if !at.synthetic && !seen.contains_key(at.bone.as_str()) {
                errs.push(DescError {
                    path: format!("{path}.bone"),
                    kind: DescErrorKind::BoneNotFound {
                        what: "",
                        bone: at.bone.clone(),
                        note: "",
                    },
                });
            }
        }

        // ---- bonemerge ----
        // 已并入骨骼字段，无需单独校验（bool 不会指向不存在的骨骼）。

        // ---- sequences ----
        let mut seq_names: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (i, s) in self.sequences.iter().enumerate() {
            let path = format!("sequences[{i}]");
            if s.name.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty",
                    },
                });
            }
            // 重名判定**只对实体序列**生效 —— 官方 `Cmd_Sequence` 走
            // `LookupAnimation`（查的是**动画池**），空壳不在池里
            // （`Cmd_DeclareSequence` 不分配 `panim`），所以
            // 「先 `$sequence x` 再 `$declaresequence x`」官方**不报错**，
            // 产出两条同名序列（实测 `fill_then_declare` → 2 条）。
            if !s.forward_declared && seq_names.insert(s.name.as_str(), i).is_some() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Duplicate {
                        what: "sequence name",
                        name: s.name.clone(),
                    },
                });
            }
            // `$declaresequence` 的空壳**没有 SMD**（官方连 `panim` 都不分配）。
            // 除它以外，`smd` 必须非空。
            if s.forward_declared {
                // 空壳不该带实体序列的字段 —— 带了说明解析器或手写 TOML
                // 把两种东西混在了一起，产物会**看起来对但语义错**。
                if !s.smd.trim().is_empty() {
                    errs.push(DescError {
                        path: format!("{path}.smd"),
                        kind: DescErrorKind::Other {
                            message: "the empty shell of a forward declaration \
                                      (`$declaresequence`) must not have an smd"
                                .into(),
                        },
                    });
                }
                continue;
            }
            if s.smd.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{path}.smd"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty -- it must point to an SMD with a multi-frame skeleton",
                    },
                });
            }
            if let Some(fps) = s.fps
                && (!fps.is_finite() || fps <= 0.0)
            {
                errs.push(DescError {
                    path: format!("{path}.fps"),
                    kind: DescErrorKind::NotPositive {
                        detail: "",
                        value: format!("{fps}"),
                    },
                });
            }
        }

        // ---- 权重表（`$weightlist`）----
        //
        // 官方对这里的**每一条**都是 `MdlError`（硬错误），不是 warning ——
        // 与 `$lod` 的未知骨骼（warning + 整块 no-op）**完全不同**。
        // 实测（`docs/_probe/cmp_weightlist_errors.js`，**11 个用例**与官方判定一致）：
        //
        // * `unknown bone reference '%s' in weightlist '%s'`（`simplify.cpp:1697`）
        //   —— 即使这张表**没被任何序列引用**也照样报错，
        //   因为 `buildAnimationWeights` 遍历的是**全部**表。
        // * `unknown weightlist '%s'`（`studiomdl.cpp:1728`）—— 序列/动画引用不存在的表。
        // * `Duplicate weightlist`（`studiomdl.cpp:3344`）—— 解析期就报。
        // * `Too many bones (128)`（`MAXWEIGHTSPERLIST`，**实测值**见常量说明）。
        // * `Too many weightlist commands (128)`（`MAXWEIGHTLISTS`，**实测值**）。
        //
        // ⚠️ 骨骼名匹配用**大小写不敏感**（官方 `findGlobalBone` 是 `stricmp`，
        // `simplify.cpp:2628`）。用 `bone_index()`（大小写敏感）会误报。
        let bone_ci: std::collections::HashMap<String, usize> = self
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name.to_ascii_lowercase(), i))
            .collect();
        let mut wl_names: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        // 官方上限含**隐式的表 0**，所以手写表最多 `MAX_WEIGHT_LISTS - 1` 张。
        if self.weight_lists.len() >= MAX_WEIGHT_LISTS {
            errs.push(DescError {
                path: "weight_lists".into(),
                kind: DescErrorKind::Other {
                    message: crate::tr_fmt!(
                        "too many tables: %{count}, the official limit is %{max} (the implicit \
                         default table 0 is included, so at most %{hand} may be written by hand)",
                        count = self.weight_lists.len(),
                        max = MAX_WEIGHT_LISTS,
                        hand = MAX_WEIGHT_LISTS - 1
                    ),
                },
            });
        }
        let default_list = WeightList {
            name: String::new(),
            bones: self.default_weight_list.clone(),
        };
        for (i, wl) in std::iter::once(&default_list)
            .chain(self.weight_lists.iter())
            .enumerate()
        {
            let path = if i == 0 {
                "default_weight_list".to_owned()
            } else {
                format!("weight_lists[{}]", i - 1)
            };
            if i != 0 && wl.name.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty",
                    },
                });
            }
            if i != 0 && wl_names.insert(wl.name.to_ascii_lowercase(), i).is_some() {
                errs.push(DescError {
                    path: format!("{path}.name"),
                    kind: DescErrorKind::Duplicate {
                        what: "weight list name",
                        name: wl.name.clone(),
                    },
                });
            }
            // 官方上限 —— **实测值 128**，不是 `studiomdl.h:980` 写的 16。
            // 见 [`MAX_WEIGHT_ENTRIES`] 的说明：按 16 写会把真实模组误判为非法。
            if wl.bones.len() > MAX_WEIGHT_ENTRIES {
                errs.push(DescError {
                    path: format!("{path}.bones"),
                    kind: DescErrorKind::Other {
                        message: crate::tr_fmt!(
                            "too many entries: %{count}, the official limit is %{max}",
                            count = wl.bones.len(),
                            max = MAX_WEIGHT_ENTRIES
                        ),
                    },
                });
            }
            for (j, e) in wl.bones.iter().enumerate() {
                if !bone_ci.contains_key(&e.bone.to_ascii_lowercase()) {
                    errs.push(DescError {
                        path: format!("{path}.bones[{j}].bone"),
                        kind: DescErrorKind::BoneNotFound {
                            what: "",
                            bone: e.bone.clone(),
                            note: " -- the official code raises MdlError \
                                   (`unknown bone reference`) here, not a warning",
                        },
                    });
                }
                for (v, what) in [(e.weight, "weight"), (e.pos_weight(), "pos_weight")] {
                    if !v.is_finite() {
                        errs.push(DescError {
                            path: format!("{path}.bones[{j}].{what}"),
                            kind: DescErrorKind::NotFinite { value: None },
                        });
                    }
                }
            }
        }
        // 序列/动画引用的表必须存在（官方 `unknown weightlist`）。
        for (i, s) in self.sequences.iter().enumerate() {
            if let Some(n) = &s.weight_list
                && !wl_names.contains_key(&n.to_ascii_lowercase())
            {
                errs.push(DescError {
                    path: format!("sequences[{i}].weight_list"),
                    kind: DescErrorKind::WeightListNotFound { name: n.clone() },
                });
            }
        }
        for (i, a) in self.animations.iter().enumerate() {
            if let Some(n) = &a.weight_list
                && !wl_names.contains_key(&n.to_ascii_lowercase())
            {
                errs.push(DescError {
                    path: format!("animations[{i}].weight_list"),
                    kind: DescErrorKind::WeightListNotFound { name: n.clone() },
                });
            }
        }

        // ---- jiggle bones ----
        // 两种输入形式互斥：`writes`（QC 的无损日志）与三个块（TOML 便捷语法）。
        // 同时给出会让「谁说了算」变成隐式规则，宁可报错。
        for (i, j) in self.jiggle_bones.iter().enumerate() {
            let path = format!("jiggle_bones[{i}]");
            if j.bone.trim().is_empty() {
                errs.push(DescError {
                    path: format!("{path}.bone"),
                    kind: DescErrorKind::Empty {
                        message: "must not be empty",
                    },
                });
            }
            let has_blocks =
                j.is_flexible.is_some() || j.is_rigid.is_some() || j.has_base_spring.is_some();
            if !j.writes.is_empty() && has_blocks {
                errs.push(DescError {
                    path: path.clone(),
                    kind: DescErrorKind::Other {
                        message: "`writes` and `is_flexible`/`is_rigid`/`has_base_spring` \
                                  cannot be given together: `writes` is the QC's lossless write \
                                  log (order included), while the three blocks are a convenience \
                                  syntax merged in a fixed order -- the two mean different things"
                            .into(),
                    },
                });
            }
            for (k, w) in j.writes.iter().enumerate() {
                if !KNOWN_JIGGLE_KEYS.contains(&w.key.as_str()) {
                    errs.push(DescError {
                        path: format!("{path}.writes[{k}].key"),
                        kind: DescErrorKind::Other {
                            message: crate::tr_fmt!(
                                "unknown `$jigglebone` key %{key} (the official code aborts with \
                                 `$jigglebone: invalid syntax`)",
                                key = format!("{:?}", w.key)
                            ),
                        },
                    });
                }
            }
        }

        // ---- FBX 源选项（`src*`）----
        //
        // 这些字段只在源是 FBX 时有意义，但**不按扩展名报错** ——
        // 写了 `srcpart` 却把 `.fbx` 换成 `.smd` 时，报「SMD 不支持 srcpart」
        // 比静默忽略更有用；而校验器看不到文件系统，只能做**值域**检查。
        // 「选项是否被用上」由 `src\compile.rs` 在真读到源之后判定。
        for (bi, bp) in self.bodyparts.iter().enumerate() {
            for (mi, m) in bp.models.iter().enumerate() {
                let path = format!("bodyparts[{bi}].models[{mi}]");
                if let Some(s) = m.src_scale
                    && (!s.is_finite() || s <= 0.0)
                {
                    errs.push(DescError {
                        path: format!("{path}.src_scale"),
                        kind: DescErrorKind::NotPositive {
                            detail: "",
                            value: format!("{s}"),
                        },
                    });
                }
                if let Some(a) = &m.src_axis
                    && crate::fbx::ForcedAxis::parse(a).is_none()
                {
                    errs.push(DescError {
                        path: format!("{path}.src_axis"),
                        kind: DescErrorKind::Other {
                            message: crate::tr_fmt!(
                                "only \"auto\" / \"y\" / \"z\" are accepted (yup / y-up / zup / z-up \
                                 are also accepted, case-insensitively), but got %{value}",
                                value = format!("{:?}", a)
                            ),
                        },
                    });
                }
                if let Some(mat) = &m.src_material
                    && mat.trim().is_empty()
                {
                    errs.push(DescError {
                        path: format!("{path}.src_material"),
                        kind: DescErrorKind::Empty {
                            message: "must not be empty (omitting this field is the official debug/debugempty)",
                        },
                    });
                }
                for (k, p) in m.src_parts.iter().enumerate() {
                    if p.trim().is_empty() {
                        errs.push(DescError {
                            path: format!("{path}.src_parts[{k}]"),
                            kind: DescErrorKind::Empty {
                                message: "the mesh name must not be empty",
                            },
                        });
                    }
                }
                for (k, n) in m.src_shape_keys.iter().enumerate() {
                    if n.trim().is_empty() {
                        errs.push(DescError {
                            path: format!("{path}.src_shape_keys[{k}]"),
                            kind: DescErrorKind::Empty {
                                message: "the morph target name must not be empty",
                            },
                        });
                    }
                }
                for (k, n) in m.src_shape_key_order.iter().enumerate() {
                    if n.trim().is_empty() {
                        errs.push(DescError {
                            path: format!("{path}.src_shape_key_order[{k}]"),
                            kind: DescErrorKind::Empty {
                                message: "the morph target name must not be empty",
                            },
                        });
                    }
                }
                // `srcshapekeyignore` 与另外两个互斥：一个是「全都要」，
                // 一个是「都不要」，同时给出会让「谁说了算」变成隐式规则。
                if m.src_shape_key_ignore
                    && (!m.src_shape_keys.is_empty() || !m.src_shape_key_order.is_empty())
                {
                    errs.push(DescError {
                        path: format!("{path}.src_shape_key_ignore"),
                        kind: DescErrorKind::Other {
                            message: "cannot be given together with `src_shape_keys` / \
                                      `src_shape_key_order`: the former means \"no morph target \
                                      at all\", the latter two mean \"take them by name\""
                                .into(),
                        },
                    });
                }
            }
        }
        for (i, s) in self.sequences.iter().enumerate() {
            if let Some(f) = s.src_fps
                && (!f.is_finite() || f <= 0.0)
            {
                errs.push(DescError {
                    path: format!("sequences[{i}].src_fps"),
                    kind: DescErrorKind::NotPositive {
                        detail: "",
                        value: format!("{f}"),
                    },
                });
            }
        }
        for (i, a) in self.animations.iter().enumerate() {
            if let Some(f) = a.src_fps
                && (!f.is_finite() || f <= 0.0)
            {
                errs.push(DescError {
                    path: format!("animations[{i}].src_fps"),
                    kind: DescErrorKind::NotPositive {
                        detail: "",
                        value: format!("{f}"),
                    },
                });
            }
        }

        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 一份最小但合法的描述。**网格不在这里** —— 由 `smd` 指向的文件承载。
    pub(crate) const MINIMAL_TOML: &str = r#"
[model]
name = "models/test/minimal.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/minimal" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "minimal-ref.smd"
"#;

    #[test]
    fn parses_minimal_toml() {
        let d = ModelDesc::from_toml(MINIMAL_TOML).expect("最小描述必须能解析");
        assert_eq!(d.output_name(), "models/test/minimal.mdl");
        assert_eq!(d.version(), 49);
        assert_eq!(d.bones.len(), 2);
        assert_eq!(d.bones[1].parent.as_deref(), Some("root"));
        assert_eq!(d.bodyparts[0].models[0].smd, "minimal-ref.smd");
        // 骨骼姿态可以省略（编译时从 SMD 取）。
        assert!(d.bones[0].position.is_none());
        d.validate().expect("最小描述必须合法");
    }

    /// **缺省值**必须钉住 —— `serde(default = "default_true")` 一旦写错
    /// （比如漏了 `#[serde(default)]`），字段会变成 `false`：
    /// 超限 mesh 又会**直接报错**，而所有单元测试仍会全绿
    /// （它们都显式构造结构体，不走 serde）。
    #[test]
    fn split_oversized_meshes_defaults_to_true() {
        let d = ModelDesc::from_toml(MINIMAL_TOML).expect("最小描述必须能解析");
        assert!(
            d.model.split_oversized_meshes,
            "`split_oversized_meshes` 的缺省必须是 true（否则用户要先撞一次墙）"
        );
        // 显式关掉也要能生效。
        let off = MINIMAL_TOML.replace("[model]", "[model]\nsplit_oversized_meshes = false");
        let d2 = ModelDesc::from_toml(&off).expect("显式 false 必须能解析");
        assert!(
            !d2.model.split_oversized_meshes,
            "显式 `split_oversized_meshes = false` 必须被尊重"
        );
    }

    /// QC 路径（不走 serde）的缺省必须与 TOML 一致 —— 两处漂移会让
    /// 「同一个模型用 QC 编能过、用 TOML 编报错」。
    #[test]
    fn qc_default_matches_toml_default() {
        assert!(
            crate::model::default_true(),
            "QC 与 TOML 必须共用同一个缺省函数"
        );
        // 防漂移：`ModelMeta` 走 serde 得到的值也必须一致。
        let d = ModelDesc::from_toml(MINIMAL_TOML).unwrap();
        assert_eq!(d.model.split_oversized_meshes, crate::model::default_true());
    }

    #[test]
    fn round_trips_through_toml() {
        let d = ModelDesc::from_toml(MINIMAL_TOML).unwrap();
        let text = d.to_toml().unwrap();
        let again = ModelDesc::from_toml(&text).unwrap();
        assert_eq!(d, again, "TOML 往返必须保持等价");
    }

    #[test]
    fn template_is_itself_valid() {
        // 模板必须自身能解析且通过校验 —— 否则用户照抄就报错。
        let d = ModelDesc::from_toml(TEMPLATE_TOML).expect("模板必须能解析");
        d.validate().expect("模板必须合法");
    }

    #[test]
    fn template_does_not_contain_mesh_data() {
        // 网格必须由 SMD 承载：模板里不该出现 vertices/triangles。
        assert!(!TEMPLATE_TOML.contains("vertices"), "模板不应内联顶点");
        assert!(!TEMPLATE_TOML.contains("triangles"), "模板不应内联三角形");
        assert!(TEMPLATE_TOML.contains("smd ="), "模板应引用 SMD");
    }

    #[test]
    fn template_documents_weightlist_where_it_actually_lives() {
        // 用户最容易踩的坑：把 `weight_lists` 写成 `[model]` 的字段。
        // 模板必须在**顶层**演示它，并显式警告位置。
        assert!(
            TEMPLATE_TOML.contains("[[weight_lists]]"),
            "模板应演示 [[weight_lists]]"
        );
        assert!(
            TEMPLATE_TOML.contains("[[weight_lists.bones]]"),
            "模板应演示 [[weight_lists.bones]]"
        );
        assert!(
            !TEMPLATE_TOML.contains("[[model.weight_lists"),
            "模板**不能**把 weight_lists 写成 [model] 的字段（deny_unknown_fields 会拒）"
        );
        assert!(
            TEMPLATE_TOML.contains("weight_list = "),
            "模板应演示序列侧的 weight_list 引用"
        );
    }

    /// 两个「优化 / 兜底」开关必须在模板里**可见**。
    ///
    /// 它们的缺省很反直觉（`optimize_vtx` 默认 **false**、
    /// `split_oversized_meshes` 默认 **true**），不写进模板的话用户只能去
    /// 读源码。`--template` 是主要发现路径。
    #[test]
    fn template_documents_both_optimization_switches() {
        assert!(
            TEMPLATE_TOML.contains("optimize_vtx = true"),
            "模板应演示 optimize_vtx（缺省 false）"
        );
        assert!(
            TEMPLATE_TOML.contains("split_oversized_meshes = false"),
            "模板应演示 split_oversized_meshes（缺省 true）"
        );
        // 两处都要说明缺省值，否则用户看不出「注释掉会怎样」。
        assert!(
            TEMPLATE_TOML.contains("默认 true"),
            "模板应写明 split_oversized_meshes 的缺省是 true"
        );
    }

    #[test]
    fn template_documents_forward_declared_sequences() {
        // 用户最容易踩的两个坑：给空壳写 smd、想用 `$sequence` 去填它。
        // 模板必须把这两条写清楚。
        assert!(
            TEMPLATE_TOML.contains("forward_declared = true"),
            "模板应演示 forward_declared"
        );
        assert!(
            TEMPLATE_TOML.contains("$declaresequence"),
            "模板应说明它对应 QC 的哪条命令"
        );
        assert!(
            TEMPLATE_TOML.contains("no animations found"),
            "模板应警告「不能用 $sequence 填充空壳」"
        );
    }

    #[test]
    fn forward_declared_round_trips_through_serde() {
        // `qc2toml` 靠 `to_toml()` 落盘 —— 空壳必须能往返。
        let toml = format!(
            r#"
{MINIMAL_TOML}
[[sequences]]
name = "shell"
forward_declared = true
"#
        );
        let d = ModelDesc::from_toml(&toml).unwrap();
        assert!(d.sequences[0].forward_declared);
        assert_eq!(d.sequences[0].smd, "", "空壳的 smd 缺省为空");
        let text = d.to_toml().unwrap();
        let d2 = ModelDesc::from_toml(&text).unwrap();
        assert_eq!(d.sequences, d2.sequences, "往返后空壳必须一致");
        assert!(text.contains("forward_declared"), "{text}");
    }

    #[test]
    fn weightlist_in_toml_round_trips_through_serde() {
        // `qc2toml` 靠 `to_toml()` 落盘 —— 权重表必须能往返，
        // 否则「QC → TOML → 编译」这条路会静默丢数据。
        let toml = format!(
            r#"
{MINIMAL_TOML}
[[weight_lists]]
name = "upper"

[[weight_lists.bones]]
bone = "tip"
weight = 0.5
pos_weight = 0.25
"#
        );
        let d = ModelDesc::from_toml(&toml).unwrap();
        assert_eq!(d.weight_lists.len(), 1);
        assert_eq!(d.weight_lists[0].bones[0].weight, 0.5);
        assert_eq!(d.weight_lists[0].bones[0].pos_weight(), 0.25);

        let text = d.to_toml().unwrap();
        let d2 = ModelDesc::from_toml(&text).unwrap();
        assert_eq!(d.weight_lists, d2.weight_lists, "往返后权重表必须一致");
        assert!(
            text.contains("weight_lists"),
            "序列化结果应含 weight_lists：\n{text}"
        );
    }

    #[test]
    fn checksum_is_stable_and_name_derived() {
        let a = ModelDesc::from_toml(MINIMAL_TOML).unwrap();
        let b = ModelDesc::from_toml(MINIMAL_TOML).unwrap();
        assert_eq!(a.checksum(), b.checksum(), "同一描述必须得到同一 checksum");
        let mut c = a.clone();
        c.model.name = "models/test/other.mdl".into();
        assert_ne!(a.checksum(), c.checksum(), "不同名字应得到不同 checksum");
    }

    #[test]
    fn explicit_checksum_wins() {
        let mut d = ModelDesc::from_toml(MINIMAL_TOML).unwrap();
        d.model.checksum = Some(12345);
        assert_eq!(d.checksum(), 12345);
    }

    #[test]
    fn rejects_unknown_fields() {
        let bad = format!("{MINIMAL_TOML}\n[nope]\nx = 1\n");
        let err = ModelDesc::from_toml(&bad).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TOML"), "{msg}");
        // 新契约：源错误保留下来，所以**出错位置**可查（旧版被 `format!` 掉了）。
        let TomlError::Parse(inner) = &err else {
            panic!("from_toml 只应产生 Parse：{err:?}");
        };
        assert!(inner.span().is_some(), "未知字段应带出错位置：{inner}");
    }

    #[test]
    fn rejects_mesh_data_in_toml() {
        // 旧的「网格内联」写法必须被拒（deny_unknown_fields）。
        let bad = format!("{MINIMAL_TOML}\n[[bodyparts.models.meshes]]\nmaterial = 0\n");
        let err = ModelDesc::from_toml(&bad).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("TOML"), "内联 mesh 应被拒绝：{msg}");
    }

    #[test]
    fn rejects_forward_parent_reference() {
        let bad = MINIMAL_TOML.replace("parent = \"root\"", "parent = \"tip\"");
        let d = ModelDesc::from_toml(&bad).unwrap();
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "bones[1].parent"),
            "应报父骨骼顺序错误：{errs:?}"
        );
    }

    #[test]
    fn rejects_missing_parent() {
        let bad = MINIMAL_TOML.replace("parent = \"root\"", "parent = \"ghost\"");
        let d = ModelDesc::from_toml(&bad).unwrap();
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                &e.kind,
                DescErrorKind::BoneNotFound { what: "parent ", bone, .. } if bone == "ghost"
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_empty_smd_reference() {
        let bad = MINIMAL_TOML.replace("smd = \"minimal-ref.smd\"", "smd = \"\"");
        let d = ModelDesc::from_toml(&bad).unwrap();
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path.ends_with(".smd")),
            "空 smd 应报错：{errs:?}"
        );
    }

    #[test]
    fn rejects_overlong_model_name() {
        let bad = MINIMAL_TOML.replace(
            "smd = \"minimal-ref.smd\"",
            "smd = \"minimal-ref.smd\"\nname = \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"",
        );
        let d = ModelDesc::from_toml(&bad).unwrap();
        // ⚠️ `Other` 的 message 在 `validate()` 里就渲染好了，语言要在那一刻生效。
        let errs = crate::test_locale::with_english(|| d.validate().unwrap_err());
        assert!(
            errs.iter()
                .any(|e| matches!(&e.kind, DescErrorKind::Other { message } if message.contains("too long"))),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_non_finite_bone_pose() {
        let bad = MINIMAL_TOML.replace(
            "name = \"root\"",
            "name = \"root\"\nposition = [0.0, 0.0, inf]",
        );
        // TOML 的 inf 是合法浮点；应被我们的有限性校验拦下。
        let d = ModelDesc::from_toml(&bad).unwrap();
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e.kind, DescErrorKind::NotFinite { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_bad_version() {
        let bad = MINIMAL_TOML.replace(
            "name = \"models/test/minimal.mdl\"",
            "name = \"models/test/minimal.mdl\"\nversion = 7",
        );
        let d = ModelDesc::from_toml(&bad).unwrap();
        let errs = d.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "model.version"));
    }

    #[test]
    fn rejects_empty_bones() {
        let bad = MINIMAL_TOML.replace(
            "[[bones]]\nname = \"root\"\n\n[[bones]]\nname = \"tip\"\nparent = \"root\"\n",
            "",
        );
        let d = ModelDesc::from_toml(&bad).unwrap();
        let errs = d.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "bones"), "{errs:?}");
    }

    #[test]
    fn normalizes_backslashes_in_name() {
        let bad = MINIMAL_TOML.replace("models/test/minimal.mdl", "models\\\\test\\\\minimal.mdl");
        let d = ModelDesc::from_toml(&bad).unwrap();
        assert_eq!(d.output_name(), "models/test/minimal.mdl");
    }
}
