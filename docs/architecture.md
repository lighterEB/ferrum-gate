# FerrumGate Architecture

## Data Plane

The public gateway stays protocol-specific only at the HTTP edge. Requests are translated into canonical inference requests and then routed through the shared scheduler and provider registry.

```text
OpenAI HTTP -> protocol-core -> scheduler -> provider-core -> provider adapter
```

This keeps future Anthropic support isolated to a new frontend module.

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
