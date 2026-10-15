use crate::{
    common::{display_version_from_release_id, release_id_from_update_url},
    hbbs_http::create_http_client_with_url_strict,
    update_metadata::{UpdateArtifactQuery, UpdateMetadataRequirements, VerifiedUpdateArtifact},
};
use hbb_common::{bail, log, ResultType};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

const UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_METADATA_SIDECAR_MAX_BYTES: u64 = 1024 * 1024;

lazy_static::lazy_static! {
    static ref VERIFIED_UPDATE_ARTIFACT: Mutex<Option<VerifiedUpdateArtifact>> = Mutex::new(None);
}

pub(crate) fn current_update_platform() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::consts::OS
    }
}

pub(crate) fn current_update_arch() -> &'static str {
    #[cfg(all(target_os = "windows", not(feature = "flutter")))]
    {
        "x86"
    }
    #[cfg(not(all(target_os = "windows", not(feature = "flutter"))))]
    {
        std::env::consts::ARCH
    }
}

pub(crate) fn current_update_format(update_msi: bool) -> &'static str {
    #[cfg(any(
        not(target_os = "windows"),
        all(target_os = "windows", not(feature = "flutter"))
    ))]
    let _ = update_msi;
    #[cfg(all(target_os = "windows", feature = "flutter"))]
    {
        if update_msi {
            return "msi";
        }
        "exe"
    }
    #[cfg(all(target_os = "windows", not(feature = "flutter")))]
    {
        "exe"
    }
    #[cfg(target_os = "macos")]
    {
        "dmg"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "unknown"
    }
}

pub fn current_update_artifact_query(update_msi: bool) -> UpdateArtifactQuery<'static> {
    UpdateArtifactQuery {
        platform: current_update_platform(),
        arch: current_update_arch(),
        format: current_update_format(update_msi),
    }
}

pub fn verified_update_artifact_for_release_page_url(
    release_page_url: &str,
    query: UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    let artifact = verified_update_artifact_from_release_page_url(release_page_url, &query)?;
    *VERIFIED_UPDATE_ARTIFACT.lock().unwrap() = Some(artifact.clone());
    Ok(artifact)
}

pub fn verified_update_artifact_for_download_url(
    download_url: &str,
) -> ResultType<VerifiedUpdateArtifact> {
    VERIFIED_UPDATE_ARTIFACT
        .lock()
        .unwrap()
        .as_ref()
        .filter(|artifact| artifact.url == download_url)
        .cloned()
        .ok_or_else(|| hbb_common::anyhow::anyhow!("update artifact was not verified"))
}

pub(super) fn verified_update_artifact_from_release_page_url(
    update_url: &str,
    query: &UpdateArtifactQuery<'_>,
) -> ResultType<VerifiedUpdateArtifact> {
    let release_id = release_id_from_update_url(update_url)?;
    let display_version = display_version_from_release_id(&release_id)?;
    let expected_artifact_url_prefix =
        format!("https://github.com/rustdesk/rustdesk/releases/download/{release_id}/");
    let metadata_url = format!("{expected_artifact_url_prefix}rustdesk-update.json");
    let signature_url = format!("{metadata_url}.sig");
    let metadata_bytes = fetch_update_sidecar_bytes(&metadata_url)?;
    let signature_bytes = fetch_update_sidecar_bytes(&signature_url)?;
    let requirements = UpdateMetadataRequirements {
        expected_version: display_version.as_str(),
        expected_release_id: release_id.as_str(),
        expected_artifact_url_prefix: expected_artifact_url_prefix.as_str(),
        artifact: *query,
    };
    crate::update_metadata::verify_update_metadata(&metadata_bytes, &signature_bytes, requirements)
}

fn fetch_update_sidecar_bytes(url: &str) -> ResultType<Vec<u8>> {
    let client = create_http_client_with_url_strict(url)?;
    let mut response = client
        .get(url)
        .timeout(UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !response.status().is_success() {
        bail!(
            "Failed to download update metadata sidecar: {}",
            response.status()
        );
    }
    read_limited_response_bytes(
        &mut response,
        UPDATE_METADATA_SIDECAR_MAX_BYTES,
        "Update metadata sidecar",
    )
}

fn read_limited_response_bytes<R: Read>(
    reader: &mut R,
    limit: u64,
    what: &str,
) -> ResultType<Vec<u8>> {
    let mut limited_reader = reader.take(limit.saturating_add(1));
    let mut bytes = Vec::new();
    limited_reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("{what} exceeds maximum allowed size of {limit} bytes");
    }
    Ok(bytes)
}

pub fn is_self_hosted_update_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Ok(base) = url::Url::parse(crate::common::software_update_base()) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.scheme() == base.scheme()
        && parsed.host_str() == base.host_str()
        && parsed.port_or_known_default() == base.port_or_known_default()
        && parsed.path().starts_with(base.path().trim_end_matches('/'))
}

pub fn prepare_self_hosted_update_artifact(
    download_url: &str,
) -> ResultType<VerifiedUpdateArtifact> {
    if !is_self_hosted_update_url(download_url) {
        bail!("Update URL is not the configured self-hosted update server");
    }
    let Some(file_path) = get_update_download_file_from_url(download_url) else {
        bail!("Invalid self-hosted update download URL: {}", download_url);
    };
    let size = probe_self_hosted_content_length(download_url)?;
    let version = crate::common::get_software_update_version();
    if version.is_empty() {
        bail!("Self-hosted update version is unknown");
    }
    let artifact = VerifiedUpdateArtifact {
        version,
        url: download_url.to_owned(),
        #[cfg(target_os = "macos")]
        file_name: file_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("update.dmg")
            .to_owned(),
        size,
        // Self-hosted updates have no trusted expected hash; install without SHA256 verify.
        sha256: String::new(),
    };
    *VERIFIED_UPDATE_ARTIFACT.lock().unwrap() = Some(artifact.clone());
    Ok(artifact)
}

