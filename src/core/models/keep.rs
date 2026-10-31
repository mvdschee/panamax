//! Which crate versions the mirror keeps permanently: the baseline. Ported from
//! infrastructure/tools/panamax-prune, with names and requirements interned so the full
//! index (~3.5M releases) fits in the nas sync's memory limit.

use std::collections::HashMap;

use semver::{Version, VersionReq};
use serde::Deserialize;

/// Newest semver lines kept per crate.
const LINES_KEPT: usize = 3;

pub type RustVersion = (u64, u64, u64);

pub struct Release {
	pub krate: u32,
	pub vers: Box<str>,
	version: Option<Version>,
	yanked: bool,
	rust: Option<RustVersion>,
	/// Normal and build dependencies as (crate id, requirement id). Dev dependencies of
	/// a dependency are never resolved by cargo.
	deps: Box<[(u32, u32)]>,
}

#[derive(Default)]
pub struct Index {
	pub releases: Vec<Release>,
	/// Crate id -> name as published (case kept). Empty for crates only seen as a dependency.
	names: Vec<Box<str>>,
	/// Lowercased name -> crate id.
	ids: HashMap<Box<str>, u32>,
	/// Crate id -> release ids, oldest to newest by semver. Unparsable versions sort first.
	by_crate: Vec<Vec<u32>>,
	/// Requirement id -> parsed requirement, None when it doesn't parse.
	reqs: Vec<Option<VersionReq>>,
	req_ids: HashMap<Box<str>, u32>,
	/// Index lines that didn't parse.
	pub bad_lines: usize,
}

#[derive(Deserialize)]
struct Line<'a> {
	#[serde(borrow)]
	name: std::borrow::Cow<'a, str>,
	#[serde(borrow)]
	vers: std::borrow::Cow<'a, str>,
	#[serde(default, borrow)]
	deps: Vec<Dep<'a>>,
	#[serde(default)]
	yanked: Option<bool>,
	#[serde(default, borrow)]
	rust_version: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Dep<'a> {
	#[serde(borrow)]
	name: std::borrow::Cow<'a, str>,
	#[serde(borrow)]
	req: std::borrow::Cow<'a, str>,
	#[serde(default, borrow)]
	kind: Option<std::borrow::Cow<'a, str>>,
	#[serde(default, borrow)]
	package: Option<std::borrow::Cow<'a, str>>,
}

impl Index {
	/// Add every line of one index file.
	pub fn add_file(&mut self, text: &str) {
		for line in text.lines().filter(|l| !l.trim().is_empty()) {
			let Ok(l) = serde_json::from_str::<Line>(line) else {
				self.bad_lines += 1;
				continue;
			};
			let krate = self.crate_id(&l.name);
			if self.names[krate as usize].is_empty() {
				self.names[krate as usize] = l.name.as_ref().into();
			}
			let deps = l
				.deps
				.iter()
				.filter(|d| d.kind.as_deref() != Some("dev"))
				.map(|d| {
					(self.crate_id(d.package.as_deref().unwrap_or(&d.name)), self.req_id(&d.req))
				})
				.collect();
			let id = self.releases.len() as u32;
			self.releases.push(Release {
				krate,
				vers: l.vers.as_ref().into(),
				version: Version::parse(&l.vers).ok(),
				yanked: l.yanked.unwrap_or(false),
				rust: l.rust_version.as_deref().and_then(parse_rust),
				deps,
			});
			self.by_crate[krate as usize].push(id);
		}
	}

	/// Sort each crate's releases by semver. Call once after the last `add_file`.
	pub fn finish(&mut self) {
		let releases = &self.releases;
		for ids in &mut self.by_crate {
			ids.sort_by(|&a, &b| releases[a as usize].version.cmp(&releases[b as usize].version));
		}
	}

	pub fn name(&self, release: &Release) -> &str {
		&self.names[release.krate as usize]
	}

