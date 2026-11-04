use std::{collections::HashSet, time::SystemTime};

use futures::StreamExt;

use crate::{
	config::{KEEP_UNUSED, MAX_DELETE_PERCENT, SYNC_DOWNLOADS, UPSTREAM_INDEX_GIT},
	core::{
		handlers::cache,
		models::{
			index,
			keep::{self, RustVersion},
		},
	},
	error::SyncError,
	services::{cmd::git, store::Store, upstream::Upstream},
};

pub struct Options {
	pub dry_run: bool,
	pub allow_mass_delete: bool,
	/// Pinned toolchains, for the MSRV rule.
	pub toolchains: Vec<RustVersion>,
}

#[derive(Debug, Default)]
pub struct Report {
	pub releases: usize,
	pub kept: usize,
	pub on_disk: usize,
	pub missing: usize,
	pub downloaded: usize,
	pub failed: usize,
	pub deleted: usize,
	pub deleted_bytes: u64,
	/// Over the mass-delete limit: a real run stops here unless `--allow-mass-delete` is set.
	pub mass_delete: bool,
}

/// Make `crates/` hold the baseline: download kept versions that are missing, and
/// delete files outside the baseline that haven't been used for `KEEP_UNUSED`.
pub async fn sync_crates(
	store: &Store,
	upstream: &Upstream,
	opts: &Options,
) -> Result<Report, SyncError> {
	git::shallow_update(UPSTREAM_INDEX_GIT, &store.checkout_dir()).await?;

	let files = store.crates().await?;
	let plan = {
		let store = store.clone();
		let toolchains = opts.toolchains.clone();
		tokio::task::spawn_blocking(move || plan(&store, &toolchains, files))
			.await
			.map_err(std::io::Error::other)??
	};
	let mut report = plan.report;

	report.deleted = plan.delete.len();
	report.deleted_bytes = plan.delete.iter().map(|f| f.size).sum();
	report.mass_delete = check_mass_delete(report.deleted, report.on_disk, false).is_err();
	// a dry run always reports; only a real run stops at the limit
	if opts.dry_run {
		return Ok(report);
	}
	check_mass_delete(report.deleted, report.on_disk, opts.allow_mass_delete)?;

	// download before deleting, so a failed run never leaves less than it found
	let results: Vec<bool> = futures::stream::iter(plan.missing)
		.map(|(name, vers)| download(store, upstream, name, vers))
		.buffer_unordered(SYNC_DOWNLOADS)
		.collect()
		.await;
	report.downloaded = results.iter().filter(|&&ok| ok).count();
	report.failed = results.len() - report.downloaded;

	for file in &plan.delete {
		if let Err(e) = store.remove(&file.path).await {
			warn!("deleting {} failed: {e}", file.path.display());
		}
	}
	Ok(report)
}

/// A run that would delete a large share of the mirror is more likely a bug than intended,
/// so it needs an explicit flag.
fn check_mass_delete(delete: usize, total: usize, allowed: bool) -> Result<(), SyncError> {
	if delete * 100 > total * MAX_DELETE_PERCENT && !allowed {
		return Err(SyncError::MassDelete {
			delete,
			total,
			percent: MAX_DELETE_PERCENT,
		});
	}
	Ok(())
}

struct Plan {
	report: Report,
	missing: Vec<(String, String)>,
	delete: Vec<crate::services::store::CrateFile>,
}

