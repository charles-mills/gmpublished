//! 骨骼参考姿态的矩阵运算。

/// `matrix3x4_t`：3 行 × 4 列，行主序。
pub type Matrix3x4 = [f32; 12];

/// 由欧拉角（弧度）构造四元数，与 Source 的 `AngleQuaternion` 一致。
///
/// 返回 `[x, y, z, w]`（**f32**，因为 `mstudiobone_t.quat` 就是 f32）。
///
/// # 为什么必须写它（而不是留 0）
///
/// `mstudiobone_t.quat`（偏移 `0x2C`）**不是**可选的冗余字段 ——
/// 引擎的 `InitPose` 直接读它来建立参考姿态。写 0 是**非法四元数**
/// （模长 0，无法归一化），会让骨骼姿态失效。
///
/// 实测依据：官方 `v_autoshotgun.mdl` 的 `bone[0]` 是
/// `rot = [1.570796, 0, 0]`、`quat = [0.7071066, 0, 0, 0.7071069]` ——
/// 正是绕 X 轴 π/2 的四元数 `[sin(π/4), 0, 0, cos(π/4)]`。
///
/// 注意参数顺序：本函数收的是 `[roll(x), pitch(y), yaw(z)]`，
/// 而 Source 的 `QAngle` 是 `[pitch, yaw, roll]` —— 内部已换算，
/// 调用方不需要关心。
///
/// **写文件**用这个（结果是 f32）。若要做欧拉角规范化，请用
/// [`canonical_euler`] —— 那条路径必须全程 f64，见该函数的说明。
pub fn angle_quaternion(angles: [f32; 3]) -> [f32; 4] {
    angle_quaternion_f64(angles.map(f64::from)).map(|v| v as f32)
}

/// `AngleQuaternion` 的 **f64** 版本（内部用）。
///
/// # 为什么需要它
///
/// 实测 studiomdl 在「欧拉角 → 四元数 → 矩阵 → 反解欧拉角」这条
/// 规范化路径上**全程用 f64**。若在四元数处降到 f32，规范化结果会差
/// 约 4e-6：
///
/// | 路径 | `bone 66 "bolt"` 的 `rot[0]` |
/// |---|---|
/// | f32 四元数 + f64 矩阵 | `3.135712` |
/// | **f64 四元数 + f64 矩阵** | **`3.135716`** ← 与官方一致 |
/// | 官方骨骼表实测 | `3.135716` |
///
/// 这个 4e-6 会让动画的存储值出现 1 LSB 偏差（89 骨骼 × 30 帧里约 26 处）。
fn angle_quaternion_f64(a: [f64; 3]) -> [f64; 4] {
    let (sr, cr) = (a[0] * 0.5).sin_cos(); // roll  = x
    let (sp, cp) = (a[1] * 0.5).sin_cos(); // pitch = y
    let (sy, cy) = (a[2] * 0.5).sin_cos(); // yaw   = z

    [
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    ]
}

/// 单位矩阵。
pub const IDENTITY: Matrix3x4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// 由四元数构造旋转矩阵的 3×3 部分（`matrix3x4_t` 的行主序，平移列留 0）。
///
/// 与 Source 的 `QuaternionMatrix` 一致。中间量用 `f64`（见
/// `angle_quaternion_f64`）。
pub fn quaternion_matrix(q: [f32; 4]) -> Matrix3x4 {
    quaternion_matrix_f64(q.map(f64::from)).map(|v| v as f32)
}

/// `QuaternionMatrix` 的 f64 版本（内部用）。
fn quaternion_matrix_f64(q: [f64; 4]) -> [f64; 12] {
    let [x, y, z, w] = q;
    [
        1.0 - 2.0 * y * y - 2.0 * z * z,
        2.0 * x * y - 2.0 * w * z,
        2.0 * x * z + 2.0 * w * y,
        0.0,
        2.0 * x * y + 2.0 * w * z,
        1.0 - 2.0 * x * x - 2.0 * z * z,
        2.0 * y * z - 2.0 * w * x,
        0.0,
        2.0 * x * z - 2.0 * w * y,
        2.0 * y * z + 2.0 * w * x,
        1.0 - 2.0 * x * x - 2.0 * y * y,
        0.0,
    ]
}

/// `QuaternionNormalize(q)`（`mathlib_base.cpp:1625`）—— 按模长归一化。
///
/// `radius == 0` 时**原样返回**（官方也只在这个分支里做除法），
/// 不会产生 NaN —— 这一点在 `QuaternionMA` 里很重要，因为
/// `QuaternionScale` 在极端输入下可能给出零四元数。
pub fn quaternion_normalize(q: [f32; 4]) -> [f32; 4] {
    let radius = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
    if radius != 0.0 {
        let iradius = 1.0 / radius.sqrt();
        [
            q[0] * iradius,
            q[1] * iradius,
            q[2] * iradius,
            q[3] * iradius,
        ]
    } else {
        q
    }
}

/// `QuaternionScale(p, t, q)`（`mathlib_base.cpp:1647`）—— 沿旋转轴缩放角度。
///
/// # 为什么必须逐字复刻（而不是「`t == 1` 就直接返回 `p`」）
///
/// 直觉上 `t == 1` 是恒等操作，但官方**仍然**走完整条路径：
///
/// ```c
/// float sinom  = MIN( sqrt(DotProduct(&p.x, &p.x)), 1.f );
/// float sinsom = sin( asin( sinom ) * t );
/// t = sinsom / (sinom + FLT_EPSILON);   // ← 分母多了 FLT_EPSILON
/// VectorScale( &p.x, t, &q.x );
/// ```
///
/// `sin(asin(x))` 在浮点下**不精确等于** `x`，`t` 又被 `sinom + FLT_EPSILON`
/// 除了一次 —— 所以 `QuaternionScale(q, 1)` 与 `q` 相差约 `1e-7`。
/// 这个差会被 `QuaternionMA` 的归一化放大到可观测的级别，进而在
/// **压缩误差载荷的量化边界**上翻转某个采样值。
///
/// `w` 分量**保留 `p.w` 的符号**（`if (p.w < 0) q.w = -r; else q.w = r;`）——
/// 不是简单的 `±r` 复制。
pub fn quaternion_scale(p: [f32; 4], t: f32) -> [f32; 4] {
    /// `FLT_EPSILON`（`<float.h>`）。
    const FLT_EPSILON: f32 = 1.192_092_9e-7;

    let sinom = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt().min(1.0);
    let sinsom = (sinom.asin() * t).sin();
    let t = sinsom / (sinom + FLT_EPSILON);
    // `r = 1 - sinsom²` 在官方被夹到非负后再开方。
    let r = (1.0 - sinsom * sinsom).max(0.0).sqrt();
    let w = if p[3] < 0.0 { -r } else { r };
    [p[0] * t, p[1] * t, p[2] * t, w]
}

/// `QuaternionMult(p, q, qt)`（`mathlib_base.cpp:1727`）—— `p · q`。
///
/// ⚠️ **必须先做 `QuaternionAlign`** —— 官方在相乘**之前**调它，把 `q` 翻成与
/// `p`「同半球」的那一个（`a·b ≥ 0`）。四元数 `q` 与 `−q` 表示**同一个
/// 旋转**，但 `(s·p)·q` 的结果会因此差一个负号 —— 分解回欧拉角时
/// 表现为「角度方向反了」。
///
/// 漏掉它时 `subtract` 的结果在**纯单轴**旋转上仍然对（负号在分解时
/// 被规范化吃掉），但复合旋转会错 —— 实测表现是 `look_poses` 三格
/// 让 `rotscale` 的全局极值涨了 10 倍以上（74/118 根骨骼的 rotscale
/// 与官方不符，而正确实现只有 4/118）。
pub fn quaternion_mult(p: [f32; 4], q: [f32; 4]) -> [f32; 4] {
    // `QuaternionAlign(p, q, q2)`：若 `|p − q|² > |p + q|²` 就取 `−q`。
    let mut a = 0.0f32;
    let mut b = 0.0f32;
    for i in 0..4 {
        a += (p[i] - q[i]) * (p[i] - q[i]);
        b += (p[i] + q[i]) * (p[i] + q[i]);
    }
    let q = if a > b {
        [-q[0], -q[1], -q[2], -q[3]]
    } else {
        q
    };
    [
        p[0] * q[3] + p[1] * q[2] - p[2] * q[1] + p[3] * q[0],
        -p[0] * q[2] + p[1] * q[3] + p[2] * q[0] + p[3] * q[1],
        p[0] * q[1] - p[1] * q[0] + p[2] * q[3] + p[3] * q[2],
        -p[0] * q[0] - p[1] * q[1] - p[2] * q[2] + p[3] * q[3],
    ]
}

