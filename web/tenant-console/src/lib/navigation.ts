import type { TFunction } from "i18next";

export const consoleNavItems = [
	{ to: "/dashboard", key: "app.navigation.dashboard" },
	{ to: "/accounts", key: "app.navigation.accounts" },
	{ to: "/api-keys", key: "app.navigation.apiKeys" },
	{ to: "/routing", key: "app.navigation.routing" },
	{ to: "/alerts", key: "app.navigation.alerts" },
	{ to: "/audit", key: "app.navigation.audit" },
] as const;

export function getRouteTitle(pathname: string, t: TFunction) {
	const match =
		consoleNavItems.find((item) => pathname === item.to) ??
		consoleNavItems.find(
			(item) => item.to !== "/dashboard" && pathname.startsWith(`${item.to}/`),
		);

	return match ? t(match.key) : t("app.navigation.dashboard");
}
