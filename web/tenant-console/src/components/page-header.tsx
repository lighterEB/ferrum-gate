import type { ReactNode } from "react";

type PageHeaderProps = {
	title: string;
	description: string;
	actions?: ReactNode;
};

export function PageHeader({ title, description, actions }: PageHeaderProps) {
	return (
		<div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
			<div className="space-y-2">
				<h1 className="text-3xl font-semibold tracking-tight text-foreground">
					{title}
				</h1>
				<p className="max-w-3xl text-sm leading-6 text-muted-foreground sm:text-base">
					{description}
				</p>
			</div>
			{actions ? (
				<div className="flex flex-wrap items-center gap-3">{actions}</div>
			) : null}
		</div>
	);
}
