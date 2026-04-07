import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import type { SessionCredentials } from "@/hooks/use-session-credentials";
import {
	getRoutingOverview,
	listAlertsOutbox,
	listAuditEvents,
	listProviderAccounts,
	probeProviderAccount,
	probeProviderAccountQuota,
	refreshProviderAccount,
	setProviderAccountState,
} from "@/lib/control-plane-api";

function requireControlPlane(credentials: SessionCredentials | null) {
	if (
		!credentials ||
		credentials.controlPlaneBaseUrl === null ||
		!credentials.controlPlaneToken
	) {
		throw new Error("Control plane is not connected");
	}

	return {
		baseUrl: credentials.controlPlaneBaseUrl ?? "",
		token: credentials.controlPlaneToken,
	};
}

const providerAccountsKey = (baseUrl: string | null | undefined) =>
	["provider-accounts", baseUrl] as const;
const alertsKey = (baseUrl: string | null | undefined) =>
	["alerts", baseUrl] as const;
const auditEventsKey = (baseUrl: string | null | undefined) =>
	["audit-events", baseUrl] as const;
const routingOverviewKey = (baseUrl: string | null | undefined) =>
	["routing-overview", baseUrl] as const;

export function useProviderAccounts(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: providerAccountsKey(credentials?.controlPlaneBaseUrl),
		enabled: Boolean(
			credentials &&
				credentials.controlPlaneBaseUrl !== null &&
				credentials.controlPlaneToken,
		),
		queryFn: async () => {
			const resolved = requireControlPlane(credentials);
			return listProviderAccounts(resolved.baseUrl, resolved.token);
		},
	});
}

export function useAccountAction(credentials: SessionCredentials | null) {
	const queryClient = useQueryClient();

	return useMutation({
		mutationFn: async ({
			accountId,
			action,
		}: {
			accountId: string;
			action: "probe" | "quota" | "refresh" | "enable" | "disable" | "drain";
		}) => {
			const resolved = requireControlPlane(credentials);

			switch (action) {
				case "probe":
					return probeProviderAccount(
						resolved.baseUrl,
						resolved.token,
						accountId,
					);
				case "quota":
					return probeProviderAccountQuota(
						resolved.baseUrl,
						resolved.token,
						accountId,
					);
				case "refresh":
					return refreshProviderAccount(
						resolved.baseUrl,
						resolved.token,
						accountId,
					);
				case "enable":
				case "disable":
				case "drain":
					return setProviderAccountState(
						resolved.baseUrl,
						resolved.token,
						accountId,
						action,
					);
			}
		},
		onSuccess: async () => {
			await Promise.all([
				queryClient.invalidateQueries({
					queryKey: providerAccountsKey(credentials?.controlPlaneBaseUrl),
				}),
				queryClient.invalidateQueries({
					queryKey: alertsKey(credentials?.controlPlaneBaseUrl),
				}),
				queryClient.invalidateQueries({
					queryKey: auditEventsKey(credentials?.controlPlaneBaseUrl),
				}),
				queryClient.invalidateQueries({
					queryKey: routingOverviewKey(credentials?.controlPlaneBaseUrl),
				}),
			]);
		},
	});
}

export function useAlerts(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: alertsKey(credentials?.controlPlaneBaseUrl),
		enabled: Boolean(
			credentials &&
				credentials.controlPlaneBaseUrl !== null &&
				credentials.controlPlaneToken,
		),
		queryFn: async () => {
			const resolved = requireControlPlane(credentials);
			return listAlertsOutbox(resolved.baseUrl, resolved.token);
		},
	});
}

export function useAuditEvents(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: auditEventsKey(credentials?.controlPlaneBaseUrl),
		enabled: Boolean(
			credentials &&
				credentials.controlPlaneBaseUrl !== null &&
				credentials.controlPlaneToken,
		),
		queryFn: async () => {
			const resolved = requireControlPlane(credentials);
			return listAuditEvents(resolved.baseUrl, resolved.token);
		},
	});
}

export function useRoutingOverview(credentials: SessionCredentials | null) {
	return useQuery({
		queryKey: routingOverviewKey(credentials?.controlPlaneBaseUrl),
		enabled: Boolean(
			credentials &&
				credentials.controlPlaneBaseUrl !== null &&
				credentials.controlPlaneToken,
		),
		queryFn: async () => {
			const resolved = requireControlPlane(credentials);
			return getRoutingOverview(resolved.baseUrl, resolved.token);
		},
	});
}
