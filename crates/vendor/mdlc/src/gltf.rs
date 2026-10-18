//! glTF / GLB 源读取（`gltf` crate → 中立 SMD）。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use gltf::animation::Interpolation;
use gltf::buffer::Source;
use gltf::mesh::Mode;

use crate::fbx::{
    FALLBACK_MATERIAL, FbxError, FbxGeometry, FbxOpts, FbxShapeKey, ForcedAxis,
    automatic_axis_matrix, convert_geometry_axes, converted_smd, ferr,
};
use crate::smd::{Smd, SmdBoneLink, SmdFrame, SmdNode, SmdPose, SmdTriangle, SmdVertex};

/// glTF 动画的**默认重采样率**。
///
/// 与 FBX 同值（[`crate::fbx::DEFAULT_FPS`]）—— 官方对 FBX 固定归一化到
/// 30 fps（`docs/fbx-support.md` §1.5），glTF 没有官方对照物，沿用同一值
/// 可以让两套输入在同一份 QC 下产出一致的帧数。
pub const DEFAULT_FPS: f32 = crate::fbx::DEFAULT_FPS;

/// 一个 glTF 源读出来的几何 + 元数据。
///
/// 与 [`crate::fbx::FbxGeometry`] 是**同一个形状** —— 上层 `compile.rs`
/// 直接把它拆成 `SourceGeometry` 的五个字段，两条路径共用一套下游。
pub type GltfGeometry = FbxGeometry;

/// 一个 glTF 源的读取选项。
///
/// **就是** [`FbxOpts`]：四条 `src*` 选项（`srcpart` / `srcmaterial` /
/// `srcscale` / `srcaxis`）按概念命名，与格式无关
/// （`docs/gltf-support.md` §6.2「新增语法 0 条」）。
pub type GltfOpts = FbxOpts;

/// 读取时的一个错误（与 [`FbxError`] 同构，共用同一个类型）。
pub type GltfError = FbxError;

// ---------------------------------------------------------------------------
// 诊断（不是错误）
// ---------------------------------------------------------------------------

/// 读一个 glTF 文件时收集到的**提示**（全部是「官方也会静默通过」的情形）。
///
/// 与 `compile.rs` 的 `fbx_diagnostics` 一样是**纯函数**（返回文案而不是直接
/// 打印），这样这条路径才测得了 —— `log` 的 logger 是进程级全局的，测试里
/// 装不了第二个。
///
/// ⚠️ **报错的那几条不走这里**（data URI 解码失败 / Draco / meshopt 都在
/// 读文件那一步直接返回 `Err`）—— 静默零几何是绝对不能接受的。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GltfNotes {
    /// 有 accessor 没有 `bufferView`（规范语义 = 全零）。crate 的校验层会
    /// 误判为错误，我们放行并按全零处理。
    pub zero_accessors: usize,
    /// 驱动 `MorphTargetWeights` 的通道数（mdlc 不做逐帧表情权重）。
    ///
    /// ⚠️ 只有 [`read_frames`] 会填这一项（读几何时表情权重动画无所谓）。
    pub morph_weight_channels: usize,
}

impl GltfNotes {
    /// 是否有任何值得报告的提示。
    pub fn is_empty(&self) -> bool {
        self.zero_accessors == 0 && self.morph_weight_channels == 0
    }

    /// 逐条文案（`at` 是调用方给的资产引用串，用于定位）。
    pub fn lines(&self, at: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.zero_accessors > 0 {
            out.push(crate::tr_fmt!(
                "Note: %{at} has %{n} accessors without a `bufferView`; per the glTF spec they mean **all zero**, and they were treated that way (the validation layer of `gltf` 1.4.1 misjudges them as an error; mdlc takes the read path that bypasses validation).",
                at = at,
                n = self.zero_accessors
            ));
        }
        if self.morph_weight_channels > 0 {
            out.push(crate::tr_fmt!(
                "Note: %{at} has %{n} animation channels driving `MorphTargetWeights` (per-frame morph weights); mdlc does **not** do per-frame morph weights — morphs are controlled only by the QC `flex` statements, and these channels are ignored.",
                at = at,
                n = self.morph_weight_channels
            ));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// 加载与校验
// ---------------------------------------------------------------------------

/// 把 `data:` URI 解成字节。
///
/// ⚠️ **关掉 `import` feature 之后 `gltf` crate 不再自带这一步**
/// （`gltf-1.4.1\src\import.rs` 里的 `Scheme::Data(_, base64) => base64::decode(..)`
/// 整段被 feature 门禁），而 `Source::Uri` 给的是**原样**的 uri 字符串
/// （`buffer.rs:86-88`）。不自己解的话内嵌 `.gltf` 会**静默读出零顶点**
/// （`docs/gltf-support.md` §3.4 实测）。
///
/// 支持两种形态：`data:<mime>;base64,<载荷>` 与 `data:;base64,<载荷>`
/// （mime 可省）。**非 base64 的百分号编码形态不支持** —— 那是给文本用的，
/// 二进制 buffer 不会那么写；真遇到就报错，不猜。
fn decode_data_uri(uri: &str, at: &str) -> Result<Vec<u8>, GltfError> {
    let rest = uri
        .strip_prefix("data:")
        .ok_or_else(|| ferr(format!("{at}：内部错误 —— {uri:?} 不是 data URI")))?;
    let (meta, payload) = rest.split_once(',').ok_or_else(|| {
        ferr(format!(
            "{at}：data URI 里没有逗号，无法定位载荷（前 64 字节：{:?}）",
            uri.chars().take(64).collect::<String>()
        ))
    })?;
    if !meta.ends_with(";base64") {
        return Err(ferr(format!(
            "{at}：data URI 不是 base64 形态（元信息 {meta:?}）。\
             glTF 的二进制 buffer 应当用 `data:application/octet-stream;base64,...`。"
        )));
    }
    // ⚠️ `base64` 0.13 的 API 是**自由函数** `base64::decode`；
    // `Engine` trait + `engine::general_purpose::STANDARD` 是 0.21 起才有的。
    // 这里钉 `0.13` 是为了与 `gltf` 的 `import` 分支同版本（不新增包）。
    base64::decode(payload.trim()).map_err(|err| {
        ferr(format!(
            "{at}：data URI 的 base64 解码失败（{err}）。\
             这条**不能静默跳过** —— 跳过会让模型读出零顶点却不报错。"
        ))
    })
}

fn decode_uri_path(uri: &str) -> Result<String, GltfError> {
    let mut decoded = Vec::with_capacity(uri.len());
    let mut bytes = uri.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|byte| char::from(byte).to_digit(16));
            let low = bytes.next().and_then(|byte| char::from(byte).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(ferr("glTF URI contains an invalid percent escape"));
            };
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).map_err(|_| ferr("glTF URI is not valid UTF-8"))
}

/// 把文件里声明的每个 buffer 都读成字节。
///
/// 三种来源（`gltf::buffer::Source`）：
///
/// * [`Source::Bin`] —— GLB 的 BIN chunk（`Gltf::blob`）；
/// * [`Source::Uri`] 且以 `data:` 开头 —— 内嵌，走 [`decode_data_uri`]；
/// * 其它 `Uri` —— 相对当前文件所在目录的**外部文件**（`.gltf` + `.bin` 分离式）。
fn load_buffers(
    doc: &gltf::Document,
    blob: Option<Vec<u8>>,
    base_dir: &Path,
    at: &str,
) -> Result<Vec<Vec<u8>>, GltfError> {
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(doc.buffers().len());
    for b in doc.buffers() {
        let bytes = match b.source() {
            Source::Bin => blob.clone().ok_or_else(|| {
                ferr(format!(
                    "{at}：buffer[{}] 声明自己是 GLB 的 BIN chunk，但文件里没有 BIN chunk。",
                    b.index()
                ))
            })?,
            Source::Uri(uri) if uri.starts_with("data:") => decode_data_uri(uri, at)?,
            Source::Uri(uri) => {
                // 外部文件：相对当前 glTF 文件所在目录解析。
                let path = base_dir.join(decode_uri_path(uri)?);
                std::fs::read(&path).map_err(|err| {
                    ferr(format!(
                        "{at}：读不到外部 buffer {uri:?}（解析为 {}）：{err}",
                        path.display()
                    ))
                })?
            }
        };
        // ⚠️ 自己补的第一项校验（`from_slice_without_validation` 把 crate 的
        // 校验全绕过了）：声明的 `byteLength` 不能超过实际拿到的字节数。
        // 少了这一条，越界的 bufferView 会在 reader 层静默变成 `None`，
        // 表现为「顶点少了一截」而不是报错。
        if bytes.len() < b.length() {
            return Err(ferr(format!(
                "{at}：buffer[{}] 声明 {} 字节，实际只有 {} 字节。",
                b.index(),
                b.length(),
                bytes.len()
            )));
        }
        out.push(bytes);
    }
    Ok(out)
}

/// 自己补的第二、三项校验：每个 bufferView 与每个 accessor 都要落在 buffer 内。
///
/// `from_slice_without_validation` 把 crate 的校验整段跳过了（那是为了绕开
/// 「无 `bufferView` 的 accessor」误判，见 `docs/gltf-support.md` §3.2），
/// 所以长度检查得自己做。三项（§8.1）：
///
/// 1. buffer 声明长度 ≤ 实际字节数 —— 在 [`load_buffers`] 里做了；
/// 2. 每个 bufferView 的 `[byteOffset, byteOffset+byteLength)` 落在对应 buffer 内；
/// 3. accessor 的 `stride × count` 落在其 bufferView 内
///    （⚠️ **没有 `bufferView` 时跳过** —— 规范语义是全零，那正是坑一）。
fn validate_ranges(
    doc: &gltf::Document,
    buffers: &[Vec<u8>],
    at: &str,
) -> Result<usize, GltfError> {
    for v in doc.views() {
        let bi = v.buffer().index();
        let len = buffers.get(bi).map_or(0, Vec::len);
        let end = v.offset().saturating_add(v.length());
        if end > len {
            return Err(ferr(format!(
                "{at}：bufferView[{}] 覆盖 [{}, {})，超出 buffer[{bi}] 的 {len} 字节。",
                v.index(),
                v.offset(),
                end
            )));
        }
    }

    // 第 3 项：accessor 的字节跨度。`size()` = 单元素字节数（分量大小 × 分量数），
    // `stride` 缺省时等于 `size()`。**没有 bufferView 的 accessor 直接跳过**
    // —— 它的语义是「全零」，没有可校验的范围。
    let mut zero_accessors = 0usize;
    let mut decoded_bytes = 0usize;
    for a in doc.accessors() {
        decoded_bytes = a
            .count()
            .checked_mul(a.size())
            .and_then(|size| decoded_bytes.checked_add(size))
            .filter(|size| *size <= 512 * 1024 * 1024)
            .ok_or_else(|| {
                ferr(format!(
                    "{at}: glTF accessors exceed the decoded size limit"
                ))
            })?;
        if let Some(sparse) = a.sparse() {
            let indices = sparse.indices();
            let index_size = match indices.index_type() {
                gltf::accessor::sparse::IndexType::U8 => 1,
                gltf::accessor::sparse::IndexType::U16 => 2,
                gltf::accessor::sparse::IndexType::U32 => 4,
            };
            let values = sparse.values();
            let valid_range = |offset: usize, count: usize, size: usize, length: usize| {
                count
                    .checked_mul(size)
                    .and_then(|bytes| offset.checked_add(bytes))
                    .is_some_and(|end| end <= length)
            };
            if sparse.count() > a.count()
                || !valid_range(
                    indices.offset(),
                    sparse.count(),
                    index_size,
                    indices.view().length(),
                )
                || !valid_range(
                    values.offset(),
                    sparse.count(),
                    a.size(),
                    values.view().length(),
                )
            {
                return Err(ferr(format!(
                    "{at}: glTF sparse accessor {} exceeds its buffer view",
                    a.index()
                )));
            }
            let view = indices.view();
            let begin = view.offset() + indices.offset();
            let bytes = &buffers[view.buffer().index()][begin..begin + sparse.count() * index_size];
            let mut previous = None;
            for bytes in bytes.chunks_exact(index_size) {
                let index = match index_size {
                    1 => usize::from(bytes[0]),
                    2 => usize::from(u16::from_le_bytes(bytes.try_into().unwrap())),
                    _ => u32::from_le_bytes(bytes.try_into().unwrap()) as usize,
                };
                if index >= a.count() || previous.is_some_and(|previous| index <= previous) {
                    return Err(ferr(format!(
                        "{at}: glTF sparse accessor {} has invalid indices",
                        a.index()
                    )));
                }
                previous = Some(index);
            }
        }
        let Some(v) = a.view() else {
            zero_accessors += usize::from(a.sparse().is_none());
            continue;
        };
        let stride = v.stride().unwrap_or_else(|| a.size());
        if stride < a.size() || stride % a.data_type().size() != 0 {
            return Err(ferr(format!(
                "{at}: glTF accessor {} has an invalid byte stride",
                a.index()
            )));
        }
        if a.count() == 0 {
            continue;
        }
        let end = a
            .offset()
            .saturating_add(stride.saturating_mul(a.count() - 1))
            .saturating_add(a.size());
        if end > v.length() {
            return Err(ferr(format!(
                "{at}：accessor[{}] 的 {} 个元素（步长 {stride}、单元素 {} 字节）\
                 需要 {end} 字节，超出 bufferView[{}] 的 {} 字节。",
                a.index(),
                a.count(),
                a.size(),
                v.index(),
                v.length()
            )));
        }
    }
    Ok(zero_accessors)
}

/// 用了这些扩展就**直接报错** —— crate 不支持它们，静默读出来的几何是错的。
///
/// `gltf` 1.4.1 的 `gltf-json` 里 `draco|meshopt` **零命中**
/// （`docs/gltf-support.md` §3.3），所以压缩过的网格读出来只有未压缩的
/// 那部分属性，顶点数是错的。上游 master 也仍未支持。
const UNSUPPORTED_EXTENSIONS: [&str; 2] = ["KHR_draco_mesh_compression", "EXT_meshopt_compression"];

/// 检查 `extensionsRequired` / `extensionsUsed` 里有没有我们不支持的。
///
/// `extensionsUsed` 也查 —— 压缩扩展**必须**同时出现在 `extensionsRequired`
/// 里才是强制的，但只写在 `extensionsUsed` 里也说明作者动过压缩，
/// 值得拦下来问一句（报错文案里会说明是哪一种）。
fn check_extensions(doc: &gltf::Document, at: &str) -> Result<(), GltfError> {
    let required: HashSet<&str> = doc.extensions_required().collect();
    let used: HashSet<&str> = doc.extensions_used().collect();
    for ext in UNSUPPORTED_EXTENSIONS {
        if required.contains(ext) {
            return Err(ferr(format!(
                "{at}：用了 {ext}（列在 `extensionsRequired` 里），mdlc 不支持解压它。\
                 请在导出时关掉网格压缩（Blender 的 glTF 导出器里叫 `Compression`）。"
            )));
        }
        if used.contains(ext) {
            return Err(ferr(format!(
                "{at}：`extensionsUsed` 里有 {ext}；mdlc 不支持它，读出来的几何可能不完整。\
                 请关掉网格压缩后重导。"
            )));
        }
    }
    Ok(())
}

/// 解析文件 + 装 buffer + 补校验，返回 `(文档, buffer 字节, 提示)`。
fn load_document(
    path: &Path,
    at: &str,
) -> Result<(gltf::Document, Vec<Vec<u8>>, GltfNotes), GltfError> {
    let bytes = std::fs::read(path)
        .map_err(|err| ferr(format!("{at}：读不到 {}：{err}", path.display())))?;

    let (doc, blob) = parse_document(&bytes)
        .map_err(|err| ferr(format!("{at}：{} 解析失败：{err}", path.display())))?;

    validate_structure(&doc, at)?;
    check_extensions(&doc, at)?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let buffers = load_buffers(&doc, blob, base_dir, at)?;
    let zero_accessors = validate_ranges(&doc, &buffers, at)?;

    let notes = GltfNotes {
        zero_accessors,
        ..Default::default()
    };
    Ok((doc, buffers, notes))
}

fn parse_document(bytes: &[u8]) -> Result<(gltf::Document, Option<Vec<u8>>), String> {
    let original = match gltf::Gltf::from_slice_without_validation(bytes) {
        Ok(parsed) => return Ok((parsed.document, parsed.blob)),
        Err(error) => error.to_string(),
    };
    let glb = bytes
        .starts_with(b"glTF")
        .then(|| gltf::binary::Glb::from_slice(bytes))
        .transpose()
        .map_err(|error| error.to_string())?;
    let json = glb.as_ref().map_or(bytes, |glb| glb.json.as_ref());
    let mut json: serde_json::Value =
        serde_json::from_slice(json).map_err(|error| error.to_string())?;
    let mut normalized = false;
    if let Some(scenes) = json
        .get_mut("scenes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for scene in scenes {
            if let Some(scene) = scene.as_object_mut()
                && !scene.contains_key("nodes")
            {
                // Scene.nodes is optional in glTF; gltf-json 1.4 requires it.
                scene.insert("nodes".into(), serde_json::json!([]));
                normalized = true;
            }
        }
    }
    if !normalized {
        return Err(original);
    }
    let json = serde_json::from_value(json).map_err(|error| error.to_string())?;
    Ok((
        gltf::Document::from_json_without_validation(json),
        glb.and_then(|glb| glb.bin)
            .map(std::borrow::Cow::into_owned),
    ))
}

fn validate_structure(document: &gltf::Document, at: &str) -> Result<(), GltfError> {
    use gltf::json::validation::{Error, Validate};

    let json = document.as_json();
    for primitive in json.meshes.iter().flat_map(|mesh| &mesh.primitives) {
        for accessor in primitive.attributes.values() {
            if accessor.value() >= json.accessors.len() {
                return Err(ferr(format!(
                    "{at}: glTF mesh accessor index is out of bounds"
                )));
            }
        }
    }
    let mut errors = Vec::new();
    json.validate(json, gltf::json::Path::new, &mut |path, error| {
        // gltf-json rejects valid zero-filled accessors without a bufferView.
        // Index and enum validation must still run before its infallible getters.
        if matches!(
            error,
            Error::IndexOutOfBounds | Error::Invalid | Error::Oversize
        ) {
            errors.push(format!("{}: {error:?}", path()));
        }
    });
    if !errors.is_empty() {
        return Err(ferr(format!(
            "{at}: invalid glTF structure: {}",
            errors.join(", ")
        )));
    }
    validate_accessor_types(document, at)?;
    let mut parents = vec![None; json.nodes.len()];
    for (parent, node) in json.nodes.iter().enumerate() {
        for child in node.children.iter().flatten() {
            let child = child.value();
            if parents[child].replace(parent).is_some() {
                return Err(ferr(format!(
                    "{at}: glTF node {child} has multiple parents"
                )));
            }
        }
    }
    let mut visited = vec![0_u8; parents.len()];
    for start in 0..parents.len() {
        let mut next = Some(start);
        while let Some(node) = next {
            if visited[node] == 2 {
                break;
            }
            if visited[node] == 1 {
                return Err(ferr(format!("{at}: glTF node hierarchy contains a cycle")));
            }
            visited[node] = 1;
            next = parents[node];
        }
        let mut next = Some(start);
        while let Some(node) = next {
            if visited[node] == 2 {
                break;
            }
            visited[node] = 2;
            next = parents[node];
        }
    }
    Ok(())
}

fn validate_accessor_types(document: &gltf::Document, at: &str) -> Result<(), GltfError> {
    use gltf::Semantic;
    use gltf::accessor::{DataType, Dimensions};

    let invalid = |accessor: &gltf::Accessor<'_>| {
        ferr(format!(
            "{at}: glTF accessor {} has an invalid type or element count",
            accessor.index()
        ))
    };
    for primitive in document.meshes().flat_map(|mesh| mesh.primitives()) {
        let position = primitive.get(&Semantic::Positions).ok_or_else(|| {
            ferr(format!(
                "{at}: glTF mesh primitive has no POSITION attribute"
            ))
        })?;
        for (semantic, accessor) in primitive.attributes() {
            let valid = match semantic {
                Semantic::Positions | Semantic::Normals => {
                    accessor.dimensions() == Dimensions::Vec3
                        && accessor.data_type() == DataType::F32
                }
                Semantic::TexCoords(_) => {
                    accessor.dimensions() == Dimensions::Vec2
                        && (accessor.data_type() == DataType::F32
                            || (matches!(accessor.data_type(), DataType::U8 | DataType::U16)
                                && accessor.normalized()))
                }
                Semantic::Joints(_) => {
                    accessor.dimensions() == Dimensions::Vec4
                        && matches!(accessor.data_type(), DataType::U8 | DataType::U16)
                        && !accessor.normalized()
                }
                Semantic::Weights(_) => {
                    accessor.dimensions() == Dimensions::Vec4
                        && (accessor.data_type() == DataType::F32
                            || (matches!(accessor.data_type(), DataType::U8 | DataType::U16)
                                && accessor.normalized()))
                }
                _ => true,
            };
            let paired = match semantic {
                Semantic::Joints(set) => primitive.get(&Semantic::Weights(set)).is_some(),
                Semantic::Weights(set) => primitive.get(&Semantic::Joints(set)).is_some(),
                _ => true,
            };
            if !valid || !paired || accessor.count() != position.count() {
                return Err(invalid(&accessor));
            }
        }
        if let Some(indices) = primitive.indices()
            && (indices.dimensions() != Dimensions::Scalar
                || !matches!(
                    indices.data_type(),
                    DataType::U8 | DataType::U16 | DataType::U32
                )
                || indices.normalized())
        {
            return Err(invalid(&indices));
        }
        for target in primitive.morph_targets() {
            for accessor in [target.positions(), target.normals()].into_iter().flatten() {
                if accessor.dimensions() != Dimensions::Vec3
                    || accessor.data_type() != DataType::F32
                    || accessor.count() != position.count()
                {
                    return Err(invalid(&accessor));
                }
            }
        }
    }
    for skin in document.skins() {
        if let Some(accessor) = skin.inverse_bind_matrices()
            && (accessor.dimensions() != Dimensions::Mat4
                || accessor.data_type() != DataType::F32
                || accessor.count() < skin.joints().len())
        {
            return Err(invalid(&accessor));
        }
    }
    for animation in document.animations() {
        let mut targets = HashSet::new();
        for channel in animation.channels() {
            let input = channel.sampler().input();
            let output = channel.sampler().output();
            let (property, dimensions) = match channel.target().property() {
                gltf::animation::Property::Translation => (0, Dimensions::Vec3),
                gltf::animation::Property::Rotation => (1, Dimensions::Vec4),
                gltf::animation::Property::Scale => (2, Dimensions::Vec3),
                gltf::animation::Property::MorphTargetWeights => (3, Dimensions::Scalar),
            };
            if !targets.insert((channel.target().node().index(), property)) {
                return Err(ferr(format!(
                    "{at}: glTF animation has duplicate channels for a node property"
                )));
            }
            if input.dimensions() != Dimensions::Scalar || input.data_type() != DataType::F32 {
                return Err(invalid(&input));
            }
            if output.dimensions() != dimensions || output.data_type() != DataType::F32 {
                return Err(invalid(&output));
            }
        }
    }
    Ok(())
}

