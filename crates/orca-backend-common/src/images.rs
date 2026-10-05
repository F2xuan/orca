use bollard::container::ListContainersOptions;
use bollard::image::{BuildImageOptions, CreateImageOptions, ListImagesOptions, RemoveImageOptions, TagImageOptions};
use std::collections::HashMap;
use tokio_stream::StreamExt;

use orca_core::image::*;

use crate::BollardRuntime;

/// Group containers by the image they reference. Pure, so the mapping (and the
/// name/state handling) is testable without a live daemon.
fn build_usage(containers: &[bollard::models::ContainerSummary]) -> HashMap<String, Vec<ImageUse>> {
    let mut usage: HashMap<String, Vec<ImageUse>> = HashMap::new();
    for c in containers {
        let Some(image_id) = c.image_id.as_deref().filter(|s| !s.is_empty()) else {
            continue;
        };
        let name = c
            .names
            .as_ref()
            .and_then(|n| n.first())
            .map(|n| n.trim_start_matches('/').to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                // No name (rare) — fall back to the short ID so the tooltip
                // still identifies the container.
                c.id.as_deref().unwrap_or("?").chars().take(12).collect()
            });

        usage.entry(image_id.to_string()).or_default().push(ImageUse {
            name,
            running: c.state.as_deref() == Some("running"),
        });
    }

    // Running containers first, then alphabetical — the UI lists them in this
    // order in the tooltip.
    for list in usage.values_mut() {
        list.sort_by(|a, b| b.running.cmp(&a.running).then_with(|| a.name.cmp(&b.name)));
    }

    usage
}

/// Which containers reference each image, keyed by full image ID.
///
/// Docker's `Containers` count on the image summary is deprecated and always
/// returns `-1`, so usage has to be derived by joining against the container
/// list (`all: true` — a *stopped* container still holds a reference and still
/// blocks image deletion).
///
/// `Err` is returned rather than an empty map so callers can distinguish
/// "nothing uses this image" from "we couldn't find out".
async fn usage_by_image(
    docker: &bollard::Docker,
) -> anyhow::Result<HashMap<String, Vec<ImageUse>>> {
    let options = ListContainersOptions::<String> {
        all: true,
        ..Default::default()
    };
    let containers = docker.list_containers(Some(options)).await?;
    Ok(build_usage(&containers))
}

/// Look up usage for a single image without listing every image.
async fn usage_for(docker: &bollard::Docker, id: &str) -> Option<Vec<ImageUse>> {
    usage_by_image(docker).await.ok()?.remove(id)
}

impl ImageManager for BollardRuntime {
    async fn list(&self) -> anyhow::Result<Vec<Image>> {
        let options = ListImagesOptions::<String> {
            all: false,
            ..Default::default()
        };
        let images = self.docker.list_images(Some(options)).await?;
        // `None` (container list unavailable) must not be flattened into
        // "unused" — see `Image::used_by`.
        let usage = usage_by_image(&self.docker).await.ok();

        Ok(images
            .iter()
            .map(|img| Image {
                id: img.id.clone(),
                repo_tags: img.repo_tags.clone(),
                size_bytes: img.size as u64,
                created_at: img.created.to_string(),
                used_by: usage
                    .as_ref()
                    .map(|u| u.get(&img.id).cloned().unwrap_or_default()),
            })
            .collect())
    }