/// `QuaternionMA(p, s, q, qt)`（`bone_setup.cpp:1158`）—— `qt = p · (s · q)`。
///
/// 三步：`QuaternionScale(q, s)` → `QuaternionMult(p, ·)` → `QuaternionNormalize`。
/// 这是官方 `CalcBoneTransforms` 重建 `STUDIO_DELTA` 动画时用的核心原语。
pub fn quaternion_ma(p: [f32; 4], s: f32, q: [f32; 4]) -> [f32; 4] {
    quaternion_normalize(quaternion_mult(p, quaternion_scale(q, s)))
}

/// 由四元数 + 位置构造局部变换（官方的 `AngleMatrix(Quaternion, Vector, m)`，
/// `mathlib_base.cpp:1751`）。
///
/// ⚠️ **不能**用 [`local_transform`]（那条路走 `AngleMatrix(euler)` 再
/// `quaternion_angles` 绕一圈）—— `quaternion_angles` 带**万向锁分支**
/// （见 [`matrix_angles`]），在锁死附近会丢掉一个自由度，与官方直接由
/// 四元数建矩阵的结果不同。
pub fn quaternion_local_transform(q: [f32; 4], position: [f32; 3]) -> Matrix3x4 {
    let mut m = quaternion_matrix(q);
    m[3] = position[0];
    m[7] = position[1];
    m[11] = position[2];
    m
}

/// 由旋转矩阵反解欧拉角，与 Source 的 `MatrixAngles(matrix, float *angles)` 一致。
///
/// 返回 `[roll(x), pitch(y), yaw(z)]`（弧度）。
///
/// # 为什么必须逐字复刻这个函数（而不是用「标准」公式）
///
/// 欧拉角**不唯一**，同一个旋转有多个等价三元组。动画数据里存的是
/// studiomdl 用**这个具体实现**分解出来的那一组，所以必须用同一个
/// 分支逻辑才能逐值复现。
///
/// 关键点是**万向锁判据 `xyDist > 0.001`**（forward 在 XY 平面的投影
/// 长度）—— 这是**矩阵元素**的量纲，不是角度，也不是常见的
/// `|pitch| ≈ π/2`。锁死时强制 `roll = 0`，把自由度全部并进 yaw。
/// 实测（`gimbal` 实验，pitch = π/2）：官方把
/// `[0.35, π/2, −0.25]` 分解成 `[0, π/2, −0.6]`。
pub fn matrix_angles(m: &Matrix3x4) -> [f32; 3] {
    matrix_angles_f64(&m.map(f64::from)).map(|v| v as f32)
}

/// `MatrixAngles` 的 f64 版本（内部用）。
///
/// # 为什么全程 f64
///
/// 实测 studiomdl 在这条路径上用 **f64**。降到 f32 会让结果差约 4e-6：
///
/// | 精度 | `bone 66 "bolt"` 的 `rot[0]` |
/// |---|---|
/// | Q32 + M32 | `3.135712` |
/// | **Q64 + M64** | **`3.135716`** ← 与官方骨骼表逐位相同 |
/// | 官方实测 | `3.135716` |
///
/// # 角度制往返**不**复刻
///
/// Source 的 `RadianEuler` 重载会 `RAD2DEG` 再 `DEG2RAD` 走一遍。实测
/// 加上它对逐位命中率**没有改善**（`Q64M64` 与 `Q64M64+deg32` 都是
/// 80/267），最大偏差也相同（2.384e-7），所以不做这层往返 ——
/// 少一次浮点运算，少一个可能出错的环节。
///
/// 剩余 2.4e-7 量级的差异（约 6 ulp）来自 studiomdl 内部更早的
/// 中间量精度（它可能是 f64 存储 + 编译器向量化重排），
/// **不影响任何量化结果**：实测 5370 个动画采样里只有 18 个差 1 LSB。
fn matrix_angles_f64(m: &[f64; 12]) -> [f64; 3] {
    // Source 按列取基向量：forward = 第 0 列，left = 第 1 列，up.z = m[2][2]。
    let forward = [m[0], m[4], m[8]];
    let left = [m[1], m[5], m[9]];
    let up_z = m[10];

    let xy_dist = (forward[0] * forward[0] + forward[1] * forward[1]).sqrt();
    /// 万向锁判据（矩阵元素量纲）。
    const GIMBAL_THRESHOLD: f64 = 0.001;

    let (yaw, pitch, roll) = if xy_dist > GIMBAL_THRESHOLD {
        (
            forward[1].atan2(forward[0]),
            (-forward[2]).atan2(xy_dist),
            left[2].atan2(up_z),
        )
    } else {
        // 万向锁：自由度丢失，强制 roll = 0。
        ((-left[0]).atan2(left[1]), (-forward[2]).atan2(xy_dist), 0.0)
    };
    [roll, pitch, yaw]
}

/// 由旋转矩阵反解**四元数** —— Source 的
/// `MatrixAngles( const matrix3x4_t &matrix, Quaternion &q, Vector &pos )`
/// 的前半段（`mathlib_base.cpp:148-204`），**不含** `pos`。
///
/// # 为什么需要它
///
/// `CompressIKErrors`（`simplify.cpp:6672/6722`）对旋转通道做的是
///
/// ```cpp
/// RadianEuler ang;
/// QuaternionAngles( pRule->pError[n].q, ang );   // ← q 来自 MatrixAngles(matrix, q, pos)
/// v = ang[k-3];
/// ```
///
/// 也就是 **矩阵 → 四元数 → 矩阵 → 欧拉角** 的完整往返。四元数的符号
/// （`q` 与 `−q`）以及归一化都会影响第二次分解落到哪个分支，所以不能
/// 简单地用 `matrix_angles(&m)` 抄近路 —— 必须逐字复刻这条路径。
///
/// 全程 `f64`：原二进制走 x87 80 位中间精度，用 `f64` 模拟比 `f32` 更接近
/// （与 [`matrix_angles`] 同样的理由与实测结论）。
pub fn matrix_quaternion(m: &Matrix3x4) -> [f32; 4] {
    matrix_quaternion_f64(&m.map(f64::from)).map(|v| v as f32)
}

