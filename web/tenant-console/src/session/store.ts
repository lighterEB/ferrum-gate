import { useSyncExternalStore } from "react";

import {
	getDefaultControlPlaneBaseUrl,
	getDefaultGatewayBaseUrl,
} from "@/lib/env";

export const SESSION_STORAGE_KEYS = {
	baseUrl: "fg.tenant.baseUrl",
	token: "fg.tenant.token",
	controlPlaneBaseUrl: "fg.control.baseUrl",
	controlPlaneToken: "fg.control.token",
	gatewayBaseUrl: "fg.gateway.baseUrl",
} as const;

export type SessionSnapshot = {
	baseUrl: string | null;
	token: string | null;
	controlPlaneBaseUrl: string | null;
	controlPlaneToken: string | null;
	gatewayBaseUrl: string | null;
	isConnected: boolean;
	hasControlPlaneAccess: boolean;
};

type SessionInput = {
	baseUrl: string;
	token: string;
	controlPlaneBaseUrl?: string;
	controlPlaneToken?: string;
	gatewayBaseUrl?: string;
};

const listeners = new Set<() => void>();

let snapshot: SessionSnapshot = {
	baseUrl: null,
	token: null,
	controlPlaneBaseUrl: null,
	controlPlaneToken: null,
	gatewayBaseUrl: null,
	isConnected: false,
	hasControlPlaneAccess: false,
};

function canUseStorage() {
	return (
		typeof window !== "undefined" &&
		typeof window.sessionStorage !== "undefined"
	);
}

function sanitizeBaseUrl(baseUrl: string) {
	return baseUrl.trim().replace(/\/+$/, "");
}

function readStorage(key: string) {
	if (!canUseStorage()) {
		return null;
	}

	return window.sessionStorage.getItem(key);
}

function writeStorage(key: string, value: string) {
	if (!canUseStorage()) {
		return;
	}

	window.sessionStorage.setItem(key, value);
}

function removeStorage(key: string) {
	if (!canUseStorage()) {
		return;
	}

	window.sessionStorage.removeItem(key);
}

function readSnapshot(): SessionSnapshot {
	const baseUrl = readStorage(SESSION_STORAGE_KEYS.baseUrl);
	const token = readStorage(SESSION_STORAGE_KEYS.token);
	const controlPlaneBaseUrl = readStorage(
		SESSION_STORAGE_KEYS.controlPlaneBaseUrl,
	);
	const controlPlaneToken = readStorage(SESSION_STORAGE_KEYS.controlPlaneToken);
	const gatewayBaseUrl = readStorage(SESSION_STORAGE_KEYS.gatewayBaseUrl);

	return {
		baseUrl,
		token,
		controlPlaneBaseUrl,
		controlPlaneToken,
		gatewayBaseUrl,
		isConnected: baseUrl !== null && Boolean(token),
		hasControlPlaneAccess:
			controlPlaneBaseUrl !== null && Boolean(controlPlaneToken),
	};
}

function emitChange() {
	snapshot = readSnapshot();
	for (const listener of listeners) {
		listener();
	}
}

export function hydrateSession() {
	snapshot = readSnapshot();
	return snapshot;
}

export function getSessionSnapshot() {
	return snapshot;
}

export function isConnected() {
	return snapshot.isConnected;
}

export function connectSession({
	baseUrl,
	token,
	controlPlaneBaseUrl,
	controlPlaneToken,
	gatewayBaseUrl,
}: SessionInput) {
	const sanitizedBaseUrl = sanitizeBaseUrl(baseUrl);
	const sanitizedToken = token.trim();
	const sanitizedControlPlaneBaseUrl = sanitizeBaseUrl(
		controlPlaneBaseUrlOrDefault(baseUrl, controlPlaneBaseUrl),
	);
	const sanitizedControlPlaneToken = controlPlaneToken?.trim() ?? "";
	const sanitizedGatewayBaseUrl = sanitizeBaseUrl(
		gatewayBaseUrlOrDefault(baseUrl, gatewayBaseUrl),
	);

	writeStorage(SESSION_STORAGE_KEYS.baseUrl, sanitizedBaseUrl);
	writeStorage(SESSION_STORAGE_KEYS.token, sanitizedToken);
	if (sanitizedControlPlaneBaseUrl) {
		writeStorage(
			SESSION_STORAGE_KEYS.controlPlaneBaseUrl,
			sanitizedControlPlaneBaseUrl,
		);
	} else if (sanitizedControlPlaneToken) {
		writeStorage(
			SESSION_STORAGE_KEYS.controlPlaneBaseUrl,
			sanitizedControlPlaneBaseUrl,
		);
	} else {
		removeStorage(SESSION_STORAGE_KEYS.controlPlaneBaseUrl);
	}
	if (sanitizedControlPlaneToken) {
		writeStorage(
			SESSION_STORAGE_KEYS.controlPlaneToken,
			sanitizedControlPlaneToken,
		);
	} else {
		removeStorage(SESSION_STORAGE_KEYS.controlPlaneToken);
	}
	if (sanitizedGatewayBaseUrl) {
		writeStorage(SESSION_STORAGE_KEYS.gatewayBaseUrl, sanitizedGatewayBaseUrl);
	} else {
		removeStorage(SESSION_STORAGE_KEYS.gatewayBaseUrl);
	}
	emitChange();
	return snapshot;
}

export function disconnectSession() {
	removeStorage(SESSION_STORAGE_KEYS.baseUrl);
	removeStorage(SESSION_STORAGE_KEYS.token);
	removeStorage(SESSION_STORAGE_KEYS.controlPlaneBaseUrl);
	removeStorage(SESSION_STORAGE_KEYS.controlPlaneToken);
	removeStorage(SESSION_STORAGE_KEYS.gatewayBaseUrl);
	emitChange();
	return snapshot;
}

export function subscribeSession(listener: () => void) {
	listeners.add(listener);
	return () => listeners.delete(listener);
}

export function useSession() {
	return useSyncExternalStore(
		subscribeSession,
		getSessionSnapshot,
		getSessionSnapshot,
	);
}

if (typeof window !== "undefined") {
	hydrateSession();
}

function controlPlaneBaseUrlOrDefault(
	baseUrl: string,
	controlPlaneBaseUrl?: string,
) {
	if (controlPlaneBaseUrl?.trim()) {
		return controlPlaneBaseUrl.trim();
	}

	if (getDefaultControlPlaneBaseUrl()) {
		return getDefaultControlPlaneBaseUrl();
	}

	try {
		const url = new URL(baseUrl);
		url.port = "3007";
		return url.toString().replace(/\/+$/, "");
	} catch {
		return baseUrl;
	}
}

function gatewayBaseUrlOrDefault(baseUrl: string, gatewayBaseUrl?: string) {
	if (gatewayBaseUrl?.trim()) {
		return gatewayBaseUrl.trim();
	}

	if (getDefaultGatewayBaseUrl()) {
		return getDefaultGatewayBaseUrl();
	}

	try {
		const url = new URL(baseUrl);
		url.port = "3005";
		url.pathname = "/v1";
		return url.toString().replace(/\/+$/, "");
	} catch {
		return baseUrl;
	}
}
