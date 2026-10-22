//! Asks every region and channel for one model and gathers the builds.
//!
//! The combinations follow the quick search of NeFeroN's LinkResolverDownloader:
//! region × model variant × channel, a fake oldest build as the current one,
//! one request at a time per server.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, bail};
use tracing::debug;

use crate::build::Build;
use crate::protocol::{self, Answer, Cert, Mode, Query};
use crate::regions::Region;

/// The release channels: A stable, C/F/H regional and carrier lines.
pub const CHANNELS: [char; 4] = ['A', 'C', 'F', 'H'];

#[derive(Clone, Debug)]
pub struct Search {
    /// e.g. `CPH2747`; a regional suffix (`EEA`, `IN`) is added per region.
    pub model: String,
    pub regions: Vec<Region>,
    pub channels: Vec<char>,
}

impl Search {
    pub fn new(model: &str) -> Search {
        Search {
            model: model.trim().to_uppercase(),
            regions: Region::all().collect(),
            channels: CHANNELS.to_vec(),
        }
    }

    fn queries(&self) -> Vec<Query> {
        let mut out = Vec::new();
        for &region in &self.regions {
            let mut models = vec![region.model(&self.model)];
            if !models.contains(&self.model) {
                models.push(self.model.clone());
            }
            for model in models {
                for &channel in &self.channels {
                    out.push(Query {
                        ota_version: format!("{}_11.{channel}", self.model),
                        model: Some(model.clone()),
                        region,
                        mode: Mode::Manual,
                    });
                }
            }
        }
        out
    }
}

/// One build and every region that offers it.
#[derive(Clone, Debug)]
pub struct Found {
    pub build: Build,
    pub regions: Vec<Region>,
}

/// Newest first. `progress(done, total)` is called after each request;
/// setting `cancel` stops before the next one.
pub fn search(
    s: &Search,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Vec<Found>> {
    let queries = s.queries();
    let total = queries.len();
    // One queue per host (see `protocol::query_on`), spread evenly.
    let mut queues: HashMap<&'static str, Vec<usize>> = HashMap::new();
    for (i, q) in queries.iter().enumerate() {
        let hosts = q.region.hosts();
        let host = *hosts
            .iter()
            .min_by_key(|h| queues.get(*h).map_or(0, Vec::len))
            .unwrap_or(&hosts[0]);
        queues.entry(host).or_default().push(i);
    }
    let done = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let last_error = Mutex::new(None);
    let builds = Mutex::new(Vec::<(usize, Region, Build)>::new());

    std::thread::scope(|scope| {
        for (host, queue) in &queues {
            let (queries, done, failures, last_error, builds) =
                (&queries, &done, &failures, &last_error, &builds);
            scope.spawn(move || {
                let mut cert = None;
                for &i in queue {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let q = &queries[i];
                    match ask(host, q, &mut cert) {
                        Ok(Some(b)) => builds.lock().unwrap().push((i, q.region, b)),
                        Ok(None) => {}
                        Err(e) => {
                            debug!("{} {:?} on {host}: {e:#}", q.ota_version, q.region);
                            failures.fetch_add(1, Ordering::SeqCst);
                            *last_error.lock().unwrap() = Some(e);
                        }
                    }
                    progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
                }
            });
        }
    });

    let mut builds = builds.into_inner().unwrap();
    if builds.is_empty()
        && failures.load(Ordering::SeqCst) == done.load(Ordering::SeqCst)
        && let Some(e) = last_error.into_inner().unwrap()
    {
        bail!("{e:#}");
    }
    // Requests finish in any order; group as if they ran one by one.
    builds.sort_by_key(|(i, _, _)| *i);
    Ok(group(builds.into_iter().map(|(_, r, b)| (r, b))))
}

fn ask(host: &str, q: &Query, cert: &mut Option<Cert>) -> Result<Option<Build>> {
    if cert.is_none() {
        *cert = Some(protocol::fetch_cert(host)?);
    }
    let cert = cert.as_ref().unwrap();
    let mut answer = protocol::query_on(host, q, cert)?;
    // Should a 500 still come, once more is enough.
    if matches!(answer, Answer::Refused { code: 500, .. }) {
        answer = protocol::query_on(host, q, cert)?;
    }
    Ok(match answer {
        Answer::Build(body) => Build::from_answer(&body),
        Answer::Nothing => None,
        Answer::Refused { code, message } => {
            debug!("{} {:?}: refused {code} {message}", q.ota_version, q.region);
            None
        }
    })
}

/// The same package (by MD5, else by version) found in several places is
/// one build; regions keep the order they were asked in.
pub fn group(found: impl IntoIterator<Item = (Region, Build)>) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for (region, build) in found {
        let key = identity(&build);
        match out.iter_mut().find(|f| identity(&f.build) == key) {
            Some(f) => {
                if !f.regions.contains(&region) {
                    f.regions.push(region);
                }
                for l in build.links {
                    if !f.build.links.contains(&l) {
                        f.build.links.push(l);
                    }
                }
            }
            None => out.push(Found {
                build,
                regions: vec![region],
            }),
        }
    }
    out.sort_by(|a, b| {
        b.build.published.cmp(&a.build.published).then_with(|| {
            b.build
                .ota_version
                .to_lowercase()
                .cmp(&a.build.ota_version.to_lowercase())
        })
    });
    out
}

fn identity(b: &Build) -> String {
    match &b.md5 {
        Some(m) => m.clone(),
        None => format!("{}|{}", b.version, b.ota_version),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(version: &str, md5: Option<&str>, published: i64, link: &str) -> Build {
        Build {
            version: version.into(),
            ota_version: version.into(),
            android: None,
            os: None,
            security_patch: None,
            published: Some(published),
            size: None,
            md5: md5.map(Into::into),
            changelog_url: None,
            links: vec![link.into()],
        }
    }

    #[test]
    fn regional_variants_are_asked_too() {
        let s = Search {
            model: "CPH2747".into(),
            regions: vec![Region::EU, Region::GL],
            channels: vec!['A'],
        };
        let q = s.queries();
        let models: Vec<_> = q.iter().map(|q| q.model.clone().unwrap()).collect();
        assert_eq!(models, ["CPH2747EEA", "CPH2747", "CPH2747"]);
        assert!(q.iter().all(|q| q.ota_version == "CPH2747_11.A"));
    }

    #[test]
    fn groups_by_package_and_sorts_newest_first() {
        let found = group([
            (Region::GL, build("v1", Some("aa"), 100, "https://a/1")),
            (Region::EU, build("v2", Some("bb"), 200, "https://b/1")),
            (Region::CN, build("v1", Some("aa"), 100, "https://a/2")),
            (Region::GL, build("v1", Some("aa"), 100, "https://a/1")),
        ]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].build.version, "v2");
        assert_eq!(found[1].regions, [Region::GL, Region::CN]);
        assert_eq!(found[1].build.links, ["https://a/1", "https://a/2"]);
    }

    #[test]
    fn same_version_different_package_stays_apart() {
        let found = group([
            (Region::GL, build("v1", Some("aa"), 100, "https://a")),
            (Region::EU, build("v1", Some("bb"), 100, "https://b")),
        ]);
        assert_eq!(found.len(), 2);
    }
}
