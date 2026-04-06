# FerrumGate

FerrumGate is a Rust multi-provider LLM gateway with three explicit surfaces:

- OpenAI-compatible public data plane: `/v1/models`, `/v1/chat/completions`, `/v1/responses`
- External tenant management API: tenant-scoped API key lifecycle, model visibility, usage, request history
- Internal control plane: provider account pool ingestion, auto-derived route groups and bindings, tenants, RBAC, audit trail

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
web/
  tenant-console/    Bun + React tenant self-service console
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
- Internal control plane auto-derives models from active provider-account capabilities
- Route groups and bindings are created automatically during ingest and revalidation
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
cp .env.example .env
export DATABASE_URL=postgres://ferrum_gate:ferrum_gate@127.0.0.1:5432/ferrum_gate
export FERRUMGATE_MASTER_KEY=local-dev-master-key
export FERRUMGATE_SEED_DEMO_DATA=true
export FERRUMGATE_TENANT_API_ALLOWED_ORIGINS=http://127.0.0.1:5173
bun install --frozen-lockfile
cargo fmt
cargo test
bun run lint
bun run typecheck
bun run test
cargo run -p gateway-http
cargo run -p tenant-api
cargo run -p control-plane
```

If `DATABASE_URL` is unset, the apps fall back to the seeded in-memory demo store. If `DATABASE_URL` is set, Postgres stays empty by default unless you opt into demo seed data with `FERRUMGATE_SEED_DEMO_DATA=true`.

Default addresses:

- `gateway-http`: `127.0.0.1:3005`
- `tenant-api`: `127.0.0.1:3006`
- `control-plane`: `127.0.0.1:3007`
- `tenant-console`: `127.0.0.1:5173`

## Backend-Only Docker Compose

For a VPS deployment that only needs the APIs, you can now run Postgres plus the three backend services with Docker Compose, fronted by Nginx.

1. Copy the environment template:

```bash
cp .env.example .env
```

2. Edit `.env` for your server:

```bash
FERRUMGATE_MASTER_KEY=<strong-random-secret>
FERRUMGATE_SEED_DEMO_DATA=true
```

If you want Compose to use its bundled Postgres service, leave:

```bash
DATABASE_URL=postgres://ferrum_gate:ferrum_gate@postgres:5432/ferrum_gate
```

If you want to use an external Postgres instead, replace `DATABASE_URL` with that external connection string.

3. Start the backend stack:

```bash
docker compose up -d --build
```

4. Verify the services:

```bash
curl http://127.0.0.1/health
curl http://127.0.0.1/v1/models -H "Authorization: Bearer fgk_demo_gateway_key"
curl http://127.0.0.1/tenant/v1/me -H "Authorization: Bearer fg_tenant_admin_demo"
curl http://127.0.0.1/internal/v1/provider-accounts -H "Authorization: Bearer fg_cp_admin_demo"
```

This backend-only deployment is enough to:

- upload provider accounts through `control-plane`
- create downstream API keys through `tenant-api`
- serve OpenAI-compatible chat and responses traffic through `gateway-http`

Security note:

- Nginx listens on `NGINX_PORT` and proxies:
- `/health` and `/v1/*` -> `gateway-http`
- `/tenant/*` -> `tenant-api`
- `/internal/*` and `/external/*` -> `control-plane`
- `tenant-api` and `control-plane` should still usually be restricted by firewall, reverse proxy auth, or source IP allowlists if they are reachable from untrusted networks

## Tenant Console

The tenant self-service console lives in `web/tenant-console` and is managed with Bun.
It is a SPA built with React, Vite, TanStack Router/Query, Tailwind v4, and `shadcn/ui`.
The current console ships as a dark-by-default operations workspace with dedicated pages for dashboard, accounts, API keys, routing overview, alerts, audit, and integration docs.

Recommended local flow:

```bash
cp .env.example .env
export FERRUMGATE_TENANT_API_ALLOWED_ORIGINS=http://127.0.0.1:5173
cargo run -p tenant-api
bun run dev --cwd web/tenant-console
```

Frontend development reads environment variables from the repository root via Vite `envDir`.
Useful variables:

- `VITE_DEFAULT_TENANT_API_BASE_URL=http://127.0.0.1:3006`
- `VITE_DEFAULT_CONTROL_PLANE_BASE_URL=http://127.0.0.1:3007`
- `VITE_DEFAULT_GATEWAY_BASE_URL=http://127.0.0.1:3005/v1`
- `VITE_TENANT_MANAGEMENT_TOKEN=fg_tenant_admin_demo`
- `VITE_CONTROL_PLANE_TOKEN=fg_cp_admin_demo`
- `VITE_CONSOLE_SECRET_TOKEN=<your-console-secret>`
- `VITE_CONSOLE_USERNAME=<optional-console-username>`
- `VITE_CONSOLE_PASSWORD=<optional-console-password>`
- `FERRUMGATE_TENANT_API_ALLOWED_ORIGINS=http://127.0.0.1:5173`

The unified console no longer asks operators to paste raw backend URLs or system tokens on the login page.
Deployment should fix the tenant API, control plane, and gateway endpoints via environment variables, then expose either a single `VITE_CONSOLE_SECRET_TOKEN` or a `VITE_CONSOLE_USERNAME` / `VITE_CONSOLE_PASSWORD` pair for operator sign-in.
When `tenant-console` and `tenant-api` are deployed on different origins, set `FERRUMGATE_TENANT_API_ALLOWED_ORIGINS` to the exact console origin.

## Routing Model Derivation

- Provider account ingest and revalidation probe upstream capabilities.
- Every discovered model automatically ensures a matching `route_group` and `route_group_binding`.
- `/v1/models` and the tenant console dashboard now derive visible models from active provider-account capabilities instead of depending on manually seeded route groups.
- Manual route-group and binding APIs still exist for advanced overrides, but the default path is automatic derivation.

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

After import, FerrumGate will probe the account, persist the discovered capabilities, and automatically derive route groups and bindings for each upstream model without a separate manual routing step.

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
- The control plane exposes `/internal/v1/routing/overview` so the console can inspect auto-derived route groups and binding counts without editing routing state.
- Redis is intentionally deferred. Runtime coordination still lives in `scheduler` and the database-backed `account_runtime` table for this phase.

See [docs/architecture.md](docs/architecture.md) for the first-pass system design.
