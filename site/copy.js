// Click-to-copy for the command lines on the site's pages: a console (.console: a drawn prompt and
// a command) and a code block in a doc. Each gets a Copy button; a click on the command copies too,
// unless the reader is selecting part of it. The button says what happened for a moment, and a
// status line tells a screen reader. Where the clipboard refuses, the command is selected instead,
// for the reader to copy.
(() => {
  const status = Object.assign(document.createElement("p"), { className: "sr-only" });
  status.setAttribute("role", "status");
  document.body.append(status);

  // The command as a reader would type it: the prompt is drawn, not part of it.
  const textOf = (box) => {
    const code = box.querySelector("code") ?? box;
    if (box.tagName === "PRE" || box.classList.contains("codeblock")) return code.textContent.replace(/\n$/, "");
    return [...code.childNodes].filter((n) => !(n.nodeType === 1 && n.getAttribute("aria-hidden") === "true"))
      .map((n) => n.textContent).join("").trim();
  };

  async function copy(box) {
    const button = box.querySelector(":scope > .copy"), text = textOf(box);
    let said = "Copied";
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const code = box.querySelector("code") ?? box, prompt = code.querySelector(':scope > [aria-hidden="true"]');
      const range = document.createRange();
      range.selectNodeContents(code);
      if (prompt) range.setStartAfter(prompt);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      said = "Selected";
    }
    // The button answers the click: a press and settle, an ink fill, its word fading to the word
    // for what happened, a copy with a tick drawn in front of it (base.css). Taking the class off and putting it back replays it
    // for a second click.
    box.classList.remove("copied");
    void box.offsetWidth;
    box.classList.add("copied");
    if (button) say(button, said);
    status.textContent = `${said}: ${text}`;
    clearTimeout(box.copyTimer);
    box.copyTimer = setTimeout(() => { box.classList.remove("copied"); if (button) say(button, "Copy"); }, 1600);
  }

  const tick = '<svg class="tick" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.2 6.4 4.9 9.1 9.8 3.2"/></svg>';
  // Only a copy that happened gets the tick: where the clipboard refused, the command is selected
  // for the reader to copy.
  const words = { Copy: "", Copied: tick, Selected: "" };
  const makeButton = () => {
    const b = Object.assign(document.createElement("button"), { type: "button", className: "copy" });
    b.innerHTML = Object.entries(words).map(([w, mark]) => `<span class="say" data-say="${w}">${mark}${w}</span>`).join("");
    say(b, "Copy");
    return b;
  };
  // Shows one word; the others are hidden from a screen reader as well as faded out. The button
  // goes from its width to the new word's: a width set from the old one, then the new, which the
  // transition runs between (base.css), and none once there, so a late font sizes it again.
  function say(b, word) {
    const from = b.offsetWidth;
    for (const s of b.children) {
      const on = s.dataset.say === word;
      s.classList.toggle("on", on);
      if (on) s.removeAttribute("aria-hidden");
      else s.setAttribute("aria-hidden", "true");
    }
    if (!b.isConnected) return;
    b.style.width = "";
    const to = b.offsetWidth;
    if (from === to || matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    b.style.width = `${from}px`;
    void b.offsetWidth;
    b.style.width = `${to}px`;
    b.addEventListener("transitionend", function settle(e) {
      if (e.propertyName !== "width") return;
      b.style.width = "";
      b.removeEventListener("transitionend", settle);
    });
  }
  const boxes = [...document.querySelectorAll(".console")];
  // A doc's code block scrolls sideways, so its button sits on a wrapper that does not. A block
  // marked text is a program's output, not something to type: Zola marks it data-lang (as output,
  // since its highlighter reads text as plain), markdown-it (tickwave's docs) a class.
  for (const pre of document.querySelectorAll("article.doc pre")) {
    if (pre.querySelector(':scope > code:is([data-lang="text"], [data-lang="output"], .language-text)')) continue;
    const wrap = Object.assign(document.createElement("div"), { className: "codeblock" });
    pre.before(wrap);
    wrap.append(pre);
    boxes.push(wrap);
  }
  // While the reader selects text in a box, its button steps aside, so it neither covers the text
  // nor looks part of what they are selecting.
  document.addEventListener("selectionchange", () => {
    const sel = getSelection(), range = sel.rangeCount && !sel.isCollapsed ? sel.getRangeAt(0) : null;
    for (const box of boxes) box.classList.toggle("selecting", !!range && range.intersectsNode(box));
  });
  for (const box of boxes) {
    const b = makeButton();
    box.append(b);
    b.addEventListener("click", () => copy(box));
    box.querySelector("code")?.addEventListener("click", () => { if (!getSelection().toString()) copy(box); });
  }
})();
