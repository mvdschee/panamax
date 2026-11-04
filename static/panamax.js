const view = document.getElementById("view");
const query = document.getElementById("query");
const { host, offline, searchLimit } = document.body.dataset;
const EXTERNAL = { target: "_blank", rel: "noopener noreferrer" };
const NOT_STORED = offline === "true"
	? "Not stored, and this mirror is offline. Builds that need this version will fail."
	: "Not stored. Cargo gets it from crates.io the first time a build needs it.";

function h(tag, props, ...children) {
	const el = document.createElement(tag);
	for (const [key, value] of Object.entries(props ?? {})) {
		if (key === "class") el.className = value;
		else if (value !== undefined && value !== null) el.setAttribute(key, value);
	}
	el.append(...children.flat(Infinity).filter((c) => c !== null && c !== undefined));
	return el;
}

const count = (n) => n.toLocaleString("en");

function bytes(n) {
	const units = ["B", "kB", "MB", "GB", "TB"];
	let i = 0;
	while (n >= 1000 && i < units.length - 1) { n /= 1000; i++; }
	return `${i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)} ${units[i]}`;
}

function ago(unix) {
	const secs = Math.max(0, Date.now() / 1000 - unix);
	const steps = [[60, "second"], [3600, "minute"], [86400, "hour"], [86400 * 30, "day"], [86400 * 365, "month"], [Infinity, "year"]];
	const sizes = [1, 60, 3600, 86400, 86400 * 30, 86400 * 365];
	const i = steps.findIndex(([limit]) => secs < limit);
	const n = Math.floor(secs / sizes[i]);
	return n === 0 && i === 0 ? "just now" : `${n} ${steps[i][1]}${n === 1 ? "" : "s"} ago`;
}

function date(iso) {
	return new Date(iso).toLocaleDateString("en-GB", { day: "numeric", month: "short", year: "numeric" });
}

function dress(root) {
	root.querySelectorAll("[data-unix]").forEach((el) => {
		el.textContent = ago(+el.dataset.unix);
		el.title = new Date(el.dataset.unix * 1000).toLocaleString("en-GB");
	});
}

function describe(v, latest) {
	const parts = [v.vers, v.cached ? "stored here" : "not stored"];
	if (v.yanked) parts.push("withdrawn by the author (yanked)");
	if (v.vers === latest) parts.push("latest");
	return parts.join(", ");
}

const chipClass = (v) => `chip${v.cached ? " stored" : ""}${v.yanked ? " out" : ""}`;
const versions = (n) => `${count(n)} ${n === 1 ? "version" : "versions"}`;

/* Cargo's compatibility line: 1.2.3 is line 1, 0.8.3 is line 0.8, 0.0.3 is line 0.0.3. */
function line(vers) {
	const [major, minor, patch] = vers.split(/[.+-]/);
	if (major !== "0") return major;
	return minor !== "0" ? `0.${minor}` : `0.0.${patch}`;
}

/* The version without its line: 0.4.12 is .12 and 1.54.1 is .54.1. A 0.0.x line holds one version, so that keeps its full number. */
function chipLabel(vers) {
	const name = line(vers);
	return name.startsWith("0.0.") ? vers : vers.slice(name.length);
}

function key(stored, notStored, out) {
	const entry = (cls, sample, label, title) => h("li", { title }, h("span", { class: `chip ${cls}` }, sample), label);
	return h("ul", { class: "key" },
		entry("stored", stored, "Stored here"),
		entry("", notStored, "Not stored"),
		entry("out", out, "Withdrawn by the author", "crates.io calls this yanked"));
}

/* Views */

/* Sync writes this file, so reading it touches no crate files. */
let status = null;

