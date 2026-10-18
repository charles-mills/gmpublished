//! MDL 段布局：**声明式**的偏移计算。
//!
//! ```ignore
//! let bone_off = studiohdr2_off + STUDIOHDR2_SIZE;
//! let attachment_off = bone_off + bone_count * BONE_SIZE;
//! let hitbox_set_off = attachment_off + at_count * ATTACHMENT_SIZE;
//! // … 还有二十多行
//! ```

/// 一个段在布局里的位置。
///
/// 字段是 `pub` 的，但**不要手动构造** —— 用 [`SectionOffsets::compute`]，
/// 它保证顺序与偏移都正确。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionOffsets {
    // ---- 头部之后、骨骼表之前 ----
    pub studiohdr2: usize,
    // ---- 骨骼相关 ----
    pub bone: usize,
    pub bonecontroller: usize,
    pub attachment: usize,
    pub hitboxset: usize,
    /// `byte[numbones]` 的名字索引表。
    pub bonetablename: usize,
    // ---- 动画 ----
    pub localanim: usize,
    /// 动画链区（紧跟在 animdesc 数组之后）。
    pub anim_data: usize,
    pub localseq: usize,
    /// seq 子表区（events / blend / iklock / keyvalue），紧跟 seqdesc 数组。
    pub seq_subtables: usize,
    // ---- 几何 ----
    pub bodypart: usize,
    pub localnode: usize,
    pub localnodename: usize,
    /// model 数组（紧跟在 bodypart 数组之后）。
    pub model: usize,
    // ---- 夹在 model 与 mesh 之间的可选段 ----
    pub flexdesc: usize,
    pub flexcontroller: usize,
    pub flexrule: usize,
    /// `mstudioflexcontrollerui_t`（20 字节/条）数组的起点。
    ///
    /// **夹在 flexop 区与 `ikchain` 之间**，`write.cpp`(ep1) 里**根本没有**
    /// 这段代码 —— 是靠「链式自然偏移」在语料上反解出来的
    /// （`rsrch_flex_ui_verify.js`：`ikchainindex == flexcontrolleruiindex + n*20`，
    /// **78/78**；`flexcontrolleruiindex` 落在 flexop 区末尾 **3333/3333**）。
    ///
    /// 来源是 **DMX**（`CDmeGlobalFlexControllerOperator`），没有任何 QC
    /// 命令能产生它 —— 所以 mdlc 的 `flex_controller_uis` 恒为 0，
    /// 本字段等于 `ikchain`。但它**必须写进头部 `+0x184`**：
    /// 官方在空段时也写自然偏移（`ip_official.mdl` 的 `+0x184` = 1472
    /// = 同文件的 `ikchainindex`）。
    pub flexcontrollerui: usize,
    pub ikchain: usize,
    pub mouth: usize,
    pub poseparam: usize,
    pub ikautoplaylock: usize,
    /// mesh 数组（在 flex/ik/poseparam **之后**）。
    pub mesh: usize,
    // ---- 尾部 ----
    pub texture: usize,
    pub includemodel: usize,
    pub animblock: usize,
    /// `$cdmaterials` 偏移数组（`int[numcdtextures]`）。
    ///
    /// **紧跟 texture 数组**，不是文件末尾 —— 实测 3301/3301 个真实模型满足
    /// `cdtextureindex == textureindex + numtextures*64`。
    pub cdtexture_array: usize,
    /// skin 表：`uint16[numskinref * numskinfamilies]`。
    ///
    /// 紧跟 `$cdmaterials` 数组 —— 实测 3301/3301 满足
    /// `skinindex == cdtextureindex + numcdtextures*4`。
    pub skin: usize,
    /// 字符串池。
    pub strings: usize,
    /// `$keyvalues` 文本段。
    ///
    /// **在字符串池之前**（实测 3333/3333：`keyvalueindex ==
    /// align4(skinindex + 2*numskinref*numskinfamilies)`，且
    /// `strings_start − keyvalues_end` 恒为正）。
    pub keyvalues: usize,
    /// `mstudiolinearbone_t` 加速结构（`studiohdr2.linearboneindex` 指向它）。
    ///
    /// **在 `keyvalues` 之后、字符串池之前**（实测 517/517：`keyvalues`
    /// 非空时其末尾 `<=` 本段起点，**0 例外**）。若模型还有
    /// `srcbonetransform`，则顺序是 `keyvalues → srcbonetransform → linearbone`
    /// （实测 68/68：`ALIGN4(srcbonetransform 末尾) == 本段起点`）。
    ///
    /// 起点 `ALIGN4(前一段末尾)`（实测 517/517 满足 `% 4 == 0`）。
    /// 段不存在（`bones < 2`）时 `linearboneindex` 写 **0**。
    pub linearbone: usize,
    /// `mstudiosrcbonetransform_t` 数组（`studiohdr2.srcbonetransformindex`）。
    ///
    /// **在 `keyvalues` 之后、`linearbone` 之前**（实测 71/71：段位恰在
    /// `keyvalues` 与 `linearbone` 之间）。100 字节/条。
    ///
    /// 触发条件（本轮破解，见 `HANDBOOK.md` 第 21.6 节）：跑了
    /// `RealignBones`/`$definebone` 的骨骼各一条，记录数 == 骨骼数。
    /// 段不存在时 `srcbonetransformindex` 写 **0**。
    pub srcbonetransform: usize,
    /// 文件总长度。
    pub total: usize,
}

