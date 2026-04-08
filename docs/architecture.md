# FerrumGate Architecture

## Data Plane

The public gateway stays protocol-specific only at the HTTP edge. Requests are translated into canonical inference requests and then routed through the shared scheduler and provider registry.

```text
OpenAI HTTP -> protocol-core -> scheduler -> provider-core -> provider adapter
```

This keeps future Anthropic support isolated to a new frontend module.

## Current Runtime Call Chain

Wave 1 of the gateway/codex refactor starts by documenting the current runtime
before responsibilities move into smaller modules.

### `POST /v1/chat/completions`

1. `apps/gateway-http/src/lib.rs` authenticates the gateway API key.
2. The gateway asks storage for the visible model route and chooses a provider
   account candidate.
3. The handler converts the OpenAI chat payload into
   `protocol_core::InferenceRequest`.
4. `provider_core::ProviderRegistry` resolves `provider-openai-codex`.
5. `crates/provider-openai-codex/src/lib.rs` decides whether the upstream
   target is the standard OpenAI-compatible surface or the ChatGPT Codex
   surface.
6. For ChatGPT Codex targets, chat requests may execute through the upstream
   `responses` API while the gateway still returns OpenAI-compatible chat JSON
   or SSE chunks.
7. The gateway shapes the provider result back into OpenAI wire format, records
   request usage, and marks the scheduler outcome.

### `POST /v1/responses`

1. `apps/gateway-http/src/lib.rs` authenticates the gateway API key.
2. The gateway resolves the public model and chooses a provider account.
3. The handler converts the OpenAI `responses` payload into
   `protocol_core::InferenceRequest`.
4. `provider-openai-codex` normalizes the request for the selected upstream
   endpoint, performs the HTTP request, and parses either JSON or SSE output.
5. The gateway converts the provider result back into OpenAI-compatible
   `responses` JSON or streaming events, then records request usage and updates
   scheduler state.

## Current Ownership Review

The current codebase is functional, but Wave 1 exists because several
boundaries are still blurred:

- `apps/gateway-http/src/lib.rs` currently owns ingress wiring, auth, route
  resolution, candidate selection, request normalization, provider invocation,
  OpenAI JSON/SSE shaping, request bookkeeping, scheduler outcome mutation, and
  a large integration-style test surface.
- `crates/provider-openai-codex/src/lib.rs` currently owns credential
  resolution, endpoint selection, request payload shaping, upstream HTTP
  transport, stream parsing, error mapping, and public-response reconstruction.
- Routing is not yet an explicit service boundary: the gateway still calls
  storage-backed route resolution and candidate selection directly.
- Execution does not yet have a dedicated typed contract seam: the current
  provider trait still accepts `InferenceRequest` and returns
  `InferenceResponse` / `InferenceStreamEvent` directly.

## Wave 1 Refactor Map

The kickoff plans under `.omx/plans/` define the first structural extraction
wave:

| Batch | Goal | Current status in this tree |
| --- | --- | --- |
| 0 | Guardrails, current-state docs, parity coverage | Tests already cover key gateway/provider parity paths; this document and the README now describe the current call chain explicitly. |
| 1 | Ingress extraction (`routes/`, `middleware/`, `openai_http.rs`) | Not yet extracted; `apps/gateway-http/src/lib.rs` still contains the combined ingress surface. |
| 2 | Routing extraction (`ResolvedRoute`, `RouteResolver`) | Not yet extracted; routing still flows through direct store calls from ingress. |
| 3 | Execution contracts (`ExecutionRequest`, `ExecutionEvent`, `ExecutionResult`, `ProviderExecutor`, `ProtocolAdapter`) | Not yet introduced; later adapter/executor batches still depend on this seam. |

## Target Ownership After Wave 1

Wave 1 should leave the system with clearer layering, without changing public
wire behavior:

```text
OpenAI HTTP routes
  -> auth/request-id middleware
  -> route resolver
  -> execution contracts
  -> provider adapter/executor
  -> scheduler/store side effects behind explicit seams
```

In practical terms:

- Batch 1 should make `gateway-http` a thin ingress package.
- Batch 2 should make routing an explicit dependency instead of a hidden store
  reach-through.
- Batch 3 should freeze the execution contract shape before adapter and
  executor extraction begin.

## Account Pool

- Smallest scheduling unit: `provider account`
- Routing model: `public model -> route group -> provider account bindings`
- Route groups and bindings are auto-derived from validated `provider account capabilities`
- Manual route-group and binding APIs remain available only as advanced overrides
- State machine: `pending_validation`, `active`, `cooling`, `draining`, `quota_exhausted`, `invalid_credentials`, `disabled`
- Runtime policy: health first, weight second, least-recently-used third

## Surfaces

### Public Gateway

- Authenticates gateway API keys
- Resolves public model to route group
- Selects a provider account candidate
- Invokes provider adapter
- Returns OpenAI-compatible JSON or SSE

### Tenant API

- Uses tenant management authentication separate from gateway keys
- Lets external users manage their own gateway API keys
- Exposes tenant-scoped models, usage, limits and request history

### Control Plane

- Imports provider credentials through a provider-specific envelope
- Validates and probes accounts via the provider registry
- Auto-creates route groups and bindings for each discovered upstream model
- Exposes routing overview data for the console while keeping manual override APIs available
- Manages tenants and API access
- Enforces RBAC using roles plus resource scopes
- Records audit events for sensitive actions

## Storage Plan

- Postgres: tenants, API keys, provider accounts, secret versions, route groups, bindings, RBAC, audit events, usage ledger
- Redis: cooldown windows, circuit breaker windows, distributed leases, in-flight counters, short queue state

The current milestone keeps runtime state in memory for the demo backend while Postgres stores account runtime, quota snapshots, and the auto-derived routing graph.
