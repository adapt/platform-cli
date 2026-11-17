//! Identity commands: organization-owned permission sets that members assume
//! through expiring, scoped sessions (`/v1/identities`).
//!
//! Platform accepts only full UUIDs in identity routes, so names are resolved
//! here. Updates replace an identity's whole configuration and reset omitted
//! fields to defaults, so every change merges onto the current configuration.

use crate::{
    api::{Api, ApiError},
    cli::{IdentityCommand, IdentityCreate, IdentitySessionCommand, Page},
    commands::resource_id,
    input, output,
};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::io::{self, Write};
use uuid::Uuid;

const ROOT: &str = "/v1/identities";
/// The configuration fields an update carries forward from the current identity.
/// `grants` is left out: omitting it keeps member access as it is.
const CONFIG_FIELDS: [&str; 7] = [
    "name",
    "description",
    "enabled",
    "permissions",
    "connections",
    "environmentVariableSetIds",
    "maxSessionSeconds",
];
const ENDS_SESSIONS: &str =
    "Platform ends an identity's active sessions when its settings or member access change.";
const ACTIONS: [&str; 3] = ["read", "write", "use"];
/// Permission resources Platform requires in every identity request.
const REQUIRED_RESOURCES: [&str; 4] = ["volumes", "sandboxes", "execs", "ingresses"];