/// Read the checkout, compute the keep set, and compare it with what's on disk. Blocking.
fn plan(
	store: &Store,
	toolchains: &[RustVersion],
	files: Vec<crate::services::store::CrateFile>,
) -> Result<Plan, SyncError> {
	let mut idx = keep::Index::default();
	store.each_checkout_file(|text| idx.add_file(text))?;
	idx.finish();

	let kept = keep::keep_set(&idx, toolchains);
	let violations = keep::closure_violations(&idx, &kept);
	for v in violations.iter().take(10) {
		error!("closure violation: {v}");
	}
	if !violations.is_empty() {
		return Err(SyncError::ClosureViolations(violations.len()));
	}

	let on_disk: HashSet<(&str, &str)> =
		files.iter().map(|f| (f.name.as_str(), f.vers.as_str())).collect();
	let missing: Vec<(String, String)> = idx
		.releases
		.iter()
		.zip(&kept)
		.filter(|&(r, &k)| k && !on_disk.contains(&(idx.name(r), &*r.vers)))
		.map(|(r, _)| (idx.name(r).to_string(), r.vers.to_string()))
		.collect();

	let cutoff = SystemTime::now() - KEEP_UNUSED;
	let report = Report {
		releases: idx.releases.len(),
		kept: kept.iter().filter(|&&k| k).count(),
		on_disk: files.len(),
		missing: missing.len(),
		..Report::default()
	};
	let delete = files
		.into_iter()
		.filter(|f| {
			f.last_used < cutoff && !idx.find(&f.name, &f.vers).is_some_and(|id| kept[id as usize])
		})
		.collect();

	Ok(Plan {
		report,
		missing,
		delete,
	})
}

async fn download(store: &Store, upstream: &Upstream, name: String, vers: String) -> bool {
	let cksum = match store.read_checkout_index(&name).await {
		Ok(file) => file.and_then(|f| index::cksum(&f, &vers)),
		Err(e) => {
			warn!("reading index for {name} failed: {e}");
			None
		},
	};
	let Some(cksum) = cksum else {
		return false;
	};
	match cache::fetch_verified(upstream, &name, &vers, &cksum).await {
		Ok(Some(data)) => match store.write_crate(&name, &vers, &data).await {
			Ok(()) => true,
			Err(e) => {
				warn!("writing {name} {vers} failed: {e}");
				false
			},
		},
		Ok(None) => false,
		Err(e) => {
			warn!("{e}");
			false
		},
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn rel(vers: &str) -> String {
		format!(r#"{{"name":"demo","vers":"{vers}","deps":[],"cksum":"00","yanked":false}}"#)
	}

	async fn age(store: &Store, vers: &str, days: u64) {
		let path = store.crates().await.unwrap().into_iter().find(|f| f.vers == vers).unwrap().path;
		let when = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
		std::fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
	}

	#[tokio::test]
	async fn plan_downloads_baseline_and_deletes_only_old_exceptions() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		// baseline for demo: 2.0.0, 3.0.0, 4.0.0 (newest 3 lines)
		let checkout = root.path().join("crates.io-index/de/mo");
		std::fs::create_dir_all(&checkout).unwrap();
		let file = ["1.0.0", "2.0.0", "3.0.0", "4.0.0"].map(rel).join("\n");
		std::fs::write(checkout.join("demo"), file).unwrap();

		for vers in ["1.0.0", "2.0.0", "3.0.0"] {
			store.write_crate("demo", vers, b"x").await.unwrap();
		}
		store.write_crate("gone", "0.1.0", b"x").await.unwrap();
		age(&store, "1.0.0", 200).await; // old exception: deleted
		age(&store, "2.0.0", 400).await; // old but baseline: kept
		age(&store, "0.1.0", 10).await; // recent exception: kept

		let files = store.crates().await.unwrap();
		let plan = plan(&store, &[], files).unwrap();
		assert_eq!(plan.report.kept, 3);
		assert_eq!(plan.missing, [("demo".to_string(), "4.0.0".to_string())]);
		let deleted: Vec<_> = plan.delete.iter().map(|f| f.vers.as_str()).collect();
		assert_eq!(deleted, ["1.0.0"]);
	}

	#[test]
	fn mass_delete_needs_the_flag() {
		assert!(check_mass_delete(10, 100, false).is_ok());
		assert!(matches!(check_mass_delete(11, 100, false), Err(SyncError::MassDelete { .. })));
		assert!(check_mass_delete(11, 100, true).is_ok());
		assert!(check_mass_delete(0, 0, false).is_ok());
	}
}
