alter table tenant_api_keys add column if not exists expires_at timestamptz;
