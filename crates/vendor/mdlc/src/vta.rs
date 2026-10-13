//! `.vta`（顶点动画）文件解析。

/// `.vta` 解析错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("第 {line} 行：{message}")]
pub struct VtaError {
    /// 1-based 行号。
    pub line: usize,
    pub message: String,
}

fn err(line: usize, message: impl Into<String>) -> VtaError {
    VtaError {
        line,
        message: message.into(),
    }
}

/// `vertexanimation` 块里的一行。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VtaVert {
    /// VTA **自己的**顶点序号（不是模型顶点下标）。
    pub index: u32,
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

/// 解析后的 `.vta`。
#[derive(Debug, Clone, PartialEq)]
pub struct Vta {
    /// `skeleton` 的第一个 `time`。
    pub start_frame: i32,
    /// `skeleton` 的最后一个 `time`。
    pub end_frame: i32,
    /// **按相对帧号索引**：`frames[i]` 是 `time (start_frame + i)` 的那一帧。
    /// 长度恒为 `end_frame - start_frame + 1`；缺数据的帧是空 `Vec`。
    pub frames: Vec<Vec<VtaVert>>,
    /// VTA 顶点数 = 全部帧里 `max(index) + 1`。
    ///
    /// `Grab_Vertexanimation` 就是在读到数据行时用
    /// `if (index >= numvertices) numvertices = index + 1;` 累出来的。
    pub num_vertices: usize,
    /// 非致命问题（studiomdl 对未知命令只 `MdlWarning`，不报错）。
    pub warnings: Vec<String>,
}

impl Vta {
    /// 相对帧 `rel` 的数据。越界返回 `None`。
    pub fn frame(&self, rel: i32) -> Option<&[VtaVert]> {
        usize::try_from(rel)
            .ok()
            .and_then(|i| self.frames.get(i))
            .map(|v| v.as_slice())
    }

    /// 相对帧总数（= `end_frame - start_frame + 1`）。
    pub fn num_frames(&self) -> usize {
        self.frames.len()
    }
}

/// 一个块的解析状态。
#[derive(PartialEq, Clone, Copy)]
enum Section {
    None,
    Nodes,
    Skeleton,
    VertexAnimation,
}

