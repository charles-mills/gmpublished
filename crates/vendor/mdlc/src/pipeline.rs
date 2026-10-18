//! 编排管线：`ModelDesc` → 四件套字节 → 落盘。
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::path::Path;
//! use mdlc::pipeline::{self, PipelineOptions};
//!
//! let text = std::fs::read_to_string("x.toml")?;
//! let desc = mdlc::model::ModelDesc::from_toml(&text)?;
//! let out = pipeline::build(&desc, Path::new("."), PipelineOptions::default())?;
//! let paths = pipeline::write_files(&out, Path::new("out"))?;
//! println!("{}", paths.mdl.display());
//! # Ok(())
//! # }
//! ```

use std::path::{Path, PathBuf};

use crate::compile::{CompileError, SrcError, compile, resolve_smd_path};
use crate::mdl_writer::{WriteError, write_mdl};
use crate::model::{CompiledModelDesc, ModelDesc};
use crate::phy::{self, PhyError};
use crate::smd::SmdError;
use crate::vtx_writer::{self, VtxOptions, VtxWriteError};
use crate::vvd::VvdError;

/// 编排选项。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineOptions {
    /// 是否对每个 strip group 跑 `meshopt::optimize_vertex_cache`。
    ///
    /// 与 TOML 的 `[model] optimize_vtx` 是 **`||`** 关系 —— 任一为真即开启。
    /// 默认关闭，以保持与真 `studiomdl.exe` 逐字节一致
    /// （见 [`crate::vtx_writer::VtxOptions`]）。
    pub optimize_vtx: bool,
}

/// 编排失败的原因。
///
/// # 为什么每个变体都带着**源错误**而不是一个 `String`
///
/// 先前这里只有 `Compile(Vec<CompileError>)` 与三个 `String` 变体
/// （`Collision` / `Write` / `Io`）。调用方拿到 `Io("读不到碰撞 SMD x.smd：
/// 系统找不到指定的文件。 (os error 2)")` 之后**只能去正则匹配中文前缀**
/// 才能判断「是读不到文件」还是「路径写法不对」，而真正的
/// [`std::io::Error`] —— 它带着 [`std::io::ErrorKind`] —— 已经被
/// `format!` 扔掉了。
///
/// 现在每个变体各自持有**产生它的那个错误类型**，`source()` 能把整条链
/// 走到底，调用方可以 `match` 到具体种类（例如
/// `Err(PipelineError::ReadCollisionSmd { source, .. }) if source.kind() ==
/// ErrorKind::NotFound`）。[`Self::lines`] 拼出的仍是**单行**文案，
/// 所以 CLI 逐行输出的形态不变。
///
/// ⚠️ 文案本身已改为走译文表（源文英文，见 `locales/app.yml`）——
/// 各变体的 `#[error("{}", crate::tr_fmt!(...))]` 形态是 `thiserror` 逼出来的：
/// `#[error(...)]` 的第一个 token 必须是字符串字面量，所以用 `"{}"` 占位、
/// 把真正的文案交给 `tr_fmt!` 表达式。
///
/// ⚠️ 因此本类型**不是** `Clone` / `PartialEq`：它内部装着
/// [`std::io::Error`]，那个类型两者都不实现，而且「两次失败是否相等」
/// 对 IO 错误本来也没有意义。需要比较请用 [`Self::kind`]。
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// [`crate::compile::compile`] 失败（含 [`ModelDesc::validate`] 的失败）。
    #[error("{}", Self::compile_lines(.0).join("\n"))]
    Compile(Vec<CompileError>),
    /// 碰撞 SMD 的**路径字符串**不合法（缺扩展名等）。
    #[error("{}", crate::tr_fmt!("Error: %{err}", err = .0))]
    ResolveCollisionSmd(#[source] SrcError),
    /// 碰撞 SMD 路径解析出来了，但文件读不进来。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Error: cannot read collision SMD %{path}: %{err}",
            path = path.display(),
            err = source
        )
    )]
    ReadCollisionSmd {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 碰撞 SMD 读进来了，但解析不了。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Error: cannot parse collision SMD %{path}: %{err}",
            path = path.display(),
            err = source
        )
    )]
    ParseCollisionSmd {
        path: PathBuf,
        #[source]
        source: SmdError,
    },
    /// [`write_mdl`] 失败（`.mdl` 字节的构造）。
    #[error("{}", crate::tr_fmt!("Error: %{err}", err = .0))]
    WriteMdl(#[source] WriteError),
    /// `lod::build_vvd` 失败。
    #[error("{}", crate::tr_fmt!("Error: cannot build VVD: %{err}", err = .0))]
    BuildVvd(#[source] VvdError),
    /// `Vvd::to_bytes` 失败。
    #[error("{}", crate::tr_fmt!("Error: cannot encode VVD: %{err}", err = .0))]
    EncodeVvd(#[source] VvdError),
    /// VVD 的自检没过 —— 这是**本实现的 bug**，不是用户输入的问题。
    #[error(
        "{}",
        crate::tr_fmt!("Error: the VVD we wrote is not self-consistent (bug in mdlc): %{err}", err = .0)
    )]
    CheckVvd(#[source] VvdError),
    /// `vtx_writer::write_vtx_with` 失败。
    #[error("{}", crate::tr_fmt!("Error: cannot write VTX: %{err}", err = .0))]
    WriteVtx(#[source] VtxWriteError),
    /// VTX 的自检没过 —— 同上，是本实现的 bug。
    #[error(
        "{}",
        crate::tr_fmt!("Error: the VTX we wrote is not self-consistent (bug in mdlc): %{err}", err = .0)
    )]
    CheckVtx(#[source] VtxWriteError),
    /// `phy::build_phy_from_smd` / `build_ragdoll_phy_from_smd` 失败。
    #[error("{}", crate::tr_fmt!("Error: cannot build PHY: %{err}", err = .0))]
    BuildPhy(#[source] PhyError),
    /// PHY 的自检没过 —— 同上，是本实现的 bug。
    #[error(
        "{}",
        crate::tr_fmt!("Error: the PHY we wrote failed self-check (bug in mdlc): %{err}", err = .0)
    )]
    CheckPhy(#[source] PhyError),
    #[error("Error: output path must remain inside the selected directory: {path}")]
    InvalidOutputPath { path: PathBuf },
    #[error("Error: output target is not a regular file: {path}")]
    NonRegularOutput { path: PathBuf },
    #[error(
        "Error: cannot publish model export at {path}: {source}; recovery directory: {recovery_dir:?}; rollback errors: {rollback_errors:?}"
    )]
    ExportTransaction {
        path: PathBuf,
        #[source]
        source: std::io::Error,
        recovery_dir: Option<PathBuf>,
        rollback_errors: Vec<String>,
    },
    #[error("Error: export completed, but cannot remove temporary backups at {path}: {source}")]
    ExportCleanup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 建输出目录失败。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Error: cannot create directory %{path}: %{err}",
            path = path.display(),
            err = source
        )
    )]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// 写产物文件失败。
    #[error(
        "{}",
        crate::tr_fmt!(
            "Error: cannot write %{path}: %{err}",
            path = path.display(),
            err = source
        )
    )]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// 失败的大类 —— 与 `mdlc.exe` 的退出码一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineErrorKind {
    /// 编译 / 写出 / 自检失败。
    Build,
    /// 路径解析 / 读文件 / 建目录 / 写文件失败。
    Io,
}

impl PipelineErrorKind {
    /// 与 `mdlc.exe` 的退出码契约一致：`Build` → 1，`Io` → 2。
    pub fn exit_code(self) -> u8 {
        match self {
            PipelineErrorKind::Build => 1,
            PipelineErrorKind::Io => 2,
        }
    }
}

impl PipelineError {
    /// 失败的大类。
    ///
    /// ⚠️ 新增变体时**必须**在这里归类 —— 先前用 `_ => Build` 兜底，
    /// 那样会把「忘了归类的 IO 失败」静默报成编译失败（退出码从 2 变 1，
    /// 而既有探针是按退出码判定的）。写成穷举后，加变体会得到编译错误。
    pub fn kind(&self) -> PipelineErrorKind {
        match self {
            PipelineError::ResolveCollisionSmd(_)
            | PipelineError::ReadCollisionSmd { .. }
            | PipelineError::CreateDir { .. }
            | PipelineError::InvalidOutputPath { .. }
            | PipelineError::NonRegularOutput { .. }
            | PipelineError::ExportTransaction { .. }
            | PipelineError::ExportCleanup { .. }
            | PipelineError::WriteFile { .. } => PipelineErrorKind::Io,
            PipelineError::Compile(_)
            | PipelineError::ParseCollisionSmd { .. }
            | PipelineError::WriteMdl(_)
            | PipelineError::BuildVvd(_)
            | PipelineError::EncodeVvd(_)
            | PipelineError::CheckVvd(_)
            | PipelineError::WriteVtx(_)
            | PipelineError::CheckVtx(_)
            | PipelineError::BuildPhy(_)
            | PipelineError::CheckPhy(_) => PipelineErrorKind::Build,
        }
    }

