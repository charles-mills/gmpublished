//! 外置动画块（`.ani`）与 `$animblocksize` 的「原始样本」载荷。
//!
//! ```text
//! q = AngleQuaternion({0, 0, 90°}) * AngleQuaternion(SMD 旋转);   // 左乘 Hamilton
//! if (q.w < 0) q = -q;                                            // 符号规范化
//! out[3] = Quaternion48_Encode(q);                                // 每帧 6 字节
//! ```

use crate::bone_math::Matrix3x4;

/// `.ani` 文件头 = `sizeof(studiohdr_t)`。实测 121/121 个语料 `.ani` 都是 416 字节。
pub const ANI_HEADER_SIZE: usize = 416;

/// `.ani` 的 magic：`IDAG`（小端字节序 `47 41 44 49`）。
pub const ANI_ID: i32 = 0x4741_4449;

/// `.ani` 的版本号。实测 121/121 为 49，与 `.mdl` 相同。
pub const ANI_VERSION: i32 = 49;

/// 载荷固定头部长度（**有**旋转通道时）。
pub const RAW_HEADER_SIZE: usize = 28;

/// 载荷固定头部长度（**无**旋转通道时）—— 多出 8 字节：
/// 6 字节静置旋转样本 + 2 字节填充。
pub const RAW_HEADER_SIZE_NO_ROT: usize = 36;

/// 头部 `+24` 位标志：**有**旋转通道。
pub const RAW_FLAG_ROT: i32 = 0x80;
/// 头部 `+24` 位标志：**无**旋转通道（该骨骼旋转恒定，走内联常量）。
pub const RAW_FLAG_ROT_CONST: u8 = 0x40;
/// 头部 `+24` 位标志：该骨骼旋转**逐帧变化**。
pub const RAW_FLAG_ROT_VARIES: u8 = 0x80;
/// 头部 `+24` 位标志：该骨骼位置恒定（`Vector48` 内联）。
pub const RAW_FLAG_POS_CONST: u8 = 0x01;
/// 头部 `+24` 位标志：该骨骼位置**逐帧变化**。
pub const RAW_FLAG_POS_VARIES: u8 = 0x04;

/// `Quaternion48` 的量化刻度。**不是 16384** ——
/// 被丢弃的是**最大**分量，剩下三个必然 `|q| <= 1/√2`；
/// `16384 * √2 ≈ 23170.5` 取整为 23168，正好把 `±1/√2` 映射到 15 位的 `±16384`。
pub const QUATERNION48_SCALE: f32 = 23168.0;

/// `Quaternion48` 的偏置（15 位字段的中点）。
pub const QUATERNION48_BIAS: i32 = 0x4000;

/// 15 位字段的上限钳位值。`0xb500` 本身带 bit15，故低 15 位饱和在 `0x3500`。
const QUATERNION48_CLAMP_HI: i32 = 0xb500;
/// 钳位判定阈值：`>= 0xb501` 才饱和。
const QUATERNION48_CLAMP_LIMIT: i32 = 0xb501;

/// 参考四元数的欧拉角：**绕 Z 轴 +90°**。
///
/// 这是 studiomdl 给根骨骼的固定基准姿态。实测：SMD 静止姿态（欧拉角全 0）
/// 存出来的四元数是 `(x,y,z,w) = (0, 0, 0.7071068, 0.7071068)` —— 正是 90° 绕 Z。
pub const ROOT_REFERENCE_ANGLES: [f32; 3] = [0.0, 0.0, std::f32::consts::FRAC_PI_2];

/// 由欧拉角（**弧度**）构造四元数，返回 `[x, y, z, w]`。
///
/// 与 Source 的 `AngleQuaternion` 一致，但**全程 f32**（见模块头「为什么必须 f32」）。
///
/// 参数顺序是 `[roll(x), pitch(y), yaw(z)]`，与 [`crate::bone_math::angle_quaternion`] 相同。
pub fn angle_quaternion_f32(angles: [f32; 3]) -> [f32; 4] {
    let (sr, cr) = (angles[0] * 0.5).sin_cos();
    let (sp, cp) = (angles[1] * 0.5).sin_cos();
    let (sy, cy) = (angles[2] * 0.5).sin_cos();
    [
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    ]
}

/// 四元数 Hamilton 积，分量序 `[x, y, z, w]`。
pub fn quaternion_mul(p: [f32; 4], r: [f32; 4]) -> [f32; 4] {
    [
        p[0] * r[3] + p[3] * r[0] + p[1] * r[2] - p[2] * r[1],
        p[1] * r[3] + p[3] * r[1] + p[2] * r[0] - p[0] * r[2],
        p[2] * r[3] + p[3] * r[2] + p[0] * r[1] - p[1] * r[0],
        p[3] * r[3] - p[0] * r[0] - p[1] * r[1] - p[2] * r[2],
    ]
}

/// 符号规范化：`q` 与 `-q` 表示同一个旋转，取 `w >= 0` 的那支。
///
/// # 为什么必需
///
/// `ab_z8` 第 7 帧（绕 Z 105°）的 `w` 变负，不做规范化会差 **6048**（约 0.26 rad）。
/// 实测四种规范化方案的命中数：
///
/// | 方案 | 逐位相同 |
/// |---|---|
/// | 不规范化 | 23 / 24 |
/// | `x < 0` 取反 | 17 / 24 |
/// | `y < 0` 取反 | 23 / 24 |
/// | `z < 0` 取反 | 23 / 24 |
/// | **`w < 0` 取反** | **24 / 24** |
pub fn canonicalize_quaternion_sign(q: [f32; 4]) -> [f32; 4] {
    if q[3] < 0.0 {
        [-q[0], -q[1], -q[2], -q[3]]
    } else {
        q
    }
}

/// 把骨骼的欧拉角（弧度）转成**原始样本路径**要编码的四元数。
///
/// 即 [`ROOT_REFERENCE_ANGLES`] 的四元数**左乘**骨骼自身旋转，再做符号规范化。
pub fn raw_rotation_quaternion(euler: [f32; 3]) -> [f32; 4] {
    let base = angle_quaternion_f32(ROOT_REFERENCE_ANGLES);
    let own = angle_quaternion_f32(euler);
    canonicalize_quaternion_sign(quaternion_mul(base, own))
}

