//! 切线空间（`SourceVector4D`）计算 —— 逐字对应 studiomdl 的
//!
//! ```text
//! 对 axis ∈ {x, y, z}：
//!   e01 = (p1[axis]-p0[axis], t1.s-t0.s, t1.t-t0.t)
//!   e02 = (p2[axis]-p0[axis], t2.s-t0.s, t2.t-t0.t)
//!   c   = e01 × e02
//!   if |c.x| > 1e-12:            ← 退化 UV 的**唯一**防线
//!       sVect[axis] += -c.y / c.x
//!       tVect[axis] += -c.z / c.x
//! VectorNormalize(sVect); VectorNormalize(tVect)
//! ```
//! ```text
//! sVect = Σ (入射三角形的 sVect)      ← 顺序 = 三角形在 mesh 里的先后
//! tVect = Σ (入射三角形的 tVect)
//! c     = sVect × tVect
//! leftHanded = (c · normal) < 0
//! if !leftHanded:  t = normal × sVect ; s = t × normal ; w = +1
//! else:            t = sVect × normal ; s = normal × t ; w = -1
//! 归一化 s、t（长度为 0 时**保持零向量**，不置默认值）
//! ```
//! ```text
//! |dot(normalize(官方切线), VVD 里存的法线)|
//!   H1 好的模型（3013 个）：平均 0.000000，最大 0.000012
//!   H1 差的模型（  54 个）：平均 0.468690，最大 1.000000
//! ```

use crate::model::Vertex;
use crate::vvd::VvdTangent;

/// studiomdl 里 `SMALL_FLOAT` 的值（`simplify.cpp` 4914 行）。
///
/// 它是退化 UV 的判据：`|cross.x| <= SMALL_FLOAT` 时该轴不贡献。
pub const SMALL_FLOAT: f32 = 1e-12;

#[inline]
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Source 的 `VectorNormalize`：长度为 0 时**保持原值**（不置成默认轴）。
///
/// 实测依据：`brokenglass_piece.vvd` 有 48 个顶点的官方切线是 `[0,0,0]`，
/// 且 w = +1。若零向量被替换成别的轴，那 48 个不会是零。
/// 返回归一化前的长度（与 Source 的返回值语义一致）。
#[inline]
fn normalize(v: &mut [f32; 3]) -> f32 {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len != 0.0 {
        v[0] /= len;
        v[1] /= len;
        v[2] /= len;
    }
    len
}

/// 一个三角形对切线空间的贡献（`CalcTriangleTangentSpace`）。
///
/// 返回**已各自归一化**的 `(sVect, tVect)`。退化三角形（UV 面积 ≈ 0）返回
/// 零向量对 —— 与 studiomdl 一致，**不做**回退。
pub fn triangle_tangent_space(
    p0: [f32; 3],
    p1: [f32; 3],
    p2: [f32; 3],
    t0: [f32; 2],
    t1: [f32; 2],
    t2: [f32; 2],
) -> ([f32; 3], [f32; 3]) {
    let mut s_vect = [0.0f32; 3];
    let mut t_vect = [0.0f32; 3];
    let p = [p0, p1, p2];
    let t = [t0, t1, t2];

    for axis in 0..3 {
        // e01 = (p1[a]-p0[a], ds, dt)；e02 = (p2[a]-p0[a], ds, dt)
        let e01 = [
            p[1][axis] - p[0][axis],
            t[1][0] - t[0][0],
            t[1][1] - t[0][1],
        ];
        let e02 = [
            p[2][axis] - p[0][axis],
            t[2][0] - t[0][0],
            t[2][1] - t[0][1],
        ];
        let c = cross(e01, e02);
        // 退化 UV（面积接近 0）→ 该轴不贡献。这是 studiomdl 唯一的防线。
        if c[0].abs() > SMALL_FLOAT {
            s_vect[axis] += -c[1] / c[0];
            t_vect[axis] += -c[2] / c[0];
        }
    }
    normalize(&mut s_vect);
    normalize(&mut t_vect);
    (s_vect, t_vect)
}

