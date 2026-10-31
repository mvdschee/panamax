use std::{io, path::Path};

use tokio::process::Command;

/// Make `dir` a shallow checkout of `url`'s master: clone it the first time, then fetch
/// only the newest commit and reset to it. History is never needed, only the files.
pub async fn shallow_update(url: &str, dir: &Path) -> io::Result<()> {
	if dir.join(".git").exists() {
		run(Command::new("git")
			.arg("-C")
			.arg(dir)
			.args(["fetch", "--quiet", "--depth", "1", "origin", "master"]))
		.await?;
		run(Command::new("git").arg("-C").arg(dir).args([
			"reset",
			"--quiet",
			"--hard",
			"FETCH_HEAD",
		]))
		.await
	} else {
		run(Command::new("git").args(["clone", "--quiet", "--depth", "1", url]).arg(dir)).await
	}
}

async fn run(cmd: &mut Command) -> io::Result<()> {
	let out = cmd.output().await?;
	if out.status.success() {
		Ok(())
	} else {
		Err(io::Error::other(format!(
			"{:?} failed: {}",
			cmd.as_std(),
			String::from_utf8_lossy(&out.stderr).trim()
		)))
	}
}
