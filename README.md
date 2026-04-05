# FerrumGate

FerrumGate is a Rust multi-provider LLM gateway with three explicit surfaces:

- OpenAI-compatible public data plane: `/v1/models`, `/v1/chat/completions`, `/v1/responses`
- External tenant management API: tenant-scoped API key lifecycle, model visibility, usage, request history
- Internal control plane: provider account pool ingestion, route groups, bindings, tenants, RBAC, audit trail

This first milestone is a runnable skeleton that fixes the module boundaries early and leaves room for:

- multiple provider adapters with compile-time registration
- provider account pool scheduling and cooldown state
- future Anthropic-compatible frontend without rewriting the scheduler
- Postgres-first persistence with a demo in-memory fallback

## Workspace Layout

```text
apps/
  gateway-http/      OpenAI-compatible public API
  tenant-api/        Tenant self-service management API
  control-plane/     Internal provider and routing management API
crates/
  protocol-core/     Canonical request/response abstractions
  provider-core/     Provider traits and registry
  provider-openai-codex/
                     First provider implementation
  scheduler/         Account state machine and candidate selection
  storage/           Unified store wrapper + memory/postgres backends
  observability/     Tracing bootstrap
migrations/          Postgres schema bootstrap
```

## Current Milestone

- Rust workspace, tooling and CI are in place
- OpenAI-compatible routes are runnable with a real `openai_codex` HTTP adapter
- Tenant API key lifecycle works through a unified store abstraction
- Internal control plane can import provider accounts, create route groups and bindings
- RBAC roles and scope checks are enforced in the control plane skeleton
- `DATABASE_URL` switches the apps onto a real Postgres backend
- Without `DATABASE_URL`, the apps still boot in demo memory mode

## Demo Credentials

The in-memory demo backend is seeded so the skeleton can run immediately:

- Gateway API key: `fgk_demo_gateway_key`
- Tenant management token: `fg_tenant_admin_demo`
- Control plane admin token: `fg_cp_admin_demo`

These are development-only seeds. Real provider credentials are never returned by the API and should only be stored encrypted.
Postgres does not auto-seed demo records unless `FERRUMGATE_SEED_DEMO_DATA=true` is set explicitly.

## Run

```bash
docker compose up -d
export DATABASE_URL=postgres://ferrum_gate:ferrum_gate@127.0.0.1:5432/ferrum_gate
export FERRUMGATE_MASTER_KEY=local-dev-master-key
export FERRUMGATE_SEED_DEMO_DATA=true
cargo fmt
cargo test
cargo run -p gateway-http
cargo run -p tenant-api
cargo run -p control-plane
```

If `DATABASE_URL` is unset, the apps fall back to the seeded in-memory demo store. If `DATABASE_URL` is set, Postgres stays empty by default unless you opt into demo seed data with `FERRUMGATE_SEED_DEMO_DATA=true`.

Default addresses:

- `gateway-http`: `127.0.0.1:3005`
- `tenant-api`: `127.0.0.1:3006`
- `control-plane`: `127.0.0.1:3007`

## Example Requests

List models through the public gateway:

```bash
curl http://127.0.0.1:3005/v1/models \
  -H "Authorization: Bearer fgk_demo_gateway_key"
```

Create a tenant API key:

```bash
curl http://127.0.0.1:3006/tenant/v1/api-keys \
  -H "Authorization: Bearer fg_tenant_admin_demo" \
  -H "Content-Type: application/json" \
  -d '{"label":"sdk"}'
```

Import a provider account into the control plane:

```bash
curl http://127.0.0.1:3007/internal/v1/provider-accounts \
  -H "Authorization: Bearer fg_cp_admin_demo" \
  -H "Content-Type: application/json" \
  -d '{
    "provider":"openai_codex",
    "credential_kind":"oauth_tokens",
    "payload_version":"v1",
    "credentials":{"access_token":"token","account_id":"acct_123"},
    "metadata":{"email":"demo@example.com","plan_type":"plus"},
    "labels":["shared"],
    "tags":{"region":"global"}
  }'
```

Upload a provider account through the external upload interface:

```bash
curl http://127.0.0.1:3007/external/v1/provider-accounts/upload \
  -H "Authorization: Bearer fg_cp_admin_demo" \
  -H "Content-Type: application/json" \
  -d '{
    "provider":"openai_codex",
    "credential_kind":"oauth_tokens",
    "payload_version":"v1",
    "credentials":{
      "access_token":"token",
      "account_id":"acct_external_123"
    },
    "metadata":{
      "email":"external@example.com",
      "plan_type":"plus"
    },
    "labels":["shared"],
    "tags":{"region":"global"}
  }'
```

## Architecture Notes

- `protocol-core` is protocol-neutral on purpose. Future Anthropic support should add a new frontend that maps into the same canonical request and response model.
- `provider-core` owns the adapter trait and registry. New providers should only add a new crate and register it at startup.
- `provider-openai-codex` now resolves encrypted account credentials and performs real upstream HTTP calls, with mock-backed tests covering chat, responses and SSE.
- `scheduler` owns the account state machine and candidate ranking. It does not know about OpenAI or Anthropic wire formats.
- `storage` now supports two backends behind one interface: demo memory mode and a Postgres-first backend that auto-applies `migrations/0001_initial.sql` on startup.
- Redis is intentionally deferred. Runtime coordination still lives in `scheduler` and the database-backed `account_runtime` table for this phase.

See [docs/architecture.md](docs/architecture.md) for the first-pass system design.
