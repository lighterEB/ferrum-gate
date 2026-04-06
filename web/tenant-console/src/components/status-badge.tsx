import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";

type StatusBadgeProps = {
	status: string;
};

const STATUS_LABELS: Record<string, string> = {
	active: "status.active",
	revoked: "status.revoked",
	disabled: "status.disabled",
	draining: "status.draining",
	cooling: "status.cooling",
	quota_exhausted: "status.quotaExhausted",
	invalid_credentials: "status.invalidCredentials",
	healthy: "status.healthy",
	unhealthy: "status.unhealthy",
	ok: "status.online",
	offline: "status.offline",
};

function badgeVariant(status: string) {
	switch (status) {
		case "active":
		case "healthy":
		case "ok":
			return "default" as const;
		case "revoked":
		case "disabled":
		case "invalid_credentials":
		case "unhealthy":
		case "offline":
			return "destructive" as const;
		default:
			return "outline" as const;
	}
}

export function StatusBadge({ status }: StatusBadgeProps) {
	const { t } = useTranslation();
	const labelKey = STATUS_LABELS[status];

	return (
		<Badge variant={badgeVariant(status)} className="capitalize">
			{labelKey ? t(labelKey as never) : status}
		</Badge>
	);
}
