//! Asks every region and channel for one model and gathers the builds.
//!
//! The combinations follow the quick search of NeFeroN's LinkResolverDownloader:
//! region × model variant × channel, a fake oldest build as the current one.
//! Each server takes a few requests at a time, each under its own device id.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, bail};
use tracing::debug;

use crate::build::Build;
use crate::protocol::{self, Answer, Cert, Mode, Query};
use crate::regions::Region;

/// The channel letter follows the Android version: A 13, C 14, F 15, H 16.
pub const CHANNELS: [char; 4] = ['A', 'C', 'F', 'H'];

/// Requests in flight at once, spread over the servers that answer.
const WORKERS: usize = 24;

#[derive(Clone, Debug)]
pub struct Search {
    /// e.g. `CPH2747`; regional variants (`CPH2747EEA`, `RMX3709TR`,
    /// `RMX3709EX`) are asked too.
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
            // As the region's phones name themselves; "EX" (export) is what
            // MEA, Malaysia or Saudi phones answer to; the plain code finds
            // older builds the others no longer offer.
            let mut models = vec![region.model(&self.model)];
            if !self.model.ends_with("EX") {
                models.push(format!("{}EX", self.model));
            }
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
    /// Offered only as early access ("taste"): a beta, often of the next
    /// Android version.
    pub beta: bool,
}

/// Rounds of "and after that?": a new Android version is offered only as an
/// update from the newest build of the one before.
const FOLLOW_UP: usize = 3;

/// Newest first. `progress(done, total)` is called after each request — the
/// total grows when found builds are asked about; setting `cancel` stops
/// before the next one.
pub fn search(
    s: &Search,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Vec<Found>> {
    let mut queries = s.queries();
    let mut asked: HashSet<(String, Option<String>, Region, Mode)> =
        queries.iter().map(key).collect();
    let done = AtomicUsize::new(0);
    let total = AtomicUsize::new(queries.len());
    // (order, region, build, beta), order = round then position
    let mut found: Vec<(usize, Region, Build, bool)> = Vec::new();
    let certs = certificates(&queries);

    for round in 0..=FOLLOW_UP {
        let t = std::time::Instant::now();
        let (builds, failures, last_error) = run(&queries, &certs, cancel, &done, &total, progress);
        debug!(
            "round {round}: {} queries, {} builds, {:?}",
            queries.len(),
            builds.len(),
            t.elapsed()
        );
        if round == 0
            && builds.is_empty()
            && failures == queries.len()
            && let Some(e) = last_error
        {
            bail!("{e:#}");
        }
        // Only from the newest build each region and model name gave: older
        // ones lead to builds already found.
        let mut newest: HashMap<(Region, Option<String>), &Build> = HashMap::new();
        for (i, b) in &builds {
            let q = &queries[*i];
            let slot = newest.entry((q.region, q.model.clone())).or_insert(b);
            if (b.published, &b.ota_version) > (slot.published, &slot.ota_version) {
                *slot = b;
            }
        }
        let mut next = Vec::new();
        for (i, b) in &builds {
            let q = &queries[*i];
            if !std::ptr::eq(newest[&(q.region, q.model.clone())], b) {
                continue;
            }
            // Asked from this build as the one installed.
            for mode in [Mode::Manual, Mode::Taste] {
                let follow = Query {
                    ota_version: b.ota_version.clone(),
                    model: q.model.clone(),
                    region: q.region,
                    mode,
                };
                if b.ota_version.split('_').count() >= 3 && asked.insert(key(&follow)) {
                    next.push(follow);
                }
            }
        }
        found.extend(builds.into_iter().map(|(i, b)| {
            (
                round * 100_000 + i,
                queries[i].region,
                b,
                queries[i].mode == Mode::Taste,
            )
        }));
        if next.is_empty() || cancel.load(Ordering::SeqCst) {
            break;
        }
        total.fetch_add(next.len(), Ordering::SeqCst);
        queries = next;
    }
    // Requests finish in any order; group as if they ran one by one.
    found.sort_by_key(|(i, ..)| *i);
    Ok(group(found.into_iter().map(|(_, r, b, beta)| (r, b, beta))))
}

fn key(q: &Query) -> (String, Option<String>, Region, Mode) {
    (q.ota_version.clone(), q.model.clone(), q.region, q.mode)
}

/// Each server's certificate once, all at the same time: a server that does
/// not answer is left out instead of timing out every request.
fn certificates(queries: &[Query]) -> HashMap<&'static str, Cert> {
    let mut wanted: Vec<&'static str> = queries
        .iter()
        .flat_map(|q| q.region.hosts().iter().copied())
        .collect();
    wanted.sort();
    wanted.dedup();
    let certs: HashMap<&'static str, Cert> = std::thread::scope(|scope| {
        let handles: Vec<_> = wanted
            .iter()
            .map(|&h| {
                (
                    h,
                    // Three times: a server with an address that hangs fails
                    // one of them and is left out while another answers.
                    scope.spawn(move || {
                        let c = protocol::fetch_cert(h)?;
                        protocol::fetch_cert(h)?;
                        protocol::fetch_cert(h)?;
                        Ok::<_, anyhow::Error>(c)
                    }),
                )
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|(h, j)| match j.join() {
                Ok(Ok(c)) => Some((h, c)),
                Ok(Err(e)) => {
                    debug!("{h} left out: {e:#}");
                    None
                }
                Err(_) => None,
            })
            .collect()
    });
    // Should every server of a region have failed, the region still gets
    // its first one, slow or not.
    let mut certs = certs;
    for q in queries {
        let hosts = q.region.hosts();
        if !hosts.iter().any(|h| certs.contains_key(h))
            && let Ok(c) = protocol::fetch_cert(hosts[0])
        {
            certs.insert(hosts[0], c);
        }
    }
    certs
}

