import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { PageHeader } from "@/components/page-header";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "@/components/ui/card";
import { useSessionCredentials } from "@/hooks/use-session-credentials";
import { useTenantModels } from "@/hooks/use-tenant-queries";

async function copySnippet(value: string) {
	await navigator.clipboard.writeText(value);
}

export function DocsPage() {
	const { t } = useTranslation();
	const { credentials } = useSessionCredentials();
	const tenantModelsQuery = useTenantModels(credentials);
	const models = tenantModelsQuery.data ?? [];
	const gatewayBaseUrl = credentials?.gatewayBaseUrl ?? "";
	const exampleModel = models[0]?.id ?? "gpt-4.1-mini";
	const curlModels = `curl ${gatewayBaseUrl}/models \\\n+  -H "Authorization: Bearer fgk_your_secret"`;
	const curlChat = `curl ${gatewayBaseUrl}/chat/completions \\\n+  -H "Authorization: Bearer fgk_your_secret" \\\n+  -H "Content-Type: application/json" \\\n+  -d '{\n+    "model": "${exampleModel}",\n+    "messages": [{"role":"user","content":"hello"}]\n+  }'`;

	return (
		<div className="space-y-6">
			<PageHeader title={t("docs.title")} description={t("docs.description")} />
			<div className="grid gap-4 xl:grid-cols-[0.95fr_1.05fr]">
				<Card className="border-border/70 bg-card/90">
					<CardHeader>
						<CardTitle>{t("docs.title")}</CardTitle>
						<CardDescription>{t("docs.description")}</CardDescription>
					</CardHeader>
					<CardContent className="space-y-4">
						<ul className="space-y-3 text-sm leading-6 text-muted-foreground">
							<li>{t("docs.steps.one")}</li>
							<li>{t("docs.steps.two")}</li>
							<li>{t("docs.steps.three")}</li>
						</ul>
						<div className="space-y-3 rounded-lg border border-border/70 bg-background/70 p-4">
							<div>
								<p className="text-xs font-medium tracking-[0.14em] uppercase text-muted-foreground">
									{t("docs.baseUrl")}
								</p>
								<p className="mt-2 text-sm text-foreground">{gatewayBaseUrl}</p>
							</div>
							<div>
								<p className="text-xs font-medium tracking-[0.14em] uppercase text-muted-foreground">
									{t("docs.apiKey")}
								</p>
								<p className="mt-2 text-sm text-foreground">fgk_...</p>
							</div>
						</div>
						<div className="rounded-lg border border-amber-400/20 bg-amber-500/8 p-4 text-sm leading-6 text-amber-100">
							<p>{t("docs.important")}</p>
							<p className="mt-2">{t("docs.liveTip")}</p>
						</div>
					</CardContent>
				</Card>

				<div className="space-y-4">
					<Card className="border-border/70 bg-card/90">
						<CardHeader>
							<CardTitle>{t("docs.models")}</CardTitle>
						</CardHeader>
						<CardContent>
							{models.length > 0 ? (
								<div className="flex flex-wrap gap-2">
									{models.map((model) => (
										<Badge key={model.id} variant="outline">
											{model.id}
										</Badge>
									))}
								</div>
							) : (
								<p className="text-sm text-muted-foreground">
									{t("docs.empty")}
								</p>
							)}
						</CardContent>
					</Card>
					<Card className="border-border/70 bg-card/90">
						<CardHeader>
							<CardTitle>{t("docs.curlModels")}</CardTitle>
						</CardHeader>
						<CardContent className="space-y-3">
							<pre className="overflow-x-auto rounded-lg border border-border/70 bg-background/70 p-4 text-sm text-foreground">
								{curlModels}
							</pre>
							<Button
								variant="outline"
								onClick={async () => {
									await copySnippet(curlModels);
									toast.success(t("common.copied"));
								}}
							>
								{t("common.copy")}
							</Button>
						</CardContent>
					</Card>
					<Card className="border-border/70 bg-card/90">
						<CardHeader>
							<CardTitle>{t("docs.curlChat")}</CardTitle>
						</CardHeader>
						<CardContent className="space-y-3">
							<pre className="overflow-x-auto rounded-lg border border-border/70 bg-background/70 p-4 text-sm text-foreground">
								{curlChat}
							</pre>
							<Button
								variant="outline"
								onClick={async () => {
									await copySnippet(curlChat);
									toast.success(t("common.copied"));
								}}
							>
								{t("common.copy")}
							</Button>
						</CardContent>
					</Card>
				</div>
			</div>
		</div>
	);
}