    /// [`Self::Compile`] 的文案（首行是计数，之后每条一行）。
    ///
    /// 抽成关联函数是为了让 `#[error(...)]` 属性也能用它 —— 属性里写不出
    /// 带循环的表达式，而这里需要「首行 + N 条缩进」的形态。
    fn compile_lines(errs: &[CompileError]) -> Vec<String> {
        let mut v = vec![crate::tr_fmt!(
            "Compilation failed with %{n} error(s):",
            n = errs.len()
        )];
        v.extend(errs.iter().map(|e| format!("  - {e}")));
        v
    }

    /// 要打印给用户的**最终文案**（`mdlc.exe` 逐行原样输出；GUI 可直接显示）。
    ///
    /// 编译失败是多行（首行是计数，之后每条一行）；其余是单行。
    ///
    /// 与 [`std::fmt::Display`] 的关系：[`Self::Compile`] 之外**完全一致**
    /// （`to_string()` 就是 `lines().join("\n")`），只有 `Compile` 需要
    /// 拆成多行才好让 CLI 逐行 `log::error!`。
    pub fn lines(&self) -> Vec<String> {
        match self {
            PipelineError::Compile(errs) => Self::compile_lines(errs),
            // 其余变体的 `Display` 已经就是那一行 —— 直接复用，
            // 避免文案在两处各写一遍（先前注释里那条「不能走 Display」的
            // 警告说的是旧写法：当时 `Display` 是本函数拼出来的，
            // 现在反过来，`Display` 是各变体自己的属性）。
            other => vec![other.to_string()],
        }
    }
}

/// 一次编排的产物：各文件的**字节**，以及编译期 IR。
///
/// 这里不存路径 —— 路径由 [`PipelineOutput::paths`] 按 `out_root` 现算，
/// 所以同一份产物可以写到任意目录。
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineOutput {
    /// 模型名去掉 `.mdl` 后的相对路径（如 `models/mymod/x`），决定各文件名。
    pub stem: String,
    /// `.mdl` 字节。
    pub mdl: Vec<u8>,
    /// `.vvd` 字节。
    pub vvd: Vec<u8>,
    /// `.dx90.vtx` 字节。
    pub vtx: Vec<u8>,
    /// `.ani` 字节（`$animblocksize` 的外置动画块，没有就是 `None`）。
    pub ani: Option<Vec<u8>>,
    /// `.phy` 字节（有碰撞 SMD 时才有）。
    pub phy: Option<Vec<u8>>,
    /// 配对令牌，已写进各文件头部。
    pub checksum: i32,
    /// 编译期 IR —— `physics_bone` 已按碰撞 SMD 填好。
    pub compiled: CompiledModelDesc,
}

/// 落盘后的实际路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPaths {
    /// `out_root/<stem>.mdl`
    pub mdl: PathBuf,
    /// `out_root/<stem>.vvd`
    pub vvd: PathBuf,
    /// `out_root/<stem>.dx90.vtx`
    pub vtx: PathBuf,
    /// `out_root/<stem>.ani`（没有就是 `None`）。
    pub ani: Option<PathBuf>,
    /// `out_root/<stem>.phy`（没有就是 `None`）。
    pub phy: Option<PathBuf>,
}

fn physics_output_name(physics: &crate::model::Physics, stem: &str) -> PathBuf {
    let Some(name) = physics.name.as_deref() else {
        return PathBuf::from(format!("{stem}.phy"));
    };
    let name = name.replace('\\', "/");
    let mut path = if name.starts_with("models/") {
        PathBuf::from(name)
    } else {
        PathBuf::from("models").join(name)
    };
    path.set_extension("phy");
    path
}

impl PipelineOutput {
    /// 产物在 `out_root` 下的实际路径。
    pub fn paths(&self, out_root: &Path) -> OutputPaths {
        let stem = self.stem.replace('\\', "/");
        OutputPaths {
            mdl: out_root.join(format!("{stem}.mdl")),
            vvd: out_root.join(format!("{stem}.vvd")),
            vtx: out_root.join(format!("{stem}.dx90.vtx")),
            ani: self
                .ani
                .as_ref()
                .map(|_| out_root.join(format!("{stem}.ani"))),
            phy: self
                .phy
                .as_ref()
                .map(|_| out_root.join(physics_output_name(&self.compiled.desc.physics, &stem))),
        }
    }

    /// 编译摘要（`mdlc.exe` 逐行原样输出；GUI 可直接显示在日志面板）。
    pub fn summary_lines(&self, paths: &OutputPaths) -> Vec<String> {
        let d = &self.compiled.desc;
        // ⚠️ 对齐宽度（`{:>10}`）与格式规格都要在**传参时**用 `format!` 做好：
        // `tr_fmt!` 的占位符只认 `%{名}`，写成 `%{n:>10}` 不会被替换（见 §103.2）。
        let mut v = vec![
            crate::tr_fmt!("model       %{name}", name = d.model.name),
            crate::tr_fmt!("version     %{version}", version = d.version()),
            crate::tr_fmt!(
                "checksum    %{sum}  (written into every file, they agree)",
                sum = self.checksum
            ),
            String::new(),
            crate::tr_fmt!("bones       %{n}", n = d.bones.len()),
            crate::tr_fmt!("materials   %{n}", n = d.materials.textures.len()),
            crate::tr_fmt!("body part   %{n}", n = d.bodyparts.len()),
            crate::tr_fmt!("vertices    %{n}", n = self.compiled.total_vertices()),
            crate::tr_fmt!("triangles   %{n}", n = self.compiled.total_triangles()),
            String::new(),
            crate::tr_fmt!(
                "MDL         %{size} bytes  %{path}",
                size = format!("{:>10}", self.mdl.len()),
                path = paths.mdl.display()
            ),
            crate::tr_fmt!(
                "VVD         %{size} bytes  %{path}",
                size = format!("{:>10}", self.vvd.len()),
                path = paths.vvd.display()
            ),
            crate::tr_fmt!(
                "VTX         %{size} bytes  %{path}",
                size = format!("{:>10}", self.vtx.len()),
                path = paths.vtx.display()
            ),
        ];
        if let (Some(p), Some(b)) = (&paths.phy, &self.phy) {
            v.push(crate::tr_fmt!(
                "PHY         %{size} bytes  %{path}",
                size = format!("{:>10}", b.len()),
                path = p.display()
            ));
        }
        v.push(String::new());
        v.push(
            crate::tr("**compiled successfully** (every file passed its layout self-check)")
                .to_string(),
        );
        v
    }
}

/// 编译 + 构造全部产物字节，**不落盘**。
///
/// `base` 是相对路径（`$modelname` / `$cdmaterials` / SMD 引用）的解析基准，
/// 通常是被编译文件（`.toml` / `.qc`）所在目录。
///
/// 这一步只读文件（SMD / FBX / `.vta`），不建目录、不写文件。
pub fn build(
    desc: &ModelDesc,
    base: &Path,
    opts: PipelineOptions,
) -> Result<PipelineOutput, PipelineError> {
    // ---- ① 编译：描述 + SMD/FBX → 编译期 IR ----
    //
    // `compile()` 内部第一件事就是 `desc.validate()`，所以这里不必再查一遍。
    let compiled = hotpath::measure_block!("pipeline: compile()", {
        compile(desc, base).map_err(PipelineError::Compile)?
    });

    build_compiled(compiled, base, opts)
}

