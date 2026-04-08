# FerrumGate - Project Context

## Project Overview

FerrumGate is a **Rust multi-provider LLM gateway** that acts as an OpenAI-compatible proxy with tenant management, routing, and provider account pooling capabilities. It exposes three main HTTP services:

1. **Gateway HTTP** (`:3005`) — OpenAI-compatible public API (`/v1/models`, `/v1/chat/completions`, `/v1/responses`)
2. **Tenant API** (`:3006`) — Tenant self-service management (API key lifecycle, usage, request history)
3. **Control Plane** (`:3007`) — Internal provider/routing management (account pool ingestion, route groups, RBAC, audit)

A **React frontend** (tenant console) lives in `web/tenant-console/` and runs on Bun + Vite at `:5173`.

## Architecture

### Workspace Layout

```
apps/
  gateway-http/        OpenAI-compatible public API (Axum)
  tenant-api/          Tenant self-service management API
  control-plane/       Internal provider and routing management API
crates/
  protocol-core/       Canonical request/response abstractions
  provider-core/       Provider traits and registry
  provider-openai-codex/  OpenAI Codex provider implementation
  provider-anthropic/     Anthropic provider implementation
  scheduler/           Account state machine and candidate selection
  storage/             Unified store wrapper (memory/Postgres backends)
  observability/       Tracing bootstrap
  http-utils/          Shared CORS builder
web/
  tenant-console/      Bun + React SPA (TanStack Router/Query, Tailwind, shadcn/ui)
migrations/            Postgres schema (versioned, auto-applied)
docker/                Dockerfiles for backend services and nginx
```

### Key Design Decisions

- **Persistence**: Postgres-first with in-memory demo fallback. Set `DATABASE_URL` to use Postgres; leave it unset for demo mode.
- **Provider Registration**: New providers implement traits in `provider-core` and register at startup.
- **Routing**: Auto-derived from provider account capabilities (models discovered via upstream probes). Manual overrides exist.
- **Fallback Routing**: Ordered fallback routes for primary public model — enabled for rate limits, upstream failures, transport failures, quota exhaustion; disabled for invalid credentials.
- **Credentials**: Encrypted at rest (AES-256-GCM). Demo seeds available for local development.
- **Migrations**: Versioned via `_migrations` table. Add new migrations to `MIGRATIONS` array in `postgres.rs`.
- **Stream Timeout**: 60s idle timeout on SSE streams across all providers.

## Building and Running

### Prerequisites

- **Rust**: Edition 2024, MSRV 1.93 (see `rust-toolchain.toml`)
- **Bun**: v1.3.9 (frontend package manager)
- **Docker Compose**: For Postgres and full-stack deployment
- **PostgreSQL 16**: If running outside Docker

### Quick Start (Local Development)

```bash
# 1. Set up environment
cp .env.example .env
export DATABASE_URL=postgres://ferrum_gate:ferrum_gate@127.0.0.1:5432/ferrum_gate
export FERRUMGATE_MASTER_KEY=local-dev-master-key
export FERRUMGATE_SEED_DEMO_DATA=true
export FERRUMGATE_TENANT_API_ALLOWED_ORIGINS=http://127.0.0.1:5173

# 2. Start Postgres
docker compose up -d postgres

# 3. Install frontend dependencies
bun install --frozen-lockfile

# 4. Run backend services (separate terminals)
cargo run -p gateway-http     # :3005
cargo run -p tenant-api       # :3006
cargo run -p control-plane    # :3007

# 5. Run frontend
bun run dev:tenant-console    # :5173
```

### Demo Credentials (Dev Only)

| Purpose | Token |
|---------|-------|
| Gateway API key | `fgk_demo_gateway_key` |
| Tenant management token | `fg_tenant_admin_demo` |
| Control plane admin token | `fg_cp_admin_demo` |

### Full Stack via Docker Compose

```bash
docker compose up -d --build
```

This starts Postgres, all three backend services, and an Nginx reverse proxy.

## Testing and Linting

### Backend (Rust)

```bash
cargo fmt --check                                          # Format check
cargo clippy --all-targets --all-features -- -D warnings   # Lint
cargo test --all-targets --all-features                    # Tests (165 tests, all passing)
```

### Frontend (Bun/React)

```bash
bun run lint           # ESLint
bun run typecheck      # TypeScript type checking
bun run test           # Unit tests
bun run test:e2e       # E2E tests (run when UI/auth/route changes)
```

### Run Commands Summary

| Command | Description |
|---------|-------------|
| `bun run dev:tenant-console` | Start frontend dev server |
| `bun run build:tenant-console` | Build frontend for production |
| `cargo run -p gateway-http` | Run gateway service |
| `cargo run -p tenant-api` | Run tenant API service |
| `cargo run -p control-plane` | Run control plane service |

