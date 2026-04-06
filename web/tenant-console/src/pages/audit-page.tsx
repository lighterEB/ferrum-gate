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

import { PageHeader } from "@/components/page-header";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
	Table,
	TableBody,
	TableCell,
	TableHead,
	TableHeader,
	TableRow,
} from "@/components/ui/table";
import { useAuditEvents } from "@/hooks/use-control-plane-queries";
import { useSessionCredentials } from "@/hooks/use-session-credentials";
import type { AuditEvent } from "@/lib/control-plane-api";
import { formatDateTime } from "@/lib/format";

export function AuditPage() {
	const { t, i18n } = useTranslation();
	const { credentials } = useSessionCredentials();
	const auditEventsQuery = useAuditEvents(credentials);
	const [sorting, setSorting] = useState<SortingState>([
		{ id: "occurred_at", desc: true },
	]);
	const data = auditEventsQuery.data ?? [];
	const columns: ColumnDef<AuditEvent>[] = [
		{
			accessorKey: "action",
			header: t("audit.columns.action"),
		},
		{
			accessorKey: "actor",
			header: t("audit.columns.actor"),
		},
		{
			accessorKey: "resource",
			header: t("audit.columns.resource"),
		},
		{
			accessorKey: "request_id",
			header: t("audit.columns.requestId"),
		},
		{
			accessorKey: "occurred_at",
			header: t("audit.columns.time"),
			cell: ({ row }) =>
				formatDateTime(
					row.original.occurred_at,
					i18n.language,
					t("common.never"),
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
				title={t("audit.title")}
				description={t("audit.description")}
			/>
			<Card className="border-border/70 bg-card/90">
				<CardHeader>
					<CardTitle>{t("audit.title")}</CardTitle>
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
													: String(cell.getValue() ?? "")}
											</TableCell>
										))}
									</TableRow>
								))}
							</TableBody>
						</Table>
					) : (
						<p className="text-sm text-muted-foreground">{t("audit.empty")}</p>
					)}
				</CardContent>
			</Card>
		</div>
	);
}
