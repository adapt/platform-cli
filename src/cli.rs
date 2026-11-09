use crate::input::pair;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "adapt",
    version,
    about = "Your Platform, from the terminal.",
    after_help = "Start here: adapt auth login\nUse --json for scripting. Run any command with --help for details."
)]
pub struct Cli {
    /// Saved connection profile
    #[arg(long, global = true, env = "ADAPT_PROFILE", default_value = "default")]
    pub profile: String,
    /// Platform API origin (defaults to https://platform.adapt.com)
    #[arg(long, global = true, env = "ADAPT_API_URL")]
    pub api_url: Option<String>,
    /// Organization ID for this request
    #[arg(long, global = true, env = "ADAPT_ORG_ID")]
    pub org: Option<Uuid>,
    /// Emit JSON (also the default when stdout is piped)
    #[arg(long, global = true)]
    pub json: bool,
    /// Configuration directory
    #[arg(long, global = true, env = "ADAPT_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,
    /// HTTP request timeout in seconds (streams use an inactivity timeout)
    #[arg(long, global = true, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub timeout: u64,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Browser sign-in, sign-out, and session status
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Install or remove the bundled guide for coding agents (works offline)
    #[command(subcommand)]
    Skill(SkillCommand),
    /// Show your effective identity and organization permissions
    Whoami,
    /// Manage organizations and choose a default
    #[command(subcommand, visible_alias = "orgs")]
    Org(OrgCommand),
    /// Manage compute sandboxes
    #[command(subcommand, visible_alias = "sandboxes")]
    Sandbox(SandboxCommand),
    /// Run a command and stream its output; arguments follow --
    Exec(ExecArgs),
    /// Inspect, cancel, or attach to executions
    #[command(subcommand)]
    Execs(ExecCommand),
    /// Upload and download sandbox files
    #[command(subcommand)]
    Files(FileCommand),
    /// Manage persistent volumes
    #[command(subcommand, visible_alias = "volume")]
    Volumes(VolumeCommand),
    /// Reserve URLs and bind them to sandboxes
    #[command(subcommand, visible_alias = "endpoint")]
    Endpoints(EndpointCommand),
    /// Import and manage immutable images
    #[command(subcommand, visible_alias = "image")]
    Images(ImageCommand),
    /// Manage encrypted environment variable sets
    #[command(subcommand)]
    Env(EnvCommand),
    /// Manage private registry credentials
    #[command(subcommand)]
    Registry(RegistryCommand),
    /// Manage upstream MCP connections and call tools
    #[command(subcommand)]
    Connections(ConnectionCommand),
    /// Manage organization API keys
    #[command(subcommand)]
    Keys(KeyCommand),
    /// Manage your browser and CLI sessions
    #[command(subcommand)]
    Sessions(SessionCommand),
    /// Inspect and revoke connected MCP clients
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Watch resource changes as newline-delimited JSON
    Events {
        #[arg(long)]
        personal: bool,
    },
    /// Inspect saved profiles (credentials are never printed)
    Profiles,
    /// Check API health, or database readiness with --ready
    Health {
        #[arg(long)]
        ready: bool,
    },
    /// Make an authenticated request to a path on this Platform origin
    Api(ApiArgs),
}

