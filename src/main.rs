#![forbid(unsafe_code)]
use clap::Parser;
use std::{net::IpAddr, path::PathBuf};

#[macro_use]
mod utils;

mod config;
mod core;
mod download;
mod error;
mod mirror;
mod progress_bar;
mod rustup;
mod services;

/// Mirror rustup and cache crates.io, for offline Rust and cargo usage.
#[derive(Debug, Parser)]
enum Panamax {
	/// Create a new mirror directory.
	Init {
		#[arg(value_parser)]
		path: PathBuf,

		/// set [rustup] sync = false
		#[arg(long)]
		ignore_rustup: bool,
	},

	/// Update the rustup mirror and the crates baseline.
	Sync {
		/// Mirror directory.
		#[arg(value_parser)]
		path: PathBuf,

		/// Report what the crates sync would download and delete, and change nothing.
		/// Still updates the index checkout.
		#[arg(long)]
		dry_run: bool,

		/// Allow deleting more than 10% of all crate files in one run.
		#[arg(long)]
		allow_mass_delete: bool,
	},

	/// Serve the rustup mirror and the crates.io cache.
	#[command(name = "serve")]
	Serve {
		/// Mirror directory.
		#[arg(value_parser)]
		path: PathBuf,

		/// IP address to listen on. Defaults to listening on everything.
		#[arg(short, long)]
		listen: Option<IpAddr>,

		/// Port to listen on. Defaults to 8080.
		#[arg(short, long)]
		port: Option<u16>,

		/// Never contact crates.io; serve only what is cached.
		#[arg(long)]
		offline: bool,
	},

	/// List platforms currently available.
	///
	/// This is useful for finding what can be used for
	/// limiting platforms in mirror.toml.
	#[command(name = "list-platforms")]
	ListPlatforms {
		#[arg(long, default_value = "https://static.rust-lang.org")]
		source: String,

		#[arg(long, default_value = "nightly")]
		channel: String,
	},
}

#[tokio::main]
async fn main() {
	let opt = Panamax::parse();
	match opt {
		Panamax::Init {
			path,
			ignore_rustup,
		} => mirror::init(&path, ignore_rustup),
		Panamax::Sync {
			path,
			dry_run,
			allow_mass_delete,
		} => mirror::sync(&path, dry_run, allow_mass_delete).await,
		Panamax::Serve {
			path,
			listen,
			port,
			offline,
		} => mirror::serve(path, listen, port, offline).await,
		Panamax::ListPlatforms {
			source,
			channel,
		} => mirror::list_platforms(source, channel).await,
	}
	.unwrap_or_else(|e| {
		error!("Panamax command failed! {e}");
		std::process::exit(1);
	});
}
