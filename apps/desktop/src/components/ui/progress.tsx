import * as React from "react"
import { Progress as ProgressPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"

function Progress({
  className,
  value,
  ...props
}: React.ComponentProps<typeof ProgressPrimitive.Root>) {
  return (
    <ProgressPrimitive.Root
      data-slot="progress"
      data-indeterminate={value == null ? "true" : undefined}
      className={cn("relative flex h-1 w-full items-center overflow-x-hidden rounded-full bg-muted", className)}
      {...props}
    >
      <ProgressPrimitive.Indicator
        data-slot="progress-indicator"
        data-indeterminate={value == null ? "true" : undefined}
        className="size-full flex-1 bg-primary transition-all motion-safe:data-[indeterminate=true]:animate-pulse motion-reduce:transition-none"
        style={{ transform: `translateX(-${value == null ? 45 : 100 - value}%)` }}
      />
    </ProgressPrimitive.Root>
  )
}

export { Progress }
