//! `mdlc` —— Source 引擎模型编译器（`studiomdl.exe` 的 Rust 重写）。

pub mod ani_writer;
mod anim_commands;
pub mod anim_writer;
mod animation_loops;
mod bone_alignment;
mod bone_hierarchy;
pub mod bone_math;
mod bone_rename;
pub mod cli;
pub mod compile;
mod contents;
pub mod diag;
pub mod fbx;
pub mod flex;
pub mod gltf;
mod ik_fixup;
pub mod layout;
mod local_hierarchy;
pub mod lod;
pub mod mdl_writer;
pub mod model;
mod motion;
pub mod phy;
pub mod pipeline;
pub mod qc;
mod root_lod;
pub mod smd;
mod source_center;
pub mod tangent;
/// 真实素材测试的路径解析（**仅测试**，`#[cfg(test)]`）。
#[cfg(test)]
pub mod test_assets;
/// 测试里切换语言（**仅测试**，`#[cfg(test)]`）。
#[cfg(test)]
pub mod test_locale;
mod transitions;
pub mod update;
pub mod vta;
pub mod vtx_writer;
pub mod vvd;

// ---------------------------------------------------------------------------
// 自有文案的 i18n
// ---------------------------------------------------------------------------

// ⚠️ 这个宏**必须在模块作用域调用，且每个 crate 只能调一次** —— 它展开成
// 一组 `pub fn`（`_rust_i18n_backend()` / `_rust_i18n_try_translate()` / …）
// 和一个 `static _RUST_I18N_BACKEND`，没有自己的 `mod` 包着。
//
// **源语言是英文**，`locales/app.yml` 里只写译文：`minify_key` 默认关，
// 键就是英文原文本身，所以英文用户（或任何未覆盖的语言）拿到的是
// `t!` 未命中时回退的键本身。这也是这里不写 `fallback = "en"` 的原因 ——
// 回退目标天然就是英文。
rust_i18n::i18n!("locales");

/// 取一条译文（自有文案）。
///
/// `key` 是**英文源文本身**（不是助记 id）—— `minify_key` 默认关，
/// 键不压缩。命中的是 `locales/app.yml` 里对应语言的译文；未命中则
/// 原样返回 `key`（英文），**不 panic、不报错**。
///
/// 之所以包一层而不是让调用方直接 `rust_i18n::t!`：`t!` 展开成
/// `crate::_rust_i18n_try_translate(...)`，`crate::` 解析到**调用方**
/// 那个 crate —— 在 `src/main.rs`（独立的 bin crate）里用会报
/// `error[E0433]: cannot find `_rust_i18n_t` in `crate``。包一层之后
/// bin 走 `mdlc::tr` 即可。
///
/// 返回 `String` 而不是 `Cow`：调用点都是把它喂给 clap 的
/// `about` / `help` / `value_name`（要 owned）或 `log::info!` 的格式串。
pub fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

/// 取一条译文并做占位符替换（自有文案的**带参数**版本）。
///
/// `args` 是 `(占位符名, 值)` 对；源文与译文里都写 `%{名}`。源文（英文）
/// 本身就是键，所以**未命中译文时照样能替换** —— 英文用户看到的是填好值
/// 的英文句子，而不是带着 `%{...}` 的模板。
///
/// 为什么不直接用 `rust_i18n::t!(key, name = value)`：那个形态把参数名
/// 当**标识符**解析（`tr.rs` 的 `Argument::parse`），且每个参数都要在宏里
/// 逐个写死。这里的调用点大多是「格式串 + 若干 `format!` 值」，用切片传
/// 更顺手，也免得为每条文案写一个专门的宏调用。
///
/// ⚠️ 占位符名要避开值里可能出现的字面 `%{`；本仓库的文案都只用
/// `{name}` / `{n}` / `{path}` 这类简单名，且值由 `format!` 产出、不含 `%{`。
pub fn tr_args(key: &str, args: &[(&str, String)]) -> String {
    let names: Vec<&str> = args.iter().map(|(k, _)| *k).collect();
    let values: Vec<String> = args.iter().map(|(_, v)| v.clone()).collect();
    rust_i18n::replace_patterns(&rust_i18n::t!(key), &names, &values)
}