#[derive(Subcommand)]
pub enum SkillCommand {
    /// Print the bundled skill as Markdown
    Show,
    /// Install or update the guide for coding agents
    Install {
        /// Agent to configure; omitted installs for all supported agents
        #[arg(value_enum, default_value = "all")]
        target: SkillTarget,
    },
    /// Remove the guide, preserving other files in its directory
    Uninstall {
        /// Agent to configure; shared directories affect every agent using them
        #[arg(value_enum, default_value = "all")]
        target: SkillTarget,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SkillTarget {
    Codex,
    #[value(alias = "claude-code")]
    Claude,
    Pi,
    Opencode,
    All,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Sign in through your browser, or import an API key from stdin
    Login {
        /// Print the approval URL without opening a browser
        #[arg(long, conflicts_with = "token_stdin")]
        no_browser: bool,
        /// Read an organization API key from stdin instead of browser login
        #[arg(long)]
        token_stdin: bool,
    },
    /// Revoke the saved CLI session and remove its local credential
    Logout {
        /// Remove only the local credential (for an unreachable server)
        #[arg(long)]
        local: bool,
    },
    /// Verify authentication without printing credentials
    Status,
}

#[derive(Args, Clone, Default)]
pub struct Page {
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub limit: Option<u16>,
    #[arg(long)]
    pub cursor: Option<Uuid>,
    /// Follow all cursor pages
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub struct Data {
    /// JSON object, @file.json, or - for stdin
    #[arg(long, short = 'd', required = true)]
    pub data: String,
}

#[derive(Args, Default)]
pub struct Creation {
    /// Reuse this UUID when retrying the same creation request
    #[arg(long)]
    pub idempotency_key: Option<Uuid>,
}

#[derive(Subcommand)]
pub enum OrgCommand {
    #[command(visible_alias = "ls")]
    List,
    /// Select by organization ID or exact slug
    Use {
        organization: String,
    },
    Current,
    Create {
        name: String,
        #[arg(long)]
        slug: String,
    },
    Update {
        #[command(flatten)]
        data: Data,
    },
    Delete,
    #[command(subcommand)]
    Members(MemberCommand),
    #[command(subcommand)]
    Invitations(InvitationCommand),
}

#[derive(Subcommand)]
pub enum MemberCommand {
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    Role {
        id: Uuid,
        #[arg(value_enum)]
        role: Role,
    },
    Remove {
        id_or_email: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Role {
    Owner,
    Admin,
    Member,
}

#[derive(Subcommand)]
pub enum InvitationCommand {
    #[command(visible_alias = "ls")]
    List,
    Create {
        email: String,
        #[arg(long, value_enum, default_value = "member")]
        role: Role,
        /// Create an invitation link without sending email
        #[arg(long)]
        link: bool,
        #[arg(long)]
        resend: bool,
    },
    Cancel {
        id: Uuid,
    },
    Accept {
        id: Uuid,
    },
    Reject {
        id: Uuid,
    },
}

#[derive(Subcommand)]
pub enum SandboxCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    Get {
        id: Uuid,
    },
    Create(SandboxCreate),
    /// Run a login shell or command in a temporary sandbox, then delete it
    #[command(
        after_help = "Accepts all sandbox create options. Always waits for the sandbox to run.\nWithout COMMAND, opens an interactive login shell (requires a terminal).\nWith COMMAND, forwards stdin and streams output. Returns the remote exit code.\nThe sandbox is deleted when the shell or command exits."
    )]
    Sh(SandboxShell),
    Start {
        id: Uuid,
        #[arg(long)]
        wait: bool,
    },
    Stop {
        id: Uuid,
        #[arg(long)]
        wait: bool,
    },
    Delete {
        id: Uuid,
    },
    /// Wait for running (or another --status), with a bounded deadline
    Wait {
        id: Uuid,
        #[arg(long, default_value = "running")]
        status: String,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=86400))]
        wait_timeout: u64,
    },
}

#[derive(Args)]
pub struct SandboxShell {
    #[command(flatten)]
    pub create: SandboxCreate,
    /// Command and literal arguments to run instead of an interactive login shell
    #[arg(last = true, num_args = 1.., value_name = "COMMAND")]
    pub argv: Vec<String>,
}

