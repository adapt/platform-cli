//! Resource operations shared by CLI workflows. Implementations own transport,
//! credentials and wire formats; unsupported capabilities fail without side effects.
pub mod cloud;
pub mod local;

use crate::{cli::*, input::ResourceRef};
use anyhow::{Result, bail};
use serde_json::Value;
use uuid::Uuid;

pub trait Backend {
    fn name(&self) -> &'static str;

    fn unsupported<T>(&self, operation: &str) -> Result<T> {
        bail!(
            "{operation} is not supported by the {} backend",
            self.name()
        )
    }

    async fn sandbox_list(&self, _args: SandboxList) -> Result<Value> {
        self.unsupported("sandbox list")
    }
    async fn sandbox_get(&self, _id: ResourceRef) -> Result<Value> {
        self.unsupported("sandbox get")
    }
    async fn sandbox_create(&self, _args: SandboxCreate) -> Result<Value> {
        self.unsupported("sandbox create")
    }
    async fn sandbox_start(&self, _id: ResourceRef) -> Result<Value> {
        self.unsupported("sandbox start")
    }
    async fn sandbox_stop(&self, _id: ResourceRef) -> Result<Value> {
        self.unsupported("sandbox stop")
    }
    async fn sandbox_delete(&self, _id: ResourceRef) -> Result<Value> {
        self.unsupported("sandbox delete")
    }
    async fn sandbox_wait(&self, _id: ResourceRef, _status: &str, _seconds: u64) -> Result<Value> {
        self.unsupported("sandbox wait")
    }
    async fn sandbox_usage(&self, _id: ResourceRef, _hours: &str) -> Result<Value> {
        self.unsupported("sandbox usage")
    }
    async fn execute(&self, _args: ExecArgs, _json: bool) -> Result<u8> {
        self.unsupported("exec")
    }
    async fn executions(&self, _command: ExecCommand, _json: bool) -> Result<u8> {
        self.unsupported("execs")
    }
    async fn files(&self, _command: FileCommand) -> Result<Value> {
        self.unsupported("files")
    }

    // Resource families can gain a local implementation independently. Commands
    // never fall back to another backend when a capability is unavailable.
    async fn volumes(&self, _command: VolumeCommand) -> Result<Value> {
        self.unsupported("volumes")
    }
    async fn images(&self, _command: ImageCommand, _json: bool) -> Result<u8> {
        self.unsupported("images")
    }
    async fn endpoints(&self, _command: EndpointCommand, _json: bool) -> Result<u8> {
        self.unsupported("endpoints")
    }

    /// Temporary workflows must confirm deletion, even if ordinary deletion is
    /// asynchronous. Backends may also tolerate an already-deleted resource here.
    async fn cleanup(&self, id: Uuid) -> Result<()> {
        self.sandbox_delete(id.into()).await?;
        Ok(())
    }

    fn cleanup_hint(&self, id: Uuid) -> String {
        format!("retry with `adapt --{} sandbox delete {id}`", self.name())
    }
}
