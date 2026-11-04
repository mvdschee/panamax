use std::{borrow::Cow, cmp::Reverse, collections::BTreeMap};

use semver::Version;
use serde::{Deserialize, Serialize};

/// Everything the index says about a crate, plus what the mirror holds of it.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CrateDetail {
	/// As published, case kept.
	pub name: String,
	/// Newest stable version that isn't yanked, or the newest prerelease when there is no
	/// stable one. None when every version is yanked.
	pub latest: Option<String>,
	/// Newest first.
	pub versions: Vec<VersionDetail>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VersionDetail {
	pub vers: String,
	pub yanked: bool,
	/// RFC 3339 publish time from the index.
	pub published: Option<String>,
	pub rust_version: Option<String>,
	pub features: BTreeMap<String, Vec<String>>,
	pub deps: Vec<Dep>,
	/// The file on disk. None when the version is only on crates.io.
	pub file: Option<CachedFile>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Dep {
	/// The crate depended on, also when the manifest renames it.
	pub name: String,
	pub req: String,
	pub kind: String,
	pub optional: bool,
	pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachedFile {
	pub size: u64,
	/// Unix seconds. Set on download and on every serve.
	pub last_used: u64,
}

/// One crate in a search result.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CrateSummary {
	pub name: String,
	pub latest: Option<String>,
	/// Newest first.
	pub versions: Vec<VersionState>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VersionState {
	pub vers: String,
	pub yanked: bool,
	pub cached: bool,
}

/// What the home page shows. Sync writes it, so that opening the page reads no crate files.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Status {
	/// Unix seconds.
	pub written: u64,
	/// Crates in the index.
	pub crates: usize,
	pub disk: DiskStats,
}

/// Totals for the crate files on disk.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct DiskStats {
	pub files: usize,
	pub bytes: u64,
	/// Newest first.
	pub recent: Vec<RecentFile>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct RecentFile {
	pub name: String,
	pub vers: String,
	pub last_used: u64,
}

#[derive(Deserialize)]
struct Line {
	name: String,
	vers: String,
	#[serde(default)]
	deps: Vec<LineDep>,
	#[serde(default)]
	features: BTreeMap<String, Vec<String>>,
	features2: Option<BTreeMap<String, Vec<String>>>,
	#[serde(default)]
	yanked: bool,
	rust_version: Option<String>,
	pubtime: Option<String>,
}

#[derive(Deserialize)]
struct LineDep {
	name: String,
	package: Option<String>,
	req: String,
	kind: Option<String>,
	#[serde(default)]
	optional: bool,
	target: Option<String>,
}

impl From<LineDep> for Dep {
	fn from(dep: LineDep) -> Self {
		Self {
			name: dep.package.unwrap_or(dep.name),
			req: dep.req,
			kind: dep.kind.unwrap_or_else(|| "normal".to_string()),
			optional: dep.optional,
			target: dep.target,
		}
	}
}

impl From<CrateDetail> for CrateSummary {
	fn from(detail: CrateDetail) -> Self {
		Self {
			name: detail.name,
			latest: detail.latest,
			versions: detail
				.versions
				.into_iter()
				.map(|v| VersionState {
					vers: v.vers,
					yanked: v.yanked,
					cached: v.file.is_some(),
				})
				.collect(),
		}
	}
}

/// Names matching `query` from `names`, one per line, best first: exact match, then prefix, then substring,
/// shorter names first within each. `-` and `_` match each other, as on crates.io.
pub fn rank<'a>(names: &'a str, query: &str, limit: usize) -> Vec<&'a str> {
	let query = normalize(query.trim());
	if query.is_empty() {
		return vec![];
	}
	let mut hits: Vec<(u8, &str)> = names
		.lines()
		.filter_map(|name| {
			let norm = normalize(name);
			let tier = if norm == query {
				0
			} else if norm.starts_with(query.as_ref()) {
				1
			} else if norm.contains(query.as_ref()) {
				2
			} else {
				return None;
			};
			Some((tier, name))
		})
		.collect();
	hits.sort_unstable_by_key(|&(tier, name)| (tier, name.len(), name));
	hits.into_iter().take(limit).map(|(_, name)| name).collect()
}