/// CLI name, API key, and the actions Platform checks, matching the dashboard's grid.
const RESOURCES: [(&str, &str, &[&str]); 9] = [
    ("sandboxes", "sandboxes", &["read", "write"]),
    ("commands", "execs", &["read", "write"]),
    ("volumes", "volumes", &["read", "write"]),
    ("images", "images", &["read", "write", "use"]),
    ("endpoints", "ingresses", &["read", "write", "use"]),
    (
        "registries",
        "registryCredentials",
        &["read", "write", "use"],
    ),
    ("models", "modelGateway", &["read", "write", "use"]),
    ("mcp-tools", "gateway", &["read", "use"]),
    ("secrets", "environmentVariables", &["read", "use"]),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Permission {
    /// The API's resource key, e.g. `execs`.
    pub resource: &'static str,
    pub action: &'static str,
}

/// Parses RESOURCE:ACTION, accepting dashboard names (`commands`) and API keys (`execs`).
pub fn permission(value: &str) -> Result<Permission, String> {
    let (resource, action) = value
        .trim()
        .split_once(':')
        .ok_or("Use RESOURCE:ACTION, e.g. sandboxes:write")?;
    let resource = resource.trim().to_ascii_lowercase();
    let action = action.trim().to_ascii_lowercase();
    let (name, key, actions) = RESOURCES
        .iter()
        .find(|(name, key, _)| resource == *name || resource == key.to_ascii_lowercase())
        .ok_or_else(|| {
            format!(
                "Unknown resource \"{resource}\"; use one of {}",
                RESOURCES.map(|(name, ..)| name).join(", ")
            )
        })?;
    let action = actions
        .iter()
        .find(|candidate| **candidate == action)
        .ok_or_else(|| format!("{name} accepts {}", actions.join(", ")))?;
    Ok(Permission {
        resource: key,
        action,
    })
}

pub async fn run(api: &Api, command: IdentityCommand, json: bool) -> Result<u8> {
    if let Some(result) = dispatch(api, command, json).await.map_err(explain)? {
        output::print(&result, json)?;
    }
    Ok(0)
}

/// Returns a value to print, or None when the command printed its own output.
async fn dispatch(api: &Api, command: IdentityCommand, json: bool) -> Result<Option<Value>> {
    let result = match command {
        IdentityCommand::List(page) => {
            let result = api.list(ROOT, "identities", &page, false, &[]).await?;
            print_rows(
                &result,
                "identities",
                &["id", "name", "enabled", "access"],
                identity_row,
                json,
            )?;
            return Ok(None);
        }
        IdentityCommand::Get { identity } => api.get(&path(resolve(api, &identity).await?)).await?,
        IdentityCommand::Create(args) => api.post(ROOT, &creation(args)?).await?,
        IdentityCommand::Update { identity, data } => {
            let Value::Object(changes) = input::object(Some(&data.data))? else {
                unreachable!("input::object returns an object")
            };
            save(api, resolve(api, &identity).await?, changes).await?
        }
        IdentityCommand::Enable { identity } => set_enabled(api, &identity, true).await?,
        IdentityCommand::Disable { identity } => set_enabled(api, &identity, false).await?,
        IdentityCommand::Delete { identity } => {
            let id = resolve(api, &identity).await?;
            api.json(Method::DELETE, &path(id), None, &[], None).await?;
            json!({"deleted": id.to_string()})
        }
        IdentityCommand::Permissions {
            identity,
            allow,
            deny,
        } => {
            let id = resolve(api, &identity).await?;
            let mut result = api.get(&path(id)).await?;
            if !allow.is_empty() || !deny.is_empty() {
                let mut permissions = result["permissions"]
                    .as_object()
                    .cloned()
                    .context("Identity response is missing its permissions")?;
                apply(&mut permissions, &allow, &deny)?;
                let changes =
                    Map::from_iter([("permissions".to_owned(), Value::Object(permissions))]);
                result = save_from(api, id, &result, changes).await?;
            }
            print_permissions(&result, json)?;
            return Ok(None);
        }
        IdentityCommand::Access {
            identity,
            member,
            assume,
            manage,
        } => {
            let id = resolve(api, &identity).await?;
            let member = member_id(api, &member).await?;
            let summary = json!({"identityId": id.to_string(), "memberId": member.to_string(), "canAssume": assume, "canManage": manage});
            // Platform ends the identity's sessions on every access change, so skip
            // one that changes nothing. Only managers see grants; others get the
            // server's answer.
            let current = api.get(&path(id)).await?;
            if current["canManage"] == true && held_access(&current, member) == (assume, manage) {
                eprintln!(
                    "This member already has that access. Nothing was saved, and the identity's sessions continue."
                );
                return Ok(Some(summary));
            }
            api.json(
                Method::PUT,
                &format!("{}/grants", path(id)),
                Some(&json!({"memberId": member.to_string(), "canAssume": assume, "canManage": manage})),
                &[],
                None,
            )
            .await?;
            changed(summary)
        }
        IdentityCommand::Account {
            identity,
            connection,
            account,
            clear: _,
        } => {
            let id = resolve(api, &identity).await?;
            api.json(
                Method::PUT,
                &format!("{}/accounts/{connection}", path(id)),
                Some(&json!({ "accountId": account.map(|id| id.to_string()) })),
                &[],
                None,
            )
            .await?;
            eprintln!(
                "Platform ends your active sessions of this identity when your account choice changes."
            );
            json!({"identityId": id.to_string(), "connectionId": connection.to_string(), "accountId": account.map(|id| id.to_string())})
        }
        IdentityCommand::Sessions(command) => return sessions(api, command, json).await,
    };
    Ok(Some(result))
}

async fn sessions(api: &Api, command: IdentitySessionCommand, json: bool) -> Result<Option<Value>> {
    let result = match command {
        IdentitySessionCommand::List { identity, page } => {
            let id = resolve(api, &identity).await?;
            let result = api
                .list(
                    &format!("{}/sessions", path(id)),
                    "sessions",
                    &page,
                    false,
                    &[],
                )
                .await?;
            print_rows(
                &result,
                "sessions",
                &["id", "label", "user", "status", "expires"],
                session_row,
                json,
            )?;
            return Ok(None);
        }
        IdentitySessionCommand::Start {
            identity,
            label,
            duration,
        } => {
            let id = resolve(api, &identity).await?;
            let mut body = json!({ "label": label });
            input::put(&mut body, "durationSeconds", duration)?;
            let result = api.post(&format!("{}/sessions", path(id)), &body).await?;
            eprintln!(
                "Started identity session {} (expires {}). The token below is shown once and is not saved; pass it to one process as ADAPT_TOKEN.",
                output::safe(result["session"]["id"].as_str().unwrap_or("?")),
                output::safe(result["session"]["expiresAt"].as_str().unwrap_or("?")),
            );
            result
        }
        IdentitySessionCommand::Revoke { identity, session } => {
            let id = resolve(api, &identity).await?;
            let sessions = format!("{}/sessions", path(id));
            api.json(
                Method::DELETE,
                &format!("{sessions}/{session}"),
                None,
                &[],
                None,
            )
            .await?;
            // Platform answers 204 even when nothing matched, so confirm the result.
            let all = api
                .list(&sessions, "sessions", &every_page(), false, &[])
                .await?;
            let found = all["sessions"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == session.to_string()))
                .cloned()
                .with_context(|| {
                    format!("This identity has no session {session} that you can revoke")
                })?;
            if found["revokedAt"].is_null() && found["active"] == true {
                bail!("Platform did not revoke session {session}");
            }
            found
        }
    };
    Ok(Some(result))
}