/// 把一个顶点的累加量正交化成最终切线（`CalcModelTangentSpaces` 的后半段）。
///
/// `acc_s` / `acc_t` 是入射三角形贡献之和。返回 `(xyz, w)`。
pub fn orthonormalize(acc_s: [f32; 3], acc_t: [f32; 3], normal: [f32; 3]) -> VvdTangent {
    let tmp = cross(acc_s, acc_t);
    // 注意是 `< 0.0`，不是 `<=`：实测 w 只在严格为负时取 -1。
    let left_handed = dot(tmp, normal) < 0.0;
    let (mut s, mut t, w) = if !left_handed {
        let t = cross(normal, acc_s);
        let s = cross(t, normal);
        (s, t, 1.0f32)
    } else {
        let t = cross(acc_s, normal);
        let s = cross(normal, t);
        (s, t, -1.0f32)
    };
    normalize(&mut s);
    normalize(&mut t);
    VvdTangent { xyz: s, w }
}

/// 为一个 mesh 计算全部顶点的切线。
///
/// # 前提：顶点必须**已经去重**
///
/// `triangles` 的下标指向 `vertices`。同一个空间位置若因 UV/法线/骨骼不同而
/// 拆成多条记录，它们在 VVD 里本来就是不同顶点，切线也各算各的 ——
/// 这正是 studiomdl 的语义（它按 mesh 的顶点池做 `vertToTriMap`）。
///
/// `vertices` 的每一项都参与输出（哪怕没有任何入射三角形，输出零切线）。
/// 这与 studiomdl 一致：它遍历 `0..pMesh->numvertices`，孤立顶点得到零。
pub fn tangents_for_mesh(vertices: &[Vertex], triangles: &[[u32; 3]]) -> Vec<VvdTangent> {
    // 累加器。按顶点分别累加入射三角形的贡献。
    let mut acc_s = vec![[0.0f32; 3]; vertices.len()];
    let mut acc_t = vec![[0.0f32; 3]; vertices.len()];

    // **按三角形顺序**遍历（studiomdl 的 vertToTriMap 是按 face 顺序 push 的），
    // 这样浮点累加顺序与官方一致，能最大化逐位吻合率。
    for tri in triangles {
        let [i0, i1, i2] = *tri;
        let (Some(v0), Some(v1), Some(v2)) = (
            vertices.get(i0 as usize),
            vertices.get(i1 as usize),
            vertices.get(i2 as usize),
        ) else {
            // 下标越界属上游 bug；跳过而不是 panic，让调用方的自检去报。
            continue;
        };
        let (s, t) = triangle_tangent_space(v0.pos, v1.pos, v2.pos, v0.uv, v1.uv, v2.uv);
        for i in [i0, i1, i2] {
            let k = i as usize;
            acc_s[k][0] += s[0];
            acc_s[k][1] += s[1];
            acc_s[k][2] += s[2];
            acc_t[k][0] += t[0];
            acc_t[k][1] += t[1];
            acc_t[k][2] += t[2];
        }
    }

    vertices
        .iter()
        .enumerate()
        .map(|(i, v)| orthonormalize(acc_s[i], acc_t[i], v.normal))
        .collect()
}

