export function formatDateTime(
	value: string | null | undefined,
	locale: string,
	fallback: string,
) {
	if (!value) {
		return fallback;
	}

	const date = new Date(value);
	if (Number.isNaN(date.getTime())) {
		return fallback;
	}

	return new Intl.DateTimeFormat(locale, {
		dateStyle: "medium",
		timeStyle: "short",
	}).format(date);
}
