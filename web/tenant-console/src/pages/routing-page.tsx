import { useTranslation } from "react-i18next";

import { PageHeader } from "@/components/page-header";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "@/components/ui/card";
import {
	Table,
	TableBody,
	TableCell,
	TableHead,
	TableHeader,
	TableRow,
} from "@/components/ui/table";
import { useRoutingOverview } from "@/hooks/use-control-plane-queries";
import { useSessionCredentials } from "@/hooks/use-session-credentials";

export function RoutingPage() {
	const { t } = useTranslation();
	const { credentials } = useSessionCredentials();
	const routingOverviewQuery = useRoutingOverview(credentials);
	const routeGroups = routingOverviewQuery.data?.route_groups ?? [];

	return (
		<div className="space-y-6">
			<PageHeader
				title={t("routing.title")}
				description={t("routing.description")}
			/>
			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("routing.title")}</CardTitle>
					<CardDescription>{t("routing.summary")}</CardDescription>
				</CardHeader>
				<CardContent>
					{routeGroups.length > 0 ? (
						<Table>
							<TableHeader>
								<TableRow>
									<TableHead>{t("routing.columns.publicModel")}</TableHead>
									<TableHead>{t("routing.columns.provider")}</TableHead>
									<TableHead>{t("routing.columns.upstreamModel")}</TableHead>
									<TableHead>{t("routing.columns.bindings")}</TableHead>
								</TableRow>
							</TableHeader>
							<TableBody>
								{routeGroups.map((routeGroup) => (
									<TableRow key={routeGroup.id}>
										<TableCell>{routeGroup.public_model}</TableCell>
										<TableCell>{routeGroup.provider_kind}</TableCell>
										<TableCell>{routeGroup.upstream_model}</TableCell>
										<TableCell>{routeGroup.binding_count}</TableCell>
									</TableRow>
								))}
							</TableBody>
						</Table>
					) : (
						<p className="text-sm text-muted-foreground">
							{t("routing.empty")}
						</p>
					)}
				</CardContent>
			</Card>
		</div>
	);
}