/// 由法线导出一个正交的切线（**不是** studiomdl 的行为）。
///
/// 用于把退化顶点的零切线替换成可用值。选择规则：取与法线最不平行的
/// 坐标轴做叉积，避免叉积退化成零向量。
///
/// **这会偏离官方产物**，所以 [`tangents_for_mesh`] 默认不用它。
pub fn fallback_tangent_from_normal(normal: [f32; 3]) -> VvdTangent {
    let mut n = normal;
    if normalize(&mut n) == 0.0 {
        // 法线本身是零 —— 无信息可用，给一个约定值。
        return VvdTangent {
            xyz: [1.0, 0.0, 0.0],
            w: 1.0,
        };
    }
    // 取与法线夹角最大的坐标轴，保证叉积不为零。
    let axis = if n[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let mut t = cross(axis, n);
    normalize(&mut t);
    VvdTangent { xyz: t, w: 1.0 }
}

/// 同 [`tangents_for_mesh`]，但把**零切线**替换成 [`fallback_tangent_from_normal`]。
///
/// 适用于「宁可切线不准也不能是零」的场合（例如某些着色器把零切线当成
/// 非法输入）。**与官方产物不一致**，不要用它做 parity 对照。
pub fn tangents_for_mesh_with_fallback(
    vertices: &[Vertex],
    triangles: &[[u32; 3]],
) -> Vec<VvdTangent> {
    let mut out = tangents_for_mesh(vertices, triangles);
    for (t, v) in out.iter_mut().zip(vertices) {
        if t.xyz == [0.0, 0.0, 0.0] {
            *t = fallback_tangent_from_normal(v.normal);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Vertex {
        Vertex {
            pos,
            normal,
            uv,
            bones: vec![[0.0, 1.0]],
        }
    }

    /// 单位正方形（XY 平面，法线 +Z），UV 沿 +X/+Y 展开。
    /// 期望切线 = +X，w = +1 —— 手算可验证。
    #[test]
    fn axis_aligned_quad_gives_plus_x_tangent() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
            v([1.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0]),
            v([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
        ];
        let tris = vec![[0u32, 1, 2], [0, 2, 3]];
        let t = tangents_for_mesh(&verts, &tris);
        assert_eq!(t.len(), 4);
        for (i, tan) in t.iter().enumerate() {
            assert!(
                (tan.xyz[0] - 1.0).abs() < 1e-6
                    && tan.xyz[1].abs() < 1e-6
                    && tan.xyz[2].abs() < 1e-6,
                "顶点 {i} 切线应为 +X，实际 {:?}",
                tan.xyz
            );
            assert_eq!(tan.w, 1.0, "顶点 {i} 手性应为 +1");
        }
    }

    /// UV 的 V 轴翻转 → 手性变 -1（切线仍是 +X）。
    #[test]
    fn flipped_v_flips_handedness() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 1.0]),
            v([1.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
        ];
        let t = tangents_for_mesh(&verts, &[[0u32, 1, 2]]);
        assert_eq!(t[0].w, -1.0, "V 翻转后手性应为 -1：{:?}", t[0]);
    }

    /// **共享顶点必须累加**：两个相邻三角形共边，共边的两个顶点应拿到
    /// 两个三角形贡献之和，而不是只看其中一个。
    #[test]
    fn shared_vertices_accumulate_across_triangles() {
        // 三角形 A 在 XY 平面（切线 +X），三角形 B 在 XZ 平面（切线 +X）。
        // 共享顶点 0、1。累加后共享顶点的切线仍应是 +X 方向（两个贡献同向），
        // 但**孤立顶点**（只属于一个三角形）不应受影响。
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]), // 共享
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]), // 共享
            v([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0]), // 只属于 A
            v([1.0, 0.0, 1.0], [0.0, 0.0, 1.0], [1.0, 1.0]), // 只属于 B
        ];
        let tris = vec![[0u32, 1, 2], [0, 3, 1]];
        let t = tangents_for_mesh(&verts, &tris);
        for (i, tan) in t.iter().enumerate() {
            assert!(
                tan.xyz[0].abs() > 0.99,
                "顶点 {i} 切线应≈±X，实际 {:?}",
                tan.xyz
            );
        }
        // 共享顶点 0 的累加量应是两个三角形之和（不能只算一个）。
        let mut a_s = [0.0f32; 3];
        let mut a_t = [0.0f32; 3];
        for tri in &tris {
            let (s, tt) = triangle_tangent_space(
                verts[tri[0] as usize].pos,
                verts[tri[1] as usize].pos,
                verts[tri[2] as usize].pos,
                verts[tri[0] as usize].uv,
                verts[tri[1] as usize].uv,
                verts[tri[2] as usize].uv,
            );
            if tri.contains(&0) {
                for k in 0..3 {
                    a_s[k] += s[k];
                    a_t[k] += tt[k];
                }
            }
        }
        // 单三角形时累加量应当只有一半左右的量级（两个同向贡献 → 加倍）。
        let single = triangle_tangent_space(
            verts[0].pos,
            verts[1].pos,
            verts[2].pos,
            verts[0].uv,
            verts[1].uv,
            verts[2].uv,
        );
        assert!(
            a_s[0].abs() > single.0[0].abs() * 1.5,
            "共享顶点的累加量应大于单三角形：{:?} vs {:?}",
            a_s,
            single.0
        );
    }

    /// 退化 UV（所有三角形 UV 相同）→ 零切线，**不是**回退值。
    /// 实测依据：官方 `brokenglass_piece.vvd` 有 48/72 个顶点是 `[0,0,0]`。
    #[test]
    fn degenerate_uv_yields_zero_tangent_like_studiomdl() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]),
            v([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]),
        ];
        let t = tangents_for_mesh(&verts, &[[0u32, 1, 2]]);
        for tan in &t {
            assert_eq!(tan.xyz, [0.0, 0.0, 0.0], "退化 UV 应给零切线：{tan:?}");
            // 零累加量的 cross 是零，dot 为 0，不满足 `< 0` → w = +1。
            assert_eq!(tan.w, 1.0, "退化顶点的手性应为 +1：{tan:?}");
        }
    }

    /// 回退版本把零切线换成由法线导出的正交基。
    #[test]
    fn fallback_replaces_zero_tangent() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]),
            v([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]),
        ];
        let t = tangents_for_mesh_with_fallback(&verts, &[[0u32, 1, 2]]);
        for tan in &t {
            let len = (tan.xyz[0].powi(2) + tan.xyz[1].powi(2) + tan.xyz[2].powi(2)).sqrt();
            assert!((len - 1.0).abs() < 1e-6, "回退切线应是单位向量：{tan:?}");
            // 必须与法线正交。
            let d = tan.xyz[2]; // 法线是 +Z
            assert!(d.abs() < 1e-6, "回退切线应与法线正交：{tan:?}");
        }
    }

    /// 孤立顶点（没有任何入射三角形）得到零切线，且不 panic。
    #[test]
    fn orphan_vertex_gets_zero_tangent() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
            v([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
            v([9.0, 9.0, 9.0], [0.0, 0.0, 1.0], [0.0, 0.0]), // 孤立
        ];
        let t = tangents_for_mesh(&verts, &[[0u32, 1, 2]]);
        assert_eq!(t.len(), 4);
        assert_eq!(t[3].xyz, [0.0, 0.0, 0.0], "孤立顶点应为零切线");
    }

    /// 越界下标不应 panic（上游 bug 交给自检报）。
    #[test]
    fn out_of_range_index_is_skipped() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
        ];
        let t = tangents_for_mesh(&verts, &[[0u32, 1, 99]]);
        assert_eq!(t.len(), 2);
    }

    /// 三角形的 s/t 是**各自归一化**的 —— 与「先累加再归一化」不等价。
    /// 用两个面积差 100 倍的三角形证明归一化发生在累加之前。
    #[test]
    fn per_triangle_normalization_happens_before_accumulation() {
        let verts = vec![
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            v([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
            v([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
            // 面积大 100 倍的三角形，UV 也放大 100 倍 → s/t 方向相同。
            v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0]),
            v([100.0, 0.0, 0.0], [0.0, 0.0, 1.0], [100.0, 0.0]),
            v([0.0, 100.0, 0.0], [0.0, 0.0, 1.0], [0.0, 100.0]),
        ];
        // 顶点 0 被两个三角形共享；若「先累加再归一化」，大三角形会主导。
        // 因为两个贡献方向完全相同，最终切线都是 +X —— 这个用例主要保证
        // 归一化不会把方向搞坏，并钉住「归一化在累加之前」不会 panic。
        let t = tangents_for_mesh(&verts, &[[0u32, 1, 2], [3, 4, 5]]);
        assert!((t[0].xyz[0] - 1.0).abs() < 1e-5, "{:?}", t[0]);
        // 直接对比：未归一化的累加会给出完全不同的量级。
        let (s_small, _) = triangle_tangent_space(
            verts[0].pos,
            verts[1].pos,
            verts[2].pos,
            verts[0].uv,
            verts[1].uv,
            verts[2].uv,
        );
        assert!(
            (s_small[0] - 1.0).abs() < 1e-6,
            "单三角形的 sVect 应已归一化为单位向量：{s_small:?}"
        );
    }

    /// 手性与法线方向绑定：法线翻转 → w 翻转。
    #[test]
    fn normal_flip_flips_handedness() {
        let make = |nz: f32| {
            vec![
                v([0.0, 0.0, 0.0], [0.0, 0.0, nz], [0.0, 0.0]),
                v([1.0, 0.0, 0.0], [0.0, 0.0, nz], [1.0, 0.0]),
                v([1.0, 1.0, 0.0], [0.0, 0.0, nz], [1.0, 1.0]),
            ]
        };
        let up = tangents_for_mesh(&make(1.0), &[[0u32, 1, 2]]);
        let down = tangents_for_mesh(&make(-1.0), &[[0u32, 1, 2]]);
        assert_eq!(up[0].w, 1.0);
        assert_eq!(down[0].w, -1.0);
    }
}
