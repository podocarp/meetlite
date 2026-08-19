//! Secure installation of the macOS system-audio capture companion.
//!
//! A release manifest is JSON with exactly `version`, `archive_url`,
//! `archive_sha256`, and `signature`. Its signature is Ed25519 over UTF-8:
//! `version=<value>\narchive_url=<value>\narchive_sha256=<value>\n`.
//! Values may not contain line breaks, so this representation is unambiguous.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MANIFEST_URL_PREFIX: &str =
    "https://github.com/podocarp/meetlite/releases/latest/download/MeetliteCapture-macos-";
const PINNED_PUBLIC_KEY: &str = "ceaMls+PPX9aKIWeBNhsAmyqv4OJxnQehljN/Gnh1rE=";
const APP_NAME: &str = "MeetliteCapture.app";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    archive_url: String,
    archive_sha256: String,
    signature: String,
}

impl Manifest {
    fn canonical_payload(&self) -> Result<Vec<u8>> {
        validate_value("version", &self.version)?;
        validate_value("archive URL", &self.archive_url)?;
        if !self
            .version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            bail!("manifest version contains unsupported characters")
        }
        let url =
            reqwest::Url::parse(&self.archive_url).context("manifest archive URL is invalid")?;
        if url.scheme() != "https" || url.host_str().is_none() {
            bail!("manifest archive URL must be an absolute HTTPS URL")
        }
        if self.archive_sha256.len() != 64
            || !self
                .archive_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self
                .archive_sha256
                .bytes()
                .any(|byte| byte.is_ascii_uppercase())
        {
            bail!("manifest archive SHA-256 must be 64 lowercase hexadecimal characters")
        }
        Ok(format!(
            "version={}\narchive_url={}\narchive_sha256={}\n",
            self.version, self.archive_url, self.archive_sha256
        )
        .into_bytes())
    }

    fn verify_signature(&self) -> Result<()> {
        let public_key: [u8; 32] = STANDARD
            .decode(PINNED_PUBLIC_KEY)
            .expect("the compiled-in public key is valid base64")
            .try_into()
            .expect("the compiled-in public key is 32 bytes");
        let signature: [u8; 64] = STANDARD
            .decode(&self.signature)
            .context("manifest signature is not base64")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("manifest signature is not an Ed25519 signature"))?;
        VerifyingKey::from_bytes(&public_key)
            .expect("the compiled-in public key is valid")
            .verify(
                &self.canonical_payload()?,
                &Signature::from_bytes(&signature),
            )
            .map_err(|_| anyhow::anyhow!("manifest signature verification failed"))
    }
}

fn validate_value(name: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.contains(['\r', '\n']) {
        bail!("manifest {name} must not be empty or contain line breaks")
    }
    Ok(())
}

fn manifest_url_for_architecture(architecture: &str) -> Result<String> {
    match architecture {
        "aarch64" | "x86_64" => Ok(format!("{MANIFEST_URL_PREFIX}{architecture}.manifest.json")),
        _ => bail!(
            "unsupported macOS target architecture {architecture:?}; no matching MeetliteCapture release is available"
        ),
    }
}

fn local_manifest_url() -> Result<String> {
    manifest_url_for_architecture(std::env::consts::ARCH)
}

fn verify_archive_checksum(archive: &[u8], expected: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(archive));
    if actual != expected {
        bail!("capture-agent archive checksum mismatch (expected {expected}, got {actual})")
    }
    Ok(())
}

pub fn installed_agent_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("could not determine home directory")?;
    Ok(PathBuf::from(home)
        .join("Library/Application Support/Meetlite")
        .join(APP_NAME))
}

#[cfg(not(target_os = "macos"))]
pub fn run() -> Result<()> {
    println!("No companion capture agent is needed on this platform; setup did nothing.");
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn run() -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("meetlite/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not create release download client")?;
    println!("Downloading the latest MeetliteCapture release manifest...");
    let manifest_url = local_manifest_url()?;
    let manifest: Manifest = serde_json::from_slice(&download(&client, &manifest_url)?)
        .context("release manifest is invalid JSON")?;
    manifest.verify_signature()?;
    let archive = download(&client, &manifest.archive_url)?;
    verify_archive_checksum(&archive, &manifest.archive_sha256)?;

    let destination = installed_agent_path()?;
    let parent = destination.parent().expect("installed agent has a parent");
    fs::create_dir_all(parent).with_context(|| format!("could not create {}", parent.display()))?;
    let temporary = tempfile::Builder::new()
        .prefix("MeetliteCapture-install-")
        .tempdir_in(parent)
        .context("could not create secure capture-agent staging directory")?;
    extract_agent_archive(&archive, temporary.path())?;
    let staged = temporary.path().join(APP_NAME);
    let signing_status = verify_codesign(&staged)?;
    install_atomically(&staged, &destination)?;
    drop(temporary);

    println!(
        "Installed Meetlite Capture {} at {}",
        manifest.version,
        destination.display()
    );
    println!("Code signing: {signing_status}.");
    println!("TCC: grant Meetlite Capture in System Settings > Privacy & Security > Audio Capture when macOS prompts.");
    Ok(())
}

