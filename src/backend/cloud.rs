use super::Backend;
use crate::{
    api::{Api, ApiError},
    cli::*,
    commands,
    input::ResourceRef,
    output, stream,
};
use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use uuid::Uuid;

pub struct CloudBackend<'a>(pub &'a Api);

impl Backend for CloudBackend<'_> {
    fn name(&self) -> &'static str {
        "cloud"
    }

    async fn sandbox_list(&self, args: SandboxList) -> Result<Value> {
        let filters = args
            .query
            .iter()
            .map(|text| ("query".into(), text.clone()))
            .collect::<Vec<_>>();
        self.0
            .list("/v1/sandboxes", "sandboxes", &args.page, true, &filters)
            .await
    }
    async fn sandbox_get(&self, id: ResourceRef) -> Result<Value> {
        self.0.get(&format!("/v1/sandboxes/{id}")).await
    }
    async fn sandbox_create(&self, args: SandboxCreate) -> Result<Value> {
        commands::create_sandbox(self.0, args).await
    }
    async fn sandbox_start(&self, id: ResourceRef) -> Result<Value> {
        self.0
            .post(&format!("/v1/sandboxes/{id}/start"), &json!({}))
            .await
    }
    async fn sandbox_stop(&self, id: ResourceRef) -> Result<Value> {
        self.0
            .post(&format!("/v1/sandboxes/{id}/stop"), &json!({}))
            .await
    }
    async fn sandbox_delete(&self, id: ResourceRef) -> Result<Value> {
        self.0
            .json(
                Method::DELETE,
                &format!("/v1/sandboxes/{id}"),
                None,
                &[],
                None,
            )
            .await
    }
    async fn sandbox_wait(&self, id: ResourceRef, status: &str, seconds: u64) -> Result<Value> {
        commands::wait_for(self.0, "sandboxes", id, status, seconds).await
    }
    async fn sandbox_usage(&self, id: ResourceRef, hours: &str) -> Result<Value> {
        self.0
            .json(
                Method::GET,
                &format!("/v1/sandboxes/{id}/usage"),
                None,
                &[("hours".into(), hours.into())],
                None,
            )
            .await
    }
    async fn execute(&self, args: ExecArgs, json: bool) -> Result<u8> {
        commands::execute(self.0, args, json).await
    }
    async fn executions(&self, command: ExecCommand, json: bool) -> Result<u8> {
        let result = match command {
            ExecCommand::Get { id } => self.0.get(&format!("/v1/execs/{id}")).await?,
            ExecCommand::Cancel { id } => {
                self.0
                    .post(&format!("/v1/execs/{id}/cancel"), &json!({}))
                    .await?
            }
            ExecCommand::Logs { id, cursor } => {
                return stream::logs(self.0, id, cursor, json).await;
            }
            ExecCommand::Attach {
                id,
                after,
                stdin,
                tty,
            } => return stream::attach(self.0, id, after, stdin || tty, tty, json).await,
        };
        output::print(&result, json)?;
        Ok(0)
    }
    async fn files(&self, command: FileCommand) -> Result<Value> {
        commands::files(self.0, command).await
    }
    async fn volumes(&self, command: VolumeCommand) -> Result<Value> {
        commands::volumes(self.0, command).await
    }
    async fn images(&self, command: ImageCommand, json: bool) -> Result<u8> {
        if let ImageCommand::List(args) = command {
            return commands::list_images(self.0, args, json).await;
        }
        output::print(&commands::images(self.0, command).await?, json)?;
        Ok(0)
    }
    async fn endpoints(&self, command: EndpointCommand, json: bool) -> Result<u8> {
        if let EndpointCommand::Request(args) = command {
            return commands::request_endpoint(self.0, args).await;
        }
        output::print(&commands::endpoints(self.0, command).await?, json)?;
        Ok(0)
    }
    async fn cleanup(&self, id: Uuid) -> Result<()> {
        cleanup(self.0, id).await
    }
}

async fn cleanup(api: &Api, id: Uuid) -> Result<()> {
    eprintln!("Deleting temporary sandbox {id}…");
    let result: Result<()> = async {
        let response = api
            .json(
                Method::DELETE,
                &format!("/v1/sandboxes/{id}"),
                None,
                &[],
                None,
            )
            .await?;
        if response["status"] != 204 {
            commands::wait_for(api, "sandboxes", id, "deleted", 600).await?;
        }
        Ok(())
    }
    .await;
    // Expiry or another client may already have removed the sandbox.
    match result {
        Err(error)
            if error
                .downcast_ref::<ApiError>()
                .is_some_and(|e| e.status == StatusCode::NOT_FOUND) =>
        {
            Ok(())
        }
        result => result,
    }
}