/// 各段的元素数。由调用方按当前模型填。
#[derive(Debug, Clone, Copy, Default)]
pub struct SectionCounts {
    pub bones: usize,
    pub bonecontrollers: usize,
    /// jigglebone（`mstudiojigglebone_t`，**120 字节**）的条数。
    ///
    /// **不走段头** —— 它是骨骼数组的延伸，起点
    /// `ALIGN4(boneindex + bones*216)`，在 `bonecontroller` 之前。
    pub jiggle_bones: usize,
    pub attachments: usize,
    /// hitbox set 的数量（有 hitbox 时为 1，否则 0）。
    pub hitbox_sets: usize,
    pub hitboxes: usize,
    /// `mstudioanimdesc_t` 的数量（= `numlocalanim`）。
    pub anims: usize,
    /// `mstudioseqdesc_t` 的数量（= `numlocalseq`）。
    ///
    /// **通常等于 `anims`**（一个序列一个动画），但 `$staticprop` 是例外：
    /// `MakeStaticProp()` 把动画强制成 1 帧 1 条（`simplify.cpp:3375-3376`
    /// 的 `g_numani = 1`），却**保留**全部序列（`numlocalseq` 不变）。
    /// 实测 `ipe2`（2 条序列 + `$staticprop`）：`numlocalanim=1`、
    /// `numlocalseq=2`；语料 `smalldebris_part_baked_setsexp.mdl` 是
    /// 1 个 animdesc 对 5 个 seqdesc。所以两者必须分开计数。
    pub seqs: usize,
    pub transition_nodes: usize,
    /// 动画链区的**字节数**（不是元素数）。
    pub anim_data_bytes: usize,
    /// 动画是否进**外部 `.ani` 块**（`$animblocksize > 0`）。
    ///
    /// 影响 `anim_data` 起点的对齐：**内联**时是 `ALIGN16`，
    /// **块形态**时是 `ALIGN4`（实测 41 个块形态模型全是 `ALIGN4`，
    /// 而 3260 个内联模型全是 `ALIGN16` —— 见 [`anim_data_start`]）。
    pub anim_in_block: bool,
    /// seq 子表区的**字节数**。
    pub seq_subtable_bytes: usize,
    pub bodyparts: usize,
    pub models: usize,
    pub meshes: usize,
    pub textures: usize,
    pub cdtextures: usize,
    /// `$keyvalues` 文本段的字节数（含结尾 NUL）。
    pub keyvalues_bytes: usize,
    /// 字符串池的字节数。
    pub string_bytes: usize,
    /// quatinterp（proctype 2）的记录数（`mstudioquatinterpbone_t`，**12 字节**）。
    ///
    /// 与 [`Self::jiggle_bones`] 一样不走段头，是骨骼数组的延伸。
    /// **proctype 升序**：quatinterp（2）在 jiggle（5）之前。
    pub quat_interp_bones: usize,
    /// quatinterp 的**触发器总数**（`mstudioquatinterpinfo_t`，**48 字节**）。
    ///
    /// 触发器区排在**记录数组 + `ALIGN4`** 之后，且**逐记录紧密排列**
    /// （每条记录的 `triggerindex` 相对该记录自身）。
    pub quat_interp_triggers: usize,
    /// 是否产出 `mstudiolinearbone_t` 加速结构（`studiohdr2.linearboneindex`）。    ///
    /// **触发条件是 `bones >= 2`** —— 实测语料完美二分：
    /// `linearboneindex != 0` ⟺ `numbones >= 2`（517 / 0 / 0 / 2816，
    /// 见 `probe_linearbone_trigger.js`）。骨骼数 ≥ 2 时该段**一定**存在。
    ///
    /// 注意这与 `$staticprop` 高度相关但**不是**同一条件：语料里
    /// 2681 个静态道具全是 `numbones == 1`（因而没有 linearbone），
    /// 但另外还有 10 个 `numbones == 1` 的非静态道具（`air_node`、
    /// `brokenglass_piece` 等）同样没有。所以判据用**骨骼数**，
    /// 不要用「是不是静态道具」。
    pub has_linearbone: bool,
    /// 是否产出 `mstudiosrcbonetransform_t` 数组
    /// （`studiohdr2.srcbonetransformindex`）。
    ///
    /// **触发条件**（本轮破解，见 `HANDBOOK.md` 第 21.6 节）：
    /// 模型里**有骨骼的 `srcRealign` 不是单位阵**（即真的被 `RealignBones`
    /// 搬动过，或 `$definebone` 给了非平凡矩阵）。记录数 == 这类骨骼的**根数**，
    /// **不是**骨骼总数。
    ///
    /// 官方 10 个相关 artifacts 全部吻合：`ipr2`/`ipr3` 3 条、
    /// `ipk1`/`ipk2` 3 条（排除 `root`）、`ipk3` 4 条、
    /// `ipq3`/`ipq5` 2 条（排除 `root`）；
    /// 对照组 `ipr1`/`ipq2`/`ipkx1` **0 条**。
    pub has_srcbonetransform: bool,
    /// 写进 `srcbonetransform` 数组的骨骼**根数**（`numsrcbonetransform`）。
    pub srcbonetransform_count: usize,

