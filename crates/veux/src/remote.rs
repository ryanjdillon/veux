//! Reach the devices of a remote host through `veux helper` over SSH.
//!
//! veux finds the host architecture with `uname -m`, and uploads a static
//! helper for it to `~/.cache/veux/veux-<sha256>` when that file is missing.
//! A host config with `helper_path` skips the upload. One SSH session serves
//! all devices of a host.

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use veux_model::Host;
use veux_rpc::Session;

pub type SshSession = Session<HelperOutput, ChildStdin>;

/// The output of the helper process. It owns the process, so the process
/// lives as long as the session: when the last device that shares the
/// session is dropped, the process ends.
pub struct HelperOutput {
    reader: BufReader<ChildStdout>,
    child: Child,
}

impl Read for HelperOutput {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

impl BufRead for HelperOutput {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.reader.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.reader.consume(amount);
    }
}

impl Drop for HelperOutput {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/helpers.rs"));
}

/// Start the helper on a host.
pub fn start(name: &str, host: &Host) -> Result<Rc<RefCell<SshSession>>> {
    let path = match &host.helper_path {
        Some(path) => path.display().to_string(),
        None => push_helper(&host.ssh).with_context(|| format!("host {name}: helper upload"))?,
    };
    let mut child = ssh(&host.ssh)
        .arg(format!("{path} helper"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The TUI owns the terminal, so helper diagnostics must not reach it.
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("host {name}: cannot run ssh"))?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let stdout = child.stdout.take().expect("stdout is piped");
    let output = HelperOutput {
        reader: BufReader::new(stdout),
        child,
    };
    let session =
        Session::start(output, stdin).map_err(|e| anyhow!("host {name}: helper {path}: {e}"))?;
    Ok(Rc::new(RefCell::new(session)))
}

fn ssh(destination: &str) -> Command {
    let mut command = Command::new("ssh");
    command
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
        .arg(destination);
    command
}

/// Run a short command on the host and return its output.
fn run(destination: &str, remote: &str, input: Option<&[u8]>) -> Result<String> {
    let mut child = ssh(destination)
        .arg(remote)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run ssh")?;
    if let Some(bytes) = input {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(bytes).context("upload")?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "ssh {destination} {remote}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Make sure the host has the helper for its architecture, and return its
/// path on the host.
fn push_helper(destination: &str) -> Result<String> {
    let arch = run(destination, "uname -m", None)?.trim().to_string();
    let binary = helper_binary(&arch)?;
    let name = helper_name(&binary);
    let path = format!("~/.cache/veux/{name}");
    let present = run(
        destination,
        &format!("test -x {path} && echo present || true"),
        None,
    )?;
    if present.trim() != "present" {
        // Upload to a temporary name, so that an interrupted upload never
        // leaves a broken file under the final name.
        run(
            destination,
            &format!(
                "mkdir -p ~/.cache/veux && cat > {path}.part && chmod 755 {path}.part && mv {path}.part {path}"
            ),
            Some(&binary),
        )?;
    }
    Ok(path)
}

/// `veux-` and the first 16 hex digits of the SHA-256 of the binary.
fn helper_name(binary: &[u8]) -> String {
    let digest = Sha256::digest(binary);
    let mut name = String::from("veux-");
    for byte in &digest[..8] {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

/// The static helper for an architecture. `VEUX_HELPER_<ARCH>` names a file
/// to use instead of the embedded one, for development.
fn helper_binary(arch: &str) -> Result<Cow<'static, [u8]>> {
    let var = format!("VEUX_HELPER_{}", arch.to_uppercase());
    if let Ok(path) = std::env::var(&var) {
        let bytes = std::fs::read(&path).with_context(|| format!("{var}={path}"))?;
        return Ok(Cow::Owned(bytes));
    }
    let embedded = match arch {
        "aarch64" => embedded::AARCH64,
        "x86_64" => embedded::X86_64,
        _ => None,
    };
    embedded.map(Cow::Borrowed).ok_or_else(|| {
        anyhow!(
            "no helper for {arch}: build veux with `just release`, set {var} to a static \
             helper, or set helper_path for the host"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_name_is_stable_and_short() {
        assert_eq!(helper_name(b"veux"), helper_name(b"veux"));
        assert_ne!(helper_name(b"veux"), helper_name(b"veux2"));
        assert_eq!(helper_name(b"").len(), "veux-".len() + 16);
        // SHA-256 of the empty input starts with e3b0c442 98fc1c14.
        assert_eq!(helper_name(b""), "veux-e3b0c44298fc1c14");
    }

    #[test]
    fn unknown_architecture_has_no_helper() {
        let error = helper_binary("mips").unwrap_err().to_string();
        assert!(error.contains("no helper for mips"), "{error}");
    }
}
