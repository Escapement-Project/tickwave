// Highlights the docs' code blocks at build time. Catppuccin decides which scopes share a colour;
// the colour itself is a role from the project's tokens.css, such as `var(--code-string)`, so a
// block follows the page's face with no stylesheet of its own. A block marked text is output, and
// stays plain, as does one with no language.

import { bundledThemes, createHighlighter, type ThemeRegistration } from "shiki";

/** The languages the docs' code blocks use. Another is refused, so a new one is added on purpose. */
const LANGUAGES = ["sh", "toml", "json", "html"] as const;
type Language = (typeof LANGUAGES)[number];
const listed = (lang: string): lang is Language => (LANGUAGES as readonly string[]).includes(lang);

/** Each colour of Catppuccin Latte, by the property of tokens.css it stands for: null is the
 * text, in the code block's own ink. Comments and punctuation are secondary text. */
const ROLES: Record<string, string | null> = {
  "#4c4f69": null, // text
  "#8839ef": "--code-keyword", // mauve
  "#df8e1d": "--code-type", // yellow
  "#04a5e5": "--code-type", // sky
  "#209fb5": "--code-type", // sapphire
  "#1e66f5": "--code-function", // blue
  "#7287fd": "--code-function", // lavender
  "#fe640b": "--code-number", // peach
  "#d20f39": "--code-builtin", // red
  "#179299": "--code-operator", // teal
  "#ea76cb": "--code-escape", // pink
  "#dc8a78": "--code-escape", // rosewater
  "#dd7878": "--code-escape", // flamingo
  "#40a02b": "--code-string", // green
  "#e64553": "--code-parameter", // maroon
  "#7c7f93": "--ink-2", // overlay2
  "#6c6f85": "--ink-2", // subtext0
};

/** Catppuccin Latte's scope rules, each colour replaced by its property. Its font styles
 * go: the code face has no italic or bold, and the browser would fake them. */
function roleTheme(latte: ThemeRegistration): ThemeRegistration {
  const role = (colour: string): string => {
    const r = ROLES[colour.toLowerCase()];
    if (r === undefined) throw new Error(`Catppuccin Latte has a colour with no role: ${colour}`);
    return r === null ? "inherit" : `var(${r})`;
  };
  return {
    name: "tickwave-roles",
    type: "light",
    fg: "inherit",
    bg: "transparent",
    settings: (latte.settings ?? latte.tokenColors ?? []).map((s) => ({
      ...s,
      settings: { ...(s.settings.foreground && { foreground: role(s.settings.foreground) }) },
    })),
  };
}

/** A function from a code block and its language to the HTML inside its `<code>`, or "" to leave
 * it plain. */
export async function codeHighlighter(): Promise<(code: string, lang: string) => string> {
  const theme = roleTheme((await bundledThemes["catppuccin-latte"]()).default as ThemeRegistration);
  const highlighter = await createHighlighter({ themes: [theme], langs: [...LANGUAGES] });
  const escape = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  return (code, lang) => {
    if (lang === "" || lang === "text") return "";
    if (!listed(lang)) {
      throw new Error(`a code block in ${lang}: add it to web/scripts/highlight.ts's languages, or mark the block text`);
    }
    const lines = highlighter.codeToTokensBase(code.replace(/\n$/, ""), { lang, theme });
    return (
      lines
        .map((line) =>
          line
            .map((t) => (t.color && t.color !== "inherit" ? `<span style="color:${t.color}">${escape(t.content)}</span>` : escape(t.content)))
            .join(""),
        )
        .join("\n") + "\n"
    );
  };
}
