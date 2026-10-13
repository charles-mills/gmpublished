//! `mdlc` 命令行入口。

use std::path::Path;
use std::process::ExitCode;

use mdlc::compile::compile;
use mdlc::model::ModelDesc;
use mdlc::phy::{self, PhyHull, PhyParams, PhySolid};
use mdlc::vvd::{Vvd, check_invariants};

// 分段计时。`#[hotpath::main]` 在函数体最前面插入一个活到 `main` 返回的
// guard，析构时把报告写出来 —— 所以**任何**退出路径（包括 `run_official`
// 里的早退）都能拿到报告。
//
// `functions_limit = 0` 是必须的：hotpath 默认只列前 15 个函数，而本仓库
// 的分段标签加上顶层 wrapper 一共 17 行，默认值会把它们截掉。
//
// 报告**写到文件**而不是 stdout —— 理由见 `src/diag.rs`：官方
// `studiomdl.exe` 把包括 `ERROR:` 在内的所有输出都写 stdout，Crowbar 靠
// stdout 判断「编译器是否活着」，往那里塞一张性能表格会污染宿主日志。
// 路径可用 `HOTPATH_OUTPUT_PATH` 覆盖。
//
// 没开 `hotpath` feature 时这个属性宏原样返回函数，零开销；开了之后
// **没有运行时开关**，每次运行都会起 worker 线程 + 本地 metrics server
// （默认端口 6770，`HOTPATH_METRICS_SERVER_OFF=1` 可关）并写出报告。
#[hotpath::main(
    percentiles = [50, 95, 99],
    functions_limit = 0,
    output_path = "mdlc-prof.txt"
)]
fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    let rest: Vec<String> = argv.iter().skip(1).cloned().collect();

    // ---- 分流：官方兼容形态 vs mdlc 自有形态 ----
    //
    // 官方形态有两种（`studiomdl.cpp:6823` 的 `studiomdl [options] <file.qc>`）：
    // - 首参以 `-` 开头：`mdlc -game <gamedir> <x.qc>`（Crowbar 用）；
    // - 首参就是 `.qc`：`mdlc <x.qc>`（选项全默认）。
    //
    // 两者与 mdlc 自有形态无歧义：mdlc 的子命令都是**裸词**，且没有以
    // `.qc` 结尾的。`-h` / `--help` / `-V` / `--version` 例外 —— 它们仍归
    // mdlc（官方 `-h` 是 dump hboxes，归一化后是 `--h`，不会撞上 `--help`）。
    //
    // ⚠️ 这段必须**早于下面的更新检测**：更新提示是一条诊断，它得遵守
    // 同一条流路由（官方形态走 stdout）。判据纯由 `rest` 决定，没有副作用。
    let first = rest.first().map(String::as_str);
    let official_form = match first {
        Some(f) if f.starts_with('-') => !matches!(f, "-h" | "--help" | "-V" | "--version"),
        Some(f) => f.to_ascii_lowercase().ends_with(".qc"),
        None => false,
    };

    // ---- 语言 ----
    //
    // 自有文案的语言（`locales/app.yml`）。**必须早于 `cli::build_cli()`** ——
    // `about` / `help` 属性里的 `tr(...)` 是在 `command_i18n()` 被求值时
    // 执行的（不是编译期）。官方兼容形态不读这张表，调了也无害。
    mdlc::cli::init_locale();

    // 装 logger、定路由。**必须早于任何诊断输出** —— 既包括下面的更新
    // 检测，也包括 `run_official`（它自己不再改路由）。
    //
    // 先定路由再检测：否则官方形态下的提示会落到 stderr 上，而 Crowbar
    // 只认 stdout（见 [`mdlc::diag`]）。
    mdlc::diag::init(official_form);

    // ---- 更新检测（后台静默，永不阻塞）----
    //
    // 放在**最前面**、早于 clap 解析：这样 mdlc 自有形态与官方兼容形态
    // 共用同一套行为，也不会因为某个子命令的早退而漏掉。
    //
    // 开销 = 读一个约 100 字节的缓存文件；**只有缓存过期（24 小时）时**
    // 才会额外派生一个子进程，且不等它。
    //
    // ⚠️ `__update-check` 正是那个被派生出来的子进程 —— 它**不能**再
    // 触发一次检测，否则每次运行都会裂变成两个进程。
    let is_update_probe = rest
        .first()
        .is_some_and(|a| a == mdlc::update::HIDDEN_SUBCOMMAND);
    if !is_update_probe {
        mdlc::update::startup();
    }

    if official_form {
        return run_official(&argv);
    }

    let cli = match mdlc::cli::parse_i18n(&argv) {
        Ok(c) => c,
        Err(e) => return cli_exit(e),
    };

    match cli.command {
        mdlc::cli::Commands::Template => {
            print!("{}", mdlc::model::TEMPLATE_TOML);
            ExitCode::SUCCESS
        }
        mdlc::cli::Commands::Build(a) => {
            let toml_path = a.toml.clone();
            let out_root = a.out_root();
            let desc = match load_desc(&toml_path) {
                Ok(d) => d,
                Err(c) => return c,
            };
            let base = toml_path.parent().unwrap_or(Path::new("."));
            compile_and_write(&desc, base, &out_root, a.optimize_vtx)
        }
        mdlc::cli::Commands::Check(a) => check(&a.toml),
        mdlc::cli::Commands::Phy(a) => phy_cmd(&a),
        mdlc::cli::Commands::Qc2toml(a) => qc2toml(&a),
        mdlc::cli::Commands::BuildQc(a) => {
            let out = a.out_root();
            build_qc_from(&a.qc, &out, a.optimize_vtx)
        }
        mdlc::cli::Commands::VvdInfo(a) => vvd_info(&a.file.to_string_lossy()),
        mdlc::cli::Commands::VvdRoundtrip(a) => vvd_roundtrip(&a.file.to_string_lossy()),
        // 隐藏子命令：`update::spawn_check` 派生出来的子进程走这里。
        // 同步跑一次检测（写缓存），**不打印任何东西** —— 它的 stdout
        // 在派生时已被接到 NUL；手动跑时想看结果就开 `MDLC_UPDATE_DEBUG=1`。
        mdlc::cli::Commands::UpdateCheck => {
            mdlc::update::run_check();
            ExitCode::SUCCESS
        }
    }
}

