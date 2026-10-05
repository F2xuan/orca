use std::collections::HashMap;

use bollard::container::ListContainersOptions;
use bollard::models::MountPointTypeEnum;
use bollard::volume::CreateVolumeOptions;

use orca_core::volume::*;

use crate::BollardRuntime;

/// Which containers mount each named volume, keyed by volume name.
///
/// `all: true` matters: a **stopped** container still holds the volume and
/// still makes `docker volume rm` fail, so it has to be counted.
async fn usage_by_volume(
    docker: &bollard::Docker,
) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let options = ListContainersOptions::<String> {
        all: true,
        ..Default::default()
    };
    let containers = docker.list_containers(Some(options)).await?;

    let mut usage: HashMap<String, Vec<String>> = HashMap::new();
    for c in &containers {
        let name = c
            .names
            .as_ref()
            .and_then(|n| n.first())
            .map(|n| n.trim_start_matches('/').to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| c.id.as_deref().unwrap_or("?").chars().take(12).collect());

        for m in c.mounts.iter().flatten() {
            // Only real named volumes — bind mounts have no volume name.
            if m.typ != Some(MountPointTypeEnum::VOLUME) {
                continue;
            }
            if let Some(vname) = m.name.as_deref().filter(|n| !n.is_empty()) {
                usage.entry(vname.to_string()).or_default().push(name.clone());
            }
        }
    }

    for names in usage.values_mut() {
        names.sort();
        names.dedup();
    }
    Ok(usage)
}

impl VolumeManager for BollardRuntime {
    async fn list(&self) -> anyhow::Result<Vec<Volume>> {
        let result = self.docker.list_volumes::<String>(None).await?;
        let volumes = result.volumes.unwrap_or_default();
        // `None` (container list unavailable) must not be flattened into
        // "unused" — see `Volume::used_by`.
        let usage = usage_by_volume(&self.docker).await.ok();

        Ok(volumes
            .iter()
            .map(|v| Volume {
                name: v.name.clone(),
                driver: v.driver.clone(),
                mountpoint: v.mountpoint.clone(),
                labels: v.labels.clone(),
                created_at: v.created_at.clone().unwrap_or_default(),
                used_by: usage
                    .as_ref()
                    .map(|u| u.get(&v.name).cloned().unwrap_or_default()),
            })
            .collect())
    }

    async fn create(&self, name: &str, labels: HashMap<String, String>) -> anyhow::Result<Volume> {
        let str_labels: HashMap<&str, &str> = labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let options = CreateVolumeOptions {
            name,
            labels: str_labels,
            ..Default::default()
        };
        let v = self.docker.create_volume(options).await?;
        Ok(Volume {
            name: v.name,
            driver: v.driver,
            mountpoint: v.mountpoint,
            labels: v.labels,
            created_at: v.created_at.unwrap_or_default(),
            // A freshly created volume cannot be in use yet.
            used_by: Some(Vec::new()),
        })
    }

    async fn remove(&self, name: &str, force: bool) -> anyhow::Result<()> {
        self.docker
            .remove_volume(name, Some(bollard::volume::RemoveVolumeOptions { force }))
            .await?;
        Ok(())
    }

    async fn inspect(&self, name: &str) -> anyhow::Result<Volume> {
        let v = self.docker.inspect_volume(name).await?;
        Ok(Volume {
            name: v.name,
            driver: v.driver,
            mountpoint: v.mountpoint,
            labels: v.labels,
            created_at: v.created_at.unwrap_or_default(),
            used_by: usage_by_volume(&self.docker)
                .await
                .ok()
                .map(|u| u.get(name).cloned().unwrap_or_default()),
        })
    }

    async fn prune(&self) -> anyhow::Result<u64> {
        let result = self.docker.prune_volumes::<String>(None).await?;
        Ok(result.space_reclaimed.unwrap_or(0) as u64)
    }
}
