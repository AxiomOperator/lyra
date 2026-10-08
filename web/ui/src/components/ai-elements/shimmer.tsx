"use client";

import { cn } from "@/lib/utils";
import type { CSSProperties, ElementType } from "react";
import { memo, useMemo } from "react";

// The stock component animates with motion (about 150 KB in the first load);
// lyra's is the same sweep as a CSS animation (`lyra-shimmer` in index.css).

export interface TextShimmerProps {
  children: string;
  as?: ElementType;
  className?: string;
  duration?: number;
  spread?: number;
}

const ShimmerComponent = ({
  children,
  as: Component = "p",
  className,
  duration = 2,
  spread = 2,
}: TextShimmerProps) => {
  const dynamicSpread = useMemo(
    () => (children?.length ?? 0) * spread,
    [children, spread]
  );

  return (
    <Component
      className={cn(
        "relative inline-block bg-[length:250%_100%,auto] bg-clip-text text-transparent",
        "[--bg:linear-gradient(90deg,#0000_calc(50%-var(--spread)),var(--color-background),#0000_calc(50%+var(--spread)))] [background-repeat:no-repeat,padding-box]",
        className
      )}
      style={
        {
          "--spread": `${dynamicSpread}px`,
          backgroundImage:
            "var(--bg), linear-gradient(var(--color-muted-foreground), var(--color-muted-foreground))",
          animation: `lyra-shimmer ${duration}s linear infinite`,
        } as CSSProperties
      }
    >
      {children}
    </Component>
  );
};

export const Shimmer = memo(ShimmerComponent);
