//! Looks for a newer release on GitHub and, when the user asks for it, downloads it and
//! replaces the running program. Nothing is ever installed without a click.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;

const LATEST: &str = "https://api.github.com/repos/voidNK/gridwatch/releases/latest";
/// Larger than any release so far, small enough to stop a runaway download.
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// The platform part of a release archive's name, e.g. `gridwatch-v0.5.0-windows-x86_64.zip`.
const PLATFORM: Option<&str> = if cfg!(all(windows, target_arch = "x86_64")) {
    Some("windows-x86_64")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("linux-x86_64")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("macos-arm64")
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some("macos-x86_64")
} else {
    None
};

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// What changed, as written on the release page.
    pub notes: String,
    /// Name of the archive for this platform; its top folder has the same name.
    archive: String,
    url: String,
    /// `sha256:<hex>` as published by GitHub, if it reports one.
    digest: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

/// `0.10.0` is newer than `0.9.3`: compare number by number, not as text.
pub fn newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| p.trim().parse().unwrap_or(0))
            .collect()
    };
    parts(candidate) > parts(current)
}

fn release_for(api: ApiRelease, platform: &str) -> Option<Release> {
    let asset = api.assets.into_iter().find(|a| {
        a.name.starts_with("gridwatch-")
            && a.name.contains(platform)
            && (a.name.ends_with(".zip") || a.name.ends_with(".tar.gz"))
    })?;
    Some(Release {
        version: api.tag_name.trim_start_matches('v').to_string(),
        // The notes are Markdown; shown as plain text they read better without its marks.
        notes: api
            .body
            .unwrap_or_default()
            .replace("**", "")
            .replace('`', "")
            .trim()
            .to_string(),
        archive: asset.name,
        url: asset.browser_download_url,
        digest: asset.digest,
    })
}

/// The latest published release for this platform, whatever its version.
pub fn latest() -> Result<Option<Release>> {
    let Some(platform) = PLATFORM else { return Ok(None) };
    let text = ureq::get(LATEST)
        .header("User-Agent", concat!("gridwatch/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("cannot reach GitHub")?
        .body_mut()
        .read_to_string()?;
    let api: ApiRelease = serde_json::from_str(&text).context("unexpected answer from GitHub")?;
    Ok(release_for(api, platform))
}

/// A release that is newer than the running program, if there is one.
pub fn check() -> Result<Option<Release>> {
    Ok(latest()?.filter(|r| newer(&r.version, env!("CARGO_PKG_VERSION"))))
}

/// Take the program out of a release archive. Archives hold one folder named like the
/// archive, with `gridwatch` (or `gridwatch.exe`) inside; on macOS it sits in an
/// application bundle, `gridwatch.app/Contents/MacOS/gridwatch`.
fn extract(archive: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut program = Vec::new();
    if let Some(folder) = archive.strip_suffix(".zip") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("the download is not a zip archive")?;
        let mut file = zip
            .by_name(&format!("{folder}/gridwatch.exe"))
            .context("the archive does not contain gridwatch.exe")?;
        file.read_to_end(&mut program)?;
    } else if let Some(folder) = archive.strip_suffix(".tar.gz") {
        let wanted = [
            format!("{folder}/gridwatch"),
            format!("{folder}/gridwatch.app/Contents/MacOS/gridwatch"),
        ];
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
        for entry in tar.entries().context("the download is not a tar archive")? {
            let mut entry = entry?;
            if wanted
                .iter()
                .any(|w| entry.path().is_ok_and(|p| p.to_string_lossy() == *w))
            {
                entry.read_to_end(&mut program)?;
                break;
            }
        }
    }
    if program.is_empty() {
        bail!("the archive {archive} does not contain the program");
    }
    Ok(program)
}

/// Download the release and return the program it contains, after comparing the
/// download with the checksum GitHub publishes for it.
fn download(release: &Release) -> Result<Vec<u8>> {
    let bytes = ureq::get(&release.url)
        .header("User-Agent", concat!("gridwatch/", env!("CARGO_PKG_VERSION")))
        .call()
        .context("download failed")?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .context("download interrupted")?;
    if let Some(expected) = release.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if !actual.eq_ignore_ascii_case(expected) {
            bail!("the download does not match its checksum; nothing was installed");
        }
    }
    extract(&release.archive, &bytes)
}

