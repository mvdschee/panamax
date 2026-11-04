use std::{
	fs, io,
	net::{IpAddr, SocketAddr},
	path::{Path, PathBuf},
};

use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::rustup::download_platform_list;

#[derive(Error, Debug)]
pub enum MirrorError {
	#[error("IO error: {0}")]
	Io(#[from] io::Error),

	#[error("TOML deserialization error: {0:?}")]
	Parse(#[from] toml::de::Error),

	#[error("Config file error: {0}")]
	Config(String),

	#[error("Download error: {0}")]
	DownloadError(#[from] crate::download::DownloadError),

	#[error("Crates sync error: {0}")]
	Sync(#[from] crate::error::SyncError),

	#[error("Toml error: {0}")]
	Serialize(#[from] toml_edit::TomlError),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigMirror {
	pub retries: usize,
	pub contact: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigRustup {
	pub sync: bool,
	pub download_threads: usize,
	pub source: String,
	pub download_dev: Option<bool>,
	pub download_gz: Option<bool>,
	pub download_xz: Option<bool>,
	pub platforms_unix: Option<Vec<String>>,
	pub platforms_windows: Option<Vec<String>>,
	pub keep_latest_stables: Option<usize>,
	pub keep_latest_betas: Option<usize>,
	pub keep_latest_nightlies: Option<usize>,
	pub pinned_rust_versions: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigCrates {
	pub sync: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
	pub mirror: ConfigMirror,
	pub rustup: Option<ConfigRustup>,
	pub crates: Option<ConfigCrates>,
}

pub fn create_mirror_directories(path: &Path, ignore_rustup: bool) -> Result<(), io::Error> {
	if !ignore_rustup {
		// Rustup directories
		fs::create_dir_all(path.join("rustup/dist"))?;
		fs::create_dir_all(path.join("dist"))?;
	}

	// Crate cache directories
	fs::create_dir_all(path.join("index"))?;
	fs::create_dir_all(path.join("crates"))?;
	Ok(())
}

pub fn create_mirror_toml(path: &Path, ignore_rustup: bool) -> Result<bool, MirrorError> {
	if path.join("mirror.toml").exists() {
		return Ok(false);
	}

	// Read the defautlt toml, edit if required, using toml_edit to keep format
	let config = include_str!("mirror.default.toml");
	let mut config = config.parse::<toml_edit::DocumentMut>()?;

	if ignore_rustup {
		config["rustup"]["sync"] = toml_edit::value(false);
	}

	let path = path.join("mirror.toml");
	let bytes = config.to_string();
	fs::write(path, bytes)?;

	Ok(true)
}

pub fn load_mirror_toml(path: &Path) -> Result<Config, MirrorError> {
	Ok(toml::from_str(&fs::read_to_string(path.join("mirror.toml"))?)?)
}

pub fn init(path: &Path, ignore_rustup: bool) -> Result<(), MirrorError> {
	create_mirror_directories(path, ignore_rustup)?;
	if create_mirror_toml(path, ignore_rustup)? {
		info!("Successfully created mirror base at `{}`.", path.display());
	} else {
		info!("Mirror base already exists at `{}`.", path.display());
	}
	info!(
		"Make any desired changes to {}/mirror.toml, then run panamax sync {} for rustup, \
		 and panamax serve {} for rustup and the crate cache.",
		path.display(),
		path.display(),
		path.display()
	);

	Ok(())
}

pub fn default_user_agent() -> String {
	format!("Panamax/{}", env!("CARGO_PKG_VERSION"))
}

pub async fn sync(path: &Path, dry_run: bool, allow_mass_delete: bool) -> Result<(), MirrorError> {
	if !path.join("mirror.toml").exists() {
		error!("Mirror base not found! Run panamax init {} first.", path.display());
		return Ok(());
	}
	let mirror = load_mirror_toml(path)?;

	// Handle the contact information
	let user_agent_str = if let Some(ref contact) = mirror.mirror.contact {
		if contact != "your@email.com" {
			format!("Panamax/{} ({})", env!("CARGO_PKG_VERSION"), contact)
		} else {
			default_user_agent()
		}
	} else {
		default_user_agent()
	};

	// Set the user agent with contact information.
	let user_agent = match HeaderValue::from_str(&user_agent_str) {
		Ok(h) => h,
		Err(e) => {
			error!("Your contact information contains invalid characters!");
			error!("It's recommended to use a URL or email address as contact information.");
			error!("{e:?}");
			return Ok(());
		},
	};

	// toolchains we pin are the ones crate versions must still build on (MSRV rule)
	let toolchains = mirror
		.rustup
		.as_ref()
		.and_then(|r| r.pinned_rust_versions.as_ref())
		.map(|v| v.iter().filter_map(|s| crate::core::models::keep::parse_rust(s)).collect())
		.unwrap_or_default();

	// rustup and crates are independent: a failure in one doesn't skip the other
	let mut rustup_result = Ok(());
	let mut crates_result = Ok(None);

	if let Some(rustup) = mirror.rustup {
		if dry_run {
			info!("Dry run, skipping rustup...");
		} else if rustup.sync {
			rustup_result = crate::rustup::sync(path, &mirror.mirror, &rustup, &user_agent).await;
		} else {
			info!("Rustup sync is disabled, skipping...");
		}
	} else {
		info!("Rustup section missing, skipping...");
	}

	if mirror.crates.is_some_and(|c| c.sync) {
		info!("{}", console::style("Syncing crates baseline...").bold());
		let opts = crate::core::handlers::sync::Options {
			dry_run,
			allow_mass_delete,
			toolchains,
		};
		let store = crate::services::store::Store::new(path.to_path_buf());
		let upstream = crate::services::upstream::Upstream::new(false);
		crates_result =
			crate::core::handlers::sync::sync_crates(&store, &upstream, &opts).await.map(Some);
	} else {
		info!("Crates sync is disabled, skipping...");
	}

	if let Ok(Some(r)) = &crates_result {
		let verb = if dry_run {
			"would "
		} else {
			""
		};
		info!("  releases in index:  {}", r.releases);
		info!("  kept (baseline):    {}", r.kept);
		info!("  crate files:        {}", r.on_disk);
		info!("  kept but missing:   {}", r.missing);
		if !dry_run {
			info!("  downloaded:         {} ({} failed)", r.downloaded, r.failed);
		}
		info!(
			"  {verb}delete:  {:>11} ({:.1} GB, outside baseline and unused for 180 days)",
			r.deleted,
			r.deleted_bytes as f64 / 1e9
		);
		if dry_run && r.mass_delete && !allow_mass_delete {
			warn!("  that is over 10% of all crate files: a real run needs --allow-mass-delete");
		}
	}

	match (rustup_result, crates_result) {
		(Ok(()), Ok(_)) => {
			info!("Sync complete.");
			Ok(())
		},
		(Err(e), Ok(_)) => {
			error!("Rustup sync failed.");
			Err(e)
		},
		(Ok(()), Err(e)) => {
			error!("Crates sync failed.");
			Err(e.into())
		},
		(Err(rustup), Err(crates)) => {
			error!("Rustup sync failed: {rustup}");
			Err(crates.into())
		},
	}
}

pub async fn serve(
	path: PathBuf,
	listen: Option<IpAddr>,
	port: Option<u16>,
	offline: bool,
) -> Result<(), MirrorError> {
	let listen =
		listen.unwrap_or_else(|| "::".parse().expect(":: IPv6 address should never fail to parse"));
	let socket_addr = SocketAddr::new(listen, port.unwrap_or(8080));
	crate::services::web::serve(path, socket_addr, offline).await?;
	Ok(())
}

/// Print out a list of all platforms.
pub(crate) async fn list_platforms(source: String, channel: String) -> Result<(), MirrorError> {
	let targets = download_platform_list(source.as_str(), channel.as_str()).await?;

	println!("All currently available platforms for the {channel} channel:");
	for t in targets {
		println!("  {t}");
	}

	Ok(())
}