	/// The release id of `name vers`, case-insensitive on the name.
	pub fn find(&self, name: &str, vers: &str) -> Option<u32> {
		let krate = *self.ids.get(name.to_ascii_lowercase().as_str())?;
		self.by_crate[krate as usize]
			.iter()
			.copied()
			.find(|&id| &*self.releases[id as usize].vers == vers)
	}

	fn crate_id(&mut self, name: &str) -> u32 {
		let key = name.to_ascii_lowercase();
		if let Some(&id) = self.ids.get(key.as_str()) {
			return id;
		}
		let id = self.names.len() as u32;
		self.ids.insert(key.into(), id);
		self.names.push("".into());
		self.by_crate.push(Vec::new());
		id
	}

	fn req_id(&mut self, req: &str) -> u32 {
		if let Some(&id) = self.req_ids.get(req) {
			return id;
		}
		let id = self.reqs.len() as u32;
		self.req_ids.insert(req.into(), id);
		self.reqs.push(VersionReq::parse(req).ok());
		id
	}

	fn release(&self, id: u32) -> &Release {
		&self.releases[id as usize]
	}
}

pub fn parse_rust(s: &str) -> Option<RustVersion> {
	let mut parts = s.trim().split('.').map(|p| p.parse::<u64>().ok());
	let major = parts.next()??;
	let minor = parts.next().unwrap_or(Some(0))?;
	let patch = parts.next().unwrap_or(Some(0))?;
	Some((major, minor, patch))
}

/// The semver-compatible line: `X` for X >= 1, `0.Y`, or `0.0.Z`.
fn line(v: &Version) -> (u64, u64, u64) {
	match (v.major, v.minor) {
		(0, 0) => (0, 0, v.patch),
		(0, minor) => (0, minor, 0),
		(major, _) => (major, 0, 0),
	}
}

fn rust_ok(r: &Release, toolchain: RustVersion) -> bool {
	r.rust.is_none_or(|rv| rv <= toolchain)
}

/// The newest of `ids` (oldest to newest), plus the newest that builds on each
/// toolchain, which is what cargo's MSRV-aware resolver prefers.
fn newest(index: &Index, ids: &[u32], toolchains: &[RustVersion]) -> Vec<u32> {
	let mut out: Vec<u32> = ids.last().copied().into_iter().collect();
	for &t in toolchains {
		out.extend(ids.iter().rev().find(|&&id| rust_ok(index.release(id), t)));
	}
	out
}

/// One crate's own keep set: the newest release of each of its newest LINES_KEPT lines,
/// ignoring yanked releases and prereleases.
fn base_keep(index: &Index, ids: &[u32], toolchains: &[RustVersion]) -> Vec<u32> {
	let mut keep: Vec<u32> =
		ids.iter().copied().filter(|&id| index.release(id).version.is_none()).collect();
	let stable: Vec<u32> = ids
		.iter()
		.copied()
		.filter(|&id| {
			let r = index.release(id);
			!r.yanked && r.version.as_ref().is_some_and(|v| v.pre.is_empty())
		})
		.collect();
	if stable.is_empty() {
		// only prereleases: keep the newest so the crate stays usable
		let pre: Vec<u32> = ids
			.iter()
			.copied()
			.filter(|&id| !index.release(id).yanked && index.release(id).version.is_some())
			.collect();
		keep.extend(newest(index, &pre, toolchains));
		return keep;
	}
	let line_of = |id: u32| line(index.release(id).version.as_ref().expect("stable has a version"));
	let mut lines: Vec<_> = stable.iter().rev().map(|&id| line_of(id)).collect();
	lines.dedup();
	for l in lines.into_iter().take(LINES_KEPT) {
		let in_line: Vec<u32> = stable.iter().copied().filter(|&id| line_of(id) == l).collect();
		keep.extend(newest(index, &in_line, toolchains));
	}
	keep
}

