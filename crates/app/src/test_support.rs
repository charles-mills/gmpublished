use std::{
    fs,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use gif::{DisposalMethod, Encoder, Frame, Repeat};
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use tempfile::TempDir;
use vformats::vtf::VtfFormat;

use crate::bridge::gma::{FixtureGmaEntry, FixtureGmaFile, GMA_VERSION, GmaHeader, GmaMetadata};

pub struct TestDir {
    inner: TempDir,
}

impl TestDir {
    pub(crate) fn new(prefix: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("test temp directory should be creatable");
        Self { inner }
    }

    pub(crate) fn path(&self) -> &Path {
        self.inner.path()
    }

    pub(crate) fn path_text(&self) -> String {
        self.path().to_string_lossy().into_owned()
    }

    pub(crate) fn join(&self, relative_path: impl AsRef<Path>) -> PathBuf {
        self.path().join(relative_path)
    }

    pub(crate) fn dir(&self, relative_path: impl AsRef<Path>) -> PathBuf {
        let path = self.join(relative_path);
        fs::create_dir_all(&path).expect("fixture directory should be creatable");
        path
    }

    pub(crate) fn file(
        &self,
        relative_path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> PathBuf {
        let path = self.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent directory should be creatable");
        }
        fs::write(&path, contents).expect("fixture file should be writable");
        path
    }

    pub(crate) fn image(
        &self,
        relative_path: impl AsRef<Path>,
        format: ImageFormat,
        width: u32,
        height: u32,
    ) -> PathBuf {
        let path = self.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent directory should be creatable");
        }

        let mut file = File::create(&path).expect("image fixture should be creatable");
        let image = RgbaImage::from_pixel(width, height, Rgba([80, 120, 160, 255]));
        DynamicImage::ImageRgba8(image)
            .write_to(&mut file, format)
            .expect("image fixture should encode");
        path
    }

    pub(crate) fn gif(&self, relative_path: impl AsRef<Path>, width: u32, height: u32) -> PathBuf {
        let path = self.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent directory should be creatable");
        }

        let file = File::create(&path).expect("gif fixture should be creatable");
        let width_u16 = u16::try_from(width).expect("gif fixture width should fit u16");
        let height_u16 = u16::try_from(height).expect("gif fixture height should fit u16");
        let mut encoder =
            Encoder::new(file, width_u16, height_u16, &[]).expect("gif should encode");
        encoder
            .set_repeat(Repeat::Infinite)
            .expect("gif repeat should encode");
        for (color, delay_ms) in [([255, 0, 0, 255], 30_u32), ([0, 255, 0, 255], 90_u32)] {
            let frame = solid_gif_frame(width_u16, height_u16, color, delay_ms);
            encoder
                .write_frame(&frame)
                .expect("gif frame should encode");
        }
        path
    }
}

fn solid_gif_frame(width: u16, height: u16, color: [u8; 4], delay_ms: u32) -> Frame<'static> {
    let pixels = vec![0; usize::from(width) * usize::from(height)];
    let palette = vec![color[0], color[1], color[2]];
    let mut frame = Frame::from_palette_pixels(width, height, pixels, palette, None);
    frame.delay = u16::try_from(delay_ms / 10).expect("gif delay should fit u16");
    frame.dispose = DisposalMethod::Background;
    frame
}

/// Minimal VTF 7.1 container wrapping raw high-resolution mip payloads,
/// laid out exactly as `vformats::vtf::parse` expects: one frame, no
/// lowres image, mips stored smallest-first (VTF file order).
pub fn fixture_vtf_bytes(
    width: u16,
    height: u16,
    format: VtfFormat,
    stored_mips: &[&[u8]],
) -> Vec<u8> {
    const HEADER_SIZE: u32 = 64;
    let format_raw: i32 = match format {
        VtfFormat::Rgba8888 => 0,
        VtfFormat::Rgb888 => 2,
        VtfFormat::Dxt1 => 13,
        VtfFormat::Dxt3 => 14,
        VtfFormat::Dxt5 => 15,
        other => panic!("fixture format {other:?} has no on-disk code mapped"),
    };
    let mut bytes = Vec::with_capacity(HEADER_SIZE as usize);
    bytes.extend_from_slice(b"VTF\0");
    bytes.extend_from_slice(&7_u32.to_le_bytes()); // version major
    bytes.extend_from_slice(&1_u32.to_le_bytes()); // version minor
    bytes.extend_from_slice(&HEADER_SIZE.to_le_bytes());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes()); // flags
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // frames
    bytes.extend_from_slice(&0_u16.to_le_bytes()); // first frame
    bytes.extend_from_slice(&[0; 4]); // padding
    bytes.extend_from_slice(&[0; 12]); // reflectivity [0.0; 3]
    bytes.extend_from_slice(&[0; 4]); // padding
    bytes.extend_from_slice(&1.0_f32.to_le_bytes()); // bumpmap scale
    bytes.extend_from_slice(&format_raw.to_le_bytes());
    bytes.push(u8::try_from(stored_mips.len()).expect("fixture mip count"));
    bytes.extend_from_slice(&(-1_i32).to_le_bytes()); // no lowres image
    bytes.push(0); // lowres width
    bytes.push(0); // lowres height
    bytes.resize(HEADER_SIZE as usize, 0);
    for mip in stored_mips {
        bytes.extend_from_slice(mip);
    }
    bytes
}

