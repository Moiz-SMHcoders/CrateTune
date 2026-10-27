// Ported from ArchiveTune (GPL-3.0), © Rukamori and ArchiveTune contributors:
// app/src/main/kotlin/moe/rukamori/archivetune/musicrecognition/MusicRecognitionRepository.kt
// (history: newest first, de-duplicated by stable key, 50 entries).
// Changes: stored as JSON in the library's key/value table instead of a DataStore
// preference; entries keep links and the YouTube id too.

//! Recognition history in the library database (key/value store).

use serde::{Deserialize, Serialize};

use super::shazam::Recognition;
use crate::Library;
use crate::Result;

/// Key/value key of the history list.
pub const HISTORY_KEY: &str = "recognition.history";
/// Entries kept (oldest dropped).
pub const HISTORY_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognitionEntry {
    #[serde(flatten)]
    pub result: Recognition,
    /// Unix ms.
    pub recognized_at: i64,
}

impl Library {
    /// Recognized songs, newest first. A damaged value reads as empty.
    pub fn recognition_history(&self) -> Result<Vec<RecognitionEntry>> {
        Ok(self
            .get_json::<Vec<RecognitionEntry>>(HISTORY_KEY)?
            .unwrap_or_default())
    }

    /// Adds `result` on top (replacing an older entry of the same song).
    pub fn add_recognition(&self, result: &Recognition, recognized_at: i64) -> Result<()> {
        let key = result.stable_key();
        let mut list = self.recognition_history()?;
        list.retain(|e| e.result.stable_key() != key);
        list.insert(
            0,
            RecognitionEntry {
                result: result.clone(),
                recognized_at,
            },
        );
        list.truncate(HISTORY_LIMIT);
        self.set_json(HISTORY_KEY, &list)
    }

    /// Removes the entry with this [`Recognition::stable_key`].
    pub fn remove_recognition(&self, stable_key: &str) -> Result<()> {
        let mut list = self.recognition_history()?;
        list.retain(|e| e.result.stable_key() != stable_key);
        self.set_json(HISTORY_KEY, &list)
    }

    pub fn clear_recognition_history(&self) -> Result<()> {
        self.remove_value(HISTORY_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, title: &str) -> Recognition {
        Recognition {
            track_id: id.into(),
            title: title.into(),
            artist: "Artist".into(),
            ..Recognition::default()
        }
    }

    #[test]
    fn newest_first_deduplicated_and_capped() {
        let lib = Library::open_in_memory().unwrap();
        assert!(lib.recognition_history().unwrap().is_empty());
        lib.add_recognition(&song("1", "One"), 10).unwrap();
        lib.add_recognition(&song("2", "Two"), 20).unwrap();
        lib.add_recognition(&song("1", "One again"), 30).unwrap();
        let list = lib.recognition_history().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].result.title, "One again");
        assert_eq!(list[0].recognized_at, 30);
        assert_eq!(list[1].result.track_id, "2");

        for i in 0..60 {
            lib.add_recognition(&song(&format!("x{i}"), "X"), 100 + i)
                .unwrap();
        }
        let list = lib.recognition_history().unwrap();
        assert_eq!(list.len(), HISTORY_LIMIT);
        assert_eq!(list[0].result.track_id, "x59");

        lib.remove_recognition("x59").unwrap();
        assert_eq!(lib.recognition_history().unwrap()[0].result.track_id, "x58");
        lib.clear_recognition_history().unwrap();
        assert!(lib.recognition_history().unwrap().is_empty());
    }

    #[test]
    fn damaged_value_reads_empty() {
        let lib = Library::open_in_memory().unwrap();
        lib.set_value(HISTORY_KEY, "{not json").unwrap();
        assert!(lib.recognition_history().unwrap().is_empty());
        lib.add_recognition(&song("1", "One"), 1).unwrap();
        assert_eq!(lib.recognition_history().unwrap().len(), 1);
    }
}