/// 把 clap 的错误映射成退出码。
///
/// - `--help` / `--version` 之类走 **stdout、退出码 0**（clap 的
///   `DisplayHelp` / `DisplayVersion`）；
/// - 其余用法错误走 **stderr、退出码 2** —— 与迁移前的**手写解析契约一致**，
///   既有脚本（`probe_*.js` / `cmp_*.js`）都按这个码判定。
///
/// 对 formatter 泛型：mdlc 自有形态的错误带 `ClapI18nRichFormatter`
/// （见 [`mdlc::cli::parse_i18n`]），官方兼容形态是 clap 默认的
/// `DefaultFormatter`。两者的 `print` / `use_stderr` 行为一致，这里不必分家。
fn cli_exit<F: clap::error::ErrorFormatter>(e: clap::error::Error<F>) -> ExitCode {
    let _ = e.print();
    if e.use_stderr() {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// 官方兼容模式：`mdlc -game <gamedir> [-nop4] [-verbose] <model.qc>`。
///
/// 语义与官方 `studiomdl` 对齐（见 [`mdlc::cli`] 模块文档）：
/// 产物写到 `<gamedir>\models\<$modelname>`。
fn run_official(argv: &[String]) -> ExitCode {
    // 诊断流已由 `main` 里的 `mdlc::diag::init(official_form)` 定成 **stdout**：
    // 官方 studiomdl 把 `ERROR:` 也写在 stdout，而 Crowbar 的「编译器是否
    // 活着」标志只在 stdout 处理器里置位（`Compiler.vb:751` vs `:785-803`）。
    // 写 stderr 会让 Crowbar 一边显示错误、一边补一句误导的
    // `The compiler did not return any status messages.`
    // 详见 [`mdlc::diag`] 模块文档。

    let norm = mdlc::cli::normalize_official_args(argv);

    // 用户要求：未知选项**一律警告后继续**（不中断编译）。
    if !norm.unknown.is_empty() {
        log::warn!(
            "{}",
            mdlc::tr_fmt!(
                "Warning: ignoring unrecognized option(s) %{names} (not implemented by mdlc, or not an official option)",
                names = norm.unknown.join(" ")
            )
        );
    }

    let m = match mdlc::cli::build_official_cli().try_get_matches_from(&norm.argv) {
        Ok(m) => m,
        Err(e) => return cli_exit(e),
    };

    // 未实现的官方 flag 逐一告警 —— **静默忽略会产出与官方不同的模型**，
    // 那比拒绝更危险（例如 `-minlod` 会截断 LOD）。
    //
    // ⚠️ 布尔 flag 必须用 `get_flag` 判断，**不能用 `value_source`**：
    // clap 对未出现的 `SetTrue` 参数也返回 `Some(DefaultValue)`，
    // 用它会把「没传的 flag」也报成「已忽略」。
    for name in ["striplods", "definebones", "printbones"] {
        if m.get_flag(name) {
            log::warn!(
                "{}",
                mdlc::tr_fmt!(
                    "Warning: the official option -%{name} is not implemented yet; ignored",
                    name = name
                )
            );
        }
    }
    for name in ["minlod", "t", "a"] {
        if m.value_source(name).is_some() {
            log::warn!(
                "{}",
                mdlc::tr_fmt!(
                    "Warning: the official option -%{name} is not implemented yet; ignored",
                    name = name
                )
            );
        }
    }

    let Some(qc) = m.get_one::<String>("qc") else {
        log::error!("{}", mdlc::tr("Error: missing the .qc file argument"));
        log::error!(
            "{}",
            mdlc::tr("Usage: mdlc -game <gamedir> [options] <model.qc>")
        );
        return ExitCode::from(2);
    };

    let out_root = mdlc::cli::official_out_root(&m);
    build_qc_from(Path::new(qc), &out_root, m.get_flag("optimize_vtx"))
}

/// 读入并解析描述文件，打印全部校验错误。
fn load_desc(path: &Path) -> Result<ModelDesc, ExitCode> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!(
                    "Error: cannot read %{path}: %{err}",
                    path = path.display(),
                    err = e
                )
            );
            return Err(ExitCode::from(2));
        }
    };
    let desc = match ModelDesc::from_toml(&text) {
        Ok(d) => d,
        Err(e) => {
            log::error!("{}", mdlc::tr_fmt!("Error: %{err}", err = e));
            return Err(ExitCode::from(2));
        }
    };
    if let Err(errs) = desc.validate() {
        log::error!(
            "{}",
            mdlc::tr_fmt!("The description file has %{n} error(s):", n = errs.len())
        );
        for e in &errs {
            log::error!("  - {e}");
        }
        return Err(ExitCode::from(1));
    }
    Ok(desc)
}