/// Download the release and put it in place of the running program. The new version
/// runs from the next start on.
pub fn install(release: &Release) -> Result<()> {
    let program = download(release)?;
    let current = std::env::current_exe()?;
    // Next to the program, so that replacing it is a rename on the same disk.
    let staged = current.with_extension("update");
    std::fs::write(&staged, program).with_context(|| format!("cannot write {}", staged.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let replaced = self_replace::self_replace(&staged).context("cannot replace the program (is its folder writable?)");
    let _ = std::fs::remove_file(&staged);
    replaced?;
    // Inside an application bundle the signature covers the program; sign the bundle
    // again so macOS keeps accepting it. Failing here does not undo the update.
    #[cfg(target_os = "macos")]
    if let Some(bundle) = bundle(&current) {
        write_bundle_version(bundle, &release.version);
        sign(bundle);
    }
    Ok(())
}

/// The application bundle the program sits in, if it does.
#[cfg(target_os = "macos")]
fn bundle(program: &std::path::Path) -> Option<&std::path::Path> {
    program.ancestors().find(|p| p.extension().is_some_and(|e| e == "app"))
}

#[cfg(target_os = "macos")]
fn sign(bundle: &std::path::Path) {
    let _ = std::process::Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(bundle)
        .output();
}

/// `Info.plist` with another version number in it.
#[cfg(any(target_os = "macos", test))]
fn plist_with_version(plist: &str, version: &str) -> String {
    let mut text = plist.to_string();
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        let Some(key_at) = text.find(&format!("<key>{key}</key>")) else {
            continue;
        };
        let Some(open) = text[key_at..].find("<string>").map(|i| key_at + i + "<string>".len()) else {
            continue;
        };
        let Some(close) = text[open..].find("</string>").map(|i| open + i) else {
            continue;
        };
        text.replace_range(open..close, version);
    }
    text
}

/// Put the version into the bundle's `Info.plist`; says whether it changed anything.
#[cfg(target_os = "macos")]
fn write_bundle_version(bundle: &std::path::Path, version: &str) -> bool {
    let path = bundle.join("Contents").join("Info.plist");
    let Ok(plist) = std::fs::read_to_string(&path) else {
        return false;
    };
    let wanted = plist_with_version(&plist, version);
    wanted != plist && std::fs::write(&path, wanted).is_ok()
}

/// macOS takes the version it shows under "About gridwatch" and in Finder from the
/// bundle's `Info.plist`, and an update replaces only the program inside the bundle.
/// Versions before 0.10.5 left the file as it was installed, so it is set right at start.
#[cfg(target_os = "macos")]
pub fn sync_bundle_version() {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    if let Some(bundle) = bundle(&current)
        && write_bundle_version(bundle, env!("CARGO_PKG_VERSION"))
    {
        sign(bundle);
    }
}

/// Start the program again; the caller closes this instance.
pub fn restart() -> Result<()> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args(std::env::args_os().skip(1));
    command.spawn().context("cannot start the new version")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_the_version_in_a_plist() {
        let plist = include_str!("../packaging/macos/Info.plist").replace("@VERSION@", "0.9.3");
        let updated = plist_with_version(&plist, "0.10.5");
        assert_eq!(updated.matches("<string>0.10.5</string>").count(), 2);
        assert!(!updated.contains("0.9.3"));
        assert!(updated.contains("<string>11.0</string>"));
        assert_eq!(plist_with_version(&updated, "0.10.5"), updated);
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(newer("0.5.1", "0.5"));
        assert!(!newer("0.5.0", "0.5.0"));
        assert!(!newer("0.4.9", "0.5.0"));
    }

    #[test]
    fn picks_the_archive_for_the_platform() {
        let json = r#"{"tag_name":"v0.6.0","body":"- **news** with `code`\n","assets":[
            {"name":"gridwatch-v0.6.0-linux-x86_64.tar.gz","browser_download_url":"https://example.org/l","digest":"sha256:ab"},
            {"name":"gridwatch-v0.6.0-windows-x86_64.zip","browser_download_url":"https://example.org/w"}]}"#;
        let release = release_for(serde_json::from_str(json).unwrap(), "windows-x86_64").unwrap();
        assert_eq!(
            (release.version.as_str(), release.notes.as_str()),
            ("0.6.0", "- news with code")
        );
        assert_eq!((release.url.as_str(), release.digest), ("https://example.org/w", None));
        assert!(release_for(serde_json::from_str(json).unwrap(), "macos-arm64").is_none());
    }

    #[test]
    fn extracts_the_program_from_both_archive_kinds() {
        use std::io::Write;
        let mut zipped = Vec::new();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zipped));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("gridwatch-v1-windows-x86_64/README.md", options)
            .unwrap();
        zip.write_all(b"readme").unwrap();
        zip.start_file("gridwatch-v1-windows-x86_64/gridwatch.exe", options)
            .unwrap();
        zip.write_all(b"windows program").unwrap();
        zip.finish().unwrap();
        assert_eq!(
            extract("gridwatch-v1-windows-x86_64.zip", &zipped).unwrap(),
            b"windows program"
        );

        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, content) in [
            ("gridwatch-v1-linux-x86_64/README.md", &b"readme"[..]),
            ("gridwatch-v1-linux-x86_64/gridwatch", b"linux program"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, path, content).unwrap();
        }
        let tarred = tar.into_inner().unwrap().finish().unwrap();
        assert_eq!(
            extract("gridwatch-v1-linux-x86_64.tar.gz", &tarred).unwrap(),
            b"linux program"
        );
        assert!(extract("gridwatch-v1-linux-x86_64.tar.gz", &zipped).is_err());

        // macOS releases carry the program inside an application bundle.
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, content) in [
            (
                "gridwatch-v1-macos-arm64/gridwatch.app/Contents/Info.plist",
                &b"plist"[..],
            ),
            (
                "gridwatch-v1-macos-arm64/gridwatch.app/Contents/MacOS/gridwatch",
                b"mac program",
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, path, content).unwrap();
        }
        let bundled = tar.into_inner().unwrap().finish().unwrap();
        assert_eq!(
            extract("gridwatch-v1-macos-arm64.tar.gz", &bundled).unwrap(),
            b"mac program"
        );
    }

    /// Downloads the latest real release and checks it: `cargo test real_release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_release() {
        let release = latest().unwrap().expect("no release for this platform");
        println!("latest release: {} ({})", release.version, release.archive);
        assert!(release.digest.is_some());
        let program = download(&release).unwrap();
        assert!(program.len() > 1_000_000);
        let path = std::env::temp_dir().join(if cfg!(windows) {
            "gridwatch-update-test.exe"
        } else {
            "gridwatch-update-test"
        });
        std::fs::write(&path, program).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let out = std::process::Command::new(&path).arg("--version").output().unwrap();
        let _ = std::fs::remove_file(&path);
        // Release builds for Windows have no console, so only the exit status counts there.
        println!("it answers: {:?} {}", String::from_utf8_lossy(&out.stdout), out.status);
        assert!(out.status.success());
    }
}