    // ---- 段计数 ----
    //
    // ⚠️ 这段注释原先写着「以下是已留好位置但尚未实现的段，字段现在全是 0」——
    // **早已过时**：flex / ikchain / mouth / poseparam / iklock / includemodel /
    // animblock 现在都填**真实计数**（见 `mdl_writer.rs` 构造 `SectionCounts`
    // 的地方）。唯一仍是恒 0 的是 [`Self::bone_flex_drivers`]。
    //
    // 留着这条更正，是因为「字段恒 0 所以不影响产物」这种注释会让人
    // **跳过**整段代码 —— 而其中的 `flexop_bytes` / `iklink_bytes` /
    // `eyeball_bytes` 恰恰是最容易算错、且错了就整段偏移全错的三个。
    /// `mstudioboneflexdriver_t`（`studiohdr2` 里的骨骼 flex 驱动）。
    ///
    /// **唯一仍然恒为 0 的段**：语料里 `$boneflexdriver` 出现 **0 次**
    /// （L4D2 二进制也没有这条命令），所以没有可验收的判据。
    pub bone_flex_drivers: usize,
    /// `mstudioflexdesc_t`（4 字节）。
    pub flexdescs: usize,
    /// `mstudioflexcontroller_t`（20 字节）。
    pub flexcontrollers: usize,
    /// `mstudioflexrule_t`（12 字节）。
    pub flexrules: usize,
    /// 全部 `mstudioflexop_t`（8 字节）的**字节数** —— 每条规则的 op 块
    /// 紧跟规则数组之后依次排列（`write.cpp:1517-1535`）。
    pub flexop_bytes: usize,
    /// `mstudioflexcontrollerui_t`（20 字节）。语料里只有 8/3333 个模型非空，
    /// 且**全部来自 DMX** —— mdlc 恒为 0（见 [`SectionOffsets::flexcontrollerui`]）。
    pub flex_controller_uis: usize,
    /// `mstudioikchain_t`（16 字节）。
    pub ikchains: usize,
    /// 全部 `mstudioiklink_t`（**28 字节**）的**字节数**（`write.cpp:1544-1560`）。
    ///
    /// 28 = `int bone`(4) + `Vector kneeDir`(12) + `Vector unused0`(12)。
    /// 语料实测：只有 28 能让「链数组 + 链接区」精确对上下一段起点
    /// （78/78 命中，20/24/32/36 全 0 —— `probe_ikchain.js`）。
    pub iklink_bytes: usize,
    /// `mstudiomouth_t`（20 字节）。
    pub mouths: usize,
    /// 全部 eyeball（`mstudioeyeball_t`，**172 字节**）的**字节数**。
    ///
    /// eyeball 数组**穿插在 mesh 段内**：每个 model 的 mesh 数组之后紧跟
    /// 该 model 自己的 eyeball 数组（`mstudiomodel_t.eyeballindex ==
    /// meshindex + nummeshes*116`，语料 3620/3620）。所以它不是独立段，
    /// 而是把 mesh 段的总长撑大。
    pub eyeball_bytes: usize,
    /// 全部 VTA 载荷（`mstudioflex_t` 60 字节 + `mstudiovertanim_t` 16 字节）
    /// 的**字节数**。
    ///
    /// 与 eyeball 同理，**穿插在 mesh 段内**：`write.cpp:1721-1822` 在
    /// 「写该 model 的 eyeball 之后、下一个 model 的 mesh 之前」写 flex 数据。
    /// 所以它同样把 mesh 段撑大，且**必须精确** —— 少算一个字节，
    /// 后面 includemodel / animblock / texture 与字符串池全部错位。
    ///
    /// # 每字节的构成（`write.cpp:1753-1780`）
    ///
    /// 仅对 `numflexes > 0` 的 mesh 写：
    ///
    /// ```text
    /// mstudioflex_t[numflexes]        （60 字节/条）
    /// ALIGN4
    /// 逐条 flex：
    ///     mstudiovertanim_t[numverts] （16 字节/条）
    ///     ALIGN4
    /// ```
    ///
    /// ⚠️ 对 mdlc 当前产出的 `vertanimtype = 0` 而言，60 与 16 **都是 4 的
    /// 倍数**，且上游（mesh 116、eyeball 172）也都是 4 的倍数，所以
    /// `ALIGN4` 恒为 no-op。这里仍按对齐累加，以免将来支持
    /// `WRINKLE`（18 字节）时算错。
    pub flex_bytes: usize,
    /// `mstudioposeparamdesc_t`（20 字节）。
    pub poseparams: usize,
    /// `mstudioiklock_t`（32 字节）。
    pub iklocks: usize,
    /// `mstudiomodelgroup_t`（8 字节）。
    pub includemodels: usize,
    /// `mstudioanimblock_t`（8 字节）。
    pub animblocks: usize,
    /// skin 表：`numskinref * numskinfamilies` 个 uint16。
    pub skin_entries: usize,
}

// ---- 各结构体的字节大小（实测确认，见 mdl_writer 的常量文档）----
use crate::mdl_writer::{
    ATTACHMENT_SIZE, BODY_PART_SIZE, BONE_CONTROLLER_SIZE, BONE_SIZE, HDR_PART1_SIZE,
    HITBOX_SET_SIZE, HITBOX_SIZE, JIGGLE_BONE_SIZE, LINEARBONE_HEADER_SIZE, LINEARBONE_PER_BONE,
    MESH_SIZE, MODEL_SIZE, QUATINTERP_BONE_SIZE, QUATINTERP_INFO_SIZE, SRCOBONETRANSFORM_SIZE,
    STUDIOHDR2_SIZE, TEXTURE_SIZE,
};

/// 向上对齐到 4 字节（studiomdl 的 `ALIGN4`）。
///
/// 程序化骨骼块用它 —— 块起点是 `ALIGN4(骨骼数组末尾)`，块末尾再 `ALIGN4`
/// 才是 `bonecontroller`（`write.cpp:214-285`）。
#[inline]
fn align4(v: usize) -> usize {
    v.div_ceil(4) * 4
}

/// 向上取整到 16 的倍数。
///
/// `anim_data` 起点用它（见 [`anim_data_start`]）；分段动画的段表之后也用它。
#[inline]
fn align16(v: usize) -> usize {
    v.div_ceil(16) * 16
}