/// 数一下有多少个通道驱动 `MorphTargetWeights`（只为发一条提示）。
///
/// ⚠️ 这里用**通道的 target 属性**判断，不读 output 数据 —— 提示只需要条数，
/// 而 `read_outputs()` 会真去解一遍 accessor（代价白花）。
fn count_morph_weight_channels(anim: &gltf::Animation<'_>) -> usize {
    anim.channels()
        .filter(|c| c.target().property() == gltf::animation::Property::MorphTargetWeights)
        .count()
}

// ---------------------------------------------------------------------------
// 场景图
// ---------------------------------------------------------------------------

/// 一个节点在**世界空间**的变换（4×4，列主序，与 [`crate::fbx`] 的
/// `ufbx::Matrix` 同布局，便于复用那边的 `bone_offset`）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct World {
    /// 3×3 部分，按列存：`[列0, 列1, 列2]`。
    m: [[f64; 3]; 3],
    /// 平移列。
    t: [f64; 3],
}

impl World {
    /// 单位阵。
    fn identity() -> Self {
        Self {
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            t: [0.0, 0.0, 0.0],
        }
    }

    /// 从 glTF 的局部 TRS 建一个（`T · R · S`，与规范一致）。
    fn from_trs(t: [f32; 3], r: [f32; 4], s: [f32; 3]) -> Self {
        let (x, y, z, w) = (
            f64::from(r[0]),
            f64::from(r[1]),
            f64::from(r[2]),
            f64::from(r[3]),
        );
        // 四元数 → 3×3（列主序，与 glTF 规范的矩阵约定一致）。
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (wx, wy, wz) = (w * x, w * y, w * z);
        let (sx, sy, sz) = (f64::from(s[0]), f64::from(s[1]), f64::from(s[2]));
        Self {
            m: [
                [
                    (1.0 - 2.0 * (yy + zz)) * sx,
                    (2.0 * (xy + wz)) * sx,
                    (2.0 * (xz - wy)) * sx,
                ],
                [
                    (2.0 * (xy - wz)) * sy,
                    (1.0 - 2.0 * (xx + zz)) * sy,
                    (2.0 * (yz + wx)) * sy,
                ],
                [
                    (2.0 * (xz + wy)) * sz,
                    (2.0 * (yz - wx)) * sz,
                    (1.0 - 2.0 * (xx + yy)) * sz,
                ],
            ],
            t: [f64::from(t[0]), f64::from(t[1]), f64::from(t[2])],
        }
    }

    /// 从 glTF 的 4×4 矩阵建一个（列主序）。
    fn from_matrix(m: [[f32; 4]; 4]) -> Self {
        Self {
            m: [
                [f64::from(m[0][0]), f64::from(m[0][1]), f64::from(m[0][2])],
                [f64::from(m[1][0]), f64::from(m[1][1]), f64::from(m[1][2])],
                [f64::from(m[2][0]), f64::from(m[2][1]), f64::from(m[2][2])],
            ],
            t: [f64::from(m[3][0]), f64::from(m[3][1]), f64::from(m[3][2])],
        }
    }

    /// `self · rhs`（`self` 是父，`rhs` 是子）。
    fn mul(&self, rhs: &Self) -> Self {
        let mut m = [[0.0f64; 3]; 3];
        for (c, col) in m.iter_mut().enumerate() {
            for (r, cell) in col.iter_mut().enumerate() {
                *cell = self.m[0][r] * rhs.m[c][0]
                    + self.m[1][r] * rhs.m[c][1]
                    + self.m[2][r] * rhs.m[c][2];
            }
        }
        let mut t = [0.0f64; 3];
        for (r, cell) in t.iter_mut().enumerate() {
            *cell = self.m[0][r] * rhs.t[0]
                + self.m[1][r] * rhs.t[1]
                + self.m[2][r] * rhs.t[2]
                + self.t[r];
        }
        Self { m, t }
    }

    /// 求逆（4×4 仿射，隐含最后一行 `[0, 0, 0, 1]`）。
    ///
    /// ⚠️ 返回 `Option`：导出器写了 0 缩放时矩阵**奇异**，那是**源文件**的属性
    /// 而不是 mdlc 的 bug —— 算出 `inf`/`NaN` 一路传进 SMD 比在这里放弃糟得多。
    /// 调用方拿到 `None` 就退回节点自己的 rest 姿态（见 [`reference_poses`]）。
    fn invert(&self) -> Option<Self> {
        // 行主序便于写余子式：`a[r][c] = self.m[c][r]`。
        let a = [
            [self.m[0][0], self.m[1][0], self.m[2][0]],
            [self.m[0][1], self.m[1][1], self.m[2][1]],
            [self.m[0][2], self.m[1][2], self.m[2][2]],
        ];
        let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
            - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        // 伴随矩阵 / det（= 余子式矩阵的转置），行主序。
        let inv_det = 1.0 / det;
        let b = [
            [
                (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inv_det,
                (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inv_det,
                (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inv_det,
            ],
            [
                (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inv_det,
                (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inv_det,
                (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inv_det,
            ],
            [
                (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inv_det,
                (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inv_det,
                (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inv_det,
            ],
        ];
        // 平移：`t' = -A⁻¹ · t`。
        let t = [
            -(b[0][0] * self.t[0] + b[0][1] * self.t[1] + b[0][2] * self.t[2]),
            -(b[1][0] * self.t[0] + b[1][1] * self.t[1] + b[1][2] * self.t[2]),
            -(b[2][0] * self.t[0] + b[2][1] * self.t[1] + b[2][2] * self.t[2]),
        ];
        Some(Self {
            m: [
                [b[0][0], b[1][0], b[2][0]],
                [b[0][1], b[1][1], b[2][1]],
                [b[0][2], b[1][2], b[2][2]],
            ],
            t,
        })
    }

    /// 把本节点转成 [`crate::fbx`] 的矩阵类型（那边的一整套口径都吃它）。
    ///
    /// ⚠️ `ufbx::Matrix` 是 **12 个字段、无第 4 行**，且**列主序**
    /// （`m00 m10 m20` 是第 0 列）。`Matrix::default()` 是**全 0**，
    /// 所以每个字段都得写。
    fn to_ufbx(self) -> ufbx::Matrix {
        ufbx::Matrix {
            m00: self.m[0][0],
            m10: self.m[0][1],
            m20: self.m[0][2],
            m01: self.m[1][0],
            m11: self.m[1][1],
            m21: self.m[1][2],
            m02: self.m[2][0],
            m12: self.m[2][1],
            m22: self.m[2][2],
            m03: self.t[0],
            m13: self.t[1],
            m23: self.t[2],
        }
    }
}

/// 一个场景节点的局部变换（从 `Node::transform()` 归一化出来）。
fn local_of(n: &gltf::Node) -> World {
    match n.transform() {
        gltf::scene::Transform::Matrix { matrix } => World::from_matrix(matrix),
        // ⭐ `decomposed()` 连 `Matrix` 变体都会自动分解，所以这里其实永远
        // 走这一支；保留 `Matrix` 分支只是为了不用 `unreachable!()`。
        gltf::scene::Transform::Decomposed {
            translation,
            rotation,
            scale,
        } => World::from_trs(translation, rotation, scale),
    }
}

/// 逐节点的父下标表（`parent[i]` = 节点 `i` 的父）。
///
/// glTF 的 `Node::children()` 是**单向**的（只有父知道子），要反查父必须
/// 扫全表。`world_transforms` / `kept_joints` / `reference_poses` / `smd_nodes`
/// 四处都要它，所以建一次传下去 —— 每处都自己扫一遍是 O(节点数²)。
fn parent_table(doc: &gltf::Document) -> Vec<Option<usize>> {
    let mut p: Vec<Option<usize>> = vec![None; doc.nodes().len()];
    for node in doc.nodes() {
        let pi = node.index();
        for c in node.children() {
            p[c.index()] = Some(pi);
        }
    }
    p
}

fn world_transforms(doc: &gltf::Document, parent: &[Option<usize>]) -> Vec<World> {
    let locals: Vec<World> = doc.nodes().map(|n| local_of(&n)).collect();
    accumulate(&locals, parent)
}

fn accumulate(locals: &[World], parent: &[Option<usize>]) -> Vec<World> {
    let mut world = vec![World::identity(); locals.len()];
    let mut done = vec![false; locals.len()];
    let mut chain = Vec::new();
    for start in 0..locals.len() {
        let mut current = Some(start);
        while let Some(index) = current {
            if done[index] {
                break;
            }
            done[index] = true;
            chain.push(index);
            current = parent[index];
        }
        while let Some(index) = chain.pop() {
            world[index] = match parent[index] {
                Some(parent) => world[parent].mul(&locals[index]),
                None => locals[index],
            };
        }
    }
    world
}

// ---------------------------------------------------------------------------
// 收骨
// ---------------------------------------------------------------------------

/// 收骨：**显式** `skin.joints` + 祖先上溯 + DFS 先序。
///
/// # 与 FBX 的差别
///
/// FBX 没有「骨骼列表」这个概念，官方（与 mdlc）靠「被蒙皮权重引用的骨骼
/// ∪ 其全部祖先」**推断**（`docs/fbx-support.md` §1.4）。glTF 有
/// **显式的 `skin.joints`**，所以推断那一步不需要 —— 但**祖先上溯仍要**
/// （`joints` 只列被引用的骨骼，它们的父节点可能不在列表里，而 SMD 的
/// 骨骼表必须是连通的）。
///
/// ⚠️ 顺序仍是 **DFS 先序**（`scene.nodes` 是层序）—— 直接复用
/// [`crate::fbx::dfs_preorder`]，它只吃 `(id, parent)` 与 id 集合，与格式无关。
///
/// ⚠️ **没有蒙皮的网格节点也算一根骨骼**（对齐 FBX 的
/// 「无蒙皮的网格节点自身算一个种子」）—— 静态道具（`box.fbx` / `morph.fbx`）
/// 只有一根骨骼就是这个道理。否则 `SmdInfo::vert_refs` 为空 ⟹ 报
/// 「至少需要一根骨骼」。
fn kept_joints(
    doc: &gltf::Document,
    parent: &[Option<usize>],
    opts: &GltfOpts,
    selectors: &[String],
) -> Vec<usize> {
    let mut used: HashSet<u32> = HashSet::new();

    for node in doc.nodes() {
        if !node_selected(&node, opts, selectors) {
            continue;
        }
        match node.skin() {
            Some(skin) => {
                for j in skin.joints() {
                    used.insert(j.index() as u32);
                }
            }
            None => {
                // 没有蒙皮的**网格节点**自身算一根；非网格节点（纯 Empty /
                // 骨架根）不主动加 —— 它们只作为祖先被带进来。
                if node.mesh().is_some() {
                    used.insert(node.index() as u32);
                }
            }
        }
    }

    // 祖先上溯：`skin.joints` 只列被引用的，父节点可能不在里面。
    let mut ids: Vec<u32> = used.iter().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let mut cur = parent[id as usize];
        while let Some(p) = cur {
            let pu = p as u32;
            if !used.insert(pu) {
                break;
            }
            cur = parent[p];
        }
    }
    if opts.axis == Some(ForcedAxis::Auto) {
        for node in doc.nodes().filter(|node| node.mesh().is_none()) {
            let wrapper = node.extras().as_ref().is_some_and(|extras| {
                serde_json::from_str::<serde_json::Value>(extras.get()).is_ok_and(|extras| {
                    extras["gmpublishedSourceMetadata"]
                        .as_str()
                        .is_some_and(|metadata| {
                            serde_json::from_str::<serde_json::Value>(metadata)
                                .is_ok_and(|metadata| metadata["sourceToGltfAxes"].is_array())
                        })
                })
            });
            if wrapper {
                used.remove(&(node.index() as u32));
            }
        }
    }
    let nodes: Vec<(u32, Option<u32>)> = (0..doc.nodes().len())
        .map(|i| (i as u32, parent[i].map(|p| p as u32)))
        .collect();
    crate::fbx::dfs_preorder(&nodes, &used)
        .into_iter()
        .map(|id| id as usize)
        .collect()
}

/// 该节点是否被 `srcpart` 选中（没写 `srcpart` 时全选）。
///
/// 与 FBX 侧的判据一致（比节点名），但 glTF 上多一个来源：**网格名**。
/// 用户看到的「部件」多半是网格名（Blender 里的 mesh 名），而节点名可能
/// 是 `Body` / `Body.001` 这类。两者任一命中就算选中。
fn part_selectors(
    doc: &gltf::Document,
    parent: &[Option<usize>],
) -> Result<Vec<String>, GltfError> {
    let names = stable_source_names(doc.nodes().map(|node| node.name()), "node");
    let identities = doc
        .nodes()
        .zip(names)
        .map(|(node, bone_name)| {
            crate::fbx::source_part_identity(
                node.name().unwrap_or(""),
                &bone_name,
                node.mesh().and_then(|mesh| mesh.name()).unwrap_or(""),
                node.transform()
                    .matrix()
                    .into_iter()
                    .flatten()
                    .map(f64::from),
            )
        })
        .collect::<Vec<_>>();
    crate::fbx::source_part_selectors(&identities, parent).map_err(ferr)
}

fn node_selected(n: &gltf::Node, opts: &GltfOpts, selectors: &[String]) -> bool {
    if opts.parts.is_empty() {
        return true;
    }
    let node_name = n.name().unwrap_or("");
    let selector = &selectors[n.index()];
    let mesh_name = n.mesh().and_then(|m| m.name().map(str::to_string));
    opts.parts
        .iter()
        .any(|part| match part.strip_prefix("@node:") {
            Some(index) => index.parse::<usize>() == Ok(n.index()) || part == selector,
            None => {
                part.eq_ignore_ascii_case(node_name)
                    || mesh_name
                        .as_deref()
                        .is_some_and(|name| part.eq_ignore_ascii_case(name))
            }
        })
}

fn smd_nodes(doc: &gltf::Document, parent: &[Option<usize>], kept: &[usize]) -> Vec<SmdNode> {
    let parent = skeleton_parents(parent, kept);
    let idx_of: HashMap<usize, i32> = kept
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i as i32))
        .collect();
    let names = stable_source_names(doc.nodes().map(|node| node.name()), "node");
    kept.iter()
        .enumerate()
        .map(|(i, &id)| SmdNode {
            index: i as i32,
            name: names[id].clone(),
            parent: parent[id]
                .and_then(|p| idx_of.get(&p).copied())
                .unwrap_or(-1),
        })
        .collect()
}

fn skeleton_parents(parent: &[Option<usize>], kept: &[usize]) -> Vec<Option<usize>> {
    let mut selected = vec![false; parent.len()];
    for &id in kept {
        selected[id] = true;
    }
    let mut result = vec![None; parent.len()];
    for &id in kept {
        let mut candidate = parent[id];
        while let Some(index) = candidate {
            if selected[index] {
                break;
            }
            candidate = parent[index];
        }
        result[id] = candidate;
    }
    result
}

pub fn stable_source_names<'a>(
    names: impl Iterator<Item = Option<&'a str>>,
    prefix: &str,
) -> Vec<String> {
    let names = names.collect::<Vec<_>>();
    let reserved = names
        .iter()
        .flatten()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut used = HashSet::new();
    names
        .into_iter()
        .enumerate()
        .map(|(index, name)| match name.filter(|name| !name.is_empty()) {
            Some(name) if used.insert(name.to_ascii_lowercase()) => name.to_owned(),
            _ => {
                let mut name = format!("{prefix}_{index}");
                let mut suffix = 1;
                while reserved.contains(&name.to_ascii_lowercase())
                    || !used.insert(name.to_ascii_lowercase())
                {
                    name = format!("{prefix}_{index}_{suffix}");
                    suffix += 1;
                }
                name
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 几何
// ---------------------------------------------------------------------------

fn material_name(prim: &gltf::Primitive, opts: &GltfOpts, names: &[String]) -> String {
    if let Some(index) = prim.material().index() {
        return names[index].clone();
    }
    opts.material
        .clone()
        .unwrap_or_else(|| FALLBACK_MATERIAL.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureCoordinates {
    pub set: u32,
    pub offset: [f32; 2],
    pub rotation: f32,
    pub scale: [f32; 2],
}

impl TextureCoordinates {
    pub fn from_json(texture: &serde_json::Value) -> Result<Self, GltfError> {
        let transform = &texture["extensions"]["KHR_texture_transform"];
        if !transform.is_null() && !transform.is_object() {
            return Err(ferr("glTF texture transform must be an object"));
        }
        let vector = |name: &str, default: [f32; 2]| -> Result<[f32; 2], GltfError> {
            if transform[name].is_null() {
                return Ok(default);
            }
            let values = transform[name]
                .as_array()
                .filter(|values| values.len() == 2)
                .ok_or_else(|| ferr("glTF texture transform has an invalid vector"))?;
            Ok([
                values[0]
                    .as_f64()
                    .ok_or_else(|| ferr("glTF texture transform has an invalid vector"))?
                    as f32,
                values[1]
                    .as_f64()
                    .ok_or_else(|| ferr("glTF texture transform has an invalid vector"))?
                    as f32,
            ])
        };
        let set = transform
            .get("texCoord")
            .or_else(|| texture.get("texCoord"))
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| ferr("glTF texture has an invalid UV set"))
            })
            .transpose()?
            .unwrap_or(0);
        let coordinates = Self {
            set,
            offset: vector("offset", [0.0; 2])?,
            rotation: transform
                .get("rotation")
                .map(|value| {
                    value
                        .as_f64()
                        .ok_or_else(|| ferr("glTF texture has an invalid rotation"))
                })
                .transpose()?
                .unwrap_or(0.0) as f32,
            scale: vector("scale", [1.0; 2])?,
        };
        coordinates.validate()?;
        Ok(coordinates)
    }

    fn validate(self) -> Result<(), GltfError> {
        if self
            .offset
            .iter()
            .chain(&self.scale)
            .any(|value| !value.is_finite())
            || !self.rotation.is_finite()
        {
            return Err(ferr("glTF texture transform must be finite"));
        }
        Ok(())
    }

    fn apply(self, uv: [f32; 2]) -> [f32; 2] {
        let (sine, cosine) = self.rotation.sin_cos();
        let uv = [uv[0] * self.scale[0], uv[1] * self.scale[1]];
        [
            self.offset[0] + cosine * uv[0] - sine * uv[1],
            self.offset[1] + sine * uv[0] + cosine * uv[1],
        ]
    }
}

fn texture_coordinates(
    primitive: &gltf::Primitive<'_>,
) -> Result<Option<TextureCoordinates>, GltfError> {
    let material = primitive.material();
    if let Some(base) = material.pbr_metallic_roughness().base_color_texture() {
        let transform = base.texture_transform();
        let coordinates = TextureCoordinates {
            set: transform
                .as_ref()
                .and_then(|transform| transform.tex_coord())
                .unwrap_or_else(|| base.tex_coord()),
            offset: transform
                .as_ref()
                .map_or([0.0; 2], |transform| transform.offset()),
            rotation: transform
                .as_ref()
                .map_or(0.0, |transform| transform.rotation()),
            scale: transform
                .as_ref()
                .map_or([1.0; 2], |transform| transform.scale()),
        };
        coordinates.validate()?;
        return Ok(Some(coordinates));
    }
    if let Some(normal) = material.normal_texture() {
        return TextureCoordinates::from_json(&serde_json::json!({
            "texCoord": normal.tex_coord(),
            "extensions": {"KHR_texture_transform": normal.extension_value("KHR_texture_transform")}
        }))
        .map(Some);
    }
    Ok(None)
}

/// 把一个 primitive 的三角形追加到 `out`。
///
/// 与 FBX 侧 `from_scene` 的三角形循环同构，差别只在数据来源：
///
/// | | FBX | glTF |
/// |---|---|---|
/// | 位置 / 法线 / UV | 按**角**下标（`corner`） | 按**顶点**下标（`vi`） |
/// | 权重 | 按顶点下标 | 按顶点下标 |
/// | UV | 要翻 V | **不翻** |
/// | 无权重时 | 绑到网格节点 | 绑到网格节点（同） |
///
/// ⚠️ glTF 的索引就是顶点下标（`POSITION` 数组的下标），没有 SMD 那种
/// 「角」的概念 —— `TEXCOORD_0` 等属性数组与 `POSITION` **等长同序**。
/// 所以这里只用一个 `vi`。
#[allow(clippy::too_many_arguments)]
fn push_primitive<'a, 's, F>(
    prim: &gltf::Primitive<'a>,
    node: &gltf::Node,
    material: &str,
    cp_off: u32,
    bone_ix: Option<i32>,
    idx_of: &HashMap<usize, i32>,
    parent: &[Option<usize>],
    get: F,
    out: &mut Vec<SmdTriangle>,
) -> Result<(), GltfError>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let pos = prim.get(&gltf::Semantic::Positions);
    let Some(pos) = pos else {
        return Err(ferr("glTF mesh primitive has no POSITION attribute"));
    };
    let positions = read_vec3(&pos, get.clone());
    let normals = prim
        .get(&gltf::Semantic::Normals)
        .map(|a| read_vec3(&a, get.clone()));
    let coordinates = texture_coordinates(prim)?;
    let uv_accessor = prim.get(&gltf::Semantic::TexCoords(
        coordinates.map_or(0, |coordinates| coordinates.set),
    ));
    if coordinates.is_some() && uv_accessor.is_none() {
        return Err(ferr(
            "glTF texture references a UV set missing from its mesh",
        ));
    }
    let uvs = uv_accessor.map(|accessor| {
        let mut uvs = read_uv(&accessor, get.clone());
        if let Some(coordinates) = coordinates {
            for uv in &mut uvs {
                *uv = coordinates.apply(*uv);
            }
        }
        uvs
    });
    let skin_weights = primitive_skin_weights(prim, node, positions.len(), idx_of, get.clone())?;
    if positions.iter().flatten().any(|value| !value.is_finite())
        || normals
            .iter()
            .flatten()
            .flatten()
            .any(|value| !value.is_finite())
        || uvs
            .iter()
            .flatten()
            .flatten()
            .any(|value| !value.is_finite())
    {
        return Err(ferr("glTF mesh has invalid vertex attributes"));
    }

    let _ = parent;

    // 索引：没写 `indices` 时按规范是「0,1,2,…」（非索引化）。
    let indices = match prim.indices() {
        Some(a) => read_indices(&a, get.clone()),
        None => (0..positions.len() as u32).collect(),
    };
    if indices
        .iter()
        .any(|index| *index as usize >= positions.len())
        || (prim.mode() == Mode::Triangles && !indices.len().is_multiple_of(3))
    {
        return Err(ferr("glTF mesh has invalid triangle indices"));
    }
    let tris = triangles_of(prim.mode(), &indices);

    for t in tris {
        let flat_normal = if normals.is_none() {
            let [a, b, c] = t.map(|index| positions[index as usize]);
            let u: [f64; 3] = std::array::from_fn(|axis| f64::from(b[axis]) - f64::from(a[axis]));
            let v: [f64; 3] = std::array::from_fn(|axis| f64::from(c[axis]) - f64::from(a[axis]));
            let normal = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
            if length > 0.0 {
                normal.map(|value| (value / length) as f32)
            } else {
                [0.0, 0.0, 1.0]
            }
        } else {
            [0.0, 0.0, 1.0]
        };
        let mut vs: Vec<SmdVertex> = Vec::with_capacity(3);
        for &vi in &t {
            let vi = vi as usize;
            let p = positions.get(vi).copied().unwrap_or([0.0; 3]);
            let nrm = normals
                .as_ref()
                .and_then(|n| n.get(vi).copied())
                .unwrap_or(flat_normal);
            // ⚠️ **glTF 路径不翻 V**（`docs/gltf-support.md` §5.1）——
            // Blender 的 glTF 导出器写的就是最终值，而 SMD 解析器那次翻转
            // 发生在**解析 SMD** 时，glTF 不经过它。
            let uv = uvs
                .as_ref()
                .and_then(|u| u.get(vi).copied())
                .unwrap_or([0.0, 0.0]);

            let mut links = skin_weights[vi].clone();
            // ⚠️ 与 FBX 侧同一条兜底：静态网格（没有 `skin`）的顶点没有权重，
            // 绑到**它所在的那个网格节点**上。不补的话 `SmdInfo::vert_refs`
            // 为空 ⟹ 收骨判据看不到顶点引用 ⟹ 报「至少需要一根骨骼」。
            let parent_bone = links
                .iter()
                .max_by(|a, b| {
                    a.weight
                        .partial_cmp(&b.weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|l| l.bone)
                .unwrap_or_else(|| bone_ix.unwrap_or(0));
            if links.is_empty() {
                links.push(SmdBoneLink {
                    bone: parent_bone,
                    weight: 1.0,
                });
            }

            vs.push(SmdVertex {
                parent_bone,
                position: p,
                normal: nrm,
                uv,
                links,
                // 控制点号 = 全局号段 + 本网格内的顶点号（与
                // [`shape_keys_of`] 里 `cp_off + vi` 用的是同一套）。
                src_index: cp_off + vi as u32,
            });
        }
        if vs.len() == 3 {
            out.push(SmdTriangle {
                material: material.to_string(),
                vertices: [vs[0].clone(), vs[1].clone(), vs[2].clone()],
            });
        }
    }
    Ok(())
}

fn primitive_skin_weights<'a, 's, F>(
    primitive: &gltf::Primitive<'a>,
    node: &gltf::Node<'_>,
    count: usize,
    bone_indices: &HashMap<usize, i32>,
    get: F,
) -> Result<Vec<Vec<SmdBoneLink>>, GltfError>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let mut influences = vec![Vec::<SmdBoneLink>::new(); count];
    let Some(skin) = node.skin() else {
        return Ok(influences);
    };
    let joint_to_node = skin.joints().map(|joint| joint.index()).collect::<Vec<_>>();
    for (semantic, joints) in primitive.attributes() {
        let gltf::Semantic::Joints(set) = semantic else {
            continue;
        };
        let weights = primitive
            .get(&gltf::Semantic::Weights(set))
            .ok_or_else(|| ferr("glTF mesh has joints without matching weights"))?;
        let joints = read_joints(&joints, get.clone());
        let weights = read_weights(&weights, get.clone());
        for ((joints, weights), links) in joints.iter().zip(weights.iter()).zip(&mut influences) {
            for (&joint, &weight) in joints.iter().zip(weights) {
                if !weight.is_finite() || weight < 0.0 || usize::from(joint) >= joint_to_node.len()
                {
                    return Err(ferr("glTF mesh has invalid skin weights or joint indices"));
                }
            }
            for link in weights_of(Some(joints), Some(weights), &joint_to_node, bone_indices) {
                if let Some(existing) = links.iter_mut().find(|existing| existing.bone == link.bone)
                {
                    existing.weight += link.weight;
                    if !existing.weight.is_finite() {
                        return Err(ferr("glTF skin weight sum is not finite"));
                    }
                } else {
                    links.push(link);
                }
            }
        }
    }
    for links in &mut influences {
        links.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    }
    Ok(influences)
}

/// 读一个 `[f32; 3]` 的 accessor。
///
/// ⚠️ **两种 `None` 必须分开看**（`docs/gltf-support.md` §3.2 的坑一）：
///
/// * accessor **本身不存在** —— 那是调用方用 `Primitive::get` 判断的，
///   轮不到这里；
/// * accessor **没有 `bufferView`** —— 规范语义是**全零**，而 crate 的
///   `Iter::new` 对这种情况返回 `None`（`gltf-1.4.1\src\accessor\util.rs`
///   里 `accessor.view().and_then(..)`，`view()` 为 `None` 就直接短路）。
///
/// 这里统一按**全零**处理（长度取 `count()`）—— 那正是 Blender 对「只改位置、
/// 不改法线」的 morph target 写出来的东西（实测 `morph_zeronrm.glb`）。
/// 把它当读失败会让**最常见的 shape key 形态**直接编不出来。
///
/// ⚠️ `Iter::new` 内部算 `stride × (count − 1)`，`count == 0` 时在 debug 下
/// 会**下溢 panic**，所以零元素要提前返回（release 下则是无意义的越界读）。
///
/// 这个函数之所以能替代 `Primitive::reader`：`mesh::Reader` 的字段是
/// `pub(crate)`（`mesh\mod.rs:126-134`），外部造不出来；但 `accessor::Iter`
/// 是**公开**的（`accessor\mod.rs:76 pub use self::util::{Item, Iter}`），
/// 而且 `Primitive::reader` 内部走的正是同一个 `Iter::new`
/// （`mesh\mod.rs:336-347`）。
fn read_vec3<'a, 's, F>(a: &gltf::Accessor<'a>, get: F) -> Vec<[f32; 3]>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    if a.count() == 0 {
        return Vec::new();
    }
    match gltf::accessor::Iter::new(a.clone(), get) {
        Some(it) => it.collect(),
        None => vec![[0.0; 3]; a.count()],
    }
}

/// 读一个 `[f32; 2]` 的 UV accessor。
///
/// ⚠️ `TEXCOORD_n` 允许 `U8` / `U16`（**归一化**）与 `F32` 三种分量类型，
/// 而 `accessor::Iter` 是按 Rust 类型硬解字节的（`debug_assert_eq!` 会检查
/// `size_of::<T>() == accessor.size()`）⟹ 用错类型在 debug 下直接 panic。
/// 所以这里必须先按 `data_type()` 分派，再按规范做归一化
/// （`u8 / 255`、`u16 / 65535`，与 crate 内部 `Normalize` 的口径一致）。
fn read_uv<'a, 's, F>(a: &gltf::Accessor<'a>, get: F) -> Vec<[f32; 2]>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    use gltf::accessor::DataType;
    if a.count() == 0 {
        return Vec::new();
    }
    match a.data_type() {
        DataType::U8 => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it
                .map(|v: [u8; 2]| [f32::from(v[0]) / 255.0, f32::from(v[1]) / 255.0])
                .collect(),
            None => vec![[0.0; 2]; a.count()],
        },
        DataType::U16 => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it
                .map(|v: [u16; 2]| [f32::from(v[0]) / 65535.0, f32::from(v[1]) / 65535.0])
                .collect(),
            None => vec![[0.0; 2]; a.count()],
        },
        _ => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it.collect(),
            None => vec![[0.0; 2]; a.count()],
        },
    }
}

/// 读一个 `[f32; 4]` 的蒙皮权重 accessor（`WEIGHTS_0`）。
///
/// 与 [`read_uv`] 同样的理由要先按 `data_type()` 分派：`WEIGHTS_n` 也允许
/// `U8` / `U16`（归一化）与 `F32`，且 Blender 默认写 **`F32`**，而别的导出器
/// 可能写 `U8`。
fn read_weights<'a, 's, F>(a: &gltf::Accessor<'a>, get: F) -> Vec<[f32; 4]>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    use gltf::accessor::DataType;
    if a.count() == 0 {
        return Vec::new();
    }
    match a.data_type() {
        DataType::U8 => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it
                .map(|v: [u8; 4]| v.map(|x| f32::from(x) / 255.0))
                .collect(),
            None => vec![[0.0; 4]; a.count()],
        },
        DataType::U16 => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it
                .map(|v: [u16; 4]| v.map(|x| f32::from(x) / 65535.0))
                .collect(),
            None => vec![[0.0; 4]; a.count()],
        },
        _ => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it.collect(),
            None => vec![[0.0; 4]; a.count()],
        },
    }
}

/// 读一个 `[u16; 4]` 的 accessor（`JOINTS_0`）。
///
/// ⚠️ **`JOINTS_0` 是「`skin.joints` 数组的下标」，不是节点下标**
/// （`docs/_probe/gltf_joints.js` 实测 `rig.glb`：底部顶点 `joints[0]` ⟹
/// `skin.joints[0] = 2` = 节点 2 = `Pelvis`）⟹ 拿到之后还要再查一次
/// `skin.joints()`。这个坑很隐蔽：直接当节点下标用会**静默**绑错骨骼，
/// 而骨骼数恰好够多时不会越界，于是产物只是「动起来不对劲」。
///
/// 走 `u16` 是为了兼容 `Uint8` / `Uint16` 两种分量类型（Blender 默认写
/// `Uint8`）；`Uint32` 在规范里不允许用于 `JOINTS_n`。
fn read_joints<'a, 's, F>(a: &gltf::Accessor<'a>, get: F) -> Vec<[u16; 4]>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    use gltf::accessor::DataType;
    if a.count() == 0 {
        return Vec::new();
    }
    match a.data_type() {
        DataType::U8 => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it.map(|v: [u8; 4]| v.map(u16::from)).collect(),
            None => vec![[0; 4]; a.count()],
        },
        DataType::U16 => match gltf::accessor::Iter::new(a.clone(), get) {
            Some(it) => it.collect(),
            None => vec![[0; 4]; a.count()],
        },
        // 规范里 `JOINTS_n` 只能是 U8 / U16；别的取值是坏文件，按全零处理
        // 会让顶点绑到第 0 根骨骼上，所以这里报不了错也至少不 panic。
        _ => vec![[0; 4]; a.count()],
    }
}

