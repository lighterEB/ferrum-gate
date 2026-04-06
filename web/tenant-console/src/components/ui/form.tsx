import * as React from "react";
import {
	Controller,
	type ControllerProps,
	type FieldPath,
	type FieldValues,
	FormProvider,
	useFormContext,
} from "react-hook-form";

import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";

const Form = FormProvider;

type FormFieldContextValue<
	TFieldValues extends FieldValues = FieldValues,
	TName extends FieldPath<TFieldValues> = FieldPath<TFieldValues>,
> = {
	name: TName;
};

const FormFieldContext = React.createContext<FormFieldContextValue | null>(
	null,
);

const FormItemContext = React.createContext<{ id: string } | null>(null);

function FormField<
	TFieldValues extends FieldValues = FieldValues,
	TName extends FieldPath<TFieldValues> = FieldPath<TFieldValues>,
>({ ...props }: ControllerProps<TFieldValues, TName>) {
	return (
		<FormFieldContext.Provider value={{ name: props.name }}>
			<Controller {...props} />
		</FormFieldContext.Provider>
	);
}

function FormItem({ className, ...props }: React.ComponentProps<"div">) {
	const id = React.useId();

	return (
		<FormItemContext.Provider value={{ id }}>
			<div className={cn("space-y-2", className)} {...props} />
		</FormItemContext.Provider>
	);
}

function useFormField() {
	const fieldContext = React.useContext(FormFieldContext);
	const itemContext = React.useContext(FormItemContext);
	const { getFieldState, formState } = useFormContext();

	if (!fieldContext || !itemContext) {
		throw new Error("useFormField must be used within FormField and FormItem");
	}

	const fieldState = getFieldState(fieldContext.name, formState);

	return {
		id: itemContext.id,
		name: fieldContext.name,
		formItemId: `${itemContext.id}-item`,
		formDescriptionId: `${itemContext.id}-description`,
		formMessageId: `${itemContext.id}-message`,
		...fieldState,
	};
}

function FormLabel({
	className,
	...props
}: React.ComponentProps<typeof Label>) {
	const { error, formItemId } = useFormField();

	return (
		<Label
			className={cn(error ? "text-destructive" : null, className)}
			htmlFor={formItemId}
			{...props}
		/>
	);
}

function FormControl({ children }: { children: React.ReactElement }) {
	const { error, formItemId, formDescriptionId, formMessageId } =
		useFormField();

	return React.cloneElement(
		children as React.ReactElement<Record<string, unknown>>,
		{
			id: formItemId,
			"aria-describedby": error
				? `${formDescriptionId} ${formMessageId}`
				: formDescriptionId,
			"aria-invalid": Boolean(error),
		},
	);
}

function FormDescription({ className, ...props }: React.ComponentProps<"p">) {
	const { formDescriptionId } = useFormField();

	return (
		<p
			id={formDescriptionId}
			className={cn("text-sm text-muted-foreground", className)}
			{...props}
		/>
	);
}

function FormMessage({
	className,
	children,
	...props
}: React.ComponentProps<"p">) {
	const { error, formMessageId } = useFormField();
	const body = error ? String(error.message ?? "") : children;

	if (!body) {
		return null;
	}

	return (
		<p
			id={formMessageId}
			className={cn("text-sm font-medium text-destructive", className)}
			{...props}
		>
			{body}
		</p>
	);
}

export {
	Form,
	FormControl,
	FormDescription,
	FormField,
	FormItem,
	FormLabel,
	FormMessage,
};