fn check(p: &Path) -> ExitCode {
    let desc = match load_desc(p) {
        Ok(d) => d,
        Err(c) => return c,
    };
    // check 也读 SMD —— 「描述合法」但 SMD 缺失/材质对不上，同样是错误。
    let base = p.parent().unwrap_or(Path::new("."));
    match compile(&desc, base) {
        Ok(c) => {
            println!("{}", mdlc::tr_fmt!("%{path} is valid", path = p.display()));
            println!(
                "{}",
                mdlc::tr_fmt!(
                    "  bones %{bones}, materials %{materials}, body parts %{bodyparts}, vertices %{vertices}, triangles %{triangles}",
                    bones = desc.bones.len(),
                    materials = desc.materials.textures.len(),
                    bodyparts = c.bodyparts.len(),
                    vertices = c.total_vertices(),
                    triangles = c.total_triangles()
                )
            );
            for (bi, bp) in c.bodyparts.iter().enumerate() {
                for (mi, m) in bp.models.iter().enumerate() {
                    println!(
                        "{}",
                        mdlc::tr_fmt!(
                            "    bodyparts[%{bi}].models[%{mi}] %{name} <- %{smd} (%{meshes} mesh(es), %{vertices} vertices)",
                            bi = bi,
                            mi = mi,
                            name = m.name,
                            smd = m.smd_path.display(),
                            meshes = m.meshes.len(),
                            vertices = m.meshes.iter().map(|k| k.vertices.len()).sum::<usize>()
                        )
                    );
                }
            }
            if let Some((min, max)) = c.bounds() {
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "  bounds %{min} .. %{max}",
                        min = format!("{min:?}"),
                        max = format!("{max:?}")
                    )
                );
            }
            ExitCode::SUCCESS
        }
        Err(errs) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!(
                    "%{path} has %{n} error(s):",
                    path = p.display(),
                    n = errs.len()
                )
            );
            for e in &errs {
                log::error!("  - {e}");
            }
            ExitCode::from(1)
        }
    }
}

/// 把 QC 解析成 `ModelDesc`，报错格式与 TOML 路径一致。
fn load_qc(path: &Path) -> Result<ModelDesc, ExitCode> {
    match mdlc::qc::parse_qc_file(path) {
        Ok(d) => Ok(d),
        Err(errs) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!(
                    "Cannot parse %{path}: %{n} error(s):",
                    path = path.display(),
                    n = errs.len()
                )
            );
            for e in &errs {
                log::error!("  - {e}");
            }
            Err(ExitCode::from(1))
        }
    }
}

