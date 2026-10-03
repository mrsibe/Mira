import * as React from "react";
import { cn } from "../../utils/cn";

export const Input = React.forwardRef<
  HTMLInputElement,
  React.InputHTMLAttributes<HTMLInputElement>
>(({ className, ...props }, ref) => (
  <input
    ref={ref}
    className={cn(
      "h-10 w-full rounded-md border border-[var(--border-strong)] bg-[var(--panel)] px-3 text-sm text-[var(--text)] outline-none transition placeholder:text-[var(--subtle)] focus:border-[var(--primary)] focus:ring-2 focus:ring-[var(--border)]",
      className,
    )}
    {...props}
  />
));

Input.displayName = "Input";