/// 解析 `.vta` 文本。
///
/// 严格程度对齐 studiomdl：
/// - `version` 必须是 `1`（否则 `bad version`）；
/// - `vertexanimation` 的每个数据行必须恰好 7 个字段；
/// - `time <n>` 必须落在 `skeleton` 定义的 `[start, end]` 内；
/// - `skeleton` 里 `time` 不得倒退、不得跳号（studiomdl 会
///   `is missing frame %d`）；
/// - 未知命令只记进 [`Vta::warnings`]。
pub fn parse_vta(text: &str) -> Result<Vta, VtaError> {
    let mut section = Section::None;
    let mut version_seen = false;

    // skeleton
    let mut start_frame: Option<i32> = None;
    let mut end_frame: i32 = -1;
    let mut skeleton_times: Vec<i32> = Vec::new();
    let mut last_time: Option<i32> = None;

    // vertexanimation
    let mut cur_time: i32 = -1;
    let mut cur_verts: Vec<VtaVert> = Vec::new();
    let mut frames: Vec<Vec<VtaVert>> = Vec::new();
    let mut num_vertices: usize = 0;
    let mut warnings: Vec<String> = Vec::new();

    /// 把 `cur_verts` 落进 `frames`（相对帧号）。
    fn flush(frames: &mut [Vec<VtaVert>], cur_verts: &mut Vec<VtaVert>, cur_time: i32, start: i32) {
        let rel = cur_time - start;
        if let Ok(i) = usize::try_from(rel)
            && i < frames.len()
            && !cur_verts.is_empty()
        {
            frames[i] = std::mem::take(cur_verts);
        }
        cur_verts.clear();
    }

    for (lineno0, raw) in text.lines().enumerate() {
        let lineno = lineno0 + 1;
        let line = raw.trim_end_matches(['\r', '\n']);
        // 去掉行尾注释（真实 `.vta` 有 —— Crowbar 反编译会在
        // `time N` 后面写 `# frame01`，也会在文件头写 `// Created by ...`）。
        //
        // ⚠️ 两种注释风格都要处理：
        //   * `//` —— 整行注释；
        //   * `#`  —— **行尾**注释（`time 4 # f04L+f04R`）。
        // 若不去掉 `#` 之后的内容，`time 4 # f04L+f04R` 会因
        // `toks.len() != 7` 走进「未知命令」分支。
        let line = match line.find('#') {
            Some(i) => &line[..i],
            None => line,
        };
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        let cmd = toks[0];

        // `end` 结束当前块。
        if cmd.eq_ignore_ascii_case("end") {
            match section {
                Section::Skeleton => {
                    // studiomdl 在 `end` 时校验帧连续（`is missing frame %d`）。
                    let start = start_frame.unwrap_or(0);
                    let n = end_frame - start + 1;
                    for t in 0..n.max(0) {
                        if !skeleton_times.contains(&(t + start)) {
                            return Err(err(
                                lineno,
                                format!(
                                    "skeleton 缺帧 {}（`time {}` 未出现）",
                                    t + start,
                                    t + start
                                ),
                            ));
                        }
                    }
                }
                Section::VertexAnimation => {
                    flush(
                        &mut frames,
                        &mut cur_verts,
                        cur_time,
                        start_frame.unwrap_or(0),
                    );
                }
                _ => {}
            }
            section = Section::None;
            continue;
        }

        match section {
            Section::Nodes => {
                // 骨骼表；本模块不需要，跳过。
            }
            Section::Skeleton => {
                if cmd.eq_ignore_ascii_case("time") {
                    let t = parse_i32(toks.get(1), lineno, "skeleton 的 time")?;
                    if let Some(prev) = last_time
                        && t < prev
                    {
                        return Err(err(lineno, format!("skeleton 的 time 倒退：{prev} → {t}")));
                    }
                    last_time = Some(t);
                    if start_frame.is_none() {
                        start_frame = Some(t);
                    }
                    if t > end_frame {
                        end_frame = t;
                    }
                    skeleton_times.push(t);
                } else if toks.len() == 7 {
                    // 骨骼姿态行，忽略。
                } else {
                    warnings.push(format!("第 {lineno} 行：skeleton 里忽略 {line:?}"));
                }
            }
            Section::VertexAnimation => {
                // ⚠️ **顺序很重要**：studiomdl 先试 7 字段数据行，
                // 失败才去解析命令（`studiomdl.cpp:5988-6058`）。
                // 所以「4 字段行」不是「未知命令」，而是「数据行格式错」——
                // 两者报的错误不同（`MdlError(15)` vs `MdlError(17)`），
                // 但**都是错误**。
                if toks.len() == 7 {
                    // 受控实验 `u3`：数据行出现在任何 `time` 之前 ⟹
                    // `ERROR: VTA Frame Sync`。
                    if cur_time < 0 {
                        return Err(err(
                            lineno,
                            format!("vertexanimation 的数据行出现在 `time` 之前：{line:?}"),
                        ));
                    }
                    let index = parse_i32(Some(&toks[0]), lineno, "顶点下标")?;
                    if index < 0 {
                        return Err(err(lineno, format!("顶点下标为负：{index}")));
                    }
                    let idx = index as usize;
                    if idx >= num_vertices {
                        num_vertices = idx + 1;
                    }
                    let f = |k: usize, what: &str| parse_f32(toks.get(k), lineno, what);
                    cur_verts.push(VtaVert {
                        index: index as u32,
                        pos: [f(1, "PosX")?, f(2, "PosY")?, f(3, "PosZ")?],
                        normal: [f(4, "NormX")?, f(5, "NormY")?, f(6, "NormZ")?],
                    });
                } else if cmd.eq_ignore_ascii_case("time") {
                    flush(
                        &mut frames,
                        &mut cur_verts,
                        cur_time,
                        start_frame.unwrap_or(0),
                    );
                    let t = parse_i32(toks.get(1), lineno, "vertexanimation 的 time")?;
                    let start = start_frame.ok_or_else(|| {
                        err(lineno, "vertexanimation 出现在 skeleton 之前（帧范围未知）")
                    })?;
                    if t < start || t > end_frame {
                        return Err(err(
                            lineno,
                            format!("time {t} 超出 skeleton 范围 [{start}, {end_frame}]"),
                        ));
                    }
                    cur_time = t;
                } else {
                    // 受控实验 `u2`：块内的未知命令是 **ERROR**（不是警告）——
                    // 与**顶层**的未知命令（只 `MdlWarning`，见 `Section::None`）
                    // 行为相反。这是很容易搞反的一处。
                    //
                    // 4 字段的数据行也走到这里（`u4`），错误信息因此要同时
                    // 覆盖两种情形。
                    return Err(err(
                        lineno,
                        format!(
                            "vertexanimation 里的未知命令或格式错误的数据行：{line:?}\
                             （数据行必须是 7 个字段：`<下标> <px py pz> <nx ny nz>`，\
                             实际 {} 个）",
                            toks.len()
                        ),
                    ));
                }
            }
            Section::None => {
                if cmd.eq_ignore_ascii_case("version") {
                    let v = parse_i32(toks.get(1), lineno, "version")?;
                    if v != 1 {
                        return Err(err(lineno, format!("bad version：{v}（只支持 1）")));
                    }
                    version_seen = true;
                } else if cmd.eq_ignore_ascii_case("nodes") {
                    section = Section::Nodes;
                } else if cmd.eq_ignore_ascii_case("skeleton") {
                    section = Section::Skeleton;
                } else if cmd.eq_ignore_ascii_case("vertexanimation") {
                    let start = start_frame.ok_or_else(|| {
                        err(lineno, "vertexanimation 出现在 skeleton 之前（帧范围未知）")
                    })?;
                    // 此时 end_frame 已由 skeleton 定下，据此开好 frames 数组。
                    let n = usize::try_from(end_frame - start + 1).unwrap_or(0);
                    frames = vec![Vec::new(); n];
                    section = Section::VertexAnimation;
                } else if cmd.starts_with("//") {
                    // ⚠️ **注释行**（真实 `.vta` 有 —— Crowbar 反编译时会写
                    // `// Created by Crowbar 0.74`）。
                    // 不记警告：`compile.rs` 把警告升级成错误，而注释显然
                    // 不该让编译失败。受控实验用的 fixture 没有注释，
                    // 所以这一点是**真实生产文件**才暴露出来的。
                } else {
                    warnings.push(format!("第 {lineno} 行：未知命令 {cmd:?}（已忽略）"));
                }
            }
        }
    }

    let start = start_frame.ok_or_else(|| err(0, "缺少 `skeleton` 块（无法确定帧范围）"))?;
    if !version_seen {
        warnings.push("缺少 `version 1`（studiomdl 会报 bad version）".to_string());
    }

    Ok(Vta {
        start_frame: start,
        end_frame,
        frames,
        num_vertices,
        warnings,
    })
}

