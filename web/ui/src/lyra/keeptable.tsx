// A table in a reply, to keep: a heading sorts it (again: the other way),
// Chart shows a numeric column as bars, Excel downloads it as a spreadsheet
// (copying and CSV are Streamdown's own, on the table).

import { cn } from "@/lib/utils";
import { BarChart3, FileSpreadsheet } from "lucide-react";
import { Children, cloneElement, isValidElement, useMemo, useState, type ComponentType, type JSX, type ReactElement, type ReactNode } from "react";
import { defaultComponents, type ExtraProps } from "streamdown";
import { asNumber, toXlsx } from "./xlsx";

type Hast = { type: string; tagName?: string; value?: string; children?: Hast[] };
type Props = JSX.IntrinsicElements["table"] & ExtraProps;
type El = ReactElement<{ children?: ReactNode; className?: string; onClick?: () => void; title?: string }>;

const text = (n: Hast): string => (n.type === "text" ? (n.value ?? "") : (n.children ?? []).map(text).join(""));
const kids = (n: Hast | undefined, tag: string) => (n?.children ?? []).filter((c) => c.tagName === tag);
const elements = (node: ReactNode) => Children.toArray(node).filter(isValidElement) as El[];

/** The table's cells as text: its headings and rows. */
function data(node: Hast | undefined) {
  const head = kids(kids(node, "thead")[0], "tr")[0];
  const headers = (head?.children ?? []).filter((c) => c.tagName === "th" || c.tagName === "td").map(text);
  const rows = kids(kids(node, "tbody")[0], "tr").map((tr) => (tr.children ?? []).filter((c) => c.tagName === "td" || c.tagName === "th").map((c) => text(c).trim()));
  return { headers, rows };
}

/** Columns that are numbers in most rows (a chart can show them). */
function numeric(rows: string[][], width: number) {
  const out: number[] = [];
  for (let c = 0; c < width; c++) {
    const filled = rows.filter((r) => (r[c] ?? "").trim());
    if (filled.length >= 2 && filled.filter((r) => asNumber(r[c]) !== null).length >= filled.length * 0.7) out.push(c);
  }
  return out;
}

const DefaultTable = defaultComponents.table as ComponentType<Props>;

export function KeepTable(props: Props) {
  const { headers, rows } = useMemo(() => data(props.node as Hast | undefined), [props.node]);
  const [sort, setSort] = useState<{ col: number; up: boolean } | null>(null);
  const [chart, setChart] = useState(false);
  const nums = useMemo(() => numeric(rows, headers.length), [rows, headers.length]);
  const [shown, setShown] = useState<number | null>(null);
  // Streamdown draws thead and tbody with its own components: they're found by their place, as in the table's tree.
  const parts = elements(props.children);
  const tags = ((props.node as Hast | undefined)?.children ?? []).filter((c) => c.type === "element").map((c) => c.tagName);
  const thead = parts[tags.indexOf("thead")];
  const tbody = parts[tags.indexOf("tbody")];
  if (!thead || !tbody || rows.length < 2) return <DefaultTable {...props} />;
  const trs = elements(tbody.props.children);
  // The rows in order: by the heading picked (numbers as numbers), the way it was otherwise.
  const order = rows.map((_, i) => i);
  if (sort) {
    const key = (i: number) => rows[i][sort.col] ?? "";
    order.sort((a, b) => {
      const [x, y] = [asNumber(key(a)), asNumber(key(b))];
      const d = x !== null && y !== null ? x - y : key(a).localeCompare(key(b), undefined, { numeric: true, sensitivity: "base" });
      return sort.up ? d : -d;
    });
  }
  const headRow = elements(thead.props.children)[0];
  const ths = headRow ? elements(headRow.props.children) : [];
  const newHead = headRow
    ? cloneElement(
        thead,
        {},
        cloneElement(
          headRow,
          {},
          ths.map((th, c) =>
            cloneElement(
              th,
              { key: c, className: cn(th.props.className, "cursor-pointer select-none hover:text-foreground"), title: "Sort by this", onClick: () => setSort(sort?.col === c ? { col: c, up: !sort.up } : { col: c, up: asNumber(rows[0][c] ?? "") === null }) },
              <>
                {th.props.children}
                {sort?.col === c && <span className="ml-1 text-teal-300">{sort.up ? "▲" : "▼"}</span>}
              </>,
            ),
          ),
        ),
      )
    : thead;
  const sorted = cloneElement(tbody, {}, order.map((i) => trs[i]).filter(Boolean));
  const col = shown ?? nums[0];
  const label = headers.findIndex((_, c) => !nums.includes(c));
  const excel = () => {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(toXlsx(headers, order.map((i) => rows[i])));
    a.download = `lyra-table-${new Date().toISOString().slice(0, 10)}.xlsx`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  };
  const values = col === undefined ? [] : order.map((i) => asNumber(rows[i][col] ?? "") ?? 0);
  const most = Math.max(...values.map(Math.abs), 0) || 1;
  return (
    <div className="my-2">
      <div className="flex flex-wrap items-center justify-end gap-1 text-muted-foreground text-xs">
        {nums.length > 0 && (
          <button type="button" onClick={() => setChart(!chart)} className={cn("flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-accent/50 hover:text-foreground", chart && "text-teal-300")} title="Show a column as a chart">
            <BarChart3 className="size-3.5" /> Chart
          </button>
        )}
        <button type="button" onClick={excel} className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-accent/50 hover:text-foreground" title="Download as an Excel file">
          <FileSpreadsheet className="size-3.5" /> Excel
        </button>
      </div>
      {chart && col !== undefined && (
        <div className="my-2 space-y-1 rounded-md border p-3">
          {nums.length > 1 && (
            <div className="mb-2 flex flex-wrap gap-1">
              {nums.map((c) => (
                <button key={c} type="button" onClick={() => setShown(c)} className={cn("rounded-full border px-2 py-0.5 text-xs", c === col ? "border-teal-600 text-teal-300" : "text-muted-foreground hover:bg-accent/50")}>
                  {headers[c] || `Column ${c + 1}`}
                </button>
              ))}
            </div>
          )}
          {order.slice(0, 40).map((i, k) => (
            <div key={i} className="flex items-center gap-2 text-xs">
              <span className="w-1/3 shrink-0 truncate text-muted-foreground" title={rows[i][label] ?? ""}>
                {label >= 0 ? rows[i][label] : `#${i + 1}`}
              </span>
              <span className="h-3.5 rounded-sm" style={{ width: `${(Math.abs(values[k]) / most) * 55}%`, minWidth: 2, background: values[k] < 0 ? "rgb(248 113 113 / 0.7)" : "rgb(45 212 191 / 0.7)" }} />
              <span className="shrink-0 tabular-nums">{rows[i][col]}</span>
            </div>
          ))}
          {order.length > 40 && <p className="text-muted-foreground text-xs">…and {order.length - 40} more rows</p>}
        </div>
      )}
      <DefaultTable {...props}>
        {newHead}
        {sorted}
      </DefaultTable>
    </div>
  );
}