/// 读索引 accessor（`primitive.indices()`），统一成 `u32`。
fn read_indices<'a, 's, F>(a: &gltf::Accessor<'a>, get: F) -> Vec<u32>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    use gltf::accessor::DataType;
    if a.count() == 0 {
        return Vec::new();
    }
    match a.data_type() {
        // ⚠️ 分量类型必须**显式**写在 `Iter::<T>` 上：`it.map(u32::from)` 里的
        // `u32::from` 对多种整型都有实现，编译器推不出 `T`（E0283）。
        DataType::U8 => match gltf::accessor::Iter::<u8>::new(a.clone(), get) {
            Some(it) => it.map(u32::from).collect(),
            None => vec![0; a.count()],
        },
        DataType::U16 => match gltf::accessor::Iter::<u16>::new(a.clone(), get) {
            Some(it) => it.map(u32::from).collect(),
            None => vec![0; a.count()],
        },
        DataType::U32 => match gltf::accessor::Iter::<u32>::new(a.clone(), get) {
            Some(it) => it.collect(),
            None => vec![0; a.count()],
        },
        _ => Vec::new(),
    }
}

/// 把 `primitive.indices()` 的结果按 `mode` 展开成三角形列表（每 3 个一组）。
///
/// `mode` 缺省是 `TRIANGLES`（规范）。`TRIANGLE_STRIP` / `TRIANGLE_FAN` 也一并
/// 支持 —— 它们只是索引空间的重排，顺手做了比报错更省事；`POINTS` / `LINES`
/// 之类没有面，返回空（调用方会看到「没有任何三角形」那条错误）。
fn triangles_of(mode: Mode, idx: &[u32]) -> Vec<[u32; 3]> {
    match mode {
        Mode::Triangles => idx
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| [c[0], c[1], c[2]])
            .collect(),
        Mode::TriangleStrip => (0..idx.len().saturating_sub(2))
            .map(|i| {
                // 奇数号三角形要翻转绕序（规范要求）。
                if i % 2 == 0 {
                    [idx[i], idx[i + 1], idx[i + 2]]
                } else {
                    [idx[i + 1], idx[i], idx[i + 2]]
                }
            })
            .collect(),
        Mode::TriangleFan => (1..idx.len().saturating_sub(1))
            .map(|i| [idx[0], idx[i], idx[i + 1]])
            .collect(),
        _ => Vec::new(),
    }
}

/// 从一个网格的 `extras.targetNames` 取形变目标名。
///
/// glTF 2.0 **没有**给 morph target 起名字的字段，`targetNames` 是
/// **事实标准**（Blender 导出器、three.js、model-viewer 都用它），
/// 放在 `mesh.extras` 里（`docs/gltf-support.md` §5.5）。
///
/// 拿不到就返回空表，调用方回退到 [`crate::fbx::shape_key_frame`] 的编号名。
/// ⚠️ `extras()` 返回的是 `&Option<Box<RawValue>>`（`gltf-json` 的
/// `pub type Extras = Option<Box<RawValue>>`），所以要 `.as_ref()` 再
/// `RawValue::get()` 拿原始 JSON 串自己解析 —— 那正是 `extras` feature 的用法。
fn morph_target_names(mesh: &gltf::Mesh) -> Vec<String> {
    let Some(raw) = mesh.extras().as_ref() else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw.get()) else {
        return Vec::new();
    };
    v.get("targetNames")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn shape_keys_of<'a, 's, F>(
    prim: &gltf::Primitive<'a>,
    names: &[String],
    cp_off: u32,
    get: F,
) -> Result<Vec<FbxShapeKey>, GltfError>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let mut out: Vec<FbxShapeKey> = Vec::new();
    let count = prim
        .get(&gltf::Semantic::Positions)
        .map_or(0, |accessor| accessor.count());
    for (ti, target) in prim.morph_targets().enumerate() {
        let offsets = target
            .positions()
            .map(|accessor| read_vec3(&accessor, get.clone()))
            .unwrap_or_else(|| vec![[0.0; 3]; count]);
        let normals = target
            .normals()
            .map(|accessor| read_vec3(&accessor, get.clone()))
            .unwrap_or_else(|| vec![[0.0; 3]; count]);
        if offsets
            .iter()
            .flatten()
            .chain(normals.iter().flatten())
            .any(|value| !value.is_finite())
        {
            return Err(ferr("glTF morph target has invalid vertex attributes"));
        }

        let mut position_offsets = Vec::with_capacity(offsets.len());
        let mut normal_offsets = Vec::with_capacity(offsets.len());
        let mut vertex_index = Vec::with_capacity(offsets.len());
        for (vi, d) in offsets.iter().enumerate() {
            if *d == [0.0; 3] && normals[vi] == [0.0; 3] {
                continue;
            }
            position_offsets.push(*d);
            normal_offsets.push(normals.get(vi).copied().unwrap_or([0.0; 3]));
            vertex_index.push(cp_off + vi as u32);
        }
        if vertex_index.is_empty() {
            continue;
        }
        let name = names
            .get(ti)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| FbxGeometry::shape_key_frame(ti).to_string());
        out.push(FbxShapeKey {
            name,
            position_offsets,
            normal_offsets,
            vertex_index,
        });
    }
    Ok(out)
}

/// 一个顶点的蒙皮绑定。
///
/// `joints` 是 `JOINTS_0` 的四个值，它们是 **`skin.joints` 数组的下标**
/// （见 [`read_joints`]），所以这里要先经 `joint_to_node` 换成节点下标，
/// 再经 `idx_of` 换成 SMD 骨骼下标。
fn weights_of(
    joints: Option<&[u16; 4]>,
    weights: Option<&[f32; 4]>,
    joint_to_node: &[usize],
    idx_of: &HashMap<usize, i32>,
) -> Vec<SmdBoneLink> {
    let (Some(j), Some(w)) = (joints, weights) else {
        return Vec::new();
    };
    let mut out: Vec<SmdBoneLink> = Vec::new();
    for k in 0..4 {
        if w[k] <= 0.0 {
            continue;
        }
        let Some(&node) = joint_to_node.get(j[k] as usize) else {
            continue;
        };
        let Some(&bone) = idx_of.get(&node) else {
            continue;
        };
        out.push(SmdBoneLink { bone, weight: w[k] });
    }
    // 权重降序（与 FBX 侧 `weights_of` 一致；下游按这个顺序取前 3 组）。
    out.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn bind_worlds<'a, 's, F>(
    doc: &'a gltf::Document,
    get: F,
) -> Result<HashMap<usize, World>, GltfError>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let mut out: HashMap<usize, World> = HashMap::new();
    for skin in doc.skins() {
        let matrices = if let Some(accessor) = skin.inverse_bind_matrices() {
            gltf::accessor::Iter::<[[f32; 4]; 4]>::new(accessor, get.clone())
                .ok_or_else(|| ferr("glTF inverse bind matrices could not be read"))?
                .take(skin.joints().len())
                .map(|matrix| {
                    if matrix.iter().flatten().any(|value| !value.is_finite())
                        || matrix[0][3] != 0.0
                        || matrix[1][3] != 0.0
                        || matrix[2][3] != 0.0
                        || matrix[3][3] != 1.0
                    {
                        return Err(ferr("glTF inverse bind matrix must be finite and affine"));
                    }
                    World::from_matrix(matrix)
                        .invert()
                        .ok_or_else(|| ferr("glTF inverse bind matrix is singular"))
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            vec![World::identity(); skin.joints().len()]
        };
        for (joint, matrix) in skin.joints().zip(matrices) {
            if let Some(previous) = out.get(&joint.index()) {
                let same = previous
                    .m
                    .iter()
                    .flatten()
                    .chain(&previous.t)
                    .zip(matrix.m.iter().flatten().chain(&matrix.t))
                    .all(|(a, b)| (a - b).abs() <= 1e-5 * a.abs().max(b.abs()).max(1.0));
                if !same {
                    return Err(ferr(format!(
                        "glTF joint '{}' has different bind poses in multiple skins; split the armatures before exporting",
                        joint.name().unwrap_or("unnamed")
                    )));
                }
            } else {
                out.insert(joint.index(), matrix);
            }
        }
    }
    Ok(out)
}

fn reference_poses(
    parent: &[Option<usize>],
    world: &[World],
    bind: &HashMap<usize, World>,
    kept: &[usize],
    opts: &GltfOpts,
) -> Vec<SmdPose> {
    let parent = skeleton_parents(parent, kept);
    let pose_world: Vec<World> = (0..world.len())
        .map(|i| bind.get(&i).copied().unwrap_or(world[i]))
        .collect();
    kept.iter()
        .enumerate()
        .map(|(i, &id)| {
            let (pos, rot) = if opts.axis == Some(ForcedAxis::Auto) {
                let world = pose_world[id].to_ufbx();
                let parent = parent[id].map(|parent| pose_world[parent].to_ufbx());
                crate::fbx::affine_local_pose(&world, parent.as_ref())
            } else {
                match parent[id] {
                    Some(p) => (
                        rigid_bone_offset(&pose_world[p], &pose_world[id]),
                        crate::fbx::local_rotation(
                            &pose_world[p].to_ufbx(),
                            &pose_world[id].to_ufbx(),
                        ),
                    ),
                    None => (
                        world_translation(&pose_world[id]),
                        ufbx::matrix_to_transform(&pose_world[id].to_ufbx()).rotation,
                    ),
                }
            };
            let (pos, rot) = opts.local(ufbx::Transform {
                translation: pos,
                rotation: rot,
                scale: ufbx::Vec3 {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
            });
            SmdPose {
                bone: i as i32,
                position: [pos.x as f32, pos.y as f32, pos.z as f32],
                rotation: crate::fbx::source_euler_from_quat(rot.x, rot.y, rot.z, rot.w),
            }
        })
        .collect()
}

/// 一个文件里所有动画栈的稳定、唯一名字。
///
/// [`read`] 拿它填 `FbxGeometry::anim_stacks`，[`read_frames`] 拿它做
/// `srcstack` 的匹配与报错文案 —— **两处必须用同一个函数**，否则诊断里
/// 印出来的名字会跟用户能写的名字对不上。
fn anim_names(doc: &gltf::Document) -> Vec<String> {
    stable_source_names(
        doc.animations().map(|animation| animation.name()),
        "animation",
    )
}

pub fn source_parts(path: &Path, at: &str) -> Result<Vec<crate::fbx::SourcePart>, GltfError> {
    let (document, _, _) = load_document(path, at)?;
    let selectors = part_selectors(&document, &parent_table(&document))?;
    Ok(document
        .nodes()
        .filter_map(|node| {
            let mesh = node.mesh()?;
            if !mesh.primitives().any(|primitive| {
                matches!(
                    primitive.mode(),
                    Mode::Triangles | Mode::TriangleStrip | Mode::TriangleFan
                ) && primitive
                    .get(&gltf::Semantic::Positions)
                    .is_some_and(|accessor| accessor.count() >= 3)
            }) {
                return None;
            }
            Some(crate::fbx::SourcePart {
                selector: selectors[node.index()].clone(),
                name: node
                    .name()
                    .or_else(|| mesh.name())
                    .filter(|name| !name.is_empty())
                    .map_or_else(|| format!("mesh{}", node.index()), str::to_owned),
            })
        })
        .collect())
}

pub fn collision_parts(path: &Path, at: &str) -> Result<Vec<String>, GltfError> {
    let (document, _, _) = load_document(path, at)?;
    let selectors = part_selectors(&document, &parent_table(&document))?;
    Ok(document
        .nodes()
        .filter(|node| node.mesh().is_some())
        .filter(|node| {
            node.name()
                .is_some_and(|name| name.starts_with("collision / "))
                || node.extras().as_ref().is_some_and(|extras| {
                    serde_json::from_str::<serde_json::Value>(extras.get()).is_ok_and(|extras| {
                        extras["gmpublishedCollision"] == true || extras["sourceCollision"] == true
                    })
                })
        })
        .map(|node| selectors[node.index()].clone())
        .collect())
}

pub fn source_metadata(path: &Path, at: &str) -> Result<Vec<String>, GltfError> {
    let (document, _, _) = load_document(path, at)?;
    Ok(document
        .nodes()
        .filter_map(|node| {
            let extras = node.extras().as_ref()?;
            let extras: serde_json::Value = serde_json::from_str(extras.get()).ok()?;
            extras["gmpublishedSourceMetadata"]
                .as_str()
                .map(str::to_owned)
        })
        .collect())
}

pub fn animation_names(path: &Path, at: &str) -> Result<Vec<String>, GltfError> {
    let (document, _, _) = load_document(path, at)?;
    Ok(anim_names(&document))
}

// ---------------------------------------------------------------------------
// 主入口
// ---------------------------------------------------------------------------

fn mesh_instances(
    doc: &gltf::Document,
    opts: &GltfOpts,
    selectors: &[String],
) -> Vec<(usize, usize)> {
    doc.nodes()
        .filter(|n| node_selected(n, opts, selectors))
        .filter_map(|n| n.mesh().map(|m| (n.index(), m.index())))
        .collect()
}

/// accessor → 字节 的取值器（`gltf::accessor::Iter::new` 的第二个参数）。
///
/// ⚠️ **为什么是一个具名函数而不是就地写的闭包**：就地写 `move |b: gltf::Buffer|
/// buffers.get(b.index()).map(Vec::as_slice)` 会把生命周期推成
/// `for<'x> Fn(gltf::Buffer<'x>) -> Option<&'x [u8]>`（输出借自**输入**），
/// 而 `Iter::new` 要的是 `Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>`（输出借自
/// **buffers**）。两者只在「文档与 buffers 共享同一个借用」时才等价 —— 这里
/// 不是（`doc` 与 `buffers` 是 `load_document` 返回的两个独立值），于是就地
/// 写会报 `returning this value requires that '1 must outlive '2`。
fn buffer_getter<'a, 's>(
    buffers: &'s [Vec<u8>],
) -> impl Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]> + 's {
    move |b: gltf::Buffer<'a>| buffers.get(b.index()).map(Vec::as_slice)
}

fn world_translation(world: &World) -> ufbx::Vec3 {
    ufbx::Vec3 {
        x: world.t[0],
        y: world.t[1],
        z: world.t[2],
    }
}

fn rigid_bone_offset(parent: &World, child: &World) -> ufbx::Vec3 {
    let rotation = crate::fbx::rotation_of(&parent.to_ufbx());
    let delta = std::array::from_fn::<_, 3, _>(|axis| child.t[axis] - parent.t[axis]);
    ufbx::Vec3 {
        x: rotation.m00 * delta[0] + rotation.m10 * delta[1] + rotation.m20 * delta[2],
        y: rotation.m01 * delta[0] + rotation.m11 * delta[1] + rotation.m21 * delta[2],
        z: rotation.m02 * delta[0] + rotation.m12 * delta[1] + rotation.m22 * delta[2],
    }
}

fn transformed_vector(value: [f32; 3], matrix: &World, position: bool) -> [f32; 3] {
    std::array::from_fn(|axis| {
        (matrix.m[0][axis] * f64::from(value[0])
            + matrix.m[1][axis] * f64::from(value[1])
            + matrix.m[2][axis] * f64::from(value[2])
            + if position { matrix.t[axis] } else { 0.0 }) as f32
    })
}

fn transformed_normal(value: [f32; 3], inverse: &World) -> [f32; 3] {
    let normal: [f64; 3] = std::array::from_fn(|axis| {
        inverse.m[axis]
            .iter()
            .zip(value)
            .map(|(a, b)| a * f64::from(b))
            .sum()
    });
    let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
    if length > 0.0 {
        normal.map(|value| (value / length) as f32)
    } else {
        [0.0; 3]
    }
}

fn import_units(opts: &GltfOpts) -> std::borrow::Cow<'_, GltfOpts> {
    if opts.axis == Some(ForcedAxis::Auto) {
        let mut options = opts.clone();
        // glTF uses metres; Source models use inches.
        options.scale /= 0.0254;
        std::borrow::Cow::Owned(options)
    } else {
        std::borrow::Cow::Borrowed(opts)
    }
}