/// Builds a create request from `--data`, then flags. Permissions always include
/// the resources Platform requires, so flags alone make a valid request.
fn creation(args: IdentityCreate) -> Result<Value> {
    let mut body = input::object(args.data.as_deref())?;
    input::put(&mut body, "name", args.name)?;
    input::put(&mut body, "description", args.description)?;
    input::put(&mut body, "maxSessionSeconds", args.max_session)?;
    let mut permissions = match body["permissions"].take() {
        Value::Object(permissions) => permissions,
        Value::Null => Map::new(),
        _ => bail!("permissions must be a JSON object"),
    };
    for resource in REQUIRED_RESOURCES {
        permissions.entry(resource).or_insert_with(|| json!([]));
    }
    apply(&mut permissions, &args.allow, &[])?;
    body["permissions"] = Value::Object(permissions);
    Ok(body)
}

/// The member's current (assume, manage) access, or none without a grant.
fn held_access(identity: &Value, member: Uuid) -> (bool, bool) {
    identity["grants"]
        .as_array()
        .and_then(|grants| {
            grants
                .iter()
                .find(|grant| grant["memberId"] == member.to_string())
        })
        .map_or((false, false), |grant| {
            (grant["canAssume"] == true, grant["canManage"] == true)
        })
}

fn path(id: Uuid) -> String {
    format!("{ROOT}/{id}")
}

fn every_page() -> Page {
    Page {
        all: true,
        ..Page::default()
    }
}

