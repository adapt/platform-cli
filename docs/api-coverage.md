# API coverage

Based on the resource OpenAPI document from `../platform` at `f20ded3`, plus its dashboard
auth integrations. Every resource operation has a named command. `adapt api METHOD PATH`
also exposes same-origin JSON requests for less common account operations.

`{id}` denotes the corresponding resource ID. Writes requiring an idempotency key receive
one from the CLI, or reuse the UUID supplied with `--idempotency-key`.

| API operation | CLI |
| --- | --- |
| `GET /health` | `health` |
| `GET /ready` | `health --ready` |
| `GET /openapi.json` | `api GET /openapi.json` |
| `GET /v1/whoami` | `whoami` |
| `GET /v1/events` | `events` |
| `GET /v1/auth/events` | `events --personal` |
| `POST /v1/sandboxes` | `sandbox create` |
| `GET /v1/sandboxes` | `sandbox list` |
| `GET /v1/sandboxes/{id}` | `sandbox get ID` |
| `DELETE /v1/sandboxes/{id}` | `sandbox delete ID` |
| `POST /v1/sandboxes/{id}/start` | `sandbox start ID` |
| `POST /v1/sandboxes/{id}/stop` | `sandbox stop ID` |
| `POST /v1/sandboxes/{id}/exec` | `exec ID -- COMMAND ARGS...` |
| `GET /v1/execs/{id}` | `execs get ID` |
| `POST /v1/execs/{id}/cancel` | `execs cancel ID` |
| `GET /v1/execs/{id}/events` | `execs logs ID` |
| `GET /v1/execs/{id}/stream` (WebSocket) | `execs attach ID` |
| `GET /v1/sandboxes/{id}/files` | `files get ID REMOTE LOCAL` |
| `PUT /v1/sandboxes/{id}/files` | `files put ID LOCAL REMOTE` |
| `POST /v1/volumes` | `volumes create` |
| `GET /v1/volumes` | `volumes list` |
| `GET /v1/volumes/{id}` | `volumes get ID` |
| `PUT /v1/volumes/{id}/quota` | `volumes quota ID` |
| `DELETE /v1/volumes/{id}` | `volumes delete ID` |
| `POST /v1/ingresses` | `endpoints create NAME` |
| `GET /v1/ingresses` | `endpoints list` |
| `GET /v1/ingresses/{id}` | `endpoints get ID` |
| `DELETE /v1/ingresses/{id}` | `endpoints delete ID` |
| `PUT /v1/ingresses/{id}/binding` | `endpoints bind ID SANDBOX --port PORT` |
| `DELETE /v1/ingresses/{id}/binding` | `endpoints unbind ID` |
| `PUT /v1/ingresses/{id}/access` | `endpoints access ID public\|authenticated` |
| `POST /v1/ingresses/{id}/access-tokens` | `endpoints token ID` |
| `DELETE /v1/ingresses/{id}/access-tokens` | `endpoints revoke-tokens ID` |
| `POST /v1/images` | `images import SOURCE --name NAME` |
| `GET /v1/images` | `images list` |
| `GET /v1/images/{id}` | `images get ID` |
| `POST /v1/images/{id}/pull` | `images pull ID` |
| `DELETE /v1/images/{id}` | `images delete ID` |
| `POST /v1/registry-credentials` | `registry create --data JSON` |
| `GET /v1/registry-credentials` | `registry list` |
| `PUT /v1/registry-credentials/{id}` | `registry update ID --data JSON` |
| `DELETE /v1/registry-credentials/{id}` | `registry delete ID` |
| `POST /v1/environment-variable-sets` | `env create --data JSON` |
| `GET /v1/environment-variable-sets` | `env list` |
| `GET /v1/environment-variable-sets/{id}` | `env get ID` |
| `PUT /v1/environment-variable-sets/{id}` | `env update ID --data JSON` |
| `DELETE /v1/environment-variable-sets/{id}` | `env delete ID` |
| `POST /v1/gateway/connections` | `connections create --data JSON` |
| `GET /v1/gateway/connections` | `connections list` |
| `GET /v1/gateway/connections/{id}` | `connections get ID` |
| `PUT /v1/gateway/connections/{id}` | `connections update ID --data JSON` |
| `DELETE /v1/gateway/connections/{id}` | `connections delete ID` |
| `POST /v1/gateway/connections/{id}/refresh` | `connections refresh ID` |
| `PUT /v1/gateway/connections/{id}/grants` | `connections grant ID --data JSON` |
| `POST /v1/gateway/connections/{id}/oauth` | `connections oauth ID` |
| `POST /v1/gateway/connections/{id}/oauth/complete` | `connections oauth-complete ID --data JSON` |
| `GET /v1/gateway/connections/{id}/calls` | `connections calls ID` |
| `GET /v1/gateway/principals` | `connections principals` |
| `GET /v1/gateway/search` | `connections search QUERY` |
| `POST /v1/gateway/describe` | `connections describe ID TOOL` |
| `POST /v1/gateway/call` | `connections call ID TOOL --arguments JSON` |

Auth commands use `/v1/auth/device/code`, `/device/token`, `/get-session`, and `/sign-out`.
Organization commands use Better Auth's `/v1/auth/organization/*` routes; organization
selection is local and sent as `X-Organization-Id`, leaving the browser's active organization
unchanged. API-key commands use `/v1/auth/api-key/*` with `configId: "platform"`.
Session and MCP client management use Platform's `/v1/auth/sessions*` and
`/v1/auth/mcp/{connections,revoke}` routes. Provider callback and browser approval endpoints
remain browser interactions.
