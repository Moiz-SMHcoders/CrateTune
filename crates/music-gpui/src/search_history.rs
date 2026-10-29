//! Ordered, flushable writes of the search history to the library database.
//!
//! **Why:** saving and forgetting searches used to spawn one detached thread per
//! call. The library serializes calls on one connection mutex, which is not fair:
//! while something else held it (a sync, a scan, a stats query), several waiting
//! threads were let in in any order, and the process could quit before a waiting
//! write ran. A search saved first could thus be lost (or a "remove" could overtake
//! the "add" it was meant to undo) — the "justice genesis vanished" report. Now one
//! worker applies the writes in submission order, and quitting flushes it.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use music_core::Library;

enum Op {
    Add(String),
    Remove(String),
    Flush(Sender<()>),
}

/// The search-history writer thread (one per library).
pub struct SearchHistoryWriter {
    tx: Sender<Op>,
    _thread: Option<JoinHandle<()>>,
}

impl SearchHistoryWriter {
    pub fn spawn(library: Library) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("search-history".into())
            .spawn(move || run(&library, &rx))
            .ok();
        Self {
            tx,
            _thread: thread,
        }
    }

    /// Saves `query` (moves it to the top if known, in any letter case).
    pub fn add(&self, query: &str) {
        let _ = self.tx.send(Op::Add(query.to_owned()));
    }

    /// Forgets `query` (every letter-case variant).
    pub fn remove(&self, query: &str) {
        let _ = self.tx.send(Op::Remove(query.to_owned()));
    }

    /// Waits until every earlier write is in the database (at most `timeout`).
    pub fn flush(&self, timeout: Duration) -> bool {
        let (ack, done) = mpsc::channel();
        self.tx.send(Op::Flush(ack)).is_ok() && done.recv_timeout(timeout).is_ok()
    }
}

fn run(library: &Library, rx: &Receiver<Op>) {
    for op in rx {
        let result = match op {
            Op::Add(q) => library.add_search_query(&q),
            Op::Remove(q) => library.remove_search_query(&q),
            Op::Flush(ack) => {
                let _ = ack.send(());
                Ok(())
            }
        };
        if let Err(e) = result {
            eprintln!("music-gpui: search history: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queries(lib: &Library) -> Vec<String> {
        lib.search_history("", 30)
            .unwrap()
            .into_iter()
            .map(|e| e.query)
            .collect()
    }

    /// The reported sequence: "justice genesis", then a query YouTube corrects, more
    /// searches and removals, a quit right after (flush), a relaunch.
    #[test]
    fn saved_searches_are_written_in_order_and_flushed_on_quit() {
        let dir = std::env::temp_dir().join(format!("music-search-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.sqlite");
        {
            let lib = Library::open(&path).unwrap();
            let writer = SearchHistoryWriter::spawn(lib.clone());
            writer.add("justice genesis");
            writer.add("justice genisis"); // YouTube shows results for "justice genesis"
            writer.add("Justice Genesis"); // same query, other casing: one entry
            writer.add("daft punk");
            writer.remove("justice genisis");
            writer.add("justice genisis");
            writer.remove("nothing like this");
            // Quit: flush before the process ends.
            assert!(writer.flush(Duration::from_secs(5)));
        }
        let lib = Library::open(&path).unwrap();
        assert_eq!(
            queries(&lib),
            ["justice genisis", "daft punk", "Justice Genesis"]
        );
        // Best effort: on Windows the writer thread may still hold the database
        // open for a moment, and an open file can't be deleted there.
        drop(lib);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
