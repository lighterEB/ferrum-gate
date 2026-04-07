export type Tenant = {
	id: string;
	slug: string;
	name: string;
	suspended: boolean;
	created_at: string;
};

export type TenantModel = {
	id: string;
	route_group: string;
	provider_kind: string;
	upstream_model: string;
	capabilities: string[];
};

export type TenantApiKeyStatus = "active" | "revoked";

export type TenantApiKeyView = {
	id: string;
	tenant_id: string;
	label: string;
	prefix: string;
	status: TenantApiKeyStatus;
	created_at: string;
	last_used_at: string | null;
};

export type CreatedApiKey = {
	record: TenantApiKeyView;
	secret: string;
};

export class TenantApiError extends Error {
	status: number | null;
	code: string;

	constructor(code: string, message: string, status: number | null = null) {
		super(message);
		this.name = "TenantApiError";
		this.code = code;
		this.status = status;
	}
}

type RequestOptions = {
	baseUrl: string;
	token: string;
	path: string;
	method?: "GET" | "POST";
	body?: unknown;
};

function authorizationHeader(token: string) {
	const sanitized = token.trim();
	if (!sanitized || sanitized === "__proxy_auth__") {
		return null;
	}

	return `Bearer ${sanitized}`;
}

export function sanitizeTenantApiBaseUrl(baseUrl: string) {
	return baseUrl.trim().replace(/\/+$/, "");
}

async function request<T>({
	baseUrl,
	token,
	path,
	method = "GET",
	body,
}: RequestOptions): Promise<T> {
	const url = `${sanitizeTenantApiBaseUrl(baseUrl)}${path}`;

	let response: Response;
	try {
		const bearer = authorizationHeader(token);
		response = await fetch(url, {
			method,
			headers: {
				...(bearer ? { Authorization: bearer } : {}),
				...(body ? { "Content-Type": "application/json" } : {}),
			},
			body: body ? JSON.stringify(body) : undefined,
		});
	} catch (error) {
		throw new TenantApiError(
			"network",
			error instanceof Error ? error.message : "network error",
		);
	}

	const text = await response.text();
	const payload = text ? safeJsonParse(text) : null;

	if (!response.ok) {
		const message =
			extractErrorMessage(payload) ?? response.statusText ?? "request failed";

		throw new TenantApiError(
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

export function getTenantApiErrorKey(error: unknown) {
	if (error instanceof TenantApiError) {
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

export async function getTenantMe(baseUrl: string, token: string) {
	return request<Tenant>({
		baseUrl,
		token,
		path: "/tenant/v1/me",
	});
}

export async function listTenantApiKeys(baseUrl: string, token: string) {
	const response = await request<{ data: TenantApiKeyView[] }>({
		baseUrl,
		token,
		path: "/tenant/v1/api-keys",
	});

	return response.data;
}

export async function listTenantModels(baseUrl: string, token: string) {
	const response = await request<{ data: TenantModel[] }>({
		baseUrl,
		token,
		path: "/tenant/v1/models",
	});

	return response.data;
}

export async function createTenantApiKey(
	baseUrl: string,
	token: string,
	label: string,
) {
	return request<CreatedApiKey>({
		baseUrl,
		token,
		path: "/tenant/v1/api-keys",
		method: "POST",
		body: { label },
	});
}

export async function rotateTenantApiKey(
	baseUrl: string,
	token: string,
	apiKeyId: string,
) {
	return request<CreatedApiKey>({
		baseUrl,
		token,
		path: `/tenant/v1/api-keys/${apiKeyId}/rotate`,
		method: "POST",
	});
}

export async function revokeTenantApiKey(
	baseUrl: string,
	token: string,
	apiKeyId: string,
) {
	return request<TenantApiKeyView>({
		baseUrl,
		token,
		path: `/tenant/v1/api-keys/${apiKeyId}/revoke`,
		method: "POST",
	});
}
