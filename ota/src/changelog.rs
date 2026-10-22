//! "What's new": the page behind a build's `panelUrl`, as plain text.

use anyhow::{Context, Result, bail};

pub fn fetch(agent: &ureq::Agent, url: &str) -> Result<String> {
    let mut resp = agent.get(url).call().context("changelog: no answer")?;
    if !resp.status().is_success() {
        bail!("changelog: HTTP {}", resp.status());
    }
    let html = resp.body_mut().read_to_string()?;
    Ok(parse(&html))
}

/// Each `<dt>` heading with its `<dd>` text, separated by blank lines.
/// The page's fixed notes ("Must-knows") are left out.
pub fn parse(html: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<dt") {
        rest = &rest[start..];
        let Some(end) = rest.find("</dl>") else { break };
        let block = &rest[..end];
        rest = &rest[end..];
        for (tag, item) in items(block) {
            let text = text(item);
            if text.is_empty() {
                continue;
            }
            match (tag, parts.last_mut()) {
                ("dd", Some(last)) => {
                    last.push('\n');
                    last.push_str(&text);
                }
                _ => parts.push(text),
            }
        }
    }
    parts.join("\n\n")
}

/// The `<dt>` / `<dd>` elements of one `<dl>`, in order.
fn items(block: &str) -> Vec<(&'static str, &str)> {
    let mut out = Vec::new();
    let mut rest = block;
    loop {
        let dt = rest.find("<dt");
        let dd = rest.find("<dd");
        let (tag, at) = match (dt, dd) {
            (Some(a), Some(b)) if a < b => ("dt", a),
            (_, Some(b)) => ("dd", b),
            (Some(a), None) => ("dt", a),
            (None, None) => break,
        };
        let after = &rest[at..];
        let Some(open_end) = after.find('>') else {
            break;
        };
        let close = format!("</{tag}>");
        let Some(close_at) = after.find(&close) else {
            break;
        };
        out.push((tag, &after[open_end + 1..close_at]));
        rest = &after[close_at + close.len()..];
    }
    out
}

/// Strips tags; `<br>`, `</li>`, `</div>` and `</p>` become line breaks.
fn text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let Some(gt) = rest[lt..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim().to_ascii_lowercase();
        if tag.starts_with("br") || ["/li", "/div", "/p"].contains(&tag.as_str()) {
            out.push('\n');
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(rest);
    let out = unescape(&out);
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn unescape(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_oneplus_page() {
        let text = parse(include_str!("../testdata/panel-cph2747-gl.html"));
        assert!(text.starts_with("System\n"), "{text}");
        assert!(text.contains("September 2026 Android security patch"));
        assert!(!text.contains("Must-knows"));
    }

    #[test]
    fn headings_group_their_items() {
        let html = "<dl><dt>Camera</dt><dd>Sharper&nbsp;photos<br>Faster focus</dd>\
                    <dd>Less noise</dd><dt></dt><dd></dd></dl>\
                    <dl><dt>Battery</dt><dd>Lasts &amp; charges</dd></dl>";
        assert_eq!(
            parse(html),
            "Camera\nSharper photos\nFaster focus\nLess noise\n\nBattery\nLasts & charges"
        );
    }
}