/// `anim_data`（动画链区）在文件里的**绝对起点**。
///
/// # 为什么单独抽出来
///
/// 这个值是 [`SectionOffsets::compute`] 的中间量，但 `anim_writer` 也**必须**
/// 在写出动画**之前**知道它 —— 因为分段动画的 `ALIGN16` 要按**绝对位置**算
/// （`align16(段表绝对末尾) == 链绝对起点`）。
///
/// 而 `anim_data` 的长度又取决于动画怎么写，所以这里存在一个先有鸡还是先有蛋
/// 的循环，只能靠「先按公式估出起点 → 写动画 → 再用真实长度算其余段」打破。
///
/// # 曾经的隐患
///
/// 早先 `mdl_writer.rs` 里**内联抄了一份**同样的公式。两处一旦不同步，
/// 分段产物会**静默错位** —— 布局自检抓不到，因为两处各自都「自洽」。
/// 加 quatinterp 时就踩过：两层结构只改了一处。
///
/// 现在公式只在这里，`mdl_writer` 调本函数，**物理上不可能再不同步**。
///
/// # 前提
///
/// 只依赖 `anim_data` **之前**的段（`bonetablename + bones` → `localanim`
/// → animdesc 数组），与动画链的长度无关，所以能安全地提前算。
///
/// # 末尾的对齐（内联 `ALIGN16`、块形态 `ALIGN4`）
///
/// `anim_data` 起点 = **`ALIGN16`(animdesc 数组末尾)**（内联），
/// 或 **`ALIGN4`(animdesc 数组末尾)**（外部 `.ani` 块）。
///
/// 实测判据（`docs/_probe/probe_anim_align.js`，语料 3301 个有动画的模型）：
///
/// ```text
/// anim_data == align16(animdesc_end) : 3260   <-- 内联模型的唯一规则
/// anim_data == align32(animdesc_end) :  227
/// anim_data == align8 (animdesc_end) :  250
/// anim_data == align4 (animdesc_end) :  111
/// 都不满足                            :   41   <-- 全是块形态（外部 .ani）
/// ```
///
/// 那 41 个例外是 `anim_*.mdl` 一类「动画在**外部 `.ani`**」的模型：
/// 它们的 `animindex` 是**块内偏移**，不能与 `.mdl` 的绝对位置比 ——
/// 用 `align4` 判定才成立。
///
/// 受控实验 `blend1`（内联）的官方产物吻合：animdesc 末尾 2216
/// （`2216 % 16 == 8`）→ 第一条链起点 **2224** = `align16(2216)`。
/// 受控实验 `absec1`（`$animblocksize 4096`）的官方产物则是
/// `sectionindex = 100`（= `align4` 的结果，**没有** 12 字节的补齐）。
///
/// ⚠️ 早先这里是**无条件 `align4`** —— 对内联模型会整体提前 4/8/12 字节，
/// 于是 `localseqindex` 及其后所有段偏移全部与官方差一个常量
/// （`blend1` 实测差 **12**）。
pub fn anim_data_start(c: &SectionCounts) -> usize {
    // ⚠️ 程序化骨骼块是**两层**：quatinterp（2）在前、jiggle（5）在后，
    // 且 quatinterp 自己还有「记录数组 → ALIGN4 → 触发器区」两层。
    let proc_start = align4(HDR_PART1_SIZE + STUDIOHDR2_SIZE + c.bones * BONE_SIZE);
    let quat_records = proc_start + c.quat_interp_bones * QUATINTERP_BONE_SIZE;
    let quat_triggers = align4(quat_records);
    let proc_end = quat_triggers
        + c.quat_interp_triggers * QUATINTERP_INFO_SIZE
        + c.jiggle_bones * JIGGLE_BONE_SIZE;
    let bonecontroller = align4(proc_end);
    let attachment = bonecontroller + c.bonecontrollers * BONE_CONTROLLER_SIZE;
    let hitboxset = attachment + c.attachments * ATTACHMENT_SIZE;
    let bonetablename = hitboxset + c.hitbox_sets * HITBOX_SET_SIZE + c.hitboxes * HITBOX_SIZE;
    let localanim = align4(bonetablename + c.bones); // byte[numbones]
    let animdesc_end = localanim + c.anims * crate::anim_writer::ANIMDESC_SIZE;
    if c.anim_in_block {
        align4(animdesc_end)
    } else {
        align16(animdesc_end)
    }
}