fn source_axis_matrix(document: &gltf::Document) -> Result<ufbx::Matrix, GltfError> {
    for node in document.nodes() {
        let Some(extras) = node.extras().as_ref() else {
            continue;
        };
        let extras: serde_json::Value = serde_json::from_str(extras.get())
            .map_err(|error| ferr(format!("Invalid glTF node metadata: {error}")))?;
        let Some(metadata) = extras["gmpublishedSourceMetadata"].as_str() else {
            continue;
        };
        let metadata: serde_json::Value = serde_json::from_str(metadata)
            .map_err(|error| ferr(format!("Invalid Source axis metadata: {error}")))?;
        let Some(axes) = metadata.get("sourceToGltfAxes") else {
            continue;
        };
        let axes: [[f64; 3]; 3] = serde_json::from_value(axes.clone())
            .map_err(|error| ferr(format!("Invalid Source axis matrix: {error}")))?;
        for row in 0..3 {
            for other in 0..3 {
                let dot: f64 = (0..3)
                    .map(|column| axes[row][column] * axes[other][column])
                    .sum();
                let expected = if row == other { 1.0 } else { 0.0 };
                if !dot.is_finite() || (dot - expected).abs() > 0.000001 {
                    return Err(ferr(
                        "Source axis metadata must contain an orthonormal matrix",
                    ));
                }
            }
        }
        return Ok(ufbx::Matrix {
            m00: axes[0][0],
            m01: axes[1][0],
            m02: axes[2][0],
            m10: axes[0][1],
            m11: axes[1][1],
            m12: axes[2][1],
            m20: axes[0][2],
            m21: axes[1][2],
            m22: axes[2][2],
            m03: 0.0,
            m13: 0.0,
            m23: 0.0,
        });
    }
    automatic_axis_matrix(ufbx::axes_right_handed_y_up())
}

fn option_vector(value: [f32; 3], opts: &GltfOpts, position: bool) -> [f32; 3] {
    let vector = ufbx::Vec3 {
        x: f64::from(value[0]),
        y: f64::from(value[1]),
        z: f64::from(value[2]),
    };
    let vector = if position {
        opts.point(vector)
    } else {
        opts.direction(vector)
    };
    [vector.x as f32, vector.y as f32, vector.z as f32]
}

/// 读一个 glTF / GLB 文件的几何，转成中立 SMD。
///
/// 返回的 [`GltfGeometry`] 与 [`crate::fbx::read`] 同构 ⟹ 上层 `compile.rs`
/// 两条路径共用一套下游（材质表、shape key → flex、LOD、诊断）。
///
/// # 诊断
///
/// [`GltfNotes`] 里的每一条都在这里 `log::info!` 出去（与 `compile.rs` 的
/// `emit_fbx_diagnostics` 一个路数）。**报错的那几条不在这里** —— data URI
/// 解码失败 / Draco / meshopt 都在读文件那一步就返回 `Err` 了。
///
/// ⚠️ 与 FBX 不同，glTF **没有**「骨骼被过滤」这条诊断：收骨用的是显式的
/// `skin.joints`，没有 FBX 那套「蒙皮簇 + 祖先上溯」的推断，也就没有
/// 「某个节点被丢掉了」这种需要解释的情形。
pub fn read(path: &Path, at: &str, opts: &GltfOpts) -> Result<GltfGeometry, GltfError> {
    let units = import_units(opts);
    let opts = units.as_ref();
    let (doc, buffers, notes) = load_document(path, at)?;
    for line in notes.lines(at) {
        log::info!("{line}");
    }

    // accessor → 字节 的闭包（`Iter::new` 要吃它）。按 `Buffer::index()` 取。
    let get = buffer_getter(&buffers);

    let parent = parent_table(&doc);
    let selectors = part_selectors(&doc, &parent)?;
    let kept = kept_joints(&doc, &parent, opts, &selectors);
    let nodes = smd_nodes(&doc, &parent, &kept);
    let idx_of: HashMap<usize, i32> = kept
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i as i32))
        .collect();
    let world = world_transforms(&doc, &parent);
    let bind = bind_worlds(&doc, get.clone())?;
    let poses = reference_poses(&parent, &world, &bind, &kept, opts);

    let mut triangles: Vec<SmdTriangle> = Vec::new();
    let mut shape_keys: Vec<FbxShapeKey> = Vec::new();
    let mut untextured_meshes: Vec<String> = Vec::new();
    let mut merged_meshes: Vec<String> = Vec::new();
    let material_names =
        stable_source_names(doc.materials().map(|material| material.name()), "material");

    let mut cp_base: u32 = 0;
    let mut cp_done: HashMap<usize, u32> = HashMap::new();

    for (node_ix, mesh_ix) in mesh_instances(&doc, opts, &selectors) {
        let node = doc.nodes().nth(node_ix).expect("下标来自 nodes()");
        let mesh = doc.meshes().nth(mesh_ix).expect("下标来自 meshes()");
        let mesh_name = mesh
            .name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("mesh{mesh_ix}"));
        if !merged_meshes.contains(&mesh_name) {
            merged_meshes.push(mesh_name.clone());
        }
        let bone_ix = idx_of.get(&node_ix).copied();
        let cp_off = if opts.axis == Some(ForcedAxis::Auto) {
            0
        } else {
            *cp_done.entry(mesh_ix).or_insert_with(|| {
                let off = cp_base;
                cp_base += mesh
                    .primitives()
                    .map(|p| p.get(&gltf::Semantic::Positions).map_or(0, |a| a.count()))
                    .max()
                    .unwrap_or(0) as u32;
                off
            })
        };

        let names = morph_target_names(&mesh);
        for prim in mesh.primitives() {
            let primitive_cp_off = if opts.axis == Some(ForcedAxis::Auto) {
                // Transformed instances need independent morph target vertex indices.
                let offset = cp_base;
                cp_base += prim
                    .get(&gltf::Semantic::Positions)
                    .map_or(0, |a| a.count()) as u32;
                offset
            } else {
                cp_off
            };
            let mat = material_name(&prim, opts, &material_names);
            if (mat == FALLBACK_MATERIAL || Some(&mat) == opts.material.as_ref())
                && !untextured_meshes.contains(&mesh_name)
            {
                untextured_meshes.push(mesh_name.clone());
            }
            // ⚠️ 形变目标的名字是**网格级**的（`mesh.extras.targetNames`），
            // 而 morph target 挂在 primitive 上。多 primitive 的网格里每个
            // primitive 各有一份 target 列表，但名字表是共享的。
            let shape_begin = shape_keys.len();
            let triangle_begin = triangles.len();
            shape_keys.extend(shape_keys_of(&prim, &names, primitive_cp_off, get.clone())?);
            push_primitive(
                &prim,
                &node,
                &mat,
                primitive_cp_off,
                bone_ix,
                &idx_of,
                &parent,
                get.clone(),
                &mut triangles,
            )?;
            if opts.axis == Some(ForcedAxis::Auto) && node.skin().is_none() {
                let matrix = world[node_ix];
                let inverse = matrix
                    .invert()
                    .ok_or_else(|| ferr("glTF mesh transform is singular"))?;
                let rotation = matrix.to_ufbx();
                let determinant = rotation.m00
                    * (rotation.m11 * rotation.m22 - rotation.m12 * rotation.m21)
                    - rotation.m01 * (rotation.m10 * rotation.m22 - rotation.m12 * rotation.m20)
                    + rotation.m02 * (rotation.m10 * rotation.m21 - rotation.m11 * rotation.m20);
                for triangle in &mut triangles[triangle_begin..] {
                    for vertex in &mut triangle.vertices {
                        vertex.position = transformed_vector(vertex.position, &matrix, true);
                        vertex.normal = transformed_normal(vertex.normal, &inverse);
                    }
                    if determinant < 0.0 {
                        triangle.vertices.swap(1, 2);
                    }
                }
                let base_normals = prim
                    .get(&gltf::Semantic::Normals)
                    .map(|accessor| read_vec3(&accessor, get.clone()));
                for shape in &mut shape_keys[shape_begin..] {
                    for delta in &mut shape.position_offsets {
                        *delta = transformed_vector(*delta, &matrix, false);
                    }
                    for (index, delta) in shape.vertex_index.iter().zip(&mut shape.normal_offsets) {
                        let base = base_normals
                            .as_ref()
                            .and_then(|normals| normals.get((*index - primitive_cp_off) as usize))
                            .copied()
                            .unwrap_or([0.0; 3]);
                        let morphed = std::array::from_fn(|axis| base[axis] + delta[axis]);
                        let base = transformed_normal(base, &inverse);
                        let morphed = transformed_normal(morphed, &inverse);
                        *delta = std::array::from_fn(|axis| morphed[axis] - base[axis]);
                    }
                }
            }
        }
    }

    if triangles.is_empty() {
        return Err(ferr(format!(
            "{at}：{} 里没有任何三角形（网格）。\
             若写了 `srcpart`，请核对网格名字是否与文件里的一致。",
            path.display()
        )));
    }

    let mut geometry = GltfGeometry {
        smd: Smd {
            version: 1,
            nodes,
            frames: vec![SmdFrame { time: 0, poses }],
            triangles,
        },
        shape_keys,
        anim_stacks: anim_names(&doc),
        untextured_meshes,
        merged_meshes,
    };
    for triangle in &mut geometry.smd.triangles {
        for vertex in &mut triangle.vertices {
            vertex.position = option_vector(vertex.position, opts, true);
            vertex.normal = option_vector(vertex.normal, opts, false);
        }
    }
    for shape in &mut geometry.shape_keys {
        for delta in &mut shape.position_offsets {
            *delta = option_vector(*delta, opts, true);
        }
        for delta in &mut shape.normal_offsets {
            *delta = option_vector(*delta, opts, false);
        }
    }
    if opts.axis == Some(ForcedAxis::Auto) {
        convert_geometry_axes(&mut geometry, &source_axis_matrix(&doc)?);
    }
    Ok(geometry)
}

/// 读一个 glTF / GLB 文件的动画，转成 SMD 帧。
///
/// # 与 FBX 的差别
///
/// FBX 路径让 ufbx 把关键帧**烘焙**成定长采样（`bake_anim` + `resample_rate`），
/// glTF 没有这个设施，要自己按秒轴求值：
///
/// * `input` accessor = **秒**（不是帧号）；
/// * `output` accessor = 关键帧值，个数与 `input` 相同（`CubicSpline` 是 3 倍）；
/// * 采样点 = `time_begin + i / fps`，`i` 从 0 到 `⌊(time_end − time_begin) × fps⌉`；
/// * 通道缺省 = 保持参考姿态（Blender 对「没动的通道」写 `Step` 常量，
///   所以「缺省」与「常量」是两件事，都要处理）。
///
/// ⚠️ 骨骼集合与 [`read`] **完全一致**（同一套收骨），所以帧里的
/// `SmdPose.bone` 可以直接与几何侧的 `nodes` 对齐。
pub fn read_frames(
    path: &Path,
    at: &str,
    stack: Option<&str>,
    fps: f32,
    opts: &GltfOpts,
) -> Result<Smd, GltfError> {
    let units = import_units(opts);
    let opts = units.as_ref();
    let (doc, buffers, notes) = load_document(path, at)?;
    let axes = (opts.axis == Some(ForcedAxis::Auto))
        .then(|| source_axis_matrix(&doc))
        .transpose()?;
    for line in notes.lines(at) {
        log::info!("{line}");
    }

    let parent = parent_table(&doc);
    let selectors = part_selectors(&doc, &parent)?;
    let kept = kept_joints(&doc, &parent, opts, &selectors);
    let nodes = smd_nodes(&doc, &parent, &kept);
    let get = buffer_getter(&buffers);

    let skeleton_parent = skeleton_parents(&parent, &kept);

    // 参考姿态（= 没有动画时的姿态，也是每条通道缺省时的取值）。
    let reference_world = world_transforms(&doc, &parent);
    bind_worlds(&doc, get.clone())?;
    let reference = reference_poses(&parent, &reference_world, &HashMap::new(), &kept, opts);

    let names = anim_names(&doc);
    let anim = match stack {
        None => {
            // ⭐ 多栈而用户没点名时发提示。**与 FBX 不同**：glTF 的栈有名字，
            // 而 `srcstack` 就是按名字选的，所以这条提示只是告知默认值。
            if names.len() > 1 {
                log::info!(
                    "{}",
                    crate::tr_fmt!(
                        "Note: %{path} has %{n} animations %{names}; only the **first** one is used by default. To use another, write `srcstack \"name\"` (inside `$sequence` / `$animation`).",
                        path = path.display(),
                        n = names.len(),
                        names = format!("{:?}", names)
                    )
                );
            }
            doc.animations().next()
        }
        Some(want) => names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(want))
            .and_then(|index| doc.animations().nth(index))
            .or_else(|| {
                doc.animations().enumerate().find_map(|(i, a)| {
                    (format!("animation{i}").eq_ignore_ascii_case(want)).then_some(a)
                })
            }),
    };

    // 没有动画（或点名了不存在的）⟹ 1 帧的参考姿态。
    // ⚠️ 与 FBX 同一条口径：**「文件里没有动画」不是错误**（静态文件本来就是
    // 1 帧），只有**显式点名**了不存在的栈才是错误。
    let Some(anim) = anim else {
        if let Some(want) = stack {
            return Err(ferr(format!(
                "{} 里没有名为 {want:?} 的动画；现有的是 {names:?}。",
                path.display()
            )));
        }
        return Ok(converted_smd(
            Smd {
                version: 1,
                nodes,
                frames: vec![SmdFrame {
                    time: 0,
                    poses: reference,
                }],
                triangles: Vec::new(),
            },
            axes.as_ref(),
        ));
    };

    if !fps.is_finite() {
        return Err(ferr("glTF animation frame rate must be finite"));
    }
    let fps = if fps > 0.0 { fps } else { DEFAULT_FPS };
    let channels = sample_channels(&anim, &get)?;
    // ⭐ 逐帧表情权重 mdlc 不做（表情只由 QC 的 `flex` 控制）。这里只统计条数
    // 用来发一条提示 —— 静默忽略会让用户以为「glTF 里的表情动画生效了」。
    let morph_notes = GltfNotes {
        morph_weight_channels: count_morph_weight_channels(&anim),
        ..GltfNotes::default()
    };
    for line in morph_notes.lines(at) {
        log::info!("{line}");
    }
    let (begin, end) = time_range(&channels);
    let sampled_frames = ((end - begin) * f64::from(fps)).round().max(0.0);
    if !sampled_frames.is_finite() || sampled_frames >= i32::MAX as f64 {
        return Err(ferr("glTF animation has too many sampled frames"));
    }
    let n_frames = sampled_frames as usize + 1;
    if n_frames
        .checked_mul(kept.len().max(1))
        .is_none_or(|poses| poses > 2_000_000)
    {
        return Err(ferr("glTF animation exceeds the sampled pose limit"));
    }

    // 逐帧：从**参考姿态**（节点自己的 TRS）出发，把有通道的节点覆盖掉，
    // 再累乘世界矩阵。没有通道的节点因此天然保持参考姿态 —— 那正是规范
    // 对「某节点没有动画」的要求。
    let base: Vec<LocalTrs> = doc.nodes().map(|n| local_trs(&n)).collect();
    let base_locals: Vec<World> = doc.nodes().map(|node| local_of(&node)).collect();
    let animated_nodes: HashSet<_> = channels.iter().filter_map(|channel| channel.node).collect();
    for channel in &channels {
        if channel.property != gltf::animation::Property::Scale {
            continue;
        }
        let Some(node) = channel.node.filter(|node| kept.contains(node)) else {
            continue;
        };
        let scale = base[node].scale;
        let scale = [scale.x as f32, scale.y as f32, scale.z as f32];
        let ChannelValues::Vec3(values) = &channel.values else {
            unreachable!();
        };
        let unchanged = values.iter().enumerate().all(|(index, value)| {
            let expected = if channel.interp == Interpolation::CubicSpline && index % 3 != 1 {
                [0.0; 3]
            } else {
                scale
            };
            value
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (actual - expected).abs() < 1e-6)
        });
        if !unchanged {
            return Err(ferr(format!(
                "glTF animation '{}' changes scale on '{}'; Source models require bone animation with fixed scale",
                anim.name().unwrap_or("unnamed"),
                nodes[kept.iter().position(|id| *id == node).unwrap()].name
            )));
        }
    }
    let mut samples: Vec<_> = (0..n_frames)
        .map(|frame| (begin + frame as f64 / f64::from(fps), Some(frame)))
        .collect();
    if opts.axis == Some(ForcedAxis::Auto) {
        // Validate source rotation keys between output frames too: inherited
        // shear must not disappear merely because a low frame rate missed it.
        for channel in &channels {
            if channel.property == gltf::animation::Property::Rotation
                && channel.node.is_some_and(|node| kept.contains(&node))
            {
                samples.extend(channel.times.iter().map(|time| (f64::from(*time), None)));
                samples.extend(
                    channel
                        .times
                        .windows(2)
                        .map(|times| ((f64::from(times[0]) + f64::from(times[1])) * 0.5, None)),
                );
            }
        }
        samples.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        samples.dedup_by(|a, b| a.0 == b.0);
        if samples
            .len()
            .checked_mul(kept.len().max(1))
            .is_none_or(|poses| poses > 2_000_000)
        {
            return Err(ferr(
                "glTF animation exceeds the deformation validation limit",
            ));
        }
    }
    let mut frames: Vec<SmdFrame> = Vec::with_capacity(n_frames);
    for (t, output_frame) in samples {
        let mut trs = base.clone();
        for ch in &channels {
            if let Some(node) = ch.node
                && let Some(slot) = trs.get_mut(node)
            {
                ch.apply(t, slot)?;
            }
        }
        let locals: Vec<World> = trs
            .iter()
            .enumerate()
            .map(|(id, transform)| {
                if animated_nodes.contains(&id) {
                    transform.to_world()
                } else {
                    base_locals[id]
                }
            })
            .collect();
        let world = accumulate(&locals, &parent);

        for (i, &id) in kept.iter().enumerate() {
            let current = world[id].to_ufbx();
            if opts.axis == Some(ForcedAxis::Auto)
                && !crate::fbx::affine_motion_supported(&reference_world[id].to_ufbx(), &current)
            {
                return Err(ferr(format!(
                    "glTF animation '{}' changes scale or shear on '{}'; Source models require fixed bone scale. Bake the deformation before exporting",
                    anim.name().unwrap_or("unnamed"),
                    nodes[i].name
                )));
            }
        }
        let Some(fi) = output_frame else { continue };
        let mut poses: Vec<SmdPose> = Vec::with_capacity(kept.len());
        for (i, &id) in kept.iter().enumerate() {
            let current = world[id].to_ufbx();
            let (pos, rotation) = if opts.axis == Some(ForcedAxis::Auto) {
                let parent = skeleton_parent[id].map(|parent| world[parent].to_ufbx());
                crate::fbx::affine_local_pose(&current, parent.as_ref())
            } else {
                let pos = match skeleton_parent[id] {
                    Some(p) => rigid_bone_offset(&world[p], &world[id]),
                    None => world_translation(&world[id]),
                };
                let rotation = if skeleton_parent[id] == parent[id] {
                    trs[id].rotation
                } else {
                    match skeleton_parent[id] {
                        Some(p) => crate::fbx::local_rotation(&world[p].to_ufbx(), &current),
                        None => ufbx::matrix_to_transform(&current).rotation,
                    }
                };
                (pos, rotation)
            };
            let (pos, rot) = opts.local(ufbx::Transform {
                translation: pos,
                rotation,
                scale: ufbx::Vec3 {
                    x: 1.0,
                    y: 1.0,
                    z: 1.0,
                },
            });
            poses.push(SmdPose {
                bone: i as i32,
                position: [pos.x as f32, pos.y as f32, pos.z as f32],
                rotation: crate::fbx::source_euler_from_quat(rot.x, rot.y, rot.z, rot.w),
            });
        }
        frames.push(SmdFrame {
            time: fi as i32,
            poses,
        });
    }

    Ok(converted_smd(
        Smd {
            version: 1,
            nodes,
            frames,
            triangles: Vec::new(),
        },
        axes.as_ref(),
    ))
}

