// Click-to-copy for the command lines on the site's pages: a console (.console: a drawn prompt and
// a command) and a code block in a doc. Each gets a Copy button; a click on the command copies too,
// unless the reader is selecting part of it. The button says what happened for a moment, and a
// status line tells a screen reader. Where the clipboard refuses, the command is selected instead,
// for the reader to copy. The landing page's script copies its console through window.copyCommand.
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
      const range = document.createRange();
      range.selectNodeContents(box.querySelector("code") ?? box);
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      said = "Selected";
    }
    // The button answers the click: a press and settle, an ink fill, a tick drawn in front of the
    // word (site.css). Taking the class off and putting it back replays it for a second click.
    box.classList.remove("copied");
    void box.offsetWidth;
    box.classList.add("copied");
    if (button) button.innerHTML = `<svg class="tick" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.2 6.4 4.9 9.1 9.8 3.2"/></svg>${said}`;
    status.textContent = `${said}: ${text}`;
    clearTimeout(box.copyTimer);
    box.copyTimer = setTimeout(() => { box.classList.remove("copied"); if (button) button.textContent = "Copy"; }, 1600);
  }
  window.copyCommand = copy;

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
    const button = Object.assign(document.createElement("button"), { type: "button", className: "copy", textContent: "Copy" });
    box.append(button);
    button.addEventListener("click", () => copy(box));
    box.querySelector("code")?.addEventListener("click", () => { if (!getSelection().toString()) copy(box); });
  }
})();