function showStatus() {
	status ??= fetchJson("api/status");
	const fill = (id, text) => {
		const el = document.getElementById(id);
		if (el) el.textContent = text;
	};
	const showRecent = (content) => document.getElementById("recent")?.replaceChildren(content);
	status.then(
		(found) => {
			if (!found) {
				fill("crate-count", "Not counted yet");
				fill("disk-totals", "Not counted yet");
				showRecent(h("p", { class: "note" }, "The next panamax sync counts the crate files."));
				return;
			}
			fill("crate-count", `${count(found.crates)} crates`);
			fill("disk-totals", `${count(found.disk.files)} crate files, ${bytes(found.disk.bytes)}, counted ${ago(found.written)}`);
			showRecent(recent(found.disk.recent));
		},
		(error) => {
			fill("crate-count", "Unavailable");
			fill("disk-totals", "Unavailable");
			showRecent(h("p", { class: "note" }, error.message));
		});
}

function recent(files) {
	if (files.length === 0) return h("p", { class: "note" }, "Nothing stored yet. Point cargo at this mirror and build something.");
	return h("ul", { class: "rows" }, files.map((f) => h("li", null,
		h("a", { class: "row recent", href: `#/crates/${f.name}/${f.vers}` },
			h("span", { class: "name" }, f.name),
			h("span", { class: "vers" }, f.vers),
			h("time", { class: "label", title: new Date(f.last_used * 1000).toLocaleString("en-GB") }, ago(f.last_used))))));
}

function showHome() {
	view.replaceChildren(document.getElementById("home").content.cloneNode(true));
	showStatus();
	const select = document.getElementById("platform");
	if (select) {
		const update = () => {
			const option = select.selectedOptions[0];
			const triple = option.textContent;
			const pre = document.getElementById("rustup-init");
			document.getElementById("rustup-copy").hidden = option.dataset.exe === "true";
			if (option.dataset.exe === "true") {
				const url = `${host}/rustup/dist/${triple}/rustup-init.exe`;
				pre.replaceChildren("Download rustup-init.exe here:\n", h("a", { href: url }, url));
			} else {
				const url = `${host}/rustup/dist/${triple}/rustup-init`;
				pre.replaceChildren("wget ", h("a", { href: url }, url), "\nchmod +x rustup-init\n./rustup-init");
			}
		};
		select.addEventListener("change", update);
		update();
	}
}

async function fetchJson(url) {
	const response = await fetch(url);
	if (response.status === 404) return null;
	if (!response.ok) throw new Error(`The server answered ${response.status}. Its log has the cause.`);
	return response.json();
}

/* The server stops at its limit, so a full page says only that these are the first ones. */
function matches(n, q) {
	if (n >= +searchLimit) return `First ${count(n)} crates matching “${q}”`;
	return `${count(n)} ${n === 1 ? "crate matches" : "crates match"} “${q}”`;
}

function hit(krate) {
	const stored = krate.versions.filter((v) => v.cached);
	const withdrawn = krate.versions.filter((v) => v.yanked).length;
	return h("li", null,
		h("a", { class: "row hit", href: `#/crates/${krate.name}` },
			h("span", { class: "name" }, krate.name),
			h("span", { class: "vers" }, krate.latest ?? "all withdrawn"),
			h("span", { class: "held" },
				stored.map((v) => h("span", { class: chipClass(v), title: describe(v, krate.latest) }, v.vers)),
				h("span", null, stored.length > 0
					? `${stored.length} of ${versions(krate.versions.length)} stored`
					: `None stored · ${versions(krate.versions.length)}`),
				withdrawn > 0 ? h("span", { class: "withdrawn", title: "crates.io calls this yanked" }, `${withdrawn} withdrawn`) : null)));
}