/// Non-yanked releases of a crate that satisfy `req`, oldest to newest.
fn matching(index: &Index, krate: u32, req: &VersionReq) -> Vec<u32> {
	index.by_crate[krate as usize]
		.iter()
		.copied()
		.filter(|&id| {
			let r = index.release(id);
			!r.yanked && r.version.as_ref().is_some_and(|v| req.matches(v))
		})
		.collect()
}

/// Every crate's own keep set, closed over dependencies: for each kept release, the
/// newest match of each requirement is kept too. Indexed by release id.
pub fn keep_set(index: &Index, toolchains: &[RustVersion]) -> Vec<bool> {
	let mut kept = vec![false; index.releases.len()];
	let mut queue = Vec::new();
	let mark = |id: u32, kept: &mut Vec<bool>, queue: &mut Vec<u32>| {
		if !kept[id as usize] {
			kept[id as usize] = true;
			queue.push(id);
		}
	};
	for ids in &index.by_crate {
		for id in base_keep(index, ids, toolchains) {
			mark(id, &mut kept, &mut queue);
		}
	}
	while let Some(id) = queue.pop() {
		for &(krate, req) in &index.release(id).deps {
			let Some(req) = &index.reqs[req as usize] else {
				continue;
			};
			for dep in newest(index, &matching(index, krate, req), toolchains) {
				mark(dep, &mut kept, &mut queue);
			}
		}
	}
	kept
}

