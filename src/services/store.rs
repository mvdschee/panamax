use std::{
	io,
	path::{Path, PathBuf},
	sync::atomic::{AtomicU64, Ordering},
	time::SystemTime,
};

use tokio::fs;
use walkdir::WalkDir;

/// The crate half of the mirror on disk:
/// - `crates.io-index/`: shallow git checkout of the full index, used by sync
/// - `index/<sparse path>`: last copy of each upstream index file served online
/// - `crates/<prefix>/<name>/<vers>/<name>-<vers>.crate`: the crates themselves
#[derive(Clone)]
pub struct Store {
	root: PathBuf,
}

impl Store {
	pub fn new(root: PathBuf) -> Self {
		Self {
			root,
		}
	}

	/// Sparse index path for a crate, e.g. `se/rd/serde`. None for an invalid name.
	pub fn index_path(name: &str) -> Option<String> {
		if !valid_name(name) {
			return None;
		}
		let name = name.to_ascii_lowercase();
		Some(match name.len() {
			1 => format!("1/{name}"),
			2 => format!("2/{name}"),
			3 => format!("3/{}/{name}", &name[..1]),
			_ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
		})
	}

	pub async fn read_index(&self, name: &str) -> io::Result<Option<String>> {
		let Some(path) = Self::index_path(name) else {
			return Ok(None);
		};
		read_optional(&self.root.join("index").join(path))
			.await
			.map(|data| data.and_then(|d| String::from_utf8(d).ok()))
	}

	pub async fn write_index(&self, name: &str, data: &[u8]) -> io::Result<()> {
		let path = Self::index_path(name).ok_or_else(invalid)?;
		write_atomic(&self.root.join("index").join(path), data).await
	}

	pub fn checkout_dir(&self) -> PathBuf {
		self.root.join("crates.io-index")
	}

	/// A crate's file in the git checkout, for crates never requested through serve.
	pub async fn read_checkout_index(&self, name: &str) -> io::Result<Option<String>> {
		let Some(path) = Self::index_path(name) else {
			return Ok(None);
		};
		read_optional(&self.checkout_dir().join(path))
			.await
			.map(|data| data.and_then(|d| String::from_utf8(d).ok()))
	}

	/// Calls `f` with the text of every index file in the git checkout. Blocking.
	pub fn each_checkout_file(&self, mut f: impl FnMut(&str)) -> io::Result<()> {
		let walk = WalkDir::new(self.checkout_dir())
			.into_iter()
			.filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'));
		for entry in walk {
			let entry = entry?;
			if entry.file_type().is_file() && entry.file_name() != "config.json" {
				f(&std::fs::read_to_string(entry.path())?);
			}
		}
		Ok(())
	}

	/// Every crate name in the git checkout, lowercase as the index stores it. Blocking.
	pub fn checkout_names(&self) -> io::Result<Vec<String>> {
		let dir = self.checkout_dir();
		let mut out = vec![];
		if !dir.exists() {
			return Ok(out);
		}
		let walk = WalkDir::new(&dir)
			.into_iter()
			.filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'));
		for entry in walk {
			let entry = entry?;
			if !entry.file_type().is_file() {
				continue;
			}
			let Some(name) = entry.file_name().to_str() else {
				continue;
			};
			// only files at their sparse path, which skips README.md and config.json
			let rel = entry.path().strip_prefix(&dir).ok().and_then(|p| p.to_str());
			if Self::index_path(name).as_deref() == rel {
				out.push(name.to_string());
			}
		}
		Ok(out)
	}

	/// Uses the same layout as the old panamax mirror, so an existing crates/ dir is reused.
	fn crate_dir(&self, name: &str) -> Option<PathBuf> {
		if !valid_name(name) {
			return None;
		}
		let prefix = match name.len() {
			1 => PathBuf::from("1"),
			2 => PathBuf::from("2"),
			3 => PathBuf::from("3").join(&name[..1]),
			_ => PathBuf::from(&name[..2]).join(&name[2..4]),
		};
		Some(self.root.join("crates").join(prefix).join(name))
	}

	fn crate_path(&self, name: &str, vers: &str) -> Option<PathBuf> {
		if !valid_version(vers) {
			return None;
		}
		Some(self.crate_dir(name)?.join(vers).join(format!("{name}-{vers}.crate")))
	}