/// `matrix_quaternion` 的 f64 版本（内部用）。
///
/// # ⚠️ 存储是**行主序**（`f[r*4 + c] == m[r][c]`）—— 注释曾写反
///
/// `angle_matrix` 写的是 `[cp*cy, sr*sp*cy−cr*sy, cr*sp*cy+sr*sy, 0, cp*sy, …]`，
/// 与官方 `AngleMatrix(RadianEuler)` 的 `matrix[row][col]` 逐一对应：
///
/// | 下标 | mdlc 值 | 官方 |
/// |---|---|---|
/// | `f[0]` | `cp*cy` | `m[0][0]` |
/// | `f[1]` | `sr*sp*cy−cr*sy` | `m[0][1]` |
/// | `f[4]` | `cp*sy` | `m[1][0]` |
/// | `f[9]` | `sr*cp` | `m[2][1]` |
///
/// ⟹ `f[r*4+c] = m[r][c]`，**行主序**。
///
/// # R22：`m12` 与 `m21` 曾经互换（真 bug）
///
/// 原实现按「列主序」取下标：
/// ```text
/// let (m10, m12) = (f[4], f[9]);   // m12 取了 f[9]（那是 m[2][1]）
/// let (m20, m21) = (f[8], f[6]);   // m21 取了 f[6]（那是 m[1][2]）
/// ```
/// 行主序下正确的下标是 `m12 = f[6]`、`m21 = f[9]` —— **恰好互换**。
///
/// 后果：四元数的 `x` 分量用 `m21 − m12` 算（`mathlib_base.cpp:1129`），
/// 互换后变成 `m12 − m21` ⟹ **x 分量符号翻转**，`w` 也受影响 ⟹
/// 解出的旋转是**另一个旋转**（实测 IK 误差的旋转三轴差最大 **1.24 rad**）。
///
/// 判据（`docs/_probe/probe_ik_error_census.js`）：
/// `v_silenced_smg` `a_idle_1` 的 `rhand touch` 规则，
/// 位置三轴**逐位相同**、旋转三轴差 `0.98 / 1.24 / 0.19`。
///
/// ⚠️ 之所以长期没被发现：这条路径**只被 IK 误差用到**，而
/// 「无 IK rule 的动画」和「`IK_RELEASE`（载荷全 0）」都碰不到它。
fn matrix_quaternion_f64(f: &[f64; 12]) -> [f64; 4] {
    // `f[r*4 + c] == m[r][c]`（行主序）。
    let (m00, m11, m22) = (f[0], f[5], f[10]);
    let (m01, m02) = (f[1], f[2]);
    let (m10, m12) = (f[4], f[6]);
    let (m20, m21) = (f[8], f[9]);

    let mut trace = m00 + m11 + m22 + 1.0;
    let mut q;
    if trace > 1.0 + f64::EPSILON {
        q = [m21 - m12, m02 - m20, m10 - m01, trace];
    } else if m00 > m11 && m00 > m22 {
        trace = 1.0 + m00 - m11 - m22;
        q = [trace, m10 + m01, m02 + m20, m21 - m12];
    } else if m11 > m22 {
        trace = 1.0 + m11 - m00 - m22;
        q = [m01 + m10, trace, m21 + m12, m02 - m20];
    } else {
        trace = 1.0 + m22 - m00 - m11;
        q = [m02 + m20, m21 + m12, trace, m10 - m01];
    }

    // `QuaternionNormalize`（`mathlib_base.cpp`）：半径为 0 时**原样返回**。
    let radius = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if radius != 0.0 {
        let inv = 1.0 / radius;
        for v in &mut q {
            *v *= inv;
        }
    }
    q
}

/// Source 的 `QuaternionAngles( const Quaternion &q, RadianEuler &angles )`
/// —— 返回 **`[roll, pitch, yaw]`（弧度）**。
///
/// `mathlib_base.cpp:2075-2086` 的实现是
/// `QuaternionMatrix(q, matrix); MatrixAngles(matrix, angles);`，而
/// `mathlib.h:752-757` 的 `RadianEuler` 重载是
///
/// ```cpp
/// MatrixAngles( matrix, &angles.x );                                   // 角度制、[pitch, yaw, roll]
/// angles.Init( DEG2RAD( angles.z ), DEG2RAD( angles.x ), DEG2RAD( angles.y ) );
/// ```
///
/// 即**先按角度制分解，再转弧度并换序**成 `[roll, pitch, yaw]`。
pub fn quaternion_angles(q: [f32; 4]) -> [f32; 3] {
    matrix_angles(&quaternion_matrix(q))
}

/// 把任意欧拉角三元组规范化为 studiomdl 会写出的那一组。
///
/// # 这条规则纠正了 `docs/animation-layout.md` 的核心结论
///
/// 文档 §3.5 写「逐帧值 = 相对第 0 帧的增量」，依据是 `exp50`/`exp53`/
/// `exp64`/`exp65` 等受控实验。但那些 SMD 的**第 0 帧旋转恰好都是 0**，
/// 此时「绝对」「增量」给出**完全相同**的结果 —— 实验无法区分。
///
/// 在真实规模模型（89 骨骼 × 30 帧）上实测，两种模型的命中率是：
///
/// | 模型 | 命中率 |
/// |---|---|
/// | 相对第 0 帧的增量（文档写的） | **13.14%** |
/// | `canonical_euler(pose) − 参考姿态` | **100.00%** |
///
/// 所以真正的流程是：**先规范化，再与参考姿态逐分量相减**。
///
/// # 实测判据（三个独立实验）
///
/// | 实验 | 构造 | 结果 |
/// |---|---|---|
/// | `biganim` | 89 骨骼、参考姿态全 0 | `canonical − ref` 7110/7110，`delta(f0)` 934/7110 |
/// | `refpose` | 3 骨骼、参考姿态非零 | `canonical − ref` 54/54，`delta(f0)` 18/54 |
/// | `bigref` | 参考姿态 30°/40°/50°（大角度，能区分欧拉相减与四元数相对） | `canonical − ref` 30/30，四元数相对 12/30 |
/// | `gimbal` | pitch = π/2 万向锁 + 非零参考 | `canonical(smd) − ref` 15/15，`canonical(smd − ref)` 2/15 |
///
/// # 为什么不是四元数相对旋转
///
/// 「`conj(ref_q) · pose_q` 再分解」看起来更「数学正确」，但实测只有
/// 40%（`bigref`）/ 40%（`refpose`）。studiomdl 做的是**朴素的欧拉分量
/// 相减**，且相减发生在**规范化之后**（`gimbal` 实验排除了先减后规范化）。
pub fn canonical_euler(angles: [f32; 3]) -> [f32; 3] {
    // **全程 f64**：四元数、矩阵、反解都不降到 f32。
    //
    // 若在四元数处降到 f32，规范化结果会差约 4e-6（实测
    // `bone 66 "bolt"` 的 `rot[0]`：f32 路径给 `3.135712`，
    // f64 路径给 `3.135716`，而官方骨骼表是 `3.135716`）。
    // 这个偏差会让动画存储值出现 1 LSB 误差。
    let q = angle_quaternion_f64(angles.map(f64::from));
    let m = quaternion_matrix_f64(q);
    matrix_angles_f64(&m).map(|v| v as f32)
}

/// 由欧拉角（弧度，x/y/z = roll/pitch/yaw）构造旋转矩阵。
///
/// # 约定必须与 Source 的 `AngleMatrix` 一致
///
/// `R = Rz(yaw) · Ry(pitch) · Rx(roll)`，展开为：
///
/// ```text
/// [ cp·cy,  sr·sp·cy − cr·sy,  cr·sp·cy + sr·sy ]
/// [ cp·sy,  sr·sp·sy + cr·cy,  cr·sp·sy − sr·cy ]
/// [ −sp,    sr·cp,             cr·cp            ]
/// ```
///
/// # 这条曾经写错过（且注释谎称有实测依据）
///
/// 早先的实现写的是上面矩阵的**转置**（即逆旋转）。对 `rotation = 0` 的
/// 合成模型完全看不出问题，但真实模型的第一根骨骼几乎都带 π/2 旋转 ——
/// 用错约定会让 `poseToBone` 变成 Source 的 `R` 而不是 `R⁻¹`，
/// 结果是**顶点被反向旋转，模型在游戏里扭曲，而编译器一声不吭**。
///
/// 实测依据（现在真的跑过了）：官方 `v_autoshotgun.mdl` 的
/// `bone[0].rot = [1.570796, 0, 0]`，其 `poseToBone` 旋转部分是
/// `[[1,0,0],[0,~0,1],[0,-1,~0]]` —— 正是本函数结果的**转置**
/// （因为 `poseToBone` 是世界的逆）。逐骨骼比对见
/// `real_model_pose_to_bone_matches_official` 测试。
pub fn angle_matrix(angles: [f32; 3]) -> Matrix3x4 {
    let (sr, cr) = angles[0].sin_cos();
    let (sp, cp) = angles[1].sin_cos();
    let (sy, cy) = angles[2].sin_cos();

    [
        cp * cy,
        sr * sp * cy - cr * sy,
        cr * sp * cy + sr * sy,
        0.0,
        cp * sy,
        sr * sp * sy + cr * cy,
        cr * sp * sy - sr * cy,
        0.0,
        -sp,
        sr * cp,
        cr * cp,
        0.0,
    ]
}

