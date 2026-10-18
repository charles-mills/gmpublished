//! QC 语法层 —— token 流 → [`crate::model::ModelDesc`]。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::QcError;
use super::lexer::{Lexer, Token};
use crate::model::*;

#[path = "compat.rs"]
mod compat;

// ---------------------------------------------------------------------------
// 头部 flags 位（`studio.h` 的 `STUDIOHDR_FLAGS_*`）
// ---------------------------------------------------------------------------

/// `STUDIOHDR_FLAGS_STATIC_PROP`（`1<<4`）—— 由 `mdl_writer` 自动置位。
const FLAG_STATIC_PROP: i32 = 1 << 4;
/// `STUDIOHDR_FLAGS_NO_FORCED_FADE`（`1<<11`）—— `$noforcedfade`。
const FLAG_NO_FORCED_FADE: i32 = 1 << 11;
/// `STUDIOHDR_FLAGS_FORCE_PHONEME_CROSSFADE`（`1<<12`）—— `$forcephonemecrossfade`。
const FLAG_FORCE_PHONEME_CROSSFADE: i32 = 1 << 12;
/// `STUDIOHDR_FLAGS_CONSTANT_DIRECTIONAL_LIGHT_DOT`（`1<<13`）。
const FLAG_CONSTANT_DIRECTIONAL_LIGHT_DOT: i32 = 1 << 13;
/// `STUDIOHDR_FLAGS_AMBIENT_BOOST`（`1<<16`）—— `$ambientboost`。
const FLAG_AMBIENT_BOOST: i32 = 1 << 16;
/// `STUDIOHDR_FLAGS_DO_NOT_CAST_SHADOWS`（`1<<17`）。
const FLAG_DO_NOT_CAST_SHADOWS: i32 = 1 << 17;
/// `STUDIOHDR_FLAGS_CAST_TEXTURE_SHADOWS`（`1<<18`）。
const FLAG_CAST_TEXTURE_SHADOWS: i32 = 1 << 18;
/// `STUDIOHDR_FLAGS_FORCE_OPAQUE`（`1<<2`）—— `$opaque`。
const FLAG_OPAQUE: i32 = 1 << 2;
/// `STUDIOHDR_FLAGS_TRANSLUCENT_TWOPASS`（`1<<3`）—— `$mostlyopaque`。
const FLAG_MOSTLY_OPAQUE: i32 = 1 << 3;

/// 官方 `MAXSTUDIOSEQUENCES`（`studiomdl.h:28`，**1524**）。
///
/// `$declaresequence` 与 `$sequence` **共用**同一个 `g_sequence` 池，
/// 所以上限是两者之和。
///
/// 超出时官方报 `Too many sequences (1524 max)`。
///
/// ⚠️ 与 weightlist 的 16/32 不同，这个值**没实测**过 —— 要造 1525 条
/// 序列的 QC 成本高，而 survivor 的 937 条离上限还有距离。
/// 头文件的值在这里先按「够用」处理，标注为未实测。
const MAX_SEQUENCES: usize = 1524;

/// 官方 `MAXSTUDIOCMDS`（`studiomdl.h:37`）—— 一张命令列表最多几条，
/// 也是**一个动画**最多能挂几条（`ParseAnimationToken` 的 `cmdlist`
/// 分支在拷贝时逐条检查，超了报 `Too many cmds in %s`）。
const MAX_CMDS: usize = 64;

type LodReplacement = (String, Option<(String, bool)>, Token);

fn attachment_axes(x: [f32; 3], z: [f32; 3]) -> Result<crate::bone_math::Matrix3x4, &'static str> {
    let normalize = |axis: [f32; 3]| {
        let length = axis.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !length.is_finite() || length == 0.0 {
            return Err("x_and_z_axes requires finite nonzero independent axes");
        }
        Ok(axis.map(|value| value / length))
    };
    let x = normalize(x)?;
    let dot: f32 = x.iter().zip(z).map(|(x, z)| x * z).sum();
    let z = normalize(std::array::from_fn(|axis| z[axis] - dot * x[axis]))?;
    let dot: f32 = x.iter().zip(z).map(|(x, z)| x * z).sum();
    if !dot.is_finite() || dot.abs() > 0.0001 {
        return Err("x_and_z_axes requires finite nonzero independent axes");
    }
    let y = [
        z[1] * x[2] - z[2] * x[1],
        z[2] * x[0] - z[0] * x[2],
        z[0] * x[1] - z[1] * x[0],
    ];
    Ok([
        x[0], y[0], z[0], 0.0, x[1], y[1], z[1], 0.0, x[2], y[2], z[2], 0.0,
    ])
}

#[derive(Debug, Clone)]
struct ParsedLod {
    options: LodModel,
    replacements: Vec<LodReplacement>,
}

fn lod_source_key(source: &str) -> String {
    let normalized = source.replace('\\', "/").to_ascii_lowercase();
    let path = Path::new(&normalized);
    let without_extension = match path.extension().and_then(|extension| extension.to_str()) {
        Some("smd" | "fbx" | "gltf" | "glb" | "obj" | "dmx") => {
            path.with_extension("").to_string_lossy().into_owned()
        }
        _ => normalized,
    };
    without_extension.trim_start_matches("./").to_string()
}

/// `lookupControl`（`studiomdl.cpp:395-415`）。
///
/// 接受的 token（大小写不敏感）与位值：
/// `X`=0x1 `Y`=0x2 `Z`=0x4 `XR`=0x8 `YR`=0x10 `ZR`=0x20
/// `LX`=0x40 `LY`=0x80 `LZ`=0x100 `LXR`=0x200 `LYR`=0x400 `LZR`=0x800
/// `LM`=0x1000（`STUDIO_LINEAR`）`LQ`=0x2000（`STUDIO_QUADRATIC_MOTION`）。
///
/// ⚠️ **没有 `RX`/`RY`/`RZ`** —— 官方表里只有 `XR`/`YR`/`ZR`。
/// 认不出返回 `None`（官方返回 `-1`），调用方据此停止吃 token。
fn lookup_control(text: &str) -> Option<i32> {
    Some(match text.to_ascii_lowercase().as_str() {
        "x" => 0x0001,
        "y" => 0x0002,
        "z" => 0x0004,
        "xr" => 0x0008,
        "yr" => 0x0010,
        "zr" => 0x0020,
        "lx" => 0x0040,
        "ly" => 0x0080,
        "lz" => 0x0100,
        "lxr" => 0x0200,
        "lyr" => 0x0400,
        "lzr" => 0x0800,
        "lm" => 0x1000,
        "lq" => 0x2000,
        _ => return None,
    })
}

/// QC 解析器。
pub struct Parser<'a> {
    /// 词法器。
    pub lex: Lexer,
    /// 主 QC 路径（仅用于报错）。
    pub main_path: &'a Path,
    /// `qdir` —— 主 QC 所在目录。
    qdir: PathBuf,
    /// 累积中的描述。
    desc: ModelDesc,
    /// `$definebone` 的骨骼（顺序敏感，必须排在 SMD 骨骼之前）。
    import_bones: Vec<Bone>,
    /// `$cd` / `$pushd` 栈（`cddir[32]`）。元素是**已带结尾 `/`** 的前缀。
    cddir: Vec<String>,
    /// `$definevariable` 之外：`$modelname` 是否已出现。
    outname: Option<String>,
    /// `$scale` 的当前值（`g_defaultscale`）。
    default_scale: f32,
    default_motion_rollback: f32,
    default_adjust: [f32; 3],
    default_rotation: [f32; 3],
    /// 头部 flags 累加（`gflags`）。
    gflags: i32,
    /// `$staticprop`。
    static_prop: bool,
    /// `$contents` 的当前值（`s_nDefaultContents`，初值 `CONTENTS_SOLID`）。
    contents: i32,
    /// `$texturegroup` 的组（`[命令][family][组内第 j 项]` 的材质名）。
    ///
    /// 官方只支持一条 `$texturegroup`（`SetSkinValues` 只读 `[0]`），
    /// 但这里保留全部以便报错更清楚。
    texture_groups: Vec<Vec<Vec<String>>>,
    /// 已按「遇到顺序」注册的材质名（`g_texture[].name`）。
    texture_order: Vec<String>,
    /// 材质名 → `texture_order` 下标。
    texture_index: HashMap<String, usize>,
    /// 序列名集合（查重）。
    sequence_names: HashSet<String>,
    /// 动画名集合（查重）。
    animation_names: HashSet<String>,
    /// `$weightlist` 是否出现过（暂只记录，见 `PROGRESS.md` §1.1）。
    pub weightlists: Vec<String>,
    /// 所有被引用的网格/动画文件（相对 qdir），供骨骼与材质扫描。
    referenced_files: Vec<String>,
    /// `referenced_files` 里哪些是**网格源**（`$model` / `$bodygroup { studio … }`）。
    ///
    /// 官方 `Load_Source` 的 `isActiveModel` 只有 `Option_Studio` 一处传 `true`
    /// （`studiomdl.cpp:963`），而 `TagUsedBones` 的顶点权重循环正由它把关
    /// （`simplify.cpp:3441-3442`）—— **只有网格源的顶点权重**能让骨骼入表。
    /// 动画源 / `$lod` 的 `replacemodel` / `$collisionmodel` 的顶点权重一概不算
    /// （它们的 `isActiveModel` 都是默认的 `false`，见 `studiomdl.h:1127`）。
    mesh_sources: HashSet<String>,
    /// 合成附着点（`$illumposition x y z <骨骼>`）在 QC 里的**源位置**。
    ///
    /// 按 push 顺序与 `desc.attachments` 里的 `synthetic` 项一一对应。
    /// 存在的唯一理由：那条命令的骨骼是否有效**只能等到 `finish()`**
    /// 才判定（要先读遍所有 SMD 的 `nodes`），而 `Attachment` 本身不带
    /// 位置信息 —— 没有它，错误只能报在「主 QC 第 0 行」。
    synthetic_att_locs: Vec<(String, usize)>,
    /// 解析出的错误（累积，最后一起报）。
    errors: Vec<QcError>,
    /// `$bonemerge` 的骨骼名（骨骼表建好后回填）。
    bonemerge_names: Vec<String>,
    /// `$unlockdefinebones` 是否出现（`g_bOverridePreDefinedBones`）。
    ///
    /// # L4D2 的语义与 darkm SDK **相反**
    ///
    /// L4D2 `studiomdl.exe` 的命令表里有 `$unlockdefinebones`
    /// （`0x578F99`），**没有** `$lockdefinebones`；且 `-overridedefinebones`
    /// 的 help 写着 `equivalent to specifying $unlockdefinebones in .qc file`。
    ///
    /// ⟹ **默认锁定** `$definebone` 给的参考姿态，写了本命令才让 **SMD 赢**。
    ///
    /// 真 studiomdl 裁决（`docs/_probe/oracle_unlockdefinebones.js`）：
    ///
    /// | 变体 | 官方骨骼 `b1.z` | 官方顶点 z |
    /// |---|---|---|
    /// | 无 flag | **20**（`$definebone` 赢） | 20 |
    /// | `$unlockdefinebones` | **10**（SMD 赢） | 10 |
    ///
    /// > darkm SDK 的 `g_bOverridePreDefinedBones` **默认 true**
    /// > （`studiomdl.cpp:59`），只有 `$lockdefinebones` 置 false
    /// > （`:5733`）—— 那是**旧版**语义，**不要照抄**。
    unlock_define_bones: bool,
    /// `$jointsurfaceprop` 的待办（骨骼表建好后回填）。
    joint_surface_props: Vec<(String, String)>,
    /// LOD blocks apply across all bodyparts.
    pending_lods: Vec<ParsedLod>,
    /// `$model` 块里 `flexfile` 设的当前 `.vta`（官方是**粘性变量**）。
    pending_vta: Option<String>,
    /// **顶层** `$sectionframes <每段帧数> <阈值>` 的全局值。
    ///
    /// 官方是全局变量；mdlc 的 IR 是逐序列字段，所以在 `finish()` 里
    /// 回填给尚未显式设置的序列。
    global_section_frames: Option<(i32, i32)>,
    current_hitbox_set: usize,
    lowercase_sequences: bool,
    default_fade_in: f32,
    default_fade_out: f32,
}

impl<'a> Parser<'a> {
    /// 新建。
    pub fn new(lex: Lexer, main_path: &'a Path) -> Self {
        let qdir = lex.qdir.clone();
        Self {
            lex,
            main_path,
            qdir,
            desc: ModelDesc {
                model: ModelMeta {
                    center_bones_on_vertices: false,
                    collapse_bones: false,
                    collapse_bones_aggressive: false,
                    always_collapse: Vec::new(),
                    screen_alignments: Vec::new(),
                    max_vertices: None,
                    lock_bone_lengths: false,
                    internal_name: None,
                    source_search_paths: Vec::new(),
                    hitgroups: Vec::new(),
                    preserve_triangle_order: false,
                    zbrush: false,
                    rotation_limits: Vec::new(),
                    rotation_alignments: Vec::new(),
                    root_bone: None,
                    allowed_root_lods: 0,
                    constant_directional_light_dot: None,
                    check_uv: None,
                    source_dependencies: Vec::new(),
                    append_sources: Vec::new(),
                    virtual_vtas: Vec::new(),
                    bone_save_frames: Vec::new(),
                    clamp_worldspace: None,
                    bbox_only_vertices: false,
                    bone_flex_drivers: Vec::new(),
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
                    name: String::new(),
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
                    // 缺省走**与 TOML 同一个缺省函数**（避免两处各写一遍而漂移）。
                    // 两者都能被 mdlc 扩展命令改写（`$optimizevtx` /
                    // `$splitoversizedmeshes` / `$nosplitoversizedmeshes`）。
                    split_oversized_meshes: crate::model::default_true(),
                    key_values: None,
                    pose_parameters: Vec::new(),
                    realign_bones: false,
                    anim_block_size: None,
                    section_frames: None,
                    material_renames: Vec::new(),
                },
                physics: Physics::default(),
                materials: Materials::default(),
                bones: Vec::new(),
                bodyparts: Vec::new(),
                hitboxes: Hitboxes::default(),
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
            import_bones: Vec::new(),
            cddir: vec![String::new()],
            outname: None,
            default_scale: 1.0,
            default_motion_rollback: crate::model::default_motion_rollback(),
            default_adjust: [0.0; 3],
            default_rotation: crate::ani_writer::ROOT_REFERENCE_ANGLES,
            gflags: 0,
            static_prop: false,
            contents: 1,
            texture_groups: Vec::new(),
            texture_order: Vec::new(),
            texture_index: HashMap::new(),
            sequence_names: HashSet::new(),
            animation_names: HashSet::new(),
            weightlists: Vec::new(),
            referenced_files: Vec::new(),
            mesh_sources: HashSet::new(),
            synthetic_att_locs: Vec::new(),
            errors: Vec::new(),
            bonemerge_names: Vec::new(),
            unlock_define_bones: false,
            joint_surface_props: Vec::new(),
            pending_lods: Vec::new(),
            pending_vta: None,
            global_section_frames: None,
            current_hitbox_set: 0,
            lowercase_sequences: false,
            default_fade_in: 0.2,
            default_fade_out: 0.2,
        }
    }

    /// 当前 `cddir` 前缀（`cddir[numdirs]`）。
    fn cd_prefix(&self) -> String {
        self.cddir.last().cloned().unwrap_or_default()
    }

    // ---------------------------------------------------------------
    // token 辅助
    // ---------------------------------------------------------------

    fn tok(&mut self, crossline: bool) -> Result<Token, QcError> {
        self.lex.expect_token(crossline)
    }

    /// 取一个浮点参数（官方 `verify_atof`）。
    fn f(&mut self) -> Result<f32, QcError> {
        let t = self.tok(false)?;
        parse_f32(&t.text).ok_or_else(|| {
            QcError::new(
                t.file.clone(),
                t.line,
                crate::tr_fmt!(
                    "expected a number, got %{text}",
                    text = format!("{:?}", t.text)
                ),
            )
        })
    }

    /// 取一个整数参数（官方 `verify_atoi`）。
    fn i(&mut self) -> Result<i32, QcError> {
        let t = self.tok(false)?;
        parse_i32(&t.text).ok_or_else(|| {
            QcError::new(
                t.file.clone(),
                t.line,
                crate::tr_fmt!(
                    "expected an integer, got %{text}",
                    text = format!("{:?}", t.text)
                ),
            )
        })
    }

    /// 取三个浮点（一个 Vector）。
    fn v3(&mut self) -> Result<[f32; 3], QcError> {
        Ok([self.f()?, self.f()?, self.f()?])
    }

    /// 是否还有 token 在本行（`TokenAvailable`）。
    fn avail(&mut self) -> bool {
        self.lex.token_available()
    }

    // ---------------------------------------------------------------
    // 主循环（`ParseScript`）
    // ---------------------------------------------------------------

    /// 跑完整个脚本，产出描述。
    pub fn run(&mut self) -> Result<ModelDesc, Vec<QcError>> {
        while let Some(t) = self.lex.next_token(true).map_err(|e| vec![e])? {
            if let Err(e) = self.dispatch(&t) {
                self.errors.push(e);
                // 出错后不再继续（后续 token 边界不可信）。
                break;
            }
        }
        if !self.errors.is_empty() {
            return Err(std::mem::take(&mut self.errors));
        }
        self.finish()
    }

    /// 分发一条顶层命令。
    fn dispatch(&mut self, t: &Token) -> Result<(), QcError> {
        if self.dispatch_compat(t)? {
            return Ok(());
        }
        let name = t.text.to_ascii_lowercase();
        match name.as_str() {
            "$checkuv" => self.cmd_checkuv(),
            "$modelname" => self.cmd_modelname(),
            "$cd" => self.cmd_cd(),
            "$pushd" => self.cmd_pushd(),
            "$popd" => self.cmd_popd(),
            "$scale" => self.cmd_scale(),
            "$motionrollback" => {
                let value = self.f()?;
                if !value.is_finite() || value < 0.0 {
                    return Err(self.lex.error("$motionrollback requires a finite nonnegative duration"));
                }
                self.default_motion_rollback = value;
                Ok(())
            }
            "$origin" => self.cmd_origin(),
            "$upaxis" => self.cmd_upaxis(),
            "$cdmaterials" => self.cmd_cdmaterials(),
            "$surfaceprop" => {
                let v = self.tok(false)?;
                self.desc.model.surface_prop = Some(v.text);
                Ok(())
            }
            "$contents" => self.cmd_contents(),
            "$jointcontents" => self.cmd_jointcontents(),
            "$eyeposition" => {
                let v = self.v3()?;
                self.desc.model.eye_position = Some(v);
                Ok(())
            }
            "$illumposition" => {
                let v = self.v3()?;
                self.desc.model.illum_position = Some(v);
                // 可选的**第 4 个 token**（必须同一行）是骨骼名 —— 官方
                // `Cmd_Illumposition` 的 `GetToken(false)`。三处已由真 exe 钉死：
                //
                // ① 它让官方新建一个名为 `__illumPosition` 的**合成附着点**
                //    （绑该骨骼、零旋转、平移 = 这三个坐标、`type` 带 `IS_RIGID`），
                //    并让 `studiohdr2.illumpositionattachmentindex`（`+0x08`）指向它
                //    （**1 起**下标）—— `oracle_illumposition3/4.js`；
                // ② 此时 `illumposition`（`0x5C`）落盘**原样 `[x,y,z]`、不做轴变换**，
                //    而 3 参数形式落盘 `[-y,x,z]` —— `oracle_illumposition6.js`；
                // ③ 不判 `$`：`$illumposition 0 0 0 $attachment ...` 会把
                //    `$attachment` 当骨骼名吃掉 —— `oracle_illumposition9.js`。
                if self.avail() {
                    let bone = self.tok(false)?.text;
                    self.desc.model.illum_position_from_bone = true;
                    self.synthetic_att_locs.push((t.file.clone(), t.line));
                    self.desc.attachments.push(Attachment {
                        name: "__illumPosition".to_string(),
                        bone,
                        position: Some(v),
                        rotation: None,
                        absolute: false,
                        absolute_rotation: None,
                        // 保活语义 = `rigid`（`oracle_illumposition10.js` 证实
                        // 合成附着点与 `rigid` 附着点对骨骼表的效应逐字节一致）。
                        rigid: true,
                        flags: Some(0),
                        synthetic: true,
                        // 由 `finish()` 的「2b」段在骨骼表建好之后回填 ——
                        // 那时才知道它绑的骨骼有没有被收骨判据丢掉。
                        resolved: None,
                    });
                }
                Ok(())
            }
            "$maxeyedeflection" => {
                let v = self.f()?;
                self.desc.model.max_eye_deflection = Some(v);
                Ok(())
            }
            "$bbox" => {
                let min = self.v3()?;
                let max = self.v3()?;
                self.desc.model.hull_min = Some(min);
                self.desc.model.hull_max = Some(max);
                Ok(())
            }
            "$cbox" => {
                let min = self.v3()?;
                let max = self.v3()?;
                if !(0..3).all(|axis| min[axis].is_finite() && max[axis].is_finite() && min[axis] <= max[axis]) {
                    return Err(QcError::new(t.file.clone(), t.line, "$cbox requires finite ordered minimum and maximum coordinates"));
                }
                self.desc.model.view_bb_min = Some(min);
                self.desc.model.view_bb_max = Some(max);
                Ok(())
            }
            "$staticprop" => {
                self.static_prop = true;
                self.gflags |= FLAG_STATIC_PROP;
                Ok(())
            }
            "$defaultfadein" | "$defaultfadeout" => {
                let value = self.f()?;
                if !value.is_finite() || value < 0.0 {
                    return Err(self.lex.error("default fade duration must be finite and nonnegative"));
                }
                if name == "$defaultfadein" {
                    self.default_fade_in = value;
                } else {
                    self.default_fade_out = value;
                }
                Ok(())
            }
            "$autocenter" => {
                self.desc.model.autocenter = true;
                Ok(())
            }
            "$root" => {
                if self.avail() {
                    self.desc.model.root_bone = Some(self.tok(false)?.text);
                }
                Ok(())
            }
            "$limitrotation" => {
                let bone = self.tok(false)?.text;
                let mut sequences = Vec::new();
                while self.avail() {
                    sequences.push(self.tok(false)?.text);
                }
                self.desc.model.rotation_limits.push(crate::model::BoneRotationLimit {
                    bone,
                    sequences,
                });
                Ok(())
            }
            "$forcerealign" => {
                let bone = self.tok(false)?.text;
                self.tok(false)?;
                let rotation = self.v3()?;
                self.desc.model.forced_realign.push(crate::model::ForcedBoneRealign {
                    bone,
                    rotation,
                });
                Ok(())
            }
            "$realignbones" => {
                self.desc.model.realign_bones = true;
                Ok(())
            }
            "$skipboneinbbox" => {
                self.desc.model.skip_bone_in_bbox = true;
                Ok(())
            }
            "$optimizevtx" => {
                self.desc.model.optimize_vtx = true;
                Ok(())
            }
            // 默认已是 `true`，这条存在的唯一理由是**反向覆盖**：`$include`
            // 进来的 `.qci` 里写了 `$nosplitoversizedmeshes` 之后，主 QC
            // 还得能开回来（QC 自上而下、后写覆盖）。
            "$splitoversizedmeshes" => {
                self.desc.model.split_oversized_meshes = true;
                Ok(())
            }
            "$nosplitoversizedmeshes" => {
                self.desc.model.split_oversized_meshes = false;
                Ok(())
            }
            "$animblocksize" => {
                let v = self.i()?;
                self.desc.model.anim_block_size = Some(v);
                Ok(())
            }
            "$keyvalues" => {
                let text = self.option_keyvalues()?;
                self.desc.model.key_values = Some(text);
                Ok(())
            }
            "$collisiontext" => self.cmd_collisiontext(),
            "$sectionframes" => self.cmd_sectionframes_top(),
            "$poseparameter" => self.cmd_poseparameter(),
            "$texturegroup" => self.cmd_texturegroup(),
            "$body" => self.cmd_body(),
            "$bodygroup" => self.cmd_bodygroup(),
            "$model" => self.cmd_model(),
            "$sequence" => self.cmd_sequence(),
            "$animation" => self.cmd_animation(),
            "$definebone" => self.cmd_definebone(),
            "$controller" => self.cmd_controller(),
            "$bonemerge" => self.cmd_bonemerge(),
            "$attachment" => self.cmd_attachment(),
            "$hboxset" => self.cmd_hboxset(),
            "$hbox" => self.cmd_hbox(),
            "$calctransitions" => {
                self.desc.model.transition_multistage = true;
                Ok(())
            }
            "$skiptransition" => self.cmd_skiptransition(),
            "$ikchain" => self.cmd_ikchain(),
            "$ikautoplaylock" => self.cmd_ikautoplaylock(),
            "$includemodel" => self.cmd_includemodel(),
            "$lod" => self.cmd_lod(false),
            "$shadowlod" => self.cmd_lod(true),
            "$jigglebone" => self.cmd_jigglebone(),
            "$proceduralbones" => self.cmd_proceduralbones(),
            "$collisionmodel" => self.cmd_collisionmodel(false),
            "$collisionjoints" => self.cmd_collisionmodel(true),
            "$jointsurfaceprop" => self.cmd_jointsurfaceprop(),
            "$weightlist" => self.cmd_weightlist(),
            "$defaultweightlist" => self.cmd_defaultweightlist(),
            "$declaresequence" => self.cmd_declaresequence(),
            "$declareanimation" => self.cmd_declareanimation(),
            "$cmdlist" => self.cmd_cmdlist(),
            "$continue" => self.cmd_modify_animation("continue"),
            "$append" => self.cmd_modify_animation("append"),
            "$prepend" => self.cmd_modify_animation("prepend"),
            "$definevariable" => {
                // 词法层已消费（`scriplib.cpp` 在 `GetToken` 内部处理）。
                // 走到这里说明它作为**普通 token** 出现了 —— 官方也一样
                // （`GetToken` 只在词法阶段拦截；这里不会到达）。
                //
                // ⚠️ **故意不支持 `$redefinevariable`**：它是 NekoMDL 扩展，
                // 官方没有（exe 串扫描 0 命中），且官方对它报
                // `bad command $redefinevariable`
                // （`docs/_probe/oracle_redefinevariable.js`）。它落到下面的
                // `other` 分支报错，与官方一致；而它想表达的需求已由
                // `$definevariable` 的**覆盖语义**满足
                // （见 `crate::qc::lexer::Lexer::define_variable`）。
                Ok(())
            }
            // ---- 已知但**故意不支持**的命令（见各分支注释）----
            "$nekomodel" => Err(self.lex.error(crate::tr(
                "$nekomodel points at a DMX source; mdlc does not implement DMX (the official code delegates to dmxconvert.exe as well)",
            ))),
            "$jointconstrain" => self.cmd_jointconstrain(),
            "$animatedfriction" => self.cmd_animatedfriction(),
            "$noselfcollisions" => {
                self.desc.physics.no_self_collisions = true;
                Ok(())
            }
            "$jointcollide" => self.cmd_collision_pair(false),
            "$jointmerge" => self.cmd_collision_pair(true),
            "$jointskip" => {
                let bone = self.tok(false)?.text;
                self.desc.physics.skip.push(bone);
                Ok(())
            }
            "$mass" | "$masscenter" | "$automass" | "$maxconvexpieces" | "$phyname" | "$concave"
            | "$damping" | "$rotdamping" | "$inertia" | "$drag" | "$rootbone" => {
                self.cmd_physics_kv(&name)
            }
            // ---- 纯标志位（写进 `extra_flags`）----
            "$opaque" => self.set_flag(FLAG_OPAQUE),
            "$mostlyopaque" => self.set_flag(FLAG_MOSTLY_OPAQUE),
            "$noforcedfade" => self.set_flag(FLAG_NO_FORCED_FADE),
            "$casttextureshadows" => self.set_flag(FLAG_CAST_TEXTURE_SHADOWS),
            "$ambientboost" => self.set_flag(FLAG_AMBIENT_BOOST),
            "$donotcastshadows" => self.set_flag(FLAG_DO_NOT_CAST_SHADOWS),
            "$forcephonemecrossfade" => self.set_flag(FLAG_FORCE_PHONEME_CROSSFADE),
            "$constantdirectionallight" => {
                let value = self.f()?;
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err(self.lex.error("$constantdirectionallight must be finite and in 0..=1"));
                }
                self.desc.model.constant_directional_light_dot = Some((value * 255.0) as u8);
                self.set_flag(FLAG_CONSTANT_DIRECTIONAL_LIGHT_DOT)
            }
            // ---- `$unlockdefinebones`：让 SMD 骨架覆盖 `$definebone` ----
            //
            // L4D2 的语义与 darkm SDK 相反（详见字段文档）。这里只置标志，
            // 真正的覆盖在 `finish()` 建骨骼表时做。
            "$unlockdefinebones" => {
                self.unlock_define_bones = true;
                Ok(())
            }
            "$lcaseallsequences" => {
                self.lowercase_sequences = true;
                Ok(())
            }
            "$minlod" => {
                let value = self.i()?;
                if value < 0 {
                    return Err(self.lex.error("$minlod must be nonnegative"));
                }
                self.desc.model.minimum_lod = value as u32;
                Ok(())
            }
            "$allowrootlods" => {
                let value = self.i()?;
                if !(0..=255).contains(&value) {
                    return Err(self.lex.error("$allowrootlods must be in 0..=255"));
                }
                self.desc.model.allowed_root_lods = value as u8;
                Ok(())
            }
            "$hierarchy" | "$heirarchy" => {
                let child = self.tok(false)?.text;
                let parent = self.tok(false)?.text;
                self.desc.model.bone_hierarchy.push([child, parent]);
                Ok(())
            }
            "$renamebone" => {
                let from = self.tok(false)?.text;
                let to = self.tok(false)?.text;
                self.desc.model.bone_renames.push([from, to]);
                Ok(())
            }
            "$renamematerial" => {
                let from = self.tok(false)?.text;
                let to = self.tok(false)?.text;
                self.desc.model.material_renames.push([from, to]);
                Ok(())
            }
            "$filebuffersize" => {
                let _ = self.i()?;
                Ok(())
            }
            "$decal" | "$ignorez" | "$vertexcolor" | "$jigglebonerealign" | "$maxbones" => {
                Err(self.unsupported(t, &name))
            }
            other => Err(self.lex.error(crate::tr_fmt!(
                "unknown QC command %{cmd} (the official code says bad command)",
                cmd = format!("{other:?}")
            ))),
        }
    }

    fn cmd_checkuv(&mut self) -> Result<(), QcError> {
        let mut checks = crate::uv_checks::UvChecks::default();
        let mut any = false;
        while self.avail() {
            let token = self.tok(false)?;
            match token.text.to_ascii_lowercase().as_str() {
                "0to1" => checks.normalized = true,
                "overlap" => checks.overlap = true,
                "inverse" => checks.inverse = true,
                "gutter" => {
                    let mut values = [512, 5];
                    for value in &mut values {
                        if !self.avail() {
                            break;
                        }
                        let number = self.tok(false)?;
                        if !number
                            .text
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_digit)
                        {
                            self.lex.unget(number);
                            break;
                        }
                        *value = number.text.parse::<u32>().map_err(|_| {
                            self.lex
                                .error("$checkuv gutter values must be positive integers")
                        })?;
                        if *value == 0 {
                            return Err(self
                                .lex
                                .error("$checkuv gutter values must be positive integers"));
                        }
                    }
                    checks.gutter = Some(values);
                }
                _ => {
                    return Err(QcError::new(
                        token.file,
                        token.line,
                        format!("unknown $checkuv option {:?}", token.text),
                    ));
                }
            }
            any = true;
        }
        self.desc.model.check_uv = Some(if any {
            checks
        } else {
            crate::uv_checks::UvChecks::all()
        });
        Ok(())
    }

    fn cmd_origin(&mut self) -> Result<(), QcError> {
        self.default_adjust = self.v3()?;
        if self.avail() {
            self.default_rotation[2] = (self.f()? + 90.0).to_radians();
        }
        if self
            .default_adjust
            .iter()
            .chain(self.default_rotation.iter())
            .any(|v| !v.is_finite())
        {
            return Err(self
                .lex
                .error("$origin coordinates and rotation must be finite"));
        }
        self.store_source_transform();
        Ok(())
    }

    fn cmd_upaxis(&mut self) -> Result<(), QcError> {
        let token = self.tok(false)?;
        let half_pi = std::f32::consts::FRAC_PI_2;
        self.default_rotation = match token.text.to_ascii_lowercase().as_str() {
            "x" => [0.0, half_pi, half_pi],
            "-x" => [0.0, -half_pi, half_pi],
            "y" => [half_pi, 0.0, half_pi],
            "-y" => [-half_pi, 0.0, half_pi],
            "z" | "-z" => [0.0, 0.0, half_pi],
            _ => return Err(self.lex.error("$upaxis requires x, -x, y, -y, z or -z")),
        };
        self.store_source_transform();
        Ok(())
    }

    fn store_source_transform(&mut self) {
        self.desc.model.source_origin = Some(self.default_adjust);
        self.desc.model.source_rotation = Some(self.default_rotation);
    }

    fn unsupported(&self, token: &Token, command: &str) -> QcError {
        QcError::new(
            token.file.clone(),
            token.line,
            format!(
                "{command} is not supported yet; compilation stopped to avoid exporting a model with ignored settings"
            ),
        )
    }

    /// 设置一个头部 flag 位。
    fn set_flag(&mut self, bit: i32) -> Result<(), QcError> {
        self.gflags |= bit;
        Ok(())
    }

    /// 消费本行剩余 token。
    fn skip_rest_of_line(&mut self) {
        while self.avail() {
            if self.lex.next_token(false).ok().flatten().is_none() {
                break;
            }
        }
    }

    // ---------------------------------------------------------------
    // 各命令
    // ---------------------------------------------------------------

    fn cmd_modelname(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        // 官方：首字符是 `/` 或 `\` 时剥掉并警告。
        let name = if t.first() == b'/' || t.first() == b'\\' {
            t.text[1..].to_string()
        } else {
            t.text.clone()
        };
        self.outname = Some(name.clone());
        self.desc.model.name = name;
        Ok(())
    }

    fn cmd_cd(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        self.cddir = vec![format!(
            "{}/",
            t.text.replace('\\', "/").trim_end_matches('/')
        )];
        Ok(())
    }

    fn cmd_pushd(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        let cur = self.cd_prefix();
        self.cddir.push(format!(
            "{}{}/",
            cur,
            t.text.replace('\\', "/").trim_end_matches('/')
        ));
        Ok(())
    }

    fn cmd_popd(&mut self) -> Result<(), QcError> {
        if self.cddir.len() > 1 {
            self.cddir.pop();
        }
        Ok(())
    }

    fn cmd_scale(&mut self) -> Result<(), QcError> {
        let v = self.f()?;
        if !v.is_finite() || v <= 0.0 {
            return Err(self.lex.error("$scale must be a positive finite number"));
        }
        self.default_scale = v;
        Ok(())
    }

    fn cmd_cdmaterials(&mut self) -> Result<(), QcError> {
        while self.avail() {
            let t = self.tok(false)?;
            // 官方：末尾补 `/`（若没有），再 `Q_FixSlashes`。
            //
            // ⚠️ 空串**原样保留**（不补 `/`）—— 实测 `$cdmaterials ""`
            // 会产生一条空 cdtexture（受控实验 `cdexp{1,2,3}.qc`）。
            let s = if t.text.is_empty() {
                String::new()
            } else if t.text.ends_with('/') || t.text.ends_with('\\') {
                t.text.clone()
            } else {
                format!("{}/", t.text)
            };
            self.desc.materials.search_paths.push(s.replace('\\', "/"));
        }
        Ok(())
    }

    fn contents_flags(&mut self) -> Result<(i32, i32), QcError> {
        use crate::contents::{GRATE, LADDER, MONSTER, SOLID};
        let mut add = 0;
        let mut remove = 0;
        loop {
            let t = self.tok(false)?;
            match t.text.to_ascii_lowercase().as_str() {
                "grate" => {
                    add |= GRATE;
                    remove |= SOLID;
                }
                "ladder" => add |= LADDER,
                "solid" => add |= SOLID,
                "monster" => add |= MONSTER,
                "notsolid" => remove |= SOLID,
                _ => {}
            }
            if !self.avail() {
                break;
            }
        }
        Ok((add, remove))
    }

    fn cmd_contents(&mut self) -> Result<(), QcError> {
        let (add, remove) = self.contents_flags()?;
        self.contents = (self.contents | add) & !remove;
        Ok(())
    }

    fn cmd_jointcontents(&mut self) -> Result<(), QcError> {
        let bone = self.tok(false)?.text;
        let (add, remove) = self.contents_flags()?;
        self.desc
            .model
            .joint_contents
            .push((bone, (crate::contents::SOLID | add) & !remove));
        Ok(())
    }

    fn cmd_collisiontext(&mut self) -> Result<(), QcError> {
        let opening = self.tok(true)?;
        if opening.quoted || opening.text != "{" {
            return Err(QcError::new(
                opening.file,
                opening.line,
                "$collisiontext requires a block",
            ));
        }
        let mut depth = 1usize;
        let mut text = String::new();
        while let Some(token) = self.lex.next_token(true)? {
            match token.text.as_str() {
                "{" | "}" if token.quoted => {
                    return Err(QcError::new(
                        token.file,
                        token.line,
                        "Quoted braces cannot be represented in collision text",
                    ));
                }
                "{" => {
                    depth += 1;
                    if depth > 64 {
                        return Err(QcError::new(
                            token.file,
                            token.line,
                            "Collision text nesting is too deep",
                        ));
                    }
                    text.push_str("{\n");
                }
                "}" => {
                    depth -= 1;
                    if depth == 0 {
                        self.desc
                            .physics
                            .collision_text
                            .get_or_insert_default()
                            .push_str(&text);
                        return Ok(());
                    }
                    text.push_str("}\n");
                }
                value => {
                    if value.contains('"') || value.chars().any(char::is_control) {
                        return Err(QcError::new(
                            token.file,
                            token.line,
                            "Invalid collision text token",
                        ));
                    }
                    text.push('"');
                    text.push_str(value);
                    text.push_str("\" ");
                }
            }
            if text.len() > 1_048_576 {
                return Err(QcError::new(
                    token.file,
                    token.line,
                    "Collision text is too large",
                ));
            }
        }
        Err(QcError::new(
            opening.file,
            opening.line,
            "Unterminated collision text block",
        ))
    }

    /// `$keyvalues { ... }` —— 按官方 `Option_KeyValues` 原样重组文本。
    ///
    /// 官方把块内 token 重新拼成
    /// `mdlkeyvalue\n{\n` + 内容 + `}\n`，**第 2 层及更深**的 token 加引号。
    /// 这里产出**内层文本**（不含外层包装），由 `mdl_writer` 加包装
    /// —— 与 TOML 的 `key_values` 字段约定一致。
    fn option_keyvalues(&mut self) -> Result<String, QcError> {
        let t = self.tok(true)?;
        if t.text != "{" {
            return Ok(String::new());
        }
        let mut out = String::new();
        let mut level = 1;
        while let Some(t) = self.lex.next_token(true)? {
            if t.text == "}" {
                level -= 1;
                if level <= 0 {
                    break;
                }
                out.push_str(" }\n");
            } else if t.text == "{" {
                out.push_str("{\n");
                level += 1;
            } else if level > 1 {
                out.push('"');
                out.push_str(&t.text);
                out.push_str("\" ");
            } else {
                out.push_str(&t.text);
                out.push(' ');
            }
        }
        Ok(out)
    }

    /// **顶层** `$sectionframes <每段帧数> <阈值>`。
    ///
    /// # 为什么是顶层命令（而不是 `$sequence` 块内选项）
    ///
    /// 实测：`exp61.qc` / `f07.qc` / `t15_59.qc` 等都把它写在**顶层**
    /// （`$sequence` 之后另起一行），而 `$sequence` 的 `ParseSequence`
    /// 并不处理 `sectionframes` —— 它是 `Cmd_SectionFrames`
    /// （`studiomdl.cpp:1479` 附近的 `$animblocksize` 同一批全局命令）。
    ///
    /// # 作用域
    ///
    /// 官方把它设成**全局** `g_sectionframes` / `g_sectionframes_threshold`，
    /// 之后编译的**所有**序列都用它。mdlc 的 IR 是逐序列字段
    /// （`Sequence::section_frames`），所以这里记成「全局默认」，
    /// 在 `finish()` 里给**尚未显式设置**的序列回填。
    ///
    /// ⚠️ 官方在**写 `$sequence` 时**就读了全局值（`ParseSequence` 之后
    /// 在 `SimplifyModel` 里用），所以「`$sectionframes` 写在 `$sequence`
    /// 之后」对官方**无效**。mdlc 的逐序列字段天然表达不了这个时序 ——
    /// 实测语料里 23 个用例的写法都是「`$sequence` 之后写
    /// `$sectionframes`」，而官方产物**确实生效了**（`exp61` 等有产物），
    /// 所以这里按「生效」处理（与官方实测行为一致，而不是与源码时序一致）。
    fn cmd_sectionframes_top(&mut self) -> Result<(), QcError> {
        let len = self.i()?;
        let thr = if self.avail() { self.i()? } else { 120 };
        self.global_section_frames = Some((len, thr));
        Ok(())
    }

    /// `LookupPoseParameter`（`studiomdl.cpp:2333-2352`）—— 按名查姿势参数，
    /// **查不到就新建一个**并返回新下标。
    ///
    /// ```c
    /// for (i = 0; i < g_numposeparameters; i++)
    ///     if (!stricmp( name, g_pose[i].name )) return i;
    /// strcpyn( g_pose[i].name, name );      // ← 就地新建（min/max 保持 0）
    /// g_numposeparameters = i + 1;
    /// return i;
    /// ```
    ///
    /// ⚠️ **不区分大小写**（`stricmp`），且**不去重**同名（与
    /// [`Self::cmd_poseparameter`] 的注释一致：`ipp3.qc` 写两次同名会得到
    /// 两条 —— 但那是 `$poseparameter` 命令本身的行为；`LookupPoseParameter`
    /// 走的是**先查后建**，所以它能命中已存在的那条）。
    fn poseparam_index(&mut self, name: &str) -> usize {
        if let Some(i) = self
            .desc
            .model
            .pose_parameters
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(name))
        {
            return i;
        }
        self.desc.model.pose_parameters.push(PoseParameter {
            name: name.to_string(),
            start: 0.0,
            end: 0.0,
            loop_mode: None,
        });
        self.desc.model.pose_parameters.len() - 1
    }

    /// `$poseparameter <名> <min> <max> [wrap | loop <值>]`。
    fn cmd_poseparameter(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let min = if self.avail() { self.f()? } else { 0.0 };
        let max = if self.avail() { self.f()? } else { 0.0 };
        let mut loop_mode = None;
        while self.avail() {
            let t = self.tok(false)?;
            match t.text.to_ascii_lowercase().as_str() {
                "wrap" => loop_mode = Some(PoseLoop::Wrap(PoseWrap::Wrap)),
                "loop" => {
                    let v = self.f()?;
                    loop_mode = Some(PoseLoop::Explicit(v));
                }
                _ => {}
            }
        }
        // ⚠️ 官方 `LookupPoseParameter` **不去重**（同名会新建一条）——
        // 实测 `ipp3.qc` 写两次 `$poseparameter "x"` 得到两条。
        self.desc.model.pose_parameters.push(PoseParameter {
            name,
            start: min,
            end: max,
            loop_mode,
        });
        Ok(())
    }

    /// `$texturegroup "name" { { "a" "b" } { "a2" "b2" } }`。
    ///
    /// 只记录**组结构**；纹理顺序与 skin 表在 `finish()` 里按
    /// `SetSkinValues` 推导（那时才知道 SMD 里的材质）。
    ///
    /// # 语义（`Cmd_TextureGroup`，`studiomdl.cpp:4740`）
    ///
    /// 外层 `{}` 是命令块（`depth` 1），**内层每个 `{}` 是一个 family**
    /// （`depth` 2）。官方在 `depth == 2` 时把 token 当材质名注册，
    /// 遇到 `}` 时 `group++`（换下一个 family）。
    ///
    /// 每个 family 是「一组材质名」，下标 `j` 是**组内位置** ——
    /// `SetSkinValues` 用它把「family 0 的第 j 个槽位」重映射到
    /// 「family i 的第 j 个材质」。
    fn cmd_texturegroup(&mut self) -> Result<(), QcError> {
        if self.avail() {
            let _ = self.tok(false)?; // 组名（官方只当注释用）
        }
        let mut groups: Vec<Vec<String>> = Vec::new();
        let mut depth = 0i32;
        while let Some(t) = self.lex.next_token(true)? {
            if t.text == "{" {
                depth += 1;
                if depth == 2 {
                    groups.push(Vec::new());
                }
            } else if t.text == "}" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if depth == 2
                && let Some(g) = groups.last_mut()
            {
                g.push(t.text);
            }
        }
        // 官方只支持**一个** `$texturegroup`（`g_numtexturegroups` 递增，
        // 但 `SetSkinValues` 只读 `[0]`）。这里保留全部以便报错更清楚，
        // 但 `finish()` 只用第一个 —— 与官方一致。
        self.texture_groups.push(groups);
        Ok(())
    }

    /// `$body <name> <smd> [opts]`（`Cmd_Body`，`studiomdl.cpp:1034`）。
    fn cmd_body(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let base = self.next_bodypart_base();
        let mut bp = BodyPart {
            name,
            base: Some(base),
            models: Vec::new(),
        };
        let m = self.option_studio(None)?;
        bp.models.push(m);
        self.desc.bodyparts.push(bp);
        Ok(())
    }

    /// `$bodygroup <name> { studio "x.smd" blank }`（`Cmd_Bodygroup`，`989`）。
    fn cmd_bodygroup(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let base = self.next_bodypart_base();
        let mut bp = BodyPart {
            name,
            base: Some(base),
            models: Vec::new(),
        };
        loop {
            let t = self.tok(true)?;
            if t.text == "{" {
                continue;
            }
            if t.text == "}" {
                break;
            }
            match t.text.to_ascii_lowercase().as_str() {
                "studio" => {
                    let m = self.option_studio(None)?;
                    bp.models.push(m);
                }
                "blank" => {
                    bp.models.push(BodyModel {
                        sphere_normals: Vec::new(),
                        smd: String::new(),
                        name: Some("blank".to_string()),
                        flip_triangles: true,
                        lods: Vec::new(),
                        eyeballs: Vec::new(),
                        flexes: Vec::new(),
                        src_parts: Vec::new(),
                        src_material: None,
                        src_scale: None,
                        src_axis: None,
                        src_shape_keys: Vec::new(),
                        src_shape_key_order: Vec::new(),
                        src_shape_key_ignore: false,
                        input_scale: None,
                    });
                }
                other => {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "unknown $bodygroup option %{opt}",
                            opt = format!("{other:?}")
                        ),
                    ));
                }
            }
        }
        self.desc.bodyparts.push(bp);
        Ok(())
    }

    /// bodypart 的 `base`（`g_bodypart[n].base = prev.base * prev.nummodels`）。
    fn next_bodypart_base(&self) -> i32 {
        match self.desc.bodyparts.last() {
            None => 1,
            Some(prev) => {
                let b = prev.base.unwrap_or(1);
                b * prev.models.len().max(1) as i32
            }
        }
    }

    /// `Option_Studio`（`studiomdl.cpp:917`）：读文件名 + 行内选项。
    ///
    /// `name_override` 给 `$model` 用（它把 bodypart 名也当 model 名）。
    fn option_studio(&mut self, name_override: Option<String>) -> Result<BodyModel, QcError> {
        let t = self.tok(false)?;
        let filename = t.text.clone();
        let mut model_name = name_override;
        // ⚠️ **`flip_triangles` 默认是 `true`（翻转），`reverse` 把它关掉。**
        //
        // `studiomdl.cpp:926`（`Cmd_Body`）/ `:7040`（`Cmd_Studio`）在每条
        // `$body`/`$studio` 开头都 `flip_triangles = 1;`，只有写了 `reverse`
        // 才置 `0`（`:935`）。**方向容易记反** —— 写 `reverse` 是
        // 「**不要**翻转」，不是「翻转」。
        let mut flip_triangles = true;

        // ---- mdlc 扩展：`src*`（见 `docs/fbx-support.md` §4.3）----
        //
        // 只对 `.fbx` 源有意义，但**不在这里按扩展名报错**：文件名可能
        // 还要过 `resolve_src`（`$pushd` 前缀），而且「源换成 SMD 了但
        // 选项还留着」这种情况，交给 `compile()` 在真读到源之后判定更有用。
        let mut src_parts: Vec<String> = Vec::new();
        let mut src_material: Option<String> = None;
        let mut src_scale: Option<f32> = None;
        let mut input_scale = self.default_scale;
        let mut src_axis: Option<String> = None;
        let mut src_shape_keys: Vec<String> = Vec::new();
        let mut src_shape_key_order: Vec<String> = Vec::new();
        let mut src_shape_key_ignore = false;

        // 行内选项。
        while self.avail() {
            let o = self.tok(false)?;
            match o.text.to_ascii_lowercase().as_str() {
                "reverse" => flip_triangles = false,
                "scale" => {
                    input_scale = self.f()?;
                    if !input_scale.is_finite() || input_scale <= 0.0 {
                        return Err(self
                            .lex
                            .error("studio scale must be a positive finite number"));
                    }
                }
                "faces" | "bias" => {
                    let _ = self.tok(false)?;
                }
                "srcpart" => {
                    // ⚠️ **每次只读一个 token**，可重复写。
                    //
                    // 读到行尾看起来更省事（`srcpart "body" "hair"`），但会
                    // **吞掉后面的选项**：`srcpart "hat" srcaxis "y"` 里的
                    // `srcaxis` 会变成第二个网格名（实测 T9 夹具）。
                    // 网格名带空格由引号解决（`srcpart "my mesh"`），
                    // 不需要靠「读整行」来支持。
                    src_parts.push(self.tok(false)?.text);
                }
                "srcmaterial" => {
                    src_material = Some(self.tok(false)?.text);
                }
                "srcname" => model_name = Some(self.tok(false)?.text),
                "srcscale" => {
                    let t = self.tok(false)?;
                    let v: f32 = t.text.parse().map_err(|_| {
                        QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcscale needs a number, got %{text}",
                                text = format!("{:?}", t.text)
                            ),
                        )
                    })?;
                    if !v.is_finite() || v <= 0.0 {
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcscale must be a positive finite number, got %{v}",
                                v = v
                            ),
                        ));
                    }
                    src_scale = Some(v);
                }
                "srcaxis" => {
                    let t = self.tok(false)?;
                    if crate::fbx::ForcedAxis::parse(&t.text).is_none() {
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcaxis only accepts \"auto\" / \"y\" / \"z\" (also yup / y-up / zup / z-up, case-insensitive), got %{text}",
                                text = format!("{:?}", t.text)
                            ),
                        ));
                    }
                    src_axis = Some(t.text);
                }
                "srcshapekey" => {
                    // ⚠️ 每次只读一个 token（理由同上面的 `srcpart`）：
                    // 读到行尾会把后面的 `srcshapekeyignore` 之类当成形状名。
                    src_shape_keys.push(self.tok(false)?.text);
                }
                "srcshapekeyorder" => {
                    src_shape_key_order.push(self.tok(false)?.text);
                }
                "srcshapekeyignore" => src_shape_key_ignore = true,
                "{" => {
                    self.lex.unget(o);
                    break;
                }
                other => {
                    return Err(QcError::new(
                        o.file.clone(),
                        o.line,
                        crate::tr_fmt!("unknown studio option %{opt}", opt = format!("{other:?}")),
                    ));
                }
            }
        }

        // `srcshapekeyignore` 与另外两个互斥（见 `ModelDesc::validate` 的同名检查）。
        // 这里也报一次：`validate()` 报的是 TOML 的字段路径，对 QC 用户没意义。
        if src_shape_key_ignore && (!src_shape_keys.is_empty() || !src_shape_key_order.is_empty()) {
            return Err(self.lex.error(crate::tr(
                "`srcshapekeyignore` cannot be given together with `srcshapekey` / `srcshapekeyorder`: the former means \"no morph targets at all\", the latter two mean \"take them by name\"",
            )));
        }

        let smd = self.resolve_src(&filename);
        if self.desc.model.first_source.is_none() {
            self.desc.model.first_source = Some(smd.clone());
        }
        self.referenced_files.push(smd.clone());
        // ⭐ 这里是**唯一**的「网格源」标记点 —— 对应官方
        // `Load_Source( pmodel->filename, "", false, true )`
        // （`studiomdl.cpp:963`，全树唯一 `isActiveModel = true`）。
        // 官方即使命中缓存也会补置该标志（`studiomdl.cpp:1589-1590`），
        // 所以「同一个 SMD 既当网格源又当动画源」时按**网格源**算，
        // 这里的集合语义正好一致。
        self.mesh_sources.insert(smd.clone());
        let model = BodyModel {
            sphere_normals: Vec::new(),
            smd,
            name: model_name.take(),
            flip_triangles,
            lods: Vec::new(),
            eyeballs: Vec::new(),
            flexes: Vec::new(),
            src_parts,
            src_material,
            src_scale,
            src_axis,
            src_shape_keys,
            src_shape_key_order,
            src_shape_key_ignore,
            input_scale: Some(input_scale),
        };
        let path = crate::compile::resolve_smd_path(&self.qdir, &model.smd)
            .map_err(|message| QcError::new(&t.file, t.line, message.to_string()))?;
        if crate::compile::SourceKind::of(&path) == Ok(crate::compile::SourceKind::Dmx) {
            let options = crate::compile::SrcOpts::of_model(&model)
                .map_err(|message| QcError::new(&t.file, t.line, message.to_string()))?;
            let geometry = crate::compile::read_source_geometry(&path, &model.smd, &options)
                .map_err(|message| QcError::new(&t.file, t.line, message.to_string()))?;
            if let Some(metadata) = geometry.dmx {
                for attachment in metadata.attachments {
                    if !self.desc.attachments.contains(&attachment) {
                        self.desc.attachments.push(attachment);
                    }
                }
            }
        }
        Ok(model)
    }

    /// 把 QC 里的文件名解析成「相对 qdir 的路径字符串」，并拼上 `cddir` 前缀。
    ///
    /// 官方用 `cddir[numdirs] + name` 拼路径，而 mdlc 的路径字段是
    /// 「相对描述文件所在目录」—— 所以这里把前缀**拼进字符串**，
    /// 写出器就不需要知道 `$pushd` 存在。
    fn resolve_src(&self, name: &str) -> String {
        let p = Path::new(name);
        if p.is_absolute() {
            return name.to_string();
        }
        let prefix = self.cd_prefix();
        let resolved = if prefix.is_empty() {
            name.replace('\\', "/")
        } else {
            format!("{}{}", prefix, name.replace('\\', "/"))
        };
        if crate::compile::resolve_smd_path(&self.qdir, &resolved).is_ok_and(|path| path.is_file())
        {
            return resolved;
        }
        for directory in &self.desc.model.source_search_paths {
            for relative in [&resolved, name] {
                let candidate = self.qdir.join(directory).join(relative);
                if crate::compile::resolve_smd_path(&self.qdir, &candidate.to_string_lossy())
                    .is_ok_and(|path| path.is_file())
                {
                    return candidate.to_string_lossy().replace('\\', "/");
                }
            }
        }
        resolved
    }

    /// `$model <name> <smd> [opts] { ...flex/eyeball/mouth... }`（`4228`）。
    fn cmd_model(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let base = self.next_bodypart_base();
        let m = self.option_studio(Some(name.clone()))?;
        let mut bp = BodyPart {
            name,
            base: Some(base),
            models: vec![m],
        };

        // 块内选项（`depth` 语义与官方一致：`{` 之后 depth>0 时跨行取 token）。
        let mut depth = 0i32;
        loop {
            let t = if depth > 0 {
                match self.lex.next_token(true)? {
                    Some(t) => t,
                    None => break,
                }
            } else {
                if !self.avail() {
                    break;
                }
                self.tok(false)?
            };
            if t.text == "{" {
                depth += 1;
                continue;
            }
            if t.text == "}" {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                continue;
            }
            let low = t.text.to_ascii_lowercase();
            let model = bp
                .models
                .last_mut()
                .expect("a $model block always has a model");
            match low.as_str() {
                "flexfile" => {
                    let v = self.tok(false)?.text;
                    self.pending_vta = Some(self.resolve_src(&v));
                }
                "flex" => {
                    let n = self.tok(false)?.text;
                    // ⚠️ `$model <名> <smd> flex "vanim" "vt2"` 的**内联形式**：
                    // 本行还有一个 token 时它就是 `.vta` 路径
                    // （官方 `depth == 0` 时读第二个 token 当 `vtafile`）。
                    // 实测 `sc3.qc` / `t01.qc` 用的是这个写法。
                    if depth == 0 && self.avail() {
                        let v = self.tok(false)?.text;
                        self.pending_vta = Some(self.resolve_src(&v));
                    }
                    let opts = self.flex_options(0.0)?;
                    self.push_flex(model, n, opts, false)?;
                }
                "flexpair" => {
                    let n = self.tok(false)?.text;
                    let split = self.f()?;
                    if depth == 0 && self.avail() {
                        let v = self.tok(false)?.text;
                        self.pending_vta = Some(self.resolve_src(&v));
                    }
                    let opts = self.flex_options(split)?;
                    self.push_flex(model, n, opts, true)?;
                }
                "defaultflex" => {
                    if depth == 0 && self.avail() {
                        let v = self.tok(false)?.text;
                        self.pending_vta = Some(self.resolve_src(&v));
                    }
                    let opts = self.flex_options(0.0)?;
                    self.push_flex(model, "default".to_string(), opts, false)?;
                }
                "localvar" => {
                    while self.avail() {
                        let t = self.tok(false)?;
                        self.add_flexdesc(&t.text);
                    }
                }
                "eyeball" => self.option_eyeball(model)?,
                "eyelid" => self.option_eyelid(model)?,
                "mouth" => self.option_mouth(model)?,
                "flexcontroller" => self.option_flexcontroller()?,
                "spherenormals" => {
                    let material = self.tok(false)?.text;
                    let center = self.v3()?;
                    model
                        .sphere_normals
                        .push(crate::source_support::SphereNormals { material, center });
                }
                "attachment" => {}
                // ---- mdlc 扩展：`src*` 的**块内**形式 ----
                //
                // 与 `option_studio` 的行内分支同一套语义。两处都要有，因为
                // `$model "n" "x.fbx" { srcpart "body" }` 是更自然的写法
                // （官方 `$model` 本来就带块），而 `$body n x.fbx srcpart "body"`
                // 走的是行内那条路。
                "srcpart" => {
                    while self.avail() {
                        let t = self.tok(false)?;
                        model.src_parts.push(t.text);
                    }
                }
                "srcmaterial" => {
                    model.src_material = Some(self.tok(false)?.text);
                }
                "srcscale" => {
                    let t = self.tok(false)?;
                    let v: f32 = t.text.parse().map_err(|_| {
                        QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcscale needs a number, got %{text}",
                                text = format!("{:?}", t.text)
                            ),
                        )
                    })?;
                    if !v.is_finite() || v <= 0.0 {
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcscale must be a positive finite number, got %{v}",
                                v = v
                            ),
                        ));
                    }
                    model.src_scale = Some(v);
                }
                "srcaxis" => {
                    let t = self.tok(false)?;
                    if crate::fbx::ForcedAxis::parse(&t.text).is_none() {
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcaxis only accepts \"auto\" / \"y\" / \"z\" (also yup / y-up / zup / z-up, case-insensitive), got %{text}",
                                text = format!("{:?}", t.text)
                            ),
                        ));
                    }
                    model.src_axis = Some(t.text);
                }
                "srcshapekey" => {
                    // ⚠️ 每次只读一个 token（理由同上面的 `srcpart`）。
                    let t = self.tok(false)?;
                    model.src_shape_keys.push(t.text);
                }
                "srcshapekeyorder" => {
                    let t = self.tok(false)?;
                    model.src_shape_key_order.push(t.text);
                }
                "srcshapekeyignore" => {
                    model.src_shape_key_ignore = true;
                }
                other => {
                    // ⚠️ 规则名必须取**原始大小写**的 `t.text`，**不能**用 `low`。
                    //
                    // 官方是 `Option_Flexrule( g_model[g_nummodels], &token[1] )`
                    // （`studiomdl.cpp:4366`）—— `token` 保留原样，随后用 `stricmp`
                    // 在 `g_flexdesc` 里查（`:3911`）。而 `low` 是全小写的：把
                    // `%AU1R` 存成 `au1r` 之后，写出阶段的 `flexdesc_index`
                    // （大小写敏感）就查不到 `AU1R` ⟹ 报
                    // `flex "au1r" 在 [[flex_descriptors]] 里找不到`。
                    if let Some(rest) = t.text.strip_prefix('%') {
                        // `%<flex> = <表达式>`
                        //
                        // ⚠️ 先把两张名字表**取出来**再调 —— `parse_flexrule`
                        // 需要 `&mut self.lex`，同时又要 `&self.desc` 里的名字，
                        // 直接借用会冲突。
                        let controllers = self.flex_controller_names();
                        let flexdescs = self.flex_desc_names();
                        let rest = rest.to_string();
                        let rule = super::flexrule::parse_flexrule(
                            &mut self.lex,
                            &rest,
                            &controllers,
                            &flexdescs,
                        )?;
                        self.desc.flex_rules.push(rule);
                    } else {
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "unknown model option %{opt}",
                                opt = format!("{other:?}")
                            ),
                        ));
                    }
                }
            }
        }

        self.desc.bodyparts.push(bp);
        Ok(())
    }

    /// `Option_Flex` 的后缀选项：`frame` / `position` / `split` / `decay`。
    ///
    /// 官方在一个 `while (TokenAvailable())` 里消费它们，**顺序任意、
    /// 个数任意**。返回 `(frame, position, split, decay)`。
    ///
    /// 默认值（官方 `Option_Flex` 初始化）：
    /// `frame = 0`、`target1`(=position) = **1.0**、`split` = 传入的
    /// `pairsplit`、`decay` = **1.0**。
    fn flex_options(&mut self, split_default: f32) -> Result<FlexOpts, QcError> {
        let mut o = FlexOpts {
            frame: 0,
            position: 1.0,
            split: split_default,
            decay: 1.0,
        };
        while self.avail() {
            let t = self.tok(false)?;
            match t.text.to_ascii_lowercase().as_str() {
                "frame" => o.frame = self.i()?,
                "position" => o.position = self.f()?,
                "split" => o.split = self.f()?,
                "decay" => o.decay = self.f()?,
                other => {
                    // 官方这里 `TokenError("unknown option: %s")` ——
                    // 报错。实测 `t01.qc` 的 `flex "vanim" "vt2"` 就是被
                    // 这条拦住的（`"vt2"` 不是合法选项）。
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "unknown flex option %{opt} (the official code says unknown option)",
                            opt = format!("{other:?}")
                        ),
                    ));
                }
            }
        }
        Ok(o)
    }

    /// `Option_Flex`：把一条 VTA 形状绑定推入当前 model。
    ///
    /// # 官方形态（`Option_Flex`，`studiomdl.cpp`）
    ///
    /// 支持三个后缀选项：`frame <n>` / `position <f>` / `split <f>` /
    /// `decay <f>`。`position` 落进 `target1`（即 TOML 的 `position`），
    /// `decay` 落进 `decay`。
    ///
    /// # ⚠️ 粘性 `vtafile` 的真实行为
    ///
    /// `Cmd_Model` 里 `char FAC[256], vtafile[256];` 是**栈上未初始化**的局部
    /// 数组，`flexfile` 只是给它赋值。所以：
    ///
    /// * `flexfile "x"` 之后，后续 `flex` / `flexpair` 复用 `"x"`；
    /// * **从未出现 `flexfile`** 时，`vtafile` 是**未初始化栈残留**
    ///   —— 官方行为不确定（实测 `t01.qc` 仍能产出 `.mdl`，
    ///   说明残留值恰好不是合法路径或官方容忍了加载失败）。
    ///
    /// mdlc **不复制未初始化行为**（那是 UB，无法复现）。
    /// 实测语料里这种写法的 29 个用例中，**只有 8 个有官方产物**，
    /// 其余 21 个官方自己也没产出 —— 说明它们本来就是无效 QC。
    /// 所以这里**显式报错**，并指出「缺 `flexfile`」这个真实原因。
    fn push_flex(
        &mut self,
        model: &mut BodyModel,
        name: String,
        opts: FlexOpts,
        pair: bool,
    ) -> Result<(), QcError> {
        // ⭐ **解析期**就注册 desc —— 官方 `Option_Flex` 是当场调
        // `Add_Flexdesc` 的（`studiomdl.cpp:3568-3580`），注册发生在读完
        // `flexfile`/`flexpair <split>` 之后、读 `frame`/`position` 等选项
        // **之前**。同一条 `flex` 内不会再插进别的 `Add_Flexdesc`，所以
        // 放在这里（`flex_options` 之后）得到的 desc 顺序与官方**逐项相同**。
        //
        // ⚠️ 这条曾经缺失，后果是**解析期查不到 desc**：`%<名>` flexrule 走
        // `flex_desc_names()`，而 mdlc 早期只在编译期
        // （`compile::resolve_vta_flexes`）注册 ⟹ `survivors_facerules.qci`
        // 的 `%AU1R` 直接报 `unknown flex AU1R`。
        //
        // 判据是 `pair`（= 官方 `pairsplit != 0`），**不是**「用了 `flexpair`
        // 关键字」：`flexpair "X" 0` 官方走 else 分支，只注册 `X`。
        if pair {
            let (rn, ln) = crate::flex::pair_names(&name);
            self.add_flexdesc(&rn);
            self.add_flexdesc(&ln);
        } else {
            self.add_flexdesc(&name);
        }

        let Some(vta) = self.pending_vta.clone() else {
            return Err(self.lex.error(crate::tr_fmt!(
                "there is no `flexfile` before `flex \"%{name}\"` — the official code uses an **uninitialized** `vtafile` here (UB, not reproducible). Write a `flexfile \"<x.vta>\"` line before `flex`, or use the inline form `$model <name> <smd> flex \"<name>\" \"<vta>\"`.",
                name = name
            )));
        };
        let descriptor = if pair {
            crate::flex::pair_names(&name).0
        } else {
            name.clone()
        };
        let mut targets = None;
        if let Some(previous) = model.flexes.last_mut() {
            let previous_descriptor = if previous.pair {
                crate::flex::pair_names(&previous.name).0
            } else {
                previous.name.clone()
            };
            if previous_descriptor.eq_ignore_ascii_case(&descriptor) {
                let previous_targets =
                    previous
                        .targets
                        .get_or_insert([0.0, previous.position, 10.0, 11.0]);
                previous_targets[2] = previous_targets[1];
                previous_targets[3] = opts.position;
                targets = Some([previous_targets[1], opts.position, 10.0, 11.0]);
            }
        }
        model.flexes.push(Flex {
            vta,
            name,
            frame: opts.frame,
            pair,
            split: opts.split,
            position: opts.position,
            decay: opts.decay,
            targets,
            from_eyelid: false,
        });
        Ok(())
    }

    /// 把一条 flex 规格推入 `model.flexes`，但**直接给定** `vta`/`targets`。
    ///
    /// 只给官方 `eyelid` 用（`Option_Eyelid` 自带 `vtafile` token，
    /// 不依赖粘性的 `flexfile`，且三条 flexkey 的分段 targets 各不相同）。
    #[allow(clippy::too_many_arguments)]
    fn push_flex_explicit(
        &mut self,
        model: &mut BodyModel,
        vta: String,
        name: String,
        frame: i32,
        split: f32,
        targets: [f32; 4],
    ) {
        model.flexes.push(Flex {
            vta,
            name,
            frame,
            pair: false,
            split,
            position: 1.0,
            decay: 0.0,
            targets: Some(targets),
            from_eyelid: true,
        });
    }

    /// 注册一个 flexdesc（官方 `Add_Flexdesc`）。
    fn add_flexdesc(&mut self, name: &str) {
        if self
            .desc
            .flex_descriptors
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case(name))
        {
            return;
        }
        self.desc.flex_descriptors.push(FlexDescriptor {
            name: name.to_string(),
        });
    }

    fn flex_desc_names(&self) -> Vec<String> {
        self.desc
            .flex_descriptors
            .iter()
            .map(|d| d.name.clone())
            .collect()
    }

    fn flex_controller_names(&self) -> Vec<String> {
        self.desc
            .flex_controllers
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    /// `flexcontroller <类型> [range <min> <max>] <名> [<名> ...]`。
    ///
    /// # 语义（`Option_Flexcontroller`，`studiomdl.cpp`）
    ///
    /// 第一个 token 是**类型**；之后逐个 token 扫：
    ///
    /// * `range <min> <max>` —— 更新**后续**控制器共用的范围；
    /// * 其它 token —— 就是**一个控制器名**，用**当前**范围注册。
    ///
    /// ⚠️ 所以一行可以注册**多个**控制器（实测 `fx2.qc`：
    /// `flexcontroller lid range 0 1 right_lid_raiser right_lid_droop`
    /// 注册 2 个）。早先按「一个类型 + 一个名字 + 两个数字」解析是错的。
    fn option_flexcontroller(&mut self) -> Result<(), QcError> {
        let kind = self.tok(false)?.text;
        let mut range_min = 0.0f32;
        let mut range_max = 1.0f32;
        while self.avail() {
            let t = self.tok(false)?;
            if t.text.eq_ignore_ascii_case("range") {
                range_min = self.f()?;
                range_max = self.f()?;
            } else {
                self.desc.flex_controllers.push(FlexController {
                    name: t.text,
                    kind: kind.clone(),
                    min: range_min,
                    max: range_max,
                });
            }
        }
        Ok(())
    }

    /// `eyeball <名> <骨骼> <x> <y> <z> "<材质>" <直径> <zangle> "<虹膜材质>" <pupil_scale>`。
    ///
    /// ⚠️ 第 7 个 token 官方叫「radius」但语义是**直径** ——
    /// `studiomdl.cpp:3411` 是 `eyeball->radius = verify_atof(token) / 2.0;`。
    /// 这个 `/2` 同时也是 `eyelid` 范围检查（`fabs(target) > radius`）的基准，
    /// 少除一次会让检查**放宽一倍**、产物里的 `radius` 字段**大一倍**。
    fn option_eyeball(&mut self, model: &mut BodyModel) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let bone = self.tok(false)?.text;
        let org = self.v3()?;
        let material = self.tok(false)?.text;
        let radius = self.f()? / 2.0;
        let zangle = self.f()?;
        let iris_material = self.tok(false)?.text;
        let pupil_scale = self.f()?;
        // 官方把 `zangle` 转成 `tan(deg2rad(zangle))`、`pupil_scale` 转成
        // `1/pupil_scale`（见 `model.rs` 的 `Eyeball` 文档）。
        let _ = iris_material;
        model.eyeballs.push(Eyeball {
            name: Some(name),
            bone,
            org,
            material,
            radius,
            zoffset: zangle.to_radians().tan(),
            iris_scale: if pupil_scale == 0.0 {
                1.0
            } else {
                1.0 / pupil_scale
            },
            upper_lid: None,
            lower_lid: None,
        });
        Ok(())
    }

    /// `eyelid <名> <vta> lowerer <帧> <目标> neutral <帧> <目标> raiser <帧> <目标>
    ///  [split <距离>] [eyeball <眼球名>]` —— 官方的眼睑语法。
    ///
    /// # 官方（`Option_Eyelid`，`studiomdl.cpp:3645-3802`）
    ///
    /// 一条 `eyelid` 会**注册 4 个 flexdesc**（顺序 = 先 `<名>`，再按 token
    /// 出现顺序的 `<名>_lowerer` / `<名>_neutral` / `<名>_raiser`），
    /// 并推**三条 flexkey**（全部 `flexdesc = <名>`、`decay = 0.0`、共用同一个
    /// `.vta`、共用同一个 `split`）：
    ///
    /// | # | frame | target0..3 |
    /// |---|---|---|
    /// | 0 | `lowerer` 的帧 | `-11, -10, lowerer, neutral` |
    /// | 1 | `neutral` 的帧 | `lowerer, neutral, neutral, raiser` |
    /// | 2 | `raiser` 的帧 | `neutral, raiser, 10, 11` |
    ///
    /// ⚠️ `neutral 0` 会让第 1 条落在 **frame 0** —— 官方那里是「载荷清零」的
    /// 特殊帧（`simplify.cpp:2453-2457`），所以它**合法且必然产出空载荷**。
    /// 这正是 [`Flex::from_eyelid`] 存在的唯一理由。
    ///
    /// ⚠️ `eyeball <名>` 可以省略；省略时官方把这条眼睑挂到该 model 的
    /// **全部**眼球上（`studiomdl.cpp:3756-3765`）。上下眼睑由 **`type[0]`**
    /// 决定（`switch(type[0])` 的 `case 'u'` / `case 'l'`），
    /// **不是**靠 `contains("upper")`；首字母不是 `u`/`l` 时官方静默不挂载
    /// （三条 flexkey 照样注册）。官方那两处比较**区分大小写**。
    ///
    /// # 与官方的两处有意偏离
    ///
    /// 1. [`Self::add_flexdesc`] 按名**去重**（官方 `Option_Eyelid` 直接
    ///    `strcpyn(g_flexdesc[g_numflexdesc++])`，同一个 `type` 写两次会产生
    ///    两条同名 desc）。去重是 mdlc 的既有约定（`resolve_vta_flexes` 的
    ///    `register` 同样去重），重复的 `type` 只会让后续下标整体漂移。
    /// 2. 写了 `eyeball <名>` 但该眼球不存在时**报错**（官方静默什么都不做）。
    ///    静默会让「眼睑没生效」变成零诊断的哑谜 —— 与
    ///    [`Self::option_ikrule`] 对未知链名的处理一致。
    fn option_eyelid(&mut self, model: &mut BodyModel) -> Result<(), QcError> {
        let type_name = self.tok(false)?.text;
        let vta_name = self.tok(false)?.text;
        let vta = self.resolve_src(&vta_name);

        // 官方 `:3665-3666`：base desc **无条件先注册**（不查重、不看后续 token）。
        self.add_flexdesc(&type_name);

        // (desc 名, 帧, 目标值) —— desc 名按官方拼成 `<type>_<token>`，
        // 其中 `<token>` 用**原样**大小写（官方 `strcat` 的就是它）。
        let mut lowerer: Option<(String, i32, f32)> = None;
        let mut neutral: Option<(String, i32, f32)> = None;
        let mut raiser: Option<(String, i32, f32)> = None;
        let mut split = 0.0f32;
        let mut eyeball: Option<String> = None;

        while self.avail() {
            let key = self.tok(false)?.text;
            let lower = key.to_ascii_lowercase();
            match lower.as_str() {
                "lowerer" | "neutral" | "raiser" => {
                    let frame = self.i()?;
                    let target = self.f()?;
                    let desc = format!("{type_name}_{key}");
                    self.add_flexdesc(&desc);
                    let slot = match lower.as_str() {
                        "lowerer" => &mut lowerer,
                        "neutral" => &mut neutral,
                        _ => &mut raiser,
                    };
                    *slot = Some((desc, frame, target));
                }
                "split" => split = self.f()?,
                "eyeball" => eyeball = Some(self.tok(false)?.text),
                other => {
                    return Err(self.lex.error(crate::tr_fmt!(
                        "unknown eyelid option %{opt} (the official code says unknown option)",
                        opt = format!("{other:?}")
                    )));
                }
            }
        }

        let (Some(lowerer), Some(neutral), Some(raiser)) = (lowerer, neutral, raiser) else {
            return Err(self.lex.error(crate::tr_fmt!(
                "eyelid %{name} is missing one of lowerer/neutral/raiser — the frame numbers and target values of the official's three flexkeys all come from them, and missing one reads an uninitialized stack value",
                name = format!("{type_name:?}")
            )));
        };
        let (lowerer_desc, lowerer_frame, lowerer_target) = lowerer;
        let (neutral_desc, neutral_frame, neutral_target) = neutral;
        let (raiser_desc, raiser_frame, raiser_target) = raiser;

        // 三条 flexkey 共用同一个 desc 名 ⟹ `resolve_vta_flexes` 的 `register`
        // 会把它们收敛到同一下标，正是官方 `flexdesc = basedesc` 的语义。
        self.push_flex_explicit(
            model,
            vta.clone(),
            type_name.clone(),
            lowerer_frame,
            split,
            [-11.0, -10.0, lowerer_target, neutral_target],
        );
        self.push_flex_explicit(
            model,
            vta.clone(),
            type_name.clone(),
            neutral_frame,
            split,
            [
                lowerer_target,
                neutral_target,
                neutral_target,
                raiser_target,
            ],
        );
        self.push_flex_explicit(
            model,
            vta,
            type_name.clone(),
            raiser_frame,
            split,
            [neutral_target, raiser_target, 10.0, 11.0],
        );

        // ---- 挂到眼球上（官方 `:3756-3801`）----
        let targets = [
            ("lowerer", lowerer_target),
            ("neutral", neutral_target),
            ("raiser", raiser_target),
        ];
        let mut matched = 0usize;
        for eb in model.eyeballs.iter_mut() {
            if let Some(want) = &eyeball
                && !eb
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(want))
            {
                continue;
            }
            matched += 1;

            // 官方 `:3767-3778` 的三条范围检查，逐条 `TokenError`。
            for (what, t) in targets {
                if t.abs() > eb.radius {
                    return Err(self.lex.error(crate::tr_fmt!(
                        "eyelid %{name} %{what} out of range (+-%{radius}): the absolute value of the target %{target} exceeds the radius %{r} of eyeball %{eyeball}",
                        name = format!("{type_name:?}"),
                        what = what,
                        radius = format!("{:.1}", eb.radius),
                        target = t,
                        r = eb.radius,
                        eyeball = format!("{:?}", eb.name.as_deref().unwrap_or(""))
                    )));
                }
            }

            let lid = EyeballLid {
                lid_flexdesc: type_name.clone(),
                lowerer: EyeballLidEntry {
                    flexdesc: lowerer_desc.clone(),
                    target: lowerer_target,
                },
                neutral: EyeballLidEntry {
                    flexdesc: neutral_desc.clone(),
                    target: neutral_target,
                },
                raiser: EyeballLidEntry {
                    flexdesc: raiser_desc.clone(),
                    target: raiser_target,
                },
            };
            // 官方 `switch(type[0])`：`case 'u'` → upper、`case 'l'` → lower，
            // 其余首字母**静默不挂载**（三条 flexkey 已经注册了）。
            // 官方那两处比较区分大小写，这里照做。
            match type_name.as_bytes().first() {
                Some(b'u') => eb.upper_lid = Some(lid),
                Some(b'l') => eb.lower_lid = Some(lid),
                _ => {}
            }
        }

        if let Some(want) = &eyeball
            && matched == 0
        {
            return Err(self.lex.error(crate::tr_fmt!(
                "eyelid %{name} references a nonexistent eyeball %{want} (the official code skips it silently; mdlc reports an error so the eyelid does not fail silently)",
                name = format!("{type_name:?}"),
                want = format!("{want:?}")
            )));
        }
        Ok(())
    }

    /// `mouth <index> "<名>" "<骨骼>" <fx> <fy> <fz>`。
    ///
    /// ⚠️ 第 2 个 token 是 **flexdesc 名**，官方 `Option_Mouth`（`:3818`）走的是
    /// `g_mouth[index].flexdesc = Add_Flexdesc( token );` —— 也就是**顺带注册**
    /// 一个 flexdesc（同样按 `stricmp` 去重）。这里照做：漏掉注册会让
    /// `%mouth = …` 这类 flexrule 在 `[[flex_descriptors]]` 里找不到目标。
    fn option_mouth(&mut self, model: &mut BodyModel) -> Result<(), QcError> {
        let _ = model;
        let index = self.i()?;
        let flexdesc = self.tok(false)?.text;
        let bone = self.tok(false)?.text;
        let forward = self.v3()?;
        self.add_flexdesc(&flexdesc);
        self.desc.mouths.push(Mouth {
            index,
            flexdesc,
            bone,
            forward,
        });
        Ok(())
    }

    /// `$declaresequence "<名>"` —— 前向声明的**空壳序列**。
    ///
    /// # 官方（`Cmd_DeclareSequence`，`studiomdl.cpp:3204-3218`）
    ///
    /// ```c
    /// if (g_sequence.Count() >= MAXSTUDIOSEQUENCES)
    ///     TokenError("Too many sequences (%d max)\n", MAXSTUDIOSEQUENCES );
    /// s_sequence_t *pseq = &g_sequence[ g_sequence.AddToTail() ];
    /// memset( pseq, 0, sizeof( s_sequence_t ) );
    /// pseq->flags = STUDIO_OVERRIDE;
    /// GetToken( false );
    /// strcpyn( pseq->name, token );
    /// ```
    ///
    /// 三件事：占槽、清零、置 `STUDIO_OVERRIDE`（0x0800）。
    /// 它**不**分配 `panim`、不读 SMD、不建动画。
    ///
    /// # 为什么 survivor 模组靠它（用户问的正是这个）
    ///
    /// 引擎在 `studio_virtualmodel.cpp:185` 做**跨模型序列替换**：
    /// 主模型里的空壳会被 `$includemodel` 进来的模型**按名字替换**。
    /// 于是「网格 + 骨骼 + flex」与「几百条动画」可以**分开发布**。
    ///
    /// 实测真实工程 `linnea_replaces_zoey`：
    /// `Zoey_$DeclareSequence.qci` 935 行**全是** `$DeclareSequence`，
    /// 编出的 `survivor_teenangst.mdl` 有 937 条序列、
    /// 其中 **933 条是 `STUDIO_OVERRIDE` 空壳**。
    ///
    /// # 与 `$sequence` 的关系（实测的硬约束）
    ///
    /// 声明之后**不能**再用 `$sequence <同名>` 去填 —— 官方报
    /// `no animations found`（`ParseSequence` 末尾检查 `numblends == 0`）。
    /// 所以这里只占名字、不建序列体，填充交给引擎。
    ///
    /// ⚠️ 但**反过来**可以：先 `$sequence` 再 `$declaresequence` 同名，
    /// 官方会产出**两条**（第二条是空壳），不报错。这是实测行为，
    /// 所以查重只在「两边都是实体序列」时报。
    fn cmd_declaresequence(&mut self) -> Result<(), QcError> {
        let name_t = self.tok(false)?;
        let name = name_t.text.clone();
        // 官方上限 `MAXSTUDIOSEQUENCES`。**注意它数的是
        // `g_sequence.Count()`** —— 声明与实体序列**共用**同一个池，
        // 所以这里要数两者之和。
        if self.desc.sequences.len() >= MAX_SEQUENCES {
            return Err(QcError::new(
                name_t.file.clone(),
                name_t.line,
                crate::tr_fmt!(
                    "too many sequences: the official limit is %{max}",
                    max = MAX_SEQUENCES
                ),
            ));
        }
        // 与实体序列重名 ⟹ 官方**不报错**，产出两条（实测）。
        // 但 `sequence_names` 是「实体序列已占用」的集合，空壳**不**写进去
        // —— 否则后面一条正常 `$sequence` 会被我们误报为重复，
        // 而官方在那种情况下是 `no animations found`（不同错误）。
        self.desc.sequences.push(Sequence {
            inherited_flags: None,
            no_anim_block: false,
            no_anim_block_stall: false,
            loop_restart: 0,
            fudge_loop: false,
            motion_rollback: self.default_motion_rollback,
            motion_type: 0,
            cycle_pose: None,
            key_values: None,
            name,
            smd: String::new(),
            fps: None,
            looping: false,
            delta: false,
            activity: None,
            activity_weight: 0,
            events: Vec::new(),
            // ⚠️ 官方 `memset` 之后这些字段**保持 0**，不是普通序列的
            // 默认值（`fade_in/out = 0.2`、`activity = -1`）。
            fade_in: 0.0,
            fade_out: 0.0,
            forward_declared: true,
            no_auto_ik: false,
            ik_rules: Vec::new(),
            cmds: Vec::new(),
            scale: None,
            input_scale: None,
            frames: None,
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
            weight_list: None,
            subtract: None,
            subtract_frame: None,
            num_frames: None,
            // `$declaresequence` 只声明名字，动画侧选项留空（默认 = 官方行为）。
            src_stack: None,
            src_fps: None,
        });
        Ok(())
    }

    /// `$sequence`（`Cmd_Sequence` + `ParseSequence`）。
    fn cmd_sequence(&mut self) -> Result<(), QcError> {
        let name_t = self.tok(false)?;
        let name = if self.lowercase_sequences {
            name_t.text.to_ascii_lowercase()
        } else {
            name_t.text.clone()
        };
        if !self.sequence_names.insert(name.to_ascii_lowercase()) {
            return Err(QcError::new(
                name_t.file.clone(),
                name_t.line,
                crate::tr_fmt!(
                    "duplicate sequence name %{name} (the official code says Duplicate sequence name)",
                    name = format!("{name:?}")
                ),
            ));
        }

        let mut seq = Sequence {
            inherited_flags: None,
            no_anim_block: false,
            no_anim_block_stall: false,
            loop_restart: 0,
            fudge_loop: false,
            motion_rollback: self.default_motion_rollback,
            motion_type: 0,
            cycle_pose: None,
            key_values: None,
            name: name.clone(),
            smd: String::new(),
            fps: None,
            looping: false,
            delta: false,
            activity: None,
            activity_weight: 0,
            events: Vec::new(),
            fade_in: self.default_fade_in,
            fade_out: self.default_fade_out,
            forward_declared: false,
            no_auto_ik: false,
            ik_rules: Vec::new(),
            cmds: Vec::new(),
            scale: None,
            input_scale: (self.default_scale != 1.0).then_some(self.default_scale),
            frames: None,
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
            weight_list: None,
            subtract: None,
            subtract_frame: None,
            num_frames: None,
            src_stack: None,
            src_fps: None,
        };

        self.parse_sequence_body(&mut seq, &name_t, false)?;
        let reference = seq.blends.first().unwrap_or(&seq.smd);
        if !self
            .animation_names
            .contains(&reference.to_ascii_lowercase())
        {
            seq.adjust.get_or_insert(self.default_adjust);
            seq.rotation.get_or_insert(self.default_rotation);
        }
        self.desc.sequences.push(seq);
        Ok(())
    }

    /// `ParseSequence` 的**主体**（`studiomdl.cpp:2650-2999`）——
    /// `$sequence` 块与 `$continue <序列名>` 共用。
    ///
    /// `is_append` 对应官方的同名形参。`$continue` 命中序列池时官方调
    /// `ParseSequence( pseq, true )`（`studiomdl.cpp:3175`），与
    /// `$sequence` 的差别**只有三处**：
    ///
    /// 1. `:2944` 的动画级门是 `(numblends || isAppend)` —— append 时
    ///    **无条件**先试 `ParseAnimationToken( animations[0] )`（mdlc 本来
    ///    就无条件先试，见 `_ =>` 兜底）；
    /// 2. `:2948-2972` 的「查 `$animation` 池 / `Cmd_ImpliedAnimation`」
    ///    分支被 `!isAppend` 挡住，落到 `:2973` 的
    ///    `TokenError( "unknown command \"%s\"\n" )`；
    /// 3. `:2984 if (isAppend) return 0;` —— 直接返回，**不做**
    ///    `no animations found` 检查，也不推导 `groupsize`。
    ///
    /// ⚠️ 官方 `ParseAnimation` 的 `isAppend` 形参（`:2442`）**完全没用**：
    /// 函数体只有那个 `while (1)` 循环，不认识就
    /// `Unknown animation option'%s'`。所以 `$continue <动画名>` 与
    /// `$animation` 体是同一套解析，mdlc 的 `cmd_continue` 动画路径照抄。
    ///
    /// ⚠️ 提取这个函数的原因：修复前 `cmd_continue` 的序列路径**只**调
    /// `parse_animation_token`，于是 `ACT_*`/`fps`/`fadein`/`fadeout`/
    /// `addlayer`/`hidden`/`numframes`/`ikrule` 这些**序列级**选项全部报
    /// `未知的命令`。实测 `anims_fix.qci:212`
    /// `$DebiddoChargerLoop Idle_Fall_From_Charger ACT_TERROR_IDLE_FALL_FROM_CHARGERHIT -1`
    /// 就死在这里。
    fn parse_sequence_body(
        &mut self,
        seq: &mut Sequence,
        name_t: &Token,
        is_append: bool,
    ) -> Result<(), QcError> {
        let name = seq.name.clone();
        let mut depth = 0i32;
        // 本序列引用的动画名（blend 网格，行主序）。
        let mut blend_names: Vec<String> = Vec::new();
        let mut motion_rollback_override = None;
        let mut loop_restart_override = None;
        let mut fudge_loop_override = false;
        // 兜底委派 `ParseAnimationToken` 时的落点（官方落在
        // `pseq->panim[0][0]` 上；mdlc 解析期还不知道第一格是哪个动画，
        // 所以先记在 `holder` 里，块尾并回 `seq`）。
        let mut holder = Animation {
            no_anim_block: seq.no_anim_block,
            no_anim_block_stall: seq.no_anim_block_stall,
            forward_declared: false,
            loop_restart: seq.loop_restart,
            fudge_loop: seq.fudge_loop,
            extra_flags: 0,
            motion_rollback: seq.motion_rollback,
            motion_type: seq.motion_type,
            name: name.clone(),
            smd: String::new(),
            fps: None,
            looping: false,
            frames: seq.frames,
            subtract: None,
            subtract_frame: None,
            ik_rules: Vec::new(),
            no_auto_ik: false,
            weight_list: None,
            cmds: Vec::new(),
            // ⚠️ 从 `seq` **播种**（而不是 `None`）：`$continue <序列名>`
            // 是「在已有序列上追加」，官方 `panim->scale` 会保留到被显式
            // 覆写为止。`$sequence` 走到这里时 `seq.scale` 必为 `None`
            // （上面那个字面量），所以对 `$sequence` 是无操作。
            scale: seq.scale,
            adjust: seq.adjust,
            rotation: seq.rotation,
            // 同上：`$continue` 是追加语义，动画侧选项也从 `seq` 播种。
            src_stack: seq.src_stack.clone(),
            src_fps: seq.src_fps,
        };
        loop {
            let t = if depth > 0 {
                match self.lex.next_token(true)? {
                    Some(t) => t,
                    None => break,
                }
            } else {
                if !self.avail() {
                    break;
                }
                self.tok(false)?
            };
            if t.text == "{" {
                depth += 1;
                continue;
            }
            if t.text == "}" {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                continue;
            }
            let low = t.text.to_ascii_lowercase();
            match low.as_str() {
                "fps" => seq.fps = Some(self.f()?),
                "cyclepose" => {
                    let name = self.tok(false)?.text;
                    self.poseparam_index(&name);
                    seq.cycle_pose = Some(name);
                }
                "loop" => seq.looping = true,
                "delta" => seq.delta = true,
                "predelta" => seq.extra_flags = Some(seq.extra_flags.unwrap_or(0) | 0x0004),
                "noautoik" => seq.no_auto_ik = true,
                "autoik" => seq.no_auto_ik = false,
                "snap" | "hidden" | "autoplay" | "post" | "realtime" | "worldspace" | "local" => {
                    // 纯标志位关键字 —— `ParseSequence` 逐个
                    // `pseq->flags |= XXX`（`studiomdl.cpp:2720-2866`）。
                    // 位值见 `studio.h` 的 `STUDIO_*`。
                    let bit = match low.as_str() {
                        "snap" => 0x0002,       // STUDIO_SNAP
                        "autoplay" => 0x0008,   // STUDIO_AUTOPLAY
                        "post" => 0x0010,       // STUDIO_POST
                        "realtime" => 0x0080,   // STUDIO_REALTIME
                        "hidden" => 0x0400,     // STUDIO_HIDDEN
                        "worldspace" => 0x2010, // STUDIO_WORLD | STUDIO_POST
                        "local" => 0x1000,
                        _ => 0,
                    };
                    seq.extra_flags = Some(seq.extra_flags.unwrap_or(0) | bit);
                }
                "fadein" => seq.fade_in = self.f()?,
                "fadeout" => seq.fade_out = self.f()?,
                "activity" => {
                    let a = self.tok(false)?.text;
                    let w = if self.avail() { self.i()? } else { 0 };
                    seq.activity = Some(a);
                    seq.activity_weight = w;
                }
                "node" => {
                    let name = self.transition_node()?;
                    seq.transition = Some([name.clone(), name]);
                }
                "transition" | "rtransition" => {
                    seq.transition = Some([self.transition_node()?, self.transition_node()?]);
                    seq.transition_reversible |= low == "rtransition";
                }
                "blendwidth" => seq.blend_width = Some(self.i()?),
                "blend" => {
                    let parameter = self.tok(false)?.text;
                    let start = self.f()?;
                    let end = self.f()?;
                    // 官方在解析 blend 时还会放宽 pose parameter 的 min/max。
                    if let Some(pi) = self
                        .desc
                        .model
                        .pose_parameters
                        .iter_mut()
                        .find(|p| p.name.eq_ignore_ascii_case(&parameter))
                    {
                        pi.start = pi.start.min(start).min(end);
                        pi.end = pi.end.max(start).max(end);
                    }
                    seq.blend_params.push(BlendParam {
                        parameter,
                        start,
                        end,
                        // 纯 `blend`：没有附着点，`paramcontrol` 保持官方
                        // `memset` 的 **0**（`studiomdl.cpp:2731-2752`
                        // 只写 paramindex/paramstart/paramend）。
                        attachment: None,
                        control: None,
                    });
                }
                "calcblend" => {
                    // 官方 `ParseSequence`（`studiomdl.cpp:2753-2774`）：
                    //
                    // ```c
                    // GetToken(false); j = LookupPoseParameter(token); pseq->paramindex[i] = j;
                    // GetToken(false); pseq->paramattachment[i] = LookupAttachment(token);
                    // if (pseq->paramattachment[i] == -1) TokenError("Unknown calcblend attachment ...");
                    // GetToken(false); pseq->paramcontrol[i] = lookupControl(token);
                    // ```
                    //
                    // ⚠️ **槽位 `i` 的算法与 `blend` 逐字相同** ——
                    // 两个关键字共用一套「按出现顺序占槽」的规则，
                    // 所以它们必须推进**同一个** `blend_params` 列表。
                    let parameter = self.tok(false)?.text;
                    let attachment = self.tok(false)?.text;
                    let control = if self.avail() {
                        Some(self.tok(false)?.text)
                    } else {
                        None
                    };
                    // `calcblend` **不写** `paramstart`/`paramend` ——
                    // 两个值由 `CalcPoseParameters` 在编译期算出。
                    seq.blend_params.push(BlendParam {
                        parameter,
                        start: 0.0,
                        end: 0.0,
                        attachment: Some(attachment),
                        control,
                    });
                }
                "blendref" => {
                    // `studiomdl.cpp:2775-2783`：`pseq->paramanim = LookupAnimation(token)`。
                    //
                    // ⚠️ `LookupAnimation` **先查动画池、再查序列池**
                    // （`studiomdl.cpp:2381-2397`），所以这里**不能**在解析期
                    // 就按动画名校验 —— 一个序列名同样合法。解析期也看不到
                    // 后面才声明的动画，所以名字**留到 `compile.rs` 解析**
                    // （与 `subtract` / `weight_list` 同一套做法）。
                    seq.blend_ref = Some(self.tok(false)?.text);
                }
                "blendcomp" => {
                    // `studiomdl.cpp:2784-2792`：`pseq->paramcompanim = LookupAnimation(token)`。
                    seq.blend_comp = Some(self.tok(false)?.text);
                }
                "blendcenter" => {
                    // `studiomdl.cpp:2793-2801`：`pseq->paramcenter = LookupAnimation(token)`。
                    seq.blend_center = Some(self.tok(false)?.text);
                }
                "addlayer" => {
                    let s = self.tok(false)?.text;
                    seq.auto_layers.push(AutoLayer {
                        sequence: s,
                        pose: 0,
                        flags: 0,
                        start: 0.0,
                        peak: 0.0,
                        tail: 0.0,
                        end: 0.0,
                    });
                }
                "blendlayer" => {
                    // 官方 `ParseSequence`（`studiomdl.cpp:2890-2943`）：
                    //
                    // ```c
                    // pseq->autolayer[n].flags = 0;              // ← 先清零
                    // GetToken; name                              // 序列名
                    // GetToken; start = verify_atoi(token);
                    // GetToken; peak  = verify_atoi(token);
                    // GetToken; tail  = verify_atoi(token);
                    // GetToken; end   = verify_atoi(token);
                    // while (TokenAvailable()) {                  // ← 子标志循环
                    //     GetToken;
                    //     if      "xfade"         flags |= STUDIO_AL_XFADE;   // 0x0080
                    //     else if "spline"        flags |= STUDIO_AL_SPLINE;  // 0x0040
                    //     else if "noblend"       flags |= STUDIO_AL_NOBLEND; // 0x0200
                    //     else if "poseparameter" flags |= STUDIO_AL_POSE;    // 0x4000
                    //                             GetToken; pose = LookupPoseParameter(token);
                    //     else if "local"         flags |= STUDIO_AL_LOCAL;   // 0x1000
                    //                             pseq->flags |= STUDIO_LOCAL; // 0x1000
                    //     else { UnGetToken(); break; }          // ← **不认识就吐回并停**
                    // }
                    // pseq->numautolayers++;
                    // ```
                    //
                    // ⚠️ **`addlayer` 与 `blendlayer` 是同一个数组的两个形态**：
                    // 前者只给名字（四个时间量全 0、`flags = 0`），后者给名字 +
                    // 四个时间量 + 可选标志。官方两者都推进
                    // `pseq->autolayer[pseq->numautolayers++]`，**顺序就是 QC 顺序**。
                    //
                    // ⚠️ 子标志循环**遇到不认识的就停**（`UnGetToken` + `break`），
                    // 所以 `blendlayer "x" 1 2 3 4` 后面紧跟的动画名/其它关键字
                    // **不会被吃掉**。
                    let s = self.tok(false)?.text;
                    let start = self.f()?;
                    let peak = self.f()?;
                    let tail = self.f()?;
                    let end = self.f()?;
                    let mut flags = 0i32;
                    let mut pose = 0i16;
                    let mut local = false;
                    // 子标志：遇到不认识的就**吐回并停**
                    // （官方 `UnGetToken(); break;`）。
                    while self.avail() {
                        let t = self.tok(false)?;
                        match t.text.to_ascii_lowercase().as_str() {
                            "xfade" => flags |= 0x0080,
                            "spline" => flags |= 0x0040,
                            "noblend" => flags |= 0x0200,
                            "local" => {
                                flags |= 0x1000;
                                local = true;
                            }
                            "poseparameter" => {
                                flags |= 0x4000;
                                // `LookupPoseParameter` 找不到会**新建**一个
                                // （`studiomdl.cpp:2333-2352`），所以这里也
                                // 按需追加，而不是报错。
                                let name = self.tok(false)?.text;
                                pose = self.poseparam_index(&name) as i16;
                            }
                            _ => {
                                // 不认识 ⟹ 吐回该 token 并停（官方 `UnGetToken`）。
                                self.lex.unget(t);
                                break;
                            }
                        }
                    }
                    if local {
                        // `pseq->flags |= STUDIO_LOCAL`（0x1000）。
                        seq.extra_flags = Some(seq.extra_flags.unwrap_or(0) | 0x1000);
                    }
                    seq.auto_layers.push(AutoLayer {
                        sequence: s,
                        pose,
                        flags,
                        start,
                        peak,
                        tail,
                        end,
                    });
                }
                "event" => {
                    let (ev, consumed_depth) = self.option_event(&seq.name)?;
                    seq.events.push(ev);
                    depth -= consumed_depth;
                }
                "ikrule" => {
                    let r = self.option_ikrule()?;
                    seq.ik_rules.push(r);
                }
                "iklock" => {
                    let chain = self.tok(false)?.text;
                    let pos_weight = self.f()?;
                    let local_q_weight = self.f()?;
                    seq.iklocks.push(IkAutoplayLock {
                        chain,
                        pos_weight,
                        local_q_weight,
                    });
                }
                "weightlist" => {
                    // `$sequence ... weightlist "<名>"`（`ParseAnimationToken`
                    // 的 `strnicmp("weightlist", token, 6)` 分支）。
                    // 官方把它记成序列 `cmds[]` 里的 `CMD_WEIGHTS`，
                    // 最终作用在**动画**上（`setAnimationWeight`）。
                    let n = self.tok(false)?.text;
                    seq.weight_list = Some(n.clone());
                    holder.cmds.push(AnimCmd::Weights { weight_list: n });
                }
                "sectionframes" => {
                    seq.section_frames = Some(self.i()?);
                    if self.avail() {
                        seq.section_threshold = Some(self.i()?);
                    }
                }
                "keyvalues" => {
                    seq.key_values = Some(self.option_keyvalues()?);
                }
                "subtract" => {
                    let reference = self.tok(false)?.text;
                    let frame = if self.avail() {
                        let token = self.tok(false)?;
                        if let Some(frame) = parse_i32(&token.text) {
                            frame
                        } else {
                            self.lex.unget(token);
                            0
                        }
                    } else {
                        0
                    };
                    seq.subtract = Some(reference.clone());
                    seq.subtract_frame = Some(frame);
                    holder.cmds.push(AnimCmd::Subtract {
                        reference,
                        frame,
                        post: true,
                    });
                }
                "numframes" => {
                    let frames = self.i()?;
                    seq.num_frames = Some(frames);
                    holder.cmds.push(AnimCmd::NumFrames { frames });
                }
                // ---- mdlc 扩展：动画侧的两个 `src*` 选项 ----
                "srcstack" => {
                    seq.src_stack = Some(self.tok(false)?.text);
                }
                "srcfps" => {
                    let t = self.tok(false)?;
                    let v: f32 = t.text.parse().map_err(|_| {
                        QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcfps needs a number, got %{text}",
                                text = format!("{:?}", t.text)
                            ),
                        )
                    })?;
                    if !v.is_finite() || v <= 0.0 {
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "srcfps must be a positive finite number, got %{v}",
                                v = v
                            ),
                        ));
                    }
                    seq.src_fps = Some(v);
                }
                other if other.starts_with("act_") => {
                    // 官方 `strnicmp(token,"ACT_",4)==0` 时 `UnGetToken()`
                    // 后走 `Option_Activity`（`studiomdl.cpp:2714-2718`）。
                    //
                    // ⚠️ **`Option_Activity` 读两个 token**（`studiomdl.cpp:
                    // 1165-1178`）：`GetToken(名)` + `GetToken(权重
                    // verify_atoi)`。所以这里**必须也吃掉权重**，否则
                    // `"ACT_VM_IDLE" 1` 里的 `1` 会漏进 `blend_names`，
                    // 变成「找不到动画 "1"」。
                    //
                    // ⚠️ 权重**不能**无条件读：`$sequence "x" "ACT_VM_IDLE"`
                    // （不写权重）官方会 `verify_atoi` 一个不属于本序列的
                    // token。判据是「下一个 token 是否还在本序列的行内」——
                    // 与 `["activity"]` 分支同一套逻辑。
                    seq.activity = Some(t.text.clone());
                    seq.activity_weight = if self.avail() {
                        let t2 = self.tok(false)?;
                        match parse_i32(&t2.text) {
                            Some(v) => v,
                            None => {
                                // 不是数字 ⟹ 它不是权重，退回让它按原样处理。
                                self.lex.unget(t2);
                                0
                            }
                        }
                    } else {
                        0
                    };
                }
                _ => {
                    // ⭐ 官方 `ParseSequence` 的**三段**尾部分派
                    // （`studiomdl.cpp:2944-2976`）：
                    //
                    // ```c
                    // else if ((numblends || isAppend) && ParseAnimationToken( animations[0] )) { }
                    // else if (!isAppend) { /* 查 $animation 池，否则 Cmd_ImpliedAnimation */ }
                    // else { TokenError( "unknown command \"%s\"\n", token ); }
                    // ```
                    //
                    // ⚠️ **修复前这里漏掉了整段 `ParseAnimationToken` 委派**，
                    // 于是 `frame`（单数）/`origin`/`rotate`/`angles`/`scale`/
                    // `fixuploop`/`noanimation`/`align`/`alignto`/`walkframe`/
                    // `walkalignto`/`cmdlist` 以及运动控制位 `X Y Z LX LY`
                    // **全部静默漏进 `blend_names`**，变成假 blend 格
                    // （实测 `anims_fix.qci:165` 的 `align Death X Y 100 0`
                    // 产生 6 个假格 ⟹ `compile.rs` 报「blend 格数 6 不是
                    // 完全平方数」）。
                    //
                    // 官方那个 `(numblends || isAppend)` 门在 mdlc 里没法
                    // 照抄（`blend_names` 此刻可能还空着，但**第一格动画**
                    // 尚未确定）。这里选择**无条件先试**：动画级选项不认识
                    // 就返回 `Ok(false)`，此时才退回下面的回退逻辑。
                    if self.parse_animation_token(&mut holder, &t)? {
                        if t.text.eq_ignore_ascii_case("motionrollback") {
                            motion_rollback_override = Some(holder.motion_rollback);
                        }
                        if t.text.to_ascii_lowercase().starts_with("startloop") {
                            loop_restart_override = Some(holder.loop_restart);
                        }
                        fudge_loop_override |= t.text.eq_ignore_ascii_case("fudgeloop");
                        // 认出了 —— 选项已写进 `holder`（`$sequence` 级的
                        // `scale`/`adjust`/`rotation`/`cmds` 在块尾并回 `seq`）。
                        continue;
                    }
                    if is_append {
                        // 官方 `studiomdl.cpp:2973-2976`：
                        // `else { TokenError( "unknown command \"%s\"\n", token ); }`
                        // —— `$continue` 的序列路径**不**做「查 `$animation`
                        // 池 / 建隐含动画」那一步（它被 `!isAppend` 挡住了）。
                        return Err(QcError::new(
                            t.file.clone(),
                            t.line,
                            crate::tr_fmt!(
                                "unknown command %{cmd} (the official code says unknown command %{cmd2})",
                                cmd = format!("{:?}", t.text),
                                cmd2 = format!("{:?}", t.text)
                            ),
                        ));
                    }
                    if self.animation_names.contains(&t.text.to_ascii_lowercase()) {
                        blend_names.push(t.text.clone());
                    } else if crate::compile::is_source_ref(&t.text)
                        || crate::compile::resolve_smd_path(&self.qdir, &self.resolve_src(&t.text))
                            .is_ok_and(|path| path.is_file())
                    {
                        if blend_names.is_empty() && seq.smd.is_empty() {
                            seq.smd = self.resolve_src(&t.text);
                            if self.desc.model.first_source.is_none() {
                                self.desc.model.first_source = Some(seq.smd.clone());
                            }
                            self.referenced_files.push(seq.smd.clone());
                        } else {
                            blend_names.push(t.text.clone());
                        }
                    } else {
                        // 池里没有 ⟹ 建**隐含动画**（`Cmd_ImpliedAnimation`，
                        // `studiomdl.cpp:2506-2547`），它调
                        // `Load_Source( panim->filename, "" )` —— 而 `Load_Source`
                        // 是用 `cddir[numdirs]` 拼路径的（`%s%s.smd`，
                        // `studiomdl.cpp:1603-1638`），即 **`$pushd` 的前缀是官方
                        // 自动加的**。
                        //
                        // ⚠️ 早先这里原样 push 裸名，于是 `$pushd anims` 之下的
                        // 隐含动画全被当成「工程根目录下的文件」—— 实测用户工程
                        // `incap_anim_fix` 报 22 条
                        // `sequences[N].smd: 读不到 .\NamVet_*.smd`。
                        // 解析出的名字**不带扩展名** —— mdlc 要求写完整文件名，
                        // 所以这种裸名会在编译期被
                        // [`crate::compile::resolve_smd_path`] 拒绝并报
                        // 「资产引用没有扩展名」（官方则会按 `Load_Source`
                        // 的试探链找文件）。
                        blend_names.push(self.resolve_src(&t.text));
                    }
                }
            }
        }

        // 兜底委派的成果并回序列（官方直接写在 `pseq->panim[0][0]` 上；
        // mdlc 记在 `holder` 里，编译期由 `compile.rs` 挂到第一格 ——
        // 与 `ik_rules` 同一套路）。
        //
        // 只有走 `_ =>` 兜底的关键字才可能落在 `holder` 上：`fps`/`loop`/
        // `delta`/`subtract`/`numframes`/`weightlist`/`ikrule`/标志位都被
        // 上面的专有 arm 提前接住了。所以实际会并的只有
        // `scale`/`adjust`/`rotation`/`cmds` 与 `fudgeloop`/`startloop`。
        if holder.looping {
            seq.looping = true;
        }
        seq.cmds.extend(holder.cmds);
        seq.motion_type = holder.motion_type;
        seq.no_anim_block = holder.no_anim_block;
        seq.no_anim_block_stall = holder.no_anim_block_stall;
        seq.motion_rollback = holder.motion_rollback;
        seq.loop_restart = holder.loop_restart;
        seq.fudge_loop = holder.fudge_loop;
        // ⚠️ **无条件赋值**（不是 `if seq.scale.is_none()`）：`holder` 是从
        // `seq` **播种**的（见上面 `holder` 字面量），所以未改动时两者相等、
        // 赋值是无操作；而 `$continue` 在已有序列上追加 `scale 0.5` 时，
        // `seq.scale` 早已是 `Some(旧值)`，条件赋值会把新值丢掉。
        seq.scale = holder.scale;
        seq.adjust = holder.adjust;
        seq.rotation = holder.rotation;
        seq.frames = holder.frames;
        // `holder.fps`/`delta`/`no_auto_ik`/`weight_list`/`subtract`/
        // `ik_rules` 同理 —— 它们都有专有 arm 提前接住，永远到不了这里。

        // 官方 `studiomdl.cpp:2984 if (isAppend) return 0;` —— `$continue`
        // 的序列路径到这里就结束：**不做** `no animations found` 检查，
        // 也不推导 `groupsize`（那是新序列才需要的）。
        if is_append {
            self.lower_sequence_source_overrides(
                seq,
                motion_rollback_override,
                loop_restart_override,
                fudge_loop_override,
            );
            return Ok(());
        }

        if blend_names.is_empty() {
            if seq.smd.is_empty() {
                return Err(QcError::new(
                    name_t.file.clone(),
                    name_t.line,
                    crate::tr_fmt!(
                        "sequence %{name} found no animations (the official code says no animations found)",
                        name = format!("{name:?}")
                    ),
                ));
            }
            // 单动画序列：`blends` **留空**，引用放进 `smd`
            // —— 这是 IR 的约定（`Sequence::blends` 文档：
            // 「空 = 单动画序列，它自己的 animdesc 由 `@序列名` 隐含产生」）。
            //
            // `smd` 既可能是 SMD 路径，也可能是 `$animation` 名 ——
            // `compile.rs` 先拿它查动画池，查不到才当路径加载，
            // 与官方 `studiomdl.cpp:2952-2959` 的行为一致。
        } else if blend_names.len() == 1 && seq.smd.is_empty() && seq.blend_params.is_empty() {
            // 只引用了一个**动画名**（非 SMD 路径）、且没有任何 `blend` 轴
            // —— 这就是单动画序列，与 `$sequence "x" "a_idle"` 同义。
            //
            // ⚠️ 必须排除 `blend_params` 非空的情形：`{ "a_idle" blend "p" 0 1 }`
            // 是一个**只有 1 格的 blend 网格**（`groupsize = 1×1`），
            // 它要保留 `paramindex`/`paramstart`/`paramend`，collapse 掉会丢数据。
            seq.smd = blend_names[0].clone();
        } else {
            // blend 网格：每一格是**动画名**（或 SMD 路径，官方会建隐含动画）。
            // `smd` 在 blend 形态下被忽略，但仍要求非空。
            if seq.smd.is_empty() {
                seq.smd = blend_names[0].clone();
            }
            seq.blends = blend_names;
        }
        self.lower_sequence_source_overrides(
            seq,
            motion_rollback_override,
            loop_restart_override,
            fudge_loop_override,
        );
        let references = if seq.blends.is_empty() {
            std::slice::from_ref(&seq.smd)
        } else {
            &seq.blends
        };
        seq.inherited_flags = Some(
            references
                .iter()
                .filter_map(|name| {
                    self.desc
                        .animations
                        .iter()
                        .find(|animation| animation.name.eq_ignore_ascii_case(name))
                })
                .fold(0, |flags, animation| {
                    flags
                        | animation.extra_flags
                        | i32::from(animation.looping)
                        | if animation.forward_declared { 0x800 } else { 0 }
                }),
        );
        // ⚠️ **不 push** —— 调用方负责（`$sequence` 新建后 push，
        // `$continue` 则写回 `self.desc.sequences[si]`）。
        Ok(())
    }

    fn lower_sequence_source_overrides(
        &mut self,
        seq: &mut Sequence,
        motion_rollback: Option<f32>,
        loop_restart: Option<i32>,
        fudge_loop: bool,
    ) {
        let first = seq.blends.first().unwrap_or(&seq.smd);
        if let Some(anim) = self
            .desc
            .animations
            .iter_mut()
            .find(|anim| anim.name.eq_ignore_ascii_case(first))
        {
            if seq
                .cmds
                .iter()
                .any(|command| matches!(command, AnimCmd::Subtract { .. }))
            {
                anim.subtract = seq.subtract.take();
                anim.subtract_frame = seq.subtract_frame.take();
            }
            if seq
                .cmds
                .iter()
                .any(|command| matches!(command, AnimCmd::Weights { .. }))
            {
                anim.weight_list = seq.weight_list.take();
            }
            if seq
                .cmds
                .iter()
                .any(|command| matches!(command, AnimCmd::NumFrames { .. }))
            {
                seq.num_frames = None;
            }
            anim.cmds.append(&mut seq.cmds);
            // Named references share the animation object. Apply these assignments
            // during parsing so interleaved $continue commands retain QC order.
            if let Some(frames) = seq.frames.take() {
                anim.frames = Some(frames);
            }
            anim.motion_type |= std::mem::take(&mut seq.motion_type);
            anim.looping |= seq.looping;
            anim.no_anim_block |= seq.no_anim_block;
            anim.no_anim_block_stall |= seq.no_anim_block_stall;
            if let Some(value) = motion_rollback {
                anim.motion_rollback = value;
            }
            if let Some(value) = loop_restart {
                anim.loop_restart = value;
                anim.looping = true;
            }
            if fudge_loop {
                anim.fudge_loop = true;
                anim.looping = true;
            }
            if let Some(scale) = seq.scale.take() {
                anim.scale = Some(scale);
            }
        }
    }

    /// `event <名或数字> <帧> "<参数>"`（`Option_Event`，`1186`）。
    ///
    /// 返回 `(事件, 需要从 depth 减去的量)` —— 官方允许事件带 `{ }` 块
    /// （`Option_Event` 返回值即「是否吃掉了 `}`」）。
    ///
    /// # ⚠️ **回归（R67）**：收尾的 `}` 必须先判、不能当 options
    ///
    /// 官方 `Option_Event`（`studiomdl.cpp:1206-1214`）读 options 之前**先**
    /// 检查这个 token 是不是块结束符：
    ///
    /// ```c
    /// if (TokenAvailable())
    /// {
    ///     GetToken( false );
    ///     if (token[0] == '}') return 1;      // ← 不吃它，让调用方减 depth
    ///     strcpyn( psequence->events[psequence->numevents-1].options, token );
    /// }
    /// return 0;
    /// ```
    ///
    /// 调用点 `studiomdl.cpp:2706-2709` 是 `depth -= Option_Event( pseq );`。
    ///
    /// 修前本实现**无条件**把下一个 token 当 options 字符串 ⟹
    /// `{ event AE_PICKUP_CLIPIN 40 }` 里那个收尾 `}` 被吞进 `options`，
    /// `depth` 永不归零 ⟹ **整个块不再闭合**，后续每一条 `$sequence`
    /// 的关键字全部漏进本序列的 `blends`。用户的
    /// `v_huntingrifle_processed.qc` 就是这么变成 103 格 blend 的
    /// （12 处 `{ event … }` × 后续 token）。真 exe 裁决见
    /// `target/_r67/oracle_event.js`：`}` 同行与换行两种写法产物
    /// **逐字节相同**，即官方两种都正确解析。
    fn option_event(&mut self, _seq: &str) -> Result<(SequenceEvent, i32), QcError> {
        let name = self.tok(false)?.text;
        let frame = self.i()?;
        let mut consumed_depth = 0i32;
        let options = if self.avail() {
            let opt = self.tok(false)?;
            // 官方判据逐字照抄：只看**首字节**是不是 `}`。
            if opt.text.starts_with('}') {
                consumed_depth = 1;
                String::new()
            } else {
                opt.text
            }
        } else {
            String::new()
        };
        // 官方 `write.cpp:487-523`：名字以数字开头 ⟹ 旧式数字事件。
        let numeric = name.starts_with(|c: char| c.is_ascii_digit());
        Ok((
            SequenceEvent {
                // `cycle = frame / (numframes - 1)`，帧数在写出期才知道 ——
                // 这里先存**帧号**在 `cycle` 里，由 `compile`/`mdl_writer` 换算？
                // 不：TOML 的 `cycle` 语义就是 cycle。所以这里存帧号，
                // 由 `finish()` 之后的 `compile` 路径换算 —— 见下面的说明。
                cycle: frame as f32,
                event_type: if numeric { 0 } else { 1024 },
                event: if numeric {
                    parse_i32(&name).unwrap_or(0)
                } else {
                    0
                },
                name: if numeric { String::new() } else { name },
                options,
            },
            consumed_depth,
        ))
    }

    /// `ikrule`（`Option_IKRule`，`1216`）。
    fn option_ikrule(&mut self) -> Result<IkRule, QcError> {
        let chain = self.tok(false)?.text;
        let kind_t = self.tok(false)?;
        let kind = match kind_t.text.to_ascii_lowercase().as_str() {
            "touch" => IkRuleType::Touch,
            "footstep" => IkRuleType::Footstep,
            "attachment" => IkRuleType::Attachment,
            "release" => IkRuleType::Release,
            "unlatch" => IkRuleType::Unlatch,
            other => {
                return Err(QcError::new(
                    kind_t.file.clone(),
                    kind_t.line,
                    crate::tr_fmt!("unknown ikrule type %{kind}", kind = format!("{other:?}")),
                ));
            }
        };
        let mut rule = IkRule {
            chain,
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
            use_sequence: false,
        };
        if kind == IkRuleType::Touch {
            let b = self.tok(false)?;
            if !b.text.is_empty() {
                rule.bone = Some(b.text);
            }
        } else if kind == IkRuleType::Attachment {
            // 官方 `Option_IKRule` 的 `attachment` 分支**只读一个 token**
            // （附着点名），把它存进 `pRule->attachment`。
            //
            // ⚠️ 多写的第二个 token（`abi3.qc` 的 `ikrule "leg" attachment
            // "ankle" "muzzle"` 里的 `"muzzle"`）**不是被忽略** —— 它落进
            // 下面的选项循环，而那个循环的不认识分支是
            // `UnGetToken(); return;`（`studiomdl.cpp:1378-1382`）⟹
            // **吐回给调用者**。真 exe 裁决（`target/_r67/oracle_ikrule.js`）：
            // 官方把它当**第二格 blend 动画**（`Cmd_ImpliedAnimation` →
            // `Load_Source` 找 `muzzle.smd`），文件不存在报
            // `could not load file 'muzzle'`，存在则变成 2 格非方形网格报
            // `calcblend failed in idle`（`simplify.cpp:5568`）。
            rule.attachment = Some(self.tok(false)?.text);
        }
        while self.avail() {
            let t = self.tok(false)?;
            match t.text.to_ascii_lowercase().as_str() {
                "usesource" => {
                    rule.use_source = true;
                    rule.use_sequence = false;
                }
                "usesequence" => {
                    rule.use_sequence = true;
                    rule.use_source = false;
                }
                "fakeorigin" => rule.fake_origin = Some(self.v3()?),
                "fakerotate" => rule.fake_rotate = Some(self.v3()?),
                "floor" => rule.floor = Some(self.f()?),
                "height" => rule.height = Some(self.f()?),
                "radius" => {
                    rule.radius = Some(self.f()?);
                    rule.pad = None;
                }
                "pad" => {
                    rule.pad = Some(self.f()?);
                    rule.radius = None;
                }
                "target" => rule.target = Some(self.i()?),
                "contact" => rule.contact = Some(self.i()?),
                "range" => {
                    let mut r: [Option<i32>; 4] = [None; 4];
                    for slot in r.iter_mut() {
                        let v = self.tok(false)?;
                        *slot = if v.text == "." {
                            None
                        } else {
                            Some(parse_i32(&v.text).ok_or_else(|| {
                                QcError::new(
                                    v.file,
                                    v.line,
                                    "IK range must contain integer frames or dots",
                                )
                            })?)
                        };
                    }
                    rule.range = Some(r);
                }
                _ => {
                    // ⚠️ **回归（R67.2）**：官方 `Option_IKRule` 的选项循环
                    // 结尾是 `else { UnGetToken(); return; }`
                    // （`studiomdl.cpp:1378-1382`），而 `UnGetToken` 的真身
                    // 只是 `tokenready = true`（`scriplib.cpp:330-333`）
                    // ⟹ 不认识的 token **原样吐回调用者**，不是吞掉。
                    //
                    // 修前本实现写 `let _ = other;`（静默丢弃）—— 于是
                    // 那个 token 既没进规则、也没回到 `$sequence` 主循环，
                    // 官方「当成第二格 blend 动画」的行为完全消失。
                    //
                    // 吐回之后由 `parse_sequence_body` 的兜底 arm 接住，
                    // 走 `is_source_ref`/动画池/隐含动画三条路 —— 与官方
                    // `ParseSequence` 的三段尾部分派同构（见该函数文档）。
                    self.lex.unget(t);
                    return Ok(rule);
                }
            }
        }
        Ok(rule)
    }

    fn cmd_declareanimation(&mut self) -> Result<(), QcError> {
        let token = self.tok(false)?;
        if self.desc.animations.len() >= 2000 {
            return Err(self.lex.error("Too many animations"));
        }
        self.animation_names.insert(token.text.to_ascii_lowercase());
        self.desc.animations.push(Animation {
            no_anim_block: false,
            no_anim_block_stall: false,
            forward_declared: true,
            name: token.text,
            smd: String::new(),
            loop_restart: 0,
            fudge_loop: false,
            extra_flags: 0,
            motion_rollback: self.default_motion_rollback,
            motion_type: 0,
            fps: Some(0.0),
            looping: false,
            frames: None,
            subtract: None,
            subtract_frame: None,
            ik_rules: Vec::new(),
            no_auto_ik: false,
            weight_list: None,
            cmds: Vec::new(),
            scale: None,
            adjust: None,
            rotation: None,
            src_stack: None,
            src_fps: None,
        });
        Ok(())
    }

    /// `$animation <名> <smd> [块]`（`Cmd_Animation` + `ParseAnimation`）。
    fn cmd_animation(&mut self) -> Result<(), QcError> {
        let name_t = self.tok(false)?;
        let name = name_t.text.clone();
        if !self.animation_names.insert(name.to_ascii_lowercase()) {
            return Err(QcError::new(
                name_t.file.clone(),
                name_t.line,
                crate::tr_fmt!(
                    "duplicate animation name %{name} (the official code says Duplicate animation name)",
                    name = format!("{name:?}")
                ),
            ));
        }
        let smd_raw = self.tok(false)?.text;
        let smd = self.resolve_src(&smd_raw);
        if self.desc.model.first_source.is_none() {
            self.desc.model.first_source = Some(smd.clone());
        }
        self.referenced_files.push(smd.clone());

        let mut anim = Animation {
            no_anim_block: false,
            no_anim_block_stall: false,
            forward_declared: false,
            loop_restart: 0,
            fudge_loop: false,
            extra_flags: 0,
            motion_rollback: self.default_motion_rollback,
            motion_type: 0,
            name,
            smd,
            fps: None,
            looping: false,
            frames: None,
            subtract: None,
            subtract_frame: None,
            ik_rules: Vec::new(),
            no_auto_ik: false,
            weight_list: None,
            cmds: Vec::new(),
            scale: (self.default_scale != 1.0).then_some(self.default_scale),
            adjust: Some(self.default_adjust),
            rotation: Some(self.default_rotation),
            src_stack: None,
            src_fps: None,
        };
        let mut depth = 0i32;
        loop {
            let t = if depth > 0 {
                match self.lex.next_token(true)? {
                    Some(t) => t,
                    None => break,
                }
            } else {
                if !self.avail() {
                    break;
                }
                self.tok(false)?
            };
            if t.text == "{" {
                depth += 1;
                continue;
            }
            if t.text == "}" {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                continue;
            }
            if !self.parse_animation_token(&mut anim, &t)? {
                // 官方 `ParseAnimation` 的 `:2489`：
                // `TokenError( "Unknown animation option'%s'\n", token );`
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "unknown animation option %{opt} (the official code says Unknown animation option'%{opt2}')",
                        opt = format!("{:?}", t.text),
                        opt2 = format!("{:?}", t.text)
                    ),
                ));
            }
        }
        self.desc.animations.push(anim);
        Ok(())
    }

    /// `ParseAnimationToken`（`studiomdl.cpp:2157-2308`）。
    ///
    /// 返回 `Ok(true)` = 认出了并消费了 `t`；`Ok(false)` = 不认识
    /// （官方 `return false`）。**调用方决定**不认识时是报错还是继续：
    /// `$animation` 体报 `Unknown animation option '%s'`，`$sequence`
    /// 兜底则退化成「动画名」。
    ///
    /// # ⚠️ 分支顺序即语义
    ///
    /// 官方是一条 `if/else if` 长链，顺序不能动。mdlc 有两处**故意的**
    /// 偏离，都是把官方走 `ParseCmdlistToken` 的关键字**提前**用专有字段
    /// 接住 —— 否则编译期消费端（`compile.rs` 的 subtract 循环 /
    /// `resolve_weight_lists` / `ik_rules` 挂第一格）会收不到：
    ///
    /// | 关键字 | 官方落点 | mdlc 落点 |
    /// |---|---|---|
    /// | `subtract` | `AnimCmd::Subtract` | `anim.subtract` + `subtract_frame` |
    /// | `weightlist` | `AnimCmd::Weights` | `anim.weight_list` |
    /// | `ikrule` | `AnimCmd::IkRule` | `anim.ik_rules` |
    ///
    /// 其余关键字（`fixuploop`/`alignto`/`align`/`walkframe`/…）走
    /// [`Self::parse_cmdlist_token`] 委派，与官方 `:2273` 一致。
    fn parse_animation_token(&mut self, anim: &mut Animation, t: &Token) -> Result<bool, QcError> {
        match t.text.to_ascii_lowercase().as_str() {
            "fps" => anim.fps = Some(self.f()?),
            "motionrollback" => {
                let value = self.f()?;
                if !value.is_finite() || value < 0.0 {
                    return Err(self
                        .lex
                        .error("motionrollback requires a finite nonnegative duration"));
                }
                anim.motion_rollback = value;
            }
            "origin" => anim.adjust = Some(self.v3()?),
            "rotate" => {
                let mut rotation = anim.rotation.unwrap_or(self.default_rotation);
                rotation[2] = (self.f()? + 90.0).to_radians();
                anim.rotation = Some(rotation);
            }
            "angles" => {
                let mut angles = self.v3()?;
                angles[2] += 90.0;
                anim.rotation = Some(angles.map(f32::to_radians));
            }
            "scale" => anim.scale = Some(self.f()?),
            "frame" | "frames" => {
                let a = self.i()?;
                let b = self.i()?;
                anim.frames = Some([a, b]);
            }
            "subtract" => {
                let reference = self.tok(false)?.text;
                let frame = if self.avail() {
                    let token = self.tok(false)?;
                    if let Some(frame) = parse_i32(&token.text) {
                        frame
                    } else {
                        self.lex.unget(token);
                        0
                    }
                } else {
                    0
                };
                anim.subtract = Some(reference.clone());
                anim.subtract_frame = Some(frame);
                anim.cmds.push(AnimCmd::Subtract {
                    reference,
                    frame,
                    post: true,
                });
            }
            "noautoik" => anim.no_auto_ik = true,
            "autoik" => anim.no_auto_ik = false,
            "weightlist" => {
                let weight_list = self.tok(false)?.text;
                anim.weight_list = Some(weight_list.clone());
                anim.cmds.push(AnimCmd::Weights { weight_list });
            }
            "ikrule" => {
                let r = self.option_ikrule()?;
                anim.ik_rules.push(r);
            }
            // ---- mdlc 扩展：动画侧的两个 `src*` 选项 ----
            //
            // ⚠️ 必须放在 `other =>` **之前**，否则会落到
            // `parse_cmdlist_token` 里变成「未知的命令」。
            "srcstack" => {
                anim.src_stack = Some(self.tok(false)?.text);
            }
            "srcfps" => {
                let t = self.tok(false)?;
                let v: f32 = t.text.parse().map_err(|_| {
                    QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "srcfps needs a number, got %{text}",
                            text = format!("{:?}", t.text)
                        ),
                    )
                })?;
                if !v.is_finite() || v <= 0.0 {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!("srcfps must be a positive finite number, got %{v}", v = v),
                    ));
                }
                anim.src_fps = Some(v);
            }
            other => {
                // 官方 `:2224-2271` 的前缀匹配组 —— 顺序即语义
                // （`strnicmp("loop", token, 4)` 在前，`startloop` 不冲突）。
                if other.starts_with("loop") {
                    anim.looping = true;
                } else if other.starts_with("startloop") {
                    let token = self.tok(false)?;
                    let restart = token.text.parse::<i32>().map_err(|_| {
                        QcError::new(
                            token.file.clone(),
                            token.line,
                            "startloop requires a nonnegative integer frame",
                        )
                    })?;
                    if restart < 0 {
                        return Err(QcError::new(
                            token.file,
                            token.line,
                            "startloop requires a nonnegative integer frame",
                        ));
                    }
                    anim.loop_restart = restart;
                    anim.looping = true;
                } else if other == "fudgeloop" {
                    anim.fudge_loop = true;
                    anim.looping = true;
                } else if other == "noanimblock" {
                    anim.no_anim_block = true;
                } else if other == "noanimblockstall" {
                    anim.no_anim_block_stall = true;
                } else if other == "snap" {
                    anim.extra_flags |= 0x0002;
                } else if other == "post" {
                    anim.extra_flags |= 0x0010;
                } else if other == "realtime" {
                    anim.extra_flags |= 0x0080;
                } else if self.parse_cmdlist_token(t, &mut anim.cmds)? {
                    // 官方 `:2273` 的 `ParseCmdlistToken` 委派 —— 整套 25 个
                    // 关键字在 `$animation` 体内同样合法。
                } else if other == "cmdlist" {
                    // 官方 `:2277-2300`：按名找 `$cmdlist`，逐条**拷贝**到
                    // 本动画的 `cmds[]`（不是引用），超 `MAXSTUDIOCMDS` 报
                    // `Too many cmds in %s`。
                    self.append_cmdlist(t, &mut anim.cmds)?;
                } else if let Some(control) = lookup_control(&t.text) {
                    anim.motion_type |= control;
                } else {
                    // 官方 `return false` —— 由调用方决定报错还是继续
                    // （`$animation` 体报 `Unknown animation option '%s'`，
                    // `$sequence` 兜底退化成「动画名」）。
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// `cmdlist "<名>"` 的**引用**展开（官方 `ParseAnimationToken` 的
    /// `cmdlist` 分支，`studiomdl.cpp:2277-2300`）—— 三处共用。
    ///
    /// 官方按名线性扫 `g_cmdlist[]`（`stricmp`），未命中报
    /// `unknown cmdlist %s`；命中则逐条**拷贝**（`panim->cmds[numcmds++] =
    /// g_cmdlist[i].cmds[j]`），每次拷贝前检查 `numcmds >= MAXSTUDIOCMDS`
    /// 报 `Too many cmds in %s`。
    ///
    /// ⚠️ 官方**不拷贝名字**，只拷贝命令 —— `$cmdlist` 定义之后被修改
    /// 不影响已展开的引用。
    fn append_cmdlist(&mut self, t: &Token, out: &mut Vec<AnimCmd>) -> Result<(), QcError> {
        let n = self.tok(false)?.text;
        let Some(cl) = self
            .desc
            .cmd_lists
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&n))
        else {
            return Err(QcError::new(
                t.file.clone(),
                t.line,
                crate::tr_fmt!(
                    "unknown cmdlist %{name} (the official code says unknown cmdlist %{name2})",
                    name = format!("{n:?}"),
                    name2 = n
                ),
            ));
        };
        let copied = cl.cmds.clone();
        for c in copied {
            if out.len() >= MAX_CMDS {
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "too many commands: over the official limit of %{max} (the official code says Too many cmds in)",
                        max = MAX_CMDS
                    ),
                ));
            }
            out.push(c);
        }
        Ok(())
    }

    /// `$cmdlist <名> { ... }`（`Cmd_Cmdlist`，`studiomdl.cpp:2317-2378`）。
    ///
    /// 容器本身很薄：读名字，然后跑一个与 `cmd_sequence` **同构**的 depth
    /// 循环，每个 token 交给 [`Self::parse_cmdlist_token`]。认不出来就是
    /// `unknown command: %s`（官方原文）。
    ///
    /// ⚠️ 官方**不检查** `depth == 0` 就退出 —— 只有 `endofscript` 时
    /// 才检查 `depth != 0`（报 `missing }\n`）。照抄。
    fn cmd_cmdlist(&mut self) -> Result<(), QcError> {
        let name_t = self.tok(false)?;
        let mut cmds: Vec<AnimCmd> = Vec::new();
        let mut depth = 0i32;
        loop {
            let t = if depth > 0 {
                match self.lex.next_token(true)? {
                    Some(t) => t,
                    None => break,
                }
            } else {
                if !self.avail() {
                    break;
                }
                self.tok(false)?
            };
            if t.text == "{" {
                depth += 1;
                continue;
            }
            if t.text == "}" {
                depth -= 1;
                continue;
            }
            if !self.parse_cmdlist_token(&t, &mut cmds)? {
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "unknown command in $cmdlist: %{cmd} (the official code says unknown command: %{cmd2})",
                        cmd = format!("{:?}", t.text),
                        cmd2 = t.text
                    ),
                ));
            }
        }
        self.desc.cmd_lists.push(CmdList {
            name: name_t.text,
            cmds,
        });
        Ok(())
    }

    /// `$continue <名> <选项...>`（`Cmd_Continue`，`studiomdl.cpp:3170-3198`）。
    ///
    /// 官方按名字找一个**已存在**的序列或动画（`LookupSequence` 先，
    /// `LookupAnimation` 后 —— 后者内部还会回退查序列池），然后以
    /// `isAppend = true` 重入 `ParseSequence` / `ParseAnimation`：
    /// 把本行剩下的选项**追加**到那个实体上，**不新建**。
    ///
    /// ⚠️ 官方在解析前先 `GetToken(true); UnGetToken();` 再检查
    /// `token[0] != '$'` —— 这是**宏展开守卫**。`anims_fix.qci:137` 的
    /// `$continue $FileName$` 在宏展开后 `$FileName$` 已变成真实序列名，
    /// 所以会真的执行；但若展开结果为空（或下一个 token 仍是 `$...`），
    /// 官方**什么都不做**而不是报错。照抄。
    fn cmd_modify_animation(&mut self, mode: &str) -> Result<(), QcError> {
        let name_t = self.tok(false)?;
        let name = name_t.text.clone();
        // 官方 `Cmd_Continue`（`studiomdl.cpp:3170-3198`）：
        //
        // ```c
        // GetToken(false);
        // s_sequence_t *pseq = LookupSequence( token );
        // if (pseq) { GetToken(true); UnGetToken(); if (token[0] != '$') ParseSequence( pseq, true ); return; }
        // else { s_animation_t *panim = LookupAnimation( token );
        //     if (panim) { GetToken(true); UnGetToken(); if (token[0] != '$') ParseAnimation( panim, true ); return; } }
        // TokenError( "unknown continue animation %s\n", token );
        // ```
        //
        // ⚠️ **`LookupSequence` 优先**。`LookupAnimation` 内部虽然也会回退查
        // 序列池（`studiomdl.cpp:2381-2397` 的 `return pseq->panim[0][0]`），
        // 但那条路只有在名字**不是**序列时才会走到 —— 所以这里直接写成
        // 「先序列、后动画」，与官方等价。
        let si = self
            .desc
            .sequences
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(&name));
        let ai = self
            .desc
            .animations
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(&name));
        if si.is_none() && ai.is_none() {
            return Err(QcError::new(
                name_t.file.clone(),
                name_t.line,
                crate::tr_fmt!(
                    "$continue cannot find the sequence or animation %{name} (the official code says unknown continue animation %{name2})",
                    name = format!("{name:?}"),
                    name2 = name
                ),
            ));
        }

        // 宏展开守卫（官方 `GetToken(true); UnGetToken(); if (token[0] != '$')`）。
        //
        // ⚠️ **必须跨帧**：官方 `GetToken(true)` 在宏体耗尽时走
        // `EndOfScript` 弹掉宏帧、继续读**外层**脚本
        // （`scriplib.cpp:435-457` 的 `script--; return GetToken(crossline);`）。
        //
        // ⚠️ **修复前这里写的是 `if !self.avail() { return Ok(()); }`** ——
        // `token_available()` 只看**当前帧**（`lexer.rs` 的
        // `self.stack.last()`），宏体末行之后没有 token 就返回 `false`，
        // 于是 `$continue` 提前返回、宏帧**没被弹出**；随后 `run()` 的
        // `next_token(true)` 才跨帧，外层那行剩下的 token 就落到
        // **顶层 `dispatch`** 的 `other =>` 兜底。实测
        // `anims_fix.qci:212`
        // `$DebiddoChargerLoop Idle_Fall_From_Charger ACT_TERROR_IDLE_FALL_FROM_CHARGERHIT -1`
        // 报 `未知的 QC 命令 "act_terror_idle_fall_from_chargerhit"`。
        if mode != "continue" && !self.avail() {
            return if mode == "prepend" {
                Err(self.lex.error("$prepend requires an animation command"))
            } else {
                Ok(())
            };
        }
        let peek = match self.lex.next_token(mode == "continue")? {
            Some(t) => t,
            // 真正的脚本结尾：官方 `GetToken(true)` 返回 false 后 `token`
            // 保持旧值，随后 `ParseSequence` 的 `TokenAvailable()` 也是
            // false ⟹ 什么都不做。照抄。
            None => return Ok(()),
        };
        if peek.text.starts_with('$') {
            self.lex.unget(peek);
            return Ok(());
        }
        self.lex.unget(peek);

        // 序列优先（官方 `LookupSequence` 先查）。`ParseSequence( pseq, true )`
        // 的序列级选项链与 `$sequence` **完全相同** —— 复用
        // `parse_sequence_body`，只把 `is_append` 置真。
        //
        // ⚠️ 修复前这里写的是「先看第一格名字能不能命中 `$animation`，
        // 命中就改走动画路径」，于是 `ACT_*`/`fps`/`fadein`/`fadeout`/
        // `addlayer`/`hidden`/`numframes`/`ikrule` 这些**序列级**选项全部报
        // `未知的命令`。官方没有这条捷径：命中序列池就一定走
        // `ParseSequence(pseq, true)`。
        if let Some(si) = si {
            let mut seq = self.desc.sequences[si].clone();
            let first = seq.blends.first().unwrap_or(&seq.smd);
            let source = self
                .desc
                .animations
                .iter()
                .position(|anim| anim.name.eq_ignore_ascii_case(first));
            let before = source.map_or(seq.cmds.len(), |index| {
                self.desc.animations[index].cmds.len()
            });
            self.parse_sequence_body(&mut seq, &name_t, true)?;
            if mode == "prepend" {
                let commands =
                    source.map_or(&mut seq.cmds, |index| &mut self.desc.animations[index].cmds);
                if commands.len() == before {
                    return Err(self.lex.error("$prepend requires an animation command"));
                }
                commands.rotate_right(1);
            }
            self.desc.sequences[si] = seq;
            return Ok(());
        }

        let ai = ai.expect("already checked that it is not None");
        // `ParseAnimation( panim, true )` —— 官方那个 `isAppend` 形参
        // **完全没用**（`:2442-2499` 的函数体只有 `while (1)` 循环），
        // 所以这里与 `$animation` 体同一套解析。
        let mut anim = self.desc.animations[ai].clone();
        let before = anim.cmds.len();
        let mut depth = 0i32;
        loop {
            let t = if depth > 0 {
                match self.lex.next_token(true)? {
                    Some(t) => t,
                    None => break,
                }
            } else {
                if !self.avail() {
                    break;
                }
                self.tok(false)?
            };
            if t.text == "{" {
                depth += 1;
                continue;
            }
            if t.text == "}" {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
                continue;
            }
            if !self.parse_animation_token(&mut anim, &t)? {
                // 官方 `ParseAnimation` 的 `:2489`。
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "unknown animation option %{opt} (the official code says Unknown animation option'%{opt2}')",
                        opt = format!("{:?}", t.text),
                        opt2 = format!("{:?}", t.text)
                    ),
                ));
            }
        }
        if mode == "prepend" {
            if anim.cmds.len() == before {
                return Err(self.lex.error("$prepend requires an animation command"));
            }
            anim.cmds.rotate_right(1);
        }
        self.desc.animations[ai] = anim;
        Ok(())
    }

    /// `ParseCmdlistToken`（`studiomdl.cpp:1698-2150`）—— **一套 25 个关键字**，
    /// 三处共用：`$cmdlist` 块、`$animation` 体、`$sequence` 块。
    ///
    /// 返回 `Ok(true)` = 认出了并消费了 `t`（官方 `return true`）；
    /// `Ok(false)` = 不认识（官方 `return false`，由调用方决定报错还是继续）。
    ///
    /// ⚠️ 官方开头是 `if (numcmds >= MAXSTUDIOCMDS) return false;` —— 满了
    /// 就**当不认识**，于是调用方报 `unknown command` / `Unknown animation
    /// option`。照抄。
    ///
    /// ⚠️ 参考动画名一律**不在这里解析**：官方用 `LookupAnimation` 立刻查，
    /// mdlc 与 `subtract`/`weightlist` 同一套做法 —— 留到 `compile.rs`
    /// （`LookupAnimation` 先查动画池再回退序列池，解析期也看不到后面才
    /// 声明的动画）。
    fn parse_cmdlist_token(&mut self, t: &Token, out: &mut Vec<AnimCmd>) -> Result<bool, QcError> {
        if out.len() >= MAX_CMDS {
            return Ok(false);
        }
        let low = t.text.to_ascii_lowercase();
        // `weightlist` 官方是 `strnicmp(token,"weightlist",6)` ——
        // **6 字符前缀**（`weight`），不是全词比较。
        let cmd = if low.starts_with("weight") {
            AnimCmd::Weights {
                weight_list: self.tok(false)?.text,
            }
        } else {
            match low.as_str() {
                "fixuploop" => {
                    let start = self.i()?;
                    let end = self.i()?;
                    AnimCmd::FixupLoop { start, end }
                }
                "subtract" | "presubtract" => AnimCmd::Subtract {
                    reference: self.tok(false)?.text,
                    frame: self.i()?,
                    // `subtract` 置 `STUDIO_POST`，`presubtract` 不置
                    // （`studiomdl.cpp:1750`）。
                    post: low == "subtract",
                },
                "alignto" => AnimCmd::Align {
                    reference: self.tok(false)?.text,
                    // `STUDIO_X | STUDIO_Y`。
                    motion_type: 0x0001 | 0x0002,
                    src_frame: 0,
                    dest_frame: 0,
                    bone: None,
                },
                "align" => {
                    let reference = self.tok(false)?.text;
                    let (motion_type, src_frame) = self.parse_control_run(t, "align")?;
                    let dest_frame = self.i()?;
                    AnimCmd::Align {
                        reference,
                        motion_type,
                        src_frame,
                        dest_frame,
                        bone: None,
                    }
                }
                "alignboneto" => {
                    let bone = self.tok(false)?.text;
                    AnimCmd::Align {
                        reference: self.tok(false)?.text,
                        motion_type: 0x0001 | 0x0002,
                        src_frame: 0,
                        dest_frame: 0,
                        bone: Some(bone),
                    }
                }
                "match" => AnimCmd::Match {
                    reference: self.tok(false)?.text,
                },
                "matchblend" => AnimCmd::MatchBlend {
                    reference: self.tok(false)?.text,
                    src_frame: self.i()?,
                    dest_frame: self.i()?,
                    dest_pre: self.i()?,
                    dest_post: self.i()?,
                },
                "worldspaceblend" => AnimCmd::WorldSpaceBlend {
                    reference: self.tok(false)?.text,
                    start_frame: 0,
                    loops: false,
                },
                "worldspaceblendloop" => {
                    let reference = self.tok(false)?.text;
                    // ⚠️ 官方这里用的是 `atoi`（不是 `verify_atoi`）——
                    // 非法输入静默变 0。
                    let start_frame = self
                        .tok(false)
                        .ok()
                        .and_then(|t| parse_i32(&t.text))
                        .unwrap_or(0);
                    AnimCmd::WorldSpaceBlend {
                        reference,
                        start_frame,
                        loops: true,
                    }
                }
                "rotateto" => AnimCmd::Angle { angle: self.f()? },
                "ikrule" => AnimCmd::IkRule {
                    rule: self.option_ikrule()?,
                },
                "ikfixup" => AnimCmd::IkFixup {
                    rule: self.option_ikrule()?,
                },
                "walkframe" => {
                    let end_frame = self.i()?;
                    // 官方的控制位循环**带 `UnGetToken`**（`:1965`）——
                    // 遇到第一个非控制 token 就吐回。
                    let motion_type = self.parse_control_peek()?;
                    AnimCmd::Motion {
                        motion_type,
                        end_frame,
                    }
                }
                "walkalignto" => {
                    let end_frame = self.i()?;
                    let reference = self.tok(false)?.text;
                    let motion_type = self.parse_control_peek()?;
                    AnimCmd::RefMotion {
                        motion_type,
                        end_frame,
                        // 官方 `iSrcFrame = iEndFrame`（`:1988`）。
                        src_frame: end_frame,
                        reference,
                        ref_frame: 0,
                    }
                }
                "walkalign" => {
                    let end_frame = self.i()?;
                    let reference = self.tok(false)?.text;
                    let (motion_type, ref_frame) = self.parse_control_run(t, "walkalign")?;
                    let src_frame = self.i()?;
                    AnimCmd::RefMotion {
                        motion_type,
                        end_frame,
                        src_frame,
                        reference,
                        ref_frame,
                    }
                }
                "derivative" => AnimCmd::Derivative { scale: self.f()? },
                // 官方**不读任何 token**（`:2081-2084`）。
                "noanimation" => AnimCmd::NoAnimation,
                "lineardelta" => AnimCmd::LinearDelta { flags: 0x0010 },
                // ⚠️ `splinedelta` 产生的仍是 `CMD_LINEARDELTA`
                // （`CMD_SPLINEDELTA` 是死常量）。
                "splinedelta" => AnimCmd::LinearDelta {
                    flags: 0x0010 | 0x0040,
                },
                "compress" => AnimCmd::Compress { frames: self.i()? },
                "numframes" => AnimCmd::NumFrames { frames: self.i()? },
                "localhierarchy" => {
                    let bone = self.tok(false)?.text;
                    let parent = self.tok(false)?.text;
                    let range = if self.avail() {
                        let token = self.tok(false)?;
                        if token.text.eq_ignore_ascii_case("range") {
                            [self.i()?, self.i()?, self.i()?, self.i()?]
                        } else {
                            self.lex.unget(token);
                            [0; 4]
                        }
                    } else {
                        [0; 4]
                    };
                    AnimCmd::LocalHierarchy {
                        bone,
                        parent,
                        range,
                    }
                }
                "counterrotate" => AnimCmd::CounterRotate {
                    bone: self.tok(false)?.text,
                    target_angle: None,
                },
                "counterrotateto" => {
                    // 官方顺序：先三个角度（pitch/yaw/roll），**再**骨骼名。
                    let a = self.v3()?;
                    AnimCmd::CounterRotate {
                        bone: self.tok(false)?.text,
                        target_angle: Some(a),
                    }
                }
                _ => return Ok(false),
            }
        };
        out.push(cmd);
        Ok(true)
    }

    /// `align` / `walkalign` 共用的控制位循环 —— 官方这两处的循环
    /// **不带 `UnGetToken`**：循环退出时 `token` 里留着那个非控制 token，
    /// 紧接着被 `verify_atoi` 当帧号用掉。
    ///
    /// 返回 `(控制位掩码, 帧号)`。
    fn parse_control_run(&mut self, t: &Token, what: &str) -> Result<(i32, i32), QcError> {
        let mut motion_type = 0i32;
        let mut cur = self.tok(false)?;
        while let Some(c) = lookup_control(&cur.text) {
            motion_type |= c;
            cur = self.tok(false)?;
        }
        if motion_type == 0 {
            return Err(QcError::new(
                t.file.clone(),
                t.line,
                crate::tr_fmt!(
                    "%{what} is missing controls (the official code says missing controls on %{what2})",
                    what = what,
                    what2 = what
                ),
            ));
        }
        let frame = parse_i32(&cur.text).ok_or_else(|| {
            QcError::new(
                cur.file.clone(),
                cur.line,
                crate::tr_fmt!(
                    "expected an integer, got %{text}",
                    text = format!("{:?}", cur.text)
                ),
            )
        })?;
        Ok((motion_type, frame))
    }

    /// `walkframe` / `walkalignto` 共用的控制位循环 —— 官方这两处**带
    /// `UnGetToken`**：遇到第一个非控制 token 就吐回，不消费它。
    fn parse_control_peek(&mut self) -> Result<i32, QcError> {
        let mut motion_type = 0i32;
        while self.avail() {
            let t = self.tok(false)?;
            match lookup_control(&t.text) {
                Some(c) => motion_type |= c,
                None => {
                    self.lex.unget(t);
                    break;
                }
            }
        }
        Ok(motion_type)
    }

    /// `$definebone <名> <父> <x> <y> <z> <pitch> <yaw> <roll> [<rx> <ry> <rz> <rp> <ry2> <rr>]`。
    ///
    /// # ⚠️ 参数顺序是 `QAngle`，而落盘字段是 `RadianEuler` —— 必须重排
    ///
    /// 官方 `Cmd_DefineBone`（`studiomdl.cpp:5920-5925`）把这 3 个数字
    /// **按顺序**读进 `QAngle angles`，再 `AngleMatrix(angles, pos, rawLocal)`
    /// （`:5926`）。`QAngle` 是 `{pitch, yaw, roll}`（= `{x, y, z}`）。
    ///
    /// 而 `mstudiobone_t.rotation` 是 **`RadianEuler`** `{roll, pitch, yaw}`
    /// —— `RebuildLocalPose` 用 `MatrixAngles`（`simplify.cpp:4415`）反解，
    /// 落盘就是这个顺序。
    ///
    /// 实测（`docs/_probe/oracle_definebone_rotorder.js`，真 `studiomdl`）：
    ///
    /// ```text
    /// QC  : $definebone "b1" "b0" 0 0 5 11 22 33     ← pitch=11 yaw=22 roll=33
    /// 官方: rot = [33.0000, 11.0000, 22.0000]°        ← [roll, pitch, yaw]
    /// ```
    ///
    /// 所以 `[roll, pitch, yaw] = [输入[2], 输入[0], 输入[1]]`。
    /// 后 6 个数字的旋转部分**同理**（`AngleMatrix(angles, pos, srcRealign)`，
    /// `:5945`）—— 官方 `-definebones` 的 dump 用 `MatrixAngles(srcRealign)`
    /// 打印成 `QAngle`，实测给 `11 22 33` 就打印 `11 22 33`
    /// （`oracle_realign_rotorder.js`），所以它与前 3 个数字同序。
    ///
    /// 重排在这里做（而不是在 `explicit_src_realign`）是为了让
    /// **QC 与 TOML 两条路径产出同一个 `Bone`** —— TOML 的 `rotation` /
    /// `realign_rotation` 本来就是 `[roll, pitch, yaw]`（实测
    /// `parity/refpose.toml` 与官方产物逐位相同）。
    ///
    /// # `bPreAligned` **总是** true
    ///
    /// `Cmd_DefineBone` 只在 12 数字形式里写 `g_importbone[].bPreAligned`
    /// （`:5930`），但那只影响 `g_importbone` 这个**中间**结构。骨骼表里的
    /// `bPreAligned` 由 `BuildGlobalBonetable` 对**每一条** `$definebone`
    /// **无条件**置位（`simplify.cpp:3653`），与数字个数无关。
    ///
    /// 实测（`ipq2` = 6 数字 + `$realignbones`）：官方产物 `a.pos=[0,0,10]`
    /// ——**没有**被重排（对照组 `ipq1` 不写 `$definebone`，同样写
    /// `$realignbones`，`a.pos` 变成 `[10,0,0]`）。
    ///
    /// > 早先这里按「有没有后 6 个数字」置 `pre_aligned`，于是 6 数字形式
    /// > 被当成**未**预对齐 ⟹ 走了 `RealignBones` ⟹ 参考姿态被改写。
    /// > 症状：`ipq2` 走 QC 路径得 `[10,0,0]`，走 TOML 路径得 `[0,0,10]`，
    /// > 而官方是后者 —— **同一个夹具两条路径不一致**。
    fn cmd_definebone(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let parent = self.tok(false)?.text;
        let pos = self.v3()?;
        let rot = self.v3()?;
        // 官方：本行还有 token ⟹ 读 `srcRealign`（后 6 个数字）。
        let mut realign_pos = None;
        let mut realign_rot = None;
        if self.avail() {
            realign_pos = Some(self.v3()?);
            realign_rot = Some(self.v3()?);
        }
        self.import_bones.push(Bone {
            name,
            parent: if parent.is_empty() {
                None
            } else {
                Some(parent)
            },
            position: Some(pos),
            // `QAngle{pitch,yaw,roll}` → `RadianEuler{roll,pitch,yaw}`。
            rotation: Some([rot[2], rot[0], rot[1]]),
            flags: None,
            surface_prop: None,
            bonemerge: false,
            // 见上：官方对每条 `$definebone` 都置位，与数字个数无关。
            pre_aligned: Some(true),
            realign_position: realign_pos,
            realign_rotation: realign_rot.map(|r| [r[2], r[0], r[1]]),
        });
        Ok(())
    }

    /// `$bonemerge "<骨骼名>"`。
    fn cmd_bonemerge(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        // 可能引用尚未定义的骨骼（SMD 里的）—— 先记成待办。
        self.bonemerge_names.push(t.text);
        Ok(())
    }

    /// `$attachment "<名>" "<骨骼>" <x> <y> <z> [absolute|rigid|world_align|rotate rx ry rz|x_and_z_axes ...]`。
    fn cmd_attachment(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        let bone = self.tok(false)?.text;
        let pos = self.v3()?;
        // ⚠️ 这个 `flags` 是**直接落进产物**的 `mstudioattachment_t.flags`。
        // 官方只有 `world_align` 会写进去（`studiomdl.cpp:5255` +
        // `write.cpp:350`）—— `absolute`/`rigid` 走的是**另一个**字段
        // `g_attachment[].type`（`studiomdl.h:277-278`），从不落盘。
        // 真 studiomdl 裁决（`docs/_probe/oracle_attachment_flags.js`）：
        // `absolute`/`rigid` 官方都写 `0x0`，只有 `world_align` 写 `0x10000`。
        let mut flags = 0i32;
        let mut rotation = [0.0f32; 3];
        let mut absolute = false;
        let mut rigid = false;
        // `local` 的**旋转**最终由最后一个改旋转的选项决定（官方就是顺序覆盖
        // 同一个 `local` 矩阵）。mdlc 把旋转拆成了欧拉角 + 这一个 bit。
        let mut absolute_rotation: Option<bool> = None;
        while self.avail() {
            let t = self.tok(false)?;
            match t.text.to_ascii_lowercase().as_str() {
                "absolute" => {
                    absolute = true;
                    // 官方紧接着就 `AngleIMatrix( g_defaultrotation, local )`
                    // （`studiomdl.cpp:5246`）。
                    if self.default_rotation == crate::ani_writer::ROOT_REFERENCE_ANGLES {
                        absolute_rotation = Some(true);
                    } else {
                        rotation = crate::bone_math::matrix_angles(
                            &crate::bone_math::angle_imatrix(self.default_rotation),
                        )
                        .map(f32::to_degrees);
                        absolute_rotation = Some(false);
                    }
                }
                "rigid" => rigid = true,
                "world_align" => flags |= 0x10000, // ATTACHMENT_FLAG_WORLD_ALIGN
                "rotate" => {
                    // ⚠️ **官方读进来的是 `QAngle{pitch, yaw, roll}`**，
                    // 而 `Attachment.rotation` 的约定是 `RadianEuler{roll, pitch, yaw}`
                    // （写出器 `to_radians()` 后喂 `bone_math::angle_matrix`）。
                    // **必须重排**，否则附着点朝向错 90°。
                    //
                    // 官方 `Cmd_Attachment`（`studiomdl.cpp:5260-5272`）：
                    // ```cpp
                    // else if (stricmp(token,"rotate") == 0) {
                    //     QAngle angles;
                    //     for (int i = 0; i < 3; ++i) { GetToken(false); angles[i] = verify_atof(token); }
                    //     AngleMatrix( angles, g_attachment[...].local );
                    // }
                    // ```
                    // `QAngle` 的成员顺序是 `x=pitch, y=yaw, z=roll`，而
                    // `AngleMatrix(QAngle)`（`mathlib_base.cpp:2837`）按
                    // `angles[PITCH/YAW/ROLL]` 组装。
                    //
                    // 真 studiomdl 裁决（`docs/_probe/oracle_attachment_rotate.js`）：
                    // `rotate 0 0 -90` / `90 0 0` / `0 90 0` 三个受控夹具，
                    // 官方落盘的 `local` 旋转与 **QAngle 口径**吻合到 `4.37e-8`，
                    // 与 RadianEuler 口径差 **1.0**（正好 90°）。
                    //
                    // 影响面：真实语料里 `$attachment` 带 `rotate` 的有 **315 处**，
                    // 而 parity 唯一用到它的夹具写的是 `rotate 0 0 0`（恒等）
                    // ⟹ **parity 测不出来**。
                    let mut q = [0.0f32; 3];
                    for slot in q.iter_mut() {
                        if !self.avail() {
                            break;
                        }
                        *slot = self.f()?;
                    }
                    // `QAngle{pitch, yaw, roll}` → `RadianEuler{roll, pitch, yaw}`
                    rotation = [q[2], q[0], q[1]];
                    // `rotate` 在 `absolute` **之后** ⟹ 旋转来自 `rotate`
                    // （官方同一个 `local` 被后写者覆盖，`studiomdl.cpp:5268`）。
                    absolute_rotation = Some(false);
                }
                "x_and_z_axes" => {
                    let x = self.v3()?;
                    let z = self.v3()?;
                    let matrix = attachment_axes(x, z).map_err(|error| self.lex.error(error))?;
                    rotation = crate::bone_math::matrix_angles(&matrix).map(f32::to_degrees);
                    absolute_rotation = Some(false);
                }
                other => {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "unknown $attachment option %{opt}",
                            opt = format!("{other:?}")
                        ),
                    ));
                }
            }
        }
        self.desc.attachments.push(Attachment {
            name,
            bone,
            position: Some(pos),
            rotation: Some(rotation),
            absolute,
            absolute_rotation,
            rigid,
            flags: Some(flags),
            synthetic: false,
            resolved: None,
        });
        Ok(())
    }

    /// `$hboxset "<名>"`。
    fn cmd_hboxset(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        if let Some(index) = std::iter::once(&self.desc.hitboxes)
            .chain(&self.desc.additional_hitbox_sets)
            .position(|set| {
                set.set_name
                    .as_deref()
                    .unwrap_or("default")
                    .eq_ignore_ascii_case(&t.text)
            })
        {
            self.current_hitbox_set = index;
            if index == 0 {
                self.desc.hitboxes.set_name = Some(t.text);
            }
        } else if self.desc.hitboxes.set_name.is_none() && self.desc.hitboxes.boxes.is_empty() {
            self.desc.hitboxes.set_name = Some(t.text);
            self.current_hitbox_set = 0;
        } else {
            self.desc.additional_hitbox_sets.push(Hitboxes {
                set_name: Some(t.text),
                ..Hitboxes::default()
            });
            self.current_hitbox_set = self.desc.additional_hitbox_sets.len();
        }
        Ok(())
    }

    fn cmd_controller(&mut self) -> Result<(), QcError> {
        let input = self.tok(false)?;
        let field = if input.text.eq_ignore_ascii_case("mouth") {
            4
        } else {
            parse_i32(&input.text).unwrap_or(-1)
        };
        if !(0..=4).contains(&field) {
            return Err(QcError::new(
                input.file,
                input.line,
                "Controller input must be 0 through 3, or mouth",
            ));
        }
        let bone = self.tok(false)?.text;
        let axis = self.tok(false)?;
        let mut kind = match axis.text.to_ascii_lowercase().as_str() {
            "x" => 1,
            "y" => 2,
            "z" => 4,
            "xr" => 8,
            "yr" => 16,
            "zr" => 32,
            _ => {
                return Err(QcError::new(
                    axis.file,
                    axis.line,
                    "Invalid bone controller axis",
                ));
            }
        };
        let start = self.f()?;
        let end = self.f()?;
        if !start.is_finite()
            || !end.is_finite()
            || start.abs() > 1_000_000.0
            || end.abs() > 1_000_000.0
        {
            return Err(QcError::new(
                input.file,
                input.line,
                "Invalid controller bounds",
            ));
        }
        if kind >= 8 && ((start + 360.0) as i32) % 360 == ((end + 360.0) as i32) % 360 {
            kind |= 0x8000;
        }
        self.desc.bonecontrollers.push(BoneController {
            bone,
            type_: Some(kind),
            start: Some(start),
            end: Some(end),
            rest: Some(0),
            input_field: Some(field),
        });
        Ok(())
    }

    /// `$hbox <组> "<骨骼>" <minx> <miny> <minz> <maxx> <maxy> <maxz> ["<名>"]`。
    fn cmd_hbox(&mut self) -> Result<(), QcError> {
        let group = self.i()?;
        let bone = self.tok(false)?.text;
        let bbmin = self.v3()?;
        let bbmax = self.v3()?;
        let name = if self.avail() {
            Some(self.tok(false)?.text)
        } else {
            None
        };
        let set = if self.current_hitbox_set == 0 {
            &mut self.desc.hitboxes
        } else {
            &mut self.desc.additional_hitbox_sets[self.current_hitbox_set - 1]
        };
        set.boxes.push(Hitbox {
            bone,
            group: Some(group),
            bbmin,
            bbmax,
            name,
        });
        Ok(())
    }

    /// `$ikchain "<名>" "<骨骼>" [knee x y z] [height h] [pad p] [floor f] [center x y z]`。
    fn transition_node(&mut self) -> Result<String, QcError> {
        let token = self.tok(false)?;
        if token.text.is_empty()
            || self.desc.model.transition_nodes.len() >= 255
                && !self
                    .desc
                    .model
                    .transition_nodes
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&token.text))
        {
            return Err(QcError::new(
                token.file,
                token.line,
                "Invalid or excessive transition node names",
            ));
        }
        if !self
            .desc
            .model
            .transition_nodes
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&token.text))
        {
            self.desc.model.transition_nodes.push(token.text.clone());
        }
        Ok(token.text)
    }

    fn cmd_skiptransition(&mut self) -> Result<(), QcError> {
        let mut nodes = Vec::new();
        while self.avail() {
            nodes.push(self.transition_node()?);
        }
        for from in &nodes {
            for to in &nodes {
                if !from.eq_ignore_ascii_case(to) {
                    self.desc
                        .model
                        .transition_skips
                        .push([from.clone(), to.clone()]);
                }
            }
        }
        Ok(())
    }

    fn cmd_ikchain(&mut self) -> Result<(), QcError> {
        let name = self.tok(false)?.text;
        if self
            .desc
            .ikchains
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&name))
        {
            // 官方：重复链名**静默忽略**（消费掉本行剩余 token）。
            self.skip_rest_of_line();
            return Ok(());
        }
        let bone = self.tok(false)?.text;
        let mut knee_dir = None;
        let mut center = None;
        let mut height = None;
        let mut floor = None;
        let mut pad = None;
        while self.avail() {
            let t = self.tok(false)?;
            match t.text.to_ascii_lowercase().as_str() {
                "knee" => knee_dir = Some(self.v3()?),
                "height" => height = Some(self.f()?),
                "pad" => pad = Some(self.f()?),
                "floor" => floor = Some(self.f()?),
                "center" => center = Some(self.v3()?),
                other => {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "unknown $ikchain option %{opt}",
                            opt = format!("{other:?}")
                        ),
                    ));
                }
            }
        }
        self.desc.ikchains.push(IkChain {
            name,
            bone,
            knee_dir,
            center,
            height,
            floor,
            pad,
        });
        Ok(())
    }

    /// `$ikautoplaylock "<链名>" <posW> <localQW>`。
    fn cmd_ikautoplaylock(&mut self) -> Result<(), QcError> {
        let chain = self.tok(false)?.text;
        let pos_weight = self.f()?;
        let local_q_weight = self.f()?;
        self.desc.ik_autoplay_locks.push(IkAutoplayLock {
            chain,
            pos_weight,
            local_q_weight,
        });
        Ok(())
    }

    /// `$includemodel "<路径>"` —— 官方**自动加 `models/` 前缀**。
    fn cmd_includemodel(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        // 官方 `Cmd_IncludeModel`（`studiomdl.cpp:5961`）把 `"models/"` 直接
        // `strcat` 到 token 前面。
        let name = t.text.replace('\\', "/");
        let full = if name.starts_with("models/") {
            name
        } else {
            format!("models/{name}")
        };
        self.desc.include_models.push(full);
        Ok(())
    }

    /// `$lod <距离> { replacemodel "a" "b" | bonetreecollapse "b" | replacebone "a" "b" | nofacial }`。
    fn cmd_lod(&mut self, shadow: bool) -> Result<(), QcError> {
        if self
            .pending_lods
            .iter()
            .any(|lod| lod.options.switch_point == Some(-1.0))
        {
            return Err(self
                .lex
                .error("$shadowlod must be the final LOD and can only occur once"));
        }
        let switch = if shadow { -1.0 } else { self.f()? };
        if !switch.is_finite() {
            return Err(self
                .lex
                .error("LOD switch distance must be finite and non-negative"));
        }
        if !shadow && switch < 0.0 {
            return Err(self
                .lex
                .error("negative $lod switch distances are not supported; use $shadowlod"));
        }
        if shadow {
            self.gflags |= 0x40;
        }
        let opening = self.tok(true)?;
        if opening.text != "{" {
            return Err(QcError::new(
                opening.file,
                opening.line,
                "$lod requires a block",
            ));
        }
        let mut parsed = ParsedLod {
            options: LodModel {
                reverse: false,
                removed: false,
                removed_materials: Vec::new(),
                material_replacements: Vec::new(),
                smd: None,
                switch_point: Some(switch),
                bone_tree_collapse: Vec::new(),
                replace_bone: Vec::new(),
                no_facial: false,
            },
            replacements: Vec::new(),
        };
        loop {
            let token = self.lex.next_token(true)?.ok_or_else(|| {
                self.lex
                    .error("unterminated $lod block (missing closing brace)")
            })?;
            match token.text.to_ascii_lowercase().as_str() {
                "}" => break,
                "replacemodel" => {
                    let from = self.tok(false)?;
                    let to = self.tok(false)?;
                    let source = self.resolve_src(&from.text);
                    let target = self.resolve_src(&to.text);
                    let mut reverse = false;
                    if self.avail() {
                        let next = self.tok(false)?;
                        if next.text.eq_ignore_ascii_case("reverse") {
                            reverse = true;
                        } else {
                            self.lex.unget(next);
                        }
                    }
                    self.referenced_files.push(target.clone());
                    parsed
                        .replacements
                        .push((source, Some((target, reverse)), from));
                }
                "bonetreecollapse" => {
                    parsed
                        .options
                        .bone_tree_collapse
                        .push(self.tok(false)?.text);
                }
                "replacebone" => {
                    let from = self.tok(false)?.text;
                    let to = self.tok(false)?.text;
                    parsed.options.replace_bone.push([from, to]);
                }
                "nofacial" => parsed.options.no_facial = true,
                "facial" => parsed.options.no_facial = false,
                "removemodel" => {
                    let from = self.tok(false)?;
                    parsed
                        .replacements
                        .push((self.resolve_src(&from.text), None, from));
                }
                "removemesh" => parsed.options.removed_materials.push(self.tok(false)?.text),
                "replacematerial" => {
                    let from = self.tok(false)?.text;
                    let to = self.tok(false)?.text;
                    parsed.options.material_replacements.push([from, to]);
                }
                "use_shadowlod_materials" if shadow => self.gflags |= 0x100,
                "reverse" => {
                    return Err(self.unsupported(&token, &format!("$lod {}", token.text)));
                }
                other => {
                    return Err(QcError::new(
                        token.file.clone(),
                        token.line,
                        format!("unknown $lod option {other:?}"),
                    ));
                }
            }
        }
        self.pending_lods.push(parsed);
        Ok(())
    }

    fn apply_lods(&mut self) {
        for parsed in std::mem::take(&mut self.pending_lods) {
            let mut matched = vec![false; parsed.replacements.len()];
            for bodypart in &mut self.desc.bodyparts {
                for model in &mut bodypart.models {
                    let mut level = parsed.options.clone();
                    let source = lod_source_key(&model.smd);
                    for (index, (from, to, _)) in parsed.replacements.iter().enumerate() {
                        if !model.smd.is_empty() && source == lod_source_key(from) {
                            matched[index] = true;
                            level.removed = to.is_none();
                            level.reverse = to.as_ref().is_some_and(|(_, reverse)| *reverse);
                            level.smd = to
                                .as_ref()
                                .filter(|(to, reverse)| *reverse || source != lod_source_key(to))
                                .map(|(to, _)| to.clone());
                        }
                    }
                    model.lods.push(level);
                }
            }
            for (matched, (source, _, token)) in matched.into_iter().zip(parsed.replacements) {
                if !matched {
                    self.errors.push(QcError::new(
                        token.file,
                        token.line,
                        format!("replacemodel source {source:?} does not match any bodypart model"),
                    ));
                }
            }
        }
    }

    /// `$jigglebone "<骨骼>" { is_flexible { ... } ... }`。
    ///
    /// # 只记录「按 token 顺序的写入日志」（`JiggleBone::writes`）
    ///
    /// 官方解析器**没有**三个独立子结构 —— 它只有一个扁平记录，每个键
    /// 按 QC 里出现的先后顺序直接写进去，**后写覆盖先写**。两个只有块顺序
    /// 不同的夹具结果不同（`jig47` → `-30°/40°`，`jig48` → `-10°/20°`），
    /// 所以这里**必须**保真 token 顺序，单位换算与钳位留到
    /// `compile::resolve_jiggle_bones` 里做一次。
    ///
    /// 未知键一律报错（官方是 `$jigglebone: invalid syntax '%s'` +
    /// `Aborted Processing`，**不是**静默忽略）。
    fn cmd_jigglebone(&mut self) -> Result<(), QcError> {
        let bone = self.tok(false)?.text;
        let mut jb = JiggleBone {
            bone,
            is_flexible: None,
            is_rigid: None,
            has_base_spring: None,
            writes: Vec::new(),
        };
        // 找 `{`。
        loop {
            let t = self.tok(true)?;
            if t.text == "{" {
                break;
            }
            if t.text == "}" {
                self.desc.jiggle_bones.push(jb);
                return Ok(());
            }
        }
        let mut depth = 1i32;
        // 当前所在的子块名（`None` = 顶层，官方在顶层没有任何键可写）。
        let mut block: Option<String> = None;
        while depth > 0 {
            let Some(t) = self.lex.next_token(true)? else {
                break;
            };
            if t.text == "{" {
                depth += 1;
                if depth != 2 {
                    // 块里的嵌套 `{`：官方块解析器的键表里没有 `{`，
                    // 连共享子解析器也不认 ⟹ `invalid syntax '{'` + abort。
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "$jigglebone %{bone}: a nested `{{` appears inside a block — the official code aborts (`$jigglebone: invalid syntax '{{'`)",
                            bone = format!("{:?}", jb.bone)
                        ),
                    ));
                }
                let Some(b) = block.as_deref() else {
                    // 顶层直接 `{`（`$jigglebone "x" { { ... } }`）。
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "$jigglebone %{bone}: a nested `{{` with no block name before it — the official code aborts (`$jigglebone: invalid syntax`)",
                            bone = format!("{:?}", jb.bone)
                        ),
                    ));
                };
                // 记一条「进入该块」的写入（它负责置 `flags`）；
                // 块名保留到 `}` 返回 depth 1 为止，供块内的键查表用。
                jb.writes.push(JiggleWrite {
                    key: b.to_string(),
                    values: Vec::new(),
                });
                continue;
            }
            if t.text == "}" {
                depth -= 1;
                if depth > 0 {
                    block = None;
                }
                continue;
            }
            let low = t.text.to_ascii_lowercase();
            if depth == 1 {
                // `depth == 1` 的 token 只能是**块名**（它由随后那个 `{`
                // 触发「进入该块」的写入）。顶层裸键官方一律 abort ——
                // 实测 `jig5`/`jig31`（`tip_mass 7` 写在所有块之外）都报
                // `$jigglebone: invalid syntax 'tip_mass'` 并中止。
                if matches!(low.as_str(), "is_flexible" | "is_rigid" | "has_base_spring") {
                    block = Some(low);
                    continue;
                }
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "$jigglebone %{bone}: the key %{key} is written outside any block — the official code aborts (`$jigglebone: invalid syntax '%{key2}'`)",
                        bone = format!("{:?}", jb.bone),
                        key = format!("{low:?}"),
                        key2 = low
                    ),
                ));
            }
            let Some(b) = block.as_deref() else {
                // 嵌套 `{` 之后又出现键（如 `is_flexible { { ... } }`）——
                // 官方此时也没有可用的键表。
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "$jigglebone %{bone}: the key %{key} appears in an unnamed nested block",
                        bone = format!("{:?}", jb.bone),
                        key = format!("{low:?}")
                    ),
                ));
            };
            // 键是否属于当前块 —— 这是官方「每个块只认自己那组键」的硬规则。
            if !jiggle_block_accepts(b, &low) {
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "$jigglebone %{bone}: the %{block} block does not accept the key %{key} — the official code aborts (`$jigglebone: invalid syntax '%{key2}'`)",
                        bone = format!("{:?}", jb.bone),
                        block = b,
                        key = format!("{low:?}"),
                        key2 = low
                    ),
                ));
            }
            // 按键的元数消费数值，原样记录（不转单位、不钳位）。
            let values = match low.as_str() {
                "allow_length_flex" => Vec::new(),
                "yaw_constraint" | "pitch_constraint" | "left_constraint" | "up_constraint"
                | "forward_constraint" => {
                    let a = self.f()?;
                    let b = self.f()?;
                    vec![a, b]
                }
                _ => vec![self.f()?],
            };
            jb.writes.push(JiggleWrite { key: low, values });
        }
        self.desc.jiggle_bones.push(jb);
        Ok(())
    }

    /// `$proceduralbones "<vrd>"` —— 读 `.vrd` 生成 quatinterp 骨骼。
    ///
    /// # ⚠️ VRD 文件不存在时官方**静默跳过**（oracle 实测）
    ///
    /// `vrj3.qc` 写 `$proceduralbones "ai1.txt"`，而 `ai1.txt` **不存在**。
    /// 官方 studiomdl：**退出码 0**、产物正常、stdout/stderr 里
    /// **没有任何**关于 `ai1` 的消息。
    ///
    /// 所以这里也按「文件不存在 ⟹ 无 quatinterp 骨骼」处理，
    /// 只打一行提示（官方连提示都没有，mdlc 多给一行更友好，
    /// 但不改变产物）。
    ///
    /// 文件**存在但解析失败**时仍然报错 —— 那说明是格式问题，
    /// 静默跳过会掩盖真错误。
    fn cmd_proceduralbones(&mut self) -> Result<(), QcError> {
        let t = self.tok(false)?;
        let vrd = self.resolve_src(&t.text);
        let path = self.qdir.join(&vrd);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 与官方一致：文件不在 ⟹ 没有程序化骨骼。
                log::info!(
                    "{}",
                    crate::tr_fmt!(
                        "Note: the VRD pointed to by $proceduralbones does not exist; skipped, matching the official behavior: %{path}",
                        path = path.display()
                    )
                );
                return Ok(());
            }
            Err(e) => {
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "cannot read the VRD %{path}: %{err}",
                        path = path.display(),
                        err = e
                    ),
                ));
            }
        };
        let bones = parse_vrd(&text).map_err(|m| QcError::new(t.file.clone(), t.line, m))?;
        self.desc.model.source_dependencies.push(path);
        self.desc.quat_interp_bones.extend(bones);
        Ok(())
    }

    /// `$jointsurfaceprop "<骨骼>" "<材质>"`。
    fn cmd_jointsurfaceprop(&mut self) -> Result<(), QcError> {
        let bone = self.tok(false)?.text;
        let prop = self.tok(false)?.text;
        self.joint_surface_props.push((bone, prop));
        Ok(())
    }

    /// `$weightlist "<名>" [<骨骼> <权重> ...] [{ ... }]`。
    ///
    /// # 官方语法（`Cmd_Weightlist` + `Option_Weightlist`，
    /// `studiomdl.cpp:3248-3353`）
    ///
    /// **两种写法等价**（`Option_Weightlist` 的循环把 `{`/`}` 当 depth 调整，
    /// 其它 token 一律当 `<骨骼> <权重>` 对）：
    ///
    /// ```text
    /// $weightlist FULLBODY $Bone$Pelvis 1          ← 单行、无块
    /// $weightlist INJUREDIDLENOISE {               ← 带块
    ///   $Bone$L_Thigh 0
    ///   $Bone$Pelvis .6
    /// }
    /// ```
    ///
    /// 另有一条 `posweight <v>` 后缀，作用于**上一条**骨骼条目
    /// （`i = pweightlist->numbones - 1`）。
    ///
    /// # 校验（官方会 `MdlError` 的两条）
    ///
    /// 1. `posweight` 出现在任何骨骼条目**之前** ⟹ 报错；
    /// 2. `weight == 0 && posweight > 0` ⟹ 报错
    ///    （`Non-zero Position weight with zero Rotation weight not allowed`）。
    fn cmd_weightlist(&mut self) -> Result<(), QcError> {
        let name_t = self.tok(false)?;
        let name = name_t.text.clone();
        // 表数上限：官方 L4D2 `MAXWEIGHTLISTS` = **128**，且**含隐式的表 0**
        // （`studiomdl.cpp:6886` `g_numweightlist = 1`），所以手写最多 127 张。
        // 实测边界：127 张 OK / 128 张 ERROR（`Too many weightlist commands (128)`）。
        //
        // ⚠️ episode1 头文件写的是 32，是**错的**。
        if self.desc.weight_lists.len() >= crate::model::MAX_WEIGHT_LISTS - 1 {
            return Err(QcError::new(
                name_t.file.clone(),
                name_t.line,
                crate::tr_fmt!(
                    "more weight lists than the official limit of %{max} (including the implicit default list, at most %{hand} may be written by hand)",
                    max = crate::model::MAX_WEIGHT_LISTS,
                    hand = crate::model::MAX_WEIGHT_LISTS - 1
                ),
            ));
        }
        // 查重（官方 `for (i = 1; i < g_numweightlist; i++)` —— 跳过表 0）。
        if self
            .desc
            .weight_lists
            .iter()
            .any(|w| w.name.eq_ignore_ascii_case(&name))
        {
            return Err(QcError::new(
                name_t.file.clone(),
                name_t.line,
                crate::tr_fmt!("duplicate $weightlist %{name}", name = format!("{name:?}")),
            ));
        }

        let bones = self.option_weightlist(&name)?;
        self.desc.weight_lists.push(WeightList { name, bones });
        Ok(())
    }

    fn option_weightlist(&mut self, name: &str) -> Result<Vec<WeightEntry>, QcError> {
        let mut bones: Vec<WeightEntry> = Vec::new();
        let mut depth = 0i32;
        loop {
            // 与官方一致：depth > 0 时跨行取 token；否则只在本行内取。
            let t = if depth > 0 {
                match self.lex.next_token(true)? {
                    Some(t) => t,
                    None => return Err(self.lex.error("unterminated weight list")),
                }
            } else {
                if !self.avail() {
                    break;
                }
                self.tok(false)?
            };
            if t.text == "{" {
                depth += 1;
            } else if t.text == "}" {
                depth -= 1;
                if depth <= 0 {
                    break;
                }
            } else if t.text.eq_ignore_ascii_case("posweight") {
                let v = self.f()?;
                let Some(last) = bones.last_mut() else {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "posweight appears before any bone entry (weightlist %{name})",
                            name = format!("{name:?}")
                        ),
                    ));
                };
                if last.weight == 0.0 && v > 0.0 {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "the bone %{bone} has a rotation weight of 0 but a position weight of %{v} — the official code does not allow this",
                            bone = format!("{:?}", last.bone),
                            v = v
                        ),
                    ));
                }
                last.pos_weight = Some(v);
            } else {
                let bone = t.text;
                let weight = self.f()?;
                // 官方 L4D2 的 `MAXWEIGHTSPERLIST` = **128**（实测，
                // 见 `crate::model::MAX_WEIGHT_ENTRIES` 的说明 ——
                // episode1 头文件写的 16 是**错的**），
                // 在**解析期**报 `Too many bones (128) in weightlist '%s'`
                // （`Option_Weightlist`，`studiomdl.cpp:3310-3313`）。
                if bones.len() >= crate::model::MAX_WEIGHT_ENTRIES {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        crate::tr_fmt!(
                            "the weight list %{name} has more entries than the official limit of %{max}",
                            name = format!("{name:?}"),
                            max = crate::model::MAX_WEIGHT_ENTRIES
                        ),
                    ));
                }
                bones.push(WeightEntry {
                    bone,
                    weight,
                    // 官方：`boneposweight[i] = boneweight[i]`。
                    pos_weight: None,
                });
            }
        }
        Ok(bones)
    }

    fn cmd_defaultweightlist(&mut self) -> Result<(), QcError> {
        self.desc.default_weight_list = self.option_weightlist("default")?;
        Ok(())
    }

    /// `$collisionmodel "<smd>" { ... }` / `$collisionjoints`。
    fn cmd_collisionmodel(&mut self, joints: bool) -> Result<(), QcError> {
        let t = self.tok(false)?;
        let smd = self.resolve_src(&t.text);
        self.desc.physics.smd = Some(smd.clone());
        if self.desc.model.first_source.is_none() {
            self.desc.model.first_source = Some(smd.clone());
        }
        self.referenced_files.push(smd);
        self.desc.physics.joints = joints;
        self.desc.physics.input_scale = Some(self.default_scale);
        if let Some(t) = self.lex.next_token(true)? {
            if t.text == "{" {
                let mut depth = 1;
                while depth > 0 {
                    let t = self.lex.next_token(true)?.ok_or_else(|| {
                        self.lex
                            .error("unterminated collision block (missing closing brace)")
                    })?;
                    if t.text == "{" {
                        depth += 1;
                        continue;
                    }
                    if t.text == "}" {
                        depth -= 1;
                        continue;
                    }
                    self.physics_subkey(&t)?;
                }
            } else {
                self.lex.unget(t);
            }
        }
        Ok(())
    }

    /// `$collisionmodel` / `$collisionjoints` 块内的一个子键。
    ///
    /// ⚠️ **块内子键带 `$` 前缀**（与顶层命令同形）——
    /// `ParseCollisionCommands`（`collisionmodel.cpp:1767`）里
    /// 逐条 `stricmp(command, "$mass")` 等，命令名是**原样**的 token。
    /// 所以这里比较时要把 `$` 去掉，但**不能**假设它不存在。
    fn physics_subkey(&mut self, t: &Token) -> Result<(), QcError> {
        let low = t.text.to_ascii_lowercase();
        // 剥掉 `$`（若在），再比较。
        let key = low.strip_prefix('$').unwrap_or(&low);
        match key {
            "mass" => {
                self.desc.physics.mass = Some(self.f()?);
                self.desc.physics.auto_mass = false;
            }
            "concave" => self.desc.physics.concave = true,
            "damping" => self.desc.physics.damping = Some(self.f()?),
            "rotdamping" => self.desc.physics.rot_damping = Some(self.f()?),
            "inertia" => self.desc.physics.inertia = Some(self.f()?),
            "drag" => self.desc.physics.drag = Some(self.f()?),
            "rootbone" => self.desc.physics.root_bone = Some(self.tok(false)?.text),
            "noselfcollisions" => self.desc.physics.no_self_collisions = true,
            "automass" => {
                self.desc.physics.auto_mass = true;
                self.desc.physics.mass = None;
            }
            "masscenter" => self.desc.physics.mass_center = Some(self.v3()?),
            "jointmerge" => self.cmd_collision_pair(true)?,
            "jointcollide" => self.cmd_collision_pair(false)?,
            "jointconstrain" => self.cmd_jointconstrain()?,
            "jointskip" => {
                let bone = self.tok(false)?.text;
                self.desc.physics.skip.push(bone);
            }
            "jointsurfaceprop" => self.cmd_jointsurfaceprop()?,
            "jointcontents" => self.cmd_jointcontents()?,
            "contents" => self.cmd_contents()?,
            "animatedfriction" => self.cmd_animatedfriction()?,
            "phyname" => self.desc.physics.name = Some(self.tok(false)?.text),
            "weldposition" => self.desc.physics.weld_position = Some(self.f()?),
            "weldnormal" => self.desc.physics.weld_normal = Some(self.f()?),
            "remove2d" => self.desc.physics.remove_2d = true,
            "assumeworldspace" => self.desc.physics.assume_world_space = true,
            "concaveperjoint" => self.desc.physics.concave_per_joint = true,
            "maxconvexpieces" => {
                let value = self.i()?;
                if !(1..=1024).contains(&value) {
                    return Err(QcError::new(
                        t.file.clone(),
                        t.line,
                        "Convex piece limit must be between 1 and 1024",
                    ));
                }
                self.desc.physics.max_convex_pieces = Some(value as usize);
            }
            // `$rollingDrag` 官方读了参数但**什么也不做**
            // （`collisionmodel.cpp:1756`：调用被注释掉了）。
            "rollingdrag" => {
                let _ = self.f()?;
            }
            "polysoup" | "snapcollisionjoints" | "preservetriangleorder" => {
                return Err(self.unsupported(t, &t.text));
            }
            "jointmassbias" | "jointdamping" | "jointrotdamping" | "jointinertia" => {
                self.joint_override_key(key)?
            }
            other => {
                return Err(QcError::new(
                    t.file.clone(),
                    t.line,
                    crate::tr_fmt!(
                        "unknown collision block subkey %{key}",
                        key = format!("{other:?}")
                    ),
                ));
            }
        }
        Ok(())
    }

    /// 逐 joint 覆盖（`$jointdamping` 等）。
    fn joint_override_key(&mut self, key: &str) -> Result<(), QcError> {
        let bone = self.tok(false)?.text;
        let v = self.f()?;
        let entry = match self
            .desc
            .physics
            .joint_overrides
            .iter_mut()
            .find(|j| j.bone.eq_ignore_ascii_case(&bone))
        {
            Some(e) => e,
            None => {
                self.desc.physics.joint_overrides.push(JointOverride {
                    bone,
                    damping: None,
                    rot_damping: None,
                    inertia: None,
                    mass_bias: None,
                });
                self.desc.physics.joint_overrides.last_mut().unwrap()
            }
        };
        match key {
            "jointdamping" => entry.damping = Some(v),
            "jointrotdamping" => entry.rot_damping = Some(v),
            "jointinertia" => entry.inertia = Some(v),
            "jointmassbias" => entry.mass_bias = Some(v),
            _ => {}
        }
        Ok(())
    }

    /// `$jointconstrain "<骨骼>" <轴> <类型> <min> <max> [<摩擦>]`。
    ///
    /// # 摩擦系数是**可选**的
    ///
    /// 实测 `jc_nofric.qc` 写的是 5 个参数
    /// （`$jointconstrain "bone_mid" "x" "limit" -10 10`），
    /// 而 `phy-jointconstrain*.toml` 的官方对照用例写 6 个。
    ///
    /// 官方 `CCmd_JointConstrain`（`collisionmodel.cpp:1900` 附近）
    /// 用 `ReadArgs(args, 6)` —— 它**读满 6 个**，读不到就用空串，
    /// 而 `Safe_atof("")` = 0。所以缺省摩擦 = **0**。
    fn cmd_jointconstrain(&mut self) -> Result<(), QcError> {
        let bone = self.tok(false)?.text;
        let axis = self.tok(false)?.text;
        let kind = self.tok(false)?.text;
        let min = self.f()?;
        let max = self.f()?;
        let friction = if self.avail() { self.f()? } else { 0.0 };
        self.desc.physics.constraints.push(JointConstraintSpec {
            bone,
            axis,
            kind,
            min,
            max,
            friction: Some(friction),
        });
        Ok(())
    }

    /// `$animatedfriction <min> <max> <timehold> <timeout> <timein>`。
    fn cmd_animatedfriction(&mut self) -> Result<(), QcError> {
        let min = self.i()?;
        let max = self.i()?;
        let time_hold = self.f()?;
        let time_out = self.f()?;
        let time_in = self.f()?;
        self.desc.physics.animated_friction = Some(AnimatedFrictionSpec {
            min,
            max,
            time_in,
            time_out,
            time_hold,
        });
        Ok(())
    }

    /// `$jointcollide "<a>" "<b>"` / `$jointmerge "<a>" "<b>"`。
    fn cmd_collision_pair(&mut self, merge: bool) -> Result<(), QcError> {
        let a = self.tok(false)?.text;
        let b = self.tok(false)?.text;
        let spec = CollisionPairSpec { a, b };
        if merge {
            self.desc.physics.merge.push(spec);
        } else {
            self.desc.physics.collision_pairs.push(spec);
        }
        Ok(())
    }

    /// `$mass` / `$damping` 等**顶层**物理键。
    fn cmd_physics_kv(&mut self, name: &str) -> Result<(), QcError> {
        match name {
            "$mass" => {
                self.desc.physics.mass = Some(self.f()?);
                self.desc.physics.auto_mass = false;
            }
            "$damping" => self.desc.physics.damping = Some(self.f()?),
            "$rotdamping" => self.desc.physics.rot_damping = Some(self.f()?),
            "$inertia" => self.desc.physics.inertia = Some(self.f()?),
            "$drag" => self.desc.physics.drag = Some(self.f()?),
            "$rootbone" => self.desc.physics.root_bone = Some(self.tok(false)?.text),
            "$concave" => self.desc.physics.concave = true,
            "$automass" => {
                self.desc.physics.auto_mass = true;
                self.desc.physics.mass = None;
            }
            "$masscenter" => self.desc.physics.mass_center = Some(self.v3()?),
            "$maxconvexpieces" => {
                let value = self.i()?;
                if !(1..=1024).contains(&value) {
                    return Err(self
                        .lex
                        .error("Convex piece limit must be between 1 and 1024"));
                }
                self.desc.physics.max_convex_pieces = Some(value as usize);
            }
            "$phyname" => self.desc.physics.name = Some(self.tok(false)?.text),
            _ => {}
        }
        Ok(())
    }

    /// 收尾：读 SMD 补全骨骼表 / 材质表 / 事件 cycle，并回填跨阶段引用。
    ///
    /// # 为什么必须读 SMD
    ///
    /// 官方在 `SimplifyModel()` 里做、而 QC 文本里**看不到**的三件事：
    ///
    /// 1. **骨骼表**（`BuildGlobalBonetable`，`simplify.cpp:3616`）：
    ///    `$definebone` 的骨骼**排在前面**（按书写序），然后才是各 SMD
    ///    里「被顶点引用」的骨骼（按 source 序 × SMD 内下标序）。
    ///    mdlc 的 `[[bones]]` 必须完整列出 —— `compile.rs` 对未声明的
    ///    骨骼**直接报错**（这是故意的，见 `PROGRESS.md` §8.4）。
    /// 2. **材质表**（`SetSkinValues` / `lookup_texture`）：
    ///    `$texturegroup` 的材质**先注册**（按组序），其余按 SMD 首现序。
    /// 3. **事件 cycle**（`write.cpp:494`）：
    ///    `cycle = frame / (numframes - 1)` —— 需要该序列 SMD 的帧数。
    fn finish(&mut self) -> Result<ModelDesc, Vec<QcError>> {
        self.apply_lods();
        if let Err(error) = crate::bone_rename::description(&mut self.desc) {
            return Err(vec![QcError::new(
                self.qdir.display().to_string(),
                0,
                error,
            )]);
        }
        for bone in &mut self.import_bones {
            crate::bone_rename::name(&mut bone.name, &self.desc.model.bone_renames);
            if let Some(parent) = &mut bone.parent {
                crate::bone_rename::name(parent, &self.desc.model.bone_renames);
            }
        }
        for name in &mut self.bonemerge_names {
            crate::bone_rename::name(name, &self.desc.model.bone_renames);
        }
        for (name, _) in &mut self.joint_surface_props {
            crate::bone_rename::name(name, &self.desc.model.bone_renames);
        }

        // ---- 0. 网格源的 `src*` 选项 ----
        //
        // ⚠️ 这一趟读源**必须**用上 `srcmaterial`：它替换的是「源里没有材质
        // 时合成的那个名字」（官方 `debug/debugempty`），而材质表是在这里
        // 按「源里出现的材质名」登记的。用默认选项读，登记的就是
        // `debug/debugempty`，而 `compile()` 那边按 `srcmaterial` 取到的是
        // 用户写的名字 ⟹ 报「SMD 里的材质名 … 在 [materials].textures 里
        // 找不到」（实测 S7 夹具）。所以两侧必须用**同一套**选项。
        //
        // `srcpart` / `srcscale` / `srcaxis` 不影响材质名，但一并带上没有坏处
        // —— 少一处「哪些选项在这一趟生效」的心智负担。
        let mut src_opts_of: HashMap<String, Vec<crate::compile::SrcOpts>> = HashMap::new();
        for body in &self.desc.bodyparts {
            for model in &body.models {
                if model.smd.is_empty() {
                    continue;
                }
                if let Ok(options) = crate::compile::SrcOpts::of_model(model) {
                    merge_source_options(
                        src_opts_of.entry(model.smd.clone()).or_default(),
                        options,
                    );
                }
            }
        }

        // ---- 1. 读所有被引用的 SMD（缓存，避免重复读）----
        let mut smd_cache: HashMap<String, Option<SmdInfo>> = HashMap::new();
        let files: Vec<String> = self.referenced_files.clone();
        for f in &files {
            if smd_cache.contains_key(f) {
                continue;
            }
            let p = match crate::compile::resolve_smd_path(&self.qdir, f) {
                Ok(p) => p,
                Err(msg) => {
                    self.errors
                        .push(QcError::new(f.clone(), 0, msg.to_string()));
                    continue;
                }
            };
            // ⚠️ **不要静默吞掉读失败** —— 那正是本项目反复踩到的
            // 「静默吃数据」：源读不到时骨骼表会变空，
            // 最后只报一句「至少需要一根骨骼」，指向不了真因。
            //
            // ⚠️⚠️ 必须走 `compile::read_source`，**不能**自己
            // `read_to_string` + `parse_smd`：QC 里的资产引用从 R35 起可以
            // 是 `.fbx`（`docs/fbx-support.md`），而 FBX 是**二进制** ——
            // 走文本读取会直接报 `stream did not contain valid UTF-8`，
            // 与「文件坏了」毫无关系。格式分派只允许存在于 `read_source`
            // 一处，否则加 glTF 时要记得改这里（第 N 个调用点）。
            //
            // ⚠️ `CompileError` 没有行号字段，`.smd` 的解析行号在
            // `SmdError` 的 `Display` 里（`第 N 行：…`），已包含在 message
            // 中；`.fbx` 本来就没有行号概念。所以这里统一填 0。
            // ⚠️ 这里用**默认** [`crate::compile::SrcOpts`]，不读 `src*` 选项：
            // 这一趟只做「文件读不读得到 + 节点名/材质名是什么」的存在性检查，
            // 而 `src*` 改变的是**取哪些网格、怎么变换**，不改变「这个文件
            // 是不是合法的源」。真正的几何读取在 `compile()` 里，
            // 那边会用上 QC 填好的选项。
            // ⚠️ 这里用该源的 `src*` 选项（见上面第 0 步的注释）——
            // `srcmaterial` 决定的材质名必须与 `compile()` 那边一致。
            // 拿不到选项（理论上不会，`referenced_files` 里的网格源都来自
            // `option_studio`）就退回默认值。
            let fallback = [crate::compile::SrcOpts::default()];
            let options = src_opts_of
                .get(f)
                .map_or(fallback.as_slice(), Vec::as_slice);
            let at = crate::tr("referenced by the QC");
            let mut info: Option<SmdInfo> = None;
            for options in options {
                let source =
                    crate::compile::read_source(&p, &at, options).and_then(|mut source| {
                        crate::bone_rename::source(&mut source, &self.desc.model.bone_renames)
                            .map_err(|message| crate::compile::CompileError::Message {
                                at: at.clone(),
                                message,
                            })?;
                        Ok(source)
                    });
                match source {
                    Err(error) => self.errors.push(QcError::new(
                        p.display().to_string(),
                        0,
                        error.to_string(),
                    )),
                    Ok(source) => {
                        let incoming = SmdInfo::from_source(&source);
                        if let Some(info) = &mut info {
                            info.merge(incoming);
                        } else {
                            info = Some(incoming);
                        }
                    }
                }
            }
            smd_cache.insert(f.clone(), info);
        }

        // ---- 2. 骨骼表 ----
        //
        // 复刻官方 `TagUsedBones`（`simplify.cpp:3424-3547`）+
        // `BuildGlobalBonetable`（`simplify.cpp:3616-3695`）的**收骨判据**：
        //
        //   入表 = `$definebone`（无条件）∪ `psource->boneref[j] != 0`
        //
        // 而 `boneref` 只有六个来源：① **网格源**的顶点权重（`isActiveModel`
        // 把关，`simplify.cpp:3441-3442`）、② `$attachment`、③ `$ikchain`、
        // ④ `$mouth`、⑤ `$bonemerge`、⑥ 眼球骨骼；最后沿**父链**上传
        // （`UpdateBonerefRecursive`，`simplify.cpp:3404-3418`）。
        //
        // # 为什么不能「每个 SMD 的每个 node 都收」
        //
        // 那会把**只在动画源里挂着、没有任何顶点引用**的骨骼也收进来。官方
        // 不收，于是 mdlc 的骨骼总数会比官方多 —— 一旦顶过引擎的
        // `MAXSTUDIOBONES`（128，`hl2sdk-l4d2/public/studio.h:83`），
        // `CBoneCache::CreateResource` 的 `short studioToCachedIndex[128]`
        // （`hl2sdk-l4d2/public/bone_setup.cpp:62-89`）就会被逐骨骼无条件写入
        // 而**越界写栈** ⟹ 游戏加载时无响应 / 闪退。
        //
        // ⭐ 实测裁决（`docs/_probe/official_user_qc3.js`）：用户工程
        // `linnea_replaces_zoey` 的 QC 用官方 studiomdl 编出 **122 根**，
        // mdlc 旧实现出 **134 根**；且官方 122 与 `definebones.qci` 里生效的
        // 122 条 `$definebone` **零差异**（`check_db_vs_official.js`），
        // 多出的 12 根全是「只在动画 SMD 的 nodes 里、零顶点引用」的骨骼。
        //
        // ⚠️ 顺序不能动：官方先插 `$definebone`，再按 **source 序 × SMD 内
        // 下标序** 追加 —— 这个顺序决定了骨骼下标，进而决定 `parent` 的编码。
        let mut bone_refs: HashMap<String, HashSet<String>> = HashMap::new();
        {
            // 眼球骨骼按**模型**归属到它自己的 SMD（`simplify.cpp:3539-3546`）。
            let mut eyeball_bones: HashMap<&str, HashSet<String>> = HashMap::new();
            for bp in &self.desc.bodyparts {
                for m in &bp.models {
                    for eb in &m.eyeballs {
                        eyeball_bones
                            .entry(m.smd.as_str())
                            .or_default()
                            .insert(eb.bone.to_ascii_lowercase());
                    }
                }
            }
            for f in &files {
                let Some(info) = smd_cache.get(f).and_then(|x| x.as_ref()) else {
                    continue;
                };
                let n = info.nodes.len();
                let lower: Vec<String> =
                    info.nodes.iter().map(|s| s.to_ascii_lowercase()).collect();
                // 本 source 的 `boneflags[]`（官方用 source 内下标，这里用同名布尔）。
                let mut flagged: Vec<bool> = vec![false; n];

                // ① 顶点权重 —— **只有网格源**算。动画源 / `$lod` / `$collisionmodel`
                // 的 `isActiveModel` 都是 `false`，它们的顶点权重一律不生效。
                if self.mesh_sources.contains(f) {
                    for &i in &info.vert_refs {
                        if i < n {
                            flagged[i] = true;
                        }
                    }
                }

                // ② `$attachment`（`simplify.cpp:3464-3489`）。
                for at in &self.desc.attachments {
                    let Some(j) = info
                        .nodes
                        .iter()
                        .position(|x| x.eq_ignore_ascii_case(&at.bone))
                    else {
                        continue;
                    };
                    // `rigid` ⟹ `IS_RIGID`（`0x0002`，`studiomdl.h:278`）。
                    // ⚠️ 该位在 `g_attachment[].type` 里、**不落盘**，所以判据
                    // 用 IR 的 [`crate::model::Attachment::rigid`]，
                    // **不能**去读 `flags`（那里只会有 `0x10000`）。
                    if at.rigid {
                        // 刚性附着点：沿父链上溯到第一根**有顶点权重**的骨骼，
                        // 标它而不是标自己（`simplify.cpp:3478-3484`）。
                        let mut k = j as i32;
                        while k >= 0 {
                            let ki = k as usize;
                            if ki >= n {
                                break;
                            }
                            if info.vert_refs.contains(&ki) {
                                flagged[ki] = true;
                                break;
                            }
                            k = info.parents.get(ki).copied().unwrap_or(-1);
                        }
                    } else {
                        flagged[j] = true;
                    }
                }
                // ③ `$ikchain`（`simplify.cpp:3491-3502`）。
                for ik in &self.desc.ikchains {
                    if let Some(j) = info
                        .nodes
                        .iter()
                        .position(|x| x.eq_ignore_ascii_case(&ik.bone))
                    {
                        flagged[j] = true;
                    }
                }
                // ④ `$mouth`（`simplify.cpp:3504-3515`）。
                for mo in &self.desc.mouths {
                    if let Some(j) = info
                        .nodes
                        .iter()
                        .position(|x| x.eq_ignore_ascii_case(&mo.bone))
                    {
                        flagged[j] = true;
                    }
                }
                // ⑤ `$bonemerge`（`simplify.cpp:3517-3528`）。
                // ⚠️ 官方是「给**已存在**的骨骼打 `BONE_USED_BY_BONE_MERGE`」，
                // **不能创造骨骼** —— 这里同样只在 `nodes` 里找。
                for bm in &self.bonemerge_names {
                    if let Some(j) = info.nodes.iter().position(|x| x.eq_ignore_ascii_case(bm)) {
                        flagged[j] = true;
                    }
                }
                // ⑥ 眼球（`simplify.cpp:3544`）。
                if let Some(eb) = eyeball_bones.get(f.as_str()) {
                    for (j, l) in lower.iter().enumerate() {
                        if eb.contains(l) {
                            flagged[j] = true;
                        }
                    }
                }

                // 沿父链上传（`UpdateBonerefRecursive`）。官方注释强调
                // 「This must come last; after all flags have been set!」
                for j in 0..n {
                    if !flagged[j] {
                        continue;
                    }
                    let mut k = info.parents.get(j).copied().unwrap_or(-1);
                    while k >= 0 {
                        let ki = k as usize;
                        if ki >= n {
                            break;
                        }
                        flagged[ki] = true;
                        k = info.parents.get(ki).copied().unwrap_or(-1);
                    }
                }

                bone_refs.insert(
                    f.clone(),
                    (0..n)
                        .filter(|&j| flagged[j])
                        .map(|j| lower[j].clone())
                        .collect(),
                );
            }
        }

        let mut bones: Vec<Bone> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // 记住哪些名字来自 `$definebone`（`g_bonetable[].bPreDefined`），
        // 供下面的 `$unlockdefinebones` 覆盖使用。
        let mut predefined: HashSet<String> = HashSet::new();
        for b in std::mem::take(&mut self.import_bones) {
            let key = b.name.to_ascii_lowercase();
            if seen.insert(key.clone()) {
                predefined.insert(key);
                bones.push(b);
            }
        }
        for f in &files {
            let Some(info) = smd_cache.get(f).and_then(|x| x.as_ref()) else {
                continue;
            };
            let Some(refs) = bone_refs.get(f) else {
                continue;
            };
            for name in &info.nodes {
                let key = name.to_ascii_lowercase();
                if refs.contains(&key) && seen.insert(key) {
                    bones.push(Bone {
                        name: name.clone(),
                        parent: None,
                        position: None,
                        rotation: None,
                        flags: None,
                        surface_prop: None,
                        bonemerge: false,
                        pre_aligned: None,
                        realign_position: None,
                        realign_rotation: None,
                    });
                }
            }
        }

        // ---- `$unlockdefinebones`：SMD 骨架**覆盖** `$definebone` ----
        //
        // 官方 `BuildGlobalBonetable`（`simplify.cpp:3688-3706`）：
        //
        // ```c
        // else if (g_bOverridePreDefinedBones && g_bonetable[k].bPreDefined)
        // {
        //     g_bonetable[k].bPreDefined = false;      // ← 不再是 pre-aligned
        //     MatrixCopy( srcBoneToWorld[j], g_bonetable[k].boneToPose );
        //     // rawLocal = parent.boneToPose⁻¹ ∘ srcBoneToWorld[j]  ← 即 SMD 的局部姿态
        // }
        // ```
        //
        // 对「`$definebone` 声明的骨骼同时也在 SMD 里」这一常见情形，
        // 结果就是**取 SMD 的局部姿态**。mdlc 的 IR 里「`position`/`rotation`
        // 为 `None`」正是这个语义（`resolve_bone_pose` 会回退到 SMD 第 0 帧），
        // 所以这里把它们清空，并把 `pre_aligned` 显式置 false
        // （否则 `is_pre_aligned()` 会按「有显式姿态」推断出 true）。
        //
        // ⚠️ **只对「也在 SMD nodes 里」的骨骼生效** —— 官方那条分支由
        // `psource->boneref[j]` 把关，SMD 里没有的骨骼根本不会走到。
        if self.unlock_define_bones {
            let smd_nodes: HashSet<String> = files
                .iter()
                .filter_map(|f| smd_cache.get(f).and_then(|x| x.as_ref()))
                .flat_map(|info| info.nodes.iter())
                .map(|n| n.to_ascii_lowercase())
                .collect();
            for b in &mut bones {
                let key = b.name.to_ascii_lowercase();
                if predefined.contains(&key) && smd_nodes.contains(&key) {
                    b.position = None;
                    b.rotation = None;
                    b.pre_aligned = Some(false);
                }
            }
        }
        // 父链：`$definebone` 显式给了就用；否则从**网格 SMD** 的 nodes 段推。
        //
        // 官方 `BuildGlobalBonetable` 用 `psource->localBone[j].parent`
        // （即 SMD nodes 的父下标），所以这里要找到「第一个含该骨骼的
        // SMD」并取其父名。
        let mut parent_of: HashMap<String, String> = HashMap::new();
        for b in &bones {
            if let Some(p) = &b.parent {
                parent_of.insert(b.name.to_ascii_lowercase(), p.clone());
            }
        }
        //
        // ⚠️⚠️ **必须走 `smd_cache`，不能自己 `read_to_string` + `parse_smd`**。
        // 这里曾经是第二个「硬编码的 SMD 文本读取点」：`.fbx` 是二进制，
        // `read_to_string` 直接失败 ⟹ `continue` ⟹ **父链整段丢失**，
        // 全部骨骼都写成了根骨骼（`parent = -1`）。产物字节数只差几十字节，
        // 肉眼看不出，但骨骼树整个塌成平的 —— 引擎里就是一堆互不相干的
        // 独立骨骼。同一个教训在 `finish()` 的资产读取处也记过一次
        // （见上面 `read_source` 那段注释）：**格式分派只允许存在于
        // `read_source` 一处**。
        //
        // `smd_cache` 里的 `SmdInfo` 已经带了 `nodes` 与 `parents`
        // （逐字对应 `psource->localBone[j].parent`），顺序也与 `files`
        // 一致，所以直接查表即可，顺带省掉一次重复解析。
        for info in files
            .iter()
            .filter_map(|f| smd_cache.get(f).and_then(|x| x.as_ref()))
        {
            for (i, name) in info.nodes.iter().enumerate() {
                let key = name.to_ascii_lowercase();
                if parent_of.contains_key(&key) {
                    continue;
                }
                if let Some(&par) = info.parents.get(i)
                    && par >= 0
                    && let Some(pname) = info.nodes.get(par as usize)
                {
                    parent_of.insert(key, pname.clone());
                }
            }
        }
        // ⚠️⚠️ `$definebone` 声明过的骨骼**绝不回填父链**。
        //
        // 官方 `BuildGlobalBonetable`（`simplify.cpp:3616`）分两段：
        //
        //   1. `:3624-3654` 先插 `$definebone`（`bPreDefined = true`），
        //      父由 `$definebone` 给 —— **空串显式写 `-1`**（`:3631-3634`），
        //      即「这根本来就是根」。
        //   2. `:3666-3712` 并入各源用到的骨骼：只有 `k == -1`
        //      （**表里还没有**）时才从源取父（`:3671-3679`）；
        //      已存在的走 `:3707-3711`，**只做 `flags |=`，从不碰 `parent`**
        //      （`$unlockdefinebones` 那条 `:3688` 分支同样不碰父）。
        //
        // 所以「`$definebone` 声明的根骨骼」在官方产物里是**独立的根**，
        // 不会被源树的父链收编。
        //
        // 这条曾经是错的：`cmd_definebone` 把空父存成 `None`（`:3302`），
        // 与「根本没写 `$definebone`」不可区分，于是这里无条件回填，
        // 把**显式声明的根**塞进了源树的父节点下。
        //
        // 实测（真工程 `v_snip_awp_processed.qc`，2026-10）：
        // `$definebone "ValveBiped.ValveBiped" "" …` 被回填成
        // `parent = "awp_model_Inverted_edit_skeleton"` —— 那是 FBX 的骨架
        // 根节点，自身带 **−90° 绕 X** 与 ×100 缩放。整棵骨骼树因此被带着
        // 转了 90°：70 根骨骼的 `poseToBone` **每一行都不同**（`[0]` 从
        // 单位阵变成 `1,0,0,0|0,0,-1,0|0,1,0,0`），VVD 的包围盒 Y/Z 两轴
        // 互换。官方产物里 `ValveBiped.ValveBiped` 是 `p = -1` 的独立根，
        // 骨架根反而是**另一个**根且排在最后。
        //
        // 把回填去掉后（手工改 TOML 复现），骨骼表与 SMD 路径的差异
        // **从 70 行降到 8 行**。
        for b in &mut bones {
            if b.parent.is_none()
                && !predefined.contains(&b.name.to_ascii_lowercase())
                && let Some(p) = parent_of.get(&b.name.to_ascii_lowercase())
            {
                b.parent = Some(p.clone());
            }
        }
        if let Err(error) = crate::bone_hierarchy::description(
            &mut bones,
            &self.desc.model.bone_hierarchy,
            |name| {
                files
                    .iter()
                    .filter_map(|file| smd_cache.get(file).and_then(Option::as_ref))
                    .find_map(|source| {
                        source
                            .nodes
                            .iter()
                            .position(|node| node.eq_ignore_ascii_case(name))
                            .map(|index| {
                                (source.rest_positions[index], source.rest_rotations[index])
                            })
                    })
            },
        ) {
            self.errors
                .push(QcError::new(self.qdir.display().to_string(), 0, error));
        }
        // 注：拓扑排序在字段回填之后做（见 `sort_parents_first`）。
        // `$bonemerge` 回填。
        let bm: HashSet<String> = self
            .bonemerge_names
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        for b in &mut bones {
            if bm.contains(&b.name.to_ascii_lowercase()) {
                b.bonemerge = true;
            }
        }
        // `$jointsurfaceprop` 回填。
        for (bone, prop) in std::mem::take(&mut self.joint_surface_props) {
            if let Some(b) = bones
                .iter_mut()
                .find(|b| b.name.eq_ignore_ascii_case(&bone))
            {
                b.surface_prop = Some(prop);
            }
        }
        // 拓扑排序放在**所有字段回填之后** —— 否则 `$bonemerge` /
        // `$jointsurfaceprop` 的回填会作用在排序前的下标上（虽然它们是
        // 按名字找的，但排序会换掉 Vec 的顺序，容易读错）。
        self.desc.bones = sort_parents_first(bones);

        // ---- 2b. 合成附着点的骨骼解析（`LinkAttachments()` 的阶段 1 / 2） ----
        //
        // `$illumposition x y z <骨骼>` 会合成一个绑该骨骼的附着点（见
        // `dispatch` 里那条分支）。官方 `LinkAttachments()`
        // （`simplify.cpp:5313-5401`）分两阶段定它的骨骼与 `local`，
        // 全部由真 exe 钉死（`docs/_probe/oracle_illumposition3..10.js`
        // + `docs/_probe/diff_illumposition.js`）：
        //
        //   - 骨骼名**从未**在任何 SMD 的 `nodes` 段里出现 ⟹ 阶段 2 的
        //     `if (!found)` 报 `MdlError( "unknown attachment link '%s'\n" )`
        //     （`:5375`）⟹ **硬报错、中止**
        //     （`oracle_illumposition7.js` 的 `illum4 bad bone`）。
        //   - 骨骼**直命中全局骨骼表**（阶段 1，`:5324-5339`）⟹ `local` 原样落盘
        //     —— `:5385` 左乘的 `boneToPose` 与 `:5388` 左乘的 `poseToBone`
        //     互为逆（`:5334-5335` 就是同一个矩阵求逆）。
        //   - 骨骼**被收骨判据丢掉**（零顶点引用且无 `$definebone`）⟹ 阶段 2
        //     （`:5341-5376`）沿父链上溯到第一根直命中的祖先，`bone` 取**祖先**
        //     的全局下标，`local` 被重算成
        //     `inverse(source 第 0 帧 world[祖先]) ∘ source 第 0 帧 world[原始] ∘ local`。
        //     整条链都没直命中 ⟹ `MdlError( "unable to find valid bone for attachment %s:%s\n" )`
        //     （`:5360-5362`）。
        //
        // ⚠️ 判据是「名字是否出现在某个 SMD 的 nodes 里」，**不是**
        // 「是否在 `desc.bones` 里」—— 被丢掉的骨骼同样不在 `desc.bones` 里，
        // 用后者会把官方**静默**的那一类误报成错误（那正是 `illum b2`）。
        //
        // ⚠️ 阶段 2 用的 `boneToPose` 是 `g_source[j]->boneToPose[k]`
        // （**该 source** 第 0 帧沿父链累积的姿态，`Build_Reference()`
        // `studiomdl.cpp:728-762`），**不是**全局骨骼表的
        // `g_bonetable[k].boneToPose` —— 所以修正矩阵只能在解析期算。
        {
            // 全局骨骼表按名字查下标 —— 官方 `findGlobalBone` 是 `stricmp`，
            // 这里统一小写。
            let global_index: HashMap<String, usize> = self
                .desc
                .bones
                .iter()
                .enumerate()
                .map(|(i, b)| (b.name.to_ascii_lowercase(), i))
                .collect();
            let all_nodes: HashSet<String> = files
                .iter()
                .filter_map(|f| smd_cache.get(f).and_then(|x| x.as_ref()))
                .flat_map(|info| info.nodes.iter())
                .map(|n| n.to_ascii_lowercase())
                .collect();
            // 阶段 2 按 `g_source[]` 的顺序扫（官方 `for (j = 0; ...)`）。
            let sources: Vec<&SmdInfo> = files
                .iter()
                .filter_map(|f| smd_cache.get(f).and_then(|x| x.as_ref()))
                .collect();
            // `synthetic_att_locs` 按 push 顺序记录，与 `desc.attachments`
            // 里的 `synthetic` 项一一对应（普通附着点不占位）。
            let mut loc = self.synthetic_att_locs.iter();
            let mut out: Vec<Option<(usize, [f32; 12])>> = Vec::new();
            let mut errs: Vec<QcError> = Vec::new();
            for at in &self.desc.attachments {
                let (file, line) = if at.synthetic {
                    loc.next()
                        .cloned()
                        .unwrap_or_else(|| (self.main_path.display().to_string(), 0))
                } else {
                    (self.main_path.display().to_string(), 0)
                };
                let want = at.bone.to_ascii_lowercase();
                if at.synthetic && !all_nodes.contains(&want) {
                    errs.push(QcError::new(
                        file,
                        line,
                        crate::tr_fmt!(
                            "unknown attachment link %{bone} — the 4th argument of `$illumposition` must be one of the SMD bones (the official `simplify.cpp:5375`)",
                            bone = format!("{:?}", at.bone)
                        ),
                    ));
                    out.push(None);
                    continue;
                }
                // 阶段 1：直命中 ⟹ `local` 原样落盘（修正矩阵 = 单位阵）。
                //
                // ⚠️ 这里**必须**由本函数给出下标，不能让写出器自己查
                // `bone_index` —— 官方 `findGlobalBone` 用的是 `stricmp`
                // （大小写不敏感），而 `ModelDesc::bone_index` 是精确匹配。
                if let Some(&gi) = global_index.get(&want) {
                    out.push(if at.synthetic || at.bone != self.desc.bones[gi].name {
                        Some((gi, crate::bone_math::identity()))
                    } else {
                        None
                    });
                    continue;
                }
                // 阶段 2：找第一个含该骨骼的 source。
                let Some((info, k0)) = sources.iter().find_map(|info| {
                    let k0 = info
                        .nodes
                        .iter()
                        .position(|n| n.eq_ignore_ascii_case(&at.bone))?;
                    Some((*info, k0))
                }) else {
                    errs.push(QcError::new(
                        file,
                        line,
                        crate::tr_fmt!(
                            "unknown attachment link %{bone}",
                            bone = format!("{:?}", at.bone)
                        ),
                    ));
                    out.push(None);
                    continue;
                };
                // 沿父链上溯到第一根直命中全局骨骼表的祖先。
                let mut k = k0 as i32;
                while k != -1 {
                    let ki = k as usize;
                    if ki >= info.nodes.len() {
                        break;
                    }
                    if global_index.contains_key(&info.nodes[ki].to_ascii_lowercase()) {
                        break;
                    }
                    k = info.parents.get(ki).copied().unwrap_or(-1);
                }
                if k == -1 {
                    errs.push(QcError::new(
                        file,
                        line,
                        crate::tr_fmt!(
                            "unable to find valid bone for attachment %{attachment}:%{bone} (the official `simplify.cpp:5360`)",
                            attachment = at.name,
                            bone = at.bone
                        ),
                    ));
                    out.push(None);
                    continue;
                }
                let ka = k as usize;
                // `boneToPose` = 该 source **第 0 帧**沿父链累积的姿态。
                let world = crate::bone_math::compute_world(
                    &info.rest_positions,
                    &info.rest_rotations,
                    &info.parents,
                );
                let gi = global_index[&info.nodes[ka].to_ascii_lowercase()];
                let corr = if at.absolute {
                    crate::bone_math::identity()
                } else {
                    crate::bone_math::concat(&crate::bone_math::invert(&world[ka]), &world[k0])
                };
                out.push(Some((gi, corr)));
            }
            self.errors.extend(errs);
            let mut it = out.into_iter();
            for at in &mut self.desc.attachments {
                at.resolved = it.next().flatten();
            }
        }

        // ---- 3. 材质表 ----
        //
        // 官方顺序：`$texturegroup` 里的材质**先**（`Cmd_TextureGroup`
        // 按遇到顺序 `use_texture_as_material`），其余按 SMD 首现序。
        // 实测见 `derive_skin_from_qc.js`。
        //
        // ⚠️ 官方只读**第一条** `$texturegroup`（`SetSkinValues` 用
        // `g_texturegroup[0]`），所以这里也只用第一条。
        let families_src: Vec<Vec<String>> =
            self.texture_groups.first().cloned().unwrap_or_default();
        for g in &families_src {
            for name in g {
                self.register_texture(name);
            }
        }
        for f in &files {
            let Some(info) = smd_cache.get(f).and_then(|x| x.as_ref()) else {
                continue;
            };
            for m in &info.materials {
                self.register_texture(m);
            }
        }
        // 纹理名规范化：官方对**不在 `$texturegroup` 里**的材质带
        // `$cdmaterials` 前缀（实测 survivor：组内 26 条裸名、其余 20 条带前缀）。
        //
        // 判据见 `PROGRESS.md` §43.3：`lookup_texture` 用的是**原样**名字，
        // 而 SMD 里的名字自带前缀 —— 所以「带不带前缀」由 SMD 决定，
        // 这里不做推断，只把已注册的名字原样写出。
        let cd = self.desc.materials.search_paths.clone();
        self.desc.materials.textures = self
            .texture_order
            .iter()
            .map(|n| Texture {
                name: crate::mdl_writer::normalize_texture_name(n, &cd),
                flags: None,
            })
            .collect();

        // ---- 4. skin family（`SetSkinValues`）----
        //
        // ```cpp
        // for i,j: g_skinref[i][j] = j;                        // 恒等
        // for i in layers: for j in reps:
        //     g_skinref[i][ g_texturegroup[0][0][j] ] = g_texturegroup[0][i][j];
        // ```
        if !families_src.is_empty() {
            let idx_of: HashMap<String, i32> = self
                .texture_order
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), i as i32))
                .collect();
            let base: Vec<i32> = families_src[0]
                .iter()
                .map(|n| *idx_of.get(n).unwrap_or(&-1))
                .collect();
            let n_tex = self.texture_order.len() as i32;
            let mut families = Vec::new();
            for g in &families_src {
                let mut row: Vec<i32> = (0..n_tex).collect();
                for (j, name) in g.iter().enumerate() {
                    if j >= base.len() {
                        break;
                    }
                    let slot = base[j];
                    if slot >= 0 && (slot as usize) < row.len() {
                        row[slot as usize] = *idx_of.get(name).unwrap_or(&slot);
                    }
                }
                families.push(row);
            }
            self.desc.materials.skin_families = families;
        }

        let anim_sources = self.desc.animations.clone();
        let mut event_source_frames = HashMap::new();
        for seq in &mut self.desc.sequences {
            if seq.events.is_empty() {
                continue;
            }
            let first = seq.blends.first().unwrap_or(&seq.smd);
            let named = anim_sources
                .iter()
                .find(|animation| animation.name.eq_ignore_ascii_case(first));
            let (source, range, stack, source_fps, fudge_loop, commands) = named.map_or(
                (
                    first,
                    seq.frames,
                    seq.src_stack.as_deref(),
                    seq.src_fps,
                    seq.fudge_loop,
                    &seq.cmds,
                ),
                |animation| {
                    (
                        &animation.smd,
                        animation.frames,
                        animation.src_stack.as_deref(),
                        animation.src_fps,
                        animation.fudge_loop,
                        &animation.cmds,
                    )
                },
            );
            let key = (
                source.clone(),
                stack.map(str::to_string),
                source_fps.map(f32::to_bits),
            );
            let source_nf = if let Some(&count) = event_source_frames.get(&key) {
                count
            } else {
                let options = src_opts_of
                    .get(source)
                    .and_then(|options| options.first())
                    .cloned()
                    .unwrap_or_default()
                    .with_anim(stack, source_fps);
                let path =
                    crate::compile::resolve_smd_path(&self.qdir, source).map_err(|error| {
                        vec![QcError::new(
                            self.main_path.display().to_string(),
                            0,
                            error.to_string(),
                        )]
                    })?;
                let count = crate::compile::read_source_frames(&path, &seq.name, &options)
                    .map_err(|error| {
                        vec![QcError::new(
                            self.main_path.display().to_string(),
                            0,
                            error.to_string(),
                        )]
                    })?
                    .frames
                    .len();
                event_source_frames.insert(key, count);
                count
            };
            let mut count = match range {
                Some([lo, hi]) if source_nf > 0 => {
                    let last = source_nf.saturating_sub(1) as i32;
                    (hi.clamp(0, last) - lo.clamp(0, last) + 1).max(0) as usize
                }
                _ => source_nf,
            };
            if fudge_loop {
                count = count.saturating_add(1);
            }
            if let Some(frames) = seq.num_frames.filter(|_| {
                !commands
                    .iter()
                    .any(|command| matches!(command, AnimCmd::NumFrames { .. }))
            }) {
                count = count.max(frames.max(0) as usize);
            }
            for command in commands {
                match command {
                    AnimCmd::NoAnimation => count = 1,
                    AnimCmd::NumFrames { frames } if *frames > 0 => count = *frames as usize,
                    AnimCmd::Compress { frames } if *frames > 0 => {
                        count = count.div_ceil(*frames as usize)
                    }
                    _ => {}
                }
            }
            let last = count.saturating_sub(1) as f32;
            for event in &mut seq.events {
                if count == 0 || !event.cycle.is_finite() {
                    self.errors.push(QcError::new(
                        self.main_path.display().to_string(),
                        0,
                        format!(
                            "event frame {} is invalid for sequence {:?} ({} frames)",
                            event.cycle, seq.name, count
                        ),
                    ));
                } else if event.cycle < 0.0 || event.cycle > last {
                    crate::warning!(
                        "Event {} (frame {}) is outside sequence {:?} ({} frames); using cycle 0",
                        if event.name.is_empty() {
                            event.event.to_string()
                        } else {
                            event.name.clone()
                        },
                        event.cycle,
                        seq.name,
                        count
                    );
                    event.cycle = 0.0;
                } else {
                    event.cycle = if last > 0.0 { event.cycle / last } else { 0.0 };
                }
            }
        }

        // ---- 6. 杂项回填 ----
        //
        // 顶层 `$sectionframes` 是**全局**设置（官方 `g_sectionframes`），
        // 而 mdlc 的 IR 是逐序列字段 —— 回填给尚未显式设置的序列。
        if let Some((len, thr)) = self.global_section_frames {
            for seq in &mut self.desc.sequences {
                if seq.section_frames.is_none() {
                    seq.section_frames = Some(len);
                    seq.section_threshold = Some(thr);
                }
            }
            // ⚠️ **同时记到 model 上** —— 没有被任何序列引用的 `$animation`
            // 拿不到上面那个逐序列回填，只能按全局值判（见
            // `ModelMeta::section_frames`）。实测 `look_neutral`：
            // 官方 `sf=0`，而 mdlc 因为落到 `seq[0]` 而继承了 `idle` 的 30。
            self.desc.model.section_frames = Some((len, thr));
        }
        self.desc.model.contents = Some(self.contents);
        self.desc.model.static_prop = self.static_prop;
        // `extra_flags`：`static_prop` 已由 `mdl_writer` 自动置位，
        // 这里只写「额外的」那些位。
        let extra = self.gflags & !FLAG_STATIC_PROP;
        if extra != 0 {
            self.desc.model.extra_flags = Some(extra);
        }
        // 若 `$modelname` 没写，用主 QC 的文件名（官方要求必须写，这里更宽容）。
        if self.desc.model.name.is_empty() {
            let stem = self
                .main_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "model".to_string());
            self.desc.model.name = format!("{stem}.mdl");
        }
        // `$lod` 的骨骼选项里，**不存在的骨骼要剔除**。
        //
        // # 官方行为（oracle 实测，不是推测）
        //
        // `lodbc.qc` 写 `bonetreecollapse "tip"`，而 `lodbc.smd` 的 nodes
        // 只有 `root/hip/knee/ankle`。官方 studiomdl 输出：
        //
        // ```text
        // WARNING: Couldn't find bone tip for bonetreecollapse, skipping
        // ```
        //
        // 然后**正常产出** `.mdl`（4 根骨骼，无该 LOD 的坍缩）——
        // 即「未知骨骼 ⟹ 该条**跳过**」，不是错误。
        //
        // ⚠️ mdlc 的 `ModelDesc::validate()` 会把未知骨骼当**错误**
        // （见那里的注释：静默失效最难排查）。这个判断对**手写 TOML**
        // 是对的 —— 用户写错名字应当被告知。但 QC 路径下官方是容忍的，
        // 而解析器的职责是「复刻官方」，所以在这里**先剔除**，
        // 让 QC 与官方行为一致；手写 TOML 仍然会被 validate 拦住。
        //
        // 剔除时打印一行提示（官方也是 WARNING 而不是静默）。
        let known: HashSet<String> = self
            .desc
            .bones
            .iter()
            .map(|b| b.name.to_ascii_lowercase())
            .collect();
        let mut skipped: Vec<String> = Vec::new();
        for bp in &mut self.desc.bodyparts {
            for m in &mut bp.models {
                for lod in &mut m.lods {
                    lod.bone_tree_collapse.retain(|b| {
                        let ok = known.contains(&b.to_ascii_lowercase());
                        if !ok {
                            skipped.push(b.clone());
                        }
                        ok
                    });
                    lod.replace_bone.retain(|p| {
                        // `replacebone <源> <目标>`：**两个**都必须存在，
                        // 否则整条跳过（官方同一处 warning）。
                        let ok = known.contains(&p[0].to_ascii_lowercase())
                            && known.contains(&p[1].to_ascii_lowercase());
                        if !ok {
                            skipped.push(format!("{} → {}", p[0], p[1]));
                        }
                        ok
                    });
                }
            }
        }
        if !skipped.is_empty() {
            skipped.sort();
            skipped.dedup();
            log::info!(
                "{}",
                crate::tr_fmt!(
                    "Note: $lod names %{n} bones that do not exist; skipped, matching the official behavior: %{list}",
                    n = skipped.len(),
                    list = skipped.join(", ")
                )
            );
        }
        // 若没有任何 bodypart（官方会报错），保持空 —— `validate()` 会报。

        if !self.errors.is_empty() {
            return Err(std::mem::take(&mut self.errors));
        }
        // `desc` 用 `clone()` 而不是 `mem::take` —— 后者要求
        // `ModelDesc: Default`，而它没有（且不该有：`model` 是必填）。
        Ok(self.desc.clone())
    }

    /// 注册一个材质名（官方 `lookup_texture` + `use_texture_as_material`）。
    fn register_texture(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        let key = name.to_string();
        if self.texture_index.contains_key(&key) {
            return;
        }
        let i = self.texture_order.len();
        self.texture_order.push(key.clone());
        self.texture_index.insert(key, i);
    }
}

/// `Option_Flex` 的后缀选项集合。
///
/// 默认值来自官方 `Option_Flex` 的初始化段：
/// `frame = 0`、`target1`（= `position`）= **1.0**、`decay` = **1.0**，
/// `split` = 调用方传入的 `pairsplit`。
struct FlexOpts {
    /// `frame <n>` —— 取 `.vta` 的相对帧号。
    frame: i32,
    /// `position <f>` —— 落进 `mstudioflex_t.target1`。
    position: f32,
    /// `split <f>` —— smoothstep 分割点。
    split: f32,
    /// `decay <f>` —— 每条 vertanim 的 `speed` 通道。
    decay: f32,
}

/// 把骨骼表排成「父骨骼必定在子骨骼之前」的顺序。
///
/// # 为什么需要（oracle 实测）
///
/// `BuildGlobalBonetable`（`simplify.cpp:3616`）先把 `$definebone` 的骨骼
/// 按**书写序**插入，然后才并入各 SMD 的骨骼。于是可能出现
/// **父在子之后**的排列：
///
/// ```text
/// ipq8.qc:  $definebone "b" "a" 0 0 10 …   ← b 先插入，父是 a
/// ipr.smd:  nodes = root, a, b             ← a 后插入
/// ```
///
/// 而官方 `ipq8.mdl` 的骨骼表是 **`BONE[0]=root, [1]=a, [2]=b`**
/// —— 官方在 `BuildGlobalBonetable` 之后有一步按父链重排，
/// 保证 `parent < index` 这个引擎不变量（否则引擎解算父链会读到未初始化项）。
///
/// mdlc 的 `ModelDesc::validate()` 也要求这条不变量，所以在解析收尾时
/// 做一次**稳定**拓扑排序：保持原有相对顺序，只把父提到子之前。
///
/// 悬空父引用（父不在表里）**不在这里报错** —— 交给 `validate()`，
/// 它能给出带路径的可读错误。
fn sort_parents_first(bones: Vec<Bone>) -> Vec<Bone> {
    let present: HashSet<String> = bones.iter().map(|b| b.name.to_ascii_lowercase()).collect();
    let mut sorted: Vec<Bone> = Vec::with_capacity(bones.len());
    let mut placed: HashSet<String> = HashSet::new();
    loop {
        if sorted.len() == bones.len() {
            break;
        }
        let mut progressed = false;
        for b in &bones {
            let key = b.name.to_ascii_lowercase();
            if placed.contains(&key) {
                continue;
            }
            let ready = match b.parent.as_deref() {
                None => true,
                Some(p) => {
                    let pk = p.to_ascii_lowercase();
                    !present.contains(&pk) || placed.contains(&pk)
                }
            };
            if ready {
                placed.insert(key);
                sorted.push(b.clone());
                progressed = true;
            }
        }
        if !progressed {
            // 存在环（A 的父是 B、B 的父是 A）—— 原样接上，
            // 交给 `validate()` 报「父骨骼必须更靠前」。
            for b in &bones {
                if !placed.contains(&b.name.to_ascii_lowercase()) {
                    sorted.push(b.clone());
                }
            }
            break;
        }
    }
    sorted
}

fn merge_source_options(
    groups: &mut Vec<crate::compile::SrcOpts>,
    options: crate::compile::SrcOpts,
) {
    let compatible = groups.iter_mut().find(|existing| {
        existing.input_scale == options.input_scale
            && existing.root_adjust == options.root_adjust
            && existing.root_rotation == options.root_rotation
            && existing.fbx.material == options.fbx.material
            && existing.fbx.scale == options.fbx.scale
            && existing.fbx.axis == options.fbx.axis
            && existing.stack == options.stack
            && existing.fps == options.fps
            && existing.shape_keys == options.shape_keys
            && existing.shape_key_order == options.shape_key_order
            && existing.shape_key_ignore == options.shape_key_ignore
    });
    if let Some(existing) = compatible {
        if existing.fbx.parts.is_empty() || options.fbx.parts.is_empty() {
            existing.fbx.parts.clear();
        } else {
            for part in options.fbx.parts {
                if !existing
                    .fbx
                    .parts
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&part))
                {
                    existing.fbx.parts.push(part);
                }
            }
        }
    } else {
        groups.push(options);
    }
}

/// 一个 SMD 里 `finish()` 需要的信息。
struct SmdInfo {
    /// `nodes` 段的骨骼名（按下标序）。
    nodes: Vec<String>,
    /// 三角形段里材质名的首现序。
    materials: Vec<String>,
    /// **被顶点引用**的 `nodes` 下标（升序去重）。
    ///
    /// 官方 `TagUsedBones` 的顶点循环（`simplify.cpp:3441-3455`）只给这些骨骼
    /// 置 `BONE_USED_BY_VERTEX_LOD0`，而 `BuildGlobalBonetable`
    /// （`simplify.cpp:3668`）正是靠 `psource->boneref[j]` 决定收不收 ——
    /// 没有顶点引用的骨骼（只在 `nodes` 段挂着、动画里动一动）**根本进不了
    /// 骨骼表**。mdlc 早期把每个 SMD 的每个 node 都收进来，多出的骨骼会把
    /// 总数顶过引擎的 `MAXSTUDIOBONES`（128），导致引擎栈越界崩溃。
    ///
    /// 数据源：`triangles` 段每个顶点的 `links[].bone`。注意 `smd.rs` 已保证
    /// `links` 非空（`links == 0` 时会塞进行首的 `parentBone`），所以这里
    /// 不必再看 `parent_bone`。
    vert_refs: std::collections::BTreeSet<usize>,
    /// `nodes` 段每根骨骼的**父下标**（`-1` 表示无父）。
    ///
    /// 对应官方 `psource->localBone[j].parent`。两处判据要用它：
    /// 刚性 `$attachment` 沿父链找第一根有顶点权重的骨骼
    /// （`simplify.cpp:3478-3484`），以及 `UpdateBonerefRecursive`
    /// 的父链上传（`simplify.cpp:3404-3418`）。
    parents: Vec<i32>,
    /// 第 0 帧（`Smd::reference_frame`）里每根骨骼的**局部**位置。
    ///
    /// 官方 `Build_Reference()` 拿第 0 帧的 `rawanim[0][i]` 沿父链累积出
    /// `psource->boneToPose[]`（`studiomdl.cpp:728-762`），而 `LinkAttachments()`
    /// 的阶段 2 要用**这个**矩阵（不是全局骨骼表的 `g_bonetable[k].boneToPose`）
    /// 重算附着点的 `local`：`simplify.cpp:5350` 取它、`:5365` 对它求逆、
    /// `:5385` 用它左乘。
    ///
    /// 第 0 帧没提到的骨骼按 `[0,0,0]` 算 —— 官方 `Grab_Animation`
    /// （`studiomdl.cpp:1065-1135`）用 `kalloc` 零填充并逐骨骼拷贝上一帧，
    /// 首帧缺失的骨骼就是零。
    rest_positions: Vec<[f32; 3]>,
    /// 同上，**弧度**（`SmdPose::rotation` 就是弧度）。
    rest_rotations: Vec<[f32; 3]>,
}
impl SmdInfo {
    fn from_source(source: &crate::smd::Smd) -> Self {
        let mut positions = vec![[0.0; 3]; source.nodes.len()];
        let mut rotations = vec![[0.0; 3]; source.nodes.len()];
        if let Some(frame) = source.reference_frame() {
            for pose in &frame.poses {
                if let Ok(bone) = usize::try_from(pose.bone)
                    && bone < positions.len()
                {
                    positions[bone] = pose.position;
                    rotations[bone] = pose.rotation;
                }
            }
        }
        Self {
            nodes: source.nodes.iter().map(|node| node.name.clone()).collect(),
            materials: source.materials_in_order(),
            parents: source.nodes.iter().map(|node| node.parent).collect(),
            vert_refs: source
                .triangles
                .iter()
                .flat_map(|triangle| &triangle.vertices)
                .flat_map(|vertex| &vertex.links)
                .filter_map(|link| usize::try_from(link.bone).ok())
                .filter(|&bone| bone < source.nodes.len())
                .collect(),
            rest_positions: positions,
            rest_rotations: rotations,
        }
    }

    fn merge(&mut self, incoming: Self) {
        let mut indices: HashMap<_, _> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, name)| (name.to_ascii_lowercase(), index))
            .collect();
        let mut remap = Vec::with_capacity(incoming.nodes.len());
        let original_count = self.nodes.len();
        for (index, name) in incoming.nodes.into_iter().enumerate() {
            let next = self.nodes.len();
            let mapped = *indices.entry(name.to_ascii_lowercase()).or_insert(next);
            remap.push(mapped);
            if mapped == next {
                self.nodes.push(name);
                self.parents.push(-1);
                self.rest_positions.push(incoming.rest_positions[index]);
                self.rest_rotations.push(incoming.rest_rotations[index]);
            }
        }
        for (index, &mapped) in remap.iter().enumerate() {
            if mapped >= original_count {
                self.parents[mapped] = usize::try_from(incoming.parents[index])
                    .ok()
                    .and_then(|parent| remap.get(parent))
                    .map_or(-1, |&parent| parent as i32);
            }
        }
        self.vert_refs
            .extend(incoming.vert_refs.into_iter().map(|index| remap[index]));
        for material in incoming.materials {
            if !self.materials.contains(&material) {
                self.materials.push(material);
            }
        }
    }
}

/// 把字符串解析成 `f32`（官方 `verify_atof`）。
///
/// 官方 `verify_atof` 用 `atof` —— 它**只读前导数字**（`"30fps"` → 30、
/// `"abc"` → 0），且接受 `.5` 这种没有整数部分的形式。
/// 这里先试严格解析，失败再取前导数字前缀。
fn parse_f32(s: &str) -> Option<f32> {
    let t = s.trim();
    if let Ok(v) = t.parse::<f32>() {
        return Some(v);
    }
    // 前导数字前缀（模拟 `atof` 的宽容）。
    let end = t
        .find(|c: char| {
            !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')
        })
        .unwrap_or(t.len());
    t[..end].parse::<f32>().ok()
}

/// `$jigglebone` 的 `block` 块是否接受键 `key`。
///
/// # 权威来源：反编译 + 27 个受控夹具
///
/// 三个块解析器（`FUN_00454800` = `is_flexible`、`FUN_004549d0` = `is_rigid`、
/// `FUN_00454cc0` = `has_base_spring`）**不是**同一套键表：
///
/// * `is_flexible` 与 `has_base_spring` 在自己的独有键之外，还会把
///   不匹配的 token 交给**共享子解析器** `FUN_004545b0`（被两者调用的
///   那 9 个「通用键」）。
/// * `is_rigid` **不调用**共享子解析器，而是把全部键**内联**在自己体内 ——
///   所以它接受那 9 个通用键，但**不接受** `allow_length_flex`
///   （实测 `jig45` 官方 abort）。
///
/// `key` 与 `block` 都必须是**小写**（调用方已 `to_ascii_lowercase`）。
///
/// 官方对任何不接受的键走 `_ => abort`：
/// `$jigglebone: invalid syntax '%s'` + `Aborted Processing on '<mdl>'`。
fn jiggle_block_accepts(block: &str, key: &str) -> bool {
    /// 三块共享的 9 个「通用键」（共享子解析器 `FUN_004545b0`）。
    const SHARED: [&str; 9] = [
        "tip_mass",
        "length",
        "angle_constraint",
        "yaw_constraint",
        "yaw_friction",
        "yaw_bounce",
        "pitch_constraint",
        "pitch_friction",
        "pitch_bounce",
    ];
    if SHARED.contains(&key) {
        // 三块都接受它们。
        return matches!(block, "is_flexible" | "is_rigid" | "has_base_spring");
    }
    match block {
        "is_flexible" => matches!(
            key,
            "yaw_stiffness"
                | "yaw_damping"
                | "pitch_stiffness"
                | "pitch_damping"
                | "along_stiffness"
                | "along_damping"
                | "allow_length_flex"
        ),
        // ⚠️ 没有 `allow_length_flex`（实测 `jig45` abort）。
        "is_rigid" => false,
        "has_base_spring" => matches!(
            key,
            // 官方 QC 是**裸键**，没有 `base_` 前缀。
            "stiffness"
                | "damping"
                | "base_mass"
                | "left_constraint"
                | "up_constraint"
                | "forward_constraint"
                | "left_friction"
                | "up_friction"
                | "forward_friction"
        ),
        _ => false,
    }
}

/// 把字符串解析成 `i32`（官方 `verify_atoi` / `atoi`）。
///
/// 官方 `verify_atoi` 用 `atoi` —— 它**只读前导数字**，
/// 所以 `"30fps"` 得到 30、`"abc"` 得到 0。这里先试严格整数，
/// 失败再试浮点截断，最后回落到 0（与 `atoi` 一致）。
fn parse_i32(s: &str) -> Option<i32> {
    let t = s.trim();
    t.parse::<i32>()
        .ok()
        .or_else(|| t.parse::<f32>().ok().map(|v| v as i32))
}

/// 解析 `.vrd`（`$proceduralbones` 指向的文件）。
///
/// 结构见 `PROGRESS.md` §22.1（**两层**：`proctype 2` 的骨骼 + 触发器表）。
fn parse_vrd(text: &str) -> Result<Vec<QuatInterpBone>, String> {
    fn tokens(line: &str) -> Result<Vec<String>, String> {
        let mut chars = line.chars().peekable();
        let mut tokens = Vec::new();
        while let Some(ch) = chars.next() {
            if ch.is_whitespace() {
                continue;
            }
            if ch == '/' && chars.peek() == Some(&'/') {
                break;
            }
            let mut token = String::new();
            if ch == '"' {
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '"' {
                        closed = true;
                        break;
                    }
                    token.push(ch);
                }
                if !closed {
                    return Err("Unterminated quoted VRD token".into());
                }
            } else {
                token.push(ch);
                while chars.peek().is_some_and(|ch| !ch.is_whitespace()) {
                    token.push(chars.next().unwrap());
                }
            }
            tokens.push(token);
        }
        Ok(tokens)
    }
    let mut out = Vec::new();
    let mut current: Option<QuatInterpBone> = None;
    for (line, raw) in text.lines().enumerate() {
        let tokens = tokens(raw)?;
        let Some(key) = tokens.first() else { continue };
        let values = &tokens[1..];
        let invalid = || format!("Invalid VRD {} directive on line {}", key, line + 1);
        match key.to_ascii_lowercase().as_str() {
            "bone" | "<helper>" => {
                let standard = key.eq_ignore_ascii_case("<helper>");
                if values.len() != if standard { 4 } else { 2 } {
                    return Err(invalid());
                }
                if let Some(bone) = current.take() {
                    out.push(bone);
                }
                current = Some(QuatInterpBone {
                    bone: values[0].clone(),
                    control: values[if standard { 3 } else { 1 }].clone(),
                    base_pos: None,
                    triggers: Vec::new(),
                });
            }
            "basepos" | "base_pos" | "<basepos>" | "trigger" | "<trigger>" => {
                let is_base = matches!(
                    key.to_ascii_lowercase().as_str(),
                    "basepos" | "base_pos" | "<basepos>"
                );
                let numbers: Vec<f32> = values
                    .iter()
                    .map(|value| value.parse::<f32>().map_err(|_| invalid()))
                    .collect::<Result<_, _>>()?;
                if numbers.iter().any(|value| !value.is_finite())
                    || (is_base && numbers.len() != 3)
                    || (!is_base && ![7, 10].contains(&numbers.len()))
                {
                    return Err(invalid());
                }
                let bone = current.as_mut().ok_or_else(invalid)?;
                if is_base {
                    bone.base_pos = Some([numbers[0], numbers[1], numbers[2]]);
                } else {
                    if numbers[0] <= 0.0 {
                        return Err(invalid());
                    }
                    bone.triggers.push(QuatInterpTrigger {
                        tolerance: numbers[0],
                        trigger: [numbers[1], numbers[2], numbers[3]],
                        angles: [numbers[4], numbers[5], numbers[6]],
                        pos: (numbers.len() == 10).then(|| [numbers[7], numbers[8], numbers[9]]),
                    });
                }
            }
            "<display>" => {}
            _ => {
                return Err(format!(
                    "Unsupported VRD directive {} on line {}",
                    key,
                    line + 1
                ));
            }
        }
    }
    if let Some(bone) = current {
        out.push(bone);
    }
    if out
        .iter()
        .any(|bone| bone.bone.is_empty() || bone.control.is_empty() || bone.triggers.is_empty())
    {
        return Err("VRD helpers require a target, control bone, and triggers".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    // 本模块全部走**完整路径**（`crate::qc::parse_qc_str` /
    // `crate::model::ModelDesc`），所以**不需要** `use super::*` ——
    // 加了反而触发 `unused_imports`（CI 的 `-D warnings` 会当错误）。

    /// 一个最小 SMD（2 骨骼 + 1 三角形）。
    const MIN_SMD: &str = "\
version 1
nodes
0 \"root\" -1
1 \"bone1\" 0
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
1 0.000000 0.000000 1.000000 0.000000 0.000000 0.000000
end
triangles
mat
0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000
0 1.000000 0.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000
0 0.000000 1.000000 0.000000 0.000000 0.000000 1.000000 0.000000 1.000000 1 1 1.000000
end
";

    /// 建一个临时目录，写 `a.smd` / `b.smd` / `c.smd`。
    ///
    /// 目录名带 **pid**，避免并行测试互踩（与 `flexrule` 的 e2e 测试同法）。
    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mdlc_qcparse_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("应能建临时目录");
        for n in ["a.smd", "b.smd", "c.smd"] {
            std::fs::write(dir.join(n), MIN_SMD).expect("应能写 SMD");
        }
        dir
    }

    /// 解析一段 QC，返回 `ModelDesc`。
    fn parse(qc: &str, dir: &std::path::Path) -> crate::model::ModelDesc {
        crate::qc::parse_qc_str(qc, dir)
            .unwrap_or_else(|errs| panic!("QC 应解析成功，实际：{errs:?}"))
    }

    #[test]
    fn contents_commands_write_sdk_bits_and_accumulate_model_masks() {
        let dir = fixture("contents_masks");
        for combination in 0..16 {
            let mask = [1, 8, 0x0200_0000, 0x2000_0000]
                .into_iter()
                .enumerate()
                .filter(|(index, _)| combination & (1 << index) != 0)
                .fold(0, |mask, (_, flag)| mask | flag);
            let mut commands = String::from("$contents notsolid\n");
            for (flag, name) in [
                (8, "grate"),
                (0x0200_0000, "monster"),
                (0x2000_0000, "ladder"),
                (1, "solid"),
            ] {
                if mask & flag != 0 {
                    commands.push_str(&format!("$contents {name}\n"));
                }
            }
            let desc = parse(&format!("$body body a.smd\n{commands}"), &dir);
            let output =
                crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                    .unwrap();
            let integer =
                |at: usize| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
            assert_eq!(integer(332), mask, "combination {combination}");
            let bones = integer(160) as usize;
            for index in 0..integer(156) as usize {
                assert_eq!(integer(bones + index * 216 + 180), mask);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn joint_contents_reset_and_inherit_after_bone_rename_and_hierarchy() {
        let dir = fixture("joint_contents_hierarchy");
        std::fs::write(
            dir.join("rig.smd"),
            include_str!("../../tests/fixtures/rig.smd"),
        )
        .unwrap();
        let desc = parse(
            "$body body rig.smd\n$sequence idle rig.smd\n$contents grate ladder monster\n$contents solid\n$jointcontents ROOT grate ladder\n$jointcontents root monster\n$jointcontents mid grate\n$renamebone mid middle\n$hierarchy tip root\n",
            &dir,
        );
        assert_eq!(desc.model.joint_contents.last().unwrap().0, "middle");
        let output =
            crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                .unwrap();
        let integer = |at: usize| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap());
        assert_eq!(integer(332), 0x2200_0009);
        let bones = integer(160) as usize;
        for (index, bone) in output.compiled.desc.bones.iter().enumerate() {
            let expected = if bone.name == "middle" {
                8
            } else {
                0x0200_0001
            };
            assert_eq!(
                integer(bones + index * 216 + 180),
                expected,
                "{}",
                bone.name
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn collision_blocks_accept_contents_and_joint_contents_with_reset_semantics() {
        let dir = fixture("collision_contents");
        let desc = parse(
            "$body body a.smd\n$contents monster\n$collisionjoints b.smd\n{\n$contents grate ladder\n$jointcontents ROOT grate monster\n$jointcontents root ladder\n}\n",
            &dir,
        );
        assert_eq!(desc.model.contents, Some(0x2200_0008));
        assert_eq!(
            crate::contents::bone_contents(&desc).unwrap(),
            [0x2000_0001; 2]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attachment_axes_orthogonalize_skew_vectors_and_respect_option_order() {
        let dir = fixture("attachment_axes");
        let desc = parse(
            "$body body a.smd\n$attachment axes root 3 -4 5 rotate 17 23 41 x_and_z_axes 2 2 0 1 1 3 world_align\n$attachment rotated root 3 -4 5 x_and_z_axes 2 2 0 1 1 3 rotate 0 90 0\n$attachment absolute_first root 3 -4 5 absolute x_and_z_axes 2 2 0 1 1 3\n",
            &dir,
        );
        let output =
            crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                .unwrap();
        let attachments = i32::from_le_bytes(output.mdl[244..248].try_into().unwrap()) as usize;
        let c = std::f32::consts::FRAC_1_SQRT_2;
        for (index, expected) in [
            [c, -c, 0.0, 3.0, c, c, 0.0, -4.0, 0.0, 0.0, 1.0, 5.0],
            [0.0, -1.0, 0.0, 3.0, 1.0, 0.0, 0.0, -4.0, 0.0, 0.0, 1.0, 5.0],
        ]
        .into_iter()
        .enumerate()
        {
            let at = attachments + index * 92;
            for (component, expected) in expected.into_iter().enumerate() {
                let field = at + 12 + component * 4;
                let actual = f32::from_le_bytes(output.mdl[field..field + 4].try_into().unwrap());
                assert!(
                    (actual - expected).abs() < 0.00001,
                    "attachment={index}, component={component}: {actual} != {expected}"
                );
            }
        }
        assert_eq!(
            i32::from_le_bytes(
                output.mdl[attachments + 4..attachments + 8]
                    .try_into()
                    .unwrap()
            ),
            0x10000
        );
        assert_eq!(desc.attachments[2].absolute_rotation, Some(false));
        for axes in [
            "0 0 0 0 0 1",
            "1 0 0 0 0 0",
            "1 2 3 2 4 6",
            "NaN 0 0 0 0 1",
            "1 0 0 0 inf 1",
            "1 0 0 0 0",
        ] {
            assert!(
                crate::qc::parse_qc_str(
                    &format!("$body body a.smd\n$attachment bad root 0 0 0 x_and_z_axes {axes}\n"),
                    &dir
                )
                .is_err(),
                "{axes}"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn joint_surface_properties_inherit_after_rename_and_reparenting() {
        let dir = fixture("joint_surface_inheritance");
        std::fs::write(
            dir.join("rig.smd"),
            include_str!("../../tests/fixtures/rig.smd"),
        )
        .unwrap();
        for (hierarchy, expected_tip) in [("", "flesh"), ("$hierarchy tip root\n", "metal")] {
            let desc = parse(
                &format!(
                    "$body body rig.smd\n$sequence idle rig.smd\n$surfaceprop wood\n$jointsurfaceprop ROOT rubber\n$jointsurfaceprop root metal\n$jointsurfaceprop mid flesh\n$renamebone mid middle\n{hierarchy}"
                ),
                &dir,
            );
            let output =
                crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                    .unwrap();
            let bones = i32::from_le_bytes(output.mdl[160..164].try_into().unwrap()) as usize;
            for (index, bone) in output.compiled.desc.bones.iter().enumerate() {
                let at = bones + index * 216;
                let relative =
                    i32::from_le_bytes(output.mdl[at + 176..at + 180].try_into().unwrap());
                let start = at.checked_add_signed(relative as isize).unwrap();
                let end = start
                    + output.mdl[start..]
                        .iter()
                        .position(|&byte| byte == 0)
                        .unwrap();
                let expected = match bone.name.as_str() {
                    "root" => "metal",
                    "middle" => "flesh",
                    _ => expected_tip,
                };
                assert_eq!(
                    &output.mdl[start..end],
                    expected.as_bytes(),
                    "{}",
                    bone.name
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lowercasing_sequences_is_positional_and_preserves_layer_references() {
        let dir = fixture("lowercase_sequences");
        let desc = parse(
            "$modelname models/tests/lowercase.mdl\n$body body a.smd\n$sequence Before a.smd\n$lcaseallsequences\n$animation AnimCase a.smd\n$sequence AfTeR AnimCase { addlayer Before addlayer LAYER }\n$sequence LaYeR a.smd\n$declaresequence DeclaredCase\n",
            &dir,
        );
        assert_eq!(
            desc.sequences
                .iter()
                .map(|seq| seq.name.as_str())
                .collect::<Vec<_>>(),
            ["Before", "after", "layer", "DeclaredCase"]
        );
        assert_eq!(desc.animations[0].name, "AnimCase");
        let output =
            crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                .unwrap();
        let integer =
            |at: usize| i32::from_le_bytes(output.mdl[at..at + 4].try_into().unwrap()) as usize;
        let sequences = integer(192);
        for (index, expected) in ["Before", "after", "layer", "DeclaredCase"]
            .into_iter()
            .enumerate()
        {
            let record = sequences + index * 212;
            let name = record + integer(record + 4);
            let end = output.mdl[name..]
                .iter()
                .position(|byte| *byte == 0)
                .unwrap();
            assert_eq!(&output.mdl[name..name + end], expected.as_bytes());
        }
        let record = sequences + 212;
        assert_eq!(integer(record + 148), 2);
        let layers = record + integer(record + 152);
        assert_eq!(
            u16::from_le_bytes(output.mdl[layers..layers + 2].try_into().unwrap()),
            0
        );
        assert_eq!(
            u16::from_le_bytes(output.mdl[layers + 24..layers + 26].try_into().unwrap()),
            2
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn root_lod_restrictions_write_the_header_byte_without_changing_geometry() {
        let dir = fixture("allowed_root_lods");
        let build = |value: i32| {
            let desc = parse(
                &format!(
                    "$modelname models/tests/root_lods.mdl\n$body body a.smd\n$allowrootlods {value}\n"
                ),
                &dir,
            );
            crate::pipeline::build(&desc, &dir, crate::pipeline::PipelineOptions::default())
                .unwrap()
        };
        let original = build(0);
        for value in [1, 2, 8, 255] {
            let output = build(value);
            assert_eq!(output.mdl[0x17a], value as u8);
            assert_eq!(output.mdl[0x179], 0);
            assert_eq!(&original.vvd[12..], &output.vvd[12..]);
            assert_eq!(&original.vtx[20..], &output.vtx[20..]);
        }
        for value in [-1, 256] {
            assert!(crate::qc::parse_qc_str(&format!("$allowrootlods {value}\n"), &dir).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn write_five_frame_animation(dir: &std::path::Path) {
        let mut smd = String::from("version 1\nnodes\n0 \"root\" -1\nend\nskeleton\n");
        for frame in 0..5 {
            smd.push_str(&format!("time {frame}\n0 {frame} 0 0 0 0 0\n"));
        }
        smd.push_str("end\n");
        std::fs::write(dir.join("motion.smd"), smd).unwrap();
    }

    #[test]
    fn implicit_sequence_frame_range_controls_event_cycles() {
        let dir = fixture("implicit_sequence_range");
        write_five_frame_animation(&dir);
        let desc = parse(
            "$body body a.smd\n$sequence selected motion.smd { frames 1 3\n{ event 5004 1 \"middle\" }\n{ event 5004 2 \"end\" }\n}\n$sequence clamped motion.smd { frame -3 100\n{ event 5004 2 \"middle\" }\n}",
            &dir,
        );
        assert_eq!(desc.sequences[0].frames, Some([1, 3]));
        assert_eq!(desc.sequences[0].events[0].cycle, 0.5);
        assert_eq!(desc.sequences[0].events[1].cycle, 1.0);
        assert_eq!(desc.sequences[1].events[0].cycle, 0.5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn write_multi_part_source(dir: &std::path::Path) {
        let mut bytes = Vec::new();
        let mut views = Vec::new();
        let mut accessors = Vec::new();
        let mut accessor = |data: Vec<u8>, component: u32, count: usize, kind: &str| {
            let offset = bytes.len();
            bytes.extend_from_slice(&data);
            views.push(serde_json::json!({"buffer":0,"byteOffset":offset,"byteLength":data.len()}));
            let index = accessors.len();
            accessors.push(serde_json::json!({"bufferView":index,"componentType":component,"count":count,"type":kind}));
            index
        };
        let floats = |data: &[f32]| data.iter().flat_map(|value| value.to_le_bytes()).collect();
        let positions = accessor(
            floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
            5126,
            3,
            "VEC3",
        );
        let normals = accessor(
            floats(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
            5126,
            3,
            "VEC3",
        );
        let weights = accessor(
            floats(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]),
            5126,
            3,
            "VEC4",
        );
        let joints = accessor(vec![0; 24], 5123, 3, "VEC4");
        let inverse_binds: Vec<_> = (0..4)
            .map(|index| {
                let matrix = [
                    1.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                    0.0,
                    -(index as f32) * 10.0,
                    0.0,
                    0.0,
                    1.0,
                ];
                accessor(floats(&matrix), 5126, 1, "MAT4")
            })
            .collect();
        let mut nodes = vec![serde_json::json!({"name":"root","children":[1,2,3,4]})];
        for index in 0..4 {
            nodes.push(
                serde_json::json!({"name":format!("bone{index}"),"translation":[index * 10,0,0]}),
            );
        }
        for index in 0..4 {
            nodes
                .push(serde_json::json!({"name":format!("part{index}"),"mesh":index,"skin":index}));
        }
        let meshes: Vec<_> = (0..4).map(|index| serde_json::json!({"primitives":[{
            "attributes":{"POSITION":positions,"NORMAL":normals,"JOINTS_0":joints,"WEIGHTS_0":weights},"material":index
        }]})).collect();
        let materials: Vec<_> = (0..4)
            .map(|index| serde_json::json!({"name":format!("material{index}")}))
            .collect();
        let skins: Vec<_> = (0..4)
            .map(|index| serde_json::json!({"joints":[index+1],"inverseBindMatrices":inverse_binds[index]}))
            .collect();
        let document = serde_json::json!({"asset":{"version":"2.0"},
            "buffers":[{"uri":"parts.bin","byteLength":bytes.len()}],"bufferViews":views,"accessors":accessors,
            "materials":materials,"meshes":meshes,"skins":skins,"nodes":nodes,
            "scenes":[{"nodes":[0,5,6,7,8]}],"scene":0});
        std::fs::write(dir.join("parts.bin"), bytes).unwrap();
        std::fs::write(
            dir.join("parts.gltf"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn repeated_sources_discover_all_selected_bodygroup_bones_and_materials() {
        let dir = fixture("bodygroup_source_discovery");
        write_multi_part_source(&dir);
        for (scale, unfiltered) in [(1, false), (2, false), (1, true)] {
            let main = if unfiltered { "" } else { "srcpart @node:5" };
            let text = format!(
                "$body body parts.gltf {main}\n$bodygroup variants {{\n studio parts.gltf srcpart @node:6 srcscale {scale} srcname hat\n studio parts.gltf srcpart @node:7 srcname shoes\n blank\n}}\n"
            );
            let desc = parse(&text, &dir);
            for index in 0..3 {
                assert!(
                    desc.bones
                        .iter()
                        .any(|bone| bone.name == format!("bone{index}")),
                    "{scale} {unfiltered}"
                );
                assert!(
                    desc.materials
                        .textures
                        .iter()
                        .any(|texture| texture.name == format!("material{index}")),
                    "{scale} {unfiltered}"
                );
            }
            assert_eq!(
                desc.bones.iter().any(|bone| bone.name == "bone3"),
                unfiltered
            );
            assert_eq!(
                desc.materials
                    .textures
                    .iter()
                    .any(|texture| texture.name == "material3"),
                unfiltered
            );
            let compiled = crate::compile::compile(&desc, &dir).unwrap();
            assert_eq!(compiled.bodyparts[1].models[0].name, "hat");
            assert_eq!(compiled.bodyparts[1].models[1].name, "shoes");
            for (choice, bone_name, material_name) in
                [(0, "bone1", "material1"), (1, "bone2", "material2")]
            {
                let bone = compiled
                    .desc
                    .bones
                    .iter()
                    .position(|bone| bone.name == bone_name)
                    .unwrap();
                let model = &compiled.bodyparts[1].models[choice];
                assert_eq!(model.meshes.len(), 1);
                assert_eq!(
                    compiled.desc.materials.textures[model.meshes[0].material].name,
                    material_name
                );
                assert!(
                    model.meshes[0]
                        .vertices
                        .iter()
                        .all(|vertex| vertex.bones == [[bone as f32, 1.0]])
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repeated_source_discovery_retains_incompatible_fallback_materials_and_first_bind_pose() {
        let dir = fixture("bodygroup_source_fallbacks");
        write_multi_part_source(&dir);
        let path = dir.join("parts.gltf");
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for mesh in document["meshes"].as_array_mut().unwrap() {
            mesh["primitives"][0]
                .as_object_mut()
                .unwrap()
                .remove("material");
        }
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let desc = parse(
            "$body first parts.gltf srcpart @node:6 srcscale 2 srcmaterial first\n$body second parts.gltf srcpart @node:6 srcscale 3 srcmaterial second\n",
            &dir,
        );
        assert_eq!(
            desc.materials
                .textures
                .iter()
                .map(|texture| texture.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let bone = desc
            .bones
            .iter()
            .position(|bone| bone.name == "bone1")
            .unwrap();
        assert_eq!(desc.bones[bone].position, None);
        let compiled = crate::compile::compile(&desc, &dir).unwrap();
        assert_eq!(
            crate::compile::resolve_bone_pose(&desc, &compiled, bone).0,
            [20.0, 0.0, 0.0]
        );
        let second_pose = compiled.bodyparts[1].models[0]
            .poses
            .iter()
            .find(|pose| pose.bone == bone as i32)
            .unwrap();
        assert_eq!(second_pose.position, [30.0, 0.0, 0.0]);
        assert_eq!(compiled.bodyparts[0].models[0].meshes[0].material, 0);
        assert_eq!(compiled.bodyparts[1].models[0].meshes[0].material, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn event_cycles_follow_processed_frame_counts_and_warn_for_out_of_range_frames() {
        let dir = fixture("event_output_frames");
        write_five_frame_animation(&dir);
        for (options, count, frame, cycle) in [
            ("compress 2", 3, 1, 0.5),
            ("numframes 3 compress 2", 2, 1, 1.0),
            ("compress 2 numframes 5", 5, 2, 0.5),
            ("fudgeloop", 6, 5, 1.0),
            ("noanimation", 1, 0, 0.0),
        ] {
            let text = format!(
                "$body body a.smd\n$animation shared motion.smd {options}\n$sequence action shared {{\n event 5004 {frame} \"sound\"\n}}\n"
            );
            let desc = parse(&text, &dir);
            let compiled = crate::compile::compile(&desc, &dir).unwrap();
            assert_eq!(compiled.sequences[0].frames.len(), count, "{options}");
            assert_eq!(compiled.sequences[0].events[0].cycle, cycle, "{options}");
        }
        for frame in [-1, 5] {
            let text = format!(
                "$body body a.smd\n$sequence action motion.smd {{\n event 5004 {frame} \"sound\"\n}}\n"
            );
            let (result, warnings) =
                crate::diag::capture_warnings(|| crate::qc::parse_qc_str(&text, &dir));
            let desc = result.unwrap();
            assert_eq!(desc.sequences[0].events[0].cycle, 0.0);
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("Event 5004") && warning.contains("action"))
            );
            let output = crate::pipeline::build(&desc, &dir, Default::default()).unwrap();
            let sequence = i32::from_le_bytes(output.mdl[0xC0..0xC4].try_into().unwrap()) as usize;
            let event = sequence
                + i32::from_le_bytes(output.mdl[sequence + 28..sequence + 32].try_into().unwrap())
                    as usize;
            assert_eq!(
                f32::from_le_bytes(output.mdl[event..event + 4].try_into().unwrap()),
                0.0
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn named_animation_range_is_shared_and_normalizes_earlier_events() {
        let dir = fixture("shared_sequence_range");
        write_five_frame_animation(&dir);
        let desc = parse(
            "$body body a.smd\n$animation motion motion.smd frames 0 4\n$sequence earlier motion {\n{ event 5004 1 \"middle\" }\n}\n$sequence later motion frames 1 3\n",
            &dir,
        );
        assert_eq!(desc.animations[0].frames, Some([1, 3]));
        assert!(desc.sequences.iter().all(|seq| seq.frames.is_none()));
        assert_eq!(desc.sequences[0].events[0].cycle, 0.5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blend_source_options_modify_only_the_first_shared_animation() {
        let dir = fixture("blend_sequence_source_options");
        write_five_frame_animation(&dir);
        let desc = parse(
            "$body body a.smd\n$animation first motion.smd scale 1 frames 0 4\n$animation second motion.smd scale 1 frames 0 4\n$sequence blended { first second blendwidth 2 scale 2 frames 1 3 }\n",
            &dir,
        );
        assert_eq!(desc.animations[0].frames, Some([1, 3]));
        assert_eq!(desc.animations[0].scale, Some(2.0));
        assert_eq!(desc.animations[1].frames, Some([0, 4]));
        assert_eq!(desc.animations[1].scale, Some(1.0));
        assert_eq!(desc.sequences[0].blends, ["first", "second"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn source_overrides_follow_interleaved_continue_order() {
        let dir = fixture("sequence_source_override_order");
        let desc = parse(
            "$body body a.smd\n$animation shared a.smd scale 1 frames 0 0\n$sequence first shared scale 2 frame 0 2\n$sequence second shared scale 3 frame 0 3\n$continue first scale 4 frame 0 4\n$continue shared scale 5 frames 0 5\n$continue second frames 0 6\n",
            &dir,
        );
        assert_eq!(desc.animations[0].scale, Some(5.0));
        assert_eq!(desc.animations[0].frames, Some([0, 6]));
        assert!(
            desc.sequences
                .iter()
                .all(|seq| seq.scale.is_none() && seq.frames.is_none())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn global_scale_is_captured_for_implicit_sources_without_overriding_named_sources() {
        let dir = fixture("sequence_default_source_scale");
        let desc = parse(
            "$body body a.smd\n$scale 2\n$animation named a.smd\n$scale 3\n$sequence reused named\n$sequence implicit a.smd\n$sequence explicit named scale 4\n",
            &dir,
        );
        assert_eq!(desc.animations[0].scale, Some(4.0));
        assert!(
            desc.sequences
                .iter()
                .all(|seq| seq.input_scale == Some(3.0))
        );
        assert!(desc.sequences.iter().all(|seq| seq.scale.is_none()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lod_replacements_match_all_bodyparts_and_preserve_other_models() {
        let dir = fixture("lod_multiple_sources");
        let desc = parse(
            "$body first a.smd\n$body second b.smd\n$body third c.smd\n$lod 15 {\nreplacemodel A.SMD b.smd\nreplacemodel B c.smd\nnofacial\n}\n$lod 30 { replacemodel a c.smd }",
            &dir,
        );
        let models: Vec<_> = desc.bodyparts.iter().map(|part| &part.models[0]).collect();
        assert!(models.iter().all(|model| model.lods.len() == 2));
        assert!(models.iter().all(|model| model.lods[0].no_facial));
        assert_eq!(models[0].lods[0].smd.as_deref(), Some("b.smd"));
        assert_eq!(models[1].lods[0].smd.as_deref(), Some("c.smd"));
        assert_eq!(models[2].lods[0].smd, None);
        assert_eq!(models[0].lods[1].smd.as_deref(), Some("c.smd"));
        assert_eq!(models[1].lods[1].smd, None);
        assert_eq!(models[2].lods[1].smd, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lod_before_models_matches_windows_source_path_without_extension() {
        let dir = fixture("lod_before_models");
        std::fs::create_dir_all(dir.join("meshes")).unwrap();
        std::fs::write(dir.join("meshes/a.smd"), MIN_SMD).unwrap();
        let desc = parse(
            "$lod 20 { replacemodel \"MESHES\\A\" b.smd }\n$body first meshes/a.smd\n$body second c.smd\n",
            &dir,
        );
        assert_eq!(
            desc.bodyparts[0].models[0].lods[0].smd.as_deref(),
            Some("b.smd")
        );
        assert_eq!(desc.bodyparts[1].models[0].lods[0].smd, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lod_rejects_unmatched_replacement_source() {
        let dir = fixture("lod_unknown_source");
        let errors = crate::qc::parse_qc_str(
            "$body body a.smd\n$lod 20 { replacemodel missing c.smd }",
            &dir,
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("does not match any bodypart"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blank_bodygroup_members_keep_their_positions() {
        let dir = fixture("blank_bodygroup");
        let desc = parse(
            "$bodygroup accessories {\nblank\nstudio a.smd\nblank\nstudio b.smd\n}",
            &dir,
        );
        let models = &desc.bodyparts[0].models;
        assert_eq!(models.len(), 4);
        for index in [0, 2] {
            assert!(models[index].smd.is_empty());
            assert_eq!(models[index].name.as_deref(), Some("blank"));
        }
        assert_eq!(models[1].smd, "a.smd");
        assert_eq!(models[3].smd, "b.smd");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn collision_block_next_line_reads_skip_and_surface_props() {
        let dir = fixture("collision_next_line");
        let desc = parse(
            "$body body a.smd\n$collisionjoints b.smd\n{\n$mass 42\n$jointskip bone1\n$jointsurfaceprop root metal\n$jointdamping root 0.25\n}\n$surfaceprop wood\n",
            &dir,
        );
        assert_eq!(desc.physics.mass, Some(42.0));
        assert_eq!(desc.physics.skip, vec!["bone1"]);
        assert_eq!(desc.physics.joint_overrides[0].damping, Some(0.25));
        assert_eq!(desc.model.surface_prop.as_deref(), Some("wood"));
        assert_eq!(
            desc.bones
                .iter()
                .find(|bone| bone.name == "root")
                .unwrap()
                .surface_prop
                .as_deref(),
            Some("metal")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn collision_without_block_keeps_next_command() {
        let dir = fixture("collision_without_block");
        let desc = parse(
            "$body body a.smd\n$collisionmodel b.smd\n$surfaceprop metal\n",
            &dir,
        );
        assert_eq!(desc.model.surface_prop.as_deref(), Some("metal"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn windows_include_and_pushd_paths_work_on_native_platforms() {
        let dir = fixture("windows_paths");
        std::fs::create_dir_all(dir.join("includes")).unwrap();
        std::fs::create_dir_all(dir.join("meshes/nested")).unwrap();
        std::fs::write(dir.join("includes/props.qci"), "$surfaceprop metal\n").unwrap();
        std::fs::write(dir.join("meshes/nested/a.smd"), MIN_SMD).unwrap();
        let desc = parse(
            "$include \"includes\\props.qci\"\n$pushd \"meshes\\nested\"\n$body body a.smd\n$popd\n",
            &dir,
        );
        assert_eq!(desc.model.surface_prop.as_deref(), Some("metal"));
        assert_eq!(desc.bodyparts[0].models[0].smd, "meshes/nested/a.smd");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scales_capture_declaration_order_and_per_studio_override() {
        let dir = fixture("scale_capture");
        let desc = parse(
            "$scale 2\n$body first a.smd scale 3\n$scale 0.5\n$body second b.smd\n$animation walk a.smd\n$sequence idle c.smd\n$collisionmodel b.smd\n",
            &dir,
        );
        assert_eq!(desc.bodyparts[0].models[0].input_scale, Some(3.0));
        assert_eq!(desc.bodyparts[1].models[0].input_scale, Some(0.5));
        assert_eq!(desc.animations[0].scale, Some(0.5));
        assert_eq!(desc.sequences[0].input_scale, Some(0.5));
        assert_eq!(desc.sequences[0].scale, None);
        assert_eq!(desc.physics.input_scale, Some(0.5));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn material_rename_preserves_source_texture_registration() {
        let dir = fixture("rename_material");
        let desc = parse("$body body a.smd\n$renamematerial mat replacement\n", &dir);
        assert_eq!(
            desc.model.material_renames,
            vec![["mat".to_string(), "replacement".to_string()]]
        );
        assert_eq!(desc.materials.textures[0].name, "mat");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unsupported_collision_flags_are_errors() {
        let errors = crate::qc::parse_qc_str(
            "$collisionmodel a.smd { $snapcollisionjoints }",
            std::path::Path::new("."),
        )
        .unwrap_err();
        assert!(
            errors[0].message.contains("not supported yet"),
            "{errors:?}"
        );
    }

    #[test]
    fn malformed_global_transforms_are_rejected() {
        for qc in ["$origin nan 2 3", "$origin 1 2 3 inf", "$upaxis q"] {
            assert!(crate::qc::parse_qc_str(qc, std::path::Path::new(".")).is_err());
        }
    }

    #[test]
    fn unterminated_collision_and_lod_blocks_are_errors() {
        for qc in [
            "$collisionmodel a.smd {\n$mass 5\n",
            "$lod 20 {\nnofacial\n",
        ] {
            let errors = crate::qc::parse_qc_str(qc, std::path::Path::new(".")).unwrap_err();
            assert!(errors[0].message.contains("unterminated"), "{errors:?}");
        }
    }

    #[test]
    fn shadow_lod_is_a_final_special_level_with_material_and_removal_options() {
        let desc = parse(
            "$body body a.smd\n$lod 25 { replacematerial material low }\n$shadowlod { removemodel a.smd use_shadowlod_materials nofacial }",
            &fixture("shadow_lod"),
        );
        let lods = &desc.bodyparts[0].models[0].lods;
        assert_eq!(lods[0].switch_point, Some(25.0));
        assert_eq!(lods[0].material_replacements, [["material", "low"]]);
        assert_eq!(lods[1].switch_point, Some(-1.0));
        assert!(lods[1].removed && lods[1].no_facial);
        assert_eq!(desc.model.extra_flags.unwrap_or(0) & 0x140, 0x140);
        for text in [
            "$lod -1 { nofacial }",
            "$shadowlod {}\n$lod 5 {}",
            "$shadowlod {}\n$shadowlod {}",
        ] {
            assert!(crate::qc::parse_qc_str(text, std::path::Path::new(".")).is_err());
        }
    }

    #[test]
    fn extensionless_sequence_source_is_loaded_and_named_animation_wins() {
        let dir = fixture("extensionless_sequence");
        let desc = parse(
            "$body body a\n$animation b a\n$sequence explicit b\n$sequence implicit c\n",
            &dir,
        );
        assert_eq!(desc.sequences[0].smd, "b");
        assert_eq!(desc.sequences[1].smd, "c");
        assert!(
            desc.sequences
                .iter()
                .all(|sequence| sequence.blends.is_empty())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// ⚠️ **回归**：`ACT_*` 必须吃掉紧跟其后的**权重** token。
    ///
    /// 官方 `Option_Activity`（`studiomdl.cpp:1165-1178`）读**两个** token
    /// （名 + 权重）。修复前本实现只认名字、把权重留在流里，于是
    /// `$sequence "x" "a_idle" "ACT_VM_IDLE" 1` 里的 `1` 漏进 `blends`，
    /// 编译时报「找不到动画 "1"」。
    ///
    /// 真实影响：`nahida_themed_autoshotgun` 的 39 处错误里 **32 处**是这一条。
    #[test]
    fn act_token_consumes_its_weight() {
        let dir = fixture("act_weight");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$sequence \"seq_idle\" \"a_idle\" \"ACT_VM_IDLE\" 1
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let s = &d.sequences[0];
        assert_eq!(s.activity.as_deref(), Some("ACT_VM_IDLE"));
        assert_eq!(s.activity_weight, 1, "权重必须被读进来");
        assert!(
            s.blends.is_empty(),
            "单动画序列不该有 blends，实际：{:?}（权重漏进去了）",
            s.blends
        );
    }

    /// `ACT_*` **不带权重**时也必须能解析（不能把下一个 token 硬吃掉）。
    ///
    /// 官方会 `verify_atoi` 下一个 token —— 若那是另一条 `$sequence` 的
    /// 名字就会出错。本实现的做法是「不是数字就退回流」，这条测试钉住它。
    #[test]
    fn act_token_without_weight_does_not_eat_next_token() {
        let dir = fixture("act_noweight");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$sequence \"seq_a\" \"a_idle\" \"ACT_VM_IDLE\"
$sequence \"seq_b\" \"a_idle\" \"ACT_VM_DRAW\" 1
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.sequences.len(), 2, "两条序列都必须被解析出来");
        assert_eq!(d.sequences[0].activity.as_deref(), Some("ACT_VM_IDLE"));
        assert_eq!(d.sequences[0].activity_weight, 0);
        assert_eq!(d.sequences[1].activity.as_deref(), Some("ACT_VM_DRAW"));
        assert_eq!(d.sequences[1].activity_weight, 1);
    }

    /// ⚠️ **回归**：`$sequence` 块里的 `subtract "<动画>" <帧>` 是**命令**，
    /// 不是动画名。
    ///
    /// 官方走 `ParseAnimationToken` → `ParseCmdlistToken`
    /// （`studiomdl.cpp:1733-1751`，`CMD_SUBTRACT`）。
    /// 修复前把 `subtract` 本身压进 `blends`，于是
    /// `"al_deploy" subtract "a_idle" 0` 被当成 5 格 blend 网格，
    /// 报「blend 格数 5 不是完全平方数」。
    #[test]
    fn sequence_subtract_is_a_command_not_an_animation_name() {
        let dir = fixture("seq_subtract");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$animation \"al_deploy\" \"b.smd\" fps 30
$sequence \"deploy_layer\" \"al_deploy\" snap fadeout 0.2 subtract \"a_idle\" 0 delta \"ACT_VM_DEPLOY_LAYER\" 1
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let s = &d.sequences[0];
        assert_eq!(
            d.animations[1].subtract.as_deref(),
            Some("a_idle"),
            "subtract 应记为参考动画名"
        );
        assert_eq!(d.animations[1].subtract_frame, Some(0));
        assert!(
            s.blends.is_empty(),
            "单动画序列不该有 blends，实际：{:?}",
            s.blends
        );
        assert_eq!(s.activity.as_deref(), Some("ACT_VM_DEPLOY_LAYER"));
        assert_eq!(s.activity_weight, 1);
    }

    /// ⚠️ **回归**：`$sequence` 的 `numframes <N>` 必须被吃掉。
    ///
    /// 官方 `ParseCmdlistToken` 的 `CMD_NUMFRAMES`
    /// （`studiomdl.cpp:2104-2111`）。修复前 `numframes` 与 `90` 都漏进
    /// `blends`，报「找不到动画 "numframes"」。
    #[test]
    fn sequence_numframes_is_consumed() {
        let dir = fixture("seq_numframes");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_look_mid\" \"a.smd\" fps 30
$sequence \"fidget\" \"a_look_mid\" \"ACT_VM_FIDGET\" 100 numframes 90 fps 1
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let s = &d.sequences[0];
        assert!(matches!(
            d.animations[0].cmds.last(),
            Some(crate::model::AnimCmd::NumFrames { frames: 90 })
        ));
        assert_eq!(s.activity.as_deref(), Some("ACT_VM_FIDGET"));
        assert_eq!(s.activity_weight, 100);
        assert!(s.blends.is_empty(), "不该有 blends，实际：{:?}", s.blends);
    }

    /// `blendwidth` + `blend` 的**正常**用法不能被上面的修复破坏。
    ///
    /// 这是真实 `look_poses` 的写法：3 格 + `blendwidth 3` ⟹ 1×3 网格。
    #[test]
    fn blend_grid_still_parses() {
        let dir = fixture("blend_grid");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_look_down\" \"a.smd\" fps 30
$animation \"a_look_mid\" \"b.smd\" fps 30
$animation \"a_look_up\" \"c.smd\" fps 30
$poseparameter \"ver_aims\" -1 1 loop 0
$sequence \"look_poses\" \"a_look_down\" \"a_look_mid\" \"a_look_up\" hidden {
	delta
	blend \"ver_aims\" 1 -1
	blendwidth 3
}
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let s = &d.sequences[0];
        assert_eq!(
            s.blends,
            vec!["a_look_down", "a_look_mid", "a_look_up"],
            "blend 网格的三格必须原样保留"
        );
        assert_eq!(s.blend_width, Some(3));
        assert_eq!(s.blend_params.len(), 1);
        assert_eq!(s.blend_params[0].parameter, "ver_aims");
    }

    /// 多条序列混用上述四种写法 —— 整体不串位。
    ///
    /// 这是真实工程的最小复现：修复前这种文件会产出几十处错误。
    #[test]
    fn mixed_sequence_forms_do_not_bleed_into_each_other() {
        let dir = fixture("mixed");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$animation \"al_reload\" \"b.smd\" fps 30
$sequence \"reload\" \"a_idle\" hidden fadein 0.1 fadeout 0 \"ACT_VM_RELOAD\" 1
$sequence \"reload_layer\" \"al_reload\" fadein 0.1 fadeout 0 addlayer \"reload\" \"ACT_VM_RELOAD_LAYER\" 1
$sequence \"reload_end\" \"al_reload\" subtract \"a_idle\" 0 delta \"ACT_VM_RELOAD_END\" 1
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.sequences.len(), 3);
        // 三条都必须是「单动画序列」—— 一条都不该变成 blend 网格。
        for s in &d.sequences {
            assert!(
                s.blends.is_empty(),
                "序列 {:?} 不该有 blends，实际：{:?}",
                s.name,
                s.blends
            );
            assert!(
                s.activity_weight == 1,
                "序列 {:?} 的 actweight 应为 1，实际 {}",
                s.name,
                s.activity_weight
            );
        }
        assert_eq!(d.animations[1].subtract.as_deref(), Some("a_idle"));
    }

    /// ⚠️ **回归**：`$attachment ... rotate` 的三个数是 **`QAngle{pitch, yaw, roll}`**，
    /// 必须重排成 `RadianEuler{roll, pitch, yaw}` 再存。
    ///
    /// 官方 `Cmd_Attachment`（`studiomdl.cpp:5260-5272`）把三个 token 读进
    /// `QAngle angles`（成员顺序 `x=pitch, y=yaw, z=roll`），再交给
    /// `AngleMatrix(QAngle)`（`mathlib_base.cpp:2837`，按 `angles[PITCH/YAW/ROLL]` 组装）。
    /// 而本实现 `Attachment.rotation` 的约定是 `RadianEuler{roll, pitch, yaw}`
    /// （写出器 `to_radians()` 后喂 `bone_math::angle_matrix`）。
    ///
    /// 修复前把三个数**原样**存 ⟹ 附着点朝向错 **90°**。
    ///
    /// # 为什么 parity 测不出来
    ///
    /// 真实语料里 `$attachment` 带 `rotate` 的有 **315 处**，但 parity 唯一
    /// 用到它的夹具（`docs/_probe/smdl/myprop.qc`）写的是 `rotate 0 0 0` ——
    /// 恒等旋转，两种口径结果相同。
    ///
    /// 真 studiomdl 裁决：`docs/_probe/oracle_attachment_rotate.js`
    /// （`rotate 0 0 -90` / `90 0 0` / `0 90 0` 三个受控夹具，**3/3** 与
    /// QAngle 口径吻合到 `4.37e-8`，与 RadianEuler 口径差 **1.0**）。
    ///
    /// # 真实影响
    ///
    /// 用户的 `v_autoshotgun.qc` 第 19 行：
    /// `$attachment "attach_camera" "ValveBiped.attach_camera" 0 0 0 rotate 0 0 -90`
    /// —— 引擎用 `attach_camera` 定位第一人称相机 ⟹ 朝向错 90°
    /// ⟹ **游戏内相机被顺时针旋转 90°**（用户实测截图）。
    #[test]
    fn attachment_rotate_is_reordered_from_qangle() {
        let dir = fixture("attach_rotate");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$attachment \"cam\" \"root\" 0 0 0 rotate 0 0 -90
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let at = &d.attachments[0];
        assert_eq!(at.name, "cam");
        assert_eq!(
            at.rotation,
            Some([-90.0, 0.0, 0.0]),
            "QC 的 `rotate 0 0 -90`（QAngle{{pitch,yaw,roll}}）必须重排成 \
             RadianEuler{{roll,pitch,yaw}} = `[-90, 0, 0]`；\
             若得到 `[0, 0, -90]` 说明没重排（朝向会错 90°）"
        );
    }

    /// `$attachment ... rotate` 的三个分量**各自**都要落到正确位置。
    ///
    /// 上面那条只用了 `0 0 -90`（只有一个分量非零）—— 把 `[p,y,r]` 重排成
    /// `[r,p,y]` 时，若误写成 `[y,r,p]` 之类，单分量夹具**照样通过**。
    /// 这条用**三个互不相同**的值把位置钉死。
    #[test]
    fn attachment_rotate_places_each_component() {
        let dir = fixture("attach_rotate3");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$attachment \"cam\" \"root\" 0 0 0 rotate 11 22 33
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        // `QAngle{pitch=11, yaw=22, roll=33}` → `RadianEuler{roll=33, pitch=11, yaw=22}`
        assert_eq!(
            d.attachments[0].rotation,
            Some([33.0, 11.0, 22.0]),
            "`rotate 11 22 33` 应重排成 `[roll=33, pitch=11, yaw=22]`"
        );
    }

    /// ⭐ `absolute`/`rigid` **不能**落进 `flags` —— 它们走官方另一个字段 `type`。
    ///
    /// 官方 `Cmd_Attachment`（`studiomdl.cpp:5212-5310`）把三个选项写进**两个
    /// 不同字段**：`absolute` → `type |= IS_ABSOLUTE`（`:5245`）、`rigid` →
    /// `type |= IS_RIGID`（`:5251`）、`world_align` → `flags |= ATTACHMENT_FLAG_WORLD_ALIGN`
    /// （`:5255`）。而 `write.cpp:350` 只写 `pattachment[i].flags = g_attachment[i].flags;`
    /// ⟹ **产物里只可能看到 `world_align`**。
    ///
    /// 真 studiomdl 裁决（`docs/_probe/oracle_attachment_flags.js`，5 变体）：
    /// ```text
    ///   plain          att=0x0        absolute       att=0x0
    ///   rigid          att=0x0        world_align    att=0x10000
    ///   abs_rigid_wa   att=0x10000
    /// ```
    /// 修复前 mdlc 写的是 `absolute→0x1` / `rigid→0x2` / `world_align→0x4`
    /// —— 三个全错，且 `world_align` 的**值**也错（`0x4` 不是 `0x10000`）。
    #[test]
    fn attachment_options_do_not_leak_into_flags() {
        let dir = fixture("attach_flags");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$attachment \"plain\" \"root\" 0 0 0
$attachment \"abs\" \"root\" 0 0 0 absolute
$attachment \"rig\" \"root\" 0 0 0 rigid
$attachment \"wa\" \"root\" 0 0 0 world_align
$attachment \"all\" \"root\" 0 0 0 absolute rigid world_align
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let f = |n: &str| {
            d.attachments
                .iter()
                .find(|a| a.name == n)
                .unwrap_or_else(|| panic!("夹具里应有附着点 {n}"))
                .flags
                .unwrap_or(0)
        };
        assert_eq!(f("plain"), 0x0, "无选项 ⟹ flags 为 0");
        assert_eq!(f("abs"), 0x0, "`absolute` 走 `type`，**不落盘**");
        assert_eq!(f("rig"), 0x0, "`rigid` 走 `type`，**不落盘**");
        assert_eq!(
            f("wa"),
            0x10000,
            "`world_align` ⟹ ATTACHMENT_FLAG_WORLD_ALIGN"
        );
        assert_eq!(
            f("all"),
            0x10000,
            "三个选项同时给 ⟹ 只有 `world_align` 那一位"
        );
    }

    /// `absolute` / `rigid` 必须落到 IR 自己的字段上（`flags` 里没有它们）。
    ///
    /// 收骨判据要用 `rigid`（官方 `TagUsedBones()` 对 rigid 附着点沿父链上溯到
    /// 第一根被顶点引用的骨骼，`simplify.cpp:3472-3484`）；`absolute` 决定
    /// `local` 是否被 `poseToBone` 左乘（`simplify.cpp:5379-5388`）。
    /// 两者都不落盘 ⟹ 若只存在 `flags` 里，收骨判据与写出器就都读不到了。
    #[test]
    fn attachment_absolute_and_rigid_land_in_ir_fields() {
        let dir = fixture("attach_irflags");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$attachment \"a\" \"root\" 0 0 0 absolute
$attachment \"r\" \"root\" 0 0 0 rigid
$attachment \"plain\" \"root\" 0 0 0
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let g = |n: &str| {
            d.attachments
                .iter()
                .find(|a| a.name == n)
                .unwrap_or_else(|| panic!("夹具里应有附着点 {n}"))
        };
        assert!(g("a").absolute, "`absolute` ⟹ `Attachment::absolute`");
        assert!(!g("a").rigid);
        assert!(g("r").rigid, "`rigid` ⟹ `Attachment::rigid`");
        assert!(!g("r").absolute);
        assert!(!g("plain").absolute && !g("plain").rigid);
    }

    /// ⭐ `absolute` 与 `rotate` 的**顺序**决定 `local` 的旋转来自谁。
    ///
    /// 官方在选项循环里**顺序覆盖同一个 `local` 矩阵**：`absolute` 写
    /// `AngleIMatrix( g_defaultrotation )`（`studiomdl.cpp:5246`），`rotate` 写
    /// `AngleMatrix( angles )`（`:5268`）—— 后写者胜。**两种情形的
    /// `IS_ABSOLUTE` 都置位**（第 ② 步的左乘照做），只有旋转来源不同。
    ///
    /// 官方用一个矩阵自然表达了这点；mdlc 把旋转拆成了欧拉角，所以需要
    /// [`crate::model::Attachment::absolute_rotation`] 这一个额外的 bit。
    #[test]
    fn attachment_absolute_rotation_tracks_option_order() {
        let dir = fixture("attach_absorder");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$attachment \"abs_only\" \"root\" 0 0 0 absolute
$attachment \"abs_then_rot\" \"root\" 0 0 0 absolute rotate 11 22 33
$attachment \"rot_then_abs\" \"root\" 0 0 0 rotate 11 22 33 absolute
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let g = |n: &str| {
            d.attachments
                .iter()
                .find(|a| a.name == n)
                .unwrap_or_else(|| panic!("夹具里应有附着点 {n}"))
        };
        assert_eq!(
            g("abs_only").absolute_rotation,
            Some(true),
            "只写 `absolute` ⟹ 旋转来自 `AngleIMatrix( g_defaultrotation )`"
        );
        assert_eq!(
            g("abs_then_rot").absolute_rotation,
            Some(false),
            "`absolute rotate …` ⟹ `rotate` 后写，旋转来自 `AngleMatrix( angles )`"
        );
        assert_eq!(
            g("rot_then_abs").absolute_rotation,
            Some(true),
            "`rotate … absolute` ⟹ `absolute` 后写，旋转来自 `AngleIMatrix`"
        );
        // 三种情形的 `absolute` 本身都置位（都要做 `poseToBone` 左乘）。
        for n in ["abs_only", "abs_then_rot", "rot_then_abs"] {
            assert!(g(n).absolute, "{n} 的 `IS_ABSOLUTE` 应置位");
        }
        assert_eq!(
            g("abs_then_rot").rotation,
            Some([33.0, 11.0, 22.0]),
            "`rotate` 的值仍要按 QAngle→RadianEuler 重排存下来"
        );
    }

    /// **不写 `rotate` 时旋转必须是零** —— 这是「重排不波及绝大多数附着点」的依据。
    ///
    /// 语料里 315 处带 `rotate`，而 `$attachment` 总数远大于此 ⟹
    /// 绝大多数没有 `rotate`，必须保持 `[0,0,0]`。
    #[test]
    fn attachment_without_rotate_has_zero_rotation() {
        let dir = fixture("attach_norotate");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$attachment \"muzzle\" \"root\" 1 2 3
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let at = &d.attachments[0];
        assert_eq!(at.position, Some([1.0, 2.0, 3.0]));
        assert_eq!(
            at.rotation,
            Some([0.0, 0.0, 0.0]),
            "没写 rotate 时旋转应为零"
        );
    }

    // ---- `$jigglebone` ----
    //
    // 语义全部来自**受控实验 + 官方反编译**（见 `compile::resolve_jiggle_bones`
    // 的文档）：官方是「一个扁平记录 + token 顺序后写覆盖」，所以解析器
    // 只需保真顺序、不做单位换算（换算留给 resolve）。
    // 夹具 `docs/_probe/smdl/jig{1..50}.qc`，验收 `docs/_probe/cmp_jiggle.js`。

    /// 一段 `$jigglebone` QC 骨架，`{}` 处填块内容。
    fn jig_qc(blocks: &str) -> String {
        format!(
            "$modelname \"t.mdl\"\n$body body \"a.smd\"\n\
             $jigglebone \"tip\" {{\n{blocks}\n}}\n\
             $sequence \"idle\" \"a.smd\" fps 30\n"
        )
    }

    /// 解析后取第一条 jiggle 记录的写入日志（`(键, 值)` 序列）。
    fn jig_writes(qc: &str, dir: &std::path::Path) -> Vec<(String, Vec<f32>)> {
        let d = parse(qc, dir);
        d.jiggle_bones[0]
            .writes
            .iter()
            .map(|w| (w.key.clone(), w.values.clone()))
            .collect()
    }

    /// **回归**：`writes` 必须**保真 QC 里的 token 顺序**。
    ///
    /// 官方是扁平记录 + 后写覆盖，`jig47`/`jig48` 只有块顺序不同、
    /// 结果角度就不同 ⟹ 顺序一旦丢失，两个夹具不可能同时复现。
    #[test]
    fn jiggle_writes_keep_token_order() {
        let dir = fixture("jig_order");
        let qc = jig_qc(
            "\tis_rigid {\n\t\tyaw_constraint -10 20\n\t}\n\
             \thas_base_spring {\n\t\tyaw_constraint -30 40\n\t}",
        );
        let w = jig_writes(&qc, &dir);
        let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "is_rigid",
                "yaw_constraint",
                "has_base_spring",
                "yaw_constraint"
            ],
            "块名也要进日志（它负责置 flags），且顺序不得重排"
        );
        assert_eq!(w[1].1, vec![-10.0, 20.0], "值必须原样保留（不转弧度）");
        assert_eq!(w[3].1, vec![-30.0, 40.0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 反向块顺序也必须保真（`jig48` 与 `jig47` 只差这一处）。
    #[test]
    fn jiggle_writes_keep_reversed_block_order() {
        let dir = fixture("jig_order_rev");
        let qc = jig_qc(
            "\thas_base_spring {\n\t\tyaw_constraint -30 40\n\t}\n\
             \tis_rigid {\n\t\tyaw_constraint -10 20\n\t}",
        );
        let w = jig_writes(&qc, &dir);
        let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "has_base_spring",
                "yaw_constraint",
                "is_rigid",
                "yaw_constraint"
            ]
        );
        assert_eq!(w[1].1, vec![-30.0, 40.0]);
        assert_eq!(w[3].1, vec![-10.0, 20.0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **回归**：官方 `is_rigid` **内联**接受全套 yaw/pitch 键（实测 `jig43`）。
    ///
    /// 修复前 `set_jiggle_scalar` 的 `is_rigid` 分支只有 `length`/`tip_mass`，
    /// 这 6 个键**静默丢弃** —— 用户工程 `v_silenced_smg.qc` 的
    /// `$jigglebone "ValveBiped.strap"` 正是这种写法。
    #[test]
    fn jiggle_is_rigid_accepts_yaw_pitch_keys() {
        let dir = fixture("jig_rigid");
        let qc = jig_qc(
            "\tis_rigid {\n\t\tyaw_constraint -30 40\n\t\tyaw_friction 7\n\
             \t\tyaw_bounce 1\n\t\tpitch_constraint -20 50\n\
             \t\tpitch_friction 3\n\t\tpitch_bounce 8\n\t}",
        );
        let w = jig_writes(&qc, &dir);
        let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "is_rigid",
                "yaw_constraint",
                "yaw_friction",
                "yaw_bounce",
                "pitch_constraint",
                "pitch_friction",
                "pitch_bounce"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 官方 `has_base_spring` 的**裸键**（`stiffness`/`left_constraint`/…）。
    ///
    /// mdlc 的 TOML 侧叫 `base_stiffness`/`base_left`/…，但 **QC 侧必须
    /// 认官方裸键** —— 语料 `jigglebones.qci:40-48` 与 `jig2.qc` 都这么写。
    #[test]
    fn jiggle_has_base_spring_accepts_bare_keys() {
        let dir = fixture("jig_base");
        let qc = jig_qc(
            "\thas_base_spring {\n\t\tbase_mass 5\n\t\tstiffness 800\n\t\tdamping 10\n\
             \t\tleft_constraint -0.5 0.5\n\t\tup_constraint -0.75 2.0\n\
             \t\tforward_constraint -0.25 0.25\n\t\tleft_friction 10\n\
             \t\tup_friction 11\n\t\tforward_friction 12\n\t}",
        );
        let w = jig_writes(&qc, &dir);
        let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "has_base_spring",
                "base_mass",
                "stiffness",
                "damping",
                "left_constraint",
                "up_constraint",
                "forward_constraint",
                "left_friction",
                "up_friction",
                "forward_friction"
            ]
        );
        // `left_constraint` 的负值必须原样（**不是**角度、不转弧度）。
        assert_eq!(w[4].1, vec![-0.5, 0.5]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 三块共享的 9 个通用键：`is_flexible` 与 `has_base_spring` 都接受
    /// （官方 `FUN_004545b0` 被这两个块调用；实测 `jig17`/`jig18`/`jig41`）。
    #[test]
    fn jiggle_shared_keys_work_in_both_blocks() {
        let dir = fixture("jig_shared");
        for (tag, block) in [("flex", "is_flexible"), ("base", "has_base_spring")] {
            let qc = jig_qc(&format!(
                "\t{block} {{\n\t\tlength 20\n\t\ttip_mass 3\n\t\tangle_constraint 60\n\
                 \t\tyaw_constraint -30 40\n\t\tyaw_friction 2\n\t\tyaw_bounce 1\n\
                 \t\tpitch_constraint -20 50\n\t\tpitch_friction 3\n\t\tpitch_bounce 4\n\t}}"
            ));
            let w = jig_writes(&qc, &dir);
            let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(keys.len(), 10, "{tag}: 块名 + 9 个共享键");
            assert_eq!(keys[0], block);
            assert_eq!(keys[1], "length");
            assert_eq!(keys[9], "pitch_bounce");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **回归**：未知键必须**报错**，不得静默忽略。
    ///
    /// 官方是 `$jigglebone: invalid syntax '%s'` + `Aborted Processing`
    /// （实测 `jig9`–`jig16`、`jig19`、`jig20`、`jig45`、`jig46`）。
    /// 修复前是 `_ => {}` 静默跳过。
    #[test]
    fn jiggle_unknown_key_is_an_error() {
        let dir = fixture("jig_unknown");
        // `base_stiffness` 是 mdlc 的 TOML 键名，官方 QC 里**不存在**（jig9）。
        let qc = jig_qc("\tis_flexible {\n\t\tyaw_stiffness 100\n\t}");
        assert!(crate::qc::parse_qc_str(&qc, &dir).is_ok(), "合法键不应报错");
        let qc = jig_qc("\thas_base_spring {\n\t\tbase_stiffness 800\n\t}");
        let errs = crate::qc::parse_qc_str(&qc, &dir).expect_err("base_stiffness 官方拒绝");
        assert!(
            errs.iter().any(|e| e.message.contains("base_stiffness")),
            "报错应点名该键，实际：{errs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **回归**：`allow_length_flex` 是 `is_flexible` **独有**键。
    ///
    /// 实测 `jig45`（在 `is_rigid` 里）/ `jig46`（在 `has_base_spring` 里）
    /// 官方都 abort；`jig4`（在 `is_flexible` 里）成功。
    #[test]
    fn jiggle_allow_length_flex_is_flexible_only() {
        let dir = fixture("jig_alf");
        let qc = jig_qc("\tis_flexible {\n\t\tyaw_stiffness 100\n\t\tallow_length_flex\n\t}");
        assert!(
            crate::qc::parse_qc_str(&qc, &dir).is_ok(),
            "jig4 写法应成功"
        );
        for block in ["is_rigid", "has_base_spring"] {
            let qc = jig_qc(&format!("\t{block} {{\n\t\tallow_length_flex\n\t}}"));
            let errs = crate::qc::parse_qc_str(&qc, &dir).expect_err(&format!(
                "{block} 里的 allow_length_flex 官方拒绝（jig45/jig46）"
            ));
            assert!(
                errs.iter().any(|e| e.message.contains("allow_length_flex")),
                "报错应点名该键，实际：{errs:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **回归**：顶层裸键（不在任何块里）官方 abort。
    ///
    /// 实测 `jig5`/`jig31`：`tip_mass 7` 写在块外 ⟹
    /// `$jigglebone: invalid syntax 'tip_mass'`。
    /// 修复前 mdlc 静默接受（`jig5` 的产物 3352 B 正常落盘）。
    #[test]
    fn jiggle_bare_top_level_key_is_an_error() {
        let dir = fixture("jig_bare");
        let qc = jig_qc("\ttip_mass 7\n\tlength 20");
        let errs = crate::qc::parse_qc_str(&qc, &dir).expect_err("顶层裸键官方拒绝");
        assert!(
            errs.iter().any(|e| e.message.contains("tip_mass")),
            "报错应点名第一个键，实际：{errs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `is_flexible` 独有键在别的块里也被拒（`jig20`：`is_rigid{yaw_stiffness}`）。
    #[test]
    fn jiggle_flexible_only_keys_rejected_elsewhere() {
        let dir = fixture("jig_flexonly");
        let qc = jig_qc("\tis_rigid {\n\t\tyaw_stiffness 100\n\t}");
        let errs = crate::qc::parse_qc_str(&qc, &dir).expect_err("is_rigid 不接受 yaw_stiffness");
        assert!(
            errs.iter().any(|e| e.message.contains("yaw_stiffness")),
            "实际：{errs:?}"
        );
        // `has_base_spring` 独有键在 is_flexible 里同样被拒（`jig19`）。
        let qc = jig_qc("\tis_flexible {\n\t\tleft_constraint -0.5 0.5\n\t}");
        let errs =
            crate::qc::parse_qc_str(&qc, &dir).expect_err("is_flexible 不接受 left_constraint");
        assert!(
            errs.iter().any(|e| e.message.contains("left_constraint")),
            "实际：{errs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`$redefinevariable` 必须被拒（mdlc 的故意差异，但报错文案与官方一致）。**
    ///
    /// ⚠️ 它是 **NekoMDL 扩展**，官方**没有**这个命令：
    /// - 官方 exe 串扫描 `redefinevariable` **0 命中**（`docs/_probe/str_scan.js`）；
    /// - 真 studiomdl 裁决（`docs/_probe/oracle_redefinevariable.js`）报
    ///   `ERROR: main.qc(2): - bad command $redefinevariable`。
    ///
    /// 用户明确要求「不要支持 `$redefinevariable`，只需让 `$definevariable`
    /// 也能覆盖已经定义的变量即可」⟹ mdlc 也报「未知的 QC 命令」，
    /// 而它想表达的**覆盖**意图由 [`crate::qc::lexer::Lexer::define_variable`]
    /// 的覆盖语义满足。
    ///
    /// ⚠️ 反向判据：若有人把 `$redefinevariable` 加回命令表（历史形态是
    /// `"$definevariable" | "$redefinevariable" => { Ok(()) }`），这条测试
    /// 会立刻失败 —— 那种写法只吃掉命令名、**把值留在流里**，
    /// 于是值（如 `.922246`）会被当成下一条命令 ⟹
    /// `未知的 QC 命令 "scale"`（这正是 `incap_anim_fix` 最初的报错）。
    #[test]
    fn redefinevariable_is_rejected_like_official() {
        let dir = fixture("redefinevar");
        let qc = "\
$modelname \"t.mdl\"
$definevariable scale .922246
$redefinevariable scale .922246
$body body \"a.smd\"
";
        let errs = crate::qc::parse_qc_str(qc, &dir)
            .expect_err("`$redefinevariable` 是 NekoMDL 扩展，官方报 bad command ⟹ mdlc 也必须拒");
        assert!(
            errs.iter().any(|e| e.message.contains("redefinevariable")),
            "报错应点名 `$redefinevariable`，实际：{errs:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`$definevariable` 的覆盖语义要能被 QC 解析层看到。**
    ///
    /// 词法层测试（`qc::lexer::tests::definevariable_overrides_an_existing_variable`）
    /// 已钉住覆盖本身；这里钉的是**跨 `$include` 的场景**（用户工程
    /// `incap_anim_fix` 的真实形态：主 QC `:13` 先定义，`:18` 的
    /// `$include` 里再定义一次）—— 变量表是**全局**的
    /// （`scriplib.cpp:49-53` 的 `g_definevariable`），覆盖也必须跨文件生效。
    ///
    /// 真 studiomdl 裁决（`docs/_probe/oracle_definevariable.js`）：
    /// - 变体 D `$include` 里定义、主文件用 ⟹ `fromsub.mdl`（**跨 include 可见**）
    /// - 变体 F 主文件定义、`$include` 里使用 ⟹ `frommain.mdl`（**跨 include 可见**）
    /// - 变体 E 主文件先定义、`$include` 里再定义 ⟹ `frommain.mdl`
    ///   （官方**不覆盖** —— mdlc 这里是故意差异）
    #[test]
    fn definevariable_override_crosses_include() {
        let dir = fixture("var_include");
        std::fs::write(
            dir.join("sub.qci"),
            "$definevariable Name overridden\n$surfaceprop \"metal\"\n",
        )
        .expect("应能写 qci");
        // ⚠️ 顺序必须与用户工程一致：`anim_fix.qc` 是
        // `:1 $definevariable Name` → `:13 $definevariable scale` →
        // `:18 $include` → `:24 $modelname survivors/anim_$Name$.mdl`
        // ⟹ **先定义、再 include（覆盖）、最后才展开**。
        //
        // ⚠️ `$modelname` 必须用**裸 token**：变量只在裸 token 里展开，
        // 引号里不展开（`qc::lexer::tests::variable_is_not_expanded_inside_quotes`）。
        //
        // `$Dir$` 由**主 QC** 定义、`sub.qci` 不再定义 —— 它钉住「变量表
        // 跨 `$include` **保留**」这另一半（只测覆盖的话，一个
        // 「`push_include` 时清空变量表」的实现也能骗过测试：清空后
        // `sub.qci` 的 `Name overridden` 照样生效，产物名不变）。
        let qc = "\
$definevariable Dir var
$definevariable Name original
$include sub.qci
$modelname models/$Dir$/$Name$.mdl
$body body \"a.smd\"
";
        let d = parse(qc, &dir);
        assert_eq!(
            d.model.name, "models/var/overridden.mdl",
            "`$include` 里的再定义必须覆盖主 QC 的定义（变量表是全局的），\
             且主 QC 定义的其它变量必须**跨 include 保留**"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **`$optimizevtx`（mdlc 扩展）打开顶点缓存优化。**
    ///
    /// ⚠️ 官方**没有**这个命令：`studiomdl.exe` 的 105 条分发表里
    /// `optimize` / `vcache` / `nvtristrip` 对 QC **全部 0 命中**
    /// （官方把缓存优化做成 `-nvtristrip` **命令行**开关，
    /// 见 `tmp-qcscan\dispatch_table.tsv` 与 `docs/_probe/str_scan.js`）。
    /// 所以这条测试钉的是 **mdlc 自己的扩展**，不是官方兼容行为。
    ///
    /// 缺省是 `false`（`ModelMeta::optimize_vtx` 的 `#[serde(default)]`），
    /// 这条命令是**唯一**能从 QC 打开它的途径。
    #[test]
    fn optimizevtx_extension_turns_the_switch_on() {
        let dir = fixture("qc_ext_optimizevtx");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$optimizevtx
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            d.model.optimize_vtx,
            "`$optimizevtx` 必须把 optimize_vtx 置为 true"
        );
    }

    /// **不写 `$optimizevtx` 时缺省仍是 `false`。**
    ///
    /// 这是上一条的**反向判据** —— 否则「把缺省改成 `true`」也能骗过它。
    /// 缺省必须保持 `false`：本项目的验收基准是真 `studiomdl.exe`，
    /// 打开缓存优化会改变索引顺序（`src/model.rs:2250-2256`）。
    #[test]
    fn optimizevtx_defaults_to_off() {
        let dir = fixture("qc_ext_optimizevtx_default");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !d.model.optimize_vtx,
            "QC 不写 `$optimizevtx` 时 optimize_vtx 必须是 false（保持与既有产物逐字节相同）"
        );
    }

    #[test]
    fn nosplitoversizedmeshes_extension_turns_the_switch_off() {
        let dir = fixture("qc_ext_nosplit");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$nosplitoversizedmeshes
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !d.model.split_oversized_meshes,
            "`$nosplitoversizedmeshes` 必须把 split_oversized_meshes 置为 false"
        );
    }

    /// **不写命令时缺省仍是 `true`**（与 TOML 侧同一个缺省函数）。
    #[test]
    fn splitoversizedmeshes_defaults_to_on() {
        let dir = fixture("qc_ext_split_default");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            d.model.split_oversized_meshes,
            "QC 不写命令时 split_oversized_meshes 必须是 true（与 TOML 的 default_true 一致）"
        );
    }

    /// **两个命令成对，后写覆盖先写。**
    ///
    /// 这条钉的是「`$include` 进来的 `.qci` 关了它，主 QC 还得能开回来」——
    /// QC 是自上而下解释的，后出现的命令必须赢。若实现里把
    /// `$splitoversizedmeshes` 误写成「只在未设置时才置位」，这条会失败。
    #[test]
    fn split_extension_pair_later_write_wins() {
        let dir = fixture("qc_ext_split_pair");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$nosplitoversizedmeshes
$splitoversizedmeshes
";
        let d = parse(qc, &dir);
        assert!(
            d.model.split_oversized_meshes,
            "后写的 `$splitoversizedmeshes` 必须把前一条 `$nosplitoversizedmeshes` 覆盖回来"
        );

        let qc_rev = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$splitoversizedmeshes
$nosplitoversizedmeshes
";
        let d_rev = parse(qc_rev, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !d_rev.model.split_oversized_meshes,
            "反向顺序也必须以后写的 `$nosplitoversizedmeshes` 为准"
        );
    }

    /// **三个扩展命令名大小写不敏感**（`dispatch` 先 `to_ascii_lowercase()`）。
    ///
    /// 官方取词器对命令名同样不分大小写，所以这是「与官方风格一致」的判据。
    #[test]
    fn qc_extensions_are_case_insensitive() {
        let dir = fixture("qc_ext_case");
        let qc = "\
$ModelName \"t.mdl\"
$Body body \"a.smd\"
$OptimizeVTX
$NoSplitOversizedMeshes
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(d.model.optimize_vtx, "`$OptimizeVTX` 大小写混写也必须生效");
        assert!(
            !d.model.split_oversized_meshes,
            "`$NoSplitOversizedMeshes` 大小写混写也必须生效"
        );
    }

    /// **三个扩展命令都是「裸标志位」，不能吃掉下一行的 token。**
    ///
    /// ⚠️ 这条防的是历史形态的缺陷（见
    /// [`redefinevariable_is_rejected_like_official`]）：只吃掉命令名、
    /// **把值留在流里**，于是下一个 token 被当成下一条命令。
    /// 这里在三个命令后面各放一条 `$surfaceprop`，若命令多吃一个 token，
    /// `$surfaceprop` 就会被吞掉（`surface_prop` 落空）或报未知命令。
    ///
    /// ⚠️ QC 里的 `;` 是**行注释**，所以夹具必须**每键一行**。
    #[test]
    fn qc_extensions_do_not_eat_the_next_token() {
        let dir = fixture("qc_ext_bare");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$optimizevtx
$surfaceprop \"metal\"
$nosplitoversizedmeshes
$surfaceprop \"metal\"
$splitoversizedmeshes
$surfaceprop \"metal\"
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.model.surface_prop.as_deref(),
            Some("metal"),
            "三个扩展命令都必须只消费自己那一个 token"
        );
    }

    /// 同一块可以**重复出现**（实测 `jig26` 两个连续 `is_flexible` 成功）。
    #[test]
    fn jiggle_block_may_repeat() {
        let dir = fixture("jig_repeat");
        let qc = jig_qc(
            "\tis_flexible {\n\t\tyaw_stiffness 100\n\t}\n\
             \tis_flexible {\n\t\tpitch_stiffness 200\n\t}",
        );
        let w = jig_writes(&qc, &dir);
        let keys: Vec<&str> = w.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "is_flexible",
                "yaw_stiffness",
                "is_flexible",
                "pitch_stiffness"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 骨骼表收骨判据 ----
    //
    // 官方 `BuildGlobalBonetable()` 只收**两类**骨骼：
    //
    // 1. `$definebone` —— 无条件（`simplify.cpp:3624-3654`）；
    // 2. `psource->boneref[j] != 0`（`simplify.cpp:3668`）。
    //
    // `boneref` 的唯一生产者是 `TagUsedBones()`（`simplify.cpp:3424-3547`）：
    // **网格源**的顶点权重（`:3441-3455`，由 `isActiveModel` 把关）、
    // `$attachment`（`:3464-3489`）、`$ikchain`（`:3491-3502`）、
    // `$mouth`（`:3504-3515`）、`$bonemerge`（`:3517-3528`）、
    // 眼球骨骼（`:3544`），最后沿父链上传（`UpdateBonerefRecursive`）。
    //
    // ⭐ **为什么必须复刻**：mdlc 旧实现把每个 SMD 的每个 `node` 都无条件
    // 收进来，于是用户工程 `linnea_replaces_zoey` 得到 **134 根**，而官方
    // studiomdl 编**同一份 QC** 只有 **122 根**（裁决实验
    // `docs/_probe/official_user_qc3.js`）。134 > 引擎的 `MAXSTUDIOBONES`
    // （128，`hl2sdk-l4d2/public/studio.h:83`）⟹ `CBoneCache::CreateResource`
    // 里的 `short studioToCachedIndex[128]`（`hl4sdk-l4d2/public/bone_setup.cpp:62-89`）
    // 被逐骨骼无条件写入而**越界写栈** ⟹ 游戏加载时无响应 / 闪退。

    /// 拼一份 SMD：`nodes` 是 `(下标, 名, 父下标)`，`verts` 是每个顶点的骨骼下标。
    ///
    /// 顶点行格式取自 `smd.rs`：`parentBone pos(3) normal(3) uv(2) links 数 bone weight`。
    fn smd_with(nodes: &[(i32, &str, i32)], verts: &[i32]) -> String {
        let mut s = String::from("version 1\nnodes\n");
        for (i, n, p) in nodes {
            s.push_str(&format!("{i} \"{n}\" {p}\n"));
        }
        s.push_str("end\nskeleton\ntime 0\n");
        for (i, _, _) in nodes {
            s.push_str(&format!(
                "{i} 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n"
            ));
        }
        s.push_str("end\ntriangles\nmat\n");
        for b in verts {
            s.push_str(&format!(
                "0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 \
                 0.000000 0.000000 1 {b} 1.000000\n"
            ));
        }
        s.push_str("end\n");
        s
    }

    /// 解析 QC 后取骨骼名表（按描述文件里的**下标序**）。
    fn bone_names(qc: &str, dir: &std::path::Path) -> Vec<String> {
        parse(qc, dir)
            .bones
            .iter()
            .map(|b| b.name.clone())
            .collect()
    }

    /// ⭐ **核心回归**：只在**动画源**的 `nodes` 里、零顶点引用的骨骼**不入表**。
    ///
    /// 这就是 134 → 122 的那 12 根。官方对动画源（`isActiveModel = false`）
    /// 的顶点权重一概不认，`boneref` 始终为 0 ⟹ `:3668` 丢弃。
    #[test]
    fn anim_only_bone_without_vertex_ref_is_dropped() {
        let dir = fixture("bone_ref_animonly");
        std::fs::write(
            dir.join("b.smd"),
            smd_with(
                &[(0, "root", -1), (1, "bone1", 0), (2, "anim_only", 1)],
                &[0, 1, 1],
            ),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$sequence \"idle\" \"b.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "bone1"],
            "`anim_only` 只在动画源 `b.smd` 的 nodes 里、没有任何顶点引用 \
             ⟹ 官方 `BuildGlobalBonetable` 的 `if (psource->boneref[j])` 不收它。\
             收进来就会让骨骼总数顶过引擎上限（用户工程正是 134 > 128 ⟹ 加载闪退）"
        );
    }

    /// **网格源**里被顶点引用的骨骼必须保留（动画源完全不提它也一样）。
    #[test]
    fn mesh_vertex_ref_keeps_bone() {
        let dir = fixture("bone_ref_meshvert");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(&[(0, "root", -1), (1, "mesh_only", 0)], &[0, 1, 1]),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "mesh_only"],
            "`mesh_only` 被网格源顶点引用 ⟹ `BONE_USED_BY_VERTEX_LOD0` ⟹ 必须入表"
        );
    }

    /// ⭐ **动画源的顶点权重一概不算** —— 即使那个源自己真的有顶点引用它。
    ///
    /// 这是 `isActiveModel` 那半判据（`simplify.cpp:3441-3442`）的专用夹具：
    /// 上面那条 `anim_only_bone_without_vertex_ref_is_dropped` 的动画源里
    /// `anim_only` **零顶点引用**，所以即使把网格源门整个删掉它照样被丢 ——
    /// 那条**抓不到**这个变异。这里让动画源 `b.smd` 的顶点真的引用 `anim_vert`：
    /// 官方因为 `b.smd` 不是活动模型而不认这些权重 ⟹ `anim_vert` 必须被丢。
    #[test]
    fn anim_source_vertex_refs_do_not_count() {
        let dir = fixture("bone_ref_animvert");
        std::fs::write(
            dir.join("b.smd"),
            smd_with(
                &[(0, "root", -1), (1, "bone1", 0), (2, "anim_vert", 1)],
                &[0, 1, 2],
            ),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$sequence \"idle\" \"b.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "bone1"],
            "`anim_vert` 只被**动画源** `b.smd` 的顶点引用。官方 `TagUsedBones` 的\
             顶点循环由 `psource->isActiveModel` 把关（`simplify.cpp:3441-3442`），\
             而全树只有 `Option_Studio` 一处传 `true`（`studiomdl.cpp:963`）\
             ⟹ 动画源的顶点权重对 `boneref` **零贡献** ⟹ `anim_vert` 必须被丢。\
             若这里出现了 `anim_vert`，说明网格源门失效，骨骼数会重新膨胀"
        );
    }

    /// `$definebone` **无条件**入表 —— 顶点不引用、SMD 里甚至没有它，也照收。
    ///
    /// 这条同时钉住两件事：① 收骨判据对 `$definebone` **不设条件**（它走的是
    /// `simplify.cpp:3624-3654` 那条路，压根不看 `boneref`）；② 最终顺序由
    /// `sort_parents_first` 的**拓扑排序**决定，`extra` 的父是 `root` ⟹ 必须
    /// 排在 `root` 之后（同层的兄弟按「`$definebone` 先、SMD 序后」的入表序）。
    #[test]
    fn definebone_enters_bonetable_unconditionally() {
        let dir = fixture("bone_ref_definebone");
        let qc = "\
$modelname \"t.mdl\"
$definebone \"extra\" \"root\" 0 0 0 0 0 0 0 0 0 0 0 0
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "bone1", "extra"],
            "`$definebone` 的 `extra` 顶点不引用、SMD 里也没有 ⟹ 它**只能**靠 \
             「`$definebone` 无条件入表」（`simplify.cpp:3624-3654`）活下来。\
             若这里少了 `extra`，说明收骨判据把 `boneref == 0` 也套到了 \
             `$definebone` 上 —— 官方不这么做（用户工程的 122 根里 \
             绝大多数正是零顶点引用的 `$definebone`）。\
             顺序上 `extra` 的父是 `root`，拓扑排序必须把它排在其父之后"
        );
    }

    /// ⭐ **核心回归**：`$definebone` 声明的**根**骨骼（空父）绝不被源 SMD 的父链收编。
    ///
    /// 官方 `BuildGlobalBonetable`（`simplify.cpp:3616`）分两段：
    ///
    ///   1. `:3624-3654` 先插 `$definebone`（`bPreDefined = true`），父由
    ///      `$definebone` 给 —— **空串在 `:3631-3634` 显式写 `-1`**，
    ///      即「这根本来就是根」；
    ///   2. `:3666-3712` 并入各源用到的骨骼：只有 `k == -1`（**表里还没有**）
    ///      才从源取父（`:3671-3679`），已存在的走 `:3707-3711`
    ///      **只做 `flags |=`，从不碰 `parent`**。
    ///
    /// 回归现场（真工程 `v_snip_awp_processed.qc`，2026-10）：`bones.qci` 里
    /// `$definebone "ValveBiped.ValveBiped" "" …` 被无条件回填成
    /// `parent = "awp_model_Inverted_edit_skeleton"` —— 那是 FBX 的骨架根
    /// 节点，自身带 **−90° 绕 X**。整棵骨骼树因此被带着转了 90°：70 根骨骼的
    /// `poseToBone` **每一行都不同**，VVD 包围盒的 Y/Z 两轴互换。
    /// 官方产物里 `ValveBiped.ValveBiped` 是 `p = -1` 的独立根。
    #[test]
    fn definebone_explicit_root_is_not_adopted_by_source_parent() {
        let dir = fixture("bone_ref_defroot");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(
                &[(0, "skeleton_root", -1), (1, "root", 0), (2, "bone1", 1)],
                &[0, 1, 2],
            ),
        )
        .expect("应能写 SMD");

        // 基线：没有 `$definebone` 时，`root` 的父从源 SMD 的 `nodes` 段推。
        let base = "$modelname \"t.mdl\"\n$body body \"a.smd\"\n\
                    $sequence \"idle\" \"a.smd\" fps 30\n";
        let d0 = parse(base, &dir);
        let p0 = d0
            .bones
            .iter()
            .find(|b| b.name == "root")
            .expect("`root` 应在骨骼表里")
            .parent
            .clone();
        assert_eq!(
            p0.as_deref(),
            Some("skeleton_root"),
            "基线：没写 `$definebone` 时父链应从源 SMD 推出来"
        );

        // 显式声明 `root` 是根（空父）⟹ 父必须保持 `None`。
        let qc = "$modelname \"t.mdl\"\n\
                  $definebone \"root\" \"\" 0 0 0 0 0 0\n\
                  $body body \"a.smd\"\n\
                  $sequence \"idle\" \"a.smd\" fps 30\n";
        let d1 = parse(qc, &dir);
        let p1 = d1
            .bones
            .iter()
            .find(|b| b.name == "root")
            .expect("`root` 应在骨骼表里")
            .parent
            .clone();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            p1, None,
            "`$definebone \"root\" \"\"` 的空父是**显式声明为根**\
             （官方 `simplify.cpp:3631-3634` 写 `-1`），不是「未指定」—— \
             绝不能被源 SMD 的父链收编。若这里变成 `Some(\"skeleton_root\")`，\
             说明父链回填漏了 `predefined` 守卫，FBX 源的骨架根变换会\
             把整棵树带偏（真工程里是 −90° 绕 X）"
        );
    }

    /// `$bonemerge` 只能**保活**已在 `nodes` 里的骨骼，**不能创造**骨骼。
    ///
    /// 官方两处都证明它不创造：`TagUsedBones` 的 bonemerge 循环
    /// （`simplify.cpp:3517-3528`）遍历的是**已存在**的 `psource->localBone[]`；
    /// `TagUsedImportedBones`（`:3592-3610`）只写 `g_bonetable[j].flags`、
    /// 不碰 `psource->boneref[]`。
    #[test]
    fn bonemerge_keeps_existing_bone_but_never_creates_one() {
        let dir = fixture("bone_ref_bonemerge");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(
                &[(0, "root", -1), (1, "unused", 0), (2, "bone1", 0)],
                &[0, 0, 0],
            ),
        )
        .expect("应能写 SMD");
        let head = "$modelname \"t.mdl\"\n$body body \"a.smd\"\n";
        let tail = "$sequence \"idle\" \"a.smd\" fps 30\n";

        // 基线：`unused` 与 `bone1` 都零顶点引用 ⟹ 都被丢。
        let base = format!("{head}{tail}");
        assert_eq!(
            bone_names(&base, &dir),
            vec!["root"],
            "零顶点引用的骨骼应被丢弃（`boneref == 0`）"
        );

        // `$bonemerge` 保活已存在的 `unused`。
        let merged = format!("{head}$bonemerge \"unused\"\n{tail}");
        assert_eq!(
            bone_names(&merged, &dir),
            vec!["root", "unused"],
            "`$bonemerge` 应给**已存在**的骨骼打 `BONE_USED_BY_BONE_MERGE` 而保活它"
        );

        // `$bonemerge` **不能**凭空造出 `ghost`。
        let ghost = format!("{head}$bonemerge \"ghost\"\n{tail}");
        assert_eq!(
            bone_names(&ghost, &dir),
            vec!["root"],
            "`$bonemerge \"ghost\"` 里的 `ghost` 不在任何 SMD 的 nodes 里 \
             ⟹ 官方不会收它（`$bonemerge` 不是骨骼声明）"
        );
    }

    /// `$attachment` 保活骨骼；**`rigid` 时改为沿父链上溯到第一根有顶点权重的骨骼**
    /// （`simplify.cpp:3472-3486`）。
    ///
    /// 这条差异很隐蔽：`rigid` 的附着点自己**不**入表，入表的是它的祖先。
    #[test]
    fn attachment_keeps_bone_and_rigid_walks_up_to_vertexed_ancestor() {
        let dir = fixture("bone_ref_attach");
        // `root` 有顶点；`mid` / `tip` 都零顶点引用。
        std::fs::write(
            dir.join("a.smd"),
            smd_with(&[(0, "root", -1), (1, "mid", 0), (2, "tip", 1)], &[0, 0, 0]),
        )
        .expect("应能写 SMD");
        let head = "$modelname \"t.mdl\"\n$body body \"a.smd\"\n";
        let tail = "$sequence \"idle\" \"a.smd\" fps 30\n";

        assert_eq!(bone_names(&format!("{head}{tail}"), &dir), vec!["root"]);

        // 普通附着点：`tip` 自己保活，再沿父链把 `mid` / `root` 带上来。
        let plain = format!("{head}$attachment \"a\" \"tip\" 0 0 0\n{tail}");
        assert_eq!(
            bone_names(&plain, &dir),
            vec!["root", "mid", "tip"],
            "非 rigid 的 `$attachment` 给**自己**置位（`simplify.cpp:3485`），\
             随后父链上传把祖先一并带入"
        );

        // 刚性附着点：从 `tip` 上溯到第一根有顶点权重的骨骼 = `root` ⟹ 只保活 `root`。
        let rigid = format!("{head}$attachment \"a\" \"tip\" 0 0 0 rigid\n{tail}");
        assert_eq!(
            bone_names(&rigid, &dir),
            vec!["root"],
            "`rigid` 的 `$attachment` 沿父链找第一根 `BONE_USED_BY_VERTEX_LOD0` 的骨骼\
             （`simplify.cpp:3474-3481`）⟹ 入表的是 `root` 而不是 `tip`"
        );
    }

    /// 父链上传（`UpdateBonerefRecursive`，`simplify.cpp:3404-3418`）：只有叶子被引用时，
    /// **整条祖先链都要入表** —— 否则骨骼树的 `parent` 会指向不存在的下标。
    #[test]
    fn boneref_propagates_up_the_parent_chain() {
        let dir = fixture("bone_ref_chain");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(
                &[(0, "root", -1), (1, "mid", 0), (2, "leaf", 1)],
                &[2, 2, 2],
            ),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "mid", "leaf"],
            "顶点只引用 `leaf`，但官方 `UpdateBonerefRecursive` 会沿父链上传 \
             ⟹ `mid` / `root` 也必须入表（官方注释：This must come last）"
        );
    }

    /// `$ikchain` 保活骨骼（`simplify.cpp:3491-3502`）—— 即使零顶点引用。
    #[test]
    fn ikchain_keeps_bone() {
        let dir = fixture("bone_ref_ikchain");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(&[(0, "root", -1), (1, "mid", 0), (2, "tip", 1)], &[0, 0, 0]),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$ikchain \"ik\" \"mid\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "mid"],
            "`$ikchain` 的**第二个** token 是骨骼名（`studiomdl.cpp:4487-4488`）\
             ⟹ `mid` 保活；`tip` 无人引用，仍应被丢"
        );
    }

    /// `$mouth` 保活骨骼（`simplify.cpp:3504-3515`）。
    #[test]
    fn mouth_keeps_bone() {
        let dir = fixture("bone_ref_mouth");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(&[(0, "root", -1), (1, "mid", 0), (2, "tip", 1)], &[0, 0, 0]),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    mouth 0 \"m\" \"mid\" 0 0 0
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "mid"],
            "`mouth <i> <名> <骨骼> <fx> <fy> <fz>` 的第三个 token 是骨骼名 ⟹ `mid` 保活"
        );
    }

    /// 眼球骨骼保活（`simplify.cpp:3544`），且**只对自己的模型**生效。
    #[test]
    fn eyeball_bone_is_kept() {
        let dir = fixture("bone_ref_eyeball");
        std::fs::write(
            dir.join("a.smd"),
            smd_with(&[(0, "root", -1), (1, "mid", 0), (2, "tip", 1)], &[0, 0, 0]),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"eye\" \"mid\" 0 0 0 \"eye_mat\" 1 0 \"iris_mat\" 1
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "mid"],
            "`eyeball <名> <骨骼> ...` 的骨骼被 `boneref[...] |= BONE_USED_BY_ATTACHMENT` \
             ⟹ `mid` 保活；`tip` 仍应被丢"
        );
    }

    /// 同一根骨骼出现在**多个源**里时只入表一次（`seen` 去重）。
    #[test]
    fn bone_present_in_many_sources_enters_once() {
        let dir = fixture("bone_ref_dedup");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
$sequence \"idle2\" \"b.smd\" fps 30
$sequence \"idle3\" \"c.smd\" fps 30
";
        let names = bone_names(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            names,
            vec!["root", "bone1"],
            "三份 SMD 的 nodes 完全相同 ⟹ 骨骼表里每根只应出现一次"
        );
    }

    // ---------------------------------------------------------------
    // `$illumposition` 的可选第 4 个 token（骨骼名）
    // ---------------------------------------------------------------
    //
    // 官方 `Cmd_Illumposition` 的 `GetToken(false)` 读一个**可选**骨骼名，
    // 并新建一个名为 `__illumPosition` 的合成附着点。全部语义由真
    // studiomdl 裁决（`docs/_probe/oracle_illumposition3..10.js`）：
    //
    // | 形式 | `0x5C` 落盘 | 合成附着点 | `studiohdr2+0x08` |
    // |---|---|---|---|
    // | `$illumposition x y z` | `[-y, x, z]` | 无 | 0 |
    // | `$illumposition x y z <骨骼>` | **原样 `[x,y,z]`** | 有 | **1 起**下标 |
    //
    // 且合成附着点的 `type` 带 `IS_RIGID`（`oracle10` 证实它与 `rigid`
    // 附着点对骨骼表的效应逐字节一致）。

    /// 4 参数形式：合成附着点的字段逐个钉死。
    #[test]
    fn illumposition_with_bone_creates_synthetic_attachment() {
        let dir = fixture("illum_synth");
        let qc = "\
$modelname \"t.mdl\"
$illumposition 5 6 7 bone1
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.model.illum_position, Some([5.0, 6.0, 7.0]));
        assert!(
            d.model.illum_position_from_bone,
            "4 参数形式 ⟹ 写出时**不做**轴变换（`oracle_illumposition6.js`）"
        );
        let syn: Vec<_> = d.attachments.iter().filter(|a| a.synthetic).collect();
        assert_eq!(syn.len(), 1, "应恰好合成一个附着点");
        assert_eq!(syn[0].name, "__illumPosition");
        assert_eq!(syn[0].bone, "bone1");
        assert_eq!(
            syn[0].position,
            Some([5.0, 6.0, 7.0]),
            "三个坐标进 `local` 的平移列（`oracle5` 的 `illum_nonzero`）"
        );
        assert_eq!(syn[0].rotation, None, "零旋转 ⟹ `local` 的旋转块是单位矩阵");
        assert!(
            syn[0].rigid,
            "保活语义 = `IS_RIGID`（`oracle_illumposition10.js`）—— 缺了它，\
             绑一根零引用骨骼时该骨骼会**被留下**，与官方不符"
        );
        assert_eq!(syn[0].flags, Some(0), "`flags` 落盘 0");
    }

    /// 3 参数形式**不**合成附着点，且仍走轴变换。
    #[test]
    fn illumposition_without_bone_makes_no_attachment() {
        let dir = fixture("illum_three");
        let qc = "\
$modelname \"t.mdl\"
$illumposition 5 6 7
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.model.illum_position, Some([5.0, 6.0, 7.0]));
        assert!(
            !d.model.illum_position_from_bone,
            "3 参数形式落盘 `[-y,x,z]`（`oracle_illumposition6.js` 的 `illum3`）"
        );
        assert!(
            d.attachments.iter().all(|a| !a.synthetic),
            "没有第 4 个 token ⟹ 不合成附着点（`oracle6` 的 `illum3`：num=0 attIdx=0）"
        );
    }

    /// ⭐ 骨骼名**从未出现在任何 SMD 的 `nodes` 里** ⟹ 硬报错。
    ///
    /// 官方 `LinkAttachments()` 的 `MdlError( "unknown attachment link '%s'\n" )`
    /// （`simplify.cpp:5375`），真 exe 裁决见 `oracle_illumposition7.js`
    /// 的 `illum4 bad bone`。
    #[test]
    fn illumposition_with_unknown_bone_is_an_error() {
        let dir = fixture("illum_bad");
        let qc = "\
$modelname \"t.mdl\"
$illumposition 0 0 0 nosuchbone
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let errs = crate::qc::parse_qc_str(qc, &dir).expect_err("未知骨骼必须报错");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("unknown attachment link")
                    && e.message.contains("nosuchbone")),
            "应报官方同款 `unknown attachment link`，实际：{errs:?}"
        );
        assert!(
            errs.iter().any(|e| e.line == 2),
            "错误应指向 `$illumposition` 那一行（第 2 行），实际：{errs:?}"
        );
    }

    /// ⭐ 骨骼名**出现过但被收骨判据丢掉** ⟹ **静默**，不报错。
    ///
    /// 这是与上一条的关键区分：官方 `MapSourcesToGlobalBonetable()` 把它
    /// 静默重映射到根骨骼 0（`simplify.cpp:4180` 的 `k = 0;`），真 exe
    /// 裁决见 `oracle_illumposition8.js` 的 `illum_b2` 与
    /// `oracle_illumposition10.js` 的 `illum b2`。
    /// 若用「是否在 `desc.bones` 里」当判据，就会把这一类**误报**成错误。
    #[test]
    fn illumposition_with_dropped_bone_is_silent() {
        let dir = fixture("illum_dropped");
        // `anim_only` 只在 `nodes` 里、零顶点引用 ⟹ 收骨判据丢弃它。
        std::fs::write(
            dir.join("a.smd"),
            smd_with(
                &[(0, "root", -1), (1, "bone1", 0), (2, "anim_only", 1)],
                &[0, 1, 1],
            ),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$illumposition 0 0 0 anim_only
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            !d.bones.iter().any(|b| b.name == "anim_only"),
            "前提：`anim_only` 必须已被收骨判据丢掉"
        );
        let syn = d
            .attachments
            .iter()
            .find(|a| a.synthetic)
            .expect("合成附着点仍应存在");
        assert_eq!(
            syn.bone, "anim_only",
            "名字原样保留；写出时由 `mdl_writer` 回退到根骨骼 0"
        );
    }

    /// ⭐ 阶段 2：被丢掉的骨骼 ⟹ `bone` 取**存活祖先**的下标，
    /// 且 `local` 必须乘上修正矩阵 `inverse(world[祖先]) ∘ world[原始]`。
    ///
    /// 官方 `simplify.cpp:5341-5376`：`k` 沿父链上溯到第一根直命中全局骨骼表
    /// 的祖先，`bone` 取**祖先**的全局下标，而 `boneToPose` 仍是**原始**骨骼的
    /// ⟹ 非 absolute 时 `local' = poseToBone(祖先) ∘ boneToPose(原始) ∘ local`
    /// （`simplify.cpp:5385`/`:5388`）。
    ///
    /// 真 exe 裁决：`docs/_probe/diff_illumposition.js` 的 `illum_drop`
    /// （夹具父链 `i-1`，`b2` 的父是存活的 `b1`）⟹
    /// `bone=1 local=[1,0,0,0,0,1,0,0,0,0,1,20]`，即 30 − 10 = 20。
    #[test]
    fn illumposition_stage2_recomputes_local_from_reference_pose() {
        let dir = fixture("illum_stage2");
        // `dropped` 零顶点引用 ⟹ 被收骨判据丢弃；它的父 `b1` 存活。
        //
        // 第 0 帧必须给两者**不同**的平移，否则修正矩阵退化成单位阵 ——
        // `Some(单位阵)` 与 `None` 虽然仍可区分，但「按参考姿态重算」这件事
        // 就没被钉住。
        let mut smd = smd_with(
            &[(0, "root", -1), (1, "b1", 0), (2, "dropped", 1)],
            &[0, 1, 1],
        );
        for (bone, z) in [(1, 10.0f32), (2, 20.0f32)] {
            let from = format!("{bone} 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n");
            let to = format!("{bone} 0.000000 0.000000 {z:.6} 0.000000 0.000000 0.000000\n");
            assert!(
                smd.contains(&from),
                "夹具格式变了：找不到骨骼 {bone} 的第 0 帧行"
            );
            smd = smd.replace(&from, &to);
        }
        std::fs::write(dir.join("a.smd"), smd).expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$illumposition 0 0 0 dropped
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.bones.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            vec!["root", "b1"],
            "前提：`dropped` 必须已被收骨判据丢掉，而 `b1` 存活"
        );
        let syn = d
            .attachments
            .iter()
            .find(|a| a.synthetic)
            .expect("合成附着点仍应存在");
        let (gi, corr) = syn
            .resolved
            .expect("阶段 2 必须给出 (骨骼下标, 修正矩阵)，不能留 `None`");
        assert_eq!(
            gi, 1,
            "取的是**存活祖先** `b1` 的全局下标（不是 `dropped`）"
        );
        assert_eq!(
            corr,
            [
                1.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 20.0
            ],
            "修正矩阵 = inverse(world[b1]) ∘ world[dropped]：\
             world[b1] 的 z=10、world[dropped] 的 z=30 ⟹ 平移差 20"
        );
    }

    /// 合成附着点**豁免**「名字重复」与「找不到骨骼」两条校验。
    ///
    /// 官方对同一个 `$illumposition x y z <骨骼>` 写两次会产出**两个**同名
    /// `__illumPosition` 附着点（`oracle_illumposition5.js` 的 `illum_twice`、
    /// `oracle_illumposition10.js` 的 `illum b2 twice`）⟹ mdlc 不能因为
    /// 重名而拒绝。而「找不到骨骼」那条也被豁免，因为被丢掉的骨骼同样
    /// 不在 `desc.bones` 里（见上一条测试）。
    #[test]
    fn synthetic_attachment_is_exempt_from_validate() {
        let dir = fixture("illum_twice");
        // 必须让 `anim_only` 真的出现在 `a.smd` 的 nodes 里 —— 否则它属于
        // 「从未出现过」那一类，应当**硬报错**（上一条测试的语义）。
        std::fs::write(
            dir.join("a.smd"),
            smd_with(
                &[(0, "root", -1), (1, "bone1", 0), (2, "anim_only", 1)],
                &[0, 1, 1],
            ),
        )
        .expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$illumposition 1 2 3 anim_only
$illumposition 4 5 6 anim_only
$body body \"a.smd\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.attachments.iter().filter(|a| a.synthetic).count(),
            2,
            "写两次 ⟹ 两个合成附着点（官方不去重）"
        );
        assert!(
            d.validate().is_ok(),
            "合成附着点应豁免校验（重名 + 被丢骨骼），实际：{:?}",
            d.validate().unwrap_err()
        );
    }

    /// ⚠️ **回归（R33）**：`$cmdlist <名> { … }` 收集命令，`cmdlist <名>` 引用它。
    ///
    /// 官方 `Cmd_Cmdlist`（`studiomdl.cpp:2317-2378`）把块内命令存进
    /// `g_cmdlist[]`（`MAXSTUDIOCMDS = 64`）；引用侧是
    /// `ParseAnimationToken` 的 `cmdlist` 分支（`:2277-2300`），
    /// 逐条**拷贝**进 `panim->cmds[]`，名字未命中报
    /// `unknown cmdlist %s`。
    ///
    /// 实测来源：用户工程 `incap_anim_fix\includes\anims_fix.qci:67-71`
    /// 是全工程唯一的 `$cmdlist Release_IK { … }`（5 条 `ikrule … release`），
    /// 被 24 处 `cmdlist Release_IK` 引用 —— 修前这些引用全部报
    /// `未知的 QC 命令 "cmdlist"`。
    #[test]
    fn cmdlist_block_is_collected_and_referenced() {
        let dir = fixture("cmdlist_ref");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$cmdlist Release_IK {
    ikrule rfoot release
    ikrule lfoot release
}
$animation \"a_idle\" \"a.smd\" fps 30 cmdlist Release_IK
$sequence \"seq_idle\" \"a_idle\"
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.cmd_lists.len(), 1, "应收集到 1 个 cmdlist");
        assert_eq!(d.cmd_lists[0].name, "Release_IK");
        assert_eq!(d.cmd_lists[0].cmds.len(), 2, "块内应有 2 条命令");
        let anim = &d.animations[0];
        assert_eq!(
            anim.cmds.len(),
            2,
            "`cmdlist Release_IK` 应把两条命令**拷进**动画，实际：{:?}",
            anim.cmds
        );
        for cmd in &anim.cmds {
            match cmd {
                crate::model::AnimCmd::IkRule { rule } => {
                    assert_eq!(
                        rule.kind,
                        crate::model::IkRuleType::Release,
                        "类型应是 release"
                    );
                }
                other => panic!("应是 IkRule，实际 {other:?}"),
            }
        }
    }

    /// `cmdlist` 引用一个不存在的名字要报错（官方 `unknown cmdlist %s`）。
    #[test]
    fn cmdlist_unknown_name_is_an_error() {
        let dir = fixture("cmdlist_unknown");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30 cmdlist NoSuchList
";
        let errs = crate::qc::parse_qc_str(qc, &dir).expect_err("未定义的 cmdlist 名必须报错");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs.iter().any(|e| e.message.contains("NoSuchList")),
            "报错应点名缺失的 cmdlist，实际：{errs:?}"
        );
    }

    /// ⚠️ **回归（R34）**：`$continue <序列名> <选项…>` 走**序列级**选项链。
    ///
    /// 官方 `Cmd_Continue`（`studiomdl.cpp:3170-3198`）**先** `LookupSequence`
    /// —— 命中就以 `isAppend = true` 重入 `ParseSequence`（完整的序列级
    /// 选项链），只有名字不是序列时才退回 `ParseAnimation`。
    ///
    /// 修前 mdlc 是「先试动画池」，且序列路径只调 `parse_animation_token`
    /// ⟹ `fadeout`/`ACT_*`/`addlayer`/`hidden` 等**序列级**关键字全报
    /// `未知的命令`。实测来源：`incap_anim_fix\includes\anims_fix.qci:212`
    /// 的 `$DebiddoChargerLoop Idle_Fall_From_Charger ACT_… -1`。
    #[test]
    fn continue_appends_sequence_level_options() {
        let dir = fixture("continue_seq");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$sequence \"seq_idle\" \"a_idle\"
$continue \"seq_idle\" fadeout 0.5 hidden ACT_VM_IDLE 2
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.sequences.len(), 1, "`$continue` **不新建**序列");
        let s = &d.sequences[0];
        assert_eq!(s.fade_out, 0.5, "序列级 `fadeout` 应被追加");
        assert_eq!(
            s.extra_flags.map(|f| f & 0x0400),
            Some(0x0400),
            "序列级 `hidden` 应置位"
        );
        assert_eq!(s.activity.as_deref(), Some("ACT_VM_IDLE"));
        assert_eq!(s.activity_weight, 2);
    }

    /// `$continue <动画名>` 走**动画级**选项链（官方 `ParseAnimation`）。
    ///
    /// ⚠️ 官方 `ParseAnimation` 的 `isAppend` 形参**在函数体里完全没用**
    /// （`studiomdl.cpp:2442-2499`）⟹ 与 `$animation` 体是同一套解析。
    #[test]
    fn continue_updates_an_animation() {
        let dir = fixture("continue_anim");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$continue \"a_idle\" fps 60 loop
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(d.sequences.len(), 0, "不该多出序列");
        let a = &d.animations[0];
        assert_eq!(a.fps, Some(60.0), "动画级 `fps` 应被追加");
        assert!(a.looping, "动画级 `loop` 应被追加");
    }

    /// ⚠️ **回归（R35）**：`$sequence` 兜底必须委派 `ParseAnimationToken`。
    ///
    /// 官方 `ParseSequence` 有**三段**尾部分派（`studiomdl.cpp:2944-2976`），
    /// 第一段就是 `ParseAnimationToken( animations[0] )`。mdlc 修前只有两段
    /// ⟹ `frame`（单数）/`fudgeloop`/`noanimation`/`align`/`alignto`/
    /// `walkframe`/`walkalignto`/`cmdlist`/控制位 `X Y Z LX LY` 全被静默
    /// 压进 `blend_names` 当假 blend 格，编译时报
    /// 「blend 格数 N 不是完全平方数」（`compile.rs:937`）。
    ///
    /// 实测来源：`anims_fix.qci:165` 的 `align Death X Y 100 0` 产生 6 个假格。
    #[test]
    fn sequence_catchall_delegates_to_parse_animation_token() {
        let dir = fixture("seq_delegate");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$animation \"a_idle\" \"a.smd\" fps 30
$sequence \"seq_idle\" \"a_idle\" frame 0 1 fudgeloop noanimation
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let s = &d.sequences[0];
        assert!(
            s.blends.is_empty(),
            "`frame`/`fudgeloop`/`noanimation` 都是命令，不该变成 blend 格，实际：{:?}",
            s.blends
        );
        assert!(s.looping, "`fudgeloop` 应经 holder 并回 `looping`");
        assert_eq!(
            d.animations[0].cmds.len(),
            1,
            "`noanimation` 应变成一条 AnimCmd，实际：{:?}",
            d.animations[0].cmds
        );
        assert!(matches!(
            d.animations[0].cmds[0],
            crate::model::AnimCmd::NoAnimation
        ));
    }

    /// ⚠️ **回归（R36）**：`$sequence` 的**隐含动画**裸名要补 `$pushd` 前缀。
    ///
    /// 官方 `Cmd_ImpliedAnimation`（`studiomdl.cpp:2506-2547`）调
    /// `Load_Source( panim->filename, "" )`，而 `Load_Source` 用
    /// `cddir[numdirs]` 拼路径（`%s%s.smd`，`studiomdl.cpp:1603-1638`）
    /// ⟹ **`$pushd` 的前缀是官方自动加的**。
    ///
    /// 修前 mdlc 原样 push 裸名，于是 `$pushd anims` 之下的隐含动画全被
    /// 当成工程根目录下的文件 —— 实测用户工程报 22 条
    /// `sequences[N].smd: 读不到 .\NamVet_*.smd`。
    ///
    /// ⚠️ 夹具里的动画名**不能**叫 `x`/`y`/`z`/`lx`/`ly` ——
    /// `lookup_control` 会把它当**运动控制位**消费掉
    /// （官方 `ParseAnimationToken` 的 `lookupControl( token ) != -1`
    /// 分支，`studiomdl.cpp:2301-2304`），根本走不到「隐含动画」那一支。
    #[test]
    fn implied_animation_name_gets_cddir_prefix() {
        let dir = fixture("implied_pushd");
        std::fs::create_dir_all(dir.join("anims")).expect("应能建 anims 子目录");
        std::fs::write(dir.join("anims").join("sway.smd"), MIN_SMD).expect("应能写 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$pushd anims
$sequence \"seq_idle\" \"sway\"
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.sequences[0].smd, "anims/sway",
            "隐含动画名必须带上 `$pushd` 前缀（官方 `Load_Source` 自动加）"
        );
    }

    /// **纯动画工程（无 `$body`/`$model`）是合法输入。**
    ///
    /// 官方没有「至少需要一个 body part」这条检查：
    /// 真 exe 裁决（`docs/_probe/oracle_no_body.js`）去掉 `$body` 后
    /// `exit=0`、MDL 1048 B、`numbodyparts=0`（且**不产出** `.vvd`/`.dx90.vtx`
    /// —— `write.cpp:2203-2204` 的 `if (phdr->numbodyparts == 0) return;`）。
    ///
    /// 实测来源：用户工程 `incap_anim_fix` 是纯动画工程（只有 `$modelname`
    /// + 22 条 `$sequence`），修前卡在这条过严的校验上。
    ///
    /// ⚠️ 夹具**必须带 `$definebone`**：没有 `$body` ⟹ 没有
    /// `isActiveModel` 的源 ⟹ 收骨判据（`78b9e77`）会把所有骨骼丢掉，
    /// 于是撞上另一条 mdlc 独有的 `bones.is_empty()` 检查
    /// （`model.rs:5306-5311`）。真实的纯动画工程也总是靠 `$definebone`
    /// 撑起骨骼表 —— 用户工程的 72 根正是来自 `includes/definebones.qci`。
    #[test]
    fn animation_only_project_has_no_bodyparts_and_validates() {
        let dir = fixture("anim_only_project");
        let qc = "\
$modelname \"survivors/anim_test.mdl\"
$definebone \"root\" \"\" 0 0 0 0 0 0 0 0 0 0 0 0
$animation \"a_idle\" \"a.smd\" fps 30
$sequence \"seq_idle\" \"a_idle\"
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(d.bodyparts.is_empty(), "纯动画工程不该有 bodypart");
        assert_eq!(d.bones.len(), 1, "`$definebone` 应无条件入表");
        assert!(
            d.validate().is_ok(),
            "纯动画工程必须通过校验（官方无此检查），实际：{:?}",
            d.validate().unwrap_err()
        );
    }

    // ---------------------------------------------------------------
    // 官方 `eyelid` 语法（`Option_Eyelid`，`studiomdl.cpp:3645-3802`）
    // ---------------------------------------------------------------
    //
    // 官方形：
    //
    // ```text
    // eyelid <名> <vta> lowerer <帧> <目标> neutral <帧> <目标> raiser <帧> <目标>
    //        [split <距离>] [eyeball <眼球名>]
    // ```
    //
    // 一条 `eyelid` 会注册 **4 个 flexdesc**（`<名>` + `<名>_lowerer` /
    // `_neutral` / `_raiser`）并推 **3 条 flexkey**（全部 `flexdesc = <名>`、
    // 共用同一个 `.vta` 与同一个 `split`）：
    //
    // | # | frame | target0..3 |
    // |---|---|---|
    // | 0 | lowerer 的帧 | `-11, -10, lowerer, neutral` |
    // | 1 | neutral 的帧 | `lowerer, neutral, neutral, raiser` |
    // | 2 | raiser 的帧 | `neutral, raiser, 10, 11` |
    //
    // ⚠️ `neutral 0` 会让第 1 条落在 **frame 0**（官方的「载荷清零」特殊帧，
    // `simplify.cpp:2453-2457`）—— 它**合法**，正是 [`Flex::from_eyelid`]
    // 存在的唯一理由。夹具故意用 `neutral 0` 把这个豁免也钉住。

    /// 官方 `eyelid`：4 个 desc 的**注册顺序**、3 条 flexkey 的帧与 targets。
    #[test]
    fn official_eyelid_registers_four_descs_and_three_flexkeys() {
        let dir = fixture("eyelid_official");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper_right \"face.vta\" lowerer 1 -0.41 neutral 0 0.5 raiser 2 0.5 split 0.1 eyeball righteye
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);

        let names: Vec<&str> = d.flex_descriptors.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "upper_right",
                "upper_right_lowerer",
                "upper_right_neutral",
                "upper_right_raiser"
            ],
            "官方 `:3665-3666` 先无条件注册 base desc，再按 token 顺序各注册一个 \
             `<名>_<token>`（`studiomdl.cpp:3683`/`:3692`/`:3701`）"
        );

        let flexes = &d.bodyparts[0].models[0].flexes;
        assert_eq!(
            flexes.len(),
            3,
            "官方 `:3722-3754` 一条 eyelid 推 3 条 flexkey"
        );
        let frames: Vec<i32> = flexes.iter().map(|f| f.frame).collect();
        assert_eq!(frames, vec![1, 0, 2], "帧号来自 lowerer/neutral/raiser");
        assert_eq!(
            flexes[0].targets,
            Some([-11.0, -10.0, -0.41, 0.5]),
            "key0 = `[-11, -10, lowerer, neutral]`"
        );
        assert_eq!(
            flexes[1].targets,
            Some([-0.41, 0.5, 0.5, 0.5]),
            "key1 = `[lowerer, neutral, neutral, raiser]`"
        );
        assert_eq!(
            flexes[2].targets,
            Some([0.5, 0.5, 10.0, 11.0]),
            "key2 = `[neutral, raiser, 10, 11]`"
        );
        for (i, f) in flexes.iter().enumerate() {
            assert_eq!(
                f.name, "upper_right",
                "3 条 flexkey 的 flexdesc **全部**是 base desc（官方 `flexdesc = basedesc`），\
                 于是 `resolve_vta_flexes` 的 `register` 会把它们收敛到同一下标"
            );
            assert!(!f.pair, "eyelid 的 flexkey 从不设 flexpair（官方恒 0）");
            assert!(f.from_eyelid, "flexes[{i}] 应标记为来自 eyelid");
            assert_eq!(f.split, 0.1, "三条共用同一个 `split`");
            assert_eq!(f.vta, "face.vta", "三条共用同一个 `.vta`");
        }

        let eb = &d.bodyparts[0].models[0].eyeballs[0];
        let up = eb.upper_lid.as_ref().expect("`upper_right` 应挂成上眼睑");
        assert_eq!(up.lid_flexdesc, "upper_right");
        assert_eq!(up.lowerer.flexdesc, "upper_right_lowerer");
        assert_eq!(up.lowerer.target, -0.41);
        assert_eq!(up.neutral.flexdesc, "upper_right_neutral");
        assert_eq!(up.neutral.target, 0.5);
        assert_eq!(up.raiser.flexdesc, "upper_right_raiser");
        assert_eq!(up.raiser.target, 0.5);
        assert!(
            eb.lower_lid.is_none(),
            "`type[0] == 'u'` 只挂上眼睑，不该顺手把下眼睑也填上"
        );
    }

    /// ⭐ 判别性：`eyeball` 关键字**省略**时作用于该 model 的**全部**眼球。
    ///
    /// 官方 `:3756-3765` 是 `if (szEyeball[0] != '\0') { … continue; }` ——
    /// 空串时**不**过滤，于是循环把每个眼球都填一遍。
    #[test]
    fn official_eyelid_without_eyeball_keyword_applies_to_all_eyeballs() {
        let dir = fixture("eyelid_all_eyes");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyeball \"lefteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper \"face.vta\" lowerer 1 -0.4 neutral 0 0.5 raiser 2 0.5
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let ebs = &d.bodyparts[0].models[0].eyeballs;
        assert_eq!(ebs.len(), 2);
        for eb in ebs {
            assert!(
                eb.upper_lid.is_some(),
                "省略 `eyeball` 时**每个**眼球都该被挂上（官方 `:3761-3765`），\
                 实际 {:?} 没有",
                eb.name
            );
        }
    }

    /// `eyeball <名>` **只**作用于被点名的那一个（按 `stricmp` 比较）。
    #[test]
    fn official_eyelid_eyeball_keyword_selects_one() {
        let dir = fixture("eyelid_one_eye");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyeball \"lefteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid lower_right \"face.vta\" lowerer 3 -0.26 neutral 0 -0.5 raiser 4 -0.5 eyeball righteye
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let ebs = &d.bodyparts[0].models[0].eyeballs;
        assert!(
            ebs[0].lower_lid.is_some(),
            "`righteye` 被点名 ⟹ 应挂上下眼睑（`type[0] == 'l'`）"
        );
        assert!(
            ebs[1].lower_lid.is_none(),
            "`lefteye` 没被点名 ⟹ 不该被挂上"
        );
    }

    /// ⭐ **`eyeball` 的半径是 QC 直径的一半** —— `eyelid` 的范围检查是判据。
    ///
    /// 官方 `:3411` 是 `eyeball->radius = verify_atof(token) / 2.0;`，而
    /// `:3767-3778` 的范围检查是 `fabs(target) > peyeball->radius`。
    /// 夹具用 `eyeball … 1.0 …`（直径 1.0 ⟹ 半径 0.5）配 `lowerer … -0.6`：
    /// 少除一次 `/2` 会让半径变 1.0，`-0.6` 就**不再**越界 ⟹ 本测试失败。
    #[test]
    fn eyeball_diameter_is_halved_into_radius() {
        let dir = fixture("eyelid_radius");
        // 0.5 恰好落在半径上：`>` 不是 `>=`，所以必须**通过**。
        let ok = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper \"face.vta\" lowerer 1 -0.5 neutral 0 0.5 raiser 2 0.5
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(ok, &dir);
        assert_eq!(
            d.bodyparts[0].models[0].eyeballs[0].radius, 0.5,
            "QC 的 1.0 是**直径** ⟹ `radius` 必须是 0.5（官方 `studiomdl.cpp:3411`）"
        );

        // 0.6 > 0.5 ⟹ 官方 `TokenError( "Eyelid \"%s\" lowerer out of range (+-%.1f)\n" )`。
        let bad = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper \"face.vta\" lowerer 1 -0.6 neutral 0 0.5 raiser 2 0.5
}
$sequence \"idle\" \"a.smd\" fps 30
";
        // ⚠️ 文案在 `parse_qc_str` 里就渲染好了，所以语言必须在那一刻生效
        //（`out of range` 是英文源文里刻意保留的官方记号，但锁住更稳）。
        let errs = crate::test_locale::with_english(|| {
            crate::qc::parse_qc_str(bad, &dir).expect_err("超出半径必须报错")
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs.iter().any(|e| e.message.contains("out of range")),
            "应报官方同款 `out of range`，实际：{errs:?}"
        );
    }

    /// `type[0]` 不是 `u`/`l` 时**静默不挂载**，但三条 flexkey 照样注册。
    ///
    /// 官方 `:3780-3800` 是 `switch(type[0])` 只带 `case 'u'` / `case 'l'`
    /// —— 没有 `default`。判据是**首字母**，不是 `contains("upper")`。
    #[test]
    fn official_eyelid_switch_only_accepts_u_and_l_first_letter() {
        let dir = fixture("eyelid_switch");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid brow \"face.vta\" lowerer 1 -0.4 neutral 0 0.4 raiser 2 0.4
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.bodyparts[0].models[0].flexes.len(),
            3,
            "首字母不认识 ⟹ 不挂载，但**三条 flexkey 已经注册**（官方也是先推 key 再挂）"
        );
        let eb = &d.bodyparts[0].models[0].eyeballs[0];
        assert!(
            eb.upper_lid.is_none() && eb.lower_lid.is_none(),
            "`brow` 的首字母既不是 `u` 也不是 `l` ⟹ 官方静默不挂载"
        );
    }

    /// `lowerer`/`neutral`/`raiser` 缺一个 ⟹ 报错（官方会读到未初始化的栈值）。
    #[test]
    fn official_eyelid_missing_raiser_is_an_error() {
        let dir = fixture("eyelid_missing");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper \"face.vta\" lowerer 1 -0.4 neutral 0 0.4
}
$sequence \"idle\" \"a.smd\" fps 30
";
        // ⚠️ `QcError` 的 `message` 在 `parse_qc_str()` 里就渲染好了，
        // 所以语言必须在**那一刻**生效（不是包住 `contains`）。
        let errs = crate::test_locale::with_english(|| {
            crate::qc::parse_qc_str(qc, &dir).expect_err("缺 raiser 必须报错")
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("missing one of lowerer/neutral/raiser")),
            "应报缺项，实际：{errs:?}"
        );
    }

    /// 写了 `eyeball <名>` 但该眼球不存在 ⟹ 报错（官方静默什么都不做）。
    #[test]
    fn official_eyelid_unknown_eyeball_is_an_error() {
        let dir = fixture("eyelid_bad_eye");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper \"face.vta\" lowerer 1 -0.4 neutral 0 0.4 raiser 2 0.4 eyeball nosuch
}
$sequence \"idle\" \"a.smd\" fps 30
";
        // ⚠️ 文案在 `parse_qc_str()` 里就渲染好了，语言必须在那一刻生效。
        let errs = crate::test_locale::with_english(|| {
            crate::qc::parse_qc_str(qc, &dir).expect_err("未知眼球必须报错")
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("nonexistent eyeball") && e.message.contains("nosuch")),
            "应点名 `nosuch`，实际：{errs:?}"
        );
    }

    /// 未知选项 ⟹ 报错（官方 `TokenError( "unknown option: %s" )`）。
    #[test]
    fn official_eyelid_unknown_option_is_an_error() {
        let dir = fixture("eyelid_bad_opt");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    eyeball \"righteye\" \"bone1\" 0 0 0 \"eye_mat\" 1.0 0 \"iris_mat\" 1
    eyelid upper \"face.vta\" lowerer 1 -0.4 neutral 0 0.4 raiser 2 0.4 wat
}
$sequence \"idle\" \"a.smd\" fps 30
";
        // ⚠️ 文案在 `parse_qc_str()` 里就渲染好了，语言必须在那一刻生效。
        let errs = crate::test_locale::with_english(|| {
            crate::qc::parse_qc_str(qc, &dir).expect_err("未知选项必须报错")
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            errs.iter()
                .any(|e| e.message.contains("unknown eyelid option")),
            "应报未知选项，实际：{errs:?}"
        );
    }

    /// ⭐ `mouth` 的第 2 个 token 是 flexdesc 名，官方**顺带注册**它。
    ///
    /// `Option_Mouth`（`:3818`）走的是 `g_mouth[index].flexdesc =
    /// Add_Flexdesc( token );`。漏掉注册会让 `%mouth = …` 这类 flexrule
    /// 在 `[[flex_descriptors]]` 里找不到目标。
    #[test]
    fn mouth_registers_its_flexdesc() {
        let dir = fixture("mouth_desc");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    mouth 0 \"mouth\" \"bone1\" 0 1 0
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<&str> = d.flex_descriptors.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["mouth"],
            "`mouth <i> <名> <骨骼> <fx> <fy> <fz>` 的 `<名>` 必须进 flexdesc 表"
        );
        assert_eq!(d.mouths.len(), 1);
        assert_eq!(d.mouths[0].flexdesc, "mouth");
    }

    /// ⭐⭐ `%<flex>` 规则名必须保留**原始大小写**。
    ///
    /// 官方 `Cmd_Model` 走 `Option_Flexrule( g_model[g_nummodels], &token[1] )`
    /// （`studiomdl.cpp:4366`）—— `token` 保留原样，随后用 `stricmp` 查
    /// `g_flexdesc`（`:3911`）。
    ///
    /// ⚠️ mdlc 的 `cmd_model` 块内循环用 `low = t.text.to_ascii_lowercase()`
    /// 做分派，兜底臂里若误用 `low` 就会把 `%AU1R` 存成 `au1r`；而写出阶段的
    /// `flexdesc_index`（`compile.rs:5737`）是**大小写敏感**的 ⟹ 报
    /// `flex "au1r" 在 [[flex_descriptors]] 里找不到`。用户工程的
    /// `survivors_facerules.qci` 里 62 条大写规则全部踩中这个坑。
    #[test]
    fn flexrule_target_keeps_its_original_case() {
        let dir = fixture("flexrule_case");
        let qc = "\
$modelname \"t.mdl\"
$model \"body\" \"a.smd\" {
    flexcontroller eyes range -30 30 eyes_updown
    flexfile \"a.vta\"
    flex \"AU1R\" frame 1
    flexpair \"AU2\" 1.0 frame 2
    %AU1R = eyes_updown
    %AU2R = eyes_updown
}
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);

        // `flex "AU1R"` 直接注册；`flexpair "AU2"` 注册 `AU2R`/`AU2L`（先 R 后 L）。
        let names: Vec<&str> = d.flex_descriptors.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["AU1R", "AU2R", "AU2L"],
            "flex / flexpair 注册的 desc 名必须保留原始大小写"
        );

        let rule_names: Vec<&str> = d.flex_rules.iter().map(|r| r.flex.as_str()).collect();
        assert_eq!(
            rule_names,
            vec!["AU1R", "AU2R"],
            "`%<flex>` 的规则名必须保留原始大小写 —— 小写化之后写出阶段的 \
             `flexdesc_index`（大小写敏感）会查不到，报 \
             `flex \"au1r\" 在 [[flex_descriptors]] 里找不到`"
        );
    }

    /// ⚠️ **回归**：`srcpart` 必须**每次只吃一个 token**。
    ///
    /// 首版实现读到行尾（`while self.avail()`），于是
    /// `$body body "a.fbx" srcpart "hat" srcaxis "y"` 里的 `srcaxis`
    /// 被当成**第二个网格名** —— 不报错（不存在的网格名会被静默过滤），
    /// 只是 `srcaxis` 静默失效。实测夹具 T8/T9 的包围盒与不加 `srcaxis`
    /// 时完全相同，才发现。
    #[test]
    fn srcpart_does_not_swallow_the_following_option() {
        let dir = fixture("srcpart_swallow");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\" srcpart \"hat\" srcaxis \"y\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let m = &d.bodyparts[0].models[0];
        assert_eq!(
            m.src_parts,
            vec!["hat"],
            "`srcpart \"hat\"` 后面还有 `srcaxis \"y\"` —— 只应吃一个 token"
        );
        assert_eq!(
            m.src_axis.as_deref(),
            Some("y"),
            "`srcaxis` 必须被当成选项解析，而不是第二个网格名"
        );
    }

    /// 同一个坑的另外两个受害者：`srcshapekey` / `srcshapekeyorder`。
    ///
    /// ⚠️ 后面跟的选项**不能**是 `srcshapekeyignore` —— 它与
    /// `srcshapekey` 互斥（`validate()` 会拒绝），那样测的就变成互斥检查了。
    /// 这里用 `srcmaterial`，它和形状 key 名单毫无关系。
    #[test]
    fn srcshapekey_does_not_swallow_the_following_option() {
        let dir = fixture("srcshapekey_swallow");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\" srcshapekey \"wide\" srcmaterial \"my/fallback\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let m = &d.bodyparts[0].models[0];
        assert_eq!(
            m.src_shape_keys,
            vec!["wide"],
            "`srcshapekey \"wide\"` 后面还有 `srcmaterial` —— 只应吃一个 token"
        );
        assert_eq!(
            m.src_material.as_deref(),
            Some("my/fallback"),
            "`srcmaterial` 必须被当成选项解析，而不是第二个形状名"
        );
    }

    /// `srcpart` 可重复写，效果是**累加**（等价于一次给多个名字）。
    #[test]
    fn srcpart_is_repeatable() {
        let dir = fixture("srcpart_repeat");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\" srcpart \"body\" srcpart \"hair\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.bodyparts[0].models[0].src_parts,
            vec!["body", "hair"],
            "可重复写，顺序保留"
        );
    }

    /// ⚠️ **回归**：`finish()` 读源必须带上该源的 `src*` 选项。
    ///
    /// 材质表是在 `finish()` 里按「源里出现的材质名」登记的，而
    /// `compile()` 侧按 `srcmaterial` 取到的是用户写的名字。两边用不同的
    /// 选项 ⟹ 报 `SMD 里的材质名 "my/fallback" 在 [materials].textures
    /// 里找不到`（实测夹具 S7）。
    #[test]
    fn srcmaterial_reaches_the_texture_table() {
        let dir = fixture("srcmaterial");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\" srcmaterial \"my/fallback\"
$sequence \"idle\" \"a.smd\" fps 30
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        // `a.smd` 里的材质名是 `mat`，所以这条断言只证明「解析没报错」；
        // 真正的端到端验证在 `target\srcoptcheck` 的 S7 夹具（那边用 FBX
        // 源，`srcmaterial` 会替换掉 `debug/debugempty`）。
        assert_eq!(
            d.bodyparts[0].models[0].src_material.as_deref(),
            Some("my/fallback")
        );
        assert!(
            d.materials.textures.iter().any(|t| t.name == "mat"),
            "SMD 里出现的材质名仍须进材质表，实际：{:?}",
            d.materials
                .textures
                .iter()
                .map(|t| &t.name)
                .collect::<Vec<_>>()
        );
    }

    /// ⚠️ **回归（R67.1）**：`{ event <名> <帧> }` 的收尾 `}` 不能被当成 options。
    ///
    /// 官方 `Option_Event`（`studiomdl.cpp:1188-1215`）读完事件名与帧号后是
    /// `if (TokenAvailable()) { GetToken(false); if (token[0] == '}') return 1;
    /// strcpyn(options, token); } return 0;` —— **返回 1 表示「这个 `}` 我吃掉了」**，
    /// 调用点 `ParseSequence` 是 `depth -= Option_Event( pseq );`（`:2706-2709`）。
    ///
    /// 修前本实现无条件把下一个 token 当 options、并硬编码返回 `0`
    /// ⟹ `}` 被写进 `options` 而 `depth` 没减 ⟹ **`$sequence` 块永不闭合**，
    /// 后续每个 `$sequence` 块的 3 个 token（`$sequence`/名字/SMD）全被兜底
    /// arm 当成 blend 格吞进 `blends`。真实工程
    /// `v_huntingrifle_processed.qc` 的 12 处 `{ event … }` 累计吞出 **103 格**，
    /// 编译时报「blend 格数 103 不是完全平方数」。
    ///
    /// 真 exe 裁决（`target/_r67/oracle_event.js`）：`}` 同行与换行两种写法
    /// 官方产物**逐字节相同** ⟹ 两种都合法，只有 mdlc 会错。
    #[test]
    fn event_closing_brace_is_not_swallowed_as_options() {
        let dir = fixture("event_brace");
        // 8 帧的 SMD：帧号 7 落在范围内 ⟹ `finish()` 会把它换算成
        // `cycle = 7 / (8-1) = 1.0`（`parse.rs` 的 `k = nf - 1` 那一支）。
        // 用单帧 SMD 的话任何非零帧号都会被判越界而写成 0，断言就没有分辨力。
        let mut smd =
            String::from("version 1\nnodes\n0 \"root\" -1\n1 \"bone1\" 0\nend\nskeleton\n");
        for i in 0..8 {
            smd.push_str(&format!("time {i}\n"));
            smd.push_str("0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000\n");
            smd.push_str("1 0.000000 0.000000 1.000000 0.000000 0.000000 0.000000\n");
        }
        smd.push_str("end\ntriangles\nmat\n");
        smd.push_str(
            "0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.000000 0.000000 1 0 1.000000\n",
        );
        smd.push_str(
            "0 1.000000 0.000000 0.000000 0.000000 0.000000 1.000000 1.000000 0.000000 1 1 1.000000\n",
        );
        smd.push_str(
            "0 0.000000 1.000000 0.000000 0.000000 0.000000 1.000000 0.000000 1.000000 1 1 1.000000\n",
        );
        smd.push_str("end\n");
        std::fs::write(dir.join("a.smd"), &smd).expect("应能写 8 帧 SMD");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$sequence \"seq_a\" {
    \"a.smd\"
    { event AE_TEST 7 }
    fps 30
}
$sequence \"seq_b\" { \"b.smd\" }
$sequence \"seq_c\" { \"c.smd\" }
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d.sequences.len(),
            3,
            "`}}` 同行时后续序列不该被吞进 blends，实际序列：{:?}",
            d.sequences.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        let a = &d.sequences[0];
        assert_eq!(a.events.len(), 1, "事件本身必须照常入表");
        assert_eq!(a.events[0].name, "AE_TEST");
        assert_eq!(
            a.events[0].cycle, 1.0,
            "帧号 7 应经 `finish()` 换算成 7/(8-1)=1.0（证明帧号确实被读到了）"
        );
        assert_eq!(
            a.events[0].options, "",
            "收尾 `}}` 不是 options（官方 `token[0] == '}}'` 时返回 1）"
        );
        assert!(a.blends.is_empty(), "不该有 blend 格，实际：{:?}", a.blends);
        assert_eq!(a.fps, Some(30.0), "`}}` 之后的 `fps` 仍属于本序列");
    }

    /// ⚠️ **回归（R67.2）**：`ikrule` 选项循环遇到不认识的 token 必须**吐回**。
    ///
    /// 官方 `Option_IKRule` 的选项循环结尾是 `else { UnGetToken(); return; }`
    /// （`studiomdl.cpp:1378-1382`），而 `UnGetToken` 的真身只是
    /// `tokenready = true`（`utils\common\scriplib.cpp:330-333`）
    /// ⟹ token 原样回到 `ParseSequence` 主循环，走三段尾部分派。
    ///
    /// 修前本实现写 `let _ = other;`（静默丢弃）⟹ `abi3.qc` 的
    /// `ikrule "leg" attachment "ankle" "muzzle"` 里那个 `"muzzle"` 凭空消失。
    /// 真 exe 裁决（`target/_r67/oracle_ikrule.js`）：官方把它当**第二格
    /// blend 动画**（`Cmd_ImpliedAnimation` → `Load_Source` 找 `muzzle.smd`）——
    /// 文件不存在报 `could not load file 'muzzle'`，存在则变成 2 格非方形
    /// 网格报 `calcblend failed in idle`（`simplify.cpp:5568`）。
    #[test]
    fn ikrule_unknown_token_is_unget_not_swallowed() {
        let dir = fixture("ikrule_unget");
        let qc = "\
$modelname \"t.mdl\"
$body body \"a.smd\"
$ikchain \"leg\" \"bone1\" knee 0.5 0.5 0
$sequence \"idle\" \"a.smd\" fps 30 {
    ikrule \"leg\" attachment \"bone1\" \"muzzle\"
}
";
        let d = parse(qc, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let s = &d.sequences[0];
        assert_eq!(s.ik_rules.len(), 1, "ikrule 本身要入表");
        assert_eq!(
            s.ik_rules[0].attachment.as_deref(),
            Some("bone1"),
            "`attachment` 只吃一个 token"
        );
        assert_eq!(
            s.blends,
            vec!["muzzle".to_string()],
            "多写的 token 必须吐回 `$sequence` 主循环、被当成隐含动画\
             （官方 `Cmd_ImpliedAnimation`），实际：{:?}",
            s.blends
        );
    }
    #[test]
    fn motion_rollback_defaults_are_captured_at_source_declaration() {
        let dir = fixture("motion_rollback");
        let desc = parse(
            "$body body a.smd\n$animation original a.smd\n$sequence original_source a.smd\n$motionrollback 0.7\n$animation later a.smd\n$sequence later_source a.smd\n$sequence named original LX\n$motionrollback 0\n$sequence last_source a.smd\n",
            &dir,
        );
        assert_eq!(desc.animations[0].motion_rollback, 0.3);
        assert_eq!(desc.animations[1].motion_rollback, 0.7);
        assert_eq!(desc.sequences[0].motion_rollback, 0.3);
        assert_eq!(desc.sequences[1].motion_rollback, 0.7);
        assert_eq!(desc.sequences[3].motion_rollback, 0.0);
        assert_eq!(desc.animations[0].motion_type, 0x40);
        let desc = parse(
            "$body body a.smd\n$animation target a.smd\n$motionrollback 0.7\n$sequence named target motionrollback 0.7\n$continue named motionrollback 0.9\n",
            &dir,
        );
        assert_eq!(desc.animations[0].motion_rollback, 0.9);
        for value in ["-1", "nan", "inf"] {
            let errors =
                crate::qc::parse_qc_str(&format!("$motionrollback {value}"), &dir).unwrap_err();
            assert!(errors[0].message.contains("finite nonnegative"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn sequence_flag_inheritance_uses_authored_flags_at_declaration_time() {
        let dir = fixture("sequence_flag_order");
        let mut desc = parse(
            "$body body a.smd\n$animation source a.smd snap\n$sequence early source predelta\n$continue source post realtime loop\n$sequence late source predelta\n",
            &dir,
        );
        desc.model.version = Some(48);
        assert_eq!(desc.sequences[0].inherited_flags, Some(2));
        assert_eq!(desc.sequences[1].inherited_flags, Some(0x93));
        let built = crate::pipeline::build(&desc, &dir, Default::default()).unwrap();
        let sequences = i32::from_le_bytes(built.mdl[192..196].try_into().unwrap()) as usize;
        let flags = |index: usize| {
            i32::from_le_bytes(
                built.mdl[sequences + index * 212 + 12..sequences + index * 212 + 16]
                    .try_into()
                    .unwrap(),
            )
        };
        assert_eq!(flags(0) & 0x117, 6);
        assert_eq!(flags(1) & 0x117, 0x117);
        let desc = parse(
            "$body body a.smd\n$animation shared a.smd\n$sequence before shared\n$sequence looped shared loop\n$sequence after shared\n",
            &dir,
        );
        assert_eq!(desc.sequences[0].inherited_flags, Some(0));
        assert_eq!(desc.sequences[2].inherited_flags, Some(1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn animation_loop_options_and_flags_keep_named_source_scope() {
        let dir = fixture("loop_source_options");
        let desc = parse(
            "$body body a.smd\n$animation original a.smd startloop 1 snap post realtime\n$animation plain a.smd\n$sequence reference original\n$sequence implicit a.smd fudgeloop startloop 1\n$sequence sequence_flags plain snap post realtime\n",
            &dir,
        );
        assert_eq!(desc.animations[0].loop_restart, 1);
        assert!(desc.animations[0].looping);
        assert_eq!(desc.animations[0].extra_flags, 0x92);
        assert_eq!(desc.animations[1].extra_flags, 0);
        assert_eq!(desc.sequences[1].loop_restart, 1);
        assert!(desc.sequences[1].fudge_loop);
        assert_eq!(desc.sequences[2].extra_flags, Some(0x92));
        let desc = parse(
            "$body body a.smd\n$animation original a.smd startloop 1\n$sequence reference original\n$continue reference startloop 0 fudgeloop\n",
            &dir,
        );
        assert_eq!(desc.animations[0].loop_restart, 0);
        assert!(desc.animations[0].fudge_loop);
        for value in ["-1", "1.25", "nan", "-0.5"] {
            let errors = crate::qc::parse_qc_str(
                &format!("$animation invalid a.smd startloop {value}"),
                &dir,
            )
            .unwrap_err();
            assert!(errors[0].message.contains("nonnegative integer"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
