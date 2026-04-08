# FerrumGate Architecture

## Data Plane

The public gateway stays protocol-specific only at the HTTP edge. Requests are translated into canonical inference requests and then routed through the shared scheduler and provider registry.

```text
OpenAI HTTP -> protocol-core -> scheduler -> provider-core -> provider adapter
```

This keeps future Anthropic support isolated to a new frontend module.

## Aggregated Routing Target

FerrumGate is evolving toward an **aggregated routing gateway**:

- users keep one FerrumGate API key
- the current public ingress can stay OpenAI-compatible
- the `public model` determines the upstream provider/backend/account pool
- provider-specific credential semantics stay behind provider resolvers/backends

Target shape:

```text
OpenAI-compatible ingress
  -> canonical inference request
  -> route resolver (public model -> provider/backend/pool/upstream model)
  -> execution engine (candidate selection, retry, outcome recording)
  -> provider backend (provider-specific payload + parsing)
  -> canonical result
  -> OpenAI-compatible presenter
```

Examples:

- `codex/gpt-5.2` -> Codex provider/account pool
- `opus-4.5` -> Anthropic provider/account pool
- `gemini-2.5-pro` -> Gemini provider/account pool

## Phase 5 Runtime Additions

Phase 5 extends the initial aggregated-routing seams with:

- ordered **route fallback chains** per primary route
- explicit **fallback-eligible outcomes** (`rate_limited`, `upstream_failure`, `transport_failure`, `quota_exhausted`)
- a first **do-not-fallback** policy for `invalid_credentials`
- Anthropic **streaming chat parity** through the OpenAI-compatible ingress

Current Phase 5 behavior:

```text
public model
  -> primary route
  -> same-route account retry (up to 3 attempts)
  -> fallback route chain (when outcome is fallback-eligible)
  -> OpenAI-compatible response/SSE
```

## Current Runtime Call Chain

Wave 1 of the gateway/codex refactor starts by documenting the current runtime
before responsibilities move into smaller modules.

### `POST /v1/chat/completions`

1. `apps/gateway-http/src/routes/chat.rs` authenticates the gateway API key.
2. The handler converts the OpenAI chat payload into
   `protocol_core::InferenceRequest`.
3. `apps/gateway-http/src/core/route_resolver.rs` resolves the `public model`
   to a concrete provider route.
4. `apps/gateway-http/src/core/execution_engine.rs` selects provider account
   candidates, applies same-route retry policy, and may advance to the next
   configured fallback route when the failure is fallback-eligible.
5. `provider_core::ProviderRegistry` resolves the target provider backend.
6. `crates/provider-openai-codex/src/lib.rs` / `crates/provider-anthropic/src/lib.rs`
   normalize the request for the selected upstream surface.
7. `crates/provider-openai-codex/src/lib.rs` decides whether the upstream
   target is the standard OpenAI-compatible surface or the ChatGPT Codex
   surface.
8. For ChatGPT Codex targets, chat requests may execute through the upstream
   `responses` API while the gateway still returns OpenAI-compatible chat JSON
   or SSE chunks.
9. The gateway shapes the provider result back into OpenAI wire format, records
   request usage, and marks the scheduler outcome.

### `POST /v1/responses`

1. `apps/gateway-http/src/routes/responses.rs` authenticates the gateway API
   key.
2. The handler converts the OpenAI `responses` payload into
   `protocol_core::InferenceRequest`.
3. `apps/gateway-http/src/core/route_resolver.rs` resolves the public model.
4. `apps/gateway-http/src/core/execution_engine.rs` selects candidates and
   dispatches to the provider backend.
5. The selected provider backend normalizes the request for the selected
   upstream endpoint, performs the HTTP request, and parses either JSON or SSE
   output.
6. The gateway converts the provider result back into OpenAI-compatible
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
  storage-backed route resolution and candidate selection directly in the
  current persistence layer, even though ingress now talks through explicit
  seams.
- Execution now has a first gateway-owned seam (`ExecutionEngine`), but the
  provider trait still accepts `InferenceRequest` and returns
  `InferenceResponse` / `InferenceStreamEvent` directly.
- Route fallback is now modeled explicitly, but route/provider-level penalty
  memory is still intentionally light; account runtime state remains the
  authoritative health source.

## Wave 1 Refactor Map

The kickoff plans under `.omx/plans/` define the first structural extraction
wave:

| Batch | Goal | Current status in this tree |
| --- | --- | --- |
| 0 | Guardrails, current-state docs, parity coverage | Tests already cover key gateway/provider parity paths; this document and the README now describe the current call chain explicitly. |
| 1 | Ingress extraction (`routes/`, `middleware/`, `openai_http.rs`) | Extracted. `gateway-http` now exposes a thinner ingress package. |
| 2 | Routing extraction (`ResolvedRoute`, `RouteResolver`) | Introduced in `apps/gateway-http/src/core/route_resolver.rs`; storage remains the route source of truth. |
| 3 | Execution contracts (`ExecutionRequest`, `ExecutionEvent`, `ExecutionResult`, `ProviderExecutor`, `ProtocolAdapter`) | First `ExecutionEngine` seam is present; provider trait separation is still incomplete. |

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
- Fallback model: `primary route group -> ordered fallback route groups`
- Route groups and bindings are auto-derived from validated `provider account capabilities`
- Manual route-group and binding APIs remain available only as advanced overrides
- State machine: `pending_validation`, `active`, `cooling`, `draining`, `quota_exhausted`, `invalid_credentials`, `disabled`
- Runtime policy: health first, weight second, least-recently-used third

## Surfaces

### Public Gateway

- Authenticates gateway API keys
- Resolves public model to route group
- Selects a provider account candidate
- Applies same-route retry and optional cross-route fallback
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
