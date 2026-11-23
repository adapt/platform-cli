//! Shared temporary-sandbox lifecycle. Backend-specific wire formats stay here.
pub mod local;

use crate::{
    api::Api,
    cli::{ExecArgs, SandboxCreate},
    commands, shell,
};
use anyhow::Result;
#[cfg(unix)]
use std::collections::BTreeMap;
use uuid::Uuid;

pub struct Sandbox {
    pub id: Uuid,
    #[cfg(unix)]
    pub env: BTreeMap<String, String>,
}

pub enum SandboxBackend<'a> {
    Cloud(&'a Api),
    Local(&'a local::LocalBackend),
}

impl SandboxBackend<'_> {
    pub async fn create(&self, args: SandboxCreate) -> Result<Sandbox> {
        match self {
            Self::Cloud(api) => Ok(Sandbox {
                id: commands::resource_id(&commands::create_sandbox(api, args).await?)?,
                #[cfg(unix)]
                env: BTreeMap::new(),
            }),
            Self::Local(local) => local.create(args).await,
        }
    }

    pub async fn wait(&self, id: Uuid) -> Result<()> {
        match self {
            Self::Cloud(api) => {
                commands::wait_for(api, "sandboxes", id, "running", 600).await?;
                Ok(())
            }
            Self::Local(local) => local.wait(id).await,
        }
    }

    pub async fn execute(&self, sandbox: &Sandbox, args: ExecArgs, json: bool) -> Result<u8> {
        match self {
            Self::Cloud(api) => commands::execute(api, args, json).await,
            Self::Local(local) => local.execute(sandbox, args, json).await,
        }
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        match self {
            Self::Cloud(api) => shell::cleanup_cloud(api, id).await,
            Self::Local(local) => local.delete(id).await,
        }
    }

    pub fn cleanup_hint(&self, id: Uuid) -> String {
        match self {
            Self::Cloud(_) => format!("retry with `adapt sandbox delete {id}`"),
            Self::Local(local) => format!(
                "local sandbox {id} remains in {}; remove it through the local Goblin API",
                local.state_dir.display()
            ),
        }
    }
}