/// Asks every query: one queue per host, spread evenly, each drained by
/// WORKERS (shared) under device ids of their own. Returns the builds by
/// query index, the number of failed requests and the last failure.
fn run(
    queries: &[Query],
    certs: &HashMap<&'static str, Cert>,
    cancel: &AtomicBool,
    done: &AtomicUsize,
    total: &AtomicUsize,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> (Vec<(usize, Build)>, usize, Option<anyhow::Error>) {
    let mut queues: HashMap<&'static str, Vec<usize>> = HashMap::new();
    let mut unreachable = 0;
    for (i, q) in queries.iter().enumerate() {
        let host = q
            .region
            .hosts()
            .iter()
            .filter(|h| certs.contains_key(*h))
            .min_by_key(|h| queues.get(*h).map_or(0, Vec::len));
        match host {
            Some(&h) => queues.entry(h).or_default().push(i),
            None => unreachable += 1,
        }
    }
    if unreachable > 0 {
        progress(
            done.fetch_add(unreachable, Ordering::SeqCst) + unreachable,
            total.load(Ordering::SeqCst),
        );
    }
    let failures = AtomicUsize::new(unreachable);
    let last_error =
        Mutex::new((unreachable > 0).then(|| anyhow::anyhow!("the update servers do not answer")));
    let builds = Mutex::new(Vec::<(usize, Build)>::new());

    std::thread::scope(|scope| {
        let mut worker = 0u32;
        for (host, queue) in &queues {
            let next = std::sync::Arc::new(AtomicUsize::new(0));
            let per_host = (WORKERS / queues.len().max(1)).max(4);
            for _ in 0..per_host.min(queue.len()) {
                let device = protocol::device_id(worker);
                worker += 1;
                let next = next.clone();
                let (failures, last_error, builds) = (&failures, &last_error, &builds);
                let cert = &certs[host];
                scope.spawn(move || {
                    loop {
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&i) = queue.get(k) else { break };
                        if cancel.load(Ordering::SeqCst) {
                            break;
                        }
                        let q = &queries[i];
                        match ask(host, &device, q, cert) {
                            Ok(Some(b)) => builds.lock().unwrap().push((i, b)),
                            Ok(None) => {}
                            Err(e) => {
                                debug!("{} {:?} on {host}: {e:#}", q.ota_version, q.region);
                                failures.fetch_add(1, Ordering::SeqCst);
                                *last_error.lock().unwrap() = Some(e);
                            }
                        }
                        progress(
                            done.fetch_add(1, Ordering::SeqCst) + 1,
                            total.load(Ordering::SeqCst),
                        );
                    }
                });
            }
        }
    });
    (
        builds.into_inner().unwrap(),
        failures.into_inner(),
        last_error.into_inner().unwrap(),
    )
}

