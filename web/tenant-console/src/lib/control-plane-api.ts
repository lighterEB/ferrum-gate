export type ControlPlaneErrorResponse = {
	message: string;
	kind: string;
	status_code: number;
	code?: string | null;
};

export type ProviderAccountQuotaSnapshot = {
	provider_account_id: string;
	plan_label?: string | null;
	remaining_requests_hint?: number | null;
	details: Record<string, unknown>;
	checked_at: string;
};

export type ProviderAccountRecord = {
	id: string;
	provider: string;
	credential_kind: string;
	payload_version: string;
	state: string;
	external_account_id: string;
	redacted_display?: string | null;
	plan_type?: string | null;
	metadata: Record<string, unknown>;
	labels: string[];
	tags: Record<string, string>;
	capabilities: string[];
	expires_at?: string | null;
	last_validated_at?: string | null;
	created_at: string;
	quota?: ProviderAccountQuotaSnapshot | null;
};

export type ProbeProviderAccountResult = {
	account_id: string;
	status: string;
	provider_account?: ProviderAccountRecord | null;
	error?: ControlPlaneErrorResponse | null;
};

export type AccountInspectionRecord = {
	id: string;
	provider_account_id: string;
	actor: string;
	status: "healthy" | "unhealthy";
	error_kind?: string | null;
	error_code?: string | null;
	error_message?: string | null;
	inspected_at: string;
};

export type AlertOutboxItem = {
	id: string;
	kind: string;
	severity: string;
	resource: string;
	message: string;
	occurred_at: string;
};

export type AuditEvent = {
	id: string;
	actor: string;
	action: string;
	resource: string;
	request_id: string;
	occurred_at: string;
	details: Record<string, unknown>;
};

export type RoutingOverviewItem = {
	id: string;
	slug: string;
	public_model: string;
	provider_kind: string;
	upstream_model: string;
	created_at: string;
	binding_count: number;
};

export type RoutingOverviewResponse = {
	route_groups: RoutingOverviewItem[];
	bindings_count: number;
	auto_derived: boolean;
};

export class ControlPlaneApiError extends Error {
	status: number | null;
	code: string;

	constructor(code: string, message: string, status: number | null = null) {
		super(message);
		this.name = "ControlPlaneApiError";
		this.code = code;
		this.status = status;
	}
}

function sanitizeBaseUrl(baseUrl: string) {
	return baseUrl.trim().replace(/\/+$/, "");
}

async function request<T>({
	baseUrl,
	token,
	path,
	method = "GET",
	body,
}: {
	baseUrl: string;
	token: string;
	path: string;
	method?: "GET" | "POST";
	body?: unknown;
}): Promise<T> {
	const url = `${sanitizeBaseUrl(baseUrl)}${path}`;

	let response: Response;
	try {
		response = await fetch(url, {
			method,
			headers: {
				Authorization: `Bearer ${token}`,
				...(body ? { "Content-Type": "application/json" } : {}),
			},
			body: body ? JSON.stringify(body) : undefined,
		});
	} catch (error) {
		throw new ControlPlaneApiError(
			"network",
			error instanceof Error ? error.message : "network error",
		);
	}

	const text = await response.text();
	const payload = text ? safeJsonParse(text) : null;

	if (!response.ok) {
		const message =
			extractErrorMessage(payload) ?? response.statusText ?? "request failed";
		throw new ControlPlaneApiError(
			`http_${response.status}`,
			message,
			response.status,
		);
	}

	return (payload ?? {}) as T;
}

function safeJsonParse(text: string) {
	try {
		return JSON.parse(text) as unknown;
	} catch {
		return null;
	}
}

function extractErrorMessage(payload: unknown) {
	if (!payload || typeof payload !== "object") {
		return null;
	}

	const error = (payload as { error?: { message?: unknown } }).error;
	if (!error || typeof error !== "object") {
		return null;
	}

	return typeof error.message === "string" ? error.message : null;
}

export function getControlPlaneApiErrorKey(error: unknown) {
	if (error instanceof ControlPlaneApiError) {
		if (error.code === "network") {
			return "errors.network";
		}

		if (error.status === 401) {
			return "errors.unauthorized";
		}

		if (error.status === 403) {
			return "errors.forbidden";
		}

		if (error.status === 404) {
			return "errors.notFound";
		}
	}

	return "errors.generic";
}

export async function listProviderAccounts(baseUrl: string, token: string) {
	const response = await request<{ data: ProviderAccountRecord[] }>({
		baseUrl,
		token,
		path: "/internal/v1/runtime/provider-accounts",
	});

	return response.data;
}

export async function probeProviderAccount(
	baseUrl: string,
	token: string,
	accountId: string,
) {
	return request<ProbeProviderAccountResult>({
		baseUrl,
		token,
		path: `/internal/v1/provider-accounts/${accountId}/probe`,
		method: "POST",
	});
}

export async function probeProviderAccountQuota(
	baseUrl: string,
	token: string,
	accountId: string,
) {
	return request<ProbeProviderAccountResult>({
		baseUrl,
		token,
		path: `/internal/v1/provider-accounts/${accountId}/quota/probe`,
		method: "POST",
	});
}

export async function refreshProviderAccount(
	baseUrl: string,
	token: string,
	accountId: string,
) {
	return request<ProbeProviderAccountResult>({
		baseUrl,
		token,
		path: `/internal/v1/provider-accounts/${accountId}/refresh`,
		method: "POST",
	});
}

export async function setProviderAccountState(
	baseUrl: string,
	token: string,
	accountId: string,
	action: "enable" | "disable" | "drain",
) {
	return request<ProviderAccountRecord>({
		baseUrl,
		token,
		path: `/internal/v1/provider-accounts/${accountId}/${action}`,
		method: "POST",
	});
}

export async function listProviderAccountInspections(
	baseUrl: string,
	token: string,
	accountId: string,
) {
	const response = await request<{ data: AccountInspectionRecord[] }>({
		baseUrl,
		token,
		path: `/internal/v1/provider-accounts/${accountId}/inspections`,
	});

	return response.data;
}

export async function listAlertsOutbox(baseUrl: string, token: string) {
	const response = await request<{ data: AlertOutboxItem[] }>({
		baseUrl,
		token,
		path: "/internal/v1/alerts/outbox",
	});

	return response.data;
}

export async function listAuditEvents(baseUrl: string, token: string) {
	const response = await request<{ data: AuditEvent[] }>({
		baseUrl,
		token,
		path: "/internal/v1/audit/events",
	});

	return response.data;
}

export async function getRoutingOverview(baseUrl: string, token: string) {
	return request<RoutingOverviewResponse>({
		baseUrl,
		token,
		path: "/internal/v1/routing/overview",
	});
}