async function showSearch(q, stale) {
	const found = await fetchJson(`api/crates?q=${encodeURIComponent(q)}`);
	if (stale()) return;
	if (!found) {
		view.replaceChildren(h("p", { class: "note" }, "Search starts working after the next panamax sync."));
		return;
	}
	if (found.length === 0) {
		view.replaceChildren(h("p", { class: "note" }, `No crate in the index has “${q}” in its name.`));
		return;
	}
	view.replaceChildren(
		h("div", { class: "section-head" },
			h("p", { class: "label section-label" }, matches(found.length, q)),
			key("1.0", "1.0", "1.0")),
		h("div", null,
			h("div", { class: "label hits-head" }, h("span", null, "Crate"), h("span", null, "Latest"), h("span", null, "On this mirror")),
			h("ul", { class: "rows" }, found.map(hit))));
}

let loaded = null;

async function showCrate(name, vers, stale) {
	let detail = loaded?.name.toLowerCase() === name.toLowerCase() ? loaded : null;
	if (!detail) {
		detail = await fetchJson(`api/crates/${encodeURIComponent(name)}`);
		if (stale()) return;
		if (!detail) {
			view.replaceChildren(h("p", { class: "note" }, `The index has no crate named “${name}”. Names here match the index exactly, so try searching.`));
			return;
		}
		loaded = detail;
		window.scrollTo(0, 0);
	}
	document.title = `${detail.name} · Panamax`;
	const refocus = document.activeElement?.classList.contains("chip");
	const current = detail.versions.find((v) => v.vers === vers)
		?? detail.versions.find((v) => v.vers === detail.latest)
		?? detail.versions[0];

	const add = `cargo add ${detail.name}`;
	view.replaceChildren(
		h("div", { class: "crate-head" },
			h("h1", null, detail.name),
			h("div", { class: "lede" },
				h("div", { class: "add" }, h("code", null, add), h("button", { type: "button", class: "copy" }, "Copy")),
				h("span", null, `${detail.versions.filter((v) => v.file).length} of ${versions(detail.versions.length)} stored here`),
				h("span", { class: "links" },
					h("a", { class: "btn", href: `https://crates.io/crates/${detail.name}`, ...EXTERNAL }, "crates.io ↗"),
					h("a", { class: "btn", href: `https://docs.rs/${detail.name}`, ...EXTERNAL }, "docs.rs ↗")))),
		h("section", { class: "versions" },
			h("div", { class: "section-head" }, h("p", { class: "label section-label" }, "Versions"), key(".12", ".11", ".10")),
			plan(detail, current)),
		version(detail, current));
	if (refocus) view.querySelector("a.chip[aria-current]")?.focus();
}

function plan(detail, current) {
	const lines = new Map();
	for (const v of detail.versions) {
		const name = line(v.vers);
		if (!lines.has(name)) lines.set(name, []);
		lines.get(name).push(v);
	}
	return h("div", { class: "lines" },
		[...lines].map(([name, versions]) => h("div", { class: "line" },
			h("span", { class: "line-name" }, name),
			h("div", { class: "chips" }, versions.map((v) => {
				const state = { vers: v.vers, cached: !!v.file, yanked: v.yanked };
				const isCurrent = v === current;
				return h("a", {
					class: chipClass(state),
					href: `#/crates/${detail.name}/${v.vers}`,
					title: describe(state, detail.latest),
					"aria-label": describe(state, detail.latest),
					"aria-current": isCurrent ? "true" : null,
					tabindex: isCurrent ? 0 : -1,
				}, chipLabel(v.vers));
			})))));
}