/// 一个节点的局部 TRS（动画逐帧改的就是它）。
///
/// ⚠️ **不 derive `PartialEq`**：`ufbx::Vec3` / `ufbx::Quat` 没实现 `PartialEq`
/// （它们来自 C 绑定，只有 `Debug` / `Clone` / `Copy`）。
#[derive(Debug, Clone, Copy)]
struct LocalTrs {
    translation: ufbx::Vec3,
    rotation: ufbx::Quat,
    scale: ufbx::Vec3,
}

impl LocalTrs {
    fn to_world(self) -> World {
        World::from_trs(
            [
                self.translation.x as f32,
                self.translation.y as f32,
                self.translation.z as f32,
            ],
            [
                self.rotation.x as f32,
                self.rotation.y as f32,
                self.rotation.z as f32,
                self.rotation.w as f32,
            ],
            [
                self.scale.x as f32,
                self.scale.y as f32,
                self.scale.z as f32,
            ],
        )
    }
}

/// 一个节点的局部 TRS（从 `Node::transform()` 取）。
fn local_trs(n: &gltf::Node) -> LocalTrs {
    let (t, r, s) = n.transform().decomposed();
    LocalTrs {
        translation: ufbx::Vec3 {
            x: f64::from(t[0]),
            y: f64::from(t[1]),
            z: f64::from(t[2]),
        },
        rotation: ufbx::Quat {
            x: f64::from(r[0]),
            y: f64::from(r[1]),
            z: f64::from(r[2]),
            w: f64::from(r[3]),
        },
        scale: ufbx::Vec3 {
            x: f64::from(s[0]),
            y: f64::from(s[1]),
            z: f64::from(s[2]),
        },
    }
}

/// 一条动画通道（已经解码成 `f32` 的关键帧序列）。
#[derive(Debug, Clone)]
struct Channel {
    node: Option<usize>,
    /// 关键帧时刻（**秒**），升序。
    times: Vec<f32>,
    interp: Interpolation,
    /// 目标属性。
    ///
    /// ⚠️ **必须存下来**：`ChannelValues::Vec3` 同时承载 `translation` 与
    /// `scale` 两种属性（两者的分量形状一样），靠 `read_outputs()` 的 enum
    /// 臂区分只能知道「是 3 分量」，写回 `LocalTrs` 时还要知道写哪个字段。
    property: gltf::animation::Property,
    /// 关键帧值。
    values: ChannelValues,
}

/// 一条通道的值（按 `Property` 分派）。
#[derive(Debug, Clone)]
enum ChannelValues {
    /// `translation` / `scale`：每个关键帧一个 `[x,y,z]`。
    Vec3(Vec<[f32; 3]>),
    /// `rotation`：每个关键帧一个四元数 `[x,y,z,w]`。
    Quat(Vec<[f32; 4]>),
}

impl Channel {
    fn apply(&self, time: f64, slot: &mut LocalTrs) -> Result<(), GltfError> {
        let time = time as f32;
        let next = self.times.partition_point(|&key| key <= time);
        let key = next.saturating_sub(1).min(self.times.len() - 1);
        let interpolate = self.interp != Interpolation::Step
            && next > 0
            && next < self.times.len()
            && time != self.times[key];
        match &self.values {
            ChannelValues::Vec3(values) => {
                let value = self.sample(values, key, next, time, interpolate);
                if !value.iter().all(|value| value.is_finite()) {
                    return Err(ferr("glTF animation produced a nonfinite vector"));
                }
                let vector = ufbx::Vec3 {
                    x: f64::from(value[0]),
                    y: f64::from(value[1]),
                    z: f64::from(value[2]),
                };
                match self.property {
                    gltf::animation::Property::Translation => slot.translation = vector,
                    gltf::animation::Property::Scale => slot.scale = vector,
                    _ => {}
                }
            }
            ChannelValues::Quat(values) => {
                let value = if interpolate && self.interp == Interpolation::Linear {
                    let amount = (time - self.times[key]) / (self.times[next] - self.times[key]);
                    slerp(
                        normalized_quaternion(values[key])?,
                        normalized_quaternion(values[next])?,
                        amount,
                    )
                } else {
                    self.sample(values, key, next, time, interpolate)
                };
                let value = normalized_quaternion(value)?;
                slot.rotation = ufbx::Quat {
                    x: f64::from(value[0]),
                    y: f64::from(value[1]),
                    z: f64::from(value[2]),
                    w: f64::from(value[3]),
                };
            }
        }
        Ok(())
    }

    fn sample<const N: usize>(
        &self,
        values: &[[f32; N]],
        key: usize,
        next: usize,
        time: f32,
        interpolate: bool,
    ) -> [f32; N] {
        let cubic = self.interp == Interpolation::CubicSpline;
        let index = if cubic { key * 3 + 1 } else { key };
        if !interpolate {
            return values[index];
        }
        let duration = self.times[next] - self.times[key];
        let amount = (time - self.times[key]) / duration;
        if !cubic {
            return std::array::from_fn(|axis| {
                values[key][axis] + amount * (values[next][axis] - values[key][axis])
            });
        }
        let square = amount * amount;
        let cube = square * amount;
        std::array::from_fn(|axis| {
            (2.0 * cube - 3.0 * square + 1.0) * values[index][axis]
                + duration * (cube - 2.0 * square + amount) * values[index + 1][axis]
                + (-2.0 * cube + 3.0 * square) * values[next * 3 + 1][axis]
                + duration * (cube - square) * values[next * 3][axis]
        })
    }
}

fn normalized_quaternion(value: [f32; 4]) -> Result<[f32; 4], GltfError> {
    let length = value
        .iter()
        .map(|&value| f64::from(value).powi(2))
        .sum::<f64>()
        .sqrt();
    if !length.is_finite() || length < 1e-12 {
        return Err(ferr("glTF animation produced an invalid quaternion"));
    }
    Ok(value.map(|value| (f64::from(value) / length) as f32))
}

fn slerp(from: [f32; 4], to: [f32; 4], amount: f32) -> [f32; 4] {
    let dot = from
        .iter()
        .zip(to)
        .map(|(&a, b)| f64::from(a) * f64::from(b))
        .sum::<f64>();
    let sign = if dot < 0.0 { -1.0 } else { 1.0 };
    let angle = dot.abs().min(1.0).acos();
    let (from_weight, to_weight) = if angle < 1e-6 {
        (1.0 - f64::from(amount), f64::from(amount))
    } else {
        (
            ((1.0 - f64::from(amount)) * angle).sin() / angle.sin(),
            (f64::from(amount) * angle).sin() / angle.sin(),
        )
    };
    std::array::from_fn(|axis| {
        (f64::from(from[axis]) * from_weight + sign * f64::from(to[axis]) * to_weight) as f32
    })
}

/// 把一条动画的全部通道解码成 `f32` 关键帧。
fn sample_channels<'a, 's, F>(
    anim: &gltf::Animation<'a>,
    get: &F,
) -> Result<Vec<Channel>, GltfError>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let mut out: Vec<Channel> = Vec::new();
    for ch in anim.channels() {
        let sampler = ch.sampler();
        let interp = sampler.interpolation();
        let Some(times) = gltf::accessor::Iter::<f32>::new(sampler.input(), get.clone()) else {
            return Err(ferr("glTF animation timestamps could not be decoded"));
        };
        let times: Vec<f32> = times.collect();
        if times.is_empty()
            || times.iter().any(|time| !time.is_finite() || *time < 0.0)
            || times.windows(2).any(|pair| pair[0] >= pair[1])
            || (interp == Interpolation::CubicSpline && times.len() < 2)
        {
            return Err(ferr(
                "glTF animation timestamps must be finite and strictly increasing",
            ));
        }
        let values = match ch.reader(get.clone()).read_outputs() {
            Some(gltf::animation::util::ReadOutputs::Translations(it)) => {
                ChannelValues::Vec3(it.collect())
            }
            Some(gltf::animation::util::ReadOutputs::Scales(it)) => {
                ChannelValues::Vec3(it.collect())
            }
            Some(gltf::animation::util::ReadOutputs::Rotations(r)) => {
                ChannelValues::Quat(r.into_f32().collect())
            }
            // 逐帧表情权重：mdlc 不做（见 [`GltfNotes::morph_weight_channels`]）。
            Some(gltf::animation::util::ReadOutputs::MorphTargetWeights(_)) => continue,
            None => return Err(ferr("glTF animation values could not be decoded")),
        };

        let stride = if interp == Interpolation::CubicSpline {
            3
        } else {
            1
        };
        let expected = times
            .len()
            .checked_mul(stride)
            .ok_or_else(|| ferr("glTF animation has too many keys"))?;
        let valid = match &values {
            ChannelValues::Vec3(values) => {
                values.len() == expected && values.iter().flatten().all(|value| value.is_finite())
            }
            ChannelValues::Quat(values) => {
                values.len() == expected
                    && values.iter().flatten().all(|value| value.is_finite())
                    && (0..times.len())
                        .all(|key| normalized_quaternion(values[key * stride + stride / 2]).is_ok())
            }
        };
        if !valid {
            return Err(ferr(
                "glTF animation has invalid values or a mismatched key count",
            ));
        }

        out.push(Channel {
            node: Some(ch.target().node().index()),
            times,
            interp,
            property: ch.target().property(),
            values,
        });
    }
    Ok(out)
}

