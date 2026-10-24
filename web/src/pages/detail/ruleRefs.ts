const LOGICAL_TYPES = new Set(["AND", "OR", "NOT"]);

interface Range {
  start: number;
  end: number;
}

function nextLevelRanges(payload: string): Range[] | null {
  if (!payload.startsWith("(") || !payload.endsWith(")")) return null;
  const stack: number[] = [];
  const ranges: Range[] = [];
  for (let i = 0; i < payload.length; i++) {
    if (payload[i] === "(") stack.push(i);
    else if (payload[i] === ")") {
      const start = stack.pop();
      if (start === undefined) return null;
      ranges.push({ start, end: i });
    }
  }
  if (stack.length > 0) return null;
  ranges.sort((a, b) => a.start - b.start);
  const top: Range[] = [];
  for (const range of ranges) {
    if (range.start === 0 && range.end === payload.length - 1) continue;
    if (!top.some((other) => other.start < range.start && other.end > range.end)) {
      top.push(range);
    }
  }
  return top;
}

function collectLogicPayload(payload: string, depth: number, names: string[]): boolean {
  if (depth > 16) return false;
  const ranges = nextLevelRanges(payload);
  if (!ranges) return false;
  for (const range of ranges) {
    const items = payload
      .slice(range.start + 1, range.end)
      .split(",")
      .map((item) => item.trim());
    const type = (items[0] ?? "").toUpperCase();
    if (type === "RULE-SET") {
      if (!items[1]) return false;
      names.push(items[1]);
    } else if (LOGICAL_TYPES.has(type)) {
      const nested = items.slice(1).join(",");
      if (!nested || !collectLogicPayload(nested, depth + 1, names)) return false;
    } else if (type !== "MATCH" && !items[1]) {
      return false;
    }
  }
  return true;
}

/** 提取一条规则直接或在逻辑规则内引用的 RULE-SET 名；非法行按无引用处理。 */
export function ruleSetRefs(line: string): string[] {
  const raw = line.trim();
  if (!raw || raw.startsWith("#")) return [];
  const items = raw.split(",").map((item) => item.trim());
  const type = (items.shift() ?? "").toUpperCase();
  if (type === "RULE-SET") return items[0] && items[1] ? [items[0]] : [];
  if (type !== "AND" && type !== "OR" && type !== "NOT" && type !== "SUB-RULE") return [];
  if (items.length === 0 || !items[items.length - 1]) return [];
  items.pop();
  const payload = items.join(",");
  if (!payload) return [];
  const names: string[] = [];
  return collectLogicPayload(type === "SUB-RULE" ? `(${payload})` : payload, 1, names)
    ? names
    : [];
}
