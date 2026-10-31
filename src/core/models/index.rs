use serde::Deserialize;

/// One version from a sparse index file. Only the fields the cache needs are parsed;
/// `raw` is the original line, which is what gets served.
#[derive(Debug, PartialEq, Eq)]
pub struct IndexLine<'a> {
	pub raw: &'a str,
	pub name: String,
	pub vers: String,
	pub cksum: String,
}

#[derive(Deserialize)]
struct Entry {
	name: String,
	vers: String,
	cksum: String,
}

impl<'a> IndexLine<'a> {
	pub fn parse(raw: &'a str) -> Option<Self> {
		let entry: Entry = serde_json::from_str(raw).ok()?;
		Some(Self {
			raw,
			name: entry.name,
			vers: entry.vers,
			cksum: entry.cksum,
		})
	}
}

/// All versions in an index file. Lines that don't parse are skipped.
pub fn lines(file: &str) -> impl Iterator<Item = IndexLine<'_>> {
	file.lines().filter_map(IndexLine::parse)
}

/// The index file with only the versions `keep` accepts, lines byte for byte.
pub fn keep_versions(file: &str, keep: impl Fn(&IndexLine) -> bool) -> String {
	lines(file).filter(|line| keep(line)).fold(String::new(), |mut out, line| {
		out.push_str(line.raw);
		out.push('\n');
		out
	})
}

/// The sha256 cksum of one version, if the index file lists it.
pub fn cksum(file: &str, vers: &str) -> Option<String> {
	lines(file).find(|line| line.vers == vers).map(|line| line.cksum)
}

#[cfg(test)]
mod tests {
	use super::*;

	// features2 and v are fields we don't model; they must survive filtering untouched
	const FILE: &str = concat!(
		r#"{"name":"Demo","vers":"1.0.0","deps":[],"cksum":"aa","features":{},"yanked":false}"#,
		"\n",
		r#"{"name":"Demo","vers":"1.1.0","deps":[],"cksum":"bb","features":{},"features2":{"x":["dep:y"]},"yanked":false,"v":2}"#,
		"\n",
	);

	#[test]
	fn keep_versions_copies_lines_verbatim() {
		let kept = keep_versions(FILE, |line| line.vers == "1.1.0");
		assert_eq!(kept, format!("{}\n", FILE.lines().nth(1).unwrap()));
	}

	#[test]
	fn keep_versions_can_drop_everything() {
		assert_eq!(keep_versions(FILE, |_| false), "");
	}

	#[test]
	fn cksum_finds_the_version() {
		assert_eq!(cksum(FILE, "1.0.0").as_deref(), Some("aa"));
		assert_eq!(cksum(FILE, "2.0.0"), None);
	}

	#[test]
	fn parse_keeps_name_case() {
		assert_eq!(lines(FILE).next().unwrap().name, "Demo");
	}
}