/// 欧拉角的**逆**旋转矩阵（官方 `AngleIMatrix(RadianEuler)`）。
///
/// 官方 `AngleIMatrix`（`mathlib_base.cpp:2864-2903`）与 [`angle_matrix`]
/// 的关系就是**转置** —— 逐项展开后完全一致，而正交旋转的转置即逆。
/// 所以这里直接用转置实现：比再抄一遍 9 个三角函数表达式不容易抄错，
/// 也不会出现两份公式各自漂移。
///
/// 唯一调用方是 `$attachment ... absolute`（官方在**解析期**用它覆盖
/// `local` 的旋转部分，`studiomdl.cpp:5246`）。
pub fn angle_imatrix(angles: [f32; 3]) -> Matrix3x4 {
    let m = angle_matrix(angles);
    [
        m[0], m[4], m[8], 0.0, //
        m[1], m[5], m[9], 0.0, //
        m[2], m[6], m[10], 0.0,
    ]
}

/// 复合变换：`a ∘ b`（先应用 b，再应用 a）。
///
/// 对骨骼层级就是 `world_child = world_parent ∘ local_child`。
pub fn concat(a: &Matrix3x4, b: &Matrix3x4) -> Matrix3x4 {
    let mut out = [0.0f32; 12];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 4 + c] = a[r * 4] * b[c] + a[r * 4 + 1] * b[4 + c] + a[r * 4 + 2] * b[8 + c];
        }
        // 平移列：a 的旋转 × b 的平移 + a 的平移。
        out[r * 4 + 3] =
            a[r * 4] * b[3] + a[r * 4 + 1] * b[7] + a[r * 4 + 2] * b[11] + a[r * 4 + 3];
    }
    out
}

/// 求逆（假设旋转部分正交、无缩放）。
///
/// 正交矩阵的逆 = 转置，所以只需转置旋转部分并把平移取负再旋转。
/// 用这个性质而不是通用高斯消元：更快、且数值上不会引入误差。
pub fn invert(m: &Matrix3x4) -> Matrix3x4 {
    // 旋转部分转置：源是 3 行 × 4 列（第 4 列是平移），转置后得到 3×3，
    // 按行主序存在 r[0..9] 里（r[row*3 + col]）。
    let r = [
        m[0], m[4], m[8], // 第 0 行
        m[1], m[5], m[9], // 第 1 行
        m[2], m[6], m[10], // 第 2 行
    ];
    // 平移：-R^T · t
    let t = [m[3], m[7], m[11]];
    let inv_t = [
        -(r[0] * t[0] + r[1] * t[1] + r[2] * t[2]),
        -(r[3] * t[0] + r[4] * t[1] + r[5] * t[2]),
        -(r[6] * t[0] + r[7] * t[1] + r[8] * t[2]),
    ];
    [
        r[0], r[1], r[2], inv_t[0], //
        r[3], r[4], r[5], inv_t[1], //
        r[6], r[7], r[8], inv_t[2],
    ]
}

/// 单位变换（`SetIdentityMatrix`）。
pub fn identity() -> Matrix3x4 {
    [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0,
    ]
}

/// 由位置 + 欧拉角构造骨骼的局部变换。
pub fn local_transform(position: [f32; 3], angles: [f32; 3]) -> Matrix3x4 {
    let mut m = angle_matrix(angles);
    m[3] = position[0];
    m[7] = position[1];
    m[11] = position[2];
    m
}

/// 求每根骨骼的世界参考姿态矩阵（`boneToPose`）。
///
/// `positions` / `rotations` 是**局部**（父相对）姿态，`parents` 给出父下标。
/// 要求父下标小于子下标，一遍正向遍历即可。
pub fn compute_world(
    positions: &[[f32; 3]],
    rotations: &[[f32; 3]],
    parents: &[i32],
) -> Vec<Matrix3x4> {
    let mut world: Vec<Matrix3x4> = Vec::with_capacity(positions.len());
    for i in 0..positions.len() {
        let local = local_transform(positions[i], rotations[i]);
        let w = match parents.get(i).copied().unwrap_or(-1) {
            p if p >= 0 && (p as usize) < i => match world.get(p as usize) {
                Some(parent) => concat(parent, &local),
                None => local,
            },
            _ => local,
        };
        world.push(w);
    }
    world
}

/// 取矩阵的第 `c` 列（`c` = 0/1/2 为旋转基，3 为平移）。
fn column(m: &Matrix3x4, c: usize) -> [f32; 3] {
    [m[c], m[4 + c], m[8 + c]]
}

