import { type SessionSnapshot, useSession } from "@/session/store";

export type SessionCredentials = {
	tenantBaseUrl: string;
	tenantToken: string;
	controlPlaneBaseUrl: string | null;
	controlPlaneToken: string | null;
	gatewayBaseUrl: string | null;
};

export function getSessionCredentials(
	session: SessionSnapshot,
): SessionCredentials | null {
	if (!session.baseUrl || !session.token) {
		return null;
	}

	return {
		tenantBaseUrl: session.baseUrl,
		tenantToken: session.token,
		controlPlaneBaseUrl: session.controlPlaneBaseUrl,
		controlPlaneToken: session.controlPlaneToken,
		gatewayBaseUrl: session.gatewayBaseUrl,
	};
}

export function useSessionCredentials() {
	const session = useSession();

	return {
		session,
		credentials: getSessionCredentials(session),
	};
}
