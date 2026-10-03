import * as React from "react";
import { cn } from "../../utils/cn";

export const Textarea = React.forwardRef<
  HTMLTextAreaElement,
  React.TextareaHTMLAttributes<HTMLTextAreaElement>
>(({ className, ...props }, ref) => (
  <textarea
    ref={ref}
    className={cn(
      "min-h-24 w-full resize-none rounded-md border border-[var(--border-strong)] bg-[var(--panel)] px-3 py-3 text-sm text-[var(--text)] outline-none transition placeholder:text-[var(--subtle)] focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--border)]",
      className,
    )}
    {...props}
  />
));

Textarea.displayName = "Textarea";
