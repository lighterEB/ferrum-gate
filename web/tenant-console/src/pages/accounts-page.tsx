import {
	type ColumnDef,
	flexRender,
	getCoreRowModel,
	getSortedRowModel,
	type SortingState,
	useReactTable,
} from "@tanstack/react-table";
import { MoreHorizontalIcon } from "lucide-react";
import { Fragment, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { PageHeader } from "@/components/page-header";
import { StatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import {
	Table,
	TableBody,
	TableCell,
	TableHead,
	TableHeader,
	TableRow,
} from "@/components/ui/table";
import {
	useAccountAction,
	useProviderAccounts,
} from "@/hooks/use-control-plane-queries";
import { useSessionCredentials } from "@/hooks/use-session-credentials";
import {
	getControlPlaneApiErrorKey,
	type ProviderAccountRecord,
} from "@/lib/control-plane-api";
import { formatDateTime } from "@/lib/format";

function accountIdentity(account: ProviderAccountRecord) {
	const email = account.metadata?.email;

	return typeof email === "string"
		? email
		: account.redacted_display || account.external_account_id;
}

export function AccountsPage() {
	const { t, i18n } = useTranslation();
	const { credentials } = useSessionCredentials();
	const providerAccountsQuery = useProviderAccounts(credentials);
	const accountAction = useAccountAction(credentials);
	const [sorting, setSorting] = useState<SortingState>([
		{ id: "last_validated_at", desc: true },
	]);
	const [statusFilter, setStatusFilter] = useState("all");
	const [expandedAccountId, setExpandedAccountId] = useState<string | null>(
		null,
	);

	const allAccounts = providerAccountsQuery.data ?? [];
	const data =
		statusFilter === "all"
			? allAccounts
			: allAccounts.filter((account) => account.state === statusFilter);

	const columns: ColumnDef<ProviderAccountRecord>[] = [
		{
			id: "identity",
			header: t("accounts.columns.identity"),
			accessorFn: (row) => accountIdentity(row),
			cell: ({ row }) => (
				<div className="space-y-1">
					<p className="font-medium text-foreground">
						{accountIdentity(row.original)}
					</p>
					<p className="text-xs text-muted-foreground">
						{row.original.external_account_id}
					</p>
				</div>
			),
		},
		{
			accessorKey: "state",
			header: t("accounts.columns.status"),
			cell: ({ row }) => <StatusBadge status={row.original.state} />,
		},
		{
			accessorKey: "provider",
			header: t("accounts.columns.provider"),
		},
		{
			accessorKey: "plan_type",
			header: t("accounts.columns.plan"),
			cell: ({ row }) => row.original.plan_type ?? t("common.unknown"),
		},
		{
			accessorKey: "last_validated_at",
			header: t("accounts.columns.lastValidated"),
			cell: ({ row }) =>
				formatDateTime(
					row.original.last_validated_at,
					i18n.language,
					t("common.never"),
				),
		},
		{
			accessorKey: "expires_at",
			header: t("accounts.columns.expiresAt"),
			cell: ({ row }) =>
				formatDateTime(
					row.original.expires_at,
					i18n.language,
					t("common.never"),
				),
		},
		{
			id: "actions",
			enableSorting: false,
			header: t("accounts.columns.actions"),
			cell: ({ row }) => (
				<DropdownMenu>
					<DropdownMenuTrigger render={<Button variant="outline" size="sm" />}>
						<MoreHorizontalIcon className="size-4" />
					</DropdownMenuTrigger>
					<DropdownMenuContent align="end">
						{(
							[
								"probe",
								"quota",
								"refresh",
								"enable",
								"disable",
								"drain",
							] as const
						).map((action) => (
							<DropdownMenuItem
								key={action}
								onClick={async () => {
									try {
										await accountAction.mutateAsync({
											accountId: row.original.id,
											action,
										});
										toast.success(t(`accounts.actions.${action}`));
									} catch (error) {
										toast.error(t(getControlPlaneApiErrorKey(error)));
									}
								}}
							>
								{t(`accounts.actions.${action}`)}
							</DropdownMenuItem>
						))}
					</DropdownMenuContent>
				</DropdownMenu>
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
				title={t("accounts.title")}
				description={t("accounts.description")}
				actions={
					<div className="flex items-center gap-3">
						<span className="text-sm text-muted-foreground">
							{t("accounts.filterLabel")}
						</span>
						<Select
							value={statusFilter}
							onValueChange={(value) => {
								setStatusFilter(value ?? "all");
							}}
						>
							<SelectTrigger className="w-44">
								<SelectValue />
							</SelectTrigger>
							<SelectContent align="end">
								<SelectItem value="all">{t("status.all")}</SelectItem>
								<SelectItem value="active">{t("status.active")}</SelectItem>
								<SelectItem value="cooling">{t("status.cooling")}</SelectItem>
								<SelectItem value="disabled">{t("status.disabled")}</SelectItem>
								<SelectItem value="draining">{t("status.draining")}</SelectItem>
							</SelectContent>
						</Select>
					</div>
				}
			/>
			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("accounts.title")}</CardTitle>
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
									<Fragment key={row.id}>
										<TableRow>
											{row.getVisibleCells().map((cell) => (
												<TableCell key={cell.id}>
													{cell.column.id === "identity" ? (
														<div className="flex items-start justify-between gap-3">
															{flexRender(
																cell.column.columnDef.cell,
																cell.getContext(),
															)}
															<Button
																variant="ghost"
																size="sm"
																onClick={() => {
																	setExpandedAccountId((current) =>
																		current === row.original.id
																			? null
																			: row.original.id,
																	);
																}}
															>
																{expandedAccountId === row.original.id
																	? t("accounts.collapse")
																	: t("accounts.expand")}
															</Button>
														</div>
													) : cell.column.columnDef.cell ? (
														flexRender(
															cell.column.columnDef.cell,
															cell.getContext(),
														)
													) : (
														String(row.getValue(cell.column.id))
													)}
												</TableCell>
											))}
										</TableRow>
										{expandedAccountId === row.original.id ? (
											<TableRow>
												<TableCell
													colSpan={columns.length}
													className="whitespace-normal"
												>
													<div className="grid gap-4 rounded-lg border border-border/70 bg-background/70 p-4 lg:grid-cols-2">
														<div className="space-y-3">
															<p className="text-sm font-medium text-foreground">
																{t("accounts.capabilities")}
															</p>
															<div className="flex flex-wrap gap-2">
																{row.original.capabilities.length > 0 ? (
																	row.original.capabilities.map(
																		(capability) => (
																			<Button
																				key={capability}
																				variant="outline"
																				size="sm"
																				disabled
																			>
																				{capability}
																			</Button>
																		),
																	)
																) : (
																	<p className="text-sm text-muted-foreground">
																		{t("accounts.noCapabilities")}
																	</p>
																)}
															</div>
														</div>
														<div className="space-y-3">
															<p className="text-sm font-medium text-foreground">
																{t("accounts.quota")}
															</p>
															{row.original.quota ? (
																<div className="space-y-2 text-sm text-muted-foreground">
																	<p>
																		{row.original.quota.plan_label ??
																			t("common.unknown")}
																	</p>
																	<p>
																		remaining_requests_hint:{" "}
																		{row.original.quota
																			.remaining_requests_hint ??
																			t("common.unknown")}
																	</p>
																	<pre className="overflow-x-auto rounded-lg border border-border/70 bg-background p-3 text-xs text-foreground">
																		{JSON.stringify(
																			row.original.quota.details,
																			null,
																			2,
																		)}
																	</pre>
																</div>
															) : (
																<p className="text-sm text-muted-foreground">
																	{t("accounts.quotaMissing")}
																</p>
															)}
														</div>
													</div>
												</TableCell>
											</TableRow>
										) : null}
									</Fragment>
								))}
							</TableBody>
						</Table>
					) : (
						<p className="text-sm text-muted-foreground">
							{t("accounts.empty")}
						</p>
					)}
				</CardContent>
			</Card>
		</div>
	);
}