function version(detail, v) {
	const fact = (label, ...value) => [h("dt", { class: "label" }, label), h("dd", null, value)];
	const kinds = [["normal", "Dependencies", "No dependencies"], ["build", "Build dependencies"], ["dev", "Dev dependencies", "No dev dependencies"]];
	const features = Object.entries(v.features);
	return h("section", null,
		h("div", { class: "version-head" },
			h("h2", null, v.vers),
			v.vers === detail.latest ? h("span", { class: "label tag" }, "Latest") : null),
		h("dl", { class: "facts" },
			fact("Here",
				v.file
					? h("div", { class: "stored-line" },
						h("span", null, `Stored, ${bytes(v.file.size)}, added or last used ${ago(v.file.last_used)}.`),
						h("a", { class: "btn", href: `crates/${detail.name}/${v.vers}/download`, download: `${detail.name}-${v.vers}.crate` }, "Download .crate"))
					: NOT_STORED,
				v.yanked ? h("p", null, h("b", null, "Withdrawn by the author."), " New builds won't pick it. Projects that already lock this version can still use it. crates.io calls this “yanked”.") : null),
			v.published ? fact("Released", date(v.published)) : null,
			v.rust_version ? fact("Needs", `Rust ${v.rust_version} or newer`) : null),
		h("div", { class: "columns" },
			kinds.map(([kind, title, none]) => {
				const deps = v.deps.filter((d) => d.kind === kind);
				return deps.length > 0 || none ? column(title, none, deps.map(dep)) : null;
			}),
			column("Features", "No features", features.map(([name, enables]) => h("li", { class: "feature" },
				h("code", null, name),
				enables.length > 0 ? h("span", null, `turns on ${enables.join(", ")}`) : null)))));
}

function column(title, none, items) {
	return h("div", { class: "column" }, items.length === 0
		? h("h3", null, none)
		: [h("h3", null, `${title} `, h("span", null, items.length)), h("ul", { class: "rows" }, items)]);
}

function dep(d) {
	return h("li", null,
		h("a", { class: "row dep", href: `#/crates/${d.name}` },
			h("span", { class: "name" }, d.name),
			h("span", { class: "vers" }, d.req),
			d.optional ? h("span", { class: "label" }, "Optional") : null,
			d.target ? h("span", { class: "target" }, d.target) : null));
}

/* Routes: #/, #/search/<q>, #/crates/<name>[/<vers>] */

let route = 0;

async function render() {
	const seq = ++route;
	const stale = () => seq !== route;
	const [kind, first, second] = location.hash.slice(2).split("/").map(decodeURIComponent);
	document.body.dataset.view = kind === "search" || kind === "crates" ? kind : "home";
	try {
		if (kind === "search" && first) {
			document.title = `${first} · Panamax`;
			if (document.activeElement !== query) query.value = first;
			await showSearch(first, stale);
		} else if (kind === "crates" && first) {
			await showCrate(first, second, stale);
		} else {
			document.title = "Panamax";
			query.value = "";
			showHome();
		}
	} catch (error) {
		if (!stale()) view.replaceChildren(h("p", { class: "note" }, error.message));
	}
}

let typing;
query.addEventListener("input", () => {
	clearTimeout(typing);
	typing = setTimeout(() => {
		const q = query.value.trim();
		const target = q ? `#/search/${encodeURIComponent(q)}` : "#/";
		// one history entry per search, not one per keystroke
		if (location.hash.startsWith("#/search/") && q) {
			history.replaceState(null, "", target);
			render();
		} else {
			location.hash = target;
		}
	}, 150);
});

/* Arrow keys walk the version chips, which are one tab stop. */
view.addEventListener("keydown", (event) => {
	const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
	if (!step || !event.target.matches("a.chip")) return;
	const chips = [...view.querySelectorAll("a.chip")];
	const next = chips[chips.indexOf(event.target) + step];
	if (next) {
		event.preventDefault();
		location.hash = next.getAttribute("href");
	}
});

/* navigator.clipboard exists only on https and localhost. Over plain http the text gets selected and copied the old way. */
view.addEventListener("click", async (event) => {
	const button = event.target.closest("button.copy");
	if (!button) return;
	const source = button.closest(".code, .add").querySelector("pre, code");
	let done = true;
	try {
		await navigator.clipboard.writeText(source.textContent);
	} catch {
		getSelection().selectAllChildren(source);
		done = document.execCommand("copy");
	}
	button.textContent = done ? "Copied" : "Press Ctrl+C";
	setTimeout(() => { button.textContent = "Copy"; }, 1500);
});

window.addEventListener("hashchange", render);
dress(document.querySelector(".status"));
render();
