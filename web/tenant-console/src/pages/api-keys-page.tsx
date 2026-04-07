import {
	type ColumnDef,
	flexRender,
	getCoreRowModel,
	getSortedRowModel,
	type SortingState,
	useReactTable,
} from "@tanstack/react-table";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiKeySecretCard } from "@/components/api-key-secret-card";
import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
	Table,
	TableBody,
	TableCell,
	TableHead,
	TableHeader,
	TableRow,
} from "@/components/ui/table";
import { useSessionCredentials } from "@/hooks/use-session-credentials";
import {
	useCreateApiKey,
	useRevokeApiKey,
	useRotateApiKey,
	useTenantApiKeys,
	useTenantModels,
} from "@/hooks/use-tenant-queries";
import { formatDateTime } from "@/lib/format";
import type { TenantApiKeyView } from "@/lib/tenant-api";

type RevealedSecret = {
	label: string;
	secret: string;
	kind: "create" | "rotate";
};

async function copySnippet(value: string) {
	await navigator.clipboard.writeText(value);
}

export function ApiKeysPage() {
	const { t, i18n } = useTranslation();
	const { credentials } = useSessionCredentials();
	const apiKeysQuery = useTenantApiKeys(credentials);
	const tenantModelsQuery = useTenantModels(credentials);
	const createApiKey = useCreateApiKey(credentials);
	const rotateApiKey = useRotateApiKey(credentials);
	const revokeApiKey = useRevokeApiKey(credentials);
	const [sorting, setSorting] = useState<SortingState>([
		{ id: "created_at", desc: true },
	]);
	const [label, setLabel] = useState("");
	const [revealedSecret, setRevealedSecret] = useState<RevealedSecret | null>(
		null,
	);
	const data = apiKeysQuery.data ?? [];
	const models = tenantModelsQuery.data ?? [];
	const gatewayBaseUrl = credentials?.gatewayBaseUrl ?? "";
	const exampleModel = models[0]?.id ?? "gpt-4.1-mini";
	const curlModels = `curl ${gatewayBaseUrl}/models \\\n  -H "Authorization: Bearer fgk_your_secret"`;
	const curlChat = `curl ${gatewayBaseUrl}/chat/completions \\\n  -H "Authorization: Bearer fgk_your_secret" \\\n  -H "Content-Type: application/json" \\\n  -d '{\n    "model": "${exampleModel}",\n    "messages": [{"role":"user","content":"hello"}]\n  }'`;

	const columns: ColumnDef<TenantApiKeyView>[] = [
		{
			accessorKey: "label",
			header: t("apiKeys.columns.label"),
		},
		{
			accessorKey: "prefix",
			header: t("apiKeys.columns.prefix"),
		},
		{
			accessorKey: "status",
			header: t("apiKeys.columns.status"),
			cell: ({ row }) => <StatusBadge status={row.original.status} />,
		},
		{
			accessorKey: "created_at",
			header: t("apiKeys.columns.createdAt"),
			cell: ({ row }) =>
				formatDateTime(
					row.original.created_at,
					i18n.language,
					t("common.never"),
				),
		},
		{
			accessorKey: "last_used_at",
			header: t("apiKeys.columns.lastUsedAt"),
			cell: ({ row }) =>
				formatDateTime(
					row.original.last_used_at,
					i18n.language,
					t("common.never"),
				),
		},
		{
			id: "actions",
			enableSorting: false,
			header: t("apiKeys.columns.actions"),
			cell: ({ row }) => (
				<div className="flex items-center gap-2">
					<Button
						variant="outline"
						size="sm"
						onClick={async () => {
							try {
								const created = await rotateApiKey.mutateAsync(row.original.id);
								setRevealedSecret({
									label: created.record.label,
									secret: created.secret,
									kind: "rotate",
								});
								toast.success(t("apiKeys.toast.rotated"));
							} catch {
								toast.error(t("errors.generic"));
							}
						}}
					>
						{t("common.rotate")}
					</Button>
					<Button
						variant="destructive"
						size="sm"
						disabled={row.original.status === "revoked"}
						onClick={async () => {
							try {
								await revokeApiKey.mutateAsync(row.original.id);
								toast.success(t("apiKeys.toast.revoked"));
							} catch {
								toast.error(t("errors.generic"));
							}
						}}
					>
						{t("common.revoke")}
					</Button>
				</div>
			),
		},
	];
	const table = useReactTable({
		data,
		columns,
		state: { sorting },
		onSortingChange: setSorting,
		getCoreRowModel: getCoreRowModel(),
		getSortedRowModel: getSortedRowModel(),
	});

	return (
		<div className="space-y-6">
			<PageHeader
				title={t("apiKeys.title")}
				description={t("apiKeys.description")}
			/>
			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("apiKeys.createTitle")}</CardTitle>
					<CardDescription>{t("apiKeys.createDescription")}</CardDescription>
				</CardHeader>
				<CardContent className="grid gap-3 lg:grid-cols-[1fr_auto]">
					<Input
						aria-label={t("apiKeys.label")}
						value={label}
						placeholder={t("apiKeys.placeholder")}
						onChange={(event) => {
							setLabel(event.target.value);
						}}
					/>
					<Button
						disabled={createApiKey.isPending}
						onClick={async () => {
							const trimmedLabel = label.trim();
							if (!trimmedLabel) {
								toast.error(t("apiKeys.validation.labelRequired"));
								return;
							}
							if (trimmedLabel.length > 64) {
								toast.error(t("apiKeys.validation.labelTooLong"));
								return;
							}

							try {
								const created = await createApiKey.mutateAsync(trimmedLabel);
								setRevealedSecret({
									label: created.record.label,
									secret: created.secret,
									kind: "create",
								});
								setLabel("");
								toast.success(t("apiKeys.toast.created"));
							} catch {
								toast.error(t("errors.generic"));
							}
						}}
					>
						{createApiKey.isPending
							? t("common.saving")
							: t("apiKeys.createSubmit")}
					</Button>
				</CardContent>
			</Card>

			{revealedSecret ? (
				<ApiKeySecretCard
					label={revealedSecret.label}
					secret={revealedSecret.secret}
					kind={revealedSecret.kind}
					onDismiss={() => {
						setRevealedSecret(null);
					}}
				/>
			) : null}

			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("apiKeys.title")}</CardTitle>
				</CardHeader>
				<CardContent>
					{table.getRowModel().rows.length > 0 ? (
						<Table>
							<TableHeader>
								{table.getHeaderGroups().map((headerGroup) => (
									<TableRow key={headerGroup.id}>
										{headerGroup.headers.map((header) => (
											<TableHead key={header.id}>
												{header.isPlaceholder
													? null
													: flexRender(
															header.column.columnDef.header,
															header.getContext(),
														)}
											</TableHead>
										))}
									</TableRow>
								))}
							</TableHeader>
							<TableBody>
								{table.getRowModel().rows.map((row) => (
									<TableRow key={row.id}>
										{row.getVisibleCells().map((cell) => (
											<TableCell key={cell.id}>
												{cell.column.columnDef.cell
													? flexRender(
															cell.column.columnDef.cell,
															cell.getContext(),
														)
													: String(row.getValue(cell.column.id))}
											</TableCell>
										))}
									</TableRow>
								))}
							</TableBody>
						</Table>
					) : (
						<p className="text-sm text-muted-foreground">
							{t("apiKeys.empty")}
						</p>
					)}
				</CardContent>
			</Card>

			<div className="grid gap-4 xl:grid-cols-[0.95fr_1.05fr] pt-4">
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
