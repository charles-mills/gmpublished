//! Localized Workshop drafts and reconciliation of successful metadata writes.
use super::domain::{PublishedFileId, SteamId};
pub use gmpublished_backend::WorkshopLanguage;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct WorkshopText {
    pub title: String,
    pub description: String,
}
pub type LocalizedDrafts = BTreeMap<WorkshopLanguage, WorkshopText>;

pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\0', "")
        .trim()
        .to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldEdit {
    pub previous: Option<String>,
    pub value: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEdit {
    pub workshop_id: PublishedFileId,
    pub language: WorkshopLanguage,
    pub title: Option<FieldEdit>,
    pub description: Option<FieldEdit>,
}
impl MetadataEdit {
    pub fn needs_baseline(&self) -> bool {
        [&self.title, &self.description]
            .into_iter()
            .flatten()
            .any(|field| field.previous.is_none())
    }
    pub fn resolve_baseline(&mut self, remote: WorkshopText) {
        for (field, value) in [
            (&mut self.title, remote.title),
            (&mut self.description, remote.description),
        ] {
            if let Some(field) = field.as_mut() {
                field.previous.get_or_insert(value);
            }
        }
    }
    pub fn remove_unchanged(&mut self) {
        for field in [&mut self.title, &mut self.description] {
            if field
                .as_ref()
                .is_some_and(|field| field.previous.as_deref() == Some(field.value.as_str()))
            {
                *field = None;
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct SavedField {
    latest: String,
    superseded: Vec<String>,
    pending: bool,
}
impl SavedField {
    fn record(&mut self, edit: &FieldEdit) {
        if !self.pending {
            self.superseded.clear();
        }
        for old in std::iter::once(&self.latest).chain(edit.previous.iter()) {
            if old != &edit.value && !self.superseded.contains(old) {
                self.superseded.push(old.clone());
            }
        }
        self.superseded.retain(|old| old != &edit.value);
        self.latest.clone_from(&edit.value);
        self.pending = true;
    }
    fn reconcile(&mut self, remote: &str) -> String {
        if !self.pending {
            return remote.to_owned();
        }
        if remote == self.latest {
            self.pending = false;
        } else if self.superseded.iter().any(|old| old == remote) {
            return self.latest.clone();
        } else {
            self.pending = false;
        }
        remote.to_owned()
    }
}
#[derive(Debug, Deserialize, Serialize)]
struct SavedText {
    account: SteamId,
    item: u64,
    language: WorkshopLanguage,
    title: Option<SavedField>,
    description: Option<SavedField>,
}
#[derive(Debug, Deserialize, Serialize)]
struct Journal {
    version: u32,
    entries: Vec<SavedText>,
}
impl super::snapshot::Versioned for Journal {
    const VERSION: u32 = 1;
    const NOUN: &'static str = "Workshop metadata edits";
    fn version(&self) -> u32 {
        self.version
    }
}

#[derive(Debug)]
pub struct TextJournal {
    path: Option<PathBuf>,
    data: Journal,
}
impl TextJournal {
    pub fn load(path: Option<PathBuf>) -> Self {
        let data = path
            .as_deref()
            .and_then(super::snapshot::load)
            .unwrap_or(Journal {
                version: 1,
                entries: Vec::new(),
            });
        Self { path, data }
    }
    pub fn record(&mut self, account: SteamId, edit: &MetadataEdit) {
        let entry = if let Some(index) = self.data.entries.iter().position(|entry| {
            entry.account == account
                && entry.item == edit.workshop_id.get()
                && entry.language == edit.language
        }) {
            &mut self.data.entries[index]
        } else {
            self.data.entries.push(SavedText {
                account,
                item: edit.workshop_id.get(),
                language: edit.language,
                title: None,
                description: None,
            });
            self.data.entries.last_mut().expect("inserted entry")
        };
        for (field, change) in [
            (&mut entry.title, &edit.title),
            (&mut entry.description, &edit.description),
        ] {
            if let Some(change) = change {
                let saved = field.get_or_insert_with(|| SavedField {
                    latest: change.previous.clone().unwrap_or_default(),
                    superseded: Vec::new(),
                    pending: false,
                });
                saved.record(change);
            }
        }
        self.persist();
    }
    pub fn reconcile(
        &mut self,
        account: SteamId,
        item: PublishedFileId,
        language: WorkshopLanguage,
        mut remote: WorkshopText,
    ) -> WorkshopText {
        remote.title = normalize(&remote.title);
        remote.description = normalize(&remote.description);
        if let Some(entry) = self.data.entries.iter_mut().find(|entry| {
            entry.account == account && entry.item == item.get() && entry.language == language
        }) {
            if let Some(title) = entry.title.as_mut() {
                remote.title = title.reconcile(&remote.title);
            }
            if let Some(description) = entry.description.as_mut() {
                remote.description = description.reconcile(&remote.description);
            }
            self.persist();
        }
        remote
    }
    fn persist(&self) {
        if let Some(path) = self.path.as_deref()
            && let Err(error) = super::snapshot::write(path, &self.data)
        {
            log::warn!("could not persist successful Workshop metadata edits: {error}");
        }
    }
    #[cfg(test)]
    fn backup(&self) -> &str {
        &self.data.entries[0].description.as_ref().unwrap().latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edit(previous: &str, value: &str) -> MetadataEdit {
        MetadataEdit {
            workshop_id: PublishedFileId::fixture(42),
            language: WorkshopLanguage::French,
            title: None,
            description: Some(FieldEdit {
                previous: Some(previous.into()),
                value: value.into(),
            }),
        }
    }
    fn read(
        journal: &mut TextJournal,
        account: u64,
        language: WorkshopLanguage,
        remote: &str,
    ) -> String {
        journal
            .reconcile(
                SteamId::new(account),
                PublishedFileId::fixture(42),
                language,
                WorkshopText {
                    title: "title".into(),
                    description: remote.into(),
                },
            )
            .description
    }
    #[test]
    fn rapid_saves_survive_stale_reads_and_restart_but_allow_external_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edits.json");
        let mut journal = TextJournal::load(Some(path.clone()));
        journal.record(SteamId::new(1), &edit("A", "B"));
        journal.record(SteamId::new(1), &edit("B", "C"));
        let mut journal = TextJournal::load(Some(path));
        for stale in ["A", "B"] {
            assert_eq!(read(&mut journal, 1, WorkshopLanguage::French, stale), "C");
        }
        assert_eq!(read(&mut journal, 2, WorkshopLanguage::French, "A"), "A");
        assert_eq!(read(&mut journal, 1, WorkshopLanguage::Turkish, "A"), "A");
        assert_eq!(
            read(&mut journal, 1, WorkshopLanguage::French, "external"),
            "external"
        );
        assert_eq!(journal.backup(), "C");
        assert_eq!(read(&mut journal, 1, WorkshopLanguage::French, "A"), "A");
    }
    #[test]
    fn acknowledgement_retires_override_and_line_endings_compare_equally() {
        let mut journal = TextJournal::load(None);
        journal.record(SteamId::new(1), &edit("old", "new\ntext"));
        assert_eq!(
            read(&mut journal, 1, WorkshopLanguage::French, "new\r\ntext"),
            "new\ntext"
        );
        assert_eq!(
            read(&mut journal, 1, WorkshopLanguage::French, "external"),
            "external"
        );
        assert_eq!(journal.backup(), "new\ntext");
    }
    #[test]
    fn missing_translation_records_fallback_without_overwriting_known_baselines() {
        let mut change = edit("original description", "translated description");
        change.title = Some(FieldEdit {
            previous: None,
            value: "French title".into(),
        });
        assert!(change.needs_baseline());
        change.resolve_baseline(WorkshopText {
            title: "English fallback".into(),
            description: "other remote".into(),
        });
        assert_eq!(
            change.description.as_ref().unwrap().previous.as_deref(),
            Some("original description")
        );
        assert!(!change.needs_baseline());
        let mut journal = TextJournal::load(None);
        journal.record(SteamId::new(1), &change);
        let reconciled = journal.reconcile(
            SteamId::new(1),
            change.workshop_id,
            change.language,
            WorkshopText {
                title: "English fallback".into(),
                description: "external description".into(),
            },
        );
        assert_eq!(reconciled.title, "French title");
        assert_eq!(reconciled.description, "external description");
        change.title.as_mut().unwrap().value = "English fallback".into();
        change.remove_unchanged();
        assert!(change.title.is_none());
        assert!(change.description.is_some());
    }
}