#[cfg(target_os = "macos")]
fn download(client: &reqwest::blocking::Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("could not download {url}"))?
        .error_for_status()
        .with_context(|| format!("release download failed for {url}"))?;
    response
        .bytes()
        .map(|bytes| bytes.to_vec())
        .context("could not read release download")
}

fn extract_agent_archive(archive: &[u8], destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(io::Cursor::new(archive))
        .context("capture-agent archive is not a ZIP file")?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .context("could not read ZIP entry")?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("ZIP entry has an unsafe path"))?;
        if enclosed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            bail!("ZIP entry has an unsafe path")
        }
        if enclosed
            .components()
            .next()
            .is_none_or(|component| component.as_os_str() != APP_NAME)
        {
            bail!("capture-agent archive must contain only {APP_NAME}")
        }
        if entry.is_symlink() {
            bail!("capture-agent archive must not contain symbolic links")
        }
        let output = destination.join(enclosed);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
        } else {
            fs::create_dir_all(output.parent().expect("ZIP files have a parent path"))?;
            let mut file = fs::File::create(&output)
                .with_context(|| format!("could not extract {}", output.display()))?;
            io::copy(&mut entry, &mut file)?;
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&output, fs::Permissions::from_mode(mode))?;
            }
        }
    }
    if !destination
        .join(APP_NAME)
        .join("Contents/MacOS/meetlite")
        .is_file()
    {
        bail!("capture-agent archive does not contain {APP_NAME}/Contents/MacOS/meetlite")
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = destination.join(APP_NAME).join("Contents/MacOS/meetlite");
        let mode = fs::metadata(&executable)?.permissions().mode();
        if mode & 0o111 == 0 {
            bail!("capture-agent archive executable is not marked executable")
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_codesign(app: &Path) -> Result<&'static str> {
    let status = std::process::Command::new("codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(app)
        .status()
        .context("could not run codesign")?;
    if !status.success() {
        bail!("macOS rejected the capture-agent code signature")
    }
    let output = std::process::Command::new("codesign")
        .args(["--display", "--verbose=1"])
        .arg(app)
        .output()
        .context("could not inspect the capture-agent code signature")?;
    if !output.status.success() {
        bail!("macOS could not inspect the capture-agent code signature")
    }
    let details = String::from_utf8_lossy(&output.stderr);
    Ok(if details.contains("Signature=adhoc") {
        "ad hoc signature verified; this build is not notarized"
    } else {
        "signature verified; notarization was not checked"
    })
}

#[cfg(target_os = "macos")]
fn install_atomically(staged: &Path, destination: &Path) -> Result<()> {
    let previous = destination.with_extension("app.previous");
    if previous.exists() {
        fs::remove_dir_all(&previous)
            .with_context(|| format!("could not remove {}", previous.display()))?;
    }
    let had_current = destination.exists();
    if had_current {
        fs::rename(destination, &previous)
            .with_context(|| format!("could not preserve {}", destination.display()))?;
    }
    if let Err(error) = fs::rename(staged, destination) {
        if had_current {
            let _ = fs::rename(&previous, destination);
        }
        return Err(error).with_context(|| format!("could not install {}", destination.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn signature_verification_rejects_invalid_signature() {
        let manifest = Manifest {
            version: "v1.2.3".into(),
            archive_url: "https://github.com/podocarp/meetlite/releases/download/v1.2.3/MeetliteCapture.app.zip".into(),
            archive_sha256: "0".repeat(64),
            signature: STANDARD.encode([0_u8; 64]),
        };
        assert!(manifest.verify_signature().is_err());
    }

    #[test]
    fn checksum_verification_rejects_wrong_archive() {
        assert!(verify_archive_checksum(b"archive", &"0".repeat(64)).is_err());
    }

    #[test]
    fn manifest_url_matches_supported_macos_architectures() {
        assert_eq!(
            manifest_url_for_architecture("aarch64").unwrap(),
            "https://github.com/podocarp/meetlite/releases/latest/download/MeetliteCapture-macos-aarch64.manifest.json"
        );
        assert_eq!(
            manifest_url_for_architecture("x86_64").unwrap(),
            "https://github.com/podocarp/meetlite/releases/latest/download/MeetliteCapture-macos-x86_64.manifest.json"
        );
        assert!(manifest_url_for_architecture("arm").is_err());
    }

    #[test]
    fn extraction_rejects_path_traversal() {
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(io::Cursor::new(&mut bytes));
            writer
                .start_file("../escaped", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"unsafe").unwrap();
            writer.finish().unwrap();
        }
        let directory = tempfile::tempdir().unwrap();
        assert!(extract_agent_archive(&bytes, directory.path()).is_err());
        assert!(!directory.path().join("escaped").exists());
    }
}