fn parse_i32(tok: Option<&&str>, line: usize, what: &str) -> Result<i32, VtaError> {
    let t = tok.ok_or_else(|| err(line, format!("{what} 缺参数")))?;
    // `.vta` 里偶见 `0.000000` 形式的整数，先试 i32 再退 f32。
    if let Ok(v) = t.parse::<i32>() {
        return Ok(v);
    }
    t.parse::<f32>()
        .map(|v| v as i32)
        .map_err(|_| err(line, format!("{what} 不是合法整数：{t:?}")))
}

fn parse_f32(tok: Option<&&str>, line: usize, what: &str) -> Result<f32, VtaError> {
    let t = tok.ok_or_else(|| err(line, format!("{what} 缺参数")))?;
    t.parse::<f32>()
        .map_err(|_| err(line, format!("{what} 不是合法浮点数：{t:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小可用 `.vta`：2 帧、1 个顶点。
    const MIN: &str = "\
version 1
nodes
0 \"ValveBiped.world\" -1
end
skeleton
time 0
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
time 1
0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000
end
vertexanimation
time 0
0 1.000000 2.000000 3.000000 0.000000 0.000000 1.000000
time 1
0 1.000000 2.000000 13.000000 0.000000 0.600000 0.800000
end
";

    #[test]
    fn parses_minimal_vta() {
        let v = parse_vta(MIN).expect("应能解析");
        assert_eq!(v.start_frame, 0);
        assert_eq!(v.end_frame, 1);
        assert_eq!(v.num_frames(), 2);
        assert_eq!(v.num_vertices, 1);
        assert_eq!(v.frames[0][0].pos, [1.0, 2.0, 3.0]);
        assert_eq!(v.frames[1][0].pos, [1.0, 2.0, 13.0]);
        assert_eq!(v.frames[1][0].normal, [0.0, 0.6, 0.8]);
        assert!(v.warnings.is_empty(), "不该有警告：{:?}", v.warnings);
    }

    /// **法线必需** —— 4 字段行必须报错，不能静默当成数据。
    ///
    /// 这是受控实验里最坑的一条：studiomdl 会把它当**命令**解析，
    /// 报 `MdlError(15)`。我们必须同样拒绝。
    #[test]
    fn four_field_vertex_line_is_rejected() {
        let bad = MIN.replace(
            "0 1.000000 2.000000 3.000000 0.000000 0.000000 1.000000",
            "0 1.000000 2.000000 3.000000",
        );
        let e = parse_vta(&bad).expect_err("4 字段行必须报错");
        assert!(
            e.message.contains("7 个字段"),
            "错误信息应说明字段数要求，实际：{}",
            e.message
        );
    }
    #[test]
    fn version_must_be_one() {
        let bad = MIN.replace("version 1", "version 2");
        let e = parse_vta(&bad).expect_err("version 2 必须报错");
        assert!(e.message.contains("bad version"), "{}", e.message);
    }

    /// `time` 超出 `skeleton` 范围必须报错（studiomdl 报 `Frame MdlError`）。
    #[test]
    fn time_out_of_skeleton_range_is_rejected() {
        let bad = MIN.replace("time 1\n0 1.000000", "time 5\n0 1.000000");
        let e = parse_vta(&bad).expect_err("超范围必须报错");
        assert!(e.message.contains("超出 skeleton 范围"), "{}", e.message);
    }

    /// 帧号是**相对** `start_frame` 的 —— 这是 QC `frame <n>` 的口径。
    #[test]
    fn frames_are_relative_to_start_frame() {
        let shifted = "\
version 1
nodes
0 \"w\" -1
end
skeleton
time 2
0 0 0 0 0 0 0
time 3
0 0 0 0 0 0 0
end
vertexanimation
time 2
0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000
time 3
0 0.000000 0.000000 9.000000 0.000000 0.000000 1.000000
end
";
        let v = parse_vta(shifted).expect("应能解析");
        assert_eq!(v.start_frame, 2);
        assert_eq!(v.num_frames(), 2);
        // 相对帧 0 = 绝对 time 2；相对帧 1 = 绝对 time 3。
        assert_eq!(v.frame(0).unwrap()[0].pos[2], 0.0);
        assert_eq!(v.frame(1).unwrap()[0].pos[2], 9.0);
        assert!(v.frame(2).is_none(), "越界应返回 None");
    }

    /// `skeleton` 跳号必须报错（studiomdl 的 `is missing frame`）。
    #[test]
    fn skeleton_gap_is_rejected() {
        let bad = MIN.replace(
            "time 1\n0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000",
            "time 2\n0 0.000000 0.000000 0.000000 0.000000 0.000000 0.000000",
        );
        let e = parse_vta(&bad).expect_err("跳号必须报错");
        assert!(e.message.contains("缺帧"), "{}", e.message);
    }

    /// 顶层未知命令只**警告**（受控实验 `u5`：studiomdl 打
    /// `WARNING: unknown studio command "bogus" in vta file`，产物照出）。
    #[test]
    fn top_level_unknown_command_is_warning_not_error() {
        let with_extra = MIN.replace("vertexanimation\n", "bogus 1\nvertexanimation\n");
        let v = parse_vta(&with_extra).expect("顶层未知命令不应致命");
        assert!(
            v.warnings.iter().any(|w| w.contains("bogus")),
            "应记录未知命令：{:?}",
            v.warnings
        );
    }

    /// **注释不能产生警告** —— 真实 `.vta` 有注释，而 `compile.rs`
    /// 会把警告升级成错误，于是注释会让编译失败。
    ///
    /// 这个 bug 是**真实生产文件**（Linnea 的 2.6 MB `.vta`）暴露的，
    /// 我自己的 fixture 没有注释所以一直没发现：
    ///
    /// ```text
    /// // Created by Crowbar 0.74      ← 整行注释
    /// time 4 # f04L+f04R               ← 行尾 `#` 注释
    /// ```
    ///
    /// 两种都要吞掉，且**不得**留下警告。
    #[test]
    fn comments_produce_no_warnings() {
        let with_comments = format!("// Created by Crowbar 0.74\n{MIN}");
        let v = parse_vta(&with_comments).expect("整行注释不应致命");
        assert!(
            v.warnings.is_empty(),
            "整行注释不该产生警告（会被上游升级成错误）：{:?}",
            v.warnings
        );
        assert_eq!(v.num_frames(), 2, "注释不该影响帧解析");
    }

    /// 行尾 `#` 注释必须被剥掉 —— 否则 `time 1 # frame01` 的
    /// token 数会变成 3，走进「未知命令」分支。
    #[test]
    fn trailing_hash_comment_is_stripped() {
        let with_hash = MIN
            .replace("time 0\n0 0.000000", "time 0 # basis shape key\n0 0.000000")
            .replace("time 1\n0 0.000000", "time 1 # frame01\n0 0.000000");
        let v = parse_vta(&with_hash).expect("行尾 # 注释不该致命");
        assert!(v.warnings.is_empty(), "不该有警告：{:?}", v.warnings);
        assert_eq!(v.start_frame, 0);
        assert_eq!(v.end_frame, 1);
    }

    /// ⛔ **块内**未知命令是 **ERROR**，与顶层相反。
    ///
    /// 受控实验 `u2`：`ERROR: MdlError(20) : bogus 1` + `Aborted Processing`。
    /// 这一条极易搞反 —— 我第一版就写成了「记警告」。
    #[test]
    fn inner_unknown_command_is_fatal() {
        let bad = MIN.replace(
            "vertexanimation\ntime 0",
            "vertexanimation\nbogus 1\ntime 0",
        );
        let e = parse_vta(&bad).expect_err("块内未知命令必须致命");
        assert!(
            e.message.contains("未知命令"),
            "错误信息应指明是未知命令，实际：{}",
            e.message
        );
    }

    /// 缺 `skeleton` ⟹ 帧范围未知 ⟹ 必须报错（studiomdl 同样拒绝）。
    #[test]
    fn missing_skeleton_is_rejected() {
        let no_skel = "\
version 1
vertexanimation
time 0
0 0 0 0 0 0 1
end
";
        assert!(parse_vta(no_skel).is_err());
    }

    /// 数据行出现在任何 `time` 之前（studiomdl 的 `VTA Frame Sync`）。
    #[test]
    fn data_before_time_is_rejected() {
        let bad = MIN.replace(
            "vertexanimation\ntime 0\n",
            "vertexanimation\n0 1.000000 2.000000 3.000000 0.000000 0.000000 1.000000\n",
        );
        let e = parse_vta(&bad).expect_err("缺 time 必须报错");
        assert!(e.message.contains("`time` 之前"), "{}", e.message);
    }

    /// `num_vertices` = 全部帧里 `max(index)+1`。
    #[test]
    fn num_vertices_is_max_index_plus_one() {
        let v = parse_vta(MIN).unwrap();
        assert_eq!(v.num_vertices, 1);
        let bigger = MIN.replace("time 1\n0 1.000000", "time 1\n7 1.000000");
        let v2 = parse_vta(&bigger).unwrap();
        assert_eq!(v2.num_vertices, 8);
    }

    /// 缺失的帧是空 `Vec`（studiomdl 对 `t > 0` 无数据的帧写 `numvanims = 0`）。
    #[test]
    fn absent_frame_is_empty_not_error() {
        let sparse = MIN.replace(
            "time 1\n0 1.000000 2.000000 13.000000 0.000000 0.600000 0.800000\n",
            "",
        );
        let v = parse_vta(&sparse).expect("vertexanimation 允许缺帧");
        assert!(v.frame(0).is_some());
        assert!(v.frame(1).unwrap().is_empty(), "缺数据的帧应为空");
    }
}