#[derive(Args)]
pub struct SandboxCreate {
    /// Full request JSON; supplied flags override fields in it
    #[arg(long)]
    pub data: Option<String>,
    #[arg(long, conflicts_with = "image_id")]
    pub image: Option<String>,
    #[arg(long, conflicts_with = "image")]
    pub image_id: Option<Uuid>,
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=32))]
    pub cpu: Option<u8>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(128..=65536))]
    pub memory: Option<u32>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(64..=131072))]
    pub disk: Option<u32>,
    #[arg(long, conflicts_with = "no_expiry", value_parser = clap::value_parser!(u32).range(1..=86400))]
    pub ttl: Option<u32>,
    #[arg(long)]
    pub no_expiry: bool,
    #[arg(long = "env", short = 'e', value_parser = pair)]
    pub env: Vec<(String, String)>,
    #[arg(long = "label", value_parser = pair)]
    pub labels: Vec<(String, String)>,
    #[arg(long)]
    pub variable_set: Vec<Uuid>,
    /// Run the image's ENTRYPOINT/CMD on boot
    #[arg(long)]
    pub run_image: bool,
    #[arg(long)]
    pub wait: bool,
    #[command(flatten)]
    pub creation: Creation,
}

#[derive(Args)]
pub struct ExecArgs {
    pub sandbox: Uuid,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long = "env", short = 'e', value_parser = pair)]
    pub env: Vec<(String, String)>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=86400000))]
    pub timeout_ms: Option<u64>,
    /// Return immediately with execution metadata
    #[arg(long, conflicts_with_all = ["stdin", "tty"])]
    pub detach: bool,
    /// Forward stdin through a WebSocket
    #[arg(short = 'i', long)]
    pub stdin: bool,
    /// Allocate an interactive terminal (implies stdin)
    #[arg(short = 't', long)]
    pub tty: bool,
    #[command(flatten)]
    pub creation: Creation,
    #[arg(required = true, last = true, num_args = 1..)]
    pub argv: Vec<String>,
}

#[derive(Subcommand)]
pub enum ExecCommand {
    Get {
        id: Uuid,
    },
    Cancel {
        id: Uuid,
    },
    /// Stream retained, redacted logs, resuming after --cursor
    Logs {
        id: Uuid,
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=4096))]
        cursor: u64,
    },
    /// Attach to unredacted live output (read-only unless --stdin or --tty)
    Attach {
        id: Uuid,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(short = 'i', long)]
        stdin: bool,
        #[arg(short = 't', long)]
        tty: bool,
    },
}

#[derive(Subcommand)]
pub enum FileCommand {
    /// Upload local bytes; use - to read stdin
    Put {
        sandbox: Uuid,
        local: PathBuf,
        remote: String,
    },
    /// Download bytes; use - to write stdout
    Get {
        sandbox: Uuid,
        remote: String,
        local: PathBuf,
        /// Replace an existing local file
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum VolumeCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    Get {
        id: Uuid,
    },
    Create {
        #[arg(long)]
        quota_bytes: Option<u64>,
        #[command(flatten)]
        creation: Creation,
    },
    Quota {
        id: Uuid,
        #[arg(
            long,
            required_unless_present = "unlimited",
            conflicts_with = "unlimited"
        )]
        bytes: Option<u64>,
        #[arg(long)]
        unlimited: bool,
    },
    /// Request deletion (the current backend returns unsupported for ready volumes)
    Delete {
        id: Uuid,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Access {
    Public,
    Authenticated,
}

#[derive(Subcommand)]
pub enum EndpointCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    Get {
        id: Uuid,
    },
    Create {
        name: String,
        #[arg(long, value_enum, default_value = "public")]
        access: Access,
        #[command(flatten)]
        creation: Creation,
    },
    Access {
        id: Uuid,
        #[arg(value_enum)]
        access: Access,
    },
    Bind {
        id: Uuid,
        sandbox: Uuid,
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: u16,
    },
    Unbind {
        id: Uuid,
    },
    Token {
        id: Uuid,
    },
    RevokeTokens {
        id: Uuid,
    },
    Delete {
        id: Uuid,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Architecture {
    Amd64,
    Arm64,
}

#[derive(Subcommand)]
pub enum ImageCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    Get {
        id: Uuid,
    },
    Import {
        source: String,
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value = "amd64")]
        architecture: Architecture,
        #[arg(long)]
        credential: Option<Uuid>,
        #[arg(long)]
        wait: bool,
        #[command(flatten)]
        creation: Creation,
    },
    Pull {
        id: Uuid,
        #[arg(long, conflicts_with = "public")]
        credential: Option<Uuid>,
        #[arg(long)]
        public: bool,
        #[arg(long)]
        wait: bool,
        #[command(flatten)]
        creation: Creation,
    },
    Wait {
        id: Uuid,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=86400))]
        wait_timeout: u64,
    },
    Delete {
        id: Uuid,
    },
}

