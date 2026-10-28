//! Optional update check against the GitHub releases of [`identity::UPDATE_REPO`].
//!
//! Empty repo (the default) = no network request and no UI. Otherwise the latest
//! release is fetched once per launch (and on "Check now" in Settings → About); a
//! newer version shows a notification and a download link. Nothing is downloaded or
//! installed automatically. `MUSIC_UPDATE_REPO=owner/name` overrides the repo for
//! testing.

use gpui::{App, Context, Global};
use gpui_component::WindowExt as _;
use serde_json::Value;

use crate::app::MusicApp;
use crate::identity;
use crate::util::off_thread;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available {
        version: String,
        url: String,
    },
    Failed(String),
}

#[derive(Default)]
struct UpdateState(UpdateStatus);
impl Global for UpdateState {}

/// The repository checked (`owner/name`), if the feature is enabled.
pub fn repo() -> Option<String> {
    std::env::var("MUSIC_UPDATE_REPO")
        .ok()
        .filter(|r| !r.trim().is_empty())
        .or_else(|| Some(identity::UPDATE_REPO.to_owned()))
        .filter(|r| r.contains('/'))
}

pub fn status(cx: &App) -> UpdateStatus {
    cx.try_global::<UpdateState>()
        .map(|s| s.0.clone())
        .unwrap_or_default()
}

/// `1.2.10` > `1.2.9`; a leading `v` and pre-release suffixes are ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim()
            .trim_start_matches(['v', 'V'])
            .split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(candidate), parse(current));
    let len = a.len().max(b.len());
    let get = |v: &Vec<u64>, i: usize| v.get(i).copied().unwrap_or(0);
    (0..len)
        .map(|i| get(&a, i).cmp(&get(&b, i)))
        .find(|o| o.is_ne())
        .is_some_and(|o| o.is_gt())
}

/// `(version, page url)` of a `releases/latest` response.
pub fn parse_release(json: &Value) -> Option<(String, String)> {
    if json.get("draft").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let tag = json.get("tag_name")?.as_str()?.to_owned();
    let url = json
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some((tag.trim_start_matches(['v', 'V']).to_owned(), url))
}

impl MusicApp {
    /// Fetches the latest release (no-op without a repo). `manual`: also report
    /// "up to date" and errors.
    pub fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        let Some(repo) = repo() else { return };
        if status(cx) == UpdateStatus::Checking {
            return;
        }
        cx.set_global(UpdateState(UpdateStatus::Checking));
        cx.notify();
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        cx.spawn(async move |this, cx| {
            let result = off_thread(move || -> Result<(String, String), String> {
                let bytes = music_core::fetch_bytes(&url).map_err(|e| e.to_string())?;
                let json: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                parse_release(&json).ok_or_else(|| t!("update.no_release").to_owned())
            })
            .await;
            this.update(cx, |this, cx| {
                let window = this.main_window;
                let notify_app = |cx: &mut Context<MusicApp>, message: String| {
                    if let Some(window) = window {
                        cx.defer(move |cx| {
                            window
                                .update(cx, |_, window, cx| window.push_notification(message, cx))
                                .ok();
                        });
                    }
                };
                let status = match result {
                    Ok((version, url)) if is_newer(&version, identity::VERSION) => {
                        notify_app(cx, t!("update.available_toast", version = version));
                        UpdateStatus::Available { version, url }
                    }
                    Ok(_) => {
                        if manual {
                            notify_app(cx, t!("update.up_to_date_toast").to_owned());
                        }
                        UpdateStatus::UpToDate
                    }
                    Err(e) => UpdateStatus::Failed(e),
                };
                cx.set_global(UpdateState(status));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta.1", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn parses_a_release() {
        let json = serde_json::json!({
            "tag_name": "v0.3.1",
            "html_url": "https://github.com/o/r/releases/tag/v0.3.1",
            "draft": false,
        });
        assert_eq!(
            parse_release(&json),
            Some((
                "0.3.1".into(),
                "https://github.com/o/r/releases/tag/v0.3.1".into()
            ))
        );
        assert_eq!(
            parse_release(&serde_json::json!({"message": "Not Found"})),
            None
        );
    }
}
