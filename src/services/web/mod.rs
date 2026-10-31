use std::{io, net::SocketAddr, path::PathBuf};

use askama::Template;
use axum::{
	Json, Router,
	extract::{Path, State},
	http::{HeaderMap, StatusCode, header},
	response::{Html, IntoResponse, Response},
	routing::get,
};
use include_dir::{Dir, include_dir};
use serde_json::json;
use tower_http::services::ServeDir;

use crate::{
	core::handlers::cache,
	error::CacheError,
	services::{store::Store, upstream::Upstream},
};

const STATIC_DIR: Dir = include_dir!("static");

#[derive(Clone)]
struct AppState {
	root: PathBuf,
	store: Store,
	upstream: Upstream,
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
}

pub async fn serve(root: PathBuf, addr: SocketAddr, offline: bool) -> io::Result<()> {
	let state = AppState {
		store: Store::new(root.clone()),
		upstream: Upstream::new(offline),
		root: root.clone(),
	};

	let app = Router::new()
		.route("/", get(index_page))
		.route("/static/{*path}", get(static_file))
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
	println!("Running HTTP on {addr}{mode}");
	axum::serve(listener, app).await
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
	};
	match page.render() {
		Ok(html) => Html(html).into_response(),
		Err(e) => {
			log::error!("rendering index page failed: {e}");
			StatusCode::INTERNAL_SERVER_ERROR.into_response()
		},
	}
}

async fn static_file(Path(path): Path<String>) -> Response {
	match STATIC_DIR.get_file(&path) {
		Some(file) => {
			let mime = match path.rsplit('.').next() {
				Some("css") => "text/css",
				Some("js") => "text/javascript",
				Some("woff") => "font/woff",
				Some("ttf") => "font/ttf",
				_ => "application/octet-stream",
			};
			([(header::CONTENT_TYPE, mime)], file.contents()).into_response()
		},
		None => StatusCode::NOT_FOUND.into_response(),
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
			log::error!("{e}");
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
