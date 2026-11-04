use std::time::Duration;

/// Sparse index the cache passes through and falls back from.
pub const UPSTREAM_INDEX: &str = "https://index.crates.io";

/// Git index that sync computes the baseline from.
pub const UPSTREAM_INDEX_GIT: &str = "https://github.com/rust-lang/crates.io-index";

/// Where `.crate` files are fetched from on a cache miss.
pub const UPSTREAM_CRATES: &str = "https://static.crates.io/crates";

/// A crate outside the baseline is deleted once it has not been served for this long.
pub const KEEP_UNUSED: Duration = Duration::from_secs(180 * 24 * 60 * 60);

/// Parallel downloads during sync. The nas runs sync with a 2 GB memory cap.
pub const SYNC_DOWNLOADS: usize = 16;

/// Share of all crate files one sync may delete without `--allow-mass-delete`.
pub const MAX_DELETE_PERCENT: usize = 10;

/// Upstream gets this long to connect before the cache treats it as offline.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// After a network failure, upstream is skipped for this long. Without it every request
/// in an outage waits out `CONNECT_TIMEOUT`, which turns one build into many minutes.
pub const RETRY_UPSTREAM_AFTER: Duration = Duration::from_secs(30);

/// Whole-request limit for upstream, large enough for big crates on a slow line.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the web page reuses crate names and disk totals before reading them again.
/// Nothing reads them until someone opens the page.
pub const CATALOG_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Most crates one search returns.
pub const SEARCH_LIMIT: usize = 50;

/// Crate files listed as recently used on the index page.
pub const RECENT_LIMIT: usize = 12;