pub fn crc32(bytes: &[u8]) -> u32 {
    crate::bridge::gma::crc32(bytes)
}

pub fn write_gma_fixture(path: impl AsRef<Path>, archive: &FixtureGmaFile) -> PathBuf {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("gma fixture parent directory should be creatable");
    }

    let mut file = File::create(path).expect("gma fixture should be writable");
    file.write_all(b"GMAD").expect("gma header");
    file.write_all(&[archive.header.version])
        .expect("gma version");
    file.write_all(&0_u64.to_le_bytes()).expect("steamid");
    file.write_all(&archive.header.timestamp.to_le_bytes())
        .expect("timestamp");
    if archive.header.version > 1 {
        write_nt_string(&mut file, "");
    }
    write_nt_string(&mut file, archive.header.metadata.title());
    write_nt_string(&mut file, &embedded_description(&archive.header.metadata));
    write_nt_string(&mut file, &archive.header.author);
    file.write_all(&archive.header.addon_version.to_le_bytes())
        .expect("addon version");

    for (index, (entry, contents)) in archive.entries.iter().zip(&archive.data).enumerate() {
        file.write_all(
            &(u32::try_from(index + 1).expect("fixture entry index should fit u32")).to_le_bytes(),
        )
        .expect("entry index");
        write_nt_string(&mut file, &entry.path);
        file.write_all(
            &(i64::try_from(contents.len()).expect("fixture entry size should fit i64"))
                .to_le_bytes(),
        )
        .expect("entry size");
        file.write_all(&entry.crc32.to_le_bytes())
            .expect("entry crc");
    }
    file.write_all(&0_u32.to_le_bytes())
        .expect("entry table terminator");
    for contents in &archive.data {
        file.write_all(contents).expect("entry contents");
    }
    file.write_all(&archive.trailer_crc32.to_le_bytes())
        .expect("trailer crc");

    path.to_path_buf()
}

fn write_nt_string(file: &mut File, value: &str) {
    file.write_all(value.as_bytes()).expect("nt string bytes");
    file.write_all(&[0]).expect("nt string terminator");
}

fn embedded_description(metadata: &GmaMetadata) -> String {
    match metadata {
        GmaMetadata::Standard { .. } => {
            let backend_metadata: gmpublished_backend::GMAMetadata = metadata.clone().into();
            serde_json::to_string(&backend_metadata).expect("standard gma metadata should encode")
        }
        GmaMetadata::Legacy { description, .. } => description.clone(),
    }
}

#[derive(Debug, Clone)]
pub struct GmaFixtureBuilder {
    title: String,
    entries: Vec<(String, Vec<u8>)>,
}

impl GmaFixtureBuilder {
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            entries: Vec::new(),
        }
    }

    pub(crate) fn entry(mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        self.entries.push((path.into(), contents.into()));
        self
    }

    pub(crate) fn build(self) -> FixtureGmaFile {
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut data = Vec::with_capacity(self.entries.len());
        for (path, contents) in self.entries {
            entries.push(FixtureGmaEntry::new(path, crc32(&contents)));
            data.push(contents);
        }

        FixtureGmaFile {
            path: None,
            header: GmaHeader {
                version: GMA_VERSION,
                timestamp: 1_717_171_717,
                metadata: GmaMetadata::Legacy {
                    title: self.title,
                    description: String::new(),
                },
                author: "Author Name".to_owned(),
                addon_version: 1,
            },
            entries,
            data,
            trailer_crc32: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use image::ImageReader;

    use super::*;

    #[test]
    fn test_dir_writes_nested_fixtures() {
        let dir = TestDir::new("gmpublished-app-test-support");

        let nested = dir.file("content/lua/autorun/init.lua", b"fixture");
        let extra_dir = dir.dir("content/materials");

        assert!(nested.exists());
        assert!(extra_dir.is_dir());
        assert!(dir.path_text().contains("gmpublished-app-test-support"));
    }

    #[test]
    fn image_and_gif_builders_write_decodable_files() {
        let dir = TestDir::new("gmpublished-app-test-support-media");

        let png = dir.image("icons/icon.png", ImageFormat::Png, 8, 8);
        let gif = dir.gif("icons/animated.gif", 8, 8);

        assert_eq!(
            ImageReader::open(png)
                .expect("png should open")
                .decode()
                .expect("png should decode")
                .width(),
            8
        );
        assert!(gif.exists());
    }

    #[test]
    fn gma_fixture_builder_hashes_entries() {
        let archive = GmaFixtureBuilder::new("Fixture")
            .entry("lua/autorun/init.lua", b"print('ok')\n".to_vec())
            .build();

        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.entries[0].crc32, crc32(&archive.data[0]));
    }
}