fn probe_self_hosted_content_length(download_url: &str) -> ResultType<u64> {
    let mut last_err = None;
    for url in std::iter::once(download_url.to_owned()).chain(
        crate::common::alternate_software_update_download_url(download_url)
            .into_iter()
            .filter(|alt| alt != download_url),
    ) {
        match probe_content_length_once(&url) {
            Ok(size) => return Ok(size),
            Err(e) => {
                log::debug!("Failed to probe content length for {}: {}", url, e);
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| hbb_common::anyhow::anyhow!("content length probe failed")))
}

fn probe_content_length_once(url: &str) -> ResultType<u64> {
    let client = create_http_client_with_url_strict(url)?;
    match client.head(url).timeout(UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Some(size) = content_length_from_headers(resp.headers()) {
                return Ok(size);
            }
        }
        Ok(resp) => {
            log::debug!("HEAD {} returned {}, falling back to GET", url, resp.status());
        }
        Err(e) => {
            log::debug!("HEAD {} failed: {}, falling back to GET", url, e);
        }
    }
    let resp = client
        .get(url)
        .timeout(UPDATE_SIDECAR_HTTP_REQUEST_TIMEOUT)
        .send()?;
    if !resp.status().is_success() {
        bail!("Failed to probe content length: {}", resp.status());
    }
    content_length_from_headers(resp.headers())
        .ok_or_else(|| hbb_common::anyhow::anyhow!("Missing Content-Length for {}", url))
}

fn content_length_from_headers(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|ct_len| ct_len.to_str().ok())
        .and_then(|ct_len| ct_len.parse::<u64>().ok())
}

pub fn get_update_download_file_from_url(url: &str) -> Option<PathBuf> {
    // Reject path traversal before Url::parse normalizes `/../` away.
    if url.contains("/../") || url.contains("/..\\") || url.contains("\\..\\") {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }

    if let Ok(base) = url::Url::parse(crate::common::software_update_base()) {
        if parsed.scheme() == base.scheme()
            && parsed.host_str() == base.host_str()
            && parsed.port_or_known_default() == base.port_or_known_default()
        {
            let base_path = base.path().trim_end_matches('/');
            if !parsed.path().starts_with(base_path) {
                return None;
            }
            if let Some(query) = parsed.query() {
                if query != "redirect=y" {
                    return None;
                }
            }
            let filename = parsed.path_segments()?.next_back()?;
            if !is_plain_update_filename(filename) {
                return None;
            }
            return Some(std::env::temp_dir().join(filename));
        }
    }

    // Check the raw prefix before Url normalizes default ports.
    if !url.starts_with("https://github.com/")
        || parsed.host_str() != Some("github.com")
        || parsed.port().is_some()
        || parsed.query().is_some()
    {
        return None;
    }

    let mut segments = parsed.path_segments()?;
    let owner = segments.next()?;
    let repo = segments.next()?;
    let releases = segments.next()?;
    let download = segments.next()?;
    let tag = segments.next()?;
    let filename = segments.next()?;

    if owner != "rustdesk"
        || repo != "rustdesk"
        || releases != "releases"
        || download != "download"
        || tag.is_empty()
        || segments.next().is_some()
        || !is_plain_update_filename(filename)
    {
        return None;
    }

    Some(std::env::temp_dir().join(filename))
}

fn is_plain_update_filename(filename: &str) -> bool {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains(':')
    {
        return false;
    }

    let mut components = Path::new(filename).components();
    matches!(
        components.next(),
        Some(Component::Normal(name)) if name.to_str() == Some(filename)
    ) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_download_file_accepts_expected_github_asset_urls() {
        let file = get_update_download_file_from_url(
            "https://github.com/rustdesk/rustdesk/releases/download/1.4.0/rustdesk-1.4.0-x86_64.dmg",
        )
        .expect("valid GitHub release asset URL");

        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some("rustdesk-1.4.0-x86_64.dmg")
        );
    }

    #[test]
    fn update_download_file_accepts_self_hosted_urls() {
        let file = get_update_download_file_from_url(
            "https://spk.bobohome.store:8880/download/RustDeskClients/1.4.9/rustdesk-1.4.9-x86_64.exe?redirect=y",
        )
        .expect("valid self-hosted update URL");

        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some("rustdesk-1.4.9-x86_64.exe")
        );
    }

    #[test]
    fn update_download_file_rejects_untrusted_or_malformed_urls() {
        for url in [
            "http://github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe",
            "https://example.com/rustdesk.exe",
            "https://github.com/other/project/releases/download/1/rustdesk.exe",
            "https://github.com/rustdesk/rustdesk/releases/download/1/",
            "https://github.com/rustdesk/rustdesk/releases/download/1/nested/rustdesk.exe",
            "https://github.com/rustdesk/rustdesk/releases/download/1/C:rustdesk.exe",
            "https://user@github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe",
            "https://github.com:443/rustdesk/rustdesk/releases/download/1/rustdesk.exe",
            "https://github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe?download=1",
            "https://github.com/rustdesk/rustdesk/releases/download/1/rustdesk.exe#download",
            "https://spk.bobohome.store:8880/download/RustDeskClients/1.4.9/../evil.exe",
            "https://spk.bobohome.store:8880/other/rustdesk.exe",
            "not a url",
        ] {
            assert!(get_update_download_file_from_url(url).is_none(), "{url}");
        }
    }
}
