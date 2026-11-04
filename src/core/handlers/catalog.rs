use std::{
	io,
	time::{SystemTime, UNIX_EPOCH},
};

use crate::{
	core::models::{
		catalog::{self, CachedFile, CrateDetail, CrateSummary, DiskStats},
		index,
	},
	services::store::{CrateFile, Store},
};

/// Crates matching `query`. `names` are the index names from `Store::checkout_names`.
pub async fn search(
	store: &Store,
	names: &[String],
	query: &str,
	limit: usize,
) -> io::Result<Vec<CrateSummary>> {
	let mut out = vec![];
	for name in catalog::rank(names, query, limit) {
		if let Some(found) = crate_detail(store, name).await? {
			out.push(found.into());
		}
	}
	Ok(out)
}

/// One crate as the index checkout describes it, with what the mirror holds of it.
pub async fn crate_detail(store: &Store, name: &str) -> io::Result<Option<CrateDetail>> {
	let Some(file) = store.read_checkout_index(name).await? else {
		return Ok(None);
	};
	// the crate directory uses the published name, which may differ in case
	let Some(published) = index::lines(&file).next().map(|line| line.name) else {
		return Ok(None);
	};
	let on_disk = store.cached_versions(&published).await?;
	Ok(catalog::detail(&file, |vers| {
		on_disk.iter().find(|f| f.vers == vers).map(|f| CachedFile {
			size: f.size,
			last_used: unix_secs(f.last_used),
		})
	}))
}

/// Totals for every crate file on disk. Walks the whole store.
pub async fn disk_stats(store: &Store, recent: usize) -> io::Result<DiskStats> {
	let files = store.crates().await?.into_iter().map(|f: CrateFile| {
		let used = unix_secs(f.last_used);
		(f.name, f.vers, f.size, used)
	});
	Ok(catalog::disk_stats(files, recent))
}

pub fn unix_secs(time: SystemTime) -> u64 {
	time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::core::models::catalog::VersionState;

	#[tokio::test]
	async fn search_marks_cached_versions() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		let file = |name: &str, vers: &[&str]| {
			vers.iter()
				.map(|v| format!(r#"{{"name":"{name}","vers":"{v}","cksum":"aa","yanked":false}}"#))
				.collect::<Vec<_>>()
				.join("\n")
		};
		let checkout = store.checkout_dir();
		std::fs::create_dir_all(checkout.join("in/fl")).unwrap();
		std::fs::write(checkout.join("in/fl/inflector"), file("Inflector", &["0.11.3", "0.11.4"]))
			.unwrap();
		std::fs::create_dir_all(checkout.join("3/i")).unwrap();
		std::fs::write(checkout.join("3/i/inf"), file("inf", &["1.0.0"])).unwrap();
		store.write_crate("Inflector", "0.11.3", b"x").await.unwrap();

		let names = store.checkout_names().unwrap();
		let state = |vers: &str, cached| VersionState {
			vers: vers.into(),
			yanked: false,
			cached,
		};
		assert_eq!(
			search(&store, &names, "inf", 10).await.unwrap(),
			[
				CrateSummary {
					name: "inf".into(),
					latest: Some("1.0.0".into()),
					versions: vec![state("1.0.0", false)],
				},
				CrateSummary {
					name: "Inflector".into(),
					latest: Some("0.11.4".into()),
					versions: vec![state("0.11.4", false), state("0.11.3", true)],
				},
			]
		);
	}

	#[tokio::test]
	async fn crate_detail_is_none_without_an_index_file() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		assert_eq!(crate_detail(&store, "serde").await.unwrap(), None);
	}

	#[tokio::test]
	async fn disk_stats_counts_files() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		store.write_crate("serde", "1.0.0", b"xy").await.unwrap();
		store.write_crate("rand", "0.8.0", b"xyz").await.unwrap();
		let stats = disk_stats(&store, 1).await.unwrap();
		assert_eq!(stats.files, 2);
		assert_eq!(stats.bytes, 5);
		assert_eq!(stats.recent.len(), 1);
	}
}