/// `mdlc qc2toml <model.qc> [--out <path.toml>]`。
fn qc2toml(a: &mdlc::cli::Qc2tomlArgs) -> ExitCode {
    let qc = a.qc.clone();
    let out = a.out.clone();
    let desc = match load_qc(&qc) {
        Ok(d) => d,
        Err(c) => return c,
    };
    // 与 `build` 一样先校验 —— 「解析成功但描述非法」也应当报出来。
    if let Err(errs) = desc.validate() {
        log::error!(
            "{}",
            mdlc::tr_fmt!("The parsed description has %{n} error(s):", n = errs.len())
        );
        for e in &errs {
            log::error!("  - {e}");
        }
        return ExitCode::from(1);
    }
    let text = match desc.to_toml() {
        Ok(t) => t,
        Err(e) => {
            log::error!("{}", mdlc::tr_fmt!("Error: %{err}", err = e));
            return ExitCode::from(1);
        }
    };
    let out = out.unwrap_or_else(|| qc.with_extension("toml"));
    if let Err(e) = std::fs::write(&out, &text) {
        log::error!(
            "{}",
            mdlc::tr_fmt!(
                "Error: cannot write %{path}: %{err}",
                path = out.display(),
                err = e
            )
        );
        return ExitCode::from(2);
    }
    println!(
        "{}",
        mdlc::tr_fmt!("%{qc} -> %{out}", qc = qc.display(), out = out.display())
    );
    println!(
        "{}",
        mdlc::tr_fmt!(
            "  bones %{bones}, materials %{materials}, body parts %{bodyparts}, sequences %{sequences}, animations %{animations}",
            bones = desc.bones.len(),
            materials = desc.materials.textures.len(),
            bodyparts = desc.bodyparts.len(),
            sequences = desc.sequences.len(),
            animations = desc.animations.len()
        )
    );
    ExitCode::SUCCESS
}

/// `mdlc build-qc <model.qc> [--out <目录>] [--optimize-vtx]`，
/// 以及官方兼容模式（`mdlc -game <gamedir> <model.qc>`）共用的入口。
///
/// `out_root` 由调用方决定：
/// - mdlc 自有形态 → `--out`（默认当前目录）；
/// - 官方兼容形态 → `<gamedir>\models`（见 [`mdlc::cli::official_out_root`]）。
fn build_qc_from(qc: &Path, out_root: &Path, optimize_vtx: bool) -> ExitCode {
    let desc = match load_qc(qc) {
        Ok(d) => d,
        Err(c) => return c,
    };
    let base = qc.parent().unwrap_or(Path::new("."));
    compile_and_write(&desc, base, out_root, optimize_vtx)
}