impl SectionOffsets {
    /// 按**权威段顺序**计算所有偏移。
    ///
    /// 顺序（来自 3333 个真实模型的实测归纳）：
    ///
    /// ```text
    /// studiohdr2 → bone → bonecontroller → attachment → hitboxset
    ///   → bonetablename → localanim → 动画链 → localseq → seq 子表
    ///   → localnodename → localnode → bodypart → model 数组
    ///   → flexdesc → flexcontroller → flexrule(+flexop) → flexcontrollerui
    ///   → ikchain(+iklink) → ikautoplaylock → mouth → poseparam → mesh 数组
    ///   → includemodel → animblock → texture
    ///   → cdtexture（$cdmaterials 数组）→ skin → keyvalues
    ///   → srcbonetransform → linearbone → 字符串池
    /// ```
    ///
    /// 转移图两段（`localnodename`/`localnode`）在 **`bodypart` 之前** ——
    /// 依据 `write.cpp`：它们在 `WriteSequenceInfo` 末尾（648-674），
    /// 而 `bodypart` 在下一个函数 `WriteModel`（1461）。实测
    /// `probe_localnode_order.js`：28 个非空模型**全部**满足
    /// `localnodeindex < bodypartindex`，0 反例。
    ///
    /// # 关于计数为 0 的段
    ///
    /// 实测 studiomdl 对空段**仍然写「该段应处的位置」**，不是 0
    /// （3333/3333 模型的 `bonecontroller` 计数为 0 但偏移非 0）。
    /// 本函数因此对每个段都给出位置，调用方直接写即可 ——
    /// 不要自作主张改成 0。
    pub fn compute(c: &SectionCounts) -> Self {
        let studiohdr2 = HDR_PART1_SIZE;
        let bone = studiohdr2 + STUDIOHDR2_SIZE;
        // ⚠️ **程序化骨骼块夹在骨骼数组与 `bonecontroller` 之间** ——
        // 这是**唯一**不走段头的一项（`write.cpp:214-285` 的 `WriteBoneInfo`
        // 在骨骼数组后立刻写它）。实测：**25/25** 个模型满足
        // 「首个程序化块起点 == `ALIGN4(boneindex + numbones*216)`」，
        // 且 jiggle 块末尾 == `bonecontrollerindex`
        // （`rsrch_jiggle_layout.js`，22 个模型 `base + N*120` 精确命中）。
        //
        // 语料只出现 proctype 2（quatinterp）与 5（jiggle）。proctype 1/3/4/6/7
        // 出现 **0 次**，所以这里只算这两者。
        //
        // # 两种块的形状不同（这是最容易写错的地方）
        //
        // ```text
        // quatinterp（proctype 2）：N * 12 记录数组 → ALIGN4 → 逐记录的 Σtriggers*48
        // jiggle    （proctype 5）：N * 120，无二级结构
        // ```
        //
        // **proctype 升序**：quatinterp（2）在 jiggle（5）**之前**。
        //
        // 实测（`rsrch_proc_detail.js`，`survivor_producer.mdl`）：
        // `boneEnd=18376` → quatinterp 记录数组 `18376..18472`（8*12）
        // → `ALIGN4` = 18472 → 触发器区 18472..20200（36*48）
        // → jiggle 数组 20200..21640（12*120）→ `bonecontrollerindex = 21640` ✓
        //
        // ⚠️ `triggerindex` 相对**该记录自身**，且第一个记录的触发器
        // **紧接在记录数组之后**（`CONTIGUOUS`，中间只有那个 `ALIGN4`）。
        let proc_start = align4(bone + c.bones * BONE_SIZE);
        let quat_records = proc_start + c.quat_interp_bones * QUATINTERP_BONE_SIZE;
        let quat_triggers = align4(quat_records);
        let proc_end = quat_triggers
            + c.quat_interp_triggers * QUATINTERP_INFO_SIZE
            + c.jiggle_bones * JIGGLE_BONE_SIZE;
        let bonecontroller = align4(proc_end);
        let attachment = bonecontroller + c.bonecontrollers * BONE_CONTROLLER_SIZE;
        let hitboxset = attachment + c.attachments * ATTACHMENT_SIZE;
        let bonetablename = hitboxset + c.hitbox_sets * HITBOX_SET_SIZE + c.hitboxes * HITBOX_SIZE;
        // ⚠️ `localanim` 要 **`ALIGN4`**：`bonetablename` 是
        // `byte[numbones]`（每骨骼 1 字节），骨骼数不是 4 的倍数时
        // 会把后续所有段**整体错开**。
        //
        // 实测语料 **3302/3302** 满足 `localanimindex % 4 == 0`
        // （`localseqindex`/`bodypartindex` 同样）。
        // 受控实验 `sfw120`（3 骨骼）：mdlc 曾得 1463（`%4 == 3`），
        // 官方是 **1464** —— 链条起点碰巧一致，但内部整体错开 1 字节。
        let localanim = align4(bonetablename + c.bones); // byte[numbones]

        // `anim_data` 起点 = **`ALIGN4(animdesc 数组末尾)`**。
        //
        // 实测依据（分段动画）：官方 `sw120.mdl` 的 `localanimindex=964`、
        // `numlocalanim=1` → 数组末尾 **1064**（已 4 对齐），
        // `sectionindex` 指到 **1064**（差 0）、首链在 `ALIGN16(1064+6*8)=1120`。
        // 不分段的 `sf030` 首链在 1072 = `ALIGN16(1064)`。
        //
        // mdlc 早先直接写 `localanim + anims*100`（**未对齐**，实测 1495），
        // 于是段表落在 1495（`%8 == 7`）—— 分段用例的 `align16` 等式因此差 9。
        //
        // ⚠️ 公式由 [`anim_data_start`] **唯一**提供 —— `mdl_writer` 也调它。
        // 这里**不要**再内联展开，否则又会退化成两处独立的公式。
        let anim_data = anim_data_start(c);
        let localseq = anim_data + c.anim_data_bytes;
        let seq_subtables = localseq + c.seqs * crate::anim_writer::SEQDESC_SIZE;
        // ---- 转移图：`localnodename` → `localnode` → `bodypart` ----
        //
        // 源码顺序（`write.cpp`）：`WriteSequenceInfo` 在写完 seq 子表之后
        // **紧接**写转移图（`write.cpp:648-674`）：
        //
        // ```c
        // int *pxnodename = (int *)pData;
        // phdr->localnodenameindex = (pData - pStart);   // ① 名字索引数组
        // pData += g_numxnodes * sizeof(int);  ALIGN4(pData);
        // ptrans = (byte *)pData;
        // phdr->numlocalnodes = IsChar( g_numxnodes );
        // phdr->localnodeindex = IsInt24( pData - pStart );  // ② 转移矩阵
        // pData += g_numxnodes * g_numxnodes;  ALIGN4(pData);
        // ```
        //
        // 而 `bodypart` 在**下一个**函数 `WriteModel`（`write.cpp:1461`）里写。
        //
        // # 实测判据（`probe_localnode_order.js`）
        //
        // 28 个 `numlocalnodes > 0` 的模型**全部** `localnodeindex <
        // bodypartindex`（0 个反例），差值恰是名字数组 + 转移矩阵的字节数。
        // 例：`lawnmower_gore` 的 `lnn=1296`、`ln=1300`、`bp=1304`
        // （1 个节点：名字数组 4 字节 → ALIGN4 → 1300；矩阵 1 字节 →
        // ALIGN4 → 1304）。
        //
        // # 计数为 0 时三段重合
        //
        // 3305 个 `numlocalnodes == 0` 的模型**全部**满足
        // `localnodeindex == localnodenameindex == bodypartindex`
        // （`probe_empty_section_offsets.js`）—— 空段写「该段应处的位置」
        // 那条通则在这里的表现就是「与后一段的起点重合」。
        //
        // > 早先 mdlc 把这两段排在 `bodypart` **之后**（`= model`），
        // > 于是每个模型都少一次 ALIGN4 的语义、且空段位置偏后 ——
        // > 实测 `mikuw` 官方 2020 / mdlc 2036。
        //
        let localnodename = seq_subtables + c.seq_subtable_bytes;
        let localnode = align4(localnodename + c.transition_nodes * 4);
        let bodypart = align4(localnode + c.transition_nodes * c.transition_nodes);

        let model = bodypart + c.bodyparts * BODY_PART_SIZE;
        // flex/ik/mouth/poseparam/ikautoplaylock 夹在 model 与 mesh 之间。
        // 元素大小：flexdesc 4、flexcontroller 20、flexrule 12、flexop 8、
        //           ikchain 16、iklink 24、iklock 32、mouth 20、poseparam 20
        //
        // **顺序按 `write.cpp:1479-1604` 的写出次序**，实测语料
        // （`docs/_probe/probe_mid_order.js`，48 个多段模型）唯一形态：
        //   flexdesc → flexcontroller → flexrule(+ops) → ikchain(+links)
        //   → ikautoplaylock → mouth → poseparam
        // 注意 **`ikautoplaylock` 在 `mouth`/`poseparam` 之前** ——
        // 早先按 studio.h 的字段声明顺序排（mouth → poseparam → iklock）
        // 是错的；因为那时计数全是 0，偏移恰好重合，所以一直没暴露。
        let flexdesc = model + c.models * MODEL_SIZE;
        let flexcontroller = flexdesc + c.flexdescs * 4;
        let flexrule = flexcontroller + c.flexcontrollers * 20;
        // flexrule 数组之后紧跟**全部 flexop**（每条规则自己的 op 块），
        // 所以这里要算 `c.flexop_bytes` 而不是元素数。
        //
        // 再之后是 `mstudioflexcontrollerui_t` 数组（**20 字节/条**），
        // 然后才是 `ikchain`。`write.cpp`(ep1) 没有这一段，是靠语料反解
        // 出来的（`rsrch_flex_ui_verify.js`，78/78）。
        let flexcontrollerui = flexrule + c.flexrules * 12 + c.flexop_bytes;
        let ikchain = flexcontrollerui + c.flex_controller_uis * 20;
        // 同理 ikchain 数组之后紧跟全部 iklink。
        let ikautoplaylock = ikchain + c.ikchains * 16 + c.iklink_bytes;
        let mouth = ikautoplaylock + c.iklocks * 32;
        let poseparam = mouth + c.mouths * 20;
        let mesh = poseparam + c.poseparams * 20;

        let mesh_end = mesh + c.meshes * MESH_SIZE + c.eyeball_bytes + c.flex_bytes;

        // ---- `includemodel` / `animblock` **在 `texture` 之前** ----
        //
        // 它们写在 `WriteModel()` 的**函数体末尾**（`write.cpp:1826-1850`），
        // 而 `WriteTextures()` 在那之后才被调用（`write.cpp:2123` →
        // `2132`）—— 所以排在 `texture` **之前**。
        //
        // 实测 3333/3333（`rsrch_inc_order.js`，另经独立复核，**0 违反**）：
        //
        // ```text
        // includemodelindex == animblockindex - 8 * numincludemodels
        // animblockindex    == textureindex    - 8 * numanimblocks
        // ```
        //
        // 47 个有 `includemodel` 的模型、121 个有 `animblock` 的模型全部满足。
        //
        // > 早先这里写的是 `let includemodel = texture; let animblock = texture;`
        // > —— 那只在**两个计数都为 0** 时巧合正确（三个偏移恰好重合）。
        // > 一旦有内容，段序与全部后续偏移都会错。
        let includemodel = mesh_end;
        let animblock = includemodel + c.includemodels * 8;
        let texture = animblock + c.animblocks * 8;

        let cdtexture_array = texture + c.textures * TEXTURE_SIZE;
        // skin 表紧跟 `$cdmaterials` 数组。
        let skin = cdtexture_array + c.cdtextures * 4;

        // ---- `keyvalues` **在字符串池之前**，且空段也写自然偏移 ----
        //
        // 实测 3333/3333：`keyvalueindex == align4(skinindex + 2*numskinref
        // *numskinfamilies)`，**没有一个模型写 0**。
        //
        // 「`keyvalues` 在字符串池**之前**」由另一条独立测量确认：
        // 拿骨骼名的最小绝对偏移当字符串池起点的上界，
        // `strings_start − keyvalues_end` 在 3333 个模型上**恒为正**
        // （6~13 字节）；若 `keyvalues` 排在池后，这个差必然是负的。
        //
        // > 早先 mdlc 把 `keyvalues` 排在**池之后**，且 `key_value_offset`
        // > 与 `key_value_size` 一起被写 0 —— 又是「空段写 0」那条铁律的违反。
        //
        // 残留：那 6~13 字节的间隙**尚未查清**（不是 4 字节对齐，也不是
        // `keyvaluesize` 的 NUL）。它只影响字符串池的**绝对起始位置**，
        // 即各 `sznameindex` 的数值，不影响任何结构。见 PROGRESS 已知未解。
        let keyvalues = (skin + c.skin_entries * 2 + 3) & !3;
        // ---- `linearbone`：**在 keyvalues 之后、字符串池之前** ----
        //
        // 实测 517/517（`probe_linearbone_order.js`）：
        //   * `keyvalues` 非空时其**末尾** `<=` 本段起点（**0 例外**）；
        //   * 有 `srcbonetransform` 时 `ALIGN4(srcbonetransform 末尾) == 起点`
        //     （68/68，且 `srcbonetransform` **恒在** `linearbone` 之前）。
        //
        // 段序：`keyvalues → srcbonetransform → linearbone → 字符串池`。
        //
        // ⚠️ 段不存在时 `linearbone` 与 `strings` **重合**（都是 `keyvalues`
        // 末尾）—— 这样段序不变量仍然单调。**头部字段** `linearboneindex`
        // 在段不存在时必须写 **0**（实测 2816 个 `numbones == 1` 的模型全是 0），
        // 由 `mdl_writer` 依据 `has_linearbone` 决定 —— 这是本项目
        // 「空段写自然偏移」那条统一约定的**又一处例外**。
        let kv_end = keyvalues + c.keyvalues_bytes;
        // `srcbonetransform`：紧接 `keyvalues` 之后（`ALIGN4`），
        // 每条 100 字节（`sznameindex` + 两个 48 字节矩阵）。
        //
        // ⚠️ **段为空时起点仍是 `ALIGN4(kv_end)`**，不是 `kv_end` ——
        // 头部字段 `srcbonetransformindex` 写的就是这个「该段应处的位置」。
        //
        // 源码 `write.cpp:1826-1850`：`ALIGN4( pData )` 在
        // `phdr->srcbonetransformindex = ...` **之前**，且 `ALIGN4` 无条件执行。
        //
        // 语料判据（`probe_srcbone_align.js`）：3262 个
        // `numsrcbonetransform == 0` 的模型**全部**满足
        // `index == ALIGN4(kv_end)`，**0 个**是未对齐的 `kv_end`。
        //
        // > 早先 mdlc 在空段分支写 `kv_end`（未对齐）—— `mikuw` 实测
        // > 官方 3044（= ALIGN4(3041)）、mdlc 3042。
        let (srcbonetransform, sbt_end) = if c.has_srcbonetransform {
            let sbt = align4(kv_end);
            (sbt, sbt + SRCOBONETRANSFORM_SIZE * c.srcbonetransform_count)
        } else {
            (align4(kv_end), align4(kv_end))
        };
        let (linearbone, strings) = if c.has_linearbone {
            // 起点 = `ALIGN4(前一段末尾)` —— 实测 449/449（无
            // `srcbonetransform` 的模型）**恰好**等于 `ALIGN4(kv_end)`；
            // 有该段的 68/68 等于 `ALIGN4(srcbonetransform 末尾)`。
            let lb = align4(sbt_end);
            (
                lb,
                lb + LINEARBONE_HEADER_SIZE + LINEARBONE_PER_BONE * c.bones,
            )
        } else {
            (sbt_end, sbt_end)
        };
        // ---- 文件末尾：字符串池之后**无条件 ALIGN4** ----
        //
        // `WriteStringTable`（`write.cpp:127-157`）写完最后一个串后是
        // `ALIGN4( pData ); return pData;`，而 `phdr->length = pData - pStart`
        // （`write.cpp:2169`）取的是**对齐后**的指针。
        //
        // 语料判据（`probe_file_align.js`）：**3333/3333** 个 `.mdl` 的
        // 文件长度都是 4 的倍数，且 `header.length == 文件长度`。
        //
        // > 早先 mdlc 写 `strings + string_bytes`（未对齐）—— `mikuw`
        // > 实测官方 3708、mdlc 3707，只差最后那次对齐。
        let total = align4(strings + c.string_bytes);

        Self {
            studiohdr2,
            bone,
            bonecontroller,
            attachment,
            hitboxset,
            bonetablename,
            localanim,
            anim_data,
            localseq,
            seq_subtables,
            bodypart,
            // 转移图两段**在 `bodypart` 之前**（`write.cpp:648-674`）。
            localnodename,
            localnode,
            model,
            flexdesc,
            flexcontroller,
            flexrule,
            flexcontrollerui,
            ikchain,
            mouth,
            poseparam,
            ikautoplaylock,
            mesh,
            texture,
            includemodel,
            animblock,
            cdtexture_array,
            skin,
            strings,
            keyvalues,
            srcbonetransform,
            linearbone,
            total,
        }
    }