pub fn build_compiled(
    mut compiled: CompiledModelDesc,
    base: &Path,
    opts: PipelineOptions,
) -> Result<PipelineOutput, PipelineError> {
    if compiled.desc.version() == 48 {
        compiled.desc.physics.weld_normal.get_or_insert(0.999);
        compiled.desc.physics.max_convex_pieces.get_or_insert(40);
    }
    crate::uv_checks::validate(&compiled).map_err(|message| {
        PipelineError::Compile(vec![crate::compile::CompileError::Message {
            at: "$checkuv".into(),
            message,
        }])
    })?;
    let desc = &compiled.desc;

    // ---- ② 碰撞 SMD：**必须在 `write_mdl` 之前**解析 ----
    //
    // 因为 `physicsbone`（`mstudiobone_t` `+0xAC`）要写进**骨骼表**，
    // 而骨骼表是 `write_mdl` 产出的。碰撞几何来自**单独的 SMD**
    // （官方 `$collisionmodel` / `$collisionjoints`），所以要提前读。
    //
    // 解析结果留到下面构造 `.phy` 时**复用**，避免读两次。
    let collision_smd = match desc.physics.smd.as_deref() {
        None => None,
        Some(rel_smd) => {
            let p = resolve_smd_path(base, rel_smd).map_err(PipelineError::ResolveCollisionSmd)?;
            let text =
                std::fs::read_to_string(&p).map_err(|source| PipelineError::ReadCollisionSmd {
                    path: p.clone(),
                    source,
                })?;
            let mut source = crate::smd::parse_smd(&text).map_err(|source| {
                PipelineError::ParseCollisionSmd {
                    path: p.clone(),
                    source,
                }
            })?;
            crate::bone_rename::source(&mut source, &desc.model.bone_renames).map_err(
                |detail| {
                    PipelineError::BuildPhy(phy::PhyError::BadParameter {
                        what: "model.bone_renames",
                        detail,
                    })
                },
            )?;
            crate::bone_hierarchy::source(&mut source, &desc.model.bone_hierarchy).map_err(
                |detail| {
                    PipelineError::BuildPhy(phy::PhyError::BadParameter {
                        what: "model.bone_hierarchy",
                        detail,
                    })
                },
            )?;
            if let Some(scale) = desc.physics.input_scale {
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(PipelineError::BuildPhy(phy::PhyError::BadParameter {
                        what: "physics.input_scale",
                        detail: "must be positive and finite".into(),
                    }));
                }
                crate::compile::scale_source(&mut source, scale);
            }
            if desc.model.static_prop {
                crate::compile::flatten_static_collision(&mut source, desc);
            }
            Some(source)
        }
    };

    // ---- ③ `physicsbone`：只有碰撞几何能提供 solid → 骨骼的映射 ----
    //
    // ⚠️ 用**骨骼表**的父链（`compiled.desc.bones`）而不是碰撞 SMD 的
    // `nodes` —— 两者在静态道具 / 骨骼塌缩后可能不同，而落盘的
    // `physicsbone` 下标必须对**最终骨骼表**成立。
    if let Some(cs) = &collision_smd {
        compiled.physics_bone = Some(
            phy::physics_bone_table_for_model(cs, &compiled.desc.bones, &desc.physics)
                .map_err(PipelineError::BuildPhy)?,
        );
    }

    if let Some(name) = compiled.desc.physics.name.as_deref()
        && !ModelDesc::valid_output_name(name)
    {
        return Err(PipelineError::InvalidOutputPath {
            path: PathBuf::from(name),
        });
    }
    if let Some(smd) = &collision_smd
        && compiled.desc.physics.auto_mass
    {
        let pose_world = (!compiled.desc.model.static_prop)
            .then_some(&compiled)
            .and_then(|compiled| compiled.sequences.first())
            .and_then(|sequence| sequence.frames.first())
            .map(|frame| phy::sequence_pose_world(&compiled.desc.bones, frame));
        let mass = phy::compute_mass_for_model(
            smd,
            &compiled.desc.physics,
            &compiled.desc.bones,
            compiled
                .desc
                .model
                .surface_prop
                .as_deref()
                .unwrap_or("default"),
            pose_world.as_ref(),
        )
        .map_err(PipelineError::BuildPhy)?;
        compiled.desc.physics.mass = Some(mass);
    }
    let desc = &compiled.desc;

    // ---- ④ `.mdl` ----
    let out = hotpath::measure_block!("pipeline: write_mdl", {
        write_mdl(&compiled).map_err(PipelineError::WriteMdl)?
    });

    // ---- ⑤ `.vvd`（单 / 多 LOD 自动分派）----
    //
    // `vvd` 只在块内用（自检读它的字段）；逃逸出去的只有字节。
    let vvd_bytes = hotpath::measure_block!("pipeline: build_vvd + to_bytes", {
        let vvd =
            crate::lod::build_vvd(&compiled, out.checksum).map_err(PipelineError::BuildVvd)?;
        let bytes = vvd.to_bytes().map_err(PipelineError::EncodeVvd)?;
        // 写出后立刻自检 —— 偏移/长度对不上说明我们的写出器有 bug。
        crate::vvd::check_invariants(&vvd, bytes.len()).map_err(PipelineError::CheckVvd)?;
        bytes
    });

    // ---- ⑥ `.dx90.vtx`：没有它模型在游戏里根本不渲染 ----
    let vtx = hotpath::measure_block!("pipeline: write_vtx", {
        vtx_writer::write_vtx_with(
            &compiled,
            VtxOptions {
                optimize_vertex_cache: (desc.model.optimize_vtx || opts.optimize_vtx)
                    && !desc.model.preserve_triangle_order,
            },
        )
        .map_err(PipelineError::WriteVtx)?
    });
    vtx_writer::check_invariants(&vtx, &compiled).map_err(PipelineError::CheckVtx)?;

    // ---- ⑦ 文件名：`out_root` + 模型名（去掉 `.mdl` 换扩展名）----
    let rel = desc.output_name();
    let stem = rel.strip_suffix(".mdl").unwrap_or(&rel).to_string();

    // ---- ⑧ `.phy`（可选，由 `[physics].smd` 触发）----
    let phy_bytes = match &collision_smd {
        None => None,
        Some(smd) => {
            // `editparams.modelname` 用**不带扩展名**的模型名。
            let phys_name = Path::new(&stem)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unnamed".to_string());
            // `[physics].mass` 与 `.mdl` 头部 `+0x148` 是**同一个值**
            // （`write.cpp:2092` `phdr->mass = GetCollisionModelMass();`）。
            //
            // `joints` 走 ragdoll（每骨骼一个 solid），
            // 否则走 prop 形态（单 solid，可选 `concave`）。
            let mass = desc.physics.effective_mass();
            // prop 形态的 solid `name` = **碰撞 SMD 的 basename** ——
            // 官方 `ProcessSingleBody`（`collisionmodel.cpp:1563-1571`）用
            // `Q_FileBase(pmodel->filename, ...)`，与 `$modelname` 无关。
            let collision_smd_name = desc
                .physics
                .smd
                .as_deref()
                .map(|p| {
                    Path::new(p)
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.to_string())
                })
                .unwrap_or_else(|| phys_name.clone());
            // `surfaceprop` 从**模型头**取（实测 2498/2498 恒等）。
            let surface_prop = desc.model.surface_prop.as_deref().unwrap_or("default");
            // 官方 `ConvertToWorldSpace` 用的是**第一个序列第 0 帧**
            // （`collisionmodel.cpp:713` `CalcBoneTransforms( g_panimation[0], 0, ... )`）；
            // 碰撞 SMD 自己的姿态完全不参与。没有序列时传 `None`。
            let pose_world = (!desc.model.static_prop)
                .then_some(&compiled)
                .and_then(|compiled| compiled.sequences.first())
                .and_then(|s| s.frames.first())
                .map(|f| phy::sequence_pose_world(&compiled.desc.bones, f));
            let ident = phy::PhyIdentity {
                model_name: &phys_name,
                collision_smd_name: &collision_smd_name,
                surface_prop,
            };
            let built = if desc.physics.joints {
                phy::build_ragdoll_phy_for_model(
                    smd,
                    ident,
                    out.checksum as u32,
                    mass,
                    &desc.physics,
                    &compiled.desc.bones,
                )
            } else {
                phy::build_phy_from_smd(
                    smd,
                    ident,
                    out.checksum as u32,
                    mass,
                    &desc.physics,
                    pose_world.as_ref(),
                )
            };
            Some(built.map_err(PipelineError::BuildPhy)?)
        }
    };
    // 写出前自检 —— 失败说明是本实现的 bug。
    if let Some(b) = &phy_bytes {
        phy::check_invariants(b).map_err(PipelineError::CheckPhy)?;
    }

    let mut output = PipelineOutput {
        stem,
        mdl: out.bytes,
        vvd: vvd_bytes,
        vtx: vtx.bytes,
        // `$animblocksize` 的外置动画块。
        ani: out.ani,
        phy: phy_bytes,
        checksum: out.checksum,
        compiled,
    };
    crate::root_lod::apply(&mut output)
        .map_err(|error| PipelineError::WriteMdl(WriteError::Internal(error)))?;
    Ok(output)
}