/// 把已解析的描述编译成四件套并落盘。
///
/// `build`（TOML）与 `build-qc`（QC）共用这条路径 —— 两者只在
/// **怎么得到 `ModelDesc`** 上不同，之后完全一致。这正是
/// `model.rs` 模块文档承诺的「写出器一行都不用改」。
///
/// 编排本身在 [`mdlc::pipeline`] 里（第三方工具可直接用同一套 API）；
/// 这里只负责把结果按 `mdlc.exe` 的契约打印出来并映射成退出码。
fn compile_and_write(
    desc: &ModelDesc,
    base: &Path,
    out_root: &Path,
    cli_optimize_vtx: bool,
) -> ExitCode {
    let opts = mdlc::pipeline::PipelineOptions {
        optimize_vtx: cli_optimize_vtx,
    };
    let out = match mdlc::pipeline::build(desc, base, opts) {
        Ok(o) => o,
        Err(e) => {
            for line in e.lines() {
                log::error!("{line}");
            }
            return ExitCode::from(e.kind().exit_code());
        }
    };
    let paths = match mdlc::pipeline::write_files(&out, out_root) {
        Ok(p) => p,
        Err(e) => {
            for line in e.lines() {
                log::error!("{line}");
            }
            return ExitCode::from(e.kind().exit_code());
        }
    };
    for line in out.summary_lines(&paths) {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

fn vvd_info(path: &str) -> ExitCode {
    let buf = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!("Error: cannot read %{path}: %{err}", path = path, err = e)
            );
            return ExitCode::from(2);
        }
    };
    let v = match Vvd::parse(&buf) {
        Ok(v) => v,
        Err(e) => {
            log::error!("{}", mdlc::tr_fmt!("Error: cannot parse: %{err}", err = e));
            return ExitCode::from(2);
        }
    };
    let h = &v.header;
    println!("{}", mdlc::tr_fmt!("file        %{path}", path = path));
    println!("{}", mdlc::tr_fmt!("length      %{n} bytes", n = buf.len()));
    println!(
        "{}",
        mdlc::tr_fmt!(
            "checksum    %{checksum}  (paired with .mdl/.vtx/.phy; not a content hash)",
            checksum = h.checksum
        )
    );
    println!("{}", mdlc::tr_fmt!("numLODs     %{n}", n = h.num_lods));
    println!(
        "{}",
        mdlc::tr_fmt!(
            "vertices    %{n}   (numLODVertexes[0]; also the tangent count)",
            n = h.vertex_count()
        )
    );
    println!(
        "{}",
        mdlc::tr_fmt!(
            "numLODVertexes  %{list}",
            list = format!(
                "{:?}",
                &h.num_lod_vertexes[..(h.num_lods.max(0) as usize).clamp(1, 8)]
            )
        )
    );
    println!("{}", mdlc::tr_fmt!("numFixups   %{n}", n = h.num_fixups));
    println!(
        "{}",
        mdlc::tr_fmt!(
            "vertex block      @ %{start}  (%{n} x 48 = %{bytes} bytes)",
            start = h.vertex_data_start,
            n = h.vertex_count(),
            bytes = h.vertex_count() * 48
        )
    );
    println!(
        "{}",
        mdlc::tr_fmt!(
            "tangent block     @ %{start}  (%{n} x 16 = %{bytes} bytes)",
            start = h.tangent_data_start,
            n = h.vertex_count(),
            bytes = h.vertex_count() * 16
        )
    );
    println!(
        "{}",
        mdlc::tr_fmt!(
            "expected length   %{n} bytes",
            n = h.tangent_data_start.max(0) as usize + h.vertex_count() * 16
        )
    );

    match check_invariants(&v, buf.len()) {
        Ok(()) => println!(
            "{}",
            mdlc::tr("self-consistency  passed (offsets, lengths and block sizes all agree)")
        ),
        Err(e) => {
            println!(
                "{}",
                mdlc::tr_fmt!("self-consistency  **FAILED**: %{err}", err = e)
            );
            return ExitCode::from(1);
        }
    }

    if let Some(v0) = v.vertices.first() {
        println!(
            "{}",
            mdlc::tr_fmt!(
                "first vertex    pos=%{pos} nrm=%{nrm} uv=%{uv}",
                pos = format!("{:?}", v0.position),
                nrm = format!("{:?}", v0.normal),
                uv = format!("{:?}", v0.tex_coord)
            )
        );
        println!(
            "{}",
            mdlc::tr_fmt!(
                "            weight=%{weight} bone=%{bone} boneCount=%{n}",
                weight = format!("{:?}", v0.weight),
                bone = format!("{:?}", v0.bone),
                n = v0.bone_count
            )
        );
    }
    ExitCode::SUCCESS
}

fn vvd_roundtrip(path: &str) -> ExitCode {
    let buf = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!("Error: cannot read %{path}: %{err}", path = path, err = e)
            );
            return ExitCode::from(2);
        }
    };
    match mdlc::vvd_round_trip(&buf) {
        Ok(rt) => {
            println!(
                "{}",
                mdlc::tr_fmt!("original    %{n} bytes", n = rt.original_len)
            );
            println!(
                "{}",
                mdlc::tr_fmt!("rewritten   %{n} bytes", n = rt.rewritten_len)
            );
            if rt.is_identical() {
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "result      **byte-for-byte identical** (all %{n} bytes match)",
                        n = rt.original_len
                    )
                );
                ExitCode::SUCCESS
            } else {
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "result      **MISMATCH**: first difference @%{at}, %{n} bytes differ",
                        at = rt.first_diff.unwrap_or(0),
                        n = rt.diff_count
                    )
                );
                ExitCode::from(1)
            }
        }
        Err(e) => {
            log::error!("{}", mdlc::tr_fmt!("Error: %{err}", err = e));
            ExitCode::from(2)
        }
    }
}

