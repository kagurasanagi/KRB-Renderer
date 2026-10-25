// どのページにも入れる：説明（title）を OS の白い吹き出しではなく、Rust が出す暗い吹き出し（tooltip.html）で見せる。
// title は data-tip に移して OS の吹き出しを止め、載せて少し待ったら、要素の位置（このページの中の CSS の px）と文字を Rust に送る
(() => {
  const post = (m) => window.ipc.postMessage(JSON.stringify(m));
  const strip = (root) => {
    if (root.nodeType !== 1) return;
    const all = root.matches("[title]") ? [root, ...root.querySelectorAll("[title]")] : root.querySelectorAll("[title]");
    for (const e of all) {
      const t = e.getAttribute("title");
      e.removeAttribute("title");
      if (t) e.dataset.tip = t;
      else delete e.dataset.tip;
    }
  };
  new MutationObserver((ms) => {
    for (const m of ms) {
      if (m.type === "attributes") strip(m.target);
      else m.addedNodes.forEach(strip);
    }
  }).observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ["title"] });
  strip(document.documentElement);

  // 毎フレーム描き直すページ（ステータスバー）では要素が替わるので、要素ではなく説明の文字とマウスの位置で追う
  let current = null;
  let at = { x: 0, y: 0 };
  let timer = 0;
  let shown = false;
  const tipAt = (x, y) => {
    const e = document.elementFromPoint(x, y);
    return e && e.closest("[data-tip]");
  };
  const hide = () => {
    clearTimeout(timer);
    current = null;
    if (shown) post({ type: "tip", text: "" });
    shown = false;
  };
  document.addEventListener("mousemove", (e) => {
    at = { x: e.clientX, y: e.clientY };
    const t = tipAt(at.x, at.y);
    const text = t ? t.dataset.tip : null;
    if (text === current) return;
    hide();
    if (!text) return;
    current = text;
    timer = setTimeout(() => {
      const t = tipAt(at.x, at.y);
      if (!t || t.dataset.tip !== current) return;
      const r = t.getBoundingClientRect();
      post({ type: "tip", text: current, left: r.left, top: r.top, right: r.right, bottom: r.bottom });
      shown = true;
    }, 450);
  });
  // ページの外（3D 表示など）へ出た
  document.addEventListener("mouseout", (e) => {
    if (!e.relatedTarget) hide();
  });
  document.addEventListener("pointerdown", hide, true);
  document.addEventListener("wheel", hide, { passive: true, capture: true });
  window.addEventListener("blur", hide);
})();

// どのページにも入れる：色の欄を押したとき、下に開く色選びの窓（WebView の中に描かれ、外は切れる。高さは CSS の 250 px ほど）の
// 入る場所が無ければ、先に欄を上へスクロールしておく（窓は click で開くので、その前に）
document.addEventListener("click", (e) => {
  const c = e.target;
  if (!(c instanceof HTMLInputElement) || c.type !== "color") return;
  const need = 262 - (window.innerHeight - c.getBoundingClientRect().bottom);
  if (need <= 0) return;
  for (let p = c.parentElement; p; p = p.parentElement) {
    const y = getComputedStyle(p).overflowY;
    if ((y === "auto" || y === "scroll") && p.scrollHeight > p.clientHeight) {
      p.scrollTop += need;
      return;
    }
  }
}, true);
