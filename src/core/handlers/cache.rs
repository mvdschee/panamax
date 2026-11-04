use sha2::{Digest, Sha256};

use crate::{
	core::models::index,
	error::CacheError,
	services::{
		store::Store,
		upstream::{Fetched, Upstream},
	},
};

/// The sparse index file for a crate.
///
/// Online: upstream's file in full, saved as the offline copy. Cargo may pick a version
/// we don't have yet; `crate_file` fetches it on download.
/// Offline: the saved copy, limited to versions on disk, so cargo never resolves a
/// version it can't download.
pub async fn index_file(
	store: &Store,
	upstream: &Upstream,
	name: &str,
) -> Result<Option<Vec<u8>>, CacheError> {
	let Some(path) = Store::index_path(name) else {
		return Ok(None);
	};

	match upstream.index_file(&path).await {
		Fetched::Found(body) => {
			// a failed save only costs the offline copy; still serve it
			if let Err(e) = store.write_index(name, &body).await {
				warn!("saving index for {name} failed: {e}");
			}
			Ok(Some(body.to_vec()))
		},
		Fetched::NotFound => Ok(None),
		Fetched::Unreachable => {
			// the served copy is newest; the git checkout covers crates never requested
			let cached = match store.read_index(name).await? {
				Some(cached) => cached,
				None => match store.read_checkout_index(name).await? {
					Some(cached) => cached,
					None => return Ok(None),
				},
			};
			let kept =
				index::keep_versions(&cached, |line| store.has_crate(&line.name, &line.vers));
			Ok((!kept.is_empty()).then(|| kept.into_bytes()))
		},
	}
}

/// A `.crate` file. A hit marks it as used. A miss downloads it, checks it against the
/// index cksum, and stores it.
pub async fn crate_file(
	store: &Store,
	upstream: &Upstream,
	name: &str,
	vers: &str,
) -> Result<Option<Vec<u8>>, CacheError> {
	if let Some(data) = store.read_crate(name, vers).await? {
		// a failed touch only shortens its life; still serve it
		if let Err(e) = store.touch(name, vers).await {
			warn!("marking {name} {vers} as used failed: {e}");
		}
		return Ok(Some(data));
	}

	// cargo reads the index before downloading, so the copy is normally there already
	let mut cksum = store.read_index(name).await?.and_then(|file| index::cksum(&file, vers));
	if cksum.is_none() {
		cksum = index_file(store, upstream, name)
			.await?
			.and_then(|file| index::cksum(&String::from_utf8_lossy(&file), vers));
	}
	let Some(cksum) = cksum else {
		return Ok(None);
	};

	let data = fetch_verified(upstream, name, vers, &cksum).await?;
	if let Some(data) = &data
		&& let Err(e) = store.write_crate(name, vers, data).await
	{
		warn!("caching {name} {vers} failed: {e}");
	}
	Ok(data)
}

/// Download a `.crate` and check it against its index cksum. None when upstream doesn't
/// have it or can't be reached; a mismatch is an error so it never gets stored.
pub async fn fetch_verified(
	upstream: &Upstream,
	name: &str,
	vers: &str,
	cksum: &str,
) -> Result<Option<Vec<u8>>, CacheError> {
	let Fetched::Found(data) = upstream.crate_file(name, vers).await else {
		return Ok(None);
	};
	if sha256_hex(&data) != cksum {
		return Err(CacheError::Checksum {
			name: name.to_string(),
			vers: vers.to_string(),
		});
	}
	Ok(Some(data.to_vec()))
}