/// A full UUID is always an ID. Anything else is an exact name, ignoring case,
/// among the identities the caller can assume or manage.
async fn resolve(api: &Api, identity: &str) -> Result<Uuid> {
    if let Ok(id) = identity.parse() {
        return Ok(id);
    }
    let all = api
        .list(ROOT, "identities", &every_page(), false, &[])
        .await?;
    let wanted = identity.to_lowercase();
    let matches: Vec<&Value> = all["identities"]
        .as_array()
        .context("Invalid identity list")?
        .iter()
        .filter(|row| {
            row["name"]
                .as_str()
                .is_some_and(|name| name.to_lowercase() == wanted)
        })
        .collect();
    match matches.as_slice() {
        [one] => resource_id(one),
        [] => bail!(
            "No identity named \"{}\" that you can assume or manage. Run `adapt identities list` to see them",
            output::safe(identity)
        ),
        many => bail!(
            "\"{}\" names {} identities ({}); use an ID instead",
            output::safe(identity),
            many.len(),
            many.iter()
                .filter_map(|row| row["id"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

async fn set_enabled(api: &Api, identity: &str, enabled: bool) -> Result<Value> {
    let changes = Map::from_iter([("enabled".to_owned(), json!(enabled))]);
    save(api, resolve(api, identity).await?, changes).await
}

async fn save(api: &Api, id: Uuid, changes: Map<String, Value>) -> Result<Value> {
    let current = api.get(&path(id)).await?;
    save_from(api, id, &current, changes).await
}

/// Merges top-level `changes` onto `current` and saves with its version, so the
/// server's version check refuses an edit made from an older configuration.
/// Platform ends the identity's sessions on every save, so an edit that changes
/// nothing is not sent.
async fn save_from(
    api: &Api,
    id: Uuid,
    current: &Value,
    changes: Map<String, Value>,
) -> Result<Value> {
    let body = merged(current, changes)?;
    if unchanged(current, &body) {
        eprintln!(
            "The identity already has these settings. Nothing was saved, and its sessions continue."
        );
        return Ok(current.clone());
    }
    let saved = api
        .json(Method::PUT, &path(id), Some(&body), &[], None)
        .await?;
    eprintln!("{ENDS_SESSIONS}");
    Ok(saved)
}

/// True when every field the save would send already has that value. Supplied
/// `grants` never match, because responses add each member's name and email.
fn unchanged(current: &Value, body: &Value) -> bool {
    body.as_object().is_some_and(|fields| {
        fields
            .iter()
            .all(|(field, value)| field == "version" || current.get(field) == Some(value))
    })
}

fn merged(current: &Value, mut changes: Map<String, Value>) -> Result<Value> {
    let version = current["version"]
        .as_u64()
        .context("Identity response is missing its version")?;
    if let Some(expected) = changes.remove("version")
        && expected.as_u64() != Some(version)
    {
        bail!(
            "This identity is at version {version}, not {expected}. Run `adapt identities get` and apply your change to the current configuration"
        );
    }
    let mut body: Map<String, Value> = CONFIG_FIELDS
        .iter()
        .filter_map(|field| {
            current
                .get(*field)
                .map(|value| ((*field).to_owned(), value.clone()))
        })
        .collect();
    body.extend(changes);
    body.insert("version".into(), json!(version));
    Ok(Value::Object(body))
}

/// Applies allowed actions, then denied ones. Allowing write or use also allows
/// read, as in the dashboard; read can still be denied on its own afterwards.
fn apply(
    permissions: &mut Map<String, Value>,
    allow: &[Permission],
    deny: &[Permission],
) -> Result<()> {
    if let Some(both) = allow.iter().find(|p| deny.contains(p)) {
        bail!(
            "{}:{} is both allowed and denied",
            both.resource,
            both.action
        );
    }
    // Leaves a resource untouched when it already matches, so a no-op request
    // produces an identical configuration and nothing is saved.
    let mut edit = |resource: &str, action: &str, keep: bool| -> Result<()> {
        let held = permissions
            .get(resource)
            .and_then(Value::as_array)
            .is_some_and(|actions| actions.iter().any(|existing| existing == action));
        if held == keep {
            return Ok(());
        }
        let actions = permissions
            .entry(resource)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("Identity permissions for {resource} are not a list"))?;
        if keep {
            actions.push(json!(action));
            actions.sort_by_key(|existing| ACTIONS.iter().position(|a| existing == a));
        } else {
            actions.retain(|existing| existing != action);
        }
        Ok(())
    };
    for permission in allow {
        if permission.action != "read" {
            edit(permission.resource, "read", true)?;
        }
        edit(permission.resource, permission.action, true)?;
    }
    for permission in deny {
        edit(permission.resource, permission.action, false)?;
    }
    Ok(())
}

/// Resolves an email to the member ID that grants use; a UUID is taken as a member ID.
async fn member_id(api: &Api, member: &str) -> Result<Uuid> {
    if let Ok(id) = member.parse() {
        return Ok(id);
    }
    if !member.contains('@') {
        bail!("Use a member ID from `adapt org members list` or the member's email");
    }
    let organization = api
        .organization
        .as_deref()
        .context("Select an organization with `adapt org use ID` or --org ID")?;
    let wanted = member.trim().to_lowercase();
    let mut offset = 0;
    loop {
        let page = api
            .json(
                Method::GET,
                "/v1/auth/organization/list-members",
                None,
                &[
                    ("organizationId".into(), organization.into()),
                    ("limit".into(), "50".into()),
                    ("offset".into(), offset.to_string()),
                ],
                None,
            )
            .await?;
        let members = page["members"].as_array().context("Invalid member list")?;
        if let Some(found) = members.iter().find(|row| {
            row["user"]["email"]
                .as_str()
                .is_some_and(|email| email.to_lowercase() == wanted)
        }) {
            return resource_id(found);
        }
        offset += members.len();
        if members.is_empty() || offset as u64 >= page["total"].as_u64().unwrap_or(0) {
            bail!(
                "No member of this organization has the email {}",
                output::safe(member)
            );
        }
    }
}

fn changed(result: Value) -> Value {
    eprintln!("{ENDS_SESSIONS}");
    result
}

/// Adds identity-specific advice to Platform errors.
fn explain(error: anyhow::Error) -> anyhow::Error {
    let Some(failure) = error.downcast_ref::<ApiError>() else {
        return error;
    };
    let hint = match failure.status {
        StatusCode::FORBIDDEN if failure.message.starts_with("Use your signed-in account") => {
            "identity commands use your browser sign-in (`adapt auth login`), not an API key or identity token; unset ADAPT_TOKEN if it holds one"
        }
        StatusCode::BAD_REQUEST if failure.message.contains("current member") => {
            "pass a member ID from `adapt org members list` (not a user ID) or the member's email"
        }
        _ => return error,
    };
    anyhow!("{failure}; {hint}")
}

fn print_rows(
    result: &Value,
    field: &str,
    columns: &[&str],
    row: fn(&Value) -> Value,
    json: bool,
) -> Result<()> {
    if !output::tables(json) {
        return output::print(result, json);
    }
    let rows: Vec<Value> = result[field]
        .as_array()
        .map(|items| items.iter().map(row).collect())
        .unwrap_or_default();
    let mut out = io::stdout().lock();
    if rows.is_empty() {
        writeln!(out, "No results.")?;
    } else {
        output::table(&mut out, columns, &rows)?;
    }
    if let Some(cursor) = result["nextCursor"].as_str() {
        writeln!(
            out,
            "\nNext page: rerun with --cursor {cursor} (or use --all)"
        )?;
    }
    Ok(())
}

fn identity_row(identity: &Value) -> Value {
    let access = match (identity["canAssume"] == true, identity["canManage"] == true) {
        (true, true) => json!("assume, manage"),
        (true, false) => json!("assume"),
        (false, true) => json!("manage"),
        (false, false) => Value::Null,
    };
    json!({
        "id": identity["id"],
        "name": identity["name"],
        "enabled": if identity["enabled"] == true { "yes" } else { "no" },
        "access": access,
    })
}

fn session_row(session: &Value) -> Value {
    let status = if session["active"] == true {
        "active"
    } else if !session["revokedAt"].is_null() {
        "revoked"
    } else {
        "inactive"
    };
    json!({
        "id": session["id"],
        "label": session["label"],
        "user": session["userName"],
        "status": status,
        "expires": session["expiresAt"],
    })
}

fn print_permissions(identity: &Value, json: bool) -> Result<()> {
    let permissions = &identity["permissions"];
    if !output::tables(json) {
        return output::print(
            &json!({
                "id": identity["id"],
                "name": identity["name"],
                "version": identity["version"],
                "permissions": permissions,
            }),
            json,
        );
    }
    output::table(
        &mut io::stdout().lock(),
        &["resource", "read", "write", "use"],
        &grid(permissions),
    )?;
    Ok(())
}

/// One row per dashboard resource, plus any other resource the identity holds.
fn grid(permissions: &Value) -> Vec<Value> {
    let cell = |resource: &str, action: &str, offered: bool| {
        let held = permissions[resource]
            .as_array()
            .is_some_and(|actions| actions.iter().any(|a| a == action));
        match (offered, held) {
            (_, true) => json!("yes"),
            (true, false) => json!("no"),
            (false, false) => Value::Null,
        }
    };
    let row = |name: &str, key: &str, offered: &[&str]| {
        let mut row = json!({ "resource": name });
        for action in ACTIONS {
            row[action] = cell(key, action, offered.contains(&action));
        }
        row
    };
    let mut rows: Vec<Value> = RESOURCES
        .iter()
        .map(|(name, key, offered)| row(name, key, offered))
        .collect();
    if let Some(all) = permissions.as_object() {
        for (key, actions) in all {
            let known = RESOURCES.iter().any(|(_, k, _)| k == key);
            if !known && actions.as_array().is_some_and(|a| !a.is_empty()) {
                rows.push(row(key, key, &[]));
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_accepts_dashboard_and_api_names() {
        let expected = Permission {
            resource: "execs",
            action: "write",
        };
        assert_eq!(permission("commands:write").unwrap(), expected);
        assert_eq!(permission(" Execs:WRITE ").unwrap(), expected);
        assert_eq!(
            permission("registryCredentials:use").unwrap(),
            Permission {
                resource: "registryCredentials",
                action: "use"
            }
        );
        assert_eq!(permission("mcp-tools:use").unwrap().resource, "gateway");
    }

    #[test]
    fn permission_rejects_actions_platform_does_not_check() {
        assert_eq!(
            permission("sandboxes:use").unwrap_err(),
            "sandboxes accepts read, write"
        );
        assert_eq!(
            permission("secrets:write").unwrap_err(),
            "secrets accepts read, use"
        );
        assert!(
            permission("billing:read")
                .unwrap_err()
                .starts_with("Unknown resource")
        );
        assert!(
            permission("sandboxes")
                .unwrap_err()
                .starts_with("Use RESOURCE:ACTION")
        );
        assert!(permission("sandboxes:delete").is_err());
    }

    #[test]
    fn allowing_write_or_use_adds_read_and_denials_apply_last() {
        let mut permissions = Map::from_iter([
            ("sandboxes".to_owned(), json!(["read"])),
            ("volumes".to_owned(), json!(["read", "write"])),
        ]);
        let allow = [
            permission("images:use").unwrap(),
            permission("sandboxes:write").unwrap(),
        ];
        let deny = [
            permission("volumes:write").unwrap(),
            permission("sandboxes:read").unwrap(),
        ];
        apply(&mut permissions, &allow, &deny).unwrap();
        assert_eq!(permissions["images"], json!(["read", "use"]));
        assert_eq!(permissions["sandboxes"], json!(["write"]));
        assert_eq!(permissions["volumes"], json!(["read"]));
    }

    #[test]
    fn requests_that_already_hold_leave_permissions_untouched() {
        let original = Map::from_iter([("images".to_owned(), json!(["use", "read"]))]);
        let mut permissions = original.clone();
        let allow = [permission("images:use").unwrap()];
        let deny = [permission("volumes:write").unwrap()];
        apply(&mut permissions, &allow, &deny).unwrap();
        assert_eq!(permissions, original);
    }

    #[test]
    fn unchanged_compares_only_the_fields_a_save_sends() {
        let current = json!({"version": 2, "enabled": true, "name": "bot", "canManage": true});
        assert!(unchanged(
            &current,
            &json!({"version": 2, "enabled": true, "name": "bot"})
        ));
        assert!(!unchanged(
            &current,
            &json!({"version": 2, "enabled": false, "name": "bot"})
        ));
        assert!(!unchanged(&current, &json!({"version": 2, "grants": []})));
    }

    #[test]
    fn the_same_action_cannot_be_allowed_and_denied() {
        let both = [permission("images:use").unwrap()];
        assert!(apply(&mut Map::new(), &both, &both).is_err());
    }

    #[test]
    fn merge_keeps_current_fields_and_drops_response_only_ones() {
        let current = json!({
            "id": "x", "version": 4, "name": "bot", "description": "d", "enabled": true,
            "permissions": {"sandboxes": ["read"]}, "connections": [{"connectionId": "c"}],
            "environmentVariableSetIds": ["s"], "maxSessionSeconds": 3600,
            "grants": [{"memberId": "m", "name": "n"}], "canManage": true,
        });
        let changes = Map::from_iter([("enabled".to_owned(), json!(false))]);
        assert_eq!(
            merged(&current, changes).unwrap(),
            json!({
                "name": "bot", "description": "d", "enabled": false,
                "permissions": {"sandboxes": ["read"]}, "connections": [{"connectionId": "c"}],
                "environmentVariableSetIds": ["s"], "maxSessionSeconds": 3600, "version": 4,
            })
        );
    }

    #[test]
    fn merge_refuses_a_stale_version_and_accepts_the_current_one() {
        let current = json!({"version": 4, "name": "bot"});
        let stale = Map::from_iter([("version".to_owned(), json!(3))]);
        assert!(
            merged(&current, stale)
                .unwrap_err()
                .to_string()
                .contains("version 4, not 3")
        );
        let fresh = Map::from_iter([
            ("version".to_owned(), json!(4)),
            ("name".to_owned(), json!("new")),
        ]);
        assert_eq!(
            merged(&current, fresh).unwrap(),
            json!({"name": "new", "version": 4})
        );
    }

    #[test]
    fn grid_marks_actions_a_resource_does_not_have() {
        let rows = grid(&json!({"execs": ["read"], "gateway": ["use"], "billing": ["read"]}));
        assert_eq!(
            rows[1],
            json!({"resource": "commands", "read": "yes", "write": "no", "use": null})
        );
        assert_eq!(
            rows[7],
            json!({"resource": "mcp-tools", "read": "no", "write": null, "use": "yes"})
        );
        assert_eq!(
            rows[9],
            json!({"resource": "billing", "read": "yes", "write": null, "use": null})
        );
    }

    #[test]
    fn rows_summarize_access_and_session_status() {
        let identity = json!({"id": "i", "name": "bot", "enabled": false, "canAssume": false, "canManage": true});
        assert_eq!(
            identity_row(&identity),
            json!({"id": "i", "name": "bot", "enabled": "no", "access": "manage"})
        );
        let revoked = json!({"id": "s", "label": "ci", "userName": "Ana", "active": false, "revokedAt": "2026-11-17T00:00:00Z", "expiresAt": "e"});
        assert_eq!(session_row(&revoked)["status"], "revoked");
        let ended = json!({"active": false, "revokedAt": null});
        assert_eq!(session_row(&ended)["status"], "inactive");
    }
}
