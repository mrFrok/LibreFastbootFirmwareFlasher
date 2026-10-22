//! A build as the update server describes it.
//!
//! Field names differ between servers and years, so each fact is read from
//! the first of several keys, the way NeFeroN's LinkResolverDownloader does.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// e.g. `CPH2747_16.0.10.601(EX01)`
    pub version: String,
    /// e.g. `CPH2747_11.A.48_0480_202609111034`
    pub ota_version: String,
    pub android: Option<String>,
    pub os: Option<String>,
    pub security_patch: Option<String>,
    /// Unix seconds.
    pub published: Option<i64>,
    /// Bytes: the largest size anywhere in the answer, i.e. the full package.
    pub size: Option<u64>,
    pub md5: Option<String>,
    pub changelog_url: Option<String>,
    /// `downloadCheck` links first: they always lead to a fresh CDN link.
    pub links: Vec<String>,
}

impl Build {
    /// `None` when the answer names no version at all.
    pub fn from_answer(body: &Value) -> Option<Build> {
        let version = first_str(
            body,
            &[
                "realVersionName",
                "versionName",
                "version_name",
                "firmwareVersion",
            ],
        );
        let ota_version = first_str(body, &["realOtaVersion", "otaVersion", "real_ota_version"]);
        if version.is_none() && ota_version.is_none() {
            return None;
        }
        let changelog_url = first_str(
            body,
            &[
                "panelUrl",
                "panel_url",
                "changeLogUrl",
                "changelogUrl",
                "changelog_url",
                "changeLog",
                "changelog",
                "descUrl",
                "detailUrl",
            ],
        )
        .filter(|u| is_url(u));

        let mut links = Vec::new();
        collect_links(body, &mut links);
        links.retain(|l| Some(l) != changelog_url.as_ref() && !is_page(l));
        links.dedup();
        let mut seen = Vec::new();
        links.retain(|l| {
            let fresh = !seen.contains(l);
            seen.push(l.clone());
            fresh
        });
        // Stable: the server's order stays within each kind.
        links.sort_by_key(|l| !l.contains("/downloadCheck"));

        Some(Build {
            version: version.clone().or(ota_version.clone()).unwrap_or_default(),
            ota_version: ota_version.or(version).unwrap_or_default(),
            android: first_str(body, &["realAndroidVersion", "androidVersion"]),
            os: first_str(body, &["realOsVersion", "colorOSVersion", "osVersion"]),
            security_patch: first_str(body, &["securityPatch", "security_patch"]),
            published: first_num(body, &["publishedTime", "publishTime", "releaseTime"]).map(|t| {
                if t > 100_000_000_000 {
                    (t / 1000) as i64
                } else {
                    t as i64
                }
            }),
            size: max_num(body, &["size", "packetSize", "fileSize"]),
            md5: first_str(body, &["md5", "md5sum", "checksum"]).map(|m| m.to_lowercase()),
            changelog_url,
            links,
        })
    }

    /// The first link to download, if any.
    pub fn link(&self) -> Option<&str> {
        self.links.first().map(String::as_str)
    }
}

/// Depth-first: the value of the first key, in priority order, found anywhere.
fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        find(v, k, &|x| match x {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

fn first_num(v: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| find(v, k, &as_num))
}

fn max_num(v: &Value, keys: &[&str]) -> Option<u64> {
    let mut max = None;
    walk(v, &mut |key, x| {
        if keys.contains(&key)
            && let Some(n) = as_num(x)
        {
            max = max.max(Some(n));
        }
    });
    max
}

fn as_num(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn find<T>(v: &Value, key: &str, pick: &dyn Fn(&Value) -> Option<T>) -> Option<T> {
    match v {
        Value::Object(map) => map
            .get(key)
            .and_then(pick)
            .or_else(|| map.values().find_map(|c| find(c, key, pick))),
        Value::Array(items) => items.iter().find_map(|c| find(c, key, pick)),
        _ => None,
    }
}

/// Calls `f(key, value)` for every member of every object.
fn walk(v: &Value, f: &mut dyn FnMut(&str, &Value)) {
    match v {
        Value::Object(map) => {
            for (k, c) in map {
                f(k, c);
                walk(c, f);
            }
        }
        Value::Array(items) => items.iter().for_each(|c| walk(c, f)),
        _ => {}
    }
}

fn collect_links(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if is_url(s) => out.push(s.trim().to_string()),
        Value::Object(map) => map.values().for_each(|c| collect_links(c, out)),
        Value::Array(items) => items.iter().for_each(|c| collect_links(c, out)),
        _ => {}
    }
}

fn is_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("https://") || s.starts_with("http://")) && !s.contains(char::is_whitespace)
}

/// A web page, not a file: `….html`, with or without `?query` or `#fragment`.
fn is_page(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.ends_with(".html") || path.ends_with(".htm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(text: &str) -> Build {
        let v: Value = serde_json::from_str(text).unwrap();
        Build::from_answer(&v["body"]).unwrap()
    }

    #[test]
    fn reads_a_oneplus_15_answer() {
        let b = fixture(include_str!("../testdata/cph2747-gl.json"));
        assert_eq!(b.version, "CPH2747_16.0.10.601(EX01)");
        assert_eq!(b.ota_version, "CPH2747_11.A.48_0480_202609111034");
        assert_eq!(b.android.as_deref(), Some("Android 16"));
        assert_eq!(b.os.as_deref(), Some("ColorOS 16.1.0"));
        assert_eq!(b.security_patch.as_deref(), Some("2026-09-01"));
        assert_eq!(b.published, Some(1_791_942_742));
        assert_eq!(b.size, Some(9_277_169_699));
        assert_eq!(b.md5.as_deref(), Some("f18405ee46ef1867265e17928cc88c2d"));
        assert!(
            b.changelog_url
                .as_deref()
                .unwrap()
                .contains("allawnofs.com")
        );
        // url and manualUrl (tr=auto / tr=manual); the about page is dropped.
        assert_eq!(b.links.len(), 2, "{:?}", b.links);
        assert!(b.links.iter().all(|l| l.contains("/downloadCheck?")));
    }

    #[test]
    fn reads_a_realme_answer() {
        let b = fixture(include_str!("../testdata/rmx5010-cn.json"));
        assert!(b.version.starts_with("RMX5010_"));
        assert!(b.link().is_some());
    }

    #[test]
    fn keys_by_priority_and_largest_size() {
        let v = json!({
            "versionName": "old",
            "realVersionName": "new",
            "publishTime": "1700000000",
            "components": [
                { "size": "100", "url": "https://cdn.example/a.zip" },
                { "packetSize": 900, "url": "https://x.example/downloadCheck?c=1" }
            ],
            "description": { "url": "https://h5.example/static/index.html#/about" }
        });
        let b = Build::from_answer(&v).unwrap();
        assert_eq!(b.version, "new");
        assert_eq!(b.ota_version, "new");
        assert_eq!(b.published, Some(1_700_000_000));
        assert_eq!(b.size, Some(900));
        assert_eq!(
            b.links,
            [
                "https://x.example/downloadCheck?c=1",
                "https://cdn.example/a.zip"
            ]
        );
    }

    #[test]
    fn no_version_is_no_build() {
        assert_eq!(Build::from_answer(&json!({ "paramFlag": 0 })), None);
    }
}
