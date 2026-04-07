import { Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";

import { DashboardMetric } from "@/components/dashboard-metric";
import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "@/components/ui/card";
import { useProviderAccounts } from "@/hooks/use-control-plane-queries";
import { useGatewayHealth } from "@/hooks/use-gateway-queries";
import { useInvalidateWorkspace } from "@/hooks/use-invalidate-workspace";
import { useSessionCredentials } from "@/hooks/use-session-credentials";
import {
	useTenantApiKeys,
	useTenantMe,
	useTenantModels,
} from "@/hooks/use-tenant-queries";
import { cn } from "@/lib/utils";

function endpointHost(url: string | null | undefined, fallback: string) {
	if (!url) {
		return fallback;
	}

	try {
		return new URL(url).host;
	} catch {
		return url;
	}
}

export function DashboardPage() {
	const { t } = useTranslation();
	const { credentials } = useSessionCredentials();
	const invalidateWorkspace = useInvalidateWorkspace();
	const tenantQuery = useTenantMe(credentials);
	const accountsQuery = useProviderAccounts(credentials);
	const apiKeysQuery = useTenantApiKeys(credentials);
	const modelsQuery = useTenantModels(credentials);
	const gatewayHealthQuery = useGatewayHealth(credentials?.gatewayBaseUrl);

	const accounts = accountsQuery.data ?? [];
	const activeApiKeys =
		apiKeysQuery.data?.filter((apiKey) => apiKey.status === "active").length ??
		0;
	const healthyAccounts = accounts.filter(
		(account) => account.state === "active",
	).length;
	const exceptionAccounts = accounts.length - healthyAccounts;
	const tenantModels = modelsQuery.data ?? [];
	const gatewayStatus =
		gatewayHealthQuery.data?.status === "ok" ? "ok" : "offline";

	return (
		<div className="space-y-6">
			<PageHeader
				title={t("dashboard.title")}
				description={t("dashboard.description")}
				actions={
					<Button
						variant="outline"
						onClick={async () => {
							await invalidateWorkspace();
						}}
					>
						{t("dashboard.quickActions.refresh")}
					</Button>
				}
			/>

			<div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
				<DashboardMetric
					label={t("dashboard.metrics.accounts")}
					value={String(accounts.length)}
					hint={t("dashboard.hints.accounts")}
				/>
				<DashboardMetric
					label={t("dashboard.metrics.healthy")}
					value={String(healthyAccounts)}
					hint={t("dashboard.hints.healthy")}
					tone="success"
				/>
				<DashboardMetric
					label={t("dashboard.metrics.exceptions")}
					value={String(exceptionAccounts)}
					hint={t("dashboard.hints.exceptions")}
					tone="warning"
				/>
				<DashboardMetric
					label={t("dashboard.metrics.apiKeys")}
					value={String(activeApiKeys)}
					hint={t("dashboard.hints.apiKeys")}
				/>
				<DashboardMetric
					label={t("dashboard.metrics.models")}
					value={String(tenantModels.length)}
					hint={t("dashboard.hints.models")}
				/>
				<DashboardMetric
					label={t("dashboard.metrics.gateway")}
					value={
						gatewayStatus === "ok" ? t("status.online") : t("status.offline")
					}
					hint={
						gatewayStatus === "ok"
							? t("dashboard.hints.gatewayUp")
							: t("dashboard.hints.gatewayDown")
					}
					tone={gatewayStatus === "ok" ? "success" : "warning"}
				/>
			</div>

			<div className="grid gap-4 xl:grid-cols-[1.2fr_0.8fr]">
				<Card className="border-border/70 bg-card/90">
					<CardHeader>
						<CardTitle>{t("dashboard.connections.title")}</CardTitle>
						<CardDescription>
							{tenantQuery.data?.name ?? t("app.title")}
						</CardDescription>
					</CardHeader>
					<CardContent className="grid gap-3 md:grid-cols-3">
						<div className="rounded-lg border border-border/70 bg-background/70 p-4">
							<p className="text-xs font-medium tracking-[0.14em] uppercase text-muted-foreground">
								{t("dashboard.connections.tenant")}
							</p>
							<p className="mt-2 text-sm text-foreground">
								{endpointHost(
									credentials?.tenantBaseUrl,
									t("dashboard.connections.unavailable"),
								)}
							</p>
						</div>
						<div className="rounded-lg border border-border/70 bg-background/70 p-4">
							<p className="text-xs font-medium tracking-[0.14em] uppercase text-muted-foreground">
								{t("dashboard.connections.control")}
							</p>
							<p className="mt-2 text-sm text-foreground">
								{endpointHost(
									credentials?.controlPlaneBaseUrl,
									t("dashboard.connections.unavailable"),
								)}
							</p>
						</div>
						<div className="rounded-lg border border-border/70 bg-background/70 p-4">
							<p className="text-xs font-medium tracking-[0.14em] uppercase text-muted-foreground">
								{t("dashboard.connections.gateway")}
							</p>
							<div className="mt-2 flex items-center gap-3">
								<p className="text-sm text-foreground">
									{endpointHost(
										credentials?.gatewayBaseUrl,
										t("dashboard.connections.unavailable"),
									)}
								</p>
								<StatusBadge status={gatewayStatus} />
							</div>
						</div>
					</CardContent>
				</Card>

				<Card className="border-border/70 bg-card/90">
					<CardHeader>
						<CardTitle>{t("dashboard.quickActions.title")}</CardTitle>
						<CardDescription>{t("app.subtitle")}</CardDescription>
					</CardHeader>
					<CardContent className="grid gap-3 sm:grid-cols-3 xl:grid-cols-1">
						<Link
							to="/accounts"
							className={cn(
								buttonVariants({ variant: "outline" }),
								"justify-start",
							)}
						>
							{t("dashboard.quickActions.accounts")}
						</Link>
						<Link
							to="/api-keys"
							className={cn(
								buttonVariants({ variant: "outline" }),
								"justify-start",
							)}
						>
							{t("dashboard.quickActions.apiKeys")}
						</Link>
					</CardContent>
				</Card>
			</div>

			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("dashboard.modelsSection.title")}</CardTitle>
					<CardDescription>
						{tenantModels.length > 0
							? t("dashboard.modelsSection.count", {
									count: tenantModels.length,
								})
							: t("dashboard.modelsSection.empty")}
					</CardDescription>
				</CardHeader>
				<CardContent>
					{tenantModels.length > 0 ? (
						<div className="flex flex-wrap gap-2">
							{tenantModels.map((model) => (
								<Badge
									key={`${model.provider_kind}-${model.id}`}
									variant="outline"
									className="h-auto items-start gap-2 rounded-lg px-3 py-2"
								>
									<span className="font-medium text-foreground">
										{model.id}
									</span>
									<span className="text-muted-foreground">
										{model.provider_kind}
									</span>
								</Badge>
							))}
						</div>
					) : (
						<p className="text-sm text-muted-foreground">
							{t("dashboard.modelsSection.empty")}
						</p>
					)}
				</CardContent>
			</Card>
		</div>
	);
}