fn ask(host: &str, device: &str, q: &Query, cert: &Cert) -> Result<Option<Build>> {
    let mut answer = match protocol::query_on(host, q, cert, device) {
        Ok(a) => a,
        Err(e) => {
            debug!("{} {:?}: {e:#}, once more", q.ota_version, q.region);
            protocol::query_on(host, q, cert, device)?
        }
    };
    // Should a 500 still come, once more is enough.
    if matches!(answer, Answer::Refused { code: 500, .. }) {
        answer = protocol::query_on(host, q, cert, device)?;
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
/// A build seen in a normal answer anywhere is not a beta.
pub fn group(found: impl IntoIterator<Item = (Region, Build, bool)>) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for (region, build, beta) in found {
        let key = identity(&build);
        match out.iter_mut().find(|f| identity(&f.build) == key) {
            Some(f) => {
                f.beta &= beta;
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
                beta,
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
            model: "RMX3709".into(),
            regions: vec![Region::parse("TR").unwrap(), Region::GL],
            channels: vec!['H'],
        };
        let q = s.queries();
        let models: Vec<_> = q.iter().map(|q| q.model.clone().unwrap()).collect();
        assert_eq!(
            models,
            ["RMX3709TR", "RMX3709EX", "RMX3709", "RMX3709", "RMX3709EX"]
        );
        assert!(q.iter().all(|q| q.ota_version == "RMX3709_11.H"));
    }

    #[test]
    fn groups_by_package_and_sorts_newest_first() {
        let found = group([
            (
                Region::GL,
                build("v1", Some("aa"), 100, "https://a/1"),
                false,
            ),
            (
                Region::EU,
                build("v2", Some("bb"), 200, "https://b/1"),
                false,
            ),
            (
                Region::CN,
                build("v1", Some("aa"), 100, "https://a/2"),
                false,
            ),
            (
                Region::GL,
                build("v1", Some("aa"), 100, "https://a/1"),
                false,
            ),
        ]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].build.version, "v2");
        assert_eq!(found[1].regions, [Region::GL, Region::CN]);
        assert_eq!(found[1].build.links, ["https://a/1", "https://a/2"]);
    }

    #[test]
    fn a_beta_seen_normally_is_no_beta() {
        let found = group([
            (Region::IN, build("v17", Some("cc"), 300, "https://c"), true),
            (
                Region::GL,
                build("v17b", Some("dd"), 300, "https://d"),
                true,
            ),
            (
                Region::EU,
                build("v17b", Some("dd"), 300, "https://d"),
                false,
            ),
        ]);
        let beta = |v: &str| found.iter().find(|f| f.build.version == v).unwrap().beta;
        assert!(beta("v17"));
        assert!(!beta("v17b"));
    }

    #[test]
    fn same_version_different_package_stays_apart() {
        let found = group([
            (Region::GL, build("v1", Some("aa"), 100, "https://a"), false),
            (Region::EU, build("v1", Some("bb"), 100, "https://b"), false),
        ]);
        assert_eq!(found.len(), 2);
    }
}
