import type { HTMLAttributes } from "react";
import { cn } from "../../utils/cn";

export function Badge({
  className,
  ...props
}: HTMLAttributes<HTMLSpanElement>) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-md border border-[var(--border-strong)] bg-[var(--hover)] px-2 py-0.5 text-xs font-medium text-[var(--muted)]",
        className,
      )}
      {...props}
    />
  );
}
