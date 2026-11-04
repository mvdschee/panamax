use std::{
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use reqwest::{Client, StatusCode};

use crate::config::{
	CONNECT_TIMEOUT, REQUEST_TIMEOUT, RETRY_UPSTREAM_AFTER, UPSTREAM_CRATES, UPSTREAM_INDEX,
};

/// Result of asking crates.io for a file.
#[derive(Debug)]
pub enum Fetched {
	Found(Bytes),
	/// Upstream answered that the file doesn't exist.
	NotFound,
	/// No usable answer: network error, timeout, 5xx, rate limit, or offline mode.
	Unreachable,
}

/// crates.io, as seen by the cache.
#[derive(Clone)]
pub struct Upstream {
	/// None means offline mode: never contact upstream.
	client: Option<Client>,
	/// Unix millis until which upstream is treated as down, shared by all clones.
	down_until: Arc<AtomicU64>,
	index_url: String,
	crates_url: String,
}

impl Upstream {
	pub fn new(offline: bool) -> Self {
		Self::with_urls(offline, UPSTREAM_INDEX, UPSTREAM_CRATES)
	}

	pub fn with_urls(offline: bool, index_url: &str, crates_url: &str) -> Self {
		let client = (!offline).then(|| {
			Client::builder()
				.user_agent(concat!("Panamax/", env!("CARGO_PKG_VERSION")))
				.connect_timeout(CONNECT_TIMEOUT)
				.timeout(REQUEST_TIMEOUT)
				.build()
				.expect("static client config is valid")
		});
		Self {
			client,
			down_until: Arc::new(AtomicU64::new(0)),
			index_url: index_url.trim_end_matches('/').to_string(),
			crates_url: crates_url.trim_end_matches('/').to_string(),
		}
	}

	/// True in offline mode, when upstream is never contacted.
	pub fn is_offline(&self) -> bool {
		self.client.is_none()
	}

	/// True while a recent network failure has upstream being skipped.
	pub fn is_down(&self) -> bool {
		now_millis() < self.down_until.load(Ordering::Relaxed)
	}

	/// An index file by its sparse path, e.g. `se/rd/serde`.
	pub async fn index_file(&self, path: &str) -> Fetched {
		self.get(&format!("{}/{path}", self.index_url)).await
	}

	pub async fn crate_file(&self, name: &str, vers: &str) -> Fetched {
		self.get(&format!("{}/{name}/{name}-{vers}.crate", self.crates_url)).await
	}

	async fn get(&self, url: &str) -> Fetched {
		let Some(client) = &self.client else {
			return Fetched::Unreachable;
		};
		if now_millis() < self.down_until.load(Ordering::Relaxed) {
			return Fetched::Unreachable;
		}
		let res = match client.get(url).send().await {
			Ok(res) => res,
			Err(e) => {
				warn!(
					"upstream unreachable for {url}, skipping it for {}s: {e}",
					RETRY_UPSTREAM_AFTER.as_secs()
				);
				let until = now_millis() + RETRY_UPSTREAM_AFTER.as_millis() as u64;
				self.down_until.store(until, Ordering::Relaxed);
				return Fetched::Unreachable;
			},
		};
		match res.status() {
			StatusCode::OK => match res.bytes().await {
				Ok(body) => Fetched::Found(body),
				Err(e) => {
					warn!("upstream body failed for {url}: {e}");
					Fetched::Unreachable
				},
			},
			StatusCode::NOT_FOUND | StatusCode::GONE => Fetched::NotFound,
			status => {
				warn!("upstream returned {status} for {url}");
				Fetched::Unreachable
			},
		}
	}
}

fn now_millis() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
	use std::time::Instant;

	use super::*;

	#[tokio::test]
	async fn network_failure_skips_upstream_for_a_while() {
		// nothing listens on port 1, so the connection is refused straight away
		let upstream = Upstream::with_urls(false, "http://127.0.0.1:1", "http://127.0.0.1:1");
		assert!(matches!(upstream.index_file("se/rd/serde").await, Fetched::Unreachable));
		assert!(upstream.down_until.load(Ordering::Relaxed) > now_millis());

		// a clone shares the state and doesn't try again
		let start = Instant::now();
		assert!(matches!(
			upstream.clone().crate_file("serde", "1.0.0").await,
			Fetched::Unreachable
		));
		assert!(start.elapsed().as_millis() < 50);
	}
}
