import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import type { SessionCredentials } from "@/hooks/use-session-credentials";
import {
	createTenantApiKey,
	getTenantMe,
	listTenantApiKeys,
	listTenantModels,
	revokeTenantApiKey,
	rotateTenantApiKey,
	type TenantApiKeyView,
} from "@/lib/tenant-api";

function requireCredentials(credentials: SessionCredentials | null) {
	if (!credentials) {
		throw new Error("Tenant session is not connected");
	}

	return credentials;
}

function mergeApiKey(
	current: TenantApiKeyView[] | undefined,
	nextRecord: TenantApiKeyView,
) {
	const records = current ?? [];
	const existingIndex = records.findIndex(
		(record) => record.id === nextRecord.id,
	);

	if (existingIndex === -1) {
		return [...records, nextRecord];
	}

	return records.map((record) =>
		record.id === nextRecord.id ? nextRecord : record,
	);
}

const tenantMeKey = (baseUrl: string | null | undefined) =>
	["tenant-me", baseUrl] as const;
const tenantModelsKey = (baseUrl: string | null | undefined) =>
	["tenant-models", baseUrl] as const;
const tenantApiKeysKey = (baseUrl: string | null | undefined) =>
	["tenant-api-keys", baseUrl] as const;

export function useTenantMe(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: tenantMeKey(credentials?.tenantBaseUrl),
		enabled: Boolean(credentials),
		queryFn: async () => {
			const resolved = requireCredentials(credentials);
			return getTenantMe(resolved.tenantBaseUrl, resolved.tenantToken);
		},
	});
}

export function useTenantModels(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: tenantModelsKey(credentials?.tenantBaseUrl),
		enabled: Boolean(credentials),
		queryFn: async () => {
			const resolved = requireCredentials(credentials);
			return listTenantModels(resolved.tenantBaseUrl, resolved.tenantToken);
		},
	});
}

export function useTenantApiKeys(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: tenantApiKeysKey(credentials?.tenantBaseUrl),
		enabled: Boolean(credentials),
		queryFn: async () => {
			const resolved = requireCredentials(credentials);
			return listTenantApiKeys(resolved.tenantBaseUrl, resolved.tenantToken);
		},
	});
}

export function useCreateApiKey(credentials: SessionCredentials | null) {
	const queryClient = useQueryClient();

	return useMutation({
		mutationFn: async (label: string) => {
			const resolved = requireCredentials(credentials);
			return createTenantApiKey(
				resolved.tenantBaseUrl,
				resolved.tenantToken,
				label,
			);
		},
		onSuccess: (created) => {
			queryClient.setQueryData<TenantApiKeyView[]>(
				tenantApiKeysKey(credentials?.tenantBaseUrl),
				(current) => mergeApiKey(current, created.record),
			);
		},
	});
}

export function useRotateApiKey(credentials: SessionCredentials | null) {
	const queryClient = useQueryClient();

	return useMutation({
		mutationFn: async (apiKeyId: string) => {
			const resolved = requireCredentials(credentials);
			return rotateTenantApiKey(
				resolved.tenantBaseUrl,
				resolved.tenantToken,
				apiKeyId,
			);
		},
		onSuccess: (created) => {
			queryClient.setQueryData<TenantApiKeyView[]>(
				tenantApiKeysKey(credentials?.tenantBaseUrl),
				(current) => mergeApiKey(current, created.record),
			);
		},
	});
}

export function useRevokeApiKey(credentials: SessionCredentials | null) {
	const queryClient = useQueryClient();

	return useMutation({
		mutationFn: async (apiKeyId: string) => {
			const resolved = requireCredentials(credentials);
			return revokeTenantApiKey(
				resolved.tenantBaseUrl,
				resolved.tenantToken,
				apiKeyId,
			);
		},
		onSuccess: (record) => {
			queryClient.setQueryData<TenantApiKeyView[]>(
				tenantApiKeysKey(credentials?.tenantBaseUrl),
				(current) => mergeApiKey(current, record),
			);
		},
	});
}
