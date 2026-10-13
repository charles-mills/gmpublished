//! 编译前端：把「描述文件 + SMD」编译成写出器要的 IR。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{
    BodyModel, Bone, CompiledBodyPart, CompiledModel, CompiledModelDesc, DescError,
    MAX_BONES_PER_VERT, Mesh, ModelDesc, ModelLods, Vertex,
};
use crate::smd::{Smd, SmdError, SmdPose, parse_smd};

/// 编译前端错误。
///
/// # 为什么是枚举
///
/// 先前它是 `struct { at: String, message: String }` —— 所有失败都被压成
/// 一句话，调用方拿到 `read_source()` 的错误后**无法分辨**是「文件读不到」
/// 还是「SMD 语法错」还是「FBX 的某个特性不支持」，只能去正则匹配中文。
///
/// 现在每一类都保留**产生它的那个错误类型**，`source()` 能走到底：
///
/// ```no_run
/// # use mdlc::compile::{CompileError, read_source, SrcOpts};
/// # use std::path::Path;
/// match read_source(Path::new("x.smd"), "model", &SrcOpts::default()) {
///     Err(CompileError::ReadFile { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
///         println!("文件不存在，该提示用户检查路径");
///     }
///     Err(CompileError::ParseSmd { source, .. }) => {
///         println!("第 {} 行语法错，可以在编辑器里跳过去", source.line);
///     }
///     _ => {}
/// }
/// ```
///
/// [`Self::Message`] 是兜底变体 —— 仍有约五十处只产出一句话的检查
/// （「`name` 不能为空」这类），它们没有更细的结构可暴露。但**凡是能拿到
/// 源错误的地方都不再用它**，所以 `match` 到 `Message` 就意味着「这条是
/// mdlc 自己的规则判定，不是底层库的错误」。
///
/// ⚠️ 因此本类型**不是** `Clone` / `PartialEq` / `Eq`：它装着
/// [`std::io::Error`]，那个类型三者都不实现。需要比较时请 `match` 变体。
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    /// 读取源文件失败（`read_smd` 的 `read_to_string`）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "%{at}: cannot read %{path}: %{err}",
            at = at,
            path = path.display(),
            err = source
        )
    )]
    ReadFile {
        at: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// SMD 文本解析失败（`read_smd`）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "%{at}: failed to parse %{path}: %{err}",
            at = at,
            path = path.display(),
            err = source
        )
    )]
    ParseSmd {
        at: String,
        path: PathBuf,
        #[source]
        source: SmdError,
    },
    /// `.vta` 解析失败。
    #[error(
        "{}",
        crate::tr_fmt!(
            "%{at}: failed to parse %{path}: %{err}",
            at = at,
            path = path.display(),
            err = source
        )
    )]
    ParseVta {
        at: String,
        path: PathBuf,
        #[source]
        source: crate::vta::VtaError,
    },
    /// 资产引用 / `src*` 选项的问题（[`SrcError`]）。
    #[error("{at}: {source}")]
    Src {
        at: String,
        #[source]
        source: SrcError,
    },
    /// FBX / glTF 读取失败（两者共用 [`crate::fbx::FbxError`]）。
    #[error("{at}: {source}")]
    Fbx {
        at: String,
        #[source]
        source: crate::fbx::FbxError,
    },
    /// flex 规格解析失败。
    #[error("{at}: {source}")]
    Flex {
        at: String,
        #[source]
        source: crate::flex::FlexError,
    },
    /// 描述层校验失败（[`crate::model::ModelDesc::validate`]）。
    ///
    /// ⚠️ 这里**不带 `at`** —— [`DescError`] 自己的 `path` 就是 `at`，
    /// 先前那处 `at: d.path.clone()` 是把它拆成两个 `String` 再拼回去。
    #[error("{0}")]
    Desc(#[from] DescError),
    /// 其余检查：只产出一句话，没有更细的结构。
    #[error("{at}: {message}")]
    Message { at: String, message: String },
}

impl CompileError {
    /// 出错位置 —— 描述文件里的路径（`models[0].bones[2]`），或源文件路径。
    ///
    /// 先前它是 `struct` 的公开字段，调用方 `err.at` 就能拿到；改成枚举后
    /// 由这个方法统一给出（[`Self::Desc`] 那支取的是
    /// [`DescError::path`]，与其余变体的 `at` 语义相同）。
    pub fn at(&self) -> &str {
        match self {
            Self::ReadFile { at, .. }
            | Self::ParseSmd { at, .. }
            | Self::ParseVta { at, .. }
            | Self::Src { at, .. }
            | Self::Fbx { at, .. }
            | Self::Flex { at, .. }
            | Self::Message { at, .. } => at,
            Self::Desc(err) => &err.path,
        }
    }

    /// 这条错误的说明文字，**不含** `at` 前缀。
    ///
    /// 先前它是 `struct` 的公开字段。想拿「位置 + 说明」的完整一行请用
    /// [`std::fmt::Display`]（即 `err.to_string()`）。
    ///
    /// ⚠️ 这里的文案与 [`std::fmt::Display`] 的那份**是两条独立的路径**
    /// （属性宏里写不出 `match`），改一处必须同时改另一处。
    pub fn message(&self) -> String {
        match self {
            Self::ReadFile { path, source, .. } => crate::tr_fmt!(
                "cannot read %{path}: %{err}",
                path = path.display(),
                err = source
            ),
            Self::ParseSmd { path, source, .. } => crate::tr_fmt!(
                "failed to parse %{path}: %{err}",
                path = path.display(),
                err = source
            ),
            Self::ParseVta { path, source, .. } => crate::tr_fmt!(
                "failed to parse %{path}: %{err}",
                path = path.display(),
                err = source
            ),
            Self::Src { source, .. } => source.to_string(),
            Self::Fbx { source, .. } => source.to_string(),
            Self::Flex { source, .. } => source.to_string(),
            Self::Desc(err) => err.kind.to_string(),
            Self::Message { message, .. } => message.clone(),
        }
    }
}

/// 模块内 `e()` 辅助函数的第二个参数 —— 决定 [`CompileError`] 落到哪个变体。
///
/// 有这个 trait，68 个 `e(at, ...)` 调用点才**一行都不用改**：
/// 传 `SrcError` 就得到 [`CompileError::Src`]，传 `FbxError` 就得到
/// [`CompileError::Fbx`]，传字符串则落到 [`CompileError::Message`]。
///
/// ⚠️ 新写代码时**优先传具体的错误类型**，不要 `err.to_string()` ——
/// 那正是本类型改成枚举要消灭的写法。
pub trait IntoCompileError {
    /// 把 `self` 与出错位置组合成一条编译错误。
    fn at(self, at: String) -> CompileError;
}

impl IntoCompileError for String {
    fn at(self, at: String) -> CompileError {
        CompileError::Message { at, message: self }
    }
}

impl IntoCompileError for &str {
    fn at(self, at: String) -> CompileError {
        CompileError::Message {
            at,
            message: self.to_string(),
        }
    }
}

impl IntoCompileError for SrcError {
    fn at(self, at: String) -> CompileError {
        CompileError::Src { at, source: self }
    }
}

impl IntoCompileError for crate::fbx::FbxError {
    fn at(self, at: String) -> CompileError {
        CompileError::Fbx { at, source: self }
    }
}

impl IntoCompileError for crate::flex::FlexError {
    fn at(self, at: String) -> CompileError {
        CompileError::Flex { at, source: self }
    }
}

/// 资产引用 / `src*` 选项的错误。
///
/// 与 [`CompileError`] 分开，是因为这些错误**不依赖任何文件内容** ——
/// 只由「用户写的路径字符串」与「`src*` 选项的值」决定，所以
/// [`crate::model::ModelDesc::validate`] 那类纯校验路径也能直接用它，
/// 不必先编出一个 `at` 定位串。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SrcError {
    #[error(
        "implicit source {path} uses an unsupported format; name a supported source file explicitly"
    )]
    UnsupportedImplicitSource { path: String },
    /// 扩展名不在支持列表里（[`SourceKind::of`]）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "unrecognized asset format %{other} (%{path}): only .smd, .fbx and .gltf/.glb \
             are supported",
            other = other.as_deref().map_or_else(|| "None".to_string(), |o| format!("{o:?}")),
            path = path
        )
    )]
    UnknownFormat { other: Option<String>, path: String },
    /// `src_axis` 的值域错误（[`SrcOpts::of_model`]）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "src_axis only accepts \"auto\" / \"y\" / \"z\" (yup / y-up / zup / z-up are also accepted, \
             case-insensitively), but got %{value}",
            value = format!("{value:?}")
        )
    )]
    BadAxis { value: String },
    /// `srcshapekey` 名单里有源文件里不存在的名字（[`SrcOpts::order_shape_keys`]）。
    #[error(
        "{}",
        crate::tr_fmt!(
            "the %{missing} in srcshapekey do not exist in the source file; \
             the available ones are %{have}",
            missing = missing.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(" / "),
            have = format!("{have:?}")
        )
    )]
    ShapeKeyMissing {
        missing: Vec<String>,
        have: Vec<String>,
    },
}

/// 未配 `eyelid` 的 eyeball 用的占位 flexdesc 名（L4D2 行为，见
/// [`resolve_flex_eyeball_mouth`] 的「0.」小节）。
const DUMMY_EYELID: &str = "dummy_eyelid";

/// `sectionframes` 的缺省段长（studiomdl `.data` 静态初值 = **30**）。
const DEFAULT_SECTION_FRAMES: i32 = 30;
/// `sectionframes` 的缺省触发阈值（studiomdl `.data` 静态初值 = **120**）。
const DEFAULT_SECTION_THRESHOLD: i32 = 120;

/// 未配 `eyelid` 时 lid 的缺省 target —— 受控实验实测**恒为 `[-1, 0, 1]`**
/// （`fxr1`/`fx2`/`fxl1` 三个用例一致，且与 eyeball 的 `radius` 无关）。
const DUMMY_LID_TARGETS: [f32; 3] = [-1.0, 0.0, 1.0];

fn e(at: impl Into<String>, message: impl IntoCompileError) -> CompileError {
    message.at(at.into())
}

/// 浮点去重键：按位比较。
///
/// 用 `to_bits` 而不是 `==` —— `-0.0 == 0.0` 但二者在文件里是不同的字节，
/// 而且 `NaN` 用 `==` 永远不相等会让去重失效。
///
/// # 为什么 `bones` 是内联数组而不是 `Vec`
///
/// `Vertex.bones` 最多 [`MAX_BONES_PER_VERT`] 组（`smd_vertex_to_ir` 里已截断），
/// 所以键里的骨骼部分天然定长。早期版本用 `Vec<(i32, u32)>`，
/// **每个顶点**都要分配一次堆内存（100 万三角形约 300 万次）。
///
/// 用「定长数组 + 计数」后零分配。为了保持 `Hash`/`Eq` 的语义与 `Vec` 版本
/// **完全一致**，比较与哈希只看前 `n` 项（见下面的手写实现）——
/// 尾部未使用的槽位不参与，否则「3 组」与「1 组 + 2 个填充」会被判成不同。
#[derive(Debug, Clone, Copy)]
struct VertexKey {
    pos: [u32; 3],
    normal: [u32; 3],
    uv: [u32; 2],
    /// 排序后的 (骨骼下标, 权重位模式)，前 `n_bones` 项有效。
    bones: [(i32, u32); MAX_BONES_PER_VERT],
    n_bones: u8,
}

impl VertexKey {
    #[inline]
    fn bones_slice(&self) -> &[(i32, u32)] {
        &self.bones[..self.n_bones as usize]
    }
}

impl PartialEq for VertexKey {
    fn eq(&self, other: &Self) -> bool {
        self.pos == other.pos
            && self.normal == other.normal
            && self.uv == other.uv
            && self.bones_slice() == other.bones_slice()
    }
}
impl Eq for VertexKey {}

impl std::hash::Hash for VertexKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.pos.hash(state);
        self.normal.hash(state);
        self.uv.hash(state);
        // 与 `Vec` 的 Hash 对齐：先写长度，再写元素。
        // `Vec<T>` 的 `Hash` 是 `len.hash()` + 每项 `hash()`（含长度前缀），
        // 这里复刻同样的口径，保证哈希分布不退化。
        self.bones_slice().hash(state);
    }
}

fn fbits(v: f32) -> u32 {
    // 归一化 -0.0 → 0.0，否则同一位置会被当成两个顶点。
    if v == 0.0 {
        0.0f32.to_bits()
    } else {
        v.to_bits()
    }
}

fn vertex_key(v: &Vertex) -> VertexKey {
    let n = v.bones.len().min(MAX_BONES_PER_VERT);
    let mut bones = [(0i32, 0u32); MAX_BONES_PER_VERT];
    for (i, p) in v.bones.iter().take(n).enumerate() {
        bones[i] = (p[0] as i32, fbits(p[1]));
    }
    // 排序让「绑定顺序不同但语义相同」的顶点也能合并。
    bones[..n].sort_unstable();
    VertexKey {
        pos: [fbits(v.pos[0]), fbits(v.pos[1]), fbits(v.pos[2])],
        normal: [fbits(v.normal[0]), fbits(v.normal[1]), fbits(v.normal[2])],
        uv: [fbits(v.uv[0]), fbits(v.uv[1])],
        bones,
        n_bones: n as u8,
    }
}

/// 官方 `lookup_index` 的**法线焊接阈值**：`cos(2°)`。
///
/// `studiomdl.cpp:6895`：
/// ```c
/// normal_blend = cos( DEG2RAD( 2.0 ));   // ≈ 0.99939083
/// ```
///
/// 可由 `-a <normal_blend_angle>` 覆盖（`studiomdl.cpp:7044`），mdlc 只实现缺省值。
const NORMAL_BLEND: f32 = 0.999_390_8;

/// 「位置 + UV」次级索引的键 —— 法线容差查找用它把候选集缩到极小。
///
/// 官方是**线性扫全池**（`for (i = 0; i < numvlist; i++)`），
/// 那在 30 万顶点上是 O(n²)。用位置+UV 建桶后，候选只剩同位置的几个顶点，
/// **语义完全等价**（官方那三个判据里，位置与 UV 都是**精确相等**，
/// 所以桶内就是官方的候选集，只是顺序可能不同 —— 而官方取「第一个命中」，
/// 我们取「桶里第一个命中的」，对同一位置而言结果一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PosUvKey {
    pos: [u32; 3],
    uv: [u32; 2],
}

/// **按官方 `lookup_index` 的口径**在池里找一个可复用的顶点，找不到就追加。
///
/// # 官方规则（`v1support.cpp:32-47`）
///
/// ```c
/// for (i = 0; i < numvlist; i++) {
///     if (v_listdata[i].m == material
///         && DotProduct( g_normal[i], normal ) > normal_blend   // ← **2° 容差**
///         && VectorCompare( g_vertex[i], vertex )               // ← 位置**精确**
///         && g_texcoord[i][0] == texcoord[0]                    // ← UV **精确**
///         && g_texcoord[i][1] == texcoord[1])
///         return i;                                             // 复用
/// }
/// ```
///
/// # 为什么不能只用法线精确相等（实测）
///
/// SMD 里同一位置的相邻面，法线常差 **1 ULP**（十进制最后一位）：
///
/// ```text
///   studio: 16.326813,0.516793,45.696392|0.004434,-0.000006,-0.999990|…
///   mdlc  : 16.326813,0.516799,45.696400|0.004435,-0.000001,-0.999990|…
/// ```
///
/// 官方用 2° 容差**焊掉**它们；mdlc 原先要求法线**逐位相等** ⟹ 多留顶点。
///
/// 实测（`docs/_probe/verify_normal_blend_weld.js`）：按官方口径重算，
/// **6/6 模型的顶点数与官方精确相同**；其中 `v_sniper_military` 从 13518 → **13515**
/// （官方值）。「因法线容差而焊接」的次数是 1266~16786 次/模型 ——
/// **绝大多数焊接靠的是这条容差**，不是精确相等。
///
/// > ⚠️ 官方**不比骨骼绑定**。6 个测试模型里「几何相同但骨骼不同」的情形
/// > 出现 0 次（`probe_weld_bones.js`），所以这里保留骨骼比较 ——
/// > 它更安全（不会把不同蒙皮的顶点合并），且在真实数据上等价。
fn weld_or_push(
    pool: &mut Vec<Vertex>,
    table: &mut HashMap<VertexKey, u32>,
    secondary: &mut HashMap<PosUvKey, Vec<u32>>,
    v: &Vertex,
) -> u32 {
    let key = vertex_key(v);
    // ① 精确命中（绝大多数顶点走这条，O(1)）。
    //
    // ⚠️ **即便位置/法线/UV/骨骼全部逐位相同，也还要过法线判据。**
    //
    // 官方的判据只有一条（`v1support.cpp:39`）：
    //
    // ```c
    // if (v_listdata[i].m == material
    //     && DotProduct( g_normal[i], normal ) > normal_blend   // ← 唯一的法线判据
    //     && VectorCompare( g_vertex[i], vertex )
    //     && g_texcoord[i][0] == texcoord[0]
    //     && g_texcoord[i][1] == texcoord[1])
    // ```
    //
    // `DotProduct` 对**零法线**恒为 `0`，而 `0 > cos(2°) = 0.99939` 为 **false**
    // ⟹ 官方**永远不会**因为「两个零法线长得一样」而复用顶点，它总是追加。
    //
    // 精确命中路径里两侧法线**逐位相同**，所以点积恰好是 `|n|²`：
    //
    // | 法线 | `|n|²` | 官方 | mdlc 修前 |
    // |---|---|---|---|
    // | 单位向量 | `1` | 复用 ✅ | 复用 ✅ |
    // | **零法线** | `0` | **追加** | **复用** ❌ |
    // | `|n| = 0.5` | `0.25` | 追加 | 复用 ❌ |
    //
    // 实测（`v_dual_pistola`，用户工程）：4 个 `nrm=[0,0,0]` 的顶点被
    // 多焊掉，VVD 少 4 个顶点（`111113 → 111109`），
    // 两个 bodypart 的 `mesh[4]` 由官方/NekoMDL 的 **5532** 变成 **5530**。
    // 顶点池一变，`origMeshVertID` 与整条 VTX 索引全部错位
    // ⟹ **HLMV 里网格被撕开**。
    //
    // 这一条对单位法线是**恒真**的（`1 > 0.99939`），所以只影响退化法线。
    if let Some(&i) = table.get(&key) {
        let e = &pool[i as usize];
        let d = e.normal[0] * v.normal[0] + e.normal[1] * v.normal[1] + e.normal[2] * v.normal[2];
        if d > NORMAL_BLEND {
            return i;
        }
    }
    // ② 法线容差命中：同位置 + 同 UV，且法线夹角 < 2°。
    //
    // ⚠️ 这一步必须**只在精确未命中时**做 —— 否则每个顶点都要线性扫描池子，
    // 25 万顶点的模型会退化成 O(n²)。精确命中是主路径，容差是补漏。
    //
    // 用「位置 + UV」做**次级索引**，把候选集缩到极小（同一位置的顶点数通常是个位数）。
    let posuv = PosUvKey {
        pos: key.pos,
        uv: key.uv,
    };
    let bones = key.bones_slice();
    if let Some(cands) = secondary.get(&posuv) {
        for &i in cands {
            let e = &pool[i as usize];
            if e.bones.len() != bones.len() {
                continue;
            }
            // 骨骼必须一致（见上面的说明）。
            let mut same_bones = true;
            for (k, p) in e.bones.iter().enumerate() {
                if p[0] as i32 != bones[k].0 || fbits(p[1]) != bones[k].1 {
                    same_bones = false;
                    break;
                }
            }
            if !same_bones {
                continue;
            }
            let d =
                e.normal[0] * v.normal[0] + e.normal[1] * v.normal[1] + e.normal[2] * v.normal[2];
            if d > NORMAL_BLEND {
                return i;
            }
        }
    }
    let i = pool.len() as u32;
    pool.push(v.clone());
    table.insert(key, i);
    secondary.entry(posuv).or_default().push(i);
    i
}

/// `$staticprop` 的几何旋转：绕 Z 轴 +90°。
///
/// # 来源
///
/// `studiomdl.cpp:6883` 设下全局默认旋转：
///
/// ```cpp
/// g_defaultrotation = RadianEuler( 0, 0, M_PI / 2 );
/// ```
///
/// `MakeStaticProp()`（`simplify.cpp:3273`）用
/// `AngleMatrix(g_defaultrotation, rotated)` 建矩阵，然后对**每个顶点**把
/// 位置、法线、切线各旋转一次（`simplify.cpp:3310-3319`）。
///
/// `AngleMatrix(RadianEuler(0,0,π/2))` 展开后是
/// `[[0,-1,0],[1,0,0],[0,0,1]]`，作用在点上即：
///
/// ```text
/// (x, y, z) -> (-y, x, z)          Z 分量不变
/// ```
///
/// 这与 `$eyeposition` / `$illumposition` 的轴变换（[`crate::mdl_writer`]
/// 的 `qc_axis_to_model`）**是同一个映射** —— 见
/// `docs/coordinate-systems.md`。
///
/// # 实测佐证
///
/// 同一份 SMD 只差 `$staticprop`（`docs/_probe/smdl/ipa1.smd`）：
///
/// | 产物 | VVD 顶点 | 法线 |
/// |---|---|---|
/// | `ipa1`（无） | `(0,0,0) (10,0,3) (0,20,7)` | `(1,0,0)` |
/// | `ipa2`（有） | `(0,0,0) (0,10,3) (-20,0,7)` | `(0,1,0)` |
///
/// 逐分量吻合。
pub fn static_prop_rotate(v: [f32; 3]) -> [f32; 3] {
    [-v[1], v[0], v[2]]
}

/// Extensionless geometry and animation sources default to SMD.
/// StudioMDL gives VRM precedence, so an existing VRM must fail explicitly.
pub fn resolve_smd_path(base_dir: &Path, smd: &str) -> Result<PathBuf, SrcError> {
    resolve_source_path(base_dir, smd, "smd")
}

pub fn resolve_vta_path(base_dir: &Path, vta: &str) -> Result<PathBuf, SrcError> {
    resolve_source_path(base_dir, vta, "vta")
}

fn resolve_source_path(
    base_dir: &Path,
    source: &str,
    default_ext: &str,
) -> Result<PathBuf, SrcError> {
    let normalized = source.replace('\\', "/");
    let p = Path::new(&normalized);
    let mut path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    };
    if p.extension().is_none_or(|ext| ext.is_empty()) {
        if default_ext == "smd" {
            let vrm = path.with_extension("vrm");
            if vrm.is_file() {
                return Err(SrcError::UnsupportedImplicitSource {
                    path: vrm.display().to_string(),
                });
            }
            let smd = path.with_extension("smd");
            if !smd.is_file() {
                for ext in ["sma", "phys", "vta", "obj"] {
                    let candidate = path.with_extension(ext);
                    if candidate.is_file() {
                        return Err(SrcError::UnsupportedImplicitSource {
                            path: candidate.display().to_string(),
                        });
                    }
                }
            }
        }
        path.set_extension(default_ext);
    }
    Ok(path)
}

/// 读取并解析一个 SMD 文件。
fn read_smd(path: &Path, at: &str) -> Result<Smd, CompileError> {
    let text = std::fs::read_to_string(path).map_err(|source| CompileError::ReadFile {
        at: at.to_string(),
        path: path.to_path_buf(),
        source,
    })?;
    parse_smd(&text).map_err(|source| CompileError::ParseSmd {
        at: at.to_string(),
        path: path.to_path_buf(),
        source,
    })
}

/// 一个资产引用的**格式**，由扩展名决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// `.smd` —— 文本 SMD。
    Smd,
    /// `.fbx` —— FBX（走 `ufbx`）。
    Fbx,
    /// `.gltf` / `.glb` —— glTF 2.0（走 `gltf` crate；官方**完全不支持**这个格式，
    /// 见 `docs/gltf-support.md` §1）。
    Gltf,
}

impl SourceKind {
    /// 按扩展名判定格式。
    ///
    /// ⚠️ 只对**几何源 / 动画源**调用。`.vta` 走 [`resolve_vta_path`] 但
    /// **不**经过这里（它有自己的解析器，见 [`crate::vta`]）。
    pub fn of(path: &Path) -> Result<Self, SrcError> {
        let ext = path
            .extension()
            .and_then(|x| x.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("smd") => Ok(SourceKind::Smd),
            Some("fbx") => Ok(SourceKind::Fbx),
            // `.gltf` 是分离式（JSON + 外部 `.bin`），`.glb` 是单文件容器；
            // 两者走同一条读取路径（`gltf::Gltf::from_slice_without_validation`
            // 自己认头四字节 `glTF`）。
            Some("gltf" | "glb") => Ok(SourceKind::Gltf),
            other => Err(SrcError::UnknownFormat {
                other: other.map(str::to_string),
                path: path.display().to_string(),
            }),
        }
    }
}

/// Only explicit supported extensions qualify; extensionless QC names are resolved separately.
pub fn is_source_ref(text: &str) -> bool {
    SourceKind::of(Path::new(text)).is_ok()
}

/// 一次源读取的全部选项 —— `src*` 扩展语法落到读取层的**唯一**载体。
///
/// # 为什么要有这个结构
///
/// `srcpart` / `srcmaterial` / `srcscale` / `srcaxis` 属于**几何**侧，
/// `srcstack` / `srcfps` 属于**动画**侧，但两者最终都只是
/// [`crate::fbx::FbxOpts`] 加两个标量。QC 前端与 TOML 都填这个结构，
/// 三个读取函数都吃它，于是「选项从哪来」与「选项怎么用」彻底解耦 ——
/// 读取层完全不知道 QC 存在。
///
/// # 默认值 = 官方行为
///
/// [`Default`] 是 [`crate::fbx::FbxOpts::default()`]（不干预单位与轴向、
/// 全部网格合并、无材质时合成 `debug/debugempty`）+ 第一条动画栈 +
/// [`crate::fbx::DEFAULT_FPS`]，逐条对齐官方实测（`docs/fbx-support.md` §1）。
///
/// Format-specific `src*` options leave SMD unchanged; QC `input_scale` applies to every format.
#[derive(Debug, Clone, Default)]
pub struct SrcOpts {
    /// QC scale, separate from format-specific import scale.
    pub input_scale: Option<f32>,
    pub root_adjust: Option<[f32; 3]>,
    pub root_rotation: Option<[f32; 3]>,
    /// FBX 的几何与变换选项。
    pub fbx: crate::fbx::FbxOpts,
    /// `srcstack`：按名字选动画栈。`None` = 第一条（官方行为）。
    pub stack: Option<String>,
    /// `srcfps`：FBX 动画的重采样率。`0.0` = [`crate::fbx::DEFAULT_FPS`]。
    pub fps: f32,
    /// `srcshapekey`：只取这些形变目标，**且按本列表的顺序定帧号**。
    pub shape_keys: Vec<String>,
    /// `srcshapekeyorder`：只定序（名单外的按文件顺序排在其后）。
    pub shape_key_order: Vec<String>,
    /// `srcshapekeyignore`：一个形变目标都不要。
    pub shape_key_ignore: bool,
}

impl SrcOpts {
    /// 从一条 `$body` / `$model` 的选项构造（几何侧）。
    ///
    /// `src_axis` 的值域错误在这里就报出来 —— [`crate::model::ModelDesc::validate`]
    /// 也会查一遍，但编译路径不该依赖「调用方先校验过」。
    pub fn of_model(m: &crate::model::BodyModel) -> Result<Self, SrcError> {
        let axis = match m.src_axis.as_deref() {
            None => None,
            Some(s) => Some(
                crate::fbx::ForcedAxis::parse(s).ok_or_else(|| SrcError::BadAxis {
                    value: s.to_string(),
                })?,
            ),
        };
        Ok(Self {
            input_scale: m.input_scale,
            root_adjust: None,
            root_rotation: None,
            fbx: crate::fbx::FbxOpts {
                parts: m.src_parts.clone(),
                material: m.src_material.clone(),
                scale: m.src_scale.unwrap_or(1.0),
                axis,
            },
            stack: None,
            fps: 0.0,
            shape_keys: m.src_shape_keys.clone(),
            shape_key_order: m.src_shape_key_order.clone(),
            shape_key_ignore: m.src_shape_key_ignore,
        })
    }

    /// 叠加动画侧的两个选项（`$animation` / `$sequence` 的 `srcstack` / `srcfps`）。
    ///
    /// `fps` 的 `None` 与 `0.0` 都表示「用官方默认 30」，所以这里只在
    /// 给了**正数**时覆盖。
    pub fn with_anim(&self, stack: Option<&str>, fps: Option<f32>) -> Self {
        Self {
            input_scale: self.input_scale,
            root_adjust: self.root_adjust,
            root_rotation: self.root_rotation,
            fbx: self.fbx.clone(),
            stack: stack.map(str::to_string).or_else(|| self.stack.clone()),
            fps: fps.filter(|f| *f > 0.0).unwrap_or(self.fps),
            shape_keys: self.shape_keys.clone(),
            shape_key_order: self.shape_key_order.clone(),
            shape_key_ignore: self.shape_key_ignore,
        }
    }

    pub fn with_scale(mut self, scale: Option<f32>) -> Self {
        self.input_scale = scale;
        self
    }

    pub fn with_root_transform(
        mut self,
        adjust: Option<[f32; 3]>,
        rotation: Option<[f32; 3]>,
    ) -> Self {
        self.root_adjust = adjust;
        self.root_rotation = rotation;
        self
    }

    /// 实际使用的重采样率。
    pub fn fps_or_default(&self) -> f32 {
        if self.fps > 0.0 {
            self.fps
        } else {
            crate::fbx::DEFAULT_FPS
        }
    }

    /// 按 `srcshapekey` / `srcshapekeyorder` / `srcshapekeyignore` 处理一份
    /// 形变目标表，返回**定好序**的结果（帧号 = 下标 + 1）。
    ///
    /// # 三条规则
    ///
    /// * `ignore` ⟹ 返回空（一个 flex 都不注册）
    /// * `shape_keys` 非空 ⟹ **过滤 + 定序**：只留名单里的，按名单顺序；
    ///   名单里有但源里没有的**报错**（静默丢掉是官方最坑的行为之一）
    /// * `shape_key_order` 非空 ⟹ **只定序**：名单里的排前面（按名单顺序），
    ///   其余的按源顺序跟在其后
    ///
    /// 两个名单都为空 ⟹ 原样返回（官方行为：文件顺序 + 1）。
    pub fn order_shape_keys(
        &self,
        keys: &[crate::fbx::FbxShapeKey],
    ) -> Result<Vec<crate::fbx::FbxShapeKey>, SrcError> {
        if self.shape_key_ignore {
            return Ok(Vec::new());
        }
        if self.shape_keys.is_empty() && self.shape_key_order.is_empty() {
            return Ok(keys.to_vec());
        }
        let find = |want: &str| -> Option<&crate::fbx::FbxShapeKey> {
            keys.iter().find(|k| k.name.eq_ignore_ascii_case(want))
        };
        if !self.shape_keys.is_empty() {
            let mut out = Vec::with_capacity(self.shape_keys.len());
            let mut missing = Vec::new();
            for want in &self.shape_keys {
                match find(want) {
                    Some(k) => out.push(k.clone()),
                    None => missing.push(want.clone()),
                }
            }
            if !missing.is_empty() {
                return Err(SrcError::ShapeKeyMissing {
                    missing,
                    have: keys.iter().map(|k| k.name.clone()).collect(),
                });
            }
            return Ok(out);
        }
        // 只定序：名单里的在前（按名单顺序），其余按源顺序。
        let mut out: Vec<crate::fbx::FbxShapeKey> = Vec::with_capacity(keys.len());
        for want in &self.shape_key_order {
            if let Some(k) = find(want) {
                out.push(k.clone());
            }
        }
        for k in keys {
            if !out.iter().any(|o| o.name == k.name) {
                out.push(k.clone());
            }
        }
        Ok(out)
    }
}

/// 读取一个源文件（`.smd` 或 `.fbx`），统一成中立的 [`Smd`]。
///
/// # 这是**唯一**的格式分派点
///
/// `compile.rs` 有一万多行，其余部分只认 [`Smd`]。让泛型或 `dyn` 穿过整条
/// 管线会把编译期与运行期成本都抬起来，而格式来自**运行期**的路径字符串，
/// 没有任何可以「延迟到单态化」的东西；读一个源文件在整次编译里也只发生
/// 四五次。所以这里用一次 `match`，把格式问题**收在一个函数里**
/// （选型讨论见 `docs/fbx-support.md` §6.2）。
///
/// # 默认值全部与官方一致
///
/// FBX 侧用的是 [`SrcOpts::default()`]，即「不干预单位与轴向、全部网格合并、
/// 无材质时合成 `debug/debugempty`」—— 逐条对齐官方实测
/// （`docs/fbx-support.md` §1）。mdlc 扩展语法（`srcpart` / `srcscale` /
/// `srcaxis` …）由调用方填进 `opts`。
pub fn read_source(path: &Path, at: &str, opts: &SrcOpts) -> Result<Smd, CompileError> {
    let mut smd = match SourceKind::of(path).map_err(|msg| e(at, msg))? {
        SourceKind::Smd => read_smd(path, at),
        SourceKind::Fbx => crate::fbx::read(path, at, &opts.fbx)
            .map(|g| g.smd)
            .map_err(|err| e(at, err.to_string())),
        SourceKind::Gltf => crate::gltf::read(path, at, &opts.fbx)
            .map(|g| g.smd)
            .map_err(|err| e(at, err.to_string())),
    }?;
    apply_source_scale(&mut smd, opts, at)?;
    Ok(smd)
}

/// Geometry and local bone translations must scale together to preserve bind poses.
pub fn scale_source(smd: &mut Smd, scale: f32) {
    if scale == 1.0 {
        return;
    }
    for frame in &mut smd.frames {
        for pose in &mut frame.poses {
            for value in &mut pose.position {
                *value *= scale;
            }
        }
    }
    for triangle in &mut smd.triangles {
        for vertex in &mut triangle.vertices {
            for value in &mut vertex.position {
                *value *= scale;
            }
        }
    }
}

fn apply_source_scale(smd: &mut Smd, opts: &SrcOpts, at: &str) -> Result<(), CompileError> {
    let scale = opts.input_scale.unwrap_or(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(e(
            at,
            "source scale must be a positive finite number; mirrored scaling is not yet supported",
        ));
    }
    scale_source(smd, scale);
    Ok(())
}

/// 一个**几何源**读出来的全部内容。
pub struct SourceGeometry {
    /// 中立几何（节点 / 参考姿态 / 三角形）。
    pub smd: Smd,
    /// FBX 的 shape key（**已变换到 Source 空间**）。SMD 源恒为空。
    ///
    /// 官方把 FBX 的 shape key 全自动注册成 flex，所以这份数据不能像
    /// [`read_source`] 那样丢掉 —— 丢了就是「模型能编译，但表情不见了」。
    pub shape_keys: Vec<crate::fbx::FbxShapeKey>,
    /// FBX 里**没有材质**、被合成了兜底名的网格名（去重）。
    ///
    /// 官方静默合成 `debug/debugempty`（或用 `srcmaterial` 给的名字）；
    /// mdlc 读出来只为发一条诊断（`docs/fbx-support.md` §4.4）。SMD 源恒为空。
    pub untextured_meshes: Vec<String>,
    /// 被**合并进同一个 model** 的网格名（去重）。
    ///
    /// 官方把 FBX 里的多块网格合并成一个 `mstudiomodel_t`（§1.10），部件
    /// 边界丢失。只有多于一块时才有意义。SMD 源恒为空。
    pub merged_meshes: Vec<String>,
    /// FBX 里的**动画栈名**（按文件顺序）。
    ///
    /// 官方恒取第一条（§1.8）。多于一条时 mdlc 发诊断提醒用户
    /// 用 `srcstack` 选。SMD 源恒为空。
    pub anim_stacks: Vec<String>,
}

/// 读取一个**几何源**，保留 FBX 的 shape key。
///
/// 与 [`read_source`] 的唯一区别就是多带一份 [`SourceGeometry::shape_keys`]；
/// 只有 `$body` / `$model` 的主网格需要它，LOD 与 QC 的文件存在性检查走
/// [`read_source`] 就够（前者官方也不产出 flex，后者根本不看内容）。
pub fn read_source_geometry(
    path: &Path,
    at: &str,
    opts: &SrcOpts,
) -> Result<SourceGeometry, CompileError> {
    let mut geom = match SourceKind::of(path).map_err(|msg| e(at, msg))? {
        SourceKind::Smd => read_smd(path, at).map(|smd| SourceGeometry {
            smd,
            shape_keys: Vec::new(),
            untextured_meshes: Vec::new(),
            merged_meshes: Vec::new(),
            anim_stacks: Vec::new(),
        }),
        SourceKind::Fbx => crate::fbx::read(path, at, &opts.fbx)
            .map(|g| SourceGeometry {
                smd: g.smd,
                shape_keys: g.shape_keys,
                untextured_meshes: g.untextured_meshes,
                merged_meshes: g.merged_meshes,
                anim_stacks: g.anim_stacks,
            })
            .map_err(|err| e(at, err.to_string())),
        SourceKind::Gltf => crate::gltf::read(path, at, &opts.fbx)
            .map(|g| SourceGeometry {
                smd: g.smd,
                shape_keys: g.shape_keys,
                untextured_meshes: g.untextured_meshes,
                merged_meshes: g.merged_meshes,
                anim_stacks: g.anim_stacks,
            })
            .map_err(|err| e(at, err.to_string())),
    }?;
    apply_source_scale(&mut geom.smd, opts, at)?;
    let scale = opts.input_scale.unwrap_or(1.0);
    if scale != 1.0 {
        for shape in &mut geom.shape_keys {
            for offset in &mut shape.position_offsets {
                for value in offset {
                    *value *= scale;
                }
            }
        }
    }
    Ok(geom)
}

/// FBX 源的四条「官方 `exit=0` 但结果可疑」诊断（`docs/fbx-support.md` §4.4）。
///
/// 全部是**提示**，不是错误 —— 这些情形官方都静默通过，用户的模型也确实
/// 能编译出来，只是结果可能不是他想要的。SMD 源三个字段恒为空，所以这个
/// 函数对 `.smd` 工程返回空表（连一行输出都不产生）。
///
/// 参数里的 `shape_keys` 是**已经过 `srcshapekey*` 过滤/定序之后**的那一份：
/// 诊断要说的是「这次编译实际注册了几个 flex」，用过滤前的那份会骗人。
///
/// ⚠️ 纯函数（返回文案而不是直接打印）—— 直接打印的话这条路径就没法测了
/// （`log` 的 logger 是**进程级全局**的，测试里装不了第二个）。打印由
/// [`emit_fbx_diagnostics`] 负责。
fn fbx_diagnostics(
    at: &str,
    geom: &SourceGeometry,
    shape_keys: &[crate::fbx::FbxShapeKey],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // ① 多块网格被合并进同一个 `mstudiomodel_t`（§1.10，部件边界丢失）。
    //    只有多于一块时才提 —— 一块网格是绝大多数情况，不值得打扰。
    if geom.merged_meshes.len() > 1 {
        out.push(crate::tr_fmt!(
            "Note: the %{n} meshes of %{at} were merged into one part: %{meshes}. The official tool merges them the same way (part boundaries are lost); to keep them apart, select them one by one with `srcpart`, or split them into several `$body` / `$model` blocks.",
            n = geom.merged_meshes.len(),
            at = at,
            meshes = format!("{:?}", geom.merged_meshes)
        ));
    }
    // ② 网格没有材质 ⟹ 合成了兜底名。官方静默用 `debug/debugempty`。
    if !geom.untextured_meshes.is_empty() {
        out.push(crate::tr_fmt!(
            "Note: the meshes %{meshes} of %{at} have no material, so %{fallback} was synthesized. To use another name, write `srcmaterial \"...\"`.",
            meshes = format!("{:?}", geom.untextured_meshes),
            at = at,
            fallback = format!("{:?}", crate::fbx::FALLBACK_MATERIAL)
        ));
    }
    // ③ 多动画栈：官方恒取第一条（§1.8），用户想用第二条只能靠 `srcstack`。
    //    这是 FBX 路径下最容易踩的坑 —— 名字明明对得上却不生效。
    if geom.anim_stacks.len() > 1 {
        out.push(crate::tr_fmt!(
            "Note: %{at} has %{n} animation stacks %{stacks}; both the official tool and mdlc use only the **first** one by default. To use another, write `srcstack \"name\"` (inside `$sequence` / `$animation`).",
            at = at,
            n = geom.anim_stacks.len(),
            stacks = format!("{:?}", geom.anim_stacks)
        ));
    }
    // ④ shape key 已自动注册成 flex（§1.6b）—— 列出来让用户知道帧号怎么来的。
    if !shape_keys.is_empty() {
        let list: Vec<String> = shape_keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                crate::tr_fmt!(
                    "%{name}(frame %{frame})",
                    name = format!("{:?}", k.name),
                    frame = crate::fbx::FbxGeometry::shape_key_frame(i)
                )
            })
            .collect();
        out.push(crate::tr_fmt!(
            "Note: the %{n} shape keys of %{at} were registered as flexes automatically: %{list}. To control which ones are taken, their order, or to ignore some, use `srcshapekey` / `srcshapekeyorder` / `srcshapekeyignore`.",
            n = shape_keys.len(),
            at = at,
            list = list.join(" ")
        ));
    }
    out
}

/// 把 [`fbx_diagnostics`] 的文案逐条打到诊断流上。
fn emit_fbx_diagnostics(at: &str, geom: &SourceGeometry, shape_keys: &[crate::fbx::FbxShapeKey]) {
    for line in fbx_diagnostics(at, geom, shape_keys) {
        log::info!("{line}");
    }
}

/// 读取一个**动画源**的帧数据（`.smd` 或 `.fbx`）。
///
/// 与 [`read_source`] 的区别：FBX 的几何读取（[`read_source`]）给的是**参考
/// 姿态**（每个节点的局部 TRS），而动画读取要走 ufbx 的烘焙路径把
/// `AnimStack` 采样成逐帧局部变换。SMD 动画还须有从零开始的连续时间戳，
/// 因为下游按帧索引取样，不能把稀疏或乱序的时间戳压缩成连续帧。
///
/// 官方把 FBX 动画**固定归一化到 30 fps**（实测 24 / 30 / 60 fps 的源全部
/// 变成同一份 31 帧 —— `docs/fbx-support.md` §1.5），所以这里用
/// [`crate::fbx::DEFAULT_FPS`]；mdlc 的 `srcfps` 可以覆盖它。
///
/// ⚠️ 返回的 [`Smd`] 的 `triangles` 是空的 —— 调用方只取 `frames`。
pub fn read_source_frames(path: &Path, at: &str, opts: &SrcOpts) -> Result<Smd, CompileError> {
    let mut smd = match SourceKind::of(path).map_err(|msg| e(at, msg))? {
        SourceKind::Smd => {
            let smd = read_smd(path, at)?;
            if let Some((index, frame)) = smd
                .frames
                .iter()
                .enumerate()
                .find(|(index, frame)| i32::try_from(*index).ok() != Some(frame.time))
            {
                return Err(e(
                    at,
                    format!(
                        "SMD animation {} has unsupported skeleton time {} at frame {index}; \
                     animation timestamps must be contiguous and start at 0",
                        path.display(),
                        frame.time
                    ),
                ));
            }
            Ok(smd)
        }
        SourceKind::Fbx => crate::fbx::read_frames(
            path,
            at,
            opts.stack.as_deref(),
            opts.fps_or_default(),
            &opts.fbx,
        )
        .map_err(|err| e(at, err.to_string())),
        SourceKind::Gltf => crate::gltf::read_frames(
            path,
            at,
            opts.stack.as_deref(),
            opts.fps_or_default(),
            &opts.fbx,
        )
        .map_err(|err| e(at, err.to_string())),
    }?;
    apply_source_scale(&mut smd, opts, at)?;
    apply_animation_root_transform(&mut smd, opts, at)?;
    Ok(smd)
}

fn apply_animation_root_transform(
    smd: &mut Smd,
    opts: &SrcOpts,
    at: &str,
) -> Result<(), CompileError> {
    let adjust = opts.root_adjust.unwrap_or([0.0; 3]);
    let rotation = opts
        .root_rotation
        .unwrap_or(crate::ani_writer::ROOT_REFERENCE_ANGLES);
    if adjust.iter().chain(rotation.iter()).any(|v| !v.is_finite()) {
        return Err(e(at, "animation origin and rotation must be finite"));
    }
    if adjust == [0.0; 3] && rotation == crate::ani_writer::ROOT_REFERENCE_ANGLES {
        return Ok(());
    }
    // Animation storage and bounds apply the canonical Rz(90) later. Keep that
    // convention while incorporating the per-animation source transform now.
    let correction = crate::bone_math::concat(
        &crate::bone_math::angle_imatrix(crate::ani_writer::ROOT_REFERENCE_ANGLES),
        &crate::bone_math::angle_matrix(rotation),
    );
    for node in smd.nodes.iter().filter(|node| node.parent == -1) {
        let mut previous = SmdPose {
            bone: node.index,
            position: [0.0; 3],
            rotation: [0.0; 3],
        };
        for frame in &mut smd.frames {
            let index = frame.poses.iter().position(|p| p.bone == node.index);
            let raw = index.map(|i| frame.poses[i]).unwrap_or(previous);
            previous = raw;
            let shifted = std::array::from_fn(|axis| raw.position[axis] - adjust[axis]);
            let transformed = crate::bone_math::concat(
                &correction,
                &crate::bone_math::local_transform(shifted, raw.rotation),
            );
            let pose = SmdPose {
                bone: node.index,
                position: [transformed[3], transformed[7], transformed[11]],
                rotation: crate::bone_math::matrix_angles(&transformed),
            };
            match index {
                Some(index) => frame.poses[index] = pose,
                None => frame.poses.push(pose),
            }
        }
    }
    Ok(())
}

pub fn static_prop_source_transform(desc: &ModelDesc) -> crate::bone_math::Matrix3x4 {
    let rotation = desc
        .model
        .source_rotation
        .unwrap_or(crate::ani_writer::ROOT_REFERENCE_ANGLES);
    let mut matrix = if rotation == crate::ani_writer::ROOT_REFERENCE_ANGLES {
        [0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    } else {
        crate::bone_math::angle_matrix(rotation)
    };
    let origin = desc.model.source_origin.unwrap_or([0.0; 3]);
    matrix[3] = origin[0];
    matrix[7] = origin[1];
    matrix[11] = origin[2];
    matrix
}

pub(crate) fn flatten_static_collision(smd: &mut Smd, desc: &ModelDesc) {
    // PHY's art-to-IVP conversion already includes the canonical Rz(90).
    let matrix = crate::bone_math::concat(
        &[0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        &static_prop_source_transform(desc),
    );
    for triangle in &mut smd.triangles {
        for vertex in &mut triangle.vertices {
            vertex.position = transform_source_vector(&matrix, vertex.position, true);
            if let Some(offset) = desc.model.autocenter_offset {
                vertex.position[0] += offset[1];
                vertex.position[1] -= offset[0];
                vertex.position[2] += offset[2];
            }
            vertex.normal = transform_source_vector(&matrix, vertex.normal, false);
            vertex.parent_bone = 0;
            vertex.links = vec![crate::smd::SmdBoneLink {
                bone: 0,
                weight: 1.0,
            }];
        }
    }
    smd.nodes = vec![crate::smd::SmdNode {
        index: 0,
        name: "static_prop".into(),
        parent: -1,
    }];
    smd.frames = vec![crate::smd::SmdFrame {
        time: 0,
        poses: vec![SmdPose {
            bone: 0,
            position: [0.0; 3],
            rotation: [0.0; 3],
        }],
    }];
}

fn transform_source_vector(
    matrix: &crate::bone_math::Matrix3x4,
    vector: [f32; 3],
    point: bool,
) -> [f32; 3] {
    std::array::from_fn(|row| {
        matrix[row * 4] * vector[0]
            + matrix[row * 4 + 1] * vector[1]
            + matrix[row * 4 + 2] * vector[2]
            + if point { matrix[row * 4 + 3] } else { 0.0 }
    })
}

/// 收集**几何源**里的 `.fbx` 文件（规范化后的路径）。
///
/// # 为什么需要这个
///
/// 官方在 FBX 路径上有一个**静默失败**：**同一个 FBX 既作网格源又作动画源
/// 时只产出 1 帧**（`docs/fbx-support.md` §1.11 陷阱 1；单变量定案见
/// `docs/_probe/oracle_fbx_samefile.js` 的 S1–S6 表）。
///
/// ⚠️ **这是 FBX 专属的** —— SMD 同文件完全正常
/// （`docs/_probe/oracle_samefile_smd.js`：8 帧源出 8 帧、5 帧源出 5 帧）。
/// 所以判据**必须按格式分流**，否则会误伤既有工程
/// （`$body x.smd` + `$sequence y x.smd` 是常见写法）。
///
/// ⚠️ 改 QC **没有用**：`$animation` 块、按名引用、重写 `$sequence` 全都被
/// 实测证明仍是 1 帧（`docs/_probe/oracle_samefile_fix.js` 的 N2/N4/N5/N6），
/// 唯一出路是把动画放到**另一个文件**（N3 = 6 帧）。所以这里只能报错，
/// 报错文案里必须把这一点说清楚，否则用户会去试那些无效的改法。
fn fbx_geometry_sources(desc: &ModelDesc, base_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for bp in &desc.bodyparts {
        for m in &bp.models {
            for src in std::iter::once(&m.smd).chain(m.lods.iter().filter_map(|l| l.smd.as_ref())) {
                let p = Path::new(src);
                if SourceKind::of(p) != Ok(SourceKind::Fbx) {
                    continue;
                }
                let joined = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    base_dir.join(p)
                };
                // 同一个文件可能被写成 `box.fbx` / `./box.fbx` / `sub/../box.fbx`
                // ⟹ 能规范化就规范化。
                out.push(std::fs::canonicalize(&joined).unwrap_or(joined));
            }
        }
    }
    out
}

fn inherit_automatic_axes(opts: &mut SrcOpts, desc: &ModelDesc, base_dir: &Path, source: &Path) {
    let matches = desc
        .bodyparts
        .iter()
        .flat_map(|part| &part.models)
        .any(|model| {
            model
                .src_axis
                .as_deref()
                .and_then(crate::fbx::ForcedAxis::parse)
                == Some(crate::fbx::ForcedAxis::Auto)
                && std::iter::once(&model.smd)
                    .chain(model.lods.iter().filter_map(|lod| lod.smd.as_ref()))
                    .filter_map(|path| resolve_smd_path(base_dir, path).ok())
                    .any(|path| same_file(&path, source))
        });
    if matches {
        opts.fbx.axis = Some(crate::fbx::ForcedAxis::Auto);
    }
}

/// 两个路径是不是**同一个文件**（Windows 大小写不敏感，故走 `canonicalize`）。
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// 「同一个 FBX 既作网格源又作动画源」的**差异提示**（见 [`fbx_geometry_sources`]）。
///
/// 返回 `Some(提示文案)` 表示应当告知用户「这里与官方产物不一致」。
///
/// # ✅ 允许，不拒绝
///
/// ⭐ **FBX 本来就是网格与动画合一的容器** —— 要求用户把网格和动画拆成两个
/// 文件，等于让用户为了迁就官方的一个 bug 而改变自己的资产组织方式。
/// 官方在这个组合下**静默地**只产出 1 帧（实测 exit=0、无警告），
/// 而 mdlc 会正常采出全部帧 —— **分歧的方向是「mdlc 对、官方错」**，
/// 所以这里**放行**，只提示。
///
/// # ⚠️ 只在**真的有分歧**时提示
///
/// 官方的退化后果是「静默 1 帧」。如果源**本来就只有 1 帧**
/// （例如 `box.fbx` / `morph.fbx` 这类没有 `AnimStack` 的静态网格），
/// 官方与 mdlc 都出 1 帧，**没有任何分歧** ⟹ 不该提示。
///
/// 所以判据里带 `n_frames`：只有 mdlc 采出 **> 1 帧**（= 官方会丢掉的那些）
/// 才提示。这让判据**自我校准** —— 不依赖「哪些 FBX 有动画」的静态判断，
/// 而是直接看「这次实际采到了几帧」。
///
/// ⚠️ **只对 `.fbx` 生效** —— SMD 同文件完全正常（8 帧源出 8 帧），
/// 没有任何分歧可言。
fn same_fbx_geometry_note(path: &Path, geometry: &[PathBuf], n_frames: usize) -> Option<String> {
    if n_frames <= 1 {
        return None;
    }
    if SourceKind::of(path) != Ok(SourceKind::Fbx) {
        return None;
    }
    if !geometry.iter().any(|g| same_file(g, path)) {
        return None;
    }
    Some(crate::tr_fmt!(
        "%{path} is used as both the mesh source and the animation source. FBX can hold meshes and animation together, and mdlc read %{n_frames} frames as usual — but the official studiomdl **silently** produces only 1 frame in this combination (measured: exit=0, no warning; see docs/fbx-support.md §4.4). This is a **deliberate deviation**: mdlc does not copy that degenerate behavior. ⚠️ If you are comparing against the official output, a frame-count mismatch here is **expected**. To make the two agree, split the animation into a separate FBX file (both the official tool and mdlc handle that normally).",
        path = path.display(),
        n_frames = n_frames
    ))
}

/// 一个 LOD 的原始网格：顶点池 + 三角形。
type LodMesh = (Vec<Vertex>, Vec<[u32; 3]>);

/// 构建一个 model 的多 LOD 数据。
///
/// # 算法（对应 studiomdl 的 `$lod` + `UnifyLODs`）
///
/// 1. 读每个 LOD 的 SMD，各自按材质名划分 mesh（与 LOD 0 同一套规则）；
/// 2. **按材质名对齐**各 LOD 的 mesh —— 同一个材质名在各 LOD 里必须是
///    同一个 mesh，否则 `mstudiomesh_t` 的对应关系会错位（材质贴错面）；
/// 3. 对每个 mesh，用 [`crate::lod::unify_lods`] 把各 LOD 的顶点池
///    精确去重合并成一个统一池，并算出每个顶点的 LOD 归属位掩码。
///
/// # 为什么按材质名而不是按 mesh 序号对齐
///
/// SMD 里 mesh 的划分是「材质名首次出现顺序」。不同 LOD 的导出器很可能
/// 以不同顺序写材质块（甚至漏掉某个材质），按下标对齐会静默贴错材质。
/// 按名字对齐 + 显式报错缺失的材质，是唯一能保证正确的方式。
fn build_model_lods(
    m: &BodyModel,
    lod0_meshes: &[Mesh],
    lod0_smd: &Smd,
    desc: &ModelDesc,
    base_dir: &Path,
    at: &str,
    opts: &SrcOpts,
) -> Result<ModelLods, Vec<CompileError>> {
    let mut errs: Vec<CompileError> = Vec::new();
    let num_lods = m.lods.len() + 1;

    // LOD 0 的材质名 → mesh 下标（`lod0_meshes[i].material`）。
    let lod0_material_of: Vec<usize> = lod0_meshes.iter().map(|k| k.material).collect();
    let _ = lod0_smd;

    // 每个 mesh 收集各 LOD 的 (顶点池, 三角形)。先放 LOD 0。
    let mut per_mesh: Vec<Vec<LodMesh>> = vec![Vec::with_capacity(num_lods); lod0_meshes.len()];
    for (ki, mesh) in lod0_meshes.iter().enumerate() {
        per_mesh[ki].push((mesh.vertices.clone(), mesh.triangles.clone()));
    }

    // ---- 每档的骨骼映射（与 mesh 无关，先算一次）----
    //
    // 顺序严格照 `SimplifyModel()`（`simplify.cpp:7246-7256`）：
    // `ConvertBoneTreeCollapsesToReplaceBones()` → `FixupReplacedBones()`。
    let bone_index = desc.bone_index();
    let n_bones = desc.bones.len();
    let parents: Vec<i32> = desc
        .bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => bone_index.get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect();
    let mut lod_bone_maps: Vec<Vec<usize>> = Vec::with_capacity(num_lods);
    lod_bone_maps.push(Vec::new()); // LOD 0 恒等（官方 `BuildBoneLODMapping(map, 0)`）
    for lod in &m.lods {
        let roots: Vec<usize> = lod
            .bone_tree_collapse
            .iter()
            .filter_map(|n| bone_index.get(n.as_str()).copied())
            .collect();
        let mut reps = crate::lod::expand_bone_tree_collapses(&roots, &parents);
        for pair in &lod.replace_bone {
            if let (Some(&s), Some(&d)) = (
                bone_index.get(pair[0].as_str()),
                bone_index.get(pair[1].as_str()),
            ) {
                reps.push((s, d));
            }
        }
        crate::lod::fixup_replaced_bones(&mut reps);
        lod_bone_maps.push(crate::lod::build_bone_lod_mapping(n_bones, &reps));
    }

    // 逐 LOD 读 SMD 并对齐。
    hotpath::measure_block!("  LOD: 读 SMD + build_meshes", {
        for (li, lod) in m.lods.iter().enumerate() {
            let lod_no = li + 1;
            let lpath = format!("{at}.lods[{li}]");

            let material_key = |name: &str| {
                name.replace('\\', "/")
                    .to_ascii_lowercase()
                    .trim_end_matches(".vmt")
                    .to_owned()
            };
            let removed_mesh = |material: usize| {
                lod.removed
                    || lod.removed_materials.iter().any(|name| {
                        desc.materials
                            .textures
                            .get(material)
                            .is_some_and(|texture| {
                                let target = material_key(name);
                                material_key(&texture.name) == target
                                    || desc.materials.search_paths.iter().any(|directory| {
                                        material_key(&format!(
                                            "{}/{}",
                                            directory.trim_end_matches(['/', '\\']),
                                            texture.name
                                        )) == target
                                    })
                            })
                    })
            };
            // 没写 `smd` ⟹ 复用 LOD 0 的网格，只应用骨骼选项。
            // 官方 `GetLODSources`：`if (!pSource && !found) pSource = pSrcModel->source;`
            let Some(lod_smd) = lod.smd.as_deref() else {
                for (ki, mesh) in lod0_meshes.iter().enumerate() {
                    per_mesh[ki].push(if removed_mesh(mesh.material) {
                        (Vec::new(), Vec::new())
                    } else {
                        (mesh.vertices.clone(), mesh.triangles.clone())
                    });
                }
                continue;
            };

            let smd_path = match resolve_smd_path(base_dir, lod_smd) {
                Ok(p) => p,
                Err(msg) => {
                    errs.push(e(&lpath, msg));
                    continue;
                }
            };
            let smd = match read_source(&smd_path, &lpath, opts).and_then(|mut s| {
                crate::bone_rename::source(&mut s, &desc.model.bone_renames)
                    .map_err(|message| e(&lpath, message))?;
                crate::bone_hierarchy::source(&mut s, &desc.model.bone_hierarchy)
                    .map_err(|message| e(&lpath, message))?;
                Ok(s)
            }) {
                Ok(s) => s,
                Err(err) => {
                    errs.push(err);
                    continue;
                }
            };
            // ⚠️ **不要**因为「LOD SMD 里有 `[[bones]]` 没有的骨骼」而报错。
            //
            // 官方 `$lod` 的源是 `Load_Source(..., isActiveModel = false)`
            // （`studiomdl.cpp:5434-5436`，默认值见 `studiomdl.h:1127`），
            // 所以它的顶点权重**不**给骨骼打 `boneref` —— LOD SMD 里那些
            // 「零顶点引用、又没被 `$definebone`/`$attachment`/`$ikchain`/
            // `$mouth`/`$bonemerge`/眼球 提到」的骨骼根本不在表里。
            // 官方靠 `MapSourcesToGlobalBonetable()`（`simplify.cpp:4148-4217`）
            // 把这些骨骼**沿父链上溯**、实在找不到就静默重映射到根骨骼 0
            // （`:4180` 的 `k = 0;`），从不报错。
            //
            // mdlc 侧的对应实现在 `smd_vertex_to_ir`（同一条父链上溯），
            // 所以这里直接放行。
            let meshes = if smd.triangles.is_empty() {
                Vec::new()
            } else {
                match build_meshes(
                    &smd,
                    desc,
                    &smd_path,
                    &lpath,
                    m.flip_triangles ^ lod.reverse,
                ) {
                    Ok((meshes, _)) => meshes,
                    Err(err) => {
                        errs.push(err);
                        continue;
                    }
                }
            };
            let mut by_material: HashMap<usize, &Mesh> =
                meshes.iter().map(|k| (k.material, k)).collect();
            for (ki, mat) in lod0_material_of.iter().enumerate() {
                let geometry = by_material
                    .remove(mat)
                    .filter(|_| !removed_mesh(*mat))
                    .map(|mesh| (mesh.vertices.clone(), mesh.triangles.clone()))
                    .unwrap_or_default();
                // Removed LOD meshes keep their MDL mesh slots with no triangles.
                per_mesh[ki].push(geometry);
            }
            if !by_material.is_empty() {
                let extra: Vec<usize> = by_material.keys().copied().collect();
                errs.push(e(
                    &lpath,
                    crate::tr_fmt!(
                        "LOD %{lod} has material indices that LOD 0 does not have: %{extra}",
                        lod = lod_no,
                        extra = format!("{extra:?}")
                    ),
                ));
            }
        }
        if !errs.is_empty() {
            return Err(errs);
        }
    });

    // 每个 mesh 统一去重（**走骨骼重映射感知的字典**）。
    //
    // `per_mesh[ki]` 是「LOD 0、LOD 1、…」的顶点池，与 `lod_bone_maps` 一一对应。
    let mut bone_lod_usage: Vec<u32> = vec![0; n_bones];
    let mesh_lods: Vec<crate::lod::MeshLods> =
        hotpath::measure_block!("  LOD: unify_lods_remapped(全部 mesh)", {
            per_mesh
                .iter()
                .map(|lods| {
                    let srcs: Vec<crate::lod::LodSource> = lods
                        .iter()
                        .enumerate()
                        .map(|(n, (verts, tris))| crate::lod::LodSource {
                            vertices: verts.clone(),
                            triangles: tris.clone(),
                            bone_map: lod_bone_maps.get(n).cloned().unwrap_or_default(),
                        })
                        .collect();
                    crate::lod::unify_lods_remapped(&srcs, &mut bone_lod_usage)
                })
                .collect()
        });

    // switchPoint：LOD 0 恒 0；其余用显式值，否则按 20/40/80… 推算。
    let mut switch_points = Vec::with_capacity(num_lods);
    switch_points.push(0.0f32);
    for (li, lod) in m.lods.iter().enumerate() {
        let auto = 20.0f32 * (1u32 << li) as f32;
        switch_points.push(lod.switch_point.unwrap_or(auto));
    }

    // `nofacial`：**逐档**（官方 `scriptLOD.GetFacialAnimationEnabled()`）。
    // LOD 0 恒 `false` —— 官方那个隐式插入的空档用的是 `LodScriptData_t` 的
    // 缺省 `m_bFacialAnimation = true`（`studiomdl.h:1313`）。
    let no_facial: Vec<bool> = std::iter::once(false)
        .chain(m.lods.iter().map(|l| l.no_facial))
        .collect();

    Ok(ModelLods {
        meshes: mesh_lods,
        num_lods,
        switch_points,
        bone_lod_usage,
        no_facial,
    })
}

/// 把 SMD 的三角形按材质名划分成 mesh。
///
/// 材质名 → 材质表下标的映射：先在描述文件的 `[materials].textures` 里
/// 按名字找（忽略分隔符与 `$cdmaterials` 前缀差异）；找不到则**报错** ——
/// 静默新建一个材质会让用户在游戏里看到「材质丢失」而不知道原因。
/// 「SMD 骨骼下标 → 描述文件骨骼下标」的查找表，**每个 SMD 只建一次**。
///
/// # 为什么要把它提出顶点循环
///
/// [`smd_vertex_to_ir`] 原先在**每个顶点**上重建这两张表：
/// `node_names`（`Vec<&str>`）与 `desc.bone_index()`（`HashMap<&str, usize>`）。
/// 后者实测 **71 ns/次**（68 根骨骼），而且**每次都新分配一个 HashMap**。
///
/// 100 万三角形（300 万个顶点）时：
///
/// | 项 | 成本 |
/// |---|---|
/// | 时间 | ≈ 214 ms（占 `compile()` 的 5%） |
/// | 分配 | ≈ 300 万次 |
///
/// 表的内容在一个 SMD 内是**恒定**的（骨骼表来自描述文件，`nodes` 段来自
/// SMD 本身），所以没有任何理由逐顶点重建。
struct VertexBoneMap<'s, 'd> {
    /// SMD `nodes` 段的骨骼名（按 SMD 自己的下标）。
    node_names: Vec<&'s str>,
    /// SMD `nodes` 段每根骨骼的**父下标**（`-1` 表示无父）。
    ///
    /// 用于官方 `MapSourcesToGlobalBonetable()`（`simplify.cpp:4148-4217`）
    /// 的**父链上溯**：SMD 里的骨骼若不在描述文件的骨骼表里，就沿父链找到
    /// 第一根在表里的祖先；实在找不到则官方静默重映射到**根骨骼 0**
    /// （`:4180` 的 `k = 0;`）。
    ///
    /// ⭐ 这条路径是必需的：官方骨骼表只收 `$definebone` 与 `boneref != 0`
    /// 的骨骼，动画 SMD 里「零顶点引用、又没被保命判据提到」的骨骼不在表里
    /// （用户工程 `linnea_replaces_zoey` 的 `TeenAngst.smd`/`ragdoll.smd`
    /// 各有 12 根这样的骨骼）。旧实现直接报错，那是「官方能编过、mdlc 编不过」
    /// 的假阳性。
    smd_parents: Vec<i32>,
    /// 描述文件骨骼名 → 下标。与 [`ModelDesc::bone_index`] 一致（**大小写敏感**）。
    desc_index: HashMap<&'d str, usize>,
    /// `nodes` 段的条目数（只用于报错文案）。
    node_count: usize,
    /// 描述文件的骨骼总数。
    bone_count: usize,
}

impl<'s, 'd> VertexBoneMap<'s, 'd> {
    fn new(smd: &'s Smd, desc: &'d ModelDesc) -> Self {
        VertexBoneMap {
            node_names: smd.nodes.iter().map(|n| n.name.as_str()).collect(),
            smd_parents: smd.nodes.iter().map(|n| n.parent).collect(),
            desc_index: desc.bone_index(),
            node_count: smd.nodes.len(),
            bone_count: desc.bones.len(),
        }
    }

    /// 把 SMD 的骨骼下标映射到描述文件的骨骼下标。
    ///
    /// 复刻官方 `MapSourcesToGlobalBonetable()`（`simplify.cpp:4148-4217`）：
    /// 先按名字直接查；查不到就**沿父链上溯**（`:4167-4171`）；整条链都不在
    /// 表里则返回**根骨骼 0**（`:4180` 的 `k = 0;`，官方那条「illegal parent
    /// bone replacement」诊断被 `#if 0` 关掉了，见 `:4186-4210`）。
    ///
    /// 返回 `None` 只在描述文件的骨骼表**为空**时发生（没有 0 号骨骼可退）。
    fn map_bone(&self, node: usize) -> Option<usize> {
        // 先直接查（大小写敏感，与 `desc.bone_index()` 一致）。
        let mut k = node as i32;
        let mut guard = 0usize;
        while k >= 0 {
            let ki = k as usize;
            if let Some(name) = self.node_names.get(ki)
                && let Some(&di) = self.desc_index.get(*name)
            {
                return Some(di);
            }
            // 环保护：SMD 的 `nodes` 段理论上无环，但坏文件不该让编译器挂死。
            guard += 1;
            if guard > self.node_names.len() {
                break;
            }
            k = self.smd_parents.get(ki).copied().unwrap_or(-1);
        }
        // 整条父链都不在表里 ⟹ 官方重映射到根骨骼 0。
        if self.bone_count > 0 { Some(0) } else { None }
    }
}

/// 由 SMD 的三角形构建逐材质的 mesh。
///
/// # `flip_triangles` —— **Source 的正面是 CW**
///
/// 官方 `v1support.cpp:192-196`（`Grab_UpdateFace`）：
/// ```c
/// if (flip_triangles) { j = pFace->b;  pFace->b = pFace->c;  pFace->c = j; }
/// ```
/// 即交换第 2、3 个顶点。`flip_triangles` **默认为 1**（`studiomdl.cpp:6893`），
/// 只有 QC 写了 `reverse` 才置 0（`:935`）。
///
/// ⚠️ **漏掉它 ⟹ 每个三角形的绕序都反向 ⟹ 面法向整体朝内。**
/// 逐顶点法线（VVD 的 `normal`）**仍然完全正确**，所以
/// `cmp_vtx_vvd_full.js` 那类「按属性比顶点」的探针**测不出来** ——
/// 它把三角形当**无序**三元组比（`vtxlib.js` 的 `triSet`），
/// 绕序反了照样全绿。
///
/// 判据（`docs/_probe/probe_winding_order.js`，6 个 `vm_test_group` 模型）：
/// 官方与 SMD 原始顺序「同序 0 / 逆序 12996」，mdlc 修前「同序 6905 / 逆序 0」。
/// 把 SMD 三角形流焊接成 IR 网格。
///
/// # 返回
///
/// `(meshes, src_index)` —— `src_index` 与 `meshes` 一一对应，内层与
/// `Mesh::vertices` 一一对应，给出每个焊接顶点来自**源文件的哪个控制点号**
/// （[`crate::smd::SmdVertex::src_index`]）。
///
/// ⚠️ SMD 源没有「控制点」概念（[`crate::smd::NO_SOURCE_INDEX`]），此时
/// `src_index` 返回**空 `Vec`**，而不是「一堆 `u32::MAX`」—— 让「这份表有没有
/// 用」一眼可判（消费方 `resolve_vta_flexes` 就靠这个分流）。
fn build_meshes(
    smd: &Smd,
    desc: &ModelDesc,
    smd_path: &Path,
    at: &str,
    flip_triangles: bool,
) -> Result<(Vec<Mesh>, Vec<Vec<u32>>), CompileError> {
    let names = smd.materials_in_order();
    if names.is_empty() {
        return Err(e(
            at,
            crate::tr_fmt!(
                "there is not a single triangle in %{path}",
                path = smd_path.display()
            ),
        ));
    }

    let cd: Vec<String> = desc.materials.search_paths.clone();
    let mut lookup: HashMap<String, usize> = HashMap::new();
    for (i, t) in desc.materials.textures.iter().enumerate() {
        // 匹配键：统一分隔符 + basename 兜底。
        //
        // 为什么需要 basename：SMD/DMX 源里的材质名可能是**裸名**，
        // 而 TOML 里写的是**带路径**的全名（官方产物就是这么写的）。
        // 官方的 `$cdmaterials` 是 `models\moranyue\...\`，而材质名是
        // `moranyue/.../frown` —— **前缀对不上 cd 路径**，
        // 所以 `normalize_texture_name` 剥不掉它。这正是引擎需要
        // 「根目录搜索哨兵」（空 cdtexture）的原因。
        let key = crate::mdl_writer::texture_match_key(&t.name, &cd);
        // 先插入者优先（`entry` 保留首个），保证确定性。
        lookup.entry(key).or_insert(i);
    }

    // 材质名 → mesh 下标。
    let mut material_of: HashMap<&str, usize> = HashMap::new();
    for n in &names {
        let key = crate::mdl_writer::texture_match_key(n, &cd);
        let Some(&mi) = lookup.get(&key) else {
            return Err(e(
                at,
                crate::tr_fmt!(
                    "the material name %{name} in the SMD was not found in [materials].textures (the lookup key is %{key})",
                    name = format!("{n:?}"),
                    key = format!("{key:?}")
                ),
            ));
        };
        material_of.insert(n.as_str(), mi);
    }

    // 逐 mesh 累积（保持材质首次出现顺序）。
    let mut order: Vec<usize> = Vec::new();
    let mut per_mesh: HashMap<usize, Vec<Vertex>> = HashMap::new();
    let mut tris_per_mesh: HashMap<usize, Vec<[u32; 3]>> = HashMap::new();
    // 每个 mesh 自己的顶点去重表 + 「位置+UV」次级索引（法线容差焊接用）。
    let mut dedup: HashMap<usize, HashMap<VertexKey, u32>> = HashMap::new();
    let mut secondary: HashMap<usize, HashMap<PosUvKey, Vec<u32>>> = HashMap::new();
    // 「焊接顶点 → 源控制点号」。SMD 源没有这个概念，只在见到第一个
    // 非 `NO_SOURCE_INDEX` 的顶点时才建表（见函数文档）。
    let mut src_per_mesh: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut has_src_index = false;

    // 骨骼查找表**只建一次**（原先在 `smd_vertex_to_ir` 里逐顶点重建）。
    let bone_map = VertexBoneMap::new(smd, desc);

    for t in &smd.triangles {
        let mi = material_of[t.material.as_str()];
        if let std::collections::hash_map::Entry::Vacant(e) = per_mesh.entry(mi) {
            order.push(mi);
            e.insert(Vec::new());
            tris_per_mesh.insert(mi, Vec::new());
            dedup.insert(mi, HashMap::new());
            secondary.insert(mi, HashMap::new());
            src_per_mesh.insert(mi, Vec::new());
        }
        let pool = per_mesh.get_mut(&mi).unwrap();
        let table = dedup.get_mut(&mi).unwrap();
        let sec = secondary.get_mut(&mi).unwrap();
        let srcs = src_per_mesh.get_mut(&mi).unwrap();
        let mut corner = [0u32; 3];
        for (c, sv) in t.vertices.iter().enumerate() {
            let v = smd_vertex_to_ir(sv, desc, &bone_map, smd_path, at)?;
            let idx = weld_or_push(pool, table, sec, &v);
            // ⚠️ 只在**新建**顶点时记录源号：焊接会把多个源角并成一个焊接
            // 顶点，此时它们的 `src_index` 必然指向同一个控制点（焊接的判据
            // 里位置/法线/UV 全部相同，而 FBX 的同一控制点的多个角本来就会
            // 焊在一起），所以「先到先得」与「任意一个」等价。
            if idx as usize == srcs.len() {
                if sv.src_index != crate::smd::NO_SOURCE_INDEX {
                    has_src_index = true;
                }
                srcs.push(sv.src_index);
            }
            corner[c] = idx;
        }
        // ⚠️ **绕序翻转**（`v1support.cpp:192-196`）：交换第 2、3 个角。
        // 见 [`build_meshes`] 的文档 —— 漏掉它会让整个模型的面法向朝内。
        if flip_triangles {
            corner.swap(1, 2);
        }
        // 退化三角形（去重后有两个角相同）直接跳过 —— 它们在 VVD/VTX 里
        // 是零面积面，会让 strip 生成器产出无效数据。
        if corner[0] == corner[1] || corner[1] == corner[2] || corner[0] == corner[2] {
            continue;
        }
        tris_per_mesh.get_mut(&mi).unwrap().push(corner);
    }

    let mut meshes = Vec::with_capacity(order.len());
    let mut src_index: Vec<Vec<u32>> = Vec::with_capacity(order.len());
    let mut dropped = 0usize;
    for mi in order {
        let vertices = per_mesh.remove(&mi).unwrap();
        let triangles = tris_per_mesh.remove(&mi).unwrap();
        if vertices.is_empty() || triangles.is_empty() {
            dropped += 1;
            continue;
        }
        if has_src_index {
            src_index.push(src_per_mesh.remove(&mi).unwrap());
        }
        meshes.push(Mesh {
            material: mi,
            vertices,
            triangles,
            eyeball_tag: None,
        });
    }
    // 全部三角形都退化掉时必须报错 —— 否则会产出一个「有材质但没有几何」
    // 的模型，在游戏里表现为看不见任何东西，且没有任何提示。
    if meshes.is_empty() {
        return Err(e(
            at,
            crate::tr_fmt!(
                "%{path} has no usable triangle left (%{dropped} materials had every triangle degenerate)",
                path = smd_path.display(),
                dropped = dropped
            ),
        ));
    }
    Ok((meshes, src_index))
}

/// 把一个 SMD 三角形顶点转成 IR 顶点，并做边界校验。
///
/// `bone_map` 由调用方**每个 SMD 建一次**并复用 —— 见 [`VertexBoneMap`]。
/// 早期版本在这里逐顶点重建骨骼表，是 `compile()` 分配量的主要来源之一。
fn smd_vertex_to_ir(
    sv: &crate::smd::SmdVertex,
    desc: &ModelDesc,
    bone_map: &VertexBoneMap<'_, '_>,
    smd_path: &Path,
    at: &str,
) -> Result<Vertex, CompileError> {
    let bone_count = bone_map.bone_count;

    // SMD 的绑定用的是**SMD 自己的**骨骼下标，需要映射到描述文件的骨骼表。
    // 映射走 [`VertexBoneMap::map_bone`]（复刻官方
    // `MapSourcesToGlobalBonetable()`：按名查 → 沿父链上溯 → 退根骨骼 0）。

    // 权重按从大到小排序，并只保留前 MAX_BONES_PER_VERT 组（引擎上限）。
    //
    // # 为什么不克隆 `sv.links`
    //
    // `SmdVertex.links` 是 `Vec`，`clone()` 会在**每个顶点**上分配一次堆内存
    // （100 万三角形约 300 万次）。这里改用栈上的定长数组。
    //
    // ⚠️ **必须排全部元素**：若先截断到内联容量再排序，超过容量的顶点
    // 就可能漏掉本该进前 3 的绑定 —— 那是**语义改变**，不是优化。
    // 因此超出内联容量时回退到原来的 `Vec` 路径，保证任何输入都逐位等价。
    const INLINE: usize = 8;
    let mut inline_buf: [crate::smd::SmdBoneLink; INLINE] = [crate::smd::SmdBoneLink {
        bone: 0,
        weight: 0.0,
    }; INLINE];
    let mut fallback: Vec<crate::smd::SmdBoneLink>;
    let links: &mut [crate::smd::SmdBoneLink] = if sv.links.len() <= INLINE {
        let n = sv.links.len();
        inline_buf[..n].copy_from_slice(&sv.links[..n]);
        &mut inline_buf[..n]
    } else {
        fallback = sv.links.clone();
        &mut fallback
    };
    links.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut bones: Vec<[f32; 2]> = Vec::with_capacity(links.len().min(MAX_BONES_PER_VERT));
    for l in links.iter().take(MAX_BONES_PER_VERT) {
        if l.weight <= 0.0 {
            continue;
        }
        let node_ix = usize::try_from(l.bone).unwrap_or(usize::MAX);
        if node_ix >= bone_map.node_count {
            return Err(e(
                at,
                crate::tr_fmt!(
                    "a vertex of %{path} references the bone index %{bone}, but the nodes section only has %{nodes} entries",
                    path = smd_path.display(),
                    bone = l.bone,
                    nodes = bone_map.node_count
                ),
            ));
        }
        // 复刻官方 `MapSourcesToGlobalBonetable()`（`simplify.cpp:4148-4217`）：
        // 名字查不到就沿父链上溯，整条链都不在表里则重映射到根骨骼 0。
        // 旧实现直接报「不在描述的 [[bones]] 里」—— 那是假阳性：官方骨骼表
        // 只收 `$definebone` 与 `boneref != 0` 的骨骼，动画 SMD 里那些
        // 「零顶点引用、又没被保命判据提到」的骨骼本就不在表里。
        let Some(di) = bone_map.map_bone(node_ix) else {
            return Err(e(
                at,
                crate::tr_fmt!(
                    "the bone index %{bone} of %{path} cannot be mapped to [[bones]] (the bone table is empty)",
                    bone = node_ix,
                    path = smd_path.display()
                ),
            ));
        };
        if di >= bone_count {
            return Err(e(
                at,
                crate::tr_fmt!(
                    "the bone index %{bone} is out of range (there are %{count} bones)",
                    bone = di,
                    count = bone_count
                ),
            ));
        }
        bones.push([di as f32, l.weight]);
    }
    if bones.is_empty() {
        return Err(e(
            at,
            crate::tr_fmt!(
                "%{path} has a vertex whose skin weights are all 0, or which has no binding at all",
                path = smd_path.display()
            ),
        ));
    }
    // 归一化权重：SMD 里的权重常有浮点残差（0.999999），
    // 不归一化会让引擎的蒙皮出现微小但可见的偏差。
    let sum: f32 = bones.iter().map(|b| b[1]).sum();
    if sum > 0.0 && (sum - 1.0).abs() > 1e-6 {
        for b in &mut bones {
            b[1] /= sum;
        }
    }

    // `$staticprop`：几何整体旋转 `Rz(90°)`，且**所有**权重归到骨骼 0。
    //
    // 这一步必须发生在**顶点构造时**（而不是编译末尾的后处理），因为
    // `unify_lods` 的去重键**包含骨骼绑定**：先把权重塌缩再统一，
    // 才能让「原本只差绑定」的顶点正确合并。studiomdl 的顺序也是如此 ——
    // `MakeStaticProp()` 在 `RemapBones()` 里，早于 `UnifyLODs()`。
    //
    // 权重直接写成「骨骼 0、权重 1.0」：原权重之和恒为 1，全部归到骨骼 0
    // 之后总权重仍是 1，语义与「逐项置 0」等价，且省掉 studiomdl 那步
    // `MergeLikeBoneIndicesWithinVert` 合并。实测官方产物正是
    // `bone=[0,0,0] bone_count=1 weight=[1.0, 0, 0]`。
    if desc.model.static_prop {
        return Ok(Vertex {
            pos: transform_source_vector(&static_prop_source_transform(desc), sv.position, true),
            normal: transform_source_vector(&static_prop_source_transform(desc), sv.normal, false),
            uv: sv.uv,
            bones: vec![[0.0, 1.0]],
        });
    }

    Ok(Vertex {
        pos: sv.position,
        normal: sv.normal,
        uv: sv.uv,
        bones,
    })
}

/// blend 网格的尺寸 `(groupsize[0], groupsize[1])`。
///
/// # 推断规则（`simplify.cpp:2994-3024`）
///
/// * `blend_width` 给了 ⟹ `groupsize[0] = blend_width`、
///   `groupsize[1] = 格数 / blend_width`；除不尽直接报错。
/// * 没给 ⟹ 格数 < 4 时 `groupsize = (格数, 1)`；
///   否则要求**完全平方数**，开方成方阵，否则报
///   `non-square (%d) number of blends without "blendwidth" set`。
fn blend_grid_size(
    s: &crate::model::Sequence,
    at: &str,
    errs: &mut Vec<CompileError>,
) -> Option<(i32, i32)> {
    let n = s.blends.len() as i32;
    let e = |msg: String| CompileError::Message {
        at: format!("{at}.blends"),
        message: msg,
    };
    match s.blend_width {
        Some(w) if w > 0 => {
            if n % w != 0 {
                errs.push(e(crate::tr_fmt!(
                    "the blend cell count %{cells} is not divisible by blend_width %{width} (groupsize[0]*groupsize[1] must equal the cell count)",
                    cells = n,
                    width = w
                )));
                return None;
            }
            Some((w, n / w))
        }
        Some(w) => {
            errs.push(e(crate::tr_fmt!(
                "blend_width must be positive, but is %{width}",
                width = w
            )));
            None
        }
        None => {
            if n < 4 {
                Some((n, 1))
            } else {
                let r = (n as f64).sqrt() as i32;
                if r * r == n {
                    Some((r, r))
                } else {
                    errs.push(e(crate::tr_fmt!(
                        "the blend cell count %{cells} is not a perfect square and no blend_width was given -- \
                         the official code raises non-square number of blends (simplify.cpp:3011)",
                        cells = n
                    )));
                    None
                }
            }
        }
    }
}

const MAX_ANIMATION_POOL_POSES: usize = 8_000_000;

fn reserve_animation_poses(
    retained: &mut usize,
    frames: usize,
    bones: usize,
) -> Result<(), String> {
    *retained = frames
        .checked_mul(bones.max(1))
        .and_then(|poses| retained.checked_add(poses))
        .filter(|poses| *poses <= MAX_ANIMATION_POOL_POSES)
        .ok_or("Animation pool exceeds the 8,000,000 pose limit")?;
    Ok(())
}

fn animation_pool_poses(
    animations: &[crate::model::CompiledAnimation],
    sequences: &[crate::model::CompiledSequence],
    bones: usize,
) -> Result<usize, String> {
    let mut poses = 0;
    for (frames, original) in animations
        .iter()
        .map(|animation| (&animation.frames, &animation.pre_subtract_frames))
        .chain(
            sequences
                .iter()
                .map(|sequence| (&sequence.frames, &sequence.pre_subtract_frames)),
        )
    {
        reserve_animation_poses(&mut poses, frames.len(), bones)?;
        if let Some(original) = original {
            reserve_animation_poses(&mut poses, original.len(), bones)?;
        }
    }
    Ok(poses)
}

/// 读一个 SMD 并把它转成「按描述骨骼顺序排列」的逐帧姿态。
///
/// 抽出来是为了让**单动画序列**与 **blend 的每一格**共用同一条路径 ——
/// 两处各写一份迟早会分叉。
///
/// 失败时把错误推进 `errs` 并返回 `None`。
/// 读一个 SMD 的**全部帧**，按描述文件的骨骼表对齐成稠密行。
///
/// # 骨骼表 = `$definebone` ∪ SMD 的 `nodes`
///
/// 官方 `BuildGlobalBonetable`（`simplify.cpp:3616-3654`）**先**把
/// `g_importbone`（`$definebone` 收集来的）逐条插进骨骼表，**再**并入
/// 各 SMD 用到的骨骼（同名的靠 `findGlobalBone` 去重）。
///
/// 所以 `$definebone` 声明了、而 SMD 里**没有**的骨骼**依然存在**，
/// 参考姿态取 `$definebone` 给的 `rawLocal`。
///
/// 实测（`parity/myprop.qc` → 官方 `myprop.mdl`）：
///
/// ```text
/// $definebone "root" ""    0 0 0 0 0 0     SMD nodes 有 root
/// $definebone "tip" "root" 0 0 8 0 0 0     SMD nodes **没有** tip
///
/// 官方产物：numbones = 2
///   BONE[0] root  pos = [0, 0, 0]  flags = 0x40700
///   BONE[1] tip   pos = [0, 0, 8]  flags = 0x200   ← 来自 $definebone
/// ```
///
/// > 早先 mdlc 要求「每帧都必须有全部骨骼的姿态」，于是这种 QC
/// > 直接报「缺少部分骨骼的姿态（1 / 2 根有数据）」——
/// > 而官方是正常编过的。`verify_parity.ps1` 因此长期红灯。
///
/// # 缺失骨骼的姿态从哪来
///
/// 优先取 `$definebone` 声明的 `position`/`rotation`（TOML 的
/// `[[bones]]`）；**两者都没有时退回零姿态，不报错**。
///
/// 官方在这一点上**从不报错**。`TranslateAnimations`
/// （`simplify.cpp:1498-1529`）逐骨骼查 `psource->boneGlobalToLocal[k]`：
///
/// ```cpp
/// int q = psource->boneGlobalToLocal[k];
/// if (q == -1) {
///     // unknown bone, copy over defaults
///     if (g_bonetable[k].parent >= 0) {
///         AngleMatrix( g_bonetable[k].rot, g_bonetable[k].pos, bonematrix );
///         ConcatTransforms( destBoneToWorld[g_bonetable[k].parent], bonematrix, destBoneToWorld[k] );
///     } else { AngleMatrix( g_bonetable[k].rot, g_bonetable[k].pos, destBoneToWorld[k] ); }
/// } else { ConcatTransforms( srcBoneToWorld[q], g_bonetable[k].srcRealign, destBoneToWorld[k] ); }
/// ```
///
/// 即「该 source 没有这根骨骼」时，**拿骨骼表的参考姿态当默认值**继续，
/// 而不是拒绝编译。`Grab_Animation`（`studiomdl.cpp:1065-1135`）同理：
/// `rawanim[t]` 由 `kalloc`（= `calloc`）零填充，只检查**帧**是否存在，
/// 从不检查「每根骨骼都有数据」。
///
/// 实测（`docs/_probe/oracle_definebone_partial_smd.js`，2026-09）：
///
/// ```text
/// $unlockdefinebones
/// $definebone "b0" ""    0 0 0  0 0 0
/// $definebone "b1" "b0"  0 0 20 0 0 0     ← 与 SMD 的 z=10 冲突
/// $body    body "part.smd"                 ← nodes 有 b1
/// $sequence idle "nopart.smd"              ← nodes **没有** b1
///
/// 官方：✅ numbones=2  b1.pos=[0,0,10]      ← SMD 赢，且对缺失零抱怨
/// 旧 mdlc：❌ exit=1「nopart.smd 里没有骨骼 "b1"…无法确定参考姿态」
/// ```
///
/// 用户报的 `linnea_replaces_zoey`（`anims/foot_fix.smd` 缺
/// `ValveBiped.forward`）正是同一形态：官方跑到 `SMD MODEL` 阶段之后才因
/// 无关的 `Too many materials used, max 32` 失败，对该骨骼缺失**零抱怨**
/// （`docs/_probe/oracle_linnea_real.js`）。
fn load_smd_frames(
    smd_path: &std::path::Path,
    desc: &ModelDesc,
    desc_index: &std::collections::HashMap<&str, usize>,
    at: &str,
    opts: &SrcOpts,
    errs: &mut Vec<CompileError>,
) -> Option<(Vec<Vec<crate::smd::SmdPose>>, Smd)> {
    let e = |msg: String| CompileError::Message {
        at: at.to_string(),
        message: msg,
    };
    let bone_count = desc.bones.len();
    let smd = match read_source_frames(smd_path, at, opts).and_then(|mut s| {
        crate::bone_rename::source(&mut s, &desc.model.bone_renames).map_err(e)?;
        crate::bone_hierarchy::source(&mut s, &desc.model.bone_hierarchy).map_err(e)?;
        Ok(s)
    }) {
        Ok(s) => s,
        Err(err) => {
            errs.push(err);
            return None;
        }
    };
    if smd.frames.is_empty() {
        errs.push(e(crate::tr_fmt!(
            "the skeleton section of %{path} has no frame at all",
            path = smd_path.display()
        )));
        return None;
    }
    if smd
        .frames
        .len()
        .checked_mul(bone_count.max(smd.nodes.len()).max(1))
        .is_none_or(|poses| poses > 2_000_000)
    {
        errs.push(e("Animation source exceeds the 2,000,000 pose limit".into()));
        return None;
    }
    // ⚠️ **不要**因为「SMD 里有 `[[bones]]` 没有的骨骼」而报错。
    //
    // 官方对这种情况**从不报错**：骨骼表（`BuildGlobalBonetable`，
    // `simplify.cpp:3616-3695`）只收 `$definebone` 与 `boneref != 0` 的骨骼，
    // 动画 SMD 里那些「零顶点引用、又没被 `$definebone`/`$attachment`/
    // `$ikchain`/`$mouth`/`$bonemerge`/眼球 提到」的骨骼**根本不在表里**，
    // 而 `TranslateAnimations` 只是按 `boneLocalToGlobal[]` 映射、映射不到的
    // 就跳过（`MapSourcesToGlobalBonetable`，`simplify.cpp:4148-4217`）。
    //
    // ⭐ 实测：用户工程 `linnea_replaces_zoey` 的 `TeenAngst.smd` / `ragdoll.smd`
    // 各有 12 根这样的骨骼（7 根 `jiggy_hair_*`/`jiggle_holster` + 5 根
    // `attachment_bandage_*`/`attachment_arm*_T`），官方编出 122 根骨骼、
    // 编过；mdlc 旧实现把每个 SMD 的每个 node 都收进表（134 根），于是这里
    // 反而「不报错」—— 收紧判据后必须同步放宽这条检查，否则会出现
    // 「官方能编过、mdlc 编不过」的假阳性。
    //
    // 逐帧构造时（见下方）对映射不到的骨骼已经 `continue` 跳过，语义一致。
    let smd_names: std::collections::HashSet<&str> =
        smd.nodes.iter().map(|n| n.name.as_str()).collect();
    let mut fallback: Vec<Option<crate::smd::SmdPose>> = vec![None; bone_count];
    for (di, b) in desc.bones.iter().enumerate() {
        if smd_names.contains(b.name.as_str()) {
            continue;
        }
        fallback[di] = Some(crate::smd::SmdPose {
            bone: di as i32,
            position: b.position.unwrap_or([0.0; 3]),
            // TOML 的 `rotation` 是**角度**，SMD 的 `SmdPose.rotation` 是**弧度**。
            rotation: b.rotation.unwrap_or([0.0; 3]).map(f32::to_radians),
        });
    }
    let mut frames: Vec<Vec<crate::smd::SmdPose>> = Vec::with_capacity(smd.frames.len());
    // 官方 `Grab_Animation`（`studiomdl.cpp:1065-1135`）的行构造顺序：
    //
    // ```cpp
    // psource->rawanim[t] = (s_bone_t *)kalloc( 1, size );   // kalloc = calloc ⟹ **零填充**
    // if (t > 0 && psource->rawanim[t-1]) {                  // 再**逐骨骼**拷贝上一帧
    //     for (int j = 0; j < psource->numbones; j++) { VectorCopy(...); }
    // }
    // // 最后才用本帧的骨骼行覆盖
    // ```
    //
    // ⚠️ **官方从不检查「每根骨骼都有数据」** —— 本帧没写的骨骼就保留
    // 「上一帧的值」（第 0 帧则是 `(0,0,0)` / 单位旋转）。
    //
    // # 这条曾经写错过（且把真实模型挡在门外）
    //
    // 早先的实现要求「每帧每根骨骼都必须有姿态行」，否则报
    // 「缺少部分骨骼的姿态（1 / 64 根有数据）」并**拒绝编译**。
    //
    // 真实反例（`vm_test_group`，6 个官方 viewmodel 全部命中）：
    // Crowbar 反编译出的 `*_corrective_animation.smd` 只有
    // **一行**骨架数据（bone 0），而 `nodes` 段列了全部 64 根 ——
    // 真 `studiomdl.exe` **静默接受**（编译日志里只有 `SMD MODEL xxx`，
    // 零告警），mdlc 却 6/6 编译失败。
    //
    // 这类 SMD 的语义正是「除 bone 0 外全为零」：它们是 `subtract` 用的
    // 修正动画（去掉参考姿态里的 −90°），其余骨骼保持 0 ⟹ 相减后不变。
    //
    // 对照证据：`docs/_probe/cmp_vm_test_group.js`、
    // `docs/_probe/oracle_partial_frame_smd.js`。
    let zero_pose = crate::smd::SmdPose {
        bone: 0,
        position: [0.0; 3],
        rotation: [0.0; 3],
    };
    let mut prev: Vec<crate::smd::SmdPose> = vec![zero_pose; bone_count];
    for f in &smd.frames {
        // ① 从上一帧继承（第 0 帧 = 零填充）。
        let mut row = prev.clone();
        // ② `$definebone` 的兜底姿态（描述里有、SMD `nodes` 里没有的骨骼）。
        for (di, fb) in fallback.iter().enumerate() {
            if let Some(p) = fb {
                row[di] = *p;
            }
        }
        // ③ 本帧显式给出的骨骼行覆盖之。
        for p in &f.poses {
            let Some(node) = smd.nodes.get(p.bone.max(0) as usize) else {
                continue;
            };
            let Some(&di) = desc_index.get(node.name.as_str()) else {
                continue;
            };
            row[di] = crate::smd::SmdPose {
                bone: di as i32,
                position: p.position,
                rotation: p.rotation,
            };
        }
        frames.push(row.clone());
        prev = row;
    }
    Some((frames, smd))
}

/// 解析 blend 参数名 → `[[model.pose_parameters]]` 的下标。
///
/// 接受**名字**或**十进制下标**（与 `[[ikchains]]` 的「名字或下标」惯例一致）。
fn resolve_pose_param_index(desc: &ModelDesc, name: &str) -> Option<i32> {
    if let Ok(i) = name.parse::<i32>()
        && i >= 0
        && (i as usize) < desc.model.pose_parameters.len()
    {
        return Some(i);
    }
    desc.model
        .pose_parameters
        .iter()
        .position(|p| p.name == name)
        .map(|i| i as i32)
}

/// 对每一帧减掉 `base` 的某一帧 —— QC 的 `subtract`。
///
/// # 语义（`simplify.cpp:1066-1122`，`STUDIO_POST` 分支）
///
/// ```c
/// QuaternionSMAngles( -1, src[k].rot, pdest->sanim[j][k].rot, ... );  // 旋转
/// VectorSubtract( pdest->sanim[j][k].pos, src[k].pos, ... );          // 位置
/// ```
///
/// `QuaternionSM(s, p, q) = normalize((s·p) · q)`（`bone_setup.cpp:1131-1142`），
/// 所以旋转结果是 **`(−src) · dest`** —— 注意顺序：**参考在左**。
/// 写成 `dest · (−src)` 会得到共轭的结果（旋转方向相反），
/// 在纯单轴旋转上不易察觉，但复合旋转会明显错。
///
/// # 为什么 `subtract` 必然带 `STUDIO_POST`
///
/// QC 的 `subtract` 关键字自己就 `|= STUDIO_POST`（`studiomdl.cpp:1750`），
/// 所以永远走上面这一支；`else` 分支（`QuaternionMAAngles`）对应的是
/// **没有** `STUDIO_POST` 的内部调用路径，QC 表达不出来。
fn subtract_base_frames(
    frames: &mut [Vec<crate::smd::SmdPose>],
    base: &[Vec<crate::smd::SmdPose>],
    base_frame: usize,
    weights: &[f32],
) {
    let Some(src) = base.get(base_frame) else {
        return;
    };
    for row in frames.iter_mut() {
        for (k, pose) in row.iter_mut().enumerate() {
            // ⚠️ **权重 ≤ 0 的骨骼不做减除。**
            //
            // 官方 `subtractBaseAnimations`（`simplify.cpp:1084-1120`）：
            //
            // ```c
            // for (k = 0; k < g_numbones; k++)
            //     for (j = 0; j < pdest->numframes; j++)
            //         if (pdest->weight[k] > 0)      // ← 判据在这里
            //         {
            //             QuaternionSMAngles( -1, src[k].rot, pdest->sanim[j][k].rot, ... );
            //             VectorSubtract( pdest->sanim[j][k].pos, src[k].pos, ... );
            //         }
            // ```
            //
            // # 实测影响（`v_smg_mp5` / `v_snip_awp` / `v_snip_scout`）
            //
            // 这三个模型的 `weights_helping_hand_extend` 把**全部骨骼的权重
            // 设成 0**，用于 `helping_hand_*` / `item_*` 共 6 条序列。
            // 官方那边 `weight[k] > 0` **全部为假** ⟹ **减除一次都不做**
            // ⟹ 动画保留 SMD 的原始姿态。
            //
            // 后果（`docs/_probe/probe_bbox_weightlist.js`）：
            //
            // | | 官方 bbmin[2] | mdlc（忽略权重时） |
            // |---|---|---|
            // | `helping_hand_extend_layer` | **−59.8125** | −9.2092 |
            //
            // 而 `−59.8125` 正是 `a_idle_1.smd` 第 0 帧骨骼 0 的 `pos[2]`
            // —— 即**未减除**的原始值。18 条序列受影响，Δ 达 **50.6**。
            //
            // > ⚠️ 这不只影响包围盒：**写进动画链的姿态本身也不同**
            // > （官方保留原值，mdlc 减掉了参考姿态）⟹ 游戏里这些
            // > 「全 0 权重」的层会**整体偏移**。这是真实的渲染差异。
            let w = weights.get(k).copied().unwrap_or(1.0);
            if w <= 0.0 {
                continue;
            }
            let Some(s) = src.get(k) else { continue };
            // 旋转：`conj(src) · dest`，再转回欧拉角。
            //
            // ⚠️ **`QuaternionScale(p, -1)` 是共轭，不是「四个分量全取负」。**
            //
            // 官方 `QuaternionScale`（`mathlib_base.cpp`）：
            //
            // ```c
            // float sinom = MIN( sqrt(DotProduct(&p.x, &p.x)), 1.f );
            // float sinsom = sin( asin(sinom) * t );      // t = -1 → -sinom
            // t = sinsom / (sinom + FLT_EPSILON);         // ≈ -1
            // VectorScale( &p.x, t, &q.x );               // 向量部分 × (-1)
            // r = sqrt( 1 - sinsom*sinsom );              // = |w|
            // if (p.w < 0) q.w = -r; else q.w = r;        // **w 保持 p.w 的符号**
            // ```
            //
            // 所以 `q = (-x, -y, -z, w)` = **共轭**。对单位四元数，
            // 共轭是**逆旋转**，而「全取负」`-q` 是**同一个**旋转 ——
            // 两者语义完全不同：
            //
            // | 写法 | 结果 | Rz(20°) 与 Rz(35°) 的差 |
            // |---|---|---|
            // | 共轭 `conj(src)·dest` | **相对旋转** | **15°** ✓ |
            // | 取负 `(-src)·dest` | 复合旋转 | 55° ✗ |
            //
            // 实测判据：受控实验 `blend1`/`blend2` 的参考动画 `a_base`
            // 第 0 帧旋转**全为 0**（单位四元数），此时共轭与取负**同结果**
            // —— 所以那两个实验照不到这个 bug。miku 的 `a_idle` 参考姿态
            // **非零**，实测 mdlc 给出 **54.999996°**（正是「取负」的预测），
            // 修正后为 15°。
            let qs = crate::bone_math::angle_quaternion(s.rotation);
            let qd = crate::bone_math::angle_quaternion(pose.rotation);
            let p1 = [-qs[0], -qs[1], -qs[2], qs[3]];
            let q = quaternion_mult(p1, qd);
            pose.rotation = crate::bone_math::quaternion_angles(q);
            // 位置：直接相减。
            for i in 0..3 {
                pose.position[i] -= s.position[i];
            }
        }
    }
}

/// 四元数乘法 `a · b`（与 `QuaternionMult` 同序）。
///
/// 实现在 [`crate::bone_math::quaternion_mult`] —— 那里有完整的
/// `QuaternionAlign` 说明。这里留一个薄封装是因为本模块有若干调用点
/// 是按「欧拉角 → 四元数」直接传的，名字短一点读起来更顺。
fn quaternion_mult(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    crate::bone_math::quaternion_mult(a, b)
}

/// 一个 blend 轴上**逐格**的参数取值（`simplify.cpp:5579-5590`）。
///
/// ```text
/// for m in 0..groupsize:
///     f = m / (groupsize - 1)
///     keys[m] = start * (1 - f) + end * f
/// ```
///
/// `groupsize == 1` 时官方**不写这个轴**（`write.cpp:449` 只在
/// `groupsize[0] > 1 || groupsize[1] > 1` 时写 posekey），
/// 但 `param1[0]` 在官方是 0 —— 所以这里返回长度 1 的 `[0.0]`，
/// 由写出器决定要不要用它。
fn blend_param_keys(start: f32, end: f32, groupsize: usize) -> Vec<f32> {
    if groupsize <= 1 {
        return vec![0.0];
    }
    (0..groupsize)
        .map(|m| {
            let f = m as f32 / (groupsize - 1) as f32;
            start * (1.0 - f) + end * f
        })
        .collect()
}

/// 自动层的一个时间量（`write.cpp:539-551`）。
///
/// 不带 `STUDIO_AL_POSE`（**0x4000**）时**除以 `numframes − 1`** 转成 cycle。
fn layer_time(v: f32, flags: i32, num_frames: f32) -> f32 {
    const STUDIO_AL_POSE: i32 = 0x4000;
    if flags & STUDIO_AL_POSE != 0 || num_frames <= 1.0 {
        v
    } else {
        v / (num_frames - 1.0)
    }
}

/// `CalcPoseParameterValue`（`simplify.cpp:5428-5446`）。
///
/// ⚠️ **兜底的 `return 0.0` 是这条路径的核心，不是「错误处理」。**
///
/// `paramcontrol` 在官方是 `memset` 后的 **0** —— 只有 `calcblend` 分支会
/// 写它（`studiomdl.cpp:2773`）。而 `switch(0)` 不匹配任何 case
/// ⟹ 每一格都算出 `0.0` ⟹ `paramstart == paramend` ⟹
/// `MdlError("calcblend failed in %s")`（`simplify.cpp:5566-5569`）。
///
/// 这就是「只写 `blendwidth` 不写 `blend`」的序列被官方拒绝的**全部原因**
/// —— 不是「缺了参数」，而是「误入 calc 分支 + 控制轴是 0」。
fn calc_pose_parameter_value(control: i32, angles: [f32; 3], pos: [f32; 3]) -> f32 {
    use crate::model::control as c;
    /// `RAD2DEG(x)` —— 官方是 `(float)(x * (180.0/M_PI))`。
    fn rad2deg(v: f32) -> f32 {
        v * (180.0f32 / std::f32::consts::PI)
    }
    match control {
        c::X => pos[0],
        c::Y => pos[1],
        c::Z => pos[2],
        c::XR => rad2deg(angles[0]),
        c::YR => rad2deg(angles[1]),
        c::ZR => rad2deg(angles[2]),
        // 含 `0`（memset 残留）与 `-1`（`lookupControl` 不认识）——
        // 官方两者都落到这个兜底。
        _ => 0.0,
    }
}

/// 一个动画在 `CalcBoneTransforms` 下需要的全部输入。
#[derive(Clone, Copy)]
struct AnimPose<'a> {
    /// 第 0 帧的骨骼姿态。
    frame0: &'a [crate::smd::SmdPose],
    /// `STUDIO_DELTA` —— 本动画存的是增量。
    delta: bool,
    /// `panimation->weight[k]`（来自 `$weightlist`）。
    weights: &'a [f32],
}

/// 取动画池第 `ix` 条的 [`AnimPose`]（越界得到空姿态）。
fn anim_pose<'a>(
    anims: &'a [crate::model::CompiledAnimation],
    anim_weights: &'a [Vec<f32>],
    ix: usize,
) -> AnimPose<'a> {
    let a = anims.get(ix);
    AnimPose {
        frame0: a
            .and_then(|a| a.frames.first())
            .map(|r| r.as_slice())
            .unwrap_or(&[]),
        delta: a.map(|a| a.delta).unwrap_or(false),
        weights: anim_weights.get(ix).map(|w| w.as_slice()).unwrap_or(&[]),
    }
}

/// `CalcBoneTransforms(panimation, pbaseanimation, 0, boneToWorld)`
/// （`simplify.cpp:4539-4589`）—— 求第 0 帧的**世界**矩阵。
///
/// 与 [`delta_frame_worlds`] 的区别：`panimation->weight[k]` 是**逐骨骼**的
/// （`$weightlist`），而那个函数把 `WEIGHT` 写死成 `1.0`，所以这里要整条
/// 权重表。
///
/// ⚠️ 官方**不**在根骨骼上左乘 `rootxform`（对比 `simplify.cpp:4580-4583`
/// 与 `bone_setup.cpp:2698`）。但本函数走的
/// [`frame_worlds_from_locals`] **会**加 —— 这不影响结果：
/// 两个世界矩阵用的是同一个根变换，`worldToBoneMid ∘ boneToWorldRel`
/// 里它会被**约掉**。
fn calcblend_worlds(
    desc: &ModelDesc,
    parents: &[i32],
    p: &AnimPose<'_>,
    base: &AnimPose<'_>,
) -> Vec<crate::bone_math::Matrix3x4> {
    let empty = |k: usize| crate::smd::SmdPose {
        bone: k as i32,
        position: [0.0; 3],
        rotation: [0.0; 3],
    };
    let locals: Vec<crate::bone_math::Matrix3x4> = (0..parents.len())
        .map(|k| {
            let f = p.frame0.get(k).copied().unwrap_or_else(|| empty(k));
            if !p.delta {
                // `AngleMatrix( sanim[frame][k].rot, sanim[frame][k].pos, bonematrix )`
                return crate::bone_math::local_transform(f.position, f.rotation);
            }
            // delta：`QuaternionMA(q_base, s, q_delta)` 重建（`:4562-4577`）。
            let b = base.frame0.get(k).copied().unwrap_or_else(|| empty(k));
            let s = p.weights.get(k).copied().unwrap_or(1.0);
            let q1 = crate::bone_math::angle_quaternion(b.rotation);
            let q2 = crate::bone_math::angle_quaternion(f.rotation);
            let q3 = crate::bone_math::quaternion_ma(q1, s, q2);
            let mut p3 = b.position;
            for (d, src) in p3.iter_mut().zip(f.position.iter()) {
                *d += s * src;
            }
            crate::bone_math::quaternion_local_transform(q3, p3)
        })
        .collect();
    frame_worlds_from_locals(desc, parents, &locals)
}

/// 一条 `calcblend` 轴的逐格取值（官方 `CalcPoseParameters`，
/// `simplify.cpp:5448-5596`）。
///
/// 返回 `(param_i[], paramstart, paramend)`。
///
/// # 算法
///
/// ```text
/// refWorld = CalcBoneTransforms(paramanim, 0)            // 1 参数版 ⟹ 基准 = g_panimation[0]
/// mid      = refWorld[att.bone] ∘ att.local              // 附着点的「零点」
/// invMid   = inverse(mid)
/// for m in 0..groupsize[axis]:
///     cell     = panim[m[0]][m[1]]                       // 另一根轴取 other
///     rel      = CalcBoneTransforms(cell, paramcompanim, 0)[att.bone] ∘ att.local
///     boneRel  = invMid ∘ rel
///     v        = CalcPoseParameterValue(paramcontrol, angles(boneRel), pos(boneRel))
///     param_i[m] = v；m == 0 ⟹ paramstart；m == last ⟹ paramend
/// ```
#[allow(clippy::too_many_arguments)]
fn calc_blend_axis(
    desc: &ModelDesc,
    parents: &[i32],
    anims: &[crate::model::CompiledAnimation],
    anim_weights: &[Vec<f32>],
    // 网格每一格 → 动画池下标（**行主序**：`cell = j + k * groupsize[0]`）。
    cell_anim: &[usize],
    grid: [usize; 2],
    axis: usize,
    // `blendcenter` 在网格里的位置（`None` = 没写或没找到）。
    center: Option<[usize; 2]>,
    att_bone: usize,
    att_local: &crate::bone_math::Matrix3x4,
    control: i32,
    // `blendref` 解析出的动画下标（缺省 `0` = `g_panimation[0]`）。
    ref_anim: usize,
    // `blendcomp` 解析出的动画下标（缺省同 `ref_anim`）。
    comp_anim: usize,
) -> (Vec<f32>, f32, f32) {
    // 基准 `g_panimation[0]` —— 官方 1 参数版 `CalcBoneTransforms` 固定传它
    // （`simplify.cpp:4533-4536`）。
    let base0 = anim_pose(anims, anim_weights, 0);
    let ref_pose = anim_pose(anims, anim_weights, ref_anim);
    let comp_pose = anim_pose(anims, anim_weights, comp_anim);

    // 附着点的「零点」世界位姿（`:5487-5492`）。
    let ref_world = calcblend_worlds(desc, parents, &ref_pose, &base0);
    let mid = ref_world
        .get(att_bone)
        .map_or(*att_local, |m| crate::bone_math::concat(m, att_local));
    let inv_mid = crate::bone_math::invert(&mid);

    // 「另一根轴」取网格的哪一格（`:5503-5521`）。
    //
    // 命中 `blendcenter` 时官方把 `m[0]`/`m[1]` **都**写死，于是另一根轴
    // 也跟着它走；没写（或没找到）时取中点 `groupsize[1-axis] / 2`。
    let other = match center {
        Some(c) => c[1 - axis],
        None => grid[1 - axis] / 2,
    }
    .min(grid[1 - axis].saturating_sub(1));

    let n = grid[axis];
    let mut keys = Vec::with_capacity(n);
    let (mut start, mut end) = (0.0f32, 0.0f32);
    for m in 0..n {
        let (ci, cj) = if axis == 0 { (m, other) } else { (other, m) };
        let pose = cell_anim
            .get(cj * grid[0] + ci)
            .map(|&ix| anim_pose(anims, anim_weights, ix))
            .unwrap_or(AnimPose {
                frame0: &[],
                delta: false,
                weights: &[],
            });
        let world = calcblend_worlds(desc, parents, &pose, &comp_pose);
        let rel = world
            .get(att_bone)
            .map_or(*att_local, |m| crate::bone_math::concat(m, att_local));
        let bone_rel = crate::bone_math::concat(&inv_mid, &rel);
        let v = calc_pose_parameter_value(
            control,
            crate::bone_math::matrix_angles(&bone_rel),
            [bone_rel[3], bone_rel[7], bone_rel[11]],
        );
        if m == 0 {
            start = v;
        }
        if m + 1 == n {
            end = v;
        }
        keys.push(v);
    }
    (keys, start, end)
}

/// 按名字查附着点，返回 `(骨骼下标, local 矩阵)`。
///
/// 官方 `LookupAttachment`（`studiomdl.cpp:5316-5327`）是**线性查表、
/// 大小写不敏感**（`stricmp`），查不到返回 `-1` ⟹
/// `Unknown calcblend attachment "<名>"`（`:2766-2770`）。
///
/// ⚠️ 官方在**解析期**查表，所以「写在 `$sequence` 之后」的 `$attachment`
/// 官方看不到。本实现查**全部**附着点（更宽松）—— 真实 QC 里
/// `$attachment` 都在序列之前，这个差异不可达。
fn lookup_attachment(
    desc: &ModelDesc,
    bone_index: &std::collections::HashMap<&str, usize>,
    name: &str,
    pose_to_bone: &[crate::bone_math::Matrix3x4],
) -> Option<(usize, crate::bone_math::Matrix3x4)> {
    let (at, bone) = find_attachment(desc, bone_index, name)?;
    Some((bone, attachment_local_matrix(desc, at, pose_to_bone, bone)))
}

/// 按名字查附着点，返回 `(附着点, 绑定的骨骼下标)` —— **不构造** `local` 矩阵。
///
/// 只做「名字 + 骨骼都存在吗」这一半的判定（`local` 需要 `poseToBone`，
/// 而序列建立期还拿不到），供 `$sequence` 的 `calcblend` 参数占位时做存在性检查。
fn find_attachment<'d>(
    desc: &'d ModelDesc,
    bone_index: &std::collections::HashMap<&str, usize>,
    name: &str,
) -> Option<(&'d crate::model::Attachment, usize)> {
    let at = desc
        .attachments
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(name))?;
    let bone = *bone_index.get(at.bone.as_str())?;
    Some((at, bone))
}

/// `blendref` / `blendcomp` / `blendcenter` 的名字 → 动画池下标。
///
/// 官方 `LookupAnimation`（`studiomdl.cpp:2381-2397`）**先查动画池、
/// 再查序列池**（序列 ⟹ 它的 `panim[0][0]`），两者都是 `stricmp`
/// （大小写不敏感）。
fn resolve_lookup_animation(
    name: &str,
    anims: &[crate::model::CompiledAnimation],
    anim_index: &std::collections::HashMap<&str, usize>,
    sequences: &[crate::model::CompiledSequence],
) -> Option<usize> {
    // 动画池（先按精确名查快表，再退回大小写不敏感扫描）。
    if let Some(&i) = anim_index.get(name) {
        return Some(i);
    }
    if let Some(i) = anims.iter().position(|a| a.name.eq_ignore_ascii_case(name)) {
        return Some(i);
    }
    // 序列池 ⟹ 该序列的第一格动画。
    sequences
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .and_then(|s| s.cells.first().copied())
}

/// `CalcPoseParameters`（`simplify.cpp:5448-5596`）—— 把每条序列的
/// **calc 轴**算出来，写回 `blend_params`。
///
/// # 时序
///
/// 官方在 `ProcessData` 末尾调用（`simplify.cpp:7315`），**晚于**
/// `LinkAttachments`（7298）与 `ProcessIKRules`（7311）。所以：
///
/// * 附着点已链接（`att.bone` 是全局骨骼下标）；
/// * 动画池、序列池都已建完（`blendref` 才能回落到序列池）。
///
/// # 失败判据
///
/// ```c
/// if (fabs( pseq->paramstart[iPose] - pseq->paramend[iPose]) < 0.01)
///     MdlError( "calcblend failed in %s\n", pseq->name );
/// ```
///
/// ⚠️ 这是**硬错误**（`MdlError` 直接终止编译，不产生产物）—— 不是警告。
/// 复刻它是本函数的**主要目的**：官方拒绝的 QC，mdlc 也必须拒绝，
/// 否则会静默产出 `paramindex`/`posekey` 全错、且引擎里姿势参数
/// **完全失效**的模型。
fn apply_calc_blend_axes(
    compiled: &mut CompiledModelDesc,
    anim_weights: &[Vec<f32>],
    base_dir: &Path,
) -> Result<(), Vec<CompileError>> {
    // 没有任何 calc 轴 ⟹ 完全不动（对既有产物零影响）。
    if !compiled.sequences.iter().any(|s| !s.calc_axes.is_empty()) {
        return Ok(());
    }

    let desc = compiled.desc.clone();
    let bone_index = desc.bone_index();
    let parents = bone_parents(&desc);
    // `absolute` 附着点的 `local` 需要附着点骨骼的 `poseToBone`
    // （官方 `LinkAttachments()`，`simplify.cpp:5388`）—— 必须与落盘的那一份
    // 完全一致，所以走同一个 [`bone_pose_to_bone`]。
    let pose_to_bone = bone_pose_to_bone(&desc, compiled, &parents);
    let mut errors: Vec<CompileError> = Vec::new();

    // 先把每条序列的 `calc_axes` 取出来（避免同时借用 `compiled` 的两部分）。
    let plans: Vec<(usize, Vec<crate::model::CompiledCalcAxis>)> = compiled
        .sequences
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.calc_axes.is_empty())
        .map(|(i, s)| (i, s.calc_axes.clone()))
        .collect();

    // `blendref` / `blendcomp` 的解析结果（逐序列）。
    //
    // 官方 `LookupAnimation` 查不到时返回 `NULL`，而
    // `simplify.cpp:5476-5484` 对 `NULL` 有回落 —— 但解析期的
    // `TokenError("Unknown blendref animation")`（`studiomdl.cpp:2781`）
    // 已经挡掉了不存在的名字，所以这里解析不出来只可能是内部不一致。
    let mut per_seq: Vec<Vec<(usize, crate::model::CompiledBlendParam)>> = Vec::new();
    for (si, axes) in &plans {
        let seq = &compiled.sequences[*si];
        let anims = &compiled.animations;
        // `blendref` → `paramanim`；缺省 `g_panimation[0]`（下标 0）。
        let ref_anim = seq
            .blend_ref
            .as_ref()
            .and_then(|n| {
                resolve_lookup_animation(n, anims, &compiled_anim_index(anims), &compiled.sequences)
            })
            .unwrap_or(0);
        // `blendcomp` → `paramcompanim`；缺省回落到 `paramanim`。
        let comp_anim = seq
            .blend_comp
            .as_ref()
            .and_then(|n| {
                resolve_lookup_animation(n, anims, &compiled_anim_index(anims), &compiled.sequences)
            })
            .unwrap_or(ref_anim);

        let grid = seq_grid_of(seq);
        let mut out = Vec::new();
        for ax in axes {
            // 附着点：`None` 是官方 `memset` 残留路径。
            //
            // ⚠️ 此时 `paramcontrol` 也必然是 0 ⟹ `CalcPoseParameterValue`
            // 恒返回 `0.0`，所以**附着点取什么都一样**。用第 0 根骨骼 +
            // 单位矩阵即可（保证不越界）。
            let (att_bone, att_local) = match &ax.attachment {
                Some(name) => match lookup_attachment(&desc, &bone_index, name, &pose_to_bone) {
                    Some(v) => v,
                    None => {
                        errors.push(CompileError::Message {
                            at: format!("sequences[{si}].blend_params[{}]", ax.axis),
                            message: crate::tr_fmt!(
                                "unknown calcblend attachment point %{name}",
                                name = format!("{name:?}")
                            ),
                        });
                        continue;
                    }
                },
                None => (0usize, crate::bone_math::identity()),
            };
            let control = ax
                .control
                .as_deref()
                .map(crate::model::control::lookup)
                .unwrap_or(0);
            let (keys, start, end) = calc_blend_axis(
                &desc,
                &parents,
                &compiled.animations,
                anim_weights,
                &seq.cells,
                grid,
                ax.axis,
                seq.blend_center,
                att_bone,
                &att_local,
                control,
                ref_anim,
                comp_anim,
            );
            // ---- 失败判据（`simplify.cpp:5566-5569`）----
            if (start - end).abs() < 0.01 {
                errors.push(CompileError::Message {
                    at: format!("sequences[{si}]"),
                    message: crate::tr_fmt!(
                        "calcblend failed in %{name} (paramstart=%{start} paramend=%{end}, \
                         the difference is < 0.01; the official code aborts the compile here)",
                        name = seq.name,
                        start = start,
                        end = end
                    ),
                });
                continue;
            }
            out.push((
                ax.axis,
                crate::model::CompiledBlendParam {
                    parameter_index: seq.blend_params[ax.axis]
                        .as_ref()
                        .map(|p| p.parameter_index)
                        .unwrap_or(-1),
                    start,
                    end,
                    keys,
                },
            ));
        }
        per_seq.push(out);
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    for ((si, _), vals) in plans.iter().zip(per_seq) {
        for (axis, p) in vals {
            compiled.sequences[*si].blend_params[axis] = Some(p);
        }
    }
    let _ = base_dir;
    Ok(())
}

/// `(groupsize[0], groupsize[1])` —— 与 `anim_writer::seq_grid` 同口径。
fn seq_grid_of(seq: &crate::model::CompiledSequence) -> [usize; 2] {
    if seq.forward_declared || seq.cells.len() <= 1 {
        return [1, 1];
    }
    let w = seq.blend_width.max(1) as usize;
    [w, seq.cells.len() / w]
}

/// 动画池的「名字 → 下标」快表（大小写敏感，与 `anim_index` 同口径）。
fn compiled_anim_index(
    anims: &[crate::model::CompiledAnimation],
) -> std::collections::HashMap<&str, usize> {
    anims
        .iter()
        .enumerate()
        .map(|(i, a)| (a.name.as_str(), i))
        .collect()
}

/// 逐骨骼的 `poseToBone` 矩阵。
///
/// 官方 `g_bonetable[k].boneToPose` 是骨骼的**世界**矩阵，而文件里的
/// `mstudiobone_t.poseToBone`（偏移 `0x60`）是它的**逆**；写出器把本函数的
/// 结果落进那个字段，`LinkAttachments()` 又用它给 `absolute` 附着点做左乘
/// （`simplify.cpp:5388`）⟹ 两处必须是**同一个**矩阵，所以只算一次、共用。
///
/// `parents` 必须是**骨骼表下标**空间的父下标（[`bone_parents`] 的产物）。
pub(crate) fn bone_pose_to_bone(
    desc: &ModelDesc,
    compiled: &CompiledModelDesc,
    parents: &[i32],
) -> Vec<crate::bone_math::Matrix3x4> {
    let n = desc.bones.len();
    let mut positions = Vec::with_capacity(n);
    let mut rotations = Vec::with_capacity(n);
    for i in 0..n {
        let (p, r) = resolve_bone_pose(desc, compiled, i);
        positions.push(p);
        rotations.push(r);
    }
    crate::bone_math::compute_pose_to_bone(&positions, &rotations, parents)
}

/// 附着点的 `local` 矩阵 —— **唯一实现**，写出器直接调用它。
///
/// 之所以只有一份：`calcblend` 用它算附着点相对位姿，而同一个矩阵也会落进
/// `mstudioattachment_t.local`。两份实现一旦漂移，算出来的姿势参数就会
/// 与实际渲染用的附着点不是同一个东西。
///
/// # 官方口径
///
/// 1. **解析期**（`studiomdl.cpp:5212-5310`）：以 `AngleMatrix( QAngle(0,0,0) )`
///    起步（`:5237`），选项按**出现顺序**覆盖同一个 `local`：
///    - `absolute` ⟹ `AngleIMatrix( g_defaultrotation )`（`:5246`）
///    - `rotate` ⟹ `AngleMatrix( angles )`（`:5268`）
///    - `x_and_z_axes` ⟹ 直接写列（`:5294-5297`）
///
///    平移列在选项循环**之后**才写（`:5305-5307`）⟹ 没有任何选项能覆盖它。
/// 2. **`$staticprop`**：`MakeStaticProp()` 做
///    `ConcatTransforms( rotated, local, local )`（`simplify.cpp:3392`）。
/// 3. **`LinkAttachments()`**（`simplify.cpp:5379-5388`）：
///    `absolute` ⟹ `local = poseToBone ∘ local`；否则 `poseToBone ∘ boneToPose`
///    互相抵消，`local` 原样落盘。
///
/// `pose_to_bone[bone]` 必须与落进产物 `mstudiobone_t.poseToBone` 的那一份
/// 完全一致 —— 即 [`bone_pose_to_bone`] 的产物。
pub(crate) fn attachment_local_matrix(
    desc: &ModelDesc,
    at: &crate::model::Attachment,
    pose_to_bone: &[crate::bone_math::Matrix3x4],
    bone: usize,
) -> crate::bone_math::Matrix3x4 {
    let pos = at.position.unwrap_or([0.0; 3]);
    let rot = at.rotation.unwrap_or([0.0; 3]);
    let angles = [
        rot[0].to_radians(),
        rot[1].to_radians(),
        rot[2].to_radians(),
    ];
    // 旋转由**最后一个**改旋转的选项决定（官方就是顺序覆盖同一个矩阵）。
    // `absolute_rotation == None`（TOML 只写 `absolute = true`）⟹ 跟随 `absolute`。
    let use_imatrix = at.absolute_rotation.unwrap_or(at.absolute);
    let mut local = if use_imatrix {
        // `AngleIMatrix( g_defaultrotation )`（`studiomdl.cpp:5246`）。
        // `g_defaultrotation` 默认 `RadianEuler( 0, 0, M_PI / 2 )`
        // （`studiomdl.cpp:6883`），与 `ani_writer::ROOT_REFERENCE_ANGLES`
        // 是同一个值 —— 用户 QC 没有 `$origin`/`$upaxis`，走的就是默认。
        crate::bone_math::angle_imatrix(crate::ani_writer::ROOT_REFERENCE_ANGLES)
    } else {
        crate::bone_math::angle_matrix(angles)
    };
    // 平移列最后写（`studiomdl.cpp:5305-5307`）。
    local[3] = pos[0];
    local[7] = pos[1];
    local[11] = pos[2];
    if desc.model.static_prop
        && desc.model.autocenter
        && desc
            .attachments
            .last()
            .is_some_and(|last| std::ptr::eq(last, at))
    {
        local
    } else if desc.model.static_prop {
        // `$staticprop`：`MakeStaticProp()`（`simplify.cpp:3386-3397`）。
        //
        // ⚠️ 它还把 `type` 清零（`:3396`）⟹ 紧随其后的 `LinkAttachments()`
        // 里 `IS_ABSOLUTE` 已不复存在，静态道具的附着点**不会**被左乘。
        crate::bone_math::concat(&static_prop_source_transform(desc), &local)
    } else if at.absolute {
        // `LinkAttachments()`：`ConcatTransforms( poseToBone, world, local )`，
        // 而 `absolute` 时 `world` 就是 `local`（`simplify.cpp:5379-5388`）。
        crate::bone_math::concat(&pose_to_bone[bone], &local)
    } else {
        local
    }
}

/// 取 SMD 参考姿态里某根骨骼的姿态。
fn pose_for(smd: &Smd, node_index: usize) -> Option<&SmdPose> {
    let f = smd.reference_frame()?;
    // skeleton 段的行序与 nodes 顺序一一对应，所以按下标取；
    // 但也允许文件里显式给出 bone 下标（用 bone 字段匹配）。
    f.poses
        .iter()
        .find(|p| p.bone == node_index as i32)
        .or_else(|| f.poses.get(node_index))
}

fn select_source_frames(
    frames: &mut Vec<Vec<crate::smd::SmdPose>>,
    range: Option<[i32; 2]>,
    at: &str,
) -> Result<(), CompileError> {
    let Some([lo, hi]) = range else {
        return Ok(());
    };
    if frames.is_empty() {
        return Err(e(at, "animation source contains no frames"));
    }
    let last = frames.len() as i32 - 1;
    let lo = lo.clamp(0, last);
    let hi = hi.clamp(0, last);
    if hi < lo {
        return Err(e(
            at,
            crate::tr_fmt!(
                "the end frame %{hi} is earlier than the start frame %{lo} (the source only has %{n} frames)",
                hi = hi,
                lo = lo,
                n = frames.len()
            ),
        ));
    }
    *frames = frames[lo as usize..=hi as usize].to_vec();
    Ok(())
}

/// 编译：描述 + SMD → IR。
///
/// `base_dir` 是描述文件所在目录，用于解析 SMD 的相对路径。
#[hotpath::measure(label = "compile() 顶层")]
pub fn compile(desc: &ModelDesc, base_dir: &Path) -> Result<CompiledModelDesc, Vec<CompileError>> {
    // 先做描述层校验（能一次报出全部问题，比逐个文件报错友好）。
    if let Err(errs) = desc.validate() {
        return Err(errs.into_iter().map(CompileError::Desc).collect());
    }

    // Every model needs every global LOD, including bodygroup choices without
    // a replacement source. Missing replacements retain the LOD 0 geometry.
    let mut normalized = desc.clone();
    let levels = desc
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .map(|m| m.lods.len())
        .max()
        .unwrap_or(0);
    let defaults: Vec<_> = (0..levels)
        .map(|level| {
            let mut lod = desc
                .bodyparts
                .iter()
                .flat_map(|bp| &bp.models)
                .find_map(|m| m.lods.get(level))
                .unwrap()
                .clone();
            lod.smd = None;
            lod.removed = false;
            lod.reverse = false;
            lod
        })
        .collect();
    for model in normalized
        .bodyparts
        .iter_mut()
        .flat_map(|bp| &mut bp.models)
    {
        model.lods.extend_from_slice(&defaults[model.lods.len()..]);
    }
    normalized.apply_sequence_source_overrides();
    let desc = &normalized;

    let mut errors: Vec<CompileError> = Vec::new();
    let mut bodyparts = Vec::with_capacity(desc.bodyparts.len());

    hotpath::measure_block!("bodyparts: 读 SMD + build_meshes + LOD", {
        for (bi, bp) in desc.bodyparts.iter().enumerate() {
            let mut models = Vec::with_capacity(bp.models.len());
            for (mi, m) in bp.models.iter().enumerate() {
                let at = format!("bodyparts[{bi}].models[{mi}]");
                if m.smd.is_empty() && m.name.as_deref() == Some("blank") {
                    models.push(CompiledModel {
                        smd_path: PathBuf::new(),
                        name: "blank".into(),
                        poses: Vec::new(),
                        source_world: Vec::new(),
                        meshes: Vec::new(),
                        lods: (!m.lods.is_empty()).then(|| ModelLods {
                            meshes: Vec::new(),
                            num_lods: m.lods.len() + 1,
                            switch_points: std::iter::once(0.0)
                                .chain(m.lods.iter().enumerate().map(|(i, l)| {
                                    l.switch_point.unwrap_or(20.0 * (1u32 << i) as f32)
                                }))
                                .collect(),
                            bone_lod_usage: vec![0; desc.bones.len()],
                            no_facial: std::iter::once(false)
                                .chain(m.lods.iter().map(|l| l.no_facial))
                                .collect(),
                        }),
                        eyeballs: Vec::new(),
                        mesh_flexes: Vec::new(),
                        mesh_src_index: Vec::new(),
                        shape_keys: Vec::new(),
                    });
                    continue;
                }
                let smd_path = match resolve_smd_path(base_dir, &m.smd) {
                    Ok(p) => p,
                    Err(msg) => {
                        errors.push(e(format!("{at}.smd"), msg));
                        continue;
                    }
                };
                let sopts = match SrcOpts::of_model(m) {
                    Ok(o) => o,
                    Err(msg) => {
                        errors.push(e(&at, msg));
                        continue;
                    }
                };
                let geom = match read_source_geometry(&smd_path, &format!("{at}.smd"), &sopts)
                    .and_then(|mut s| {
                        crate::bone_rename::source(&mut s.smd, &desc.model.bone_renames)
                            .map_err(|message| e(&at, message))?;
                        crate::bone_hierarchy::source(&mut s.smd, &desc.model.bone_hierarchy)
                            .map_err(|message| e(format!("{at}.smd"), message))?;
                        Ok(s)
                    }) {
                    Ok(s) => s,
                    Err(err) => {
                        errors.push(err);
                        continue;
                    }
                };
                // `srcshapekey` / `srcshapekeyorder` / `srcshapekeyignore`
                // 在**读取层之外**应用：`read_source_geometry` 只管把源里
                // 有什么读出来，取哪些、按什么顺序是 QC/TOML 层的决定。
                let shape_keys = match sopts.order_shape_keys(&geom.shape_keys) {
                    Ok(v) => v,
                    Err(msg) => {
                        errors.push(e(&at, msg));
                        continue;
                    }
                };
                // ⚠️ 必须在 `let smd = geom.smd;` **之前**调用 —— 那一步会把
                // `geom` 部分移走，之后就没法再借它的诊断字段了。
                emit_fbx_diagnostics(&at, &geom, &shape_keys);
                let smd = geom.smd;

                // ⚠️ **不要**因为「网格源 SMD 里有 `[[bones]]` 没有的骨骼」而报错。
                //
                // 官方骨骼表只收 `$definebone` 与 `boneref != 0` 的骨骼
                // （`BuildGlobalBonetable`，`simplify.cpp:3668`）。网格源里
                // 「零顶点引用、又没被保命判据提到」的骨骼同样不在表里，
                // 官方靠 `MapSourcesToGlobalBonetable()`（`simplify.cpp:4148-4217`）
                // 沿父链上溯、找不到就静默重映射到根骨骼 0（`:4180`）。
                //
                // mdlc 侧对应实现在 `smd_vertex_to_ir`（同一条父链上溯），
                // 所以这里直接放行。

                let (meshes, mesh_src_index) =
                    match build_meshes(&smd, desc, &smd_path, &at, m.flip_triangles) {
                        Ok(v) => v,
                        Err(err) => {
                            errors.push(err);
                            continue;
                        }
                    };

                // ---- 多 LOD：读每个 LOD 的 SMD，按材质名对齐 mesh ----
                // 只有描述里写了 `lods` 才走这条路；否则 `lods` 保持 `None`，
                // 写出器走单 LOD 路径，产物与加这个特性之前完全一致。
                let lods = if m.lods.is_empty() {
                    None
                } else {
                    match build_model_lods(m, &meshes, &smd, desc, base_dir, &at, &sopts) {
                        Ok(v) => Some(v),
                        Err(errs) => {
                            errors.extend(errs);
                            continue;
                        }
                    }
                };

                let name = model_name(m, &smd_path);
                // 参考姿态：SMD 第 0 帧（用于自动补全描述里没写的骨骼姿态）。
                //
                // ⚠️ **必须把 `p.bone` 从「SMD node 下标」改写成「骨骼表下标」。**
                //
                // `f.poses[].bone` 是 **SMD `nodes` 段的下标**，而骨骼表是
                // `[$definebone 顺序] ++ [SMD node 顺序]`（`$definebone` 的骨骼
                // 会被提到前面）。两者**不同**，例如 `v_autoshotgun`：
                //
                // | 骨骼表 | 名字 | SMD node |
                // |---|---|---|
                // | 2 | `ValveBiped.Camera` | 84 |
                // | 84 | `attachment_jiggle_19` | 22 |
                //
                // 而 `resolve_bone_pose` 是按**骨骼表下标**查的
                // （`m.poses.iter().find(|p| p.bone == bone_index)`）。
                // 不改写的话，表[84] 会拿到 SMD node 84（= `ValveBiped.Camera`）
                // 的姿态 —— **静默的骨骼姿态错位**，实测让 `weapon` 的参考位置
                // 偏 `50.965` 单位、附着点世界位置偏 `70.9` 单位。
                //
                // 用**名字**建映射（与上面 `missing` 检查同一份 `desc_index`），
                // 这样 SMD 的 node 顺序与描述里的 `[[bones]]` 顺序无关。
                //
                // 查不到时走官方的父链上溯（`MapSourcesToGlobalBonetable()`，
                // `simplify.cpp:4148-4217`），整条链都不在表里则退到根骨骼 0。
                let bone_map = VertexBoneMap::new(&smd, desc);

                // ---- 源骨架第 0 帧的世界矩阵（官方 `BuildRawTransforms(psource, 0, …)`）----
                //
                // ⚠️ **必须用源自己的父链**（`smd.nodes[].parent`），不能用描述文件的
                // 父链。官方 `simplify.cpp:1432-1492` 逐骨骼走的是
                // `psource->localBone[k].parent`，与 `g_bonetable` 无关。
                //
                // 两条父链会分叉的典型场景：`$definebone "X" "" …` 把 `X` 重新声明为
                // 根 —— 骨骼表里 `X.parent == -1`，源里 `X` 却仍有祖先。用描述文件的
                // 父链重建会丢掉那个祖先，于是「源世界矩阵」与 `boneToPose` 恰好相等
                // ⟹ 顶点重映射被判成恒等 ⟹ 顶点原地不动而骨骼表已搬 ⟹ 网格与骨骼错开。
                let node_count = smd.nodes.len();
                let src_node_world: Vec<crate::bone_math::Matrix3x4> = match smd.reference_frame() {
                    None => Vec::new(),
                    Some(f) => {
                        let mut src_pos = vec![[0.0f32; 3]; node_count];
                        let mut src_rot = vec![[0.0f32; 3]; node_count];
                        for p in &f.poses {
                            if let Ok(ni) = usize::try_from(p.bone)
                                && ni < node_count
                            {
                                src_pos[ni] = p.position;
                                src_rot[ni] = p.rotation;
                            }
                        }
                        let src_parents: Vec<i32> = smd.nodes.iter().map(|n| n.parent).collect();
                        crate::bone_math::compute_world(&src_pos, &src_rot, &src_parents)
                    }
                };

                let mut poses: Vec<SmdPose> = Vec::new();
                // 骨骼表下标 → 源骨架第 0 帧世界矩阵。
                //
                // ⚠️ 只有**名字精确命中**时才填 —— 官方 `MapSourcesToGlobalBonetable()`
                // （`simplify.cpp:4148-4217`）只在 `k != -1` 的分支里写
                // `boneGlobalToLocal[k] = j`（`:4212`）；走父链上溯或退到根骨骼 0 的
                // 那些**不写** ⟹ 官方 `TranslateAnimations()` 那边 `q == -1`，
                // 改走 `g_bonetable` 的局部姿态沿全局父链累乘（`:1512-1524`）。
                let mut source_world: Vec<Option<crate::bone_math::Matrix3x4>> =
                    vec![None; desc.bones.len()];
                if let Some(f) = smd.reference_frame() {
                    for p in &f.poses {
                        let Ok(node_ix) = usize::try_from(p.bone) else {
                            continue;
                        };
                        if node_ix >= node_count {
                            continue;
                        }
                        if let Some(&di) = bone_map.desc_index.get(smd.nodes[node_ix].name.as_str())
                            && let Some(slot) = source_world.get_mut(di)
                        {
                            *slot = Some(src_node_world[node_ix]);
                        }
                        let Some(di) = bone_map.map_bone(node_ix) else {
                            continue;
                        };
                        poses.push(SmdPose {
                            bone: di as i32,
                            position: p.position,
                            rotation: p.rotation,
                        });
                    }
                }
                let _ = pose_for; // 保留该辅助函数供后续「按名取姿态」使用

                models.push(CompiledModel {
                    smd_path,
                    name,
                    poses,
                    source_world,
                    meshes,
                    lods,
                    eyeballs: Vec::new(),
                    mesh_flexes: Vec::new(),
                    mesh_src_index,
                    shape_keys,
                });
            }
            bodyparts.push(CompiledBodyPart {
                name: bp.name.clone(),
                base: bp.base.unwrap_or(1),
                models,
            });
        }

        if !errors.is_empty() {
            return Err(errors);
        }
    });

    // ---- 动画池：`$animation` 一条，顺序 = 声明顺序 ----
    let mut seq_errors: Vec<CompileError> = Vec::new();
    // ⚠️ 这个池**先于序列**建，因为序列的 blend 格子只是**引用**它。
    // 官方 `g_panimation[]` 就是这么一个全局池
    // （`studiomdl.cpp:2400-2413` 的 `g_panimation[g_numani++]->index`）。
    //
    // `subtract` 也在这一步做掉 —— 它引用的是**另一个动画**，
    // 所以必须等池里全部读完之后再减（QC 的声明顺序与引用顺序无关）。
    let bone_index = desc.bone_index();
    // ---- 权重表（`$weightlist`）----
    //
    // 先解析成「逐骨骼权重数组」。`resolved` 与 `desc.weight_lists` 等长；
    // 索引 0 是**隐式默认表**（全 1）。
    //
    // `weights_of` 把「动画声明的表名」映射成实际数组，缺省 = 表 0。
    let resolved_weights = resolve_weight_lists(desc);
    let n_bones = desc.bones.len();
    let default_weights = resolve_default_weight_list(desc);
    // 每个动画的权重（下标与 `anims` 对齐，最后用它算序列权重）。
    let mut anim_weights: Vec<Vec<f32>> = Vec::with_capacity(desc.animations.len());
    let weights_of = |name: Option<&str>| -> Vec<f32> {
        match name {
            None => default_weights.clone(),
            Some(n) => match weight_list_index(desc, n) {
                // 下标从 1 起 ⟹ 数组下标减 1。
                Some(i) => resolved_weights[i - 1].clone(),
                // 未知名在 `validate()` 里已报错；这里回落到默认。
                None => default_weights.clone(),
            },
        }
    };
    let mut anims: Vec<crate::model::CompiledAnimation> = Vec::with_capacity(desc.animations.len());
    // 官方的 FBX「同文件静默 1 帧」陷阱需要知道**哪些文件是几何源**
    // （见 `fbx_geometry_sources`）。⚠️ 只对 `.fbx` 生效。
    let fbx_geometry = fbx_geometry_sources(desc, base_dir);
    let mut anim_index: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::with_capacity(desc.animations.len());
    // 先只读原始帧（不减除），并记下减除参数。
    let mut retained_animation_poses = 0;
    let mut pending_subtract: Vec<Option<(String, i32)>> =
        Vec::with_capacity(desc.animations.len());
    for (ai, a) in desc.animations.iter().enumerate() {
        let at = format!("animations[{ai}]");
        if a.forward_declared {
            if !a.smd.is_empty() || !a.cmds.is_empty() || !a.ik_rules.is_empty() {
                seq_errors.push(e(
                    at,
                    "Declared animations cannot have local source data or commands",
                ));
                continue;
            }
            anim_index.entry(a.name.as_str()).or_insert(anims.len());
            pending_subtract.push(None);
            anim_weights.push(vec![0.0; n_bones]);
            anims.push(crate::model::CompiledAnimation {
                local_hierarchy: Vec::new(),
                weights: Vec::new(),
                loop_restart: 0,
                fudge_loop: false,
                extra_flags: 0x0800,
                movements: Vec::new(),
                name: a.name.clone(),
                smd_path: std::path::PathBuf::new(),
                fps: 0.0,
                looping: false,
                frames: Vec::new(),
                delta: false,
                ik_rules: Vec::new(),
                no_auto_ik: true,
                pre_subtract_frames: None,
            });
            continue;
        }
        if anim_index.contains_key(a.name.as_str()) {
            seq_errors.push(CompileError::Message {
                at: format!("{at}.name"),
                message: crate::tr_fmt!(
                    "the animation name %{name} is duplicated",
                    name = format!("{:?}", a.name)
                ),
            });
            continue;
        }
        let p = match resolve_smd_path(base_dir, &a.smd) {
            Ok(p) => p,
            Err(msg) => {
                seq_errors.push(e(format!("{at}.smd"), msg));
                continue;
            }
        };
        // ⚠️ 官方的 FBX「同文件静默 1 帧」陷阱 —— 见 `same_fbx_geometry_note`。
        // 判据要 `n_frames`，所以先读帧再判（读一个 FBX 两次的代价可接受：
        // 只有「同文件」这一种情形会走到第二次）。
        //
        // `srcstack` / `srcfps` 是**动画侧**的选项，所以基准是
        // `SrcOpts::default()`（几何侧的 `srcpart` 之类对动画源无意义）。
        let mut aopts = SrcOpts::default()
            .with_anim(a.src_stack.as_deref(), a.src_fps)
            .with_scale(a.scale)
            .with_root_transform(a.adjust, a.rotation);
        inherit_automatic_axes(&mut aopts, desc, base_dir, &p);
        let loaded = load_smd_frames(
            &p,
            desc,
            &bone_index,
            &format!("{at}.smd"),
            &aopts,
            &mut seq_errors,
        );
        let Some((mut frames, _smd)) = loaded else {
            continue;
        };
        // ---- 取帧区间（QC 的 `frames a b`，闭区间）----
        //
        // ⚠️ **必须在 `same_fbx_geometry_note` 之前**：那个提示看的是「最终采到
        // 几帧」，而 `frames 0 0` 正是「我只要 1 帧」的合法表达 —— 放在之前会
        // 把「只要 1 帧」误报成「有分歧」。
        if let Err(error) = select_source_frames(&mut frames, a.frames, &format!("{at}.frames")) {
            seq_errors.push(error);
            continue;
        }
        // ⚠️ 官方的 FBX「同文件静默 1 帧」陷阱 —— 见 `same_fbx_geometry_note`。
        // 判据要 `n_frames`，所以先读帧（并应用 `frames` 区间）再判。
        // ✅ **只提示、不拒绝**：FBX 本来就是网格与动画合一的容器。
        if let Some(msg) = same_fbx_geometry_note(&p, &fbx_geometry, frames.len()) {
            log::info!("{msg}");
        }
        reserve_animation_poses(
            &mut retained_animation_poses,
            frames.len(),
            desc.bones.len(),
        )
        .map_err(|message| vec![e(format!("{at}.smd"), message)])?;
        pending_subtract.push(
            a.subtract
                .as_deref()
                .filter(|_| {
                    !a.cmds
                        .iter()
                        .any(|cmd| matches!(cmd, crate::model::AnimCmd::Subtract { .. }))
                })
                .map(|s| (s.to_owned(), a.subtract_frame.unwrap_or(0))),
        );
        anim_index.insert(a.name.as_str(), anims.len());
        anim_weights.push(weights_of(a.weight_list.as_deref()));
        anims.push(crate::model::CompiledAnimation {
            local_hierarchy: Vec::new(),
            weights: Vec::new(),
            loop_restart: a.loop_restart,
            fudge_loop: a.fudge_loop,
            extra_flags: a.extra_flags,
            movements: Vec::new(),
            name: a.name.clone(),
            smd_path: p,
            fps: a.fps.unwrap_or(30.0),
            looping: a.looping,
            frames,
            // `subtract` 会让 `ProcessIKRules` 置 `STUDIO_DELTA`
            // （`simplify.cpp:163-166`），那又**抑制自动补的 IK 规则**。
            delta: a.subtract.is_some()
                && !a
                    .cmds
                    .iter()
                    .any(|cmd| matches!(cmd, crate::model::AnimCmd::Subtract { .. })),
            ik_rules: a.ik_rules.clone(),
            no_auto_ik: a.no_auto_ik,
            // 由下面的 `subtract` 循环填。
            pre_subtract_frames: None,
        });
    }
    // ---- 减除参考（QC 的 `subtract "x" f`）----
    //
    // 旋转走 `QuaternionSM(-1, src, dest)`，即 **`(−1·src) · dest`**
    // （`simplify.cpp:1102-1107`，`subtract` 自带 `STUDIO_POST`）。
    //
    // 同时把**减除前**的帧留一份（`pre_subtract`）—— 官方包围盒用的是
    // 未减除的姿态，见 `CompiledSequence::pre_subtract_frames`。
    let mut pre_subtract: Vec<Option<Vec<Vec<crate::smd::SmdPose>>>> = vec![None; anims.len()];
    for (i, sub) in pending_subtract.iter().enumerate() {
        let Some((ref_name, ref_frame)) = sub else {
            continue;
        };
        // ⚠️ **参考名可以是「序列名」，不只是「动画名」。**
        //
        // 官方 `LookupAnimation`（`studiomdl.cpp:1674-1692`）**先查动画池
        // `g_panimation`、查不到再退回 `LookupSequence`**，命中序列时返回
        // 它的 `panim[0][0]`（第一格动画）。`subtract` 走的正是它
        // （`studiomdl.cpp:1733-1751` 的 `else if (stricmp("subtract", token) == 0)`）。
        //
        // 用户工程的 `$animation "a_proportions" "anims/foot_fix.smd"
        // subtract "reference" 0` 里，`reference` 就是 QC 上一行那条
        // `$sequence "reference" "anims/ref.smd" fps 1` 的**序列名** ——
        // 官方接受（实测见 `docs/_probe/oracle_subtract_seqname.js`）。
        //
        // mdlc 是**分阶段**编译的（先全部 `$animation`、再全部 `$sequence`），
        // 而官方是**流式**解析 QC ⟹ 动画阶段 `anims` 里还没有序列建的隐含
        // 动画。所以这里手工补上「序列池」那一半：命中 `desc.sequences`
        // 里的某条序列时，参考帧 = 它的第一格动画（`panim[0][0]`）的帧。
        //
        // * 该序列的 token 指向一条**已声明动画** ⟹ 复用它的帧；
        // * 否则那是 `Cmd_ImpliedAnimation` 建的**隐含动画**，其帧就是
        //   那个 SMD 的帧 ⟹ **现场读一遍 SMD**（不建动画对象，序列阶段
        //   会自己建；同一份 SMD 因此被读两次，但 `subtract` 引用序列名
        //   是罕见写法，代价可接受）。
        //
        // 已知的宽松处：官方在**流式**解析时解析名字，所以被引用的序列
        // 必须**写在前面**；mdlc 分阶段后拿不到「书写先后」，这里对
        // `desc.sequences` 里**任意位置**的序列都接受。这会比官方**宽松**
        // （官方对「引用了后面的序列」会报 `unknown subtract animation`）。
        let resolved: Option<Vec<Vec<crate::smd::SmdPose>>> =
            if let Some(&j) = anim_index.get(ref_name.as_str()) {
                Some(anims[j].frames.clone())
            } else {
                match desc
                    .sequences
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(ref_name))
                {
                    // `$declaresequence` 的空壳没有 `panim` ⟹ 官方
                    // `pseq->panim[0][0]` 是 NULL ⟹ 解析失败。
                    Some(sq) if sq.forward_declared => None,
                    Some(sq) => {
                        // `pseq->panim[0][0]`：blend 序列取**第一格**的名字；
                        // 单动画序列取它的 token（`sq.smd`）。
                        let cell = sq
                            .blends
                            .first()
                            .map(|n| n.as_str())
                            .unwrap_or(sq.smd.as_str());
                        // 现场读的这条源属于**这个序列**，所以动画侧选项取它的。
                        let sopts = SrcOpts::default()
                            .with_anim(sq.src_stack.as_deref(), sq.src_fps)
                            .with_scale(sq.scale.or(sq.input_scale))
                            .with_root_transform(sq.adjust, sq.rotation);
                        match anim_index.get(cell) {
                            Some(&j) => Some(anims[j].frames.clone()),
                            None => match resolve_smd_path(base_dir, &sq.smd) {
                                Ok(p) => load_smd_frames(
                                    &p,
                                    desc,
                                    &bone_index,
                                    &crate::tr_fmt!(
                                        "animations[%{i}].subtract (sequence %{name})",
                                        i = i,
                                        name = format!("{ref_name:?}")
                                    ),
                                    &sopts,
                                    &mut seq_errors,
                                )
                                .and_then(|(mut frames, _smd)| {
                                    match select_source_frames(
                                        &mut frames,
                                        sq.frames,
                                        &format!("sequences[{}].frames", sq.name),
                                    ) {
                                        Ok(()) => Some(frames),
                                        Err(error) => {
                                            seq_errors.push(error);
                                            None
                                        }
                                    }
                                }),
                                Err(msg) => {
                                    seq_errors.push(e(
                                        crate::tr_fmt!(
                                            "animations[%{i}].subtract (sequence %{name})",
                                            i = i,
                                            name = format!("{ref_name:?}")
                                        ),
                                        msg,
                                    ));
                                    None
                                }
                            },
                        }
                    }
                    None => None,
                }
            };
        let Some(src) = resolved else {
            seq_errors.push(CompileError::Message {
                at: format!("animations[{i}].subtract"),
                message: crate::tr_fmt!(
                    "cannot find the reference animation %{name} (subtract takes an **animation or sequence name**; \
                     existing animations: %{anims}; existing sequences: %{seqs})",
                    name = format!("{ref_name:?}"),
                    anims = desc.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
                    seqs = desc.sequences.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
            });
            continue;
        };
        let bf = (*ref_frame).max(0) as usize;
        if bf >= src.len() {
            seq_errors.push(CompileError::Message {
                at: format!("animations[{i}].subtract_frame"),
                message: crate::tr_fmt!(
                    "the reference animation %{name} only has %{n} frames; frame %{frame} is out of reach",
                    name = format!("{ref_name:?}"),
                    n = src.len(),
                    frame = bf
                ),
            });
            continue;
        }
        // ⚠️ **先把减除前的帧留一份**给包围盒用（见
        // `CompiledAnimation::pre_subtract_frames` 的说明）。
        reserve_animation_poses(
            &mut retained_animation_poses,
            anims[i]
                .frames
                .len()
                .checked_mul(2)
                .ok_or_else(|| vec![e("animations", "Animation frame count overflows")])?,
            desc.bones.len(),
        )
        .map_err(|message| vec![e(format!("animations[{i}].subtract"), message)])?;
        let before = anims[i].frames.clone();
        anims[i].pre_subtract_frames = Some(before.clone());
        pre_subtract[i] = Some(before);
        // 权重来自 `$weightlist`；`weight[k] <= 0` 的骨骼**不减除**
        // （`simplify.cpp:1088` 的 `if (pdest->weight[k] > 0)`）。
        subtract_base_frames(&mut anims[i].frames, &src, bf, &anim_weights[i]);
    }
    if !seq_errors.is_empty() {
        return Err(seq_errors);
    }

    // ---- 序列：读每个序列的 SMD，取全部帧 ----
    // `sequences` 要在块外声明：它活到 `:2675` 的段表构造和 `:2707` 的
    // `CompiledModelDesc` 组装，`measure_block!` 会引入新作用域。
    let mut sequences = Vec::with_capacity(desc.sequences.len());
    hotpath::measure_block!("sequences: 读 SMD 帧", {
        for (si, s) in desc.sequences.iter().enumerate() {
            let at = format!("sequences[{si}]");
            // ---- `$declaresequence`：前向声明的**空壳** ----
            //
            // 官方 `Cmd_DeclareSequence` 只 `memset` 一条 `s_sequence_t` 再置
            // `STUDIO_OVERRIDE`，**不分配 `panim`、不读 SMD**。
            // 所以这里**必须**在所有「读 SMD / 解析动画」之前短路 ——
            // 否则会去读一个空路径。
            //
            // 落盘值与普通序列**处处不同**，但那不是特例代码，而是
            // 「`memset` 之后一个字段都没被赋值」的自然结果。实测表见
            // [`crate::model::Sequence::forward_declared`]。
            if s.forward_declared {
                sequences.push(crate::model::CompiledSequence {
                    key_values: s.key_values.clone(),
                    name: s.name.clone(),
                    smd_path: std::path::PathBuf::new(),
                    // 官方 `memset` 后 `fps = 0`（普通序列在 `Cmd_Sequence`
                    // 里才被设成 30）。它不落盘，但保持一致以免误导。
                    fps: 0.0,
                    looping: false,
                    // ⚠️ 这两个是**空壳与普通序列差别最大的地方**：
                    // 普通序列 `activity = -1`、`fade = 0.2`，
                    // 空壳全是 `memset` 的 0。
                    activity: 0,
                    activity_name: String::new(),
                    activity_weight: 0,
                    delta: false,
                    frames: Vec::new(),
                    // `groupsize = [0, 0]` ⟹ `cells` 空 ⟹ 写出器走空壳分支。
                    cells: Vec::new(),
                    blend_width: 0,
                    blend_params: [None, None],
                    // `groupsize = [0,0]` ⟹ `CalcPoseParameters` 的
                    // `groupsize[iPose] > 1` 不成立 ⟹ 没有 calc 轴。
                    calc_axes: Vec::new(),
                    blend_ref: None,
                    blend_comp: None,
                    blend_center: None,
                    transition: None,
                    transition_reversible: false,
                    auto_layers: Vec::new(),
                    events: Vec::new(),
                    fade_in: 0.0,
                    fade_out: 0.0,
                    forward_declared: true,
                    no_auto_ik: false,
                    ik_rules: Vec::new(),
                    iklocks: Vec::new(),
                    movements: Vec::new(),
                    section_frames: 0,
                    num_sections: 0,
                    // ⚠️ **全 0，不是全 1** —— 见 `merge_weights` 的说明：
                    // `groupsize = [0,0]` 让官方的 MAX 循环一次都不跑。
                    weights: vec![0.0; n_bones],
                    pre_subtract_frames: None,
                    extra_flags: None,
                });
                continue;
            }

            // ---- blend 网格（`$sequence` 块里写了多个动画名）----
            //
            // ⚠️ **格子只是引用**，不是「每格一个 animdesc」。
            // 官方把 animdesc 放在全局池 `g_panimation[]` 里，`$sequence`
            // 里的裸名字先按名字查池（`studiomdl.cpp:2952-2959`），查到就
            // **复用同一个 animdesc**。实测 `v_autoshotgun.mdl`：27 个
            // seqdesc / 29 个 animdesc —— `idle` 的两格 `a_run` 共用一份，
            // `idle` 与 `idle_raw` 的 `a_idle` 共用一份。
            //
            // 所以这里只记录**动画下标**，帧数据在下面的动画池里统一建。
            // 单动画序列走下面的老路径（`s.smd`）。
            if !s.blends.is_empty() {
                let (width, height) = match blend_grid_size(s, &at, &mut seq_errors) {
                    Some(v) => v,
                    None => continue,
                };
                // 名字 → 动画池下标。查不到就报错（官方的「隐含动画」由
                // 单动画序列那条路径覆盖，blend 的格子必须已声明）。
                let mut cell_idx = Vec::with_capacity(s.blends.len());
                let mut ok = true;
                for (ci, nm) in s.blends.iter().enumerate() {
                    match anim_index.get(nm.as_str()) {
                        Some(&i) => cell_idx.push(i),
                        None => {
                            seq_errors.push(e(
                                format!("{at}.blends[{ci}]"),
                                crate::tr_fmt!(
                                    "cannot find the animation %{name} (every blend cell must be a name declared in [[animations]]; \
                                     existing: %{anims})",
                                    name = format!("{nm:?}"),
                                    anims = desc.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
                                ),
                            ));
                            ok = false;
                        }
                    }
                }
                if !ok {
                    continue;
                }
                let _ = (width, height);

                // 参数轴：名字 → 下标，并算逐格取值。
                let mut params: [Option<crate::model::CompiledBlendParam>; 2] = [None, None];
                let grid = [width as usize, height as usize];
                let mut bad = false;
                for (pi, bp) in s.blend_params.iter().take(2).enumerate() {
                    let idx = match resolve_pose_param_index(desc, &bp.parameter) {
                        Some(i) => i,
                        None => {
                            seq_errors.push(e(
                                format!("{at}.blend_params[{pi}].parameter"),
                                crate::tr_fmt!(
                                    "cannot find the pose parameter %{name} ([[model.pose_parameters]] has %{n}: %{list})",
                                    name = format!("{:?}", bp.parameter),
                                    n = desc.model.pose_parameters.len(),
                                    list = desc.model
                                        .pose_parameters
                                        .iter()
                                        .map(|p| p.name.as_str())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ),
                            ));
                            bad = true;
                            continue;
                        }
                    };
                    if let Some(att) = &bp.attachment {
                        // `calcblend` 轴：`paramstart`/`paramend`/`posekey` **全是
                        // 算出来的**，这里只占位（真值由
                        // [`apply_calc_blend_axes`] 在序列全部建完后填）。
                        //
                        // ⚠️ 附着点名字**必须在编译期解析**（官方在解析期
                        // `LookupAttachment`，查不到就 `TokenError`），
                        // 所以这里就查一次，查不到直接报错。
                        //
                        // 这里只要**存在性**（`local` 要等骨骼姿态定下来才有意义），
                        // 所以用 [`find_attachment`]。
                        if find_attachment(desc, &bone_index, att).is_none() {
                            seq_errors.push(e(
                                format!("{at}.blend_params[{pi}].attachment"),
                                crate::tr_fmt!(
                                    "unknown calcblend attachment point %{name} (the official code says Unknown calcblend attachment)",
                                    name = format!("{att:?}")
                                ),
                            ));
                            bad = true;
                            continue;
                        }
                        params[pi] = Some(crate::model::CompiledBlendParam {
                            parameter_index: idx,
                            start: 0.0,
                            end: 0.0,
                            keys: Vec::new(),
                        });
                    } else {
                        params[pi] = Some(crate::model::CompiledBlendParam {
                            parameter_index: idx,
                            start: bp.start,
                            end: bp.end,
                            keys: blend_param_keys(bp.start, bp.end, grid[pi]),
                        });
                    }
                }
                if bad {
                    continue;
                }

                // ---- 官方 `CalcPoseParameters` 会遍历**每一根轴** ----
                //
                // 判据（`simplify.cpp:5461-5463`）是**两个条件**：
                //
                // ```c
                // if (pseq->groupsize[iPose] > 1) {
                //     if (pseq->paramattachment[iPose] != -1) { /* calc 分支 */ }
                //     else { /* 线性插值：param_i[m] = start*(1-f) + end*f */ }
                // }
                // ```
                //
                // ⚠️ 循环边界是 `groupsize`，**不是**「QC 写了几个
                // `blend`/`calcblend`」。所以 `blendwidth 3` 配 0 个 `blend` 时
                // 轴 0 照样被遍历 —— 而它的 `paramattachment[0]` 是 `memset`
                // 残留的 **0**（`≠ -1`）⟹ 进入 **calc 分支**
                // ⟹ `paramcontrol[0]` 同样是 0 ⟹ 每格算出 `0.0`
                // ⟹ `calcblend failed`（`simplify.cpp:5566-5569`）。
                //
                // 这条路径**必须复刻**，否则 mdlc 会对官方拒绝的 QC
                // 静默产出与官方不同的产物（实测：`idle` 的 `paramindex`
                // 官方 `[0,-1]` / mdlc `[-1,-1]`，且引擎侧 `move_x` 完全失效）。
                //
                // 三种轴的归属：
                //
                // | 轴的状态 | `paramattachment` | 走哪条分支 |
                // |---|---|---|
                // | 写了 `calcblend` | 附着点下标 | **calc** |
                // | 写了 `blend` | **-1**（`:2742`） | 线性插值 |
                // | **两个都没写** | **0**（`memset`） | **calc**（恒 0 ⟹ 报错） |
                let mut calc_axes: Vec<crate::model::CompiledCalcAxis> = Vec::new();
                for (axis, &gs) in grid.iter().enumerate() {
                    if gs <= 1 {
                        continue;
                    }
                    let declared = s.blend_params.get(axis);
                    // 纯 `blend` 轴（声明了但没附着点）走线性插值，**不进** calc。
                    let is_pure_blend = matches!(declared, Some(bp) if bp.attachment.is_none());
                    if is_pure_blend {
                        continue;
                    }
                    calc_axes.push(crate::model::CompiledCalcAxis {
                        axis,
                        // `None` = 官方 `memset` 残留那条路径（轴根本没声明）——
                        // 此时控制轴也是 0，所以结果与「哪个附着点」无关。
                        attachment: declared.and_then(|bp| bp.attachment.clone()),
                        control: declared.and_then(|bp| bp.control.clone()),
                    });
                }

                // 自动层：序列名 → 下标。时间量在这里就转成落盘值。
                //
                // 帧数取**本序列第一格**的（官方用 `panim[0][0]->numframes`，
                // `write.cpp:541-544`）。
                let nf = anims[cell_idx[0]].frames.len().max(1) as f32;
                let mut auto_layers = Vec::with_capacity(s.auto_layers.len());
                for (li, al) in s.auto_layers.iter().enumerate() {
                    let Some(seq_idx) = desc
                        .sequences
                        .iter()
                        .position(|x| x.name.eq_ignore_ascii_case(&al.sequence))
                    else {
                        seq_errors.push(e(
                            format!("{at}.auto_layers[{li}].sequence"),
                            crate::tr_fmt!(
                                "cannot find the sequence %{name}",
                                name = format!("{:?}", al.sequence)
                            ),
                        ));
                        bad = true;
                        continue;
                    };
                    auto_layers.push(crate::model::CompiledAutoLayer {
                        sequence: seq_idx as i16,
                        pose: al.pose,
                        flags: al.flags,
                        // `write.cpp:539-551`：不带 `STUDIO_AL_POSE` 时
                        // 四个量**除以 `numframes − 1`**（转 cycle）。
                        start: layer_time(al.start, al.flags, nf),
                        peak: layer_time(al.peak, al.flags, nf),
                        tail: layer_time(al.tail, al.flags, nf),
                        end: layer_time(al.end, al.flags, nf),
                    });
                }
                if bad {
                    continue;
                }

                // `$sequence` 块里的 `ikrule` 属于**第一格动画**（R23），见
                // [`sequence_ik_rules_attach_to_first_cell`] 的说明。
                if let Some(&first_cell) = cell_idx.first() {
                    anims[first_cell]
                        .ik_rules
                        .extend(s.ik_rules.iter().cloned());
                }

                reserve_animation_poses(
                    &mut retained_animation_poses,
                    anims[cell_idx[0]].frames.len(),
                    desc.bones.len(),
                )
                .map_err(|message| vec![e(format!("{at}.frames"), message)])?;
                let first = anims[cell_idx[0]].frames.clone();
                let nf_i = first.len() as i32;
                let sec_len = s.section_frames.unwrap_or(DEFAULT_SECTION_FRAMES);
                let sec_thr = s.section_threshold.unwrap_or(DEFAULT_SECTION_THRESHOLD);
                // `blendcenter` 在网格里的位置（`simplify.cpp:5503-5517`）：
                // 逐格比 animdesc 指针，命中就记下 `(i0, i1)`。
                //
                // ⚠️ 官方比的是**动画对象指针**，所以「同一格被引用两次」时
                // 取**先命中的那个**（双层循环 `i0` 外层、`i1` 内层）。
                let blend_center = s.blend_center.as_ref().and_then(|name| {
                    let want = resolve_lookup_animation(name, &anims, &anim_index, &sequences)?;
                    (0..grid[1]).find_map(|k| {
                        (0..grid[0])
                            .find(|&j| cell_idx.get(k * grid[0] + j) == Some(&want))
                            .map(|j| [j, k])
                    })
                });
                sequences.push(crate::model::CompiledSequence {
                    key_values: s.key_values.clone(),
                    name: s.name.clone(),
                    smd_path: anims[cell_idx[0]].smd_path.clone(),
                    fps: s.fps.unwrap_or(30.0),
                    looping: s.looping,
                    activity: -1,
                    activity_name: s.activity.clone().unwrap_or_default(),
                    activity_weight: s.activity_weight,
                    delta: s.delta,
                    frames: first,
                    cells: cell_idx.clone(),
                    blend_width: width,
                    blend_params: params,
                    calc_axes,
                    blend_ref: s.blend_ref.clone(),
                    blend_comp: s.blend_comp.clone(),
                    blend_center,
                    transition: s.transition.clone(),
                    transition_reversible: s.transition_reversible,
                    auto_layers,
                    events: s.events.clone(),
                    fade_in: s.fade_in,
                    fade_out: s.fade_out,
                    no_auto_ik: s.no_auto_ik,
                    ik_rules: s.ik_rules.clone(),
                    iklocks: s.iklocks.clone(),
                    movements: s.movements.clone(),
                    section_frames: if sec_len > 0 && nf_i >= sec_thr {
                        sec_len
                    } else {
                        0
                    },
                    num_sections: 0,
                    // 本序列的权重 = 各格动画**逐骨骼取 MAX**（`simplify.cpp:302-318`）。
                    weights: merge_weights(&cell_idx, &anim_weights, desc.bones.len()),
                    // 取**第一格**减除前的帧（官方按格取，但 blend 的每一格
                    // 都是独立的 animdesc，这里 `first` 也是第一格的）。
                    pre_subtract_frames: pre_subtract.get(cell_idx[0]).and_then(|p| p.clone()),
                    extra_flags: s.extra_flags,
                    forward_declared: false,
                });
                continue;
            }

            // ---- 单动画序列：隐含动画 ----
            //
            // QC 里 `$sequence "reload" "reload.smd"` 与 `$sequence "x" "a_idle"`
            // 是**同一个语法**：块里给的是一个**名字**（token）。官方先按
            // 这个名字查 `$animation` 池（`studiomdl.cpp:2952-2959`）——
            //
            // * 查到 ⟹ **复用**那个 animdesc（不新建）；
            // * 查不到 ⟹ `Cmd_ImpliedAnimation` 新建一个，名字加 `@` 前缀。
            //
            // ⚠️ **隐含动画登记在 `@序列名` 下，不是文件名下。**
            // 所以 `$sequence "reload" "reload.smd"` 与
            // `$sequence "reload_layer" "reload.smd"` 是**两条独立动画**
            // （`@reload` / `@reload_layer`）—— 后者的 token 再去查池时
            // 找不到 `reload.smd`（池里没有这个名字），于是又建一个。
            // 早先本实现按**文件名**登记，于是这两条被错误地并成一条
            // （实测 `numlocalanim` 17 vs 官方 29）。
            // ⚠️ **路径解析必须留在 `None` 臂里** —— `s.smd` 既可能是
            // 文件路径，也可能是**动画名**（`$sequence "base" "a_base"`，
            // 见 `parity\blend1.toml`）。查池命中时根本不读文件，
            // 而 `a_base` 这种动画名没有扩展名 ⟹ 提前解析会误报。
            let by_name = anim_index.get(s.smd.as_str()).copied();
            let reused = by_name.is_some();
            let (anim_ix, mut frames) = match by_name {
                Some(i) => {
                    reserve_animation_poses(
                        &mut retained_animation_poses,
                        anims[i].frames.len(),
                        desc.bones.len(),
                    )
                    .map_err(|message| vec![e(format!("{at}.frames"), message)])?;
                    (i, anims[i].frames.clone())
                }
                None => {
                    let smd_path = match resolve_smd_path(base_dir, &s.smd) {
                        Ok(p) => p,
                        Err(msg) => {
                            seq_errors.push(e(format!("{at}.smd"), msg));
                            continue;
                        }
                    };
                    // 隐含动画的源就是**序列自己**的源，所以动画侧选项取序列的。
                    let mut sopts = SrcOpts::default()
                        .with_anim(s.src_stack.as_deref(), s.src_fps)
                        .with_scale(s.scale.or(s.input_scale))
                        .with_root_transform(s.adjust, s.rotation);
                    inherit_automatic_axes(&mut sopts, desc, base_dir, &smd_path);
                    let Some((mut frames, _smd)) = load_smd_frames(
                        &smd_path,
                        desc,
                        &bone_index,
                        &format!("{at}.smd"),
                        &sopts,
                        &mut seq_errors,
                    ) else {
                        continue;
                    };
                    if let Err(error) =
                        select_source_frames(&mut frames, s.frames, &format!("{at}.frames"))
                    {
                        seq_errors.push(error);
                        continue;
                    }
                    // ⚠️ 官方的 FBX「同文件静默 1 帧」陷阱（SMD 不受影响）。
                    // 判据带帧数：只有真的会丢帧（> 1 帧）才提示。
                    // ✅ **只提示、不拒绝** —— 见 `same_fbx_geometry_note`。
                    if let Some(msg) =
                        same_fbx_geometry_note(&smd_path, &fbx_geometry, frames.len())
                    {
                        log::info!("{msg}");
                    }
                    reserve_animation_poses(
                        &mut retained_animation_poses,
                        frames
                            .len()
                            .checked_mul(2)
                            .ok_or_else(|| vec![e(&at, "Animation frame count overflows")])?,
                        desc.bones.len(),
                    )
                    .map_err(|message| vec![e(format!("{at}.frames"), message)])?;
                    let i = anims.len();
                    let name = format!("@{}", s.name);
                    // 登记在**动画名**下，与官方一致。
                    anim_index.insert(Box::leak(name.clone().into_boxed_str()), i);
                    // 隐含动画的权重取**序列**的 `weightlist`
                    // （官方 `Cmd_ImpliedAnimation` 建完动画后，序列的
                    // `cmds[]` 里的 `CMD_WEIGHTS` 会作用到它）。
                    anim_weights.push(weights_of(s.weight_list.as_deref()));
                    anims.push(crate::model::CompiledAnimation {
                        local_hierarchy: Vec::new(),
                        weights: Vec::new(),
                        loop_restart: s.loop_restart,
                        fudge_loop: s.fudge_loop,
                        extra_flags: 0,
                        movements: Vec::new(),
                        name,
                        smd_path: smd_path.clone(),
                        fps: s.fps.unwrap_or(30.0),
                        looping: s.looping,
                        frames: frames.clone(),
                        delta: false,
                        ik_rules: s.ik_rules.clone(),
                        no_auto_ik: s.no_auto_ik,
                        pre_subtract_frames: None,
                    });
                    (i, frames)
                }
            };

            // `$sequence` 块里的 `ikrule` 属于**第一格动画**（R23）。
            //
            // 隐含动画在上面构造时已经把 `s.ik_rules` 放进去了；只有**复用**
            // 已声明动画（`$sequence "reload" "a_reload"`）时才需要补 ——
            // 那种写法下规则在 `$animation` 块里没有，只能从序列搬过来。
            if reused {
                anims[anim_ix].ik_rules.extend(s.ik_rules.iter().cloned());
            }

            // ---- `$sequence` 块里的 `weightlist`（`CMD_WEIGHTS`）----
            //
            // 与 `ikrule`（R23）**同一个机制**：官方 `ParseSequence` 把序列块里的
            // 「动画选项」交给 `ParseAnimationToken(animations[0])`
            // （`studiomdl.cpp:2944`），而 `weightlist` 只由 `ParseCmdlistToken`
            // 处理（`studiomdl.cpp:1714-1732`）⟹ 它落成
            // **`animations[0]->cmds[]` 里的一条 `CMD_WEIGHTS`**，
            // 由 `processAnimations`（`simplify.cpp:154-166`）执行
            // `setAnimationWeight(panim, index)` —— **作用在动画对象上**。
            //
            // 隐含动画那条路径（`!reused`）已经在上面把 `s.weight_list` 用掉了；
            // **复用**已声明动画时（`$sequence "fidget" "a_look_mid" weightlist "empty"`）
            // 必须**覆盖**那个共享动画的权重 —— 官方就是改共享对象。
            //
            // ⚠️ 这不是「序列自己的权重」：序列的 `weight[]` 是
            // `merge_weights`（`simplify.cpp:302-318`）**对各格取 MAX** 得来的。
            // 所以 `fidget`（单格 = `a_look_mid`）得到**全 0**，而共用
            // `a_look_mid` 的 `look_poses`（三格）取 MAX 后仍是**全 1** ——
            // 实测 NekoMDL 与发布版**都是这个形态**（`fidget` 全 0、
            // `look_poses` 全非零），而 mdlc 修前两边都给全 1。
            //
            // 早先只在 `!reused` 分支处理 ⟹ `fidget` / `fidget_layer` 的
            // `weightlist "empty"` **被静默丢弃**。
            if s.weight_list.is_some() {
                anim_weights[anim_ix] = weights_of(s.weight_list.as_deref());
            }

            // ---- `$sequence` 块里的 `numframes <N>`（`CMD_NUMFRAMES`）----
            //
            // 同一条机制：官方把它落成 `animations[0]->cmds[]` 里的
            // `CMD_NUMFRAMES`，由 `processAnimations`（`simplify.cpp:248-252`）
            // 执行 `forceNumframes(panim, frames)`。
            //
            // 官方实现（`simplify.cpp:1279-1293`）：
            // ```c
            // for (j = panim->numframes; j < numframes; j++) {
            //     panim->sanim[j] = kalloc(1, size);
            //     memcpy( panim->sanim[j], panim->sanim[panim->numframes-1], size );
            // }
            // panim->numframes = numframes;
            // ```
            // ⟹ **只延长，不缩短**：把**最后一帧**复制到 `numframes` 为止。
            // 而且它改的是**共享的动画对象** —— 所以 `$sequence "fidget" "a_look_mid"
            // … numframes 90` 会把 `a_look_mid` 本身变成 90 帧，
            // 引用同一个 `a_look_mid` 的 `look_poses` 也**跟着变成 90 帧**
            // （实测 NekoMDL 与发布版都是这个形态：`look_poses` 的 blend 表里
            //  `a_look_mid` 的 nf = 90）。
            //
            // 早先 mdlc **完全没实现** `CMD_NUMFRAMES` —— QC 解析器把值读进
            // `Sequence::num_frames` 后只用于事件 cycle 的换算
            // （`qc/parse.rs` 的 `nf`），动画帧数**从未被改过** ⟹
            // `a_look_mid` 停在 1 帧（`look_poses.smd` 只有 3 帧，
            // `frames 1 1` 取第 1 帧），而 `fidget` 序列期望 90 帧。
            //
            // ⚠️ 必须**同时**改共享动画与本地副本 —— 只改本地 `frames` 会让
            // 「共享」这件事丢掉（`look_poses` 仍是 1 帧），只改 `anims[]`
            // 则本序列拿到的还是旧副本。
            // ---- `$sequence` 块里的 `numframes <N>`（`CMD_NUMFRAMES`）----
            //
            // 同一条机制：官方把它落成 `animations[0]->cmds[]` 里的
            // `CMD_NUMFRAMES`，由 `processAnimations`（`simplify.cpp:248-252`）
            // 执行 `forceNumframes(panim, frames)`。
            //
            // 官方实现（`simplify.cpp:1279-1293`）：
            // ```c
            // for (j = panim->numframes; j < numframes; j++) {
            //     panim->sanim[j] = kalloc(1, size);
            //     memcpy( panim->sanim[j], panim->sanim[panim->numframes-1], size );
            // }
            // panim->numframes = numframes;
            // ```
            // ⟹ **只延长，不缩短**：把**最后一帧**复制到 `numframes` 为止。
            // 而且它改的是**共享的动画对象** —— 所以 `$sequence "fidget" "a_look_mid"
            // … numframes 90` 会把 `a_look_mid` 本身变成 90 帧，
            // 引用同一个 `a_look_mid` 的 `look_poses` 也**跟着变成 90 帧**
            // （实测 NekoMDL 与发布版都是这个形态：`look_poses` 的 blend 表里
            //  `a_look_mid` 的 nf = 90）。
            //
            // 早先 mdlc **完全没实现** `CMD_NUMFRAMES` —— QC 解析器把值读进
            // `Sequence::num_frames` 后只用于事件 cycle 的换算
            // （`qc/parse.rs` 的 `nf`），动画帧数**从未被改过** ⟹
            // `a_look_mid` 停在 1 帧（`look_poses.smd` 只有 3 帧，
            // `frames 1 1` 取第 1 帧），而 `fidget` 序列期望 90 帧。
            //
            // ⚠️ 必须**同时**改共享动画与本地副本 —— 只改本地 `frames` 会让
            // 「共享」这件事丢掉（`look_poses` 仍是 1 帧），只改 `anims[]`
            // 则本序列拿到的还是旧副本。
            if let Some(n) = s.num_frames.filter(|_| {
                !s.cmds
                    .iter()
                    .any(|cmd| matches!(cmd, crate::model::AnimCmd::NumFrames { .. }))
            }) {
                let n = n.max(0) as usize;
                if n.checked_mul(desc.bones.len())
                    .is_none_or(|poses| poses > 2_000_000)
                {
                    return Err(vec![e(
                        format!("{at}.numframes"),
                        "numframes exceeds the animation pose limit",
                    )]);
                }
                if n > 0 && n > anims[anim_ix].frames.len() {
                    reserve_animation_poses(
                        &mut retained_animation_poses,
                        (n - anims[anim_ix].frames.len()) * 2,
                        desc.bones.len(),
                    )
                    .map_err(|message| vec![e(format!("{at}.numframes"), message)])?;
                    let last = anims[anim_ix].frames.last().cloned();
                    if let Some(last) = last {
                        while anims[anim_ix].frames.len() < n {
                            anims[anim_ix].frames.push(last.clone());
                        }
                    }
                }
            }
            // 本序列的帧副本与共享动画保持一致（`forceNumframes` 只延长）。
            if s.num_frames.is_some() {
                frames = anims[anim_ix].frames.clone();
            }

            // ---- `$sequence` 块里的 `subtract`（`CMD_SUBTRACT`）----
            //
            // 官方 `ParseSequence` 在 `numblends || isAppend` 时把 token 交给
            // `ParseAnimationToken(animations[0])`（`studiomdl.cpp:2944`），所以
            // `subtract` 在 `$sequence` 里**同样合法**，且作用对象是
            // `animations[0]` 那个**动画**（cmds 挂在 panim 上）。
            //
            // 本实现把减除作用在**本序列自己的帧副本**上，而不是去改共享的
            // `anims[j].frames` —— 官方那样会让「同一个动画被两条序列引用」时
            // 互相污染（减除被叠加两次）。后者在本工程里观测不到（每条
            // `*_layer` 序列各有独立动画），但改共享状态是更差的选择。
            let mut seq_pre_subtract: Option<Vec<Vec<crate::smd::SmdPose>>> = None;
            // ⚠️ **`$sequence` 的 `delta` / `subtract` 必须把
            // `anims[anim_ix].delta` 也置上** —— 官方是**同一个**标志。
            //
            // 官方 `ParseSequence`（`studiomdl.cpp:2827-2831`）把 `delta` 置到
            // `pseq->flags`（**seqdesc**）；而 `ParseAnimationToken` 的
            // `CMD_SUBTRACT`（`simplify.cpp:163-166`）把 `panim->flags` 置上
            // `STUDIO_DELTA`。**两条路径最终都作用到动画的 `flags`** ——
            // `write.cpp:1013` 是 `panimdesc[i].flags = srcanim->flags`，
            // 而 `anim_writer.rs:3110` 正是照抄这条。
            //
            // # 实测症状（R9）
            //
            // `vm_test_group` 的 6 个官方 viewmodel 里，`@*_layer` 动画：
            // ```text
            //   animdesc.flags   mdlc = 0x000        官方 = 0x004 (STUDIO_DELTA)
            //   seqdesc.flags    mdlc = 0x014        官方 = 0x014   ✅ 已对
            // ```
            // 即 **seqdesc 说「我是增量」，animdesc 却说「我是绝对姿态」** ——
            // 两者矛盾。`.mdl` 体积也因此差 ~16 KB（3/6 个模型）。
            //
            // `delta` 同时驱动**动画数据的编码方式**（`anim_writer.rs:2154/2690/2876`），
            // 所以这不只是标志位不一致 —— **动画数据本身按错误的方式写了**。
            // 这正是用户报的「动画错乱」的一个具体成因。
            //
            // ⚠️ **但 `subtract` 与 `delta` 对 seqdesc 的影响不同**（R11）：
            //
            // | QC 写法 | `animdesc.flags` | `seqdesc.flags` |
            // |---|---|---|
            // | 只有 `subtract` | **有** DELTA | **无** DELTA |
            // | `delta`（可同时有 `subtract`） | 有 DELTA | **有** DELTA |
            //
            // 因为官方两条路径落在**不同对象**上：
            //   * `CMD_SUBTRACT`（`simplify.cpp:163-166`）→ `panim->flags`（**动画**）
            //   * `delta` 关键字（`studiomdl.cpp:2827-2831`）→ `pseq->flags`（**序列**）
            // 而 `write.cpp:436` 是 `pseqdesc->flags = g_sequence[i].flags`、
            // `write.cpp:1013` 是 `panimdesc[i].flags = srcanim->flags` —— 各写各的。
            //
            // 实测（更新后的 `vm_test_group`）：`helping_hand_extend_layer` /
            // `item_extend_layer` 等 6 条只有 `subtract` 没有 `delta`，
            // 官方 seqdesc = `0x000`，而第一版修复给了 `0x004`。
            let seq_is_delta =
                (s.delta || s.extra_flags.unwrap_or(0) & 0x0004 != 0 || s.subtract.is_some())
                    && !s
                        .cmds
                        .iter()
                        .any(|cmd| matches!(cmd, crate::model::AnimCmd::Subtract { .. }));
            if seq_is_delta {
                anims[anim_ix].delta = true;
            }
            if let Some(ref_name) = s.subtract.as_deref().filter(|_| {
                !s.cmds
                    .iter()
                    .any(|cmd| matches!(cmd, crate::model::AnimCmd::Subtract { .. }))
            }) {
                // ⚠️ 与 `[[animations]]` 那条路径**同一个** `LookupAnimation`
                // （`studiomdl.cpp:1674-1692`）：**先查动画池、再退回序列池**，
                // 命中序列时取它的 `panim[0][0]`。这里 `sequences` 里已经有
                // 前面处理过的序列（本序列自己还没 push）—— 与官方「流式解析，
                // 只能看到写在前面、且已进 `g_sequence` 的序列」同序。
                //
                // ⚠️⚠️ **但「自己」必须显式认下**（R31）。官方 `Cmd_Sequence`
                // 在解析体**之前**就把序列 `AddToTail` 进 `g_sequence`
                // （`studiomdl.cpp:2623-2627`）：
                //
                // ```c
                // s_sequence_t *pseq = &g_sequence[ g_sequence.AddToTail() ];  // ← 先入表
                // memset( pseq, 0, sizeof( s_sequence_t ) );
                // strcpyn( pseq->name, token );
                // ...
                // ParseSequence( pseq, false );                               // ← 再解析体
                // ```
                //
                // 所以 `$sequence X ... subtract X 0` 里的
                // `LookupAnimation("X")` 能在**序列池**找到本序列，返回
                // `pseq->panim[0][0]` —— 而那一格此刻**已经建好**（体里第一个
                // 非关键字 token 就走 `Cmd_ImpliedAnimation`
                // 或命中 `g_panimation`，随后 `if (numblends == 1)
                // pseq->panim[0][0] = animations[0];`，`studiomdl.cpp:2964-2967`）。
                //
                // mdlc 的 `sequences` 要到本循环末尾才 push 本序列
                // ⟹ 查不到自己，必须在这里补上。语义上「自引用帧 0」等价于
                // 「把整条动画变成相对第 0 帧的增量」，与官方一致
                // （`subtractBaseAnimations` 先把 `psrc->sanim[srcframe]`
                // 快照进局部 `s_bone_t src[]`，`psrc == pdest` 也安全，
                // `simplify.cpp:1071-1082`）。
                //
                // 实测触发点：用户工程
                // `incap_anim_fix\includes\anims_fix.qci:161`
                // ```text
                // $sequence IncapIdlenoise NamVet_Idle_Standing_01 X Y Z fixuploop -15 15
                //   loop weightlist INJUREDIDLENOISE subtract IncapIdlenoise 0 delta hidden
                // ```
                // （修前报 `sequences[1].subtract: 找不到参考动画 "IncapIdlenoise"`。）
                //
                // 顺序仍与官方一致：`resolve_lookup_animation` 先查动画池
                // （若真有同名动画，它赢），只有没命中时才认自引用 ——
                // 本序列此刻确实**不在** `sequences` 里。
                let target = resolve_lookup_animation(ref_name, &anims, &anim_index, &sequences)
                    .or_else(|| {
                        if ref_name.eq_ignore_ascii_case(&s.name) {
                            Some(anim_ix)
                        } else {
                            None
                        }
                    });
                match target {
                    Some(j) => {
                        let src = anims[j].frames.clone();
                        let bf = s.subtract_frame.unwrap_or(0).max(0) as usize;
                        if bf >= src.len() {
                            seq_errors.push(CompileError::Message {
                                at: format!("{at}.subtract_frame"),
                                message: crate::tr_fmt!(
                                    "the reference animation %{name} only has %{n} frames; frame %{frame} is out of reach",
                                    name = format!("{ref_name:?}"),
                                    n = src.len(),
                                    frame = bf
                                ),
                            });
                            continue;
                        }
                        // 包围盒用**减除前**的姿态（与 `[[animations]]` 同规则）。
                        seq_pre_subtract = Some(frames.clone());
                        // 权重 ≤ 0 的骨骼不减除（`simplify.cpp:1088`）。
                        let w = weights_of(s.weight_list.as_deref());
                        subtract_base_frames(&mut frames, &src, bf, &w);
                    }
                    None => {
                        seq_errors.push(CompileError::Message {
                            at: format!("{at}.subtract"),
                            message: crate::tr_fmt!(
                                "cannot find the reference animation %{name} (subtract takes an **animation or sequence name**; \
                                 existing animations: %{anims}; existing sequences: %{seqs})",
                                name = format!("{ref_name:?}"),
                                anims = desc.animations.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
                                seqs = desc.sequences.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ")
                            ),
                        });
                        continue;
                    }
                }
            }

            let nf = frames.len() as i32;
            let sec_len = s.section_frames.unwrap_or(DEFAULT_SECTION_FRAMES);
            let sec_thr = s.section_threshold.unwrap_or(DEFAULT_SECTION_THRESHOLD);
            // ---- 自动层（`addlayer <序列名>`）----
            //
            // ⚠️ **单动画序列与 blend 序列走的是两条不同的代码路径**，而
            // `addlayer` 在**两条路径上都必须处理**。
            //
            // 官方 `ParseSequence` 的 `addlayer` 分支（`studiomdl.cpp:2867-2872`）
            // 只把序列名记进 `pseq->autolayer[]`，**与 `numblends` 无关** ——
            // 所以 `$sequence "reload_layer" "al_reload" … addlayer "look_poses"`
            // （单动画 + addlayer）与 `$sequence "idle" "a_run" "a_idle" … addlayer …`
            // （blend + addlayer）**是同一件事**。
            //
            // 早先这里对单动画路径写死了 `auto_layers: Vec::new()` ⟹
            // `reload_layer` / `reload_loop_layer` / `reload_end_layer` 三条序列的
            // `addlayer "look_poses"` **被静默丢弃**（NekoMDL 有、mdlc 没有）。
            // 自动层丢失 ⟹ 引擎不会把这些序列与 `look_poses` 混合 ⟹
            // **手部/上身姿态少了一层**。
            //
            // 帧数取**本序列第一格**的（官方用 `panim[0][0]->numframes`，
            // `write.cpp:541-544`），与 blend 路径同一口径。
            let mut auto_layers = Vec::with_capacity(s.auto_layers.len());
            {
                let nf = frames.len().max(1) as f32;
                for (li, al) in s.auto_layers.iter().enumerate() {
                    let Some(seq_idx) = desc
                        .sequences
                        .iter()
                        .position(|x| x.name.eq_ignore_ascii_case(&al.sequence))
                    else {
                        seq_errors.push(e(
                            format!("{at}.auto_layers[{li}].sequence"),
                            crate::tr_fmt!(
                                "cannot find the sequence %{name}",
                                name = format!("{:?}", al.sequence)
                            ),
                        ));
                        continue;
                    };
                    auto_layers.push(crate::model::CompiledAutoLayer {
                        sequence: seq_idx as i16,
                        pose: al.pose,
                        flags: al.flags,
                        // `write.cpp:539-551`：不带 `STUDIO_AL_POSE` 时
                        // 四个量**除以 `numframes − 1`**（转 cycle）。
                        start: layer_time(al.start, al.flags, nf),
                        peak: layer_time(al.peak, al.flags, nf),
                        tail: layer_time(al.tail, al.flags, nf),
                        end: layer_time(al.end, al.flags, nf),
                    });
                }
            }
            sequences.push(crate::model::CompiledSequence {
                key_values: s.key_values.clone(),
                name: s.name.clone(),
                // 复用已声明动画时 `smd_path` 是那个动画的源；新建隐含动画时
                // 就是刚解析出的路径 ⟹ 两者都取 `anims[anim_ix]` 的。
                // （原先这里直接用局部 `smd_path`，但路径解析现在只在
                // `None` 臂里发生 —— 见上面那段说明。）
                smd_path: anims[anim_ix].smd_path.clone(),
                fps: s.fps.unwrap_or(30.0),
                looping: s.looping,
                activity: -1,
                activity_name: s.activity.clone().unwrap_or_default(),
                activity_weight: s.activity_weight,
                delta: s.delta,
                frames,
                cells: vec![anim_ix],
                blend_width: 1,
                blend_params: [None, None],
                // 单动画序列的 `groupsize` 是 1×1 ⟹ 官方
                // `CalcPoseParameters` 的 `groupsize[iPose] > 1` **不成立**
                // ⟹ 一根轴都不遍历 ⟹ 没有 calc 轴。
                calc_axes: Vec::new(),
                blend_ref: None,
                blend_comp: None,
                blend_center: None,
                transition: s.transition.clone(),
                transition_reversible: s.transition_reversible,
                auto_layers,
                events: s.events.clone(),
                fade_in: s.fade_in,
                fade_out: s.fade_out,
                no_auto_ik: s.no_auto_ik,
                ik_rules: s.ik_rules.clone(),
                iklocks: s.iklocks.clone(),
                movements: s.movements.clone(),
                section_frames: if sec_len > 0 && nf >= sec_thr {
                    sec_len
                } else {
                    0
                },
                num_sections: 0, // 下面按 section_frames 算（依赖 frames 数）
                // 序列级 `subtract` 的「减除前帧」优先；否则用动画自己的。
                pre_subtract_frames: seq_pre_subtract
                    .or_else(|| pre_subtract.get(anim_ix).and_then(|p| p.clone())),
                extra_flags: s.extra_flags,
                forward_declared: false,
                weights: merge_weights(&[anim_ix], &anim_weights, n_bones),
            });
        }
        if !seq_errors.is_empty() {
            return Err(seq_errors);
        }
    });

    let mut motion_options =
        vec![(0i32, Vec::new(), crate::model::default_motion_rollback()); anims.len()];
    for animation in &desc.animations {
        if let Some(&index) = anim_index.get(animation.name.as_str()) {
            motion_options[index] = (
                animation.motion_type,
                animation.cmds.clone(),
                animation.motion_rollback,
            );
        }
    }
    for (source, sequence) in desc.sequences.iter().zip(&sequences) {
        if let Some(&index) = sequence.cells.first() {
            motion_options[index].0 |= source.motion_type;
            motion_options[index].1.extend(source.cmds.iter().cloned());
            if !desc
                .animations
                .iter()
                .any(|animation| anim_index.get(animation.name.as_str()) == Some(&index))
            {
                motion_options[index].2 = source.motion_rollback;
            }
            if let Some(fps) = source.fps {
                anims[index].fps = fps;
            }
            anims[index].looping |= source.looping;
            anims[index].no_auto_ik |= source.no_auto_ik;
        }
    }
    let parents: Vec<_> = desc
        .bones
        .iter()
        .map(|bone| {
            bone.parent
                .as_deref()
                .and_then(|parent| desc.bones.iter().position(|bone| bone.name == parent))
                .map_or(-1, |index| index as i32)
        })
        .collect();
    let bind: Vec<_> = desc
        .bones
        .iter()
        .enumerate()
        .map(|(index, bone)| {
            let source = bodyparts
                .iter()
                .flat_map(|part| &part.models)
                .flat_map(|model| &model.poses)
                .find(|pose| pose.bone == index as i32);
            crate::smd::SmdPose {
                bone: index as i32,
                position: bone
                    .position
                    .unwrap_or_else(|| source.map_or([0.0; 3], |pose| pose.position)),
                rotation: bone
                    .rotation
                    .map(|angles| angles.map(f32::to_radians))
                    .unwrap_or_else(|| source.map_or([0.0; 3], |pose| pose.rotation)),
            }
        })
        .collect();
    let mut pos_desc = desc.clone();
    for bone in &mut pos_desc.default_weight_list {
        bone.weight = bone.pos_weight();
    }
    for list in &mut pos_desc.weight_lists {
        for bone in &mut list.bones {
            bone.weight = bone.pos_weight();
        }
    }
    let position_lists = resolve_weight_lists(&pos_desc);
    let default_position_weights = resolve_default_weight_list(&pos_desc);
    let position_weights_of = |name: Option<&str>| {
        weight_list_index(desc, name.unwrap_or(""))
            .and_then(|index| position_lists.get(index - 1))
            .cloned()
            .unwrap_or_else(|| default_position_weights.clone())
    };
    let ik_fixups: Vec<Vec<_>> = motion_options
        .iter()
        .map(|(_, commands, _)| {
            commands
                .iter()
                .filter_map(|command| match command {
                    crate::model::AnimCmd::IkFixup { rule } => Some(rule.clone()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let motion_root = desc
        .model
        .root_bone
        .as_deref()
        .and_then(|name| {
            desc.bones
                .iter()
                .position(|bone| bone.name.eq_ignore_ascii_case(name))
        })
        .unwrap_or(0);
    for (index, (motion_type, commands, rollback)) in motion_options.into_iter().enumerate() {
        if motion_type == 0
            && commands.is_empty()
            && !anims[index].fudge_loop
            && !anims[index].looping
            && anims[index].loop_restart == 0
        {
            continue;
        }
        let mut retained = animation_pool_poses(&anims, &sequences, parents.len())
            .map_err(|message| vec![e(format!("animations[{index}]"), message)])?;
        reserve_animation_poses(&mut retained, anims[index].frames.len(), parents.len())
            .map_err(|message| vec![e(format!("animations[{index}]"), message)])?;
        let mut working = anims[index].clone();
        if working.delta
            && (motion_type != 0
                || commands.iter().any(|command| {
                    matches!(
                        command,
                        crate::model::AnimCmd::Motion { .. }
                            | crate::model::AnimCmd::RefMotion { .. }
                    )
                }))
        {
            return Err(vec![e(
                format!("animations[{index}].motion"),
                "Motion extraction on delta animations is not supported",
            )]);
        }
        let mut weights = if commands
            .iter()
            .any(|cmd| matches!(cmd, crate::model::AnimCmd::Weights { .. }))
        {
            default_weights.clone()
        } else {
            anim_weights[index].clone()
        };
        let source_name = desc
            .animations
            .iter()
            .find(|animation| anim_index.get(animation.name.as_str()) == Some(&index))
            .and_then(|animation| animation.weight_list.as_deref());
        let mut position_weights = position_weights_of(
            if commands
                .iter()
                .any(|cmd| matches!(cmd, crate::model::AnimCmd::Weights { .. }))
            {
                None
            } else {
                source_name
            },
        );
        let root_rotation = desc
            .animations
            .iter()
            .find(|animation| anim_index.get(animation.name.as_str()) == Some(&index))
            .and_then(|animation| animation.rotation)
            .or_else(|| {
                desc.sequences
                    .iter()
                    .zip(&sequences)
                    .find(|(_, sequence)| sequence.cells.first() == Some(&index))
                    .and_then(|(source, _)| source.rotation)
            })
            .unwrap_or(crate::ani_writer::ROOT_REFERENCE_ANGLES);

        let mut hierarchy_source = Vec::new();
        let mut hierarchy_start = 0;
        let mut hierarchy_frame_counts = Vec::new();
        if commands
            .iter()
            .any(|cmd| matches!(cmd, crate::model::AnimCmd::LocalHierarchy { .. }))
        {
            let named = desc
                .animations
                .iter()
                .find(|animation| anim_index.get(animation.name.as_str()) == Some(&index));
            let sequence = desc
                .sequences
                .iter()
                .zip(&sequences)
                .find(|(_, sequence)| sequence.cells.first() == Some(&index))
                .map(|(source, _)| source);
            let mut options = if let Some(source) = named {
                hierarchy_start = source.frames.map_or(0, |range| range[0].max(0) as usize);
                SrcOpts::default()
                    .with_anim(source.src_stack.as_deref(), source.src_fps)
                    .with_scale(source.scale)
                    .with_root_transform(source.adjust, source.rotation)
            } else if let Some(source) = sequence {
                hierarchy_start = source.frames.map_or(0, |range| range[0].max(0) as usize);
                SrcOpts::default()
                    .with_anim(source.src_stack.as_deref(), source.src_fps)
                    .with_scale(source.scale)
                    .with_root_transform(source.adjust, source.rotation)
            } else {
                SrcOpts::default()
            };
            inherit_automatic_axes(&mut options, desc, base_dir, &working.smd_path);
            let mut errors = Vec::new();
            hierarchy_source = load_smd_frames(
                &working.smd_path,
                desc,
                &bone_index,
                &format!("animations[{index}].localhierarchy"),
                &options,
                &mut errors,
            )
            .map(|(frames, _)| frames)
            .ok_or(errors)?;
            crate::anim_commands::convert(&mut hierarchy_source, &parents, true);
        }
        crate::animation_loops::prepare(&mut working)
            .map_err(|message| vec![e(format!("animations[{index}].cmds"), message)])?;
        let mut processor = crate::motion::Processor::begin(
            &mut working.frames,
            &parents,
            motion_type,
            working.delta,
            motion_root,
        )
        .map_err(|message| vec![e(format!("animations[{index}].cmds"), message)])?;
        for command in &commands {
            let reference_name = match command {
                crate::model::AnimCmd::Align { reference, .. }
                | crate::model::AnimCmd::Match { reference }
                | crate::model::AnimCmd::WorldSpaceBlend { reference, .. }
                | crate::model::AnimCmd::MatchBlend { reference, .. }
                | crate::model::AnimCmd::Subtract { reference, .. } => Some(reference.as_str()),
                _ => None,
            };
            let reference_index = reference_name
                .and_then(|name| resolve_lookup_animation(name, &anims, &anim_index, &sequences));
            let reference = reference_index.map(|reference| {
                if reference == index {
                    let mut frames = working.frames.clone();
                    if !working.delta {
                        crate::anim_commands::convert(&mut frames, &parents, false);
                    }
                    frames
                } else {
                    anims[reference].frames.clone()
                }
            });
            let result: Result<(), String> = (|| {
                match command {
                    crate::model::AnimCmd::LocalHierarchy {
                        bone,
                        parent,
                        range,
                    } => {
                        working.local_hierarchy.push(crate::local_hierarchy::create(
                            crate::local_hierarchy::Source {
                                bones: &desc.bones,
                                parents: &parents,
                                frames: &hierarchy_source,
                                start: hierarchy_start,
                                count: working.frames.len(),
                                looping: working.looping,
                            },
                            bone,
                            parent,
                            *range,
                        )?);
                        hierarchy_frame_counts.push(working.frames.len());
                    }
                    crate::model::AnimCmd::Weights { weight_list } => {
                        if weight_list_index(desc, weight_list).is_none() {
                            return Err(format!("Unknown weightlist {weight_list:?}"));
                        }
                        weights = weights_of(Some(weight_list));
                        position_weights = position_weights_of(Some(weight_list));
                    }
                    crate::model::AnimCmd::IkRule { rule } => working.ik_rules.push(rule.clone()),
                    crate::model::AnimCmd::IkFixup { .. } => {}
                    crate::model::AnimCmd::Motion { .. }
                    | crate::model::AnimCmd::RefMotion { .. } => {
                        if working.delta {
                            return Err(
                                "Motion extraction on delta animations is not supported".into()
                            );
                        }
                        processor.command(&mut working.frames, &parents, command, |name| {
                            resolve_lookup_animation(name, &anims, &anim_index, &sequences)
                                .map(|reference| anims[reference].frames.clone())
                        })?;
                    }
                    _ => {
                        working.movements.clone_from(&processor.movements);
                        let context = crate::anim_commands::Context {
                            motion_root,
                            bones: &desc.bones,
                            parents: &parents,
                            motion_type,
                            reference: reference.as_deref(),
                            pos_weights: &position_weights,
                            bind: &bind,
                            engine_space: true,
                            reference_is_delta: reference_index.is_some_and(|reference| {
                                if reference == index {
                                    working.delta
                                } else {
                                    anims[reference].delta
                                }
                            }),
                            root_rotation,
                        };
                        if !crate::anim_commands::apply(
                            &mut working,
                            &mut weights,
                            command,
                            &context,
                        )? {
                            return Err(format!(
                                "Animation command {command:?} is not implemented"
                            ));
                        }
                        processor.movements.clone_from(&working.movements);
                    }
                }
                Ok(())
            })();
            result.map_err(|message| vec![e(format!("animations[{index}].cmds"), message)])?;
        }
        if !working.local_hierarchy.is_empty() && working.frames.len() < 2 {
            return Err(vec![e(
                format!("animations[{index}].localhierarchy"),
                "localhierarchy requires at least two final animation frames",
            )]);
        }
        for (rule, count) in working
            .local_hierarchy
            .iter_mut()
            .zip(hierarchy_frame_counts)
        {
            let scale = (count - 1) as f32 / (working.frames.len() - 1) as f32;
            for phase in &mut rule.range {
                *phase *= scale;
            }
        }
        working.movements = processor
            .finish(
                &mut working.frames,
                &parents,
                motion_type,
                working.looping,
                working.fps,
                rollback,
                working.delta,
                working.loop_restart,
            )
            .map_err(|message| vec![e(format!("animations[{index}].cmds"), message)])?;
        if desc.version() <= 48 {
            crate::local_hierarchy::bake(&mut working, &parents).map_err(|message| {
                vec![e(format!("animations[{index}].localhierarchy"), message)]
            })?;
        }
        reserve_animation_poses(&mut retained, working.frames.len(), parents.len())
            .map_err(|message| vec![e(format!("animations[{index}].cmds"), message)])?;
        anims[index] = working;
        anim_weights[index] = weights;
        for sequence in &mut sequences {
            if sequence.cells.first() == Some(&index) {
                reserve_animation_poses(&mut retained, anims[index].frames.len(), parents.len())
                    .map_err(|message| vec![e(format!("animations[{index}].cmds"), message)])?;
                sequence.frames.clone_from(&anims[index].frames);
                sequence
                    .pre_subtract_frames
                    .clone_from(&anims[index].pre_subtract_frames);
                sequence.fps = anims[index].fps;
            }
        }
    }
    animation_pool_poses(&anims, &sequences, parents.len())
        .map_err(|message| vec![e("animations", message)])?;
    for (animation, weights) in anims.iter_mut().zip(&anim_weights) {
        animation.weights.clone_from(weights);
    }
    for sequence in &mut sequences {
        if !sequence.forward_declared {
            sequence.weights = merge_weights(&sequence.cells, &anim_weights, n_bones);
        }
    }

    // 段表条目数 = `floor(numframes / sectionframes) + 2`（**不是 ceil**）。
    // 引擎索引的最大下标是 `numframes/sectionframes + 1`（`studio.cpp:345`）。
    for s in sequences.iter_mut() {
        s.num_sections = if s.section_frames > 0 {
            (s.frames.len() as i32 / s.section_frames) as usize + 2
        } else {
            0
        };
    }

    // ---- `$staticprop`：骨骼塌缩 + 动画塌陷 ----
    //
    // 顶点级的旋转与权重归零已在 [`smd_vertex_to_ir`] 里做掉（必须在
    // `unify_lods` 之前）。这里补上**骨骼表**与**动画**两处。
    //
    // 顺序与 studiomdl 一致：`MakeStaticProp()` 在 `RemapBones()`
    // （`simplify.cpp:4461`）里跑，而 `RemapBones()` 是 `SimplifyModel()`
    // 的第一步，早于 `UnifyLODs()` 与 `CalcSequenceBoundingBoxes()`。
    let desc = if desc.model.static_prop {
        collapse_static_prop(desc)
    } else {
        desc.clone()
    };

    // ---- 自动生成 hitbox（`SetupHitBoxes`，`simplify.cpp:6849-6973`）----
    //
    // 时序：`SetupHitBoxes()` 在 `simplify.cpp:7319` 被调用，**晚于**
    // `RemapBones()`（7231），所以静态道具的骨骼此时**已经**塌缩成
    // 单根 `static_prop` —— 自动 hitbox 自然就落在那一根上。
    //
    // 只在用户**没有**写显式 hitbox 时生成（`g_hitboxsets.Size() == 0`）。
    // 先构造出 `CompiledModelDesc`，因为参考姿态要用 [`resolve_bone_pose`]
    // 解析（它会回退到 SMD 第 0 帧并做 `canonical_euler` 规范化 ——
    // 必须与写进 `mstudiobone_t` 的值**完全一致**）。
    let mut compiled = CompiledModelDesc {
        desc,
        bodyparts,
        sequences,
        animations: anims,
        realigned: None,
        resolved_flex_rules: Vec::new(),
        resolved_flex_controller_ui: Vec::new(),
        resolved_mouths: Vec::new(),
        resolved_jiggle_bones: Vec::new(),
        resolved_quat_interp_bones: Vec::new(),
        // `physicsbone` 由 `main.rs` 在解析**碰撞 SMD** 之后填 ——
        // 编译前端看不到碰撞几何（那是另一份 SMD）。
        physics_bone: None,
    };
    crate::source_center::apply(&mut compiled)
        .map_err(|message| vec![e("model.autocenter", message)])?;

    // ---- flex 系列 / eyeball / mouth：把名字解析成下标、算骨骼空间量 ----
    //
    // 时序：**在 `RealignBones` 之后做**（eyeball 的 `up`/`forward`/`org`
    // 要用重排后的 `boneToPose` 逆变换，与自动 hitbox 同一份世界矩阵口径）。
    // 所以这里先占位，真正的解析在 `compiled.realigned` 定稿之后（见下）。

    // ---- 骨骼轴重对齐（`RealignBones`，`simplify.cpp:4224-4425`）----
    //
    // 时序：`RealignBones()` 在 `simplify.cpp:7237` 调用，**晚于**
    // `LinkIKChains()`（7233）—— 所以 `$ikchain` 推出来的 `childbone[]`
    // 此时已经就绪。它**早于** `SetupHitBoxes()`（7319），所以自动 hitbox
    // 用的是重排后的 `boneToPose`。
    hotpath::measure_block!("realign + flex/jiggle 收尾", {
        compiled.realigned = compute_realigned_poses(&compiled);

        // 动画帧也要搬进重排后的空间（`simplify.cpp:1527`）：
        //
        // ```cpp
        // ConcatTransforms( srcBoneToWorld[q], g_bonetable[k].srcRealign, destBoneToWorld[k] );
        // ```
        //
        // 即「源骨架的第 f 帧世界变换」右乘 `srcRealign` 就得到重排后的世界变换，
        // 再由它反解出新的**局部**姿态。漏掉这一步的症状：骨骼表是对的，
        // 但动画的骨骼位置整体错位 —— `hull`/`seqdesc` 包围盒、`rotscale`、
        // 动画链头全都跟着错，而**不会报任何错**。
        if compiled.realigned.is_some() {
            realign_sequence_frames(&mut compiled);
        }

        // ---- 顶点搬到最终参考姿态的空间（`RemapVerticesToGlobalBones`）----
        //
        // 官方时序：`RealignBones()`（`:7237`）→ **本步**（`:7258`）→
        // `UnifyLODs()`（`:7262`）。
        //
        // ⚠️ mdlc 的 `build_model_lods`（LOD 统一池）是在上面的 bodypart 循环里
        // 跑的，**早于**这里 —— 但它把 `mesh.vertices` **克隆**进 LOD 0 池
        // （`compile.rs:244` 的 `per_mesh[ki].push((mesh.vertices.clone(), …))`），
        // 所以统一池里的 LOD 0 顶点是**当时**的值。若在这里才改
        // `mesh.vertices`，统一池就与它分叉了。
        //
        // 因此本步必须在**读 LOD 之前**做。但 `resolve_bone_pose` 需要
        // `compiled.bodyparts[].models[].poses`（SMD 第 0 帧），而那是 bodypart
        // 循环里才填的 —— 循环依赖。
        //
        // 解法：本函数**同时**改 `mesh.vertices` 与 `lods.meshes[].vertices`
        // （两者都要改，各恰好一次）。见 `remap_vertices_to_reference_pose`。
        hotpath::measure_block!("remap_vertices_to_reference_pose", {
            let remapped = remap_vertices_to_reference_pose(&mut compiled);
            let _ = remapped;
        });

        // ---- `$ikchain` 的 kneeDir 自动推导 ----
        //
        // 官方 `simplify.cpp:2839-2912`：QC 没写 `knee` 时，**从动画里算**出来。
        // 语料里 272 条链有 **263 条非零**，所以这条不是可选项。
        compiled.desc.model.rotation_alignments = crate::bone_alignment::calculate(&compiled);
        derive_ikchain_knee_dirs(&mut compiled);
        for (index, rules) in ik_fixups.iter().enumerate() {
            if rules.is_empty() {
                continue;
            }
            crate::ik_fixup::apply(
                &mut compiled.animations[index],
                &compiled.desc,
                &parents,
                rules,
            )
            .map_err(|message| vec![e(format!("animations[{index}].ikfixup"), message)])?;
            for sequence in &mut compiled.sequences {
                if sequence.cells.first() == Some(&index) {
                    sequence
                        .frames
                        .clone_from(&compiled.animations[index].frames);
                }
            }
        }

        // ---- `CalcPoseParameters`（`simplify.cpp:5448-5596`）----
        //
        // 官方在 `ProcessData` 的**末尾**调用（`simplify.cpp:7315`），晚于
        // `LinkAttachments`（7298）与 `ProcessIKRules`（7311）。这里放在
        // kneeDir 之后 —— 两者互不影响（kneeDir 只看 `$ikchain` 的骨骼，
        // calcblend 只看附着点），但保持「序列/动画/附着点全就绪」的前提。
        //
        // ⚠️ 这是**硬错误**路径：官方在这里 `MdlError` 直接中止编译。
        apply_calc_blend_axes(&mut compiled, &anim_weights, base_dir)?;

        // ---- flex / eyeball / mouth 解析（在重排定稿后）----
        // eyeball 的 `up`/`forward`/`org` 用 [`internal_bone_world`] 的世界矩阵
        // 逆变换 —— 与自动 hitbox / 姿态包围盒**同一份口径**（官方
        // `g_bonetable[k].boneToPose`，不是骨骼表最终矩阵）。
        //
        // ⚠️ flexdesc 的注册必须在这里做（**早于** flexrule/eyeball 的
        // 名字解析），因为两者都会**追加** flexdesc，而后续所有按名查表
        // 都依赖最终的下标。所以先注册 desc，再统一解析。
        //
        // 顺序：**shape key 在前，`.vta` 在后**。官方在 FBX 导入阶段
        // （`Load_Source` 里）就把 shape key 注册成 flex，而 QC 的 `flex`
        // 语句要等 `$model` 块解析时才处理 —— 实测
        // `oracle_fbx_isolate.js` 的 `W2_morphfbx_vta_flex` 得到
        // `desc = ["wide", "tall", "v1"]`，shape key 的两条在显式 `flex`
        // 之前。
        resolve_shape_key_flexes(&mut compiled)?;
        resolve_vta_flexes(&mut compiled, base_dir)?;

        resolve_flex_eyeball_mouth(&mut compiled)?;

        // ---- jigglebone（`mstudiojigglebone_t`，L4D2 独有的 `$jigglebone`）----
        //
        // 程序化骨骼块夹在骨骼数组与 `bonecontroller` 之间（`write.cpp:214-285`），
        // 所以 `layout.rs` 的公式依赖这里的条数；解析必须在写出之前完成。
        resolve_jiggle_bones(&mut compiled)?;
        resolve_quat_interp_bones(&mut compiled)?;

        if compiled.desc.hitbox_sets().next().is_none() {
            let auto = auto_hitboxes(&compiled);
            // **即使 `auto` 为空也要建 set** —— 官方在过滤前就建好了 set 并置了
            // 标志（`simplify.cpp:6884-6892` 在建 set 与置标志之后才过滤）。
            // 实测语料 166 个模型是「set 存在但 0 box」且**全部**带 `0x1` 标志。
            compiled.desc.hitboxes.set_name = Some("default".to_string());
            compiled.desc.hitboxes.boxes = auto;
            compiled.desc.hitboxes.autogenerated = true;
            // `simplify.cpp:6892`：自动生成路径置
            // `gflags |= STUDIOHDR_FLAGS_AUTOGENERATED_HITBOX`（**0x1**）。
            //
            // 注意它是 flags 的**最低位**，不是 0x2000。
            // 实测 `ip_official.mdl`（`ip.qc` 没写 `$hbox`）的 `flags == 0x1`
            // 正是它。
            let f = compiled.desc.model.extra_flags.unwrap_or(0);
            compiled.desc.model.extra_flags =
                Some(f | crate::mdl_writer::FLAG_AUTOGENERATED_HITBOX);
        }
    });

    // ---- 顶点超限的 mesh 自动拆分（`split_oversized_meshes`，默认开）----
    //
    // 放在**最后**：此时所有几何（含 LOD、flex、权重）都已定稿，
    // 拆分只是「按三角形重切一刀」，不再动任何数值。
    //
    // 不超限时它是 **no-op**（第一行就返回），所以对既有产物零影响。
    if compiled.desc.model.split_oversized_meshes {
        hotpath::measure_block!("split_oversized_meshes", {
            split_oversized_meshes(&mut compiled)?;
        });
    }

    // Keep texture indices and skin-family mappings stable while changing labels.
    for texture in &mut compiled.desc.materials.textures {
        texture.name = renamed_material(&texture.name, &compiled.desc.model.material_renames);
    }
    Ok(compiled)
}

fn renamed_material(name: &str, replacements: &[[String; 2]]) -> String {
    let normalized = name.replace('\\', "/");
    replacements
        .iter()
        .find(|[old, _]| old.replace('\\', "/").eq_ignore_ascii_case(&normalized))
        .map_or_else(|| name.to_owned(), |[_, new]| new.replace('\\', "/"))
}

/// 把超过 `MAXSTUDIOVERTS_PER_MESH` 的 mesh 按三角形顺序切成多块。
///
/// # 为什么需要
///
/// VTX 的 `Vertex_t.origMeshVertID` 是 `uint16` ⟹ **一个 mesh 最多 65536 个
/// 顶点**。一个 mesh 对应一个材质，所以某个材质本身顶点太多时（真实案例：
/// 305,703 个），官方 `studiomdl` 直接拒绝（`ERROR: too many indices in
/// source`），本实现原先也会拒绝。
///
/// # 拆法
///
/// **在同一个 model 内拆成多个 mesh，全部指向同一个材质下标。**
///
/// 为什么不拆成新 bodypart（NekoMDL 的 `$maxverts` 那样）：
/// bodypart 数量一变，引擎的 `$bodygroup` 选择（按下标）就会错位；
/// 而且 NekoMDL 自己的产物里出现了重名 bodypart。`mesh.material` 只是
/// `pSkinref[]` 的下标，多个 mesh 共用它是完全合法的。
///
/// # 切块规则
///
/// 按**三角形顺序**贪心累积：一个三角形是原子的（不会跨块切断），
/// 累积到再加一个就会超过上限时收尾、开新块。所以每块顶点数 ≤ 上限，
/// 且**所有块合起来与原来逐三角形等价**（顶点池按需重建，
/// 只保留该块三角形实际引用的顶点）。
///
/// # 多 LOD 的判据是「统一池」而不是 LOD 0
///
/// `origMeshVertID` 的上界是 `mstudiomesh_t.numvertices`，即**跨 LOD 去重
/// 后的统一池**大小，所以超限判据也必须是它（LOD 0 不超限但 LOD 1 引入
/// 大量新顶点时同样会溢出）。
///
/// 切块用 LOD 0 的三角形当种子（LOD 0 最细），其余 LOD 的三角形按
/// 「与哪一块共享顶点最多」归块；块内池**以原统一池下标为键**，所以跨 LOD
/// 的同一个顶点在块内仍是同一条 —— LOD 统一语义不变。
///
/// 若归块后某块仍超限（多个 LOD 把顶点挤进同一块），**报错**而不是写出
/// `origMeshVertID` 溢出的产物。
///
/// # 同步维护的并行数组
///
/// [`CompiledModel`] 里有几个**与 `meshes` 同下标**的数组，拆完必须
/// 一起重建，否则会静默错位：
/// - `mesh_flexes`（每个 mesh 的 flex 载荷，按块重编顶点下标）
/// - `lods.meshes`（每个 mesh 的多 LOD 数据）
fn split_oversized_meshes(
    compiled: &mut crate::model::CompiledModelDesc,
) -> Result<(), Vec<CompileError>> {
    use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;
    use std::collections::{HashMap, HashSet};

    /// 超限判据：单 LOD 看 `mesh.vertices`；多 LOD 看统一池大小。
    fn size_of(mesh: &Mesh, ml: Option<&crate::lod::MeshLods>) -> usize {
        ml.map_or(mesh.vertices.len(), |l| l.vertices.len())
    }

    // 先扫一遍：一个超限的都没有 ⟹ 完全不动（保证既有产物零影响）。
    //
    // ⚠️ 这个提前返回**也**要覆盖「lods.meshes 长度不对」的情况 ——
    // 否则长度不对但恰好没超限时会**静默放过**（那个不变式就白查了）。
    let any = compiled.bodyparts.iter().any(|bp| {
        bp.models.iter().any(|m| {
            let mismatch = m
                .lods
                .as_ref()
                .is_some_and(|l| l.meshes.len() != m.meshes.len());
            mismatch
                || m.meshes.iter().enumerate().any(|(k, mesh)| {
                    size_of(mesh, m.lods.as_ref().and_then(|l| l.meshes.get(k))) > LIMIT
                })
        })
    });
    if !any {
        return Ok(());
    }

    let mut errs: Vec<CompileError> = Vec::new();

    for (bi, bp) in compiled.bodyparts.iter_mut().enumerate() {
        for (mi, model) in bp.models.iter_mut().enumerate() {
            let at = format!("bodyparts[{bi}].models[{mi}]");

            // ⚠️ `lods.meshes` 必须与 `meshes` **等长**（写出器靠下标对应）。
            // 长度不对时「按 mesh 逐项搬」会让结果比原数组短 ⟹ 写出器按下标取
            // 就会**材质贴错面**（不报错）。这是**结构前提**，所以查在
            // 「有没有超限」**之前** —— 否则长度不对但没超限时会静默放过。
            if let Some(ls) = &model.lods
                && ls.meshes.len() != model.meshes.len()
            {
                errs.push(e(
                    &at,
                    crate::tr_fmt!(
                        "internal error: lods.meshes has %{lods} entries, but meshes has %{meshes} \
                         (the two must correspond one to one) -- the split cannot guarantee index alignment",
                        lods = ls.meshes.len(),
                        meshes = model.meshes.len()
                    ),
                ));
                continue;
            }

            let oversized = model.meshes.iter().enumerate().any(|(k, mesh)| {
                size_of(mesh, model.lods.as_ref().and_then(|l| l.meshes.get(k))) > LIMIT
            });
            if !oversized {
                continue;
            }

            let old_meshes = std::mem::take(&mut model.meshes);
            let old_flexes = std::mem::take(&mut model.mesh_flexes);
            let old_lods = model.lods.as_mut().map(|l| std::mem::take(&mut l.meshes));

            let mut new_meshes: Vec<Mesh> = Vec::with_capacity(old_meshes.len());
            let mut new_flexes: Vec<Vec<crate::flex::ResolvedFlex>> =
                Vec::with_capacity(old_meshes.len());
            let mut new_lods: Vec<crate::lod::MeshLods> = Vec::with_capacity(old_meshes.len());

            for (k, mesh) in old_meshes.iter().enumerate() {
                let ml = old_lods.as_ref().and_then(|ls| ls.get(k));
                let flexes: &[crate::flex::ResolvedFlex] =
                    old_flexes.get(k).map(Vec::as_slice).unwrap_or(&[]);

                // 没超限 ⟹ 原样搬过去（这是绝大多数 mesh 的路径）。
                if size_of(mesh, ml) <= LIMIT {
                    new_meshes.push(mesh.clone());
                    new_flexes.push(flexes.to_vec());
                    if let Some(l) = ml {
                        new_lods.push(l.clone());
                    }
                    continue;
                }

                let Some(ml) = ml else {
                    // ---- 单 LOD ----
                    let blocks = greedy_triangle_blocks(&mesh.triangles, LIMIT);
                    if blocks.is_empty() {
                        // 退化网格（有顶点没三角形）：原样保留，交给写出器报错。
                        new_meshes.push(mesh.clone());
                        new_flexes.push(flexes.to_vec());
                        continue;
                    }
                    for blk in &blocks {
                        let mut remap: HashMap<u32, u32> = HashMap::new();
                        let mut verts: Vec<Vertex> = Vec::new();
                        let mut tris: Vec<[u32; 3]> = Vec::with_capacity(blk.len());
                        for tri in blk {
                            let mut t = [0u32; 3];
                            for (j, &vi) in tri.iter().enumerate() {
                                let nv = *remap.entry(vi).or_insert_with(|| {
                                    verts.push(mesh.vertices[vi as usize].clone());
                                    (verts.len() - 1) as u32
                                });
                                t[j] = nv;
                            }
                            tris.push(t);
                        }
                        new_meshes.push(Mesh {
                            material: mesh.material,
                            vertices: verts,
                            triangles: tris,
                            eyeball_tag: mesh.eyeball_tag,
                        });
                        new_flexes.push(remap_flexes(flexes, &remap));
                    }
                    continue;
                };

                // ---- 多 LOD：把**每一档**的三角形都放进块里 ----
                //
                // 不能只按 LOD 0 切块再让别的档「就近归块」：LOD 1 可能引入
                // LOD 0 完全没有的顶点（简化算法换边、导出器换网格），
                // 那些顶点没有「就近」的块可去。所以逐档处理，每遇到一个
                // 装不下的三角形就**开新块**。
                let num_lods = ml.triangles.len().max(1);
                // 种子 LOD：优先 LOD 0，为空则取第一个非空的。
                let seed = (0..num_lods)
                    .find(|&n| ml.triangles.get(n).is_some_and(|t| !t.is_empty()))
                    .unwrap_or(0);
                // `owned[b]` = 第 b 块的顶点并集；`per_lod[n][b]` = 第 n 档在
                // 第 b 块里的三角形。
                let mut owned: Vec<HashSet<u32>> = Vec::new();
                let mut per_lod: Vec<Vec<Vec<[u32; 3]>>> = vec![Vec::new(); num_lods];
                // 种子档用贪心切块（它最细，顶点最多）。
                for b in greedy_triangle_blocks(
                    ml.triangles.get(seed).map(Vec::as_slice).unwrap_or(&[]),
                    LIMIT,
                ) {
                    owned.push(b.iter().flat_map(|t| t.iter().copied()).collect());
                    per_lod[seed].push(b);
                }
                // ⚠️ `per_lod[n]` 是「每块一个 Vec」——种子循环只填了
                // `per_lod[seed]`，其余档此时长度是 0。**必须先补齐**，
                // 否则下面的 `per_lod[n][bi2]` 直接越界 panic。
                for lod in per_lod.iter_mut() {
                    while lod.len() < owned.len() {
                        lod.push(Vec::new());
                    }
                }
                // 其余档：每个三角形优先进「命中顶点最多**且装得下**」的块，
                // 都不行就开新块（LOD 1 独有顶点的情况）。
                for (n, lod_blocks) in per_lod.iter_mut().enumerate() {
                    if n == seed {
                        continue;
                    }
                    let Some(src) = ml.triangles.get(n) else {
                        continue;
                    };
                    for tri in src {
                        let mut best: Option<usize> = None;
                        let mut best_hits = 0usize;
                        for (bi2, set) in owned.iter().enumerate() {
                            let fresh = tri.iter().filter(|v| !set.contains(v)).count();
                            // 判据与贪心切块一致：`>` 而非 `>=`。
                            if set.len() + fresh > LIMIT {
                                continue;
                            }
                            let hits = 3 - fresh;
                            if best.is_none() || hits > best_hits {
                                best = Some(bi2);
                                best_hits = hits;
                            }
                        }
                        let bi2 = best.unwrap_or_else(|| {
                            owned.push(HashSet::new());
                            owned.len() - 1
                        });
                        for &v in tri {
                            owned[bi2].insert(v);
                        }
                        // 开新块是「按档后补」的 ⟹ 本档可能还没有那么多槽位。
                        while lod_blocks.len() < owned.len() {
                            lod_blocks.push(Vec::new());
                        }
                        lod_blocks[bi2].push(*tri);
                    }
                }
                // 开新块是「按档后补」的 ⟹ 统一补齐到 `owned.len()`。
                for lod in per_lod.iter_mut() {
                    while lod.len() < owned.len() {
                        lod.push(Vec::new());
                    }
                }
                if owned.is_empty() {
                    // 所有档位都没有三角形（退化网格）⟹ 原样保留，交给写出器。
                    new_meshes.push(mesh.clone());
                    new_flexes.push(flexes.to_vec());
                    new_lods.push(ml.clone());
                    continue;
                }

                // 统一池里**没有任何三角形引用**的顶点（导出器残留，
                // `unify_lods` 会强制把它们归到最低细节档）。不搬过去就会
                // 静默丢顶点 ⟹ 顶点数变化。逐个塞进「还装得下」的块，
                // 都不行就**另开一块** —— 绝不能硬塞进去把块撑超限
                // （`origMeshVertID` 溢出，写出损坏的 VTX）。
                let lowest = 1u32 << (num_lods.max(1) - 1);
                let orphans: Vec<u32> = (0..ml.vertices.len() as u32)
                    .filter(|u| !owned.iter().any(|s| s.contains(u)))
                    .collect();
                for u in orphans {
                    match owned.iter().position(|s| s.len() < LIMIT) {
                        Some(b) => {
                            owned[b].insert(u);
                        }
                        None => {
                            owned.push(HashSet::from([u]));
                            for lod in per_lod.iter_mut() {
                                lod.push(Vec::new());
                            }
                        }
                    }
                }
                // 开新块可能又加长了 ⟹ 再统一补齐一次。
                for lod in per_lod.iter_mut() {
                    while lod.len() < owned.len() {
                        lod.push(Vec::new());
                    }
                }

                let mut worst = 0usize;
                for (b, set) in owned.iter().enumerate() {
                    // ---- 块内池：**按原统一池下标升序** ----
                    //
                    // 用「升序」而不是「三角形遇到顺序」，有两个理由：
                    // ① 确定性（`HashSet` 的迭代顺序不定，块内编号会被它污染）；
                    // ② 原统一池里 LOD 0 的顶点就在最前面（`unify_lods_impl`
                    //    的 CopyVerts 是**恒等追加**），升序 ⟹ LOD 0 顶点在块内
                    //    仍按原序排在前面，于是 `l0pos` 就是 flex 需要的
                    //    「原 mesh 内下标 → 块内下标」。
                    let mut ordered: Vec<u32> = set.iter().copied().collect();
                    ordered.sort_unstable();
                    let mut map: HashMap<u32, u32> = HashMap::with_capacity(ordered.len());
                    for (i, &u) in ordered.iter().enumerate() {
                        map.insert(u, i as u32);
                    }
                    let mut flags: Vec<u32> = vec![0; ordered.len()];
                    let mut tris_per_lod: Vec<Vec<[u32; 3]>> = Vec::with_capacity(num_lods);
                    for (n, lod_blocks) in per_lod.iter().enumerate() {
                        let mut out: Vec<[u32; 3]> = Vec::new();
                        for tri in lod_blocks.get(b).map(Vec::as_slice).unwrap_or(&[]) {
                            let t = [map[&tri[0]], map[&tri[1]], map[&tri[2]]];
                            for &v in &t {
                                flags[v as usize] |= 1u32 << n;
                            }
                            out.push(t);
                        }
                        tris_per_lod.push(out);
                    }
                    // 孤立顶点强制归到最低细节档 —— 与 `unify_lods` 的收尾
                    // 一致；否则它的 `lodFlags` 是 0，排序时 `Q_log2(0)` 未定义。
                    for f in flags.iter_mut() {
                        if *f == 0 {
                            *f = lowest;
                        }
                    }
                    let counts: Vec<usize> = (0..num_lods)
                        .map(|n| flags.iter().filter(|f| **f & (1u32 << n) != 0).count())
                        .collect();
                    // `lod_vertex_index[n]`：本档的**紧凑局部编号** → 块内池
                    // 下标。块的统一池只含本块用到的顶点，所以局部编号就是
                    // 「带本档位的顶点」按池序排出来的 0..k。
                    let index_per_lod: Vec<Vec<u32>> = (0..num_lods)
                        .map(|n| {
                            flags
                                .iter()
                                .enumerate()
                                .filter(|(_, f)| **f & (1u32 << n) != 0)
                                .map(|(i, _)| i as u32)
                                .collect()
                        })
                        .collect();
                    let verts: Vec<Vertex> = ordered
                        .iter()
                        .map(|&u| ml.vertices[u as usize].clone())
                        .collect();
                    worst = worst.max(verts.len());

                    // `Mesh` 的 LOD 0 视图（多 LOD 下写出器不读它，
                    // 但保持语义诚实）。
                    let lod0 = &index_per_lod[0];
                    let mut l0pos: HashMap<u32, u32> = HashMap::with_capacity(lod0.len());
                    for (i, &u) in lod0.iter().enumerate() {
                        l0pos.insert(u, i as u32);
                    }
                    let l0_verts: Vec<Vertex> =
                        lod0.iter().map(|&u| verts[u as usize].clone()).collect();
                    let l0_tris: Vec<[u32; 3]> = tris_per_lod[0]
                        .iter()
                        .map(|t| [l0pos[&t[0]], l0pos[&t[1]], l0pos[&t[2]]])
                        .collect();

                    new_flexes.push(remap_flexes(flexes, &map));
                    new_meshes.push(Mesh {
                        material: mesh.material,
                        vertices: l0_verts,
                        triangles: l0_tris,
                        eyeball_tag: mesh.eyeball_tag,
                    });
                    new_lods.push(crate::lod::MeshLods {
                        vertices: verts,
                        lod_flags: flags,
                        triangles: tris_per_lod,
                        lod_vertex_counts: counts,
                        lod_vertex_index: index_per_lod,
                    });
                }
                if worst > LIMIT {
                    errs.push(e(
                        &at,
                        crate::tr_fmt!(
                            "mesh[%{k}] (material index %{mat}) still has a block with %{worst} vertices after the split \
                             (the limit is %{limit}): several LODs of this mesh share vertices, and the grouping pushed \
                             them into the same block. Reduce the number of LOD levels for this material, or split the \
                             material into several SMD materials.",
                            k = k,
                            mat = mesh.material,
                            worst = worst,
                            limit = LIMIT
                        ),
                    ));
                }
            }

            // ---- 收尾：并行数组的**等长不变式** ----
            //
            // 这三个数组必须严格等长（写出器与 VTX 都靠下标对应）。
            // 不等长是**静默**错误（材质贴错面 / flex 挂错 mesh），
            // 所以这里显式查一次并报错，而不是写出去。
            //
            // ⚠️ 比的是 `new_lods`（刚造好的），**不是** `model.lods.meshes`
            // —— 后者在函数开头已被 `std::mem::take` 掏空（长度为 0）。
            // 第一版就是比了后者，于是恒报「0 项 vs N 项」。
            if old_lods.is_some() && new_lods.len() != new_meshes.len() {
                errs.push(e(
                    &at,
                    crate::tr_fmt!(
                        "internal error: after the split lods.meshes has %{lods} entries, but meshes has %{meshes}",
                        lods = new_lods.len(),
                        meshes = new_meshes.len()
                    ),
                ));
            }
            if new_flexes.len() != new_meshes.len() {
                errs.push(e(
                    &at,
                    crate::tr_fmt!(
                        "internal error: after the split mesh_flexes has %{flexes} entries, but meshes has %{meshes}",
                        flexes = new_flexes.len(),
                        meshes = new_meshes.len()
                    ),
                ));
            }

            model.meshes = new_meshes;
            model.mesh_flexes = new_flexes;
            if let Some(lods) = model.lods.as_mut() {
                lods.meshes = new_lods;
            }
        }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// 按三角形顺序贪心切块：每块的**顶点并集**不超过 `limit`。
///
/// `tris` 的下标指向同一顶点空间；返回的三角形下标**原样保留**
/// （不做重编号 —— 那是调用方按块重建顶点池时的事）。
///
/// # 为什么不能 clone 集合
///
/// 第一版写的是 `let next = used.clone(); next.extend(tri)`，
/// 那是 **O(块大小)/三角形** —— 真实案例 232,099 个三角形 × 最多 65,536
/// 个顶点的集合，量级 10¹⁰，会退化成分钟级。这里改成只数「本三角形带来
/// 几个新顶点」（O(3)）。
///
/// 退化三角形（三个下标相同）会被**高估**为新顶点，于是块提前收尾 ——
/// 只影响块大小，不影响正确性。
fn greedy_triangle_blocks(tris: &[[u32; 3]], limit: usize) -> Vec<Vec<[u32; 3]>> {
    let mut blocks: Vec<Vec<[u32; 3]>> = Vec::new();
    let mut cur: Vec<[u32; 3]> = Vec::new();
    let mut used: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for tri in tris {
        let fresh = tri.iter().filter(|v| !used.contains(v)).count();
        if used.len() + fresh > limit && !cur.is_empty() {
            blocks.push(std::mem::take(&mut cur));
            used.clear();
        }
        for &v in tri {
            used.insert(v);
        }
        cur.push(*tri);
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    blocks
}

/// 把原 mesh 的 flex 载荷按**块内**顶点重编号重写。
///
/// `ResolvedVertAnim::index` 是**该 mesh 内**的顶点下标；拆分后下标变了，
/// 不重写会让引擎把形状应用到错误的顶点上（**不报错**）。
///
/// 跨块的 flex 会被**复制**到每一块、各自只带本块那份 vertanim ——
/// flex 本来就是 per-mesh 的（同一 flexdesc 出现在多个 mesh 里是常态，
/// 一个材质一个 `mstudioflex_t`）。veranim 全落空的 flex 直接丢弃。
fn remap_flexes(
    flexes: &[crate::flex::ResolvedFlex],
    remap: &std::collections::HashMap<u32, u32>,
) -> Vec<crate::flex::ResolvedFlex> {
    let mut out = Vec::new();
    for f in flexes {
        let mut nf = f.clone();
        nf.vertanims.retain_mut(|a| match remap.get(&a.index) {
            Some(&n) => {
                a.index = n;
                true
            }
            None => false,
        });
        if !nf.vertanims.is_empty() {
            out.push(nf);
        }
    }
    out
}

/// 把动画的每一帧搬到**重排后**的骨骼空间（`simplify.cpp:1527`）。
///
/// # 原理
///
/// `srcRealign[k] = srcWorld[k]⁻¹ ∘ newWorld[k]`，所以
/// `srcWorld[k] ∘ srcRealign[k] == newWorld[k]`。
/// 对动画的第 `f` 帧，把**源**骨架的世界变换右乘 `srcRealign[k]`，
/// 就得到「同一姿态在重排后骨架上的世界变换」；再由它和父骨骼的
/// 世界变换反解出新的局部姿态。
///
/// # 为什么不能只改骨骼表
///
/// 骨骼表存的是**参考姿态**，动画存的是**每帧相对参考姿态的增量**。
/// 重排换了局部基，增量也跟着换基 —— 只改一边会让两者不一致。
fn realign_sequence_frames(compiled: &mut CompiledModelDesc) {
    let Some(r) = compiled.realigned.clone() else {
        return;
    };
    let desc = &compiled.desc;
    let n = desc.bones.len();
    let bone_index = desc.bone_index();
    let parents: Vec<i32> = desc
        .bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => bone_index.get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect();

    for frames in compiled
        .sequences
        .iter_mut()
        .flat_map(|sequence| {
            (!sequence.delta)
                .then_some(&mut sequence.frames)
                .into_iter()
                .chain(sequence.pre_subtract_frames.iter_mut())
        })
        .chain(compiled.animations.iter_mut().flat_map(|animation| {
            (!animation.delta)
                .then_some(&mut animation.frames)
                .into_iter()
                .chain(animation.pre_subtract_frames.iter_mut())
        }))
    {
        for frame in frames {
            if frame.len() < n {
                continue;
            }
            // 源骨架在这一帧的世界变换。
            let src_pos: Vec<[f32; 3]> = (0..n).map(|i| frame[i].position).collect();
            let src_rot: Vec<[f32; 3]> = (0..n).map(|i| frame[i].rotation).collect();
            let src_world = crate::bone_math::compute_world(&src_pos, &src_rot, &parents);
            // 搬到重排后的空间。
            let new_world: Vec<crate::bone_math::Matrix3x4> = (0..n)
                .map(|i| crate::bone_math::concat(&src_world[i], &r.src_realign[i]))
                .collect();
            // 反解局部姿态。
            for i in 0..n {
                let local = match parents[i] {
                    p if p >= 0 => crate::bone_math::concat(
                        &crate::bone_math::invert(&new_world[p as usize]),
                        &new_world[i],
                    ),
                    _ => new_world[i],
                };
                frame[i].position = [local[3], local[7], local[11]];
                frame[i].rotation = crate::bone_math::matrix_angles(&local);
            }
        }
    }
}

/// 推导 `$ikchain` 的 `kneeDir`（官方 `simplify.cpp:2839-2912`）。
///
/// # 官方语义
///
/// ```c
/// Vector kneeDir = g_ikchain[k].link[0].kneeDir;
/// if (kneeDir.Length() > 0.0) { hasKnees = true; }   // QC 写了 `knee` ⟹ 直接用
/// else {
///     for (每个动画 i) {
///         if (panim->flags & STUDIO_DELTA)  continue;   // ← 跳过 delta 动画
///         if (panim->flags & STUDIO_HIDDEN) continue;   // ← 跳过 hidden 动画
///         for (每帧 j) {
///             CalcBoneTransforms( panim, j, boneToWorld );
///             MatrixPosition( boneToWorld[link[0].bone], worldThigh );
///             MatrixPosition( boneToWorld[link[1].bone], worldKnee  );
///             MatrixPosition( boneToWorld[link[2].bone], worldFoot  );
///             l1 = |worldKnee - worldThigh|;  l2 = |worldFoot - worldKnee|;
///             l3 = |worldFoot - worldThigh|;
///             ikHalf    = (worldFoot + worldThigh) * 0.5;
///             ikKneeDir = normalize( worldKnee - ikHalf );
///             if (l3 > (l1 + l2) * 0.999)  needsFixup = true;   // 腿太直 ⟹ 标记
///             else {
///                 VectorIRotate( ikKneeDir, boneToWorld[link[0].bone], tmp );
///                 bend = ((dot(worldThigh - worldKnee, worldFoot - worldKnee) / (l1*l3)) + 1) / 2;
///                 kneeDir += tmp * bend;      // ← **累加**（不是平均）
///                 hasKnees = true;
///             }
///         }
///     }
/// }
/// if (!needsFixup) continue;              // ⚠️ 没有任何一帧「太直」⟹ **不写回**
/// if (!hasKnees) { printf("ik rules but no clear knee direction\n"); continue; }
/// VectorNormalize( kneeDir );
/// g_ikchain[k].link[0].kneeDir = kneeDir;  // ← 归一化后写回
/// ```
///
/// # ⚠️ 两个反直觉之处（都实测过）
///
/// ① **`needsFixup` 是「写回」的开关，不是「出错」的开关。**
///    只有当**至少有一帧**满足 `l3 > (l1+l2)*0.999`（腿几乎伸直）时，
///    官方才把累加出来的 `kneeDir` 归一化写回。若所有帧都不满足，
///    算出来的 `kneeDir` **被丢弃**，保持 QC 的原值（此处是 0）。
///
/// ② **`kneeDir += tmp * bend` 是累加，不是加权平均。**
///    所以「帧数多」会让方向被放大 —— 归一化后等价于「按 bend 加权求和的方向」。
///
/// 实测（`vm_test_group`）：QC 里 **6 个模型全部没写 `knee`**，
/// 而官方产物的 `links[0].kneeDir` 是**非零单位向量**
/// （如 `v_silenced_smg` 的 `[0.611487, 0.622407, 0.488562]`）
/// —— 25 个分量差异全出自这里。
///
/// 语料普查（`survey_mdl_name_kneedir.js`，272 条链）：
/// **非零 263 条 / 零 9 条** ⟹ 这条规则影响**绝大多数真实模型**。
fn derive_ikchain_knee_dirs(compiled: &mut CompiledModelDesc) {
    if compiled.desc.ikchains.is_empty() {
        return;
    }
    // 只处理「QC 没写 `knee`」的链。
    let pending: Vec<usize> = compiled
        .desc
        .ikchains
        .iter()
        .enumerate()
        .filter(|(_, c)| c.knee_dir.is_none())
        .map(|(i, _)| i)
        .collect();
    if pending.is_empty() {
        return;
    }
    let bone_index = compiled.desc.bone_index();
    // 链末端骨骼下标（`link[2]`），父、祖父由骨骼表推出（与写出器同口径）。
    let parents = bone_parents(&compiled.desc);
    let mut chains: Vec<(usize, usize, usize)> = Vec::new();
    for &ci in &pending {
        let c = &compiled.desc.ikchains[ci];
        let Some(&tip) = bone_index.get(c.bone.as_str()) else {
            continue;
        };
        let Ok(mid) = usize::try_from(parents[tip]) else {
            continue;
        };
        let Ok(root) = usize::try_from(parents[mid]) else {
            continue;
        };
        chains.push((root, mid, tip));
    }
    if chains.is_empty() {
        return;
    }

    // 逐链累加（`kneeDir += tmp * bend`）与「是否出现过太直的帧」。
    let mut acc: Vec<[f64; 3]> = vec![[0.0; 3]; chains.len()];
    let mut needs_fixup = vec![false; chains.len()];
    let mut has_knees = vec![false; chains.len()];

    // 世界矩阵的父链缓存（逐帧重建）。
    for anim in &compiled.animations {
        // `STUDIO_DELTA` / `STUDIO_HIDDEN` 的动画整个跳过（`simplify.cpp:2855-2859`）。
        if anim.delta {
            continue;
        }
        let seq = &compiled.sequences[anim
            .name
            .strip_prefix('@')
            .and_then(|n| {
                compiled
                    .sequences
                    .iter()
                    .position(|s| s.name.eq_ignore_ascii_case(n))
            })
            .unwrap_or(0)];
        if seq.extra_flags.is_some_and(|f| f & 0x0400 != 0) {
            continue;
        }
        for frame in &anim.frames {
            // 每帧的世界矩阵（`CalcBoneTransforms`）。
            let positions: Vec<[f32; 3]> = frame.iter().map(|p| p.position).collect();
            let rotations: Vec<[f32; 3]> = frame.iter().map(|p| p.rotation).collect();
            let world = crate::bone_math::compute_world(&positions, &rotations, &parents);
            for (ci, &(root, mid, tip)) in chains.iter().enumerate() {
                let Some((wr, wm, wt)) = world
                    .get(root)
                    .zip(world.get(mid))
                    .zip(world.get(tip))
                    .map(|((a, b), c)| (a, b, c))
                else {
                    continue;
                };
                let thigh = [f64::from(wr[3]), f64::from(wr[7]), f64::from(wr[11])];
                let knee = [f64::from(wm[3]), f64::from(wm[7]), f64::from(wm[11])];
                let foot = [f64::from(wt[3]), f64::from(wt[7]), f64::from(wt[11])];
                let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
                let len = |a: [f64; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                let l1 = len(sub(knee, thigh));
                let l2 = len(sub(foot, knee));
                let l3 = len(sub(foot, thigh));
                // ⚠️ **不要在这里 `continue` 跳过 `l1 == 0` / `l3 == 0`。**
                //
                // 官方没有这个保护：`bend = ((dot / (l1*l3)) + 1) / 2`
                // 在 `l1 == 0` 时算的是 `0/0 = NaN`，然后
                // `kneeDir += tmp * NaN` 让**累加器整体变成 NaN**，
                // 最后 `VectorNormalize` 把 NaN 原样写进产物。
                //
                // 实测（`docs/_probe/probe_kneedir_nan.js`）：`v_smg_mp5` 的
                // `rhand` 链官方 `kneeDir = [NaN, NaN, NaN]`，
                // 而「发现非法就跳过」的写法会写出一个**有限的单位向量**。
                //
                // ⚠️ `VectorNormalize` **不会**把 0 变成 NaN ——
                // `mathlib_base.cpp:70-73` 有 `1/(radius + FLT_EPSILON)` 保护。
                // 所以 NaN 只可能来自**输入已经是 NaN**。
                // **「除法有保护」和「结果有限」是两件事。**
                let half = [
                    (foot[0] + thigh[0]) * 0.5,
                    (foot[1] + thigh[1]) * 0.5,
                    (foot[2] + thigh[2]) * 0.5,
                ];
                let mut ikd = sub(knee, half);
                let n = len(ikd);
                if n > 0.0 {
                    ikd = [ikd[0] / n, ikd[1] / n, ikd[2] / n];
                }
                if l3 > (l1 + l2) * 0.999 {
                    needs_fixup[ci] = true;
                } else {
                    // `VectorIRotate(ikKneeDir, boneToWorld[root], tmp)` ——
                    // **世界 → 骨骼局部**。
                    //
                    // ⚠️ `VectorIRotate` 是 `VectorRotate` 的**转置**：
                    // ```c
                    // void VectorIRotate( const Vector& in1, const matrix3x4_t& in2, Vector& out ) {
                    //     out[0] = in1[0]*in2[0][0] + in1[1]*in2[1][0] + in1[2]*in2[2][0];
                    //     out[1] = in1[0]*in2[0][1] + in1[1]*in2[1][1] + in1[2]*in2[2][1];
                    //     out[2] = in1[0]*in2[0][2] + in1[1]*in2[1][2] + in1[2]*in2[2][2];
                    // }
                    // ```
                    // 而 `matrix3x4_t` 是 `float m[3][4]`（**列主**），所以
                    // `in2[j][i]` = 第 j 列第 i 行 = 本仓库扁平表示的 `m[j*4 + i]`。
                    //
                    // 用错成 `VectorRotate`（正向旋转）会得到一个**模长仍为 1
                    // 但方向错**的向量 —— 实测点积 −0.29 ~ −0.77，
                    // **看起来「像」对的，很难发现**。
                    let m = wr;
                    let tmp = [
                        ikd[0] * f64::from(m[0])
                            + ikd[1] * f64::from(m[4])
                            + ikd[2] * f64::from(m[8]),
                        ikd[0] * f64::from(m[1])
                            + ikd[1] * f64::from(m[5])
                            + ikd[2] * f64::from(m[9]),
                        ikd[0] * f64::from(m[2])
                            + ikd[1] * f64::from(m[6])
                            + ikd[2] * f64::from(m[10]),
                    ];
                    let vt = sub(thigh, knee);
                    let vf = sub(foot, knee);
                    let dot = vt[0] * vf[0] + vt[1] * vf[1] + vt[2] * vf[2];
                    let bend = ((dot / (l1 * l3)) + 1.0) / 2.0;
                    for k in 0..3 {
                        acc[ci][k] += tmp[k] * bend;
                    }
                    has_knees[ci] = true;
                }
            }
        }
    }

    for (ci, &(_, _, _)) in chains.iter().enumerate() {
        // ⚠️ 没有任何一帧「太直」⟹ 官方**不写回**（`simplify.cpp:2902-2903`）。
        if !needs_fixup[ci] || !has_knees[ci] {
            continue;
        }
        let n =
            (acc[ci][0] * acc[ci][0] + acc[ci][1] * acc[ci][1] + acc[ci][2] * acc[ci][2]).sqrt();
        // ⚠️ **`n` 可能是 NaN**（累加器被 `0/0` 污染，见上面 `bend` 的说明）。
        // `NaN <= 0.0` 是 **false** ⟹ 会走到下面的除法，
        // `NaN / NaN = NaN` ⟹ 写出 NaN —— 与官方一致。
        //
        // 这里**故意**不写 `if !n.is_finite() { continue; }`：
        // 那会让 mdlc 写出一个有限值，而官方写的是 NaN。
        // 引擎对 NaN 与有限值的处理不同（NaN 会让 IK 解算整体失效），
        // **复刻官方行为比「修正」它更正确**。
        if n == 0.0 {
            continue;
        }
        let d = [
            (acc[ci][0] / n) as f32,
            (acc[ci][1] / n) as f32,
            (acc[ci][2] / n) as f32,
        ];
        compiled.desc.ikchains[pending[ci]].knee_dir = Some(d);
    }
}

/// 把每个顶点搬到**最终参考姿态**的空间（`RemapVerticesToGlobalBones`，
/// `simplify.cpp:5156-5241`）。
///
/// # 为什么必须做
///
/// SMD 里的顶点位置是在 **SMD 自己的骨架空间**里画的。一旦参考姿态被改写
/// （`$definebone` 给了不同的 `pos`/`rot`、`$unlockdefinebones` 让 SMD 覆盖、
/// 或 `$realignbones`/`$ikchain` 触发重排），**骨骼表**与**顶点**就处在
/// 两个不同的空间里。引擎算 `boneToWorld · poseToBone⁻¹ · v` 时，
/// 参考姿态下看着还行，一动起来整体偏掉 —— 表现就是**模型炸开**。
///
/// 官方逐顶点、逐权重骨骼累加（`:5179-5231`）：
///
/// ```c
/// BuildRawTransforms( psource, 0, srcBoneToWorld );        // SMD 骨架第 0 帧
/// TranslateAnimations( psource, srcBoneToWorld, destBoneToWorld );
/// //   destBoneToWorld[k] = srcBoneToWorld[q] ∘ g_bonetable[k].srcRealign   (:1527)
///
/// VectorITransform( vertex.position, destBoneToWorld[k], tmp1 );  // → 骨骼局部
/// VectorTransform( tmp1, g_bonetable[k].boneToPose, tmp2 );       // → 新参考世界
/// VectorMA( vdest, weight[n], tmp2, vdest );                      // 按权重累加
/// ```
///
/// 即 `v_new = Σ_k w_k · M_k · v_old`，其中
/// **`M_k = boneToPose[k] ∘ destBoneToWorld[k]⁻¹`**。
///
/// 法线同理，但只用旋转部分（`VectorIRotate` / `VectorRotate`），
/// 累加后统一 `VectorNormalize`（`:5226-5237`）。
///
/// # 两个矩阵在 mdlc 里的来源
///
/// | 官方 | mdlc |
/// |---|---|
/// | `destBoneToWorld[k]` | [`internal_bone_world`]（已含 `srcRealign` 口径） |
/// | `g_bonetable[k].boneToPose` | [`resolve_bone_pose`] 算出的**骨骼表**世界矩阵 |
///
/// 两者的差别恰好就是「参考姿态是否被改写」—— 没有改写时 `M_k` 是**精确**
/// 单位阵（`destBoneToWorld[k] == boneToPose[k]`），所以本函数可以直接返回
/// `false` 跳过，产物逐字节不变。
///
/// # 为什么判「单位阵」必须逐位比较
///
/// 用 `abs() < eps` 会把「几乎不改写」的情形也判成恒等，于是**漏掉**真实的
/// 重映射；而 `M_k = A⁻¹ ∘ A` 在浮点下**未必**逐位等于单位阵，用逐位比较
/// 可能把恒等情形误判成需要重映射（多算一次，引入 1 ulp 误差）。
/// 这里取**逐位**判据并配 `parity` 101 个产物做兜底 —— 一旦某个既有夹具
/// 被误判，`parity_snapshot.js` 会立刻变红。
///
/// # 调用时序
///
/// 官方 `SimplifyModel()`（`simplify.cpp:7220-7262`）里它在
/// `RealignBones()` **之后**、`UnifyLODs()` **之前**。mdlc 的
/// `build_model_lods` 是在 bodypart 循环里跑的（早于重排定稿），
/// 但它**克隆** `mesh.vertices` 当 LOD 0 池（`compile.rs:244`），
/// 所以只要在克隆**之前**改好 `mesh.vertices`，统一池自然跟着对。
fn remap_vertices_to_reference_pose(compiled: &mut CompiledModelDesc) -> bool {
    let desc = &compiled.desc;
    let n = desc.bones.len();
    if desc.model.static_prop || n == 0 {
        return false;
    }

    // ---- 结构判据：参考姿态**有没有被改写** ----
    //
    // 官方对**每根**骨骼都算 `M_k = boneToPose[k] ∘ destBoneToWorld[k]⁻¹`，
    // 但只有在「参考姿态与源姿态不同」时它才不是恒等。没有下列任一触发时
    // 两者语义相同，**必须整个跳过** —— 否则 `A ∘ A⁻¹` 的浮点残差会经法线
    // 归一化写进 VVD，让**所有**既有产物变字节。
    //
    // ⚠️ 判据必须用**结构信号**，不能用「矩阵是否逐位相同」：
    // `resolve_bone_pose` 会做 `canonical_euler` 规范化（四元数→矩阵→欧拉），
    // 而 `source_bone_pose` 原样返回 SMD 弧度 —— 两者**语义相同但浮点不同**
    // （实测 `blend3` 夹具：无任何 `$definebone`，却因这一条让产物变字节）。
    //
    // 三类触发（与官方会分叉的三条路径一一对应）：
    //   1. `$definebone` / `$importbone` → 骨骼表取了显式姿态
    //   2. `$realignbones` / `$ikchain`  → `RealignBones` 改写了世界矩阵
    //   3. 12 数字形式 → 显式 `srcRealign` 搬动源骨架
    let has_explicit_pose = desc
        .bones
        .iter()
        .any(|b| b.position.is_some() || b.rotation.is_some());
    let has_explicit_realign = desc
        .bones
        .iter()
        .any(|b| b.explicit_src_realign().is_some());
    if !has_explicit_pose && !has_explicit_realign && compiled.realigned.is_none() {
        return false;
    }

    let parents = bone_parents(desc);

    // `boneToPose[k]`：骨骼表最终的参考世界矩阵。
    let table: Vec<([f32; 3], [f32; 3])> = (0..n)
        .map(|i| resolve_bone_pose(desc, compiled, i))
        .collect();
    let table_world = crate::bone_math::compute_world(
        &table.iter().map(|p| p.0).collect::<Vec<_>>(),
        &table.iter().map(|p| p.1).collect::<Vec<_>>(),
        &parents,
    );

    // `g_bonetable[k].srcRealign` —— 与源无关，先算好。
    //
    // `compiled.realigned.src_realign` 已把 `$definebone` 的显式值覆盖进去了
    // （`compute_realigned_poses` 末尾的 `explicit` 循环），所以有它就优先用；
    // 否则退回骨骼自己声明的 `srcRealign`（12 数字形式，或
    // `$unlockdefinebones` 下 `$definebone` 仍在骨骼表里的情形）。
    let src_realign: Vec<crate::bone_math::Matrix3x4> = (0..n)
        .map(|k| {
            compiled
                .realigned
                .as_ref()
                .map(|r| r.src_realign[k])
                .or_else(|| desc.bones[k].explicit_src_realign())
                .unwrap_or(crate::bone_math::IDENTITY)
        })
        .collect();

    // `destBoneToWorld[k]`：官方 `TranslateAnimations()`（`simplify.cpp:1498-1564`）。
    //
    // ⚠️ **逐 model（= 逐源）算一套**，与官方「`for (i = 0; i < g_numsources; i++)`
    // 里各自 `BuildRawTransforms` + `TranslateAnimations`」一一对应 —— 每个源的
    // 骨架可以不同，用同一个 `m` 的顶点配另一个源的骨架是错的。
    //
    // 两根分支：
    //
    // * `source_world[k] == Some(src)`（官方 `boneGlobalToLocal[k] != -1`，即
    //   **名字精确命中**）⟹ `dest[k] = src ∘ srcRealign[k]`（`:1527`）。
    //   这里的 `src` 是**源自己的第 0 帧世界矩阵**（源父链），不是骨骼表父链。
    // * `None`（官方 `q == -1`）⟹ `dest[k] = dest[g_bonetable[k].parent] ∘
    //   local(g_bonetable[k])`（`:1512-1524`），即用**骨骼表**的局部姿态沿
    //   **骨骼表**的父链累乘。
    //
    // ⚠️ **不能用 [`internal_bone_world`]** —— 那个函数的口径是官方的
    // `g_bonetable[k].boneToPose`（hitbox / 姿态包围盒用），它对
    // **非** pre-aligned 骨骼返回的正是 `table_world[k]`，于是与本函数的
    // `boneToPose[k]` **逐位相同** ⟹ `M_k` 被判成单位阵 ⟹ **漏掉重映射**。
    //
    // 真实反例（`v_autoshotgun`，实测顶点差 **62.886765**）：顶点绑在
    // SMD-only 的 `weapon` 上，而 `weapon` 的祖先是 `$definebone` 覆盖过的
    // `ValveBiped.ValveBiped`。`weapon` 自己没被重排（`srcRealign = I`），
    // 但它的**源**世界矩阵带着根骨骼的 `z = −62.886765`，而骨骼表里根骨骼
    // 被 `$definebone` 挪到了原点 —— 两者不同，顶点必须跟着挪。
    let identity = crate::bone_math::IDENTITY;

    // `M_k = boneToPose[k] ∘ destBoneToWorld[k]⁻¹`。
    //
    // ⚠️ **逐骨骼判「两个世界矩阵是否逐位相同」**，相同就直接取**精确**单位阵。
    //
    // 不能一律算 `concat(table_world[k], invert(dest[k]))`：对「参考姿态没被
    // 改写」的骨骼，`dest[k]` 与 `table_world[k]` 是**同一个矩阵**，
    // 但 `A ∘ A⁻¹` 在浮点下**不**逐位等于单位阵（约 1 ulp 残差）。
    // 那点残差经法线归一化后会写进 VVD ⟹ 让**所有**既有产物变字节。
    //
    // 判据用 `dest[k]` 与 `table_world[k]` 的**逐位相等**（而不是判 `M_k`
    // 是否接近单位阵）—— 前者是「官方这一步对这根骨骼确实是恒等」的
    // 充分条件，且与浮点误差无关。
    let mats_for = |source_world: &[Option<crate::bone_math::Matrix3x4>]| {
        let mut dest: Vec<crate::bone_math::Matrix3x4> = Vec::with_capacity(n);
        for k in 0..n {
            let d = match source_world.get(k).copied().flatten() {
                Some(sw) => crate::bone_math::concat(&sw, &src_realign[k]),
                None => {
                    let local = crate::bone_math::local_transform(table[k].0, table[k].1);
                    match parents[k] {
                        p if p >= 0 => crate::bone_math::concat(&dest[p as usize], &local),
                        _ => local,
                    }
                }
            };
            dest.push(d);
        }
        dest.iter()
            .zip(table_world.iter())
            .map(|(d, t)| {
                let same = d
                    .iter()
                    .zip(t.iter())
                    .all(|(a, b)| a.to_bits() == b.to_bits());
                if same {
                    identity
                } else {
                    crate::bone_math::concat(t, &crate::bone_math::invert(d))
                }
            })
            .collect::<Vec<crate::bone_math::Matrix3x4>>()
    };

    let mut moved = false;
    for bp in &mut compiled.bodyparts {
        for m in &mut bp.models {
            let mats = mats_for(&m.source_world);

            // 全是精确单位阵 ⟹ 官方这一步对**这个源**是恒等变换，跳过
            // （产物逐字节不变）。
            if mats.iter().all(|mm| {
                mm.iter()
                    .zip(identity.iter())
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            }) {
                continue;
            }
            moved = true;

            for mesh in &mut m.meshes {
                remap_vertex_slice(&mut mesh.vertices, &mats, n);
            }
            // 多 LOD 的统一池是**独立**的一份（`build_model_lods` 在
            // bodypart 循环里跑，早于本步），所以必须**同样**改一次 ——
            // 只改 `meshes` 会让 LOD 0 与其它 LOD 处在不同空间。
            if let Some(lods) = &mut m.lods {
                for ml in &mut lods.meshes {
                    remap_vertex_slice(&mut ml.vertices, &mats, n);
                }
            }
        }
    }
    moved
}

/// 把一组顶点按 `M_k` 重映射（[`remap_vertices_to_reference_pose`] 的内层）。
///
/// 抽出来是为了让 `mesh.vertices` 与 `lods.meshes[].vertices` 走**同一条**
/// 代码 —— 两处各写一份迟早会分叉，而分叉的后果是 LOD 之间空间不一致。
fn remap_vertex_slice(
    verts: &mut [crate::model::Vertex],
    mats: &[crate::bone_math::Matrix3x4],
    n: usize,
) {
    for v in verts.iter_mut() {
        let mut p = [0.0f32; 3];
        let mut nr = [0.0f32; 3];
        for pair in &v.bones {
            let k = pair[0] as usize;
            let w = pair[1];
            if k >= n || w == 0.0 {
                continue;
            }
            let mm = &mats[k];
            let t = transform_point(mm, v.pos);
            // 法线只用旋转部分（不含平移列）。
            let r = [
                mm[0] * v.normal[0] + mm[1] * v.normal[1] + mm[2] * v.normal[2],
                mm[4] * v.normal[0] + mm[5] * v.normal[1] + mm[6] * v.normal[2],
                mm[8] * v.normal[0] + mm[9] * v.normal[1] + mm[10] * v.normal[2],
            ];
            for a in 0..3 {
                p[a] += w * t[a];
                nr[a] += w * r[a];
            }
        }
        v.pos = p;
        // `VectorNormalize`（`:5237`）—— 零向量时保持原值。
        let len = (nr[0] * nr[0] + nr[1] * nr[1] + nr[2] * nr[2]).sqrt();
        if len > 0.0 {
            v.normal = [nr[0] / len, nr[1] / len, nr[2] / len];
        }
    }
}

/// 计算骨骼轴重对齐后的局部姿态；不需要重对齐时返回 `None`。
///
/// # `childbone[]` 的两条来源（`simplify.cpp:4236-4286`）
///
/// 1. **`$ikchain`** —— 对每条链填相邻两段：
///    `childbone[link[0]] = link[1]`、`childbone[link[1]] = link[2]`。
///    链的三段由 [`crate::mdl_writer`] 同一套规则推出（末端 / 父 / 祖父）。
/// 2. **`$realignbones`** —— 额外把所有「父骨骼只有唯一子骨骼」的
///    `parent → child` 也填进去。
///
/// 两条路径**都**可能被触发，官方是依次执行（先 ikchain 后 realignbones），
/// 后面的赋值会覆盖前面的 —— 这里照做。
///
/// # 返回 `None` 的两种情形
///
/// - 两条路径都没触发（`childbone[]` 全 -1）；
/// - 触发了但**没有一根**骨骼满足判据（都已沿 +X）—— 此时重排是恒等变换，
///   返回 `Some(原值)` 与 `None` 等价，但省掉 `None` 分支的判断开销不重要，
///   真正的好处是**保持语义清晰**：`Some` 表示「官方确实跑了 RealignBones」。
fn compute_realigned_poses(compiled: &CompiledModelDesc) -> Option<crate::model::RealignedBones> {
    let desc = &compiled.desc;
    let n = desc.bones.len();
    if n == 0 {
        return None;
    }
    let bone_index = desc.bone_index();
    let parents: Vec<i32> = desc
        .bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => bone_index.get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect();

    // `bPreAligned`（`simplify.cpp:4299`）：写了 `$definebone` 的骨骼被
    // `RealignBones` **整个跳过**。判定规则见
    // [`crate::model::Bone::is_pre_aligned`]。
    let pre_aligned: Vec<bool> = desc.bones.iter().map(|b| b.is_pre_aligned()).collect();
    // 显式 `srcRealign`（`$definebone` 的**后 6 个数字**，`simplify.cpp:3652`）。
    let explicit: Vec<Option<crate::bone_math::Matrix3x4>> = desc
        .bones
        .iter()
        .map(|b| b.explicit_src_realign())
        .collect();

    let mut childbone = vec![-1i32; n];
    let mut any = false;

    // 1) `$ikchain`（`LinkIKChains`，`simplify.cpp:5604-5638`）。
    for ch in &desc.ikchains {
        let Some(&tip) = bone_index.get(ch.bone.as_str()) else {
            continue;
        };
        let Ok(mid) = usize::try_from(parents[tip]) else {
            continue;
        };
        let Ok(root) = usize::try_from(parents[mid]) else {
            continue;
        };
        // 与写出器同序：link[0]=祖父、link[1]=父、link[2]=末端。
        childbone[root] = mid as i32;
        childbone[mid] = tip as i32;
        any = true;
    }

    // 2) `$realignbones`（`simplify.cpp:4261-4286`）。
    if desc.model.realign_bones {
        let mut children = vec![0i32; n];
        for &p in &parents {
            if p >= 0 {
                children[p as usize] += 1;
            }
        }
        for (k, &p) in parents.iter().enumerate() {
            if p >= 0 && children[p as usize] == 1 {
                childbone[p as usize] = k as i32;
                any = true;
            }
        }
    }

    // 显式 `srcRealign`（`$definebone` 的 12 数字形式）**本身**也要搬运动画帧，
    // 哪怕一根骨骼都没被重排 —— 所以它单独就能让本函数返回 `Some`。
    if explicit.iter().any(|e| e.is_some()) || !desc.model.forced_realign.is_empty() {
        any = true;
    }

    if !any {
        return None;
    }

    // 原始局部姿态：**不能**走 `resolve_bone_pose`（它会读 `realigned_poses`，
    // 而此时正是 `None`，所以其实是安全的；但显式取原始值更能表明意图）。
    let orig: Vec<([f32; 3], [f32; 3])> = (0..n)
        .map(|i| resolve_bone_pose(desc, compiled, i))
        .collect();
    let positions: Vec<[f32; 3]> = orig.iter().map(|p| p.0).collect();
    let rotations: Vec<[f32; 3]> = orig.iter().map(|p| p.1).collect();

    let forced: Vec<_> = desc
        .model
        .forced_realign
        .iter()
        .map(|forced| {
            let bone = desc
                .bones
                .iter()
                .position(|bone| bone.name.eq_ignore_ascii_case(&forced.bone))
                .unwrap();
            (bone, forced.rotation.map(f32::to_radians))
        })
        .collect();
    let (poses, mut src_realign) = crate::bone_math::realign_bones_with_forced(
        &positions,
        &rotations,
        &parents,
        &childbone,
        &pre_aligned,
        &forced,
    );

    // 显式给出的 `srcRealign` **覆盖**推导值。
    //
    // 官方两条路径合起来正是这个效果：`simplify.cpp:3652` 把 importbone 的
    // `srcRealign` 拷进骨骼表，而 4390-4401 的推导循环对 pre-aligned 骨骼
    // **整个跳过** —— 于是文件里留下的就是 `$definebone` 给的那个矩阵。
    //
    // 反过来说：**pre-aligned 骨骼的 `srcRealign` 不是** `srcWorld⁻¹ ∘ newWorld`，
    // 因为它的世界矩阵压根没被重排过（推导出来会恒等于单位阵）。
    for (k, m) in explicit.iter().enumerate() {
        if let Some(m) = m {
            src_realign[k] = *m;
        }
    }

    // 一根骨骼都**没真正动过**（例如全部骨骼都是 pre-aligned，`ipq2`）时
    // 返回 `None`：
    //
    // - `src_realign` 全是单位阵，`poses` 与原值逐位相同 —— 语义上与
    //   「官方跑了 `RealignBones` 但什么都没改」等价；
    // - 但保留 `Some` 会让 `realign_sequence_frames` 白跑一遍，把动画帧
    //   从 `world` 反解回 `local`，**凭空引入一次浮点往返误差**。
    //
    // 所以这里宁可返回 `None`。（触发过重排、或给了显式 `srcRealign` 时
    // 仍然返回 `Some`。）
    let untouched = src_realign.iter().all(|m| {
        m.iter()
            .zip(crate::bone_math::IDENTITY.iter())
            .all(|(a, b)| a == b)
    }) && poses.iter().zip(orig.iter()).all(|(a, b)| a == b);
    if untouched {
        return None;
    }

    Some(crate::model::RealignedBones { poses, src_realign })
}

/// 自动生成 hitbox（`SetupHitBoxes`，`simplify.cpp:6884-6973`）。
///
/// # 算法（逐步对应源码）
///
/// 1. **bbox 起点**（`simplify.cpp:6899-6909`）：`g_bUseBoneInBBox` 在
///    `studiomdl.cpp:57` 默认 **true**，所以每根骨骼的 bbox 从 `(0,0,0)` 起步。
///    只有 QC 写了 `$skipboneinbbox`（`Cmd_SkipBoneInBBox`，
///    `studiomdl.cpp:5706`）才改成 `±9999`。
///    > 起点为 0 意味着「原点恒在 bbox 内」—— 实测产物里 `bbmin` 经常
///    > 出现 `0.00`，就是这个原因。
///    >
///    > 语料里 3104 个自动生成 hitbox 的模型有 **21 个**是 `±9999` 形态，
///    > 用「原点是否在 box 内」可干净二分（3082 全含 / 21 全不含 / 1 混合），
///    > 且那 21 个**全部**是 `static_prop` 碎片模型
///    > （`probe_skipboneinbbox.js`）。
/// 2. **遍历所有顶点，对每一组权重骨骼都算**（`simplify.cpp:6920-6935`）：
///    ```cpp
///    for (n = 0; n < globalBoneweight.numbones; n++) {
///        k = globalBoneweight.bone[n];
///        VectorITransform( vertex[j].position, g_bonetable[k].boneToPose, p );
///        // 并入骨骼 k 的 bbox
///    }
///    ```
///    注意是**每一组**权重都算，不是只算主导骨骼。
/// 3. **并入子骨骼位置**（`simplify.cpp:6937-6948`）：对每根有 parent 的
///    骨骼 k，把 `g_bonetable[k].pos`（子骨骼在**父空间**的局部位移）
///    并入**父骨骼** j 的 bbox。
/// 4. **过滤**（`simplify.cpp:6950-6955`）：只保留三轴厚度**都 > 1** 的骨骼
///    （`bmin[a] < bmax[a] - 1`），按**骨骼下标顺序**生成 hitbox。
///    一个都没通过时 set 仍存在但 `numhitboxes == 0` —— 语料里有 166 个
///    这样的模型，是**合法状态**。
///
/// # 矩阵方向
///
/// 源码写的是 `VectorITransform(pos, g_bonetable[k].boneToPose)`，但
/// `g_bonetable` 是**内部**结构 `s_bone_t`，其 `boneToPose` 与**文件里**的
/// `mstudiobone_t.poseToBone`（偏移 `0x60`）**互为逆矩阵**。所以
/// `VectorITransform(pos, boneToPose) ≡ pos × poseToBone` —— 对**文件矩阵**
/// 做普通正向变换。
///
/// 这一条已用 `dump_hboxes` 的**内部真值**验证：`docs/_probe/verify_hbox_algo.js`
/// 在受控实验 `iph1` 上 **4/4 逐字段命中**（含 3 组权重混合与链式父子骨骼）。
///
/// # 权重口径（澄清 PROGRESS.md 的一处误判）
///
/// 早先怀疑「VVD 只有 3 组权重、而内部 `globalBoneweight` 上限 4」是
/// 12% 不符的原因。**不是**：
///
/// - `studiomdl.h:36` 的 `MAXSTUDIOBONEWEIGHTS` 就是 **3**；
/// - `v1support.cpp:174` 在解析 SMD 时已经调
///   `SortAndBalanceBones(iCount, MAXSTUDIOBONEWEIGHTS, ...)` 裁到 3 组
///   （并且丢掉权重 < 0.05 的轴）；
/// - `UnifyLODs.cpp:1238` 的 `Assert(... <= 4)` 只在「骨骼折叠后不同局部
///   骨骼映射到同一全局骨骼并累加」（`simplify.cpp:5210-5215`）时才可能触及。
///
/// 所以 **VVD 的 3 组就是全部权重**。用 VVD 顶点重算的实测命中率是
/// **391/392**（`verify_autohitbox_recheck.js`），唯一那个「失败」样本
/// 经查是**脚本自己多转了一次** `Rz(90°)`（它是 `static_prop`，几何已被
/// 旋转过）—— 即算法本身 **392/392** 正确。
fn auto_hitboxes(compiled: &CompiledModelDesc) -> Vec<crate::model::Hitbox> {
    let desc = &compiled.desc;
    let n = desc.bones.len();
    if n == 0 {
        return Vec::new();
    }

    // 参考姿态：必须与写进 `mstudiobone_t` 的值**完全一致**，所以走同一个
    // [`resolve_bone_pose`]（它会回退到 SMD 第 0 帧并做 `canonical_euler`）。
    let poses: Vec<([f32; 3], [f32; 3])> = (0..n)
        .map(|i| resolve_bone_pose(desc, compiled, i))
        .collect();
    let parents: Vec<i32> = desc
        .bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => desc.bone_index().get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect();

    // bbox 起点：`g_bUseBoneInBBox` 默认 **true** → 全 0 起步。
    // 置了 `$skipboneinbbox` 则从 ±9999 起步（`simplify.cpp:6899-6909`）。
    let start_at_zero = !desc.model.skip_bone_in_bbox;
    let init = if start_at_zero { 0.0f32 } else { 9999.0 };
    let init_max = if start_at_zero { 0.0f32 } else { -9999.0 };
    let mut bmin = vec![[init; 3]; n];
    let mut bmax = vec![[init_max; 3]; n];

    // 世界矩阵走 **[源骨架] ∘ `srcRealign`**，**不是**骨骼表最终的参考姿态
    // —— 见 [`source_bone_pose`] 的说明。
    let hit_ptb: Vec<crate::bone_math::Matrix3x4> = internal_bone_world(compiled, &parents)
        .iter()
        .map(crate::bone_math::invert)
        .collect();

    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                for v in &mesh.vertices {
                    for pair in &v.bones {
                        let k = pair[0] as usize;
                        if k >= n {
                            continue;
                        }
                        // `VectorITransform(pos, boneToPose)` 对**文件矩阵**
                        // 等价于正向 `VectorTransform`。
                        let p = transform_point(&hit_ptb[k], v.pos);
                        for a in 0..3 {
                            if p[a] < bmin[k][a] {
                                bmin[k][a] = p[a];
                            }
                            if p[a] > bmax[k][a] {
                                bmax[k][a] = p[a];
                            }
                        }
                    }
                }
            }
        }
    }

    // 并入子骨骼位置（子骨骼在父空间的局部位移 → 父骨骼的 bbox）。
    for k in 0..n {
        let j = parents[k];
        if j < 0 {
            continue;
        }
        let j = j as usize;
        let pos = poses[k].0;
        for a in 0..3 {
            if pos[a] < bmin[j][a] {
                bmin[j][a] = pos[a];
            }
            if pos[a] > bmax[j][a] {
                bmax[j][a] = pos[a];
            }
        }
    }

    // 过滤：三轴厚度都 > 1。
    let mut out = Vec::new();
    for k in 0..n {
        if bmin[k][0] < bmax[k][0] - 1.0
            && bmin[k][1] < bmax[k][1] - 1.0
            && bmin[k][2] < bmax[k][2] - 1.0
        {
            out.push(crate::model::Hitbox {
                bone: desc.bones[k].name.clone(),
                group: Some(0),
                bbmin: bmin[k],
                bbmax: bmax[k],
                name: None,
            });
        }
    }
    out
}

/// 每根骨骼的**渲染包围盒**（`SetupFullBoneRenderBounds`，`simplify.cpp:7014`）。
///
/// # 用途
///
/// `CalcSequenceBoundingBoxes()`（`simplify.cpp:7049`）在算每个序列的包围盒时，
/// 除了蒙皮顶点，还会把每根骨骼的渲染包围盒用 `bonetransform` 变换后**并入**
/// （`simplify.cpp:7120-7130`）。这个「渲染包围盒」由两部分构成：
///
/// 1. **自动 hitbox 的 bbox** —— 即 `g_bonetable[i].bmin/bmax`
///    （`SetupHitBoxes` 填的，见 `auto_hitboxes`）；
/// 2. **显式 hitbox** —— `simplify.cpp:7030-7045` 把每个 hitbox 的
///    `bmin/bmax` 按骨骼并进去。
///
/// # 实测印证
///
/// `ip`（无 `$hbox`）：几何 z ∈ [-8,-8]，自动 hitbox 因**全 0 起步**而得到
/// `bbmax[2] == 0`。于是序列包围盒的 `bbmax[2]` 是 **0** 而不是 **-8** ——
/// 官方 `ip_official.mdl` 的 `hull_max = [8,8,0]` 正是这么来的。
pub fn bone_render_bounds(
    desc: &ModelDesc,
    compiled: &CompiledModelDesc,
) -> Vec<([f32; 3], [f32; 3])> {
    let n = desc.bones.len();
    let mut out = vec![([0.0f32; 3], [0.0f32; 3]); n];
    let index = desc.bone_index();

    // 1) 自动 hitbox 的 bbox（= `g_bonetable[].bmin/bmax`）。
    if desc.hitboxes.autogenerated {
        let auto = auto_hitboxes(compiled);
        for hb in &auto {
            if let Some(&k) = index.get(hb.bone.as_str()) {
                for a in 0..3 {
                    out[k].0[a] = out[k].0[a].min(hb.bbmin[a]);
                    out[k].1[a] = out[k].1[a].max(hb.bbmax[a]);
                }
            }
        }
    }

    // 2) 显式 hitbox。
    for hb in desc.hitbox_sets().flat_map(|set| &set.boxes) {
        if let Some(&k) = index.get(hb.bone.as_str()) {
            for a in 0..3 {
                out[k].0[a] = out[k].0[a].min(hb.bbmin[a]);
                out[k].1[a] = out[k].1[a].max(hb.bbmax[a]);
            }
        }
    }
    out
}

/// 用 `matrix3x4`（行主序 12 个 f32）变换一个点。
fn transform_point(m: &[f32; 12], v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2] + m[3],
        m[4] * v[0] + m[5] * v[1] + m[6] * v[2] + m[7],
        m[8] * v[0] + m[9] * v[1] + m[10] * v[2] + m[11],
    ]
}

/// 一个序列的**摆好姿势**包围盒（`CalcSequenceBoundingBoxes`，
/// `simplify.cpp:7049-7163`）。
///
/// # 为什么要算它
///
/// `write.cpp:2071-2087` 把 `g_sequence[0].bmin/bmax` 直接写进头部的
/// `hull_min/hull_max`。所以 hull **不是**静止姿势的顶点 AABB ——
/// 它来自**逐帧摆姿势后**的顶点包围盒，再加上每根骨骼的渲染包围盒。
///
/// 用静止姿势 AABB 会在有动画时明显偏小，而且**静态道具也会偏**：
/// 自动 hitbox 的 bbox 从全 0 起步，会撑大这个盒子。
///
/// # 算法（逐帧）
///
/// 对序列的**每一帧**：
///
/// 1. 逐骨骼算 `bonetransform[k]`：用该帧的局部姿态
///    （`AngleMatrix(sanim[j][k].rot, sanim[j][k].pos)`）沿父链
///    `ConcatTransforms` 累乘；
/// 2. `posetransform[k] = bonetransform[k] ∘ inverse(boneToPose[k])`
///    —— 即「该帧世界矩阵」与「参考姿态世界矩阵」的相对变换；
/// 3. 把每根骨骼的渲染包围盒用 `bonetransform[k]` 变换后并入
///    （`simplify.cpp:7120-7130` 的 `TransformAABB`）；
/// 4. 把每个顶点用**它绑定的每一根**骨骼的 `posetransform` 变换后
///    加权求和，并入（`simplify.cpp:7141-7158`）。
///    注意源码这里是「先逐骨骼变换再按权重累加」，**不是**「先蒙皮再变换」。
///
/// # `boneToPose`
///
/// `g_bonetable[k].boneToPose` 是**内部**结构里的世界矩阵，与文件里的
/// `mstudiobone_t.poseToBone`（偏移 `0x60`）**互为逆**。
/// 这里用 [`crate::bone_math::compute_pose_to_bone`] 拿到与写出器**完全一致**
/// 的那一份，再取逆得到世界矩阵。
/// 骨骼父链（`-1` = 根），下标与 [`ModelDesc::bones`] 一致。
///
/// 父名查不到时按根处理 —— 与写出器的容错一致（`validate()` 会先报错）。
pub fn bone_parents(desc: &ModelDesc) -> Vec<i32> {
    let index = desc.bone_index();
    desc.bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => index.get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect()
}

/// 一帧的**世界**变换 —— 官方 `CalcBoneTransforms`（`simplify.cpp:4533-4589`）。
///
/// # 根骨骼要先套一层 `panim->rotation`（= `g_defaultrotation`）
///
/// `BuildRawTransforms`（`simplify.cpp:1432-1486`）对根骨骼
/// （`localBone[k].parent == -1`）做：
///
/// ```cpp
/// AngleMatrix( rotate, rootxform );          // rotate = panim->rotation
/// VectorSubtract( pos, shift, tmp );
/// VectorRotate( tmp, rootxform, pos );       // 平移也转
/// AngleMatrix( rot, m );
/// ConcatTransforms( rootxform, m, bonematrix );
/// MatrixAngles( bonematrix, rot );
/// ```
///
/// 而 `panim->rotation` 在 `studiomdl.cpp:2427` 被设为
/// `g_defaultrotation` = `RadianEuler(0, 0, π/2)`。
///
/// **实测印证**（`ipe1`，2 条序列、含真实动画）：不加这一层时
/// 算出的 `seq[1]` 盒子是 `[-19.95,-19.8,0]..[10,20,7]`，
/// 而官方是 `[-20,-19.95,0]..[19.8,10,7]` —— 恰好差一个 `Rz(90°)`
/// （`(x,y,z) → (-y,x,z)`：`-19.95 ↔ -19.8`、`20 → 19.8`…）。
///
/// `$staticprop` 时 `panim->rotation` 被置 0（`simplify.cpp:3379`），
/// 所以那一层**不套** —— 与几何已被旋转的事实一致。
///
/// # 为什么 `frame` 里的是「局部」姿态
///
/// `realign_sequence_frames` 已经把 `srcRealign` 折进
/// 每一帧的局部姿态里，所以这里对根骨骼**再**左乘一次 `Rz(90°)` 就等价于
/// 官方的 `sanim`（它本身也是「父相对」的：`ConvertAnimation` 用
/// `inverse(destBoneToWorld[parent]) ∘ destBoneToWorld[k]` 反解出来）。
/// 全局左乘一个旋转不改变任何非根骨骼的局部姿态，所以两种约定只在根上分叉。
pub fn frame_worlds(
    desc: &ModelDesc,
    parents: &[i32],
    frame: &[crate::smd::SmdPose],
) -> Vec<crate::bone_math::Matrix3x4> {
    let locals: Vec<crate::bone_math::Matrix3x4> = (0..parents.len())
        .map(|k| {
            let p = frame.get(k).copied().unwrap_or(crate::smd::SmdPose {
                bone: k as i32,
                position: [0.0; 3],
                rotation: [0.0; 3],
            });
            // `sanim[j][k].rot` 是**弧度**（`s_bone_t` 与骨骼表同构）。
            crate::bone_math::local_transform(p.position, p.rotation)
        })
        .collect();
    frame_worlds_from_locals(desc, parents, &locals)
}

/// 由**已算好的局部矩阵**求世界矩阵（官方 `CalcBoneTransforms` 的
/// `ConcatTransforms` 那半段）。
///
/// 抽出来是因为 [`delta_local_matrices`] 的局部矩阵**不是**由欧拉角直接建的，
/// 而是「四元数重建」的产物 —— 两条路径必须共用同一段层级合成代码，
/// 否则根骨骼的 `Rz(90°)` 偏置很容易在一边漏掉。
pub fn frame_worlds_from_locals(
    desc: &ModelDesc,
    parents: &[i32],
    locals: &[crate::bone_math::Matrix3x4],
) -> Vec<crate::bone_math::Matrix3x4> {
    let root_rot = if desc.model.static_prop {
        None
    } else {
        Some(crate::bone_math::angle_matrix([
            0.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
        ]))
    };
    let mut world: Vec<crate::bone_math::Matrix3x4> = Vec::with_capacity(parents.len());
    for (k, &parent) in parents.iter().enumerate() {
        let mut local = locals
            .get(k)
            .copied()
            .unwrap_or_else(|| crate::bone_math::local_transform([0.0; 3], [0.0; 3]));
        if parent == -1
            && let Some(rr) = &root_rot
        {
            // `ConcatTransforms(rootxform, m, bonematrix)` —— 左乘。
            local = crate::bone_math::concat(rr, &local);
        }
        let m = match parent {
            -1 => local,
            par => crate::bone_math::concat(&world[par as usize], &local),
        };
        world.push(m);
    }
    world
}

/// **`STUDIO_DELTA` 动画的逐骨骼局部矩阵重建** —— 官方
/// `CalcBoneTransforms` 的 `else` 分支（`simplify.cpp:4562-4578`）。
///
/// # 官方代码
///
/// ```c
/// if (!(panimation->flags & STUDIO_DELTA)) {
///     AngleMatrix( panimation->sanim[frame][k].rot, panimation->sanim[frame][k].pos, bonematrix );
/// } else {
///     AngleQuaternion( pbaseanimation->sanim[0][k].rot, q1 );   // ← 基准动画**第 0 帧**
///     AngleQuaternion( panimation->sanim[frame][k].rot, q2 );  // ← 本帧存的**增量**
///     float s = panimation->weight[k];                          // ← $weightlist，缺省 1
///     QuaternionMA( q1, s, q2, q3 );
///     p3 = pbaseanimation->sanim[0][k].pos + s * panimation->sanim[frame][k].pos;
///     AngleMatrix( q3, p3, bonematrix );
/// }
/// ```
///
/// # 为什么这是个真 bug（不是「等价实现」）
///
/// `subtract`（`subtractBaseAnimations`）把动画变成**增量**：
/// `delta.rot = conj(src.rot) · raw.rot`、`delta.pos = raw.pos − src.pos`。
/// 重建在 `s = 1` 时**精确抵消**它：
/// `q1·q2 = src·conj(src)·raw = raw`、`p3 = src.pos + (raw.pos − src.pos) = raw.pos`。
///
/// 所以官方产物对「有无 `subtract`」**不变**。mdlc 之前把增量直接当完整
/// 姿态喂给 [`frame_worlds`]，增量近零 ⟹ 所有骨骼塌到原点 ⟹ IK 误差 ≈ 0。
///
/// **实测判据**（`docs/_probe/ab_iksub_mdlc.js`，同一份几何只改有无
/// `subtract`，对照官方 `@idle` 的 `mstudiocompressedikerror_t`）：
///
/// | 通道 | 官方（两种情况**相同**） | mdlc 修复前 | mdlc 修复后 |
/// |---|---|---|---|
/// | `pos.x` 采样 | `[2674, 2274, 1826]` | `[0, 0, 0]` | `[2674, 2274, 1826]` |
/// | 通道字节数 | `[8, 8, 4, 4, 4, 4]` | `[4, 4, 4, 4, 4, 4]` | `[8, 8, 4, 4, 4, 4]` |
///
/// # 基准动画是 `g_panimation[0]`
///
/// 官方调用的是单参数版 `CalcBoneTransforms(panimation, frame, …)`
/// （`simplify.cpp:4533-4536`），它固定传 `g_panimation[0]` —— 即 QC 里
/// **第一条** `$animation`，**不是** `subtract` 引用的那条。
/// 所以基准取 [`CompiledModelDesc::animations`]`[0]` 的第 0 帧。
pub fn delta_local_matrices(
    base_frame0: &[crate::smd::SmdPose],
    frame: &[crate::smd::SmdPose],
) -> Vec<crate::bone_math::Matrix3x4> {
    delta_local_matrices_weighted(base_frame0, frame, &[])
}

pub fn delta_local_matrices_weighted(
    base_frame0: &[crate::smd::SmdPose],
    frame: &[crate::smd::SmdPose],
    weights: &[f32],
) -> Vec<crate::bone_math::Matrix3x4> {
    let n = base_frame0.len().max(frame.len());
    (0..n)
        .map(|k| {
            let empty = crate::smd::SmdPose {
                bone: k as i32,
                position: [0.0; 3],
                rotation: [0.0; 3],
            };
            let b = base_frame0.get(k).copied().unwrap_or(empty);
            let f = frame.get(k).copied().unwrap_or(empty);
            let weight = weights.get(k).copied().unwrap_or(1.0);
            let q1 = crate::bone_math::angle_quaternion(b.rotation);
            let q2 = crate::bone_math::angle_quaternion(f.rotation);
            let q3 = crate::bone_math::quaternion_ma(q1, weight, q2);
            // `p3 = pbaseanimation->sanim[0][k].pos + s * panimation->sanim[frame][k].pos`
            let mut p3 = b.position;
            for (dst, src) in p3.iter_mut().zip(f.position.iter()) {
                *dst += weight * src;
            }
            crate::bone_math::quaternion_local_transform(q3, p3)
        })
        .collect()
}

/// [`frame_worlds`] 的 `STUDIO_DELTA` 版本 —— 先重建局部矩阵再合成世界矩阵。
pub fn delta_frame_worlds(
    desc: &ModelDesc,
    parents: &[i32],
    base_frame0: &[crate::smd::SmdPose],
    frame: &[crate::smd::SmdPose],
) -> Vec<crate::bone_math::Matrix3x4> {
    delta_frame_worlds_weighted(desc, parents, base_frame0, frame, &[])
}

pub fn delta_frame_worlds_weighted(
    desc: &ModelDesc,
    parents: &[i32],
    base_frame0: &[crate::smd::SmdPose],
    frame: &[crate::smd::SmdPose],
    weights: &[f32],
) -> Vec<crate::bone_math::Matrix3x4> {
    let locals = delta_local_matrices_weighted(base_frame0, frame, weights);
    frame_worlds_from_locals(desc, parents, &locals)
}

pub fn sequence_pose_bounds(
    desc: &ModelDesc,
    compiled: &CompiledModelDesc,
    seq_index: usize,
    render_bounds: &[([f32; 3], [f32; 3])],
) -> Option<([f32; 3], [f32; 3])> {
    let seq = compiled.sequences.get(seq_index)?;
    let n = desc.bones.len();
    if n == 0 || seq.frames.is_empty() {
        return None;
    }

    // 父链（与写出器同源）。
    //
    // 注意这里**不**再取 `resolve_bone_pose` —— 参考世界矩阵改走
    // [`source_world`]（源骨架 + `srcRealign`），见下面的 `ref_world`。
    let parents: Vec<i32> = bone_parents(desc);

    // 参考世界矩阵走 **[源骨架] ∘ `srcRealign`**（= 官方的
    // `g_bonetable[k].boneToPose`），**不是**骨骼表最终的参考姿态 ——
    // 见 [`source_bone_pose`] / [`source_world`]。
    //
    // 必须与上面 `world[]`（帧世界矩阵）**同源**：`posetransform` 是二者的
    // 相对变换，不同源会凭空多出一次平移（实测 `ipq2` 顶点被推远 2）。
    let ref_world: Vec<crate::bone_math::Matrix3x4> = internal_bone_world(compiled, &parents);

    let mut bmin = [f32::INFINITY; 3];
    let mut bmax = [f32::NEG_INFINITY; 3];

    // `$staticprop`：动画已被 `MakeStaticProp()` 压成 **1 条 1 帧的身份动画**
    // （`simplify.cpp:3362-3380`）——
    //   `rawanim[0][0].pos = 0`、`rawanim[0][0].rot = 0`、
    //   `g_panimation[0]->rotation = 0`、`numframes = 1`
    // 所以包围盒必须用**那一帧**算，而不是原始序列的逐帧姿态。
    //
    // 实测（`ipe2`）：用原始 2 帧算 `seq[0]` 会得到 `bbmax=[0,10,12]`，
    // 而官方是 `[0,10,7]` —— 后者正是「只有身份帧」的结果
    // （`12` 来自 `ipeanim.smd` 第 1 帧把 root 抬到 z=5）。
    let identity_frames: Vec<Vec<crate::smd::SmdPose>> = if desc.model.static_prop {
        let mut row = vec![
            crate::smd::SmdPose {
                bone: 0,
                position: [0.0; 3],
                rotation: [0.0; 3],
            };
            n
        ];
        for (k, p) in row.iter_mut().enumerate() {
            p.bone = k as i32;
        }
        vec![row]
    } else {
        Vec::new()
    };

    // ⚠️ **blend 序列的包围盒是「每一格的并集」。**
    //
    // 官方分两步（`simplify.cpp:7049-7202` 的 `CalcSequenceBoundingBoxes`）：
    //
    // ```c
    // // ① 逐动画算自己的 bmin/bmax（遍历该动画的所有帧）
    // for (i = 0; i < g_numani; i++) { ... g_panimation[i]->bmin = bmin; }
    //
    // // ② 逐**序列**把所有格的盒子求并
    // for (i = 0; i < g_sequence.Count(); i++)
    //   for (j = 0; j < g_sequence[i].groupsize[0]; j++)
    //     for (k = 0; k < g_sequence[i].groupsize[1]; k++) {
    //       s_animation_t *panim = g_sequence[i].panim[j][k];
    //       if (panim->bmin[0] < bmin[0]) bmin[0] = panim->bmin[0];
    //       ...
    //     }
    // ```
    //
    // 早先本实现只用了**第一格**的帧（`seq.frames` 就是第一格的帧），
    // 于是 miku `look_poses`（3 格 → `look_down`/`look_mid`/`look_up`）
    // 的包围盒只有 `look_down` 一格的：
    //
    // | | bbmin | bbmax |
    // |---|---|---|
    // | 官方 | `[-9.681, -15.120, -13.719]` | `[37.516, 5.152, 56.902]` |
    // | mdlc（修前） | `[-5.265, -27.594, -1.5]` | `[3.230, 4.423, 1.5]` |
    //
    // 注意三格的帧数可能不同（官方按**每格自己的** `numframes` 遍历）。
    //
    // ⚠️ **仍未闭合**：miku `look_poses`（seq[0]，3 个 `subtract` 格）的
    // 包围盒与官方差很多（span `[32.05, 9.78, 3.05]` vs `[47.20, 20.27, 70.62]`）。
    //
    // 已**证否**的假设（都实测过，记录在此避免重走）：
    //
    // 1. 「只转根位置、不转根旋转」—— 推理是 `subtract` 在已带 `Rz90` 的
    //    值上做差，旋转里相消、位置里留下一次。改完 bbmin 从
    //    `[-6.551, -27.594, -1.550]` 变成 `[-27.594, -3.230, -1.550]`，
    //    只是把错误换了个轴，离官方更远。
    // 2. 「`write.cpp:2071-2078` 的 `CollisionModel_ExpandBBox` 撑开了 seq[0]」
    //    —— 但官方 `look_poses.bbmin[1] = -15.120` **小于**官方
    //    `hull_min[1] = -12.029`，而 hull 才是被撑开的那个，所以不是。
    // 3. 「每格自己的盒子求并」—— **这条是对的**（`simplify.cpp:7184-7196`），
    //    已修（`idle`/`idle_raw` 因此与官方一致到 1e-6），但不足以解释
    //    `look_poses`：3 个 `subtract` 格的增量都很小，并起来仍然很小。
    //
    // 线索：官方 `CalcSequenceBoundingBoxes` 用 **`AngleMatrix(sanim.rot,
    // sanim.pos)`**（`simplify.cpp:7087`）直接建局部矩阵，**不**走
    // `CalcBoneTransforms` 的 DELTA 合成分支（`simplify.cpp:4568-4575`）。
    // 对 `subtract` 动画，`sanim` 存的是**增量**，所以官方那边等于把
    // 「增量」当成「完整姿态」用 —— 增量近零 ⟹ `bonetransform` 近单位阵
    // ✅ **正解（已实测）**：官方包围盒用**减除之前**的姿态算。
    //
    // 官方 `CalcSequenceBoundingBoxes` 用 `AngleMatrix(sanim.rot, sanim.pos)`
    // （`simplify.cpp:7087`）直接建局部矩阵，**不**走 `CalcBoneTransforms`
    // 的 DELTA 合成分支（`simplify.cpp:4558-4577`）。对 `subtract` 动画
    // `sanim` 是**增量**，于是官方那边 `bonetransform` 近单位阵 ⟹
    // `posetransform = inverse(boneToPose)` ⟹ 顶点被映射到**参考姿态**附近。
    //
    // **决定性判据**（`probe_nosub_bbox.js`）：把 `subtract` 从 miku 的
    // TOML 里去掉再编译，`look_poses` 的包围盒从 **44276588 ULP → 51 ULP**：
    //
    // | 序列 | 带 `subtract` | 去掉 `subtract` |
    // |---|---|---|
    // | `look_poses` | 44276588 ULP | **51 ULP** |
    //
    // 所以这里改用 `pre_subtract_frames`（减除前的原始 SMD 姿态）。
    // 注意**只影响包围盒** —— 写进动画链的仍是 `frames`（减除后）。
    let cell_frames: Vec<&[Vec<crate::smd::SmdPose>]> = if desc.model.static_prop {
        vec![&identity_frames]
    } else if seq.cells.is_empty() {
        vec![seq.pre_subtract_frames.as_ref().unwrap_or(&seq.frames)]
    } else {
        seq.cells
            .iter()
            .filter_map(|c| compiled.animations.get(*c))
            .map(|a| {
                a.pre_subtract_frames
                    .as_ref()
                    .unwrap_or(&a.frames)
                    .as_slice()
            })
            .collect()
    };

    // ---- 蒙皮计划：帧循环外建一次 ----
    //
    // 顶点遍历与帧无关，逐帧变的只有 `posetransform`。原实现把遍历放在
    // 帧循环里，于是每帧都要重走 bodypart/model/mesh 三层、并追一次
    // `Vertex.bones` 的堆指针。真实视角模型上这一步是 `write_mdl` 的
    // 84%（`v_silenced_smg`：35.3 / 41.9 ms，×24 格 × 772 帧）。
    //
    // 拆成两条路径：
    //
    // - **单骨骼顶点**（真实模型 98%）：按骨骼分组，同组顶点在扁平数组里
    //   连续，整组共用一份矩阵，不必每顶点再查一次表。
    // - **多骨骼顶点**：保持原始遍历顺序 —— `pos[a] += w * t[a]` 的累加
    //   顺序影响浮点结果，必须逐位保持。
    //
    // 包围盒的 min/max 与顺序无关（NaN 比较恒假，永远不会被写进去），
    // 所以两条路径的先后不影响结果。
    //
    // `k >= n` 的骨骼在**建表时**就滤掉：原实现在循环里 `continue`，
    // 效果相同（该骨骼不参与累加）。
    let mut singles: Vec<(u32, [f32; 3], f32)> = Vec::new();
    let mut multi: Vec<([f32; 3], Vec<[f32; 2]>)> = Vec::new();
    let mut has_orphan = false;
    for bp in &compiled.bodyparts {
        for m in &bp.models {
            for mesh in &m.meshes {
                for v in &mesh.vertices {
                    let nvalid = v.bones.iter().filter(|p| (p[0] as usize) < n).count();
                    if nvalid == 0 {
                        has_orphan = true;
                    } else if nvalid == 1 {
                        let p = v
                            .bones
                            .iter()
                            .find(|p| (p[0] as usize) < n)
                            .expect("nvalid == 1");
                        singles.push((p[0] as u32, v.pos, p[1]));
                    } else {
                        let mut all = Vec::with_capacity(nvalid);
                        for p in &v.bones {
                            if (p[0] as usize) < n {
                                all.push(*p);
                            }
                        }
                        multi.push((v.pos, all));
                    }
                }
            }
        }
    }
    // 按骨骼排序 ⟹ 同组连续。`sort_unstable_by_key` 对同键元素不保序，
    // 但组内顺序不影响结果：同组顶点各自独立地做一次「变换 + 权重 +
    // 并入包围盒」，没有跨顶点累加。
    singles.sort_unstable_by_key(|s| s.0);
    let ng = singles.len();
    let mut s_px: Vec<f32> = Vec::with_capacity(ng);
    let mut s_py: Vec<f32> = Vec::with_capacity(ng);
    let mut s_pz: Vec<f32> = Vec::with_capacity(ng);
    let mut s_w: Vec<f32> = Vec::with_capacity(ng);
    let mut groups: Vec<(u32, u32, u32)> = Vec::new(); // (骨骼号, lo, hi)
    for (i, &(bone, p, wt)) in singles.iter().enumerate() {
        if groups.last().is_none_or(|g| g.0 != bone) {
            groups.push((bone, i as u32, i as u32));
        }
        groups.last_mut().expect("刚 push 过").2 = i as u32 + 1;
        s_px.push(p[0]);
        s_py.push(p[1]);
        s_pz.push(p[2]);
        s_w.push(wt);
    }

    // 帧循环一次都不跑时（`cells` 全部越界，或每格的帧数都是 0），
    // `bmin`/`bmax` 必须**停在 ±INF** —— 尾部据此返回 `None`。所以下面
    // `has_orphan` 的并入必须以「确实跑过至少一帧」为前提，否则会把
    // ±INF 改写成 `0.0`，把原实现的 `None` 变成 `Some(([0,0,0],[0,0,0]))`。
    let any_frames = cell_frames.iter().any(|f| !f.is_empty());

    for frames in cell_frames {
        for frame in frames {
            // 1) 该帧的局部世界矩阵（官方 `CalcBoneTransforms`）。
            let world = frame_worlds(desc, &parents, frame);

            // 2) `posetransform[k] = world[k] ∘ inverse(ref_world[k])`。
            let posetransform: Vec<crate::bone_math::Matrix3x4> = world
                .iter()
                .zip(ref_world.iter())
                .map(|(w, r)| crate::bone_math::concat(w, &crate::bone_math::invert(r)))
                .collect();

            // 3) 并入每根骨骼的渲染包围盒（用 `bonetransform`，即 `world`）。
            for (k, w) in world.iter().enumerate() {
                let (mn, mx) = render_bounds
                    .get(k)
                    .copied()
                    .unwrap_or(([0.0; 3], [0.0; 3]));
                let (tmn, tmx) = transform_aabb(w, mn, mx);
                for a in 0..3 {
                    if tmn[a] < bmin[a] {
                        bmin[a] = tmn[a];
                    }
                    if tmx[a] > bmax[a] {
                        bmax[a] = tmx[a];
                    }
                }
            }

            // 4) 并入蒙皮后的顶点。
            //
            // 单骨骼顶点按骨骼分组：同组共用一份矩阵，且顶点在扁平数组里
            // 连续，循环体是纯逐元素算术 + min/max 归约，可以直接向量化。
            //
            // `0.0f32 +` 保留 `+0.0` 对 `-0.0` 的归一化（否则 bmin/bmax
            // 可能存下 `-0.0`，与官方逐位不同）。
            //
            // 归约用 `f32::min/max`：它与 `if p < bmin` 在**本处**等价 ——
            // bmin/bmax 初值是 ±INF、且只被非 NaN 值覆盖，所以永远不会是
            // NaN，而 `f32::min` 忽略 NaN 的行为因此不可达。
            for &(bone, lo, hi) in &groups {
                let m = &posetransform[bone as usize];
                let (m0, m1, m2, m3) = (m[0], m[1], m[2], m[3]);
                let (m4, m5, m6, m7) = (m[4], m[5], m[6], m[7]);
                let (m8, m9, m10, m11) = (m[8], m[9], m[10], m[11]);
                let mut mn = [f32::INFINITY; 3];
                let mut mx = [f32::NEG_INFINITY; 3];
                for i in lo as usize..hi as usize {
                    let (x, y, z, w) = (s_px[i], s_py[i], s_pz[i], s_w[i]);
                    let px = 0.0f32 + w * (m0 * x + m1 * y + m2 * z + m3);
                    let py = 0.0f32 + w * (m4 * x + m5 * y + m6 * z + m7);
                    let pz = 0.0f32 + w * (m8 * x + m9 * y + m10 * z + m11);
                    mn[0] = mn[0].min(px);
                    mn[1] = mn[1].min(py);
                    mn[2] = mn[2].min(pz);
                    mx[0] = mx[0].max(px);
                    mx[1] = mx[1].max(py);
                    mx[2] = mx[2].max(pz);
                }
                for a in 0..3 {
                    if mn[a] < bmin[a] {
                        bmin[a] = mn[a];
                    }
                    if mx[a] > bmax[a] {
                        bmax[a] = mx[a];
                    }
                }
            }

            // 多骨骼顶点：保持原始累加顺序（浮点加法不结合）。
            for (vp, bones) in &multi {
                let mut pos = [0.0f32; 3];
                for pair in bones {
                    let k = pair[0] as usize;
                    let t = transform_point(&posetransform[k], *vp);
                    for a in 0..3 {
                        pos[a] += pair[1] * t[a];
                    }
                }
                for a in 0..3 {
                    if pos[a] < bmin[a] {
                        bmin[a] = pos[a];
                    }
                    if pos[a] > bmax[a] {
                        bmax[a] = pos[a];
                    }
                }
            }
        }
    }

    // 全部骨骼都越界的顶点：`pos` 恒为 `[0,0,0]`，原实现照样并进包围盒。
    // 常量只需并一次（min/max 幂等）—— 但**必须**至少跑过一帧，否则
    // `bmin`/`bmax` 仍是 ±INF，此处会把 `None` 变成 `Some([0,0,0])`。
    if has_orphan && any_frames {
        for a in 0..3 {
            if 0.0f32 < bmin[a] {
                bmin[a] = 0.0;
            }
            if 0.0f32 > bmax[a] {
                bmax[a] = 0.0;
            }
        }
    }

    if bmin[0].is_finite() && bmax[0].is_finite() {
        Some((bmin, bmax))
    } else {
        None
    }
}

/// 把一个 AABB 用 `matrix3x4` 变换后求新的 AABB（`TransformAABB`）。
///
/// 做法是变换 8 个角再取 min/max —— 对旋转矩阵这是**精确**的
/// （不像只变换两个角那样会低估）。
fn transform_aabb(
    m: &crate::bone_math::Matrix3x4,
    mn: [f32; 3],
    mx: [f32; 3],
) -> ([f32; 3], [f32; 3]) {
    let mut out_min = [f32::INFINITY; 3];
    let mut out_max = [f32::NEG_INFINITY; 3];
    for i in 0..8 {
        let c = [
            if i & 1 == 0 { mn[0] } else { mx[0] },
            if i & 2 == 0 { mn[1] } else { mx[1] },
            if i & 4 == 0 { mn[2] } else { mx[2] },
        ];
        let t = transform_point(m, c);
        for a in 0..3 {
            if t[a] < out_min[a] {
                out_min[a] = t[a];
            }
            if t[a] > out_max[a] {
                out_max[a] = t[a];
            }
        }
    }
    (out_min, out_max)
}

/// `$staticprop` 的骨骼塌缩（`simplify.cpp:3278-3305`）。
///
/// # 做什么
///
/// 1. 骨骼 0 改名为 [`STATIC_PROP_BONE`]、`parent = -1`；
/// 2. 位置与旋转**归零** —— `simplify.cpp:3362-3366`：
///    ```cpp
///    psource->rawanim[0][0].pos = Vector( 0, 0, 0 );
///    psource->rawanim[0][0].rot = RadianEuler( 0, 0, 0 );
///    AngleMatrix( QAngle( 0, 0, 0 ), psource->boneToPose[0] );
///    ```
///    实测：即便 SMD 的 root 带非零姿态（`ipg.smd` 的 pos `(5,6,7)`、
///    rot `(0.1,0.2,0.3)`），产物 `static_prop` 仍是
///    `pos=[0,0,0] rot=[0,0,0] poseToBone=单位矩阵`。
/// 3. **其余骨骼全部丢弃** —— 实测 `numbones == 1`（2681/2681）。
///
/// # 为什么保留第 0 根骨骼的 `surface_prop` / `bonemerge`
///
/// 只有第 0 根会被写出，其余字段对产物没有影响；但保留它们可以让
/// 「用户显式写的 `flags`」等覆盖继续生效，语义上更贴近
/// 「第 0 根骨骼被改名」而不是「新建一根骨骼」。
fn collapse_static_prop(desc: &ModelDesc) -> ModelDesc {
    let mut out = desc.clone();
    let Some(first) = out.bones.first().cloned() else {
        return out;
    };
    out.bones = vec![Bone {
        name: crate::model::STATIC_PROP_BONE.to_string(),
        parent: None,
        position: Some([0.0; 3]),
        rotation: Some([0.0; 3]),
        flags: first.flags,
        surface_prop: first.surface_prop,
        // `$bonemerge` 对静态道具没有意义（只有一根骨骼，无可合并的层级）。
        bonemerge: false,
        // 塌缩后的姿态是**手工写死**的 `[0,0,0]`，等价于 `$definebone`，
        // 所以显式标成 pre-aligned（`MakeStaticProp`，`simplify.cpp:3362-3366`）。
        // 单根骨骼没有子骨骼，`childbone == -1`，重排本来也不会碰它 ——
        // 这里只是把意图写明，不依赖推断。
        pre_aligned: Some(true),
        realign_position: None,
        realign_rotation: None,
    }];

    // hitbox / attachment 的骨骼引用全部重定向到那唯一一根骨骼。
    //
    // 对 hitbox：studiomdl 在 `$staticprop` + 显式 `$hbox` 时**直接报错**
    // （`simplify.cpp:6991` 的 `cannot find bone %s for bbox`，因为骨骼已改名
    // 而 `$hbox` 里写的还是旧名 —— 实测 `ipf2`/`ipf3`/`ipf5` 三个 QC 全部
    // 编译失败）。而语料里 2681/2681 个静态道具的 hitbox 都带
    // `AUTOGENERATED` 标志、0 个用显式 `$hbox` —— 与「显式路径必失败」
    // 完全一致。
    //
    // 这里**不报错**而是重定向：报错会让 `static_prop = true` 与显式
    // `[[hitboxes]]` 无法共存，而重定向产出的 hitbox 与官方自动生成路径
    // 落在同一根骨骼上，是更有用的行为。要复刻官方「硬错误」语义的话，
    // 应该在 `validate()` 里拒绝这种组合（见 [`ModelDesc::validate`]）。
    for hb in std::iter::once(&mut out.hitboxes)
        .chain(&mut out.additional_hitbox_sets)
        .flat_map(|set| &mut set.boxes)
    {
        hb.bone = crate::model::STATIC_PROP_BONE.to_string();
    }

    // 附着点：`MakeStaticProp()` 会把 `bonename` 改成 `static_prop`
    // （`simplify.cpp:3394`），并把 `local` 矩阵左乘旋转矩阵
    // （`simplify.cpp:3392` 的 `ConcatTransforms( rotated, local, local )`）。
    //
    // 矩阵的合成放在**写出器**里做（见 `mdl_writer` 的 attachment 段），
    // 因为那里才有「角度 → 矩阵」的原始精度；在这里拆成欧拉角再让写出器
    // 重建会多走一趟 `to_degrees()`/`to_radians()`，破坏逐位一致性。
    // 这里只负责把骨骼引用重定向。
    //
    // 实测 `ipf4`（`$staticprop` + `$attachment "muzzle" "root" 7 8 9`）：
    // ```text
    //   bone = 0
    //   local = [[-0, -1, 0, -8],
    //            [ 1, -0, 0,  7],
    //            [ 0,  0, 1,  9]]
    // ```
    // 正是 `Rz(90°) × 单位矩阵(平移 7,8,9)`：旋转部分变成 `Rz(90°)`，
    // 平移部分变成 `(-8, 7, 9)`。
    for at in &mut out.attachments {
        at.bone = crate::model::STATIC_PROP_BONE.to_string();
    }

    out
}

/// `$staticprop` 的几何旋转矩阵（`Rz(90°)`）。
///
/// 与 [`static_prop_rotate`] 是同一个映射，这里给需要矩阵形式的调用方
/// （附着点的 `ConcatTransforms`）用。
pub fn static_prop_matrix() -> crate::bone_math::Matrix3x4 {
    crate::bone_math::angle_matrix([0.0, 0.0, std::f32::consts::FRAC_PI_2])
}

/// `mstudiomodel_t.name`：显式给了就用，否则取 SMD 的文件名
/// （studiomdl 实测行为）。
fn model_name(m: &BodyModel, smd_path: &Path) -> String {
    if let Some(n) = &m.name {
        return n.clone();
    }
    smd_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 解析骨骼的最终参考姿态：描述显式值优先，否则取 SMD。
///
/// 返回值是 **(位置, 旋转弧度)**。描述里的 `rotation` 是**角度**，
/// 这里转成弧度以统一口径。
///
/// # 旋转会被**规范化**
///
/// 欧拉角不唯一，同一个旋转有多组等价三元组。实测官方 studiomdl 写进
/// `mstudiobone_t.rot` 的是它自己那套 `MatrixAngles` 分解出来的值 ——
/// `gimbal` 实验（pitch = π/2 万向锁）里：
///
/// | 来源 | 值 |
/// |---|---|
/// | SMD 第 0 帧 | `[0.35, 1.570796, -0.25]` |
/// | 规范化后 | `[0, 1.570796, -0.6]` |
/// | **官方骨骼表** | **`[0, 1.570796, -0.6]`** ← 规范化 |
///
/// 不规范化会让 `mstudiobone_t.rot` 与动画的参考姿态基准不一致，
/// 解码后整个动画偏移一个常量（**不报错**，只是动作错位）。
/// 旋转本身不变，所以 `quat` / `poseToBone` 不受影响。
pub fn resolve_bone_pose(
    desc: &ModelDesc,
    compiled: &CompiledModelDesc,
    bone_index: usize,
) -> ([f32; 3], [f32; 3]) {
    // **骨骼轴重对齐优先** —— 一旦触发，骨骼表的 pos/rot 就来自
    // `RealignBones` 重建的结果，不再是 SMD 的原始姿态。
    //
    // 这一步必须在这里做（而不是在写出器里）：`poseToBone`、自动 hitbox、
    // 动画的参考姿态**全部**经由本函数取姿态，任何一处漏掉都会让它们
    // 与骨骼表互相矛盾。
    if let Some(r) = &compiled.realigned
        && let Some(v) = r.poses.get(bone_index)
    {
        return *v;
    }
    let b = &desc.bones[bone_index];
    // 从任意一个 model 的参考姿态里找（同一次编译里它们应当一致）。
    let from_smd = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .find_map(|m| {
            m.poses
                .iter()
                .find(|p| p.bone == bone_index as i32)
                .copied()
        });

    let pos = b
        .position
        .or_else(|| from_smd.map(|p| p.position))
        .unwrap_or([0.0; 3]);
    let rot = match b.rotation {
        // 描述里写的是角度 → 转弧度。
        Some(deg) => [
            deg[0].to_radians(),
            deg[1].to_radians(),
            deg[2].to_radians(),
        ],
        // SMD 里本来就是弧度，原样搬运。
        None => from_smd.map(|p| p.rotation).unwrap_or([0.0; 3]),
    };
    (pos, crate::bone_math::canonical_euler(rot))
}

/// 取自 **SMD / skeleton** 的参考姿态 —— **忽略** TOML 的
/// `position`/`rotation`。
///
/// # 为什么单要一份「源」姿态
///
/// 官方 `SetupHitBoxes`（`simplify.cpp:6925`）用的世界矩阵是
/// **`srcWorld ∘ srcRealign`** —— 也就是 `simplify.cpp:1527` 那条**动画**
/// 路径的产物，**不是**骨骼表最终的参考姿态。两者只在写了 `$definebone`
/// （TOML 的 `position`/`rotation`）时才会分叉。
///
/// 实测（受控实验，对照官方产物）：
///
/// | 用例 | SMD 里 b 的世界 z | 文件里 b 的世界 z | 官方 hitbox(bone 2) 的 z | ⟹ 用的世界 z |
/// |---|---|---|---|---|
/// | `ipq2`（`$definebone` **6** 数字） | 30 | **20** | **`-8 .. 0`** | **30** = SMD |
/// | `ipq3`（**12** 数字，`srcRealign` 平移 `[0,0,10]`） | 30 | **20** | **`-18 .. 0`** | **40** = 30+10 |
/// | `ipr2`（`$ikchain` 触发重排，无 `$definebone`） | 30 | 重排后 x=20 | 落在**重排后**的基里 | = `srcWorld ∘ srcRealign` |
///
/// 顶点 z 是 22，于是 `22 − 30 = −8`、`22 − 40 = −18` —— 三个用例同一公式。
///
/// > 所以官方产物**内部并不自洽**：`poseToBone` 说 b 在世界 z=20，
/// > 而 hitbox 是按 30 算的。照搬这个实际行为即可 —— 修好它之后
/// > `ipq2`/`ipq3` 的 `hull_max` 与 `illumposition` 会跟着归位
/// > （姿态包围盒把每根骨骼的渲染 bbox 并了进去）。
///
/// 骨骼**没有** SMD 姿态时（纯 TOML 骨骼）回退到 [`resolve_bone_pose`]。
pub fn source_bone_pose(compiled: &CompiledModelDesc, bone_index: usize) -> ([f32; 3], [f32; 3]) {
    let from_smd = compiled
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .find_map(|m| {
            m.poses
                .iter()
                .find(|p| p.bone == bone_index as i32)
                .copied()
        });
    match from_smd {
        // SMD 里本来就是弧度，**不**做 `canonical_euler` —— 动画帧
        // （`realign_sequence_frames`）也是原样用的，两边必须一致。
        Some(p) => (p.position, p.rotation),
        None => resolve_bone_pose(&compiled.desc, compiled, bone_index),
    }
}

/// 官方**内部**的 `g_bonetable[k].boneToPose` —— `SetupHitBoxes`
/// （`simplify.cpp:6925`）与 `CalcSequenceBoundingBoxes` 用的世界矩阵。
///
/// **不是**骨骼表最终写进文件的参考姿态。两者的差别与证据见
/// [`source_bone_pose`]。
///
/// # 取值规则（逐骨骼）
///
/// | 骨骼 | 内部世界矩阵 |
/// |---|---|
/// | 写了 `$definebone`（TOML 有 `position`/`rotation`）或有显式 `srcRealign` | **`srcWorld ∘ srcRealign`** |
/// | 其余 | **骨骼表的重排后世界矩阵**（= `resolve_bone_pose` 那套） |
///
/// # 为什么不能一律用 `srcWorld ∘ srcRealign`
///
/// 对**非** pre-aligned 骨骼，`srcRealign = srcWorld⁻¹ ∘ newWorld`，
/// 于是 `srcWorld ∘ srcRealign` 在代数上就是 `newWorld` —— 但**浮点上不是**：
/// 多做一次「求逆再相乘」会引入约 1 ulp 的误差。
///
/// 实测（回归对照 `ipr2`/`ipr3`，全部骨骼都走了重排）：
///
/// | | `hull_min[2]` | `hull_max[0]` |
/// |---|---|---|
/// | 一律往返 | `-3.4969110629390343e-7` | `8` |
/// | 直接用 `newWorld` | **`-3.4969110629390343e-7`** ✓ | **`7.999999523162842`** ✓ = 官方 |
///
/// 所以只在**真正需要**（pre-aligned / 显式 `srcRealign`）时才走
/// `srcWorld ∘ srcRealign`，其余走骨骼表矩阵，逐位与官方一致。
///
/// # 为什么两处调用必须共用同一份
///
/// `CalcSequenceBoundingBoxes` 里
/// `posetransform[k] = bonetransform[k] ∘ inverse(boneToPose[k])` ——
/// 帧世界矩阵与参考世界矩阵**必须同源**，否则这个「相对变换」会凭空多出
/// 一次平移。实测 `ipq2`：`bonetransform` 用 30、参考用骨骼表的 20 时，
/// 顶点 z=22 被推到 **32**，而官方是 **30**。
fn internal_bone_world(
    compiled: &CompiledModelDesc,
    parents: &[i32],
) -> Vec<crate::bone_math::Matrix3x4> {
    let desc = &compiled.desc;
    let n = desc.bones.len();

    let src: Vec<([f32; 3], [f32; 3])> = (0..n).map(|i| source_bone_pose(compiled, i)).collect();
    let src_world = crate::bone_math::compute_world(
        &src.iter().map(|p| p.0).collect::<Vec<_>>(),
        &src.iter().map(|p| p.1).collect::<Vec<_>>(),
        parents,
    );

    let table: Vec<([f32; 3], [f32; 3])> = (0..n)
        .map(|i| resolve_bone_pose(desc, compiled, i))
        .collect();
    let table_world = crate::bone_math::compute_world(
        &table.iter().map(|p| p.0).collect::<Vec<_>>(),
        &table.iter().map(|p| p.1).collect::<Vec<_>>(),
        parents,
    );

    (0..n)
        .map(|k| {
            let explicit = desc.bones[k].explicit_src_realign();
            if desc.bones[k].is_pre_aligned() || explicit.is_some() {
                let sr = explicit.unwrap_or(crate::bone_math::IDENTITY);
                crate::bone_math::concat(&src_world[k], &sr)
            } else {
                table_world[k]
            }
        })
        .collect()
}

/// 把矩阵的旋转部分作用到一个方向向量上（`VectorIRotate` 语义）。
///
/// 对**正交**矩阵，旋转一个向量就是 `R · v`（不含平移）。
/// 用于把 `(0,0,1)`/`(0,1,0)` 逆旋转成 eyeball 的骨骼空间 `up`/`forward`。
fn rotate_vector(m: &crate::bone_math::Matrix3x4, v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[4] * v[0] + m[5] * v[1] + m[6] * v[2],
        m[8] * v[0] + m[9] * v[1] + m[10] * v[2],
    ]
}

/// 归一化成单位向量（零向量原样返回，由调用方保证输入非退化）。
fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len == 0.0 {
        v
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

/// **VTA 形状解析**：`[[bodyparts.models.flexes]]` → 每个 mesh 的载荷。
///
/// # 做两件事
///
/// 1. **注册 flexdesc**（按名去重，与 `Add_Flexdesc` 一致）——
///    `pair = true` 时注册 **`<名>R` 与 `<名>L` 两个**（先 R 后 L）。
///    这一步会**改变 `numflexdesc` 与段布局**，所以必须在
///    [`resolve_flex_eyeball_mouth`] 建查找表**之前**完成。
/// 2. **算载荷**：读 `.vta` → 就近匹配 → 差量 → smoothstep → `speed`，
///    结果按 mesh 分组存进 [`CompiledModel::mesh_flexes`]。
///
/// # 顶点池口径（关键）
///
/// `mstudioflex_t.vertindex` 指向的 vertanim 数组里，`index` 是
/// **该 mesh 内的局部顶点下标**（`write.cpp:1789` 的
/// `n = vanim[k].vertex - pmesh[m].vertexoffset`）。
/// 所以匹配必须在「该 model 的顶点池」里做，再减去该 mesh 的
/// `vertexoffset` 折算成局部下标 —— 这个 `vertexoffset` 必须与
/// `mdl_writer` 写出的**完全一致**（多 LOD 时它来自 `LodLayout`）。
fn resolve_vta_flexes(
    compiled: &mut CompiledModelDesc,
    base_dir: &Path,
) -> Result<(), Vec<CompileError>> {
    let mut errs: Vec<CompileError> = Vec::new();

    // 有没有活要干？没有就直接返回（保证零开销、产物逐字节不变）。
    let any = compiled
        .desc
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .any(|m| !m.flexes.is_empty());
    if !any {
        return Ok(());
    }

    // ---- 1. 注册全部 flexdesc 名（先 R 后 L）----
    //
    // `Add_Flexdesc` 按名去重（`stricmp`），所以同名只注册一次。
    // 这里先把「每条 flex 规格 → (desc 下标, pair 下标)」算出来，
    // 再在下面算载荷时用。
    //
    // ⚠️ 先把 flex 规格**克隆**出来：注册会改 `compiled.desc.flex_descriptors`，
    // 而同时借用 `compiled.desc.bodyparts` 会被借用检查拒绝。
    let specs: Vec<Vec<Vec<crate::model::Flex>>> = compiled
        .desc
        .bodyparts
        .iter()
        .map(|bp| bp.models.iter().map(|m| m.flexes.clone()).collect())
        .collect();

    let mut desc_index: HashMap<String, usize> = compiled
        .desc
        .flex_descriptors
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), i))
        .collect();
    let mut register = |name: &str, list: &mut Vec<crate::model::FlexDescriptor>| -> usize {
        if let Some(&i) = desc_index.get(name) {
            return i;
        }
        list.push(crate::model::FlexDescriptor {
            name: name.to_string(),
        });
        let i = list.len() - 1;
        desc_index.insert(name.to_string(), i);
        i
    };

    // 每条 flex 规格的 (主 desc 下标, 配对 desc 下标)。0 表示无配对。
    let mut desc_of: Vec<Vec<Vec<(usize, usize)>>> = Vec::new();
    for (bi, bp) in specs.iter().enumerate() {
        let mut per_model = Vec::new();
        for (mi, m) in bp.iter().enumerate() {
            let mut row = Vec::with_capacity(m.len());
            for (fi, f) in m.iter().enumerate() {
                let at = format!("bodyparts[{bi}].models[{mi}].flexes[{fi}]");
                if f.name.is_empty() {
                    errs.push(e(&at, crate::tr("the name must not be empty")));
                    row.push((0, 0));
                    continue;
                }
                if f.pair {
                    let (rn, ln) = crate::flex::pair_names(&f.name);
                    let r = register(&rn, &mut compiled.desc.flex_descriptors);
                    let l = register(&ln, &mut compiled.desc.flex_descriptors);
                    row.push((r, l));
                } else {
                    let d = register(&f.name, &mut compiled.desc.flex_descriptors);
                    row.push((d, 0));
                }
            }
            per_model.push(row);
        }
        desc_of.push(per_model);
    }

    // `.vta` 按路径缓存（同一文件常被多条 flex 共用）。
    let mut vta_cache: HashMap<(std::path::PathBuf, u32), crate::vta::Vta> = HashMap::new();

    for (bi, bp) in specs.iter().enumerate() {
        for (mi, m) in bp.iter().enumerate() {
            if m.is_empty() {
                continue;
            }
            let at = format!("bodyparts[{bi}].models[{mi}]");

            // 该 model 的顶点池 + 「池下标 → (mesh, mesh 内局部下标)」。
            let comp = &compiled.bodyparts[bi].models[mi];
            let mut pool: Vec<Vertex> = Vec::new();
            let mut mesh_of_vertex: Vec<(usize, u32)> = Vec::new();
            for (ki, mesh) in comp.meshes.iter().enumerate() {
                let vertices = comp
                    .lods
                    .as_ref()
                    .and_then(|lods| lods.meshes.get(ki))
                    .map(|lod| &lod.vertices)
                    .unwrap_or(&mesh.vertices);
                for (vi, v) in vertices.iter().enumerate() {
                    pool.push(v.clone());
                    mesh_of_vertex.push((ki, vi as u32));
                }
            }

            let mut per_mesh: Vec<Vec<crate::flex::ResolvedFlex>> =
                vec![Vec::new(); comp.meshes.len()];

            // `vanim_map` 只取决于 `(vta 第 0 帧, model 顶点池)`，与 flex 无关，
            // 所以同一 `(model, vta)` 下复用一份。真实模型上
            // `build_vanim_map` 是十万×十万量级 —— 逐条 flex 重算会让
            // 编译时间从秒级涨到分钟级（`main.vta` 42 条 flex 共用一份）。
            let mut vanim_map_cache: HashMap<std::path::PathBuf, crate::flex::VanimMap> =
                HashMap::new();

            // `base_index` 同样只取决于 `(vta 第 0 帧)`，与 flex 无关。
            // 逐条 flex 重建是 42 × 180180 = 757 万次 SipHash 插入 ——
            // 实测占一次完整编译的约 25%，是当前最大的单项开销。
            let mut base_index_cache: HashMap<std::path::PathBuf, crate::flex::BaseIndex> =
                HashMap::new();

            for (fi, f) in m.iter().enumerate() {
                let fat = format!("{at}.flexes[{fi}]");
                let vta_path = match resolve_vta_path(base_dir, &f.vta) {
                    Ok(p) => p,
                    Err(msg) => {
                        errs.push(e(&fat, msg));
                        continue;
                    }
                };
                let scale = compiled.desc.bodyparts[bi].models[mi]
                    .input_scale
                    .unwrap_or(1.0);
                let vta = match vta_cache.entry((vta_path.clone(), scale.to_bits())) {
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        let text = match std::fs::read_to_string(&vta_path) {
                            Ok(t) => t,
                            Err(err) => {
                                errs.push(e(
                                    &fat,
                                    crate::tr_fmt!(
                                        "cannot read %{path}: %{err}",
                                        path = vta_path.display(),
                                        err = err
                                    ),
                                ));
                                continue;
                            }
                        };
                        match crate::vta::parse_vta(&text) {
                            Ok(mut v) => {
                                if scale != 1.0 {
                                    for frame in &mut v.frames {
                                        for vertex in frame {
                                            for value in &mut vertex.pos {
                                                *value *= scale;
                                            }
                                        }
                                    }
                                }
                                for w in &v.warnings {
                                    errs.push(e(&fat, format!("{}：{w}", vta_path.display())));
                                }
                                slot.insert(v)
                            }
                            Err(err) => {
                                errs.push(e(
                                    &fat,
                                    crate::tr_fmt!(
                                        "failed to parse %{path}: %{err}",
                                        path = vta_path.display(),
                                        err = err
                                    ),
                                ));
                                continue;
                            }
                        }
                    }
                };

                let fd = desc_of[bi][mi][fi];
                let vanim_map = vanim_map_cache
                    .entry(vta_path.clone())
                    .or_insert_with(|| crate::flex::build_vanim_map(vta, &pool));
                let base_index = base_index_cache
                    .entry(vta_path.clone())
                    .or_insert_with(|| crate::flex::BaseIndex::build(vta));
                match crate::flex::resolve_flex_indexed(
                    f,
                    vta,
                    base_index,
                    vanim_map,
                    &mesh_of_vertex,
                    (fd.0 as i32, fd.1 as i32),
                    fi,
                ) {
                    Ok(meshes) => {
                        for (k, mf) in meshes.into_iter().enumerate() {
                            if k < per_mesh.len() {
                                per_mesh[k].extend(mf.flexes);
                            }
                        }
                    }
                    Err(err) => errs.push(e(&fat, err)),
                }
            }

            // 一致性检查：`index` 是 mesh 内局部下标，必须落在该 mesh 的
            // 顶点数内。错位时静默产出「引擎读到错误顶点」的产物，
            // 所以这里显式拦一道。
            for (k, fx) in per_mesh.iter().enumerate() {
                let n = comp
                    .lods
                    .as_ref()
                    .and_then(|lods| lods.meshes.get(k))
                    .map(|lod| lod.vertices.len())
                    .unwrap_or(comp.meshes[k].vertices.len());
                for f in fx {
                    for a in &f.vertanims {
                        if a.index as usize >= n {
                            errs.push(e(
                                &at,
                                crate::tr_fmt!(
                                    "internal error: the vertanim index %{index} of mesh %{k} is past the %{n} vertices \
                                     (the vertex pool does not agree with the writer)",
                                    index = a.index,
                                    k = k,
                                    n = n
                                ),
                            ));
                        }
                    }
                }
            }

            // ⚠️ **追加**，不是覆盖：`resolve_shape_key_flexes` 先跑，
            // 它的载荷已经在这个字段里了。直接赋值会把 FBX 的 shape key
            // 静默丢掉 —— 而那种产物**不报任何错**，只是表情不动。
            let slot = &mut compiled.bodyparts[bi].models[mi].mesh_flexes;
            if slot.len() < per_mesh.len() {
                slot.resize(per_mesh.len(), Vec::new());
            }
            for (mesh, add) in per_mesh.into_iter().enumerate() {
                slot[mesh].extend(add);
            }
        }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// **FBX shape key → flex 自动注册**。
///
/// # 官方行为（真 exe 实测，`docs/fbx-support.md` §1.6b）
///
/// QC 里**不需要**写任何 flex 语法 —— 只要几何源是 FBX，官方就把文件里的
/// 每个 shape key 自动变成一条 flex：
///
/// | 段 | 内容 |
/// |---|---|
/// | `flexdesc` | shape key 名**原样** |
/// | `flexcontroller` | 每个 shape key 一条，`type = name = 形状名`、`min = 0`、`max = 1` |
/// | `flexrule` | 每条一个 op：`STUDIO_FETCH1`（`code = 2`）指向自己的 controller |
/// | mesh flex | `targets = [0, 1, 10, 11]`、`pair = 0`、`vertanimtype = 0` |
/// | vertanim | `speed = 255`、`side = 255`、`ndelta = (0,0,0)`，`delta` = 变换后的 `pos_off` |
///
/// # 与 `.vta` 路径（[`resolve_vta_flexes`]）的三点差别
///
/// 1. **没有 `.vta` 文件** —— 位移直接来自 FBX 的 `position_offsets`，
///    在 [`crate::fbx::read`] 里已经用 `rot_norm(geometry_to_world)` 变换到
///    Source 空间（实测 9/9 数据点，见 `docs/fbx-support.md` §1.6b）。
/// 2. **不做就近匹配** —— shape key 给的是**控制点号**，而
///    [`CompiledModel::mesh_src_index`] 记着每个焊接顶点来自哪个控制点，
///    所以这里是精确的「一对多展开」，不是 `build_vanim_map` 那种
///    3×3×3 网格搜索（也就没有 `.vta` 路径的匹配失败问题）。
/// 3. **`side` 写 255**（不是 `.vta` 非 pair 路径的 0）。官方 FBX 路径实测
///    如此；对 `flexpair = 0` 的 flex 这个字段被引擎忽略，写它是为了让
///    产物与官方**逐字段**一致。
///
/// # 与 `.vta` 路径可以共存
///
/// 实测 `oracle_fbx_isolate.js` 的 `W2_morphfbx_vta_flex`：同一个 FBX 网格源
/// 上，shape key 的 desc 与显式 `flex` 的 desc **共存**
/// （`desc = ["wide", "tall", "v1"]`）。所以本函数是**追加**，不是替换。
///
/// ⚠️ 必须**早于** [`resolve_flex_eyeball_mouth`]：后者按名建 flexdesc /
/// flexcontroller 查找表，而本函数会**追加**这两张表的内容。
fn resolve_shape_key_flexes(compiled: &mut CompiledModelDesc) -> Result<(), Vec<CompileError>> {
    let mut errs: Vec<CompileError> = Vec::new();

    // 先把 shape key **克隆**出来：注册会改 `compiled.desc.*`，
    // 而同时借用 `compiled.bodyparts` 会被借用检查拒绝
    // （与 `resolve_vta_flexes` 的 `specs` 同一手法）。
    let specs: Vec<Vec<Vec<crate::fbx::FbxShapeKey>>> = compiled
        .bodyparts
        .iter()
        .map(|bp| bp.models.iter().map(|m| m.shape_keys.clone()).collect())
        .collect();

    // 没有 shape key 就直接返回 —— 保证非 FBX 工程的产物**逐字节不变**。
    let any = specs.iter().any(|bp| bp.iter().any(|m| !m.is_empty()));
    if !any {
        return Ok(());
    }

    // ---- 1. 注册 flexdesc / flexcontroller / flexrule ----
    //
    // desc 与 controller 都**按名去重**（官方 `Add_Flexdesc` 用 `stricmp`；
    // 用户显式写过同名的 `flexcontroller` 时复用自己的，不去制造第二条），
    // 而 flexrule **不去重** —— 每条 flex 一条规则。
    let mut desc_index: HashMap<String, usize> = compiled
        .desc
        .flex_descriptors
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), i))
        .collect();
    let mut fc_index: HashMap<String, usize> = compiled
        .desc
        .flex_controllers
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.clone(), i))
        .collect();

    // 每个 `(bodypart, model, shape key)` 的 flexdesc 下标。
    let mut desc_of: Vec<Vec<Vec<usize>>> = Vec::new();

    for (bi, bp) in specs.iter().enumerate() {
        let mut per_model = Vec::new();
        for (mi, keys) in bp.iter().enumerate() {
            let mut row = Vec::with_capacity(keys.len());
            for (ki, k) in keys.iter().enumerate() {
                let at = format!("bodyparts[{bi}].models[{mi}].shape_keys[{ki}]");
                if k.name.is_empty() {
                    errs.push(e(
                        &at,
                        crate::tr(
                            "the shape key name is empty and cannot be registered as a flexdesc",
                        ),
                    ));
                    row.push(0);
                    continue;
                }
                let d = match desc_index.get(&k.name) {
                    Some(&i) => i,
                    None => {
                        compiled
                            .desc
                            .flex_descriptors
                            .push(crate::model::FlexDescriptor {
                                name: k.name.clone(),
                            });
                        let i = compiled.desc.flex_descriptors.len() - 1;
                        desc_index.insert(k.name.clone(), i);
                        i
                    }
                };
                if !fc_index.contains_key(&k.name) {
                    compiled
                        .desc
                        .flex_controllers
                        .push(crate::model::FlexController {
                            name: k.name.clone(),
                            kind: k.name.clone(),
                            min: 0.0,
                            max: 1.0,
                        });
                    fc_index.insert(k.name.clone(), compiled.desc.flex_controllers.len() - 1);
                }
                compiled.desc.flex_rules.push(crate::model::FlexRule {
                    flex: k.name.clone(),
                    ops: vec![crate::model::FlexOp {
                        op: crate::model::FlexOpKind::Fetch1,
                        value: None,
                        controller: Some(k.name.clone()),
                        flexdesc: None,
                    }],
                });
                row.push(d);
            }
            per_model.push(row);
        }
        desc_of.push(per_model);
    }

    // ---- 2. 算载荷 ----
    //
    // 每个 shape key 的 `position_offsets[k]` 对应控制点 `vertex_index[k]`。
    // 该控制点在模型里可能被焊成**多个**顶点（位置/法线/UV 任一不同就会分裂），
    // 官方对每一个都写一条 vertanim —— 实测 `axisprobe.fbx` 的一个控制点
    // 展开成 3 条（`docs/fbx-support.md` §1.6b）。
    for (bi, bp) in specs.iter().enumerate() {
        for (mi, keys) in bp.iter().enumerate() {
            if keys.is_empty() {
                continue;
            }
            let n_meshes = compiled.bodyparts[bi].models[mi].meshes.len();

            // 「控制点号 → [(mesh, mesh 内局部下标)]」倒排表。
            let mut by_cp: HashMap<u32, Vec<(usize, u32)>> = HashMap::new();
            for (ki, srcs) in compiled.bodyparts[bi].models[mi]
                .mesh_src_index
                .iter()
                .enumerate()
            {
                for (vi, &cp) in srcs.iter().enumerate() {
                    by_cp.entry(cp).or_default().push((ki, vi as u32));
                }
            }

            let mut per_mesh: Vec<Vec<crate::flex::ResolvedFlex>> = vec![Vec::new(); n_meshes];
            for (ki, k) in keys.iter().enumerate() {
                let d = desc_of[bi][mi][ki];
                // 按 mesh 分组收集本 shape key 的全部 vertanim。
                let mut grouped: Vec<Vec<crate::flex::ResolvedVertAnim>> =
                    vec![Vec::new(); n_meshes];
                for (k2, &cp) in k.vertex_index.iter().enumerate() {
                    // shape key 动了一个没进模型的顶点 —— 官方静默跳过。
                    let Some(hits) = by_cp.get(&cp) else {
                        continue;
                    };
                    let delta = k.position_offsets[k2];
                    for &(mesh, local) in hits {
                        grouped[mesh].push(crate::flex::ResolvedVertAnim {
                            index: local,
                            speed: 255,
                            side: 255,
                            delta,
                            ndelta: [0.0; 3],
                        });
                    }
                }
                for (mesh, mut anims) in grouped.into_iter().enumerate() {
                    if anims.is_empty() {
                        continue;
                    }
                    // ⚠️ **有意偏离**：官方 FBX 路径的 vertanim 顺序既不是升序
                    // 也不是任何从文件内容能推出的序（实测 `k4_morph.mdl` 与
                    // `shapemix.fbx` 两组互不相同，见 `docs/fbx-support.md`
                    // §4.6）。这里按顶点号升序 —— 数组顺序对渲染无影响。
                    anims.sort_by_key(|a| a.index);
                    per_mesh[mesh].push(crate::flex::ResolvedFlex {
                        flexdesc: d as i32,
                        targets: [0.0, 1.0, 10.0, 11.0],
                        flexpair: 0,
                        vertanimtype: 0,
                        vertanims: anims,
                    });
                }
            }

            // 追加到 `.vta` 路径算出来的载荷之后。`mesh_flexes` 可能是空的
            // （没有 `flex` 语句时 `resolve_vta_flexes` 直接早退了），
            // 所以先补齐长度。
            let slot = &mut compiled.bodyparts[bi].models[mi].mesh_flexes;
            if slot.len() < n_meshes {
                slot.resize(n_meshes, Vec::new());
            }
            for (mesh, add) in per_mesh.into_iter().enumerate() {
                slot[mesh].extend(add);
            }
        }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// flex 系列 / eyeball / mouth 的名字解析与骨骼空间量计算。
///
/// # 做什么
///
/// 1. **flexrule**：把 `flex`（flexdesc 名）与每个 op 的 `controller`/`flexdesc`
///    名解析成下标（`fetch1` → flexcontroller、`fetch2` → flexdesc）。
/// 2. **flexcontrollerui**：按 fc 数组顺序扫描，**紧邻**的 `right_X` + `left_X`
///    对（`X` 逐字节相同、大小写敏感、与 `type` 无关）合并成一条 **stereo** ui
///    （`name = X`，`szindex0` 指向**下标更大**的 `left_X`）；其余每条 fc 各
///    产一条单声道 ui（`name` = fc 名原样）。用户显式写的追加在后。
/// 3. **mouth**：按显式 `index` 摆进数组（长度 = `max(index)+1`），
///    `bone`/`flexdesc` 解析成下标，空洞写全 0。
/// 4. **eyeball**：`org` 经 [`internal_bone_world`] 的逆变换成骨骼空间，
///    `up`/`forward` 由逆旋转 `(0,0,1)`/`(0,1,0)` 得到；lid 名解析成下标；
///    并把对应 mesh 打标 `eyeball_tag`。
fn resolve_flex_eyeball_mouth(compiled: &mut CompiledModelDesc) -> Result<(), Vec<CompileError>> {
    let mut errs: Vec<CompileError> = Vec::new();

    // ---- 0. `dummy_eyelid`：未配 `eyelid` 的 eyeball 会得到一个占位 flexdesc ----
    //
    // ⚠️ 这段在 episode1 源码里**不存在**（`grep dummy_eyelid` 零命中）——
    // 是 L4D2 的行为，只用受控实验钉死：
    //
    // | QC | flexdesc 产物 |
    // |---|---|
    // | `fxr1`（只写 2 条 `eyeball`，**无**任何 flexdesc/eyelid） | `["dummy_eyelid"]` |
    // | `fxl1`（`localvar alpha beta` + eyeball） | `[alpha, beta, dummy_eyelid]` |
    // | `fx2`（localvar/mouth + eyeball） | `[…, dummy_eyelid]` |
    // | 4 个 survivor（**写了** `eyelid`） | **不含** dummy_eyelid |
    //
    // 即：**只要某个 eyeball 缺 lid 数据就追加一条**，且**永远追加在末尾**
    // （`fxl1` 的 alpha/beta 在它之前）。
    //
    // 必须在这里（构造查找表与写字节**之前**）做 —— 它改变
    // `numflexdesc`、flexdesc 数组下标、字符串池内容与段布局。
    let dummy_index: Option<usize> = {
        let needs = compiled
            .desc
            .bodyparts
            .iter()
            .flat_map(|bp| &bp.models)
            .flat_map(|m| &m.eyeballs)
            .any(|eb| eb.upper_lid.is_none() || eb.lower_lid.is_none());
        if !needs {
            None
        } else {
            // 用户显式写了同名就复用，否则追加到末尾（`Add_Flexdesc` 按名去重）。
            match compiled
                .desc
                .flex_descriptors
                .iter()
                .position(|f| f.name == DUMMY_EYELID)
            {
                Some(i) => Some(i),
                None => {
                    compiled
                        .desc
                        .flex_descriptors
                        .push(crate::model::FlexDescriptor {
                            name: DUMMY_EYELID.to_string(),
                        });
                    Some(compiled.desc.flex_descriptors.len() - 1)
                }
            }
        }
    };

    let desc = &compiled.desc;

    // 名字 → 下标的查找表。
    let bone_index = desc.bone_index();
    let flexdesc_index: HashMap<&str, usize> = desc
        .flex_descriptors
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();
    let fc_index: HashMap<&str, usize> = desc
        .flex_controllers
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();

    // ---- 1. flexrule ----
    let mut rules = Vec::with_capacity(desc.flex_rules.len());
    for (ri, r) in desc.flex_rules.iter().enumerate() {
        let at = format!("flex_rules[{ri}]");
        let Some(&flex) = flexdesc_index.get(r.flex.as_str()) else {
            errs.push(e(
                &at,
                crate::tr_fmt!(
                    "the flex %{name} was not found in [[flex_descriptors]]",
                    name = format!("{:?}", r.flex)
                ),
            ));
            continue;
        };
        let mut ops = Vec::with_capacity(r.ops.len());
        for (oi, op) in r.ops.iter().enumerate() {
            let oat = format!("{at}.ops[{oi}]");
            let kind = op.op;
            let d = match kind.operand() {
                crate::model::FlexOperand::Value => {
                    let Some(v) = op.value else {
                        errs.push(e(
                            &oat,
                            crate::tr_fmt!(
                                "the op %{op} needs a value",
                                op = format!("{:?}", kind.code())
                            ),
                        ));
                        continue;
                    };
                    crate::model::FlexOpData::Value(v)
                }
                crate::model::FlexOperand::Index => match kind {
                    crate::model::FlexOpKind::Fetch1 => {
                        let Some(name) = op.controller.as_deref() else {
                            errs.push(e(&oat, crate::tr("fetch1 needs a controller")));
                            continue;
                        };
                        let Some(&idx) = fc_index.get(name) else {
                            errs.push(e(
                                &oat,
                                crate::tr_fmt!(
                                    "the controller %{name} of fetch1 was not found in [[flex_controllers]]",
                                    name = format!("{name:?}")
                                ),
                            ));
                            continue;
                        };
                        crate::model::FlexOpData::Index(idx as i32)
                    }
                    crate::model::FlexOpKind::Fetch2 => {
                        let Some(name) = op.flexdesc.as_deref() else {
                            errs.push(e(&oat, crate::tr("fetch2 needs a flexdesc")));
                            continue;
                        };
                        let Some(&idx) = flexdesc_index.get(name) else {
                            errs.push(e(
                                &oat,
                                crate::tr_fmt!(
                                    "the flexdesc %{name} of fetch2 was not found in [[flex_descriptors]]",
                                    name = format!("{name:?}")
                                ),
                            ));
                            continue;
                        };
                        crate::model::FlexOpData::Index(idx as i32)
                    }
                    _ => unreachable!("operand()==Index 只有 fetch1/fetch2"),
                },
                crate::model::FlexOperand::None => crate::model::FlexOpData::None,
            };
            ops.push(crate::model::ResolvedFlexOp { op: kind.code(), d });
        }
        rules.push(crate::model::ResolvedFlexRule {
            flex: flex as i32,
            ops,
        });
    }
    compiled.resolved_flex_rules = rules;

    // ---- 2. flexcontrollerui（自动 + 显式）----
    //
    // 官方规则（真 exe 17 用例裁决，见 `docs/_probe/oracle_fcui.js` /
    // `oracle_fcui2.js`）：按 **fc 数组顺序**扫描，若**紧邻**的一对满足
    //
    //     name[i] == "right_" + X   且   name[i+1] == "left_" + X
    //
    // （`X` **逐字节相同**、**大小写敏感**；**与 `type` 无关**；**与是否被
    // flexrule 引用无关**）⟹ 合并成 **1 条 stereo ui**：`name = X`、
    // `stereo = 1`、`szindex0` → `left_X`（**下标更大**那条）、`szindex1` →
    // `right_X`，两条一起跳过。
    //
    // 否则当前条单独成 **1 条非 stereo ui**：`name` = fc 名**原样**、
    // `stereo = 0`、`szindex0` → 自身、`szindex1 = 0`。
    //
    // 判别性反例（全部实测，别把它们当成「显然」）：
    //   - `left_X right_X`（顺序反转）        ⟹ **不**合并
    //   - `zzz_right zzz_left`（后缀式）      ⟹ **不**合并
    //   - `Right_A` / `Left_A`（前缀大小写）  ⟹ **不**合并
    //   - `right_a left_A`（剩余部分大小写）  ⟹ **不**合并
    //   - `right_A mid left_A`（不相邻）      ⟹ **不**合并
    //   - `eyelid right_A` + `nose left_A`    ⟹ **仍合并**（type 不参与判据）
    //   - `right_a_b left_a_b`（剩余含下划线）⟹ 合并，`name = "a_b"`
    //
    // ⚠️ 曾经这里是「每条 fc 各产一条非 stereo ui」，对用户工程会得到
    // 52 条；官方是 33 条（52 − 19 对）。
    let mut uis: Vec<crate::model::ResolvedFlexControllerUi> = Vec::new();
    let mut i = 0usize;
    while i < desc.flex_controllers.len() {
        let pair_name = {
            let cur = desc.flex_controllers[i].name.as_str();
            cur.strip_prefix("right_").and_then(|x| {
                desc.flex_controllers
                    .get(i + 1)
                    .and_then(|n| n.name.strip_prefix("left_"))
                    .filter(|y| *y == x)
                    .map(|_| x.to_string())
            })
        };
        match pair_name {
            Some(x) => {
                uis.push(crate::model::ResolvedFlexControllerUi {
                    name: x,
                    fc0: (i + 1) as i32, // left（下标更大）
                    fc1: Some(i as i32), // right（下标更小）
                    stereo: true,
                });
                i += 2;
            }
            None => {
                uis.push(crate::model::ResolvedFlexControllerUi {
                    name: desc.flex_controllers[i].name.clone(),
                    fc0: i as i32,
                    fc1: None,
                    stereo: false,
                });
                i += 1;
            }
        }
    }
    // 用户显式写的（DMX 形态 / stereo 对）。
    for (ui_i, u) in desc.flex_controller_ui.iter().enumerate() {
        let at = format!("flex_controller_ui[{ui_i}]");
        // 解析 left/right 到 fc 下标；缺省回退：单条时指向同一个。
        let li = u.left.as_deref().and_then(|n| fc_index.get(n).copied());
        let ri = u.right.as_deref().and_then(|n| fc_index.get(n).copied());
        // 校验引用存在。
        for (which, name) in [("left", &u.left), ("right", &u.right)] {
            if let Some(n) = name
                && !fc_index.contains_key(n.as_str())
            {
                errs.push(e(
                    &at,
                    crate::tr_fmt!(
                        "the flexcontroller %{name} of %{which} was not found in [[flex_controllers]]",
                        name = format!("{n:?}"),
                        which = which
                    ),
                ));
            }
        }
        let (fc0, fc1) = match (li, ri) {
            (Some(a), Some(b)) => {
                // `szindex0` 指向**下标更大**那条（语料 108/108）。
                if a >= b {
                    (a as i32, Some(b as i32))
                } else {
                    (b as i32, Some(a as i32))
                }
            }
            (Some(a), None) => (a as i32, None),
            (None, Some(b)) => (b as i32, None),
            (None, None) => {
                errs.push(e(
                    &at,
                    crate::tr("a stereo ui needs at least one of left/right"),
                ));
                continue;
            }
        };
        uis.push(crate::model::ResolvedFlexControllerUi {
            name: u.name.clone(),
            fc0,
            fc1,
            stereo: u.stereo,
        });
    }
    compiled.resolved_flex_controller_ui = uis;

    // ---- 3. mouth（按显式 index 摆放）----
    if !desc.mouths.is_empty() {
        let max_index = desc.mouths.iter().map(|m| m.index).max().unwrap_or(0);
        if max_index < 0 {
            errs.push(e("mouths", crate::tr("the index must not be negative")));
        } else {
            let n = (max_index + 1) as usize;
            // 空洞写全 0（与官方 g_mouth[index] 未初始化一致）。
            let mut slots = vec![
                crate::model::ResolvedMouth {
                    bone: 0,
                    forward: [0.0; 3],
                    flexdesc: 0,
                };
                n
            ];
            let mut filled = vec![false; n];
            for (mi, m) in desc.mouths.iter().enumerate() {
                let at = format!("mouths[{mi}]");
                if m.index < 0 {
                    errs.push(e(
                        &at,
                        crate::tr_fmt!("the index %{index} must not be negative", index = m.index),
                    ));
                    continue;
                }
                let slot = m.index as usize;
                if filled[slot] {
                    errs.push(e(
                        &at,
                        crate::tr_fmt!("the index %{index} was written twice", index = m.index),
                    ));
                    continue;
                }
                let Some(&bone) = bone_index.get(m.bone.as_str()) else {
                    errs.push(e(
                        &at,
                        crate::tr_fmt!(
                            "cannot find the bone %{bone}",
                            bone = format!("{:?}", m.bone)
                        ),
                    ));
                    continue;
                };
                let Some(&fd) = flexdesc_index.get(m.flexdesc.as_str()) else {
                    errs.push(e(
                        &at,
                        crate::tr_fmt!(
                            "the flexdesc %{name} was not found in [[flex_descriptors]]",
                            name = format!("{:?}", m.flexdesc)
                        ),
                    ));
                    continue;
                };
                slots[slot] = crate::model::ResolvedMouth {
                    bone: bone as i32,
                    forward: m.forward,
                    flexdesc: fd as i32,
                };
                filled[slot] = true;
            }
            compiled.resolved_mouths = slots;
        }
    }

    // ---- 4. eyeball（骨骼空间变换 + mesh 打标）----
    // 世界矩阵口径与自动 hitbox / 姿态包围盒一致（`internal_bone_world`）。
    let any_eyeball = desc
        .bodyparts
        .iter()
        .flat_map(|bp| &bp.models)
        .any(|m| !m.eyeballs.is_empty());
    if any_eyeball {
        let parents: Vec<i32> = bone_parents(desc);
        let world = internal_bone_world(compiled, &parents);
        // 逐 model 处理；把描述里的 material 名映射到该 model 的 mesh 下标。
        for (bi, bp) in compiled.bodyparts.iter_mut().enumerate() {
            for (mi, cm) in bp.models.iter_mut().enumerate() {
                let dmodel = &desc.bodyparts[bi].models[mi];
                if dmodel.eyeballs.is_empty() {
                    continue;
                }
                let at = format!("bodyparts[{bi}].models[{mi}].eyeballs");
                // 该 model 的「材质名 → mesh 下标」。
                let mat_to_mesh: HashMap<usize, usize> = cm
                    .meshes
                    .iter()
                    .enumerate()
                    .map(|(k, mesh)| (mesh.material, k))
                    .collect();
                let mut out_eb = Vec::with_capacity(dmodel.eyeballs.len());
                for (ej, eb) in dmodel.eyeballs.iter().enumerate() {
                    let eat = format!("{at}[{ej}]");
                    let Some(&bone) = bone_index.get(eb.bone.as_str()) else {
                        errs.push(e(
                            &eat,
                            crate::tr_fmt!(
                                "cannot find the bone %{bone}",
                                bone = format!("{:?}", eb.bone)
                            ),
                        ));
                        continue;
                    };
                    // org：世界空间 → 骨骼空间（`VectorITransform` 对内部
                    // `boneToPose` 是正向变换；等价于 `org × poseToBone`，
                    // 即「世界矩阵取逆后变换」）。
                    let inv = crate::bone_math::invert(&world[bone]);
                    let org = transform_point(&inv, eb.org);
                    // up / forward：`VectorIRotate`（逆旋转）—— 对内部矩阵
                    // 是正向旋转 `(0,0,1)`/`(0,-1,0)`；等价于用**逆矩阵**旋转。
                    //
                    // ⚠️ **`forward` 是 `(0,-1,0)` 而不是 `(0,1,0)`。**
                    // episode1 源码 `studiomdl.cpp:3451` 写的是 `(0,1,0)`，
                    // 还带一行注释 `// FIXME: this is backwards` —— L4D2
                    // **把这个 FIXME 修掉了**（又一次「episode1 源码不是
                    // L4D2 的 studiomdl」）。
                    //
                    // 证据（真实语料，`opt_eyeball_forward` 判据）：
                    // 4 个 survivor 模型共 8 条 eyeball，
                    // `forward == VectorIRotate((0,-1,0), poseToBone)`
                    // **8/8 命中**，而 `(0,+1,0)` **0/8**。
                    // `up == VectorIRotate((0,0,1), …)` 同样 **8/8**。
                    //
                    // `rotate_vector` 走的正是 `poseToBone`（= `inv(world)`），
                    // 所以这里直接给世界空间的 `(0,-1,0)`。
                    let up = normalize3(rotate_vector(&inv, [0.0, 0.0, 1.0]));
                    let forward = normalize3(rotate_vector(&inv, [0.0, -1.0, 0.0]));
                    // lid 三元组 → 下标。
                    //
                    // 缺省（未写 `eyelid` / 未给 lid）：三个槽与 `*lidflexdesc`
                    // 一律指向 `dummy_eyelid`，target 写死 `[-1, 0, 1]`
                    // （受控实验 `fxr1`/`fx2`/`fxl1` 一致）。
                    let d = dummy_index.unwrap_or(0) as i32;
                    let (mut ufd, mut utgt, mut ulid) = ([d; 3], DUMMY_LID_TARGETS, d);
                    let (mut lfd, mut ltgt, mut llid) = ([d; 3], DUMMY_LID_TARGETS, d);
                    if let Some(lid) = &eb.upper_lid
                        && let Err(msg) =
                            resolve_lid(lid, &flexdesc_index, &mut ufd, &mut utgt, &mut ulid)
                    {
                        errs.push(e(format!("{eat}.upper_lid"), msg));
                    }
                    if let Some(lid) = &eb.lower_lid
                        && let Err(msg) =
                            resolve_lid(lid, &flexdesc_index, &mut lfd, &mut ltgt, &mut llid)
                    {
                        errs.push(e(format!("{eat}.lower_lid"), msg));
                    }
                    // mesh 打标：按材质名定位 mesh。
                    //
                    // ⚠️ 与 `build_meshes` 同样用**匹配键**（统一分隔符 +
                    // basename 兜底）—— QC 的 `eyeball` 用裸名（`pupil_r`），
                    // 而 TOML 的材质名可能带路径前缀，两者都要能匹配。
                    let want = crate::mdl_writer::texture_match_key(
                        &eb.material,
                        &desc.materials.search_paths,
                    );
                    let mat_idx = desc.materials.textures.iter().position(|t| {
                        crate::mdl_writer::texture_match_key(&t.name, &desc.materials.search_paths)
                            == want
                    });
                    match mat_idx.and_then(|mi2| mat_to_mesh.get(&mi2).copied()) {
                        Some(mesh_k) => cm.meshes[mesh_k].eyeball_tag = Some(ej),
                        None => errs.push(e(
                            &eat,
                            crate::tr_fmt!(
                                "the material %{name} was not found among the meshes of this model",
                                name = format!("{:?}", eb.material)
                            ),
                        )),
                    }
                    out_eb.push(crate::model::CompiledEyeball {
                        bone: bone as i32,
                        org,
                        zoffset: eb.zoffset,
                        radius: eb.radius,
                        up,
                        forward,
                        iris_scale: eb.iris_scale,
                        upperflexdesc: ufd,
                        lowerflexdesc: lfd,
                        uppertarget: utgt,
                        lowertarget: ltgt,
                        upperlidflexdesc: ulid,
                        lowerlidflexdesc: llid,
                    });
                }
                cm.eyeballs = out_eb;
            }
        }
    }

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// 把 `$jigglebone` 解析成「可直接写字节」的形态：算 `flags`、角度转弧度、
/// 钳位 stiffness、填缺省、骨骼名查下标。
///
/// # 官方语义：**扁平记录 + token 顺序「后写覆盖」**
///
/// 反编译 + 受控实验一起钉死（`jig47`/`jig48` 是决定性的）：
/// 官方并**不**为三个块各存一份再合并 —— 它只有**一个扁平记录**，
/// 每个键**按 QC 里出现的先后顺序**写进那一个记录，**后写覆盖先写**。
/// `flags` 则是**按位 OR 累积**（只有 `0x20` 会被 `allow_length_flex` 清掉）。
///
/// | 夹具 | QC 写法 | 实测 `min_yaw`/`max_yaw` |
/// |---|---|---|
/// | `jig47` | `is_rigid{yaw_constraint -10 20}` → `has_base_spring{yaw_constraint -30 40}` | `−30°/40°` |
/// | `jig48` | `has_base_spring{yaw_constraint -30 40}` → `is_rigid{yaw_constraint -10 20}` | `−10°/20°` |
///
/// ⟹ 所以这里**只有一条路径**：拿 [`JiggleBone::effective_writes`] 的顺序日志，
/// 逐条按序应用。TOML 的三个块也先被展开成同一个日志（见 `effective_writes`），
/// 于是「QC 路径」与「TOML 路径」不会各写一套逻辑。
///
/// # `flags`（受控实验 `jig{1,2,4,6,7,8,43,47,48,49,50}` 钉死）
///
/// | 位 | 何时置位 |
/// |---|---|
/// | `0x01 IS_FLEXIBLE` | `is_flexible` 块出现 |
/// | `0x02 IS_RIGID` | `is_rigid` 块出现 |
/// | `0x04 YAW_CONSTRAINT` | `yaw_constraint` 出现（`is_flexible` **或** `is_rigid` 内） |
/// | `0x08 PITCH_CONSTRAINT` | `pitch_constraint` 出现（同上） |
/// | `0x10 ANGLE_CONSTRAINT` | `angle_constraint` 出现（任何块） |
/// | **`0x20 LENGTH_CONSTRAINT`** | **`is_flexible` 或 `is_rigid` 出现就置位**；`allow_length_flex` 清掉它 |
/// | `0x40 BASE_SPRING` | `has_base_spring` 块出现 |
///
/// ⚠️ `0x20` 与**块**有关、与里头的字段无关（`jig6` 的 `is_flexible`/`is_rigid`
/// 分别得 `0x21`/`0x22`）；而 `jig4`（`is_flexible` 里有 `allow_length_flex`）
/// 得 `0x01`。`jig43`（`is_rigid` 里写 yaw/pitch 约束）得 `0x2e`。
///
/// # 角度单位：**只有 `yaw_constraint`/`pitch_constraint`/`angle_constraint` 是角度**
///
/// 输入是**度**，写盘转**弧度**（实测 `angle_constraint 60` → `1.0471976` = π/3，
/// 转换公式是反编译出的 `d * π / 180`，**只转一次**）。
///
/// ⚠️ `left_constraint`/`up_constraint`/`forward_constraint` **不是角度**、
/// **不转**（实测 `jig38`：QC 的 `-0.5 0.5` → 盘上 `baseMinLeft = -0.5`）。
///
/// # 钳位
///
/// `is_flexible` 的 6 个 `*_stiffness`/`*_damping` 与 `has_base_spring` 的
/// `stiffness`/`damping` 经官方 `FUN_004542d0` 钳到 `[0, 1000]`（实测 `jig35`/`jig36`）。
/// **`*_friction`/`*_bounce` 不钳位**（实测 `jig41`/`jig42` 的 `5000`/`6000`/`-7` 原值透传）。
fn resolve_jiggle_bones(compiled: &mut CompiledModelDesc) -> Result<(), Vec<CompileError>> {
    use crate::model::clamp_stiffness;
    use crate::model::jiggle_defaults as def;
    use crate::model::jiggle_flags as fl;

    let mut errs: Vec<CompileError> = Vec::new();
    let desc = &compiled.desc;
    let deg2rad = |d: f32| d * std::f32::consts::PI / 180.0;

    let mut out = Vec::with_capacity(desc.jiggle_bones.len());
    for (i, j) in desc.jiggle_bones.iter().enumerate() {
        let at = format!("jiggle_bones[{i}]");
        // 目标骨骼找不到 ⟹ **丢弃这一条**，不是错误。
        //
        // 官方 `TagProceduralBones`（`simplify.cpp:3911-3989`）对每一类程序化
        // 骨骼都先 `findGlobalBone`，`bone == -1` 时只
        // `printf("… \"%s\" unused\n", …)` 然后 `continue;
        // // optimized out, don't complain`（`:3922-3929` axisinterp、
        // `:3948-3955` quatinterp、`:3972-3979` aimat）。
        //
        // jigglebone 是 L4D2 新增的（darkm 的 `TagProceduralBones` 里
        // **没有**这个分支，grep `jiggle` 在该目录 0 命中），但 L4D2 官方
        // exe 里有同形串 `jigglebone "%s" unused`（@0x5763e4，与
        // `axisinterpbone "%s" unused` / `quatinterpbone "%s" unused` /
        // `<aimconstraint> "%s" unused` 并排）⟹ 同一条语义。
        //
        // ⭐ 实测 `docs/_probe/oracle_jiggle_missing_bone.js`：官方对
        // `$jigglebone "nosuchbone"` **编过**，stdout 只打
        // `jigglebone "nosuchbone" unused`，产物里既没有该 proctype 条目、
        // 也不影响同一 QC 里其它 jigglebone。修前 mdlc 在这里报
        // `jiggle_bones[1]: 骨骼 "nosuchbone" 找不到` 并 `exit=1`。
        //
        // ⚠️ 注意：**control/parent 骨骼**找不到才是硬错误
        // （`Missing control bone "%s" for procedural bone "%s"`），
        // 见 `resolve_quat_interp_bones` 里的 control 段。
        let Some(&bone) = desc.bone_index().get(j.bone.as_str()) else {
            log::info!(
                "{}",
                crate::tr_fmt!(
                    "Note: the bone %{bone} of %{at} was not found; skipped, matching the official behavior",
                    bone = format!("{:?}", j.bone),
                    at = at
                )
            );
            continue;
        };

        let mut flags = 0i32;
        // 缺省：length 10、六个 stiffness 100、其余 0、base 三轴 ±100。
        let mut length = def::LENGTH;
        let mut tip_mass = 0.0;
        let mut yaw_stiffness = def::STIFFNESS;
        let mut yaw_damping = 0.0;
        let mut pitch_stiffness = def::STIFFNESS;
        let mut pitch_damping = 0.0;
        let mut along_stiffness = def::STIFFNESS;
        let mut along_damping = 0.0;
        let mut angle_limit = 0.0;
        let (mut min_yaw, mut max_yaw, mut yaw_friction, mut yaw_bounce) = (0.0, 0.0, 0.0, 0.0);
        let (mut min_pitch, mut max_pitch, mut pitch_friction, mut pitch_bounce) =
            (0.0, 0.0, 0.0, 0.0);
        let mut base_mass = 0.0;
        let mut base_stiffness = def::STIFFNESS;
        let mut base_damping = 0.0;
        let (mut base_min_left, mut base_max_left, mut base_left_friction) =
            (def::BASE_MIN, def::BASE_MAX, 0.0);
        let (mut base_min_up, mut base_max_up, mut base_up_friction) =
            (def::BASE_MIN, def::BASE_MAX, 0.0);
        let (mut base_min_fwd, mut base_max_fwd, mut base_fwd_friction) =
            (def::BASE_MIN, def::BASE_MAX, 0.0);

        // ---- 唯一的一条应用路径：按 token 顺序逐条写 ----
        for (k, w) in j.effective_writes().iter().enumerate() {
            let v0 = w.values.first().copied().unwrap_or(0.0);
            let v1 = w.values.get(1).copied().unwrap_or(0.0);
            match w.key.as_str() {
                // 块进入：只动 `flags`。
                "is_flexible" => flags |= fl::IS_FLEXIBLE | fl::HAS_LENGTH_CONSTRAINT,
                "is_rigid" => flags |= fl::IS_RIGID | fl::HAS_LENGTH_CONSTRAINT,
                "has_base_spring" => flags |= fl::HAS_BASE_SPRING,
                // `allow_length_flex` 清 `0x20`（唯一会**清**位的键）。
                "allow_length_flex" => flags &= !fl::HAS_LENGTH_CONSTRAINT,
                // 三块共享的通用键（值不钳位，除非是 stiffness/damping）。
                "length" => length = v0,
                "tip_mass" => tip_mass = v0,
                "angle_constraint" => {
                    angle_limit = deg2rad(v0);
                    flags |= fl::HAS_ANGLE_CONSTRAINT;
                }
                "yaw_constraint" => {
                    min_yaw = deg2rad(v0);
                    max_yaw = deg2rad(v1);
                    flags |= fl::HAS_YAW_CONSTRAINT;
                }
                "yaw_friction" => yaw_friction = v0,
                "yaw_bounce" => yaw_bounce = v0,
                "pitch_constraint" => {
                    min_pitch = deg2rad(v0);
                    max_pitch = deg2rad(v1);
                    flags |= fl::HAS_PITCH_CONSTRAINT;
                }
                "pitch_friction" => pitch_friction = v0,
                "pitch_bounce" => pitch_bounce = v0,
                // `is_flexible` 独有：6 个 stiffness/damping **要钳位**。
                "yaw_stiffness" => yaw_stiffness = clamp_stiffness(v0),
                "yaw_damping" => yaw_damping = clamp_stiffness(v0),
                "pitch_stiffness" => pitch_stiffness = clamp_stiffness(v0),
                "pitch_damping" => pitch_damping = clamp_stiffness(v0),
                "along_stiffness" => along_stiffness = clamp_stiffness(v0),
                "along_damping" => along_damping = clamp_stiffness(v0),
                // `has_base_spring` 独有：`stiffness`/`damping` **要钳位**。
                "stiffness" => base_stiffness = clamp_stiffness(v0),
                "damping" => base_damping = clamp_stiffness(v0),
                "base_mass" => base_mass = v0,
                // ⚠️ 这三个**不是角度**，原值透传（实测 `jig38`）。
                "left_constraint" => {
                    base_min_left = v0;
                    base_max_left = v1;
                }
                "up_constraint" => {
                    base_min_up = v0;
                    base_max_up = v1;
                }
                "forward_constraint" => {
                    base_min_fwd = v0;
                    base_max_fwd = v1;
                }
                "left_friction" => base_left_friction = v0,
                "up_friction" => base_up_friction = v0,
                "forward_friction" => base_fwd_friction = v0,
                other => {
                    // `validate()` 已经拦过；这里兜底，绝不静默忽略
                    // （官方是 `$jigglebone: invalid syntax` + abort）。
                    errs.push(e(
                        format!("{at}.writes[{k}]"),
                        crate::tr_fmt!(
                            "unknown `$jigglebone` key %{key}",
                            key = format!("{other:?}")
                        ),
                    ));
                }
            }
        }

        out.push(crate::model::ResolvedJiggleBone {
            bone: bone as i32,
            flags,
            length,
            tip_mass,
            yaw_stiffness,
            yaw_damping,
            pitch_stiffness,
            pitch_damping,
            along_stiffness,
            along_damping,
            angle_limit,
            min_yaw,
            max_yaw,
            yaw_friction,
            yaw_bounce,
            min_pitch,
            max_pitch,
            pitch_friction,
            pitch_bounce,
            base_mass,
            base_stiffness,
            base_damping,
            base_min_left,
            base_max_left,
            base_left_friction,
            base_min_up,
            base_max_up,
            base_up_friction,
            base_min_forward: base_min_fwd,
            base_max_forward: base_max_fwd,
            base_forward_friction: base_fwd_friction,
        });
    }
    compiled.resolved_jiggle_bones = out;

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// 解析 `[[quat_interp_bones]]`：名字 → 下标、角度（度）→ 四元数、
/// `inv_tolerance = 1/tolerance`。
///
/// 复刻 `Grab_QuatInterpBones`（`studiomdl.cpp:6371-6400`）与写出
/// （`write.cpp:257-263`）：
///
/// ```text
/// tolerance  = DEG2RAD(<trigger> 的第 1 个数字)
/// trigger[k] = AngleQuaternion(DEG2RAD(<trigger> 的 tx ty tz))
/// quat[k]    = AngleQuaternion(DEG2RAD(<trigger> 的 ax ay az))
/// pos[k]     = <basepos> + (px py pz)          // VectorAdd
/// 落盘 inv_tolerance = 1.0 / tolerance         // write.cpp:259
/// ```
///
/// ⚠️ **`inv_tolerance` 是倒数**，不是 `tolerance` 本身 —— 实测语料
/// `survivor_producer` 的第一个触发器 `inv_tolerance = 0.6366197466850281`
/// = `1 / 1.5707963`（即 `tol = 90°`）。
fn resolve_quat_interp_bones(compiled: &mut CompiledModelDesc) -> Result<(), Vec<CompileError>> {
    let bone_index = compiled.desc.bone_index();
    let mut out: Vec<crate::model::ResolvedQuatInterpBone> = Vec::new();
    let mut errs: Vec<CompileError> = Vec::new();

    for (qi, q) in compiled.desc.quat_interp_bones.iter().enumerate() {
        let at = format!("quat_interp_bones[{qi}]");
        // 目标骨骼找不到 ⟹ **丢弃这一条**，不是错误（同 `resolve_jiggle_bones`）。
        // 官方 `simplify.cpp:3948-3955`：`printf("quatinterpbone \"%s\" unused\n")`
        // 后 `continue; // optimized out, don't complain`。L4D2 exe 里该串在
        // @0x5764a4。修前 mdlc 在这里报「骨骼 {:?} 找不到」并中止编译。
        let Some(&bone) = bone_index.get(q.bone.as_str()) else {
            log::info!(
                "{}",
                crate::tr_fmt!(
                    "Note: the bone %{bone} of %{at} was not found; skipped, matching the official behavior",
                    bone = format!("{:?}", q.bone),
                    at = at
                )
            );
            continue;
        };
        let bone = bone as i32;
        // `Missing control bone "…" for procedural bone "…"` ——
        // `simplify.cpp:3957-3959` 是**硬错误**，这里同样拒绝。
        let Some(&control) = bone_index.get(q.control.as_str()) else {
            errs.push(e(
                &at,
                crate::tr_fmt!(
                    "cannot find the control bone %{control} (the target bone is %{bone})",
                    control = format!("{:?}", q.control),
                    bone = format!("{:?}", q.bone)
                ),
            ));
            continue;
        };
        let control = control as i32;
        let base = q.base_pos.unwrap_or([0.0; 3]);
        let mut triggers = Vec::with_capacity(q.triggers.len());
        for t in &q.triggers {
            let tolerance = t.tolerance.to_radians();
            // 触发四元数 / 目标四元数：`AngleQuaternion` 的输入是弧度。
            let trigger = crate::bone_math::angle_quaternion([
                t.trigger[0].to_radians(),
                t.trigger[1].to_radians(),
                t.trigger[2].to_radians(),
            ]);
            let quat = crate::bone_math::angle_quaternion([
                t.angles[0].to_radians(),
                t.angles[1].to_radians(),
                t.angles[2].to_radians(),
            ]);
            let p = t.pos.unwrap_or([0.0; 3]);
            triggers.push(crate::model::ResolvedQuatInterpTrigger {
                // `1.0 / tolerance` —— 官方写盘时取倒数（`write.cpp:259`）。
                inv_tolerance: 1.0 / tolerance,
                trigger,
                pos: [base[0] + p[0], base[1] + p[1], base[2] + p[2]],
                quat,
            });
        }
        out.push(crate::model::ResolvedQuatInterpBone {
            bone,
            control,
            triggers,
        });
    }
    compiled.resolved_quat_interp_bones = out;

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// 把一个 lid 三元组的名字解析成 flexdesc 下标。
///
/// 填 `fd[3]`（lowerer/neutral/raiser 下标）、`tgt[3]`（target）、
/// `lid_base`（基准 `<type>` 的下标，落 `upperlidflexdesc`）。
fn resolve_lid(
    lid: &crate::model::EyeballLid,
    flexdesc_index: &HashMap<&str, usize>,
    fd: &mut [i32; 3],
    tgt: &mut [f32; 3],
    lid_base: &mut i32,
) -> Result<(), String> {
    let get = |name: &str| -> Result<i32, String> {
        flexdesc_index.get(name).map(|&i| i as i32).ok_or_else(|| {
            crate::tr_fmt!(
                "the flexdesc %{name} was not found in [[flex_descriptors]]",
                name = format!("{name:?}")
            )
        })
    };
    *lid_base = get(&lid.lid_flexdesc)?;
    fd[0] = get(&lid.lowerer.flexdesc)?;
    fd[1] = get(&lid.neutral.flexdesc)?;
    fd[2] = get(&lid.raiser.flexdesc)?;
    tgt[0] = lid.lowerer.target;
    tgt[1] = lid.neutral.target;
    tgt[2] = lid.raiser.target;
    Ok(())
}

/// 把一条序列的各格动画权重合并成序列权重：**逐骨骼取 MAX**。
///
/// # 官方依据（`simplify.cpp:302-318`，逐字）
///
/// ```c
/// for (i = 0; i < g_sequence.Count(); i++) {
///     for (n = 0; n < g_numbones; n++) {
///         g_sequence[i].weight[n] = 0.0;
///         for (j = 0; j < g_sequence[i].groupsize[0]; j++)
///             for (k = 0; k < g_sequence[i].groupsize[1]; k++)
///                 g_sequence[i].weight[n] =
///                     MAX( g_sequence[i].weight[n], g_sequence[i].panim[j][k]->weight[n] );
///     }
/// }
/// ```
///
/// ⚠️ 是 **MAX 而不是「取第一格」** —— blend 序列的每一格可能是不同动画，
/// 各自带不同权重表。
///
/// 空的 `cells`（理论上不会出现）回落到全 1。
fn merge_weights(cells: &[usize], anim_weights: &[Vec<f32>], n_bones: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; n_bones];
    for &c in cells {
        let Some(w) = anim_weights.get(c) else {
            continue;
        };
        for (n, slot) in out.iter_mut().enumerate() {
            if let Some(v) = w.get(n) {
                *slot = slot.max(*v);
            }
        }
    }
    // 没有任何格（或格为空）⟹ 全 1，与「无 `$weightlist`」一致。
    if cells.is_empty() {
        return default_weight_list(n_bones);
    }
    out
}

/// 把权重表解析成**逐骨骼权重数组**（`float[numbones]`）。
///
/// # 算法（`buildAnimationWeights`，`simplify.cpp:1646-1719` 逐字复刻）
///
/// ```c
/// for (i = 0; i < g_numweightlist; i++) {
///     if (i == 0) {                              // 隐式默认表
///         for (j) if (parent[j] != -1) weight[j] = -1;   // 子骨骼：未初始化
///                 else                 weight[j] = 1;    // 根骨骼：1
///     } else {
///         for (j) if (parent[j] != -1) weight[j] = g_weightlist[0].weight[j];
///                 else                 weight[j] = 0;    // 根骨骼：**0**
///     }
///     for (j = 0; j < numbones; j++) {           // 显式条目
///         k = findGlobalBone(bonename[j]);
///         if (k == -1) MdlError(...);            // 未知骨骼 = 硬错误
///         weight[k] = boneweight[j];
///     }
/// }
/// for (i) for (j) {                              // 沿父链补齐
///     if (weight[j] < 0.0 && parent[j] != -1)
///         weight[j] = weight[parent[j]];
/// }
/// ```
///
/// # ⚠️ 三个容易读错的点（都有实测判据）
///
/// 1. **`i != 0` 的表把根骨骼置 0，不是 1。**
///    实测 `$weightlist WL mid 0.5` → `[0, 0.5, 0.5, 0.5]`（root = **0**）。
/// 2. **`i != 0` 抄表 0 时抄到的是 `-1`（哨兵），不是 1。**
///    因为第 ③ 步（沿父链补齐）在**全部**表初始化完之后才跑 ——
///    抄的那一刻表 0 的子骨骼还是 `-1`。所以子骨骼最终由第 ③ 步
///    按**父链**决定，而不是「默认 1」。
/// 3. **沿父链补齐是「子取父」，方向向下。**
///    实测 `mid 0.5` 让 `leaf`/`tip` 也变 0.5。
///
/// # 返回
///
/// 与 `desc.weight_lists` **等长**的数组（不含隐式表 0）。
/// 调用方用 [`weight_list_index`] 拿到「官方下标」（从 1 起）后减 1 索引。
pub fn resolve_weight_lists(desc: &ModelDesc) -> Vec<Vec<f32>> {
    resolve_all_weight_lists(desc).1
}

pub fn resolve_default_weight_list(desc: &ModelDesc) -> Vec<f32> {
    resolve_all_weight_lists(desc).0
}

fn resolve_all_weight_lists(desc: &ModelDesc) -> (Vec<f32>, Vec<Vec<f32>>) {
    let n = desc.bones.len();
    let index = desc.bone_index();
    // ⚠️ 官方 `findGlobalBone` 用 **`stricmp`**（`simplify.cpp:2628`），
    // 所以 `$weightlist WL MID 0.5` 能命中 `mid`。
    // `bone_index()` 是**大小写敏感**的，直接用它会让这条静默失效 ——
    // 而 `validate()` 现在用的是大小写不敏感的表，两边必须一致。
    let ci: std::collections::HashMap<String, usize> = desc
        .bones
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.to_ascii_lowercase(), i))
        .collect();
    let parents: Vec<i32> = desc
        .bones
        .iter()
        .map(|b| match b.parent.as_deref() {
            Some(p) => index.get(p).map(|v| *v as i32).unwrap_or(-1),
            None => -1,
        })
        .collect();

    // 表 0 = 隐式默认：根 1、子骨骼 -1（哨兵）。
    let mut default: Vec<f32> = (0..n)
        .map(|j| if parents[j] != -1 { -1.0 } else { 1.0 })
        .collect();
    for entry in &desc.default_weight_list {
        if let Some(&bone) = ci.get(&entry.bone.to_ascii_lowercase()) {
            default[bone] = entry.weight;
        }
    }

    let mut out: Vec<Vec<f32>> = Vec::with_capacity(desc.weight_lists.len());
    for wl in &desc.weight_lists {
        // `i != 0` 分支：根 0、子骨骼抄表 0（此刻表 0 的子骨骼仍是 -1）。
        let mut w: Vec<f32> = (0..n)
            .map(|j| if parents[j] != -1 { default[j] } else { 0.0 })
            .collect();
        for e in &wl.bones {
            if let Some(&k) = ci.get(&e.bone.to_ascii_lowercase()) {
                w[k] = e.weight;
            }
        }
        out.push(w);
    }

    // 沿父链补齐 —— **表 0 与所有具名表都要跑**。
    // 表 0 的子骨骼是 `-1`，必须由父链补成 1。
    for w in std::iter::once(&mut default).chain(out.iter_mut()) {
        // `j` 升序：父下标一定小于子下标（`validate()` 保证）。
        for j in 0..n {
            if w[j] < 0.0 && parents[j] != -1 {
                w[j] = w[parents[j] as usize];
            }
        }
    }

    (default, out)
}

/// 隐式默认权重表（`g_weightlist[0]`，全 1）。
///
/// 实测：`probe_weightlist_semantics.js` 的 `none` 行 → `[1,1,1,1]`。
pub fn default_weight_list(n_bones: usize) -> Vec<f32> {
    vec![1.0; n_bones]
}

/// 权重表名 → **官方下标**（从 **1** 起，0 是隐式默认表）。
///
/// 官方查找循环是 `for (i = 1; i < g_numweightlist; i++)` ——
/// **跳过 0**（`studiomdl.cpp:1719`）。找不到报
/// `unknown weightlist '<名>'`。
pub fn weight_list_index(desc: &ModelDesc, name: &str) -> Option<usize> {
    desc.weight_lists
        .iter()
        .position(|w| w.name.eq_ignore_ascii_case(name))
        .map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DescErrorKind, ModelDesc};

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    // ---------------------------------------------------------------------
    // 分配优化的回归测试
    // ---------------------------------------------------------------------
    //
    // `VertexKey` 的 `bones` 从 `Vec<(i32,u32)>` 换成了
    // 「定长数组 + 计数」（为了消掉每顶点一次堆分配）。这带来一个**真实的
    // 风险**：手写 `PartialEq`/`Hash` 时若把尾部未使用的槽位也算进去，
    // 「3 组绑定」与「1 组绑定 + 2 个零填充」就会被判成不同 ——
    // 去重失效 ⟹ 顶点数变多 ⟹ 产物字节变化。

    fn vkey(bones: &[[f32; 2]], pos: [f32; 3]) -> VertexKey {
        vertex_key(&Vertex {
            pos,
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
            bones: bones.to_vec(),
        })
    }

    /// **法线判据是严格 `>`，不是 `>=`**（边界值必须不焊）。
    ///
    /// 官方：`DotProduct(g_normal[i], normal) > normal_blend`
    /// （`v1support.cpp:39`）。点积**恰好等于** `cos(2°)` 时不成立 ⟹ 追加。
    ///
    /// # 判据用「最小的 `|n|²` 超过阈值」来钉
    ///
    /// `cos(2°)` 不是任何 f32 的精确平方（实测搜遍邻近 128 个位模式都没有），
    /// 所以无法构造 `d == c`。退而求其次，钉住**两个相邻的 `|n|²` 落在
    /// 阈值的哪一侧**：取使 `len*len` 恰好跨过阈值的那一对 `len`，
    /// 断言小的那侧不复用、大的那侧复用。`>` 与 `>=` 在这两侧**必须**
    /// 给出不同结果，否则测试失败。
    #[test]
    fn normal_blend_is_a_strict_inequality() {
        let c = NORMAL_BLEND;
        // 找一对相邻的 f32 `len`，使 `len²` 从 `<= c` 跳到 `> c`。
        let base = c.sqrt();
        let mut below = None; // len² <= c
        let mut above = None; // len² >  c
        for delta in -256i32..=256 {
            let cand = f32::from_bits((base.to_bits() as i32 + delta) as u32);
            let sq = cand * cand;
            if sq <= c {
                if below.is_none_or(|b: f32| b < cand) {
                    below = Some(cand);
                }
            } else if above.is_none_or(|a: f32| cand < a) {
                above = Some(cand);
            }
        }
        let below = below.expect("应能找到 |n|² <= c 的 f32");
        let above = above.expect("应能找到 |n|² >  c 的 f32");
        assert!(
            below * below <= c && above * above > c,
            "夹具不满足跨阈值条件"
        );
        // 二者应相邻（否则判据跨度太大，测不出 `>` vs `>=`）。
        assert_eq!(
            above.to_bits(),
            below.to_bits() + 1,
            "取到的两个 len 不相邻，夹具太松"
        );

        let mk = |n: [f32; 3]| Vertex {
            pos: [1.0, 2.0, 3.0],
            normal: n,
            uv: [0.5, 0.5],
            bones: vec![[0.0, 1.0]],
        };
        let run = |len: f32| {
            let mut pool: Vec<Vertex> = Vec::new();
            let mut table: HashMap<VertexKey, u32> = HashMap::new();
            let mut secondary: HashMap<PosUvKey, Vec<u32>> = HashMap::new();
            let n = [0.0f32, 0.0, len];
            weld_or_push(&mut pool, &mut table, &mut secondary, &mk(n));
            weld_or_push(&mut pool, &mut table, &mut secondary, &mk(n));
            pool.len()
        };

        // `|n|² <= c` ⟹ `>` 与 `>=` 都**不**复用（`<=` 时 `>=` 也不成立）。
        assert_eq!(run(below), 2, "`|n|² <= cos(2°)` ⟹ 必须新增（严格 `>`）");
        // `|n|² > c` ⟹ 都复用。
        assert_eq!(run(above), 1, "`|n|² > cos(2°)` ⟹ 必须复用");

        // ⚠️ 这条才是把 `>` 与 `>=` 分开的判据：阈值本身。
        // 用 `f32` 能表示的、**恰好等于** `c` 的 `|n|²` 不存在，
        // 所以直接构造「法线位模式相同且 `|n|² == c`」不可能；
        // 但可以用**非单位法线**走容差路径，让点积精确等于 `c`：
        // `n1 = [1,0,0]`、`n2 = [c,0,0]` ⟹ 点积 = `1*c = c` **精确**。
        let mut pool3: Vec<Vertex> = Vec::new();
        let mut t3: HashMap<VertexKey, u32> = HashMap::new();
        let mut s3: HashMap<PosUvKey, Vec<u32>> = HashMap::new();
        // 注意：两者法线**不同** ⟹ 走的是容差路径，`d = n1·n2 = c` 精确。
        weld_or_push(&mut pool3, &mut t3, &mut s3, &mk([1.0, 0.0, 0.0]));
        weld_or_push(&mut pool3, &mut t3, &mut s3, &mk([c, 0.0, 0.0]));
        assert_eq!(
            pool3.len(),
            2,
            "点积**恰好等于** `cos(2°)` 时官方不复用（严格 `>`，不是 `>=`）"
        );
    }

    /// **零法线顶点绝不能被焊接** —— 官方的法线判据是 `DotProduct > cos(2°)`，
    /// 对零法线恒为 `0 > 0.99939` = **false**。
    ///
    /// # 这是「HLMV 里网格被撕开」的根因
    ///
    /// 实测（`v_dual_pistola`，用户工程）有 4 个 `nrm=[0,0,0]` 的顶点：
    /// mdlc 的**精确位匹配**快路径把它们当成「完全相同 ⟹ 复用」，
    /// 而官方**永不**复用它们（法线判据不成立）⟹ 官方追加、mdlc 合并。
    ///
    /// 后果：VVD 少 4 个顶点（`111113 → 111109`），两个 bodypart 的
    /// `mesh[4]` 由 **5532** 变成 **5530**（官方与 NekoMDL 都是 5532）。
    /// 顶点池一变，`origMeshVertID` 与整条 VTX 索引全部错位 ⟹ 网格撕裂。
    ///
    /// 判据：三个顶点「除法线外逐位相同」，法线分别是单位向量 / 零 / 半个单位。
    /// 只有**单位向量**那一对能焊，另两个必须各自成点。
    #[test]
    fn zero_and_short_normals_are_never_welded() {
        let mk = |n: [f32; 3]| Vertex {
            pos: [1.0, 2.0, 3.0],
            normal: n,
            uv: [0.5, 0.5],
            bones: vec![[0.0, 1.0]],
        };
        let mut pool: Vec<Vertex> = Vec::new();
        let mut table: HashMap<VertexKey, u32> = HashMap::new();
        let mut secondary: HashMap<PosUvKey, Vec<u32>> = HashMap::new();
        let mut push = |v: &Vertex| weld_or_push(&mut pool, &mut table, &mut secondary, v);

        let unit = mk([0.0, 0.0, 1.0]);
        let a = push(&unit);
        // 同一个单位法线 ⟹ 必须复用（`|n|² = 1 > 0.99939`）。
        let b = push(&unit);
        assert_eq!(a, b, "单位法线且其余逐位相同 ⟹ 应复用");

        // 零法线：`DotProduct = 0`，官方不复用 ⟹ 必须新增。
        let zero = mk([0.0, 0.0, 0.0]);
        let c = push(&zero);
        assert_ne!(a, c, "零法线绝不能被焊到单位法线上（官方判据不成立）");
        // 再来一个零法线：**同样不能互相焊接**（`0 > 0.99939` 仍为 false）。
        let d = push(&zero);
        assert_ne!(c, d, "两个零法线也不能互相焊接 —— 官方每次都追加");

        // 半个单位长度：`DotProduct = 0.25`，同样不成立。
        let half = mk([0.0, 0.0, 0.5]);
        let e = push(&half);
        assert_ne!(a, e, "`|n|² = 0.25` 不满足 2° 容差 ⟹ 不能复用");
        let f = push(&half);
        assert_ne!(e, f, "非单位法线之间也不能互相焊接");

        assert_eq!(
            pool.len(),
            5,
            "应恰好 5 个独立顶点：1 个单位 + 2 个零 + **2 个半长**\
             （非单位法线连自己都不能复用 —— `|n|² = 0.25 < 0.99939`）"
        );
    }

    /// **单位法线仍然走 O(1) 精确路径**（性能护栏）。
    ///
    /// 上面那条修复只在精确命中后**多算一次点积**；对正常（单位）法线
    /// 结果不变，所以绝大多数模型完全不受影响 ——
    /// 实测 parity 101 个夹具**逐字节不变**。
    #[test]
    fn unit_normals_still_use_the_exact_path() {
        let mut pool: Vec<Vertex> = Vec::new();
        let mut table: HashMap<VertexKey, u32> = HashMap::new();
        let mut secondary: HashMap<PosUvKey, Vec<u32>> = HashMap::new();
        // 1000 个顶点，位置各不同 ⟹ 全部新增，一个都不该被误焊。
        for i in 0..1000 {
            let v = Vertex {
                pos: [i as f32, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                bones: vec![[0.0, 1.0]],
            };
            weld_or_push(&mut pool, &mut table, &mut secondary, &v);
        }
        assert_eq!(pool.len(), 1000, "位置不同 ⟹ 1000 个独立顶点");
        // 再把第 0 个重复一次 ⟹ 必须复用，不新增。
        let again = Vertex {
            pos: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.0, 0.0],
            bones: vec![[0.0, 1.0]],
        };
        let i = weld_or_push(&mut pool, &mut table, &mut secondary, &again);
        assert_eq!(i, 0, "重复顶点应复用下标 0");
        assert_eq!(pool.len(), 1000, "复用时不得新增");
    }

    fn hash_of(k: &VertexKey) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        h.finish()
    }

    /// 绑定组数不同 ⟹ 键必须不同（否则会把不同蒙皮的顶点合并）。
    #[test]
    fn vertex_key_distinguishes_bone_counts() {
        let a = vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.0]);
        let b = vkey(&[[0.0, 1.0], [1.0, 0.0]], [1.0, 2.0, 3.0]);
        let c = vkey(&[[0.0, 1.0], [1.0, 0.0], [0.0, 0.0]], [1.0, 2.0, 3.0]);
        assert_ne!(a, b, "1 组 vs 2 组必须不同");
        assert_ne!(b, c, "2 组 vs 3 组必须不同");
        assert_ne!(a, c);
    }

    /// 同内容同组数 ⟹ 必须相等**且哈希相同**。
    #[test]
    fn vertex_key_equal_for_identical_bones() {
        let a = vkey(&[[0.0, 0.5], [1.0, 0.5]], [1.0, 2.0, 3.0]);
        let b = vkey(&[[0.0, 0.5], [1.0, 0.5]], [1.0, 2.0, 3.0]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b), "相等对象必须同哈希");
    }

    /// **绑定顺序不同但语义相同**必须合并（键里会先排序）。
    #[test]
    fn vertex_key_is_order_insensitive_within_bones() {
        let a = vkey(&[[0.0, 0.5], [1.0, 0.5]], [1.0, 2.0, 3.0]);
        let b = vkey(&[[1.0, 0.5], [0.0, 0.5]], [1.0, 2.0, 3.0]);
        assert_eq!(a, b, "绑定顺序不应影响去重");
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    /// **尾部填充槽位绝不能参与比较**（这是手写 `PartialEq` 最容易犯的错）。
    ///
    /// `bones` 是定长数组 + `n_bones` 计数。若把整个数组拿来比，
    /// 「1 组绑定」与「1 组绑定 + 未使用槽位里的垃圾」就会被判成不同 ⟹
    /// 去重失效 ⟹ 顶点数变多 ⟹ 产物字节变化。
    ///
    /// 本测试往**未使用**的槽位里塞垃圾，断言相等性与哈希都不受影响。
    #[test]
    fn vertex_key_ignores_unused_padding_slots() {
        let mut a = vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.0]);
        let b = vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.0]);
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));

        assert_eq!(a.n_bones, 1, "本夹具只有 1 组有效绑定");
        a.bones[1] = (999, 0xDEAD_BEEF);
        a.bones[2] = (123, 0x1234_5678);
        assert_eq!(a, b, "未使用的填充槽位不得参与相等性比较（否则去重会失效）");
        assert_eq!(
            hash_of(&a),
            hash_of(&b),
            "未使用的填充槽位不得参与哈希（否则 HashMap 行为不一致）"
        );
    }

    /// 位置/法线/UV 任一不同 ⟹ 键不同。
    #[test]
    fn vertex_key_distinguishes_geometry() {
        let base = vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.0]);
        let other_pos = vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.5]);
        assert_ne!(base, other_pos, "位置不同必须区分");

        // 法线不同（构造完整 Vertex，走真实的 vertex_key 路径）。
        let other_normal = vertex_key(&Vertex {
            pos: [1.0, 2.0, 3.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0, 0.0],
            bones: vec![[0.0, 1.0]],
        });
        assert_ne!(base, other_normal, "法线不同必须区分");

        // UV 不同。
        let other_uv = vertex_key(&Vertex {
            pos: [1.0, 2.0, 3.0],
            normal: [0.0, 0.0, 1.0],
            uv: [0.5, 0.0],
            bones: vec![[0.0, 1.0]],
        });
        assert_ne!(base, other_uv, "UV 不同必须区分");
    }

    /// **`-0.0` 必须归一化到 `0.0`**（否则同一位置会被拆成两个顶点）。
    #[test]
    fn vertex_key_normalizes_negative_zero() {
        let a = vkey(&[[0.0, 1.0]], [0.0, 1.0, 2.0]);
        let b = vkey(&[[0.0, 1.0]], [-0.0, 1.0, 2.0]);
        assert_eq!(a, b, "-0.0 与 0.0 必须视为同一位置");
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    /// `NaN` 用位模式比较 ⟹ 相同位模式的 NaN 必须相等（`==` 语义会失效）。
    #[test]
    fn vertex_key_compares_nan_by_bits() {
        let a = vkey(&[[0.0, 1.0]], [f32::NAN, 0.0, 0.0]);
        let b = vkey(&[[0.0, 1.0]], [f32::NAN, 0.0, 0.0]);
        assert_eq!(a, b, "同一位模式的 NaN 必须相等（按位比较）");
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    /// 键的相等性必须与「直接用 HashMap 去重」的结果一致 ——
    /// 这是 `build_meshes` 真正依赖的性质。
    #[test]
    fn vertex_key_dedups_consistently_in_hashmap() {
        let keys = [
            vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.0]),
            vkey(&[[0.0, 1.0]], [1.0, 2.0, 3.0]), // 重复
            vkey(&[[1.0, 1.0]], [1.0, 2.0, 3.0]), // 不同骨骼
            vkey(&[[0.0, 1.0]], [1.0, 2.0, 4.0]), // 不同位置
            vkey(&[[1.0, 0.5], [0.0, 0.5]], [1.0, 2.0, 3.0]),
            vkey(&[[0.0, 0.5], [1.0, 0.5]], [1.0, 2.0, 3.0]), // 与上一条等价
        ];
        let mut m: HashMap<VertexKey, u32> = HashMap::new();
        for k in keys {
            let n = m.len() as u32;
            m.entry(k).or_insert(n);
        }
        assert_eq!(m.len(), 4, "6 个键应去重成 4 个");
    }

    /// **骨骼查找表提出顶点循环后，报错信息必须仍然准确。**
    ///
    /// `VertexBoneMap` 把 `node_names` / `desc_index` / 两个计数缓存起来，
    /// 报错文案里用的 `node_count` 必须等于 `smd.nodes.len()`，
    /// 否则「nodes 段只有 N 项」会给出错误数字。
    #[test]
    fn vertex_bone_map_reports_correct_node_count() {
        let dir = std::env::temp_dir().join("mdlc_opt_bonemap_test");
        let _ = std::fs::create_dir_all(&dir);
        // SMD 引用了一个不存在的骨骼下标 7（nodes 只有 2 项）。
        let smd = SMD.replace("1 1 1.000000", "1 7 1.000000");
        write(&dir, "bad.smd", &smd);
        let toml = desc_toml("bad.smd");
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        // ⚠️ 文案在 `compile()` 那一刻就渲染好了（`crate::tr_fmt!`），
        // 所以语言必须在**调用**那一刻生效 —— 事后包 `with_english` 是空操作。
        let errs = crate::test_locale::with_english(|| compile(&desc, &dir).unwrap_err());
        let joined = errs
            .iter()
            .map(|e| e.message())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("the nodes section only has 2 entries"),
            "报错必须给出正确的 nodes 项数，实际：{joined}"
        );
    }

    /// 与 SMD 配套的最小描述。
    fn desc_toml(smd: &str) -> String {
        format!(
            r#"
[model]
name = "models/test/minimal.mdl"
surface_prop = "metal"

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
smd = "{smd}"
"#
        )
    }

    /// 与 [`desc_toml`] 同形，但材质表里放的是 [`crate::fbx::FALLBACK_MATERIAL`]。
    ///
    /// [`MIN_FBX`] 里的网格**没有任何材质**，FBX 路径会合成兜底名
    /// `debug/debugempty`（官方行为，见 `docs/fbx-support.md` §1.9），
    /// 而 `desc_toml` 的材质表里只有 `myprop` ⟹ 报「材质名找不到」。
    /// 要编过就必须把这个兜底名登记进材质表。
    fn fbx_desc_toml() -> String {
        fbx_desc_toml_with_body("anim.fbx")
    }

    /// 同 [`fbx_desc_toml`]，但 `[[bodyparts]]` 的几何源换成 `body`。
    ///
    /// 用于「同一个 FBX 既作网格源又作动画源」的对照测试 —— 两遍编译只有
    /// `$body` 的源不同，其余完全一致。
    fn fbx_desc_toml_with_body(body: &str) -> String {
        let base = desc_toml(body);
        let needle = "textures = [{ name = \"models/test/myprop\" }]";
        assert!(
            base.contains(needle),
            "desc_toml 的材质表变了，本辅助函数需要同步更新"
        );
        base.replace(
            needle,
            &format!(
                "textures = [{{ name = \"{}\" }}]",
                crate::fbx::FALLBACK_MATERIAL
            ),
        )
    }

    /// 与 [`desc_toml`] 相同，但给 `tip` 注入一个**与 SMD 冲突**的参考姿态
    /// （等价于 QC 的 `$definebone "tip" "root" 0 0 20 0 0 0`）。
    ///
    /// 用于验证顶点重映射：SMD 把 `tip` 放在 `z=8`、顶点也画在 `z=8`；
    /// 真 studiomdl 实测产物是**骨骼 z=20 且顶点 z=20**
    /// （`docs/_probe/oracle_vertex_space2.js`）。
    fn desc_toml_with_tip_pose(smd: &str, tip_z: f32) -> String {
        // 在 `[[bones]] name = "tip"` 之后插入 `position`/`rotation`。
        // 用字符串替换而不是重写整份 TOML —— 这样与 `desc_toml` 永远同源。
        let base = desc_toml(smd);
        let needle = "[[bones]]\nname = \"tip\"\nparent = \"root\"\n";
        assert!(
            base.contains(needle),
            "desc_toml 的骨架变了，本辅助函数需要同步更新"
        );
        base.replace(
            needle,
            &format!(
                "[[bones]]\nname = \"tip\"\nparent = \"root\"\n\
                 position = [0.0, 0.0, {tip_z}]\nrotation = [0.0, 0.0, 0.0]\n"
            ),
        )
    }

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

    /// 与 [`SMD`] 同形，但骨架带**非平凡旋转**（`pitch` 非零）。
    ///
    /// # 为什么单要这一份
    ///
    /// `remap_vertices_is_noop_without_reference_pose_override` 必须用它：
    /// 旋转全 0 时 `canonical_euler` 产出的 `-0.0` 与原始 `0.0` 在矩阵里
    /// 恰好抵消，于是**抓不到**「用矩阵逐位比较当恒等判据」这个 bug ——
    /// 第一版就是零旋转，变异测试时逃逸了。
    ///
    /// 这里给 `root` 一个 `Rx(17.188734°)`（= 0.3 rad）、`tip` 一个
    /// `Ry(-11.459156°)`（= -0.2 rad）—— 都取自 `parity/refpose.toml`
    /// 的受控实验值，保证 `canonical_euler` 真的走一遍分解。
    const SMD_ROT: &str = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.300000 0.000000 0.000000
    1 0.000000 0.000000 8.000000 0.000000 -0.200000 0.000000
end
triangles
myprop
  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000
  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000
end
"#;

    /// [`SMD_ROT`] 里三个三角形顶点的**位置**（逐位可比的 `f32`）。
    ///
    /// 手抄自上面的 SMD 文本 —— 与 `SMD_ROT` 一起改，否则测试会误报。
    const SMD_ROT_VERTEX_POS: [[f32; 3]; 3] =
        [[-8.0, -8.0, 0.0], [8.0, -8.0, 0.0], [0.0, 8.0, 0.0]];

    /// ⭐ **手写的最小 ASCII FBX**（2131 B）：1 个立方体网格 + 1 条动画栈（6 帧）。
    ///
    /// # 为什么要内联一份 FBX
    ///
    /// FBX 夹具的常规来源是 Blender（`_fbxresearch/samples/`），但那些**不能进
    /// 仓库**（体量大、且是外部素材），而 FBX 又**以二进制为主**，没法像 SMD 那样
    /// 直接把文本内联进测试。ASCII FBX 恰好两全：**ufbx 能读**（`ufbx.c` 有完整的
    /// ASCII 解析路径），文本可以内联。
    ///
    /// # 它到底长什么样
    ///
    /// - 8 个控制点（立方体，边长 2，中心在原点）、6 个四边面（`PolygonVertexIndex`
    ///   里每面最后一个下标按 FBX 约定**取反再减 1**，如 `-2` 表示面结束于下标 1）。
    /// - 一个 `Model::box` 节点，`Lcl Translation/Rotation/Scaling` 全是单位值。
    /// - 一条 `AnimStack::Take 001`，其 `T.X` 曲线有 **6 个关键帧**
    ///   （`KeyTime` 从 0 到 230930790000 = 1 秒，FBX 的 KTime 单位是 1/46186158000 秒）
    ///   ⟹ 采出 **6 帧**，正好能触发「同文件静默 1 帧」陷阱（判据要求 `> 1` 帧）。
    ///
    /// ⚠️ 网格节点**自身**是唯一骨骼（无蒙皮）—— 这正是官方骨骼过滤规则的
    /// 「无蒙皮的网格节点自身算一根」（见 `docs/fbx-support.md` §1.4），所以
    /// `desc_toml` 里那两根 `root`/`tip` 会被 SMD 侧忽略、这里只需一根。
    ///
    /// ⚠️ **不要**用 `Count:` 里的数字当真 —— ufbx 不校验它，但保持自洽省得
    /// 以后有人以为是错的。
    const MIN_FBX: &str = r#"; FBX 7.4.0 project file
FBXHeaderExtension:  {
	FBXHeaderVersion: 1003
	FBXVersion: 7400
}
GlobalSettings:  {
	Version: 1000
	Properties70:  {
		P: "UpAxis", "int", "Integer", "",1
		P: "UpAxisSign", "int", "Integer", "",1
		P: "FrontAxis", "int", "Integer", "",2
		P: "FrontAxisSign", "int", "Integer", "",1
		P: "CoordAxis", "int", "Integer", "",0
		P: "CoordAxisSign", "int", "Integer", "",1
		P: "UnitScaleFactor", "double", "Number", "",1
	}
}
Definitions:  {
	Version: 100
	Count: 6
	ObjectType: "Model" { Count: 1 }
	ObjectType: "Geometry" { Count: 1 }
	ObjectType: "AnimationStack" { Count: 1 }
	ObjectType: "AnimationLayer" { Count: 1 }
	ObjectType: "AnimationCurveNode" { Count: 1 }
	ObjectType: "AnimationCurve" { Count: 1 }
}
Objects:  {
	Geometry: 100, "Geometry::box", "Mesh" {
		Vertices: *24 {
			a: -1,-1,-1,1,-1,-1,1,1,-1,-1,1,-1,-1,-1,1,1,-1,1,1,1,1,-1,1,1
		}
		PolygonVertexIndex: *24 {
			a: 0,3,2,-2,4,5,6,-8,0,1,5,-5,3,7,6,-3,0,4,7,-4,1,2,6,-6
		}
		GeometryVersion: 124
	}
	Model: 200, "Model::box", "Mesh" {
		Version: 232
		Properties70:  {
			P: "Lcl Translation", "Lcl Translation", "", "A",0,0,0
			P: "Lcl Rotation", "Lcl Rotation", "", "A",0,0,0
			P: "Lcl Scaling", "Lcl Scaling", "", "A",1,1,1
		}
		Shading: T
		Culling: "CullingOff"
	}
	AnimationStack: 300, "AnimStack::Take 001", "" {
		Properties70:  {
			P: "LocalStart", "KTime", "Time", "",0
			P: "LocalStop", "KTime", "Time", "",230930790000
		}
	}
	AnimationLayer: 400, "AnimLayer::BaseLayer", "" {
	}
	AnimationCurveNode: 500, "AnimCurveNode::T", "" {
		Properties70:  {
			P: "d|X", "Number", "", "A",0
			P: "d|Y", "Number", "", "A",0
			P: "d|Z", "Number", "", "A",0
		}
	}
	AnimationCurve: 600, "AnimCurve::T.X", "" {
		Default: 0
		KeyVer: 4009
		KeyTime: *6 {
			a: 0,46186158000,92372316000,138558474000,184744632000,230930790000
		}
		KeyValueFloat: *6 {
			a: 0,1,2,3,4,5
		}
		KeyAttrFlags: *1 { a: 24836 }
		KeyAttrDataFloat: *4 { a: 0,0,0,0 }
		KeyAttrRefCount: *1 { a: 6 }
	}
}
Connections:  {
	C: "OO",100,200
	C: "OO",300,0
	C: "OO",400,300
	C: "OO",500,400
	C: "OP",600,500, "d|X"
	C: "OP",500,200, "Lcl Translation"
}
"#;

    #[test]
    fn automatic_fbx_axes_reach_reference_geometry_and_shared_animation_sources() {
        let directory = tmpdir("automatic-fbx-axes");
        let source = MIN_FBX
            .replace(
                "\"A\",0,0,0\n\t\t\tP: \"Lcl Rotation\"",
                "\"A\",0,2,0\n\t\t\tP: \"Lcl Rotation\"",
            )
            .replace("600,500, \"d|X\"", "600,500, \"d|Y\"");
        write(&directory, "source.fbx", &source);
        let opts = SrcOpts {
            fbx: crate::fbx::FbxOpts {
                axis: Some(crate::fbx::ForcedAxis::Auto),
                ..Default::default()
            },
            ..Default::default()
        };
        let geometry =
            read_source_geometry(&directory.join("source.fbx"), "source.fbx", &opts).unwrap();
        assert_eq!(geometry.smd.frames[0].poses[0].position, [0.0, 0.0, 2.0]);
        assert!(
            geometry
                .smd
                .triangles
                .iter()
                .flat_map(|triangle| &triangle.vertices)
                .all(|vertex| (1.0..=3.0).contains(&vertex.position[2]))
        );
        let qc = "$modelname models/test/automatic.mdl\n$body body source.fbx srcaxis auto\n$animation named source.fbx srcfps 1\n$sequence implicit source.fbx srcfps 1\n$sequence explicit named\n";
        let desc = crate::qc::parse_qc_str(qc, &directory).unwrap();
        let compiled = compile(&desc, &directory).unwrap();
        assert_eq!(
            resolve_bone_pose(&compiled.desc, &compiled, 0).0,
            [0.0, 0.0, 2.0]
        );
        assert_eq!(compiled.sequences.len(), 2);
        for sequence in &compiled.sequences {
            let last = sequence.frames.last().unwrap()[0].position;
            assert!(
                (last[0]).abs() < 1e-4 && last[1].abs() < 1e-4 && (last[2] - 5.0).abs() < 1e-4,
                "{}: {last:?}",
                sequence.name
            );
        }
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn static_prop_does_not_unbake_a_rotated_fbx_root() {
        let directory = tmpdir("automatic-static-fbx-root");
        let source = MIN_FBX
            .replace(
                "\"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",0,0,0",
                "\"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",-90,0,0",
            )
            .replace(
                "a: -1,-1,-1,1,-1,-1,1,1,-1,-1,1,-1,-1,-1,1,1,-1,1,1,1,1,-1,1,1",
                "a: -2,-3,-5,2,-3,-5,2,3,-5,-2,3,-5,-2,-3,5,2,-3,5,2,3,5,-2,3,5",
            );
        write(&directory, "source.fbx", &source);
        let qc = "$modelname models/test/automatic-static.mdl\n$staticprop\n$body body source.fbx srcaxis auto\n$sequence idle source.fbx\n";
        let desc = crate::qc::parse_qc_str(qc, &directory).unwrap();
        let compiled = compile(&desc, &directory).unwrap();
        let geometry = crate::fbx::read(
            &directory.join("source.fbx"),
            "source.fbx",
            &crate::fbx::FbxOpts {
                axis: Some(crate::fbx::ForcedAxis::Auto),
                ..Default::default()
            },
        )
        .unwrap();
        for mesh in &compiled.bodyparts[0].models[0].meshes {
            for vertex in &mesh.vertices {
                assert!(
                    geometry
                        .smd
                        .triangles
                        .iter()
                        .flat_map(|triangle| &triangle.vertices)
                        .any(|source| static_prop_rotate(source.position)
                            .iter()
                            .zip(vertex.pos)
                            .all(|(a, b)| (a - b).abs() < 1e-5))
                );
            }
        }
        let _ = std::fs::remove_dir_all(directory);
    }

    // ---- `$animation` 的 `subtract` ----
    //
    // 真实项目（miku SPAS-12）里 `look_poses.smd` 与 `a_idle.smd` 的
    // **骨骼顺序不同**（118 个位置里 77 个不同），而 `subtract` 必须按
    // **名字**对齐后相减。这两条测试锁住「按名字映射」与「减除生效」。

    /// 两个 SMD 的骨骼顺序**不同**时，`subtract` 仍按**名字**正确相减。
    ///
    /// 构造：`base.smd` 顺序是 `root, mid, tip`，`pose.smd` 顺序是
    /// `root, tip, mid`（**打乱**）。`tip` 在两边的第 0 帧都是绕 Z 20°，
    /// 而 `pose.smd` 第 0 帧的 `tip` 是 35° —— 减完应当是 15°。
    ///
    /// 若实现按**下标**相减，`tip`（`pose` 的第 1 项）会减到 `base` 的
    /// 第 1 项（`mid`，0°），得到 35° —— 与 15° 差得很明显。
    #[test]
    fn subtract_aligns_bones_by_name_not_index() {
        let d = tmpdir("subtract-byname");
        // base.smd：root(0), mid(1), tip(2)；tip 绕 Z 20°（0.349066 rad）
        let base = r#"version 1
nodes
  0 "root" -1
  1 "mid" 0
  2 "tip" 1
end
skeleton
  time 0
    0 0 0 0 0 0 0
    1 0 0 4 0 0 0
    2 0 0 8 0 0 0.349066
end
triangles
myprop
  2 -8 -8 8 0 0 1 0 0 1 2 1
  2 8 -8 8 0 0 1 1 0 1 2 1
  2 0 8 8 0 0 1 0.5 1 1 2 1
end
"#;
        // pose.smd：**顺序打乱** —— root(0), tip(1), mid(2)
        // tip 第 0 帧绕 Z 35°（弧度 0.610865）
        let pose = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
  2 "mid" 1
end
skeleton
  time 0
    0 0 0 0 0 0 0
    1 0 0 8 0 0 0.610865
    2 0 0 4 0 0 0
end
triangles
myprop
  1 -8 -8 8 0 0 1 0 0 1 1 1
  1 8 -8 8 0 0 1 1 0 1 1 1
  1 0 8 8 0 0 1 0.5 1 1 1 1
end
"#;
        write(&d, "base.smd", base);
        write(&d, "pose.smd", pose);
        let toml = r#"
[model]
name = "models/test/sub.mdl"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "mid"
parent = "root"

[[bones]]
name = "tip"
parent = "mid"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "base.smd"

[[animations]]
name = "base"
smd = "base.smd"

[[animations]]
name = "posed"
smd = "pose.smd"
frames = [0, 0]
subtract = "base"
subtract_frame = 0
"#;
        let desc: ModelDesc = toml::from_str(toml).expect("TOML 解析");
        let c = compile(&desc, &d).expect("编译");
        assert_eq!(c.animations.len(), 2);
        // 先确认**源数据**是对的：base 的 tip 是 20°。
        assert!(
            (c.animations[0].frames[0][2].rotation[2].to_degrees() - 20.0).abs() < 0.01,
            "base 的 tip 应为 20°，实际 {}",
            c.animations[0].frames[0][2].rotation[2].to_degrees()
        );
        let posed = &c.animations[1];
        assert!(posed.delta, "subtract ⇒ delta");
        // `tip` 是**描述骨骼**的第 2 项。
        let tip = posed.frames[0][2].rotation;
        let deg = tip[2].to_degrees();
        assert!(
            (deg - 15.0).abs() < 0.01,
            "tip 的 Z 应为 35° − 20° = 15°，实际 {deg}°（按名字对齐失败？）"
        );
        // `mid` 两边都是 0° ⇒ 减完仍是 0。
        let mid = posed.frames[0][1].rotation;
        assert!(
            mid.iter().all(|v| v.abs() < 1e-4),
            "mid 两边同为 0° ⇒ 差值应为 0，实际 {mid:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// ⭐ `subtract` 可以引用**序列名**，不只是动画名。
    ///
    /// 官方 `LookupAnimation`（`studiomdl.cpp:1674-1692`）**先查动画池
    /// `g_panimation`、查不到再退回 `LookupSequence`**，命中序列时返回它的
    /// `panim[0][0]`（第一格动画）。`subtract` 走的正是它
    /// （`studiomdl.cpp:1733-1751` 的 `else if (stricmp("subtract", token) == 0)`）。
    ///
    /// 用户工程（`linnea_replaces_zoey`）正是这种写法：
    ///
    /// ```text
    /// $sequence  "reference"     "anims/ref.smd"      fps 1
    /// $animation "a_proportions" "anims/foot_fix.smd" subtract "reference" 0
    /// ```
    ///
    /// 修前 mdlc 报 `animations[0].subtract: 找不到参考动画 "reference"`
    /// 并中止编译（官方照编不误）。
    ///
    /// 夹具刻意让序列名**不与任何动画同名**、且序列的 `smd` 也不是任何
    /// `[[animations]]` 的 `smd` —— 逼实现走「序列池 ⟹ 现场读该 SMD」
    /// 那条分支（而不是靠 `anim_index` 里恰好有同名条目蒙对）。
    #[test]
    fn subtract_can_reference_a_sequence_name() {
        let d = tmpdir("subtract-seqname");
        // base.smd：root(0), mid(1), tip(2)；tip 绕 Z 20°（0.349066 rad）
        let base = r#"version 1
nodes
  0 "root" -1
  1 "mid" 0
  2 "tip" 1
end
skeleton
  time 0
    0 0 0 0 0 0 0
    1 0 0 4 0 0 0
    2 0 0 8 0 0 0.349066
end
triangles
myprop
  2 -8 -8 8 0 0 1 0 0 1 2 1
  2 8 -8 8 0 0 1 1 0 1 2 1
  2 0 8 8 0 0 1 0.5 1 1 2 1
end
"#;
        // pose.smd：**骨骼顺序相同**，只有 tip 是 35°（0.610865 rad）
        let pose = r#"version 1
nodes
  0 "root" -1
  1 "mid" 0
  2 "tip" 1
end
skeleton
  time 0
    0 0 0 0 0 0 0
    1 0 0 4 0 0 0
    2 0 0 8 0 0 0.610865
end
triangles
myprop
  2 -8 -8 8 0 0 1 0 0 1 2 1
  2 8 -8 8 0 0 1 1 0 1 2 1
  2 0 8 8 0 0 1 0.5 1 1 2 1
end
"#;
        write(&d, "base.smd", base);
        write(&d, "pose.smd", pose);
        let toml = r#"
[model]
name = "models/test/subseq.mdl"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "mid"
parent = "root"

[[bones]]
name = "tip"
parent = "mid"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "pose.smd"

[[sequences]]
name = "reference"
smd = "base.smd"

[[animations]]
name = "posed"
smd = "pose.smd"
subtract = "reference"
subtract_frame = 0
"#;
        let desc: ModelDesc = toml::from_str(toml).expect("TOML 解析");
        let c = compile(&desc, &d).expect(
            "`subtract` 引用**序列名**必须能编译（官方 `LookupAnimation` \
             查不到动画时会退回序列池）",
        );
        // 按名字取：序列阶段会另建隐含动画（登记在 `@reference` 下），
        // 所以下标不能假定。
        let posed = c
            .animations
            .iter()
            .find(|a| a.name == "posed")
            .expect("产物里应有 posed 动画");
        assert!(posed.delta, "subtract ⇒ delta");
        let deg = posed.frames[0][2].rotation[2].to_degrees();
        assert!(
            (deg - 15.0).abs() < 0.01,
            "tip 的 Z 应为 35° − 20° = 15°，实际 {deg}°（序列名没解析到参考动画？）"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// ⚠️ **回归（R31）**：序列级 `subtract` **引用它自己**必须能解析。
    ///
    /// 官方 `Cmd_Sequence` 在解析体**之前**就把序列 `AddToTail` 进
    /// `g_sequence`（`studiomdl.cpp:2623-2627`）：
    ///
    /// ```c
    /// s_sequence_t *pseq = &g_sequence[ g_sequence.AddToTail() ];  // ← 先入表
    /// memset( pseq, 0, sizeof( s_sequence_t ) );
    /// strcpyn( pseq->name, token );
    /// ParseSequence( pseq, false );                               // ← 再解析体
    /// ```
    ///
    /// 于是 `$sequence X ... subtract X 0` 里的 `LookupAnimation("X")` 能在
    /// **序列池**找到本序列，返回 `pseq->panim[0][0]` —— 那一格此刻已建好。
    /// mdlc 的 `sequences` 要到循环末尾才 push 本序列 ⟹ 必须显式认下自引用。
    ///
    /// 自引用的语义 = 「整条动画变成相对第 0 帧的增量」：
    /// `subtractBaseAnimations` 先把 `psrc->sanim[srcframe]` 快照进局部
    /// `s_bone_t src[]`（`simplify.cpp:1071-1082`），所以 `psrc == pdest`
    /// 也安全。
    ///
    /// 实测触发点：用户工程 `incap_anim_fix\includes\anims_fix.qci:161`
    /// ```text
    /// $sequence IncapIdlenoise NamVet_Idle_Standing_01 X Y Z fixuploop -15 15
    ///   loop weightlist INJUREDIDLENOISE subtract IncapIdlenoise 0 delta hidden
    /// ```
    /// 修前报 `sequences[1].subtract: 找不到参考动画 "IncapIdlenoise"` 并中止。
    #[test]
    fn subtract_can_reference_itself() {
        let d = tmpdir("subtract-self");
        // two.smd：2 帧；tip 的 Z 第 0 帧 20°（0.349066）、第 1 帧 35°（0.610865）
        let two = r#"version 1
nodes
  0 "root" -1
  1 "mid" 0
  2 "tip" 1
end
skeleton
  time 0
    0 0 0 0 0 0 0
    1 0 0 4 0 0 0
    2 0 0 8 0 0 0.349066
  time 1
    0 0 0 0 0 0 0
    1 0 0 4 0 0 0
    2 0 0 8 0 0 0.610865
end
triangles
myprop
  2 -8 -8 8 0 0 1 0 0 1 2 1
  2 8 -8 8 0 0 1 1 0 1 2 1
  2 0 8 8 0 0 1 0.5 1 1 2 1
end
"#;
        write(&d, "two.smd", two);
        let toml = r#"
[model]
name = "models/test/selfsub.mdl"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "mid"
parent = "root"

[[bones]]
name = "tip"
parent = "mid"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "two.smd"

[[sequences]]
name = "selfsub"
smd = "two.smd"
subtract = "selfsub"
subtract_frame = 0
"#;
        let desc: ModelDesc = toml::from_str(toml).expect("TOML 解析");
        let c = compile(&desc, &d).expect(
            "序列级 `subtract` 引用**自己**必须能编译（官方 `Cmd_Sequence` \
             在解析体之前就把序列放进 `g_sequence`）",
        );
        let seq = c
            .sequences
            .iter()
            .find(|s| s.name == "selfsub")
            .expect("产物里应有 selfsub 序列");
        // ⚠️ `subtract` 只置 **animdesc** 的 DELTA，**不置 seqdesc** 的
        // （见上面 `seq_is_delta` 处的 R11 表：`CMD_SUBTRACT` 落 `panim->flags`，
        // `delta` 关键字才落 `pseq->flags`）。所以这里断言的是动画。
        let anim = c
            .animations
            .iter()
            .find(|a| a.name == "@selfsub")
            .expect("产物里应有隐含动画 @selfsub");
        assert!(anim.delta, "subtract ⇒ animdesc 的 DELTA");
        assert!(
            !seq.delta,
            "只有 `subtract`、没有 `delta` 关键字 ⟹ seqdesc **不该**有 DELTA"
        );
        assert_eq!(seq.frames.len(), 2, "两帧都应保留");
        let f0 = seq.frames[0][2].rotation[2].to_degrees();
        let f1 = seq.frames[1][2].rotation[2].to_degrees();
        assert!(
            f0.abs() < 0.01,
            "第 0 帧减去自己 ⟹ 恒等，实际 {f0}°（自引用没解析到？）"
        );
        assert!(
            (f1 - 15.0).abs() < 0.01,
            "第 1 帧应为 35° − 20° = 15°，实际 {f1}°"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `subtract` 出来的动画在**写出阶段**只保留真正变化的骨骼。
    ///
    /// 这是 miku `look_*` 的核心判据：官方 `look_down` 只有 **1** 条轨道
    /// （只有 `ankle`/`body` 变了），而不是全部 118 根。
    #[test]
    fn subtract_marks_only_changed_bones() {
        let d = tmpdir("subtract-tracks");
        write(&d, "base.smd", SMD);
        // pose.smd 与 base.smd **骨骼顺序相同**，只有 tip 的 Z 不同。
        let pose = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
end
skeleton
  time 0
    0 0 0 0 0 0 0
    1 0 0 8 0 0 0.5
end
triangles
myprop
  1 -8 -8 8 0 0 1 0 0 1 1 1
  1 8 -8 8 0 0 1 1 0 1 1 1
  1 0 8 8 0 0 1 0.5 1 1 1 1
end
"#;
        write(&d, "pose.smd", pose);
        let toml = format!(
            "{}[[animations]]\nname = \"base\"\nsmd = \"base.smd\"\n\n\
             [[animations]]\nname = \"posed\"\nsmd = \"pose.smd\"\nframes = [0, 0]\n\
             subtract = \"base\"\nsubtract_frame = 0\n\n\
             [[sequences]]\nname = \"posed\"\nsmd = \"posed\"\n",
            desc_toml("base.smd")
        );
        let desc: ModelDesc = toml::from_str(&toml).expect("TOML 解析");
        let c = compile(&desc, &d).expect("编译");
        assert_eq!(c.animations.len(), 2);
        let posed = &c.animations[1];
        // root 两边都是 0 ⇒ 差值 0；tip 差 0.5 rad ⇒ 非 0。
        assert!(
            posed.frames[0][0].rotation.iter().all(|v| v.abs() < 1e-4),
            "root 应无差异"
        );
        assert!(
            (posed.frames[0][1].rotation[2] - 0.5).abs() < 1e-4,
            "tip 的 Z 应为 0.5 rad，实际 {}",
            posed.frames[0][1].rotation[2]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- `$definebone` 声明的骨骼即使 SMD 里没有也要保留 ----
    //
    // 官方 `BuildGlobalBonetable`（`simplify.cpp:3616-3654`）**先**把
    // `g_importbone`（`$definebone` 收集的）插进骨骼表，**再**并入各 SMD
    // 用到的骨骼（同名靠 `findGlobalBone` 去重）。
    //
    // 判据是 `parity/myprop.qc` → 官方 `myprop.mdl`：
    // QC 写 `$definebone "tip" "root" 0 0 8`，而 `myprop-ref.smd` 的
    // `nodes` **只有 root** —— 官方产物仍是 2 根骨骼，
    // `BONE[1].pos = [0, 0, 8]`、`flags = 0x200`。

    /// SMD 里没有、但 `[[bones]]` 显式给了 `position` 的骨骼**必须保留**，
    /// 参考姿态取那个显式值。
    ///
    /// 这条曾经缺失，症状是 `verify_parity.ps1` 长期红灯：
    /// 报「骨骼数 2 vs 1」以及十几处由它引起的段偏移差异 ——
    /// **看起来像代码坏了，实际是一个真实缺口**。
    #[test]
    fn bone_declared_but_absent_from_smd_is_kept() {
        let d = tmpdir("definebone-only");
        // SMD 只有 root（与 `parity/myprop-ref.smd` 同形态）。
        // 顶点行 12 个 token：parentBone + pos3 + nrm3 + uv2 + **links 数** + bone + weight。
        let smd = r#"version 1
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
        write(&d, "myprop-ref.smd", smd);
        let toml = r#"
[model]
name = "models/test/dbonly.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"
position = [0.0, 0.0, 8.0]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "myprop-ref.smd"

[[sequences]]
name = "idle"
smd = "myprop-ref.smd"
fps = 30.0
"#;
        let desc2: ModelDesc = toml::from_str(toml).expect("TOML 解析");
        let c = compile(&desc2, &d).expect("编译");
        assert_eq!(c.desc.bones.len(), 2, "tip 必须保留（官方 numbones = 2）");
        // 动画帧里 tip 的姿态取自 `[[bones]].position`（= `$definebone` 的
        // rawLocal）—— `poses` 是 SMD 参考帧的**原始记录**，只含 root，
        // 所以要看**编译后的动画帧**。
        let frames = &c.animations[0].frames;
        assert_eq!(
            frames[0][1].position,
            [0.0, 0.0, 8.0],
            "tip 的参考位置应来自显式声明，实际 {:?}",
            frames[0][1].position
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// SMD 里没有、`[[bones]]` 也**没给**姿态的骨骼：**不报错**，参考姿态兜底零。
    ///
    /// # 为什么不再是错误
    ///
    /// 这条曾经是硬错误，理由是「静默用 `[0,0,0]` 会让骨骼塌到原点」。
    /// 但官方**从不做这个检查**：
    ///
    /// - `Grab_Animation`（`studiomdl.cpp:1065-1135`）给每帧
    ///   `kalloc(1, size)`（= `calloc`，**零填充**），再逐骨骼从上一帧拷贝，
    ///   最后才用本帧的骨骼行覆盖。**它从不检查「每根骨骼都有数据」** ——
    ///   本帧没写的骨骼保留上一帧的值（第 0 帧就是 `(0,0,0)`/单位旋转）。
    /// - `TranslateAnimations`（`simplify.cpp:1498-1529`）里查不到骨骼时
    ///   （`q == -1`）只 printf 一句，**不报错**。
    ///
    /// ⭐ 而 L4D2 官方 exe 里根本**没有**「某骨骼在某 SMD 里缺失」类的错误串
    /// （已逐字扫描：只有 `%s is missing frame %d`、`Missing frame start(%d) : %s`、
    /// `Imported bone %s tried to access parent bone %s and failed!` 等）
    /// ⟹ 官方不可能抛这种错。
    ///
    /// 实测反例（用户工程 `survivor_teenangst.qc`）：`anims/foot_fix.smd`
    /// 的 `nodes` 没有 `ValveBiped.forward`，官方照编不误。
    ///
    /// 兜底零与 mdlc 自身的 `resolve_bone_pose`（`compile.rs:4916-4955`）
    /// 口径一致 —— 那里对「body SMD 里没有该骨骼」也是回退 `[0,0,0]`。
    #[test]
    fn bone_absent_from_smd_without_explicit_pose_falls_back_to_zero() {
        let d = tmpdir("definebone-missing");
        let smd = r#"version 1
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
        write(&d, "myprop-ref.smd", smd);
        let toml = r#"
[model]
name = "models/test/dbmiss.mdl"
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

[[sequences]]
name = "idle"
smd = "myprop-ref.smd"
fps = 30.0
"#;
        let desc: ModelDesc = toml::from_str(toml).expect("TOML 解析");
        let c =
            compile(&desc, &d).expect("官方对「SMD 里没有该骨骼」不报错（Grab_Animation 零填充）");
        assert_eq!(c.desc.bones.len(), 2, "tip 必须保留（官方 numbones = 2）");
        let frames = &c.animations[0].frames;
        assert_eq!(
            frames[0][1].position,
            [0.0, 0.0, 0.0],
            "没有姿态来源的骨骼兜底零（与官方 Grab_Animation 的 calloc 一致），\
             实际 {:?}",
            frames[0][1].position
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 每个用例用**唯一**的临时目录：测试并行跑，共用目录会互相删文件。
    fn tmpdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // ---- flex / eyeball / mouth ----
    //
    // 这三条规则都**不在 episode1 源码里**（`dummy_eyelid` grep 零命中、
    // `forward` 的符号被 L4D2 改过），只能用受控实验钉死。对应的官方产物：
    // `docs/_probe/smdl/{fx2,fxr1,fxl1}.qc` → `docs/_probe/artifacts/*.mdl`，
    // 逐字段对照脚本 `docs/_probe/cmp_flex.js`。

    /// **`dummy_eyelid` 自动追加**：只要某个 eyeball 缺 lid 数据，
    /// 就在 flexdesc 数组**末尾**追加一条（若用户没显式写过同名）。
    ///
    /// 受控实验 `fxr1.qc` —— **一个 flexdesc 都没写**，只写了两条 `eyeball`，
    /// 官方产物仍是 `numflexdesc=1`、`fd[0]="dummy_eyelid"`；
    /// `fxl1.qc`（`localvar alpha beta` + eyeball）得到
    /// `[alpha, beta, dummy_eyelid]` 证明它**永远排在最后**。
    /// 对照：4 个 survivor 模型写了 `eyelid`，产物**不含** dummy_eyelid。
    #[test]
    fn dummy_eyelid_appended_for_eyeball_without_eyelid() {
        let d = tmpdir("dummyeyelid");
        write(&d, "myprop-ref.smd", SMD);
        let toml = format!(
            r#"
{}

[[flex_descriptors]]
name = "alpha"

[[bodyparts.models.eyeballs]]
bone = "tip"
org = [0.0, 0.0, 8.0]
material = "models/test/myprop"
radius = 0.5
"#,
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let names: Vec<&str> = c
            .desc
            .flex_descriptors
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["alpha", DUMMY_EYELID],
            "dummy_eyelid 应追加在末尾（受控实验 fxl1）"
        );
        // 三个 lid 槽与两个 `*lidflexdesc` 全指向它，target 恒 [-1,0,1]。
        let eb = &c.bodyparts[0].models[0].eyeballs[0];
        assert_eq!(eb.upperlidflexdesc, 1, "upperlidflexdesc → dummy_eyelid");
        assert_eq!(eb.lowerlidflexdesc, 1, "lowerlidflexdesc → dummy_eyelid");
        assert_eq!(eb.upperflexdesc, [1, 1, 1]);
        assert_eq!(eb.lowerflexdesc, [1, 1, 1]);
        assert_eq!(eb.uppertarget, DUMMY_LID_TARGETS);
        assert_eq!(eb.lowertarget, DUMMY_LID_TARGETS);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **两个 lid 都写齐了才不补** `dummy_eyelid`
    /// （对照 survivor 语料：4/4 上下眼睑都有，产物 0/4 不含 dummy）。
    ///
    /// 只写一个（本例只给 `upper_lid`）仍算「缺 lid」→ 追加 dummy 到末尾，
    /// **缺的那个** 指向 dummy，**给了的那个** 用真实值。
    #[test]
    fn no_dummy_eyelid_only_when_both_lids_given() {
        let d = tmpdir("nodummy");
        write(&d, "myprop-ref.smd", SMD);
        let toml = format!(
            r#"
{}

[[flex_descriptors]]
name = "upper"

[[flex_descriptors]]
name = "upper_lowerer"

[[flex_descriptors]]
name = "upper_neutral"

[[flex_descriptors]]
name = "upper_raiser"

[[bodyparts.models.eyeballs]]
bone = "tip"
org = [0.0, 0.0, 8.0]
material = "models/test/myprop"
radius = 0.5

[bodyparts.models.eyeballs.upper_lid]
lid_flexdesc = "upper"
lowerer = {{ flexdesc = "upper_lowerer", target = -0.1 }}
neutral = {{ flexdesc = "upper_neutral", target = 0.2 }}
raiser = {{ flexdesc = "upper_raiser", target = 0.3 }}
"#,
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let names: Vec<&str> = c
            .desc
            .flex_descriptors
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        // 只给了 upper_lid → 仍缺 lower_lid → 追加 dummy（下标 4）。
        assert_eq!(
            names,
            vec![
                "upper",
                "upper_lowerer",
                "upper_neutral",
                "upper_raiser",
                DUMMY_EYELID
            ]
        );
        let eb = &c.bodyparts[0].models[0].eyeballs[0];
        assert_eq!(eb.upperlidflexdesc, 0, "基准 → upper");
        assert_eq!(eb.upperflexdesc, [1, 2, 3]);
        assert_eq!(eb.uppertarget, [-0.1, 0.2, 0.3]);
        // 下眼睑缺省走 dummy。
        assert_eq!(eb.lowerlidflexdesc, 4, "lowerlidflexdesc → dummy");
        assert_eq!(eb.lowerflexdesc, [4, 4, 4]);
        assert_eq!(eb.lowertarget, DUMMY_LID_TARGETS);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **`up`/`forward` 是骨骼空间的逆旋转**，且 `forward` 的符号是
    /// **`(0,-1,0)`** —— episode1 源码那个 `(0,1,0)` 带
    /// `// FIXME: this is backwards`，L4D2 修掉了。
    ///
    /// 真实语料判据（4 个 survivor / 8 条 eyeball）：
    /// `forward == VectorIRotate((0,-1,0), poseToBone)` **8/8**，
    /// `(0,+1,0)` **0/8**；`up == VectorIRotate((0,0,1))` **8/8**。
    ///
    /// 这里用**恒等参考姿态**（`tip` 无旋转）：`poseToBone` 是单位阵，
    /// 于是 `up=(0,0,1)`、`forward=(0,-1,0)` 可直接读出符号。
    #[test]
    fn eyeball_up_forward_use_bone_space_with_negated_forward() {
        let d = tmpdir("ebfw");
        write(&d, "myprop-ref.smd", SMD);
        let toml = format!(
            r#"
{}

[[bodyparts.models.eyeballs]]
bone = "tip"
org = [0.0, 0.0, 8.0]
material = "models/test/myprop"
radius = 0.5
"#,
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let eb = &c.bodyparts[0].models[0].eyeballs[0];
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6);
        assert!(
            near(eb.up, [0.0, 0.0, 1.0]),
            "up 应为 (0,0,1)，got {:?}",
            eb.up
        );
        assert!(
            near(eb.forward, [0.0, -1.0, 0.0]),
            "forward 应为 (0,-1,0)（L4D2 修掉了源码的 FIXME），got {:?}",
            eb.forward
        );
        // `org` 也要落到骨骼空间：世界 (0,0,8) 在 tip 局部（tip 原点在世界
        // (0,0,8)）就是 (0,0,0)。
        assert!(
            near(eb.org, [0.0, 0.0, 0.0]),
            "org 应逆变换到骨骼空间（got {:?}）",
            eb.org
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// **官方 `flexcontroller` 命令会顺带生成一条 flexcontrollerui**。
    ///
    /// 受控实验 `fx2.qc`（3 个 `flexcontroller`、**没有任何 ui 命令**）的
    /// 官方产物是 `numflexcontrollerui=3`、`flexcontrolleruiindex=2240`
    /// （= flexop 区末尾），每条的 `szindex0` 指向对应 fc 记录（相对自身、
    /// 恒负）、`szindex1=0`、`stereo=0`、`remaptype=0`。
    ///
    /// 报告原先标注「ui 是 DMX 专有」—— 实测 QC 也会生成，所以必须自动产。
    #[test]
    fn each_flexcontroller_gets_a_flexcontrollerui() {
        let d = tmpdir("fcui");
        write(&d, "myprop-ref.smd", SMD);
        let toml = format!(
            r#"
{}

[[flex_controllers]]
name = "right_lid_raiser"
type = "lid"

[[flex_controllers]]
name = "jaw_drop"
type = "mouth"
"#,
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let uis = &c.resolved_flex_controller_ui;
        assert_eq!(uis.len(), 2, "每条 fc 一条 ui");
        assert_eq!(uis[0].name, "right_lid_raiser", "ui 名 = fc 名（fx2 实测）");
        assert_eq!(uis[0].fc0, 0, "szindex0 指向对应 fc 自身");
        assert_eq!(uis[1].fc0, 1);
        assert!(
            uis.iter().all(|u| !u.stereo && u.fc1.is_none()),
            "自动产的都是单声道"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 辅助：给一组 fc 名，返回编译后的 ui 列表（`(name, fc0, fc1, stereo)`）。
    ///
    /// 每个名字都是 `[[flex_controllers]]` 一条，`type` 固定 `"lid"`（除非
    /// 显式传 `(name, type)`）。
    fn fcui_of(names: &[&str]) -> Vec<(String, i32, Option<i32>, bool)> {
        let d = tmpdir("fcuipair");
        write(&d, "myprop-ref.smd", SMD);
        let mut toml = desc_toml("myprop-ref.smd");
        for n in names {
            toml.push_str(&format!(
                "\n[[flex_controllers]]\nname = {n:?}\ntype = \"lid\"\n"
            ));
        }
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let out = c
            .resolved_flex_controller_ui
            .iter()
            .map(|u| (u.name.clone(), u.fc0, u.fc1, u.stereo))
            .collect();
        std::fs::remove_dir_all(&d).ok();
        out
    }

    /// ⭐ **`right_X` + `left_X` 紧邻一对合并成一条 stereo ui**。
    ///
    /// 真 exe 裁决（`docs/_probe/oracle_fcui.js` 的 `fc1`）：
    /// `flexcontroller eyelid right_lid_raiser left_lid_raiser` ⟹
    /// `nfc=2 nUI=1`，`ui[0] name="lid_raiser" stereo=1 s0->fc[1] s1->fc[0]`。
    ///
    /// ⚠️ `fc0` 指向 **`left_`**（下标更大那条），`fc1` 指向 `right_` ——
    /// 与 `studio.h` 的 `pLeftController() = this + szindex0` 一致。
    #[test]
    fn adjacent_right_left_pair_merges_into_stereo_ui() {
        let uis = fcui_of(&["right_lid_raiser", "left_lid_raiser"]);
        assert_eq!(uis.len(), 1, "一对应合并成 1 条 ui，got {uis:?}");
        assert_eq!(uis[0].0, "lid_raiser", "名字 = 去掉 right_/left_ 前缀");
        assert_eq!(uis[0].1, 1, "szindex0 → left_（下标更大）");
        assert_eq!(uis[0].2, Some(0), "szindex1 → right_（下标更小）");
        assert!(uis[0].3, "stereo = true");
    }

    /// 非配对（既无 `right_` 也无相邻 `left_`）各成一条单声道 ui，名字原样。
    ///
    /// `oracle_fcui.js` 的 `fc2`/`fc8`：`half_closed` 单条、4 个无前后缀的名字
    /// ⟹ 条数不变、`stereo=0`、名原样。
    #[test]
    fn unpaired_controllers_each_get_a_mono_ui_with_verbatim_name() {
        let uis = fcui_of(&["half_closed", "bite", "presser", "tightener"]);
        assert_eq!(uis.len(), 4, "4 个非配对 fc → 4 条 ui");
        assert_eq!(uis[0].0, "half_closed");
        assert_eq!(uis[3].0, "tightener");
        for (i, u) in uis.iter().enumerate() {
            assert_eq!(u.1, i as i32, "szindex0 → 自身");
            assert_eq!(u.2, None, "szindex1 = 0");
            assert!(!u.3, "stereo = 0");
        }
    }

    /// ⚠️ **顺序敏感**：`left_X` 在前**不合并**。
    ///
    /// `oracle_fcui.js` 的 `fc4`（`left_lid_raiser right_lid_raiser`）⟹
    /// `nfc=2 nUI=2`，两条**非 stereo**、名原样。
    ///
    /// 这条最容易写错成「按名字配对」—— 那样会得到 1 条 stereo ui。
    #[test]
    fn reversed_left_right_order_does_not_merge() {
        let uis = fcui_of(&["left_lid_raiser", "right_lid_raiser"]);
        assert_eq!(uis.len(), 2, "顺序反转不合并，got {uis:?}");
        assert_eq!(uis[0].0, "left_lid_raiser", "名原样");
        assert_eq!(uis[1].0, "right_lid_raiser");
        assert!(uis.iter().all(|u| !u.3), "两条都是单声道");
    }

    /// ⚠️ **前缀敏感**：`zzz_right` / `zzz_left` 这种**后缀式**不合并。
    ///
    /// `oracle_fcui.js` 的 `fc7` ⟹ `nfc=2 nUI=2`，两条非 stereo。
    #[test]
    fn suffix_style_right_left_does_not_merge() {
        let uis = fcui_of(&["zzz_right", "zzz_left"]);
        assert_eq!(uis.len(), 2, "后缀式不合并，got {uis:?}");
        assert_eq!(uis[0].0, "zzz_right");
        assert_eq!(uis[1].0, "zzz_left");
    }

    /// ⚠️ **不相邻不合并**：`right_A mid left_A` ⟹ 3 条。
    ///
    /// `oracle_fcui2.js` 的 `g2`。判据是「**紧接着的下一条**」，不是「在数组
    /// 里找配对」。
    #[test]
    fn non_adjacent_right_left_does_not_merge() {
        let uis = fcui_of(&["right_A", "mid", "left_A"]);
        assert_eq!(uis.len(), 3, "不相邻不合并，got {uis:?}");
        assert_eq!(uis[0].0, "right_A");
        assert_eq!(uis[2].0, "left_A");
    }

    /// ⚠️ **剩余部分必须逐字节相同（大小写敏感）**。
    ///
    /// `oracle_fcui2.js` 的 `g1`（`right_A left_B`）、`g3`（`Right_A`）、
    /// `g4`（`Left_A`）、`g7`（`right_a left_A`）全部**不合并**。
    ///
    /// ⭐ `g3`/`g4` 同时证明**前缀本身也大小写敏感**。
    #[test]
    fn remainder_must_match_byte_for_byte() {
        for names in [
            ["right_A", "left_B"], // 剩余不同
            ["Right_A", "left_A"], // 前缀大小写
            ["right_A", "Left_A"], // 前缀大小写
            ["right_a", "left_A"], // 剩余大小写
        ] {
            let uis = fcui_of(&names);
            assert_eq!(uis.len(), 2, "{names:?} 不应合并，got {uis:?}");
            assert!(uis.iter().all(|u| !u.3), "{names:?} 两条都应是单声道");
        }
    }

    /// ⭐ **`type` 不参与判据**：`eyelid right_A` + `nose left_A` **仍合并**。
    ///
    /// `oracle_fcui2.js` 的 `g5` ⟹ `nfc=2 nUI=1`、`ui[0] name="A" stereo=1`。
    ///
    /// 这条推翻了「同 type 才配对」的直觉假设 —— 用户工程的
    /// `right_/left_` 对恰好都同 type，光看语料分辨不出来。
    #[test]
    fn pairing_ignores_the_controller_type() {
        let d = tmpdir("fcuitype");
        write(&d, "myprop-ref.smd", SMD);
        let mut toml = desc_toml("myprop-ref.smd");
        toml.push_str("\n[[flex_controllers]]\nname = \"right_A\"\ntype = \"eyelid\"\n");
        toml.push_str("\n[[flex_controllers]]\nname = \"left_A\"\ntype = \"nose\"\n");
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let uis = &c.resolved_flex_controller_ui;
        assert_eq!(uis.len(), 1, "type 不同也合并（g5 实测），got {uis:?}");
        assert_eq!(uis[0].name, "A");
        assert!(uis[0].stereo);
        assert_eq!(uis[0].fc0, 1);
        assert_eq!(uis[0].fc1, Some(0));
        std::fs::remove_dir_all(&d).ok();
    }

    /// 连续两对：`right_A left_A right_B left_B` ⟹ 2 条 stereo ui。
    ///
    /// `oracle_fcui2.js` 的 `g6`。同时验证扫描指针**一次跳两条**（否则第二对
    /// 会被 `left_A` 打乱）。
    #[test]
    fn consecutive_pairs_each_merge() {
        let uis = fcui_of(&["right_A", "left_A", "right_B", "left_B"]);
        assert_eq!(uis.len(), 2, "两对应得 2 条 ui，got {uis:?}");
        assert_eq!((uis[0].0.as_str(), uis[0].1, uis[0].2), ("A", 1, Some(0)));
        assert_eq!((uis[1].0.as_str(), uis[1].1, uis[1].2), ("B", 3, Some(2)));
        assert!(uis.iter().all(|u| u.3));
    }

    /// 剩余部分**可以含下划线**：`right_a_b` + `left_a_b` ⟹ `name = "a_b"`。
    ///
    /// `oracle_fcui2.js` 的 `g9`。前缀只剥一次，不按 `_` 分词。
    #[test]
    fn remainder_may_contain_underscores() {
        let uis = fcui_of(&["right_a_b", "left_a_b"]);
        assert_eq!(uis.len(), 1, "got {uis:?}");
        assert_eq!(uis[0].0, "a_b", "只剥 right_/left_ 前缀，不按 _ 分词");
    }

    /// ⭐ **用户工程实测**：`survivors_facerules.qci:2-30` 的 30 条
    /// `flexcontroller` 声明 ⟹ **52 fc / 33 ui**，与官方产物
    /// `docs/_probe/_oracle_official_flex.mdl` 的 `numflexcontrollerui=33`
    /// 逐条同名同序。
    ///
    /// 这里只钉**配对计数**（19 对 + 14 条单声道 = 33），名字序列见
    /// `docs/_probe/cmp_flex.js` 的端到端比对。
    #[test]
    fn user_project_controller_list_yields_33_uis() {
        // 逐字抄自 `survivors_facerules.qci:2-30`（`range` 无关，略）。
        let decls: [&[&str]; 29] = [
            &["right_lid_raiser", "left_lid_raiser"],
            &["right_lid_tightener", "left_lid_tightener"],
            &["right_lid_droop", "left_lid_droop"],
            &["right_lid_closer", "left_lid_closer"],
            &["half_closed"],
            &["blink"],
            &["right_lid_squinter", "left_lid_squinter"],
            &["right_inner_raiser", "left_inner_raiser"],
            &["right_outer_raiser", "left_outer_raiser"],
            &["right_lowerer", "left_lowerer"],
            &["right_cheek_raiser", "left_cheek_raiser"],
            &["right_wrinkler", "left_wrinkler", "dilator"],
            &["right_upper_raiser", "left_upper_raiser"],
            &["right_corner_puller", "left_corner_puller"],
            &["right_corner_depressor", "left_corner_depressor"],
            &["chin_raiser"],
            &["right_part", "left_part"],
            &["right_puckerer", "left_puckerer"],
            &["right_funneler", "left_funneler"],
            &["right_stretcher", "left_stretcher"],
            &["bite", "presser", "tightener", "jaw_clencher"],
            &["jaw_drop"],
            &["right_mouth_drop", "left_mouth_drop"],
            &["right_cheek_puffer", "left_cheek_puffer"],
            &["mouth_sideways"],
            &["jaw_sideways"],
            &["lower_lip"],
            // 下面两条来自 `survivors_bodyrules.qci:3-4`
            // （`flexcontroller eyes range -30 30 eyes_updown` / `eyes_rightleft`）。
            &["eyes_updown"],
            &["eyes_rightleft"],
        ];
        let names: Vec<&str> = decls.iter().flat_map(|g| g.iter().copied()).collect();
        assert_eq!(names.len(), 52, "官方产物 numflexcontrollers = 52");
        let uis = fcui_of(&names);
        assert_eq!(uis.len(), 33, "官方产物 numflexcontrollerui = 33");
        assert_eq!(uis.iter().filter(|u| u.3).count(), 19, "19 对 stereo");
        assert_eq!(uis.iter().filter(|u| !u.3).count(), 14, "14 条单声道");
    }

    // ---- FBX shape key → flex 自动注册（`resolve_shape_key_flexes`）----
    //
    // 官方行为全部来自真 exe 实测（`docs/fbx-support.md` §1.6b）：
    // `oracle_fbx_morph_full.js` 8 用例 + `oracle_fbx_shapeaxis.js`
    // + `oracle_fbx_shapeany.js`（30 例 shape key 表）。
    //
    // ⚠️ 这些测试**直接调 `resolve_shape_key_flexes`**，不走 `compile()` ——
    // 造一个真 FBX 夹具要写二进制文件，而本模块的夹具全是 SMD。
    // 端到端覆盖在 `target/fbxcheck/` 的 7 用例上做（对照官方产物）。

    /// 造一个「1 个 bodypart / 1 个 model / 1 个 mesh」的 IR，并把
    /// `mesh_src_index` 设成给定的「控制点号」序列。
    fn ir_with_src_index(src: &[u32]) -> crate::model::CompiledModelDesc {
        let meshes = vec![crate::model::Mesh {
            material: 0,
            vertices: src
                .iter()
                .map(|_| crate::model::Vertex {
                    pos: [0.0, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    bones: vec![[0.0, 1.0]],
                })
                .collect(),
            triangles: Vec::new(),
            eyeball_tag: None,
        }];
        let mut c = desc_with_meshes(meshes);
        c.bodyparts[0].models[0].mesh_src_index = vec![src.to_vec()];
        c.bodyparts[0].models[0].shape_keys = Vec::new();
        c
    }

    /// 造一个 shape key：`(名字, [(控制点号, delta)])`。
    fn key(name: &str, offs: &[(u32, [f32; 3])]) -> crate::fbx::FbxShapeKey {
        crate::fbx::FbxShapeKey {
            name: name.to_string(),
            position_offsets: offs.iter().map(|&(_, d)| d).collect(),
            normal_offsets: vec![[0.0; 3]; offs.len()],
            vertex_index: offs.iter().map(|&(cp, _)| cp).collect(),
        }
    }

    /// ⭐ **没有 shape key 时必须是完全 no-op** —— 这是「既有产物逐字节不变」
    /// 的依据（非 FBX 工程走的就是这条路径）。
    #[test]
    fn shape_key_pass_is_noop_without_shape_keys() {
        let mut c = ir_with_src_index(&[0, 1, 2]);
        let before = c.desc.flex_descriptors.len();
        resolve_shape_key_flexes(&mut c).expect("不该报错");
        assert_eq!(c.desc.flex_descriptors.len(), before);
        assert!(c.desc.flex_controllers.is_empty());
        assert!(c.desc.flex_rules.is_empty());
        assert!(c.bodyparts[0].models[0].mesh_flexes.is_empty());
    }

    /// ⭐⭐⭐ **官方 `k4_morph.mdl` 的 desc / controller / rule 三段逐字段复刻**
    /// （真 exe 实测：`desc = ["wide","tall"]`、`ctrl[0] type="wide" name="wide"
    /// min=0 max=1`、`rule[0] flex=0 numops=1 {op=2 index=0}`）。
    ///
    /// `op = 2` 是 `STUDIO_FETCH1`（`studio.h:2795`），**不是** `STUDIO_MUL`。
    #[test]
    fn shape_keys_register_official_desc_controller_rule() {
        let mut c = ir_with_src_index(&[0, 1, 2, 3]);
        c.bodyparts[0].models[0].shape_keys = vec![
            key("wide", &[(0, [2.5, 0.0, 0.0]), (1, [2.5, 0.0, 0.0])]),
            key("tall", &[(2, [0.0, 2.5, 0.0]), (3, [0.0, -2.5, 0.0])]),
        ];
        resolve_shape_key_flexes(&mut c).expect("不该报错");

        let descs: Vec<&str> = c
            .desc
            .flex_descriptors
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            descs,
            ["wide", "tall"],
            "desc 名 = shape key 名，按文件顺序"
        );

        assert_eq!(c.desc.flex_controllers.len(), 2);
        for (i, n) in ["wide", "tall"].iter().enumerate() {
            let fc = &c.desc.flex_controllers[i];
            assert_eq!(fc.name, *n);
            assert_eq!(fc.kind, *n, "type 与 name 同形（fx4 实测）");
            assert_eq!((fc.min, fc.max), (0.0, 1.0), "官方 min=0 max=1");
        }

        assert_eq!(c.desc.flex_rules.len(), 2, "每条 flex 一条 rule（不去重）");
        for (i, r) in c.desc.flex_rules.iter().enumerate() {
            assert_eq!(r.flex, ["wide", "tall"][i]);
            assert_eq!(r.ops.len(), 1);
            assert_eq!(
                r.ops[0].op.code(),
                2,
                "op = STUDIO_FETCH1（k4_morph.mdl 实测 op=2）"
            );
            assert_eq!(r.ops[0].controller.as_deref(), Some(["wide", "tall"][i]));
        }
    }

    /// ⭐⭐⭐ **载荷逐字段复刻**：`targets = [0,1,10,11]`、`flexpair = 0`、
    /// `vertanimtype = 0`、`speed = side = 255`、`ndelta = 0`、`delta` 原样。
    ///
    /// 真 exe 实测（`k4_morph.mdl`）：48 条 vertanim 的 `speed` 集合 = `{255}`、
    /// `side` 集合 = `{255}`、`ndelta` 集合 = `{(0,0,0)}`。
    #[test]
    fn shape_key_payload_matches_official_fields() {
        let mut c = ir_with_src_index(&[0, 1]);
        c.bodyparts[0].models[0].shape_keys = vec![key("wide", &[(1, [2.5, 0.0, 0.0])])];
        resolve_shape_key_flexes(&mut c).expect("不该报错");

        let fx = &c.bodyparts[0].models[0].mesh_flexes[0];
        assert_eq!(fx.len(), 1);
        let f = &fx[0];
        assert_eq!(f.flexdesc, 0);
        assert_eq!(f.targets, [0.0, 1.0, 10.0, 11.0], "官方 targets 恒为这四档");
        assert_eq!(f.flexpair, 0);
        assert_eq!(f.vertanimtype, 0);
        assert_eq!(f.vertanims.len(), 1);
        let v = &f.vertanims[0];
        assert_eq!(v.index, 1, "控制点 1 → 焊接顶点 1");
        assert_eq!(v.speed, 255);
        assert_eq!(v.side, 255, "FBX 路径写 255，不是 .vta 非 pair 路径的 0");
        assert_eq!(v.delta, [2.5, 0.0, 0.0]);
        assert_eq!(v.ndelta, [0.0; 3], "官方 FBX 路径丢弃 nrm_off");
    }

    /// ⭐ **一个控制点展开到它的全部焊接顶点**。
    ///
    /// 真 exe 实测（`oracle_fbx_shapeaxis.js`）：`axisprobe.fbx` 的
    /// `(5,5,5)` 这个控制点在官方 VVD 里有 3 个焊接顶点（v9/v14/v23），
    /// 官方就写了 **3 条** vertanim，内容完全相同。
    #[test]
    fn one_control_point_expands_to_all_welded_vertices() {
        // 控制点 2 被焊成 3 个顶点（0/3/5），控制点 7 只有 1 个。
        let mut c = ir_with_src_index(&[2, 7, 2, 2]);
        c.bodyparts[0].models[0].shape_keys = vec![key("ax", &[(2, [1.0, 3.0, -2.0])])];
        resolve_shape_key_flexes(&mut c).expect("不该报错");

        let f = &c.bodyparts[0].models[0].mesh_flexes[0][0];
        let idx: Vec<u32> = f.vertanims.iter().map(|a| a.index).collect();
        assert_eq!(idx, [0, 2, 3], "控制点 2 的三个焊接顶点，升序");
        for a in &f.vertanims {
            assert_eq!(a.delta, [1.0, 3.0, -2.0], "同一控制点的位移完全相同");
        }
    }

    /// ⭐ **shape key 动了没进模型的顶点 ⟹ 静默跳过**（不报错、不写载荷）。
    ///
    /// 官方对 FBX 里没被任何网格引用的控制点也是静默的
    /// （`simplify.cpp:2524` 的 `if (scale > 0 && vanim_mapcount[vertex])`）。
    #[test]
    fn shape_key_touching_absent_control_point_is_skipped() {
        let mut c = ir_with_src_index(&[0, 1]);
        c.bodyparts[0].models[0].shape_keys = vec![key("ghost", &[(99, [1.0, 0.0, 0.0])])];
        resolve_shape_key_flexes(&mut c).expect("不该报错");
        // desc / controller / rule 照注册（官方也是先注册再算载荷），
        // 但载荷为空 ⟹ 该 mesh 一条 flex 都不写。
        assert_eq!(c.desc.flex_descriptors.len(), 1);
        assert!(
            c.bodyparts[0].models[0].mesh_flexes.is_empty()
                || c.bodyparts[0].models[0].mesh_flexes[0].is_empty(),
            "没有命中任何顶点时不该产出 flex 记录"
        );
    }

    /// ⭐ **与 `.vta` 路径的载荷共存**（官方 `oracle_fbx_isolate.js` 的
    /// `W2_morphfbx_vta_flex`：`desc = ["wide","tall","v1"]`）。
    ///
    /// ⚠️ 这里必须让 `.vta` 路径**真的算完并走到收尾赋值**，否则测不到
    /// 「追加 vs 覆盖」—— 第一版只调 `resolve_vta_flexes` 且 `flexes` 为空，
    /// 它直接早退，把收尾那行换成覆盖式赋值**变异测试逃逸了**。
    ///
    /// 所以夹具里放一条真的 `flex`：`.vta` 只动**顶点 0**（`tip` 的顶点），
    /// 而 shape key 动**控制点 0**（同一个顶点）。两者都落进同一个 mesh，
    /// 于是「覆盖」会把 flex 数从 2 打回 1。
    /// 造一份最小 `.vta`：`time 0` = 基准帧（`base`），`time 1` = 形状帧
    /// （`base` 但把**顶点 0** 换成 `moved0`）。
    ///
    /// `.vta` 的 `nodes`/`skeleton` 段与模型骨架对齐（`root` → `tip`），
    /// `vertexanimation` 的每行是 `index px py pz nx ny nz`（7 个字段）。
    fn vta_text(base: &[[f32; 3]], moved0: &[f32; 3]) -> String {
        let mut s = String::from(
            "version 1\n\
             nodes\n\
             \x20 0 \"root\" -1\n\
             \x20 1 \"tip\" 0\n\
             end\n\
             skeleton\n\
             \x20 time 0\n\
             \x20 0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n\
             \x20 1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\n\
             \x20 time 1\n\
             \x20 0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n\
             \x20 1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\n\
             end\n\
             vertexanimation\n",
        );
        let mut frame = |t: i32, first: Option<&[f32; 3]>| {
            s.push_str(&format!("  time {t}\n"));
            for (i, p) in base.iter().enumerate() {
                let q = match (i, first) {
                    (0, Some(m)) => m,
                    _ => p,
                };
                s.push_str(&format!(
                    "  {i} {:.6} {:.6} {:.6} 0.000000 0.000000 1.000000\n",
                    q[0], q[1], q[2]
                ));
            }
        };
        frame(0, None);
        frame(1, Some(moved0));
        s.push_str("end\n");
        s
    }

    #[test]
    fn splitting_preserves_flex_vertices_above_u16_in_lower_lods() {
        let high = mesh_with(1, 0);
        let mut lower = mesh_with(22_000, 0);
        for vertex in &mut lower.vertices {
            vertex.pos[1] += 20.0;
        }
        let lod = crate::lod::unify_lods(&[
            (high.vertices.clone(), high.triangles.clone()),
            (lower.vertices.clone(), lower.triangles.clone()),
        ]);
        let index = lod.vertices.len() - 1;
        assert!(index > u16::MAX as usize);
        let expected = lod.vertices[index].pos;
        let mut compiled = desc_with_meshes(vec![high]);
        compiled.bodyparts[0].models[0].lods = Some(crate::model::ModelLods {
            meshes: vec![lod],
            num_lods: 2,
            switch_points: vec![0.0, 25.0],
            bone_lod_usage: vec![3; compiled.desc.bones.len()],
            no_facial: vec![false; 2],
        });
        compiled.bodyparts[0].models[0].mesh_flexes = vec![vec![crate::flex::ResolvedFlex {
            flexdesc: 0,
            targets: [0.0, 1.0, 10.0, 11.0],
            flexpair: 0,
            vertanimtype: 0,
            vertanims: vec![crate::flex::ResolvedVertAnim {
                index: index as u32,
                speed: 255,
                side: 0,
                delta: [0.0, 0.0, 2.0],
                ndelta: [0.0; 3],
            }],
        }]];
        split_oversized_meshes(&mut compiled).unwrap();
        let model = &compiled.bodyparts[0].models[0];
        assert!(model.meshes.len() > 1);
        let mut positions = Vec::new();
        for (mesh, flexes) in model.mesh_flexes.iter().enumerate() {
            for flex in flexes {
                for animation in &flex.vertanims {
                    positions.push(
                        model.lods.as_ref().unwrap().meshes[mesh].vertices
                            [animation.index as usize]
                            .pos,
                    );
                }
            }
        }
        assert_eq!(positions, vec![expected]);
    }

    #[test]
    fn vta_flexes_preserve_lower_lod_vertices_and_final_mesh_indices() {
        let directory = tmpdir("vta_lod_union");
        let high = mesh_with(1, 0);
        let mut lower = high.clone();
        for vertex in &mut lower.vertices {
            vertex.pos[0] += 20.0;
        }
        let lod = crate::lod::unify_lods(&[
            (high.vertices.clone(), high.triangles.clone()),
            (lower.vertices.clone(), lower.triangles.clone()),
        ]);
        let mut compiled = desc_with_meshes(vec![high]);
        compiled.bodyparts[0].models[0].lods = Some(crate::model::ModelLods {
            meshes: vec![lod.clone()],
            num_lods: 2,
            switch_points: vec![0.0, 25.0],
            bone_lod_usage: vec![3; compiled.desc.bones.len()],
            no_facial: vec![false; 2],
        });
        let base: Vec<_> = lod.vertices.iter().map(|v| v.pos).collect();
        let lower_vertex = 3usize;
        let mut ordered = vec![base[lower_vertex]];
        ordered.extend(
            base.iter()
                .enumerate()
                .filter_map(|(i, p)| (i != lower_vertex).then_some(*p)),
        );
        let moved = [ordered[0][0], ordered[0][1], ordered[0][2] + 2.0];
        write(&directory, "shape.vta", &vta_text(&ordered, &moved));
        compiled.desc.bodyparts[0].models[0].flexes = vec![
            toml::from_str("vta = 'shape.vta'\nname = 'lower'\nframe = 1\ndecay = 0\n").unwrap(),
        ];
        resolve_vta_flexes(&mut compiled, &directory).unwrap();
        let flex = &compiled.bodyparts[0].models[0].mesh_flexes[0][0];
        assert_eq!(flex.vertanims.len(), 1);
        assert_eq!(flex.vertanims[0].index, lower_vertex as u32);
        assert_eq!(flex.vertanims[0].delta, [0.0, 0.0, 2.0]);
        let layout = crate::lod::build_lod_layout(&[lod]);
        let expected = layout.mesh_local_id(0, lower_vertex as u32).unwrap();
        assert_ne!(expected, lower_vertex as u32);
        let mdl = crate::mdl_writer::write_mdl(&compiled).unwrap().bytes;
        let i32_at =
            |offset| i32::from_le_bytes(mdl[offset..offset + 4].try_into().unwrap()) as usize;
        let body = i32_at(236);
        let model = body + i32_at(body + 12);
        let mesh = model + i32_at(model + 76);
        let flex = mesh + i32_at(mesh + 20);
        let animation = flex + i32_at(flex + 24);
        assert_eq!(
            u16::from_le_bytes(mdl[animation..animation + 2].try_into().unwrap()) as u32,
            expected
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn shape_key_payload_survives_the_vta_pass() {
        let d = tmpdir("skvta");
        write(&d, "myprop-ref.smd", SMD);
        let toml = format!(
            "{}\n[[bodyparts.models.flexes]]\nvta = \"shape.vta\"\nname = \"v1\"\nframe = 1\n",
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();

        // ⚠️ `.vta` 的顶点是靠**位置**就近匹配到模型顶点的
        // （`build_vanim_map` 的 3×3×3 网格，容差 `MATCH_DIST_SQR = 0.15`），
        // 而模型顶点在 `remap_vertices_to_reference_pose` 之后会**动**。
        // 所以先写一个占位 `.vta` 空跑一遍拿到真实位置，再据此重写它 ——
        // 手写坐标的第一版就因为这个匹配不上（`vta_only` 是 0），
        // 测试自己抓住了。
        write(&d, "shape.vta", &vta_text(&[[0.0, 0.0, 0.0]; 3], &[0.0; 3]));
        let c0 = compile(&desc, &d).expect("占位 .vta 也应能编译");
        let pos: Vec<[f32; 3]> = c0.bodyparts[0].models[0].meshes[0]
            .vertices
            .iter()
            .map(|v| v.pos)
            .collect();
        assert!(!pos.is_empty(), "夹具 mesh 应该有顶点");

        // 真 `.vta`：基准帧 = 真实位置，第 1 帧把**第一个顶点**沿 +X 推 4。
        let mut moved = pos.clone();
        moved[0][0] += 4.0;
        write(&d, "shape.vta", &vta_text(&pos, &moved[0]));

        let mut c = compile(&desc, &d).expect("夹具应能编译");
        let vta_only = c.bodyparts[0].models[0].mesh_flexes[0].len();
        assert!(
            vta_only > 0,
            "夹具的 .vta 应该真的产出载荷（否则测不到覆盖 bug）"
        );

        // 摆出**生产顺序**：清掉 `.vta` 的载荷，只留 shape key，
        // 然后 `resolve_shape_key_flexes` → `resolve_vta_flexes`。
        let n_verts = c.bodyparts[0].models[0].meshes[0].vertices.len();
        c.bodyparts[0].models[0].mesh_flexes = Vec::new();
        c.bodyparts[0].models[0].mesh_src_index = vec![(0..n_verts as u32).collect::<Vec<u32>>()];
        c.bodyparts[0].models[0].shape_keys = vec![key("wide", &[(0, [2.5, 0.0, 0.0])])];

        resolve_shape_key_flexes(&mut c).expect("不该报错");
        let after_sk = c.bodyparts[0].models[0].mesh_flexes[0].len();
        assert_eq!(after_sk, 1, "shape key 应产出 1 条");
        // ⚠️ desc 表里已经有一条 `.vta` 留下的 `v1`（第一次 `compile` 注册的），
        // 所以 shape key 的 `wide` 拿到的**不是** 0 —— 断言相对关系而不是绝对值。
        let sk_desc = c.bodyparts[0].models[0].mesh_flexes[0][0].flexdesc;
        let sk_name = c.desc.flex_descriptors[sk_desc as usize].name.clone();
        assert_eq!(sk_name, "wide");

        resolve_vta_flexes(&mut c, &d).expect("`.vta` 应能再跑一遍");
        std::fs::remove_dir_all(&d).ok();

        let fx = &c.bodyparts[0].models[0].mesh_flexes[0];
        let descs: Vec<i32> = fx.iter().map(|f| f.flexdesc).collect();
        let names: Vec<&str> = c
            .desc
            .flex_descriptors
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            fx.len(),
            vta_only + 1,
            "shape key 的载荷必须**留在** `.vta` 路径的载荷之前（覆盖式赋值会让它消失）；\
             实际 descs={descs:?} names={names:?}"
        );
        assert_eq!(
            c.desc.flex_descriptors[fx[0].flexdesc as usize].name, "wide",
            "shape key 的 flex 在前（它先跑）"
        );
        assert_eq!(
            c.desc.flex_descriptors[fx[1].flexdesc as usize].name, "v1",
            "`.vta` 的 flex 在后"
        );
    }

    // ---- `SrcOpts`：`src*` 选项的解析与 `srcshapekey*` 三条规则 ----
    //
    // 全部是纯函数，直接构造结构体断言，不需要文件系统。
    // 端到端（真 FBX 文件 + 官方产物对照）在 `target/fbxcheck/` 上做。

    /// 造一个只带诊断字段的 `SourceGeometry`（`smd` 用最小空壳）。
    fn geom_with(untextured: &[&str], merged: &[&str], stacks: &[&str]) -> SourceGeometry {
        SourceGeometry {
            smd: Smd {
                version: 1,
                nodes: Vec::new(),
                frames: Vec::new(),
                triangles: Vec::new(),
            },
            shape_keys: Vec::new(),
            untextured_meshes: untextured.iter().map(|s| s.to_string()).collect(),
            merged_meshes: merged.iter().map(|s| s.to_string()).collect(),
            anim_stacks: stacks.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `.smd` 源（三个诊断字段全空）必须**一条提示都不发** —— 否则既有
    /// SMD 工程的输出会被无端污染。
    #[test]
    fn fbx_diagnostics_are_silent_for_smd_sources() {
        let g = geom_with(&[], &[], &[]);
        assert!(fbx_diagnostics("bp[0].m[0]", &g, &[]).is_empty());
    }

    /// 单块网格、单条栈、无 shape key ⟹ 静默（一块网格是绝大多数情况，
    /// 提醒它「被合并了」只会制造噪声）。
    #[test]
    fn fbx_diagnostics_stay_quiet_on_the_ordinary_case() {
        let g = geom_with(&[], &["body"], &["walk"]);
        assert!(fbx_diagnostics("bp[0].m[0]", &g, &[]).is_empty());
    }

    /// 四条提示各自的触发条件与关键措辞。
    ///
    /// ⚠️ 断言的是**英文源文**（键本身），所以要显式钉住语言 —— 系统语言是
    /// 中文时 `t!` 会命中 `locales/app.yml` 的译文，不钉就会随机器而变。
    #[test]
    fn fbx_diagnostics_cover_the_four_hazards() {
        crate::test_locale::with_english(|| {
            // ① 多网格合并
            let g = geom_with(&[], &["body", "hat"], &[]);
            let d = fbx_diagnostics("bp[0].m[0]", &g, &[]);
            assert_eq!(d.len(), 1);
            assert!(d[0].contains("2 meshes"), "{}", d[0]);
            assert!(d[0].contains("srcpart"), "要指出出路：{}", d[0]);

            // ② 无材质
            let g = geom_with(&["box"], &["box"], &[]);
            let d = fbx_diagnostics("bp[0].m[0]", &g, &[]);
            assert_eq!(d.len(), 1);
            assert!(d[0].contains("no material"), "{}", d[0]);
            assert!(d[0].contains("debug/debugempty"), "{}", d[0]);
            assert!(d[0].contains("srcmaterial"), "{}", d[0]);

            // ③ 多动画栈
            let g = geom_with(&[], &["box"], &["walk", "run"]);
            let d = fbx_diagnostics("bp[0].m[0]", &g, &[]);
            assert_eq!(d.len(), 1);
            assert!(d[0].contains("2 animation stacks"), "{}", d[0]);
            assert!(d[0].contains("srcstack"), "{}", d[0]);

            // ④ shape key 自动注册（帧号 1 起，与 `shape_key_frame` 一致）
            let g = geom_with(&[], &["body"], &[]);
            let keys = vec![sk("wide"), sk("tall")];
            let d = fbx_diagnostics("bp[0].m[0]", &g, &keys);
            assert_eq!(d.len(), 1);
            assert!(d[0].contains("2 shape keys"), "{}", d[0]);
            assert!(d[0].contains("\"wide\"(frame 1)"), "{}", d[0]);
            assert!(d[0].contains("\"tall\"(frame 2)"), "{}", d[0]);
            assert!(d[0].contains("srcshapekey"), "{}", d[0]);
        });
    }

    /// 提示文案会跟着语言走（中文系统上不该再看到英文源文）。
    #[test]
    fn fbx_diagnostics_are_translated_for_chinese() {
        crate::test_locale::with_locale("zh-CN", || {
            let g = geom_with(&[], &["body", "hat"], &[]);
            let d = fbx_diagnostics("bp[0].m[0]", &g, &[]);
            assert!(d[0].contains("2 块网格被合并"), "{}", d[0]);
            assert!(d[0].contains("srcpart"), "命令名不翻译：{}", d[0]);
        });
    }

    /// 四条可以同时触发，顺序固定（多网格 → 无材质 → 多栈 → shape key）。
    #[test]
    fn fbx_diagnostics_can_fire_all_four_at_once() {
        crate::test_locale::with_english(|| {
            let g = geom_with(&["body"], &["body", "hat"], &["walk", "run"]);
            let keys = vec![sk("wide")];
            let d = fbx_diagnostics("bp[0].m[0]", &g, &keys);
            assert_eq!(d.len(), 4, "{d:#?}");
            assert!(d[0].contains("meshes"), "{}", d[0]);
            assert!(d[1].contains("no material"), "{}", d[1]);
            assert!(d[2].contains("animation stacks"), "{}", d[2]);
            assert!(d[3].contains("shape keys"), "{}", d[3]);
        });
    }

    fn sk(name: &str) -> crate::fbx::FbxShapeKey {
        crate::fbx::FbxShapeKey {
            name: name.to_string(),
            position_offsets: vec![[1.0, 0.0, 0.0]],
            normal_offsets: vec![[0.0; 3]],
            vertex_index: vec![0],
        }
    }

    fn names_of(keys: &[crate::fbx::FbxShapeKey]) -> Vec<&str> {
        keys.iter().map(|k| k.name.as_str()).collect()
    }

    /// 默认 `SrcOpts` = 官方行为：不选网格、不换材质、不缩放、不转轴向、
    /// 第一条动画栈、30 fps、形变目标原样。
    #[test]
    fn default_src_opts_are_the_official_behaviour() {
        let o = SrcOpts::default();
        assert!(o.fbx.parts.is_empty(), "默认合并全部网格（官方行为）");
        assert!(o.fbx.material.is_none(), "默认用 debug/debugempty");
        assert!((o.fbx.scale - 1.0).abs() < f32::EPSILON);
        assert!(o.fbx.axis.is_none(), "默认不干预轴向（官方只是原样搬运）");
        assert!(o.stack.is_none(), "默认第一条动画栈");
        assert_eq!(o.fps_or_default(), crate::fbx::DEFAULT_FPS);
        assert!(!o.shape_key_ignore);
        let keys = vec![sk("a"), sk("b")];
        assert_eq!(names_of(&o.order_shape_keys(&keys).unwrap()), ["a", "b"]);
    }

    /// `srcshapekey` = **过滤 + 定序**，名单里源里没有的**必须报错**
    /// （静默丢掉是官方最坑的行为之一，所以这里是有意比官方严格）。
    #[test]
    fn srcshapekey_filters_and_reorders() {
        let mut o = SrcOpts {
            shape_keys: vec!["b".into(), "a".into()],
            ..Default::default()
        };
        let keys = vec![sk("a"), sk("b"), sk("c")];
        assert_eq!(
            names_of(&o.order_shape_keys(&keys).unwrap()),
            ["b", "a"],
            "按名单顺序，名单外的（c）丢掉"
        );

        o.shape_keys = vec!["b".into(), "nope".into()];
        let err = o.order_shape_keys(&keys).unwrap_err();
        // 文案走译文表 ⟹ 断言英文源文前先钉住语言。
        let msg = crate::test_locale::with_english(|| err.to_string());
        assert!(msg.contains("nope"), "{msg}");
        assert!(
            msg.contains("available ones"),
            "错误信息要列出源里有什么：{msg}"
        );
        // 结构化断言 —— 这才是类型化的意义：调用方能直接 `match` 出
        // 「哪个名字缺了」，不必去正则匹配中文字符串。
        let SrcError::ShapeKeyMissing { missing, have } = &err else {
            panic!("应是 ShapeKeyMissing，实际：{err:?}");
        };
        assert_eq!(missing, &["nope"]);
        assert_eq!(have, &["a", "b", "c"]);
    }

    /// `srcshapekeyorder` = **只定序**：名单里的在前，其余按源顺序跟后。
    #[test]
    fn srcshapekeyorder_only_reorders() {
        let mut o = SrcOpts {
            shape_key_order: vec!["c".into(), "a".into()],
            ..Default::default()
        };
        let keys = vec![sk("a"), sk("b"), sk("c")];
        assert_eq!(
            names_of(&o.order_shape_keys(&keys).unwrap()),
            ["c", "a", "b"],
            "名单序在前，剩下的按源顺序"
        );

        // 名单里有源里没有的名字 ⟹ **不报错**（只定序，语义上允许）。
        o.shape_key_order = vec!["zzz".into(), "b".into()];
        assert_eq!(
            names_of(&o.order_shape_keys(&keys).unwrap()),
            ["b", "a", "c"]
        );
    }

    /// `srcshapekeyignore` = 一个都不要（与另外两个互斥由 QC/`validate` 保证，
    /// 这里只验证它自己的行为）。
    #[test]
    fn srcshapekeyignore_drops_everything() {
        let o = SrcOpts {
            shape_key_ignore: true,
            shape_keys: vec!["a".into()],
            ..Default::default()
        };
        let keys = vec![sk("a"), sk("b")];
        assert!(o.order_shape_keys(&keys).unwrap().is_empty());
    }

    /// 名字匹配**大小写不敏感**（与 `node_selected` 的 `eq_ignore_ascii_case`
    /// 同口径）—— FBX 里的节点名大小写不总是稳定的。
    #[test]
    fn srcshapekey_matches_case_insensitively() {
        let o = SrcOpts {
            shape_keys: vec!["WIDE".into()],
            ..Default::default()
        };
        let keys = vec![sk("wide")];
        assert_eq!(names_of(&o.order_shape_keys(&keys).unwrap()), ["wide"]);
    }

    /// `with_anim` 只覆盖**正数** fps；`None` 与 `0.0` 都表示「用默认」。
    #[test]
    fn with_anim_overrides_only_positive_fps() {
        let base = SrcOpts::default();
        assert_eq!(base.with_anim(None, None).fps_or_default(), 30.0);
        assert_eq!(base.with_anim(None, Some(0.0)).fps_or_default(), 30.0);
        assert_eq!(base.with_anim(None, Some(60.0)).fps_or_default(), 60.0);
        assert_eq!(
            base.with_anim(Some("walk"), None).stack.as_deref(),
            Some("walk")
        );
        // 不传 stack 时保留原有的（`$sequence` 叠加在 `$body` 之上）。
        let with = base.with_anim(Some("walk"), Some(60.0));
        assert_eq!(with.with_anim(None, None).stack.as_deref(), Some("walk"));
    }

    /// `of_model` 把 `src*` 的 IR 字段搬进 `FbxOpts`，且 `src_axis` 的值域
    /// 错误**在这里**就报（不依赖「调用方先 `validate` 过」）。
    #[test]
    fn of_model_maps_ir_fields_and_validates_axis() {
        let mut m = crate::model::BodyModel {
            input_scale: None,
            smd: "x.fbx".into(),
            name: None,
            flip_triangles: true,
            lods: Vec::new(),
            eyeballs: Vec::new(),
            flexes: Vec::new(),
            src_parts: vec!["body".into()],
            src_material: Some("face".into()),
            src_scale: Some(0.01),
            src_axis: Some("z-up".into()),
            src_shape_keys: vec!["wide".into()],
            src_shape_key_order: Vec::new(),
            src_shape_key_ignore: false,
        };
        let o = SrcOpts::of_model(&m).unwrap();
        assert_eq!(o.fbx.parts, ["body"]);
        assert_eq!(o.fbx.material.as_deref(), Some("face"));
        assert!((o.fbx.scale - 0.01).abs() < 1e-9);
        assert_eq!(o.fbx.axis, Some(crate::fbx::ForcedAxis::Z));
        assert_eq!(o.shape_keys, ["wide"]);

        m.src_axis = Some("sideways".into());
        let err = SrcOpts::of_model(&m).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sideways"), "{msg}");
        assert!(msg.contains("src_axis"), "{msg}");
        let SrcError::BadAxis { value } = &err else {
            panic!("应是 BadAxis，实际：{err:?}");
        };
        assert_eq!(value, "sideways");
    }

    // ---- jigglebone（`$jigglebone` → `mstudiojigglebone_t`）----
    //
    // 规则全部来自受控实验（`docs/_probe/smdl/jig{1,2,4,6,7,8}.qc` →
    // `artifacts/jig*.mdl`），验收脚本 `docs/_probe/cmp_jiggle.js`。
    // **调研报告 §6.2 的 `flags` 结论有反例**（见 `allow_length_flex`）。

    /// `flags` 的置位规则：块出现即置 `LENGTH(0x20)`，
    /// `allow_length_flex` 把它**清掉**。
    ///
    /// 受控实验对照：`jig4`（`is_flexible` + `allow_length_flex`）= `0x01`；
    /// `jig6` 的 `is_flexible`（无该键）= `0x21`、`is_rigid` = `0x22`；
    /// `jig2`（`is_rigid` + `angle_constraint` + `has_base_spring`）= `0x72`。
    #[test]
    fn jiggle_flags_follow_controlled_experiments() {
        let d = tmpdir("jigflags");
        write(&d, "myprop-ref.smd", SMD);
        let base = desc_toml("myprop-ref.smd");

        // jig6 的 is_flexible：0x21
        let t = format!(
            "{base}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\nyaw_stiffness = 100.0\n"
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        assert_eq!(c.resolved_jiggle_bones[0].flags, 0x21, "is_flexible → 0x21");

        // jig4：allow_length_flex 清掉 0x20
        let t = format!(
            "{base}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\nyaw_stiffness = 100.0\nallow_length_flex = true\n"
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        assert_eq!(
            c.resolved_jiggle_bones[0].flags, 0x01,
            "allow_length_flex 应清掉 LENGTH（受控实验 jig4 = 0x01）"
        );

        // jig6 的 is_rigid：0x22
        let t = format!(
            "{base}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_rigid]\ntip_mass = 100.0\n"
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        assert_eq!(c.resolved_jiggle_bones[0].flags, 0x22, "is_rigid → 0x22");

        // jig2：is_rigid + angle_constraint + base_spring → 0x72
        let t = format!(
            "{base}\n[[jiggle_bones]]\nbone = \"tip\"\n\
             [jiggle_bones.is_rigid]\nangle_constraint = 60.0\n\
             [jiggle_bones.has_base_spring]\nbase_stiffness = 800.0\n"
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        assert_eq!(
            c.resolved_jiggle_bones[0].flags, 0x72,
            "RIGID|ANGLE|LENGTH|BASE_SPRING"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 角度输入是**度**、写盘转**弧度**；缺省值按报告 §6.3（这节无错）。
    ///
    /// 判据来自 `jig1`：`angle_constraint 60` → `π/3`、
    /// `yaw_constraint -30 40` → `−0.5235988/0.6981317`、
    /// `pitch_constraint -20 50` → `−0.3490659/0.8726646`。
    #[test]
    fn jiggle_angles_are_degrees_in_radians_out() {
        let d = tmpdir("jigdeg");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\n\
             angle_constraint = 60.0\nyaw_constraint = [-30.0, 40.0]\n\
             pitch_constraint = [-20.0, 50.0]\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        let near = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(near(j.angle_limit, std::f32::consts::PI / 3.0), "60° → π/3");
        assert!(near(j.min_yaw, -30.0f32.to_radians()), "-30°");
        assert!(near(j.max_yaw, 40.0f32.to_radians()), "40°");
        assert!(near(j.min_pitch, -20.0f32.to_radians()), "-20°");
        assert!(near(j.max_pitch, 50.0f32.to_radians()), "50°");
        assert_eq!(j.flags, 0x3d, "FLEXIBLE|YAW|PITCH|ANGLE|LENGTH");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 缺省值：`length` 10、`*Stiffness` 100、`base*` 三轴 ±100、其余 0。
    #[test]
    fn jiggle_defaults_match_corpus() {
        let d = tmpdir("jigdef");
        write(&d, "myprop-ref.smd", SMD);
        // 只给块、不给任何字段。
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.length, 10.0, "length 缺省 10");
        assert_eq!(j.yaw_stiffness, 100.0);
        assert_eq!(j.pitch_stiffness, 100.0);
        assert_eq!(j.along_stiffness, 100.0);
        assert_eq!(
            j.base_stiffness, 100.0,
            "base_stiffness 未写 base_spring 时也是 100"
        );
        assert_eq!(j.base_min_left, -100.0);
        assert_eq!(j.base_max_left, 100.0);
        assert_eq!(j.base_min_up, -100.0);
        assert_eq!(j.base_max_up, 100.0);
        assert_eq!(j.base_min_forward, -100.0);
        assert_eq!(j.base_max_forward, 100.0);
        assert_eq!(j.tip_mass, 0.0);
        assert_eq!(j.yaw_damping, 0.0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// `procindex` 相对**骨骼记录自身**，且记录按**书写顺序**排列。
    ///
    /// 判据来自 `jig8`：QC 写 knee(2)→ankle(3)→hip(1)，产物里
    /// `@1528 knee` / `@1648 ankle` / **`@1768 hip`** ——
    /// 调研报告 §6.4 的「按骨骼下标升序」是**错的**。
    ///
    /// 这里验证「解析保序」这半步（写字节的顺序由 `mdl_writer` 保证）。
    #[test]
    fn jiggle_records_keep_written_order() {
        let d = tmpdir("jigord");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\nyaw_stiffness = 111.0\n\
             [[jiggle_bones]]\nbone = \"root\"\n[jiggle_bones.is_flexible]\nyaw_stiffness = 222.0\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones;
        assert_eq!(j.len(), 2);
        assert_eq!(j[0].bone, 1, "第一条仍是 tip(1)（书写顺序，不是下标序）");
        assert_eq!(j[0].yaw_stiffness, 111.0);
        assert_eq!(j[1].bone, 0, "第二条是 root(0)");
        assert_eq!(j[1].yaw_stiffness, 222.0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：stiffness/damping 钳到 `[0, 1000]`（实测 `jig35`/`jig36`）。
    ///
    /// 官方 `FUN_004542d0` 把 `is_flexible` 的 6 个 `*_stiffness`/`*_damping`
    /// 与 `has_base_spring` 的 `stiffness`/`damping` 钳位；实测
    /// `yaw_stiffness 5000 → 1000`、`yaw_damping -5 → 0`、
    /// `pitch_stiffness 1000.5 → 1000`、`along_stiffness -100 → 0`、
    /// base 的 `stiffness 5000 → 1000`、`damping -3 → 0`。
    ///
    /// ⚠️ `*_friction`/`*_bounce` **不钳位**（见下一个测试）。
    #[test]
    fn jiggle_stiffness_is_clamped_to_0_1000() {
        let d = tmpdir("jigclamp");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\n\
             yaw_stiffness = 5000.0\nyaw_damping = -5.0\n\
             pitch_stiffness = 1000.5\nalong_stiffness = -100.0\n\
             along_damping = 999.5\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.yaw_stiffness, 1000.0, "5000 → 1000");
        assert_eq!(j.yaw_damping, 0.0, "-5 → 0");
        assert_eq!(j.pitch_stiffness, 1000.0, "1000.5 → 1000");
        assert_eq!(j.along_stiffness, 0.0, "-100 → 0");
        assert_eq!(j.along_damping, 999.5, "界内值不动");

        // has_base_spring 的 stiffness/damping 同样钳位（jig36）。
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.has_base_spring]\n\
             base_stiffness = 5000.0\nbase_damping = -3.0\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.base_stiffness, 1000.0, "base stiffness 5000 → 1000");
        assert_eq!(j.base_damping, 0.0, "base damping -3 → 0");
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：`*_friction`/`*_bounce` **不钳位**（实测 `jig41`/`jig42`）。
    ///
    /// `yaw_friction 5000`、`yaw_bounce 6000`、`pitch_friction -7` 全部原值透传。
    #[test]
    fn jiggle_friction_and_bounce_are_not_clamped() {
        let d = tmpdir("jignoclamp");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\n\
             yaw_friction = 5000.0\nyaw_bounce = 6000.0\npitch_friction = -7.0\n\
             pitch_bounce = 8.0\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.yaw_friction, 5000.0, "不钳位（jig42）");
        assert_eq!(j.yaw_bounce, 6000.0, "不钳位（jig42）");
        assert_eq!(j.pitch_friction, -7.0, "负值也不钳位（jig42）");
        assert_eq!(j.pitch_bounce, 8.0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：`left/up/forward_constraint` **不是角度**、不转弧度。
    ///
    /// 实测 `jig38`：QC 的 `left_constraint -0.5 0.5` → 盘上
    /// `baseMinLeft = -0.5`（原值）。修复前 QC 侧对这些键也做了 `to_radians`。
    #[test]
    fn jiggle_base_spring_constraints_are_not_angles() {
        let d = tmpdir("jigbaseang");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.has_base_spring]\n\
             base_left = [-0.5, 0.5]\nbase_up = [-0.75, 2.0]\n\
             base_forward = [-0.25, 0.25]\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.base_min_left, -0.5, "原值，不乘 π/180（jig38）");
        assert_eq!(j.base_max_left, 0.5);
        assert_eq!(j.base_min_up, -0.75);
        assert_eq!(j.base_max_up, 2.0);
        assert_eq!(j.base_min_forward, -0.25);
        assert_eq!(j.base_max_forward, 0.25);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：`is_rigid` 现在也置 `YAW`/`PITCH` 约束位（实测 `jig43` = `0x2e`）。
    ///
    /// 修复前 `JiggleRigid` 只有 3 个字段，这 6 个键全部丢失，`flags` 停在 `0x22`。
    #[test]
    fn jiggle_is_rigid_sets_yaw_pitch_flags() {
        let d = tmpdir("jigrigid");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_rigid]\n\
             yaw_constraint = [-30.0, 40.0]\nyaw_friction = 7.0\n\
             pitch_constraint = [-20.0, 50.0]\npitch_bounce = 8.0\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(
            j.flags, 0x2e,
            "RIGID|YAW|PITCH|LENGTH（无 ANGLE，因为没写）"
        );
        let near = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(
            near(j.min_yaw, -30.0f32.to_radians()),
            "-30° → 弧度（jig43）"
        );
        assert!(near(j.max_yaw, 40.0f32.to_radians()));
        assert!(near(j.min_pitch, -20.0f32.to_radians()));
        assert!(near(j.max_pitch, 50.0f32.to_radians()));
        assert_eq!(j.yaw_friction, 7.0);
        assert_eq!(j.pitch_bounce, 8.0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：跨块共享键按**书写顺序**后写覆盖。
    ///
    /// 官方是扁平记录，`length` 被两个块写时**后写的赢**（实测 `jig39` →
    /// `length=20`、`jig40` → `length=5`、`jig50` → `length=20 tip_mass=9`）。
    ///
    /// TOML 侧把三个块展开成固定顺序（`is_flexible` → `is_rigid` →
    /// `has_base_spring`），所以这里 `is_rigid` 的 `length` 覆盖
    /// `is_flexible` 的。⚠️ **QC 路径不展开**，它保真 token 顺序
    /// （见 `qc::parse::tests::jiggle_writes_keep_token_order`），
    /// 两条路径共用 `resolve_jiggle_bones` 里**同一个**循环。
    ///
    /// ⚠️ TOML 的 `has_base_spring` **只有 9 个 `base_*` 字段**（官方裸键
    /// `length`/`tip_mass`/… 在 TOML 侧不存在），所以跨块覆盖只能用
    /// `is_flexible` ↔ `is_rigid` 这一对来测；`has_base_spring` 里的共享键
    /// 是 QC 独有的写法，由 `writes` 承载（见往返测试）。
    #[test]
    fn jiggle_shared_keys_are_last_write_wins() {
        let d = tmpdir("jiglww");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.is_flexible]\nlength = 5.0\n\
             [jiggle_bones.is_rigid]\nlength = 20.0\n",
            desc_toml("myprop-ref.smd")
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).unwrap();
        assert_eq!(
            c.resolved_jiggle_bones[0].length, 20.0,
            "is_rigid 后写 ⟹ 覆盖 is_flexible 的 5（jig39/jig40 同机制）"
        );
        assert_eq!(
            c.resolved_jiggle_bones[0].flags, 0x23,
            "FLEXIBLE|RIGID|LENGTH（两块都在）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：QC → TOML → 编译 必须**逐字保真** `$jigglebone`。
    ///
    /// `qc2toml` 会把 `writes` 顺序日志序列化进 TOML；重读后
    /// `effective_writes()` 必须原样返回它（而不是退化成「展开三个块」），
    /// 否则 `has_base_spring` 里的共享键（`tip_mass`/`length`/`angle_constraint`）
    /// 就会在往返中丢掉 —— 官方 `jig41` 正是这种写法。
    #[test]
    fn jiggle_qc_toml_roundtrip_preserves_writes() {
        let d = tmpdir("jigrt");
        write(&d, "myprop-ref.smd", SMD);
        // `has_base_spring` 里写共享键 `tip_mass`/`length`（官方 jig41 形态）——
        // TOML 的 `JiggleBaseSpring` 没有这两个字段，只能靠 `writes` 承载。
        let qc = "\
$modelname \"t.mdl\"
$body body \"myprop-ref.smd\"
$jigglebone \"tip\" {
\thas_base_spring {
\t\ttip_mass 3
\t\tlength 20
\t\tstiffness 800
\t}
}
$sequence \"idle\" \"myprop-ref.smd\" fps 30
";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let toml = desc.to_toml().expect("应能序列化成 TOML");
        let back = ModelDesc::from_toml(&toml).expect("应能读回 TOML");
        assert_eq!(
            desc.jiggle_bones[0].writes, back.jiggle_bones[0].writes,
            "`writes` 必须往返保真"
        );
        let c = compile(&back, &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.flags, 0x40, "只有 HAS_BASE_SPRING");
        assert_eq!(j.tip_mass, 3.0, "共享键 tip_mass 穿过往返（jig41）");
        assert_eq!(j.length, 20.0, "共享键 length 穿过往返（jig41）");
        assert_eq!(j.base_stiffness, 800.0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// `effective_writes` 把 TOML 的 `base_*` 字段名映射回官方裸键。
    ///
    /// 这层映射是 TOML 与 QC 收敛到**同一条应用路径**的关键：QC 记的是
    /// 官方裸键（`stiffness`/`left_constraint`），TOML 用的是 `base_*`，
    /// 两者必须在 `resolve_jiggle_bones` 里被同一个 `match` 认出来。
    #[test]
    fn jiggle_toml_base_fields_map_to_official_keys() {
        let d = tmpdir("jigmap");
        write(&d, "myprop-ref.smd", SMD);
        let t = format!(
            "{}\n[[jiggle_bones]]\nbone = \"tip\"\n[jiggle_bones.has_base_spring]\n\
             base_mass = 5.0\nbase_stiffness = 800.0\nbase_damping = 10.0\n\
             base_left = [-0.5, 0.5]\nbase_left_friction = 10.0\n\
             base_up = [-0.75, 2.0]\nbase_up_friction = 11.0\n\
             base_forward = [-0.25, 0.25]\nbase_forward_friction = 12.0\n",
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&t).unwrap();
        let writes = desc.jiggle_bones[0].effective_writes();
        let keys: Vec<&str> = writes.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "has_base_spring",
                "base_mass",
                "stiffness",
                "damping",
                "left_constraint",
                "left_friction",
                "up_constraint",
                "up_friction",
                "forward_constraint",
                "forward_friction"
            ],
            "`base_*` 必须映射成官方裸键"
        );
        let c = compile(&desc, &d).unwrap();
        let j = &c.resolved_jiggle_bones[0];
        assert_eq!(j.flags, 0x40, "只有 HAS_BASE_SPRING");
        assert_eq!(j.base_stiffness, 800.0);
        assert_eq!(j.base_min_left, -0.5);
        assert_eq!(j.base_forward_friction, 12.0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：`$jigglebone` 指向**不存在**的骨骼 ⟹ **丢弃该条**，不是错误。
    ///
    /// 官方 `TagProceduralBones`（`simplify.cpp:3922-3929`）对每一类程序化
    /// 骨骼都是 `bone == -1` ⟹ `printf("… \"%s\" unused\n")` + `continue;
    /// // optimized out, don't complain`。jigglebone 是 L4D2 新增的
    /// （darkm 无此分支），但 L4D2 exe 里有同形串
    /// `jigglebone "%s" unused`（@0x5763e4）。
    ///
    /// 实测 `docs/_probe/oracle_jiggle_missing_bone.js`：官方对
    /// `$jigglebone "nosuchbone"` 编过，stdout 只打一句 `unused`，产物里
    /// 既无该 proctype 条目、也不影响同一 QC 里其它 jigglebone。
    ///
    /// 修前 mdlc 报 `jiggle_bones[1]: 骨骼 "nosuchbone" 找不到` 并中止编译
    /// —— 用户的 `survivor_teenangst.qc` 正是这样被卡住的
    /// （`jigglebones.qci` 有 36 条，其中 `hb_13_1_L`/`hb_13_1_R` 在
    /// `definebones.qci` 与所有 SMD 里都不存在）。
    #[test]
    fn jiggle_missing_bone_is_dropped_not_an_error() {
        let d = tmpdir("jigmiss");
        write(&d, "myprop-ref.smd", SMD);
        let base = desc_toml("myprop-ref.smd");

        // ① 全都不存在 ⟹ 编过，且一条记录都不产出。
        let t = format!("{base}\n[[jiggle_bones]]\nbone = \"nosuchbone\"\n");
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d)
            .expect("官方对找不到骨骼的 jigglebone 是 `unused` + 丢弃，不应报错");
        assert!(
            c.resolved_jiggle_bones.is_empty(),
            "找不到骨骼的那条应被丢弃，实际：{:?}",
            c.resolved_jiggle_bones.len()
        );

        // ② 混着一条存在的 ⟹ 存在的照常产出，缺的只丢自己。
        let t = format!(
            "{base}\n[[jiggle_bones]]\nbone = \"tip\"\n\
             [jiggle_bones.is_rigid]\ntip_mass = 400.0\n\n\
             [[jiggle_bones]]\nbone = \"nosuchbone\"\n"
        );
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).expect("应能编译");
        assert_eq!(c.resolved_jiggle_bones.len(), 1, "只应留下 tip 那条");
        assert_eq!(c.resolved_jiggle_bones[0].bone, 1, "tip 是骨骼下标 1");
        assert_eq!(c.resolved_jiggle_bones[0].flags, 0x22, "IS_RIGID|LENGTH");
        assert_eq!(c.resolved_jiggle_bones[0].tip_mass, 400.0);

        // ③ 名称匹配是**精确**的（官方 `findGlobalBone` 走 `stricmp` 全名，
        //    **不**做 XSI 后缀匹配）⟹ `"ip"` 不该命中 `tip`。
        let t = format!("{base}\n[[jiggle_bones]]\nbone = \"ip\"\n");
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d).expect("应能编译");
        assert!(
            c.resolved_jiggle_bones.is_empty(),
            "后缀 `ip` 不该匹配 `tip`（oracle 的 leaf_prefixed 变体）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// **回归**：`[[quat_interp_bones]]` 的**目标骨骼**找不到 ⟹ 丢弃该条；
    /// 但 **`control` 骨骼**找不到仍是**硬错误**。
    ///
    /// 官方 `simplify.cpp:3948-3955` 对目标骨骼是
    /// `printf("quatinterpbone \"%s\" unused\n")` + `continue`；
    /// `:3957-3959` 对 control 是
    /// `MdlError("Missing control bone \"%s\" for procedural bone \"%s\"\n")`。
    /// 两条串都在 L4D2 exe 里（@0x5764a4 / @0x5764c0）。
    #[test]
    fn quat_interp_missing_bone_dropped_but_missing_control_is_error() {
        let d = tmpdir("qimiss");
        write(&d, "myprop-ref.smd", SMD);
        let base = desc_toml("myprop-ref.smd");

        // ① 目标骨骼不存在 ⟹ 丢弃，不报错。
        let t =
            format!("{base}\n[[quat_interp_bones]]\nbone = \"nosuchbone\"\ncontrol = \"root\"\n");
        let c = compile(&ModelDesc::from_toml(&t).unwrap(), &d)
            .expect("官方对找不到目标骨骼的 quatinterp 是 `unused` + 丢弃");
        assert!(c.resolved_quat_interp_bones.is_empty(), "该条应被丢弃");

        // ② control 不存在 ⟹ **硬错误**（官方 `Missing control bone`）。
        let t =
            format!("{base}\n[[quat_interp_bones]]\nbone = \"tip\"\ncontrol = \"nosuchbone\"\n");
        // ⚠️ 文案在 `compile()` 里就渲染好了，所以语言必须在那一刻生效。
        let errs = crate::test_locale::with_english(|| {
            compile(&ModelDesc::from_toml(&t).unwrap(), &d)
                .expect_err("control 找不到官方是 MdlError，必须报错")
        });
        assert!(
            errs.iter()
                .any(|x| x.message().contains("cannot find the control bone")),
            "错误信息应点名 control 骨骼：{errs:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- `$includemodel` ----
    //
    // 官方**只写名字**、从不读被包含的 `.mdl`（`g_numincludemodels` 在
    // 整个 studiomdl 里只出现 2 次），所以这里只需确认描述层能收下路径，
    // 且**不做存在性检查**、**不补 `models/` 前缀**。

    /// `$includemodel` 只存字符串：指向不存在的文件也**不该**报错。
    ///
    /// 官方行为：`Cmd_IncludeModel`（`studiomdl.cpp:5961-5967`）无去重、
    /// 无存在性检查，写不存在的文件也照样进表。
    #[test]
    fn include_models_accept_nonexistent_files() {
        let d = tmpdir("incmodel");
        write(&d, "myprop-ref.smd", SMD);
        // ⚠️ `include_models` 是**顶层**键，必须写在 `[model]`/`[[bones]]`
        // 等表**之前** —— 追加在末尾会落进最后一张表里（TOML 语义）。
        let t = format!(
            "include_models = [\"models/infected/anim_boomer.mdl\", \"models/nope/missing.mdl\"]\n{}",
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&t).expect("应能解析");
        let c = compile(&desc, &d).expect("指向不存在的 .mdl 不应报错");
        assert_eq!(
            c.desc.include_models,
            vec![
                "models/infected/anim_boomer.mdl".to_string(),
                "models/nope/missing.mdl".to_string()
            ],
            "原样保留、不补前缀、不校验存在性"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 自动生成 hitbox（`SetupHitBoxes`，`simplify.cpp:6884-6973`）----
    /// 没写显式 hitbox 时，`compile()` 应自动生成一个 `default` set，
    /// 并置 `STUDIOHDR_FLAGS_AUTOGENERATED_HITBOX`（**0x1**）。
    ///
    /// 实测语料：**3333/3333** 个模型都有 hitbox set，其中 3104 个
    /// 自动生成（带 `0x1`）、63 个显式。
    #[test]
    fn autogenerates_hitbox_when_none_given() {
        let d = tmpdir("autohb");
        write(&d, "myprop-ref.smd", SMD);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        assert!(desc.hitboxes.boxes.is_empty(), "前提：描述里没有 hitbox");
        let c = compile(&desc, &d).expect("应能编译");

        assert!(c.desc.hitboxes.autogenerated, "应标记为自动生成");
        assert_eq!(c.desc.hitboxes.set_name.as_deref(), Some("default"));
        assert_eq!(
            c.desc.model.extra_flags.unwrap_or(0) & crate::mdl_writer::FLAG_AUTOGENERATED_HITBOX,
            crate::mdl_writer::FLAG_AUTOGENERATED_HITBOX,
            "应置 0x1 标志"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// **核心判据**：自动 hitbox 的 bbox 必须与官方 `dump_hboxes` 的真值一致。
    ///
    /// 用受控实验 `docs/_probe/smdl/iph1.qc` 的真值（`studiomdl -h` 输出）：
    ///
    /// ```text
    /// $hbox 0 "b0" -8.00 -8.00 0.00  10.00 8.00 20.00
    /// $hbox 0 "b1" -18.00 -8.00 0.00  20.00 8.00 12.00
    /// $hbox 0 "b2" -38.00 -8.00 0.00  30.00 8.00 20.00
    /// $hbox 0 "b3" -64.00 -6.00 0.00  0.00 6.00 30.00
    /// ```
    ///
    /// 注意 `bbmin` 里出现 `0.00`（甚至 `b0` 的 `bbmax[2] == 20` 而
    /// `bmin[2] == 0`）—— 那是 `g_bUseBoneInBBox == true` 让 bbox 从
    /// **全 0** 起步的直接后果。
    #[test]
    fn autohitbox_matches_official_dump_hboxes() {
        let d = tmpdir("autohb-truth");
        // 与 `iph.smd` 等价的 4 骨骼链 + 混合权重。
        let smd = r#"version 1
nodes
0 "b0" -1
1 "b1" 0
2 "b2" 1
3 "b3" 2
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 10.000000 0.000000 0.000000 0.000000 0.000000 0.000000
2 20.000000 0.000000 0.000000 0.000000 0.000000 0.000000
3 30.000000 0.000000 0.000000 0.000000 0.000000 0.000000
end
triangles
myprop
0 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 3 0 0.333333 1 0.333333 2 0.333333
0 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 3 0 0.333333 1 0.333333 2 0.333333
0 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 3 0 0.333333 1 0.333333 2 0.333333
0 -6.000000 -6.000000 12.000000 0.000000 0.000000 1.000000 0.100000 0.100000 3 0 0.100000 1 0.200000 2 0.700000
0 6.000000 -6.000000 12.000000 0.000000 0.000000 1.000000 0.900000 0.100000 3 1 0.500000 2 0.300000 3 0.200000
0 0.000000 6.000000 20.000000 0.000000 0.000000 1.000000 0.500000 0.900000 3 2 0.600000 3 0.300000 0 0.100000
0 -4.000000 4.000000 30.000000 0.000000 0.000000 1.000000 0.200000 0.500000 1 3 1.000000
0 4.000000 4.000000 30.000000 0.000000 0.000000 1.000000 0.800000 0.500000 1 3 1.000000
0 0.000000 6.000000 30.000000 0.000000 0.000000 1.000000 0.500000 0.900000 1 3 1.000000
end
"#;
        write(&d, "iph.smd", smd);
        let toml = r#"
[model]
name = "models/test/iph.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "b0"

[[bones]]
name = "b1"
parent = "b0"

[[bones]]
name = "b2"
parent = "b1"

[[bones]]
name = "b3"
parent = "b2"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "iph.smd"
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");

        let got: Vec<(&str, [f32; 3], [f32; 3])> = c
            .desc
            .hitboxes
            .boxes
            .iter()
            .map(|h| (h.bone.as_str(), h.bbmin, h.bbmax))
            .collect();
        let want: [(&str, [f32; 3], [f32; 3]); 4] = [
            ("b0", [-8.0, -8.0, 0.0], [10.0, 8.0, 20.0]),
            ("b1", [-18.0, -8.0, 0.0], [20.0, 8.0, 12.0]),
            ("b2", [-38.0, -8.0, 0.0], [30.0, 8.0, 20.0]),
            ("b3", [-64.0, -6.0, 0.0], [0.0, 6.0, 30.0]),
        ];
        assert_eq!(got.len(), want.len(), "应有 4 个 hitbox，实际 {got:?}");
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert_eq!(g.0, w.0, "box[{i}] 骨骼名");
            for a in 0..3 {
                assert!(
                    (g.1[a] - w.1[a]).abs() <= 1e-2,
                    "box[{i}] bbmin[{a}]：得 {}，官方真值 {}",
                    g.1[a],
                    w.1[a]
                );
                assert!(
                    (g.2[a] - w.2[a]).abs() <= 1e-2,
                    "box[{i}] bbmax[{a}]：得 {}，官方真值 {}",
                    g.2[a],
                    w.2[a]
                );
            }
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// 自动 hitbox 的 bbox 从**全 0** 起步（`g_bUseBoneInBBox` 默认 true）。
    ///
    /// 判据：几何全在 `z > 0` 时，`bbmin[2]` 仍应为 **0**（而不是几何的最小 z）。
    /// 这正是官方产物里 `bbmin` 常见 `0.00` 的原因。
    #[test]
    fn autohitbox_starts_from_zero() {
        let d = tmpdir("autohb-zero");
        // 三角形全在 z ∈ [5, 9]，x/y 也远离原点。
        let smd = r#"version 1
nodes
  0 "root" -1
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
end
triangles
myprop
  0 100.000000 100.000000 5.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000
  0 110.000000 100.000000 5.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 0 1.000000
  0 100.000000 110.000000 9.000000 0.000000 0.000000 1.000000 0.000000 1.000000 1 0 1.000000
end
"#;
        write(&d, "zero.smd", smd);
        let toml = r#"
[model]
name = "models/test/zero.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "zero.smd"
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        assert_eq!(c.desc.hitboxes.boxes.len(), 1, "应生成 1 个 box");
        let h = &c.desc.hitboxes.boxes[0];
        assert_eq!(
            h.bbmin,
            [0.0, 0.0, 0.0],
            "bbox 必须从全 0 起步（g_bUseBoneInBBox = true）"
        );
        assert_eq!(h.bbmax, [110.0, 110.0, 9.0], "max 应是几何的真实上界");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 显式 hitbox 存在时**不得**自动生成，也不得置 `0x1` 标志。
    ///
    /// 实测语料：63 个模型用显式 `$hbox`，它们都**不带** `0x1`。
    #[test]
    fn explicit_hitbox_suppresses_autogeneration() {
        let d = tmpdir("explicit-hb");
        write(&d, "myprop-ref.smd", SMD);
        let toml = format!(
            "{}\n[hitboxes]\nset_name = \"default\"\n\n[[hitboxes.boxes]]\nbone = \"root\"\nbbmin = [-1.0, -2.0, -3.0]\nbbmax = [4.0, 5.0, 6.0]\n",
            desc_toml("myprop-ref.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        assert!(!c.desc.hitboxes.autogenerated, "不应标记为自动生成");
        assert_eq!(c.desc.hitboxes.boxes.len(), 1, "应保留显式的 1 个 box");
        assert_eq!(c.desc.hitboxes.boxes[0].bbmin, [-1.0, -2.0, -3.0]);
        assert_eq!(
            c.desc.model.extra_flags.unwrap_or(0) & crate::mdl_writer::FLAG_AUTOGENERATED_HITBOX,
            0,
            "显式 hitbox 不应置 0x1 标志"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- 姿态包围盒（`CalcSequenceBoundingBoxes`，`simplify.cpp:7049`）----

    /// **核心判据**：姿态包围盒必须与官方 studiomdl 产物一致。
    ///
    /// 用官方受控实验 `ipe1`（2 条序列、**无** `$staticprop`、含真实动画）：
    ///
    /// ```text
    /// hull   = [-20, 0, 0]      .. [0, 10, 12]
    /// seq[0] = [-20, 0, 0]      .. [0, 10, 12]      （hull 取自 seq[0]）
    /// seq[1] = [-20, -19.95, 0] .. [19.8, 10, 7]    （动画摆姿势后）
    /// ```
    ///
    /// `seq[1]` 的 y 到 **-19.95**、x 到 **19.8**，而静止姿势顶点 AABB 只有
    /// `[-20,0,0]..[0,10,7]` —— 这正是「姿态包围盒」与「顶点 AABB」的差别。
    ///
    /// 同时钉住**根骨骼的 `Rz(90°)`**（`panim->rotation` =
    /// `g_defaultrotation`，见 `BuildRawTransforms`，`simplify.cpp:264`）：
    /// 少了它，seq[1] 会算成 `[-19.95,-19.8,0]..[10,20,7]`（x/y 互换）。
    #[test]
    fn pose_bounds_match_official_ipe1() {
        let d = tmpdir("pose-bbox");
        write(&d, "ipe.smd", IPE_SMD);
        write(&d, "ipeanim.smd", IPE_ANIM1);
        write(&d, "ipeanim2.smd", IPE_ANIM2);
        let toml = r#"
[model]
name = "models/test/ipe1.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"
position = [10.0, 0.0, 3.0]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "idle"
smd = "ipeanim.smd"
fps = 30.0

[[sequences]]
name = "wave"
smd = "ipeanim2.smd"
fps = 30.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");

        let rb = bone_render_bounds(&c.desc, &c);
        let s0 = sequence_pose_bounds(&c.desc, &c, 0, &rb).expect("seq0 应有包围盒");
        let s1 = sequence_pose_bounds(&c.desc, &c, 1, &rb).expect("seq1 应有包围盒");

        // 官方 seq[0]
        let want0 = ([-20.0, 0.0, 0.0], [0.0, 10.0, 12.0]);
        // 官方 seq[1]
        let want1 = ([-20.0, -19.95, 0.0], [19.8, 10.0, 7.0]);

        for (tag, got, want) in [("seq0", s0, want0), ("seq1", s1, want1)] {
            for a in 0..3 {
                assert!(
                    (got.0[a] - want.0[a]).abs() <= 0.02,
                    "{tag} bbmin[{a}]：得 {}，官方 {}",
                    got.0[a],
                    want.0[a]
                );
                assert!(
                    (got.1[a] - want.1[a]).abs() <= 0.02,
                    "{tag} bbmax[{a}]：得 {}，官方 {}",
                    got.1[a],
                    want.1[a]
                );
            }
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// `$staticprop` **不套**根骨骼的 `Rz(90°)`。
    ///
    /// `MakeStaticProp()` 把 `g_panimation[0]->rotation` 置 0
    /// （`simplify.cpp:3379`），而几何本身已被旋转 —— 两者不能叠加，
    /// 否则会多转 90°。
    #[test]
    fn static_prop_pose_bounds_have_no_root_rotation() {
        let d = tmpdir("pose-sp");
        write(&d, "ipe.smd", IPE_SMD);
        let toml = r#"
[model]
name = "models/test/sp-pose.mdl"
surface_prop = "metal"
static_prop = true

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"
position = [10.0, 0.0, 3.0]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "idle"
smd = "ipe.smd"
fps = 30.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let rb = bone_render_bounds(&c.desc, &c);
        let b = sequence_pose_bounds(&c.desc, &c, 0, &rb).expect("应有包围盒");

        // 几何经 `Rz(90°)` 后是 x∈[-20,0] y∈[0,10] z∈[0,7]。
        // 若错误地再套一次根旋转，会变成 x∈[-10,0] y∈[-20,0]。
        assert!(
            (b.0[0] - (-20.0)).abs() <= 0.02,
            "静态道具的 x 下界应是 -20（几何已旋转过），实际 {}",
            b.0[0]
        );
        assert!(
            (b.0[1] - 0.0).abs() <= 0.02,
            "静态道具的 y 下界应是 0（**不**再套根旋转），实际 {}",
            b.0[1]
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// `hull` 取自 `seq[0]`（`write.cpp:2071-2087`）。
    #[test]
    fn hull_equals_seq0_pose_bounds() {
        let d = tmpdir("hull-seq0");
        write(&d, "ipe.smd", IPE_SMD);
        write(&d, "ipeanim.smd", IPE_ANIM1);
        let toml = r#"
[model]
name = "models/test/hull.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"
position = [10.0, 0.0, 3.0]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "idle"
smd = "ipeanim.smd"
fps = 30.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let rb = bone_render_bounds(&c.desc, &c);
        let s0 = sequence_pose_bounds(&c.desc, &c, 0, &rb).expect("应有包围盒");

        // 写出的 hull 必须等于 seq0 的姿态包围盒。
        let out = crate::mdl_writer::write_mdl(&c).unwrap();
        let g = |o: usize| f32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        for a in 0..3 {
            assert!(
                (g(crate::mdl_writer::off::HULL_MIN + a * 4) - s0.0[a]).abs() <= 1e-3,
                "hull_min[{a}] 应等于 seq0 的 {}",
                s0.0[a]
            );
            assert!(
                (g(crate::mdl_writer::off::HULL_MAX + a * 4) - s0.1[a]).abs() <= 1e-3,
                "hull_max[{a}] 应等于 seq0 的 {}",
                s0.1[a]
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// `$staticprop` 的包围盒必须用**塌缩后的身份动画**算，而不是原始序列。
    ///
    /// `MakeStaticProp()` 把动画压成 1 条 1 帧的身份动画
    /// （`simplify.cpp:3362-3380`），所以原始序列的逐帧姿态**不再参与**。
    ///
    /// 实测（`ipe2`）：`ipeanim.smd` 第 1 帧把 root 抬到 z=5，
    /// 若误用原始帧会得到 `bbmax=[0,10,12]`，而官方是 **`[0,10,7]`**。
    #[test]
    fn static_prop_pose_bounds_use_identity_frame() {
        let d = tmpdir("pose-sp-identity");
        write(&d, "ipe.smd", IPE_SMD);
        write(&d, "ipeanim.smd", IPE_ANIM1);
        let toml = r#"
[model]
name = "models/test/sp-id.mdl"
surface_prop = "metal"
static_prop = true

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"
position = [10.0, 0.0, 3.0]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "idle"
smd = "ipeanim.smd"
fps = 30.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let rb = bone_render_bounds(&c.desc, &c);
        let b = sequence_pose_bounds(&c.desc, &c, 0, &rb).expect("应有包围盒");

        // 官方 `ipe2`：hull = [0,0,0] .. [0,10,7]
        // （z 上界 7 而不是 12 —— 身份帧里 root 在 z=0）。
        assert!(
            (b.1[2] - 7.0).abs() <= 0.02,
            "bbmax[2] 应是 7（身份帧），实际 {} —— 若为 12 说明误用了原始帧",
            b.1[2]
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// **blend 序列的包围盒是「每一格的并集」**（`simplify.cpp:7184-7196`）。
    ///
    /// 官方分两步：
    ///
    /// ```c
    /// // ① 逐动画算自己的 bmin/bmax
    /// for (i = 0; i < g_numani; i++) { ... g_panimation[i]->bmin = bmin; }
    ///
    /// // ② 逐**序列**把所有格的盒子求并
    /// for (i = 0; i < g_sequence.Count(); i++)
    ///   for (j = 0; j < g_sequence[i].groupsize[0]; j++)
    ///     for (k = 0; k < g_sequence[i].groupsize[1]; k++) {
    ///       s_animation_t *panim = g_sequence[i].panim[j][k];
    ///       if (panim->bmin[0] < bmin[0]) bmin[0] = panim->bmin[0];
    ///       ...
    ///     }
    /// ```
    ///
    /// 早先本实现只用了**第一格**的帧 —— 实测 miku `look_poses`
    /// （3 格）的包围盒因此只有第一格那么大。
    ///
    /// 判据：造两条动画，一条让 `tip` 在 `+x` 伸展、另一条在 `+y`，
    /// 则序列的包围盒必须**同时**覆盖两者 —— 只看第一格会漏掉第二格。
    #[test]
    fn blend_sequence_bounds_union_all_cells() {
        let d = tmpdir("blend-bounds");
        write(&d, "ipe.smd", IPE_SMD);
        write(&d, "x.smd", IPE_ANIM_X);
        write(&d, "y.smd", IPE_ANIM_Y);
        let toml = r#"
[model]
name = "models/test/blendbounds.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[model.pose_parameters]]
name = "p"
start = 0.0
end = 1.0

[[animations]]
name = "a_x"
smd = "x.smd"

[[animations]]
name = "a_y"
smd = "y.smd"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "poses"
smd = "x.smd"
blend_width = 2
blends = ["a_x", "a_y"]

[[sequences.blend_params]]
parameter = "p"
start = 0.0
end = 1.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let rb = bone_render_bounds(&c.desc, &c);
        let b = sequence_pose_bounds(&c.desc, &c, 0, &rb).expect("应有包围盒");

        // `a_x` 把 tip 抬到 x=+30，`a_y` 抬到 y=+30（模型空间是 `(x,y,z)`，
        // 根骨骼的 Rz90 会把两者换轴 —— 所以只断言「两个方向都被覆盖」，
        // 不锁死具体轴）。
        let span = [b.1[0] - b.0[0], b.1[1] - b.0[1], b.1[2] - b.0[2]];
        let covered = span.iter().filter(|v| **v >= 25.0).count();
        assert!(
            covered >= 2,
            "两条动画分别在不同方向伸展，包围盒应覆盖**两个**方向；实际 span={span:?} \
             —— 只有 1 个方向说明只用了第一格"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// **`subtract` 动画的包围盒用「减除之前」的姿态算**（官方行为）。
    ///
    /// 官方 `CalcSequenceBoundingBoxes`（`simplify.cpp:7087`）用
    /// `AngleMatrix(sanim.rot, sanim.pos)` 直接建局部矩阵，**不**走
    /// `CalcBoneTransforms` 的 DELTA 合成分支（`simplify.cpp:4558-4577`）。
    /// 对 `subtract` 动画 `sanim` 是**增量**，于是官方那边
    /// `bonetransform` 近单位阵 ⟹ `posetransform = inverse(boneToPose)`
    /// ⟹ 顶点被映射到**参考姿态**附近 ⟹ 得到一个**全身大小**的盒子。
    ///
    /// **决定性判据**（miku，`probe_nosub_bbox.js`）：把 `subtract` 去掉
    /// 再编译，`look_poses` 从 **44276588 ULP → 51 ULP**。
    ///
    /// 本测试造一个「减除后几乎不动、减除前动很多」的样本：
    /// 若误用减除后的帧，包围盒会缩成一点；用减除前的帧才是大的。
    #[test]
    fn subtract_animation_bounds_use_pre_subtract_frames() {
        let d = tmpdir("subtract-bounds");
        write(&d, "ipe.smd", IPE_SMD);
        // 参考动画与目标动画**同一姿态**（tip 在 `[30, 0, 3]`）：
        // 减除后增量恒为 **0**，而减除前是 `[30, 0, 3]`。
        // 这正是 miku `look_*` 的处境 —— 官方包围盒用后者。
        write(&d, "base.smd", IPE_ANIM_X);
        write(&d, "far.smd", IPE_ANIM_X);
        let toml = r#"
[model]
name = "models/test/subbbox.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[animations]]
name = "a_base"
smd = "base.smd"

[[animations]]
name = "a_same"
smd = "far.smd"
subtract = "a_base"
subtract_frame = 0

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "idle"
smd = "a_same"
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        // 减除前后帧都留了一份。
        assert!(
            c.animations[1].pre_subtract_frames.is_some(),
            "`subtract` 动画必须留一份减除前的帧（包围盒要用）"
        );
        // 减除后增量应为 0（目标与参考同帧）。
        let after = &c.animations[1].frames[0][1].position;
        assert!(
            after.iter().all(|v| v.abs() < 1e-6),
            "减除后增量应为 0，实际 {after:?}"
        );
        // 而减除前不是 0。
        let before = &c.animations[1].pre_subtract_frames.as_ref().unwrap()[0][1].position;
        assert!(
            before.iter().any(|v| v.abs() > 1.0),
            "减除前的姿态不应全 0，实际 {before:?}"
        );

        // ⚠️ **真正的判据**：序列的包围盒必须用**减除前**的姿态算。
        //
        // 用减除后的帧（全 0 增量）会让 `posetransform` 恒为
        // `inverse(boneToPose)`，盒子会**明显偏小**；
        // 用减除前的帧才会覆盖 `tip` 所在的 `x=30`。
        let rb = bone_render_bounds(&c.desc, &c);
        let b = sequence_pose_bounds(&c.desc, &c, 0, &rb).expect("应有包围盒");
        let span_x = b.1[0] - b.0[0];
        let span_y = b.1[1] - b.0[1];
        assert!(
            span_x.max(span_y) >= 25.0,
            "包围盒应覆盖 `tip` 的 30 单位伸展（说明用了**减除前**的姿态）；\
             实际 span=[{span_x}, {span_y}] —— 偏小说明误用了减除后的帧"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // ---- `sequence_pose_bounds` 的分组重写（`91bac84`）差分对照 ----
    //
    // 该 commit 把「4) 并入蒙皮后的顶点」从**帧循环内的逐顶点遍历**改成
    // **帧循环外建表 + 按骨骼分组的局部 min/max 归约**（单骨骼顶点），
    // 多骨骼顶点保持原遍历顺序。这条路径在真实视角模型上是 `write_mdl`
    // 的 84%，但 `parity/` 的 101 个夹具里 91 个只有 1 条序列、且
    // `linnea-export.toml` 只有 1 帧 —— 语料对这条路径**结构性失明**。
    //
    // 所以下面这份 `sequence_pose_bounds_naive` 是**改动前的原实现逐字
    // 拷贝**（`git show 91bac84^:src/compile.rs` 的 4595-4622 段 + 尾部
    // 判据），作为 oracle 与生产实现**逐位**对照 —— 不是近似比较，
    // 因为重写引入的任何浮点重结合都会改变输出字节。

    fn sequence_pose_bounds_naive(
        desc: &ModelDesc,
        compiled: &CompiledModelDesc,
        seq_index: usize,
        render_bounds: &[([f32; 3], [f32; 3])],
    ) -> Option<([f32; 3], [f32; 3])> {
        let seq = compiled.sequences.get(seq_index)?;
        let n = desc.bones.len();
        if n == 0 || seq.frames.is_empty() {
            return None;
        }

        let parents: Vec<i32> = bone_parents(desc);
        let ref_world: Vec<crate::bone_math::Matrix3x4> = internal_bone_world(compiled, &parents);

        let mut bmin = [f32::INFINITY; 3];
        let mut bmax = [f32::NEG_INFINITY; 3];

        let identity_frames: Vec<Vec<crate::smd::SmdPose>> = if desc.model.static_prop {
            let mut row = vec![
                crate::smd::SmdPose {
                    bone: 0,
                    position: [0.0; 3],
                    rotation: [0.0; 3],
                };
                n
            ];
            for (k, p) in row.iter_mut().enumerate() {
                p.bone = k as i32;
            }
            vec![row]
        } else {
            Vec::new()
        };

        let cell_frames: Vec<&[Vec<crate::smd::SmdPose>]> = if desc.model.static_prop {
            vec![&identity_frames]
        } else if seq.cells.is_empty() {
            vec![seq.pre_subtract_frames.as_ref().unwrap_or(&seq.frames)]
        } else {
            seq.cells
                .iter()
                .filter_map(|c| compiled.animations.get(*c))
                .map(|a| {
                    a.pre_subtract_frames
                        .as_ref()
                        .unwrap_or(&a.frames)
                        .as_slice()
                })
                .collect()
        };

        for frames in cell_frames {
            for frame in frames {
                let world = frame_worlds(desc, &parents, frame);
                let posetransform: Vec<crate::bone_math::Matrix3x4> = world
                    .iter()
                    .zip(ref_world.iter())
                    .map(|(w, r)| crate::bone_math::concat(w, &crate::bone_math::invert(r)))
                    .collect();

                for (k, w) in world.iter().enumerate() {
                    let (mn, mx) = render_bounds
                        .get(k)
                        .copied()
                        .unwrap_or(([0.0; 3], [0.0; 3]));
                    let (tmn, tmx) = transform_aabb(w, mn, mx);
                    for a in 0..3 {
                        if tmn[a] < bmin[a] {
                            bmin[a] = tmn[a];
                        }
                        if tmx[a] > bmax[a] {
                            bmax[a] = tmx[a];
                        }
                    }
                }

                // ★ 原实现：逐顶点遍历，且**在帧循环内**。
                for bp in &compiled.bodyparts {
                    for m in &bp.models {
                        for mesh in &m.meshes {
                            for v in &mesh.vertices {
                                let mut pos = [0.0f32; 3];
                                for pair in &v.bones {
                                    let k = pair[0] as usize;
                                    if k >= n {
                                        continue;
                                    }
                                    let t = transform_point(&posetransform[k], v.pos);
                                    for a in 0..3 {
                                        pos[a] += pair[1] * t[a];
                                    }
                                }
                                for a in 0..3 {
                                    if pos[a] < bmin[a] {
                                        bmin[a] = pos[a];
                                    }
                                    if pos[a] > bmax[a] {
                                        bmax[a] = pos[a];
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if bmin[0].is_finite() && bmax[0].is_finite() {
            Some((bmin, bmax))
        } else {
            None
        }
    }

    /// 两个包围盒**逐位**相同（含 `None` 的一致性）。
    fn assert_bounds_bit_eq(
        got: Option<([f32; 3], [f32; 3])>,
        want: Option<([f32; 3], [f32; 3])>,
        ctx: &str,
    ) {
        match (got, want) {
            (None, None) => {}
            (Some(g), Some(w)) => {
                for a in 0..3 {
                    assert_eq!(
                        g.0[a].to_bits(),
                        w.0[a].to_bits(),
                        "{ctx}：bbmin[{a}] 与朴素实现逐位不同 —— 分组 {} vs 朴素 {}",
                        g.0[a],
                        w.0[a]
                    );
                    assert_eq!(
                        g.1[a].to_bits(),
                        w.1[a].to_bits(),
                        "{ctx}：bbmax[{a}] 与朴素实现逐位不同 —— 分组 {} vs 朴素 {}",
                        g.1[a],
                        w.1[a]
                    );
                }
            }
            (g, w) => panic!("{ctx}：有无包围盒不一致 —— 分组 {g:?} vs 朴素 {w:?}"),
        }
    }

    /// 与 [`SPB_SMD`] 配套的描述（2 骨骼 + 1 条单动画序列）。
    fn spb_toml(seq_smd: &str) -> String {
        format!(
            r#"
[model]
name = "models/test/spb.mdl"
surface_prop = "metal"

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
smd = "spb.smd"

[[sequences]]
name = "idle"
smd = "{seq_smd}"
"#
        )
    }

    /// 网格 SMD：**11 个单骨骼 + 2 个多骨骼**顶点，且第一个顶点的 x 是
    /// `-0.0`（`0.0f32 +` 归一化那条注释的对照）。
    ///
    /// 后段顶点刻意用**满尾数**坐标、并混入 2²⁴ 量级与 1e-6 量级 ——
    /// 只有三项乘积都非零且量级悬殊时，`m0*x + m1*y + m2*z + m3` 的
    /// 舍入误差才可能被括号顺序放大到改变结果（坐标里只要有 `0.0`，
    /// `m*0.0` 与 `+0.0` 都是精确运算，怎么重新结合都逐位相同）。
    ///
    /// **变异测试实测（重要，勿夸大本测试的强度）**：
    /// * ✅ 系数互换 `m0*x + m1*y` → `m1*x + m0*y`：**被抓住**（确定性差异）。
    /// * ✅ 中间量提到 f64 再降回 f32：**被抓住**（`38.766354` vs `38.76635`）。
    /// * ✅ **FMA 收缩** `m0.mul_add(x, m1.mul_add(y, m2.mul_add(z, m3)))`：
    ///   **被抓住** —— 这正是向量化最现实的失效模式（SIMD 后端默认允许
    ///   收缩），所以本测试对「向量化是否改变输出」是有实际约束力的。
    /// * ❌ 纯括号重结合 `(m0*x + m1*y) + (m2*z + m3)`：**未被抓住**。
    ///   即这份夹具（含 2²⁴ 与 1e-6 量级）也还没构造出能让该顺序产生
    ///   不同舍入的组合。这是**已知的覆盖缺口**，不要把它说成「逐位
    ///   比较能挡住一切重结合」。
    ///
    /// 真正兜底的是 `parity_snapshot.js --compare`（101 个夹具逐字节）与
    /// 6 个真实视角模型的逐字节对照 —— 本测试是快速反馈层，不是唯一防线。
    const SPB_SMD: &str = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
    1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
end
triangles
myprop
  0 -0.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000
  0 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
  0 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 2 0 0.750000 1 0.250000
  1 4.000000 0.000000 2.000000 0.000000 0.000000 1.000000 0.250000 0.500000 1 1 1.000000
  0 -8.000000 0.000000 -4.000000 0.000000 0.000000 1.000000 0.750000 0.250000 2 1 0.400000 0 0.600000
  1 -4.000000 0.000000 2.000000 0.000000 0.000000 1.000000 0.250000 0.750000 1 0 1.000000
  1 12.345678 -23.456789 34.567891 0.000000 0.000000 1.000000 0.125000 0.125000 1 1 1.000000
  1 -31.415927 17.283185 -27.182818 0.000000 0.000000 1.000000 0.375000 0.625000 1 1 1.000000
  0 2.7182818 -3.1415927 1.4142136 0.000000 0.000000 1.000000 0.625000 0.875000 1 0 1.000000
  1 1048576.000000 -2097152.000000 4194304.000000 0.000000 0.000000 1.000000 0.062500 0.937500 1 1 1.000000
  1 -8388608.000000 4194304.000000 -1048576.000000 0.000000 0.000000 1.000000 0.187500 0.312500 1 1 1.000000
  0 16777216.000000 8388608.000000 -4194304.000000 0.000000 0.000000 1.000000 0.437500 0.562500 1 0 1.000000
  0 0.000000976562 -0.000001953125 0.00000390625 0.000000 0.000000 1.000000 0.687500 0.812500 1 0 1.000000
end
"#;

    /// 3 帧，两根骨骼都有非平凡旋转（逼 `canonical_euler` 真走一遍分解）。
    ///
    /// 第 1、2 帧的 `tip` 旋转刻意用满尾数的弧度值：只有骨骼的
    /// `posetransform` 真的带旋转时，矩阵项才会是「非精确」的
    /// （纯平移矩阵是 `[1,0,0,tx; …]`，乘 `0`/加 `0` 全精确，任何重新
    /// 结合都逐位相同）。
    const SPB_ANIM: &str = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
    1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
  time 1
    0 1.000000 2.000000 3.000000 0.100000 0.200000 0.300000
    1 10.000000 4.000000 3.000000 0.432100 0.123400 0.987600
  time 2
    0 -2.000000 1.000000 0.500000 0.300000 0.000000 -0.200000
    1 12.000000 0.000000 -3.000000 1.2345678 -0.7654321 2.3456789
end
triangles
end
"#;

    /// 与 [`SPB_ANIM`] 同形但姿态与帧数都不同（blend 多格对照用）。
    const SPB_ANIM2: &str = r#"version 1
nodes
  0 "root" -1
  1 "tip" 0
end
skeleton
  time 0
    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
    1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
  time 1
    0 5.000000 -3.000000 1.000000 -0.400000 0.150000 0.250000
    1 6.000000 0.000000 3.000000 0.200000 0.000000 0.000000
end
triangles
end
"#;

    /// 分组路径（单骨骼）与多骨骼路径都必须与朴素实现**逐位**相同。
    ///
    /// 夹具里 11 个单骨骼顶点走新的分组归约、2 个多骨骼顶点走保留原序
    /// 的累加 —— 两条路径都要覆盖，否则这条测试等于没测。
    #[test]
    fn pose_bounds_grouped_path_matches_naive() {
        let d = tmpdir("spb-grouped");
        write(&d, "spb.smd", SPB_SMD);
        write(&d, "spb_anim.smd", SPB_ANIM);
        let desc = ModelDesc::from_toml(&spb_toml("spb_anim.smd")).unwrap();
        let c = compile(&desc, &d).expect("应能编译");

        assert_eq!(c.sequences[0].frames.len(), 3, "夹具应是 3 帧");
        let verts = &c.bodyparts[0].models[0].meshes[0].vertices;
        let n_single = verts.iter().filter(|v| v.bones.len() == 1).count();
        let n_multi = verts.iter().filter(|v| v.bones.len() >= 2).count();
        assert!(
            n_single >= 3,
            "夹具应有单骨骼顶点（分组路径），实际 {n_single}"
        );
        assert!(
            n_multi >= 2,
            "夹具应有多骨骼顶点（保持原序路径），实际 {n_multi}"
        );

        // ① 真实调用形态：`bone_render_bounds` 的自动命中盒。
        let rb = bone_render_bounds(&c.desc, &c);
        assert_bounds_bit_eq(
            sequence_pose_bounds(&c.desc, &c, 0, &rb),
            sequence_pose_bounds_naive(&c.desc, &c, 0, &rb),
            "单/多骨骼混合 + 3 帧（自动命中盒）",
        );

        // ② 零盒 —— 这一路才是**真正在测顶点并入**的那一条。
        //
        // 自动命中盒本身就包住所有顶点，于是顶点并入永远不改变极值，
        // 测试会退化成空转：实测把 `m0 * x + m1 * y` 写成
        // `m1 * x + m0 * y` 这种确定性变异都被放了过去。零盒只剩每根
        // 骨骼的原点（`bone_render_bounds` 在 `hitboxes.autogenerated
        // = false` 且无显式盒子时返回的正是它），顶点贡献必然决定极值。
        let zero = vec![([0.0f32; 3], [0.0f32; 3]); c.desc.bones.len()];
        let got = sequence_pose_bounds(&c.desc, &c, 0, &zero).expect("夹具应有包围盒");
        assert_bounds_bit_eq(
            Some(got),
            sequence_pose_bounds_naive(&c.desc, &c, 0, &zero),
            "单/多骨骼混合 + 3 帧（零盒）",
        );

        // 反证：顶点并入确实决定了极值 —— 去掉全部顶点后包围盒必须变窄。
        let mut c_noverts = c.clone();
        for mesh in c_noverts.bodyparts[0].models[0].meshes.iter_mut() {
            mesh.vertices.clear();
        }
        let noverts = sequence_pose_bounds(&c_noverts.desc, &c_noverts, 0, &zero)
            .expect("仅 render bounds 也应有包围盒");
        assert!(
            noverts.0 != got.0 || noverts.1 != got.1,
            "去掉全部顶点后包围盒应当变窄（{noverts:?} vs {got:?}）—— \
             否则说明顶点并入那条路径没生效，本测试是空转"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 全骨骼越界的顶点（`has_orphan`）必须与朴素实现一致。
    ///
    /// 正常编译路径**产生不了**这种顶点 —— `smd_vertex_to_ir` 保证每个
    /// 顶点至少有一组有效绑定（`compile.rs` 的 `bones.is_empty()` 报错），
    /// `$staticprop` 更是把所有权重塌到骨骼 0（而 `n` 也已塌缩为 1）。
    /// 所以只能手工构造。但生产实现里 `has_orphan` 是**独立分支**，
    /// 必须有测试锁住它。
    #[test]
    fn pose_bounds_orphan_vertices_match_naive() {
        let d = tmpdir("spb-orphan");
        write(&d, "spb.smd", SPB_SMD);
        write(&d, "spb_anim.smd", SPB_ANIM);
        let desc = ModelDesc::from_toml(&spb_toml("spb_anim.smd")).unwrap();
        let mut c = compile(&desc, &d).expect("应能编译");

        // 骨骼 99 越界（`n == 2`）⟹ `nvalid == 0` ⟹ `pos` 恒为 `[0,0,0]`。
        //
        // 为了让这个 `+0.0` 成为 `bbmin` 的**唯一来源**，其余每一项贡献都
        // 必须落在正半轴。这里有个坑：非 `$staticprop` 的根骨骼带一个
        // `Rz(90°)` 偏置（`frame_worlds_from_locals`，`compile.rs:4315-4323`），
        // 点变换是 `(x,y,z) → (-y, x, z)` —— 所以「全正」的输入盒子会被
        // 转出负坐标（实测 `bbmin[0] = -202`）。**必须把输入放在 x>0、y<0
        // 的象限**，转完才是全正：
        //   * 帧换成**零旋转**的纯平移帧（避免再叠加一层旋转）；
        //   * `render_bounds` 手工放到 `[100,-200,100]..[101,-199,101]`
        //     （不用 `bone_render_bounds` —— 它按零盒算，本身就会带上原点）；
        //   * 其余顶点全部绑到骨骼 1 且位置推到 `[200,-200,200]`。
        let zero_frames: Vec<Vec<crate::smd::SmdPose>> = vec![
            vec![
                crate::smd::SmdPose {
                    bone: 0,
                    position: [0.0, 0.0, 0.0],
                    rotation: [0.0; 3],
                },
                crate::smd::SmdPose {
                    bone: 1,
                    position: [10.0, 0.0, 3.0],
                    rotation: [0.0; 3],
                },
            ],
            vec![
                crate::smd::SmdPose {
                    bone: 0,
                    position: [1.0, 2.0, 3.0],
                    rotation: [0.0; 3],
                },
                crate::smd::SmdPose {
                    bone: 1,
                    position: [10.0, 0.0, 3.0],
                    rotation: [0.0; 3],
                },
            ],
        ];
        c.sequences[0].cells = Vec::new();
        c.sequences[0].pre_subtract_frames = None;
        c.sequences[0].frames = zero_frames;

        let verts = &mut c.bodyparts[0].models[0].meshes[0].vertices;
        verts[0].bones = vec![[99.0, 1.0]];
        for v in verts.iter_mut().skip(1) {
            v.bones = vec![[1.0, 1.0]];
            v.pos = [200.0, -200.0, 200.0];
        }
        let rb = vec![([100.0f32, -200.0, 100.0], [101.0f32, -199.0, 101.0]); c.desc.bones.len()];

        let got = sequence_pose_bounds(&c.desc, &c, 0, &rb);
        assert!(got.is_some(), "有帧可算时仍应有包围盒");
        assert_bounds_bit_eq(
            got,
            sequence_pose_bounds_naive(&c.desc, &c, 0, &rb),
            "含全越界顶点",
        );

        // 该顶点贡献的 `[0,0,0]` 必须是 `bbmin` 的**唯一来源**（其余顶点
        // 与 `render_bounds` 全部落在正半轴）。
        let g = got.unwrap();
        for a in 0..3 {
            assert_eq!(
                g.0[a].to_bits(),
                0.0f32.to_bits(),
                "越界顶点贡献的 +0.0 应成为 bbmin[{a}]，实际 {}",
                g.0[a]
            );
            assert!(
                g.1[a] > 0.0,
                "bbmax[{a}] 应来自正半轴的顶点，实际 {}",
                g.1[a]
            );
        }

        // 反证：把该顶点**整个摘掉**，`bbmin` 必须离开 0 —— 否则说明
        // `has_orphan` 那条分支根本没生效，上面的断言是空转。
        // （`sequence_pose_bounds` 只看 `mesh.vertices`，不看三角形，
        // 所以直接 `remove` 是安全的。）
        let mut c2 = c.clone();
        c2.bodyparts[0].models[0].meshes[0].vertices.remove(0);
        let valid = sequence_pose_bounds(&c2.desc, &c2, 0, &rb).expect("应仍有包围盒");
        for a in 0..3 {
            assert!(
                valid.0[a] > 0.0,
                "摘掉越界顶点后 bbmin[{a}] 必须离开 0（实际 {}）—— 说明该分支是活的",
                valid.0[a]
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// **帧循环一次都不跑时必须返回 `None`** —— 这是 `91bac84` 之后、
    /// 补本测试时发现并修掉的一处回归。
    ///
    /// `has_orphan` 的并入原本在帧循环内（每帧并一次常量），重写后提到
    /// 循环外「只需并一次」。但当 `cell_frames` 为空（`cells` 全部越界 ⟹
    /// `filter_map` 收出空 `Vec`）时，`bmin`/`bmax` 会停在 ±INF，此时再并
    /// 一个 `0.0` 就把「没有包围盒」变成了 `Some(([0,0,0],[0,0,0]))`。
    #[test]
    fn pose_bounds_empty_cell_frames_match_naive() {
        let d = tmpdir("spb-noframes");
        write(&d, "spb.smd", SPB_SMD);
        write(&d, "spb_anim.smd", SPB_ANIM);
        let desc = ModelDesc::from_toml(&spb_toml("spb_anim.smd")).unwrap();
        let base = compile(&desc, &d).expect("应能编译");

        // ① 无越界顶点：两侧都返回 None。
        let mut c = base.clone();
        c.sequences[0].cells = vec![999];
        let rb = bone_render_bounds(&c.desc, &c);
        let got = sequence_pose_bounds(&c.desc, &c, 0, &rb);
        assert!(got.is_none(), "cells 全越界 ⟹ 没有帧可算 ⟹ 应无包围盒");
        assert_bounds_bit_eq(
            got,
            sequence_pose_bounds_naive(&c.desc, &c, 0, &rb),
            "空 cell_frames",
        );

        // ② 有越界顶点：修复前这里返回 `Some(([0,0,0],[0,0,0]))`，
        //    而朴素实现返回 `None`。
        let mut c = base.clone();
        c.sequences[0].cells = vec![999];
        c.bodyparts[0].models[0].meshes[0].vertices[0].bones = vec![[99.0, 1.0]];
        let rb = bone_render_bounds(&c.desc, &c);
        let got = sequence_pose_bounds(&c.desc, &c, 0, &rb);
        assert!(
            got.is_none(),
            "没有帧可算时不该凭空造出 [0,0,0] 包围盒，实际 {got:?}"
        );
        assert_bounds_bit_eq(
            got,
            sequence_pose_bounds_naive(&c.desc, &c, 0, &rb),
            "空 cell_frames + 越界顶点",
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// blend 多格（`cell_frames` 多条、每格帧数不同）也要逐位一致。
    #[test]
    fn pose_bounds_blend_cells_match_naive() {
        let d = tmpdir("spb-blend");
        write(&d, "spb.smd", SPB_SMD);
        write(&d, "a1.smd", SPB_ANIM);
        write(&d, "a2.smd", SPB_ANIM2);
        let toml = r#"
[model]
name = "models/test/spb-blend.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[model.pose_parameters]]
name = "p"
start = 0.0
end = 1.0

[[animations]]
name = "a1"
smd = "a1.smd"

[[animations]]
name = "a2"
smd = "a2.smd"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "spb.smd"

[[sequences]]
name = "poses"
smd = "a1.smd"
blend_width = 2
blends = ["a1", "a2"]

[[sequences.blend_params]]
parameter = "p"
start = 0.0
end = 1.0
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        assert_eq!(c.sequences[0].cells.len(), 2, "夹具应是 2 格");
        // 两格的帧数**不同**（3 vs 2），这正是官方逐格用自己的 numframes 的情形。
        assert_eq!(c.animations[0].frames.len(), 3);
        assert_eq!(c.animations[1].frames.len(), 2);

        let rb = bone_render_bounds(&c.desc, &c);
        let got = sequence_pose_bounds(&c.desc, &c, 0, &rb);
        assert!(got.is_some(), "blend 序列应有包围盒");
        assert_bounds_bit_eq(
            got,
            sequence_pose_bounds_naive(&c.desc, &c, 0, &rb),
            "blend 2 格（3 帧 + 2 帧）",
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// `ipe` 的网格 SMD（官方 `docs/_probe/smdl/ipe.smd`）。
    const IPE_SMD: &str = r#"version 1
nodes
0 "root" -1
1 "tip" 0
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
end
triangles
myprop
0 0 0 0 1 0 0 0 0 1 0 1.000000
0 10 0 3 1 0 0 1 0 1 0 1.000000
1 0 20 7 1 0 0 0 1 1 0 1.000000
end
"#;

    // ---- `STUDIO_DELTA` 重建（`CalcBoneTransforms` 的 `else` 分支）----
    //
    // `subtract` 把动画变成**增量**，官方在 `CalcBoneTransforms`
    // （`simplify.cpp:4562-4578`）里用**基准动画第 0 帧**把它重建回完整姿态。
    // 不重建 ⟹ 增量近零 ⟹ 所有骨骼塌到原点 ⟹ IK 误差恒 0。
    //
    // 实测判据见 `docs/_probe/ab_iksub_mdlc.js` / `cmp_ikpayload_bytes.js`：
    // 官方与 mdlc 的 `@idle` 压缩载荷从「6 通道全不同」变成
    // **逐字节完全相同的 72 字节**。

    /// **核心判据**：`subtract` 造出的增量经重建后必须**还原出原始姿态**。
    ///
    /// 这是「有无 `subtract` 官方产物不变」的数学根据。两条恒等式：
    ///
    /// ```text
    /// p3 = base.pos + 1 · delta.pos = base.pos + (raw.pos − base.pos) = raw.pos
    /// q3 = base.q · (1 · conj(base.q)·raw.q) = raw.q
    /// ```
    ///
    /// 所以判据是：`delta_frame_worlds(base, subtract(raw, base))` 必须等于
    /// `frame_worlds(raw)`。
    ///
    /// 若 `delta_local_matrices` 被删掉（退回「直接用增量」），
    /// 左侧会给出增量本身 ⟹ 骨骼塌到原点 ⟹ 本测试变红。
    #[test]
    fn delta_reconstruction_undoes_subtract() {
        use crate::smd::SmdPose;

        let base = vec![
            SmdPose {
                bone: 0,
                position: [0.0; 3],
                rotation: [0.0; 3],
            },
            SmdPose {
                bone: 1,
                position: [30.0, 0.0, 3.0],
                rotation: [0.0, 0.0, 20.0f32.to_radians()],
            },
        ];
        // 原始（减除前）姿态：相对基准再转 15°、沿 x 再走 7。
        let raw = vec![
            SmdPose {
                bone: 0,
                position: [0.0; 3],
                rotation: [0.0; 3],
            },
            SmdPose {
                bone: 1,
                position: [37.0, 0.0, 3.0],
                rotation: [0.0, 0.0, 35.0f32.to_radians()],
            },
        ];
        // 用**生产代码**造增量（`subtract_base_frames`），避免测试自己
        // 手写一份可能与实现分叉的减法。
        let mut delta = [raw.clone()];
        // `std::slice::from_ref` 而不是 `&[base.clone()]` —— `base` 下面还要用。
        subtract_base_frames(&mut delta, std::slice::from_ref(&base), 0, &[1.0; 64]);
        let delta = &delta[0];

        // 增量确实是「小」的 —— 塌陷 bug 下误差会趋近 0。
        assert!(
            delta[1].position[0].abs() < 10.0,
            "增量应在 7 附近，实际 {:?}",
            delta[1].position
        );

        // 单骨骼的局部矩阵判据（不依赖父链，最直接）。
        let locals = delta_local_matrices(&base, delta);
        let p = [locals[1][3], locals[1][7], locals[1][11]];
        assert!(
            (p[0] - 37.0).abs() < 1e-4 && p[1].abs() < 1e-4 && (p[2] - 3.0).abs() < 1e-4,
            "重建后 tip 应还原到 [37, 0, 3]；实际 {p:?} —— \
             全 0 说明增量被当成完整姿态直接用，漏了基准帧"
        );
        let (s, c) = 35.0f32.to_radians().sin_cos();
        assert!(
            (locals[1][0] - c).abs() < 1e-4 && (locals[1][1] + s).abs() < 1e-4,
            "重建后绕 Z 应还原为 35°，实际 (0,0)={} (0,1)={}",
            locals[1][0],
            locals[1][1]
        );
    }

    /// `s == 1` 时重建**不等于**直接返回源姿态 —— `QuaternionMA` 内部的
    /// `QuaternionScale` / `QuaternionNormalize` 会引入浮点差。
    ///
    /// 这个差很小（约 `1e-7`），但足以在压缩误差的**量化边界**上翻转一个采样值，
    /// 所以实现里必须走完整条路径、不能加「`s == 1` 就直接拷贝」的短路。
    #[test]
    fn delta_reconstruction_applies_per_bone_weights_to_position_and_rotation() {
        use crate::smd::SmdPose;

        let base = vec![SmdPose {
            bone: 0,
            position: [10.0, -2.0, 5.0],
            rotation: [0.0, 0.0, 0.2],
        }];
        let delta = vec![SmdPose {
            bone: 0,
            position: [8.0, 4.0, -2.0],
            rotation: [0.0, 0.0, 0.8],
        }];
        for weight in [0.0, 0.25, 0.5, 1.0] {
            let matrices = delta_local_matrices_weighted(&base, &delta, &[weight]);
            let expected_angle = 0.2 + weight * 0.8;
            let expected = crate::bone_math::local_transform(
                [10.0 + weight * 8.0, -2.0 + weight * 4.0, 5.0 - weight * 2.0],
                [0.0, 0.0, expected_angle],
            );
            for (actual, expected) in matrices[0].iter().zip(expected) {
                assert!(
                    (actual - expected).abs() < 1e-5,
                    "weight={weight}: {actual} != {expected}"
                );
            }
        }
    }

    #[test]
    fn delta_reconstruction_keeps_float_noise_of_quaternion_ma() {
        use crate::smd::SmdPose;
        let base = vec![SmdPose {
            bone: 0,
            position: [0.0; 3],
            rotation: [0.0; 3],
        }];
        // 增量姿态刻意取一个**非平凡**旋转，让 `QuaternionScale` 的误差可见。
        let delta = vec![SmdPose {
            bone: 0,
            position: [0.0; 3],
            rotation: [0.3, 0.7, -0.4],
        }];
        let got = delta_local_matrices(&base, &delta);
        let want = crate::bone_math::local_transform([0.0; 3], [0.3, 0.7, -0.4]);
        let max = (0..12)
            .map(|i| (got[0][i] - want[i]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            max < 1e-5,
            "重建应非常接近源姿态（差 {max}）；差得离谱说明基准帧用错了"
        );
    }

    /// **核心判据**：增量为**零**时重建必须还原出基准姿态本身。
    ///
    /// 由 `p3 = base.pos + s · delta.pos`、`q3 = base.q · (s · delta.q)`，
    /// `delta = 0`（零平移 + 单位四元数）时 `p3 = base.pos`、`q3 = base.q`。
    ///
    /// ⚠️ **不能**写成 `delta_frame_worlds(base, base) == frame_worlds(base)` ——
    /// 那是错的：`p3 = base.pos + base.pos = 2·base.pos`。
    /// 这条式子恰好是「基准帧语义」的判据：若实现误把 `delta` 当完整姿态
    /// （即漏了 `+ base.pos`），零增量会给出 `[0,0,0]` 而不是 `base.pos`。
    #[test]
    fn delta_frame_worlds_with_zero_delta_reproduces_base() {
        use crate::smd::SmdPose;
        let d = tmpdir("delta-worlds");
        write(&d, "ipe.smd", IPE_SMD);
        write(&d, "base.smd", IPE_ANIM_X);
        let toml = r#"
[model]
name = "models/test/dw.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

[[bones]]
name = "root"

[[bones]]
name = "tip"
parent = "root"

[[animations]]
name = "a_base"
smd = "base.smd"

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "ipe.smd"

[[sequences]]
name = "idle"
smd = "a_base"
"#;
        let desc = ModelDesc::from_toml(toml).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        let parents = bone_parents(&c.desc);
        let base = &c.animations[0].frames[0];

        // 零增量：平移全 0、旋转全 0（单位四元数）。
        let zero: Vec<SmdPose> = base
            .iter()
            .map(|p| SmdPose {
                bone: p.bone,
                position: [0.0; 3],
                rotation: [0.0; 3],
            })
            .collect();

        let plain = frame_worlds(&c.desc, &parents, base);
        let delta = delta_frame_worlds(&c.desc, &parents, base, &zero);
        for k in 0..plain.len() {
            let max = (0..12)
                .map(|i| (plain[k][i] - delta[k][i]).abs())
                .fold(0.0f32, f32::max);
            assert!(
                max < 1e-5,
                "零增量时重建应还原基准姿态，骨骼 {k} 差 {max} —— \
                 差一个 base.pos 说明漏了「加上基准帧」那一步"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// 单帧动画：把 `tip` 沿 **+x** 推远。
    const IPE_ANIM_X: &str = r#"version 1
nodes
0 "root" -1
1 "tip" 0
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 30.000000 0.000000 3.000000 0.000000 0.000000 0.000000
end
triangles
end
"#;

    /// 单帧动画：把 `tip` 沿 **+y** 推远（与 [`IPE_ANIM_X`] 不同方向）。
    const IPE_ANIM_Y: &str = r#"version 1
nodes
0 "root" -1
1 "tip" 0
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 0.000000 30.000000 3.000000 0.000000 0.000000 0.000000
end
triangles
end
"#;

    /// `ipeanim.smd`：2 帧，root 的 z 从 0 到 5。
    const IPE_ANIM1: &str = r#"version 1
nodes
0 "root" -1
1 "tip" 0
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
time 1
0 0.000000 0.000000 5.000000 0.000000 0.000000 0.000000
1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
end
triangles
myprop
0 0 0 0 1 0 0 0 0 1 0 1.000000
0 10 0 3 1 0 0 1 0 1 0 1.000000
1 0 20 7 1 0 0 0 1 1 0 1.000000
end
"#;

    /// `ipeanim2.smd`：3 帧，root 绕 z 转到 3.0 弧度。
    const IPE_ANIM2: &str = r#"version 1
nodes
0 "root" -1
1 "tip" 0
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
time 1
0 0.000000 0.000000 0.000000 0.000000 0.000000 1.500000
1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
time 2
0 0.000000 0.000000 0.000000 0.000000 0.000000 3.000000
1 10.000000 0.000000 3.000000 0.000000 0.000000 0.000000
end
triangles
myprop
0 0 0 0 1 0 0 0 0 1 0 1.000000
0 10 0 3 1 0 0 1 0 1 0 1.000000
1 0 20 7 1 0 0 0 1 1 0 1.000000
end
"#;

    #[test]
    fn compiles_smd_into_mesh() {
        let d = tmpdir("basic");
        write(&d, "myprop-ref.smd", SMD);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        let c = compile(&desc, &d).expect("应能编译");
        assert_eq!(c.bodyparts.len(), 1);
        assert_eq!(c.bodyparts[0].models.len(), 1);
        let m = &c.bodyparts[0].models[0];
        assert_eq!(m.name, "myprop-ref.smd", "名字应自动取 SMD 文件名");
        assert_eq!(m.meshes.len(), 1);
        assert_eq!(m.meshes[0].material, 0);
        assert_eq!(m.meshes[0].vertices.len(), 3);
        assert_eq!(m.meshes[0].triangles.len(), 1);
        assert_eq!(c.total_vertices(), 3);
        assert_eq!(c.total_triangles(), 1);
        // SMD 的骨骼下标 1 是 "tip"，应映射到描述里的下标 1。
        assert_eq!(m.meshes[0].vertices[0].bones, vec![[1.0, 1.0]]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn deduplicates_shared_vertices_across_triangles() {
        let d = tmpdir("dedup");
        // 两个三角形共享一条边（4 个不同顶点，6 个顶点行）。
        let smd = SMD.replace(
            "  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000\nend",
            "  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000\nmyprop\n  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000\n  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000\n  1 8.000000 8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 1.000000 1 1 1.000000\nend",
        );
        write(&d, "myprop-ref.smd", &smd);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        let c = compile(&desc, &d).unwrap();
        let m = &c.bodyparts[0].models[0].meshes[0];
        assert_eq!(m.triangles.len(), 2);
        // 6 个顶点行 → 去重成 4 个顶点。
        assert_eq!(m.vertices.len(), 4, "共享顶点必须去重");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn splits_meshes_by_material_name() {
        let d = tmpdir("mats");
        // 注意：`str::replace` 会替换**所有**匹配，而 SMD 里有三个 `end`
        // （nodes / skeleton / triangles 各一个）。所以这里显式拼出完整文件，
        // 而不是用 replace 去插一段。
        let smd = r#"version 1
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
tex_a
  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000
  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000
tex_b
  1 1.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000
  1 2.000000 0.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
  1 1.000000 1.000000 0.000000 0.000000 0.000000 1.000000 0.000000 1.000000 1 1 1.000000
end
"#;
        write(&d, "myprop-ref.smd", smd);
        // 描述里要同时声明两个材质。
        let toml = desc_toml("myprop-ref.smd").replace(
            r#"textures = [{ name = "models/test/myprop" }]"#,
            "textures = [\n  { name = \"models/test/tex_a\" },\n  { name = \"models/test/tex_b\" },\n]",
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).unwrap();
        let m = &c.bodyparts[0].models[0];
        assert_eq!(m.meshes.len(), 2, "两个材质名应产生两个 mesh");
        assert_eq!(m.meshes[0].material, 0);
        assert_eq!(m.meshes[1].material, 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn reports_unknown_material_with_smd_path() {
        let d = tmpdir("badmat");
        let smd = SMD.replace("myprop\n", "not_declared\n");
        write(&d, "myprop-ref.smd", &smd);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        // ⚠️ 文案在 `compile()` 里就渲染好了，所以语言必须在那一刻生效。
        let errs = crate::test_locale::with_english(|| compile(&desc, &d).unwrap_err());
        assert!(
            errs.iter().any(|x| x
                .message()
                .contains("was not found in [materials].textures")),
            "应报材质未声明：{errs:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn reports_missing_smd_file() {
        let d = tmpdir("missing");
        let desc = ModelDesc::from_toml(&desc_toml("nope.smd")).unwrap();
        let errs = compile(&desc, &d).unwrap_err();
        // 文案走译文表 ⟹ 断言英文源文前先钉住语言。
        let msgs = crate::test_locale::with_english(|| {
            errs.iter().map(|x| x.message()).collect::<Vec<_>>()
        });
        assert!(msgs.iter().any(|m| m.contains("cannot read")), "{msgs:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 同一条诊断在中文系统上确实走译文表（不是只有英文源文）。
    #[test]
    fn reports_missing_smd_file_is_translated_for_chinese() {
        let d = tmpdir("missing-zh");
        let desc = ModelDesc::from_toml(&desc_toml("nope.smd")).unwrap();
        let errs = compile(&desc, &d).unwrap_err();
        let msgs = crate::test_locale::with_locale("zh-CN", || {
            errs.iter().map(|x| x.message()).collect::<Vec<_>>()
        });
        assert!(msgs.iter().any(|m| m.contains("读不到")), "{msgs:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn resolve_source_paths_use_context_defaults_and_normalize_slashes() {
        let base = Path::new("project");
        assert_eq!(
            resolve_smd_path(base, "folder\\mesh").unwrap(),
            base.join("folder/mesh.smd")
        );
        assert_eq!(
            resolve_smd_path(base, "folder.with.dots/mesh").unwrap(),
            base.join("folder.with.dots/mesh.smd")
        );
        assert_eq!(
            resolve_vta_path(base, "face").unwrap(),
            base.join("face.vta")
        );
        assert_eq!(
            resolve_smd_path(base, "mesh.FBX").unwrap(),
            base.join("mesh.FBX")
        );
        let abs = std::env::temp_dir().join("mdlc-abs-check.vta");
        assert_eq!(resolve_vta_path(base, abs.to_str().unwrap()).unwrap(), abs);
    }

    #[test]
    fn implicit_source_does_not_skip_an_unsupported_higher_priority_candidate() {
        let d = tmpdir("implicit-source-precedence");
        write(&d, "mesh.smd", SMD);
        write(&d, "mesh.vrm", "unsupported");
        let err = resolve_smd_path(&d, "mesh").unwrap_err();
        assert!(matches!(err, SrcError::UnsupportedImplicitSource { .. }));
        assert!(resolve_smd_path(&d, "mesh.smd").is_ok());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn missing_extensionless_source_reports_the_attempted_smd_file() {
        let d = tmpdir("noext");
        let desc = ModelDesc::from_toml(&desc_toml("nope")).unwrap();
        let errs = compile(&desc, &d).unwrap_err();
        assert!(errs.iter().any(|error| matches!(error, CompileError::ReadFile { path, .. } if path.ends_with("nope.smd"))));
        std::fs::remove_dir_all(&d).ok();
    }

    /// ⚠️ **动画名引用不能被路径校验误伤**。
    ///
    /// `[[sequences]] smd` 既可能是 SMD 路径，也可能是 `[[animations]]`
    /// 的名字（`parity\blend1.toml` 的 `smd = "a_base"`）。查动画池命中时
    /// **根本不读文件** ⟹ 不能提前做「必须有扩展名」的校验，
    /// 否则 `a_base` 会被误报。本用例钉住这个懒求值行为。
    #[test]
    fn sequence_referencing_an_animation_name_needs_no_extension() {
        let d = tmpdir("animname");
        write(&d, "a.smd", SMD);
        let toml = format!(
            "{}\n[[animations]]\nname = \"a_base\"\nsmd = \"a.smd\"\n\
             \n[[sequences]]\nname = \"base\"\nsmd = \"a_base\"\n",
            desc_toml("a.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("引用动画名必须编译成功");
        assert_eq!(c.sequences.len(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    /// ⭐ **同文件只发提示、不报错**，且**只在真的有分歧时**发。
    ///
    /// 三条判据（缺一不可，见 [`same_fbx_geometry_note`]）：
    /// ① 源是 `.fbx`（**SMD 同文件完全正常**，8 帧源出 8 帧）；
    /// ② 该文件确实是几何源；
    /// ③ 从它采出的帧数 **> 1**（只有 1 帧时官方与 mdlc 一致，无分歧 ⟹ 不提示）。
    ///
    /// ⚠️ 判据 ③ 是**自我校准**的关键：`box.fbx` / `morph.fbx` 这类没有
    /// `AnimStack` 的静态网格，「同文件」也不会丢任何东西
    /// （`docs/_probe/oracle_samefile_static.js` 的 G1–G6 六个组合两边都是 1 帧），
    /// 提示会变成噪声。
    #[test]
    fn same_fbx_geometry_note_is_emitted_only_when_frames_would_differ() {
        let d = tmpdir("samefbx");
        let geom = vec![PathBuf::from("box.fbx")];
        let p = Path::new("box.fbx");
        // ① 多帧 + 几何源 ⟹ 提示，且文案要点明「这是有意的偏离」。
        //    文案走译文表 ⟹ 断言英文源文前先钉住语言。
        let msg = crate::test_locale::with_english(|| {
            same_fbx_geometry_note(p, &geom, 6).expect("多帧同文件应当提示")
        });
        assert!(msg.contains("used as both the mesh source"), "{msg}");
        assert!(
            msg.contains("deliberate deviation"),
            "必须说明这是有意偏离官方行为：{msg}"
        );
        assert!(
            msg.contains("separate FBX file"),
            "必须给出「想与官方一致时」的做法：{msg}"
        );
        assert!(
            msg.contains("6 frames"),
            "必须报出 mdlc 实际采到的帧数（用户才知道分歧有多大）：{msg}"
        );
        // ② 只有 1 帧 ⟹ 官方与 mdlc 一致，**不提示**。
        assert!(
            same_fbx_geometry_note(p, &geom, 1).is_none(),
            "1 帧时没有分歧，不该提示"
        );
        assert!(same_fbx_geometry_note(p, &geom, 0).is_none());
        // ③ 不是几何源 ⟹ 不提示（FBX 只作动画源是合法的）。
        assert!(
            same_fbx_geometry_note(Path::new("rig.fbx"), &geom, 6).is_none(),
            "不是几何源就不该提示"
        );
        // ④ SMD 同文件**必须放行** —— 官方在 SMD 上没有这个陷阱。
        assert!(
            same_fbx_geometry_note(Path::new("a.smd"), &[PathBuf::from("a.smd")], 8).is_none(),
            "SMD 同文件正常（8 帧源出 8 帧），提示会变成噪声"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// ⭐ **`frames 0 0` 能拿到 1 帧**（端到端）。
    ///
    /// 同文件现在是**允许**的，本用例因此不再测「绕开判据」，而是测
    /// 「`frames` 区间照常生效」—— 它与同文件提示互不干扰。
    ///
    /// 夹具是 [`MIN_FBX`] —— 一份**手写的最小 ASCII FBX**（2131 B）。之所以不用
    /// Blender 生成的样本：那些不能进仓库（体量大），而 FBX 是**二进制**为主、
    /// 没法像 SMD 那样内联文本。ASCII FBX 恰好两全 —— ufbx 能读，文本可内联。
    #[test]
    fn frames_range_still_applies_on_a_same_file_fbx() {
        let d = tmpdir("framesorder");
        write(&d, "anim.fbx", MIN_FBX);
        // 同一份 FBX 既作网格源又作动画源 —— 现在是**允许**的组合。
        let toml = format!(
            "{}\n[[animations]]\nname = \"a1\"\nsmd = \"anim.fbx\"\nframes = [0, 0]\n\
             \n[[sequences]]\nname = \"idle\"\nsmd = \"a1\"\n",
            fbx_desc_toml()
        );
        let desc = ModelDesc::from_toml(&toml).expect("TOML 解析");
        let c = compile(&desc, &d).expect("同文件 + `frames 0 0` 必须能编过");
        let a = c
            .animations
            .iter()
            .find(|a| a.name == "a1")
            .expect("a1 应存在");
        assert_eq!(a.frames.len(), 1, "`frames 0 0` 之后应当只剩 1 帧");
        std::fs::remove_dir_all(&d).ok();
    }

    /// ⭐ **反方向**：不写 `frames` 时拿到**与「仅作动画源」完全相同的帧数**
    /// —— 不是官方的 1 帧。
    ///
    /// 与上一个测试配对。⭐ 断言刻意**不写死帧数**（[`MIN_FBX`] 的 6 个关键帧
    /// 跨 5 秒，30 fps 重采样后是 151 帧 —— 写死数字会让测试与夹具的时间轴
    /// 耦合），而是**两次编译对比**：
    ///
    /// | 编译 | `$body` 源 | 帧数 |
    /// |---|---|---|
    /// | A | 另一个文件 | N（基准） |
    /// | B | `anim.fbx` **自己** | **必须也是 N** |
    ///
    /// 这直接编码了「mdlc 不因同文件而退化」这条断言 —— 官方在这里会从 N
    /// 掉到 1，任何退化都会让 A≠B。
    #[test]
    fn same_file_fbx_without_a_frame_range_yields_all_frames() {
        let d = tmpdir("framesnoguard");
        write(&d, "anim.fbx", MIN_FBX);
        // 基准：`anim.fbx` 只作动画源（`$body` 用另一个文件）。
        write(&d, "static.fbx", MIN_FBX);
        let base_toml = format!(
            "{}\n[[animations]]\nname = \"a1\"\nsmd = \"anim.fbx\"\n\
             \n[[sequences]]\nname = \"idle\"\nsmd = \"a1\"\n",
            fbx_desc_toml_with_body("static.fbx")
        );
        let base =
            compile(&ModelDesc::from_toml(&base_toml).unwrap(), &d).expect("异文件必须能编过");
        let n_base = base
            .animations
            .iter()
            .find(|a| a.name == "a1")
            .expect("a1 应存在")
            .frames
            .len();
        assert!(n_base > 1, "夹具必须有多帧（否则本测试空洞通过）：{n_base}");

        // 对照：同一个 `anim.fbx` **既作网格源又作动画源**。
        let same_toml = format!(
            "{}\n[[animations]]\nname = \"a1\"\nsmd = \"anim.fbx\"\n\
             \n[[sequences]]\nname = \"idle\"\nsmd = \"a1\"\n",
            fbx_desc_toml_with_body("anim.fbx")
        );
        let c = compile(&ModelDesc::from_toml(&same_toml).unwrap(), &d)
            .expect("同文件必须**允许**（FBX 网格动画合一）");
        let n_same = c
            .animations
            .iter()
            .find(|a| a.name == "a1")
            .expect("a1 应存在")
            .frames
            .len();
        assert_eq!(
            n_same, n_base,
            "同文件时的帧数必须与「仅作动画源」一致 —— 官方会从 {n_base} 掉到 1，\
             mdlc 不复制这个退化行为"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// SMD 同文件必须**能编过**（端到端钉住「陷阱是 FBX 专属」这条结论）。
    ///
    /// `docs/_probe/oracle_samefile_smd.js` 实测：`$body ab_z8.smd` +
    /// `$sequence idle "ab_z8.smd"` 官方出 **8 帧**（8 帧源出 8 帧），
    /// 与 FBX 的「静默 1 帧」形成对照。mdlc 必须同样放行。
    #[test]
    fn smd_used_as_both_geometry_and_animation_is_accepted() {
        let d = tmpdir("smdsame");
        write(&d, "a.smd", SMD);
        let toml = format!(
            "{}\n[[sequences]]\nname = \"idle\"\nsmd = \"a.smd\"\n",
            desc_toml("a.smd")
        );
        let desc = ModelDesc::from_toml(&toml).unwrap();
        let c = compile(&desc, &d).expect("SMD 同文件必须编译成功（官方也是如此）");
        assert_eq!(c.sequences.len(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    /// ⭐ **同文件是允许的**（`$body` / `$model` / `$bodygroup` 三种写法都不例外）。
    ///
    /// ⭐⭐ **这是有意的偏离**：FBX 本来就是网格与动画合一的容器，
    /// 官方却在这个组合下**静默地**只产出 1 帧
    /// （`docs/_probe/oracle_samefile_forms.js`，10 用例实测）：
    ///
    /// | 几何源 | 动画源 | 官方帧数 | mdlc |
    /// |---|---|---|---|
    /// | `$body body "x.fbx"` | `$sequence idle "x.fbx"` | **1** ❌ | **6** ✅ |
    /// | `$model "body" "x.fbx" { }` | 同上 | **1** ❌ | **6** ✅ |
    /// | `$bodygroup "bg" { studio "x.fbx" }` | 同上 | **1** ❌ | **6** ✅ |
    /// | 同上三种 | `$animation a "x.fbx"` | **1** ❌ | **6** ✅ |
    ///
    /// mdlc **正常采出全部帧**，只发一条提示（`same_fbx_geometry_note`）。
    /// 理由：要求用户把网格和动画拆成两个文件，等于让用户为了迁就官方的
    /// 一个 bug 而改变自己的资产组织方式。
    ///
    /// ⚠️ 判据必须对三种几何声明形式**一视同仁** —— 少收一种写法就会漏提示。
    #[test]
    fn same_file_fbx_is_allowed_for_all_geometry_declaration_forms() {
        // 三种几何声明形式，全部指向同一个有动画的 FBX。
        let forms: [(&str, &str); 3] = [
            ("$body body \"anim.fbx\"", "body"),
            ("$model \"body\" \"anim.fbx\" {\n}", "model"),
            (
                "$bodygroup \"bg\"\n{\n\tstudio \"anim.fbx\"\n}",
                "bodygroup",
            ),
        ];
        for (geom, label) in forms {
            // 两个方向都要：`$sequence` 直引 与 `$animation` 块。
            let anims: [(&str, &str); 2] = [
                ("$sequence idle \"anim.fbx\"", "sequence"),
                (
                    "$animation a1 \"anim.fbx\"\n$sequence idle { a1 }",
                    "animation",
                ),
            ];
            for (anim, anim_label) in anims {
                let d = tmpdir(&format!("sameform_{label}_{anim_label}"));
                write(&d, "anim.fbx", MIN_FBX);
                let qc = format!(
                    "$modelname \"mymod/t.mdl\"\n$surfaceprop \"default\"\n\
                     $cdmaterials \"models/mymod\"\n{geom}\n{anim}\n"
                );
                let desc = crate::qc::parse_qc_str(&qc, &d)
                    .unwrap_or_else(|e| panic!("{label}+{anim_label}: QC 应解析成功：{e:?}"));
                let c = compile(&desc, &d).unwrap_or_else(|e| {
                    panic!("{label}+{anim_label}: 同文件必须**允许**（FBX 网格动画合一）：{e:?}")
                });
                // 关键断言：帧数必须与「异文件基准」一致，**不是**官方退化的 1 帧。
                // （`MIN_FBX` 的 6 个关键帧跨 5 秒，30 fps 下是 151 帧 ——
                //   写死数字会让测试与夹具的时间轴耦合，所以只断言 > 1。）
                let a = c
                    .animations
                    .iter()
                    .find(|a| a.name == "a1" || a.name == "@idle")
                    .unwrap_or_else(|| panic!("{label}+{anim_label}: 应产出动画"));
                assert!(
                    a.frames.len() > 1,
                    "{label}+{anim_label}: 必须采出多帧（官方退化成 1 帧，mdlc 不复制）：得到 {} 帧",
                    a.frames.len()
                );
                std::fs::remove_dir_all(&d).ok();
            }
        }
    }

    /// ⭐ **反方向**：几何源与动画源是**不同**文件时同样放行（6 帧）。
    ///
    /// 与上一个测试配对 —— 只测「同文件能过」不能证明判据没把异文件也
    /// 误判成同文件（例如 `same_file` 用错了比较口径）。
    #[test]
    fn different_file_fbx_is_accepted_for_all_geometry_declaration_forms() {
        let forms: [(&str, &str); 3] = [
            ("$body body \"static.fbx\"", "body"),
            ("$model \"body\" \"static.fbx\" {\n}", "model"),
            (
                "$bodygroup \"bg\"\n{\n\tstudio \"static.fbx\"\n}",
                "bodygroup",
            ),
        ];
        for (geom, label) in forms {
            let d = tmpdir(&format!("diffform_{label}"));
            // `static.fbx` 与 `anim.fbx` 内容相同但**路径不同** ⟹ 官方正常出 6 帧。
            write(&d, "static.fbx", MIN_FBX);
            write(&d, "anim.fbx", MIN_FBX);
            let qc = format!(
                "$modelname \"mymod/t.mdl\"\n$surfaceprop \"default\"\n\
                 $cdmaterials \"models/mymod\"\n{geom}\n\
                 $animation a1 \"anim.fbx\"\n$sequence idle {{ a1 }}\n"
            );
            let desc = crate::qc::parse_qc_str(&qc, &d)
                .unwrap_or_else(|e| panic!("{label}: QC 应解析成功：{e:?}"));
            compile(&desc, &d)
                .unwrap_or_else(|e| panic!("{label}: 异文件必须放行（官方出 6 帧）：{e:?}"));
            std::fs::remove_dir_all(&d).ok();
        }
    }

    /// SMD 里有、`[[bones]]` 里没有的骨骼 ⟹ **沿父链上溯**，不是报错。
    ///
    /// 官方 `MapSourcesToGlobalBonetable()`（`simplify.cpp:4148-4217`）：
    /// 按名查不到就沿 `localBone[].parent` 上溯（`:4167-4171`），整条链都
    /// 不在表里则静默重映射到根骨骼 0（`:4180` 的 `k = 0;`，「illegal parent
    /// bone replacement」诊断被 `#if 0` 关掉）。
    ///
    /// 这条路径是**必需的**：官方骨骼表只收 `$definebone` 与 `boneref != 0`
    /// 的骨骼，动画 SMD 里「零顶点引用、又没被保命判据提到」的骨骼不在表里。
    /// 旧实现直接报「不在描述的 [[bones]] 里」—— 那是「官方能编过、mdlc
    /// 编不过」的假阳性（用户工程 `linnea_replaces_zoey` 的 `TeenAngst.smd`
    /// / `ragdoll.smd` 各有 12 根这样的骨骼）。
    ///
    /// 本用例：SMD 的 `tip` 改名成 `ghost`，`ghost` 的父是 `root`（在表里）
    /// ⟹ 顶点应绑到 `root`（下标 0），且**编译成功**。
    #[test]
    fn smd_bone_absent_from_desc_falls_back_to_nearest_ancestor() {
        let d = tmpdir("badbone");
        let smd = SMD.replace("1 \"tip\" 0", "1 \"ghost\" 0");
        write(&d, "myprop-ref.smd", &smd);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        let c = compile(&desc, &d).expect("官方对这种情况从不报错，mdlc 也不该报");
        // `ghost` 不在 `[[bones]]` 里，但它的父 `root` 在 ⟹ 绑到 `root`。
        let m = &c.bodyparts[0].models[0];
        for v in &m.meshes[0].vertices {
            assert_eq!(
                v.bones,
                vec![[0.0, 1.0]],
                "顶点应沿父链回退到 root（下标 0）：{v:?}"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn bone_pose_falls_back_to_smd_then_desc_overrides() {
        let d = tmpdir("pose");
        write(&d, "myprop-ref.smd", SMD);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        let c = compile(&desc, &d).unwrap();
        // 描述没写 position → 取 SMD 的 (0,0,8)。
        let (pos, rot) = resolve_bone_pose(&desc, &c, 1);
        assert_eq!(pos, [0.0, 0.0, 8.0]);
        assert_eq!(rot, [0.0, 0.0, 0.0]);

        // 描述显式写 position/rotation（角度）→ 覆盖，且旋转转成弧度。
        let mut d2 = desc.clone();
        d2.bones[1].position = Some([1.0, 2.0, 3.0]);
        d2.bones[1].rotation = Some([90.0, 0.0, 0.0]);
        let c2 = compile(&d2, &d).unwrap();
        let (pos2, rot2) = resolve_bone_pose(&d2, &c2, 1);
        assert_eq!(pos2, [1.0, 2.0, 3.0]);
        assert!(
            (rot2[0] - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "{rot2:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn normalizes_weights_that_do_not_sum_to_one() {
        let d = tmpdir("weights");
        // 两个绑定，权重和 0.9（SMD 导出器常见）。
        let smd = SMD.replace(
            "1 1 1.000000\n  1 8.000000",
            "2 1 0.600000 0 0.300000\n  1 8.000000",
        );
        write(&d, "myprop-ref.smd", &smd);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        let c = compile(&desc, &d).unwrap();
        let b = &c.bodyparts[0].models[0].meshes[0].vertices[0].bones;
        let sum: f32 = b.iter().map(|x| x[1]).sum();
        assert!((sum - 1.0).abs() < 1e-5, "权重应被归一化，实际 {b:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn drops_degenerate_triangles_after_dedup() {
        let d = tmpdir("degen");
        // 三个顶点行里有两行完全相同 → 去重后退化，应被丢弃。
        let smd = SMD.replace(
            "  1 0.000000 8.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000 1 1 1.000000",
            "  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000",
        );
        write(&d, "myprop-ref.smd", &smd);
        let desc = ModelDesc::from_toml(&desc_toml("myprop-ref.smd")).unwrap();
        // ⚠️ 文案在 `compile()` 那一刻就渲染好了（`crate::tr_fmt!`），
        // 所以语言必须在**调用**那一刻生效 —— 事后包 `with_english` 是空操作。
        let errs = crate::test_locale::with_english(|| compile(&desc, &d).unwrap_err());
        // 唯一的三角形被丢弃 → 必须报错而不是产出空模型。
        assert!(
            errs.iter()
                .any(|x| x.message().contains("has no usable triangle left")),
            "{errs:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // ================= $weightlist =================
    //
    // 判据来自 `docs/_probe/cmp_weightlist_oracle.js`（11/11 与官方逐位一致）
    // 与 `docs/_probe/cmp_weightlist_errors.js`（11/11 判定一致）。
    // 这里的表就是那两张表的 Rust 版 —— 用的是**官方产物实测**的值。

    /// 骨骼链 `root → mid → leaf → tip` 的权重表描述。
    fn wl_desc(lists: &str) -> ModelDesc {
        let toml = format!(
            r#"
{lists}
[[bones]]
name = "root"

[[bones]]
name = "mid"
parent = "root"

[[bones]]
name = "leaf"
parent = "mid"

[[bones]]
name = "tip"
parent = "leaf"

[model]
name = "models/test/wl.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{{ name = "models/test/mat" }}]

[[bodyparts]]
name = "body"

[[bodyparts.models]]
smd = "wl.smd"
"#
        );
        ModelDesc::from_toml(&toml).expect("描述必须能解析")
    }

    #[test]
    fn weightlist_semantics_match_official() {
        // 官方实测（`cmp_weightlist_oracle.js`，逐位比较）：
        //   none             -> [1, 1, 1, 1]
        //   root 1           -> [1, 1, 1, 1]
        //   root 0           -> [0, 0, 0, 0]
        //   mid 0.5          -> [0, 0.5, 0.5, 0.5]   ← 根是 0，子沿父链继承
        //   mid 0            -> [0, 0, 0, 0]
        //   leaf 0           -> [0, 0, 0, 0]
        //   tip 0.25         -> [0, 0, 0, 0.25]
        //   mid 0.5 + tip 0  -> [0, 0.5, 0.5, 0]     ← 显式条目覆盖继承
        //   all four         -> [1, 0.75, 0.5, 0.25]
        let cases: &[(&str, &str, [f32; 4])] = &[
            (
                "root1",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "root"
weight = 1.0
"#,
                [1.0, 1.0, 1.0, 1.0],
            ),
            (
                "root0",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "root"
weight = 0.0
"#,
                [0.0, 0.0, 0.0, 0.0],
            ),
            (
                // 最能说明问题的一行：根是 **0**（不是 1），
                // 而 leaf/tip 沿父链继承 mid 的 0.5。
                "mid_half",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "mid"
weight = 0.5
"#,
                [0.0, 0.5, 0.5, 0.5],
            ),
            (
                "mid_zero",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "mid"
weight = 0.0
"#,
                [0.0, 0.0, 0.0, 0.0],
            ),
            (
                "tip_quarter",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "tip"
weight = 0.25
"#,
                [0.0, 0.0, 0.0, 0.25],
            ),
            (
                // 显式 `tip 0` **覆盖**从 mid 继承来的 0.5。
                "block_form",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "mid"
weight = 0.5

[[weight_lists.bones]]
bone = "tip"
weight = 0.0
"#,
                [0.0, 0.5, 0.5, 0.0],
            ),
            (
                "all_four",
                r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "root"
weight = 1.0

[[weight_lists.bones]]
bone = "mid"
weight = 0.75

[[weight_lists.bones]]
bone = "leaf"
weight = 0.5

[[weight_lists.bones]]
bone = "tip"
weight = 0.25
"#,
                [1.0, 0.75, 0.5, 0.25],
            ),
        ];

        for (tag, lists, want) in cases {
            let d = wl_desc(lists);
            let r = resolve_weight_lists(&d);
            assert_eq!(r.len(), 1, "{tag}: 应当解析出 1 张表");
            assert_eq!(r[0], want, "{tag}: 与官方实测不一致");
        }
    }

    #[test]
    fn default_weight_overrides_seed_named_lists_before_inheritance() {
        use crate::model::WeightEntry;

        let mut desc = wl_desc(
            "[[weight_lists]]\nname = 'upper'\n[[weight_lists.bones]]\nbone = 'leaf'\nweight = 0.75\n",
        );
        desc.default_weight_list = vec![
            WeightEntry {
                bone: "ROOT".into(),
                weight: 0.25,
                pos_weight: None,
            },
            WeightEntry {
                bone: "mid".into(),
                weight: 0.5,
                pos_weight: Some(0.125),
            },
        ];
        assert_eq!(resolve_default_weight_list(&desc), [0.25, 0.5, 0.5, 0.5]);
        assert_eq!(resolve_weight_lists(&desc), [vec![0.0, 0.5, 0.75, 0.75]]);
        assert!(desc.validate().is_ok());

        desc.default_weight_list[0].bone = "missing".into();
        assert!(desc.validate().unwrap_err().iter().any(|error| {
            error.path.starts_with("default_weight_list")
                && matches!(error.kind, DescErrorKind::BoneNotFound { .. })
        }));
        desc.default_weight_list[0].bone = "root".into();
        desc.default_weight_list[0].pos_weight = Some(f32::NAN);
        assert!(desc.validate().unwrap_err().iter().any(|error| {
            error.path.starts_with("default_weight_list")
                && matches!(error.kind, DescErrorKind::NotFinite { .. })
        }));
    }

    #[test]
    fn weightlist_absent_means_all_ones() {
        // 官方实测 `none` → [1,1,1,1]（隐式表 0：根 1、子骨骼沿父链继承 1）。
        let d = wl_desc("");
        assert!(resolve_weight_lists(&d).is_empty());
        assert_eq!(default_weight_list(4), vec![1.0; 4]);
    }

    #[test]
    fn weightlist_bone_name_is_case_insensitive() {
        // 官方 `findGlobalBone` 用 `stricmp`（`simplify.cpp:2628`），
        // 所以 `MID` 必须命中 `mid` —— 用大小写敏感的表会静默失效。
        let d = wl_desc(
            r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "MID"
weight = 0.5
"#,
        );
        assert_eq!(resolve_weight_lists(&d)[0], [0.0, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn weightlist_index_starts_at_one() {
        // 官方查找循环 `for (i = 1; i < g_numweightlist; i++)`
        // （`studiomdl.cpp:1719`）—— **跳过 0**（0 是隐式默认表）。
        let d = wl_desc(
            r#"[[weight_lists]]
name = "first"

[[weight_lists.bones]]
bone = "mid"
weight = 0.5

[[weight_lists]]
name = "second"

[[weight_lists.bones]]
bone = "tip"
weight = 0.25
"#,
        );
        assert_eq!(weight_list_index(&d, "first"), Some(1));
        assert_eq!(weight_list_index(&d, "second"), Some(2));
        // 表名也大小写不敏感（官方 `stricmp`）。
        assert_eq!(weight_list_index(&d, "FIRST"), Some(1));
        assert_eq!(weight_list_index(&d, "nope"), None);
    }

    #[test]
    fn merge_weights_takes_per_bone_max_across_cells() {
        // 官方 `simplify.cpp:302-318` 是 **MAX 而不是「取第一格」**。
        let a = vec![1.0, 0.0, 0.5, 0.0];
        let b = vec![0.0, 1.0, 0.25, 0.0];
        let m = merge_weights(&[0, 1], &[a.clone(), b.clone()], 4);
        assert_eq!(m, vec![1.0, 1.0, 0.5, 0.0]);
        // 单格 = 它自己。
        assert_eq!(merge_weights(&[1], &[vec![0.0; 4], b.clone()], 4), b);
        // 没有任何格 ⟹ 全 1（与「无 `$weightlist`」一致）。
        assert_eq!(merge_weights(&[], &[], 4), vec![1.0; 4]);
    }

    #[test]
    fn declared_but_unused_weightlist_leaves_weights_all_ones() {
        // 官方实测 `declared_unused` → [1,1,1,1]：表被声明但序列没引用它，
        // 于是序列用的是隐式表 0。**表仍然会被 resolve**（官方在
        // `buildAnimationWeights` 里遍历全部表，所以未知骨骼照样报错）。
        let d = wl_desc(
            r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "mid"
weight = 0.5
"#,
        );
        // 表本身被解析出来了……
        assert_eq!(resolve_weight_lists(&d)[0], [0.0, 0.5, 0.5, 0.5]);
        // ……但序列没有 `weight_list` ⟹ 用隐式表 0。
        assert!(d.sequences.is_empty());
        assert_eq!(default_weight_list(d.bones.len()), vec![1.0; 4]);
    }

    // ---- 校验：官方是硬错误，不是 warning ----

    #[test]
    fn validate_rejects_unknown_bone_in_weightlist() {
        // 官方 `unknown bone reference '%s' in weightlist '%s'`
        // （`simplify.cpp:1697`）—— 即使该表**没被引用**也报错。
        let d = wl_desc(
            r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "nosuchbone"
weight = 0.5
"#,
        );
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path.contains("weight_lists[0].bones[0].bone")
                && matches!(&e.kind, DescErrorKind::BoneNotFound { bone, .. } if bone == "nosuchbone")),
            "{errs:?}"
        );
    }

    #[test]
    fn validate_rejects_unknown_weightlist_reference() {
        // 官方 `unknown weightlist '%s'`（`studiomdl.cpp:1728`）。
        let mut d = wl_desc("");
        d.sequences.push(crate::model::Sequence {
            loop_restart: 0,
            fudge_loop: false,
            motion_rollback: crate::model::default_motion_rollback(),
            motion_type: 0,
            cycle_pose: None,
            key_values: None,
            frames: None,
            input_scale: None,
            name: "idle".into(),
            smd: "wl.smd".into(),
            fps: Some(30.0),
            looping: false,
            delta: false,
            activity: None,
            activity_weight: 0,
            events: Vec::new(),
            fade_in: 0.2,
            fade_out: 0.2,
            forward_declared: false,
            no_auto_ik: false,
            ik_rules: Vec::new(),
            cmds: Vec::new(),
            scale: None,
            adjust: None,
            rotation: None,
            iklocks: Vec::new(),
            blends: Vec::new(),
            blend_width: None,
            blend_params: Vec::new(),
            blend_ref: None,
            blend_comp: None,
            blend_center: None,
            transition: None,
            transition_reversible: false,
            auto_layers: Vec::new(),
            movements: Vec::new(),
            section_frames: None,
            section_threshold: None,
            extra_flags: None,
            weight_list: Some("NOPE".into()),
            subtract: None,
            subtract_frame: None,
            num_frames: None,
            src_stack: None,
            src_fps: None,
        });
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "sequences[0].weight_list"
                && matches!(&e.kind, DescErrorKind::WeightListNotFound { name } if name == "NOPE")),
            "{errs:?}"
        );
    }

    #[test]
    fn validate_rejects_duplicate_weightlist_name() {
        // 官方 `Duplicate weightlist '%s'`（`studiomdl.cpp:3344`），大小写不敏感。
        let d = wl_desc(
            r#"[[weight_lists]]
name = "WL"

[[weight_lists.bones]]
bone = "mid"
weight = 0.5

[[weight_lists]]
name = "wl"

[[weight_lists.bones]]
bone = "tip"
weight = 0.25
"#,
        );
        let errs = d.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(
                e.kind,
                DescErrorKind::Duplicate {
                    what: "weight list name",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn validate_weightlist_limits_match_real_binary() {
        // ⚠️ 官方 L4D2 的实测上限是 **128 条/表、128 张表**，
        // 不是 episode1 头文件写的 16/32（见 `MAX_WEIGHT_ENTRIES` 的说明）。
        // 这里把边界钉死：128 条合法、129 条非法。
        let ok = wl_desc(&format!(
            "[[weight_lists]]\nname = \"WL\"\n\n{}",
            (0..128)
                .map(|_| "[[weight_lists.bones]]\nbone = \"mid\"\nweight = 0.5\n")
                .collect::<String>()
        ));
        assert!(
            ok.validate().is_ok(),
            "128 条应当合法：{:?}",
            ok.validate().unwrap_err()
        );

        let bad = wl_desc(&format!(
            "[[weight_lists]]\nname = \"WL\"\n\n{}",
            (0..129)
                .map(|_| "[[weight_lists.bones]]\nbone = \"mid\"\nweight = 0.5\n")
                .collect::<String>()
        ));
        let errs = crate::test_locale::with_english(|| bad.validate().unwrap_err());
        assert!(
            errs.iter().any(|e| matches!(&e.kind, DescErrorKind::Other { message } if message.contains("too many entries"))),
            "{errs:?}"
        );
    }

    // ================= $declaresequence =================
    //
    // 判据来自 `docs/_probe/cmp_declaresequence_oracle.js`
    // （6/6 逐字段一致）与 `cmp_survivor_declaresequence.js`
    // （真实 41 KB survivor QCI，936 条序列含 933 条空壳全部一致）。

    /// 造一个带 `$declaresequence` 空壳的描述（TOML 侧）。
    ///
    /// `forward_declared = true` 是**顶层字段**，直接写在 `[[sequences]]` 里。
    ///
    /// ⚠️ 材质名必须与 `SMD` 夹具里的一致（`myprop`）—— 用别的名字
    /// `compile()` 会报「SMD 里的材质名找不到」。
    fn dsq_desc(extra: &str) -> String {
        format!(
            r#"
[model]
name = "models/test/dsq.mdl"
surface_prop = "metal"

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
smd = "dsq.smd"

{extra}
"#
        )
    }

    #[test]
    fn forward_declared_sequence_has_no_animation_data() {
        // 官方 `Cmd_DeclareSequence` 只 `memset` + 置 `STUDIO_OVERRIDE`：
        // **不分配 `panim`、不读 SMD**。所以编译出来的
        // `CompiledSequence` 必须是「空」的，且各字段停在 `memset` 值。
        let d = tmpdir("dsq");
        write(&d, "dsq.smd", SMD);
        let desc = ModelDesc::from_toml(&dsq_desc(
            r#"[[sequences]]
name = "shell"
forward_declared = true
"#,
        ))
        .unwrap();
        assert!(
            desc.validate().is_ok(),
            "{:?}",
            desc.validate().unwrap_err()
        );

        let c = compile(&desc, &d).unwrap();
        assert_eq!(c.sequences.len(), 1);
        let s = &c.sequences[0];
        assert!(s.forward_declared);
        // 官方 `memset` 之后这些字段**保持 0**，不是普通序列的默认值。
        assert_eq!(s.activity, 0, "空壳的 activity 是 memset 的 0，不是 -1");
        assert_eq!(s.fade_in, 0.0, "空壳的 fadeintime 是 0，不是 0.2");
        assert_eq!(s.fade_out, 0.0, "空壳的 fadeouttime 是 0，不是 0.2");
        assert!(s.cells.is_empty(), "空壳没有 blend 格");
        assert!(s.frames.is_empty(), "空壳没有帧");
        // ⚠️ **全 0 不是全 1** —— `groupsize=[0,0]` 让官方的 MAX 循环不跑。
        assert_eq!(
            s.weights,
            vec![0.0; desc.bones.len()],
            "空壳的 weightlist 是全 0（simplify.cpp:302-318 的初值）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn forward_declared_sequence_is_not_required_to_have_smd() {
        // 官方连 `panim` 都不分配 ⟹ 空壳**没有 SMD**。
        // `validate()` 对普通序列要求 `smd` 非空，对空壳必须放行。
        let d = tmpdir("dsq2");
        write(&d, "dsq.smd", SMD);
        let desc = ModelDesc::from_toml(&dsq_desc(
            r#"[[sequences]]
name = "shell"
forward_declared = true
"#,
        ))
        .unwrap();
        assert!(
            desc.validate().is_ok(),
            "空壳不该因为没有 smd 而报错：{:?}",
            desc.validate().unwrap_err()
        );
        // 反例：普通序列没有 smd 仍然要报错。
        let bad = ModelDesc::from_toml(&dsq_desc(
            r#"[[sequences]]
name = "normal"
"#,
        ))
        .unwrap();
        let errs = bad.validate().unwrap_err();
        assert!(
            errs.iter().any(|e| e.path == "sequences[0].smd"),
            "{errs:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn forward_declared_sequence_may_share_a_name_with_a_real_one() {
        // 官方 `Cmd_Sequence` 走 `LookupAnimation`（查**动画池**），
        // 空壳不在池里 ⟹ 「先 `$sequence x` 再 `$declaresequence x`」
        // 官方**不报错**，产出两条同名序列（实测 `fill_then_declare` → 2 条）。
        let d = tmpdir("dsq3");
        write(&d, "dsq.smd", SMD);
        let desc = ModelDesc::from_toml(&dsq_desc(
            r#"[[sequences]]
name = "same"
smd = "dsq.smd"
fps = 30.0

[[sequences]]
name = "same"
forward_declared = true
"#,
        ))
        .unwrap();
        assert!(
            desc.validate().is_ok(),
            "同名空壳不该报重复：{:?}",
            desc.validate().unwrap_err()
        );
        let c = compile(&desc, &d).unwrap();
        assert_eq!(c.sequences.len(), 2, "两条同名序列都要保留");
        assert!(!c.sequences[0].forward_declared);
        assert!(c.sequences[1].forward_declared);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn forward_declared_sequence_rejects_a_real_smd() {
        // 空壳带 SMD 说明两种东西被混在了一起 —— 产物会「看起来对但语义错」。
        let d = tmpdir("dsq4");
        write(&d, "dsq.smd", SMD);
        let desc = ModelDesc::from_toml(&dsq_desc(
            r#"[[sequences]]
name = "shell"
forward_declared = true
smd = "dsq.smd"
"#,
        ))
        .unwrap();
        // ⚠️ `DescErrorKind::Other` 的 message 是**构造时**就渲染好的
        // （`crate::tr_fmt!`），所以语言必须在 `validate()` 那一刻就是英文 ——
        // 事后把 `contains` 包进 `with_english` 是没用的。
        let errs = crate::test_locale::with_english(|| desc.validate().unwrap_err());
        assert!(
            errs.iter().any(|e| e.path == "sequences[0].smd"
                && matches!(&e.kind, DescErrorKind::Other { message } if message.contains("must not have an smd"))),
            "{errs:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn forward_declared_survives_a_model_with_no_animations_at_all() {
        // ⚠️ **本轮修掉的一个真 bug**：只有空壳的模型
        // `numlocalanim == 0` 而 `numlocalseq > 0`。
        // `mdl_writer` 早先把 seqdesc 数组整块挂在 `anim_count > 0` 上，
        // 于是空壳**一个字节都没写**。
        //
        // 官方实测（`dump_mdl_header.js`）：`numlocalanim=0` 时
        // `localanimindex` / `localseqindex` **都不是 0**（都是 1532），
        // seqdesc 数组照写不误。
        let d = tmpdir("dsq5");
        write(&d, "dsq.smd", SMD);
        let desc = ModelDesc::from_toml(&dsq_desc(
            r#"[[sequences]]
name = "shell_a"
forward_declared = true

[[sequences]]
name = "shell_b"
forward_declared = true
"#,
        ))
        .unwrap();
        let c = compile(&desc, &d).unwrap();
        assert_eq!(c.sequences.len(), 2);
        assert!(
            c.sequences.iter().all(|s| s.forward_declared),
            "两条都应是空壳"
        );
        // 写出后自检：`localseqindex` 必须指向 seqdesc 数组而不是 0。
        let out = crate::mdl_writer::write_mdl(&c).unwrap();
        let rd = |o: usize| i32::from_le_bytes(out.bytes[o..o + 4].try_into().unwrap());
        let seq_off = rd(0xC0);
        assert_ne!(seq_off, 0, "有序列时 localseqindex 不该是 0");
        assert_eq!(rd(0xBC), 2, "numlocalseq");
        // 该偏移处的第一条 seqdesc 必须真的是空壳：`flags = 0x800`。
        let flags = i32::from_le_bytes(
            out.bytes[seq_off as usize + 0x0C..seq_off as usize + 0x10]
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            flags & 0x800,
            0x800,
            "seqdesc[0].flags 应当带 STUDIO_OVERRIDE（说明数组真的写出来了）"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    // -----------------------------------------------------------------
    // 顶点超限自动拆分（`split_oversized_meshes`）
    // -----------------------------------------------------------------

    /// 造一个带 `n` 个三角形的 mesh，**顶点不共享**（每个三角形 3 个新顶点），
    /// 这样顶点数 == `3n`，可以精确控制到刚好越界。
    fn mesh_with(n: usize, material: usize) -> crate::model::Mesh {
        let mut verts = Vec::with_capacity(n * 3);
        let mut tris = Vec::with_capacity(n);
        for i in 0..n {
            let base = verts.len() as u32;
            for k in 0..3 {
                verts.push(crate::model::Vertex {
                    pos: [i as f32, k as f32, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    bones: vec![[0.0, 1.0]],
                });
            }
            tris.push([base, base + 1, base + 2]);
        }
        crate::model::Mesh {
            material,
            vertices: verts,
            triangles: tris,
            eyeball_tag: None,
        }
    }

    /// 造一个**三角形带**式的 mesh：`n` 个三角形、`n + 2` 个顶点
    /// （`tri[i] = [i, i+1, i+2]`）。
    ///
    /// 与 [`mesh_with`]（顶点不共享，`3n` 个顶点）互补：`3n` **够不到**
    /// 65536（不是 3 的倍数），而边界测试必须**恰好**落在 65536 上，
    /// 所以需要 `n + 2` 这种步长 —— 65534 个三角形 ⟹ 恰好 65536 个顶点。
    fn strip_mesh(n: usize, material: usize) -> crate::model::Mesh {
        let mut verts = Vec::with_capacity(n + 2);
        for i in 0..n + 2 {
            verts.push(crate::model::Vertex {
                pos: [i as f32, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                bones: vec![[0.0, 1.0]],
            });
        }
        let tris = (0..n)
            .map(|i| [i as u32, i as u32 + 1, i as u32 + 2])
            .collect();
        crate::model::Mesh {
            material,
            vertices: verts,
            triangles: tris,
            eyeball_tag: None,
        }
    }

    /// 造一个只有一个 model 的 `CompiledModelDesc`，把给定 mesh 塞进去。
    ///
    /// 先真编译一个最小夹具（保证 `desc` / `bodyparts` / 骨骼表都是**自洽**的），
    /// 再把它的 mesh 换成测试用的超大 mesh —— 这样测的是拆分本身，
    /// 而不是「夹具能不能编译」。
    fn desc_with_meshes(meshes: Vec<crate::model::Mesh>) -> crate::model::CompiledModelDesc {
        let d = tmpdir("split");
        std::fs::write(d.join("a.smd"), SMD).unwrap();
        let toml = desc_toml("a.smd");
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let mut c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(c.bodyparts.len(), 1, "夹具应只有一个 bodypart");
        assert_eq!(c.bodyparts[0].models.len(), 1, "夹具应只有一个 model");
        c.bodyparts[0].models[0].meshes = meshes;
        c.bodyparts[0].models[0].mesh_flexes = Vec::new();
        c.bodyparts[0].models[0].lods = None;
        c
    }

    /// **不超限时必须是 no-op** —— 这是「默认打开不影响既有产物」的依据。
    #[test]
    fn split_is_noop_when_under_limit() {
        let mut c = desc_with_meshes(vec![mesh_with(3, 0), mesh_with(5, 1)]);
        let before: Vec<(usize, usize, usize)> = c.bodyparts[0].models[0]
            .meshes
            .iter()
            .map(|m| (m.material, m.vertices.len(), m.triangles.len()))
            .collect();
        split_oversized_meshes(&mut c).expect("不该报错");
        let after: Vec<(usize, usize, usize)> = c.bodyparts[0].models[0]
            .meshes
            .iter()
            .map(|m| (m.material, m.vertices.len(), m.triangles.len()))
            .collect();
        assert_eq!(before, after, "未超限时不该动任何东西");
    }

    /// **拆完每块都不超限**，且三角形**总数守恒**。
    #[test]
    fn split_respects_limit_and_conserves_triangles() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH;
        // 每个三角形 3 个独立顶点 ⟹ 需要 (n*3) 个顶点。
        // 造 3 块多一点：2.5 倍上限的顶点数。
        let per_mesh_tris = MAXSTUDIOVERTS_PER_MESH / 3;
        let n_tris = per_mesh_tris * 2 + per_mesh_tris / 2;
        let mut c = desc_with_meshes(vec![mesh_with(n_tris, 7)]);
        split_oversized_meshes(&mut c).expect("拆分不该报错");

        let ms = &c.bodyparts[0].models[0].meshes;
        assert!(ms.len() >= 3, "应至少拆成 3 块，实际 {}", ms.len());
        for (i, m) in ms.iter().enumerate() {
            assert!(
                m.vertices.len() <= MAXSTUDIOVERTS_PER_MESH,
                "第 {i} 块有 {} 顶点，超过上限",
                m.vertices.len()
            );
            assert_eq!(
                m.material, 7,
                "拆出来的块必须**沿用原材质下标**（引擎靠它选材质）"
            );
        }
        let total: usize = ms.iter().map(|m| m.triangles.len()).sum();
        assert_eq!(total, n_tris, "三角形总数必须守恒");
    }

    /// **无损**：拆出来的每一块，其三角形对应的**顶点数据**与拆分前逐字段相同。
    ///
    /// 这是拆分最容易出的静默错误 —— 顶点重编号错位会让模型表面撕裂，
    /// 但不会报任何错。
    #[test]
    fn split_preserves_triangle_vertex_data() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH;
        let per_mesh_tris = MAXSTUDIOVERTS_PER_MESH / 3;
        let n_tris = per_mesh_tris + per_mesh_tris / 3;
        let original = mesh_with(n_tris, 3);

        // 拆分前的「三角形 → 三个顶点数据」多重集。
        let key = |v: &crate::model::Vertex| format!("{:?}|{:?}|{:?}", v.pos, v.normal, v.uv);
        let before: std::collections::BTreeSet<String> = original
            .triangles
            .iter()
            .map(|t| {
                let mut s: Vec<String> = t
                    .iter()
                    .map(|&i| key(&original.vertices[i as usize]))
                    .collect();
                s.sort();
                s.join(" ; ")
            })
            .collect();

        let mut c = desc_with_meshes(vec![original]);
        split_oversized_meshes(&mut c).expect("拆分不该报错");
        let ms = &c.bodyparts[0].models[0].meshes;
        assert!(ms.len() >= 2, "应至少拆成 2 块，实际 {}", ms.len());

        let mut after: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for m in ms {
            for t in &m.triangles {
                let mut s: Vec<String> = t.iter().map(|&i| key(&m.vertices[i as usize])).collect();
                s.sort();
                after.insert(s.join(" ; "));
            }
        }
        assert_eq!(
            before.len(),
            after.len(),
            "三角形（按顶点数据展开后）的数量必须相同"
        );
        let missing: Vec<&String> = before.difference(&after).collect();
        assert!(
            missing.is_empty(),
            "有 {} 个三角形在拆分后丢失或顶点数据被改，例如：{:?}",
            missing.len(),
            missing.first()
        );
    }

    /// 拆分必须**同步重建并行数组** —— `mesh_flexes` 与 `lods.meshes` 都是
    /// 与 `meshes` **同下标**的，不同步就会静默错位。
    #[test]
    fn split_keeps_parallel_arrays_in_sync() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH;
        let per_mesh_tris = MAXSTUDIOVERTS_PER_MESH / 3;
        let mut c = desc_with_meshes(vec![mesh_with(per_mesh_tris * 2, 0)]);
        // 给每个 mesh 造一个可识别的 flex 载荷，验证它跟着走。
        c.bodyparts[0].models[0].mesh_flexes = vec![vec![]];
        split_oversized_meshes(&mut c).expect("拆分不该报错");

        let m0 = &c.bodyparts[0].models[0];
        assert!(
            m0.meshes.len() >= 2,
            "应至少拆成 2 块，实际 {}",
            m0.meshes.len()
        );
        assert_eq!(
            m0.mesh_flexes.len(),
            m0.meshes.len(),
            "mesh_flexes 必须与 meshes 等长（否则下标错位）"
        );
    }

    /// **flex 的 `vertanim.index` 必须跟着重编号**。
    ///
    /// 这是拆分里最阴的一个坑：`index` 是**该 mesh 内**的顶点下标，
    /// 拆分后块内下标全变了。不重写的话引擎会把形状应用到**错误的顶点**上
    /// —— 不报错、不崩，只是形状错了（而且只有近看才明显）。
    #[test]
    fn split_remaps_flex_vertanim_indices() {
        use crate::flex::{ResolvedFlex, ResolvedVertAnim};
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH;

        let per_mesh_tris = MAXSTUDIOVERTS_PER_MESH / 3;
        let n_tris = per_mesh_tris * 2;
        let original = mesh_with(n_tris, 5);
        // 每个三角形 3 个独立顶点 ⟹ 顶点 i 的三角形是 i/3。
        // 取「第一块内」与「第二块内」各一个顶点做 vertanim，覆盖两类：
        // 第一块的应留在第一块，第二块的应跟着搬到第二块。
        let v_first = 1usize; // 第 0 个三角形 ⟹ 第 0 块
        let v_second = per_mesh_tris * 3 + 2; // 第 per_mesh_tris 个三角形 ⟹ 第 1 块
        let flex = ResolvedFlex {
            flexdesc: 0,
            targets: [0.0; 4],
            flexpair: 0,
            vertanimtype: 0,
            vertanims: vec![
                ResolvedVertAnim {
                    index: v_first as u32,
                    speed: 255,
                    side: 0,
                    delta: [1.0, 0.0, 0.0],
                    ndelta: [0.0, 0.0, 0.0],
                },
                ResolvedVertAnim {
                    index: v_second as u32,
                    speed: 255,
                    side: 0,
                    delta: [0.0, 2.0, 0.0],
                    ndelta: [0.0, 0.0, 0.0],
                },
            ],
        };
        // 拆分前的期望：每个 vertanim 落在哪个**顶点位置**上。
        let want: Vec<(usize, [f32; 3])> = flex
            .vertanims
            .iter()
            .map(|a| (a.index as usize, original.vertices[a.index as usize].pos))
            .collect();

        let mut c = desc_with_meshes(vec![original]);
        c.bodyparts[0].models[0].mesh_flexes = vec![vec![flex]];
        split_oversized_meshes(&mut c).expect("拆分不该报错");

        let m0 = &c.bodyparts[0].models[0];
        assert!(m0.meshes.len() >= 2, "应至少拆成 2 块");
        // 收集「拆完后每个 vertanim 指向的顶点位置」。
        let mut got: Vec<(usize, [f32; 3])> = Vec::new();
        for (ki, fx) in m0.mesh_flexes.iter().enumerate() {
            for f in fx {
                for a in &f.vertanims {
                    let v = &m0.meshes[ki].vertices[a.index as usize];
                    got.push((a.index as usize, v.pos));
                }
            }
        }
        got.sort_by(|a, b| a.1[0].partial_cmp(&b.1[0]).unwrap());
        let mut want_sorted = want.clone();
        want_sorted.sort_by(|a, b| a.1[0].partial_cmp(&b.1[0]).unwrap());
        assert_eq!(
            got.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
            want_sorted.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
            "拆分后 vertanim 必须仍指向**同一个几何位置**的顶点\
             （got={:?} want={:?}）",
            got.iter().map(|(i, p)| (i, p[0])).collect::<Vec<_>>(),
            want_sorted
                .iter()
                .map(|(i, p)| (i, p[0]))
                .collect::<Vec<_>>()
        );
        // 并且下标必须是**块内**合法的（越界会让写出器报内部错误）。
        for (ki, fx) in m0.mesh_flexes.iter().enumerate() {
            for f in fx {
                for a in &f.vertanims {
                    assert!(
                        (a.index as usize) < m0.meshes[ki].vertices.len(),
                        "块 {ki} 的 vertanim 下标 {} 超出该块顶点数 {}",
                        a.index,
                        m0.meshes[ki].vertices.len()
                    );
                }
            }
        }
    }

    /// 多 LOD：判据是**统一池**大小，不是 LOD 0。
    ///
    /// LOD 0 可以很小、而 LOD 1 引入大量新顶点 —— 那时统一池超限，
    /// 但「只看 LOD 0 顶点数」的实现会**静默放过**，写出
    /// `origMeshVertID` 溢出的 VTX。
    #[test]
    fn split_uses_unified_pool_size_for_multi_lod() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;

        // 手工造一个「LOD 0 只有 3 个顶点、LOD 1 有 LIMIT+2 个顶点」的 mesh。
        let mut c = desc_with_meshes(vec![mesh_with(1, 0)]);
        let n_l1 = LIMIT + 2;
        let mut l1_verts = Vec::with_capacity(n_l1);
        let mut l1_tris = Vec::with_capacity(n_l1 / 3);
        for i in 0..(n_l1 / 3) {
            let base = l1_verts.len() as u32;
            for k in 0..3 {
                l1_verts.push(Vertex {
                    pos: [i as f32, k as f32, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    bones: vec![[0.0, 1.0]],
                });
            }
            l1_tris.push([base, base + 1, base + 2]);
        }
        // LOD 0 用 mesh_with(1) 的 3 个顶点（与 LOD 1 不同 ⟹ 池是并集）。
        let l0 = mesh_with(1, 0);
        let ml = crate::lod::unify_lods(&[
            (l0.vertices.clone(), l0.triangles.clone()),
            (l1_verts, l1_tris),
        ]);
        assert!(
            ml.vertices.len() > LIMIT,
            "夹具必须让统一池超限，实际 {}",
            ml.vertices.len()
        );
        let model = &mut c.bodyparts[0].models[0];
        model.meshes[0] = l0;
        model.lods = Some(crate::model::ModelLods {
            meshes: vec![ml],
            num_lods: 2,
            switch_points: vec![0.0, 30.0],
            bone_lod_usage: vec![0; c.desc.bones.len()],
            no_facial: vec![false, false],
        });
        model.mesh_flexes = vec![Vec::new()];

        split_oversized_meshes(&mut c).expect("拆分不该报错");
        let model = &c.bodyparts[0].models[0];
        assert!(
            model.meshes.len() >= 2,
            "统一池超限就该拆，实际只拆成 {} 块（说明判据用了 LOD 0 的顶点数）",
            model.meshes.len()
        );
        assert_eq!(
            model.lods.as_ref().unwrap().meshes.len(),
            model.meshes.len(),
            "lods.meshes 必须与 meshes 等长"
        );
        // 每块（含多 LOD 统一池）都不超限。
        for (i, m) in model.lods.as_ref().unwrap().meshes.iter().enumerate() {
            assert!(
                m.vertices.len() <= LIMIT,
                "第 {i} 块的统一池有 {} 顶点，超过上限",
                m.vertices.len()
            );
        }
        // **LOD 数不变**，且每档的三角形数守恒。
        let lods = model.lods.as_ref().unwrap();
        for m in &lods.meshes {
            assert_eq!(m.triangles.len(), 2, "每块都要保留 2 档 LOD");
        }
        let l1_total: usize = lods.meshes.iter().map(|m| m.triangles[1].len()).sum();
        assert_eq!(l1_total, n_l1 / 3, "LOD 1 的三角形总数必须守恒");
        let l0_total: usize = lods.meshes.iter().map(|m| m.triangles[0].len()).sum();
        assert_eq!(l0_total, 1, "LOD 0 的三角形总数必须守恒");
    }

    /// 单 LOD 的 `mesh_flexes` 必须与拆分前的块一一对应。
    #[test]
    fn split_drops_flexes_that_have_no_vertex_in_a_block() {
        use crate::flex::{ResolvedFlex, ResolvedVertAnim};
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH;

        let per_mesh_tris = MAXSTUDIOVERTS_PER_MESH / 3;
        let original = mesh_with(per_mesh_tris * 2, 0);
        // 只给**第 0 块**里的顶点做 vertanim ⟹ 第 1 块不该有 flex。
        let flex = ResolvedFlex {
            flexdesc: 0,
            targets: [0.0; 4],
            flexpair: 0,
            vertanimtype: 0,
            vertanims: vec![ResolvedVertAnim {
                index: 0,
                speed: 255,
                side: 0,
                delta: [1.0, 0.0, 0.0],
                ndelta: [0.0; 3],
            }],
        };
        let mut c = desc_with_meshes(vec![original]);
        c.bodyparts[0].models[0].mesh_flexes = vec![vec![flex]];
        split_oversized_meshes(&mut c).expect("拆分不该报错");

        let m0 = &c.bodyparts[0].models[0];
        assert!(m0.meshes.len() >= 2);
        assert_eq!(
            m0.mesh_flexes[0].len(),
            1,
            "第 0 块含该顶点 ⟹ 应保留这条 flex"
        );
        for (ki, fx) in m0.mesh_flexes.iter().enumerate().skip(1) {
            assert!(
                fx.is_empty(),
                "块 {ki} 不含该 vertanim 的任何顶点 ⟹ 不该凭空造出 flex（会多写载荷）"
            );
        }
    }

    /// **退化网格**（有顶点、没三角形）不能把顶点弄丢，也不能 panic。
    #[test]
    fn split_leaves_degenerate_mesh_alone() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;
        // 顶点超限但**零个三角形** —— 贪心切块会给出 0 块。
        let n = LIMIT + 10;
        let mesh = crate::model::Mesh {
            material: 0,
            vertices: (0..n)
                .map(|i| Vertex {
                    pos: [i as f32, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    bones: vec![[0.0, 1.0]],
                })
                .collect(),
            triangles: Vec::new(),
            eyeball_tag: None,
        };
        let mut c = desc_with_meshes(vec![mesh]);
        split_oversized_meshes(&mut c).expect("不该 panic，也不该报错");
        let m0 = &c.bodyparts[0].models[0];
        assert_eq!(m0.meshes.len(), 1, "零三角形无法切块 ⟹ 原样保留");
        assert_eq!(m0.meshes[0].vertices.len(), n, "顶点一个都不能丢");
    }

    /// 贪心切块的**边界**：恰好等于上限 ⟹ 一块；上限 +1 ⟹ 两块。
    ///
    /// ⚠️ 这条测试必须用**顶点数可精确控制**的夹具：`mesh_with` 的顶点数
    /// 是 3 的倍数，**永远够不到** 65536。第一版就是这么写的，
    /// 于是把判据从 `>` 变异成 `>=` 时**测试照样全绿**（漏掉了一个真实
    /// 的 off-by-one —— 会把合法的 65536 顶点模型误拒）。
    /// 改用 `strip_mesh`（`n + 2` 个顶点）后 65536 可精确命中。
    #[test]
    fn greedy_blocks_are_exact_at_the_limit() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;
        assert_eq!(LIMIT, 65536);
        // `n + 2 == LIMIT` ⟹ n = 65534 个三角形。
        let exact = strip_mesh(LIMIT - 2, 0);
        assert_eq!(exact.vertices.len(), LIMIT, "夹具必须恰好等于上限");
        let blocks = greedy_triangle_blocks(&exact.triangles, LIMIT);
        assert_eq!(
            blocks.len(),
            1,
            "恰好 {LIMIT} 个顶点应是一块（判据是 `>` 不是 `>=`）"
        );

        // 再加一个三角形 ⟹ 顶点数 LIMIT+1 ⟹ 必须切成两块。
        let over = strip_mesh(LIMIT - 1, 0);
        assert_eq!(over.vertices.len(), LIMIT + 1);
        let blocks = greedy_triangle_blocks(&over.triangles, LIMIT);
        assert_eq!(blocks.len(), 2, "{LIMIT}+1 个顶点应切成两块");
        // 每块的顶点并集都不超限。
        for (i, b) in blocks.iter().enumerate() {
            let n: std::collections::HashSet<u32> =
                b.iter().flat_map(|t| t.iter().copied()).collect();
            assert!(n.len() <= LIMIT, "第 {i} 块有 {} 顶点", n.len());
        }
        // 三角形一个不少。
        assert_eq!(blocks.iter().map(Vec::len).sum::<usize>(), LIMIT - 1);
    }

    /// **恰好 65536 顶点的 mesh 必须被接受**（不许被自动拆分）。
    ///
    /// ⚠️ 夹具的三角形**逆序**排列，且断言「拆分前后 mesh **逐字段相同**」。
    /// 第一版用了正序夹具、只断言「mesh 数 == 1」—— 那样即使把判据变异成
    /// `>=`（把合法的 65536 顶点模型也送进拆分）也**照样全绿**：
    /// 切块恰好给出 1 块，且顶点按「三角形遇到顺序」重编号后与原顺序相同。
    /// 逆序之后「遇到顺序」≠「原顺序」⟹ 任何多余的拆分都会改编号，
    /// `assert_eq!` 就能抓到。
    #[test]
    fn split_accepts_a_mesh_at_exactly_the_limit() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;
        let mut mesh = strip_mesh(LIMIT - 2, 0);
        assert_eq!(mesh.vertices.len(), LIMIT, "夹具必须恰好等于上限");
        // 逆序：让「三角形遇到顺序」≠「顶点原顺序」。
        mesh.triangles.reverse();
        let before = mesh.clone();

        let mut c = desc_with_meshes(vec![mesh]);
        split_oversized_meshes(&mut c).expect("不该报错");
        let m0 = &c.bodyparts[0].models[0];
        assert_eq!(
            m0.meshes.len(),
            1,
            "恰好 {LIMIT} 个顶点是**合法**的，不该被拆（判据必须是 `>`）"
        );
        assert_eq!(
            m0.meshes[0], before,
            "恰好 {LIMIT} 个顶点 ⟹ 必须是**完全 no-op**（顶点一个都不许重编号）"
        );
    }

    /// 统一池里的**孤立顶点**（没有任何三角形引用）必须被搬进块里，
    /// 而且**不能**把某块撑超限。
    ///
    /// `unify_lods` 会把孤立顶点的 `lodFlags` 强制归到最低细节档
    /// （`write.cpp:2592`），所以它们是**真实存在**于 VVD 里的顶点 ——
    /// 拆分时丢掉会让 VVD 顶点数变化（引擎按 `numLODVertexes` 读，
    /// 结果整体错位）。硬塞进满块则会溢出 `origMeshVertID`。
    #[test]
    fn split_keeps_orphan_vertices_without_overflowing() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;

        // LOD 0：恰好 LIMIT 个顶点的带（刚好满）。
        let l0 = strip_mesh(LIMIT - 2, 0);
        assert_eq!(l0.vertices.len(), LIMIT);
        // 孤立顶点：不在任何三角形里。给它们**可识别的坐标**以便追踪。
        let n_orphan = 5usize;
        let mut verts = l0.vertices.clone();
        for i in 0..n_orphan {
            verts.push(Vertex {
                pos: [-1000.0 - i as f32, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0, 0.0],
                bones: vec![[0.0, 1.0]],
            });
        }
        let mut ml = crate::lod::unify_lods(&[(verts, l0.triangles.clone())]);
        assert_eq!(ml.vertices.len(), LIMIT + n_orphan);
        // 孤立顶点应当带最低细节档（`unify_lods` 的收尾）。
        let orphans: Vec<usize> = ml
            .vertices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.pos[0] <= -1000.0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(orphans.len(), n_orphan, "夹具应含 {n_orphan} 个孤立顶点");
        assert!(
            orphans.iter().all(|&i| ml.lod_flags[i] != 0),
            "孤立顶点的 lodFlags 不该是 0（否则排序时 Q_log2(0) 未定义）"
        );
        // 让孤立顶点带上「最高细节档」以便触发拆分路径（否则它们不参与
        // 统一池大小判据）。真实语料里它们是带最低档的，这里为了覆盖
        // 「孤立顶点 + 超限」的组合而人为置位。
        for &i in &orphans {
            ml.lod_flags[i] = 1;
        }

        let mut c = desc_with_meshes(vec![l0.clone()]);
        let model = &mut c.bodyparts[0].models[0];
        model.meshes[0] = l0;
        model.lods = Some(crate::model::ModelLods {
            meshes: vec![ml],
            num_lods: 1,
            switch_points: vec![0.0],
            bone_lod_usage: vec![0; c.desc.bones.len()],
            no_facial: vec![false],
        });
        model.mesh_flexes = vec![Vec::new()];

        split_oversized_meshes(&mut c).expect("拆分不该报错");
        let model = &c.bodyparts[0].models[0];
        let lods = model.lods.as_ref().unwrap();
        // ① 每块都不超限。
        for (i, m) in lods.meshes.iter().enumerate() {
            assert!(
                m.vertices.len() <= LIMIT,
                "第 {i} 块有 {} 顶点，超限（孤立顶点被硬塞进满块）",
                m.vertices.len()
            );
        }
        // ② 孤立顶点一个不少，且都还在。
        let kept = lods
            .meshes
            .iter()
            .flat_map(|m| m.vertices.iter())
            .filter(|v| v.pos[0] <= -1000.0)
            .count();
        assert_eq!(
            kept, n_orphan,
            "孤立顶点必须全部保留（丢掉会让 VVD 顶点数变化 ⟹ 整体错位）"
        );
    }

    /// 上游给了 `lods` 但**长度不对**时必须**报错**，而不是静默产出错位产物。
    ///
    /// `lods.meshes` 与 `meshes` 靠下标对应；长度不一致时「按 mesh 逐项搬」
    /// 会让结果比原数组短，写出器随后按下标取就会**材质贴错面**（不报错）。
    #[test]
    fn split_rejects_mismatched_lods_length() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;
        let mut c = desc_with_meshes(vec![mesh_with(LIMIT / 3 + 1, 0)]);
        let model = &mut c.bodyparts[0].models[0];
        model.meshes.push(mesh_with(2, 1));
        assert_eq!(model.meshes.len(), 2);
        // 故意只给 1 项（应当是 2 项）。
        let l0 = mesh_with(2, 1);
        model.lods = Some(crate::model::ModelLods {
            meshes: vec![crate::lod::MeshLods::single(
                l0.vertices.clone(),
                l0.triangles.clone(),
            )],
            num_lods: 1,
            switch_points: vec![0.0],
            bone_lod_usage: vec![0; c.desc.bones.len()],
            no_facial: vec![false],
        });
        model.mesh_flexes = vec![Vec::new(); 2];

        let errs = split_oversized_meshes(&mut c).expect_err("长度不一致必须报错");
        assert!(
            errs.iter().any(|x| x.message().contains("lods.meshes")),
            "错误信息应指明 lods.meshes 长度不对：{errs:?}"
        );
    }

    /// 拆完后 `lods.meshes` / `mesh_flexes` 必须与 `meshes` **严格等长**。
    ///
    /// 这是写出器与 VTX 的硬前提，不等长是**静默**错误（材质贴错面）。
    /// 这条走的是**真实多 LOD 编译**路径（不是手工造 IR），
    /// 所以能覆盖 `compile()` 里 `split_oversized_meshes` 的调用点。
    #[test]
    fn split_keeps_all_parallel_arrays_equal_length_for_multi_lod() {
        use crate::mdl_writer::MAXSTUDIOVERTS_PER_MESH as LIMIT;

        let d = tmpdir("split-lod-parallel");
        // LOD 0：小；LOD 1：超大（引入大量新顶点）⟹ 统一池超限。
        let l0 = strip_mesh(2, 0);
        let mut big = strip_mesh(LIMIT - 1, 0);
        // 挪开坐标，让 LOD 1 的顶点与 LOD 0 **不重合**（否则会被去重掉）。
        for v in big.vertices.iter_mut() {
            v.pos[0] += 100000.0;
        }
        std::fs::write(d.join("lod0.smd"), smd_of(&l0)).unwrap();
        std::fs::write(d.join("lod1.smd"), smd_of(&big)).unwrap();
        let toml = r#"
[model]
name = "models/test/splitlod.mdl"
surface_prop = "metal"

[materials]
search_paths = ["models/test"]
textures = [{ name = "models/test/myprop" }]

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
        let desc: ModelDesc = toml::from_str(toml).unwrap();
        let c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let m = &c.bodyparts[0].models[0];
        let lods = m.lods.as_ref().expect("应有多 LOD 数据");
        assert!(
            m.meshes.len() >= 2,
            "统一池超限 ⟹ 应拆成多块，实际 {}",
            m.meshes.len()
        );
        assert_eq!(
            lods.meshes.len(),
            m.meshes.len(),
            "lods.meshes 必须与 meshes 等长"
        );
        assert_eq!(
            m.mesh_flexes.len(),
            m.meshes.len(),
            "mesh_flexes 必须与 meshes 等长"
        );
        // 每块的统一池都不超限。
        for (i, ml) in lods.meshes.iter().enumerate() {
            assert!(
                ml.vertices.len() <= LIMIT,
                "第 {i} 块的统一池有 {} 顶点，超限",
                ml.vertices.len()
            );
            assert_eq!(ml.triangles.len(), 2, "第 {i} 块应保留 2 档 LOD");
        }
    }

    // ---- 三角形绕序（`flip_triangles`）----
    //
    // 起因：用户把 mdlc 的产物反编译进 Blender 后报「**面法向是反的**」，
    // 而官方产物完全正确。根因是 Source 的**正面是 CW**（与 Blender/OpenGL
    // 的 CCW 相反），官方在导入 SMD 时就把每个三角形的第 2、3 个顶点交换
    // （`v1support.cpp:192-196`，`flip_triangles` **默认 1**）。
    //
    // ⚠️ **这类 bug 逃过了所有「比顶点属性」的探针** ——
    // `cmp_vtx_vvd_full.js` 用 `vtxlib.js` 的 `triSet`（**无序**三元组集合）
    // 比三角形，绕序反了照样全绿。**判据必须显式比「顶点顺序」。**

    /// SMD 里一个三角形的三个顶点，**顺序**必须按 Source 约定翻转。
    ///
    /// 判据用**几何面法线**：SMD 的顶点行自带法线 `(0,0,1)`，
    /// 而三角形按 CCW 摆放时 `cross(v1−v0, v2−v0)` 指向 `+Z`。
    /// 官方翻转后绕序变 CW ⟹ 叉积指向 `−Z`。
    ///
    /// 夹具特意让「叉积」在翻转前后**符号相反且非零**（面积足够大），
    /// 否则会退化成空洞测试。
    #[test]
    fn flip_triangles_reverses_winding_by_default() {
        let d = tmpdir("flip-triangles");
        // 一个 CCW 三角形（从 +Z 看是逆时针）：叉积 = +Z
        //   v0=(0,0,0)  v1=(1,0,0)  v2=(0,1,0)
        //   e1=(1,0,0)  e2=(0,1,0)  cross = (0*0-0*1, 0*0-1*0, 1*1-0*0) = (0,0,1)
        let smd = r#"version 1
nodes
  0 "root" -1
end
skeleton
  time 0
    0 0 0 0 0 0 0
end
triangles
myprop
  0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000
  0 1.000000 0.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 0 1.000000
  0 0.000000 1.000000 0.000000 0.000000 0.000000 1.000000 0.000000 1.000000 1 0 1.000000
end
"#;
        write(&d, "m.smd", smd);
        // `flip_triangles = true`（默认）
        let toml_on = r#"
[[materials.textures]]
name = "myprop"
[model]
name = "m"
[[bodyparts]]
name = "b"
[[bodyparts.models]]
smd = "m.smd"
[[bones]]
name = "root"
"#;
        write(&d, "on.toml", toml_on);
        let desc_on = ModelDesc::from_toml(toml_on).expect("解析 TOML");
        let c_on = compile(&desc_on, &d).expect("编译（flip=true）");
        let tri_on = &c_on.bodyparts[0].models[0].meshes[0].triangles[0];
        let vs_on = &c_on.bodyparts[0].models[0].meshes[0].vertices;
        let n_on = tri_normal(vs_on, *tri_on);

        // `flip_triangles = false`（等价于 QC 写了 `reverse`）
        let toml_off =
            toml_on.replace("smd = \"m.smd\"", "smd = \"m.smd\"\nflip_triangles = false");
        write(&d, "off.toml", &toml_off);
        let desc_off = ModelDesc::from_toml(&toml_off).expect("解析 TOML");
        let c_off = compile(&desc_off, &d).expect("编译（flip=false）");
        let tri_off = &c_off.bodyparts[0].models[0].meshes[0].triangles[0];
        let vs_off = &c_off.bodyparts[0].models[0].meshes[0].vertices;
        let n_off = tri_normal(vs_off, *tri_off);

        let _ = std::fs::remove_dir_all(&d);

        // 非空洞硬门：两个法线都必须非零（面积够大）。
        assert!(
            n_on[2].abs() > 0.5,
            "flip=true 的叉积 Z = {}，夹具面积太小 ⟹ 空洞测试",
            n_on[2]
        );
        assert!(
            n_off[2].abs() > 0.5,
            "flip=false 的叉积 Z = {}，夹具面积太小 ⟹ 空洞测试",
            n_off[2]
        );
        // 核心判据：两者**符号相反**。
        assert!(
            n_on[2] * n_off[2] < 0.0,
            "flip=true 叉积 Z={} 与 flip=false 叉积 Z={} 应异号（绕序被翻转）",
            n_on[2],
            n_off[2]
        );
        // 且 `flip=false` 应保持 SMD 的原始 CCW（+Z）。
        assert!(
            n_off[2] > 0.0,
            "flip=false 应保持 SMD 原始绕序（叉积 +Z），实际 {}",
            n_off[2]
        );
        // `flip=true`（默认）应得到 CW（−Z）—— 这才是 Source 的约定。
        assert!(
            n_on[2] < 0.0,
            "flip=true（**默认**）应翻转成 CW（叉积 −Z），实际 {} \
             —— 这是「Blender 里面法向反了」的根因",
            n_on[2]
        );
    }

    /// 三角形 `(v0, v1, v2)` 的几何面法线（未归一化，符号即绕序）。
    fn tri_normal(verts: &[crate::model::Vertex], t: [u32; 3]) -> [f32; 3] {
        let a = verts[t[0] as usize].pos;
        let b = verts[t[1] as usize].pos;
        let c = verts[t[2] as usize].pos;
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ]
    }

    /// 把一个 `Mesh` 渲染成最小合法 SMD（材质名与 `desc_toml` 的材质一致）。
    ///
    /// SMD 顶点行末尾的三个骨骼下标必须是**真实的** `nodes` 下标 ——
    /// 写 `0 1 2` 会报「引用了骨骼下标 1，但 nodes 段只有 1 项」。
    fn smd_of(mesh: &crate::model::Mesh) -> String {
        let mut s = String::from(
            "version 1\nnodes\n  0 \"root\" -1\nend\nskeleton\n  time 0\n    \
             0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\nend\ntriangles\nmyprop\n",
        );
        for t in &mesh.triangles {
            for &i in t {
                let v = &mesh.vertices[i as usize];
                s.push_str(&format!(
                    "  0 {:.6} {:.6} {:.6} 0.000000 0.000000 1.000000 {:.6} {:.6} 1 0 1.000000\n",
                    v.pos[0], v.pos[1], v.pos[2], v.uv[0], v.uv[1]
                ));
            }
        }
        s.push_str("end\n");
        s
    }

    // ---- 顶点重映射（`RemapVerticesToGlobalBones`）----
    //
    // 起因：用户的 `nahida_themed_autoshotgun` 用 mdlc 编译后游戏内
    // **顶点错乱**。真 studiomdl 裁决见
    // `docs/_probe/oracle_vvd_vertices.js`（4/4）与
    // `docs/_probe/oracle_vertex_space2.js`。

    /// **无参考姿态改写时必须是 no-op** —— 这是「不影响既有产物」的依据。
    ///
    /// ⚠️ 判据必须是**结构信号**（有没有 `$definebone` / `$realignbones` /
    /// `$ikchain` / 显式 `srcRealign`），**不能**是「矩阵是否逐位相同」：
    /// `resolve_bone_pose` 走 `canonical_euler` 规范化、`source_bone_pose`
    /// 原样返回 SMD 弧度，两者**语义相同但浮点不同** —— `canonical_euler`
    /// 会把 `pitch` 规范化成 **`-0.0`**（`rot=[0,0,0]` → bits
    /// `[0, 0x80000000, 0]`），而 `f32::to_bits()` **区分** `0.0`/`-0.0`
    /// ⟹ 逐位比较恒为 false ⟹ `M_k` 恒被算成非单位阵。
    ///
    /// 实测踩过：只看矩阵时 `blend3` / `rr1_r45` / `zb90z` 三个**无任何覆盖**
    /// 的夹具产物变了字节（顶点被挪 1.2e-7 ~ 4.8e-7）。
    ///
    /// ⚠️ **本测试必须用「非平凡旋转」的夹具**（[`SMD_ROT`]）：旋转全 0 时
    /// `canonical_euler` 的 `-0.0` 恰好抵消，**抓不到**上面那个 bug ——
    /// 第一版就是零旋转，变异测试时逃逸了。
    #[test]
    fn remap_vertices_is_noop_without_reference_pose_override() {
        let d = tmpdir("remap-noop");
        write(&d, "a.smd", SMD_ROT);
        let toml = desc_toml("a.smd");
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let mut c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        // 直接调必须返回 `false` —— 结构判据生效的直接证据。
        let moved = remap_vertices_to_reference_pose(&mut c);
        assert!(
            !moved,
            "没有 $definebone / $realignbones / $ikchain 时不该做重映射"
        );

        // 顶点必须**逐位**停在 SMD 的位置。
        // ⚠️ 不能用 `abs() < eps`：这个 bug 的位移只有 1e-7 量级，
        // 任何合理容差都会放过它（第一版就是这么逃逸的）。
        let got: Vec<u32> = c.bodyparts[0].models[0].meshes[0]
            .vertices
            .iter()
            .flat_map(|v| v.pos.iter().map(|x| x.to_bits()))
            .collect();
        let want: Vec<u32> = SMD_ROT_VERTEX_POS
            .iter()
            .flat_map(|p| p.iter().map(|x| x.to_bits()))
            .collect();
        assert_eq!(
            got, want,
            "顶点位置必须逐位不变（no-op）；逐位不等说明重映射被误触发"
        );
    }

    /// **`$definebone` 改写参考姿态时，顶点必须跟着搬到新空间。**
    ///
    /// 夹具形状：SMD 把 `tip` 放在 `z=8`，而三个顶点都画在 **`z=0`**；
    /// `$definebone` 把 `tip` 改到 `z=20`。
    ///
    /// 逐骨骼规则 ⟹ `M = table(tip@20) ∘ src(tip@8)⁻¹ = translate(+12)`
    /// ⟹ 顶点 `z = 0 + 12 = `**`12`**。
    ///
    /// **真 studiomdl 实测正是 12.0000**（`docs/_probe/oracle_remap_unit_fixture.js`，
    /// 该探针把本夹具逐字节复刻成 QC 后跑官方）。
    ///
    /// 注意 `compile()` **内部已经调过**重映射（`compile.rs` 里
    /// `realign_sequence_frames` 之后那一步），所以这里直接断言 `compile()`
    /// 的产物 —— 再调一次会得到 24（这正是下面那条 LOD 测试要钉住的事）。
    #[test]
    fn remap_vertices_moves_geometry_when_definebone_overrides_pose() {
        let d = tmpdir("remap-definebone");
        write(&d, "a.smd", SMD);
        let toml = desc_toml_with_tip_pose("a.smd", 20.0);
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let z = c.bodyparts[0].models[0].meshes[0].vertices[0].pos[2];
        assert!(
            (z - 12.0).abs() < 0.01,
            "顶点应被搬到 z≈12（官方实测值 = SMD 的 0 + (20−8)），实际 {z}"
        );
    }

    /// **多 LOD 的统一池也要搬，且只搬一次。**
    ///
    /// 统一池是**独立**的一份（`build_model_lods` 在 bodypart 循环里跑，
    /// 早于重排定稿），只改 `meshes` 会让 LOD 0 与其它 LOD 处在不同空间。
    /// 反之，若池是从 `mesh.vertices` 派生的就会**重复应用**。
    ///
    /// 这条测试用「再跑一次必须变成 24」反证「第一次恰好加了一次 12」——
    /// 若实现里漏掉了池（或对池做了两次），读数就不是 12/24 这组。
    #[test]
    fn remap_vertices_applies_once_to_mesh_and_lod_pool() {
        let d = tmpdir("remap-lod");
        write(&d, "a.smd", SMD);
        let toml = desc_toml_with_tip_pose("a.smd", 20.0);
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let mut c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        // `compile()` 已搬过一次 ⟹ z = 12。
        let mesh_z = c.bodyparts[0].models[0].meshes[0].vertices[0].pos[2];
        assert!(
            (mesh_z - 12.0).abs() < 0.01,
            "meshes 应搬到 z≈12（恰好一次），实际 {mesh_z}"
        );

        // 造一份「与 meshes 同源」的多 LOD 池（模拟 `build_model_lods`
        // 在重排前克隆出来的那份），再调一次：两者都必须变成 24 —— 相等
        // 即证明「池与 meshes 走的是同一条路径、各恰好一次」。
        let src = c.bodyparts[0].models[0].meshes[0].vertices.clone();
        let tris = c.bodyparts[0].models[0].meshes[0].triangles.clone();
        c.bodyparts[0].models[0].lods = Some(crate::model::ModelLods {
            meshes: vec![crate::lod::MeshLods::single(src, tris)],
            num_lods: 1,
            switch_points: vec![0.0],
            bone_lod_usage: vec![0; c.desc.bones.len()],
            no_facial: vec![false],
        });

        assert!(remap_vertices_to_reference_pose(&mut c));

        let mesh_z2 = c.bodyparts[0].models[0].meshes[0].vertices[0].pos[2];
        let pool_z2 = c.bodyparts[0].models[0].lods.as_ref().unwrap().meshes[0].vertices[0].pos[2];
        assert!(
            (mesh_z2 - 24.0).abs() < 0.01,
            "第二次调用后 meshes 应为 z≈24，实际 {mesh_z2}"
        );
        assert!(
            (pool_z2 - 24.0).abs() < 0.01,
            "LOD 统一池必须与 meshes **同步**（各恰好一次），实际 {pool_z2}"
        );
    }

    /// **多骨骼权重必须按 `w` 加权累加**，不能只取一根骨骼。
    ///
    /// # 为什么单要这一条
    ///
    /// 上面三条测试用的夹具**每个顶点只绑一根骨骼**（SMD 顶点行末尾是
    /// `1 1.0`，只有一组 `links`）。于是「`p[a] += w * t[a]`」里的 `w`
    /// 是 `1.0`，**乘与不乘结果相同** —— 变异测试实测：把 `w *` 删掉，
    /// 三条测试**全部照常通过**（逃逸）。
    ///
    /// 这条测试补上缺口：两个顶点各绑两根骨骼、权重各半，且两根骨骼在
    /// 参考姿态里被搬到**不同的 z**。只有真正按权重累加，结果才落在中间。
    ///
    /// # 夹具的构造
    ///
    /// - SMD：`root` 在原点、`tip` 在 `z=8`；顶点 0 绑 `(root, tip)` 各
    ///   0.5、顶点 1 绑 `(tip, root)` 各 0.5（顺序相反，专门验证「与顺序无关」）。
    /// - TOML：`tip` 的 `position` 改成 `z=20` ⟹ 骨骼表里 `tip` 在 `z=20`，
    ///   而 `root` 仍在原点。
    ///
    /// `M_root` 是单位阵（`root` 在两边都是原点），`M_tip` 把 `z=8` 映射到
    /// `z=20`（平移 +12）。顶点原在 `z=0`：
    ///
    /// ```text
    /// v_new = 0.5 · M_root · 0 + 0.5 · M_tip · 0 = 0.5·0 + 0.5·12 = 6
    /// ```
    ///
    /// 而「只取一根骨骼」的实现会得到 **0**（取 root）或 **12**（取 tip），
    /// 都远离 6 ⟹ 变异必被抓到。
    #[test]
    fn remap_vertices_weights_each_bone_contribution() {
        let d = tmpdir("remap-weights");
        // 顶点行末尾的 `<links> <bone> <weight>`：这里给两组。
        // 顶点 0：绑 root(0) 0.5 + tip(1) 0.5
        // 顶点 1：绑 tip(1) 0.5 + root(0) 0.5（顺序反过来）
        let smd = SMD.replace(
            "  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000",
            "  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 2 0 0.500000 1 0.500000",
        )
        .replace(
            "  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000",
            "  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 2 1 0.500000 0 0.500000",
        );
        // 夹具必须真的被改到了（否则测试会空洞通过）。
        assert!(
            smd.contains("2 0 0.500000 1 0.500000") && smd.contains("2 1 0.500000 0 0.500000"),
            "SMD 夹具的顶点行没被替换成功 —— 本测试会空洞通过，必须修"
        );
        write(&d, "a.smd", &smd);
        let toml = desc_toml_with_tip_pose("a.smd", 20.0);
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let verts = &c.bodyparts[0].models[0].meshes[0].vertices;

        // ⚠️ **按骨骼绑定（而不是位置）挑顶点** —— 位置正是本测试要检验的量，
        // 用位置来定位会让变异改变定位结果本身，失败信息就指向了错误的原因
        // （实测踩过：把权重删掉后报的是「应有 x≈-8 的顶点」，而不是「z 不是 6」）。
        // 夹具里三个顶点只有两个是**双骨骼**的，这个签名不受重映射影响。
        let two_bone: Vec<&crate::model::Vertex> =
            verts.iter().filter(|v| v.bones.len() == 2).collect();
        assert_eq!(
            two_bone.len(),
            2,
            "夹具应有 2 个双骨骼顶点（实际 {}）—— 夹具坏了，不是实现坏了",
            two_bone.len()
        );

        // 加权累加 ⟹ z = 0.5·M_root·z₀ + 0.5·M_tip·z₀。
        // `M_root` 是单位阵（root 两边都在原点）⟹ 贡献 0；
        // `M_tip` 把 z=8 映射到 z=20 ⟹ 对 z=0 的顶点贡献平移 +12。
        // 所以 z 必须是 6。只取一根骨骼的变异会给出 0 或 12。
        for (i, v) in two_bone.iter().enumerate() {
            assert!(
                (v.pos[2] - 6.0).abs() < 0.01,
                "双骨骼顶点[{i}].z 必须是两根骨骼的**加权**结果 6.0\
                 （root 贡献 0、tip 贡献 12），实际 {} —— \
                 若为 0 或 12，说明漏了权重（只取了一根骨骼）",
                v.pos[2]
            );
        }
        // 两组权重的**顺序相反**（v0 是 root 在前、v1 是 tip 在前），
        // 结果必须相同 —— 加权累加与顺序无关。
        assert!(
            (two_bone[0].pos[2] - two_bone[1].pos[2]).abs() < 1e-6,
            "权重顺序不该影响结果：{} vs {}",
            two_bone[0].pos[2],
            two_bone[1].pos[2]
        );
    }

    /// **法线必须跟着一起搬，并归一化。**
    ///
    /// # 为什么单要这一条
    ///
    /// 上面四条测试**都只断言 `pos`** —— 变异测试实测：把法线归一化整段删掉，
    /// 四条测试**全绿**（逃逸）。
    ///
    /// # 夹具为什么必须「两根骨骼、旋转不同」
    ///
    /// 第一版夹具只用**一根**骨骼、且 `M` 是纯旋转 —— 那时法线天然是单位
    /// 长度，**删掉归一化照样通过**（实测逃逸）。要真正钉住归一化，累加结果
    /// 必须**短于 1**：
    ///
    /// - `root` 的参考旋转给 `Rx(90°)` ⟹ `M_root` 把 `(0,0,1)` 转成
    ///   `(0,∓1,0)`（z 分量变 0）；
    /// - `tip` 只改 `position`（旋转 0）⟹ `M_tip` 不动法线，仍是 `(0,0,1)`；
    /// - 顶点绑两者**各 0.5** ⟹ 累加得 `(0,∓0.5,0.5)`，**长度 `0.7071`**。
    ///
    /// 官方在这一步之后调 `VectorNormalize`（`simplify.cpp:5237`）⟹ 产物长度
    /// 必须是 **1.0**。不归一化的变异会留下 `0.7071` ⟹ **被抓到**。
    ///
    /// 同时断言法线**不再等于** SMD 原值 `(0,0,1)` —— 钉住「法线确实跟着
    /// 参考姿态转了」（只搬位置不搬法线的变异会留下 `(0,0,1)`）。
    #[test]
    fn remap_vertices_accumulates_and_normalizes_normals() {
        let d = tmpdir("remap-normals");
        // 顶点绑 root(0) + tip(1) 各 0.5（两个顶点权重顺序相反）。
        let smd = SMD
            .replace(
                "  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 1 1.000000",
                "  1 -8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 2 0 0.500000 1 0.500000",
            )
            .replace(
                "  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000",
                "  1 8.000000 -8.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 2 1 0.500000 0 0.500000",
            );
        assert!(
            smd.contains("2 0 0.500000 1 0.500000") && smd.contains("2 1 0.500000 0 0.500000"),
            "SMD 夹具的顶点行没被替换成功 —— 本测试会空洞通过，必须修"
        );
        write(&d, "a.smd", &smd);

        // 两根骨骼必须把 `(0,0,1)` 转到**不同**方向，加权和才会短于 1。
        //
        // ⚠️ 关键：`tip` 是 `root` 的**子**骨骼，参考旋转会沿层级传播 ——
        // 只给 `root` 一个 `Rx(90°)` 时，`M_root` 与 `M_tip` 的旋转部分
        // **都是** `Rx(90°)`，加权和长度仍是 1 ⟹ 归一化无法被区分
        // （实测踩过：z 恒为 0）。
        //
        // 正确构造：`root` 给 `Rx(+90°)`、`tip` 给 `Rx(−90°)` ⟹
        //   `M_root` 旋转 = `Rx(+90°)`            ⟹ `(0,0,1) → (0,−1,0)`
        //   `M_tip`  旋转 = `Rx(+90°)∘Rx(−90°)` = 单位 ⟹ `(0,0,1) → (0,0,1)`
        // 两者各 0.5 ⟹ `(0,−0.5,0.5)`，**长度 0.7071** ⟹ 归一化可被区分。
        //
        // `rotation` 是 `[roll, pitch, yaw]` ⟹ `Rx(θ)` 写作 `[θ, 0, 0]`。
        let mut toml = desc_toml_with_tip_pose("a.smd", 20.0);
        let root_needle = "[[bones]]\nname = \"root\"\n";
        assert!(
            toml.contains(root_needle),
            "desc_toml 的 root 块变了 —— 本测试会空洞通过，必须修"
        );
        toml = toml.replace(
            root_needle,
            "[[bones]]\nname = \"root\"\nrotation = [90.0, 0.0, 0.0]\n",
        );
        // `desc_toml_with_tip_pose` 给 `tip` 写的是 `rotation = [0.0, 0.0, 0.0]`，
        // 改成立 `Rx(−90°)`。它只出现一次（root 的那行是刚插入的 `[90.0, …]`）。
        let n_zero_rot = toml.matches("rotation = [0.0, 0.0, 0.0]").count();
        assert_eq!(
            n_zero_rot, 1,
            "应恰好有 1 行 `rotation = [0.0, 0.0, 0.0]`（tip 的），实际 {n_zero_rot}"
        );
        toml = toml.replace("rotation = [0.0, 0.0, 0.0]", "rotation = [-90.0, 0.0, 0.0]");
        assert!(
            toml.contains("rotation = [90.0, 0.0, 0.0]")
                && toml.contains("rotation = [-90.0, 0.0, 0.0]"),
            "夹具没被改到 —— 本测试会空洞通过，必须修"
        );

        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let verts = &c.bodyparts[0].models[0].meshes[0].vertices;
        // 按骨骼绑定挑顶点（位置正是被测的量，不能用来定位）。
        let two_bone: Vec<&crate::model::Vertex> =
            verts.iter().filter(|v| v.bones.len() == 2).collect();
        assert_eq!(
            two_bone.len(),
            2,
            "夹具应有 2 个双骨骼顶点（实际 {}）—— 夹具坏了，不是实现坏了",
            two_bone.len()
        );

        for (i, v) in two_bone.iter().enumerate() {
            let n = v.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            // ① 归一化生效：长度必须是 1。累加原始值是 0.7071 ⟹ 不归一化必红。
            assert!(
                (len - 1.0).abs() < 1e-4,
                "顶点[{i}] 法线长度必须是 1（`VectorNormalize`，`simplify.cpp:5237`），\
                 实际 {len} —— 若为 0.7071 说明漏了归一化"
            );
            // ② 法线确实跟着参考姿态转了（不再是 SMD 原值 `(0,0,1)`）。
            let still_z = n[0].abs() < 1e-4 && n[1].abs() < 1e-4 && (n[2] - 1.0).abs() < 1e-4;
            assert!(
                !still_z,
                "顶点[{i}] 法线仍是 (0,0,1) —— 说明法线没跟着重映射（只搬了位置）。\
                 实际 [{}, {}, {}]",
                n[0], n[1], n[2]
            );
            // ③ 两根骨骼各占一半：累加原始值是 `(0, ∓0.5, 0.5)`，
            //    **归一化后**是 `(0, ∓0.7071, 0.7071)`。
            //    所以 z 分量必须是 ±0.7071（= 1/√2）。
            //
            //    ⚠️ 这里能同时区分两种变异：
            //      * 漏归一化 ⟹ z = 0.5（而不是 0.7071）
            //      * 只取一根骨骼 ⟹ z = 0（取 root）或 1（取 tip）
            assert!(
                (n[2].abs() - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3,
                "顶点[{i}] 法线 z 分量应是两根骨骼加权并**归一化**后的 ±0.7071，\
                 实际 {} —— 若为 0.5 说明漏了归一化；若为 0 或 1 说明漏了权重",
                n[2]
            );
        }
    }

    // ---- SMD 参考姿态的下标空间（`resolve_bone_pose`）----
    //
    // 起因：用户的 `v_autoshotgun` 里 20 根骨骼的参考姿态取自**别的骨骼**
    // （`attachment_jiggle_19` 拿到了 `ValveBiped.Camera` 的姿态），
    // 让 `weapon` 的参考位置偏 50.965 单位、附着点世界位置偏 70.9 单位。
    // 真 studiomdl 裁决见 `docs/_probe/probe_pose_index_bug.js`。

    /// **SMD 的 `nodes` 顺序与 `[[bones]]` 顺序不同时，参考姿态必须按名字对齐。**
    ///
    /// # 这个 bug 的形状
    ///
    /// `m.poses` 存的是 **SMD 第 0 帧的原始 `SmdPose`**，其 `bone` 字段是
    /// **SMD `nodes` 段的下标**；而 `resolve_bone_pose(desc, compiled, k)`
    /// 按**骨骼表下标** `k` 去查它。
    ///
    /// 两者只有在「`[[bones]]` 顺序 == SMD `nodes` 顺序」时才一致 ——
    /// 而真实工程里 `$definebone` 的骨骼会被**提到骨骼表前面**
    /// （`v_autoshotgun`：表[2] = `ValveBiped.Camera` 对应 SMD node **84**，
    /// 表[84] = `attachment_jiggle_19` 对应 SMD node **22**）。
    ///
    /// # 夹具
    ///
    /// SMD 的 `nodes` 顺序是 `root, tip, mid`（`mid` 在 z=4）；
    /// `[[bones]]` 里把 **`mid` 提到 `tip` 前面** —— 但**保持 `root` 在首位**
    /// （父骨骼必须先声明，否则 `validate()` 报「找不到父骨骼」——实测踩过）。
    ///
    /// | 骨骼表 | 名字 | 应取的 SMD node | 若按**下标**取会拿到 |
    /// |---|---|---|---|
    /// | 0 | `root` | 0 | node 0（对） |
    /// | 1 | `mid` | **2** | node 1 = `tip` 的姿态（z=8，**错**） |
    /// | 2 | `tip` | **1** | node 2 = `mid` 的姿态（z=4，**错**） |
    #[test]
    fn smd_reference_pose_is_matched_by_name_not_node_index() {
        let d = tmpdir("pose-by-name");
        // SMD 的 nodes 顺序：root(0), tip(1), mid(2)；mid 在 z=4。
        let smd = SMD
            .replace(
                "nodes\n  0 \"root\" -1\n  1 \"tip\" 0\nend",
                "nodes\n  0 \"root\" -1\n  1 \"tip\" 0\n  2 \"mid\" 0\nend",
            )
            .replace(
                "    1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\nend",
                "    1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\n    \
             2 0.000000 0.000000 4.000000 0.000000 0.000000 0.000000\nend",
            );
        assert!(
            smd.contains("\"mid\" 0") && smd.contains("2 0.000000 0.000000 4.000000"),
            "SMD 夹具没被改到 —— 本测试会空洞通过，必须修"
        );
        write(&d, "a.smd", &smd);

        // `[[bones]]`：root 先（父必须先声明），然后 **mid 在 tip 之前**。
        let toml = desc_toml("a.smd").replace(
            "[[bones]]\nname = \"tip\"\nparent = \"root\"\n",
            "[[bones]]\nname = \"mid\"\nparent = \"root\"\n\n\
             [[bones]]\nname = \"tip\"\nparent = \"root\"\n",
        );
        assert!(
            toml.find("name = \"mid\"").unwrap() < toml.find("name = \"tip\"").unwrap(),
            "夹具没被改到（`mid` 应在 `tip` 之前）—— 本测试会空洞通过，必须修"
        );

        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let idx = c.desc.bone_index();
        // 按**名字**取姿态 —— 表下标与 SMD node 下标已经不同。
        for (name, want_z) in [("root", 0.0f32), ("mid", 4.0f32), ("tip", 8.0f32)] {
            let k = idx[name] as usize;
            let (pos, _) = resolve_bone_pose(&c.desc, &c, k);
            assert!(
                (pos[2] - want_z).abs() < 0.01,
                "`{name}`（骨骼表下标 {k}）的参考姿态应来自 SMD 里**同名**的 node\
                 （z={want_z}），实际 z={}。若拿到了别的 z，说明按 node 下标取了\
                 **另一根骨骼**的姿态（下标空间不一致）",
                pos[2]
            );
        }
    }

    /// **`[[bones]]` 顺序与 SMD 顺序一致时（绝大多数情况）结果不变。**
    ///
    /// 这是「按名字对齐」不破坏既有行为」的依据 —— parity 的 101 个夹具
    /// 全部属于这一类（实测改动前后**逐字节相同**）。
    #[test]
    fn smd_reference_pose_by_name_matches_by_index_when_orders_agree() {
        let d = tmpdir("pose-order-agree");
        write(&d, "a.smd", SMD);
        let toml = desc_toml("a.smd");
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect("夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let idx = c.desc.bone_index();
        // `desc_toml` 的 `[[bones]]` 顺序与 `SMD` 的 `nodes` 顺序一致
        // （root=0, tip=1）⟹ 按名字与按下标应当给出**同一**结果。
        for (name, want_z) in [("root", 0.0f32), ("tip", 8.0f32)] {
            let k = idx[name] as usize;
            let (pos, _) = resolve_bone_pose(&c.desc, &c, k);
            assert!(
                (pos[2] - want_z).abs() < 0.01,
                "`{name}` 的 z 应为 {want_z}，实际 {}",
                pos[2]
            );
        }
    }

    // ---- 帧内骨骼不全的 SMD（官方静默接受）----

    /// **`nodes` 列了 N 根、`skeleton` 只写 1 根的 SMD 必须能编译。**
    ///
    /// # 官方语义（`Grab_Animation`，`studiomdl.cpp:1065-1135`）
    ///
    /// ```cpp
    /// psource->rawanim[t] = (s_bone_t *)kalloc( 1, size );   // kalloc = calloc ⟹ 零填充
    /// if (t > 0 && psource->rawanim[t-1]) {                  // 再逐骨骼拷贝上一帧
    ///     for (int j = 0; j < psource->numbones; j++) { VectorCopy(...); }
    /// }
    /// // 最后用本帧的骨骼行覆盖
    /// ```
    ///
    /// 官方**从不检查「每根骨骼都有数据」** —— 本帧没写的骨骼保留
    /// 「上一帧的值」（第 0 帧则是 `(0,0,0)` / 单位旋转）。
    ///
    /// # 这条曾经写错过
    ///
    /// 早先要求「每帧每根骨骼都必须有姿态行」，否则报
    /// 「缺少部分骨骼的姿态（1 / N 根有数据）」并**拒绝编译**。
    ///
    /// **真实影响**（`vm_test_group`）：Crowbar 反编译出的
    /// `*_corrective_animation.smd` 只有**一行**骨架数据（bone 0），
    /// 而 `nodes` 段列了全部 63~65 根 —— 真 `studiomdl.exe` 静默接受
    /// （编译日志零告警），mdlc 却 **6/6 编译失败**。
    ///
    /// 对照证据：`docs/_probe/cmp_vm_test_group.js`（修后 **6/6 一致**，
    /// 顶点 miss=0、最大距离 0.0000）。
    #[test]
    fn smd_frame_with_partial_bones_is_accepted() {
        let d = tmpdir("partial-frame");
        // `nodes` 有 root + tip，`skeleton` 只有 root 一行。
        // `tip` 在描述里声明了 `position`，所以它有兜底姿态。
        let smd = SMD.replace(
            "skeleton\n  time 0\n    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n    \
             1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\nend",
            "skeleton\n  time 0\n    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\nend",
        );
        assert!(
            smd.contains("0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\nend"),
            "SMD 夹具没被改到（tip 的骨架行应已删除）—— 本测试会空洞通过，必须修"
        );
        write(&d, "a.smd", &smd);

        // `desc_toml` 给 `tip` 写了 `position`（= `$definebone` 语义），
        // 所以「SMD 里没有 tip」不会触发「无法确定参考姿态」那条错误。
        let toml = desc_toml_with_tip_pose("a.smd", 8.0);
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect(
            "帧内骨骼不全的 SMD 必须能编译（官方 `Grab_Animation` 用 calloc \
             零填充、从不检查每根骨骼都有数据）",
        );
        let _ = std::fs::remove_dir_all(&d);

        // 关键判据：`tip` 的参考姿态来自 `$definebone` 的兜底值（z=8），
        // 不是被「缺失」判成错误、也不是被静默置零。
        let idx = c.desc.bone_index();
        let (tip_pos, _) = resolve_bone_pose(&c.desc, &c, idx["tip"] as usize);
        assert!(
            (tip_pos[2] - 8.0).abs() < 0.01,
            "`tip` 的兜底姿态应来自 `$definebone`（z=8），实际 {}",
            tip_pos[2]
        );
    }

    /// **缺骨骼的帧继承上一帧的值**（不是恒为零）。
    ///
    /// 官方 `Grab_Animation` 的注释就是 `// duplicate previous frames keys`。
    /// 这条钉住「继承」而不是「置零」—— 两者在第 0 帧无法区分，
    /// 必须有**两帧**才能分辨。
    #[test]
    fn smd_partial_frame_inherits_previous_frame() {
        let d = tmpdir("partial-inherit");
        // 第 0 帧：root 与 tip 都有数据（tip 在 z=8）。
        // 第 1 帧：**只有** root 一行 ⟹ tip 应继承第 0 帧的 z=8。
        let smd = SMD.replace(
            "skeleton\n  time 0\n    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n    \
             1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\nend",
            "skeleton\n  time 0\n    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n    \
             1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\n  \
             time 1\n    0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\nend",
        );
        assert!(
            smd.contains("time 1"),
            "SMD 夹具没被改到（应有两帧）—— 本测试会空洞通过，必须修"
        );
        write(&d, "a.smd", &smd);

        // `tip` 不给 `position`，纯靠 SMD —— 这样「继承」与「置零」才可分辨。
        let toml = desc_toml("a.smd");
        let desc: ModelDesc = toml::from_str(&toml).unwrap();
        let c = compile(&desc, &d).expect("两帧夹具应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let idx = c.desc.bone_index();
        let tip = idx["tip"] as usize;
        let frames = &c.bodyparts[0].models[0].poses; // 参考姿态（第 0 帧）
        let _ = frames;
        // 直接查 `tip` 的参考姿态（来自 SMD 第 0 帧）—— z=8。
        let (tip_pos, _) = resolve_bone_pose(&c.desc, &c, tip);
        assert!(
            (tip_pos[2] - 8.0).abs() < 0.01,
            "`tip` 的参考姿态应来自 SMD 第 0 帧（z=8），实际 {}",
            tip_pos[2]
        );
    }

    // ------------------------------------------------------------------
    // R23：`$sequence` 块里的 `ikrule` 必须落到**第一格动画**上
    // ------------------------------------------------------------------

    /// 一份能跑通「序列级 ikrule」的最小 SMD（3 根骨骼、2 帧、1 个三角形）。
    const IKR23_SMD: &str = r#"version 1
nodes
0 "root" -1
1 "hip" 0
2 "ankle" 1
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 0.000000 0.000000 10.000000 0.000000 0.000000 0.000000
2 0.000000 0.000000 20.000000 0.000000 0.000000 0.000000
time 1
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 0.000000 0.000000 10.000000 0.000000 0.000000 0.000000
2 0.000000 0.000000 22.000000 0.000000 0.000000 0.000000
end
triangles
mat
0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.0 0.0 1 0 1.000000
0 1.000000 0.000000 0.000000 0.000000 0.000000 1.000000 1.0 0.0 1 0 1.000000
0 0.000000 1.000000 0.000000 0.000000 0.000000 1.000000 0.0 1.0 1 0 1.000000
end
"#;

    /// 写三份 SMD（供 3 格 blend 使用）。
    ///
    /// ⚠️ **每份都要落盘** —— `parse_qc_str` 在收尾时会真的去磁盘读
    /// `$animation` / `$model` 引用的 SMD，漏一个就整条 QC 解析失败。
    fn ikr23_dir(tag: &str) -> PathBuf {
        let d = tmpdir(tag);
        write(&d, "a.smd", IKR23_SMD);
        write(&d, "b.smd", IKR23_SMD);
        write(&d, "c.smd", IKR23_SMD);
        d
    }

    /// **`$sequence` 块里的 `ikrule` 属于该序列引用的第一格动画。**
    ///
    /// # 官方依据（`studiomdl.cpp:2944`）
    ///
    /// ```c
    /// else if ((numblends || isAppend) && ParseAnimationToken( animations[0] ))
    /// ```
    ///
    /// `$sequence` 块里的**动画选项**（`ikrule` / `subtract` / `weightlist` /
    /// `numframes`…）全部转交给 `ParseAnimationToken(animations[0])` ——
    /// `animations[0]` 是**本序列引用的第一个动画**。而 `CMD_IKRULE` 把规则
    /// 记进 `panim->cmds[]`（`studiomdl.cpp:1931`），`ProcessIKRules`
    /// 之后只遍历**动画池** `g_panimation[]`（`simplify.cpp:5828`）。
    ///
    /// ⟹ 规则最终落在**第一格动画**的 `ikrule[]` 上，序列对象本身
    /// **不存**规则（`write.cpp` 只从 `panim->ikrule[]` 取数据）。
    ///
    /// # 实测症状（用户的真实工程，37 条动画里 17 条错）
    ///
    /// mdlc 修前把规则留在 `sequences[..].ik_rules`，而
    /// `build_ik_rules` 只读 `animations[..].ik_rules` ⟹ 规则**从未写出**：
    ///
    /// | 动画 | QC 所在序列 | mdlc 修前 | NekoMDL |
    /// |---|---|---|---|
    /// | `a_run`（`idle` 的第 1 格） | `idle` | `REL:12x` | `touch:0,1 REL:10x` |
    /// | `al_melee` | `melee_layer` | **0 条** | `REL:0 touch:1` |
    ///
    /// 两者恰好就是用户报的两个症状：「行走（`a_run`）时手部扭曲」
    /// 与「`melee_layer` 动画时手部扭曲」。
    ///
    /// ⚠️ **`a_run` 是 `idle` 的「第 1 格」，但 `idle` 的 blends 是
    /// `"a_run" "a_idle" "a_run"`** —— 第一格是 `a_run`，所以规则落在
    /// `a_run` 上。这正是「必须按 blends[0] 而不是按名字猜」的证据。
    #[test]
    fn sequence_ik_rules_attach_to_first_cell() {
        let d = ikr23_dir("r23-first-cell");
        // 3 格 blend，第一格是 `a`（不是名字最靠前的那个）。
        //
        // ⚠️ **必须写 `blend` 与 `blendwidth` 两个关键字。**
        //
        // 只写 `blendwidth 3`（`groupsize[0] = 3 > 1`）而不写 `blend` 时，
        // 官方 `CalcPoseParameters`（`simplify.cpp:5461-5569`）会因为
        // `paramattachment[0]` 是 `memset` 残留的 **0**（`≠ -1`）而误入
        // calc 分支，`paramcontrol[0]` 同样是 0 ⟹ 每格算出 `0.0`
        // ⟹ `|paramstart − paramend| = 0 < 0.01` ⟹
        // **`ERROR: calcblend failed in multi`**。
        //
        // 实测（真 `studiomdl.exe`，3 格 + `blendwidth 3` 无 `blend`）：
        // ```text
        // ERROR: calcblend failed in multi
        // ERROR: Aborted Processing on 'r23.mdl'
        // ```
        // 所以本夹具补上 `blend "px" 1 -1` + `$poseparameter`，
        // 才是官方能编译的形态。
        let qc = "\
$modelname \"r23.mdl\"\n\
$ikchain \"leg\" \"ankle\"\n\
$poseparameter \"px\" -1 1\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"multi\" \"a\" \"b\" \"c\" {\n\
blend \"px\" 1 -1\n\
blendwidth 3\n\
ikrule \"leg\" touch \"hip\"\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let by_name = |n: &str| {
            c.animations
                .iter()
                .find(|a| a.name == n)
                .unwrap_or_else(|| panic!("动画 {n} 应存在"))
        };
        // 先证明夹具非空 —— 否则下面的断言会**空洞通过**。
        assert_eq!(c.animations.len(), 3, "夹具应有 3 条动画（a/b/c）");

        assert_eq!(
            by_name("a").ik_rules.len(),
            1,
            "`$sequence` 的 ikrule 必须落到**第一格**动画 `a` 上"
        );
        assert_eq!(by_name("a").ik_rules[0].chain, "leg");
        assert_eq!(
            by_name("b").ik_rules.len(),
            0,
            "第二格动画 `b` 不该拿到规则"
        );
        assert_eq!(
            by_name("c").ik_rules.len(),
            0,
            "第三格动画 `c` 不该拿到规则"
        );
    }

    /// **单动画序列**：`$sequence` 的 `ikrule` 落在它引用的那条动画上。
    ///
    /// 这里刻意让序列**复用**已声明的 `$animation`（而不是建隐含动画），
    /// 走的是 `compile.rs` 里 `reused == true` 的那条分支 ——
    /// 与 [`sequence_ik_rules_attach_to_first_cell`] 覆盖的 blend 分支**不同**。
    ///
    /// ⚠️ **这正是 `melee_layer` 的形态**：`$sequence "melee_layer" "al_melee" …`
    /// 引用的 `al_melee` 是**已声明**的 `$animation`，且它的
    /// `$animation` 块里**没有** ikrule —— 规则只写在 `$sequence` 块里。
    /// mdlc 修前这条路径**一条规则都不写**（实测 `al_melee` 0 条 vs
    /// NekoMDL 2 条）。
    #[test]
    fn sequence_ik_rules_attach_to_reused_declared_animation() {
        let d = ikr23_dir("r23-reused");
        let qc = "\
$modelname \"r23b.mdl\"\n\
$ikchain \"leg\" \"ankle\"\n\
$animation \"decl\" \"a.smd\" fps 30\n\
$sequence \"layer\" \"decl\" {\n\
ikrule \"leg\" release\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(c.animations.len(), 1, "夹具应恰好 1 条动画（复用 `decl`）");
        assert_eq!(
            c.animations[0].ik_rules.len(),
            1,
            "`$sequence` 的 ikrule 必须落到被复用的 `decl` 上（`al_melee` 的形态）"
        );
        assert_eq!(
            c.animations[0].ik_rules[0].kind,
            crate::model::IkRuleType::Release,
            "规则类型应原样保留"
        );
    }

    // =====================================================================
    // calcblend（官方 `CalcPoseParameters`，`simplify.cpp:5448-5596`）
    //
    // 全部判据都用真 `studiomdl.exe` 实测过（见每条的文档）。
    // =====================================================================

    /// 一份「附着点在 `tip` 上、三个格子朝向各不相同」的最小 SMD。
    ///
    /// `rot` 是 `tip` 的 Z 旋转（弧度）—— 三格分别给 0 / 0.3 / 0.6，
    /// 于是 `calcblend ... ZR` 的逐格取值必然是 `0 / 17.19 / 34.38` 度。
    fn cb_smd(rot: &str) -> String {
        format!(
            "version 1\n\
             nodes\n\
             0 \"root\" -1\n\
             1 \"tip\" 0\n\
             end\n\
             skeleton\n\
             time 0\n\
             0 0 0 0 0 0 0\n\
             1 0 0 8 0 0 {rot}\n\
             time 1\n\
             0 0 0 0 0 0 0\n\
             1 0 0 8 0 0 {rot}\n\
             end\n\
             triangles\n\
             mat\n\
             0 -8 -8 0 0 0 1 0 0 1 0 1.000000\n\
             0 8 -8 0 0 0 1 1 0 1 0 1.000000\n\
             0 0 8 0 0 0 1 0.5 1 1 0 1.000000\n\
             end\n"
        )
    }

    /// 三格 `calcblend` 夹具（`a`/`b`/`c` 的 `tip` Z 旋转 = 0 / 0.3 / 0.6 rad）。
    fn cb_dir(tag: &str) -> PathBuf {
        let d = tmpdir(tag);
        write(&d, "a.smd", &cb_smd("0.000000"));
        write(&d, "b.smd", &cb_smd("0.300000"));
        write(&d, "c.smd", &cb_smd("0.600000"));
        d
    }

    /// **`calcblend` 的逐格取值必须是「附着点相对位姿」的实测值。**
    ///
    /// # 官方（`simplify.cpp:5487-5569`）
    ///
    /// ```text
    /// mid    = CalcBoneTransforms(paramanim, 0)[att.bone] ∘ att.local
    /// invMid = inverse(mid)
    /// for m:  rel = CalcBoneTransforms(cell(m), paramcompanim, 0)[att.bone] ∘ att.local
    ///         v   = CalcPoseParameterValue(paramcontrol, angles(invMid ∘ rel), pos(...))
    /// ```
    ///
    /// # 实测判据（真 `studiomdl.exe`，3 格 + `calcblend "px" "att" ZR`）
    ///
    /// ```text
    /// paramstart = 0.0000    paramend = 114.5916
    /// posekey    = [0.0000, 57.2958, 114.5916, 0.0000]
    /// ```
    ///
    /// `57.2958 = 1 rad`、`114.5916 = 2 rad` 的度数 —— 正是三格的 Z 旋转
    /// `0 / 1 / 2 rad` 换算成度。**mdlc 必须逐值复现**（这里用 1e-4 容差
    /// 吸收 `RAD2DEG` 的浮点差；实测两侧打印到 4 位小数完全相同）。
    #[test]
    fn calcblend_measures_attachment_pose_per_cell() {
        let d = cb_dir("cb-zr");
        let qc = "\
$modelname \"cb.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
calcblend \"px\" \"att\" ZR\n\
blendwidth 3\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let seq = &c.sequences[0];
        // 先证明夹具非空 —— 否则下面的断言会**空洞通过**。
        assert_eq!(seq.cells.len(), 3, "夹具应是 3 格");
        assert_eq!(seq.calc_axes.len(), 1, "应有 1 根 calc 轴");
        let p = seq.blend_params[0].as_ref().expect("轴 0 应被填上");
        assert_eq!(p.parameter_index, 0, "paramindex[0] 应指向姿势参数 0");
        assert_eq!(p.keys.len(), 3, "应有 3 个逐格取值");
        // ⚠️ 三格旋转 0 / 0.3 / 0.6 rad ⟹ 0 / 17.1887 / 34.3775 度。
        for (i, want) in [0.0f32, 17.188_73, 34.377_47].iter().enumerate() {
            assert!(
                (p.keys[i] - want).abs() < 1e-3,
                "posekey[{i}] = {} 应约为 {want}",
                p.keys[i]
            );
        }
        assert!((p.start - 0.0).abs() < 1e-4, "paramstart = {}", p.start);
        assert!(
            (p.end - 34.377_47).abs() < 1e-3,
            "paramend = {} 应约为 34.3775",
            p.end
        );
    }

    /// **`X` / `ZR` 两个控制轴都要认**（`CalcPoseParameterValue` 的 6 个 case）。
    ///
    /// 官方只认 `X/Y/Z`（位置分量）与 `XR/YR/ZR`（角度分量，**度**），
    /// 其余一律返回 `0.0`。这里让三格在 **X 位置**上变化，验证 `X` 走的是
    /// 位置而不是角度 —— 若把 `X` 与 `XR` 弄混，得到的是 0（因为无旋转）。
    #[test]
    fn calcblend_supports_position_and_rotation_controls() {
        let d = tmpdir("cb-x");
        // 三格的 tip **X 位置** = 0 / 5 / 10（无旋转）。
        for (name, x) in [
            ("a.smd", "0.000000"),
            ("b.smd", "5.000000"),
            ("c.smd", "10.000000"),
        ] {
            write(
                &d,
                name,
                &format!(
                    "version 1\n\
                     nodes\n\
                     0 \"root\" -1\n\
                     1 \"tip\" 0\n\
                     end\n\
                     skeleton\n\
                     time 0\n\
                     0 0 0 0 0 0 0\n\
                     1 {x} 0 8 0 0 0\n\
                     time 1\n\
                     0 0 0 0 0 0 0\n\
                     1 {x} 0 8 0 0 0\n\
                     end\n\
                     triangles\n\
                     mat\n\
                     0 -8 -8 0 0 0 1 0 0 1 0 1.000000\n\
                     0 8 -8 0 0 0 1 1 0 1 0 1.000000\n\
                     0 0 8 0 0 0 1 0.5 1 1 0 1.000000\n\
                     end\n"
                ),
            );
        }
        let qc = "\
$modelname \"cbx.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
calcblend \"px\" \"att\" X\n\
blendwidth 3\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let p = c.sequences[0].blend_params[0]
            .as_ref()
            .expect("轴 0 应被填上");
        // 实测官方：`posekey = [0.0000, 5.0000, 10.0000, 0.0000]`。
        assert_eq!(p.keys.len(), 3, "应有 3 个逐格取值");
        for (i, want) in [0.0f32, 5.0, 10.0].iter().enumerate() {
            assert!(
                (p.keys[i] - want).abs() < 1e-4,
                "posekey[{i}] = {} 应约为 {want}（控制轴 X 取的是**位置**）",
                p.keys[i]
            );
        }
    }

    /// **只写 `blendwidth` 不写 `blend` ⟹ 必须报 `calcblend failed`。**
    ///
    /// # 这是本轮最重要的判据
    ///
    /// 官方 `Cmd_Sequence`（`studiomdl.cpp:2624`）先 `memset(pseq, 0, …)`，
    /// 然后**只**初始化 `paramindex` / `groupsize` / `fadein` / `fadeout`
    /// （`:2629-2640`）—— `paramattachment` 与 `paramcontrol`
    /// **都不在初始化列表里**。
    ///
    /// 只有 `blend` 分支写 `paramattachment[i] = -1`（`:2742`）。
    /// 于是「有 `blendwidth`、没有 `blend`」时：
    ///
    /// ```text
    /// groupsize[0] = 3 > 1                      ⟹ 进入外层 if
    /// paramattachment[0] == 0  (memset) != -1   ⟹ 进入 **calc 分支**
    /// paramcontrol[0]    == 0  (memset)
    ///   ⟹ CalcPoseParameterValue(0, …) 不匹配任何 case ⟹ 返回 0.0
    ///   ⟹ paramstart == paramend == 0.0
    ///   ⟹ fabs(差) < 0.01 ⟹ MdlError("calcblend failed in <序列>")
    /// ```
    ///
    /// **真 `studiomdl.exe` 实测**（3 格 + `blendwidth 3`，无 `blend`）：
    ///
    /// ```text
    /// ERROR: calcblend failed in multi
    /// ERROR: Aborted Processing on 'r23.mdl'
    /// ```
    ///
    /// 用户的真实工程正是踩了这条：Crowbar 反编译出来的 QC 把
    /// `blend "move_x" 1 -1` **注释掉了**，只留下 `blendwidth 3`。
    /// mdlc 修前**静默编过**，产出 `paramindex = [-1,-1]`、`posekey` 全 0
    /// 的模型 —— 引擎侧 `Studio_LocalPoseParameter` 见 `-1` 直接
    /// `flSetting = 0; index = 0`，姿势参数**完全失效**。
    #[test]
    fn blendwidth_without_blend_reports_calcblend_failed() {
        let d = cb_dir("cb-noblend");
        let qc = "\
$modelname \"cb2.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"multi\" \"a\" \"b\" \"c\" {\n\
blendwidth 3\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        // ⚠️ 文案在 `compile()` 里就渲染好了，所以语言必须在那一刻生效。
        // 本条是 locale 竞态的**受害者**：`calcblend failed` 是英文源文的记号，
        // 但别的测试把全局 locale 切成 zh-CN 之后它就变成中文了。
        let err = crate::test_locale::with_english(|| {
            compile(&desc, &d).expect_err("官方拒绝的 QC，mdlc 也必须拒绝")
        });
        let _ = std::fs::remove_dir_all(&d);

        assert!(
            err.iter().any(|e| e.message().contains("calcblend failed")),
            "错误信息应含 `calcblend failed`（与官方同一句），实际：{err:?}"
        );
        // 官方把**序列名**写进消息（`MdlError("calcblend failed in %s")`）。
        assert!(
            err.iter().any(|e| e.message().contains("multi")),
            "错误信息应含序列名 `multi`，实际：{err:?}"
        );
    }

    /// **单动画序列不受影响**（回归护栏）。
    ///
    /// `groupsize` 是 1×1 ⟹ 官方 `CalcPoseParameters` 的
    /// `groupsize[iPose] > 1` **不成立** ⟹ 一根轴都不遍历 ⟹ 不报错。
    ///
    /// 实测官方：单动画（`u1_single_anim`）**PASS**。
    #[test]
    fn single_animation_sequence_skips_calcblend_check() {
        let d = cb_dir("cb-single");
        let qc = "\
$modelname \"cb3.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$animation \"a\" \"a.smd\" fps 30\n\
$sequence \"idle\" \"a\" {\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("单动画序列必须能编译（groupsize = 1×1）");
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(
            c.sequences[0].cells.len(),
            1,
            "单动画序列的 `cells` 恰好 1 项（隐含动画 `@idle`）"
        );
        assert!(
            c.sequences[0].calc_axes.is_empty(),
            "groupsize = 1×1 ⟹ 不应有 calc 轴"
        );
        assert_eq!(
            c.sequences[0].blend_params[0], None,
            "单动画序列不该有参数轴"
        );
    }

    /// **纯 `blend` 轴不能误入 calc 分支**（回归护栏）。
    ///
    /// 官方 `blend` 会把 `paramattachment[i] = -1`（`studiomdl.cpp:2742`），
    /// 于是走**线性插值**那条 `else`（`simplify.cpp:5576-5591`），
    /// 逐格取值 = `start*(1-f) + end*f`。
    ///
    /// 若把「声明了 `blend`」也当成 calc 轴，就会去算附着点相对位姿 ——
    /// 而纯 `blend` 的 `paramcontrol` 是 0 ⟹ 每格 0 ⟹ 误报
    /// `calcblend failed`。**这是最容易犯的过度泛化。**
    #[test]
    fn pure_blend_axis_uses_linear_interpolation_not_calc() {
        let d = cb_dir("cb-pure");
        let qc = "\
$modelname \"cb4.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
blend \"px\" 1 -1\n\
blendwidth 3\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("纯 blend 必须能编译");
        let _ = std::fs::remove_dir_all(&d);

        assert!(
            c.sequences[0].calc_axes.is_empty(),
            "纯 `blend` 轴不该进 calc 分支"
        );
        let p = c.sequences[0].blend_params[0]
            .as_ref()
            .expect("轴 0 应被填上");
        // `start=1, end=-1, groupsize=3` ⟹ `[1, 0, -1]`。
        assert_eq!(p.keys, vec![1.0, 0.0, -1.0], "线性插值应为 [1, 0, -1]");
        assert_eq!(p.start, 1.0);
        assert_eq!(p.end, -1.0);
    }

    /// **`blend` 与 `calcblend` 共用同一套「按出现顺序占槽」规则。**
    ///
    /// 官方两个关键字的槽位算法**逐字相同**（`studiomdl.cpp:2734-2737`
    /// 与 `:2756-2759`）：
    ///
    /// ```c
    /// i = 0;
    /// if (pseq->paramindex[0] != -1) { i = 1; }
    /// ```
    ///
    /// # 判据必须让**两根轴的 `groupsize` 都 > 1**
    ///
    /// 3 格 + `blendwidth 3` 时 `groupsize = [3, 1]`，轴 1 的 `groupsize[1]`
    /// 是 1 ⟹ 官方 `CalcPoseParameters` 的外层 `if` 不成立 ⟹ 轴 1
    /// **根本不参与**（`paramstart[1]`/`paramend[1]` 保持 `blend` 写的
    /// QC 值）。那种夹具**测不出槽位顺序**。
    ///
    /// 所以这里用 **2×2 网格**（`groupsize = [2, 2]`），两根轴都真的被遍历。
    ///
    /// # ⚠️ 2×2 时**两根轴都必须声明**
    ///
    /// 只写 `calcblend`（轴 1 空着）时，轴 1 的 `paramattachment` 是
    /// `memset` 残留的 **0**、`paramcontrol` 也是 0 ⟹ 每格 0 ⟹
    /// **官方照样报 `calcblend failed`**。
    /// 实测（真 `studiomdl.exe`，2×2 只写 `calcblend`）：
    /// `ERROR: calcblend failed in idle`。
    ///
    /// # 实测官方（2×2，`calcblend` 占槽 0 + `blend` 占槽 1）
    ///
    /// 本夹具的 `tip` Z 旋转是 **0 / 0.3 / 0.6 / 0** 弧度
    /// （见 [`cb_smd`]），所以：
    ///
    /// ```text
    /// 轴 0（calc）遍历**第 1 列**（中点 `2/2 = 1`）⟹ 格子 `c`(0.6 rad) / `d`(0)
    ///   ⟹ paramstart[0] = 34.3775°、paramend[0] = 0°
    /// 轴 1（纯 blend，线性 1 → -1）⟹ paramstart[1] = 1、paramend[1] = -1
    /// ```
    ///
    /// **若槽位顺序弄反**，轴 0 会拿到线性的 `1/-1`、轴 1 拿到 calc 值 ——
    /// 两个断言都会红。
    #[test]
    fn blend_and_calcblend_share_the_axis_slot_order() {
        let d = cb_dir("cb-order1");
        write(&d, "d.smd", &cb_smd("0.000000")); // 第 4 格 = 与 `a` 相同
        let qc = "\
$modelname \"cb5.mdl\"\n\
$poseparameter \"pa\" -1 1\n\
$poseparameter \"pb\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$animation \"d\" \"d.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" \"d\" {\n\
calcblend \"pa\" \"att\" ZR\n\
blend \"pb\" 1 -1\n\
blendwidth 2\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let seq = &c.sequences[0];
        assert_eq!(seq.cells.len(), 4, "夹具应是 2×2 = 4 格");
        assert_eq!(seq.calc_axes.len(), 1, "应恰好 1 根 calc 轴");
        assert_eq!(seq.calc_axes[0].axis, 0, "calcblend 先写 ⟹ 占槽 0");
        let p0 = seq.blend_params[0].as_ref().expect("轴 0");
        let p1 = seq.blend_params[1].as_ref().expect("轴 1");
        // 轴 0 = calc：第 1 列的 `c`(0.6 rad = 34.3775°) → `d`(0)。
        assert_eq!(p0.keys.len(), 2, "轴 0 应有 2 个逐格取值");
        assert!(
            (p0.start - 34.377_47).abs() < 1e-3,
            "paramstart[0] = {} 应约为 34.3775（第 1 列的 `c`，0.6 rad）",
            p0.start
        );
        assert!((p0.end - 0.0).abs() < 1e-4, "paramend[0] = {}", p0.end);
        // 轴 1 = 纯 blend：线性插值 `1 → -1`。
        assert_eq!((p1.start, p1.end), (1.0, -1.0), "槽 1 应是 QC 的线性值");
        assert_eq!(p1.keys, vec![1.0, -1.0], "轴 1 的逐格取值应是 [1, -1]");
    }

    /// **2×2 网格上「另一根轴取中点」**（`simplify.cpp:5518-5521`）。
    ///
    /// `blendcenter` 没写时官方取 `m[1-iPose] = groupsize[1-iPose] / 2`
    /// —— **整数除法**，所以 2 格时取 `1`（不是 0、也不是 0.5）。
    ///
    /// # 判据
    ///
    /// 轴 0 必须遍历**第 1 列**（`c`/`d`）而不是第 0 列（`a`/`b`）：
    ///
    /// | 假设 | `paramstart[0]` |
    /// |---|---|
    /// | 取中点（**官方**） | `34.3775`（`c` = 0.6 rad） |
    /// | 误取第 0 列 | `0`（`a`） |
    ///
    /// # ⚠️ 两根轴都要声明
    ///
    /// 2×2 只写 `calcblend` 时轴 1 是 `memset` 路径（控制轴 0）⟹
    /// **官方直接报 `calcblend failed`**（实测）。所以夹具必须同时给
    /// 轴 1 一个 `blend`，才走得到「取中点」这段逻辑。
    #[test]
    fn calcblend_other_axis_uses_integer_midpoint() {
        let d = cb_dir("cb-mid");
        write(&d, "d.smd", &cb_smd("0.000000"));
        let qc = "\
$modelname \"cb9.mdl\"\n\
$poseparameter \"pa\" -1 1\n\
$poseparameter \"pb\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$animation \"d\" \"d.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" \"d\" {\n\
calcblend \"pa\" \"att\" ZR\n\
blend \"pb\" 1 -1\n\
blendwidth 2\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let p = c.sequences[0].blend_params[0].as_ref().expect("轴 0");
        assert_eq!(p.keys.len(), 2, "轴 0 应有 2 个逐格取值");
        assert!(
            (p.start - 34.377_47).abs() < 1e-3,
            "paramstart = {} 应约为 34.3775 —— 证明取的是**第 1 列**（中点 `2/2 = 1`）",
            p.start
        );
        assert!(
            (p.end - 0.0).abs() < 1e-4,
            "paramend = {} 应是第 1 列末格 `d` 的 0",
            p.end
        );
    }

    /// **未知的 `calcblend` 附着点必须报错**（官方 `TokenError`）。
    ///
    /// 官方 `studiomdl.cpp:2766-2770`：
    /// ```c
    /// pseq->paramattachment[i] = LookupAttachment( token );
    /// if (pseq->paramattachment[i] == -1) TokenError( "Unknown calcblend attachment \"%s\"\n", token );
    /// ```
    #[test]
    fn unknown_calcblend_attachment_is_rejected() {
        let d = cb_dir("cb-badatt");
        let qc = "\
$modelname \"cb7.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
calcblend \"px\" \"nosuchatt\" ZR\n\
blendwidth 3\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let err = compile(&desc, &d).expect_err("未知附着点必须被拒绝");
        let _ = std::fs::remove_dir_all(&d);

        assert!(
            err.iter().any(|e| e.message().contains("nosuchatt")),
            "错误信息应含附着点名，实际：{err:?}"
        );
    }

    /// **`blendref` / `blendcomp` / `blendcenter` 必须能解析且被消费。**
    ///
    /// 官方三者都是 `LookupAnimation`（`studiomdl.cpp:2775-2801`）——
    /// **先查动画池、再回落到序列池**（`:2381-2397`）。
    ///
    /// 这里用 `blendcomp` 指向**另一条动画**：`CalcBoneTransforms` 的
    /// 3 参数版只在动画带 `STUDIO_DELTA` 时读 `pbaseanimation`
    /// （`simplify.cpp:4568`），所以对非 delta 夹具它**不影响数值** ——
    /// 本测试钉的是「三个关键字都被接受、且不改变正确的 calc 结果」。
    #[test]
    fn blendref_blendcomp_blendcenter_are_accepted() {
        let d = cb_dir("cb-refs");
        let qc = "\
$modelname \"cb8.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
calcblend \"px\" \"att\" ZR\n\
blendwidth 3\n\
blendref \"a\"\n\
blendcomp \"b\"\n\
blendcenter \"b\"\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let seq = &c.sequences[0];
        assert_eq!(seq.blend_ref.as_deref(), Some("a"), "blendref 应被记录");
        assert_eq!(seq.blend_comp.as_deref(), Some("b"), "blendcomp 应被记录");
        // `blendcenter "b"` ⟹ `b` 是第 1 格 ⟹ `[i0, i1] = [1, 0]`。
        assert_eq!(
            seq.blend_center,
            Some([1, 0]),
            "blendcenter 应解析成网格位置"
        );
        // 数值仍必须正确（`blendref` 换成 `a` 后「零点」就是 `a` 的第 0 帧，
        // 与缺省 `g_panimation[0]` 恰好是同一条 ⟹ 结果不变）。
        let p = seq.blend_params[0].as_ref().expect("轴 0");
        assert!(
            (p.end - 34.377_47).abs() < 1e-3,
            "paramend = {} 应约为 34.3775",
            p.end
        );
    }

    /// **`$animation` 块里的 ikrule 不受影响**（回归护栏）。
    /// 官方两条路径都写 `panim->cmds[]`，所以「写在 `$animation` 里」
    /// 与「写在 `$sequence` 里」最终都进同一条动画。这条钉住
    /// **不要**为了修 R23 而把 `$animation` 级的规则搬走或复制一份。
    #[test]
    fn animation_block_ik_rules_are_not_duplicated() {
        let d = ikr23_dir("r23-animblock");
        let qc = "\
$modelname \"r23c.mdl\"\n\
$ikchain \"leg\" \"ankle\"\n\
$animation \"decl\" \"a.smd\" fps 30 {\n\
ikrule \"leg\" touch \"hip\"\n\
}\n\
$sequence \"s\" \"decl\"\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(c.animations.len(), 1);
        assert_eq!(
            c.animations[0].ik_rules.len(),
            1,
            "`$animation` 块里的规则应恰好 1 条 —— 序列没有规则可补，\
             不得因 R23 的修复而复制成 2 条"
        );
    }

    /// **`$animation` 与 `$sequence` 都写了规则时，两条都要在**（且顺序稳定）。
    ///
    /// 官方 `ParseAnimation` 先收 `$animation` 块的 `cmds[]`，
    /// `ParseSequence` 之后再把序列块的追加进去
    /// （`studiomdl.cpp:2944` → `ParseCmdlistToken` 往 `panim->numcmds` 尾部加）。
    /// 所以落盘顺序 = **动画块在前、序列块在后**。
    #[test]
    fn animation_and_sequence_ik_rules_concatenate_in_order() {
        let d = ikr23_dir("r23-both");
        let qc = "\
$modelname \"r23d.mdl\"\n\
$ikchain \"leg\" \"ankle\"\n\
$animation \"decl\" \"a.smd\" fps 30 {\n\
ikrule \"leg\" touch \"hip\"\n\
}\n\
$sequence \"s\" \"decl\" {\n\
ikrule \"leg\" release\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        assert_eq!(c.animations.len(), 1);
        let kinds: Vec<_> = c.animations[0].ik_rules.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                crate::model::IkRuleType::Touch,
                crate::model::IkRuleType::Release
            ],
            "顺序必须是「动画块在前、序列块在后」（官方往 `panim->numcmds` 尾部追加）"
        );
    }

    // ------------------------------------------------------------------
    // R26：`$sequence` 块里的**另外两个**动画选项 —— `addlayer` 与
    // `weightlist` / `numframes` —— 与 R23 是**同一条机制**
    // ------------------------------------------------------------------

    /// **单动画序列的 `addlayer` 不得被丢弃。**
    ///
    /// # 官方依据（`studiomdl.cpp:2867-2872`）
    ///
    /// `addlayer` 的分支**与 `numblends` 无关**，只把序列名记进
    /// `pseq->autolayer[]`：
    /// ```c
    /// else if (stricmp( "addlayer", token ) == 0) {
    ///     GetToken( false );
    ///     strcpyn( pseq->autolayer[pseq->numautolayers].name, token );
    ///     pseq->numautolayers++;
    /// }
    /// ```
    /// 所以「单动画 + addlayer」与「blend + addlayer」是**同一件事**。
    ///
    /// # 实测症状
    ///
    /// mdlc 的单动画路径写死了 `auto_layers: Vec::new()` ⟹ 用户的
    /// `reload_layer` / `reload_loop_layer` / `reload_end_layer` 三条序列
    /// （QC 里都有 `addlayer "look_poses"`）的自动层**被静默丢弃** ——
    /// NekoMDL 有 1 条、mdlc 0 条 ⟹ 引擎不会把这三条序列与 `look_poses`
    /// 混合 ⟹ **上身/手部少一层姿态**。
    #[test]
    fn single_animation_sequence_keeps_addlayer() {
        let d = ikr23_dir("r26-addlayer");
        let qc = "\
$modelname \"r26a.mdl\"\n\
$animation \"base\" \"a.smd\" fps 30\n\
$animation \"layer\" \"b.smd\" fps 30\n\
$sequence \"lps\" \"layer\"\n\
$sequence \"sl\" \"base\" {\n\
addlayer \"lps\"\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let sl = c
            .sequences
            .iter()
            .find(|s| s.name == "sl")
            .expect("序列 `sl` 应存在");
        // 先证明夹具非空 —— 否则下面的断言会**空洞通过**。
        assert!(
            c.sequences.len() >= 2,
            "夹具应至少 2 条序列（`lps` / `sl`），实际 {}",
            c.sequences.len()
        );
        assert_eq!(
            sl.auto_layers.len(),
            1,
            "**单动画**序列的 `addlayer` 必须保留（修前这里是 0 —— \
             `reload_layer` 形态）"
        );
        // `sequence` 字段存的是**序列下标**，必须指向 `lps`。
        let target = sl.auto_layers[0].sequence as usize;
        assert_eq!(
            c.sequences[target].name, "lps",
            "自动层必须指向 `lps`，实际指向 {}",
            c.sequences[target].name
        );
        // 相邻对照：blend 路径的 addlayer 早就是好的 —— 两条路径必须一致。
        assert_eq!(
            sl.auto_layers[0].flags, 0,
            "`addlayer`（不带 `blendlayer` 的时间量）flags 应为 0"
        );
    }

    /// **`blendlayer` 必须解析**（`studiomdl.cpp:2890-2943`）。
    ///
    /// # 为什么这条重要
    ///
    /// mdlc 修前**完全没有 `blendlayer` 分支** ⟹ 关键字本身与它后面的
    /// 4 个数字全被当成**动画名**压进 `blends` ⟹
    /// `blend 格数 6 不是完全平方数` 之类的**误导性**错误。
    ///
    /// 真实影响（用户的 `v_pistola_processed.qc`，8 处 `blendlayer`）：
    ///
    /// | 编译器 | 结果 |
    /// |---|---|
    /// | 真 `studiomdl.exe` | **exit 0** |
    /// | NekoMDL | **exit 0** |
    /// | mdlc（修前） | **exit 1，8 处错误** |
    ///
    /// # 官方语义（`studiomdl.cpp:2890-2943`）
    ///
    /// ```c
    /// autolayer[n].flags = 0;
    /// name  = token; start = atoi; peak = atoi; tail = atoi; end = atoi;
    /// while (TokenAvailable()) {
    ///     if      "xfade"         flags |= 0x0080;
    ///     else if "spline"        flags |= 0x0040;
    ///     else if "noblend"       flags |= 0x0200;
    ///     else if "poseparameter" flags |= 0x4000; pose = LookupPoseParameter(next);
    ///     else if "local"         flags |= 0x1000; pseq->flags |= 0x1000;
    ///     else { UnGetToken(); break; }        // ← 不认识就**吐回并停**
    /// }
    /// ```
    ///
    /// # 实测官方（真 `studiomdl.exe`）
    ///
    /// ```text
    /// blendlayer "layer" 2 5 9 12
    ///   ⟹ iSequence=<layer 的下标> iPose=0 flags=0x0
    ///      start=2 peak=5 tail=9 end=12
    /// ```
    #[test]
    fn blendlayer_parses_times_and_flags() {
        let d = cb_dir("bl-layer");
        let qc = "\
$modelname \"bl.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
blend \"px\" 1 -1\n\
blendwidth 3\n\
}\n\
$sequence \"layer\" \"a\" {\n\
}\n\
$sequence \"user\" \"b\" {\n\
blendlayer \"layer\" 2 5 9 12\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("官方能编的 QC，mdlc 也必须能编");
        let _ = std::fs::remove_dir_all(&d);

        // 先证明夹具非空 —— 否则下面的断言会**空洞通过**。
        assert_eq!(c.sequences.len(), 3, "夹具应有 3 条序列");
        let user = c
            .sequences
            .iter()
            .find(|s| s.name == "user")
            .expect("应能找到 `user`");
        assert_eq!(
            user.auto_layers.len(),
            1,
            "`blendlayer` 必须产生**一条**自动层（修前这里是 0，且 blends 被污染）"
        );
        let al = &user.auto_layers[0];
        // `sequence` 存的是**序列下标**，必须指向 `layer`。
        assert_eq!(
            c.sequences[al.sequence as usize].name, "layer",
            "自动层必须指向 `layer`"
        );
        assert_eq!(al.flags, 0, "无子标志时 flags 应为 0");
        assert_eq!(al.pose, 0, "无 `poseparameter` 时 iPose 应为 0");
        // ⚠️ 四个时间量在**不带** `STUDIO_AL_POSE` 时会被
        // `write.cpp:541-544` 除以 `numframes - 1` 转成 cycle。
        // 本夹具的动画只有 2 帧 ⟹ 除数是 1 ⟹ 落盘值 = QC 原值。
        assert_eq!(
            (al.start, al.peak, al.tail, al.end),
            (2.0, 5.0, 9.0, 12.0),
            "四个时间量应原样保留（2 帧 ⟹ 除数为 1）"
        );
    }

    /// **`blendlayer` 的子标志与 `poseparameter`**（`studiomdl.cpp:2909-2940`）。
    ///
    /// `xfade` / `spline` / `noblend` / `poseparameter` / `local` 五个子标志，
    /// 以及 `poseparameter` 会**顺带**把姿势参数下标写进 `iPose`。
    ///
    /// # 实测官方（真 `studiomdl.exe`）
    ///
    /// ```text
    /// blendlayer "layer" 3 6 9 12 xfade spline poseparameter "py"
    ///   ⟹ iPose=1 flags=0x40c0 start=3 peak=6 tail=9 end=12
    /// ```
    ///
    /// `0x40c0 = STUDIO_AL_POSE(0x4000) | STUDIO_AL_XFADE(0x0080) | STUDIO_AL_SPLINE(0x0040)`。
    ///
    /// ⚠️ 带 `STUDIO_AL_POSE` 时**不**做 cycle 换算
    /// （`write.cpp:546-552`）⟹ 时间量原样落盘。
    #[test]
    fn blendlayer_subflags_and_pose_parameter() {
        let d = cb_dir("bl-flags");
        let qc = "\
$modelname \"bl3.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$poseparameter \"py\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
blend \"px\" 1 -1\n\
blendwidth 3\n\
}\n\
$sequence \"layer\" \"a\" {\n\
}\n\
$sequence \"user\" \"b\" {\n\
blendlayer \"layer\" 3 6 9 12 xfade spline poseparameter \"py\"\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let user = c
            .sequences
            .iter()
            .find(|s| s.name == "user")
            .expect("应能找到 `user`");
        assert_eq!(user.auto_layers.len(), 1, "应有 1 条自动层");
        let al = &user.auto_layers[0];
        // `0x40c0 = POSE | XFADE | SPLINE`（实测官方就是这个值）。
        assert_eq!(
            al.flags, 0x40c0,
            "flags 应为 0x40c0（POSE|XFADE|SPLINE），实际 0x{:x}",
            al.flags
        );
        // `py` 是第 1 个 `$poseparameter` ⟹ `iPose = 1`（实测官方）。
        assert_eq!(al.pose, 1, "`poseparameter \"py\"` 应给出 iPose=1");
        // 带 `STUDIO_AL_POSE` ⟹ **不**做 cycle 换算，原样保留。
        assert_eq!(
            (al.start, al.peak, al.tail, al.end),
            (3.0, 6.0, 9.0, 12.0),
            "带 STUDIO_AL_POSE 时时间量不做 cycle 换算"
        );
    }

    /// **`blendlayer` 后面不认识的 token 必须被吐回**（官方 `UnGetToken`）。
    ///
    /// 官方子标志循环遇到不认识的就 `UnGetToken(); break;` —— 所以
    /// `blendlayer` 之后紧跟的**其它关键字**（如 `fadein`）不会被吃掉。
    ///
    /// # ⚠️ 判据必须写在**同一行**
    ///
    /// `Lexer::token_available`（`lexer.rs`）是**行内**判据 —— 遇到 `\n`
    /// 就返回 false（与官方 `TokenAvailable` 同语义）。所以把 `fadein`
    /// 写在下一行时，子标志循环**根本不会进入**，`unget` 那条路径
    /// 一次都不跑 ⟹ 测不出「吞掉一切」的变异。
    ///
    /// 实测：把 `unget` 改成 `continue`（即吞掉未知 token）后，
    /// 「`fadein` 换行」版本**全绿逃逸**；本版本（同一行）能抓住。
    ///
    /// # 实测官方（真 `studiomdl.exe`）
    ///
    /// ```text
    /// blendlayer "layer" 1 2 3 4 fadein 0.75
    ///   ⟹ seqdesc.fadeintime = 0.75   （`fadein` 被正常解析，没被子标志吃掉）
    ///      numautolayers = 1
    /// ```
    #[test]
    fn blendlayer_ungets_unknown_token() {
        let d = cb_dir("bl-unget");
        let qc = "\
$modelname \"bl4.mdl\"\n\
$poseparameter \"px\" -1 1\n\
$attachment \"att\" \"tip\" 0 0 0 rotate 0 0 0\n\
$animation \"a\" \"a.smd\" fps 30\n\
$animation \"b\" \"b.smd\" fps 30\n\
$animation \"c\" \"c.smd\" fps 30\n\
$sequence \"idle\" \"a\" \"b\" \"c\" {\n\
blend \"px\" 1 -1\n\
blendwidth 3\n\
}\n\
$sequence \"layer\" \"a\" {\n\
}\n\
$sequence \"user\" \"b\" {\n\
blendlayer \"layer\" 1 2 3 4 fadein 0.75\n\
}\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let user = c
            .sequences
            .iter()
            .find(|s| s.name == "user")
            .expect("应能找到 `user`");
        assert_eq!(user.auto_layers.len(), 1, "应有 1 条自动层");
        assert_eq!(
            user.auto_layers[0].flags, 0,
            "`fadein` 不是子标志，不该被算进 flags"
        );
        assert_eq!(
            user.fade_in, 0.75,
            "同一行的 `fadein 0.75` 必须被正常解析（官方 `UnGetToken`）—— \
             若实现成「吞掉未知 token」，这里会保持缺省 0.2"
        );
    }

    /// **`$sequence` 的 `weightlist` 必须覆盖被复用动画的权重。**
    ///
    /// 官方把它落成 `animations[0]->cmds[]` 的 `CMD_WEIGHTS`，
    /// 由 `setAnimationWeight`（`simplify.cpp:1721-1729`）**改共享动画对象**。
    /// 序列自己的 `weight[]` 是之后由 `merge_weights`
    /// （`simplify.cpp:302-318`）**对各格取 MAX** 得来的。
    ///
    /// # 实测症状（用户工程）
    ///
    /// ```text
    /// $sequence "fidget" "a_look_mid" weightlist "empty" … numframes 90 fps 1
    /// ```
    /// `empty` 表（只写了 `"ValveBiped.ValveBiped" 0`）经父链补齐后**全 0**。
    /// NekoMDL 与发布版**都**给 `fidget` 全 0，而 mdlc 修前给全 1
    /// （`weightlist` 只在 `!reused` 分支被读）。
    ///
    /// ⚠️ 这条同时钉住「**共享**」：`fidget` 走完后，被它复用的动画
    /// 权重必须是 0 —— 不是「序列自己的副本」。
    #[test]
    fn sequence_weightlist_overrides_reused_animation() {
        let d = ikr23_dir("r26-weightlist");
        let qc = "\
$modelname \"r26b.mdl\"\n\
$weightlist \"zero\" {\n\
\"root\" 0\n\
}\n\
$animation \"shared\" \"a.smd\" fps 30\n\
$sequence \"w\" \"shared\" weightlist \"zero\"\n\
$model \"body\" \"a.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let w = c
            .sequences
            .iter()
            .find(|s| s.name == "w")
            .expect("序列 `w` 应存在")
            .weights
            .clone();
        // 先证明夹具非空：骨骼数与权重向量长度必须对得上。
        assert_eq!(
            w.len(),
            c.desc.bones.len(),
            "权重向量长度应等于骨骼数（否则下面的「全 0」判据是空洞的）"
        );
        assert!(
            !w.is_empty(),
            "权重向量不能为空 —— 否则 `all(|x| x == 0.0)` 会空洞通过"
        );
        assert!(
            w.iter().all(|v| *v == 0.0),
            "`weightlist \"zero\"` 应让**全部**骨骼权重为 0（修前是 1），实际 {w:?}"
        );
    }

    /// **`$sequence` 的 `numframes` 必须延长被复用动画**（官方 `forceNumframes`）。
    ///
    /// 官方实现（`simplify.cpp:1279-1293`）把**最后一帧**复制到 `numframes`
    /// 为止，且改的是**共享动画对象** ⟹ 引用同一动画的其它序列**也跟着变长**。
    ///
    /// # 实测症状（用户工程）
    ///
    /// ```text
    /// $sequence "fidget" "a_look_mid" weightlist "empty" "ACT_VM_FIDGET" 100 numframes 90 fps 1
    /// ```
    /// `a_look_mid` 来自 `look_poses.smd` 的 `frames 1 1`（**1 帧**）。
    /// NekoMDL 把它延长到 **90 帧**，且 `look_poses` 的 blend 表里
    /// 引用的 `a_look_mid` **同样是 90 帧**；mdlc 修前**完全没有实现**
    /// `CMD_NUMFRAMES` ⟹ 两侧都停在 1 帧。
    ///
    /// ⚠️ 判据必须**同时**看「变长」与「共享」两件事 —— 只看本序列会漏掉
    /// 「另一条序列也应看到 90 帧」，而那正是官方的行为。
    #[test]
    fn sequence_numframes_extends_shared_animation() {
        let d = ikr23_dir("r26-numframes");
        // `b.smd` 只有 2 帧（`IKR23_SMD`）⟹ 延长到 5 帧后末帧应被复制 3 次。
        let qc = "\
$modelname \"r26c.mdl\"\n\
$animation \"shared\" \"b.smd\" fps 30\n\
$sequence \"grow\" \"shared\" numframes 5\n\
$sequence \"user\" \"shared\"\n\
$model \"body\" \"b.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let anim = c
            .animations
            .iter()
            .find(|a| a.name == "shared")
            .expect("动画 `shared` 应存在");
        // 先证明夹具非空：源只有 2 帧，否则「延长」判据没有意义。
        assert!(
            anim.frames.len() == 5,
            "`numframes 5` 应把 2 帧的动画延长到 5 帧，实际 {} 帧",
            anim.frames.len()
        );
        // 末帧必须被**复制**（不是补零）—— 官方是 memcpy 最后一帧。
        assert_eq!(
            anim.frames[2], anim.frames[1],
            "第 2 帧应是第 1 帧的复制（官方 memcpy 最后一帧）"
        );
        assert_eq!(
            anim.frames[4], anim.frames[1],
            "第 4 帧应是第 1 帧的复制（官方 memcpy 最后一帧）"
        );
        // **共享**：另一条引用同一动画的序列也应看到 5 帧。
        let user = c
            .sequences
            .iter()
            .find(|s| s.name == "user")
            .expect("序列 `user` 应存在");
        assert_eq!(
            user.frames.len(),
            5,
            "`numframes` 改的是**共享动画** ⟹ 引用它的另一条序列也应看到 5 帧（修前 2 帧）"
        );
    }

    #[test]
    fn sequence_numframes_smaller_value_truncates_shared_animation() {
        let d = ikr23_dir("r26-numframes-small");
        let qc = "\
$modelname \"r26d.mdl\"\n\
$animation \"shared\" \"b.smd\" fps 30\n\
$sequence \"shrink\" \"shared\" numframes 1\n\
$model \"body\" \"b.smd\" {\n\
}\n";
        let desc = crate::qc::parse_qc_str(qc, &d).expect("QC 应解析成功");
        let c = compile(&desc, &d).expect("应能编译");
        let _ = std::fs::remove_dir_all(&d);

        let anim = c
            .animations
            .iter()
            .find(|a| a.name == "shared")
            .expect("动画 `shared` 应存在");
        assert_eq!(anim.frames.len(), 1);
        assert_eq!(c.sequences[0].frames.len(), 1);
    }

    #[test]
    fn qc_scale_changes_geometry_bind_pose_animation_and_lods_together() {
        let d = tmpdir("qc-uniform-scale");
        write(&d, "source.smd", SMD);
        let qc = "$modelname models/test/scaled.mdl\n$body body source.smd\n$sequence idle source.smd\n$lod 20 { replacemodel source.smd source.smd }\n";
        let base = crate::qc::parse_qc_str(qc, &d).unwrap();
        let scaled = crate::qc::parse_qc_str(&format!("$scale 2\n{qc}"), &d).unwrap();
        let a = compile(&base, &d).unwrap();
        let b = compile(&scaled, &d).unwrap();
        let ma = &a.bodyparts[0].models[0];
        let mb = &b.bodyparts[0].models[0];
        for (va, vb) in ma.meshes[0].vertices.iter().zip(&mb.meshes[0].vertices) {
            for axis in 0..3 {
                assert!((vb.pos[axis] - va.pos[axis] * 2.0).abs() < 1e-4);
            }
            assert_eq!(va.normal, vb.normal);
            assert_eq!(va.uv, vb.uv);
            assert_eq!(va.bones, vb.bones);
        }
        for i in 0..base.bones.len() {
            let (pa, ra) = resolve_bone_pose(&a.desc, &a, i);
            let (pb, rb) = resolve_bone_pose(&b.desc, &b, i);
            for axis in 0..3 {
                assert!((pb[axis] - pa[axis] * 2.0).abs() < 1e-4);
            }
            assert_eq!(ra, rb);
        }
        for (fa, fb) in a.animations[0].frames.iter().zip(&b.animations[0].frames) {
            for (pa, pb) in fa.iter().zip(fb) {
                for axis in 0..3 {
                    assert!((pb.position[axis] - pa.position[axis] * 2.0).abs() < 1e-4);
                }
                assert_eq!(pa.rotation, pb.rotation);
            }
        }
        let la = ma.lods.as_ref().unwrap();
        let lb = mb.lods.as_ref().unwrap();
        for (ma, mb) in la.meshes.iter().zip(&lb.meshes) {
            for (va, vb) in ma.vertices.iter().zip(&mb.vertices) {
                for axis in 0..3 {
                    assert!((vb.pos[axis] - va.pos[axis] * 2.0).abs() < 1e-4);
                }
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn material_rename_preserves_geometry_and_skin_slot_indices() {
        let d = tmpdir("qc-material-rename");
        write(&d, "source.smd", SMD);
        let qc = "$modelname models/test/rename.mdl\n$body body source.smd\n$texturegroup skins { { myprop } { alternate } }\n";
        let base = crate::qc::parse_qc_str(qc, &d).unwrap();
        let renamed =
            crate::qc::parse_qc_str(&format!("{qc}$renamematerial myprop replacement\n"), &d)
                .unwrap();
        let a = compile(&base, &d).unwrap();
        let b = compile(&renamed, &d).unwrap();
        assert_eq!(
            a.desc.materials.skin_families,
            b.desc.materials.skin_families
        );
        assert_eq!(
            a.desc.materials.textures.len(),
            b.desc.materials.textures.len()
        );
        let old_slot = a
            .desc
            .materials
            .textures
            .iter()
            .position(|t| t.name == "myprop")
            .unwrap();
        assert_eq!(b.desc.materials.textures[old_slot].name, "replacement");
        assert_eq!(
            a.bodyparts[0].models[0].meshes,
            b.bodyparts[0].models[0].meshes
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn qc_scale_is_captured_at_each_source_and_studio_override_is_absolute() {
        let d = tmpdir("qc-scale-order");
        write(&d, "source.smd", SMD);
        let qc = "$modelname models/test/scale_order.mdl\n$staticprop\n$scale 2\n$body first source.smd\n$scale 3\n$body second source.smd\n$body override source.smd scale 4\n$scale 9\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let compiled = compile(&desc, &d).unwrap();
        for (body, scale) in compiled.bodyparts.iter().zip([2.0, 3.0, 4.0]) {
            let mesh = &body.models[0].meshes[0];
            let max_x = mesh
                .vertices
                .iter()
                .map(|v| v.pos[0].abs())
                .fold(0.0f32, f32::max);
            assert!((max_x - 8.0 * scale).abs() < 1e-4, "{scale}: {max_x}");
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn extensionless_qc_sources_compile_to_the_same_output_as_explicit_smd_paths() {
        let d = tmpdir("extensionless-qc");
        write(&d, "source.smd", SMD);
        let explicit = "$modelname models/test/implicit.mdl\n$body body source.smd\n$sequence idle source.smd\n";
        let implicit = explicit.replace("source.smd", "source");
        let a = crate::qc::parse_qc_str(explicit, &d).unwrap();
        let b = crate::qc::parse_qc_str(&implicit, &d).unwrap();
        let a =
            crate::pipeline::build(&a, &d, crate::pipeline::PipelineOptions::default()).unwrap();
        let b =
            crate::pipeline::build(&b, &d, crate::pipeline::PipelineOptions::default()).unwrap();
        assert_eq!(a.mdl, b.mdl);
        assert_eq!(a.vvd, b.vvd);
        assert_eq!(a.vtx, b.vtx);
        let _ = std::fs::remove_dir_all(d);
    }

    fn source_options_fixture(tag: &str) -> PathBuf {
        let d = tmpdir(tag);
        write(&d, "reference.smd", SMD);
        let animation = SMD.replace(
            "    1 0.000000 0.000000 8.000000 0.000000 0.000000 0.000000\nend\ntriangles",
            "    1 0 0 10 0 0 0\ntime 1\n0 0 0 0 0 0 0\n1 0 0 11 0 0 0\ntime 2\n0 0 0 0 0 0 0\n1 0 0 12 0 0 0\nend\ntriangles"
        );
        write(&d, "animation.smd", &animation);
        d
    }

    #[test]
    fn implicit_sequence_selects_source_range_and_scales_before_writing() {
        let d = source_options_fixture("implicit-sequence-range");
        let qc = "$modelname models/test/sequence.mdl\n$body body reference.smd\n$sequence cropped animation.smd frames 1 2 scale 2\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let out =
            crate::pipeline::build(&desc, &d, crate::pipeline::PipelineOptions::default()).unwrap();
        let tip = out
            .compiled
            .desc
            .bones
            .iter()
            .position(|b| b.name == "tip")
            .unwrap();
        let sequence = &out.compiled.sequences[0];
        assert_eq!(sequence.frames.len(), 2);
        assert_eq!(sequence.frames[0][tip].position[2], 22.0);
        assert_eq!(sequence.frames[1][tip].position[2], 24.0);
        assert_eq!(
            out.compiled.animations[sequence.cells[0]].frames,
            sequence.frames
        );
        let (bind, _) = resolve_bone_pose(&out.compiled.desc, &out.compiled, tip);
        assert_eq!(bind[2], 8.0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn named_sequence_source_options_override_shared_source_once() {
        let d = source_options_fixture("shared-sequence-range");
        let qc = "$modelname models/test/shared.mdl\n$body body reference.smd\n$animation shared animation.smd frames 0 0\n$sequence before shared\n$scale 3\n$sequence changed shared frames 1 2 scale 2\n$sequence after shared\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let c = compile(&desc, &d).unwrap();
        let tip = c.desc.bones.iter().position(|b| b.name == "tip").unwrap();
        assert_eq!(c.animations.len(), 1);
        for sequence in &c.sequences {
            assert_eq!(sequence.cells, [0]);
            assert_eq!(sequence.frames.len(), 2);
            assert_eq!(
                sequence.frames[0][tip].position[2], 22.0,
                "{}",
                sequence.name
            );
            assert_eq!(
                sequence.frames[1][tip].position[2], 24.0,
                "{}",
                sequence.name
            );
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn continue_source_override_order_is_preserved_for_shared_animation() {
        let d = source_options_fixture("continued-source-order");
        let qc = "$modelname models/test/continued.mdl\n$body body reference.smd\n$animation shared animation.smd\n$sequence first shared frames 0 0 scale 2\n$sequence second shared frames 1 2 scale 3\n$continue first frames 2 2 scale 4\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let c = compile(&desc, &d).unwrap();
        let tip = c.desc.bones.iter().position(|b| b.name == "tip").unwrap();
        for sequence in &c.sequences {
            assert_eq!(sequence.frames.len(), 1);
            assert_eq!(sequence.frames[0][tip].position[2], 48.0);
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn direct_descriptor_sequence_source_override_applies_before_shared_load() {
        let d = source_options_fixture("descriptor-source-override");
        let qc = "$modelname models/test/descriptor.mdl\n$body body reference.smd\n$animation shared animation.smd frames 0 0\n$sequence first shared\n$sequence second shared\n";
        let mut desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        desc.sequences[0].frames = Some([1, 2]);
        desc.sequences[0].scale = Some(2.0);
        desc.sequences[1].input_scale = Some(5.0);
        let c = compile(&desc, &d).unwrap();
        let tip = c.desc.bones.iter().position(|b| b.name == "tip").unwrap();
        for sequence in &c.sequences {
            assert_eq!(sequence.frames.len(), 2);
            assert_eq!(sequence.frames[0][tip].position[2], 22.0);
            assert_eq!(sequence.frames[1][tip].position[2], 24.0);
        }
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn changed_global_scale_only_affects_new_implicit_animation_sources() {
        let d = source_options_fixture("sequence-global-scale");
        let qc = "$modelname models/test/global_scale.mdl\n$body body reference.smd\n$scale 2\n$animation shared animation.smd\n$scale 3\n$sequence reused shared\n$sequence implicit animation.smd\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let c = compile(&desc, &d).unwrap();
        let tip = c.desc.bones.iter().position(|b| b.name == "tip").unwrap();
        assert_eq!(c.sequences[0].frames[1][tip].position[2], 22.0);
        assert_eq!(c.sequences[1].frames[1][tip].position[2], 33.0);
        assert_ne!(c.sequences[0].cells, c.sequences[1].cells);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn sequence_source_frame_ranges_clamp_and_reject_reversed_intervals() {
        let d = source_options_fixture("sequence-frame-clamp");
        let qc = "$modelname models/test/clamp.mdl\n$body body reference.smd\n$sequence clamped animation.smd frames -20 200\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let c = compile(&desc, &d).unwrap();
        assert_eq!(c.sequences[0].frames.len(), 3);
        let reversed = qc.replace("frames -20 200", "frames 2 0");
        let desc = crate::qc::parse_qc_str(&reversed, &d).unwrap();
        let errors = compile(&desc, &d).unwrap_err();
        assert!(errors.iter().any(
            |error| matches!(error, CompileError::Message { at, .. } if at == "sequences[0].frames")
        ));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn blend_sequence_source_options_apply_only_to_the_first_named_animation() {
        let d = source_options_fixture("blend-sequence-source-options");
        let qc = "$modelname models/test/blend_source.mdl\n$body body reference.smd\n$poseparameter movement 0 1\n$animation first animation.smd\n$animation second animation.smd\n$sequence blend { first second blend movement 0 1 frames 1 2 scale 2 }\n";
        let desc = crate::qc::parse_qc_str(qc, &d).unwrap();
        let c = compile(&desc, &d).unwrap();
        let tip = c.desc.bones.iter().position(|b| b.name == "tip").unwrap();
        assert_eq!(c.sequences[0].cells, [0, 1]);
        assert_eq!(c.animations[0].frames.len(), 2);
        assert_eq!(c.animations[0].frames[0][tip].position[2], 22.0);
        assert_eq!(c.animations[1].frames.len(), 3);
        assert_eq!(c.animations[1].frames[0][tip].position[2], 10.0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn smd_animation_rejects_timestamps_that_cannot_be_indexed_as_frames() {
        let d = source_options_fixture("smd-animation-timestamps");
        let animation = std::fs::read_to_string(d.join("animation.smd")).unwrap();
        for (case, from, to) in [
            ("nonzero", "time 0", "time 5"),
            ("sparse", "time 1\n", "time 4\n"),
            ("duplicate", "time 1\n", "time 0\n"),
            ("negative", "time 1\n", "time -1\n"),
        ] {
            write(&d, "animation.smd", &animation.replace(from, to));
            for sequence in [
                "$sequence idle animation.smd frames 1 2\n",
                "$animation named animation.smd\n$sequence idle named frames 1 2\n",
            ] {
                let qc = format!(
                    "$modelname models/test/timestamps.mdl\n$body body reference.smd\n{sequence}"
                );
                let desc = crate::qc::parse_qc_str(&qc, &d).unwrap();
                let errors = compile(&desc, &d).unwrap_err();
                assert!(
                    errors.iter().any(|error| error
                        .to_string()
                        .contains("animation timestamps must be contiguous and start at 0")),
                    "{case}: {errors:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }
    fn close_transform(actual: [f32; 3], expected: [f32; 3]) {
        for axis in 0..3 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 0.0001,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn qc_origin_and_upaxis_transform_animation_roots_in_declaration_order() {
        let dir = source_options_fixture("qc-root-transforms");
        let qc = "$modelname models/test/transforms.mdl\n$body body reference.smd\n$sequence original animation.smd\n$origin 1 2 3 90\n$sequence shifted animation.smd\n$upaxis y\n$sequence upright animation.smd\n";
        let desc = crate::qc::parse_qc_str(qc, &dir).unwrap();
        let output =
            crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                .unwrap();
        let compiled = &output.compiled;
        let parents = bone_parents(&compiled.desc);
        let first = frame_worlds(&compiled.desc, &parents, &compiled.sequences[0].frames[0]);
        let shifted = frame_worlds(&compiled.desc, &parents, &compiled.sequences[1].frames[0]);
        let upright = frame_worlds(&compiled.desc, &parents, &compiled.sequences[2].frames[0]);
        close_transform([first[0][3], first[0][7], first[0][11]], [0.0; 3]);
        close_transform(
            [shifted[0][3], shifted[0][7], shifted[0][11]],
            [1.0, 2.0, -3.0],
        );
        close_transform(
            [upright[0][3], upright[0][7], upright[0][11]],
            [-3.0, -1.0, -2.0],
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn qc_static_origin_upaxis_and_attachments_share_one_transform() {
        let dir = source_options_fixture("qc-static-transforms");
        let qc = "$modelname models/test/transforms.mdl\n$staticprop\n$origin 1 2 3\n$upaxis y\n$body body reference.smd\n$attachment marker root 4 5 6\n$sequence idle reference.smd\n";
        let desc = crate::qc::parse_qc_str(qc, &dir).unwrap();
        let compiled = compile(&desc, &dir).unwrap();
        let mesh = &compiled.bodyparts[0].models[0].meshes[0];
        let source = crate::smd::parse_smd(SMD).unwrap();
        for vertex in &mesh.vertices {
            assert!(source.triangles.iter().flat_map(|t| &t.vertices).any(|v| {
                let expected = [
                    v.position[2] + 1.0,
                    v.position[0] + 2.0,
                    v.position[1] + 3.0,
                ];
                (0..3).all(|axis| (vertex.pos[axis] - expected[axis]).abs() < 0.0001)
            }));
        }
        let local = attachment_local_matrix(&compiled.desc, &compiled.desc.attachments[0], &[], 0);
        close_transform([local[3], local[7], local[11]], [7.0, 6.0, 8.0]);
        let mut collision = source;
        flatten_static_collision(&mut collision, &desc);
        close_transform(
            collision.triangles[0].vertices[0].position,
            [-6.0, -1.0, -5.0],
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn animation_options_override_global_transform_without_affecting_other_animations() {
        let dir = source_options_fixture("qc-animation-transforms");
        let qc = "$modelname models/test/transforms.mdl\n$body body reference.smd\n$origin 1 2 3\n$animation original animation.smd\n$animation changed animation.smd origin 4 5 6 angles 0 0 -90\n$sequence shared original rotate 90\n$sequence changed changed\n";
        let desc = crate::qc::parse_qc_str(qc, &dir).unwrap();
        let compiled = compile(&desc, &dir).unwrap();
        let parents = bone_parents(&compiled.desc);
        let first = frame_worlds(&compiled.desc, &parents, &compiled.animations[0].frames[0]);
        let second = frame_worlds(&compiled.desc, &parents, &compiled.animations[1].frames[0]);
        close_transform([first[0][3], first[0][7], first[0][11]], [1.0, 2.0, -3.0]);
        close_transform(
            [second[0][3], second[0][7], second[0][11]],
            [-4.0, -5.0, -6.0],
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod animation_budget_tests {
    use super::*;
    #[test]
    fn cumulative_animation_pose_reservations_are_bounded_and_overflow_safe() {
        let mut poses = 0;
        reserve_animation_poses(&mut poses, 20_000, 100).unwrap();
        reserve_animation_poses(&mut poses, 20_000, 100).unwrap();
        assert_eq!(poses, 4_000_000);
        assert!(reserve_animation_poses(&mut poses, 50_000, 100).is_err());
        assert_eq!(poses, 4_000_000);
        assert!(reserve_animation_poses(&mut poses, usize::MAX, 2).is_err());
        assert_eq!(poses, 4_000_000);
        assert!(reserve_animation_poses(&mut poses, usize::MAX, 1).is_err());
        assert_eq!(poses, 4_000_000);
    }
}