/// 全部通道的时间范围（秒）。空表给 `(0, 0)`。
fn time_range(channels: &[Channel]) -> (f64, f64) {
    let mut begin = f64::INFINITY;
    let mut end = f64::NEG_INFINITY;
    for ch in channels {
        if let (Some(&t0), Some(&t1)) = (ch.times.first(), ch.times.last()) {
            begin = begin.min(f64::from(t0));
            end = end.max(f64::from(t1));
        }
    }
    if begin.is_finite() && end.is_finite() {
        (begin, end)
    } else {
        (0.0, 0.0)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_skin_influence_sets_are_merged_before_source_weight_reduction() {
        let directory = tmpdir("all-influences");
        let mut bytes = tri_bytes();
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&[0, 1, 2, 3].repeat(3));
        bytes.extend(f32s(&[0.05, 0.1, 0.15, 0.2].repeat(3)));
        bytes.extend_from_slice(&[4, 3, 0, 0].repeat(3));
        bytes.extend(f32s(&[0.35, 0.1, 0.05, 0.0].repeat(3)));
        let path = write_gltf(&directory, "weights.gltf", &bytes, TRI_BODY);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["bufferViews"].as_array_mut().unwrap().extend([
            serde_json::json!({"buffer": 0, "byteOffset": 104, "byteLength": 12}),
            serde_json::json!({"buffer": 0, "byteOffset": 116, "byteLength": 48}),
            serde_json::json!({"buffer": 0, "byteOffset": 164, "byteLength": 12}),
            serde_json::json!({"buffer": 0, "byteOffset": 176, "byteLength": 48}),
        ]);
        document["accessors"].as_array_mut().unwrap().extend([
            serde_json::json!({"bufferView": 4, "componentType": 5121, "count": 3, "type": "VEC4"}),
            serde_json::json!({"bufferView": 5, "componentType": 5126, "count": 3, "type": "VEC4"}),
            serde_json::json!({"bufferView": 6, "componentType": 5121, "count": 3, "type": "VEC4"}),
            serde_json::json!({"bufferView": 7, "componentType": 5126, "count": 3, "type": "VEC4"}),
        ]);
        document["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"] = 4.into();
        document["meshes"][0]["primitives"][0]["attributes"]["WEIGHTS_0"] = 5.into();
        document["meshes"][0]["primitives"][0]["attributes"]["JOINTS_1"] = 6.into();
        document["meshes"][0]["primitives"][0]["attributes"]["WEIGHTS_1"] = 7.into();
        document["nodes"][0]["skin"] = 0.into();
        document["nodes"]
            .as_array_mut()
            .unwrap()
            .extend((0..5).map(|index| serde_json::json!({"name": format!("joint_{index}")})));
        document["skins"] = serde_json::json!([{"joints": [1, 2, 3, 4, 5]}]);
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let geometry = read(&path, "weights.gltf", &GltfOpts::default()).unwrap();
        let links = &geometry.smd.triangles[0].vertices[0].links;
        assert_eq!(
            links.iter().map(|link| link.bone).collect::<Vec<_>>(),
            [4, 3, 2, 1, 0]
        );
        for (link, expected) in links.iter().zip([0.35, 0.3, 0.15, 0.1, 0.1]) {
            assert!((link.weight - expected).abs() < 0.00001);
        }
        let desc = crate::qc::parse_qc_str(
            "$modelname weights.mdl\n$body body weights.gltf\n$sequence idle \"./weights.gltf\"\n",
            &directory,
        )
        .unwrap();
        let compiled = crate::compile::compile(&desc, &directory).unwrap();
        let vertex = &compiled.bodyparts[0].models[0].meshes[0].vertices[0];
        assert_eq!(
            vertex
                .bones
                .iter()
                .map(|bone| bone[0] as i32)
                .collect::<Vec<_>>(),
            [4, 3, 2]
        );
        for (actual, expected) in vertex.bones.iter().zip([0.4375, 0.375, 0.1875]) {
            assert!((actual[1] - expected).abs() < 0.00001);
        }
        document["meshes"][0]["primitives"][0]["attributes"]
            .as_object_mut()
            .unwrap()
            .remove("JOINTS_1");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(read(&path, "weights.gltf", &GltfOpts::default()).is_err());
    }

    #[test]
    fn texture_transform_bakes_selected_uv_set_in_scale_rotate_translate_order() {
        let directory = tmpdir("texture-coordinates");
        let mut bytes = tri_bytes();
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend(f32s(&[0.25, 0.75, 0.5, 0.1, 0.9, 0.2]));
        let path = write_gltf(&directory, "transformed.gltf", &bytes, TRI_BODY);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["bufferViews"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"buffer": 0, "byteOffset": 104, "byteLength": 24}));
        document["accessors"].as_array_mut().unwrap().push(
            serde_json::json!({"bufferView": 4, "componentType": 5126, "count": 3, "type": "VEC2"}),
        );
        document["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_1"] = 4.into();
        document["images"] = serde_json::json!([{"uri": "color.png"}]);
        document["textures"] = serde_json::json!([{"source": 0}]);
        document["extensionsRequired"] = serde_json::json!(["KHR_texture_transform"]);
        document["materials"][0]["pbrMetallicRoughness"] = serde_json::json!({"baseColorTexture": {
            "index": 0, "texCoord": 0,
            "extensions": {"KHR_texture_transform": {"texCoord": 1, "offset": [0.1, 0.2], "scale": [2.0, 3.0], "rotation": std::f32::consts::FRAC_PI_2}}
        }});
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let geometry = read(&path, "transformed.gltf", &GltfOpts::default()).unwrap();
        for (vertex, expected) in
            geometry.smd.triangles[0]
                .vertices
                .iter()
                .zip([[-2.15, 0.7], [-0.2, 1.2], [-0.5, 2.0]])
        {
            assert!(
                near2(vertex.uv, expected),
                "{:?} != {expected:?}",
                vertex.uv
            );
        }
        document["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"] =
            serde_json::json!({"index": 0, "texCoord": 1});
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let geometry = read(&path, "transformed.gltf", &GltfOpts::default()).unwrap();
        assert!(near2(
            geometry.smd.triangles[0].vertices[0].uv,
            [0.25, 0.75]
        ));
        document["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["texCoord"] = 2.into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(read(&path, "transformed.gltf", &GltfOpts::default()).is_err());
    }

    #[test]
    fn normal_only_texture_uses_its_own_uv_transform() {
        let document = gltf::Gltf::from_slice_without_validation(br#"{"asset":{"version":"2.0"},"images":[{"uri":"normal.png"}],"textures":[{"source":0}],"materials":[{"normalTexture":{"index":0,"texCoord":1,"extensions":{"KHR_texture_transform":{"scale":[2,3],"offset":[0.1,0.2]}}}}],"meshes":[{"primitives":[{"attributes":{},"material":0}]}]}"#).unwrap();
        let primitive = document
            .document
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap();
        let coordinates = texture_coordinates(&primitive).unwrap().unwrap();
        assert_eq!(coordinates.set, 1);
        assert!(near2(coordinates.apply([0.25, 0.5]), [0.6, 1.7]));
        assert!(
            TextureCoordinates::from_json(
                &serde_json::json!({"extensions":{"KHR_texture_transform":{"scale":[1]}}})
            )
            .is_err()
        );
        assert!(TextureCoordinates::from_json(&serde_json::json!({"texCoord": -1})).is_err());
    }

    #[test]
    fn missing_normals_are_derived_from_triangle_winding() {
        let directory = tmpdir("flat-normals");
        let mut bytes = tri_bytes();
        for (index, value) in [0.0_f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0]
            .into_iter()
            .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        let body = TRI_BODY.replace("\"NORMAL\": 3, ", "");
        let path = write_gltf(&directory, "flat.gltf", &bytes, &body);
        let geometry = read(&path, "flat.gltf", &GltfOpts::default()).unwrap();
        assert!(
            geometry.smd.triangles[0]
                .vertices
                .iter()
                .all(|vertex| near3(vertex.normal, [-1.0, 0.0, 0.0]))
        );
    }

    #[test]
    fn invalid_mesh_attribute_types_and_index_values_return_errors() {
        let directory = tmpdir("bad-mesh");
        let mut bytes = tri_bytes();
        bytes[100..102].copy_from_slice(&99_u16.to_le_bytes());
        let path = write_gltf(&directory, "bad-index.gltf", &bytes, TRI_BODY);
        assert!(read(&path, "bad-index.gltf", &GltfOpts::default()).is_err());
        let body = TRI_BODY.replace(
            "\"bufferView\": 0, \"componentType\": 5126, \"count\": 3, \"type\": \"VEC3\"",
            "\"bufferView\": 0, \"componentType\": 5126, \"count\": 3, \"type\": \"VEC2\"",
        );
        let path = write_gltf(&directory, "bad-type.gltf", &tri_bytes(), &body);
        assert!(read(&path, "bad-type.gltf", &GltfOpts::default()).is_err());
        let mut bytes = tri_bytes();
        bytes[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        let path = write_gltf(&directory, "bad-position.gltf", &bytes, TRI_BODY);
        assert!(read(&path, "bad-position.gltf", &GltfOpts::default()).is_err());
    }

    #[test]
    fn sparse_morph_indices_and_ranges_are_checked_before_decoding() {
        let directory = tmpdir("sparse-ranges");
        for (indices, count, offset, valid) in [
            ([0, 2], 2, 0, true),
            ([0, 0], 2, 0, false),
            ([2, 0], 2, 0, false),
            ([0, 3], 2, 0, false),
            ([0, 2], 4, 0, false),
            ([0, 2], 2, 1, false),
        ] {
            let mut bytes = tri_bytes();
            bytes.extend_from_slice(&indices);
            bytes.extend(f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 2.0]));
            let path = write_gltf(&directory, "sparse.gltf", &bytes, TRI_BODY);
            let mut document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            document["bufferViews"].as_array_mut().unwrap().extend([
                serde_json::json!({"buffer": 0, "byteOffset": 102, "byteLength": 2}),
                serde_json::json!({"buffer": 0, "byteOffset": 104, "byteLength": 24}),
            ]);
            document["accessors"].as_array_mut().unwrap().push(serde_json::json!({
                "componentType": 5126, "count": 3, "type": "VEC3",
                "sparse": {"count": count, "indices": {"bufferView": 4, "componentType": 5121, "byteOffset": offset}, "values": {"bufferView": 5}}
            }));
            document["meshes"][0]["primitives"][0]["targets"] =
                serde_json::json!([{"POSITION": 4}]);
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            let result = read(&path, "sparse.gltf", &GltfOpts::default());
            assert_eq!(
                result.is_ok(),
                valid,
                "{indices:?}, {count}, {offset}: {result:?}"
            );
            if let Ok(geometry) = result {
                assert_eq!(geometry.shape_keys[0].vertex_index, [0, 2]);
                assert!(near3(
                    geometry.shape_keys[0].position_offsets[1],
                    [0.0, 0.0, 2.0]
                ));
            }
        }
    }

    #[test]
    fn malformed_document_references_and_hierarchies_return_errors() {
        let directory = tmpdir("invalid-structure");
        for malformed in [
            serde_json::json!({"nodes": [{"mesh": 1}]}),
            serde_json::json!({"nodes": [{"children": [1]}]}),
            serde_json::json!({"nodes": [{"children": [0]}]}),
            serde_json::json!({"nodes": [{"children": [1]}, {"children": [0]}]}),
            serde_json::json!({"nodes": [{"children": [2]}, {"children": [2]}, {}]}),
            serde_json::json!({"nodes": [{"children": [1, 1]}, {}]}),
            serde_json::json!({"bufferViews": [{"buffer": 9, "byteLength": 4}]}),
            serde_json::json!({"meshes": [{"primitives": [{"attributes": {"POSITION": 99}}]}]}),
            serde_json::json!({"accessors": [{"componentType": 1, "count": 1, "type": "VEC3"}]}),
            serde_json::json!({"accessors": [{"componentType": 5126, "count": 1000000000_u64, "type": "VEC3"}]}),
            serde_json::json!({"animations": [{"samplers": [], "channels": [{"sampler": 1, "target": {"node": 4, "path": "translation"}}]}]}),
        ] {
            let mut document = malformed;
            document["asset"] = serde_json::json!({"version": "2.0"});
            let path = directory.join("malformed.gltf");
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            assert!(
                read(&path, "malformed.gltf", &GltfOpts::default()).is_err(),
                "{document}"
            );
            assert!(
                animation_names(&path, "malformed.gltf").is_err(),
                "{document}"
            );
        }
    }

    #[test]
    fn missing_and_duplicate_names_remain_unique_without_stealing_declared_names() {
        let names = [
            None,
            Some("node_0"),
            Some("Joint"),
            Some("joint"),
            Some(""),
            Some("NODE_4"),
        ];
        assert_eq!(
            stable_source_names(names.into_iter(), "node"),
            [
                "node_0_1", "node_0", "Joint", "node_3", "node_4_1", "NODE_4"
            ]
        );
        assert_eq!(
            stable_source_names([None, Some("material_0")].into_iter(), "material"),
            ["material_0_1", "material_0"]
        );
    }

    #[test]
    fn generated_node_names_preserve_hierarchy_and_remain_stable_when_filtered() {
        let gltf = gltf::Gltf::from_slice_without_validation(br#"{"asset":{"version":"2.0"},"nodes":[{}, {"name":"node_0"}, {"name":"Joint"}, {"name":"joint"}]}"#).unwrap();
        let parent = [None, Some(0), Some(1), Some(2)];
        let all = smd_nodes(&gltf.document, &parent, &[0, 1, 2, 3]);
        assert_eq!(
            all.iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["node_0_1", "node_0", "Joint", "node_3"]
        );
        assert_eq!(
            all.iter().map(|node| node.parent).collect::<Vec<_>>(),
            [-1, 0, 1, 2]
        );
        let filtered = smd_nodes(&gltf.document, &parent, &[2, 3]);
        assert_eq!(filtered[1].name, all[3].name);
        assert_eq!(filtered[1].parent, 0);
    }
    use std::path::PathBuf;

    // -----------------------------------------------------------------
    // 夹具
    // -----------------------------------------------------------------

    /// 每个用例用**唯一**的临时目录（测试并行跑，共用目录会互相删文件）。
    fn tmpdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("mdlc-gltf-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn u16s(v: &[u16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// 写一个**内嵌 base64 data URI** 的最小 `.gltf`。
    ///
    /// `body` 是 JSON 里 `"buffers"` 之后的内容（`bufferViews` / `accessors` /
    /// `meshes` / `nodes` / 可选 `animations` …），由调用方按需拼。
    ///
    /// ⚠️ 走 data URI 而不是 GLB：那样能顺带把 [`decode_data_uri`] 这条
    /// 「关掉 `import` feature 之后必须自己解」的路径也覆盖掉
    /// （`docs/gltf-support.md` §3.4 —— 不自己解会**静默读出零顶点**）。
    fn write_gltf(dir: &Path, name: &str, bin: &[u8], body: &str) -> PathBuf {
        let json = format!(
            "{{\n  \"asset\": {{\"version\": \"2.0\"}},\n  \
             \"buffers\": [{{\"byteLength\": {}, \
             \"uri\": \"data:application/octet-stream;base64,{}\"}}],\n{}\n}}\n",
            bin.len(),
            base64::encode(bin),
            body
        );
        let p = dir.join(name);
        std::fs::write(&p, json).unwrap();
        p
    }

    /// 一个三角形的几何字节（102 B）：POSITION / NORMAL / TEXCOORD_0 / indices。
    ///
    /// 偏移刻意排成 `0 / 36 / 72 / 96`（全是 4 的倍数 —— glTF 要求 accessor
    /// 的 `byteOffset` 对齐到分量大小）。
    ///
    /// ⚠️ UV 取成**不对称**的（三个角的 V 各不相同），这样「V 有没有被翻过」
    /// 一眼可辨：FBX 路径**要**翻、glTF 路径**不能**翻（见 [`read_uv`] 与
    /// `docs/gltf-support.md` §5.1）。
    fn tri_bytes() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0])); // POSITION
        b.extend(f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0])); // NORMAL
        b.extend(f32s(&[0.0, 1.0, 1.0, 1.0, 0.0, 0.0])); // TEXCOORD_0
        b.extend(u16s(&[0, 1, 2])); // indices
        b
    }

    /// `tri_bytes()` 对应的 `bufferViews` + `accessors` + 材质 + 网格 + 节点。
    ///
    /// 节点名与网格名**故意不同**（`Body.001` vs `hat`），这样 `srcpart`
    /// 的两条匹配路径都能单独验证。
    const TRI_BODY: &str = r#"  "bufferViews": [
    {"buffer": 0, "byteOffset": 0, "byteLength": 36},
    {"buffer": 0, "byteOffset": 36, "byteLength": 36},
    {"buffer": 0, "byteOffset": 72, "byteLength": 24},
    {"buffer": 0, "byteOffset": 96, "byteLength": 6}
  ],
  "accessors": [
    {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
    {"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"},
    {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
    {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}
  ],
  "materials": [{"name": "face"}],
  "meshes": [{"name": "hat", "primitives": [
    {"attributes": {"POSITION": 0, "NORMAL": 3, "TEXCOORD_0": 2}, "indices": 1, "material": 0}
  ]}],
  "nodes": [{"name": "Body.001", "mesh": 0}]"#;

    fn near3(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    fn near_metres(actual: [f32; 3], expected: [f32; 3]) -> bool {
        near3(actual.map(|value| value * 0.0254), expected)
    }

    fn near2(a: [f32; 2], b: [f32; 2]) -> bool {
        (0..2).all(|i| (a[i] - b[i]).abs() < 1e-5)
    }

    /// 往 [`TRI_BODY`] 的 `accessors` 数组**末尾**追加声明（`decl` 可含多行）。
    ///
    /// ⚠️ 必须追加，不能插到开头 —— accessor 的下标就是 `attributes` 里引用的
    /// 编号，插到前面会把整张表挪位，于是 `POSITION` 指向别的东西、`TEXCOORD_0`
    /// 指向索引表……**而且不会报错**，只会静默读出垃圾（第一次写这个夹具时
    /// 就是这么栽的：报的是 `size_of` 不符，看起来像代码 bug）。
    ///
    /// 锚点取「`accessors` 数组的收尾 + 下一节的开头」而不是最后一条记录，
    /// 这样**可以连续追加多次**（锚点在每次追加后依然唯一）。
    fn add_accessor(body: &str, decl: &str) -> String {
        const ANCHOR: &str = "}\n  ],\n  \"materials\": [";
        let with = format!("}},\n    {decl}\n  ],\n  \"materials\": [");
        assert!(
            body.contains(ANCHOR),
            "夹具结构变了：找不到 accessors 数组的结尾"
        );
        body.replacen(ANCHOR, &with, 1)
    }

    /// 往 [`TRI_BODY`] 的 `bufferViews` 数组**末尾**追加一条（可连续调用）。
    fn add_view(body: &str, offset: usize, len: usize) -> String {
        const ANCHOR: &str = "}\n  ],\n  \"accessors\": [";
        let with = format!(
            "}},\n    {{\"buffer\": 0, \"byteOffset\": {offset}, \"byteLength\": {len}}}\n  ],\n  \"accessors\": ["
        );
        assert!(
            body.contains(ANCHOR),
            "夹具结构变了：找不到 bufferViews 数组的结尾"
        );
        body.replacen(ANCHOR, &with, 1)
    }

    /// 把形变目标挂到唯一的那个图元上。
    fn add_target(body: &str, accessor: usize) -> String {
        const OLD: &str = "\"indices\": 1, \"material\": 0}";
        assert!(body.contains(OLD), "夹具结构变了：找不到图元声明");
        body.replace(
            OLD,
            &format!(
                "\"indices\": 1, \"material\": 0, \"targets\": [{{\"POSITION\": {accessor}}}]}}"
            ),
        )
    }

    // -----------------------------------------------------------------
    // 纯函数
    // -----------------------------------------------------------------

    /// `data:` URI 要自己解（关掉 `import` feature 之后 crate 不管了）。
    #[test]
    fn collision_selectors_survive_hidden_scenes_and_missing_custom_properties() {
        let directory = tmpdir("collision-selectors");
        let path = write_gltf(&directory, "model.gltf", &tri_bytes(), TRI_BODY);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["nodes"] = serde_json::json!([
            {"name":"render","mesh":0},
            {"name":"authored hull","mesh":0,"extras":{"gmpublishedCollision":true,"gmpublishedSourceMetadata":"{\"sourceUnits\":\"inches\"}"}},
            {"name":"collision / prop / Piece 1","mesh":0},
            {"name":"ordinary mesh","mesh":0,"extras":{"sourceCollision":false}},
        ]);
        document["scenes"] = serde_json::json!([{"nodes":[0,3]},{"nodes":[1,2]}]);
        document["scene"] = serde_json::json!(0);
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let parts = source_parts(&path, "model").unwrap();
        assert_eq!(
            collision_parts(&path, "model").unwrap(),
            vec![parts[1].selector.clone(), parts[2].selector.clone(),]
        );
        assert_eq!(
            source_metadata(&path, "model").unwrap(),
            vec!["{\"sourceUnits\":\"inches\"}"]
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn blender_empty_scenes_keep_geometry_and_binary_buffers_readable() {
        let directory = tmpdir("empty-scenes");
        let path = write_gltf(&directory, "model.gltf", &tri_bytes(), TRI_BODY);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["scenes"] = serde_json::json!([{"name":"Collision"},{"nodes":[0]}]);
        document["scene"] = serde_json::json!(1);
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(
            read(&path, "model", &GltfOpts::default())
                .unwrap()
                .smd
                .triangles
                .len(),
            1
        );
        document["buffers"][0]
            .as_object_mut()
            .unwrap()
            .remove("uri");
        let json = serde_json::to_vec(&document).unwrap();
        let bytes = tri_bytes();
        let binary = gltf::binary::Glb {
            header: gltf::binary::Header {
                magic: *b"glTF",
                version: 2,
                length: 0,
            },
            json: std::borrow::Cow::Borrowed(&json),
            bin: Some(std::borrow::Cow::Borrowed(&bytes)),
        }
        .to_vec()
        .unwrap();
        let path = directory.join("model.glb");
        std::fs::write(&path, binary).unwrap();
        assert_eq!(
            read(&path, "model", &GltfOpts::default())
                .unwrap()
                .smd
                .triangles
                .len(),
            1
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn world_transforms_handle_deep_child_first_hierarchies_without_recursion() {
        let count = 50_000;
        let locals = vec![World::from_trs([1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [1.0; 3]); count];
        let parents = (0..count)
            .map(|index| (index + 1 < count).then_some(index + 1))
            .collect::<Vec<_>>();
        let world = accumulate(&locals, &parents);
        assert_eq!(world[0].t, [count as f64, 0.0, 0.0]);
        assert_eq!(world[count - 1].t, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn external_buffer_uris_decode_spaces_unicode_and_literal_plus() {
        let directory = tmpdir("external-uri");
        let path = write_gltf(&directory, "model.gltf", &tri_bytes(), TRI_BODY);
        std::fs::write(directory.join("mødel + data.bin"), tri_bytes()).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["buffers"][0]["uri"] = "m%C3%B8del%20+%20data.bin".into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(
            read(&path, "model.gltf", &GltfOpts::default())
                .unwrap()
                .smd
                .triangles
                .len(),
            1
        );
        for uri in ["data%G0.bin", "data%0.bin", "data%.bin", "%FF.bin"] {
            assert!(decode_uri_path(uri).is_err());
        }
        assert!(decode_data_uri(&format!("data:{}", "ø".repeat(80)), "uri").is_err());
    }

    #[test]
    fn decode_data_uri_decodes_base64() {
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            base64::encode([1u8, 2, 3, 4])
        );
        assert_eq!(decode_data_uri(&uri, "t").unwrap(), vec![1, 2, 3, 4]);
    }

    /// 非 base64 的 data URI **报错不猜** —— 猜错会让模型读出零顶点却不报错。
    #[test]
    fn decode_data_uri_rejects_percent_encoding() {
        let err = decode_data_uri("data:application/octet-stream,%01%02", "t").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("base64"),
            "报错应说明只支持 base64 形态：{msg}"
        );
    }

    /// `TriangleStrip` 的**奇数号**三角形要翻转绕序（否则法线朝里）。
    #[test]
    fn triangles_of_flips_odd_strip_winding() {
        let tris = triangles_of(Mode::TriangleStrip, &[0, 1, 2, 3]);
        assert_eq!(tris, vec![[0, 1, 2], [2, 1, 3]]);
    }

    /// `TriangleFan` 以第 0 个顶点为扇心。
    #[test]
    fn triangles_of_expands_a_fan() {
        let tris = triangles_of(Mode::TriangleFan, &[0, 1, 2, 3]);
        assert_eq!(tris, vec![[0, 1, 2], [0, 2, 3]]);
    }

    /// 非三角形图元（点 / 线）静默丢弃 —— 它们进不了 SMD。
    #[test]
    fn triangles_of_drops_non_triangle_modes() {
        assert!(triangles_of(Mode::Points, &[0, 1, 2]).is_empty());
        assert!(triangles_of(Mode::Lines, &[0, 1]).is_empty());
    }

    #[test]
    fn slerp_takes_the_short_path_across_the_sign() {
        // 绕 Z 轴 0° → 180°。`-b` 与 `b` 表示同一个旋转。
        let a = ufbx::Quat {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        };
        let b = ufbx::Quat {
            x: 0.0,
            y: 0.0,
            z: 1.0,
            w: 0.0,
        };
        let neg_b = ufbx::Quat {
            x: 0.0,
            y: 0.0,
            z: -1.0,
            w: 0.0,
        };
        let array = |q: ufbx::Quat| [q.x as f32, q.y as f32, q.z as f32, q.w as f32];
        let p = slerp(array(a), array(b), 0.5);
        let q = slerp(array(a), array(neg_b), 0.5);
        // 两者应给出**同一个**旋转（同一个 z 分量的绝对值），而不是相反方向。
        assert!(
            (p[2].abs() - q[2].abs()).abs() < 1e-9,
            "同一旋转的两种四元数表示应给出同一条路径：{p:?} vs {q:?}"
        );
        assert!(p[2] > 0.5, "90° 的四元数 z 分量应显著非零：{p:?}");
    }

    /// `World::from_trs` 用 glTF 的**列主序**约定，且 `T·R·S`。
    #[test]
    fn world_from_trs_uses_the_gltf_column_convention() {
        // 绕 Z 轴 +90°：x 轴应转到 y 轴。
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let w = World::from_trs([1.0, 2.0, 3.0], [0.0, 0.0, s, s], [1.0, 1.0, 1.0]);
        // 第 0 列 = 变换后的 x 轴 ≈ (0, 1, 0)。
        assert!(
            (w.m[0][0]).abs() < 1e-6 && (w.m[0][1] - 1.0).abs() < 1e-6,
            "第 0 列应是 x 轴转 90° 后的像：{:?}",
            w.m[0]
        );
        assert_eq!(w.t, [1.0, 2.0, 3.0]);
    }

    /// `World::invert` 是 4×4 仿射的逆（3×3 余子式 + `t' = -A⁻¹·t`）。
    #[test]
    fn invert_round_trips_an_affine_matrix() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let w = World::from_trs([1.0, 2.0, 3.0], [0.0, 0.0, s, s], [2.0, 2.0, 2.0]);
        let i = w.invert().expect("有缩放但不是奇异的，应能求逆");
        let id = w.mul(&i);
        // `W · W⁻¹` 应是单位阵（3×3 部分）与零平移。
        for (c, col) in id.m.iter().enumerate() {
            for (r, v) in col.iter().enumerate() {
                let want = if c == r { 1.0 } else { 0.0 };
                assert!(
                    (v - want).abs() < 1e-9,
                    "W·W⁻¹ 的第 {c} 列第 {r} 行应是 {want}：{v}"
                );
            }
        }
        for (r, v) in id.t.iter().enumerate() {
            assert!(v.abs() < 1e-9, "W·W⁻¹ 的平移第 {r} 位应是 0：{v}");
        }

        // 奇异矩阵（0 缩放）返回 `None`，不能吐 inf/NaN。
        let flat = World::from_trs([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], [0.0, 1.0, 1.0]);
        assert!(
            flat.invert().is_none(),
            "0 缩放的矩阵是奇异的，必须返回 None"
        );
    }

    #[test]
    fn accumulate_computes_children_before_parents() {
        let local = |y: f64| World {
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            t: [0.0, y, 0.0],
        };
        // 下标 0 = tip（父是 1），1 = mid（父是 2），2 = root（无父）。
        let locals = vec![local(5.0), local(10.0), local(0.0)];
        let parent = vec![Some(1), Some(2), None];
        let world = accumulate(&locals, &parent);
        assert!((world[2].t[1] - 0.0).abs() < 1e-9);
        assert!(
            (world[1].t[1] - 10.0).abs() < 1e-9,
            "mid 的世界 y 应是 10（父 root 在原点）：{:?}",
            world[1].t
        );
        assert!(
            (world[0].t[1] - 15.0).abs() < 1e-9,
            "tip 的世界 y 应是 10+5=15（层序下必须等父先算出来）：{:?}",
            world[0].t
        );
    }

    /// 环（规范不允许）不能让 `accumulate` 死循环。
    #[test]
    fn accumulate_breaks_a_cycle_instead_of_hanging() {
        let locals = vec![World::identity(), World::identity()];
        let parent = vec![Some(1), Some(0)];
        let world = accumulate(&locals, &parent);
        assert_eq!(world.len(), 2, "环上的节点退回单位阵，但不能卡住");
    }

    /// ⭐ `JOINTS_0` 是 **`skin.joints` 数组的下标**，不是节点下标。
    ///
    /// 夹具用 Blender 的真实形态（`skin.joints = [2, 1, 0]`，实测
    /// `docs/_probe/gltf_joints.js`）：`JOINTS_0 = 0` 指的是
    /// **节点 2**（`skin.joints[0]`），不是节点 0。
    #[test]
    fn weights_map_skin_joint_indices_through_the_node_table() {
        let joint_to_node = [2usize, 1, 0];
        let idx_of: HashMap<usize, i32> = [(0usize, 0i32), (1, 1), (2, 2)].into_iter().collect();

        let j = [0u16, 2, 3, 3];
        let w = [1.0f32, 0.0, 0.0, 0.0];
        let links = weights_of(Some(&j), Some(&w), &joint_to_node, &idx_of);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].bone, 2,
            "JOINTS_0 = 0 应映射到节点 2（skin.joints[0]），而不是节点 0"
        );

        // 权重为 0 的分量、以及越界的 joint 下标都要静默跳过。
        let j = [0u16, 1, 9, 9];
        let w = [0.25f32, 0.75, 0.0, 0.0];
        let links = weights_of(Some(&j), Some(&w), &joint_to_node, &idx_of);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].bone, 1, "权重降序：0.75 在前");
        assert_eq!(links[1].bone, 2);

        // 没有 JOINTS_0 / WEIGHTS_0（静态网格）时给空表，由调用方补兜底。
        assert!(weights_of(None, Some(&w), &joint_to_node, &idx_of).is_empty());
    }

    /// `Step` 保持上一个关键帧的值（不插值）。
    #[test]
    fn channel_step_holds_the_previous_key() {
        let ch = Channel {
            node: Some(0),
            times: vec![0.0, 1.0, 2.0],
            interp: Interpolation::Step,
            property: gltf::animation::Property::Translation,
            values: ChannelValues::Vec3(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]),
        };
        let mut slot = LocalTrs {
            translation: ufbx::Vec3 {
                x: 9.0,
                y: 9.0,
                z: 9.0,
            },
            rotation: ufbx::Quat {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 1.0,
            },
            scale: ufbx::Vec3 {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            },
        };
        ch.apply(1.5, &mut slot).unwrap();
        assert!(
            (slot.translation.x - 1.0).abs() < 1e-6,
            "Step 应停在 t=1 那一帧：{:?}",
            slot.translation
        );
    }

    /// `Linear` 在相邻关键帧之间插值。
    #[test]
    fn channel_linear_interpolates_between_keys() {
        let ch = Channel {
            node: Some(0),
            times: vec![0.0, 2.0],
            interp: Interpolation::Linear,
            property: gltf::animation::Property::Translation,
            values: ChannelValues::Vec3(vec![[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]]),
        };
        let mut slot = LocalTrs {
            translation: ufbx::Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            rotation: ufbx::Quat {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                w: 1.0,
            },
            scale: ufbx::Vec3 {
                x: 1.0,
                y: 1.0,
                z: 1.0,
            },
        };
        ch.apply(1.0, &mut slot).unwrap();
        assert!(
            (slot.translation.x - 2.0).abs() < 1e-6,
            "t=1 是 0→4 的中点：{:?}",
            slot.translation
        );
        // 超出末端要夹住，不能越界读。
        ch.apply(99.0, &mut slot).unwrap();
        assert!((slot.translation.x - 4.0).abs() < 1e-6);
        ch.apply(-99.0, &mut slot).unwrap();
        assert!((slot.translation.x - 0.0).abs() < 1e-6);
    }

    /// `time_range` 取全部通道的并集。
    #[test]
    fn time_range_spans_all_channels() {
        let mk = |t0: f32, t1: f32| Channel {
            node: Some(0),
            times: vec![t0, t1],
            interp: Interpolation::Linear,
            property: gltf::animation::Property::Translation,
            values: ChannelValues::Vec3(vec![[0.0; 3], [0.0; 3]]),
        };
        let (b, e) = time_range(&[mk(0.0, 1.0), mk(0.5, 2.0)]);
        assert!((b - 0.0).abs() < 1e-9 && (e - 2.0).abs() < 1e-9);
        assert_eq!(time_range(&[]), (0.0, 0.0), "空表不能给出 ±inf");
    }

    // -----------------------------------------------------------------
    // 端到端（`read`）
    // -----------------------------------------------------------------

    #[test]
    fn automatic_axes_normalize_geometry_reference_and_animation_together() {
        let directory = tmpdir("automatic-axes");
        let path = anim_fixture(&directory);
        let opts = GltfOpts {
            axis: Some(ForcedAxis::Auto),
            scale: 2.0,
            ..Default::default()
        };
        let geometry = read(&path, "anim.gltf", &opts).unwrap();
        let animation = read_frames(&path, "anim.gltf", None, 30.0, &opts).unwrap();
        assert_eq!(geometry.smd.nodes, animation.nodes);
        assert_eq!(geometry.smd.frames[0].poses, animation.frames[0].poses);
        let vertices = &geometry.smd.triangles[0].vertices;
        assert!(near_metres(vertices[2].position, [0.0, 0.0, 2.0]));
        assert!(near3(vertices[0].normal, [0.0, -1.0, 0.0]));
        let matrix = crate::bone_math::angle_matrix(animation.frames[30].poses[0].rotation);
        assert!(near3([matrix[0], matrix[4], matrix[8]], [0.0, 0.0, 1.0]));
    }

    #[test]
    fn scaled_meshes_preserve_translation_geometry_normals_and_morph_deltas() {
        let directory = tmpdir("scaled-mesh");
        let mut bytes = tri_bytes();
        bytes[36..72].copy_from_slice(&f32s(&[1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0]));
        bytes.extend(f32s(&[1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
        bytes.extend(f32s(&[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
        let body = add_target(
            &add_view(
                &add_accessor(
                    TRI_BODY,
                    "{\"bufferView\":4,\"componentType\":5126,\"count\":3,\"type\":\"VEC3\"}",
                ),
                102,
                36,
            ),
            4,
        );
        let body = add_view(
            &add_accessor(
                &body,
                "{\"bufferView\":5,\"componentType\":5126,\"count\":3,\"type\":\"VEC3\"}",
            ),
            138,
            36,
        );
        let path = write_gltf(&directory, "scaled.gltf", &bytes, &body);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["nodes"][0]["translation"] = serde_json::json!([10, 20, 30]);
        document["nodes"][0]["scale"] = serde_json::json!([2, 3, 4]);
        document["meshes"][0]["primitives"][0]["targets"][0]["NORMAL"] = 5.into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let opts = GltfOpts {
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        };
        let geometry = read(&path, "scaled.gltf", &opts).unwrap();
        assert!(near_metres(
            geometry.smd.frames[0].poses[0].position,
            [10.0, -30.0, 20.0]
        ));
        assert!(near_metres(
            geometry.smd.triangles[0].vertices[1].position,
            [12.0, -30.0, 20.0]
        ));
        assert!(near_metres(
            geometry.smd.triangles[0].vertices[2].position,
            [10.0, -30.0, 23.0]
        ));
        let length = 13.0_f32.sqrt();
        assert!(near3(
            geometry.smd.triangles[0].vertices[0].normal,
            [3.0 / length, 0.0, 2.0 / length]
        ));
        assert!(near_metres(
            geometry.shape_keys[0].position_offsets[0],
            [2.0, 0.0, 3.0]
        ));
        assert!(near3(
            geometry.shape_keys[0].normal_offsets[0],
            [0.6 - 3.0 / length, 0.0, 0.8 - 2.0 / length]
        ));
        let animation = read_frames(&path, "scaled.gltf", None, 30.0, &opts).unwrap();
        assert_eq!(animation.frames[0].poses, geometry.smd.frames[0].poses);

        document["meshes"][0]["primitives"][0]["targets"][0]
            .as_object_mut()
            .unwrap()
            .remove("POSITION");
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let geometry = read(&path, "scaled.gltf", &opts).unwrap();
        assert_eq!(geometry.shape_keys[0].position_offsets, [[0.0; 3]]);
        assert!(geometry.shape_keys[0].normal_offsets[0][2] > 0.2);
    }

    #[test]
    fn inverse_bind_matrices_reject_invalid_data_and_conflicting_skin_bindings() {
        let document = gltf::Gltf::from_slice_without_validation(br#"{"asset":{"version":"2.0"},"nodes":[{"name":"joint"}],"skins":[{"joints":[0]},{"joints":[0]}]}"#).unwrap();
        assert_eq!(
            bind_worlds(&document.document, |_| None).unwrap()[&0],
            World::identity()
        );
        let directory = tmpdir("bad-bind-matrices");
        let mut bytes = tri_bytes();
        bytes.extend(f32s(&[
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -20.0, 0.0, 1.0,
        ]));
        let body = add_view(
            &add_accessor(
                TRI_BODY,
                "{\"bufferView\":4,\"componentType\":5126,\"count\":1,\"type\":\"MAT4\"}",
            ),
            102,
            64,
        );
        let path = write_gltf(&directory, "bind.gltf", &bytes, &body);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        document["nodes"][0]["skin"] = 0.into();
        document["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"name":"joint"}));
        document["skins"] =
            serde_json::json!([{"joints":[1],"inverseBindMatrices":4},{"joints":[1]}]);
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            read(&path, "bind.gltf", &GltfOpts::default())
                .unwrap_err()
                .to_string()
                .contains("different bind poses")
        );
        document["skins"].as_array_mut().unwrap().pop();
        for (offset, value, expected) in [
            (102, 0.0, "singular"),
            (102, f32::NAN, "finite"),
            (114, 1.0, "affine"),
        ] {
            let mut bad = bytes.clone();
            bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            document["buffers"][0]["uri"] = format!(
                "data:application/octet-stream;base64,{}",
                base64::encode(bad)
            )
            .into();
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            assert!(
                read(&path, "bind.gltf", &GltfOpts::default())
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
        }
    }

    #[test]
    fn automatic_axes_keep_unskinned_node_rotation_and_reflection() {
        let directory = tmpdir("automatic-node-transform");
        let body = TRI_BODY.replace(
            "\"mesh\": 0",
            "\"mesh\": 0, \"rotation\": [0.70710678, 0, 0, 0.70710678]",
        );
        let path = write_gltf(&directory, "rotated.gltf", &tri_bytes(), &body);
        let opts = GltfOpts {
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        };
        let geometry = read(&path, "rotated.gltf", &opts).unwrap();
        assert!(near_metres(
            geometry.smd.triangles[0].vertices[2].position,
            [0.0, -1.0, 0.0]
        ));
        assert!(near3(
            geometry.smd.triangles[0].vertices[0].normal,
            [0.0, 0.0, -1.0]
        ));
        let body = TRI_BODY.replace("\"mesh\": 0", "\"mesh\": 0, \"scale\": [-1, 1, 1]");
        let path = write_gltf(&directory, "reflected.gltf", &tri_bytes(), &body);
        let geometry = read(&path, "reflected.gltf", &opts).unwrap();
        let triangle = &geometry.smd.triangles[0];
        assert_eq!(
            triangle.vertices.each_ref().map(|vertex| vertex.src_index),
            [0, 2, 1]
        );
        assert!(near_metres(triangle.vertices[2].position, [-1.0, 0.0, 0.0]));
    }

    #[test]
    fn automatic_axes_keep_instanced_morph_targets_separate() {
        let directory = tmpdir("automatic-instanced-morph");
        let mut bytes = tri_bytes();
        bytes.extend(f32s(&[0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0]));
        let body = add_target(&add_view(&add_accessor(TRI_BODY,
            "{\"bufferView\":4,\"componentType\":5126,\"count\":3,\"type\":\"VEC3\"}"), 102, 36), 4)
            .replace("\"nodes\": [{\"name\": \"Body.001\", \"mesh\": 0}]",
                "\"nodes\": [{\"name\": \"first\", \"mesh\":0},{\"name\":\"second\",\"mesh\":0,\"rotation\":[0.70710678,0,0,0.70710678]}]");
        let path = write_gltf(&directory, "instanced.gltf", &bytes, &body);
        let geometry = read(
            &path,
            "instanced.gltf",
            &GltfOpts {
                axis: Some(ForcedAxis::Auto),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(geometry.shape_keys[0].vertex_index, [0, 1, 2]);
        assert_eq!(geometry.shape_keys[1].vertex_index, [3, 4, 5]);
        assert!(near_metres(
            geometry.shape_keys[0].position_offsets[0],
            [0.0, 0.0, 1.0]
        ));
        assert!(near_metres(
            geometry.shape_keys[1].position_offsets[0],
            [0.0, -1.0, 0.0]
        ));
        assert_eq!(geometry.smd.triangles[1].vertices[0].src_index, 3);
    }

    #[test]
    fn automatic_axes_transform_reference_poses_and_morph_deltas() {
        let directory = tmpdir("automatic-bind-morph");
        let mut bytes = tri_bytes();
        bytes.extend(f32s(&[0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0]));
        let body = add_target(
            &add_view(
                &add_accessor(
                    TRI_BODY,
                    "{\"bufferView\":4,\"componentType\":5126,\"count\":3,\"type\":\"VEC3\"}",
                ),
                102,
                36,
            ),
            4,
        )
        .replace("\"mesh\": 0", "\"mesh\": 0, \"translation\": [0, 3, 0]");
        let path = write_gltf(&directory, "morph.gltf", &bytes, &body);
        let opts = GltfOpts {
            axis: Some(ForcedAxis::Auto),
            scale: 2.0,
            ..Default::default()
        };
        let geometry = read(&path, "morph.gltf", &opts).unwrap();
        assert!(near_metres(
            geometry.smd.frames[0].poses[0].position,
            [0.0, 0.0, 6.0]
        ));
        assert!(near_metres(
            geometry.smd.triangles[0].vertices[0].position,
            [0.0, 0.0, 6.0]
        ));
        assert!(near_metres(
            geometry.smd.triangles[0].vertices[2].position,
            [0.0, 0.0, 8.0]
        ));
        assert!(near_metres(
            geometry.shape_keys[0].position_offsets[0],
            [0.0, 0.0, 2.0]
        ));
        let animation = read_frames(&path, "morph.gltf", None, 30.0, &opts).unwrap();
        assert_eq!(geometry.smd.frames, animation.frames);
    }

    /// 最小三角形：几何 / 节点表 / 材质名都要对。
    #[test]
    fn read_builds_geometry_from_an_inline_gltf() {
        let d = tmpdir("tri");
        let p = write_gltf(&d, "tri.gltf", &tri_bytes(), TRI_BODY);
        let g = read(&p, "tri.gltf", &GltfOpts::default()).expect("应能读出三角形");

        assert_eq!(g.smd.nodes.len(), 1);
        assert_eq!(g.smd.nodes[0].name, "Body.001");
        assert_eq!(g.smd.nodes[0].parent, -1, "根节点的 parent 是 -1");
        assert_eq!(g.smd.frames.len(), 1, "几何路径只产一帧参考姿态");

        assert_eq!(g.smd.triangles.len(), 1);
        let t = &g.smd.triangles[0];
        assert_eq!(t.material, "face", "材质名要原样进表");
        assert!(near3(t.vertices[0].position, [0.0, 0.0, 0.0]));
        assert!(near3(t.vertices[1].position, [1.0, 0.0, 0.0]));
        assert!(near3(t.vertices[2].position, [0.0, 1.0, 0.0]));
        assert!(near3(t.vertices[0].normal, [0.0, 0.0, 1.0]));

        // 没有 `skin` 的静态网格：顶点绑到它所在的那个网格节点。
        assert_eq!(t.vertices[0].links.len(), 1);
        assert_eq!(t.vertices[0].links[0].bone, 0);
        assert_eq!(t.vertices[0].parent_bone, 0);

        assert!(g.merged_meshes.contains(&"hat".to_string()));
        assert!(
            g.untextured_meshes.is_empty(),
            "写了材质就不该进「无材质」清单"
        );
    }

    /// ⭐⭐⭐ **参考姿态取绑定姿态，不是节点 rest 姿态。**
    ///
    /// 与 FBX 侧的 `bind_worlds` 是同一条口径（`docs/gltf-support.md` §5.1b）：
    /// Blender 把 `Use Rest Position Armature` 关掉时，节点的 TRS 写的是
    /// **当前帧姿态**，而 `inverseBindMatrices` 仍然是 rest ⟹ 顶点（写在绑定
    /// 姿态空间）与骨骼参考姿态分属两个空间，武器/道具骨骼整片错位。
    ///
    /// 夹具刻意让两者**不同**：节点 1 的 TRS 说它在 `(0,10,0)`，而 IBM 说
    /// 绑定姿态在 `(0,20,0)`。取错了就会读到 10。
    #[test]
    fn reference_pose_comes_from_inverse_bind_matrices() {
        let d = tmpdir("ibm");
        let mut bin = tri_bytes();
        // MAT4 是 64 B、**列主序**：平移列 = `(0, -20, 0)` ⟹ 绑定姿态在 `(0, 20, 0)`。
        bin.extend(f32s(&[
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, -20.0, 0.0, 1.0,
        ]));
        // 几何 102 B ⟹ IBM 落在 102，占 64 B。
        let body = add_accessor(
            TRI_BODY,
            "{\"bufferView\": 4, \"componentType\": 5126, \"count\": 1, \"type\": \"MAT4\"}",
        );
        let body = add_view(&body, 102, 64);
        let body = body.replace(
            "\"nodes\": [{\"name\": \"Body.001\", \"mesh\": 0}]",
            "\"nodes\": [\
             {\"name\": \"Body.001\", \"mesh\": 0, \"skin\": 0}, \
             {\"name\": \"Joint\", \"translation\": [0.0, 10.0, 0.0]}],\n  \
             \"skins\": [{\"joints\": [1], \"inverseBindMatrices\": 4}]",
        );
        let p = write_gltf(&d, "ibm.gltf", &bin, &body);
        let g = read(&p, "ibm.gltf", &GltfOpts::default()).expect("应能读出蒙皮网格");

        assert_eq!(g.smd.nodes.len(), 1, "只有 `skin.joints` 里那根被收进来");
        assert_eq!(g.smd.nodes[0].name, "Joint");
        let pos = g.smd.frames[0].poses[0].position;
        assert!(
            near3(pos, [0.0, 20.0, 0.0]),
            "参考姿态要取 IBM⁻¹ 的平移列 (0,20,0)；取成节点 TRS 就会是 (0,10,0)：{pos:?}"
        );
        let opts = GltfOpts {
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        };
        let normalized = read(&p, "ibm.gltf", &opts).unwrap();
        assert!(near_metres(
            normalized.smd.frames[0].poses[0].position,
            [0.0, 0.0, 20.0]
        ));
        assert!(near_metres(
            normalized.smd.triangles[0].vertices[2].position,
            [0.0, 0.0, 1.0]
        ));
        let frames = read_frames(&p, "ibm.gltf", None, 30.0, &opts).unwrap();
        assert!(near_metres(
            frames.frames[0].poses[0].position,
            [0.0, 0.0, 10.0]
        ));
    }

    #[test]
    fn missing_inverse_bind_matrices_use_identity_bind_pose() {
        let d = tmpdir("noibm");
        let body = TRI_BODY.replace(
            "\"nodes\": [{\"name\": \"Body.001\", \"mesh\": 0}]",
            "\"nodes\": [\
             {\"name\": \"Body.001\", \"mesh\": 0, \"skin\": 0}, \
             {\"name\": \"Joint\", \"translation\": [0.0, 10.0, 0.0]}],\n  \
             \"skins\": [{\"joints\": [1]}]",
        );
        let p = write_gltf(&d, "noibm.gltf", &tri_bytes(), &body);
        let g = read(&p, "noibm.gltf", &GltfOpts::default()).expect("应能读出蒙皮网格");

        assert_eq!(g.smd.nodes.len(), 1);
        let pos = g.smd.frames[0].poses[0].position;
        assert!(
            near3(pos, [0.0, 0.0, 0.0]),
            "Missing inverse bind matrices must use identity: {pos:?}"
        );
        let animation = read_frames(&p, "noibm.gltf", None, 30.0, &GltfOpts::default()).unwrap();
        assert!(near3(
            animation.frames[0].poses[0].position,
            [0.0, 10.0, 0.0]
        ));
    }

    /// ⭐ **glTF 路径不翻 V** —— 与 FBX 相反。
    ///
    /// 这是最容易被「照抄 FBX 那边」带错的一处：FBX 的 `geometry_normal` /
    /// UV 路径要 `1.0 - v`，而 SMD 解析器**自己**翻过一次；glTF 不经过
    /// SMD 解析器，所以**必须不翻**（`docs/gltf-support.md` §5.1）。
    #[test]
    fn read_uv_is_not_flipped() {
        let d = tmpdir("uv");
        let p = write_gltf(&d, "tri.gltf", &tri_bytes(), TRI_BODY);
        let g = read(&p, "tri.gltf", &GltfOpts::default()).unwrap();
        let v = &g.smd.triangles[0].vertices;
        assert!(
            near2(v[0].uv, [0.0, 1.0]),
            "顶点 0 的 UV 应原样是 (0,1)；翻过就成 (0,0) 了：{:?}",
            v[0].uv
        );
        assert!(near2(v[1].uv, [1.0, 1.0]));
        assert!(near2(v[2].uv, [0.0, 0.0]));
    }

    /// `srcpart` 两条匹配路径都要通：**节点名**与**网格名**。
    ///
    /// 夹具的节点叫 `Body.001`、网格叫 `hat` —— 用户看到的「部件」多半是
    /// 网格名，而节点名可能是 Blender 自动加的后缀。
    #[test]
    fn srcpart_matches_either_the_node_name_or_the_mesh_name() {
        let d = tmpdir("part");
        let p = write_gltf(&d, "tri.gltf", &tri_bytes(), TRI_BODY);

        for part in ["hat", "Body.001"] {
            let opts = GltfOpts {
                parts: vec![part.to_string()],
                ..Default::default()
            };
            let g = read(&p, "tri.gltf", &opts)
                .unwrap_or_else(|e| panic!("srcpart {part:?} 应能选中：{e}"));
            assert_eq!(g.smd.triangles.len(), 1, "srcpart {part:?}");
        }

        let opts = GltfOpts {
            parts: vec!["nope".to_string()],
            ..Default::default()
        };
        let err = read(&p, "tri.gltf", &opts).unwrap_err();
        assert!(
            err.to_string().contains("没有任何三角形"),
            "选不中时要报错而不是静默出空模型：{err}"
        );
    }

    /// 没写材质的网格要落到 `debug/debugempty` 并进「无材质」清单。
    #[test]
    fn read_falls_back_to_the_empty_material() {
        let d = tmpdir("nomat");
        let body = TRI_BODY.replace("\"material\": 0", "\"material\": -1");
        // `material: -1` 不是合法的 glTF 索引，所以改成整个删掉 `material`。
        let body = body.replace(", \"material\": -1", "");
        let p = write_gltf(&d, "tri.gltf", &tri_bytes(), &body);
        let g = read(&p, "tri.gltf", &GltfOpts::default()).unwrap();
        assert_eq!(g.smd.triangles[0].material, FALLBACK_MATERIAL);
        assert_eq!(g.untextured_meshes, vec!["hat".to_string()]);
    }

    /// `srcmaterial` 显式兜底名要盖过 `debug/debugempty`。
    #[test]
    fn srcmaterial_overrides_the_fallback_name() {
        let d = tmpdir("mat");
        let body = TRI_BODY.replace(", \"material\": 0", "");
        let p = write_gltf(&d, "tri.gltf", &tri_bytes(), &body);
        let opts = GltfOpts {
            material: Some("my/fallback".to_string()),
            ..Default::default()
        };
        let g = read(&p, "tri.gltf", &opts).unwrap();
        assert_eq!(g.smd.triangles[0].material, "my/fallback");
    }

    /// ⭐ morph target 要变成 shape key，名字走 `extras.targetNames`
    /// （glTF 2.0 没有这个字段，是**事实标准**）。
    #[test]
    fn read_registers_morph_targets_as_shape_keys() {
        let d = tmpdir("morph");
        let mut bin = tri_bytes();
        // 形变偏移：三个顶点各 +1 在 z 上。
        bin.extend(f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        let body = add_target(
            &add_view(
                &add_accessor(
                    TRI_BODY,
                    "{\"bufferView\": 4, \"componentType\": 5126, \"count\": 3, \"type\": \"VEC3\"}",
                ),
                102,
                36,
            ),
            4,
        );
        // `extras` 挂在网格上（不是图元上）。
        let body = body.replace(
            "\"meshes\": [{\"name\": \"hat\"",
            "\"meshes\": [{\"name\": \"hat\", \"extras\": {\"targetNames\": [\"wide\"]}",
        );
        let p = write_gltf(&d, "morph.gltf", &bin, &body);
        let g = read(&p, "morph.gltf", &GltfOpts::default()).unwrap();

        assert_eq!(
            g.shape_keys.len(),
            1,
            "应有一个 shape key：{:?}",
            g.shape_keys
        );
        let k = &g.shape_keys[0];
        assert_eq!(k.name, "wide", "名字应来自 extras.targetNames");
        assert_eq!(k.vertex_index, vec![0, 1, 2]);
        for (i, off) in k.position_offsets.iter().enumerate() {
            assert!(near3(*off, [0.0, 0.0, 1.0]), "第 {i} 个偏移：{off:?}");
        }
        // 法线偏移官方在 FBX 路径就丢弃；glTF 这边照读但下游不用。
        // 夹具没给形变法线 ⟹ 逐顶点补零（长度与 `vertex_index` 对齐）。
        assert_eq!(k.normal_offsets.len(), k.vertex_index.len());
        for n in &k.normal_offsets {
            assert!(near3(*n, [0.0, 0.0, 0.0]), "没给形变法线时应是零：{n:?}");
        }
    }

    /// 形变偏移**全零**的目标要跳过（`0.001²` 阈值，与 `simplify.cpp:2537` 同口径）。
    #[test]
    fn shape_keys_skip_all_zero_offsets() {
        let d = tmpdir("morphzero");
        let mut bin = tri_bytes();
        bin.extend(f32s(&[0.0; 9]));
        let body = add_target(
            &add_view(
                &add_accessor(
                    TRI_BODY,
                    "{\"bufferView\": 4, \"componentType\": 5126, \"count\": 3, \"type\": \"VEC3\"}",
                ),
                102,
                36,
            ),
            4,
        );
        let p = write_gltf(&d, "morphzero.gltf", &bin, &body);
        let g = read(&p, "morphzero.gltf", &GltfOpts::default()).unwrap();
        assert!(
            g.shape_keys.is_empty(),
            "全零偏移的 target 不该产出 flex：{:?}",
            g.shape_keys
        );
    }

    /// ⭐ **无 `bufferView` 的 accessor**（规范 = 全零）要走得通 ——
    /// 这是 `from_slice_without_validation` 存在的**唯一理由**
    /// （`docs/gltf-support.md` §3.2：Blender 对「只改位置不改法线」的 morph
    /// target 正是这么写的，用带校验的路径会直接编不出来）。
    ///
    /// 夹具照 Blender 的真实形态做：**位置偏移有数据、法线偏移无 `bufferView`**。
    #[test]
    fn accessors_without_a_buffer_view_are_treated_as_zero() {
        let d = tmpdir("noview");
        let mut bin = tri_bytes();
        bin.extend(f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]));
        // accessor 4 = 位置偏移（有 bufferView）；5 = 法线偏移（**没有**）。
        let body = add_target(
            &add_view(
                &add_accessor(
                    TRI_BODY,
                    "{\"bufferView\": 4, \"componentType\": 5126, \"count\": 3, \"type\": \"VEC3\"},\n    \
                     {\"componentType\": 5126, \"count\": 3, \"type\": \"VEC3\"}",
                ),
                102,
                36,
            ),
            4,
        );
        let body = body.replace(
            "\"targets\": [{\"POSITION\": 4}]",
            "\"targets\": [{\"POSITION\": 4, \"NORMAL\": 5}]",
        );
        let p = write_gltf(&d, "noview.gltf", &bin, &body);
        let g = read(&p, "noview.gltf", &GltfOpts::default())
            .expect("无 bufferView 的 accessor 必须放行（规范 = 全零）");

        // 位置偏移照常读出来。
        assert_eq!(g.shape_keys.len(), 1);
        assert_eq!(g.shape_keys[0].vertex_index, vec![0, 1, 2]);
        assert!(near3(g.shape_keys[0].position_offsets[0], [0.0, 0.0, 1.0]));
        // 法线偏移按全零读出来（**不是**报错、也不是跳过整条 target）。
        assert_eq!(g.shape_keys[0].normal_offsets.len(), 3);
        for n in &g.shape_keys[0].normal_offsets {
            assert!(near3(*n, [0.0, 0.0, 0.0]), "应按全零处理：{n:?}");
        }
    }

    /// 压缩扩展要**直接报错** —— crate 解不了，静默读出来的顶点数是错的。
    #[test]
    fn compressed_mesh_extensions_are_rejected() {
        for ext in UNSUPPORTED_EXTENSIONS {
            let d = tmpdir("ext");
            let body = format!("{TRI_BODY},\n  \"extensionsRequired\": [\"{ext}\"]");
            let p = write_gltf(&d, "ext.gltf", &tri_bytes(), &body);
            let err = read(&p, "ext.gltf", &GltfOpts::default()).unwrap_err();
            assert!(
                err.to_string().contains(ext),
                "报错文案要点名是哪个扩展：{err}"
            );
        }
    }

    /// 声明长度超过实际字节数要报错（`from_slice_without_validation` 绕过了
    /// crate 的全部校验，这三项得自己补 —— `docs/gltf-support.md` §8.1）。
    #[test]
    fn declared_buffer_length_is_checked() {
        let d = tmpdir("shortbuf");
        let json = format!(
            "{{\n  \"asset\": {{\"version\": \"2.0\"}},\n  \
             \"buffers\": [{{\"byteLength\": 9999, \
             \"uri\": \"data:application/octet-stream;base64,{}\"}}],\n{}\n}}\n",
            base64::encode([0u8; 8]),
            TRI_BODY
        );
        let p = d.join("short.gltf");
        std::fs::write(&p, json).unwrap();
        let err = read(&p, "short.gltf", &GltfOpts::default()).unwrap_err();
        assert!(
            err.to_string().contains("9999"),
            "报错应说明声明了多少字节：{err}"
        );
    }

    // -----------------------------------------------------------------
    // 端到端（`read_frames`）
    // -----------------------------------------------------------------

    /// 一个带旋转动画的夹具：绕 Z 轴 0° → 90°，`LINEAR`，时间轴 `[0, 1]` 秒。
    ///
    /// 几何字节尾部接上动画的两个 accessor（时间轴 8 B + 四元数 32 B）。
    fn anim_fixture(dir: &Path) -> PathBuf {
        let mut bin = tri_bytes();
        bin.extend(f32s(&[0.0, 1.0]));
        let s = std::f32::consts::FRAC_1_SQRT_2;
        bin.extend(f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, s, s]));
        // 几何 102 B ⟹ 时间轴在 102，四元数在 110。
        let body = add_accessor(
            TRI_BODY,
            "{\"bufferView\": 4, \"componentType\": 5126, \"count\": 2, \"type\": \"SCALAR\"},\n    \
             {\"bufferView\": 5, \"componentType\": 5126, \"count\": 2, \"type\": \"VEC4\"}",
        );
        let body = add_view(&add_view(&body, 102, 8), 110, 32);
        let body = body.replace(
            "\"nodes\": [{\"name\": \"Body.001\", \"mesh\": 0}]",
            "\"nodes\": [{\"name\": \"Body.001\", \"mesh\": 0}],\n  \
             \"animations\": [{\"name\": \"SkeletonAction\", \"channels\": [\
             {\"sampler\": 0, \"target\": {\"node\": 0, \"path\": \"rotation\"}}], \
             \"samplers\": [{\"input\": 4, \"output\": 5, \"interpolation\": \"LINEAR\"}]}]",
        );
        write_gltf(dir, "anim.gltf", &bin, &body)
    }

    fn curve_fixture(
        directory: &Path,
        times: &[f32],
        values: &[f32],
        components: usize,
        interpolation: &str,
    ) -> PathBuf {
        let path = anim_fixture(directory);
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let encoded = document["buffers"][0]["uri"].as_str().unwrap();
        let mut bytes = base64::decode(encoded.split_once(',').unwrap().1).unwrap();
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        let time_offset = bytes.len();
        bytes.extend(f32s(times));
        let value_offset = bytes.len();
        bytes.extend(f32s(values));
        for (view, offset, length) in [
            (4, time_offset, times.len() * 4),
            (5, value_offset, values.len() * 4),
        ] {
            document["bufferViews"][view]["byteOffset"] = offset.into();
            document["bufferViews"][view]["byteLength"] = length.into();
        }
        document["accessors"][4]["count"] = times.len().into();
        document["accessors"][5]["count"] = (values.len() / components).into();
        document["accessors"][5]["type"] = if components == 3 { "VEC3" } else { "VEC4" }.into();
        document["animations"][0]["channels"][0]["target"]["path"] = if components == 3 {
            "translation"
        } else {
            "rotation"
        }
        .into();
        document["animations"][0]["samplers"][0]["interpolation"] = interpolation.into();
        document["buffers"][0]["byteLength"] = bytes.len().into();
        document["buffers"][0]["uri"] = format!(
            "data:application/octet-stream;base64,{}",
            base64::encode(bytes)
        )
        .into();
        std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        path
    }

    #[test]
    fn cubic_translation_preserves_tangents_and_segment_duration() {
        let directory = tmpdir("cubic-translation");
        let path = curve_fixture(
            &directory,
            &[0.0, 2.0],
            &[
                0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 4.0, 0.0, 0.0, 2.0, 0.0, 0.0, 4.0, 0.0, 0.0, 0.0,
                0.0, 0.0,
            ],
            3,
            "CUBICSPLINE",
        );
        let animation = read_frames(&path, "curve.gltf", None, 4.0, &GltfOpts::default()).unwrap();
        assert_eq!(animation.frames.len(), 9);
        for (frame, expected) in [(0, 0.0), (2, 1.5625), (4, 2.5), (8, 4.0)] {
            assert!((animation.frames[frame].poses[0].position[0] - expected).abs() < 1e-6);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cubic_rotation_normalizes_the_interpolated_quaternion() {
        let directory = tmpdir("cubic-rotation");
        let path = curve_fixture(
            &directory,
            &[0.0, 1.0],
            &[
                0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            ],
            4,
            "CUBICSPLINE",
        );
        let animation = read_frames(&path, "curve.gltf", None, 4.0, &GltfOpts::default()).unwrap();
        let expected_quarter = 2.0 * (0.15625_f32 / 0.84375).atan();
        assert!((animation.frames[1].poses[0].rotation[2] - expected_quarter).abs() < 1e-6);
        assert!(
            (animation.frames[2].poses[0].rotation[2] - std::f32::consts::FRAC_PI_2).abs() < 1e-6
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn linear_rotation_preserves_constant_angular_velocity() {
        let directory = tmpdir("linear-rotation");
        let path = curve_fixture(
            &directory,
            &[0.0, 1.0],
            &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0],
            4,
            "LINEAR",
        );
        let animation = read_frames(&path, "curve.gltf", None, 4.0, &GltfOpts::default()).unwrap();
        assert!(
            (animation.frames[1].poses[0].rotation[2] - std::f32::consts::FRAC_PI_4).abs() < 1e-6
        );
        assert!(
            (animation.frames[3].poses[0].rotation[2] - 3.0 * std::f32::consts::FRAC_PI_4).abs()
                < 1e-6
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn animated_scale_is_rejected_instead_of_silently_losing_deformation() {
        let directory = tmpdir("animated-scale");
        for (values, interpolation, valid) in [
            (vec![1.0; 6], "LINEAR", true),
            (vec![1.0, 1.0, 1.0, 2.0, 1.0, 1.0], "LINEAR", false),
            (
                vec![
                    0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0,
                    0.0, 0.0,
                ],
                "CUBICSPLINE",
                true,
            ),
            (
                vec![
                    0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0,
                    0.0, 0.0,
                ],
                "CUBICSPLINE",
                false,
            ),
        ] {
            let path = curve_fixture(&directory, &[0.0, 1.0], &values, 3, interpolation);
            let mut document: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            document["animations"][0]["channels"][0]["target"]["path"] = "scale".into();
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            let result = read_frames(&path, "scale.gltf", None, 30.0, &GltfOpts::default());
            assert_eq!(result.is_ok(), valid, "{result:?}");
            if let Err(error) = result {
                assert!(error.to_string().contains("changes scale"));
            }
        }
    }

    #[test]
    fn malformed_curves_and_excessive_resampling_return_errors() {
        let directory = tmpdir("malformed-curves");
        for (times, values, interpolation) in [
            (vec![0.0, 1.0], vec![0.0; 6], "CUBICSPLINE"),
            (vec![1.0, 0.0], vec![0.0; 6], "LINEAR"),
            (vec![0.0, 0.0], vec![0.0; 6], "LINEAR"),
            (vec![0.0, 1.0], vec![f32::NAN; 6], "LINEAR"),
        ] {
            let path = curve_fixture(&directory, &times, &values, 3, interpolation);
            assert!(read_frames(&path, "curve.gltf", None, 30.0, &GltfOpts::default()).is_err());
        }
        let path = anim_fixture(&directory);
        for fps in [f32::INFINITY, f32::NAN, 2_000_001.0] {
            assert!(read_frames(&path, "curve.gltf", None, fps, &GltfOpts::default()).is_err());
        }
        let path = curve_fixture(&directory, &[0.0, 1.0], &[0.0; 8], 4, "LINEAR");
        assert!(read_frames(&path, "curve.gltf", None, 30.0, &GltfOpts::default()).is_err());
        let path = curve_fixture(
            &directory,
            &[0.0, 1.0],
            &[
                0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0,
            ],
            4,
            "CUBICSPLINE",
        );
        assert!(read_frames(&path, "curve.gltf", None, 2.0, &GltfOpts::default()).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// 动画按**秒轴**重采样到 `fps`，帧数 = `round(时长 × fps) + 1`。
    #[test]
    fn read_frames_samples_the_animation_axis() {
        let d = tmpdir("anim");
        let p = anim_fixture(&d);
        let smd = read_frames(&p, "anim.gltf", None, 30.0, &GltfOpts::default()).unwrap();

        assert_eq!(smd.nodes.len(), 1);
        assert_eq!(
            smd.frames.len(),
            31,
            "时间轴 0..1 秒 @30fps ⟹ round(1.0 × 30) + 1 = 31 帧"
        );
        assert_eq!(smd.frames[0].time, 0);
        assert_eq!(smd.frames[30].time, 30);

        // 欧拉角的第三位是 yaw（绕 Z）。这里只断言**大小** ——
        // 符号约定由 `quaternion_angles` 决定，不是本测试要守的东西。
        let yaw = |i: usize| smd.frames[i].poses[0].rotation[2].abs();
        assert!(yaw(0) < 1e-4, "第 0 帧是单位四元数：{}", yaw(0));
        assert!(
            (yaw(15) - std::f32::consts::FRAC_PI_4).abs() < 1e-3,
            "第 15 帧应是 45°：{}",
            yaw(15)
        );
        assert!(
            (yaw(30) - std::f32::consts::FRAC_PI_2).abs() < 1e-3,
            "第 30 帧应是 90°：{}",
            yaw(30)
        );
    }

    /// 文件里没有动画**不是错误** —— 返回一帧参考姿态（与 FBX 同一条口径）。
    #[test]
    fn read_frames_returns_the_reference_frame_without_animation() {
        let d = tmpdir("noanim");
        let p = write_gltf(&d, "tri.gltf", &tri_bytes(), TRI_BODY);
        let smd = read_frames(&p, "tri.gltf", None, 30.0, &GltfOpts::default()).unwrap();
        assert_eq!(smd.frames.len(), 1);
        assert_eq!(smd.frames[0].poses.len(), 1);
    }

    /// `srcstack` 按名选动画；名字不存在要**报错并列出可选项**。
    #[test]
    fn srcstack_selects_by_name_and_lists_the_alternatives() {
        let d = tmpdir("stack");
        let p = anim_fixture(&d);

        let smd = read_frames(
            &p,
            "anim.gltf",
            Some("SkeletonAction"),
            30.0,
            &GltfOpts::default(),
        )
        .unwrap();
        assert_eq!(smd.frames.len(), 31);

        let err = read_frames(&p, "anim.gltf", Some("nope"), 30.0, &GltfOpts::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("SkeletonAction"),
            "报错要列出文件里真正有的动画名：{err}"
        );
    }

    /// 动画与几何的**骨骼表必须一致**（同一套收骨），否则帧对不上几何。
    #[test]
    fn read_frames_and_read_agree_on_the_bone_table() {
        let d = tmpdir("agree");
        let p = anim_fixture(&d);
        let geo = read(&p, "anim.gltf", &GltfOpts::default()).unwrap();
        let smd = read_frames(&p, "anim.gltf", None, 30.0, &GltfOpts::default()).unwrap();
        let a: Vec<_> = geo.smd.nodes.iter().map(|n| n.name.clone()).collect();
        let b: Vec<_> = smd.nodes.iter().map(|n| n.name.clone()).collect();
        assert_eq!(a, b, "两条路径的骨骼表必须逐项相同");
        for f in &smd.frames {
            assert_eq!(f.poses.len(), smd.nodes.len());
        }
    }

    #[test]
    fn source_part_selectors_distinguish_duplicate_named_mesh_instances() {
        let directory = tmpdir("part_instances");
        let body = TRI_BODY.replace(
            "\"nodes\": [{\"name\": \"Body.001\", \"mesh\": 0}]",
            "\"nodes\": [{\"name\": \"Hat\", \"mesh\": 0}, {\"name\": \"Hat\", \"mesh\": 0, \"translation\": [10,0,0]}]",
        );
        let path = write_gltf(&directory, "instances.gltf", &tri_bytes(), &body);
        let parts = source_parts(&path, "instances").unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].selector.starts_with("@node:0:"));
        assert!(parts[1].selector.starts_with("@node:1:"));
        assert_eq!(parts[0].name, parts[1].name);
        let all = read_frames(&path, "instances", None, 30.0, &GltfOpts::default()).unwrap();
        let names: Vec<_> = all.nodes.iter().map(|node| node.name.as_str()).collect();
        for part in parts {
            let geometry = read(
                &path,
                "instances",
                &GltfOpts {
                    parts: vec![part.selector],
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(geometry.smd.triangles.len(), 1);
            assert!(
                geometry
                    .smd
                    .nodes
                    .iter()
                    .all(|node| names.contains(&node.name.as_str()))
            );
        }
        let legacy = read(
            &path,
            "instances",
            &GltfOpts {
                parts: vec!["Hat".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(legacy.smd.triangles.len(), 2);
    }

    #[test]
    fn source_part_selectors_reject_stale_duplicate_instances_after_ancestry_changes() {
        let directory = tmpdir("part_ancestry");
        let path = write_gltf(&directory, "instances.gltf", &tri_bytes(), TRI_BODY);
        let mut original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        original["nodes"] = serde_json::json!([
            {"name": "Hat", "mesh": 0},
            {"name": "Hat", "mesh": 0},
            {"name": "Left", "translation": [10, 0, 0], "children": [0]},
            {"name": "Right", "translation": [20, 0, 0], "children": [1]}
        ]);
        original["scenes"] = serde_json::json!([{"nodes": [2, 3]}]);
        original["scene"] = 0.into();
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        let parts = source_parts(&path, "instances").unwrap();
        let options = GltfOpts {
            parts: vec![parts[0].selector.clone()],
            axis: Some(ForcedAxis::Auto),
            ..Default::default()
        };
        let initial = read(&path, "instances", &options).unwrap();
        assert_eq!(initial.smd.triangles.len(), 1);
        assert!(near_metres(
            initial.smd.triangles[0].vertices[0].position,
            [10.0, 0.0, 0.0]
        ));
        for change in ["swap", "rename", "transform", "reindex"] {
            let mut document = original.clone();
            match change {
                "swap" => {
                    document["nodes"][2]["children"] = serde_json::json!([1]);
                    document["nodes"][3]["children"] = serde_json::json!([0]);
                }
                "rename" => document["nodes"][2]["name"] = "Renamed".into(),
                "transform" => document["nodes"][2]["translation"] = serde_json::json!([30, 0, 0]),
                "reindex" => document["nodes"].as_array_mut().unwrap().swap(2, 3),
                _ => unreachable!(),
            }
            std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            let refreshed = source_parts(&path, "instances").unwrap();
            assert!(
                refreshed
                    .iter()
                    .all(|part| part.selector != parts[0].selector),
                "{change}"
            );
            assert!(read(&path, "instances", &options).is_err(), "{change}");
            assert!(
                read_frames(&path, "instances", None, 30.0, &options)
                    .unwrap()
                    .nodes
                    .is_empty(),
                "{change}"
            );
            let fresh_options = GltfOpts {
                parts: vec![refreshed[0].selector.clone()],
                axis: Some(ForcedAxis::Auto),
                ..Default::default()
            };
            let geometry = read(&path, "instances", &fresh_options).unwrap();
            let frames = read_frames(&path, "instances", None, 30.0, &fresh_options).unwrap();
            assert_eq!(geometry.smd.triangles.len(), 1);
            assert_eq!(geometry.smd.nodes, frames.nodes);
            let expected_offset = match change {
                "swap" => 10.0,
                "transform" => 20.0,
                _ => 0.0,
            };
            let before = initial.smd.triangles[0].vertices[0].position;
            let after = geometry.smd.triangles[0].vertices[0].position;
            assert!(
                ((after[0] - before[0]) * 0.0254 - expected_offset).abs() < 0.0001,
                "{change}: {before:?} -> {after:?}"
            );
            let root_position = geometry.smd.frames[0].poses[0].position;
            assert!(
                near_metres(root_position, [10.0 + expected_offset, 0.0, 0.0]),
                "{change}: {root_position:?}"
            );
            let legacy = read(
                &path,
                "instances",
                &GltfOpts {
                    parts: vec!["@node:0".into()],
                    axis: Some(ForcedAxis::Auto),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(legacy.smd.triangles, geometry.smd.triangles);
            let named = read(
                &path,
                "instances",
                &GltfOpts {
                    parts: vec!["Hat".into()],
                    axis: Some(ForcedAxis::Auto),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(named.smd.triangles.len(), 2);
        }
    }

    /// 提示文案是**纯函数**（`log` 的 logger 是进程级全局的，测试里装不了第二个）。
    #[test]
    fn notes_render_every_kind_of_hint() {
        let n = GltfNotes {
            zero_accessors: 2,
            morph_weight_channels: 4,
        };
        assert!(!n.is_empty());
        // 文案走译文表 ⟹ 断言英文源文前先钉住语言。
        let lines = crate::test_locale::with_english(|| n.lines("a.gltf"));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("2 accessors"), "{}", lines[0]);
        assert!(lines[1].contains("MorphTargetWeights"), "{}", lines[1]);

        // ⭐ 每条提示都要能**单独**出现（漏一条就等于静默忽略一类数据）。
        for one in [
            GltfNotes {
                zero_accessors: 1,
                ..Default::default()
            },
            GltfNotes {
                morph_weight_channels: 1,
                ..Default::default()
            },
        ] {
            assert!(!one.is_empty());
            assert_eq!(one.lines("a.gltf").len(), 1);
        }

        assert!(GltfNotes::default().is_empty());
        assert!(GltfNotes::default().lines("a.gltf").is_empty());
    }
}