#[derive(Subcommand)]
pub enum EnvCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    Get {
        id: Uuid,
    },
    /// JSON: {"name":"production","env":{"KEY":"value"}}
    Create(Data),
    /// JSON: {"name":"production","version":1,"env":{"KEY":null}}; null deletes a variable
    Update {
        id: Uuid,
        #[command(flatten)]
        data: Data,
    },
    Delete {
        id: Uuid,
    },
}

#[derive(Subcommand)]
pub enum RegistryCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    /// JSON: name, registry (HTTPS origin), username, password, optional tokenOrigin
    Create(Data),
    /// Same fields as create, plus the current version
    Update {
        id: Uuid,
        #[command(flatten)]
        data: Data,
    },
    Delete {
        id: Uuid,
    },
}

#[derive(Subcommand)]
pub enum ConnectionCommand {
    #[command(visible_alias = "ls")]
    List(Page),
    Get {
        id: Uuid,
    },
    /// JSON: name, url, authType (none/bearer/oauth), optional token and visibility
    Create(Data),
    /// JSON: version, name, enabled, allowedTools, optional token
    Update {
        id: Uuid,
        #[command(flatten)]
        data: Data,
    },
    Delete {
        id: Uuid,
    },
    Refresh {
        id: Uuid,
    },
    /// JSON: principalType (mcp/apiKey), principalId, tools
    Grant {
        id: Uuid,
        #[command(flatten)]
        data: Data,
    },
    /// Open upstream authorization in a browser
    Oauth {
        id: Uuid,
        #[arg(long)]
        no_browser: bool,
    },
    /// Complete upstream authorization with JSON: code, state, optional iss
    OauthComplete {
        id: Uuid,
        #[command(flatten)]
        data: Data,
    },
    Calls {
        id: Uuid,
        #[command(flatten)]
        page: Page,
    },
    Principals,
    Search {
        #[arg(default_value = "")]
        query: String,
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    Describe {
        connection: Uuid,
        tool: String,
    },
    Call {
        connection: Uuid,
        tool: String,
        /// Tool argument object: inline JSON, @file, or -
        #[arg(long, default_value = "{}")]
        arguments: String,
    },
}

#[derive(Subcommand)]
pub enum KeyCommand {
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Create a key; the secret is returned once
    Create {
        name: String,
        #[arg(long)]
        expires_in: Option<u64>,
        /// Permission object as inline JSON, @file, or -
        #[arg(long)]
        permissions: Option<String>,
    },
    Delete {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum SessionCommand {
    #[command(visible_alias = "ls")]
    List,
    Revoke {
        id: Uuid,
    },
    RevokeOthers,
}

#[derive(Subcommand)]
pub enum McpCommand {
    #[command(visible_alias = "ls")]
    List,
    Revoke {
        id: Uuid,
    },
}

#[derive(Args)]
pub struct ApiArgs {
    #[arg(value_parser = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"])]
    pub method: String,
    /// Absolute API path, e.g. /v1/sandboxes (never a full URL)
    pub path: String,
    #[arg(long, short = 'd')]
    pub data: Option<String>,
    #[arg(long, short = 'q', value_parser = pair)]
    pub query: Vec<(String, String)>,
    #[arg(long)]
    pub idempotency_key: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_consistent() {
        Cli::command().debug_assert();
    }
}