	/// Files of a crate on disk, in no particular order. `name` must have the published
	/// case.
	pub async fn cached_versions(&self, name: &str) -> io::Result<Vec<CrateFile>> {
		let mut out = vec![];
		let Some(dir) = self.crate_dir(name) else {
			return Ok(out);
		};
		let mut entries = match fs::read_dir(&dir).await {
			Ok(entries) => entries,
			Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
			Err(e) => return Err(e),
		};
		while let Some(entry) = entries.next_entry().await? {
			let Some(vers) = entry.file_name().to_str().map(String::from) else {
				continue;
			};
			let Some(path) = self.crate_path(name, &vers) else {
				continue;
			};
			let meta = match fs::metadata(&path).await {
				Ok(meta) => meta,
				Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
				Err(e) => return Err(e),
			};
			out.push(CrateFile {
				name: name.to_string(),
				vers,
				path,
				last_used: meta.modified()?,
				size: meta.len(),
			});
		}
		Ok(out)
	}

	/// When sync last fetched the index checkout. None before the first sync.
	pub async fn index_synced(&self) -> Option<SystemTime> {
		let git = self.checkout_dir().join(".git");
		// the first clone leaves no FETCH_HEAD
		for file in ["FETCH_HEAD", "HEAD"] {
			if let Ok(meta) = fs::metadata(git.join(file)).await {
				return meta.modified().ok();
			}
		}
		None
	}

	pub fn has_crate(&self, name: &str, vers: &str) -> bool {
		self.crate_path(name, vers).is_some_and(|p| p.is_file())
	}

	pub async fn read_crate(&self, name: &str, vers: &str) -> io::Result<Option<Vec<u8>>> {
		match self.crate_path(name, vers) {
			Some(path) => read_optional(&path).await,
			None => Ok(None),
		}
	}

	pub async fn write_crate(&self, name: &str, vers: &str, data: &[u8]) -> io::Result<()> {
		let path = self.crate_path(name, vers).ok_or_else(invalid)?;
		write_atomic(&path, data).await
	}

	/// Mark a crate as used now.
	pub async fn touch(&self, name: &str, vers: &str) -> io::Result<()> {
		let path = self.crate_path(name, vers).ok_or_else(invalid)?;
		tokio::task::spawn_blocking(move || {
			std::fs::File::options().write(true).open(path)?.set_modified(SystemTime::now())
		})
		.await?
	}

	/// Every crate file on disk.
	pub async fn crates(&self) -> io::Result<Vec<CrateFile>> {
		let dir = self.root.join("crates");
		tokio::task::spawn_blocking(move || {
			let mut out = vec![];
			if !dir.exists() {
				return Ok(out);
			}
			for entry in WalkDir::new(dir) {
				let entry = entry?;
				if entry.file_type().is_file()
					&& entry.path().extension().is_some_and(|e| e == "crate")
				{
					let meta = entry.metadata()?;
					let path = entry.path();
					// layout: .../<name>/<vers>/<name>-<vers>.crate
					let mut dirs = path.ancestors().skip(1).filter_map(|p| p.file_name()?.to_str());
					let (Some(vers), Some(name)) = (dirs.next(), dirs.next()) else {
						continue;
					};
					out.push(CrateFile {
						name: name.to_string(),
						vers: vers.to_string(),
						path: path.to_path_buf(),
						last_used: meta.modified()?,
						size: meta.len(),
					});
				}
			}
			Ok(out)
		})
		.await?
	}

	/// Remove a crate file, and its version directory if that is now empty.
	pub async fn remove(&self, path: &Path) -> io::Result<()> {
		fs::remove_file(path).await?;
		if let Some(dir) = path.parent() {
			// fails when the directory isn't empty, which is fine
			let _ = fs::remove_dir(dir).await;
		}
		Ok(())
	}
}

pub struct CrateFile {
	pub name: String,
	pub vers: String,
	pub path: PathBuf,
	pub last_used: SystemTime,
	pub size: u64,
}

/// crates.io names: ASCII letters, digits, `-` and `_`, at most 64 characters.
/// Checking this also keeps request paths from escaping the cache directory.
fn valid_name(name: &str) -> bool {
	!name.is_empty()
		&& name.len() <= 64
		&& name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Semver characters only: letters, digits, `.`, `-` and `+`.
fn valid_version(vers: &str) -> bool {
	!vers.is_empty()
		&& !vers.starts_with('.')
		&& vers.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
}

fn invalid() -> io::Error {
	io::Error::new(io::ErrorKind::InvalidInput, "invalid crate name or version")
}

async fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
	match fs::read(path).await {
		Ok(data) => Ok(Some(data)),
		Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
		Err(e) => Err(e),
	}
}