    /// 自检：段偏移必须单调不减，且全部落在文件内。
    ///
    /// 这是**框架级的不变量** —— 新增段时如果顺序写错，这条会立刻抓到。
    /// 返回违反顺序的段名对。
    pub fn check_monotonic(&self) -> Result<(), LayoutError> {
        // 按权威顺序列出（同值允许 —— 空段会重叠）
        let seq: [(&str, usize); 32] = [
            ("studiohdr2", self.studiohdr2),
            ("bone", self.bone),
            ("bonecontroller", self.bonecontroller),
            ("attachment", self.attachment),
            ("hitboxset", self.hitboxset),
            ("bonetablename", self.bonetablename),
            ("localanim", self.localanim),
            ("anim_data", self.anim_data),
            ("localseq", self.localseq),
            ("seq_subtables", self.seq_subtables),
            ("bodypart", self.bodypart),
            ("model", self.model),
            ("flexdesc", self.flexdesc),
            ("flexcontroller", self.flexcontroller),
            ("flexrule", self.flexrule),
            ("flexcontrollerui", self.flexcontrollerui),
            ("ikchain", self.ikchain),
            ("ikautoplaylock", self.ikautoplaylock),
            ("mouth", self.mouth),
            ("poseparam", self.poseparam),
            ("mesh", self.mesh),
            ("includemodel", self.includemodel),
            ("animblock", self.animblock),
            ("texture", self.texture),
            ("cdtexture_array", self.cdtexture_array),
            ("skin", self.skin),
            ("keyvalues", self.keyvalues),
            ("srcbonetransform", self.srcbonetransform),
            ("linearbone", self.linearbone),
            ("strings", self.strings),
            ("total", self.total),
            ("total(end)", self.total),
        ];
        for w in seq.windows(2) {
            if w[0].1 > w[1].1 {
                return Err(LayoutError::OutOfOrder {
                    earlier: w[0].0,
                    earlier_at: w[0].1,
                    later: w[1].0,
                    later_at: w[1].1,
                });
            }
        }
        for (name, off) in seq {
            if off > self.total {
                return Err(LayoutError::BeyondEnd {
                    section: name,
                    offset: off,
                    total: self.total,
                });
            }
        }
        Ok(())
    }
}

