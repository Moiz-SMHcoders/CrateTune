//! Script actions for recognition, onboarding and the update check (see `script.rs`):
//!
//! | action | does |
//! |---|---|
//! | `@recognize` | open the Recognition page and listen through the microphone |
//! | `@recognize-file=PATH` | open the Recognition page and identify the start of a local file |
//! | `@recognize-cancel` | stop listening |
//! | `@recognize-history=N` | show history entry N |
//! | `@onboarding[=STEP]`, `@onboarding=off` | show the first-run guide (at step N) / close it |
//! | `@onboarding-next`, `@onboarding-back` | page through it |
//! | `@update-check` | check for a new release now (needs `UPDATE_REPO`) |

use gpui::{Context, Window};

use crate::app::MusicApp;
use crate::recognize::Recognizer;
use crate::router::Route;

impl MusicApp {
    /// ⇧⌘R / menu: open the Recognition page and start listening (or stop).
    pub fn recognize_song(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Route::Recognition, window, cx);
        let recognizer = Recognizer::global(cx);
        recognizer.update(cx, |r, cx| {
            if r.phase.busy() {
                r.cancel(cx)
            } else {
                r.listen(cx)
            }
        });
    }

    /// Returns whether `name` was one of these actions.
    pub fn release_script_action(
        &mut self,
        name: &str,
        arg: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match name {
            "recognize" => self.recognize_song(window, cx),
            "recognize-file" => {
                let Some(path) = arg else {
                    eprintln!("music-gpui: @recognize-file wants a path");
                    return true;
                };
                self.navigate(Route::Recognition, window, cx);
                let path = std::path::PathBuf::from(path);
                Recognizer::global(cx).update(cx, |r, cx| r.recognize_file(path, cx));
            }
            "recognize-cancel" => {
                Recognizer::global(cx).update(cx, |r, cx| r.cancel(cx));
            }
            "recognize-history" => {
                let index = arg.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
                self.navigate(Route::Recognition, window, cx);
                Recognizer::global(cx).update(cx, |r, cx| {
                    if let Some(entry) = r.history.get(index).cloned() {
                        r.show(&entry, cx);
                    }
                });
            }
            "onboarding" => match arg {
                Some("off") => self.close_onboarding(cx),
                step => {
                    let step = step.and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
                    self.open_onboarding(step, window, cx);
                }
            },
            "onboarding-next" => self.onboarding_step(1, cx),
            "onboarding-back" => self.onboarding_step(-1, cx),
            "update-check" => self.check_for_updates(true, cx),
            _ => return false,
        }
        true
    }
}