fn sha256_hex(data: &[u8]) -> String {
	Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	// offline mode: every upstream request is Unreachable, no network involved
	fn offline() -> Upstream {
		Upstream::new(true)
	}

	fn line(vers: &str, cksum: &str) -> String {
		format!(
			r#"{{"name":"demo","vers":"{vers}","deps":[],"cksum":"{cksum}","features":{{}},"yanked":false}}"#
		)
	}

	#[tokio::test]
	async fn offline_index_lists_only_cached_versions() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		let file = format!("{}\n{}\n", line("1.0.0", "aa"), line("1.1.0", "bb"));
		store.write_index("demo", file.as_bytes()).await.unwrap();
		store.write_crate("demo", "1.0.0", b"x").await.unwrap();

		let served = index_file(&store, &offline(), "demo").await.unwrap().unwrap();
		assert_eq!(String::from_utf8(served).unwrap(), format!("{}\n", line("1.0.0", "aa")));
	}

	#[tokio::test]
	async fn offline_index_without_cached_versions_is_not_found() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		store.write_index("demo", format!("{}\n", line("1.0.0", "aa")).as_bytes()).await.unwrap();

		assert_eq!(index_file(&store, &offline(), "demo").await.unwrap(), None);
		assert_eq!(index_file(&store, &offline(), "never-seen").await.unwrap(), None);
	}

	#[tokio::test]
	async fn offline_crate_hit_and_miss() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		store.write_crate("demo", "1.0.0", b"x").await.unwrap();

		assert_eq!(
			crate_file(&store, &offline(), "demo", "1.0.0").await.unwrap().as_deref(),
			Some(&b"x"[..])
		);
		assert_eq!(crate_file(&store, &offline(), "demo", "2.0.0").await.unwrap(), None);
	}

	/// A local stand-in for crates.io: `demo` has 1.0.0 (good) and 2.0.0 (corrupt download).
	async fn fake_upstream() -> Upstream {
		use axum::{Router, routing::get};

		let index = format!(
			"{}\n{}\n",
			line("1.0.0", &sha256_hex(b"good")),
			line("2.0.0", &sha256_hex(b"other"))
		);
		let app = Router::new()
			.route("/index/de/mo/demo", get(move || async move { index }))
			.route("/crates/demo/demo-1.0.0.crate", get(|| async { "good" }))
			.route("/crates/demo/demo-2.0.0.crate", get(|| async { "corrupt" }));
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

		Upstream::with_urls(
			false,
			&format!("http://{addr}/index"),
			&format!("http://{addr}/crates"),
		)
	}

	#[tokio::test]
	async fn online_index_is_passed_through_and_saved() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		let upstream = fake_upstream().await;

		let served = index_file(&store, &upstream, "demo").await.unwrap().unwrap();
		assert_eq!(index::lines(&String::from_utf8(served).unwrap()).count(), 2);
		assert!(store.read_index("demo").await.unwrap().is_some());
		assert_eq!(index_file(&store, &upstream, "missing").await.unwrap(), None);
	}

	#[tokio::test]
	async fn online_crate_miss_is_fetched_and_cached() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());

		let data = crate_file(&store, &fake_upstream().await, "demo", "1.0.0").await.unwrap();
		assert_eq!(data.as_deref(), Some(&b"good"[..]));
		assert!(store.has_crate("demo", "1.0.0"));

		// now served from cache with upstream gone
		let data = crate_file(&store, &offline(), "demo", "1.0.0").await.unwrap();
		assert_eq!(data.as_deref(), Some(&b"good"[..]));
	}

	#[tokio::test]
	async fn corrupt_download_is_never_cached() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());

		let result = crate_file(&store, &fake_upstream().await, "demo", "2.0.0").await;
		assert!(matches!(result, Err(CacheError::Checksum { .. })));
		assert!(!store.has_crate("demo", "2.0.0"));
	}

	#[tokio::test]
	async fn cache_hit_is_served_when_touch_fails() {
		use std::os::unix::fs::PermissionsExt;

		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		store.write_crate("demo", "1.0.0", b"x").await.unwrap();
		let path = store.crates().await.unwrap().remove(0).path;
		std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

		assert!(store.touch("demo", "1.0.0").await.is_err());
		let data = crate_file(&store, &offline(), "demo", "1.0.0").await.unwrap();
		assert_eq!(data.as_deref(), Some(&b"x"[..]));
	}
}