fn normalize(s: &str) -> Cow<'_, str> {
	if s.bytes().any(|b| b == b'_' || b.is_ascii_uppercase()) {
		Cow::Owned(s.to_ascii_lowercase().replace('_', "-"))
	} else {
		Cow::Borrowed(s)
	}
}

/// A crate from its index file. `cached` looks up the file on disk for a version. None
/// when the file has no parsable line.
pub fn detail(file: &str, cached: impl Fn(&str) -> Option<CachedFile>) -> Option<CrateDetail> {
	let mut lines: Vec<Line> = file.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
	let name = lines.first()?.name.clone();
	// unparsable versions sort last
	lines.sort_by_cached_key(|line| Reverse(Version::parse(&line.vers).ok()));

	let is_stable = |line: &&Line| Version::parse(&line.vers).is_ok_and(|v| v.pre.is_empty());
	let latest = lines
		.iter()
		.filter(|line| !line.yanked)
		.find(is_stable)
		.or_else(|| lines.iter().find(|line| !line.yanked))
		.map(|line| line.vers.clone());

	let versions = lines
		.into_iter()
		.map(|line| {
			let file = cached(&line.vers);
			let mut features = line.features;
			features.extend(line.features2.unwrap_or_default());
			VersionDetail {
				vers: line.vers,
				yanked: line.yanked,
				published: line.pubtime,
				rust_version: line.rust_version,
				features,
				deps: line.deps.into_iter().map(Dep::from).collect(),
				file,
			}
		})
		.collect();
	Some(CrateDetail {
		name,
		latest,
		versions,
	})
}

