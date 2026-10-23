use std::sync::atomic::AtomicBool;

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use lfff_ota::{Found, Region, Search, changelog};
use serde_json::json;

pub fn run(
    model: &str,
    regions: Option<&str>,
    channels: Option<&str>,
    latest: bool,
    as_json: bool,
    show_changes: bool,
) -> i32 {
    let mut search = Search::new(model);
    if let Some(list) = regions {
        let mut picked = Vec::new();
        for code in list.split(',').filter(|c| !c.trim().is_empty()) {
            if code.trim().eq_ignore_ascii_case("all") {
                picked = Region::all().collect();
                break;
            }
            match Region::parse(code) {
                Some(r) => picked.push(r),
                None => {
                    let known: Vec<_> = Region::all().map(|r| r.code()).collect();
                    eprintln!(
                        "{} Unknown region {code:?}; known: {}",
                        "✗".red().bold(),
                        known.join(", ")
                    );
                    return 2;
                }
            }
        }
        search.regions = picked;
    }
    if let Some(list) = channels {
        search.channels = list
            .split(',')
            .filter_map(|c| c.trim().chars().next())
            .map(|c| c.to_ascii_uppercase())
            .collect();
    }

    let pb = ProgressBar::new(0);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} Asking the update servers {pos}/{len}")
            .unwrap()
            .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ "),
    );
    let result = lfff_ota::search(&search, &AtomicBool::new(false), &|done, total| {
        pb.set_length(total as u64);
        pb.set_position(done as u64);
    });
    pb.finish_and_clear();

    let found = match result {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{} {}", "✗".red().bold(), format!("{e:#}").red());
            return 1;
        }
    };
    if found.is_empty() {
        eprintln!(
            "{} No builds found for {}. Check the model code (Settings → About phone → Model).",
            "✗".red().bold(),
            search.model
        );
        return 1;
    }
    let found: &[Found] = if latest { &found[..1] } else { &found };

    if as_json {
        let list: Vec<_> = found.iter().map(to_json).collect();
        let out = if latest { list[0].clone() } else { json!(list) };
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }
    if latest && !show_changes {
        // Bare link, for `lfff download "$(lfff find-ota MODEL --latest)"`.
        match found[0].build.link() {
            Some(link) => {
                println!("{link}");
                return 0;
            }
            None => {
                eprintln!("{} The newest build has no link.", "✗".red().bold());
                return 1;
            }
        }
    }

    for (i, f) in found.iter().enumerate() {
        print_found(f, i == 0);
        if show_changes
            && i == 0
            && let Some(url) = &f.build.changelog_url
        {
            match changelog::fetch(url) {
                Ok(text) if !text.is_empty() => {
                    println!("  {}", "What's new:".dimmed());
                    for line in text.lines() {
                        println!("    {line}");
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("  {} {e:#}", "!".yellow()),
            }
        }
        println!();
    }
    println!(
        "  {} {}",
        "Download:".dimmed(),
        "lfff download <link>".cyan()
    );
    0
}

fn print_found(f: &Found, newest: bool) {
    let b = &f.build;
    let regions: Vec<_> = f.regions.iter().map(|r| r.code()).collect();
    let mut title = b.version.bold().to_string();
    if newest {
        title = format!("{title}  {}", "newest".green());
    }
    if f.beta {
        title = format!("{title}  {}", "beta".yellow());
    }
    println!("{title}");
    let mut facts = vec![regions.join(", ")];
    facts.extend(b.date());
    facts.extend(b.size.map(|s| format!("{:.2} GB", s as f64 / 1e9)));
    facts.extend(b.security_patch.as_ref().map(|p| format!("patch {p}")));
    facts.extend(b.android.clone());
    println!("  {}", facts.join(" · ").dimmed());
    println!("  {} {}", "OTA:".dimmed(), b.ota_version);
    if let Some(md5) = &b.md5 {
        println!("  {} {md5}", "MD5:".dimmed());
    }
    if let Some(link) = b.link() {
        println!("  {} {link}", "Link:".dimmed());
    }
}

fn to_json(f: &Found) -> serde_json::Value {
    let b = &f.build;
    json!({
        "version": b.version,
        "ota_version": b.ota_version,
        "regions": f.regions.iter().map(|r| r.code()).collect::<Vec<_>>(),
        "beta": f.beta,
        "android": b.android,
        "os": b.os,
        "security_patch": b.security_patch,
        "published": b.date(),
        "size": b.size,
        "md5": b.md5,
        "changelog_url": b.changelog_url,
        "links": b.links,
    })
}