/// 把 [`build`] 的产物落盘（自动建目录），返回实际路径。
///
/// 目录结构由模型名决定：模型名 `models/mymod/x.mdl` 会在 `out_root` 下
/// 建出 `models/mymod/`。
pub fn write_files(out: &PipelineOutput, out_root: &Path) -> Result<OutputPaths, PipelineError> {
    write_files_with_rename(out, out_root, &mut |from, to| std::fs::rename(from, to))
}

struct ExportStaging {
    path: PathBuf,
    preserve: bool,
}

impl Drop for ExportStaging {
    fn drop(&mut self) {
        if !self.preserve {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

struct ExportFile<'a> {
    target: PathBuf,
    bytes: Option<&'a [u8]>,
    staged: PathBuf,
    backup: PathBuf,
    backed_up: bool,
    installed: bool,
}

fn write_files_with_rename(
    out: &PipelineOutput,
    out_root: &Path,
    rename: &mut impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<OutputPaths, PipelineError> {
    if !ModelDesc::valid_output_name(&out.stem) {
        return Err(PipelineError::InvalidOutputPath {
            path: PathBuf::from(&out.stem),
        });
    }
    if let Some(name) = out.compiled.desc.physics.name.as_deref()
        && !ModelDesc::valid_output_name(name)
    {
        return Err(PipelineError::InvalidOutputPath {
            path: PathBuf::from(name),
        });
    }
    let paths = out.paths(out_root);
    std::fs::create_dir_all(out_root).map_err(|source| PipelineError::CreateDir {
        path: out_root.to_path_buf(),
        source,
    })?;
    let root = std::fs::canonicalize(out_root).map_err(|source| PipelineError::CreateDir {
        path: out_root.to_path_buf(),
        source,
    })?;
    let normalized_stem = out.stem.replace('\\', "/");
    let mut parent = root.clone();
    for component in normalized_stem
        .split('/')
        .collect::<Vec<_>>()
        .split_last()
        .unwrap()
        .1
    {
        parent.push(component);
        std::fs::create_dir_all(&parent).map_err(|source| PipelineError::CreateDir {
            path: parent.clone(),
            source,
        })?;
        parent = std::fs::canonicalize(&parent).map_err(|source| PipelineError::CreateDir {
            path: parent.clone(),
            source,
        })?;
        if !parent.starts_with(&root) {
            return Err(PipelineError::InvalidOutputPath { path: parent });
        }
    }
    let mut companions = vec![
        (paths.mdl.clone(), Some(out.mdl.as_slice())),
        (paths.vvd.clone(), Some(out.vvd.as_slice())),
        (paths.vtx.clone(), Some(out.vtx.as_slice())),
        (paths.mdl.with_extension("ani"), out.ani.as_deref()),
        (
            out_root.join(physics_output_name(&out.compiled.desc.physics, &out.stem)),
            out.phy.as_deref(),
        ),
    ];
    if out.compiled.desc.physics.name.is_some()
        && companions[4].0 != paths.mdl.with_extension("phy")
    {
        companions.push((paths.mdl.with_extension("phy"), None));
    }
    for (target, _) in &mut companions {
        let relative =
            target
                .strip_prefix(out_root)
                .map_err(|_| PipelineError::InvalidOutputPath {
                    path: target.clone(),
                })?;
        let mut target_parent = root.clone();
        for component in relative
            .parent()
            .into_iter()
            .flat_map(|path| path.components())
        {
            target_parent.push(component);
            std::fs::create_dir_all(&target_parent).map_err(|source| PipelineError::CreateDir {
                path: target_parent.clone(),
                source,
            })?;
            target_parent = std::fs::canonicalize(&target_parent).map_err(|source| {
                PipelineError::CreateDir {
                    path: target_parent.clone(),
                    source,
                }
            })?;
            if !target_parent.starts_with(&root) {
                return Err(PipelineError::InvalidOutputPath {
                    path: target_parent,
                });
            }
        }
        *target = target_parent.join(target.file_name().unwrap());
    }
    // Optional companions participate so obsolete .ani/.phy files are removed.
    for (path, _) in &companions {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PipelineError::InvalidOutputPath { path: path.clone() });
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(PipelineError::NonRegularOutput { path: path.clone() });
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(PipelineError::WriteFile {
                    path: path.clone(),
                    source,
                });
            }
        }
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);
    let mut staging = loop {
        let id = NEXT_EXPORT.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".mdlc-export-{}-{id}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                break ExportStaging {
                    path,
                    preserve: false,
                };
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(PipelineError::CreateDir { path, source }),
        }
    };
    let mut files: Vec<_> = companions
        .into_iter()
        .enumerate()
        .map(|(index, (path, bytes))| ExportFile {
            target: path,
            bytes,
            staged: staging.path.join(format!("new-{index}")),
            backup: staging.path.join(format!("old-{index}")),
            backed_up: false,
            installed: false,
        })
        .collect();

    // Stage and flush every file before moving any existing companion.
    for file in &files {
        if let Some(bytes) = file.bytes {
            use std::io::Write;
            let result = (|| {
                let mut staged = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&file.staged)?;
                staged.write_all(bytes)?;
                staged.sync_all()
            })();
            result.map_err(|source| PipelineError::WriteFile {
                path: file.target.clone(),
                source,
            })?;
        }
    }

    let publication = (|| {
        for file in &mut files {
            // Targets may change while files are staged.
            match std::fs::symlink_metadata(&file.target) {
                Ok(metadata) if metadata.is_file() => {
                    rename(&file.target, &file.backup)
                        .map_err(|source| (file.target.clone(), source))?;
                    file.backed_up = true;
                }
                Ok(_) => {
                    return Err((
                        file.target.clone(),
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "output target changed to a nonregular file",
                        ),
                    ));
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => return Err((file.target.clone(), source)),
            }
        }
        for file in &mut files {
            if file.bytes.is_some() {
                rename(&file.staged, &file.target)
                    .map_err(|source| (file.target.clone(), source))?;
                file.installed = true;
            }
        }
        Ok(())
    })();
    if let Err((path, source)) = publication {
        let mut rollback_errors = Vec::new();
        for file in files.iter_mut().rev() {
            if file.installed {
                if let Err(error) = std::fs::remove_file(&file.target) {
                    rollback_errors
                        .push(format!("cannot remove {}: {error}", file.target.display()));
                    continue;
                }
                file.installed = false;
            }
            if file.backed_up {
                // Never overwrite a file created by another process during rollback.
                match std::fs::symlink_metadata(&file.target) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        if let Err(error) = rename(&file.backup, &file.target) {
                            rollback_errors
                                .push(format!("cannot restore {}: {error}", file.target.display()));
                        } else {
                            file.backed_up = false;
                        }
                    }
                    Ok(_) => rollback_errors.push(format!(
                        "cannot restore {}; target exists",
                        file.target.display()
                    )),
                    Err(error) => rollback_errors
                        .push(format!("cannot inspect {}: {error}", file.target.display())),
                }
            }
        }
        staging.preserve = !rollback_errors.is_empty();
        return Err(PipelineError::ExportTransaction {
            path,
            source,
            recovery_dir: staging.preserve.then(|| staging.path.clone()),
            rollback_errors,
        });
    }
    std::fs::remove_dir_all(&staging.path).map_err(|source| PipelineError::ExportCleanup {
        path: staging.path.clone(),
        source,
    })?;
    Ok(paths)
}

