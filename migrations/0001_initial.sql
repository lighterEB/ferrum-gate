create table if not exists tenants (
  id uuid primary key,
  slug text not null unique,
  name text not null,
  suspended boolean not null default false,
  created_at timestamptz not null default now()
);

create table if not exists tenant_api_keys (
  id uuid primary key,
  tenant_id uuid not null references tenants(id),
  prefix text not null,
  label text not null,
  status text not null,
  secret_hash text not null,
  created_at timestamptz not null default now(),
  last_used_at timestamptz
);

create table if not exists tenant_management_tokens (
  id uuid primary key,
  tenant_id uuid not null references tenants(id),
  subject text not null,
  token_hash text not null unique,
  created_at timestamptz not null default now()
);

create table if not exists provider_accounts (
  id uuid primary key,
  provider text not null,
  credential_kind text not null,
  payload_version text not null,
  state text not null,
  external_account_id text not null,
  redacted_display text,
  plan_type text,
  metadata jsonb not null default '{}'::jsonb,
  labels jsonb not null default '[]'::jsonb,
  tags jsonb not null default '{}'::jsonb,
  capabilities jsonb not null default '[]'::jsonb,
  expires_at timestamptz,
  last_validated_at timestamptz,
  created_at timestamptz not null default now()
);

create table if not exists provider_account_secret_versions (
  id uuid primary key,
  provider_account_id uuid not null references provider_accounts(id),
  cipher_text bytea not null,
  key_version text not null,
  created_at timestamptz not null default now()
);

create table if not exists route_groups (
  id uuid primary key,
  slug text not null unique,
  public_model text not null,
  provider_kind text not null,
  upstream_model text not null,
  created_at timestamptz not null default now()
);

create table if not exists route_group_bindings (
  id uuid primary key,
  route_group_id uuid not null references route_groups(id),
  provider_account_id uuid not null references provider_accounts(id),
  weight integer not null,
  max_in_flight integer not null,
  created_at timestamptz not null default now()
);

create table if not exists role_bindings (
  id uuid primary key,
  subject text not null,
  role text not null,
  scope jsonb not null,
  created_at timestamptz not null default now()
);

create table if not exists service_accounts (
  id uuid primary key,
  subject text not null,
  role text not null,
  token_hash text not null unique,
  scopes jsonb not null,
  created_at timestamptz not null default now()
);

create table if not exists audit_events (
  id uuid primary key,
  actor text not null,
  action text not null,
  resource text not null,
  request_id text not null,
  details jsonb not null default '{}'::jsonb,
  occurred_at timestamptz not null default now()
);

create table if not exists usage_ledger (
  id uuid primary key,
  tenant_id uuid not null references tenants(id),
  api_key_id uuid references tenant_api_keys(id),
  public_model text not null,
  provider_kind text not null,
  status_code integer not null,
  latency_ms bigint not null,
  usage jsonb not null,
  created_at timestamptz not null default now()
);

create table if not exists account_runtime (
  provider_account_id uuid primary key references provider_accounts(id),
  state text not null,
  health_score integer not null default 100,
  cooldown_until timestamptz,
  circuit_open_until timestamptz,
  in_flight integer not null default 0,
  max_in_flight integer not null default 8,
  last_used_at timestamptz
);