/// Requirements of kept releases that some release satisfies but no kept, non-yanked
/// release does. Must be empty before anything is deleted.
pub fn closure_violations(index: &Index, kept: &[bool]) -> Vec<String> {
	let mut out = Vec::new();
	for (id, r) in index.releases.iter().enumerate().filter(|&(id, _)| kept[id]) {
		for &(krate, req) in &r.deps {
			let Some(parsed) = &index.reqs[req as usize] else {
				continue;
			};
			let m = matching(index, krate, parsed);
			if !m.is_empty() && !m.iter().any(|&d| kept[d as usize]) {
				out.push(format!(
					"{} {} needs {} {parsed}",
					index.name(r),
					index.releases[id].vers,
					index.names[krate as usize]
				));
			}
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rel(name: &str, vers: &str) -> String {
		format!(r#"{{"name":"{name}","vers":"{vers}","deps":[],"yanked":false}}"#)
	}

	fn index(lines: &[String]) -> Index {
		let mut index = Index::default();
		index.add_file(&lines.join("\n"));
		index.finish();
		assert_eq!(index.bad_lines, 0);
		index
	}

	fn kept_versions(index: &Index, kept: &[bool], name: &str) -> Vec<String> {
		let krate = index.ids[name];
		index.by_crate[krate as usize]
			.iter()
			.filter(|&&id| kept[id as usize])
			.map(|&id| index.release(id).vers.to_string())
			.collect()
	}

	#[test]
	fn keeps_newest_of_last_three_major_lines() {
		let idx = index(
			&["1.0.0", "1.0.5", "2.0.0", "2.1.0", "3.0.0", "4.0.0", "4.0.1"].map(|v| rel("foo", v)),
		);
		let kept = keep_set(&idx, &[]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["2.1.0", "3.0.0", "4.0.1"]);
	}

	#[test]
	fn zero_minor_and_zero_zero_patch_are_their_own_lines() {
		let idx = index(&["0.0.1", "0.0.2", "0.1.0", "0.1.3", "0.2.0"].map(|v| rel("foo", v)));
		let kept = keep_set(&idx, &[]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["0.0.2", "0.1.3", "0.2.0"]);
	}

	#[test]
	fn yanked_and_prereleases_do_not_count_as_lines() {
		let idx = index(&[
			rel("foo", "1.0.0"),
			r#"{"name":"foo","vers":"2.0.0","deps":[],"yanked":true}"#.to_string(),
			rel("foo", "3.0.0-alpha.1"),
		]);
		let kept = keep_set(&idx, &[]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["1.0.0"]);
	}

	#[test]
	fn prerelease_only_crate_keeps_its_newest() {
		let idx = index(&["0.1.0-alpha.1", "0.1.0-alpha.2"].map(|v| rel("foo", v)));
		let kept = keep_set(&idx, &[]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["0.1.0-alpha.2"]);
	}

	#[test]
	fn dependency_on_an_old_line_keeps_it() {
		let mut lines =
			["0.4.0", "0.4.2", "0.5.0", "0.6.0", "0.7.0"].map(|v| rel("foo", v)).to_vec();
		lines.push(
			r#"{"name":"app","vers":"1.0.0","deps":[
				{"name":"foo","req":"^0.4","kind":"normal"},
				{"name":"tool","req":"^1","kind":"dev"},
				{"name":"alias","package":"bar","req":"^0.1","kind":"build"}
			],"yanked":false}"#
				.replace(['\n', '\t'], ""),
		);
		lines.extend(["0.1.0", "0.1.1", "0.2.0", "0.3.0", "0.4.0"].map(|v| rel("bar", v)));
		lines.extend(["1.0.0", "1.1.0"].map(|v| rel("tool", v)));
		let idx = index(&lines);
		let kept = keep_set(&idx, &[]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["0.4.2", "0.5.0", "0.6.0", "0.7.0"]);
		assert_eq!(kept_versions(&idx, &kept, "bar"), ["0.1.1", "0.2.0", "0.3.0", "0.4.0"]);
		assert_eq!(kept_versions(&idx, &kept, "tool"), ["1.1.0"]);
		assert!(closure_violations(&idx, &kept).is_empty());
	}

	#[test]
	fn keeps_newest_release_that_builds_on_each_toolchain() {
		let idx = index(&[
			r#"{"name":"foo","vers":"1.0.0","deps":[],"yanked":false,"rust_version":"1.80"}"#
				.to_string(),
			r#"{"name":"foo","vers":"1.1.0","deps":[],"yanked":false,"rust_version":"1.98.0"}"#
				.to_string(),
		]);
		let kept = keep_set(&idx, &[]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["1.1.0"]);
		let kept = keep_set(&idx, &[(1, 96, 0), (1, 99, 0)]);
		assert_eq!(kept_versions(&idx, &kept, "foo"), ["1.0.0", "1.1.0"]);
	}

	#[test]
	fn dependency_seen_before_its_crate_and_case_insensitive_find() {
		let idx = index(&[
			r#"{"name":"app","vers":"1.0.0","deps":[{"name":"Late","req":"^1","kind":"normal"}],"yanked":false}"#
				.to_string(),
			rel("Late", "1.0.0"),
		]);
		let kept = keep_set(&idx, &[]);
		let late = idx.find("late", "1.0.0").unwrap();
		assert!(kept[late as usize]);
		assert_eq!(idx.name(idx.release(late)), "Late");
	}
}

/// Runs on a real index checkout: `PANAMAX_INDEX=<dir> cargo test --release -- --ignored real_index`.
#[cfg(test)]
mod real_index {
	use super::*;

	#[test]
	#[ignore]
	fn real_index() {
		let dir = std::env::var("PANAMAX_INDEX").expect("set PANAMAX_INDEX");
		let mut index = Index::default();
		for entry in walkdir::WalkDir::new(dir)
			.into_iter()
			.filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
		{
			let entry = entry.unwrap();
			if entry.file_type().is_file() && entry.file_name() != "config.json" {
				index.add_file(&std::fs::read_to_string(entry.path()).unwrap());
			}
		}
		index.finish();
		let kept = keep_set(&index, &[(1, 96, 0), (1, 99, 0)]);
		let violations = closure_violations(&index, &kept);
		eprintln!(
			"releases {} kept {} bad lines {} violations {}",
			index.releases.len(),
			kept.iter().filter(|&&k| k).count(),
			index.bad_lines,
			violations.len()
		);
		assert!(violations.is_empty());
	}
}