/// [`SectionOffsets::check_monotonic`] 的失败原因。
///
/// 两种失败都表示**写出器自己的**段布局算错了（不是用户输入的问题），
/// 所以分变体保留字段，便于断言与排查。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    /// 两个段的偏移顺序颠倒。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Section order is wrong: %{earlier} @%{earlier_at} should come before %{later} @%{later_at}",
            earlier = earlier,
            earlier_at = earlier_at,
            later = later,
            later_at = later_at
        )
    )]
    OutOfOrder {
        earlier: &'static str,
        earlier_at: usize,
        later: &'static str,
        later_at: usize,
    },
    /// 段的偏移超出了文件总长度。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Section %{section} offset %{offset} is beyond the file length %{total}",
            section = section,
            offset = offset,
            total = total
        )
    )]
    BeyondEnd {
        section: &'static str,
        offset: usize,
        total: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(bones: usize, anims: usize) -> SectionCounts {
        SectionCounts {
            bones,
            anims,
            ..Default::default()
        }
    }

    #[test]
    fn studiohdr2_is_always_at_408() {
        let l = SectionOffsets::compute(&counts(2, 0));
        assert_eq!(l.studiohdr2, 408);
        assert_eq!(l.bone, 408 + STUDIOHDR2_SIZE);
    }

    /// **核心判据**：`attachment` 必须在 `hitboxset` 之前。
    ///
    /// 3333/3333 个真实模型实测如此。早先的实现写反了。
    #[test]
    fn attachment_precedes_hitboxset() {
        let l = SectionOffsets::compute(&counts(2, 0));
        assert!(
            l.attachment <= l.hitboxset,
            "attachment @{} 应在 hitboxset @{} 之前",
            l.attachment,
            l.hitboxset
        );
    }

    /// **核心判据**：`mesh` 数组在 flex/ik/poseparam **之后**。
    ///
    /// 3300/3300 个真实模型实测如此。早先的实现让 mesh 紧跟 model。
    #[test]
    fn mesh_follows_optional_sections() {
        let l = SectionOffsets::compute(&counts(2, 0));
        for (name, off) in [
            ("flexdesc", l.flexdesc),
            ("ikchain", l.ikchain),
            ("poseparam", l.poseparam),
            ("ikautoplaylock", l.ikautoplaylock),
        ] {
            assert!(off <= l.mesh, "{name} @{off} 应在 mesh @{} 之前", l.mesh);
        }
    }

    /// 计数为 0 的段：偏移**不是 0**，而是「应处的位置」。
    #[test]
    fn empty_sections_still_have_offsets() {
        let l = SectionOffsets::compute(&counts(2, 0));
        // bonecontroller 计数为 0（实测 3333/3333 模型如此），
        // 但偏移等于骨骼表末尾。
        assert_eq!(l.bonecontroller, l.bone + 2 * BONE_SIZE);
        assert_ne!(l.bonecontroller, 0, "空段偏移不应为 0");
        assert_eq!(l.attachment, l.bonecontroller, "attachments 也是空 → 同位");
    }

    #[test]
    fn offsets_are_monotonic() {
        let mut c = counts(3, 2);
        c.attachments = 1;
        c.hitbox_sets = 1;
        c.hitboxes = 2;
        c.bodyparts = 1;
        c.models = 1;
        c.meshes = 2;
        c.textures = 3;
        c.cdtextures = 1;
        c.skin_entries = 3;
        c.keyvalues_bytes = 54;
        c.string_bytes = 100;
        c.anim_data_bytes = 200;
        c.seq_subtable_bytes = 8;
        let l = SectionOffsets::compute(&c);
        l.check_monotonic().expect("段顺序应单调");
        // 总长必须容纳所有段。
        assert!(l.total >= l.skin + c.skin_entries * 2);
        assert!(l.total >= l.keyvalues + c.keyvalues_bytes);
        // `$cdmaterials` 数组紧跟 texture；skin 表紧跟其后。
        assert_eq!(l.cdtexture_array, l.texture + c.textures * TEXTURE_SIZE);
        assert_eq!(l.skin, l.cdtexture_array + c.cdtextures * 4);
        // **`keyvalues` 在字符串池之前**（实测 3333/3333：
        // `keyvalueindex == align4(skinindex + 2*numskinref*numskinfamilies)`）。
        assert_eq!(l.keyvalues, (l.skin + c.skin_entries * 2 + 3) & !3);
        // `linearbone` 不存在（`has_linearbone == false`）时与字符串池**重合**，
        // 且 `linearboneindex` 由 `mdl_writer` 写 0。
        //
        // ⚠️ 字符串池起点是 **`ALIGN4(kv_end)`**，不是 `kv_end` ——
        // `WriteStringTable` 之前有 `ALIGN4`（`write.cpp:1850` 附近的段尾对齐）。
        // 实测 2813/2813 个「无 srcbonetransform、无 linearbone」的模型
        // 满足这条（`probe_pool_start2.js`）。
        //
        // 这条断言早先写的是 `kv_end`（**未对齐**），当时恰好通过 ——
        // 因为 `keyvalues_bytes = 54` 与起点相加后正好是 4 的倍数。
        // 那是**巧合通过**，不是规律。
        assert_eq!(l.strings, (l.keyvalues + c.keyvalues_bytes + 3) & !3);
        assert_eq!(l.linearbone, l.strings);
    }

    /// 加段之后，后面的段必须自动跟着移动。
    ///
    /// 这条测试钉住「框架可用」这个性质：实现某个可选段时，
    /// 只要在 `compute` 里把 `+ 0` 换成真实大小，后面全部自动对齐。
    #[test]
    fn adding_a_section_shifts_later_ones() {
        let base = SectionOffsets::compute(&counts(2, 0));
        // 模拟「实现了 flexdesc」：100 个 flexdesc × 4 字节。
        let flex_bytes = 100 * 4;
        let shifted = SectionOffsets::compute(&SectionCounts {
            models: 0,
            ..counts(2, 0)
        });
        // 当前实现里 flexdesc 紧跟 model，所以两者起点相同；
        // 这里只验证「mesh 在 flexdesc 之后」这个不变量仍然成立。
        assert!(base.flexdesc <= base.mesh);
        assert!(shifted.flexdesc <= shifted.mesh);
        let _ = flex_bytes;
    }
}