/// [`tr_args`] 的宏糖：`tr_fmt!("Error: cannot read %{path}: %{err}", path = p, err = e)`。
///
/// 每个值都过一遍 `ToString`；需要 `{:?}` 时显式写
/// `bone = format!("{:?}", b)`。参数名是**标识符**（`stringify!` 取名字），
/// 与源文里的 `%{...}` 一一对应。
#[macro_export]
macro_rules! tr_fmt {
    ($key:expr) => {
        $crate::tr($key)
    };
    ($key:expr, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::tr_args(
            $key,
            &[$( (stringify!($name), ::std::string::ToString::to_string(&$val)) ),+],
        )
    };
}

/// 往返比对的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundTrip {
    /// 原文件长度。
    pub original_len: usize,
    /// 重新序列化后的长度。
    pub rewritten_len: usize,
    /// 首个不同字节的位置（完全相同则为 `None`）。
    pub first_diff: Option<usize>,
    /// 不同的字节总数。
    pub diff_count: usize,
}

impl RoundTrip {
    /// 是否逐字节完全相同。
    pub fn is_identical(&self) -> bool {
        self.first_diff.is_none()
    }
}

/// 把 VVD 字节读入再写出，并逐字节比对。
///
/// 这是本阶段的核心判据。注意它**不是**「解析成功」——解析成功只说明
/// 头部字段读得对，往返相同才说明**整个文件布局**（含两个数据块的
/// 位置与内容）都写对了。
pub fn vvd_round_trip(buf: &[u8]) -> Result<RoundTrip, vvd::VvdError> {
    let parsed = vvd::Vvd::parse(buf)?;
    vvd::check_invariants(&parsed, buf.len())?;
    let rewritten = parsed.to_bytes()?;

    let mut first_diff = None;
    let mut diff_count = 0usize;
    for (i, (a, b)) in buf.iter().zip(rewritten.iter()).enumerate() {
        if a != b {
            diff_count += 1;
            if first_diff.is_none() {
                first_diff = Some(i);
            }
        }
    }
    // 长度不同时，多出来的字节也算差异。
    let common = buf.len().min(rewritten.len());
    diff_count += buf.len().abs_diff(rewritten.len());

    Ok(RoundTrip {
        original_len: buf.len(),
        rewritten_len: rewritten.len(),
        first_diff: if buf.len() == rewritten.len() {
            first_diff
        } else {
            first_diff.or(Some(common))
        },
        diff_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vvd::{
        HEADER_SIZE, MAX_NUM_LODS, TANGENT_SIZE, VERTEX_SIZE, Vvd, VvdHeader, VvdTangent,
        VvdVertex, check_invariants,
    };

    /// 官方 L4D2 模型：`v_autoshotgun.vvd`，实测的头部字段。
    /// 这些常量是**独立于实现**记录下来的实测值，用来防止实现与测试一起跑偏。
    ///
    /// 路径解析见 [`crate::test_assets`] —— 环境变量 `MDLC_TEST_VVD` 优先。
    const REAL_VVD_DEFAULT: &str = r"D:\GITHUB\plank\examples\v_autoshotgun.vvd";
    const REAL_MDL_CHECKSUM: i32 = -1709441603;
    const REAL_VERTEX_COUNT: usize = 388765;
    const REAL_FILE_LEN: usize = 24_881_024;
    const REAL_VERTEX_DATA_START: i32 = 64;
    const REAL_TANGENT_DATA_START: i32 = 18_660_784;

    /// 构造一个最小可用的合成 VVD（不依赖真实素材，CI 也能跑）。
    fn synth(checksum: i32, count: usize) -> Vvd {
        let vertices = (0..count)
            .map(|i| VvdVertex {
                weight: [1.0, 0.0, 0.0],
                bone: [(i % 7) as u8, 0, 0],
                bone_count: 1,
                position: [i as f32, 1.5, -2.25],
                normal: [0.0, 0.0, 1.0],
                tex_coord: [0.125, 0.875],
            })
            .collect();
        let tangents = (0..count)
            .map(|_| VvdTangent {
                xyz: [1.0, 0.0, 0.0],
                w: 1.0,
            })
            .collect();
        Vvd {
            header: VvdHeader {
                checksum,
                num_lods: 1,
                num_lod_vertexes: [count as i32; MAX_NUM_LODS],
                num_fixups: 0,
                fixup_table_start: HEADER_SIZE as i32,
                vertex_data_start: HEADER_SIZE as i32,
                tangent_data_start: (HEADER_SIZE + count * VERTEX_SIZE) as i32,
            },
            vertices,
            tangents,
            fixups: Vec::new(),
        }
    }

    #[test]
    fn synthetic_round_trip_is_identical() {
        let v = synth(-1709441603, 37);
        let bytes = v.to_bytes().unwrap();
        assert_eq!(
            bytes.len(),
            HEADER_SIZE + 37 * VERTEX_SIZE + 37 * TANGENT_SIZE
        );
        let rt = vvd_round_trip(&bytes).unwrap();
        assert!(rt.is_identical(), "合成数据往返应完全相同：{rt:?}");
        assert_eq!(rt.diff_count, 0);
    }

    /// 旧实现**显式拒绝** `numFixups != 0`。现在支持了，所以那条拒绝已删除。
    ///
    /// 但「`numFixups` 声明与实际表长不符」仍然必须报错 —— 否则会写出
    /// 一个头部与数据对不上的文件（引擎按头部读，读到 fixup 表里当顶点）。
    #[test]
    fn mismatched_fixup_count_is_rejected() {
        let mut v = synth(1, 4);
        v.header.num_fixups = 2; // 声明 2 条，但 `fixups` 是空的
        let err = v.to_bytes().unwrap_err();
        assert!(
            matches!(err, crate::vvd::VvdError::Inconsistent { .. }),
            "numFixups 与实际表长不符时必须报错，实际：{err:?}"
        );
    }

    /// `numLODVertexes[0]` 与实际顶点数不符必须报错 —— 否则写出的文件
    /// 长度与头部声明对不上，引擎会读到别的块里去。
    #[test]
    fn mismatched_vertex_count_is_rejected() {
        let mut v = synth(1, 4);
        v.header.num_lod_vertexes[0] = 9;
        let err = v.to_bytes().unwrap_err();
        assert!(
            matches!(err, crate::vvd::VvdError::Inconsistent { .. }),
            "numLODVertexes[0] 与顶点数不符时必须报错，实际：{err:?}"
        );
    }

    /// 构造一份**带 fixup 表**的合成 VVD，验证偏移按 ALIGN16 递推且往返一致。
    ///
    /// 这是新增能力：旧实现遇到 `numFixups != 0` 直接报错。
    /// 偏移公式来自 `write.cpp` 2799-2815，已在 53 个真实模型上验证
    /// （`docs/_probe/probe_vvd_fixup_semantics2.js`）。
    #[test]
    fn synthetic_fixup_round_trip_is_identical() {
        let mut v = synth(7, 6);
        // 3 条 fixup：12*3 = 36 字节，不是 16 的倍数 → 顶点块必须对齐到 112。
        v.fixups = vec![
            crate::lod::Fixup {
                lod: 1,
                source_vertex_id: 0,
                num_vertexes: 2,
            },
            crate::lod::Fixup {
                lod: 0,
                source_vertex_id: 2,
                num_vertexes: 2,
            },
            crate::lod::Fixup {
                lod: 0,
                source_vertex_id: 4,
                num_vertexes: 2,
            },
        ];
        v.header.num_lods = 2;
        v.header.num_lod_vertexes = [6, 2, 2, 2, 2, 2, 2, 2];
        v.recompute_offsets();

        assert_eq!(
            v.header.fixup_table_start, 64,
            "fixupTableStart = ALIGN4(64)"
        );
        assert_eq!(
            v.header.vertex_data_start, 112,
            "vertexDataStart = ALIGN16(64 + 3*12) = ALIGN16(100) = 112"
        );
        assert_eq!(
            v.header.tangent_data_start,
            (112 + 6 * VERTEX_SIZE) as i32,
            "tangentDataStart = ALIGN16(vertexDataStart + n0*48)"
        );

        let bytes = v.to_bytes().unwrap();
        assert_eq!(bytes.len(), v.declared_len());
        check_invariants(&v, bytes.len()).expect("带 fixup 的合成 VVD 必须自洽");

        let rt = vvd_round_trip(&bytes).unwrap();
        assert!(rt.is_identical(), "带 fixup 的往返必须逐字节相同：{rt:?}");
        // 解析回来必须还原出同样的 fixup 表。
        let parsed = Vvd::parse(&bytes).unwrap();
        assert_eq!(parsed.fixups, v.fixups, "fixup 表必须能读回");
        assert_eq!(parsed.header.num_lods, 2);
        assert_eq!(parsed.header.num_lod_vertexes, v.header.num_lod_vertexes);
    }

    /// fixup 表必须**精确铺满** `[0, numLODVertexes[0])` —— 有空洞或重叠
    /// 都说明布局理解错了（实测 53/53 个真实模型满足）。
    #[test]
    fn fixup_table_must_tile_the_pool() {
        let mut v = synth(7, 6);
        // 只覆盖 [0,4)，漏掉 [4,6) → 应报错。
        v.fixups = vec![
            crate::lod::Fixup {
                lod: 1,
                source_vertex_id: 0,
                num_vertexes: 2,
            },
            crate::lod::Fixup {
                lod: 0,
                source_vertex_id: 2,
                num_vertexes: 2,
            },
        ];
        v.header.num_lods = 2;
        v.header.num_lod_vertexes = [6, 2, 2, 2, 2, 2, 2, 2];
        v.recompute_offsets();
        let bytes = v.to_bytes().unwrap();
        let err = check_invariants(&v, bytes.len()).unwrap_err();
        assert!(
            matches!(err, crate::vvd::VvdError::Inconsistent { .. }),
            "有空洞时必须报错，实际：{err:?}"
        );
    }

    /// `numLODVertexes` 必须单调不增 —— 它是**累计值**（渲染该 LOD 所需
    /// 的顶点数），随 LOD 变粗而减少。递增说明语义理解反了。
    #[test]
    fn non_monotone_lod_counts_are_rejected() {
        let mut v = synth(7, 6);
        v.header.num_lods = 3;
        // [0] 必须是顶点总数（6），但 [2] > [1] 破坏单调性。
        v.header.num_lod_vertexes = [6, 2, 4, 4, 4, 4, 4, 4];
        v.recompute_offsets();
        let bytes = v.to_bytes().unwrap();
        let err =
            crate::test_locale::with_english(|| check_invariants(&v, bytes.len()).unwrap_err());
        assert!(
            matches!(err, crate::vvd::VvdError::Inconsistent { .. }),
            "单调性被破坏时必须报错，实际：{err:?}"
        );
        // 报错信息应指向单调性，而不是别的检查。
        // ⚠️ `detail` 在 `check_invariants()` 那一刻就渲染好了（`crate::tr_fmt!`），
        // 所以语言必须在**调用**那一刻生效。
        let crate::vvd::VvdError::Inconsistent { detail } = err else {
            unreachable!()
        };
        assert!(
            detail.contains("monotonically non-increasing"),
            "应报单调性：{detail}"
        );
    }

    /// `numLODVertexes` 的尾部槽位必须 ripple 成最后一个有效值 ——
    /// 留 0 会让引擎的 LOD 切换读到「该 LOD 有 0 个顶点」。
    #[test]
    fn non_rippled_tail_is_rejected() {
        let mut v = synth(7, 6);
        v.header.num_lods = 2;
        v.header.num_lod_vertexes = [6, 2, 0, 0, 0, 0, 0, 0]; // 尾部应为 2
        v.recompute_offsets();
        let bytes = v.to_bytes().unwrap();
        let err = check_invariants(&v, bytes.len()).unwrap_err();
        let crate::vvd::VvdError::Inconsistent { detail } = err else {
            panic!("应报 Inconsistent")
        };
        assert!(detail.contains("ripple"), "应报 ripple：{detail}");
    }

    #[test]
    fn parse_rejects_bad_magic_and_version() {
        let mut bytes = synth(7, 2).to_bytes().unwrap();
        bytes[0] = b'X';
        assert!(matches!(
            Vvd::parse(&bytes),
            Err(crate::vvd::VvdError::BadId { .. })
        ));

        let mut bytes = synth(7, 2).to_bytes().unwrap();
        bytes[4..8].copy_from_slice(&99i32.to_le_bytes());
        assert!(matches!(
            Vvd::parse(&bytes),
            Err(crate::vvd::VvdError::BadVersion { found: 99 })
        ));
    }

    #[test]
    fn parse_rejects_truncated_vertex_block() {
        let bytes = synth(7, 10).to_bytes().unwrap();
        let cut = &bytes[..bytes.len() - VERTEX_SIZE];
        let err = Vvd::parse(cut).unwrap_err();
        assert!(
            matches!(err, crate::vvd::VvdError::Truncated { .. }),
            "截断必须报 Truncated，实际：{err:?}"
        );
    }

    #[test]
    fn invariants_reject_wrong_tangent_start() {
        let mut v = synth(7, 5);
        let bytes = v.to_bytes().unwrap();
        v.header.tangent_data_start += 1;
        let err = check_invariants(&v, bytes.len()).unwrap_err();
        assert!(matches!(err, crate::vvd::VvdError::Inconsistent { .. }));
    }

    /// **核心判据**：官方 L4D2 模型的往返必须逐字节相同。
    ///
    /// ⚠️ **标了 `#[ignore]`**（需要仓库外的真实素材）。手动跑：
    /// `cargo test --release -- --ignored`
    ///
    /// 素材缺失时**失败**而不是静默跳过 —— 既然被显式要求跑，就该报错。
    /// （旧版用「读不到就 `return`」，在默认输出里显示成 `ok`，看起来像
    /// 验过了，实际什么也没验。）
    #[test]
    #[ignore = "需要真实素材 v_autoshotgun.vvd（MDLC_TEST_VVD）"]
    fn real_l4d2_vvd_round_trips_byte_for_byte() {
        let buf = std::fs::read(crate::test_assets::require(
            "MDLC_TEST_VVD",
            REAL_VVD_DEFAULT,
            "官方 v_autoshotgun.vvd",
        ))
        .expect("应能读取 MDLC_TEST_VVD");

        // 先用独立记录的实测常量锚定解析结果。
        let parsed = Vvd::parse(&buf).expect("官方 VVD 必须能解析");
        assert_eq!(parsed.header.checksum, REAL_MDL_CHECKSUM);
        assert_eq!(parsed.header.vertex_count(), REAL_VERTEX_COUNT);
        assert_eq!(parsed.header.num_lods, 1);
        assert_eq!(parsed.header.num_fixups, 0);
        assert_eq!(parsed.header.vertex_data_start, REAL_VERTEX_DATA_START);
        assert_eq!(parsed.header.tangent_data_start, REAL_TANGENT_DATA_START);
        assert_eq!(buf.len(), REAL_FILE_LEN);

        // 再验证布局等式（错一个字段这里就会炸）。
        check_invariants(&parsed, buf.len()).expect("头部字段之间必须自洽");

        // 最后是真正的判据。
        let rt = vvd_round_trip(&buf).expect("往返必须成功");
        assert!(
            rt.is_identical(),
            "官方 VVD 往返必须逐字节相同，实际首个差异 @{}，共 {} 字节不同",
            rt.first_diff.unwrap_or(0),
            rt.diff_count
        );
        assert_eq!(rt.rewritten_len, REAL_FILE_LEN);
    }

    /// **多 LOD + fixup 的核心判据**：真实语料里全部带 fixup 的 VVD
    /// 必须逐字节往返。
    ///
    /// 语料默认在 `D:\DSH\L4D2ReverseEngineering\mdl-corpus\`（3333 个真实
    /// 模型，其中 53 个带 fixup、230 个多 LOD）；可用环境变量
    /// `MDLC_TEST_CORPUS` 覆盖。
    ///
    /// 实测结果（本机）：**53/53 逐字节相同**，另有 230 个多 LOD 模型
    /// （含 177 个 `numFixups == 0` 的单 mesh 形态）也全部往返一致。
    ///
    /// ⚠️ **标了 `#[ignore]`**（需要仓库外的语料）。手动跑：
    /// `cargo test --release -- --ignored`
    #[test]
    #[ignore = "需要真实语料（MDLC_TEST_CORPUS）"]
    fn real_fixup_vvds_round_trip_byte_for_byte() {
        const CORPUS_DEFAULT: &str = r"D:\DSH\L4D2ReverseEngineering\mdl-corpus";
        let corpus = crate::test_assets::require(
            "MDLC_TEST_CORPUS",
            CORPUS_DEFAULT,
            "真实语料目录（3333 个 .mdl 及其 .vvd）",
        );
        let corpus = corpus.as_path();
        // 递归收集 .vvd。
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vvd")) {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(corpus, &mut files);
        assert!(
            !files.is_empty(),
            "语料目录 {} 里没有 .vvd —— 路径指错了？",
            corpus.display()
        );

        let mut total = 0usize;
        let mut with_fixup = 0usize;
        let mut multi_lod = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for f in &files {
            let Ok(buf) = std::fs::read(f) else { continue };
            // 解析失败的文件不属本测试范围（另有语料统计脚本负责）。
            let Ok(parsed) = Vvd::parse(&buf) else {
                continue;
            };
            total += 1;
            if parsed.header.num_fixups > 0 {
                with_fixup += 1;
            }
            if parsed.header.num_lods > 1 {
                multi_lod += 1;
            }
            // 自检 + 往返。
            if let Err(e) = check_invariants(&parsed, buf.len()) {
                failures.push(format!("{}: 自检失败 {e}", f.display()));
                continue;
            }
            match vvd_round_trip(&buf) {
                Ok(rt) if rt.is_identical() => {}
                Ok(rt) => failures.push(format!(
                    "{}: 往返有 {} 字节不同（首个 @{}）",
                    f.display(),
                    rt.diff_count,
                    rt.first_diff.unwrap_or(0)
                )),
                Err(e) => failures.push(format!("{}: 往返失败 {e}", f.display())),
            }
        }

        eprintln!(
            "语料实测：{total} 个 VVD（多 LOD {multi_lod}，带 fixup {with_fixup}），\
             失败 {}",
            failures.len()
        );
        assert!(
            failures.is_empty(),
            "{} 个文件往返失败：\n{}",
            failures.len(),
            failures
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        // 语料确实覆盖了 fixup 与多 LOD —— 否则这个测试等于没测。
        assert!(
            with_fixup > 0,
            "语料里应至少有 1 个带 fixup 的模型（否则该测试没覆盖到新能力）"
        );
        assert!(multi_lod > 0, "语料里应至少有多 LOD 模型");
    }
}
