import { useTranslation } from "react-i18next";

import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
	Table,
	TableBody,
	TableCell,
	TableHead,
	TableHeader,
	TableRow,
} from "@/components/ui/table";
import { useAlerts } from "@/hooks/use-control-plane-queries";
import { useSessionCredentials } from "@/hooks/use-session-credentials";
import { formatDateTime } from "@/lib/format";

export function AlertsPage() {
	const { t, i18n } = useTranslation();
	const { credentials } = useSessionCredentials();
	const alertsQuery = useAlerts(credentials);
	const alerts = alertsQuery.data ?? [];

	return (
		<div className="space-y-6">
			<PageHeader
				title={t("alerts.title")}
				description={t("alerts.description")}
			/>
			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("alerts.title")}</CardTitle>
				</CardHeader>
				<CardContent>
					{alerts.length > 0 ? (
						<Table>
							<TableHeader>
								<TableRow>
									<TableHead>{t("alerts.columns.kind")}</TableHead>
									<TableHead>{t("alerts.columns.severity")}</TableHead>
									<TableHead>{t("alerts.columns.resource")}</TableHead>
									<TableHead>{t("alerts.columns.message")}</TableHead>
									<TableHead>{t("alerts.columns.time")}</TableHead>
								</TableRow>
							</TableHeader>
							<TableBody>
								{alerts.map((alert) => (
									<TableRow key={alert.id}>
										<TableCell>{alert.kind}</TableCell>
										<TableCell>
											<StatusBadge
												status={
													alert.severity === "critical"
														? "unhealthy"
														: "cooling"
												}
											/>
										</TableCell>
										<TableCell>{alert.resource}</TableCell>
										<TableCell className="whitespace-normal">
											{alert.message}
										</TableCell>
										<TableCell>
											{formatDateTime(
												alert.occurred_at,
												i18n.language,
												t("common.never"),
											)}
										</TableCell>
									</TableRow>
								))}
							</TableBody>
						</Table>
					) : (
						<p className="text-sm text-muted-foreground">{t("alerts.empty")}</p>
					)}
				</CardContent>
			</Card>
		</div>
	);
}