    async fn pull(&self, reference: &str) -> anyhow::Result<tokio::sync::mpsc::Receiver<PullProgress>> {
        let reference = reference.to_string();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let docker = self.docker.clone();

        tokio::spawn(async move {
            let options = CreateImageOptions {
                from_image: reference.as_str(),
                ..Default::default()
            };
            let mut stream = docker.create_image(Some(options), None, None);
            while let Some(result) = stream.next().await {
                match result {
                    Ok(info) => {
                        let progress = PullProgress {
                            layer: info.id.unwrap_or_default(),
                            status: info.status.unwrap_or_default(),
                            current: info
                                .progress_detail
                                .as_ref()
                                .and_then(|d| d.current)
                                .map(|c| c as u64)
                                .unwrap_or(0),
                            total: info
                                .progress_detail
                                .as_ref()
                                .and_then(|d| d.total)
                                .map(|t| t as u64)
                                .unwrap_or(0),
                        };
                        if tx.send(progress).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(PullProgress {
                                layer: String::new(),
                                status: format!("error: {e}"),
                                current: 0,
                                total: 0,
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn remove(&self, id: &str, force: bool) -> anyhow::Result<()> {
        self.docker
            .remove_image(
                id,
                Some(RemoveImageOptions {
                    force,
                    ..Default::default()
                }),
                None,
            )
            .await?;
        Ok(())
    }

    async fn inspect(&self, id: &str) -> anyhow::Result<Image> {
        let info = self.docker.inspect_image(id).await?;
        let full_id = info.id.clone().unwrap_or_default();
        // Filled here too, so the detail panel can't disagree with the list.
        let used_by = usage_for(&self.docker, &full_id).await;
        Ok(Image {
            id: full_id,
            repo_tags: info.repo_tags.unwrap_or_default(),
            size_bytes: info.size.unwrap_or(0) as u64,
            created_at: info.created.unwrap_or_default(),
            used_by,
        })
    }

    async fn prune(&self) -> anyhow::Result<PruneResult> {
        let result = self.docker.prune_images::<String>(None).await?;
        let images_deleted = result
            .images_deleted
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.deleted.clone().or(item.untagged.clone()))
            .collect();
        Ok(PruneResult {
            images_deleted,
            space_reclaimed: result.space_reclaimed.unwrap_or(0) as u64,
        })
    }

    async fn tag(&self, source: &str, repo: &str, tag: &str) -> anyhow::Result<()> {
        self.docker
            .tag_image(source, Some(TagImageOptions { repo, tag }))
            .await?;
        Ok(())
    }

    async fn build(
        &self,
        context_path: &str,
        dockerfile: Option<&str>,
        tag: Option<&str>,
        build_args: Option<std::collections::HashMap<String, String>>,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<BuildProgress>> {
        let tar_bytes = create_build_context(context_path).await?;

        let dockerfile = dockerfile.unwrap_or("Dockerfile").to_string();
        let tag = tag.map(|t| t.to_string()).unwrap_or_default();

        let options = BuildImageOptions::<String> {
            dockerfile,
            rm: true,
            t: tag,
            buildargs: build_args.unwrap_or_default(),
            ..Default::default()
        };

        let (tx, rx) = tokio::sync::mpsc::channel(256);
        let docker = self.docker.clone();

        tokio::spawn(async move {
            let mut stream = docker.build_image(options, None, Some(tar_bytes.into()));
            while let Some(result) = stream.next().await {
                match result {
                    Ok(output) => {
                        let progress = BuildProgress {
                            stream: output.stream.unwrap_or_default(),
                            error: output.error,
                        };
                        if tx.send(progress).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(BuildProgress {
                                stream: String::new(),
                                error: Some(format!("{e}")),
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }
}

impl BollardRuntime {
    /// Pull an image with registry authentication credentials.
    pub async fn pull_with_auth(
        &self,
        reference: &str,
        auth: &orca_core::image::RegistryAuth,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<PullProgress>> {
        let reference = reference.to_string();
        let credentials = bollard::auth::DockerCredentials {
            username: Some(auth.username.clone()),
            password: Some(auth.password.clone()),
            serveraddress: auth.server.clone(),
            ..Default::default()
        };
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let docker = self.docker.clone();

        tokio::spawn(async move {
            let options = CreateImageOptions {
                from_image: reference.as_str(),
                ..Default::default()
            };
            let mut stream = docker.create_image(Some(options), None, Some(credentials));
            while let Some(result) = stream.next().await {
                match result {
                    Ok(info) => {
                        let progress = PullProgress {
                            layer: info.id.unwrap_or_default(),
                            status: info.status.unwrap_or_default(),
                            current: info
                                .progress_detail
                                .as_ref()
                                .and_then(|d| d.current)
                                .map(|c| c as u64)
                                .unwrap_or(0),
                            total: info
                                .progress_detail
                                .as_ref()
                                .and_then(|d| d.total)
                                .map(|t| t as u64)
                                .unwrap_or(0),
                        };
                        if tx.send(progress).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx
                            .send(PullProgress {
                                layer: String::new(),
                                status: format!("error: {e}"),
                                current: 0,
                                total: 0,
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }
}

/// Create a tar archive of the build context directory.
async fn create_build_context(path: &str) -> anyhow::Result<Vec<u8>> {
    use std::path::Path;

    let context_path = Path::new(path);
    if !context_path.is_dir() {
        anyhow::bail!("Build context '{path}' is not a directory");
    }

    // Respect .dockerignore if present (async, cheap).
    let ignore_patterns = load_dockerignore(context_path).await;

    // The actual tar build is blocking (synchronous stdlib fs + tar
    // writer). A large build context can take seconds to walk, which
    // would stall a tokio worker. Hand it off to spawn_blocking so the
    // async runtime stays responsive.
    let context_path = context_path.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let buf = Vec::new();
        let mut archive = tar::Builder::new(buf);
        add_dir_to_tar(&mut archive, &context_path, &context_path, &ignore_patterns)?;
        let bytes = archive.into_inner()?;
        Ok(bytes)
    })
    .await
    .map_err(|e| anyhow::anyhow!("build context task panicked: {e}"))??;
    Ok(bytes)
}

fn add_dir_to_tar(
    archive: &mut tar::Builder<Vec<u8>>,
    base: &std::path::Path,
    dir: &std::path::Path,
    ignore: &[String],
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().to_string();

        // Skip .git and .dockerignore patterns
        if relative == ".git" || relative.starts_with(".git/") {
            continue;
        }
        if should_ignore(&relative, ignore) {
            continue;
        }

        // Use `entry.file_type()` (does NOT follow symlinks) rather than
        // `path.is_dir()`/`path.is_file()`, which do. Otherwise a symlink
        // pointing to e.g. `/etc/shadow` or outside the build context
        // would be silently resolved and shipped to the docker daemon.
        // Skip symlinks entirely — safer than trying to verify their
        // target stays inside the context.
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            tracing::debug!("build context: skipping symlink {relative}");
            continue;
        }

        if file_type.is_dir() {
            add_dir_to_tar(archive, base, &path, ignore)?;
        } else if file_type.is_file() {
            archive.append_path_with_name(&path, &relative)?;
        }
    }
    Ok(())
}

async fn load_dockerignore(context: &std::path::Path) -> Vec<String> {
    let ignore_path = context.join(".dockerignore");
    match tokio::fs::read_to_string(&ignore_path).await {
        Ok(contents) => contents
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        Err(_) => vec![],
    }
}

pub(crate) fn should_ignore(path: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        // Simple glob matching — handles "node_modules", "*.tmp", "target/"
        if pattern.ends_with('/') {
            let dir_pattern = pattern.trim_end_matches('/');
            if path == dir_pattern || path.starts_with(&format!("{dir_pattern}/")) {
                return true;
            }
        } else if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 {
                let (prefix, suffix) = (parts[0], parts[1]);
                let filename = path.rsplit('/').next().unwrap_or(path);
                if filename.starts_with(prefix) && filename.ends_with(suffix) {
                    return true;
                }
            }
        } else if path == pattern || path.starts_with(&format!("{pattern}/")) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn should_ignore_exact_match() {
        assert!(should_ignore("node_modules", &patterns(&["node_modules"])));
    }

    #[test]
    fn should_ignore_directory_pattern() {
        assert!(should_ignore("target/debug/foo", &patterns(&["target/"])));
    }

    #[test]
    fn should_ignore_wildcard() {
        assert!(should_ignore("file.tmp", &patterns(&["*.tmp"])));
    }

    #[test]
    fn should_ignore_no_match() {
        assert!(!should_ignore("src/main.rs", &patterns(&["node_modules"])));
    }

    #[test]
    fn should_ignore_nested_path() {
        assert!(should_ignore("node_modules/foo/bar", &patterns(&["node_modules"])));
    }

    #[test]
    fn should_ignore_empty_patterns() {
        assert!(!should_ignore("anything.rs", &patterns(&[])));
    }

    // ── image usage join ──
    //
    // Docker's `Containers` count on the image summary is deprecated (always
    // `-1`), so usage is derived by grouping containers by their `image_id`.

    fn container(
        id: &str,
        name: Option<&str>,
        image_id: Option<&str>,
        state: &str,
    ) -> bollard::models::ContainerSummary {
        bollard::models::ContainerSummary {
            id: Some(id.to_string()),
            names: name.map(|n| vec![n.to_string()]),
            image_id: image_id.map(|i| i.to_string()),
            state: Some(state.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn usage_groups_by_image_strips_slash_and_sorts_running_first() {
        let cs = vec![
            container("c1", Some("/web"), Some("sha256:aaa"), "running"),
            container("c2", Some("/db"), Some("sha256:aaa"), "exited"),
            container("c3", Some("/other"), Some("sha256:bbb"), "running"),
        ];
        let u = build_usage(&cs);

        assert_eq!(u.len(), 2);
        let a = &u["sha256:aaa"];
        assert_eq!(a.len(), 2);
        // Docker reports names as `/web`; the leading slash must be stripped.
        assert_eq!(a[0].name, "web");
        assert!(a[0].running, "running container must sort first");
        assert_eq!(a[1].name, "db");
        assert!(!a[1].running);
    }

    #[test]
    fn usage_includes_stopped_containers() {
        // A stopped container still holds the image reference and still blocks
        // `docker rmi`, so it must not be filtered out.
        let cs = vec![container("c1", Some("/gone"), Some("sha256:aaa"), "exited")];
        let u = build_usage(&cs);
        assert_eq!(u["sha256:aaa"].len(), 1);
        assert!(!u["sha256:aaa"][0].running);
    }

    #[test]
    fn usage_ignores_containers_without_usable_image_id() {
        let cs = vec![
            container("c1", Some("/x"), None, "running"),
            container("c2", Some("/y"), Some(""), "running"),
        ];
        assert!(build_usage(&cs).is_empty());
    }

    #[test]
    fn usage_falls_back_to_short_id_when_container_is_unnamed() {
        let cs = vec![container(
            "abcdef0123456789",
            None,
            Some("sha256:aaa"),
            "running",
        )];
        assert_eq!(build_usage(&cs)["sha256:aaa"][0].name, "abcdef012345");
    }

    #[test]
    fn usage_groups_multiple_containers_on_one_image() {
        let cs = vec![
            container("c1", Some("/a"), Some("sha256:same"), "running"),
            container("c2", Some("/b"), Some("sha256:same"), "running"),
        ];
        assert_eq!(build_usage(&cs)["sha256:same"].len(), 2);
    }
}