/// `mdlc phy`：SMD 三角形 → 凸包 → `.phy`。
///
/// 存在的意义是**让 PHY 写出能脱离完整模型编译独立测试**：只要有任意一个
/// SMD 就能产出一个可被 `validate-final.js` 校验的碰撞文件。
///
/// 参数已在 [`mdlc::cli::PhyArgs`] 里解析完（`--checksum` 的十进制 /
/// `0x` 十六进制两种写法由它的 `value_parser` 处理，非法值在 clap 那一层
/// 就变成用法错误、退出码 2）。
fn phy_cmd(args: &mdlc::cli::PhyArgs) -> ExitCode {
    let text = match std::fs::read_to_string(&args.input) {
        Ok(t) => t,
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!(
                    "Error: cannot read %{path}: %{err}",
                    path = args.input.display(),
                    err = e
                )
            );
            return ExitCode::from(2);
        }
    };
    let smd = match mdlc::smd::parse_smd(&text) {
        Ok(s) => s,
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!(
                    "Error: cannot parse %{path}: %{err}",
                    path = args.input.display(),
                    err = e
                )
            );
            return ExitCode::from(1);
        }
    };
    if smd.triangles.is_empty() {
        log::error!(
            "{}",
            mdlc::tr_fmt!(
                "Error: %{path} contains no triangles",
                path = args.input.display()
            )
        );
        return ExitCode::from(1);
    }

    // SMD 的三角形顶点是**逐面独立**的（没有索引行，且 UV/法线接缝会让同一
    // 位置出现多次）。这里按"三个 float 的位模式完全相同"焊接成索引表 ——
    // 只做精确去重，不做 epsilon 合并，否则会改动坐标、进而让
    // `upper_limit_radius` / `box_sizes` 与写入的点对不上。
    let mut vertices: Vec<[f32; 3]> = Vec::new();
    let mut index_of: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
    // 焊接后的面表。
    //
    // ⚠️ 早先这里还顺手按**三角形行的 `parentBone`** 建了一张
    // `by_bone` 表给 `--ragdoll` 用 —— 那是**错的**（见
    // `phy::group_by_bone` 的说明：官方按 `links[].bone` 做**面级**归属）。
    // 现在 ragdoll 走 `phy::build_ragdoll_phy_from_smd`，这里不再建表。
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
        // 焊接后可能出现退化三角形（三个下标不全不同）。凸包计算不关心面表，
        // 但 VHACD 会关心，所以这里直接剔除。
        if f[0] != f[1] && f[1] != f[2] && f[0] != f[2] {
            faces.push(f);
        }
    }
    if vertices.len() < 4 {
        log::error!(
            "{}",
            mdlc::tr_fmt!(
                "Error: only %{n} distinct vertices after welding; a hull needs at least 4",
                n = vertices.len()
            )
        );
        return ExitCode::from(1);
    }

    let model_name = args
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());

    // 凸包计算：默认把整个网格当一个凸包；`--vhacd` 走 VHACD 体分解。
    //
    // 渲染网格通常既不凸也不闭合（有 T 型接缝、有重复边），所以**不能**
    // 直接把面表当凸包喂进去 —— 必须先算凸包。
    // ⚠️ 这里返回的串**不带** `Error: ` 前缀 —— 调用点（`hull_of` 的唯一消费者）
    // 会用 `Error: %{err}` 再包一层，带前缀就成了「Error: Error: …」。
    let hull_of = |verts: &[[f32; 3]], face_idx: &[usize]| -> Result<Vec<PhyHull>, String> {
        if args.vhacd_enabled() {
            let sub: Vec<[u32; 3]> = face_idx.iter().map(|&i| faces[i]).collect();
            phy::decompose_concave(verts, &sub, 64, 16)
                .map_err(|e| mdlc::tr_fmt!("convex decomposition failed: %{err}", err = e))
        } else {
            PhyHull::from_points(verts)
                .map(|h| vec![h])
                .map_err(|e| mdlc::tr_fmt!("cannot compute the convex hull: %{err}", err = e))
        }
    };

    // 每组凸块 + 对应的 solid 参数。
    let mut grouped: Vec<Vec<PhyHull>> = Vec::new();
    let mut solids: Vec<PhySolid> = Vec::new();

    if args.ragdoll {
        // `--ragdoll`：官方 `$collisionjoints` 语义 —— 按**骨骼**分组，
        // 每根骨骼一个 solid，`client_data = 骨骼下标 + 1`，
        // `parent` 经 `FixParent` 沿骨骼链上溯修正。
        //
        // 整个流程复用 `phy::build_ragdoll_phy_from_smd`（与 `build` 同一条路径），
        // 这里只是为了沿用 CLI 的 `--vhacd` / `--surfaceprop` 等选项，
        // 所以拿到字节后再自己拼参数重写一次。
        //
        // ⚠️ 早先这里按**三角形行的 `parentBone`** 分组，并把 solid 串成
        // 一条链当 `parent` —— 那是**错的**：官方按 `links[].bone`
        // （蒙皮权重）分组，`parent` 是骨骼名且要上溯。
        // `mdlc phy` 是独立子命令，没有 TOML 上下文 —— 用 CLI 选项
        // 拼一个 `Physics`（`$collisionjoints` 的默认形态）。
        let phys = mdlc::model::Physics {
            joints: true,
            mass: Some(args.mass),
            ..Default::default()
        };
        match phy::build_ragdoll_phy_from_smd(
            &smd,
            phy::PhyIdentity {
                model_name: &model_name,
                collision_smd_name: &model_name,
                surface_prop: &args.surface_prop,
            },
            args.checksum,
            args.mass,
            &phys,
        ) {
            Ok(b) => {
                // 自检后直接落盘，跳过下面通用的写出路径。
                if let Err(e) = phy::check_invariants(&b) {
                    log::error!(
                        "{}",
                        mdlc::tr_fmt!(
                            "Error: the PHY we wrote failed self-check (bug in mdlc): %{err}",
                            err = e
                        )
                    );
                    return ExitCode::from(1);
                }
                if let Some(dir) = args.output.parent()
                    && !dir.as_os_str().is_empty()
                    && let Err(e) = std::fs::create_dir_all(dir)
                {
                    log::error!(
                        "{}",
                        mdlc::tr_fmt!(
                            "Error: cannot create directory %{path}: %{err}",
                            path = dir.display(),
                            err = e
                        )
                    );
                    return ExitCode::from(2);
                }
                if let Err(e) = std::fs::write(&args.output, &b) {
                    log::error!(
                        "{}",
                        mdlc::tr_fmt!(
                            "Error: cannot write %{path}: %{err}",
                            path = args.output.display(),
                            err = e
                        )
                    );
                    return ExitCode::from(2);
                }
                let layout = phy::check_invariants(&b).expect("刚查过");
                println!(
                    "{}",
                    mdlc::tr_fmt!("input       %{path}", path = args.input.display())
                );
                println!(
                    "{}",
                    mdlc::tr_fmt!("output      %{path}", path = args.output.display())
                );
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "checksum    0x%{checksum}",
                        checksum = format!("{:08x}", args.checksum)
                    )
                );
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "triangles   %{tris} (%{verts} distinct vertices after welding)",
                        tris = smd.triangles.len(),
                        verts = vertices.len()
                    )
                );
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "solid       %{n} (ragdoll, one per bone)",
                        n = layout.solid_count
                    )
                );
                println!();
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "file        %{n} bytes",
                        n = format!("{:>8}", layout.file_size)
                    )
                );
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "text block  %{n} bytes  @ %{at}",
                        n = format!("{:>8}", layout.text_size),
                        at = layout.solids_end
                    )
                );
                println!();
                println!(
                    "{}",
                    mdlc::tr("**write succeeded** (all 13 hard constraints self-checked)")
                );
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                log::error!(
                    "{}",
                    mdlc::tr_fmt!("Error: cannot build the ragdoll: %{err}", err = e)
                );
                return ExitCode::from(1);
            }
        }
    } else if args.concave {
        // `--concave`：**官方 `$concave` 语义** —— 按连通分量拆成多个凸块。
        //
        // ⚠️ 与 `--vhacd` 是**两种不同算法**：官方对连通的凹体给出的是
        // **整个网格的凸包**（凹处被填平），VHACD 保留凹口。
        // 见 `phy::decompose_connected_components`。
        //
        // 它需要**法线**（焊接判据是「位置相同 **且** 法线夹角 < 2°」），
        // 所以直接吃 SMD，而不是上面那份按位置焊接过的 `vertices`/`faces`。
        //
        // ⚠️ 第二个参数是**序列姿态**（官方 `ConvertToWorldSpace` 用
        // `g_panimation[0]`）。`mdlc phy` 是独立子命令、手里只有碰撞 SMD，
        // 没有序列上下文，所以传 `None` —— 此时退化成用碰撞 SMD 自己的
        // 参考姿态。走 `mdlc build` 的路径会正确传入序列姿态。
        let hs = match phy::decompose_connected_components(&smd, None) {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "{}",
                    mdlc::tr_fmt!("Error: $concave decomposition failed: %{err}", err = e)
                );
                return ExitCode::from(1);
            }
        };
        grouped.push(hs);
        // `name` = **碰撞 SMD 的 basename**（官方 `Q_FileBase`）。
        // `mdlc phy <in.smd>` 里那个 `<in.smd>` 就是碰撞 SMD。
        solids.push(PhySolid::prop(&model_name));
    } else {
        let all_idx: Vec<usize> = (0..faces.len()).collect();
        let hs = match hull_of(&vertices, &all_idx) {
            Ok(h) => h,
            Err(e) => {
                log::error!("{}", mdlc::tr_fmt!("Error: %{err}", err = e));
                return ExitCode::from(1);
            }
        };
        grouped.push(hs);
        // 同上：`name` 取碰撞 SMD 的 basename。
        solids.push(PhySolid::prop(&model_name));
    }

    let mut params = PhyParams::new(&model_name, args.checksum);
    params.total_mass = args.mass;
    params.surface_prop = &args.surface_prop;
    // 只有真的走了 `$concave` 才写 `concave "1"`，与实测的 studiomdl 行为一致
    // （VHACD 也写 —— 它同样产出了多个凸块）。
    params.concave = args.concave || args.vhacd_enabled();

    // 凸分解出来的多个凸块属于**同一个 solid**（共用一份点数组与一棵 ledgetree），
    // 所以走 `write_phy_multi` 而不是 `write_phy` —— 后者的契约是
    // 「每个 solid 一个凸块」。
    let bytes = match phy::write_phy_multi(&grouped, &solids, &params) {
        Ok(b) => b,
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!("Error: cannot write PHY: %{err}", err = e)
            );
            return ExitCode::from(1);
        }
    };
    let hull_count: usize = grouped.iter().map(|g| g.len()).sum();

    if let Some(dir) = args.output.parent()
        && !dir.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        log::error!(
            "{}",
            mdlc::tr_fmt!(
                "Error: cannot create directory %{path}: %{err}",
                path = dir.display(),
                err = e
            )
        );
        return ExitCode::from(2);
    }
    if let Err(e) = std::fs::write(&args.output, &bytes) {
        log::error!(
            "{}",
            mdlc::tr_fmt!(
                "Error: cannot write %{path}: %{err}",
                path = args.output.display(),
                err = e
            )
        );
        return ExitCode::from(2);
    }

    // 写出后立刻重读一遍做自检 —— `write_phy` 内部已经查过，这里再查一次
    // 是为了覆盖"落盘"这一段（截断、权限、路径写错都能被它抓到）。
    match phy::check_invariants(&bytes) {
        Ok(layout) => {
            println!(
                "{}",
                mdlc::tr_fmt!("input       %{path}", path = args.input.display())
            );
            println!(
                "{}",
                mdlc::tr_fmt!("output      %{path}", path = args.output.display())
            );
            println!(
                "{}",
                mdlc::tr_fmt!(
                    "checksum    0x%{checksum}",
                    checksum = format!("{:08x}", args.checksum)
                )
            );
            println!(
                "{}",
                mdlc::tr_fmt!(
                    "triangles   %{tris} (%{verts} distinct vertices after welding)",
                    tris = smd.triangles.len(),
                    verts = vertices.len()
                )
            );
            println!(
                "{}",
                mdlc::tr_fmt!(
                    "solid       %{solids} (%{hulls} convex piece(s) in total)",
                    solids = solids.len(),
                    hulls = hull_count
                )
            );
            println!();
            println!(
                "{}",
                mdlc::tr_fmt!(
                    "file        %{n} bytes",
                    n = format!("{:>8}", layout.file_size)
                )
            );
            println!(
                "{}",
                mdlc::tr_fmt!("solid       %{n}", n = layout.solid_count)
            );
            println!(
                "{}",
                mdlc::tr_fmt!(
                    "text block  %{n} bytes  @ %{at}",
                    n = format!("{:>8}", layout.text_size),
                    at = layout.solids_end
                )
            );
            for (i, ((surf, nodes), ledge)) in layout
                .surface_sizes
                .iter()
                .zip(&layout.node_counts)
                .zip(&layout.ledge_region_sizes)
                .enumerate()
            {
                println!(
                    "{}",
                    mdlc::tr_fmt!(
                        "  solid[%{i}]  surfaceSize=%{surf}  ledge region=%{ledge}  tree nodes=%{nodes}",
                        i = i,
                        surf = surf,
                        ledge = ledge,
                        nodes = nodes
                    )
                );
            }
            println!();
            println!(
                "{}",
                mdlc::tr("**write succeeded** (all 13 hard constraints self-checked)")
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!(
                "{}",
                mdlc::tr_fmt!(
                    "Error: the PHY we wrote failed self-check (bug in mdlc): %{err}",
                    err = e
                )
            );
            ExitCode::from(1)
        }
    }
}