/// 写入矩阵的第 `c` 列。
fn set_column(m: &mut Matrix3x4, c: usize, v: [f32; 3]) {
    m[c] = v[0];
    m[4 + c] = v[1];
    m[8 + c] = v[2];
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len == 0.0 {
        return [0.0; 3];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

/// 每根骨骼的**局部** `(位置, 旋转)` 序列，按骨骼下标排列。
///
/// 抽成别名只为压掉 `clippy::type_complexity` —— 它出现在
/// [`realign_bones`] 的返回类型里。
pub type BonePoses = Vec<([f32; 3], [f32; 3])>;

/// **骨骼轴重对齐**（`RealignBones`，`simplify.cpp:4224-4425`）。
///
/// # 它解决什么问题
///
/// Source 的骨骼约定是「骨骼的 **+X 轴**指向子骨骼」。美术在 DCC 里摆的
/// 骨架经常不满足这一点（子骨骼在父空间的局部位移不沿 +X），于是
/// studiomdl 在写出前**重排骨骼的局部基**，让 X 轴指向子骨骼，
/// 同时保持骨骼的**世界位置不变**。
///
/// # 触发条件（两条独立的路径）
///
/// 1. **`$ikchain`**（`simplify.cpp:4236-4259`）—— 对每条链的相邻两段
///    填 `childbone[]`：`childbone[link[0]] = link[1]`、
///    `childbone[link[1]] = link[2]`。
/// 2. **`$realignbones`**（`simplify.cpp:4261-4286`）—— 把判据放宽到
///    「父骨骼只有唯一子骨骼」的所有骨骼。
///
/// # 判据
///
/// 对每根有 `childbone` 的骨骼 `k`（`simplify.cpp:4296-4304`）：
///
/// ```cpp
/// float d = g_bonetable[childbone[k]].pos.Length();   // 子骨骼的**局部**位移长度
/// if (d - g_bonetable[childbone[k]].pos.x > 0.01)     // 不在 +X 轴上
/// ```
///
/// 注意比的是**局部位移**（父相对），不是世界坐标。
///
/// # 重排算法（`simplify.cpp:4310-4368`）
///
/// ```text
/// forward = normalize(childWorldPos − thisWorldPos)   // 指向子骨骼
/// 在原始基里挑一个与 forward 最不平行的轴：
///   d1 = |dot(forward, X)|, d2 = |dot(forward, Y)|, d3 = |dot(forward, Z)|
///   取最小者对应的轴 A
/// up   = normalize(cross(forward, A))
/// left = cross(up, forward)
/// 新基 = (X=forward, Y=left, Z=up)，平移保持 v3（原世界位置）
/// ```
///
/// 之后重建局部姿态（`simplify.cpp:4406-4425`）：
/// `local = inverse(newWorld[parent]) ∘ newWorld[k]`，再 `MatrixAngles`。
///
/// # 验证
///
/// 受控实验 `ipr1/ipr2/ipr3`（同一份 SMD，骨骼沿 Z 轴排列）：
///
/// | QC | 结果 |
/// |---|---|
/// | `ipr1`（无触发） | 骨骼**不**重排：`pos=[0,0,10]`、`rot=[0,0,0]` |
/// | `ipr2`（`$ikchain`） | 重排：`a.pos=[10,0,0]`、`b.rot=[π/2,0,π/2]` |
/// | `ipr3`（`$realignbones`） | 与 `ipr2` **逐位相同** |
///
/// # 返回值
///
/// `(poses, src_realign)`：
///
/// - `poses[i]` = 骨骼 `i` 重排后的**局部** `(pos, rot)` —— 直接写进骨骼表；
/// - `src_realign[i]` = `srcWorld⁻¹ ∘ newWorld`（`simplify.cpp:4397`）——
///   动画构建姿态时用 `srcWorld ∘ srcRealign` 把**源**骨架的姿态搬进
///   重排后的空间（`simplify.cpp:1527`）。漏掉它会让动画的骨骼位置整体错位
///   （不报错，只是动作不对）。
pub fn realign_bones(
    positions: &[[f32; 3]],
    rotations: &[[f32; 3]],
    parents: &[i32],
    childbone: &[i32],
    pre_aligned: &[bool],
) -> (BonePoses, Vec<Matrix3x4>) {
    realign_bones_with_forced(positions, rotations, parents, childbone, pre_aligned, &[])
}

pub fn realign_bones_with_forced(
    positions: &[[f32; 3]],
    rotations: &[[f32; 3]],
    parents: &[i32],
    childbone: &[i32],
    pre_aligned: &[bool],
    forced: &[(usize, [f32; 3])],
) -> (BonePoses, Vec<Matrix3x4>) {
    let n = positions.len();
    let mut world = compute_world(positions, rotations, parents);
    // Source 在循环前把世界矩阵**快照**一份（`simplify.cpp:4290-4293`），
    // 之后挑轴时读快照、取平移时读活值。
    let snapshot = world.clone();

    for k in 0..n {
        // `simplify.cpp:4299` 的 `!g_bonetable[k].bPreAligned` —— 被
        // `$definebone` / `$importbone` 标记过的骨骼**整个跳过**重排。
        //
        // 这类骨骼的姿态是美术在 DCC 里**手工对齐**过的，重排会破坏它。
        // 实测（`ipq2`/`ipq3`）：3 根骨骼全部 `$definebone` 时，
        // 即使写了 `$realignbones` 也**一根都不重排**。
        if pre_aligned.get(k).copied().unwrap_or(false) {
            continue;
        }
        let Some(&child) = childbone.get(k) else {
            continue;
        };
        if child < 0 {
            continue;
        }
        let child = child as usize;
        if child >= n {
            continue;
        }
        // 判据用的是子骨骼的**局部**位移。
        let cp = positions[child];
        let d = (cp[0] * cp[0] + cp[1] * cp[1] + cp[2] * cp[2]).sqrt();
        if d - cp[0] <= 0.01 {
            continue; // 已在 +X 轴上
        }

        let v2 = column(&world[child], 3);
        let v3 = column(&world[k], 3);
        let forward = normalize([v2[0] - v3[0], v2[1] - v3[1], v2[2] - v3[2]]);

        // 在**原始**基里挑与 forward 最不平行的轴。
        let forward2 = column(&snapshot[k], 0);
        let left2 = column(&snapshot[k], 1);
        let up2 = column(&snapshot[k], 2);
        let d1 = dot(forward, forward2).abs();
        let d2 = dot(forward, left2).abs();
        let d3 = dot(forward, up2).abs();
        let pick = if d1 <= d2 && d1 <= d3 {
            forward2
        } else if d2 <= d1 && d2 <= d3 {
            left2
        } else {
            up2
        };

        let up = normalize(cross(forward, pick));
        let left = cross(up, forward);

        set_column(&mut world[k], 0, forward);
        set_column(&mut world[k], 1, left);
        set_column(&mut world[k], 2, up);
        // 平移保持原世界位置 —— 重排**不移动**骨骼。
        set_column(&mut world[k], 3, v3);
    }

    for &(bone, rotation) in forced {
        if !pre_aligned.get(bone).copied().unwrap_or(false) {
            world[bone] = concat(&world[bone], &angle_matrix(rotation));
        }
    }

    // `srcRealign`（`simplify.cpp:4390-4401`）：把**源**骨骼世界变换搬进重排后的空间。
    //
    // 定义是 `srcRealign = srcWorld⁻¹ ∘ newWorld`，于是
    // `srcWorld ∘ srcRealign = newWorld` —— 这正是 `simplify.cpp:1527`
    // 在构建动画姿态时做的（`ConcatTransforms(srcBoneToWorld, srcRealign, dest)`）。
    //
    // 对**参考帧**，`srcWorld == snapshot`，所以结果就是 `newWorld`；
    // 对动画的其它帧，它把源骨架的姿态整体搬到重排后的骨架上。
    // `srcRealign`（`simplify.cpp:4390-4401`）：把**源**骨骼世界变换搬进重排后的空间。
    //
    // 定义是 `srcRealign = srcWorld⁻¹ ∘ newWorld`，于是
    // `srcWorld ∘ srcRealign = newWorld` —— 这正是 `simplify.cpp:1527`
    // 在构建动画姿态时做的（`ConcatTransforms(srcBoneToWorld, srcRealign, dest)`）。
    //
    // 对**参考帧**，`srcWorld == snapshot`，所以结果就是 `newWorld`；
    // 对动画的其它帧，它把源骨架的姿态整体搬到重排后的骨架上。
    //
    // 官方的这个循环对 pre-aligned 骨骼是**跳过**的（4392），于是它们保留
    // `$definebone` 给的 `srcRealign`。这里算出来天然是**单位阵**（因为
    // `world[k]` 没被改过）—— 正是 6 数字形式下 `SetIdentityMatrix`
    // （`studiomdl.cpp:5949`）的结果。12 数字形式的显式矩阵由调用方覆盖
    // （见 `compile.rs` 的 `explicit` 循环）。
    let src_realign: Vec<Matrix3x4> = (0..n)
        .map(|k| concat(&invert(&snapshot[k]), &world[k]))
        .collect();

    // 重建局部姿态（`simplify.cpp:4406-4425`）。
    //
    // **同样跳过 pre-aligned 骨骼**（4408）—— 它们的 `rot`/`pos` 保持
    // `$definebone` 给的值，**不**从 `boneToPose` 反解。
    //
    // 这两处跳过缺一不可：只跳过第一处（重排）的话，`a` 的局部姿态仍然会
    // 被 `invert(world[parent]) ∘ world[a]` 重算 —— 而父骨骼的基已经变了，
    // 于是算出来的是 `[10,0,0]` 而不是 `$definebone` 写的 `[0,0,10]`。
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        if pre_aligned.get(k).copied().unwrap_or(false) {
            out.push((positions[k], rotations[k]));
            continue;
        }
        let bonematrix = match parents.get(k).copied().unwrap_or(-1) {
            p if p >= 0 => concat(&invert(&world[p as usize]), &world[k]),
            _ => world[k],
        };
        out.push((
            [bonematrix[3], bonematrix[7], bonematrix[11]],
            matrix_angles(&bonematrix),
        ));
    }
    (out, src_realign)
}

/// 求每根骨骼的 `poseToBone`。
///
/// `positions` / `rotations` 按骨骼下标给出**局部**参考姿态（相对父骨骼），
/// `parents` 给出父骨骼下标（-1 为根）。
///
/// 返回与输入等长的数组，第 i 项是骨骼 i 的 `poseToBone`。
///
/// 要求父骨骼下标**小于**子骨骼下标（调用方已在校验里保证），
/// 所以一遍正向遍历即可算出全部世界矩阵。
pub fn compute_pose_to_bone(
    positions: &[[f32; 3]],
    rotations: &[[f32; 3]],
    parents: &[i32],
) -> Vec<Matrix3x4> {
    debug_assert_eq!(positions.len(), parents.len());
    debug_assert_eq!(positions.len(), rotations.len());

    let mut world: Vec<Matrix3x4> = Vec::with_capacity(positions.len());
    for i in 0..positions.len() {
        let local = local_transform(positions[i], rotations[i]);
        let w = match parents.get(i).copied().unwrap_or(-1) {
            p if p >= 0 && (p as usize) < i => {
                // 父骨骼一定已经在 world 里（因为 p < i）。
                match world.get(p as usize) {
                    Some(parent) => concat(parent, &local),
                    None => local,
                }
            }
            // -1（根）或非法父级（越界/前向引用）—— 退化为根，避免 panic。
            // 调用方应当已经报错，这里只是不让坏输入把程序打崩。
            _ => local,
        };
        world.push(w);
    }
    world.iter().map(invert).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- `matrix_quaternion` 的 `m12` / `m21`（R22）----

    /// **`m12` / `m21` 不能互换** —— 存储是**行主序** `f[r*4+c] == m[r][c]`。
    ///
    /// # 这条测试为什么必要
    ///
    /// `matrix_quaternion` 只被 **IK 误差**用到，而 IK 误差只在
    /// 「有 `ikrule` 且不是 `release`」的动画上出现 ⟹ 绝大多数夹具碰不到它。
    /// 一旦下标写错（R22：`m12` 取了 `f[9]`、`m21` 取了 `f[6]`，恰好互换），
    /// 四元数的 `x` 分量符号翻转，解出**另一个旋转**，
    /// 而所有「比顶点 / 比参考姿态 / 比逐帧欧拉角」的探针**全都测不出来**
    /// —— 实测只表现为 IK 误差的旋转三轴差 0.98/1.24/0.19 rad。
    ///
    /// # 判据
    ///
    /// 用一个**非对称**旋转（绕 X 30°）：`m[1][2] = −sin30 = −0.5`、
    /// `m[2][1] = +sin30 = +0.5` ⟹ 互换**可观测**。
    /// （对称矩阵上互换是恒等变换，那就是**空洞测试** —— 下面有硬门挡它。）
    #[test]
    fn matrix_quaternion_round_trips_angle_matrix() {
        let e = [30f32.to_radians(), 0.0, 0.0];
        let m = angle_matrix(e);
        // 非空洞硬门：`m[1][2]`（f[6]）与 `m[2][1]`（f[9]）必须真的不同。
        assert!(
            (m[6] - m[9]).abs() > 0.5,
            "夹具的 m[1][2]={} 与 m[2][1]={} 太接近 ⟹ 互换不可观测（空洞测试）",
            m[6],
            m[9]
        );

        let got = matrix_quaternion(&m);
        let want = angle_quaternion(e);
        // 四元数 `q` 与 `-q` 等价，所以比绝对值点积。
        let dot: f32 = (0..4).map(|k| got[k] * want[k]).sum();
        assert!(
            dot.abs() > 0.9999,
            "matrix_quaternion(angle_matrix([30°,0,0])) 应还原该旋转：\
             得到 {got:?}，应为 {want:?}（|dot| = {}）",
            dot.abs()
        );
        // 反证：`x` 分量必须非零**且与期望同号** —— 互换会让它翻号。
        assert!(got[0].abs() > 0.2, "x 分量 {} 太小", got[0]);
        assert!(
            got[0] * want[0] > 0.0,
            "x 分量符号反了（{} vs {}）⟹ `m12`/`m21` 被互换（R22 回来了）",
            got[0],
            want[0]
        );
    }

    /// 绕 Y、绕 Z、复合旋转也要往返（覆盖 `MatrixAngles` 的其余分支）。
    #[test]
    fn matrix_quaternion_round_trips_all_axes() {
        for e in [
            [0f32, 40f32.to_radians(), 0.0],
            [0f32, 0.0, 55f32.to_radians()],
            [20f32.to_radians(), 35f32.to_radians(), 15f32.to_radians()],
        ] {
            let m = angle_matrix(e);
            let got = matrix_quaternion(&m);
            let want = angle_quaternion(e);
            let dot: f32 = (0..4).map(|k| got[k] * want[k]).sum();
            assert!(
                dot.abs() > 0.9999,
                "euler {e:?}：得到 {got:?}，应为 {want:?}（|dot| = {}）",
                dot.abs()
            );
        }
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn identity_rotation_has_identity_matrix() {
        let m = angle_matrix([0.0, 0.0, 0.0]);
        for (i, v) in IDENTITY.iter().enumerate() {
            assert!(approx(m[i], *v), "第 {i} 项：{} vs {v}", m[i]);
        }
    }

    #[test]
    fn invert_round_trips() {
        let m = local_transform([1.0, 2.0, 3.0], [0.3, -0.5, 0.7]);
        let prod = concat(&m, &invert(&m));
        for (i, v) in IDENTITY.iter().enumerate() {
            assert!(approx(prod[i], *v), "第 {i} 项：{} vs {v}", prod[i]);
        }
    }

    #[test]
    fn invert_of_pure_translation_negates() {
        let m = local_transform([5.0, -2.0, 0.5], [0.0, 0.0, 0.0]);
        let inv = invert(&m);
        assert!(approx(inv[3], -5.0));
        assert!(approx(inv[7], 2.0));
        assert!(approx(inv[11], -0.5));
    }

    #[test]
    fn concat_translations_add() {
        let a = local_transform([1.0, 0.0, 0.0], [0.0; 3]);
        let b = local_transform([0.0, 2.0, 0.0], [0.0; 3]);
        let c = concat(&a, &b);
        assert!(approx(c[3], 1.0));
        assert!(approx(c[7], 2.0));
    }

    #[test]
    fn child_inherits_parent_rotation() {
        // 父绕 X 转 π/2；子沿局部 Z 平移 10。
        // Source 约定 R = Rz·Ry·Rx，绕 X 的旋转把 (0,0,10) 映射到 (0,-10,0)，
        // 所以骨骼 1 的世界位置是 (0,-10,0)。
        let parents = [-1, 0];
        let positions = [[0.0, 0.0, 0.0], [0.0, 0.0, 10.0]];
        let rotations = [[std::f32::consts::FRAC_PI_2, 0.0, 0.0], [0.0; 3]];
        let world = local_transform([0.0, -10.0, 0.0], [std::f32::consts::FRAC_PI_2, 0.0, 0.0]);

        let ptb = compute_pose_to_bone(&positions, &rotations, &parents);
        // poseToBone 必须是 world 的逆：ptb ∘ world == 单位阵。
        let prod = concat(&ptb[1], &world);
        for (i, v) in IDENTITY.iter().enumerate() {
            assert!(approx(prod[i], *v), "第 {i} 项：{} vs {v}", prod[i]);
        }
    }

    #[test]
    fn x_rotation_maps_y_to_minus_z_like_source() {
        // 钉住约定：绕 X 轴 +π/2 时，R 的第 1 行是 Y 轴的行。
        // Source: [cp·sy, sr·sp·sy+cr·cy, cr·sp·sy−sr·cy] = [0, 0, -1]
        let m = angle_matrix([std::f32::consts::FRAC_PI_2, 0.0, 0.0]);
        assert!(approx(m[4], 0.0), "m[4]={}", m[4]);
        assert!(approx(m[5], 0.0), "m[5]={}", m[5]);
        assert!(
            approx(m[6], -1.0),
            "m[6]={}（应为 -1；若为 +1 说明约定写反了）",
            m[6]
        );
        // 第 2 行应为 [0, 1, 0]
        assert!(approx(m[9], 1.0), "m[9]={}", m[9]);
    }

    #[test]
    fn parent_past_self_does_not_panic() {
        // 前向引用（非法输入）不应 panic。
        let parents = [1, -1];
        let positions = [[0.0; 3], [0.0; 3]];
        let rotations = [[0.0; 3], [0.0; 3]];
        let ptb = compute_pose_to_bone(&positions, &rotations, &parents);
        assert_eq!(ptb.len(), 2);
    }

    /// **核心判据**：官方模型的 `quat` 必须能被逐骨骼复现。
    ///
    /// `quat` 是引擎 `InitPose` 直接读的字段，写 0 是非法四元数。
    ///
    /// ⚠️ **标了 `#[ignore]`**（需要官方 `v_autoshotgun.mdl`）。手动跑：
    /// `cargo test --release -- --ignored`
    #[test]
    #[ignore = "需要真实素材 v_autoshotgun.mdl（MDLC_TEST_MDL）"]
    fn real_model_quat_matches_official() {
        let p = crate::test_assets::require(
            "MDLC_TEST_MDL",
            r"D:\GITHUB\plank\examples\v_autoshotgun.mdl",
            "官方 v_autoshotgun.mdl",
        );
        let b = std::fs::read(&p).unwrap_or_else(|e| panic!("读不到 {}：{e}", p.display()));
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let f = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bone_off = g(0xA0) as usize;
        let n = g(0x9C) as usize;
        assert_eq!(n, 89);

        let mut worst = 0.0f32;
        let mut worst_bone = 0;
        for i in 0..n {
            let o = bone_off + i * 216;
            let rot = [f(o + 0x3C), f(o + 0x40), f(o + 0x44)];
            let expect = [f(o + 0x2C), f(o + 0x30), f(o + 0x34), f(o + 0x38)];
            let got = angle_quaternion(rot);
            for k in 0..4 {
                // 四元数 q 与 -q 表示同一旋转；取两者中较小的差。
                let d = (got[k] - expect[k]).abs().min((got[k] + expect[k]).abs());
                if d > worst {
                    worst = d;
                    worst_bone = i;
                }
            }
        }
        assert!(
            worst < 1e-4,
            "quat 与官方模型不符：最大偏差 {worst}（骨骼 {worst_bone}）\n\
             这通常意味着 angle_quaternion 的欧拉顺序或半角写错了。"
        );
    }

    #[test]
    fn angle_quaternion_handles_known_cases() {
        // 单位旋转 → [0,0,0,1]
        let q = angle_quaternion([0.0, 0.0, 0.0]);
        assert!(approx(q[3], 1.0), "{q:?}");
        assert!(
            approx(q[0], 0.0) && approx(q[1], 0.0) && approx(q[2], 0.0),
            "{q:?}"
        );

        // 绕 X 轴 π/2 → [sin(π/4), 0, 0, cos(π/4)]（官方 bone[0] 的实测值）
        let q = angle_quaternion([std::f32::consts::FRAC_PI_2, 0.0, 0.0]);
        assert!(approx(q[0], std::f32::consts::FRAC_1_SQRT_2), "{q:?}");
        assert!(approx(q[3], std::f32::consts::FRAC_1_SQRT_2), "{q:?}");

        // 官方 bone[5]：rot = [-π/2, 0, π/2] → quat = [-0.5, -0.5, 0.5, 0.5]
        let h = std::f32::consts::FRAC_PI_2;
        let q = angle_quaternion([-h, 0.0, h]);
        for (k, v) in [-0.5f32, -0.5, 0.5, 0.5].iter().enumerate() {
            assert!(approx(q[k], *v), "第 {k} 项：{} vs {v}（{q:?}）", q[k]);
        }
    }

    #[test]
    fn quaternion_is_unit_length() {
        for a in [
            [0.3f32, -0.7, 1.1],
            [std::f32::consts::PI, 0.0, 0.0],
            [-1.2, 0.4, -2.9],
        ] {
            let q = angle_quaternion(a);
            let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "{a:?} → {q:?} 模长 {len}");
        }
    }

    /// **核心判据**：官方模型的 `poseToBone` 必须能被逐骨骼复现。
    ///
    /// 这条测试就是为上面那个「欧拉约定写反」的 bug 设的 ——
    /// 它会在 89 根骨骼上全部失败，而任何只看合成数据的测试都发现不了。
    ///
    /// ⚠️ **标了 `#[ignore]`**（需要官方 `v_autoshotgun.mdl`）。手动跑：
    /// `cargo test --release -- --ignored`
    #[test]
    #[ignore = "需要真实素材 v_autoshotgun.mdl（MDLC_TEST_MDL）"]
    fn real_model_pose_to_bone_matches_official() {
        let p = crate::test_assets::require(
            "MDLC_TEST_MDL",
            r"D:\GITHUB\plank\examples\v_autoshotgun.mdl",
            "官方 v_autoshotgun.mdl",
        );
        let b = std::fs::read(&p).unwrap_or_else(|e| panic!("读不到 {}：{e}", p.display()));
        let g = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let f = |o: usize| f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let bone_off = g(0xA0) as usize;
        let n = g(0x9C) as usize;
        assert_eq!(n, 89, "官方模型应有 89 根骨骼");

        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);
        let mut parents = Vec::with_capacity(n);
        let mut expect = Vec::with_capacity(n);
        for i in 0..n {
            let o = bone_off + i * 216;
            positions.push([f(o + 0x20), f(o + 0x24), f(o + 0x28)]);
            rotations.push([f(o + 0x3C), f(o + 0x40), f(o + 0x44)]);
            parents.push(g(o + 0x04));
            let mut m = [0f32; 12];
            for (k, slot) in m.iter_mut().enumerate() {
                *slot = f(o + 0x60 + k * 4);
            }
            expect.push(m);
        }

        let got = compute_pose_to_bone(&positions, &rotations, &parents);
        let mut worst = 0.0f32;
        let mut worst_bone = 0;
        for i in 0..n {
            for k in 0..12 {
                let d = (got[i][k] - expect[i][k]).abs();
                if d > worst {
                    worst = d;
                    worst_bone = i;
                }
            }
        }
        assert!(
            worst < 1e-3,
            "poseToBone 与官方模型不符：最大偏差 {worst}（骨骼 {worst_bone}）\n\
             这通常意味着 angle_matrix 的欧拉约定与 Source 的 AngleMatrix 不一致。"
        );
    }

    // ---- 骨骼轴重对齐（`RealignBones`）----

    /// 受控实验的骨骼布局：3 根骨骼沿 **Z** 轴排列。
    ///
    /// 子骨骼的局部位移是 `[0,0,10]` / `[0,0,20]` —— `d - x = 10 / 20 > 0.01`，
    /// 所以**全部**满足重对齐判据。
    ///
    /// 对应 `docs/_probe/smdl/ipr.smd`。
    fn z_chain() -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<i32>) {
        (
            vec![[0.0, 0.0, 0.0], [0.0, 0.0, 10.0], [0.0, 0.0, 20.0]],
            vec![[0.0; 3]; 3],
            vec![-1, 0, 1],
        )
    }

    /// 官方 `ipr1.mdl`（**不**触发重对齐）：骨骼原样保留。
    ///
    /// ```text
    /// [0] "root" pos=[0,0,0]    rot=[0,0,0]
    /// [1] "a"    pos=[0,0,10]   rot=[0,0,0]
    /// [2] "b"    pos=[0,0,20]   rot=[0,0,0]
    /// ```
    #[test]
    fn realign_is_noop_without_childbone() {
        let (pos, rot, parents) = z_chain();
        // 全 -1 = 没有任何骨骼需要对齐。
        let (got, _) = realign_bones(&pos, &rot, &parents, &[-1, -1, -1], &[false; 3]);
        for i in 0..3 {
            assert_eq!(got[i].0, pos[i], "bone[{i}].pos 不应改变");
            assert_eq!(got[i].1, rot[i], "bone[{i}].rot 不应改变");
        }
    }

    /// 官方 `ipr2.mdl`（`$ikchain "leg" "b"` → `childbone = [1, 2, -1]`）。
    ///
    /// 逐位对照官方产物（`docs/_probe/artifacts/ipr2.mdl`）：
    ///
    /// ```text
    /// [0] "root" pos=[0,0,0]  rot=[0, -π/2, -π/2]
    /// [1] "a"    pos=[10,0,0] rot=[0, 0, 0]
    /// [2] "b"    pos=[20,0,0] rot=[π/2, 0, π/2]
    /// ```
    ///
    /// 注意 `b` **没有** `childbone`，所以它自己的基**不重排**；
    /// 但它的局部姿态要相对**已重排的父骨骼**重算 —— 于是 `pos` 从
    /// `[0,0,20]` 变成 `[20,0,0]`、`rot` 从 `[0,0,0]` 变成 `[π/2,0,π/2]`。
    #[test]
    fn realign_matches_official_ipr2() {
        let (pos, rot, parents) = z_chain();
        let (got, _) = realign_bones(&pos, &rot, &parents, &[1, 2, -1], &[false; 3]);

        let half_pi = std::f32::consts::FRAC_PI_2;
        let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 1e-6);

        assert!(
            close(got[0].0, [0.0, 0.0, 0.0]),
            "root.pos = {:?}，应为 [0,0,0]",
            got[0].0
        );
        assert!(
            close(got[0].1, [0.0, -half_pi, -half_pi]),
            "root.rot = {:?}，应为 [0,-π/2,-π/2]",
            got[0].1
        );
        assert!(
            close(got[1].0, [10.0, 0.0, 0.0]),
            "a.pos = {:?}，应为 [10,0,0]",
            got[1].0
        );
        assert!(
            close(got[1].1, [0.0, 0.0, 0.0]),
            "a.rot = {:?}，应为 [0,0,0]",
            got[1].1
        );
        assert!(
            close(got[2].0, [20.0, 0.0, 0.0]),
            "b.pos = {:?}，应为 [20,0,0]",
            got[2].0
        );
        assert!(
            close(got[2].1, [half_pi, 0.0, half_pi]),
            "b.rot = {:?}，应为 [π/2,0,π/2]",
            got[2].1
        );
    }

    /// 重对齐**不移动骨骼的世界位置**（`simplify.cpp:4368` 把 `v3` 写回平移列）。
    ///
    /// 这是该算法的核心不变量 —— 局部基换了，但骨骼在世界里的位置不变。
    #[test]
    fn realign_preserves_world_positions() {
        let (pos, rot, parents) = z_chain();
        let before = compute_world(&pos, &rot, &parents);
        let (got, _) = realign_bones(&pos, &rot, &parents, &[1, 2, -1], &[false; 3]);
        let after_pos: Vec<[f32; 3]> = got.iter().map(|p| p.0).collect();
        let after_rot: Vec<[f32; 3]> = got.iter().map(|p| p.1).collect();
        let after = compute_world(&after_pos, &after_rot, &parents);

        for i in 0..3 {
            for c in 0..3 {
                let a = column(&before[i], 3)[c];
                let b = column(&after[i], 3)[c];
                assert!(
                    (a - b).abs() < 1e-5,
                    "bone[{i}] 的世界位置第 {c} 轴变了：{a} → {b}"
                );
            }
        }
    }

    /// 已在 +X 轴上的子骨骼**不触发**重对齐（判据 `d - x <= 0.01`）。
    ///
    /// 这解释了为什么 `ipkx.smd`（骨骼沿 X 排列）在 `$ikchain` 下
    /// 仍然逐位一致 —— 判据不成立，`RealignBones` 什么都不做。
    #[test]
    fn realign_skips_bones_already_on_positive_x() {
        let pos = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [20.0, 0.0, 0.0]];
        let rot = vec![[0.0; 3]; 3];
        let parents = vec![-1, 0, 1];
        let (got, _) = realign_bones(&pos, &rot, &parents, &[1, 2, -1], &[false; 3]);
        for i in 0..3 {
            assert_eq!(got[i].0, pos[i], "bone[{i}].pos 不应改变");
            assert_eq!(got[i].1, rot[i], "bone[{i}].rot 不应改变");
        }
    }

    /// `bPreAligned` 的骨骼被 `RealignBones` **整个跳过**
    /// （`simplify.cpp:4299` 的 `!g_bonetable[k].bPreAligned`）。
    ///
    /// 对照官方 `ipq2.mdl`（三根骨骼都写了 `$definebone`，**6** 个数字），
    /// 同一份 SMD 上还写了 `$realignbones`：
    ///
    /// ```text
    /// [0] "root" pos=[0,0,0]   rot=[0,0,0]
    /// [1] "a"    pos=[0,0,10]  rot=[0,0,0]
    /// [2] "b"    pos=[0,0,10]  rot=[0,0,0]
    /// ```
    ///
    /// 对照组 `ipq1`（**不写** `$definebone`，其余完全相同）会重排成
    /// `root.rot=[0,-π/2,-π/2]`、`a.pos=[10,0,0]`、`b.rot=[π/2,0,π/2]` ——
    /// 所以差异**确凿**来自 `$definebone` 而不是别的。
    ///
    /// `ipq3`（同样三根骨骼，但写满 **12** 个数字）产物与 `ipq2` 的骨骼表
    /// **逐位相同** —— 说明 L4D2 的 `$definebone` 与数字个数无关，
    /// **总是**置 `bPreAligned`（episode1 源码的 `TokenAvailable()` 闸门
    /// 与实际二进制不符）。
    #[test]
    fn realign_skips_pre_aligned_bones() {
        let (pos, rot, parents) = z_chain();
        // 全部 pre-aligned —— 即使 `childbone[]` 填满也一根都不动。
        let (got, _) = realign_bones(&pos, &rot, &parents, &[1, 2, -1], &[true; 3]);
        for i in 0..3 {
            assert_eq!(got[i].0, pos[i], "bone[{i}].pos 不应改变");
            assert_eq!(got[i].1, rot[i], "bone[{i}].rot 不应改变");
        }
    }

    /// `bPreAligned` 是**逐骨骼**的，不是「一旦有 `$definebone` 就全跳过」。
    ///
    /// 只标记中间那根 `a`：
    ///
    /// - `root`（未标记）→ 基重排成 X → +Z，`rot` 变成 `[0,-π/2,-π/2]`；
    /// - `a`（已标记）→ **局部**姿态原样保留 `pos=[0,0,10]`、`rot=[0,0,0]`；
    /// - `b`（未标记，但**没有** `childbone`）→ 局部姿态是
    ///   `invert(world[a]) ∘ world[b]`；而 `world[a]` 因为 `a` 被跳过
    ///   所以**没变**，于是结果就是 SMD 原值 `[0,0,20]`。
    ///
    /// 最后这条正是官方 `simplify.cpp:4419` 的行为：重建 `b` 的局部姿态时
    /// 用的是 `g_bonetable[a].boneToPose` —— 而它在 4390-4401 的循环里
    /// 因为 `a` 是 pre-aligned 而**没有被更新**。
    #[test]
    fn realign_skips_only_marked_bones() {
        let (pos, rot, parents) = z_chain();
        let (got, _) = realign_bones(&pos, &rot, &parents, &[1, 2, -1], &[false, true, false]);
        let half_pi = std::f32::consts::FRAC_PI_2;

        assert!(
            (got[0].1[1] + half_pi).abs() < 1e-6 && (got[0].1[2] + half_pi).abs() < 1e-6,
            "root 未标记，应被重排成 [0,-π/2,-π/2]，实际 {:?}",
            got[0].1
        );
        assert_eq!(
            got[1].0,
            [0.0, 0.0, 10.0],
            "a 已标记 pre-aligned，pos 应原样保留"
        );
        assert_eq!(
            got[1].1,
            [0.0, 0.0, 0.0],
            "a 已标记 pre-aligned，rot 应原样保留"
        );
        assert_eq!(
            got[2].0,
            [0.0, 0.0, 20.0],
            "b 的父 a 没被重排，所以 b 的局部姿态也不应改变"
        );
        assert_eq!(got[2].1, [0.0, 0.0, 0.0], "b.rot 不应改变");
    }
}