## Development Conventions

- **Frontend**: Stay inside `web/tenant-console/` unless API contracts change. Use Bun only (no npm/pnpm/yarn).
- **Backend**: Follow Rust 2024 edition conventions. Use `sqlx` for Postgres, `axum` for HTTP, `tokio` for async.
- **Demo Mode**: Prefer in-memory demo mode for local development unless working on Postgres/migrations.
- **Env Vars**: Do not rename existing vars from `.env.example` or README.
- **Minimal Diffs**: Keep changes focused. Do not edit unrelated files.

## Key Crates

| Crate | Responsibility |
|-------|----------------|
| `protocol-core` | Protocol-neutral canonical request/response models |
| `provider-core` | Provider adapter traits and registry, `STREAM_IDLE_TIMEOUT` (60s) |
| `provider-openai-codex` | OpenAI Codex provider (endpoint selection, normalization, upstream HTTP, SSE) |
| `provider-anthropic` | Anthropic provider implementation |
| `scheduler` | Account state machine, candidate selection, cooldown |
| `storage` | Unified storage interface (memory + Postgres backends), versioned migrations |
| `observability` | Tracing/metrics setup |
| `http-utils` | Shared CORS builder (eliminated duplicate CORS code across 3 apps) |

## Test Coverage

165 tests total (up from ~90). Key additions:

| Module | Tests Added | Coverage |
|--------|-------------|----------|
| `execution_engine.rs` | 8 | `fallback_eligible`, `canonical_request_for_candidate` |
| `scheduler` | 6 | `InvalidCredentials`, `QuotaExhausted`, `is_schedulable` boundaries |
| `route_resolver.rs` | 5 | Unknown model, no route groups, Responses contract |
| `middleware/auth.rs` | 5 | `parse_bearer_token` edge cases |
| `middleware/request_id.rs` | 3 | ID format, prefix, uniqueness |
| `core/types.rs` | 2 | `ExecutionError` conversion |
| `openai_http.rs` | 23 | Message conversion, tools, responses input, error mapping, JSON builders |
| `http-utils` | 6 | CORS builder edge cases |
| `provider-openai-codex` | 2 | Stream timeout |
| `provider-anthropic` | 1 | Stream timeout |

## Recent Refactoring (Wave 1)

### Phase 1 & 2: Test Coverage
- Added 75+ unit tests across core modules (see table above)

### Phase 3: Code Quality Fixes
- **3.1** Split `lib.rs` tests: `lib.rs` 3172→116 lines, tests in `src/tests.rs`
- **3.2** Extracted shared CORS tool: new `crates/http-utils` crate
- **3.3** Fixed RouteResolver N+1 query: batch `list_all_route_group_fallbacks()`
- **3.4** Versioned migrations: `_migrations` table + `MIGRATIONS` static array
- **3.5** Registered Anthropic provider in control-plane
- **3.6** Stream idle timeout: 60s across all providers

### Remaining Tasks (Phase 4-5)
- Define Execution Contract types
- Extract `ProviderExecutor` trait
- Extract `ProtocolAdapter` trait
- Extract `RouteStore` trait
- `PlatformStore` trait to eliminate enum dispatch

## Deployment

### Docker Compose (Backend Only)

The `docker-compose.yml` now deploys backend services + Nginx (no frontend SPA):
- `/health` → gateway health check
- `/v1/*` → gateway-http
- `/tenant/*` → tenant-api (server-injected auth)
- `/internal/*` and `/external/*` → control-plane (server-injected auth)

### Security Checklist for VPS

1. **TLS**: Add SSL certificates via Nginx (Let's Encrypt / Cloudflare)
2. **Master Key**: Generate strong random key (`openssl rand -base64 32`)
3. **IP Whitelist**: Restrict `/internal/` and `/tenant/` to known IPs
4. **Rate Limiting**: Add Nginx `limit_req` for API and admin paths
5. **Security Headers**: `X-Content-Type-Options`, `HSTS`, `X-Frame-Options`
6. **File Permissions**: `chmod 600 .env`
7. **Demo Data**: `FERRUMGATE_SEED_DEMO_DATA=false`

## Quota Management

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/internal/v1/provider-accounts/{id}/quota` | Get latest quota snapshot |
| `POST` | `/internal/v1/provider-accounts/{id}/quota/probe` | Trigger quota probe |

Note: No automatic scheduled inspection — all probes are manually triggered via API.

## Further Reading

- `docs/architecture.md` — System design, Wave 1 review, planned Batch refactoring
- `AGENTS.md` — Coding conventions and "done when" checklist
- `README.md` — Full documentation with deployment instructions
