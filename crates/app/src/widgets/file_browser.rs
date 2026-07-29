use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
    sync::Arc,
};

use crate::i18n::Arg;
use iced::widget::{button, container, image, row, text};
use iced::{Center, Color, Element, Length};

use crate::assets;
use crate::bridge::gma::{ArchiveDirectoryPath, ArchiveEntryPath};
use crate::format::format_bytes;
use crate::theme::{self, ViewCtx};
use crate::widgets::file_types::{SilkIcon, file_type_info};
use crate::widgets::tooltip as tooltip_widget;

const SILKICON_SIZE: f32 = 16.0;
const TYPE_COLUMN_MAX_WIDTH: f32 = 170.0;
const SIZE_COLUMN_WIDTH: f32 = 78.0;
pub const ROW_HEIGHT: f32 = 32.0;
const ROW_OVERSCAN: usize = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualRows {
    pub(crate) range: Range<usize>,
    pub(crate) top_padding: f32,
    pub(crate) bottom_padding: f32,
}

pub fn virtual_rows(row_count: usize, offset: f32, viewport_height: f32) -> VirtualRows {
    let viewport_height = if viewport_height.is_finite() {
        viewport_height.max(0.0)
    } else {
        0.0
    };
    // The scrollable clamps its own offset to the content extent (e.g. after
    // the window grows); mirror that here so a stale stored offset can never
    // hide the top rows behind the spacer.
    let max_offset = (row_count as f32 * ROW_HEIGHT - viewport_height).max(0.0);
    let offset = if offset.is_finite() {
        offset.clamp(0.0, max_offset)
    } else {
        0.0
    };
    let visible_start = (offset / ROW_HEIGHT).floor() as usize;
    let visible_end = ((offset + viewport_height) / ROW_HEIGHT).ceil() as usize;
    let start = visible_start.saturating_sub(ROW_OVERSCAN).min(row_count);
    let end = visible_end.saturating_add(ROW_OVERSCAN).min(row_count);
    VirtualRows {
        range: start..end,
        top_padding: start as f32 * ROW_HEIGHT,
        bottom_padding: row_count.saturating_sub(end) as f32 * ROW_HEIGHT,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    path: ArchiveEntryPath,
    size_bytes: u64,
}

impl Entry {
    pub(crate) fn from_archive_path(path: ArchiveEntryPath, size_bytes: u64) -> Self {
        Self { path, size_bytes }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RowKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub(crate) kind: RowKind,
    path: Arc<String>,
    shortcut_prefix_end: Option<usize>,
    display_name_start: usize,
    pub(crate) size_bytes: u64,
}

impl Row {
    pub(crate) fn shortcut_prefix(&self) -> Option<&str> {
        self.shortcut_prefix_end.map(|end| &self.path[..end])
    }

    pub(crate) fn display_name(&self) -> &str {
        &self.path[self.display_name_start..]
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &str {
        self.path.as_str()
    }

    pub(crate) fn shared_path(&self) -> Arc<String> {
        Arc::clone(&self.path)
    }
}

/// Shared row renderer for archive/folder browsers: silkicon, name (with the
/// dimmed collapsed-chain prefix), and for files a type label and size
/// column. The host modal supplies the activation message — directory
/// navigation or file preview/extraction — and `None` renders the row inert.
pub fn row_view<'a, Message: Clone + 'a>(
    row_data: &'a Row,
    activation: Option<Message>,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;

    let content: Element<'a, Message> = match row_data.kind {
        RowKind::Directory => {
            let mut name = row![].spacing(0.0);
            if let Some(prefix) = row_data.shortcut_prefix() {
                name = name.push(
                    text(prefix)
                        .size(tokens.typography.body_sm)
                        .color(Color::from(tokens.colors.browser_shortcut_dim)),
                );
            }
            name = name.push(text(row_data.display_name()).size(tokens.typography.body_sm));

            row![
                silk_image(SilkIcon::Folder, i18n.tr("file-type-folder"), &tokens),
                name,
            ]
            .align_y(Center)
            .spacing(tokens.spacing.gap_sm)
            .into()
        }
        RowKind::File => {
            let info = file_type_info(row_data.display_name());
            let type_label = i18n.trn(
                info.translation_key,
                &[("extension", Arg::Text(info.extension.as_ref()))],
            );

            row![
                silk_image(info.icon, type_label.clone(), &tokens),
                text(row_data.display_name())
                    .size(tokens.typography.body_sm)
                    .width(Length::Fill),
                text(type_label)
                    .size(tokens.typography.body_sm)
                    .color(Color::from(tokens.colors.text_dim))
                    .align_x(iced::alignment::Horizontal::Right)
                    .width(Length::Fixed(TYPE_COLUMN_MAX_WIDTH)),
                text(format_bytes(row_data.size_bytes, i18n))
                    .size(tokens.typography.body_sm)
                    .align_x(Center)
                    .width(Length::Fixed(SIZE_COLUMN_WIDTH)),
            ]
            .align_y(Center)
            .spacing(tokens.spacing.gap_sm)
            .into()
        }
    };

    let padding = [tokens.spacing.gap_sm, tokens.spacing.pad_sm];
    match activation {
        Some(message) => button(content)
            .on_press(message)
            .padding(padding)
            .width(Length::Fill)
            .height(Length::Fixed(ROW_HEIGHT))
            .style(move |_, status| theme::styles::browser_row(&tokens, status))
            .into(),
        None => container(content)
            .padding(padding)
            .width(Length::Fill)
            .height(Length::Fixed(ROW_HEIGHT))
            .into(),
    }
}

fn silk_image<'a, Message: 'a>(
    icon: SilkIcon,
    tooltip: String,
    tokens: &theme::Tokens,
) -> Element<'a, Message> {
    tooltip_widget::below(
        image(assets::silkicons::silkicon(icon))
            .width(Length::Fixed(SILKICON_SIZE))
            .height(Length::Fixed(SILKICON_SIZE)),
        tooltip,
        tokens,
        tokens.dims.tooltip_max_width,
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    root: DirNode,
    dir_index: HashMap<ArchiveDirectoryPath, DirIndexEntry>,
    current_path: ArchiveDirectoryPath,
    total_files: usize,
    total_size_bytes: u64,
}

impl State {
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = Entry>) -> Self {
        let mut root = DirNode::root();
        let mut total_files = 0_usize;
        let mut total_size_bytes = 0_u64;

        for entry in entries {
            total_files = total_files.saturating_add(1);
            total_size_bytes = total_size_bytes.saturating_add(entry.size_bytes);
            root.insert(entry);
        }

        root.sort_files_recursive();
        let root = collapse_shortcuts(root, &ArchiveDirectoryPath::root());
        let dir_index = build_dir_index(&root);

        Self {
            root,
            dir_index,
            current_path: ArchiveDirectoryPath::root(),
            total_files,
            total_size_bytes,
        }
    }

    pub(crate) fn can_go_up(&self) -> bool {
        !self.current_path.is_root() && self.parent_path_for(&self.current_path).is_some()
    }

    pub(crate) fn open_directory(&mut self, path: &str) -> bool {
        let Some(path) = ArchiveDirectoryPath::from_validated(path) else {
            return false;
        };
        if self.dir_index.contains_key(&path) {
            self.current_path = path;
            true
        } else {
            false
        }
    }

    pub(crate) fn go_up(&mut self) -> bool {
        let Some(parent_path) = self.parent_path_for(&self.current_path) else {
            return false;
        };
        self.current_path = parent_path;
        true
    }

    pub(crate) fn rows(&self) -> Vec<Row> {
        self.current_dir().map(rows_for_dir).unwrap_or_default()
    }

    pub(crate) fn shown_count(&self) -> usize {
        self.current_dir()
            .map_or(0, |dir| dir.dirs.len().saturating_add(dir.files.len()))
    }

    pub(crate) fn footer_total_files(&self) -> i32 {
        saturating_i32(self.total_files)
    }

    pub(crate) fn footer_shown_count(&self) -> i32 {
        saturating_i32(self.shown_count())
    }

    pub(crate) const fn footer_total_size_bytes(&self) -> u64 {
        self.total_size_bytes
    }

    pub(crate) fn header_path(&self, browse_path: Option<&str>) -> String {
        let Some(browse_path) = browse_path else {
            return String::new();
        };
        let mut normalized = browse_path.replace('\\', "/");
        if normalized.is_empty() || self.current_path.is_root() {
            return normalized;
        }
        if !normalized.ends_with('/') {
            normalized.push('/');
        }
        normalized.push_str(self.current_path.as_str());
        normalized
    }

    fn current_dir(&self) -> Option<&DirNode> {
        self.find_dir(&self.current_path)
    }

    fn find_dir(&self, path: &ArchiveDirectoryPath) -> Option<&DirNode> {
        let entry = self.dir_index.get(path)?;
        find_dir_by_route(&self.root, &entry.route)
    }

    fn parent_path_for(&self, path: &ArchiveDirectoryPath) -> Option<ArchiveDirectoryPath> {
        self.dir_index.get(path)?.parent_path.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirNode {
    path: ArchiveDirectoryPath,
    sort_name: String,
    dirs: BTreeMap<String, Self>,
    files: Vec<FileNode>,
    shortcut: Option<DirShortcut>,
}

impl DirNode {
    fn root() -> Self {
        Self {
            path: ArchiveDirectoryPath::root(),
            sort_name: String::new(),
            dirs: BTreeMap::new(),
            files: Vec::new(),
            shortcut: None,
        }
    }

    fn child(path: ArchiveDirectoryPath) -> Self {
        let sort_name = path
            .file_name()
            .expect("directory children are never the archive root")
            .to_ascii_lowercase();
        Self {
            path,
            sort_name,
            dirs: BTreeMap::new(),
            files: Vec::new(),
            shortcut: None,
        }
    }

    fn insert(&mut self, entry: Entry) {
        let mut dir = self;
        if let Some((directory, _)) = entry.path.as_str().rsplit_once('/') {
            let mut prefix_end = 0;
            for component in directory.split('/') {
                prefix_end += component.len();
                let prefix = &directory[..prefix_end];
                if !dir.dirs.contains_key(component) {
                    dir.dirs.insert(
                        component.to_owned(),
                        Self::child(
                            ArchiveDirectoryPath::from_validated(prefix.to_owned())
                                .expect("validated entry prefixes remain valid directory paths"),
                        ),
                    );
                }
                dir = dir
                    .dirs
                    .get_mut(component)
                    .expect("directory was present or inserted above");
                prefix_end += 1; // separator before the next component
            }
        }

        dir.files.push(FileNode::new(entry.path, entry.size_bytes));
    }

    fn sort_files_recursive(&mut self) {
        self.files.sort_by(compare_files);
        for child in self.dirs.values_mut() {
            child.sort_files_recursive();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirShortcut {
    prefix: ArchiveDirectoryPath,
    leaf: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileNode {
    path: ArchiveEntryPath,
    sort_name: String,
    size_bytes: u64,
}

impl FileNode {
    fn new(path: ArchiveEntryPath, size_bytes: u64) -> Self {
        let sort_name = path.file_name().to_ascii_lowercase();
        Self {
            path,
            sort_name,
            size_bytes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirIndexEntry {
    parent_path: Option<ArchiveDirectoryPath>,
    route: Box<str>,
}

fn collapse_shortcuts(mut node: DirNode, path: &ArchiveDirectoryPath) -> DirNode {
    let dirs = std::mem::take(&mut node.dirs);
    node.dirs = dirs
        .into_iter()
        .map(|(name, child)| {
            let child_path = path
                .join_child(&name)
                .expect("directory child names come from validated archive paths");
            (name, collapse_shortcuts(child, &child_path))
        })
        .collect();

    if !path.is_root() && node.files.is_empty() && node.dirs.len() == 1 {
        let mut dirs = std::mem::take(&mut node.dirs);
        let Some((child_name, mut child)) = dirs.pop_first() else {
            return node;
        };
        if child.shortcut.is_none() {
            child.shortcut = Some(DirShortcut {
                prefix: path.clone(),
                leaf: child_name,
            });
        }
        return child;
    }

    node
}

fn rows_for_dir(dir: &DirNode) -> Vec<Row> {
    let mut dirs = dir.dirs.iter().collect::<Vec<_>>();
    dirs.sort_by(|left, right| compare_dirs(left.1, right.1));

    dirs.into_iter()
        .map(directory_row)
        .chain(dir.files.iter().map(file_row))
        .collect()
}

fn directory_row((name, dir): (&String, &DirNode)) -> Row {
    let path = Arc::new(dir.path.to_string());
    let (shortcut_prefix_end, display_name_start) = dir.shortcut.as_ref().map_or_else(
        || (None, path.len().saturating_sub(name.len())),
        |shortcut| {
            (
                Some(shortcut.prefix.as_str().len().saturating_add(1)),
                path.len().saturating_sub(shortcut.leaf.len()),
            )
        },
    );

    Row {
        kind: RowKind::Directory,
        path,
        shortcut_prefix_end,
        display_name_start,
        size_bytes: 0,
    }
}

fn file_row(file: &FileNode) -> Row {
    let path = file.path.shared_string();
    let display_name_start = path.len().saturating_sub(file.path.file_name().len());
    Row {
        kind: RowKind::File,
        path,
        shortcut_prefix_end: None,
        display_name_start,
        size_bytes: file.size_bytes,
    }
}

fn build_dir_index(root: &DirNode) -> HashMap<ArchiveDirectoryPath, DirIndexEntry> {
    let mut index = HashMap::new();
    index_dir_recursive(&mut index, root, None, &mut String::new());
    index
}

fn index_dir_recursive(
    index: &mut HashMap<ArchiveDirectoryPath, DirIndexEntry>,
    dir: &DirNode,
    parent_path: Option<&ArchiveDirectoryPath>,
    route: &mut String,
) {
    index.insert(
        dir.path.clone(),
        DirIndexEntry {
            parent_path: parent_path.cloned(),
            route: route.clone().into_boxed_str(),
        },
    );

    for (name, child) in &dir.dirs {
        let parent_route_len = route.len();
        if !route.is_empty() {
            route.push('/');
        }
        route.push_str(name);
        index_dir_recursive(index, child, Some(&dir.path), route);
        route.truncate(parent_route_len);
    }
}

fn find_dir_by_route<'a>(root: &'a DirNode, route: &str) -> Option<&'a DirNode> {
    let mut dir = root;
    for name in route.split('/').filter(|name| !name.is_empty()) {
        dir = dir.dirs.get(name)?;
    }
    Some(dir)
}

fn compare_files(a: &FileNode, b: &FileNode) -> std::cmp::Ordering {
    a.sort_name
        .cmp(&b.sort_name)
        .then_with(|| a.path.file_name().cmp(b.path.file_name()))
        .then_with(|| a.path.cmp(&b.path))
}

fn compare_dirs(a: &DirNode, b: &DirNode) -> std::cmp::Ordering {
    a.sort_name
        .cmp(&b.sort_name)
        .then_with(|| a.path.cmp(&b.path))
}

fn saturating_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use crate::bridge::gma::ArchiveEntryPath;

    use super::*;

    fn entry(path: &str, size_bytes: u64) -> Entry {
        Entry::from_archive_path(
            ArchiveEntryPath::from_validated(path).expect("fixture path should validate"),
            size_bytes,
        )
    }

    #[test]
    fn virtual_rows_cover_viewport_with_overscan_and_exact_spacers() {
        let window = virtual_rows(100, ROW_HEIGHT * 40.0, ROW_HEIGHT * 10.0);
        assert_eq!(window.range, 36..54);
        assert_eq!(window.top_padding, ROW_HEIGHT * 36.0);
        assert_eq!(window.bottom_padding, ROW_HEIGHT * 46.0);
        assert_eq!(
            window.top_padding + window.range.len() as f32 * ROW_HEIGHT + window.bottom_padding,
            ROW_HEIGHT * 100.0
        );
    }

    #[test]
    fn virtual_rows_clamp_a_stale_offset_to_the_content_extent() {
        // A remembered offset can exceed the content extent after the window
        // grows or the listing shrinks; the top rows must stay visible.
        let window = virtual_rows(10, ROW_HEIGHT * 300.0, ROW_HEIGHT * 20.0);
        assert_eq!(window.range, 0..10);
        assert_eq!(window.top_padding, 0.0);
        assert_eq!(window.bottom_padding, 0.0);

        let window = virtual_rows(100, ROW_HEIGHT * 300.0, ROW_HEIGHT * 10.0);
        assert_eq!(window.range, 86..100);
        assert_eq!(window.top_padding, ROW_HEIGHT * 86.0);
        assert_eq!(window.bottom_padding, 0.0);
    }

    #[test]
    fn builds_tree_from_flat_entries() {
        let browser = State::from_entries([
            entry("lua/autorun/init.lua", 100),
            entry("materials/icon.vmt", 200),
        ]);

        let rows = browser.rows();
        assert_eq!(rows[0].shortcut_prefix(), Some("lua/"));
        assert_eq!(rows[0].display_name(), "autorun");
        assert_eq!(rows[1].shortcut_prefix(), None);
        assert_eq!(rows[1].display_name(), "materials");
        assert_eq!(browser.footer_total_files(), 2);
        assert_eq!(browser.footer_total_size_bytes(), 300);
    }

    #[test]
    fn navigates_collapsed_directories_and_parent() {
        let mut browser = State::from_entries([entry("lua/autorun/init.lua", 100)]);

        assert!(browser.open_directory("lua/autorun"));
        assert_eq!(browser.rows()[0].display_name(), "init.lua");
        assert!(browser.can_go_up());

        assert!(browser.go_up());
        assert_eq!(browser.rows()[0].display_name(), "autorun");
    }

    #[test]
    fn shortcut_collapses_a_whole_empty_chain_to_its_final_segment() {
        let browser = State::from_entries([entry("materials/models/props/metal.vmt", 64)]);

        let rows = browser.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].shortcut_prefix(), Some("materials/models/"));
        assert_eq!(rows[0].display_name(), "props");
        // Clicking navigates to the END of the chain.
        assert_eq!(rows[0].path(), "materials/models/props");
    }

    #[test]
    fn shortcut_stops_where_a_level_has_files() {
        let browser = State::from_entries([
            entry("materials/readme.txt", 10),
            entry("materials/models/props/metal.vmt", 64),
        ]);

        // `materials` has a file, so it renders plain; the empty
        // `models` level collapses into `props`.
        let rows = browser.rows();
        assert_eq!(rows[0].shortcut_prefix(), None);
        assert_eq!(rows[0].display_name(), "materials");

        let mut browser = browser;
        assert!(browser.open_directory("materials"));
        let rows = browser.rows();
        assert_eq!(rows[0].shortcut_prefix(), Some("materials/models/"));
        assert_eq!(rows[0].display_name(), "props");
    }

    #[test]
    fn root_level_directories_never_collapse() {
        let browser = State::from_entries([entry("lua/init.lua", 5)]);

        let rows = browser.rows();
        assert_eq!(rows[0].shortcut_prefix(), None);
        assert_eq!(rows[0].display_name(), "lua");
    }
}
