use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
	#[error("IO error: {0}")]
	Io(#[from] io::Error),

	#[error("checksum mismatch for {name} {vers}")]
	Checksum {
		name: String,
		vers: String,
	},
}

#[derive(Debug, Error)]
pub enum SyncError {
	#[error("IO error: {0}")]
	Io(#[from] io::Error),

	#[error("{0} kept versions have a dependency with no kept match; nothing was changed")]
	ClosureViolations(usize),

	#[error(
		"refusing to delete {delete} of {total} crate files (over {percent}%); \
		 check with --dry-run, then pass --allow-mass-delete"
	)]
	MassDelete {
		delete: usize,
		total: usize,
		percent: usize,
	},
}