impl DiskStats {
	/// Count one file, keeping only the `limit` most recently used in `recent`, so the
	/// memory this takes doesn't depend on how many files there are.
	pub fn add(&mut self, file: RecentFile, size: u64, limit: usize) {
		self.files += 1;
		self.bytes += size;
		let at = self.recent.partition_point(|f| f.last_used >= file.last_used);
		if at < limit {
			self.recent.insert(at, file);
			self.recent.truncate(limit);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn names(list: &[&str]) -> String {
		list.join("\n")
	}

	#[test]
	fn rank_exact_then_prefix_then_substring() {
		let names = names(&["serde_json", "serde", "my-serde", "serde-x", "tokio"]);
		assert_eq!(rank(&names, "serde", 10), ["serde", "serde-x", "serde_json", "my-serde"]);
	}

	#[test]
	fn rank_treats_dash_and_underscore_alike() {
		let names = names(&["serde_json", "foo-bar"]);
		assert_eq!(rank(&names, "serde-json", 10), ["serde_json"]);
		assert_eq!(rank(&names, "Foo_Bar", 10), ["foo-bar"]);
	}

	#[test]
	fn rank_limits_and_ignores_empty_query() {
		let names = names(&["a1", "a2", "a3"]);
		assert_eq!(rank(&names, "a", 2).len(), 2);
		assert!(rank(&names, "  ", 10).is_empty());
	}

	fn line(vers: &str, yanked: bool) -> String {
		format!(
			r#"{{"name":"Demo","vers":"{vers}","deps":[],"cksum":"aa","features":{{}},"yanked":{yanked}}}"#
		)
	}

	fn vers(detail: &CrateDetail) -> Vec<&str> {
		detail.versions.iter().map(|v| v.vers.as_str()).collect()
	}

	#[test]
	fn detail_sorts_newest_first_and_marks_cached() {
		let file = [line("1.2.0", false), line("1.10.0", false), line("junk", false)].join("\n");
		let on_disk = CachedFile {
			size: 7,
			last_used: 9,
		};
		let found = detail(&file, |v| (v == "1.2.0").then(|| on_disk.clone())).unwrap();
		assert_eq!(found.name, "Demo");
		assert_eq!(vers(&found), ["1.10.0", "1.2.0", "junk"]);
		assert_eq!(found.versions[1].file, Some(on_disk));
		assert_eq!(found.versions[0].file, None);
	}

	#[test]
	fn latest_prefers_stable_and_skips_yanked() {
		let latest = |lines: &[String]| detail(&lines.join("\n"), |_| None).unwrap().latest;
		let stable = [line("1.0.0", false), line("2.0.0-rc.1", false), line("3.0.0", true)];
		assert_eq!(latest(&stable).as_deref(), Some("1.0.0"));
		let only_pre = [line("2.0.0-rc.1", false), line("2.0.0-rc.2", false)];
		assert_eq!(latest(&only_pre).as_deref(), Some("2.0.0-rc.2"));
		assert_eq!(latest(&[line("1.0.0", true)]), None);
		assert_eq!(detail("", |_| None), None);
	}

	#[test]
	fn detail_merges_features2_and_resolves_renamed_deps() {
		let file = r#"{"name":"Demo","vers":"1.0.0","yanked":false,"v":2,
			"features":{"a":[]},"features2":{"b":["dep:x"]},"pubtime":"2026-07-18T23:05:13Z",
			"rust_version":"1.56","deps":[
				{"name":"alias","package":"real","req":"^1","optional":true,"target":"cfg(unix)","kind":"dev"},
				{"name":"plain","req":"^2"}
			]}"#
		.replace(['\n', '\t'], "");
		let found = detail(&file, |_| None).unwrap();
		let version = &found.versions[0];
		assert_eq!(version.published.as_deref(), Some("2026-07-18T23:05:13Z"));
		assert_eq!(version.rust_version.as_deref(), Some("1.56"));
		assert_eq!(version.features.keys().collect::<Vec<_>>(), ["a", "b"]);
		assert_eq!(
			version.deps,
			[
				Dep {
					name: "real".into(),
					req: "^1".into(),
					kind: "dev".into(),
					optional: true,
					target: Some("cfg(unix)".into()),
				},
				Dep {
					name: "plain".into(),
					req: "^2".into(),
					kind: "normal".into(),
					optional: false,
					target: None,
				},
			]
		);
	}

	#[test]
	fn summary_keeps_order_and_cached_state() {
		let file = [line("1.0.0", false), line("2.0.0", true)].join("\n");
		let on_disk = CachedFile {
			size: 1,
			last_used: 1,
		};
		let found = detail(&file, |v| (v == "1.0.0").then(|| on_disk.clone())).unwrap();
		let summary = CrateSummary::from(found);
		assert_eq!(summary.latest.as_deref(), Some("1.0.0"));
		assert_eq!(
			summary.versions,
			[
				VersionState {
					vers: "2.0.0".into(),
					yanked: true,
					cached: false,
				},
				VersionState {
					vers: "1.0.0".into(),
					yanked: false,
					cached: true,
				},
			]
		);
	}

	#[test]
	fn disk_stats_totals_all_and_lists_newest() {
		let mut stats = DiskStats::default();
		for (name, size, last_used) in [("a", 1, 5), ("b", 2, 9), ("c", 4, 7)] {
			stats.add(recent_file(name, last_used), size, 2);
		}
		assert_eq!(stats.files, 3);
		assert_eq!(stats.bytes, 7);
		let recent: Vec<_> = stats.recent.iter().map(|f| f.name.as_str()).collect();
		assert_eq!(recent, ["b", "c"]);
	}

	#[test]
	fn disk_stats_memory_does_not_grow_with_the_file_count() {
		let limit = 12;
		let mut stats = DiskStats::default();
		for last_used in 0..10_000 {
			stats.add(recent_file("a", last_used), 1, limit);
		}
		assert_eq!(stats.files, 10_000);
		assert_eq!(stats.recent.len(), limit);
		assert!(stats.recent.capacity() <= limit * 2);
		assert_eq!(stats.recent[0].last_used, 9_999);
	}

	fn recent_file(name: &str, last_used: u64) -> RecentFile {
		RecentFile {
			name: name.to_string(),
			vers: "1.0.0".to_string(),
			last_used,
		}
	}
}