/// Write to a temp file next to `path`, then rename, so readers and concurrent
/// writers never see a partial file.
async fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
	static COUNTER: AtomicU64 = AtomicU64::new(0);

	let dir = path.parent().ok_or_else(invalid)?;
	fs::create_dir_all(dir).await?;
	let n = COUNTER.fetch_add(1, Ordering::Relaxed);
	let tmp = dir.join(format!(".tmp-{}-{n}", std::process::id()));
	fs::write(&tmp, data).await?;
	fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn index_path_layout() {
		assert_eq!(Store::index_path("a").as_deref(), Some("1/a"));
		assert_eq!(Store::index_path("ab").as_deref(), Some("2/ab"));
		assert_eq!(Store::index_path("abc").as_deref(), Some("3/a/abc"));
		assert_eq!(Store::index_path("serde").as_deref(), Some("se/rd/serde"));
		assert_eq!(Store::index_path("Inflector").as_deref(), Some("in/fl/inflector"));
	}

	#[test]
	fn rejects_path_tricks() {
		assert_eq!(Store::index_path(".."), None);
		assert_eq!(Store::index_path("a/b"), None);
		assert_eq!(Store::index_path(""), None);
		let store = Store::new(PathBuf::from("/m"));
		assert_eq!(store.crate_path("serde", "../../x"), None);
		assert_eq!(store.crate_path("serde", ".."), None);
	}

	#[test]
	fn crate_path_keeps_case() {
		let store = Store::new(PathBuf::from("/m"));
		assert_eq!(
			store.crate_path("Inflector", "0.11.4"),
			Some(PathBuf::from("/m/crates/In/fl/Inflector/0.11.4/Inflector-0.11.4.crate"))
		);
	}

	#[tokio::test]
	async fn write_touch_list_remove() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());

		store.write_crate("serde", "1.0.0", b"data").await.unwrap();
		assert!(store.has_crate("serde", "1.0.0"));
		assert_eq!(
			store.read_crate("serde", "1.0.0").await.unwrap().as_deref(),
			Some(&b"data"[..])
		);

		let before = SystemTime::now();
		store.touch("serde", "1.0.0").await.unwrap();
		let crates = store.crates().await.unwrap();
		assert_eq!(crates.len(), 1);
		assert_eq!(
			(crates[0].name.as_str(), crates[0].vers.as_str(), crates[0].size),
			("serde", "1.0.0", 4)
		);
		assert!(crates[0].last_used >= before - std::time::Duration::from_secs(1));

		store.remove(&crates[0].path).await.unwrap();
		assert!(!store.has_crate("serde", "1.0.0"));
		assert!(store.crates().await.unwrap().is_empty());
	}

	#[tokio::test]
	async fn cached_versions_lists_only_crate_files() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		assert!(store.cached_versions("serde").await.unwrap().is_empty());

		store.write_crate("serde", "1.0.0", b"x").await.unwrap();
		store.write_crate("serde", "1.1.0", b"xy").await.unwrap();
		std::fs::create_dir(root.path().join("crates/se/rd/serde/2.0.0")).unwrap();
		let mut found: Vec<_> = store
			.cached_versions("serde")
			.await
			.unwrap()
			.into_iter()
			.map(|f| (f.vers, f.size))
			.collect();
		found.sort();
		assert_eq!(found, [("1.0.0".to_string(), 1), ("1.1.0".to_string(), 2)]);
	}

	#[tokio::test]
	async fn index_synced_needs_a_checkout() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		assert!(store.index_synced().await.is_none());

		std::fs::create_dir_all(store.checkout_dir().join(".git")).unwrap();
		std::fs::write(store.checkout_dir().join(".git/HEAD"), "").unwrap();
		assert!(store.index_synced().await.is_some());
	}

	#[test]
	fn checkout_names_skips_non_index_files() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());
		assert!(store.checkout_names().unwrap().is_empty());

		let dir = store.checkout_dir();
		for path in ["se/rd/serde", "1/a", "3/a/abc"] {
			std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
			std::fs::write(dir.join(path), "").unwrap();
		}
		std::fs::write(dir.join("README.md"), "").unwrap();
		std::fs::write(dir.join("config.json"), "").unwrap();
		std::fs::create_dir_all(dir.join(".git")).unwrap();
		std::fs::write(dir.join(".git/HEAD"), "").unwrap();

		let mut names = store.checkout_names().unwrap();
		names.sort();
		assert_eq!(names, ["a", "abc", "serde"]);
	}

	#[tokio::test]
	async fn index_roundtrip_and_missing() {
		let root = tempfile::tempdir().unwrap();
		let store = Store::new(root.path().to_path_buf());

		assert_eq!(store.read_index("serde").await.unwrap(), None);
		store.write_index("serde", b"line\n").await.unwrap();
		assert_eq!(store.read_index("Serde").await.unwrap().as_deref(), Some("line\n"));
	}
}
