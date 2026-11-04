use std::{
	io,
	net::SocketAddr,
	path::PathBuf,
	sync::Arc,
	time::{Duration, Instant},
};

use askama::Template;
use axum::{
	Json, Router,
	extract::{Path, Query, State},
	http::{HeaderMap, StatusCode, header},
	response::{Html, IntoResponse, Response},
	routing::get,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tower_http::services::ServeDir;

use crate::{
	config::{CATALOG_MAX_AGE, RECENT_LIMIT, SEARCH_LIMIT},
	core::{
		handlers::{cache, catalog},
		models::catalog::DiskStats,
	},
	error::CacheError,
	services::{store::Store, upstream::Upstream},
};

/// A value read from disk when first asked for, and when it was read.
type Slot<T> = Arc<Mutex<Option<(Instant, Arc<T>)>>>;

#[derive(Clone)]
struct AppState {
	root: PathBuf,
	store: Store,
	upstream: Upstream,
	/// Crate names from the index checkout.
	names: Slot<Vec<String>>,
	/// Totals from walking the crate files.
	disk: Slot<DiskStats>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Platform {
	is_exe: bool,
	platform_triple: String,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
	platforms: Vec<Platform>,
	host: String,
	/// Unix seconds. None before the first sync.
	index_synced: Option<u64>,
	offline: bool,
	upstream_down: bool,
	search_limit: usize,
}

impl IndexTemplate {
	/// The host without its scheme, for the header.
	fn host_name(&self) -> &str {
		self.host.split_once("://").map_or(&self.host, |(_, name)| name)
	}
}

#[derive(Deserialize)]
struct SearchQuery {
	q: String,
}

pub async fn serve(root: PathBuf, addr: SocketAddr, offline: bool) -> io::Result<()> {
	let state = AppState {
		store: Store::new(root.clone()),
		upstream: Upstream::new(offline),
		root: root.clone(),
		names: Arc::default(),
		disk: Arc::default(),
	};

	let app = Router::new()
		.route("/", get(index_page))
		.route("/static/{*path}", get(static_file))
		.route("/api/crates", get(search_crates))
		.route("/api/crates/{name}", get(crate_detail))
		.route("/api/status/index", get(index_status))
		.route("/api/status/disk", get(disk_status))
		.route("/index/config.json", get(index_config))
		.route("/index/{*path}", get(index_file))
		.route("/crates/{name}/{vers}/download", get(crate_file))
		.nest_service("/dist", ServeDir::new(root.join("dist")))
		.nest_service("/rustup", ServeDir::new(root.join("rustup")))
		.with_state(state);

	let listener = tokio::net::TcpListener::bind(addr).await?;
	let mode = if offline {
		" (offline)"
	} else {
		""
	};
	info!("Running on {}{mode}", browser_url(addr));
	axum::serve(listener, app).await
}

/// Where to open this server from the same machine. A wildcard address can't be visited.
fn browser_url(addr: SocketAddr) -> String {
	if addr.ip().is_unspecified() {
		format!("http://localhost:{}", addr.port())
	} else {
		format!("http://{addr}")
	}
}

/// Public URL of this server as the client sees it. Behind a TLS-terminating proxy
/// the scheme comes from X-Forwarded-Proto.
fn base_url(headers: &HeaderMap) -> String {
	let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
	let scheme = header("x-forwarded-proto").unwrap_or("http");
	let host = header("host").unwrap_or("panamax.internal");
	format!("{scheme}://{host}")
}

async fn index_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
	let page = IndexTemplate {
		platforms: rustup_platforms(&state.root).await,
		host: base_url(&headers),
		index_synced: state.store.index_synced().await.map(catalog::unix_secs),
		offline: state.upstream.is_offline(),
		upstream_down: state.upstream.is_down(),
		search_limit: SEARCH_LIMIT,
	};
	match page.render() {
		Ok(html) => Html(html).into_response(),
		Err(e) => {
			error!("rendering index page failed: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

/// The page's own files, in the binary so that the image needs nothing next to it.
async fn static_file(Path(path): Path<String>) -> Response {
	let (body, content_type): (&'static [u8], _) = match path.as_str() {
		"panamax.css" => (include_bytes!("../../../static/panamax.css"), "text/css; charset=utf-8"),
		"panamax.js" => {
			(include_bytes!("../../../static/panamax.js"), "text/javascript; charset=utf-8")
		},
		"fonts/archivo-wordmark.woff2" => {
			(include_bytes!("../../../static/fonts/archivo-wordmark.woff2"), "font/woff2")
		},
		"fonts/hanken-grotesk.woff2" => {
			(include_bytes!("../../../static/fonts/hanken-grotesk.woff2"), "font/woff2")
		},
		"fonts/martian-mono.woff2" => {
			(include_bytes!("../../../static/fonts/martian-mono.woff2"), "font/woff2")
		},
		_ => return StatusCode::NOT_FOUND.into_response(),
	};
	([(header::CONTENT_TYPE, content_type)], body).into_response()
}

async fn search_crates(
	State(state): State<AppState>,
	Query(query): Query<SearchQuery>,
) -> Response {
	let found = match names(&state).await {
		Ok(names) => catalog::search(&state.store, &names, &query.q, SEARCH_LIMIT).await,
		Err(e) => Err(e),
	};
	match found {
		Ok(found) => Json(found).into_response(),
		Err(e) => {
			error!("crate search failed: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

async fn crate_detail(State(state): State<AppState>, Path(name): Path<String>) -> Response {
	match catalog::crate_detail(&state.store, &name).await {
		Ok(Some(found)) => Json(found).into_response(),
		Ok(None) => StatusCode::NOT_FOUND.into_response(),
		Err(e) => {
			error!("reading crate {name} failed: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

async fn index_status(State(state): State<AppState>) -> Response {
	match names(&state).await {
		Ok(names) => Json(json!({ "crates": names.len() })).into_response(),
		Err(e) => {
			error!("reading crate names from the index checkout failed: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

async fn disk_status(State(state): State<AppState>) -> Response {
	let slot = state.disk.clone();
	let store = state.store.clone();
	// spawned so that closing the tab doesn't throw away a walk that is nearly done
	let task = tokio::spawn(async move {
		load_if_old(&slot, CATALOG_MAX_AGE, || catalog::disk_stats(&store, RECENT_LIMIT)).await
	});
	match task.await.map_err(io::Error::from).and_then(|result| result) {
		Ok(stats) => Json(stats.as_ref()).into_response(),
		Err(e) => {
			error!("reading crate files failed: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

async fn names(state: &AppState) -> io::Result<Arc<Vec<String>>> {
	let slot = state.names.clone();
	let store = state.store.clone();
	tokio::spawn(async move {
		load_if_old(&slot, CATALOG_MAX_AGE, || async move {
			tokio::task::spawn_blocking(move || store.checkout_names()).await?
		})
		.await
	})
	.await?
}

/// The value in `slot`, read again with `load` when it is older than `max_age`. Later
/// callers wait on the lock, so only one load runs. A failed load keeps the old value.
async fn load_if_old<T, F, Fut>(slot: &Slot<T>, max_age: Duration, load: F) -> io::Result<Arc<T>>
where
	F: FnOnce() -> Fut,
	Fut: Future<Output = io::Result<T>>,
{
	let mut slot = slot.lock().await;
	if let Some((at, value)) = slot.as_ref()
		&& at.elapsed() < max_age
	{
		return Ok(value.clone());
	}
	match load().await {
		Ok(value) => {
			let value = Arc::new(value);
			*slot = Some((Instant::now(), value.clone()));
			Ok(value)
		},
		Err(e) => match slot.as_ref() {
			Some((_, old)) => {
				warn!("keeping the previous catalog values: {e}");
				Ok(old.clone())
			},
			None => Err(e),
		},
	}
}

/// Generated per request, so the download URL always matches how the client reached us.
async fn index_config(headers: HeaderMap) -> Json<serde_json::Value> {
	Json(json!({ "dl": format!("{}/crates", base_url(&headers)) }))
}

async fn index_file(State(state): State<AppState>, Path(path): Path<String>) -> Response {
	// the path must be exactly the sparse layout for the name it ends in
	let name = path.rsplit('/').next().unwrap_or_default();
	if Store::index_path(name).as_deref() != Some(path.as_str()) {
		return StatusCode::NOT_FOUND.into_response();
	}
	respond(cache::index_file(&state.store, &state.upstream, name).await, "text/plain")
}

async fn crate_file(
	State(state): State<AppState>,
	Path((name, vers)): Path<(String, String)>,
) -> Response {
	respond(
		cache::crate_file(&state.store, &state.upstream, &name, &vers).await,
		"application/x-tar",
	)
}

fn respond(result: Result<Option<Vec<u8>>, CacheError>, content_type: &'static str) -> Response {
	match result {
		Ok(Some(body)) => ([(header::CONTENT_TYPE, content_type)], body).into_response(),
		Ok(None) => StatusCode::NOT_FOUND.into_response(),
		Err(e) => {
			error!("{e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

/// rustup-init platforms in the mirror, unix first.
async fn rustup_platforms(root: &std::path::Path) -> Vec<Platform> {
	let mut out = vec![];
	let Ok(mut dir) = tokio::fs::read_dir(root.join("rustup/dist")).await else {
		return out;
	};
	while let Ok(Some(entry)) = dir.next_entry().await {
		let Some(platform_triple) = entry.file_name().to_str().map(String::from) else {
			continue;
		};
		if entry.path().join("rustup-init").is_file() {
			out.push(Platform {
				is_exe: false,
				platform_triple,
			});
		} else if entry.path().join("rustup-init.exe").is_file() {
			out.push(Platform {
				is_exe: true,
				platform_triple,
			});
		}
	}
	out.sort();
	out
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicUsize, Ordering};

	use super::*;

	const HOUR: Duration = Duration::from_secs(3600);

	#[tokio::test]
	async fn nothing_is_read_again_within_max_age() {
		let slot: Slot<u32> = Arc::default();
		let loads = AtomicUsize::new(0);
		for _ in 0..3 {
			let value = load_if_old(&slot, HOUR, || async {
				loads.fetch_add(1, Ordering::Relaxed);
				Ok(7)
			})
			.await
			.unwrap();
			assert_eq!(*value, 7);
		}
		assert_eq!(loads.load(Ordering::Relaxed), 1);
	}

	#[tokio::test]
	async fn old_value_is_read_again() {
		let slot: Slot<u32> = Arc::default();
		load_if_old(&slot, HOUR, || async { Ok(1) }).await.unwrap();
		let value = load_if_old(&slot, Duration::ZERO, || async { Ok(2) }).await.unwrap();
		assert_eq!(*value, 2);
	}

	#[tokio::test]
	async fn failed_load_keeps_the_old_value_and_tries_again() {
		let slot: Slot<u32> = Arc::default();
		let fail = || async { Err(io::Error::other("walk failed")) };
		assert!(load_if_old(&slot, HOUR, fail).await.is_err());

		load_if_old(&slot, HOUR, || async { Ok(1) }).await.unwrap();
		assert_eq!(*load_if_old(&slot, Duration::ZERO, fail).await.unwrap(), 1);
		assert_eq!(*load_if_old(&slot, Duration::ZERO, || async { Ok(2) }).await.unwrap(), 2);
	}

	#[tokio::test]
	async fn concurrent_callers_share_one_load() {
		let slot: Slot<u32> = Arc::default();
		let loads = AtomicUsize::new(0);
		let call = || {
			load_if_old(&slot, HOUR, || async {
				loads.fetch_add(1, Ordering::Relaxed);
				tokio::task::yield_now().await;
				Ok(7)
			})
		};
		let (a, b) = tokio::join!(call(), call());
		assert_eq!((*a.unwrap(), *b.unwrap()), (7, 7));
		assert_eq!(loads.load(Ordering::Relaxed), 1);
	}
}