/// `Quaternion48` 编码器，逐字复刻 `studiomdl.exe` 的 `FUN_00463710`（RVA `0x463710`）。
///
/// # 算法
///
/// ```text
/// k      = argmax|q[i]|          // 严格 '<' → 平局取【小】下标
/// s      = (k+1) & 3             // 丢弃的是 k，存 s, s+1, s+2
/// raw[i] = clamp(0x4000 + (int)(q[(s+i)&3] * 23168.0), 0, 0xb500)
/// out[0] = (raw[0] & 0x7fff) | ((s >> 1) << 15)      // 下标高位
/// out[1] = (raw[1] & 0x7fff) | ((s &  1) << 15)      // 下标低位
/// out[2] = (raw[2] & 0x7fff) | ((被丢分量 < 0) << 15) // 被丢分量的符号
/// ```
///
/// # 注意 bit15 是**元数据**不是符号位
///
/// 三个 16 位字段的低 15 位才是数值，bit15 分别承载
/// 「下标高位 / 下标低位 / 被丢分量的符号」。
/// （把 bit15 当成符号位会读出「刻度 16382」这种假结论 —— 这个坑踩过。）
///
/// 返回 6 字节，按 `out[0], out[1], out[2]` 的小端序排列。
pub fn encode_quaternion48(q: [f32; 4]) -> [u8; 6] {
    let a = [q[0].abs(), q[1].abs(), q[2].abs(), q[3].abs()];
    let mut k = if a[0] < a[1] { 1 } else { 0 };
    if a[k] < a[2] {
        k = 2;
    }
    if a[k] < a[3] {
        k = 3;
    }
    let s = (k + 1) & 3;

    let mut out = [0u16; 3];
    out[1] = (out[1] & 0x7fff) | (((s & 1) as u16) << 15);
    out[0] = (out[0] & 0x7fff) | (((s >> 1) as u16) << 15);

    for (i, slot) in out.iter_mut().enumerate() {
        // 复刻 `iVar3 = 0x4000 - (int)(param_2[..] * -23168.0)`：
        // 乘 `-23168` 再取负，等价于乘 `+23168`；`(int)` 是**向零截断**。
        let scaled = q[(s + i) & 3] * -QUATERNION48_SCALE;
        let mut v = QUATERNION48_BIAS - (scaled as i32);
        if v < QUATERNION48_CLAMP_LIMIT {
            if v < 0 {
                v = 0;
            }
        } else {
            v = QUATERNION48_CLAMP_HI;
        }
        *slot = (*slot & 0x8000) | ((v as u16) & 0x7fff);
    }

    // 被丢弃分量的符号
    out[2] = (out[2] & 0x7fff) | (if q[(s + 3) & 3] < 0.0 { 1u16 } else { 0 } << 15);

    let mut bytes = [0u8; 6];
    for (i, w) in out.iter().enumerate() {
        bytes[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

/// 载荷里的一条**逐帧通道**。
///
/// 一条通道 = 一根骨骼的一个「槽」。槽在帧内按**骨骼主序**排列：
/// 每根参与骨骼先 ROT 后 POS。
#[derive(Debug, Clone, PartialEq)]
pub struct RawChannel {
    /// `true` = 旋转槽（`Quaternion48`）；`false` = 位置槽（`Vector48`）。
    pub is_rot: bool,
    /// 逐帧数据：旋转是欧拉角（弧度），位置是位移。
    pub frames: Vec<[f32; 3]>,
    /// 旋转槽是否左乘根骨骼基准四元数 `Q(90°Z)`。
    ///
    /// **只有根骨骼为 `true`** —— `BuildRawTransforms` 只对根骨骼套
    /// `rootxform`，子骨骼的局部变换不受影响（见 [`RawBoneTrack`]）。
    /// 位置槽忽略此字段。
    pub rot_base: bool,
}

impl RawChannel {
    /// 旋转槽的便捷构造（默认**不**施加基准旋转，即子骨骼形态）。
    pub fn rot(frames: Vec<[f32; 3]>) -> Self {
        Self {
            is_rot: true,
            frames,
            rot_base: false,
        }
    }
    /// 位置槽的便捷构造。
    pub fn pos(frames: Vec<[f32; 3]>) -> Self {
        Self {
            is_rot: false,
            frames,
            rot_base: false,
        }
    }
}

/// 一根骨骼在原始样本载荷里的两条轨道。
///
/// # 轨道**存在性**由「相对参考姿态的增量」决定，**内容**却是绝对量
///
/// 这是本格式最容易写错的一处（实测 10 个受控样本反解）：
///
/// * **存在性**：`rot` 存在 ⟺ 根骨骼 **或** 旋转增量非零；
///   `pos` 存在 ⟺ 位置增量非零。
///   —— 实测 `abi9`（4 骨骼、`ankle` 恒定 20°，恰好等于它的参考姿态）：
///   `flags = [0x40, 0, 0, 0x00]`，`ankle` **一个位都没有**。
///   若改用「绝对姿态」判存在性，会多写一条常量轨道。
/// * **内容**：旋转存**绝对**欧拉角（经基准四元数左乘），位置存**绝对**位置。
///   —— 实测 `abiB` 的 `ankle`（增量 `0/0.1745/0.349`）存的是
///   `Q(0.349)/Q(0.5236)/Q(0.6981)`（**绝对**），不是增量 `Q(0)/…`。
///
/// # 根骨骼的基准旋转只作用于根骨骼
///
/// `BuildRawTransforms`（`simplify.cpp:247-285`）只对**根骨骼**左乘
/// `rootxform = AngleMatrix(g_defaultrotation)`（`Rz(90°)`），子骨骼的
/// 局部变换不受影响。所以：
///
/// * 根骨骼旋转 = `Q(90°Z) ⊗ Q(绝对)`；子骨骼旋转 = `Q(绝对)`。
/// * 根骨骼位置 = `Rz(90°) · 绝对位置`；子骨骼位置 = **原样**绝对位置。
///
/// 实测：`abi1` 的 `ankle`（子骨骼）存 `[30,0,5]` —— **未**旋转；
/// 而 `abiB` 的 `root` 存 `[0,10,0]` = `Rz(90°)·[10,0,0]`。
/// 早期实现对所有骨骼一律施加基准旋转，于是 `abi1` 差 8 字节、`abi7` 差 23 字节。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawBoneTrack {
    /// 旋转轨道：`(是否施加根骨骼基准旋转, 逐帧**绝对**欧拉角)`。
    pub rot: Option<(bool, Vec<[f32; 3]>)>,
    /// 位置轨道：逐帧**绝对**位置（根骨骼的已过基准旋转）。
    pub pos: Option<Vec<[f32; 3]>>,
}

/// 写一段动画的「原始样本」载荷（`$animblocksize` 路径）。
///
/// # 布局
///
/// ```text
/// +0   i32  恒为 28
/// +4   i32  头部长度 = 逐帧数据起点：28（有旋转）/ 36（无旋转）
/// +8   i32  stride = 6 × 通道数
/// +12  i32  0
/// +16  i32  0
/// +20  i32  0
/// +24  i32  位标志（0x80 有旋转 / 0x40 无旋转 / 0x04 有位置）
/// +28  i16×3  仅当「无旋转通道」时存在 = 静置旋转样本
/// +34  i16    0（填充）
/// 数据：每帧按通道顺序写 [旋转 6] 或 [位置 6]
/// ```
///
/// # 通道是「按需」的，不是每根骨骼都有
///
/// 实测：`ab_z4`（只有旋转在变、位移恒 0）的 `stride = 6`（**只有旋转槽**），
/// 而 `ab_zp4`（位移也在变）才是 `12`。
/// 若给恒定的位移也开一个槽，`stride` 会变成 12、载荷长 76 而非 52 ——
/// 这正是第一次实现时的偏差。
///
/// 所以调用方要**先判定每条轨道是否真的在变**（见 [`channel_varies`]），
/// 只把会变的轨道放进 `channels`。
pub fn write_raw_payload(channels: &[RawChannel]) -> Vec<u8> {
    // 单骨骼、通道由调用方显式给定（测试与 `write_raw_animation_payload` 用）。
    let mut flags = vec![0u8; 1];
    for c in channels {
        flags[0] |= if c.is_rot {
            RAW_FLAG_ROT_VARIES
        } else {
            RAW_FLAG_POS_VARIES
        };
    }
    write_payload_raw(1, flags, Vec::new(), channels)
}

/// 按**逐骨骼轨道**写载荷（主线路径，复刻 `FUN_0046aaa0`）。
///
/// `tracks[b]` 描述骨骼 `b` 的两条轨道；`None` = 该轨道**不存在**。
///
/// # 轨道存在性（实测 8 个受控样本全中）
///
/// 官方对**每根骨骼独立**判定，判据是「该轨道的**内部原始值**是否恒为 0」：
///
/// * **旋转**：`is_root || 增量不全为 0`。
///   根骨骼**恒有**旋转轨道 —— 因为 `BuildRawTransforms`（`simplify.cpp:247-285`）
///   对根骨骼左乘 `rootxform = AngleMatrix(g_defaultrotation)`（`Rz(90°)`），
///   所以即使增量恒 0，内部值也是 `Rz(90°) ≠ 单位阵`。
///   **判据**：`abi9`（4 骨骼、全部姿态恒定）的根骨骼仍是 `0x40` +
///   内联常量 `feff00c00040`（= `Q(90°Z)`）。
/// * **位置**：增量不全为 0。根骨骼**没有**特殊待遇
///   （`ab_z4` 的根位移恒 0 → 只有 `0x80`，没有位置位）。
///
/// 轨道存在但**恒定** → 写内联常量（`0x40`/`0x01`）；**在变** → 逐帧（`0x80`/`0x04`）。
///
/// # 为什么参考姿态必须减掉
///
/// 增量是**相对骨骼表参考姿态**的。漏掉减法会把「姿态恒定但非零」的骨骼
/// 也写成一条常量轨道 —— 实测 `abi9` 的 `ankle`（恒定 20°）官方
/// `flags[3] == 0x00`（**整条轨道不存在**），不减参考姿态就会写成 `0x40`。
pub fn write_raw_payload_tracks(tracks: &[RawBoneTrack]) -> Vec<u8> {
    let bone_count = tracks.len();
    let mut flags = vec![0u8; bone_count];
    let mut inline: Vec<u8> = Vec::new();
    let mut channels: Vec<RawChannel> = Vec::new();

    for (b, t) in tracks.iter().enumerate() {
        if let Some((base, frames)) = &t.rot
            && !frames.is_empty()
        {
            let enc = |v: [f32; 3]| {
                if *base {
                    encode_quaternion48(raw_rotation_quaternion(v))
                } else {
                    encode_quaternion48(canonicalize_quaternion_sign(angle_quaternion_f32(v)))
                }
            };
            let first = frames[0];
            if frames.iter().all(|f| *f == first) {
                flags[b] |= RAW_FLAG_ROT_CONST;
                inline.extend_from_slice(&enc(first));
            } else {
                flags[b] |= RAW_FLAG_ROT_VARIES;
                channels.push(RawChannel {
                    is_rot: true,
                    frames: frames.clone(),
                    rot_base: *base,
                });
            }
        }
        if let Some(frames) = &t.pos
            && !frames.is_empty()
        {
            let first = frames[0];
            if frames.iter().all(|f| *f == first) {
                flags[b] |= RAW_FLAG_POS_CONST;
                for x in first {
                    inline.extend_from_slice(&half_from_f32(x).to_le_bytes());
                }
            } else {
                flags[b] |= RAW_FLAG_POS_VARIES;
                channels.push(RawChannel {
                    is_rot: false,
                    frames: frames.clone(),
                    rot_base: false,
                });
            }
        }
    }
    write_payload_raw(bone_count, flags, inline, &channels)
}

/// 载荷写出的公共底层：给定 flags / 内联常量 / 逐帧通道，拼出完整载荷。
///
/// ```text
/// +0   i32  ALIGN4(24 + numbones)      ← 内联常量区起点
/// +4   i32  ALIGN4(内联常量区末尾)      ← 逐帧数据起点
/// +8   i32  stride = 6 × 变化的通道数
/// +12  i32  0
/// +16  i32  0
/// +20  i32  0
/// +24  u8[numbones]  逐骨骼 flags
/// +h0  内联常量（按骨骼序：先 rot 后 pos）
/// +h4  逐帧数据（每帧按骨骼序写变化通道）
/// ```
fn write_payload_raw(
    bone_count: usize,
    flags: Vec<u8>,
    inline: Vec<u8>,
    channels: &[RawChannel],
) -> Vec<u8> {
    let stride: usize = channels.iter().map(|_| 6).sum();
    let h0 = align4(24 + bone_count);
    let h4 = align4(h0 + inline.len());
    let nframes = channels.iter().map(|c| c.frames.len()).max().unwrap_or(0);

    let mut out = Vec::with_capacity(h4 + stride * nframes);
    out.extend_from_slice(&(h0 as i32).to_le_bytes()); // +0
    out.extend_from_slice(&(h4 as i32).to_le_bytes()); // +4
    out.extend_from_slice(&(stride as i32).to_le_bytes()); // +8
    out.extend_from_slice(&0i32.to_le_bytes()); // +12
    out.extend_from_slice(&0i32.to_le_bytes()); // +16
    out.extend_from_slice(&0i32.to_le_bytes()); // +20
    out.extend_from_slice(&flags); // +24：**每根骨骼一个字节**
    out.resize(h0, 0);
    out.extend_from_slice(&inline);
    out.resize(h4, 0);

    for f in 0..nframes {
        for c in channels {
            let Some(v) = c.frames.get(f) else { continue };
            if c.is_rot {
                // 只有根骨骼左乘基准 `Q(90°Z)`；子骨骼存绝对欧拉角。
                let q = if c.rot_base {
                    raw_rotation_quaternion(*v)
                } else {
                    canonicalize_quaternion_sign(angle_quaternion_f32(*v))
                };
                out.extend_from_slice(&encode_quaternion48(q));
            } else {
                for x in v {
                    out.extend_from_slice(&half_from_f32(*x).to_le_bytes());
                }
            }
        }
    }
    out
}

/// `ALIGN4`。
fn align4(v: usize) -> usize {
    (v + 3) & !3
}

/// 单骨骼的便捷入口（`rotations` / `positions` 为 `None` 表示该槽不写）。
///
/// 多骨骼请直接用 [`write_raw_payload`] —— 本函数只为单骨骼场景与测试服务。
pub fn write_raw_animation_payload(
    rotations: Option<&[[f32; 3]]>,
    positions: Option<&[[f32; 3]]>,
) -> Vec<u8> {
    let mut channels = Vec::new();
    if let Some(r) = rotations
        && !r.is_empty()
    {
        // 默认按**根骨骼**处理（左乘基准 `Q(90°Z)`）—— 该入口服务于
        // 单骨骼场景，而单骨骼模型的那一根就是根骨骼。
        channels.push(RawChannel {
            is_rot: true,
            frames: r.to_vec(),
            rot_base: true,
        });
    }
    if let Some(p) = positions
        && !p.is_empty()
    {
        channels.push(RawChannel {
            is_rot: false,
            frames: p.to_vec(),
            rot_base: false,
        });
    }
    write_raw_payload(&channels)
}

/// 一组逐帧三元组是否**真的在变**（决定要不要为它开一个槽）。
///
/// 实测依据：`ab_z4` 的位移恒为 0 → 官方不给它开位置槽（`stride = 6`）；
/// `ab_zp4` 的位移在变 → 开槽（`stride = 12`）。
pub fn channel_varies(frames: &[[f32; 3]]) -> bool {
    let Some(first) = frames.first() else {
        return false;
    };
    frames.iter().any(|f| f != first)
}

/// 写 `.ani` 的 416 字节容器头。
///
/// 实测（121/121 个语料 `.ani`）只有三个字段非零：
/// `id`@`0x00` = `IDAG`、`version`@`0x04` = 49、`length`@`0x4C` = 文件总长。
/// **块表不在这里** —— 它在 `.mdl` 的 `numanimblocks`@`0x160` /
/// `animblockindex`@`0x164`（每项 8 字节 `{datastart, dataend}`）。
///
/// `file_len` 是整个 `.ani` 的长度（= 416 + 所有块数据）。
pub fn write_ani_container(file_len: usize) -> Vec<u8> {
    let mut out = vec![0u8; ANI_HEADER_SIZE];
    out[0x00..0x04].copy_from_slice(&ANI_ID.to_le_bytes());
    out[0x04..0x08].copy_from_slice(&ANI_VERSION.to_le_bytes());
    out[0x4C..0x50].copy_from_slice(&(file_len as i32).to_le_bytes());
    out
}

/// 把一段块数据补齐到 **16 字节对齐**（`.ani` 内每段动画的数据起点规则）。
///
/// 实测 15782/15782 条动画满足 `animindex % 16 == ((16 - block.start % 16) % 16)`。
pub fn align16(v: usize) -> usize {
    (v + 15) & !15
}

/// `float32` → IEEE half（`FloatToHalf`，RVA `0x4639D0`）。
///
/// 含 `±65504` 饱和与下溢到零；**舍入是「向零截断」**（见下）。
///
/// # ⚠️ 舍入模式：**截断**，不是 round-to-nearest-even
///
/// 本函数原先是 round-to-nearest-even（写在注释里、也写在文档里），
/// 但那是**没有证据的假设** —— 早期全部位置通道样本（`ab_zp4` 的
/// `0.5/1.0/1.5`）**都恰好是 half 可精确表示的**，两种舍入给出同样的位模式，
/// 所以从未被区分过。
///
/// 直到 `absec1`（120 帧的分段动画，位置是 `0.084454/0.168487/…`
/// 这类不可精确表示的值）才暴露出来：
///
/// | 输入 f32 | 截断 | RNE | 官方 |
/// |---|---|---|---|
/// | `0.084454` | **`0x2d67`** ✓ | `0x2d68` | `0x2d67` |
///
/// 判据（`docs/_probe/_tmp9.js` 的算法，31 帧）：
/// **截断 30/31 命中，RNE 只有 10/31**。
///
/// 注意这与**旋转**通道的 `Quaternion48` 不同 —— 那里是 `(int)` 强转
/// （也是截断），两者一致；本函数统一成截断即可。
fn half_from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mantissa = bits & 0x007f_ffff;

    // 溢出 → ±inf（再按官方行为饱和到 ±65504）
    if exp == 0xff {
        // inf / NaN → 饱和到最大值（NaN 保留符号）
        return sign | 0x7bff;
    }
    let unbiased = exp - 127;
    if unbiased > 15 {
        return sign | 0x7bff; // ±65504
    }
    if unbiased < -24 {
        return sign; // 下溢到 ±0
    }
    if unbiased < -14 {
        // 非规格化：**截断**（不补 round-to-nearest 的进位）。
        let shift = (-unbiased - 14) as u32;
        let m = (mantissa | 0x0080_0000) >> (shift + 13);
        return sign | (m as u16);
    }
    // 规格化：**截断**（`mantissa >> 13`，不补进位）。
    let e = (unbiased + 15) as u32;
    let m = mantissa >> 13;
    // 截断不会进位到 `m == 0x400`，所以不需要进位处理；
    // 保留这个断言以钉住「截断」这一行为（RNE 会在这里进位）。
    debug_assert!(m < 0x400, "截断不可能产生尾数溢出");
    sign | ((e as u16) << 10) | (m as u16)
}

/// 供上层使用的世界矩阵类型别名（避免调用方再 import 一次）。
pub type WorldMatrix = Matrix3x4;

#[cfg(test)]
mod tests {
    use super::*;
    // 块打包与文件拼接定义在 `anim_writer`（它们消费 `AnimWriteOutcome` 的上下文）。
    use crate::anim_writer::{build_ani_file, pack_anim_blocks};

    /// 把「度」转成弧度 —— **必须与验证脚本同式**：`deg * (PI/180)`，全程 f32。
    fn deg(d: f32) -> f32 {
        d * (std::f32::consts::PI / 180.0)
    }

    /// 黄金向量：5 组受控实验、共 24 帧，全部来自真实 `studiomdl.exe` 产物。
    ///
    /// 覆盖 X / Y / Z 单轴，以及三轴同时变（用于判定「欧拉角 +90°」与
    /// 「四元数左乘」的区别）。
    const GOLDEN: &[(&str, [f32; 3], [u8; 6])] = &[
        // ab_z4：绕 Z 每帧 +30°
        (
            "ab_z4 f0",
            [0.0, 0.0, 0.0],
            [0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z4 f1",
            [0.0, 0.0, 30.0],
            [0x3f, 0xed, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z4 f2",
            [0.0, 0.0, 60.0],
            [0x6c, 0xd7, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z4 f3",
            [0.0, 0.0, 90.0],
            [0x00, 0xc0, 0x00, 0xc0, 0x00, 0x40],
        ),
        // ab_z8：绕 Z 每帧 +15°
        (
            "ab_z8 f0",
            [0.0, 0.0, 0.0],
            [0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f1",
            [0.0, 0.0, 15.0],
            [0x17, 0xf7, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f2",
            [0.0, 0.0, 30.0],
            [0x3f, 0xed, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f3",
            [0.0, 0.0, 45.0],
            [0xa2, 0xe2, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f4",
            [0.0, 0.0, 60.0],
            [0x6c, 0xd7, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f5",
            [0.0, 0.0, 75.0],
            [0xd0, 0xcb, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f6",
            [0.0, 0.0, 90.0],
            [0x00, 0xc0, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_z8 f7",
            [0.0, 0.0, 105.0],
            [0xd0, 0xcb, 0x00, 0xc0, 0x00, 0xc0],
        ),
        // ab_x4：绕 X 每帧 +30°
        (
            "ab_x4 f0",
            [0.0, 0.0, 0.0],
            [0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_x4 f1",
            [30.0, 0.0, 0.0],
            [0xd0, 0xfd, 0x90, 0xd0, 0x90, 0x50],
        ),
        (
            "ab_x4 f2",
            [60.0, 0.0, 0.0],
            [0x6b, 0xf7, 0xff, 0xdf, 0xff, 0x5f],
        ),
        (
            "ab_x4 f3",
            [90.0, 0.0, 0.0],
            [0x3f, 0x6d, 0x3f, 0xed, 0x3f, 0x6d],
        ),
        // ab_y4：绕 Y 每帧 +30°
        (
            "ab_y4 f0",
            [0.0, 0.0, 0.0],
            [0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_y4 f1",
            [0.0, 30.0, 0.0],
            [0xd0, 0xfd, 0x70, 0xaf, 0x90, 0x50],
        ),
        (
            "ab_y4 f2",
            [0.0, 60.0, 0.0],
            [0x6b, 0xf7, 0x01, 0xa0, 0xff, 0x5f],
        ),
        (
            "ab_y4 f3",
            [0.0, 90.0, 0.0],
            [0x3f, 0x6d, 0x3f, 0xed, 0x3f, 0xed],
        ),
        // ab_xyz4：三轴同时变（X+20°、Y+30°、Z+40° 每帧）
        (
            "ab_xyz4 f0",
            [0.0, 0.0, 0.0],
            [0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40],
        ),
        (
            "ab_xyz4 f1",
            [20.0, 30.0, 40.0],
            [0x11, 0xe8, 0x83, 0xb1, 0x81, 0x57],
        ),
        (
            "ab_xyz4 f2",
            [40.0, 60.0, 80.0],
            [0xd6, 0xd5, 0xfb, 0x97, 0x68, 0x5e],
        ),
        (
            "ab_xyz4 f3",
            [60.0, 90.0, 120.0],
            [0x90, 0x50, 0xd0, 0xfd, 0x90, 0xd0],
        ),
    ];

    /// **核心验收**：24 帧黄金向量逐字节相同。
    #[test]
    fn quaternion48_matches_official_24_of_24() {
        let mut bad = Vec::new();
        for (name, euler, want) in GOLDEN {
            let e = [deg(euler[0]), deg(euler[1]), deg(euler[2])];
            let got = encode_quaternion48(raw_rotation_quaternion(e));
            if &got != want {
                bad.push(format!("{name}: got {got:02x?} want {want:02x?}"));
            }
        }
        assert!(
            bad.is_empty(),
            "{} 帧不匹配:\n{}",
            bad.len(),
            bad.join("\n")
        );
    }

    /// 对照：`bone_math::angle_quaternion` 内部走 f64。
    ///
    /// 它**也能**通过（因为 f64 的 `sin_cos` 结果舍入到 f32 与直接算 f32 一致），
    /// 所以本模块自带的 [`angle_quaternion_f32`] 与它在此用例上等价 ——
    /// 保留自带版本只是为了「整条链路统一 f32」这一可读性与确定性。
    ///
    /// **真正不能做的是让后续的乘法与量化也走 f64** —— 见
    /// [`all_f64_chain_would_be_wrong`]。
    #[test]
    fn bone_math_angle_quaternion_is_equivalent_here() {
        let base = crate::bone_math::angle_quaternion(ROOT_REFERENCE_ANGLES);
        let mut ok = 0;
        for (_, euler, want) in GOLDEN {
            let e = [deg(euler[0]), deg(euler[1]), deg(euler[2])];
            let own = crate::bone_math::angle_quaternion(e);
            let q = canonicalize_quaternion_sign(quaternion_mul(base, own));
            if &encode_quaternion48(q) == want {
                ok += 1;
            }
        }
        assert_eq!(
            ok, 24,
            "bone_math::angle_quaternion 在本用例上应与 f32 版等价"
        );
    }

    /// **真正的护栏**：若把乘法与量化也降级成 f64，命中数会掉到 10/24。
    ///
    /// 这正是「必须 f32」的证据 —— 与 [`bone_math::angle_quaternion`] 只把
    /// `sin_cos` 放 f64 不同，全程 f64 会真的算错 14 帧。
    #[test]
    fn all_f64_chain_would_be_wrong() {
        fn angle_q64(a: [f64; 3]) -> [f64; 4] {
            let (sr, cr) = (a[0] * 0.5).sin_cos();
            let (sp, cp) = (a[1] * 0.5).sin_cos();
            let (sy, cy) = (a[2] * 0.5).sin_cos();
            [
                sr * cp * cy - cr * sp * sy,
                cr * sp * cy + sr * cp * sy,
                cr * cp * sy - sr * sp * cy,
                cr * cp * cy + sr * sp * sy,
            ]
        }
        fn mul64(p: [f64; 4], r: [f64; 4]) -> [f64; 4] {
            [
                p[0] * r[3] + p[3] * r[0] + p[1] * r[2] - p[2] * r[1],
                p[1] * r[3] + p[3] * r[1] + p[2] * r[0] - p[0] * r[2],
                p[2] * r[3] + p[3] * r[2] + p[0] * r[1] - p[1] * r[0],
                p[3] * r[3] - p[0] * r[0] - p[1] * r[1] - p[2] * r[2],
            ]
        }
        fn enc64(q: [f64; 4]) -> [u8; 6] {
            let a = q.map(f64::abs);
            let mut k = if a[0] < a[1] { 1 } else { 0 };
            if a[k] < a[2] {
                k = 2;
            }
            if a[k] < a[3] {
                k = 3;
            }
            let s = (k + 1) & 3;
            let mut out = [0u16; 3];
            out[1] |= ((s & 1) as u16) << 15;
            out[0] |= ((s >> 1) as u16) << 15;
            for i in 0..3 {
                let mut v = QUATERNION48_BIAS - (q[(s + i) & 3] * -23168.0) as i32;
                if v < QUATERNION48_CLAMP_LIMIT {
                    if v < 0 {
                        v = 0;
                    }
                } else {
                    v = QUATERNION48_CLAMP_HI;
                }
                out[i] = (out[i] & 0x8000) | ((v as u16) & 0x7fff);
            }
            out[2] |= if q[(s + 3) & 3] < 0.0 { 1u16 } else { 0 } << 15;
            let mut b = [0u8; 6];
            for (i, w) in out.iter().enumerate() {
                b[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
            }
            b
        }

        let d2r = std::f64::consts::PI / 180.0;
        let base = angle_q64([0.0, 0.0, std::f64::consts::FRAC_PI_2]);
        let mut ok = 0;
        for (_, euler, want) in GOLDEN {
            let e = [
                euler[0] as f64 * d2r,
                euler[1] as f64 * d2r,
                euler[2] as f64 * d2r,
            ];
            let mut q = mul64(base, angle_q64(e));
            if q[3] < 0.0 {
                q = q.map(|v| -v);
            }
            if &enc64(q) == want {
                ok += 1;
            }
        }
        assert!(
            ok < 24,
            "全程 f64 应当劣于 f32（f32 = 24/24），实测 {ok}/24 —— 若这里变成 24，\
             说明有人把整条链路改成了 f64 而恰好撞对，请复核"
        );
        // 具体命中数取决于「度→弧度」的写法（`deg * (PI/180)` 还是直接用 `FRAC_PI_2`），
        // 因此**不写死数值** —— 真正要守的性质是「严格劣于 f32」。
        assert!(
            ok <= 22,
            "全程 f64 的命中数明显偏低才符合预期，实测 {ok}/24"
        );
    }

    /// 符号规范化是必需的：去掉它会掉到 23/24，且唯一失败的是 w 变负的
    /// `ab_z8` f7（绕 Z 105°）。
    #[test]
    fn sign_canonicalization_is_required() {
        let base = angle_quaternion_f32(ROOT_REFERENCE_ANGLES);
        let mut bad = Vec::new();
        for (name, euler, want) in GOLDEN {
            let e = [deg(euler[0]), deg(euler[1]), deg(euler[2])];
            let q = quaternion_mul(base, angle_quaternion_f32(e)); // 不规范化
            if &encode_quaternion48(q) != want {
                bad.push(*name);
            }
        }
        assert_eq!(bad, vec!["ab_z8 f7"], "不规范化时唯一失败帧应是 ab_z8 f7");
    }

    /// 编码器自洽性：任意单位四元数编码后再解码，应能还原（模掉被丢分量的符号）。
    #[test]
    fn quaternion48_roundtrip_is_self_consistent() {
        for (_, euler, _) in GOLDEN {
            let e = [deg(euler[0]), deg(euler[1]), deg(euler[2])];
            let q = raw_rotation_quaternion(e);
            let enc = encode_quaternion48(q);

            let w0 = u16::from_le_bytes([enc[0], enc[1]]);
            let w1 = u16::from_le_bytes([enc[2], enc[3]]);
            let w2 = u16::from_le_bytes([enc[4], enc[5]]);
            let s = (((w0 >> 15) << 1) | (w1 >> 15)) as usize;
            let k = (s + 3) & 3;

            let mut got = [0.0f32; 4];
            for i in 0..3usize {
                let raw = match i {
                    0 => w0,
                    1 => w1,
                    _ => w2,
                } & 0x7fff;
                got[(s + i) & 3] = (raw as f32 - QUATERNION48_BIAS as f32) / QUATERNION48_SCALE;
            }
            let rest =
                1.0 - (got[0] * got[0] + got[1] * got[1] + got[2] * got[2] + got[3] * got[3]);
            got[k] = rest.max(0.0).sqrt() * if (w2 >> 15) != 0 { -1.0 } else { 1.0 };

            // 还原出的分量应与原分量同号（被丢的那个由符号位决定）
            for i in 0..4 {
                if i == k {
                    continue;
                }
                assert!(
                    (got[i] - q[i]).abs() < 1.0 / QUATERNION48_SCALE,
                    "分量 {i} 还原偏差过大: {} vs {}",
                    got[i],
                    q[i]
                );
            }
        }
    }

    /// 载荷头部：有旋转通道时 28 字节、flags 0x80；无旋转通道时 36 字节、flags 0x40。
    #[test]
    fn payload_header_shapes() {
        let rot = [
            [0.0f32, 0.0, 0.0],
            [0.0, 0.0, deg(30.0)],
            [0.0, 0.0, deg(60.0)],
            [0.0, 0.0, deg(90.0)],
        ];
        let p = write_raw_animation_payload(Some(&rot), None);
        assert_eq!(i32::from_le_bytes(p[0..4].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(p[4..8].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(p[8..12].try_into().unwrap()), 6);
        // `+24` 现在是**逐骨骼 flags 字节数组**（单骨骼 = 1 字节 + 3 字节填充）。
        assert_eq!(p[24], RAW_FLAG_ROT_VARIES);
        assert_eq!(&p[25..28], &[0, 0, 0]);
        assert_eq!(p.len(), 28 + 6 * 4);

        // 只有位置通道（无旋转轨道）→ 无内联常量，数据紧接头后。
        //
        // ⚠️ 旧实现会在这里内联一个「静置旋转样本」把 `+4` 撑到 36；
        // 反编译 `FUN_0046aaa0` 后确认那是**错的** —— 内联区只装
        // **真实存在**的常量轨道。官方 `ab_n4` 的 `+4 = 36` 是因为
        // 它的根骨骼有一条**旋转常量**轨道（`flags[0] = 0x40`），
        // 不是「无旋转就补一个静置样本」。
        let pos = [[0.0f32, 0.0, 0.0], [0.5, 0.0, 0.0]];
        let p2 = write_raw_animation_payload(None, Some(&pos));
        assert_eq!(i32::from_le_bytes(p2[0..4].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(p2[4..8].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(p2[8..12].try_into().unwrap()), 6);
        assert_eq!(p2[24], RAW_FLAG_POS_VARIES);
        assert_eq!(p2.len(), 28 + 6 * 2);
    }

    /// 位置通道 = 3 × float16；`0.5` 的 half 是 `0x3800`（实测 `ab_zp4` f1）。
    #[test]
    fn position_channel_is_vector48() {
        assert_eq!(half_from_f32(0.5), 0x3800);
        assert_eq!(half_from_f32(0.0), 0x0000);
        assert_eq!(half_from_f32(1.0), 0x3c00);
        assert_eq!(half_from_f32(0.75), 0x3a00);
        assert_eq!(half_from_f32(-0.0), 0x8000);
        assert_eq!(half_from_f32(65504.0), 0x7bff);
        assert_eq!(half_from_f32(1.0e9), 0x7bff); // 饱和
    }

    /// `.ani` 容器：416 字节，只有 id / version / length 非零。
    #[test]
    fn ani_container_header() {
        let c = write_ani_container(468);
        assert_eq!(c.len(), ANI_HEADER_SIZE);
        assert_eq!(i32::from_le_bytes(c[0..4].try_into().unwrap()), ANI_ID);
        assert_eq!(i32::from_le_bytes(c[4..8].try_into().unwrap()), ANI_VERSION);
        assert_eq!(i32::from_le_bytes(c[0x4C..0x50].try_into().unwrap()), 468);
        for (i, b) in c.iter().enumerate() {
            if !matches!(i, 0..=7 | 0x4C..=0x4F) {
                assert_eq!(*b, 0, "偏移 0x{i:x} 应为 0");
            }
        }
    }

    /// 防回归：确认「载荷格式只对一个构建负责」这条结论仍然成立。
    ///
    /// 若哪天有人把语料 `.ani` 接成 oracle 且测试开始大面积失败，
    /// 先看这里 —— 语料的 `+0` 不是 28。
    #[test]
    fn payload_format_is_build_specific_not_corpus() {
        // 本模块产出的载荷，+0 恒为 28
        let rot = [[0.0f32, 0.0, 0.0], [0.0, 0.0, deg(30.0)]];
        let p = write_raw_animation_payload(Some(&rot), None);
        assert_eq!(i32::from_le_bytes(p[0..4].try_into().unwrap()), 28);

        // 语料实测的取值（`rsrch_corpus_ani_scan.js`）：56/84/88/92 —— 一个都不是 28。
        // 这里只做「常量不等于语料取值」的守卫，避免有人把 28 改成语料的值。
        for corpus_value in [56, 84, 88, 92] {
            assert_ne!(
                RAW_HEADER_SIZE as i32, corpus_value,
                "本模块复刻的构建 +0 是 28；{corpus_value} 是语料的取值"
            );
        }
    }

    /// **端到端**：`ab_z4` 的完整 `.ani` 载荷逐字节复刻官方产物。
    ///
    /// 官方 `ab_z4.ani` = 416 字节容器头 + 52 字节块（`[416, 468)`），
    /// 块内容 = 28 字节头 + 4 帧 × 6 字节旋转。**没有位置通道**
    /// （位移恒 0，官方不给它开槽）。
    #[test]
    fn ab_z4_ani_payload_is_byte_exact() {
        // 4 帧、绕 Z 0/30/60/90°
        let rots: Vec<[f32; 3]> = [0.0f32, 30.0, 60.0, 90.0]
            .iter()
            .map(|d| [0.0, 0.0, deg(*d)])
            .collect();
        // 位移恒 0 → 按官方规则**不开位置槽**
        let poss: Vec<[f32; 3]> = vec![[0.0; 3]; 4];
        assert!(!channel_varies(&poss), "位移恒定，不该开位置槽");

        let mut channels = Vec::new();
        if channel_varies(&rots) {
            // 单骨骼模型的那一根就是根骨骼 → 左乘基准 `Q(90°Z)`。
            channels.push(RawChannel::rot(rots.clone()));
            channels[0].rot_base = true;
        }
        if channel_varies(&poss) {
            channels.push(RawChannel::pos(poss));
        }
        let payload = write_raw_payload(&channels);

        // 官方块：28 字节头 + 4×6
        assert_eq!(payload.len(), 52, "载荷长度应为 52（stride=6，无位置槽）");
        assert_eq!(i32::from_le_bytes(payload[0..4].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(payload[4..8].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(payload[8..12].try_into().unwrap()), 6);
        assert_eq!(payload[24], RAW_FLAG_ROT_VARIES);

        // 逐字节对照官方 `ab_z4.ani` 的 [416, 468)
        let want: [u8; 52] = [
            0x1c, 0x00, 0x00, 0x00, 0x1c, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00,
            // 4 帧旋转样本
            0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40, //
            0x3f, 0xed, 0x00, 0xc0, 0x00, 0x40, //
            0x6c, 0xd7, 0x00, 0xc0, 0x00, 0x40, //
            0x00, 0xc0, 0x00, 0xc0, 0x00, 0x40,
        ];
        assert_eq!(payload, want, "ab_z4 载荷应与官方逐字节相同");
    }

    /// 完整 `.ani` 文件：容器 + 块 + 块表，与官方 `ab_z4.ani` 同长同内容。
    #[test]
    fn ab_z4_ani_file_is_byte_exact() {
        let rots: Vec<[f32; 3]> = [0.0f32, 30.0, 60.0, 90.0]
            .iter()
            .map(|d| [0.0, 0.0, deg(*d)])
            .collect();
        let mut ch = RawChannel::rot(rots);
        ch.rot_base = true; // 单骨骼 = 根骨骼
        let payload = write_raw_payload(&[ch]);
        let (file, table) = build_ani_file(&[payload]);

        assert_eq!(file.len(), 468, "官方 ab_z4.ani 是 468 字节");
        assert_eq!(table, vec![(0, 0), (416, 468)], "块表（含哨兵）");
        // 容器头
        assert_eq!(i32::from_le_bytes(file[0..4].try_into().unwrap()), ANI_ID);
        assert_eq!(
            i32::from_le_bytes(file[4..8].try_into().unwrap()),
            ANI_VERSION
        );
        assert_eq!(
            i32::from_le_bytes(file[0x4C..0x50].try_into().unwrap()),
            468
        );
    }

    /// 逐骨骼 flags：**每根骨骼一个字节**，且轨道存在性由「增量」决定。
    ///
    /// 用官方受控实验 `abi9`（4 骨骼、`ankle` 恒定 20°，恰好等于它的参考姿态）
    /// 的真实字节：`flags = [0x40, 0x00, 0x00, 0x00]` —— `ankle` **一个位都没有**。
    ///
    /// 若误用「绝对姿态」判存在性，`ankle` 会多出 `0x40`（常量旋转轨道），
    /// 这是本项目实际写错过一次的地方。
    #[test]
    fn per_bone_flags_use_delta_for_existence() {
        let constant = [0.0f32, 0.0, 0.0];
        let rot: Vec<[f32; 3]> = vec![constant; 4];
        // 4 骨骼：只有根骨骼有旋转轨道（承载基准 Q(90°Z)）。
        let tracks = vec![
            RawBoneTrack {
                rot: Some((true, rot.clone())),
                pos: None,
            },
            RawBoneTrack::default(),
            RawBoneTrack::default(),
            // `ankle` 的**绝对**姿态是 20°，但增量恒 0 → 轨道不存在。
            RawBoneTrack::default(),
        ];
        let p = write_raw_payload_tracks(&tracks);
        assert_eq!(&p[24..28], &[0x40, 0x00, 0x00, 0x00], "abi9 实测 flags");
        // `+0` = ALIGN4(24 + 4) = 28；`+4` = ALIGN4(28 + 6) = 36（1 条常量旋转）。
        assert_eq!(i32::from_le_bytes(p[0..4].try_into().unwrap()), 28);
        assert_eq!(i32::from_le_bytes(p[4..8].try_into().unwrap()), 36);
        assert_eq!(
            i32::from_le_bytes(p[8..12].try_into().unwrap()),
            0,
            "无变化通道"
        );
        // 内联常量 = Q(90°Z)（与官方 `abi9` 的 `feff00c00040` 相同）。
        assert_eq!(&p[28..34], &[0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40]);
    }

    /// 基准旋转 `Q(90°Z)` **只作用于根骨骼**；子骨骼存绝对欧拉角。
    ///
    /// 判据（官方 `abi1` 的 `ankle`，子骨骼、绝对姿态恒 0）：
    /// 内联常量是 `Q(0)`（单位四元数）而不是 `Q(90°Z)`。
    /// 早期实现对所有骨骼一律施加基准旋转 → `abi1` 差 8 字节。
    #[test]
    fn base_rotation_applies_to_root_only() {
        let zero = [0.0f32, 0.0, 0.0];
        let rot = vec![zero; 4];
        let tracks = vec![
            RawBoneTrack {
                rot: Some((true, rot.clone())),
                pos: None,
            }, // 根：过基准
            RawBoneTrack::default(),
            RawBoneTrack::default(),
            RawBoneTrack {
                rot: Some((false, rot.clone())),
                pos: None,
            }, // 子：不过基准
        ];
        let p = write_raw_payload_tracks(&tracks);
        assert_eq!(
            &p[24..28],
            &[0x40, 0x00, 0x00, 0x40],
            "b0 与 b3 都是常量旋转"
        );
        // b0 的常量 = Q(90°Z)；b3 的常量 = Q(0) = 单位四元数。
        //
        // ⚠️ 单位四元数的**编码值不是 0**：`Quaternion48` 的偏置是 `0x4000`，
        // 所以 `Q(0)` 编出来是每分量 `0x4000`。
        // 实测官方 `abi7` 的 b3 第 0 帧（绝对角 0）正是 `4000 4000 4000`。
        assert_eq!(&p[28..34], &[0xfe, 0xff, 0x00, 0xc0, 0x00, 0x40], "根骨骼");
        assert_eq!(&p[34..40], &[0x00, 0x40, 0x00, 0x40, 0x00, 0x40], "子骨骼");
    }

    /// 位置通道存**绝对**位置；根骨骼过基准旋转、子骨骼不过。
    ///
    /// 判据（官方 `abiB`）：`root` 的 SMD 位置 `(10,0,0)` 存成 `(0,10,0)`
    /// （= `Rz(90°)·(10,0,0)`），而 `abi1` 的 `ankle` 子骨骼存 `[30,0,5]`
    /// —— **未**旋转。
    #[test]
    fn position_is_absolute_and_root_rotated() {
        let root_pos = vec![[0.0f32, 10.0, 0.0]; 3]; // 已过基准旋转的绝对位置
        let child_pos = vec![[30.0f32, 0.0, 5.0]; 3];
        let tracks = vec![
            RawBoneTrack {
                rot: None,
                pos: Some(root_pos),
            },
            RawBoneTrack {
                rot: None,
                pos: Some(child_pos),
            },
        ];
        let p = write_raw_payload_tracks(&tracks);
        // 两根骨骼都是「位置常量」→ 各 6 字节内联。
        assert_eq!(&p[24..26], &[0x01, 0x01]);
        assert_eq!(
            i32::from_le_bytes(p[4..8].try_into().unwrap()),
            40,
            "28 + 12"
        );
        // 根骨骼 (0,10,0)：half(0)=0x0000, half(10)=0x4900。
        assert_eq!(half_from_f32(10.0), 0x4900);
        assert_eq!(&p[28..34], &[0x00, 0x00, 0x00, 0x49, 0x00, 0x00]);
        // 子骨骼 (30,0,5)：half(30)=0x4f80? 直接验分量即可。
        assert_eq!(half_from_f32(30.0), 0x4f80);
        assert_eq!(half_from_f32(5.0), 0x4500);
        assert_eq!(&p[34..40], &[0x80, 0x4f, 0x00, 0x00, 0x00, 0x45]);
    }

    /// `$animblocksize` 一旦设置就**至少一块** —— 即使没有任何动画进块。
    ///
    /// 实测 `ab_z1`（单帧、全部内联）：`numanimblocks = 2`、
    /// `block[1] = [416, 416)`（空块）。
    #[test]
    fn empty_block_list_still_yields_one_block() {
        let (idx, off, blocks) = pack_anim_blocks(&[None, None], 4096);
        assert_eq!(blocks.len(), 1, "至少要有一块");
        assert!(blocks[0].is_empty(), "该块应为空");
        assert_eq!(idx, vec![0, 0], "没有动画进块");
        assert_eq!(off, vec![None, None]);
    }

    /// 预算耗尽时开新块（复刻 `g_animblocksize < 已用` 这条判据）。
    #[test]
    fn budget_exhaustion_opens_new_block() {
        // 每段 100 字节，预算 150 → 第 1 段进块 0（100 字节），
        // 第 2 段时 `150 < 100` 为假 → 仍进块 0（变成 200）；
        // 第 3 段时 `150 < 200` 为真 → 开块 1。
        let mk = |n: usize| Some(vec![0u8; n]);
        let (idx, _off, blocks) = pack_anim_blocks(&[mk(100), mk(100), mk(100)], 150);
        assert_eq!(idx, vec![1, 1, 2], "前两段同块，第三段开新块");
        assert_eq!(blocks.len(), 2);
        // 块 0 = 第 1 段 100 字节，第 2 段从 `ALIGN16(100) = 112` 起 → 112 + 100 = 212。
        // （对齐是每段前的 `ALIGN16`，所以 100 之后要空 12 字节。）
        assert_eq!(blocks[0].len(), 212, "含第二段前的 ALIGN16 填充");
        assert_eq!(blocks[1].len(), 100);
    }
}
