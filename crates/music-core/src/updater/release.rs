//! GitHub releases (the `GET /repos/{owner}/{repo}/releases` response) and which
//! one is offered.

use serde_json::Value;

use super::sums::SUMS_FILE;
use super::version::Version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    /// `browser_download_url`.
    pub url: String,
    /// Bytes, as the API reports them (0 = unknown).
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    /// Marked "pre-release" on GitHub.
    pub prerelease: bool,
    pub draft: bool,
    /// The release page (`html_url`).
    pub page_url: String,
    pub assets: Vec<Asset>,
}

impl Release {
    /// One release object; `None` without a tag that parses as a version.
    pub fn from_json(json: &Value) -> Option<Self> {
        let tag = json.get("tag_name")?.as_str()?.to_owned();
        let version = Version::parse(&tag)?;
        let text = |key: &str| {
            json.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let assets = json
            .get("assets")
            .and_then(Value::as_array)
            .map(|assets| {
                assets
                    .iter()
                    .filter_map(|a| {
                        Some(Asset {
                            name: a.get("name")?.as_str()?.to_owned(),
                            url: a.get("browser_download_url")?.as_str()?.to_owned(),
                            size: a.get("size").and_then(Value::as_u64).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Release {
            tag,
            version,
            prerelease: json.get("prerelease").and_then(Value::as_bool) == Some(true),
            draft: json.get("draft").and_then(Value::as_bool) == Some(true),
            page_url: text("html_url"),
            assets,
        })
    }

    /// A list response (an array) or a single release (`releases/latest`).
    pub fn list_from_json(json: &Value) -> Vec<Self> {
        match json {
            Value::Array(items) => items.iter().filter_map(Release::from_json).collect(),
            other => Release::from_json(other).into_iter().collect(),
        }
    }

    /// Counts as a pre-release: marked as one, or a pre-release version.
    pub fn is_prerelease(&self) -> bool {
        self.prerelease || self.version.is_prerelease()
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// `SHA256SUMS.txt`.
    pub fn sums(&self) -> Option<&Asset> {
        self.asset(SUMS_FILE)
            .or_else(|| self.asset("SHA256SUMS"))
            .or_else(|| self.asset("sha256sums.txt"))
    }

    /// The package `<app_id>-<version><suffix>`. If the version in the name doesn't
    /// match the tag, the one asset `<app_id>-…<suffix>` is taken (never a guess
    /// between several).
    pub fn package(&self, app_id: &str, suffix: &str) -> Option<&Asset> {
        let exact = format!("{app_id}-{}{suffix}", self.version);
        if let Some(asset) = self.asset(&exact) {
            return Some(asset);
        }
        let prefix = format!("{app_id}-");
        let mut candidates = self
            .assets
            .iter()
            .filter(|a| a.name.starts_with(&prefix) && a.name.ends_with(suffix));
        match (candidates.next(), candidates.next()) {
            (Some(one), None) => Some(one),
            _ => None,
        }
    }
}

/// The newest published release above `current`. Pre-releases (marked on GitHub
/// or by version) only with `include_prereleases`; drafts never.
pub fn pick_release<'a>(
    releases: &'a [Release],
    current: &Version,
    include_prereleases: bool,
) -> Option<&'a Release> {
    releases
        .iter()
        .filter(|r| !r.draft && r.version > *current)
        .filter(|r| include_prereleases || !r.is_prerelease())
        .max_by(|a, b| a.version.cmp(&b.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, prerelease: bool, draft: bool) -> Value {
        serde_json::json!({
            "tag_name": tag,
            "prerelease": prerelease,
            "draft": draft,
            "html_url": format!("https://github.com/o/r/releases/tag/{tag}"),
            "assets": [
                {"name": format!("cratetune-{}-macos-arm64.app.zip", tag.trim_start_matches('v')),
                 "browser_download_url": format!("https://github.com/o/r/releases/download/{tag}/a.zip"),
                 "size": 1234},
                {"name": "SHA256SUMS.txt",
                 "browser_download_url": format!("https://github.com/o/r/releases/download/{tag}/SHA256SUMS.txt"),
                 "size": 10},
                {"name": "broken"}
            ]
        })
    }

    #[test]
    fn parses_the_api_response() {
        let json = serde_json::json!([
            release("v0.3.0", false, false),
            {"tag_name": "nightly"},
            {"message": "no tag"},
        ]);
        let list = Release::list_from_json(&json);
        assert_eq!(list.len(), 1);
        let r = &list[0];
        assert_eq!(r.version, Version::new(0, 3, 0));
        assert_eq!(r.assets.len(), 2);
        assert_eq!(r.assets[0].size, 1234);
        assert_eq!(r.sums().unwrap().name, "SHA256SUMS.txt");
        assert_eq!(
            r.package("cratetune", "-macos-arm64.app.zip").unwrap().name,
            "cratetune-0.3.0-macos-arm64.app.zip"
        );
        assert!(r.package("cratetune", "-windows-x64.zip").is_none());
        // A single object (`releases/latest`) works too.
        assert_eq!(
            Release::list_from_json(&release("v1.0.0", false, false)).len(),
            1
        );
    }

    #[test]
    fn package_falls_back_only_to_a_single_match() {
        let mut r = Release::from_json(&release("v0.3.0", false, false)).unwrap();
        r.assets[0].name = "cratetune-0.3.0-hotfix-macos-arm64.app.zip".into();
        assert!(r.package("cratetune", "-macos-arm64.app.zip").is_some());
        r.assets.push(Asset {
            name: "cratetune-0.3.1-macos-arm64.app.zip".into(),
            url: String::new(),
            size: 0,
        });
        assert!(r.package("cratetune", "-macos-arm64.app.zip").is_none());
        // Another app's asset doesn't count.
        assert!(r.package("other", "-macos-arm64.app.zip").is_none());
    }

    #[test]
    fn picks_by_channel() {
        let json = serde_json::json!([
            release("v0.1.0", true, false),
            release("v0.2.0-beta.2", false, false),
            release("v0.1.5", false, false),
            release("v0.9.0", false, true),
            release("v0.2.0-beta.10", true, false),
        ]);
        let list = Release::list_from_json(&json);
        let pick = |current: &str, pre: bool| {
            pick_release(&list, &Version::parse(current).unwrap(), pre).map(|r| r.tag.as_str())
        };
        // Drafts are never offered; beta.10 > beta.2 numerically.
        assert_eq!(pick("0.1.0", true), Some("v0.2.0-beta.10"));
        // Stable only: 0.1.5 (v0.1.0 is marked pre-release on GitHub).
        assert_eq!(pick("0.0.9", false), Some("v0.1.5"));
        assert_eq!(pick("0.1.5", false), None);
        assert_eq!(pick("0.2.0-beta.10", true), None);
        assert_eq!(pick("0.2.0-beta.2", true), Some("v0.2.0-beta.10"));
        assert_eq!(pick("1.0.0", true), None);
    }
}
