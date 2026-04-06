import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";

type DashboardMetricProps = {
	label: string;
	value: string;
	hint: string;
	tone?: "default" | "success" | "warning";
};

export function DashboardMetric({
	label,
	value,
	hint,
	tone = "default",
}: DashboardMetricProps) {
	return (
		<Card
			className={cn(
				"border-border/70 bg-card/90 backdrop-blur-sm",
				tone === "success" && "border-cyan-400/30 bg-cyan-500/8",
				tone === "warning" && "border-amber-400/30 bg-amber-500/8",
			)}
		>
			<CardHeader className="pb-3">
				<CardTitle className="text-xs font-medium tracking-[0.16em] uppercase text-muted-foreground">
					{label}
				</CardTitle>
			</CardHeader>
			<CardContent className="space-y-2">
				<div className="text-3xl font-semibold tracking-tight text-foreground">
					{value}
				</div>
				<p className="text-sm leading-6 text-muted-foreground">{hint}</p>
			</CardContent>
		</Card>
	);
}
