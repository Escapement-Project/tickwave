// A sidecar's hand-written file. Its TOML is parsed here: the WASM resolver carries no TOML
// parser and takes the file as JSON.

import { parse } from "smol-toml";

/** The file's TOML as JSON, and the names it includes. `where` names the file in a syntax error. */
export function handJson(text: string, where: string): { json: string; include: string[] } {
  let parsed: { include?: unknown };
  let json: string;
  try {
    parsed = parse(text, { integersAsBigInt: true });
    json = JSON.stringify(parsed, tomlNumber);
  } catch (e) {
    throw new Error(`${where}: ${e instanceof Error ? e.message : e}`);
  }
  const include = Array.isArray(parsed.include) ? parsed.include.filter((n): n is string => typeof n === "string") : [];
  return { json, include };
}

/**
 * Integers arrive as BigInt, so a TOML float stays apart from a whole number once in JSON. An
 * integer goes on as a number, refused where a double cannot hold it exactly. A float goes on as
 * an object, which no field takes, so the resolver refuses it as the CLI's TOML parser does.
 */
function tomlNumber(key: string, value: unknown): unknown {
  if (typeof value === "bigint") {
    if (value > BigInt(Number.MAX_SAFE_INTEGER) || value < BigInt(Number.MIN_SAFE_INTEGER)) {
      throw new Error(`${key} = ${value}: a JavaScript number cannot hold it exactly`);
    }
    return Number(value);
  }
  if (typeof value === "number") return { float: String(value) };
  return value;
}