/// [`build`] + [`write_files`] 的便捷组合。
pub fn build_and_write(
    desc: &ModelDesc,
    base: &Path,
    out_root: &Path,
    opts: PipelineOptions,
) -> Result<OutputPaths, PipelineError> {
    let out = build(desc, base, opts)?;
    write_files(&out, out_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdl_writer::{BONE_SIZE, off};

    /// 每个用例用**唯一**的临时目录：测试并行跑，共用目录会互相删文件。
    fn tmpdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-pipe-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    /// 渲染网格：2 根骨骼，一个挂在 `tip` 上的三角形。
    const MESH_SMD: &str = r#"version 1
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

    /// 碰撞网格：**两根骨骼各一个盒子** ⟹ 两个 solid。
    ///
    /// 单 solid 时 `physicsbone` 天然全 0（与语料 2459/2459 一致），
    /// 证明不了「编排真的填了它」—— 所以这里必须有两个。
    fn collision_smd() -> String {
        let mut s = String::from("version 1\nnodes\n");
        s.push_str("0 \"root\" -1\n1 \"tip\" 0\n");
        s.push_str("end\nskeleton\ntime 0\n");
        s.push_str("0 0 0 0 0 0 0\n1 0 0 40 0 0 0\n");
        s.push_str("end\ntriangles\nphy\n");
        for bone in 0..2 {
            let z = bone as f32 * 40.0;
            let verts: [[f32; 3]; 8] = [
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
        s
    }

    /// 带碰撞 SMD 的描述；`physics_smd` 决定 `[physics].smd` 写什么。
    fn desc_toml(physics_smd: &str) -> String {
        format!(
            r#"
[model]
name = "models/test/minimal.mdl"
surface_prop = "metal"

[physics]
smd = "{physics_smd}"
joints = true
mass = 100.0

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
smd = "a.smd"
"#
        )
    }

    /// 读出第 `i` 根骨骼的 `physicsbone`（`mstudiobone_t` `+0xAC`）。
    ///
    /// `bone_off` 是私有模块里的常量，所以这里用公开的
    /// [`off::BONE_OFFSET`] + [`BONE_SIZE`] 自算。
    fn physics_bone_at(mdl: &[u8], i: usize) -> i32 {
        let g = |at: usize| i32::from_le_bytes(mdl[at..at + 4].try_into().unwrap());
        let bone_off = g(off::BONE_OFFSET) as usize;
        g(bone_off + i * BONE_SIZE + 0xAC)
    }

    fn parse(toml: &str) -> ModelDesc {
        ModelDesc::from_toml(toml).expect("描述应能解析")
    }

    /// 同 [`desc_toml`]，但**去掉整个 `[physics]` 表**（无碰撞 SMD）。
    fn plain_toml() -> String {
        desc_toml("phys.smd").replace(
            "[physics]\nsmd = \"phys.smd\"\njoints = true\nmass = 100.0\n\n",
            "",
        )
    }

    /// 一个 `n×n` 的四边形网格（三角形数够多，缓存优化才有可重排的余地）。
    ///
    /// [`MESH_SMD`] 只有一个三角形 —— `meshopt::optimize_vertex_cache`
    /// 在它上面**没有可重排的空间**，所以拿它测 `optimize_vtx` 会假阴性。
    fn grid_smd(n: usize) -> String {
        let mut s = String::from("version 1\nnodes\n0 \"root\" -1\n1 \"tip\" 0\nend\n");
        s.push_str("skeleton\ntime 0\n0 0 0 0 0 0 0\n1 0 0 8 0 0 0\nend\ntriangles\nmyprop\n");
        let at = |(i, j): (usize, usize)| {
            [
                i as f32 * 4.0 - n as f32 * 2.0,
                j as f32 * 4.0 - n as f32 * 2.0,
                0.0f32,
            ]
        };
        let uv = [
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (0.0, 0.0),
            (1.0, 1.0),
            (0.0, 1.0),
        ];
        for i in 0..n {
            for j in 0..n {
                let quad = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                // 两个三角形：0-1-2 与 0-2-3。
                for k in [0usize, 1, 2, 0, 2, 3] {
                    let p = at(quad[k]);
                    let (u, v) = uv[k];
                    s.push_str(&format!(
                        "1 {} {} {} 0 0 1 {u} {v} 1 1 1.000000\n",
                        p[0], p[1], p[2]
                    ));
                }
            }
        }
        s.push_str("end\n");
        s
    }

    /// ⭐ 编排**真的**把碰撞 SMD 接进了 `physicsbone`。
    ///
    /// 这是本模块存在的理由：第三方工具自己串管线时最常漏的一步。
    /// 漏了以后 `.mdl` / `.vvd` / `.dx90.vtx` 全对，**只有这个字段悄悄全 0**。
    ///
    /// 判据（对照官方 `rjd1`，见 `src/phy.rs` 的同名测试）：
    /// 两根骨骼各一个盒子 ⟹ `physicsbone = [0, 1]`。
    #[test]
    fn build_wires_the_collision_smd_into_physicsbone() {
        let d = tmpdir("pb");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        let desc = parse(&desc_toml("phys.smd"));

        let out = build(&desc, &d, PipelineOptions::default()).expect("应编译成功");

        assert_eq!(
            out.compiled.physics_bone.as_deref(),
            Some([0i32, 1].as_slice()),
            "IR 里的 physics_bone 应是 [0, 1]"
        );
        // 关键：它必须真的**落进 .mdl 字节**（`mdl_writer` 是唯一消费点）。
        assert_eq!(physics_bone_at(&out.mdl, 0), 0, "骨骼 0 的 physicsbone");
        assert_eq!(
            physics_bone_at(&out.mdl, 1),
            1,
            "骨骼 1 的 physicsbone 必须是 1 —— 全 0 说明碰撞 SMD 没接进来"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn merged_ragdoll_uses_one_solid_in_both_mdl_and_phy() {
        let d = tmpdir("merged-ragdoll");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        let mut desc = parse(&desc_toml("phys.smd"));
        desc.physics.merge.push(crate::model::CollisionPairSpec {
            a: "root".into(),
            b: "tip".into(),
        });
        let mut hand = desc.bones[1].clone();
        hand.name = "hand".into();
        hand.parent = Some("tip".into());
        desc.bones.push(hand);
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        let physics = output.phy.as_ref().unwrap();
        assert_eq!(
            crate::phy::check_invariants(physics).unwrap().solid_count,
            1
        );
        assert_eq!(physics_bone_at(&output.mdl, 0), 0);
        assert_eq!(physics_bone_at(&output.mdl, 1), 0);
        assert_eq!(
            physics_bone_at(&output.mdl, 2),
            0,
            "unbound child follows merged parent"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn reordered_collision_skeleton_binds_solids_by_name() {
        let d = tmpdir("reordered-ragdoll");
        write(&d, "a.smd", MESH_SMD);
        // The child has the lower source index; neither source index is a model index.
        let source = collision_smd()
            .replace(
                "0 \"root\" -1\n1 \"tip\" 0",
                "20 \"root\" -1\n10 \"tip\" 20",
            )
            .replace("0 0 0 0 0 0 0\n1 0 0 40", "20 0 0 0 0 0 0\n10 0 0 40")
            .lines()
            .map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() == 12 {
                    let bone = if fields[0] == "0" { "20" } else { "10" };
                    let mut fields: Vec<String> = fields.into_iter().map(str::to_owned).collect();
                    fields[0] = bone.into();
                    fields[10] = bone.into();
                    fields.join(" ")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        write(&d, "phys.smd", &source);
        let desc = parse(&desc_toml("phys.smd"));
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        let physics = output.phy.as_ref().unwrap();
        let layout = crate::phy::check_invariants(physics).unwrap();
        assert_eq!(layout.solid_count, 2);
        assert_eq!(physics_bone_at(&output.mdl, 0), 0);
        assert_eq!(physics_bone_at(&output.mdl, 1), 1);
        let read = |at| i32::from_le_bytes(physics[at..at + 4].try_into().unwrap());
        let first = crate::phy::PHY_HEADER_SIZE;
        let ledge = crate::phy::SOLID_HEADER_SIZE + crate::phy::COMPACT_SURFACE_SIZE;
        assert_eq!(
            read(first + ledge + 4),
            1,
            "parent solid binds to final model bone 0"
        );
        let second = first + read(first) as usize + 4;
        assert_eq!(
            read(second + ledge + 4),
            2,
            "child solid binds to final model bone 1"
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn skipped_joint_inherits_parent_solid_and_materials_remain_per_joint() {
        let d = tmpdir("skipped-ragdoll");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        let mut desc = parse(&desc_toml("phys.smd"));
        desc.bones[0].surface_prop = Some("wood".into());
        desc.bones[1].surface_prop = Some("flesh".into());
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        let physics = output.phy.unwrap();
        let layout = crate::phy::check_invariants(&physics).unwrap();
        let text = std::str::from_utf8(&physics[layout.solids_end..physics.len() - 1]).unwrap();
        assert!(text.contains("\"surfaceprop\" \"wood\""));
        assert!(text.contains("\"surfaceprop\" \"flesh\""));
        desc.physics.skip.push("tip".into());
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        assert_eq!(
            crate::phy::check_invariants(output.phy.as_ref().unwrap())
                .unwrap()
                .solid_count,
            1
        );
        assert_eq!(physics_bone_at(&output.mdl, 1), 0);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn unused_collision_bones_are_allowed_and_surface_material_is_inherited() {
        let d = tmpdir("unused-collision-bone");
        write(&d, "a.smd", MESH_SMD);
        let source = collision_smd()
            .replace("end\nskeleton", "99 \"unused\" 0\nend\nskeleton")
            .replace("end\ntriangles", "99 0 0 0 0 0 0\nend\ntriangles");
        write(&d, "phys.smd", &source);
        let mut desc = parse(&desc_toml("phys.smd"));
        desc.bones[0].surface_prop = Some("wood".into());
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        let physics = output.phy.unwrap();
        let layout = crate::phy::check_invariants(&physics).unwrap();
        assert_eq!(layout.solid_count, 2);
        let text = std::str::from_utf8(&physics[layout.solids_end..physics.len() - 1]).unwrap();
        assert_eq!(text.matches("\"surfaceprop\" \"wood\"").count(), 2);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn collision_joint_names_are_case_insensitive() {
        let d = tmpdir("joint-name-case");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        let mut desc = parse(&desc_toml("phys.smd"));
        desc.physics
            .joint_overrides
            .push(crate::model::JointOverride {
                bone: "TIP".into(),
                damping: Some(0.25),
                rot_damping: None,
                inertia: None,
                mass_bias: None,
            });
        desc.physics
            .constraints
            .push(crate::model::JointConstraintSpec {
                bone: "TIP".into(),
                axis: "x".into(),
                kind: "limit".into(),
                min: -15.0,
                max: 25.0,
                friction: Some(5.0),
            });
        desc.physics
            .collision_pairs
            .push(crate::model::CollisionPairSpec {
                a: "ROOT".into(),
                b: "TIP".into(),
            });
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        let physics = output.phy.unwrap();
        let layout = crate::phy::check_invariants(&physics).unwrap();
        let text = std::str::from_utf8(&physics[layout.solids_end..physics.len() - 1]).unwrap();
        assert!(text.contains("\"damping\" \"0.250000\""));
        assert!(text.contains("\"xmin\" \"-15.000000\""));
        assert!(text.contains("\"collisionpair\" \"0,1\""));
        desc.physics.constraints.clear();
        desc.physics.collision_pairs.clear();
        desc.physics.joint_overrides.clear();
        desc.physics.merge.push(crate::model::CollisionPairSpec {
            a: "ROOT".into(),
            b: "TIP".into(),
        });
        let output = build(&desc, &d, PipelineOptions::default()).unwrap();
        assert_eq!(
            crate::phy::check_invariants(output.phy.as_ref().unwrap())
                .unwrap()
                .solid_count,
            1
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn parameters_for_merged_or_skipped_joints_are_rejected() {
        let d = tmpdir("eliminated-joint-parameters");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        for merged in [false, true] {
            let mut desc = parse(&desc_toml("phys.smd"));
            if merged {
                desc.physics.merge.push(crate::model::CollisionPairSpec {
                    a: "root".into(),
                    b: "tip".into(),
                });
            } else {
                desc.physics.skip.push("tip".into());
            }
            desc.physics
                .constraints
                .push(crate::model::JointConstraintSpec {
                    bone: "tip".into(),
                    axis: "x".into(),
                    kind: "limit".into(),
                    min: -15.0,
                    max: 25.0,
                    friction: Some(5.0),
                });
            let error = build(&desc, &d, PipelineOptions::default()).unwrap_err();
            assert!(matches!(
                error,
                PipelineError::BuildPhy(crate::phy::PhyError::BadParameter {
                    what: "physics.constraints",
                    ..
                })
            ));
            desc.physics.constraints.clear();
            desc.physics
                .collision_pairs
                .push(crate::model::CollisionPairSpec {
                    a: "root".into(),
                    b: "tip".into(),
                });
            let error = build(&desc, &d, PipelineOptions::default()).unwrap_err();
            assert!(matches!(
                error,
                PipelineError::BuildPhy(crate::phy::PhyError::BadParameter {
                    what: "physics.collision_pairs",
                    ..
                })
            ));
        }
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn unknown_retained_collision_bone_is_rejected() {
        let d = tmpdir("unknown-ragdoll-bone");
        write(&d, "a.smd", MESH_SMD);
        write(
            &d,
            "phys.smd",
            &collision_smd().replace("\"tip\"", "\"unknown\""),
        );
        let desc = parse(&desc_toml("phys.smd"));
        let error = build(&desc, &d, PipelineOptions::default()).unwrap_err();
        assert!(matches!(
            error,
            PipelineError::BuildPhy(crate::phy::PhyError::BadParameter {
                what: "collision skeleton",
                ..
            })
        ));
        std::fs::remove_dir_all(d).unwrap();
    }

    /// 没有碰撞 SMD 时**不写**该字段（保持 0），与官方一致
    /// （实测单 solid 2459/2459 全 0）。
    #[test]
    fn build_leaves_physicsbone_alone_without_a_collision_smd() {
        let d = tmpdir("nopb");
        write(&d, "a.smd", MESH_SMD);
        let desc = parse(&plain_toml());

        let out = build(&desc, &d, PipelineOptions::default()).expect("应编译成功");

        assert!(out.compiled.physics_bone.is_none());
        assert!(out.phy.is_none(), "没有碰撞 SMD 就不该有 .phy");
        assert_eq!(physics_bone_at(&out.mdl, 0), 0);
        assert_eq!(physics_bone_at(&out.mdl, 1), 0);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// 有碰撞 SMD ⟹ 产出 `.phy`，且 `checksum` 与 `.mdl` 同源（配对令牌）。
    #[test]
    fn build_produces_phy_only_with_a_collision_smd() {
        let d = tmpdir("phy");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        let desc = parse(&desc_toml("phys.smd"));

        let out = build(&desc, &d, PipelineOptions::default()).expect("应编译成功");

        let phy = out.phy.as_ref().expect("应有 .phy");
        crate::phy::check_invariants(phy).expect("自检应通过");
        // `.mdl` 头部 `+0x08` 就是 checksum。
        let mdl_checksum = i32::from_le_bytes(
            out.mdl[off::CHECKSUM..off::CHECKSUM + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(mdl_checksum, out.checksum);
        assert_eq!(
            i32::from_le_bytes(out.vvd[0x08..0x0C].try_into().unwrap()),
            out.checksum,
            "VVD 的 checksum 必须与 MDL 相同"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// `write_files` 按模型名建目录，并把每个产物写到算出来的路径上。
    #[test]
    fn write_files_creates_dirs_and_writes_every_product() {
        let d = tmpdir("write");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", &collision_smd());
        let desc = parse(&desc_toml("phys.smd"));
        let out = build(&desc, &d, PipelineOptions::default()).expect("应编译成功");

        // 输出目录**故意不预先建** —— 模型名里有 `models/test/` 两级。
        let out_root = d.join("nested/out");
        let paths = write_files(&out, &out_root).expect("应写出");

        assert_eq!(paths.mdl, out_root.join("models/test/minimal.mdl"));
        assert_eq!(paths.vvd, out_root.join("models/test/minimal.vvd"));
        assert_eq!(paths.vtx, out_root.join("models/test/minimal.dx90.vtx"));
        assert_eq!(
            paths.phy.as_deref(),
            Some(out_root.join("models/test/minimal.phy").as_path())
        );
        assert_eq!(std::fs::read(&paths.mdl).unwrap(), out.mdl);
        assert_eq!(std::fs::read(&paths.vvd).unwrap(), out.vvd);
        assert_eq!(std::fs::read(&paths.vtx).unwrap(), out.vtx);
        assert_eq!(
            std::fs::read(paths.phy.as_ref().unwrap()).unwrap(),
            out.phy.clone().unwrap()
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn automatic_mass_is_identical_in_mdl_header_and_physics_output() {
        let dir = tmpdir("automatic-mass");
        write(&dir, "a.smd", MESH_SMD);
        write(&dir, "phys.smd", &collision_smd());
        let mut desc = parse(&desc_toml("phys.smd"));
        desc.physics.auto_mass = true;
        desc.physics.surface_properties = vec![crate::model::SurfacePhysics {
            name: "metal".into(),
            density: 1000.0,
            thickness: 0.0,
        }];
        let output = build(&desc, &dir, PipelineOptions::default()).unwrap();
        let mdl_mass = f32::from_le_bytes(output.mdl[0x148..0x14c].try_into().unwrap());
        let expected = 2000.0 * 0.0254_f32.powi(3) * 1000.0;
        assert!((mdl_mass - expected).abs() < 1e-4);
        let physics = output.phy.as_ref().unwrap();
        let layout = crate::phy::check_invariants(physics).unwrap();
        let text = std::str::from_utf8(&physics[layout.solids_end..physics.len() - 1]).unwrap();
        let total: f32 = text
            .lines()
            .find_map(|line| line.strip_prefix("\"totalmass\" \""))
            .unwrap()
            .trim_end_matches('"')
            .parse()
            .unwrap();
        assert!((total - mdl_mass).abs() < 1e-4);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn physics_name_changes_output_path_and_transaction_preserves_other_products() {
        let dir = tmpdir("physics-name");
        write(&dir, "a.smd", MESH_SMD);
        write(&dir, "phys.smd", &collision_smd());
        let mut desc = parse(&desc_toml("phys.smd"));
        desc.physics.name = Some("custom/collision.mdl".into());
        let output = build(&desc, &dir, PipelineOptions::default()).unwrap();
        let root = dir.join("out");
        let paths = write_files(&output, &root).unwrap();
        assert_eq!(
            paths.phy.as_deref(),
            Some(root.join("models/custom/collision.phy").as_path())
        );
        assert_eq!(
            std::fs::read(paths.phy.as_ref().unwrap()).unwrap(),
            *output.phy.as_ref().unwrap()
        );
        assert!(!paths.mdl.with_extension("phy").exists());
        assert!(paths.mdl.exists());
        for invalid in [
            "../escape",
            "/absolute",
            "models/../../escape",
            "C:\\escape",
        ] {
            desc.physics.name = Some(invalid.into());
            assert!(matches!(
                build(&desc, &dir, PipelineOptions::default()),
                Err(PipelineError::InvalidOutputPath { .. })
            ));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn export_fixture(dir: &Path) -> PipelineOutput {
        write(dir, "a.smd", MESH_SMD);
        build(&parse(&plain_toml()), dir, PipelineOptions::default()).unwrap()
    }

    fn assert_no_export_staging(parent: &Path) {
        assert!(std::fs::read_dir(parent).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mdlc-export-")
        }));
    }

    #[test]
    fn export_preflight_rejects_companion_directory_without_changing_files() {
        let dir = tmpdir("export-nonregular");
        let output = export_fixture(&dir);
        let root = dir.join("out");
        let paths = output.paths(&root);
        let parent = paths.mdl.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(&paths.mdl, b"previous-mdl").unwrap();
        std::fs::write(&paths.vvd, b"previous-vvd").unwrap();
        std::fs::create_dir(&paths.vtx).unwrap();
        assert!(matches!(
            write_files(&output, &root),
            Err(PipelineError::NonRegularOutput { .. })
        ));
        assert_eq!(std::fs::read(&paths.mdl).unwrap(), b"previous-mdl");
        assert_eq!(std::fs::read(&paths.vvd).unwrap(), b"previous-vvd");
        assert!(paths.vtx.is_dir());
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 3);
        assert_no_export_staging(parent);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_replacement_writes_all_companions_and_removes_stale_optional_files() {
        let dir = tmpdir("export-replace");
        let output = export_fixture(&dir);
        let root = dir.join("out");
        let paths = write_files(&output, &root).unwrap();
        std::fs::write(paths.mdl.with_extension("ani"), b"obsolete-animation").unwrap();
        std::fs::write(paths.mdl.with_extension("phy"), b"obsolete-physics").unwrap();
        let mut desc = parse(&plain_toml());
        desc.model.checksum = Some(output.checksum.wrapping_add(1));
        let replacement = build(&desc, &dir, PipelineOptions::default()).unwrap();
        write_files(&replacement, &root).unwrap();
        for (path, expected) in [
            (&paths.mdl, &replacement.mdl),
            (&paths.vvd, &replacement.vvd),
            (&paths.vtx, &replacement.vtx),
        ] {
            assert_eq!(std::fs::read(path).unwrap(), *expected);
        }
        assert!(!paths.mdl.with_extension("ani").exists());
        assert!(!paths.mdl.with_extension("phy").exists());
        assert_no_export_staging(paths.mdl.parent().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_export_restores_previous_companions_and_cleans_staging() {
        let dir = tmpdir("export-rollback");
        let output = export_fixture(&dir);
        let root = dir.join("out");
        let paths = write_files(&output, &root).unwrap();
        std::fs::write(&paths.mdl, b"previous-mdl").unwrap();
        std::fs::write(&paths.vvd, b"previous-vvd").unwrap();
        std::fs::remove_file(&paths.vtx).unwrap();
        let stale = paths.mdl.with_extension("phy");
        std::fs::write(&stale, b"previous-phy").unwrap();
        let mut failed = false;
        let result = write_files_with_rename(&output, &root, &mut |from, to| {
            if !failed && to == paths.vvd {
                failed = true;
                return Err(std::io::Error::other("injected replacement failure"));
            }
            std::fs::rename(from, to)
        });
        assert!(matches!(
            result,
            Err(PipelineError::ExportTransaction {
                recovery_dir: None,
                ..
            })
        ));
        assert!(failed);
        assert_eq!(std::fs::read(&paths.mdl).unwrap(), b"previous-mdl");
        assert_eq!(std::fs::read(&paths.vvd).unwrap(), b"previous-vvd");
        assert_eq!(std::fs::read(&stale).unwrap(), b"previous-phy");
        assert!(!paths.vtx.exists());
        assert_no_export_staging(paths.mdl.parent().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_export_preserves_recovery_backups_when_restore_fails() {
        let dir = tmpdir("export-recovery");
        let output = export_fixture(&dir);
        let root = dir.join("out");
        let paths = write_files(&output, &root).unwrap();
        std::fs::write(&paths.mdl, b"previous-mdl").unwrap();
        std::fs::write(&paths.vvd, b"previous-vvd").unwrap();
        let mut publication_failed = false;
        let result = write_files_with_rename(&output, &root, &mut |from, to| {
            if !publication_failed && to == paths.vvd {
                publication_failed = true;
                return Err(std::io::Error::other("injected replacement failure"));
            }
            if publication_failed && to == paths.mdl {
                return Err(std::io::Error::other("injected restore failure"));
            }
            std::fs::rename(from, to)
        });
        let Err(PipelineError::ExportTransaction {
            recovery_dir: Some(recovery),
            rollback_errors,
            ..
        }) = result
        else {
            panic!("expected an export error with preserved recovery backups");
        };
        assert_eq!(rollback_errors.len(), 1);
        assert_eq!(std::fs::read(&paths.vvd).unwrap(), b"previous-vvd");
        assert!(recovery.is_dir());
        assert!(std::fs::read_dir(&recovery).unwrap().any(|entry| {
            std::fs::read(entry.unwrap().path()).is_ok_and(|bytes| bytes == b"previous-mdl")
        }));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_export_before_backups_cleans_staging_and_preserves_all_outputs() {
        let dir = tmpdir("export-backup-failure");
        let output = export_fixture(&dir);
        let root = dir.join("out");
        let paths = write_files(&output, &root).unwrap();
        let before = std::fs::read(&paths.mdl).unwrap();
        let result = write_files_with_rename(&output, &root, &mut |_, _| {
            Err(std::io::Error::other("injected backup failure"))
        });
        assert!(matches!(
            result,
            Err(PipelineError::ExportTransaction {
                recovery_dir: None,
                ..
            })
        ));
        assert_eq!(std::fs::read(&paths.mdl).unwrap(), before);
        assert_eq!(std::fs::read(&paths.vvd).unwrap(), output.vvd);
        assert_eq!(std::fs::read(&paths.vtx).unwrap(), output.vtx);
        assert_no_export_staging(paths.mdl.parent().unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// 摘要的行数与内容 —— `mdlc.exe` 逐行原样输出，GUI 直接显示。
    ///
    /// 不断言具体字节数（会随实现变化），只断言**结构**。
    #[test]
    fn summary_lines_mirror_the_cli_output() {
        let d = tmpdir("summary");
        write(&d, "a.smd", MESH_SMD);
        let desc = parse(&plain_toml());
        let out = build(&desc, &d, PipelineOptions::default()).expect("应编译成功");
        let paths = out.paths(Path::new("out"));

        let lines = crate::test_locale::with_english(|| out.summary_lines(&paths));
        assert_eq!(lines.len(), 15, "没有 .phy 时 15 行：{lines:?}");
        assert_eq!(lines[0], "model       models/test/minimal.mdl");
        assert!(lines[3].is_empty(), "第 4 行是空行");
        assert!(lines[9].is_empty(), "第 10 行是空行");
        assert_eq!(
            lines[14],
            "**compiled successfully** (every file passed its layout self-check)"
        );

        // 有 `.phy` 时多一行，插在 VTX 之后、空行之前。
        write(&d, "phys.smd", &collision_smd());
        let with_phy = parse(&desc_toml("phys.smd"));
        let out = build(&with_phy, &d, PipelineOptions::default()).expect("应编译成功");
        let lines =
            crate::test_locale::with_english(|| out.summary_lines(&out.paths(Path::new("out"))));
        assert_eq!(lines.len(), 16, "有 .phy 时 16 行：{lines:?}");
        assert!(lines[13].starts_with("PHY "), "第 14 行应是 PHY：{lines:?}");
        assert!(lines[14].is_empty());
        assert_eq!(
            lines[15],
            "**compiled successfully** (every file passed its layout self-check)"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// 错误分类 → 退出码。`mdlc.exe` 的契约（既有探针按码判定）：
    /// 1 = 编译 / 写出 / 自检；2 = 路径解析 / 读文件 / 建目录 / 写文件。
    #[test]
    fn error_kinds_map_to_the_cli_exit_codes() {
        assert_eq!(PipelineErrorKind::Build.exit_code(), 1);
        assert_eq!(PipelineErrorKind::Io.exit_code(), 2);

        let compile = PipelineError::Compile(vec![CompileError::Message {
            at: "x".into(),
            message: "y".into(),
        }]);
        assert_eq!(compile.kind(), PipelineErrorKind::Build);
        // 解析碰撞 SMD 失败属 **Build**（文件读到了，内容不对）。
        assert_eq!(
            PipelineError::ParseCollisionSmd {
                path: PathBuf::from("c.smd"),
                source: SmdError {
                    line: 1,
                    message: "坏".into(),
                },
            }
            .kind(),
            PipelineErrorKind::Build
        );
        assert_eq!(
            PipelineError::WriteMdl(WriteError::Internal("w".into())).kind(),
            PipelineErrorKind::Build
        );
        // 读不到 / 建目录 / 写文件属 **Io**（退出码 2）。
        assert_eq!(
            PipelineError::ReadCollisionSmd {
                path: PathBuf::from("i.smd"),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "i"),
            }
            .kind(),
            PipelineErrorKind::Io
        );
        assert_eq!(
            PipelineError::CreateDir {
                path: PathBuf::from("d"),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "d"),
            }
            .kind(),
            PipelineErrorKind::Io
        );
        assert_eq!(
            PipelineError::WriteFile {
                path: PathBuf::from("f"),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "f"),
            }
            .kind(),
            PipelineErrorKind::Io
        );

        // 编译失败是多行（首行计数），其余是单行。
        // 文案走译文表 ⟹ 断言英文源文前先钉住语言。
        let lines = crate::test_locale::with_english(|| compile.lines());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Compilation failed with 1 error(s):");
        assert_eq!(lines[1], "  - x: y");
        // 第三期把 `WriteError` 也迁完了 ⟹ 内外两层现在都是英文源文。
        assert_eq!(
            crate::test_locale::with_english(|| PipelineError::WriteMdl(WriteError::Internal(
                "i".into()
            ))
            .lines()),
            vec!["Error: Internal error (please report): i"]
        );
        // 中文系统上内外两层都走译文表。
        assert_eq!(
            crate::test_locale::with_locale("zh-CN", || {
                PipelineError::WriteMdl(WriteError::Internal("i".into())).lines()
            }),
            vec!["错误：内部错误（请报告）：i"]
        );
        // `Display` 与 `lines()` 必须一致（CLI 走前者，GUI 走后者）。
        // ⚠️ 两边必须在**同一把锁里**算：`to_string()` 吃的是当前全局语言，
        // 锁外调它就等于把这条断言交给「别的测试刚好把语言留成什么」。
        let (display, joined) =
            crate::test_locale::with_english(|| (compile.to_string(), compile.lines().join("\n")));
        assert_eq!(display, joined);
    }

    /// ⭐ 类型化的意义：调用方能**拿到源错误本身**，而不只是一个字符串。
    ///
    /// 改造前这里只有 `Io(String)`，`ErrorKind::NotFound` 被 `format!` 扔掉，
    /// 调用方只能正则匹配中文前缀。这条测试把这个能力钉住。
    #[test]
    fn io_failures_expose_the_source_error_kind() {
        let err = PipelineError::ReadCollisionSmd {
            path: PathBuf::from("nope.smd"),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "系统找不到指定的文件。"),
        };
        // ⚠️ 外层前缀已是英文（本轮），内层 `io::Error` 的文案来自操作系统
        // （中文系统上就是中文），所以断言里两段语言不一致 —— 这正是当前状态。
        // ⚠️ 外层前缀要过译文表，所以必须锁语言：别的测试把全局 locale 切成
        // zh-CN 的那一瞬间，这条就会渲染成「错误：读不到碰撞 SMD …」。
        assert_eq!(
            crate::test_locale::with_english(|| err.to_string()),
            "Error: cannot read collision SMD nope.smd: 系统找不到指定的文件。"
        );

        let source = std::error::Error::source(&err).expect("应能拿到源错误");
        let io = source
            .downcast_ref::<std::io::Error>()
            .expect("源错误应是 io::Error");
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
    }

    /// 碰撞 SMD **读不到** ⟹ `Io`（退出码 2），不是编译错误。
    #[test]
    fn missing_collision_smd_is_an_io_error() {
        let d = tmpdir("missing");
        write(&d, "a.smd", MESH_SMD);
        let desc = parse(&desc_toml("nope.smd"));

        let err = build(&desc, &d, PipelineOptions::default()).expect_err("应失败");
        assert_eq!(err.kind(), PipelineErrorKind::Io, "{err}");
        // 文案走译文表 ⟹ 断言英文源文前先钉住语言。
        assert!(
            crate::test_locale::with_english(|| err.to_string())
                .contains("cannot read collision SMD"),
            "文案：{err}"
        );
        // 端到端也验证一次：源错误真的是 NotFound，而不是被压平的字符串。
        let io = std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<std::io::Error>())
            .expect("应能 downcast 到 io::Error");
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// 碰撞 SMD 读到了但**解析不了** ⟹ `Collision`（仍属 Build，退出码 1）。
    #[test]
    fn unparsable_collision_smd_is_a_build_error() {
        let d = tmpdir("badcollision");
        write(&d, "a.smd", MESH_SMD);
        write(&d, "phys.smd", "这不是 SMD\n");
        let desc = parse(&desc_toml("phys.smd"));

        let err = build(&desc, &d, PipelineOptions::default()).expect_err("应失败");
        assert_eq!(err.kind(), PipelineErrorKind::Build, "{err}");
        // 文案走译文表 ⟹ 断言英文源文前先钉住语言。
        assert!(
            crate::test_locale::with_english(|| err.to_string())
                .contains("cannot parse collision SMD"),
            "文案：{err}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// 渲染 SMD 读不到 ⟹ `Compile`（`compile()` 自己的错误，退出码 1）。
    #[test]
    fn missing_render_smd_is_a_compile_error() {
        let d = tmpdir("norender");
        let desc = parse(&desc_toml("phys.smd"));

        let err = build(&desc, &d, PipelineOptions::default()).expect_err("应失败");
        assert!(matches!(err, PipelineError::Compile(_)), "{err}");
        assert_eq!(err.kind(), PipelineErrorKind::Build);

        let _ = std::fs::remove_dir_all(&d);
    }

    /// `optimize_vtx` 与 TOML 的 `[model] optimize_vtx` 是 **`||`** 关系。
    ///
    /// 判据用**产物字节**：优化开启后 VTX 的索引序会变（缓存友好重排），
    /// 所以两个开关各自都能让 VTX 与「全关」不同，且两者结果相同。
    #[test]
    fn optimize_vtx_is_ored_with_the_toml_flag() {
        let d = tmpdir("optvtx");
        // 三角形要够多，`meshopt::optimize_vertex_cache` 才有可重排的余地
        // —— 单个三角形上它会原样返回（假阴性）。
        write(&d, "a.smd", &grid_smd(6));
        // 用不带碰撞的描述 —— 这条测试只关心 VTX，不需要 `.phy`。
        let base = parse(&plain_toml());

        let off = build(&base, &d, PipelineOptions::default()).expect("应编译成功");
        let on = build(&base, &d, PipelineOptions { optimize_vtx: true }).expect("应编译成功");
        assert_ne!(off.vtx, on.vtx, "命令行开关应改变 VTX 索引序");

        // TOML 侧单独生效（命令行关着）—— 两者是同一条路径。
        let toml_on = parse(&plain_toml().replace(
            "surface_prop = \"metal\"",
            "surface_prop = \"metal\"\noptimize_vtx = true",
        ));
        let from_toml = build(&toml_on, &d, PipelineOptions::default()).expect("应编译成功");
        assert_eq!(from_toml.vtx, on.vtx, "TOML 与命令行应是同一条路径");
        // 两侧都开 ⟹ 仍是同一条路径（`||` 不是「两个开关叠加两次」）。
        let both = build(&toml_on, &d, PipelineOptions { optimize_vtx: true }).expect("应编译成功");
        assert_eq!(both.vtx, on.vtx, "`||` 关系：两个都开等于开一个");

        let _ = std::fs::remove_dir_all(&d);
    }
}
